#!/usr/bin/env bash
set -euo pipefail

provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

provider_init "local"

emit_resource_action() {
  local action command_cwd command_environment operation_id value
  local -a plan_command=()
  action=$(jq -c --arg capability "$capability" \
    '.resource.spec.actions[$capability] // {} | objects' <<< "$request")
  if [[ $capability == execution.ensure && $action == '{}' ]]; then
    action=$(jq -c '.resource.spec | {command, cwd, environment}' <<< "$request")
  fi
  if ! jq -e '.command | arrays and length > 0' <<< "$action" > /dev/null; then
    emit_declined "resource does not define a $capability command"
    return
  fi
  while IFS= read -r -d '' value; do
    plan_command+=("$value")
  done < <(jq -j '.command | .[] | @text, "\u0000"' <<< "$action")
  command_cwd=$(jq -r --arg scope "$scope" '.cwd // $scope' <<< "$action")
  operation_id=$(jq -er '.operationId' <<< "$request")
  command_environment=$(jq -ce --arg operation_id "$operation_id" \
    '(.environment // {} | objects) + {ORC_OPERATION_ID: $operation_id}' <<< "$action")
  emit_plan "$command_cwd" "$command_environment" "${plan_command[@]}"
}

case "$capability" in
  provider.validate)
    validate_manifest_requirements
    ;;
  execution.run)
    plan_command=()
    read_command '.plan.command' plan_command
    prior_cwd=$(jq -er '.plan.cwd // .scope' <<< "$request")
    prior_environment=$(jq -ce '.plan.environment // {} | objects' <<< "$request")
    emit_plan "$prior_cwd" "$prior_environment" "${plan_command[@]}"
    ;;
  execution.ensure | execution.observe | execution.cancel | execution.logs | session.observe | event.deliver)
    emit_resource_action
    ;;
  *) unsupported_capability ;;
esac
