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

if [[ ${1:-} == __messages ]]; then
  executable=$2
  native_id=$3
  harness=$4
  traces_args=(--view output --format jsonl --once --session "$native_id")
  if [[ -n $harness ]]; then
    traces_args+=(--service "$harness")
  fi
  traces_args+=(--color never)
  "$executable" "${traces_args[@]}" | jq -c '
    if .version != "traces.message/v1" then
      error("unsupported Traces message version")
    else
      .version = "orc.message/v1"
    end
  '
  exit
fi

validate_traces_contract() {
  local command_ok executable message output status validation validation_dir validation_file
  validation=$(validate_manifest_requirements)
  executable=$(command -v traces || true)
  validation_dir=$(mktemp -d)
  validation_file=$validation_dir/messages.json
  cat >"$validation_file" <<'JSON'
{"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"orc-validation"}},{"key":"session.id","value":{"stringValue":"orc-validation"}}]},"scopeLogs":[{"logRecords":[{"timeUnixNano":"1000000000","eventName":"assistant","body":{"stringValue":"validation message"},"attributes":[{"key":"request_id","value":{"stringValue":"validation-request"}}]}]}]}]}
JSON
  command_ok=false
  if output=$(
    XDG_CONFIG_HOME="$validation_dir/config" "$executable" \
      --file "$validation_file" \
      --view output \
      --format jsonl \
      --once \
      --session orc-validation \
      --service orc-validation \
      --color never 2>/dev/null
  ); then
    command_ok=true
  fi
  if [[ $command_ok == true ]] && jq -e -s '
    length > 0
    and all(.[];
      type == "object"
      and .version == "traces.message/v1"
      and (.id | type == "string" and length > 0)
      and .session == "orc-validation"
      and .timestamp == "1970-01-01T00:00:01Z"
      and .body == "validation message"
    )
  ' <<<"$output" >/dev/null 2>&1; then
    status=ok
    message="traces supports structured message output"
  else
    status=failed
    message="traces does not support --format jsonl"
  fi
  rm -rf -- "$validation_dir"
  jq \
    --arg status "$status" \
    --arg message "$message" \
    '.checks += [{name: "contract:messages-jsonl", status: $status, message: $message}]
     | if $status == "failed" then .status = "failed" else . end' <<< "$validation"
}

provider_program=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)/$(basename -- "${BASH_SOURCE[0]}")
provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

provider_init "traces"

case "$capability" in
  provider.validate)
    validate_traces_contract
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
  messages.read)
    executable=$(command -v traces || true)
    if [[ -z $executable ]]; then
      emit_declined "traces is unavailable"
      exit 0
    fi
    native_id=$(jq -er '.session.nativeId | select(type == "string" and length > 0)' <<< "$request")
    harness=$(jq -r '.session.harness // empty' <<< "$request")
    emit_plan_with_codes '[0]' "$scope" '{}' \
      "$provider_program" __messages "$executable" "$native_id" "$harness"
    ;;
  *) unsupported_capability ;;
esac
