#!/usr/bin/env bash
set -euo pipefail

provider_script=${ORC_PROVIDER_HARNESS_SCRIPT:?}
provider_library=${ORC_PROVIDER_LIB:?}
test_scope=${TMPDIR:?}/harness-provider-test
default_registry=$test_scope/config/sysinit/agents.json
explicit_registry=$test_scope/agents.json
mkdir -p "$(dirname "$default_registry")"

registry='{"agents":[{"name":"test","command":"true","launch":{"resumeArgs":["resume"]}}]}'
printf '%s\n' "$registry" > "$default_registry"
printf '%s\n' "$registry" > "$explicit_registry"

request=$(jq -n --arg scope "$test_scope" '{
  version: "orc.provider/v1",
  action: "validate",
  capability: "provider.validate",
  scope: $scope,
  manifest: {
    name: "harness",
    kind: "harness",
    actions: {"provider.validate": "Validate the registry"},
    requires: {environment: ["ORC_AGENT_REGISTRY"]}
  }
}')

XDG_CONFIG_HOME="$test_scope/config" ORC_PROVIDER_LIB="$provider_library" \
  bash "$provider_script" <<< "$request" > "$test_scope/missing.json"
jq -e '
  .checks == [{
    name: "registry",
    status: "failed",
    message: "ORC_AGENT_REGISTRY is required"
  }]
' "$test_scope/missing.json" > /dev/null

ORC_AGENT_REGISTRY="$explicit_registry" ORC_PROVIDER_LIB="$provider_library" \
  bash "$provider_script" <<< "$request" > "$test_scope/explicit.json"
jq -e '
  .checks == [{
    name: "harnesses",
    status: "ok",
    message: "1 registered harnesses have valid resume commands"
  }]
' "$test_scope/explicit.json" > /dev/null
