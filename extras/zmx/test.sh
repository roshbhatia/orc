#!/usr/bin/env bash
set -euo pipefail

provider_script=${ORC_PROVIDER_ZMX_SCRIPT:?}
provider_library=${ORC_PROVIDER_LIB:?}
test_scope=${TMPDIR:?}/zmx-provider-test
fake_bin=$test_scope/bin
mkdir -p "$fake_bin"

cat > "$fake_bin/zmx" << 'SCRIPT'
#!/usr/bin/env bash
set -euo pipefail
case ${1:-} in
  list)
    if [[ ${ZMX_TEST_LIST_FAILURE:-false} == true ]]; then
      exit 23
    fi
    printf 'name=demo\tpid=123\tcreated=456\n'
    ;;
  kill)
    printf '%s\n' "${2:?}" > "${ZMX_TEST_KILL_LOG:?}"
    ;;
  *) exit 2 ;;
esac
SCRIPT
chmod +x "$fake_bin/zmx"

launch_request=$(jq -n \
  --arg scope "$test_scope" \
  '{
    version: "orc.provider/v1",
    action: "launch",
    capability: "session.persist",
    scope: $scope,
    plan: {
      version: "orc.provider/v1",
      command: ["harness", "--flag"],
      environment: {BASE: "value"},
      successCodes: [0]
    },
    session: {
      id: "session-1",
      providerRef: "provisional",
      providers: [{
        provider: "zmx",
        kind: "persistence",
        status: "active",
        ref: "session-1",
        label: "Launch ownership: Zmx"
      }]
    }
  }')
PATH="$fake_bin:$PATH" ORC_PROVIDER_LIB="$provider_library" \
  bash "$provider_script" <<< "$launch_request" > "$test_scope/launch-plan.json"
jq -e --arg zmx "$fake_bin/zmx" '
  .command == [$zmx, "attach", "session-1", "harness", "--flag"]
  and .environment == {BASE: "value"}
' "$test_scope/launch-plan.json" > /dev/null

request=$(jq -n \
  --arg scope "$test_scope" \
  '{
    version: "orc.provider/v1",
    capability: "session.stop",
    operationId: "operation-1",
    scope: $scope,
    session: {
      id: "session-1",
      providerRef: "demo@123@456",
      providers: []
    }
  }')
plan=$test_scope/plan.json
PATH="$fake_bin:$PATH" \
  ORC_PROVIDER_LIB="$provider_library" \
  ORC_PROVIDER_SELF=/stable/orc-provider-zmx \
  XDG_STATE_HOME="$test_scope/state" \
  bash "$provider_script" <<< "$request" > "$plan"
jq -e '
  .command == ["/stable/orc-provider-zmx", "stop", "demo", "123", "456", "operation-1"]
' "$plan" > /dev/null

kill_log=$test_scope/killed
PATH="$fake_bin:$PATH" ZMX_TEST_KILL_LOG="$kill_log" \
  bash "$provider_script" stop demo 123 456 operation-1
grep -Fxq demo "$kill_log"

set +e
PATH="$fake_bin:$PATH" ZMX_TEST_LIST_FAILURE=true ZMX_TEST_KILL_LOG="$kill_log" \
  bash "$provider_script" stop demo 123 456 operation-1
failure_code=$?
set -e
test "$failure_code" -eq 2

bind_request=$(jq -n \
  --arg scope "$test_scope" \
  '{
    version: "orc.provider/v1",
    capability: "session.bind",
    scope: $scope,
    rebindCurrent: false,
    session: {id: "demo", providers: []}
  }')
set +e
PATH="$fake_bin:$PATH" \
  ORC_PROVIDER_LIB="$provider_library" \
  ZMX_TEST_LIST_FAILURE=true \
  bash "$provider_script" <<< "$bind_request" > /dev/null
failure_code=$?
set -e
test "$failure_code" -eq 2

legacy_request=$(jq '.session.providerRef = "demo" | .operationId = "operation-2"' <<< "$request")
set +e
PATH="$fake_bin:$PATH" \
  ORC_PROVIDER_LIB="$provider_library" \
  XDG_STATE_HOME="$test_scope/legacy-state" \
  ZMX_TEST_LIST_FAILURE=true \
  bash "$provider_script" <<< "$legacy_request" > /dev/null
failure_code=$?
set -e
test "$failure_code" -eq 2
