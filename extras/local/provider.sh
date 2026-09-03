#!/usr/bin/env bash
set -euo pipefail

provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

provider_init "local"

case "$capability" in
  provider.validate)
    emit_validation "ok" "runtime" "local process execution is available"
    ;;
  execution.run)
    plan_command=()
    read_command '.plan.command' plan_command
    prior_cwd=$(jq -er '.plan.cwd // .scope' <<< "$request")
    prior_environment=$(jq -ce '.plan.environment // {} | objects' <<< "$request")
    emit_plan "$prior_cwd" "$prior_environment" "${plan_command[@]}"
    ;;
  *) unsupported_capability ;;
esac
