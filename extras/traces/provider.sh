#!/usr/bin/env bash
set -euo pipefail

provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

provider_init "traces"

case "$capability" in
  provider.validate)
    validate_dependency "traces"
    ;;
  session.bind)
    trace_id=$(jq -r '.session.traceId // .session.nativeId // empty' <<< "$request")
    if [[ -n $trace_id ]]; then
      emit_binding "activity" "active" "$trace_id" "Traces activity"
    else
      emit_declined "session has no trace identity"
    fi
    ;;
  session.describe)
    executable=$(command -v traces || true)
    if [[ -z $executable ]]; then
      emit_declined "traces is unavailable"
      exit 0
    fi
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
  activity.read | session.inspect)
    executable=$(command -v traces || true)
    if [[ -z $executable ]]; then
      emit_declined "traces is unavailable"
      exit 0
    fi
    trace_id=$(jq -er '.session.traceId // .session.nativeId | select(type == "string" and length > 0)' <<< "$request")
    harness=$(jq -r '.session.harness // empty' <<< "$request")
    traces_args=(--once -color always -session "$trace_id")
    if [[ -n $harness ]]; then
      traces_args+=(-service "$harness")
    fi
    emit_plan_with_codes '[0, 2]' "$scope" '{}' "$executable" "${traces_args[@]}"
    ;;
  *) unsupported_capability ;;
esac
