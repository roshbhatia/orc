#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

provider_kind=${ORC_PROVIDER_KIND:?ORC_PROVIDER_KIND is required}
request=$(cat)
version=$(jq -er '.version' <<< "$request")
capability=$(jq -er '.capability' <<< "$request")
scope=$(jq -er '.scope' <<< "$request")

if [[ $version != "orc.provider/v1" ]]; then
  printf 'orc-provider-%s: unsupported request version: %s\n' "$provider_kind" "$version" >&2
  exit 2
fi

emit_plan_with_codes() {
  local success_codes cwd environment command_json
  success_codes=$1
  cwd=$2
  environment=$3
  shift 3
  if (($# == 0)); then
    printf 'orc-provider-%s: command plan is empty\n' "$provider_kind" >&2
    exit 2
  fi
  command_json=$(printf '%s\0' "$@" | jq -Rs 'split("\u0000")[:-1]')
  jq -n \
    --arg cwd "$cwd" \
    --argjson command "$command_json" \
    --argjson environment "$environment" \
    --argjson success_codes "$success_codes" \
    '{
      version: "orc.provider/v1",
      command: $command,
      cwd: $cwd,
      environment: $environment,
      successCodes: $success_codes
    }'
}

emit_plan() {
  emit_plan_with_codes '[0]' "$@"
}

emit_declined() {
  jq -n --arg reason "$1" \
    '{version: "orc.provider/v1", status: "declined", reason: $reason}'
}

emit_binding() {
  local kind binding_status ref label
  kind=$1
  binding_status=$2
  ref=$3
  label=$4
  jq -n \
    --arg kind "$kind" \
    --arg status "$binding_status" \
    --arg ref "$ref" \
    --arg label "$label" \
    '{
      version: "orc.provider/v1",
      binding: {
        kind: $kind,
        status: $status,
        ref: (if $ref == "" then null else $ref end),
        label: $label
      }
    }'
}

emit_description() {
  jq -n \
    --arg title "$1" \
    --arg goal "$2" \
    '{version: "orc.provider/v1", description: {title: $title, goal: $goal}}'
}

emit_validation() {
  jq -n \
    --arg status "$1" \
    --arg name "$2" \
    --arg message "$3" \
    '{
      version: "orc.provider/v1",
      status: $status,
      checks: [{name: $name, status: $status, message: $message}]
    }'
}

require_command() {
  provider_command=$(command -v "$1" || true)
  [[ -n $provider_command ]]
}

read_command() {
  command=()
  while IFS= read -r -d '' value; do
    command+=("$value")
  done < <(jq -j "$1 | .[] | @text, \"\u0000\"" <<< "$request")
  if ((${#command[@]} == 0)); then
    printf 'orc-provider-%s: input command is empty\n' "$provider_kind" >&2
    exit 2
  fi
}

current_session_matches() {
  local native_id candidate
  native_id=$(jq -r '.session.nativeId // empty' <<< "$request")
  if [[ -z $native_id ]]; then
    return 1
  fi
  for candidate in \
    "${ORC_NATIVE_SESSION_ID:-}" \
    "${CODEX_THREAD_ID:-}" \
    "${CODEX_SESSION_ID:-}" \
    "${CLAUDE_CODE_SESSION_ID:-}" \
    "${CLAUDE_SESSION_ID:-}" \
    "${OPENCODE_SESSION_ID:-}"; do
    if [[ -n $candidate && $candidate == "$native_id" ]]; then
      return 0
    fi
  done
  return 1
}

agent_registry=${ORC_AGENT_REGISTRY:-${XDG_CONFIG_HOME:-$HOME/.config}/sysinit/agents.json}

case "$provider_kind:$capability" in
  *:provider.validate)
    if [[ $provider_kind == "local" ]]; then
      emit_validation "ok" "runtime" "local process execution is available"
      exit 0
    fi
    if [[ $provider_kind == "harness" ]]; then
      if [[ ! -s $agent_registry ]] || ! jq -e '
        .agents
        | type == "array" and length > 0
        and all(.[]; (.name | type) == "string" and (.command | type) == "string")
      ' "$agent_registry" > /dev/null; then
        emit_validation "failed" "registry" "agent registry is missing or invalid"
        exit 0
      fi
      missing=()
      while IFS=$'\t' read -r harness command_name; do
        if ! command -v "$command_name" > /dev/null; then
          missing+=("$harness:$command_name")
        fi
      done < <(
        jq -r '
          .agents[]
          | select((.launch.resumeArgs // []) | length > 0)
          | [.name, .command]
          | @tsv
        ' "$agent_registry"
      )
      if ((${#missing[@]} > 0)); then
        emit_validation "failed" "harnesses" "missing commands: ${missing[*]}"
      else
        count=$(jq -r '.agents | length' "$agent_registry")
        emit_validation "ok" "harnesses" "$count registered harnesses have valid resume commands"
      fi
      exit 0
    fi
    require_command "$provider_kind" || {
      emit_validation "failed" "dependency" "$provider_kind is unavailable"
      exit 0
    }
    emit_validation "ok" "dependency" "$provider_kind is available at $provider_command"
    ;;
  changes:changes.inspect)
    require_command changes || {
      emit_declined "changes is unavailable"
      exit 0
    }
    executable=$provider_command
    emit_plan "$scope" '{}' "$executable" -r -root "$scope" -color always
    ;;
  harness:session.bind)
    harness=$(jq -r '.session.harness // empty' <<< "$request")
    if [[ ! -s $agent_registry ]] || ! jq -e --arg harness "$harness" '.agents[] | select(.name == $harness)' "$agent_registry" > /dev/null; then
      emit_declined "harness is absent from the agent registry"
    elif current_session_matches; then
      emit_binding "harness" "active" "$(jq -r '.session.nativeId' <<< "$request")" "$harness session"
    else
      emit_binding "harness" "available" "$(jq -r '.session.nativeId' <<< "$request")" "$harness resume"
    fi
    ;;
  harness:session.attach)
    harness=$(jq -r '.session.harness // empty' <<< "$request")
    agent=$(jq -ce --arg harness "$harness" '.agents[] | select(.name == $harness)' "$agent_registry" 2> /dev/null || true)
    if [[ -z $agent ]] || [[ $(jq -r '.launch.resumeArgs // [] | length' <<< "$agent") -eq 0 ]]; then
      emit_declined "harness does not advertise resume support"
      exit 0
    fi
    harness_command=$(jq -r '.command' <<< "$agent")
    executable=$(command -v "$harness_command" || true)
    if [[ -z $executable ]]; then
      emit_declined "harness command is unavailable"
      exit 0
    fi
    resume_command=("$executable")
    while IFS= read -r -d '' value; do
      resume_command+=("$value")
    done < <(jq -j '.launch.resumeArgs | .[] | @text, "\u0000"' <<< "$agent")
    resume_command+=("$(jq -er '.session.nativeId' <<< "$request")")
    model=$(jq -r '.session.model // empty' <<< "$request")
    model_flag=$(jq -r '.launch.modelFlag // empty' <<< "$agent")
    if [[ -n $model && -n $model_flag ]]; then
      resume_command+=("$model_flag" "$model")
    fi
    emit_plan "$scope" '{}' "${resume_command[@]}"
    ;;
  harness:session.launch)
    if [[ ! -s $agent_registry ]]; then
      emit_declined "agent registry is unavailable"
      exit 0
    fi
    harness=$(jq -r '.session.harness // empty' <<< "$request")
    agent=$(jq -ce --arg harness "$harness" '.agents[] | select(.name == $harness)' "$agent_registry" 2> /dev/null || true)
    if [[ -z $agent ]]; then
      emit_declined "harness is absent from the agent registry"
      exit 0
    fi
    harness_command=$(jq -r '.command' <<< "$agent")
    executable=$(command -v "$harness_command" || true)
    if [[ -z $executable ]]; then
      emit_declined "harness command is unavailable"
      exit 0
    fi
    launch_command=("$executable")
    model=$(jq -r '.session.model // empty' <<< "$request")
    model_flag=$(jq -r '.launch.modelFlag // empty' <<< "$agent")
    if [[ -n $model && -n $model_flag ]]; then
      launch_command+=("$model_flag" "$model")
    fi
    while IFS= read -r -d '' value; do
      launch_command+=("$value")
    done < <(jq -j '.command[1:] | .[] | @text, "\u0000"' <<< "$request")
    environment=$(jq -ce '.environment // {} | objects' <<< "$request")
    emit_plan "$scope" "$environment" "${launch_command[@]}"
    ;;
  local:execution.run)
    read_command '.plan.command'
    prior_cwd=$(jq -er '.plan.cwd // .scope' <<< "$request")
    prior_environment=$(jq -ce '.plan.environment // {} | objects' <<< "$request")
    emit_plan "$prior_cwd" "$prior_environment" "${command[@]}"
    ;;
  traces:session.bind)
    trace_id=$(jq -r '.session.traceId // .session.nativeId // empty' <<< "$request")
    if [[ -n $trace_id ]]; then
      emit_binding "activity" "active" "$trace_id" "Traces activity"
    else
      emit_declined "session has no trace identity"
    fi
    ;;
  traces:session.describe)
    require_command traces || {
      emit_declined "traces is unavailable"
      exit 0
    }
    executable=$provider_command
    trace_id=$(jq -r '.session.traceId // .session.nativeId // empty' <<< "$request")
    harness=$(jq -r '.session.harness // empty' <<< "$request")
    traces_args=(--json --once --session "$trace_id")
    if [[ -n $harness ]]; then
      traces_args+=(--service "$harness")
    fi
    prompt=$(
      "$executable" "${traces_args[@]}" 2> /dev/null |
        jq -n -r 'first(inputs | select(((.attrs.prompt? // "") | type) == "string" and ((.attrs.prompt? // "") | length) > 0) | .attrs.prompt) // empty'
    ) || true
    if [[ -z $prompt ]]; then
      emit_declined "Traces has no user prompt for this session"
    else
      title=${prompt%%$'\n'*}
      emit_description "${title:0:72}" "$prompt"
    fi
    ;;
  traces:session.inspect)
    require_command traces || {
      emit_declined "traces is unavailable"
      exit 0
    }
    executable=$provider_command
    trace_id=$(jq -er '.session.traceId // .session.nativeId | select(type == "string" and length > 0)' <<< "$request")
    harness=$(jq -r '.session.harness // empty' <<< "$request")
    traces_args=(--once -color always -session "$trace_id")
    if [[ -n $harness ]]; then
      traces_args+=(-service "$harness")
    fi
    emit_plan_with_codes '[0, 2]' "$scope" '{}' "$executable" "${traces_args[@]}"
    ;;
  wezterm:session.bind)
    require_command wezterm || {
      emit_declined "wezterm is unavailable"
      exit 0
    }
    executable=$provider_command
    existing_ref=$(jq -r '
      first(
        .session.providers[]?
        | select(.provider == "wezterm" and .kind == "display" and .status == "active")
        | .ref
      ) // empty
    ' <<< "$request")
    rebind_current=$(jq -r '.rebindCurrent // false' <<< "$request")
    existing_pane=false
    if [[ $existing_ref =~ ^[0-9]+$ ]] && "$executable" cli --no-auto-start list --format json 2> /dev/null |
      jq -e --argjson pane "$existing_ref" 'any(.[]; .pane_id == $pane)' > /dev/null; then
      existing_pane=true
    fi
    if [[ $rebind_current == true ]] && current_session_matches && [[ -n ${WEZTERM_PANE:-} ]]; then
      emit_binding "display" "active" "$WEZTERM_PANE" "WezTerm pane $WEZTERM_PANE"
    elif [[ $existing_pane == true ]]; then
      emit_binding "display" "active" "$existing_ref" "WezTerm pane $existing_ref"
    else
      emit_binding "display" "available" "" "WezTerm split"
    fi
    ;;
  wezterm:terminal.focus)
    require_command wezterm || {
      emit_declined "wezterm is unavailable"
      exit 0
    }
    executable=$provider_command
    pane_id=$(jq -er '
      first(
        .session.providers[]?
        | select(.provider == "wezterm" and .kind == "display" and .status == "active")
        | .ref
      ) | select(type == "string" and length > 0)
    ' <<< "$request")
    emit_plan "$scope" '{}' "$executable" cli --no-auto-start activate-pane --pane-id "$pane_id"
    ;;
  wezterm:terminal.open)
    require_command wezterm || {
      emit_declined "wezterm is unavailable"
      exit 0
    }
    executable=$provider_command
    direction=$(jq -er '.direction' <<< "$request")
    case "$direction" in
      right | left | top | bottom) ;;
      *)
        printf 'orc-provider-wezterm: unsupported split direction: %s\n' "$direction" >&2
        exit 2
        ;;
    esac
    prior_cwd=$(jq -er '.plan.cwd // .scope' <<< "$request")
    prior_environment=$(jq -ce '.plan.environment // {} | objects' <<< "$request")
    read_command '.plan.command'
    columns=0
    if [[ -n ${WEZTERM_PANE:-} ]]; then
      columns=$("$executable" cli --no-auto-start list --format json 2> /dev/null |
        jq -r --argjson pane "$WEZTERM_PANE" 'first(.[] | select(.pane_id == $pane) | .size.cols) // 0')
    fi
    if ((columns > 0 && columns < 160)); then
      split_command=("$executable" cli --no-auto-start spawn --cwd "$prior_cwd")
    else
      split_command=("$executable" cli --no-auto-start split-pane "--$direction" --cwd "$prior_cwd")
      if [[ -n ${WEZTERM_PANE:-} ]]; then
        split_command+=(--pane-id "$WEZTERM_PANE")
      fi
    fi
    split_command+=(-- "${ORC_PROVIDER_HOLD:?}" "${command[@]}")
    emit_plan "$scope" "$prior_environment" "${split_command[@]}"
    ;;
  zmx:session.bind)
    existing_ref=$(jq -r '
      first(
        .session.providers[]?
        | select(.provider == "zmx" and .kind == "persistence" and .status == "active")
        | .ref
      ) // empty
    ' <<< "$request")
    rebind_current=$(jq -r '.rebindCurrent // false' <<< "$request")
    existing_session=false
    if [[ -n $existing_ref ]] && command -v zmx > /dev/null &&
      zmx list --short 2> /dev/null | grep -Fx -- "$existing_ref" > /dev/null; then
      existing_session=true
    fi
    if [[ $rebind_current == true ]] && current_session_matches && [[ -n ${ZMX_SESSION:-} ]]; then
      zmx_session=${ZMX_SESSION#"${ZMX_SESSION_PREFIX:-}"}
      emit_binding "persistence" "active" "$zmx_session" "Zmx session $zmx_session"
    elif [[ $existing_session == true ]]; then
      emit_binding "persistence" "active" "$existing_ref" "Zmx session $existing_ref"
    else
      emit_binding "persistence" "available" "" "Zmx on next launch"
    fi
    ;;
  zmx:session.persist)
    require_command zmx || {
      emit_declined "zmx is unavailable"
      exit 0
    }
    executable=$provider_command
    provider_ref=$(jq -r '
      first(
        .session.providers[]?
        | select(.provider == "zmx" and .kind == "persistence" and .status == "active")
        | .ref
      ) // .session.providerRef // empty
    ' <<< "$request")
    if [[ -n $provider_ref ]]; then
      emit_plan "$scope" '{}' "$executable" attach "$provider_ref"
    else
      read_command '.plan.command'
      managed_id=$(jq -r '.session.id' <<< "$request" | sed 's/[^[:alnum:]_.-]/-/g')
      prior_environment=$(jq -ce '.plan.environment // {}' <<< "$request")
      emit_plan "$scope" "$prior_environment" "$executable" attach "$managed_id" "${command[@]}"
    fi
    ;;
  zmx:session.stop)
    require_command zmx || {
      emit_declined "zmx is unavailable"
      exit 0
    }
    executable=$provider_command
    provider_ref=$(jq -r '
      first(
        .session.providers[]?
        | select(.provider == "zmx" and .kind == "persistence" and .ref != null)
        | .ref
      ) // .session.providerRef // empty
    ' <<< "$request")
    if [[ -z $provider_ref ]]; then
      emit_declined "session has no persistent process reference"
      exit 0
    fi
    emit_plan "$scope" '{}' "$executable" kill "$provider_ref"
    ;;
  *)
    printf 'orc-provider-%s: unsupported capability: %s\n' "$provider_kind" "$capability" >&2
    exit 2
    ;;
esac
