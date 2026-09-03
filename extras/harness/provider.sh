#!/usr/bin/env bash
set -euo pipefail

provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

provider_init "harness"
agent_registry=${ORC_AGENT_REGISTRY:-${XDG_CONFIG_HOME:-$HOME/.config}/sysinit/agents.json}

case "$capability" in
  provider.validate)
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
    ;;
  session.bind)
    harness=$(jq -r '.session.harness // empty' <<< "$request")
    if [[ ! -s $agent_registry ]] || ! jq -e --arg harness "$harness" '.agents[] | select(.name == $harness)' "$agent_registry" > /dev/null; then
      emit_declined "harness is absent from the agent registry"
    elif current_session_matches; then
      emit_binding "harness" "active" "$(jq -r '.session.nativeId' <<< "$request")" "$harness session"
    else
      emit_binding "harness" "available" "$(jq -r '.session.nativeId' <<< "$request")" "$harness resume"
    fi
    ;;
  session.attach)
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
  session.launch)
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
  *) unsupported_capability ;;
esac
