#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

provider_init() {
  local expected_name
  expected_name=$1

  adapter_name=$expected_name
  request=$(< /dev/stdin)
  version=$(jq -er '.version' <<< "$request")
  capability=$(jq -er '.capability' <<< "$request")
  scope=$(jq -er '.scope' <<< "$request")
  : "$scope"

  if [[ $version != "orc.provider/v1" ]]; then
    printf 'orc-provider-%s: unsupported request version: %s\n' "$adapter_name" "$version" >&2
    exit 2
  fi
}

emit_plan_with_codes() {
  local success_codes cwd environment command_json
  success_codes=$1
  cwd=$2
  environment=$3
  shift 3
  if (($# == 0)); then
    printf 'orc-provider-%s: command plan is empty\n' "$adapter_name" >&2
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

validate_dependency() {
  local command_name executable
  command_name=$1
  executable=$(command -v "$command_name" || true)
  if [[ -z $executable ]]; then
    emit_validation "failed" "dependency" "$command_name is unavailable"
  else
    emit_validation "ok" "dependency" "$command_name is available at $executable"
  fi
}

read_command() {
  local selector output_name value
  selector=$1
  output_name=$2
  local -n output=$output_name

  output=()
  while IFS= read -r -d '' value; do
    output+=("$value")
  done < <(jq -j "$selector | .[] | @text, \"\u0000\"" <<< "$request")
  if ((${#output[@]} == 0)); then
    printf 'orc-provider-%s: input command is empty\n' "$adapter_name" >&2
    exit 2
  fi
}

current_session_matches() {
  local native_id
  native_id=$(jq -r '.session.nativeId // empty' <<< "$request")
  [[ -n $native_id && -n ${ORC_NATIVE_SESSION_ID:-} && $ORC_NATIVE_SESSION_ID == "$native_id" ]]
}

unsupported_capability() {
  printf 'orc-provider-%s: unsupported capability: %s\n' "$adapter_name" "$capability" >&2
  exit 2
}
