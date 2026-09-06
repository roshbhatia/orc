#!/usr/bin/env bash
set -euo pipefail

if [[ ${1:-} == __activity ]]; then
  executable=$2
  trace_id=$3
  harness=$4
  max_bytes=$5
  max_lines=$6
  traces_args=(--once -color always -session "$trace_id")
  if [[ -n $harness ]]; then
    traces_args+=(-service "$harness")
  fi
  activity_tail=$(mktemp)
  trap 'rm -f "$activity_tail"' EXIT
  probe_bytes=$((max_bytes + 1))
  set +e
  "$executable" "${traces_args[@]}" | tail -c "$probe_bytes" > "$activity_tail"
  pipeline_status=("${PIPESTATUS[@]}")
  set -e
  if ((pipeline_status[1] != 0)); then
    exit "${pipeline_status[1]}"
  fi

  byte_count=$(wc -c < "$activity_tail")
  line_count=$(wc -l < "$activity_tail")
  if [[ -s $activity_tail ]]; then
    last_byte=$(tail -c 1 "$activity_tail" | od -An -tu1 | tr -d ' ')
    if [[ $last_byte != 10 ]]; then
      line_count=$((line_count + 1))
    fi
  fi
  truncated=false
  if ((byte_count > max_bytes || line_count > max_lines)); then
    truncated=true
  fi
  if [[ $truncated == true ]]; then
    marker='… earlier activity omitted'
    data_bytes=$((max_bytes - ${#marker} - 1))
    printf '%s\n' "$marker"
    tail -c "$data_bytes" "$activity_tail" | tail -n "$max_lines"
  else
    cat "$activity_tail"
  fi
  exit "${pipeline_status[0]}"
fi

provider_program=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/$(basename -- "${BASH_SOURCE[0]}")
provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

provider_init "traces"

case "$capability" in
  provider.validate)
    validate_manifest_requirements
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
    max_bytes=$(jq -r '(.maxBytes // 262144) | if type == "number" and . >= 1024 and . <= 1048576 then floor else 262144 end' <<< "$request")
    max_lines=$(jq -r '(.maxLines // 100) | if type == "number" and . >= 1 and . <= 10000 then floor else 100 end' <<< "$request")
    emit_plan_with_codes '[0, 2]' "$scope" '{}' \
      "$provider_program" __activity "$executable" "$trace_id" "$harness" "$max_bytes" "$max_lines"
    ;;
  *) unsupported_capability ;;
esac
