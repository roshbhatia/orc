#!/usr/bin/env bash
set -euo pipefail

provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

if [[ ${1:-} == hold ]]; then
  shift
  started_at=$SECONDS
  set +e
  "$@"
  code=$?
  set -e
  if ((code != 0)); then
    printf '\nCommand exited with %s. Press Enter to close.\n' "$code"
    IFS= read -r _ || true
  elif ((SECONDS - started_at < 2)); then
    printf '\nCommand exited before an interactive session was ready. Press Enter to close.\n'
    IFS= read -r _ || true
  fi
  exit "$code"
fi

provider_init "wezterm"

case "$capability" in
  provider.validate)
    validate_manifest_requirements
    ;;
  session.bind)
    executable=$(command -v wezterm || true)
    if [[ -z $executable ]]; then
      emit_declined "wezterm is unavailable"
      exit 0
    fi
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
  terminal.focus)
    executable=$(command -v wezterm || true)
    if [[ -z $executable ]]; then
      emit_declined "wezterm is unavailable"
      exit 0
    fi
    pane_id=$(jq -er '
      first(
        .session.providers[]?
        | select(.provider == "wezterm" and .kind == "display" and .status == "active")
        | .ref
      ) | select(type == "string" and length > 0)
    ' <<< "$request")
    emit_plan "$scope" '{}' "$executable" cli --no-auto-start activate-pane --pane-id "$pane_id"
    ;;
  terminal.open)
    executable=$(command -v wezterm || true)
    if [[ -z $executable ]]; then
      emit_declined "wezterm is unavailable"
      exit 0
    fi
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
    plan_command=()
    read_command '.plan.command' plan_command
    session_command=("$(command -v env)")
    while IFS= read -r -d '' assignment; do
      session_command+=("$assignment")
    done < <(
      jq -j '
        .plan.environment // {}
        | to_entries[]
        | (.key + "=" + (.value | tostring)), "\u0000"
      ' <<< "$request"
    )
    session_command+=("$0" hold "${plan_command[@]}")
    if [[ -n ${WEZTERM_PANE:-} ]]; then
      split_command=("$executable" cli --no-auto-start split-pane "--$direction" --cwd "$prior_cwd")
      split_command+=(--pane-id "$WEZTERM_PANE")
    else
      split_command=("$executable" cli --no-auto-start spawn --cwd "$prior_cwd")
    fi
    split_command+=(-- "${session_command[@]}")
    emit_plan "$scope" "$prior_environment" "${split_command[@]}"
    ;;
  *) unsupported_capability ;;
esac
