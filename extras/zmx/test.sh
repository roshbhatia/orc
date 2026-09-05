#!/usr/bin/env bash
set -euo pipefail

provider_script=${ORC_PROVIDER_ZMX_SCRIPT:?}
provider_library=${ORC_PROVIDER_LIB:?}
test_scope=${TMPDIR:?}/zmx-provider-test
fake_bin=$test_scope/bin
mkdir -p "$fake_bin"

printf '#!%s\n' "${ORC_TEST_BASH:?}" >"$fake_bin/orc-provider-zmx-process-tree"
cat >>"$fake_bin/orc-provider-zmx-process-tree" <<'SCRIPT'
set -euo pipefail
if [[ ${1:-} == inspect ]]; then
  printf '123.500000000\n'
else
  printf '%s\n' "$@" > "${ZMX_TEST_TREE_LOG:?}"
fi
SCRIPT
chmod +x "$fake_bin/orc-provider-zmx-process-tree"

printf '#!%s\n' "${ORC_TEST_BASH:?}" >"$fake_bin/zmx"
cat >>"$fake_bin/zmx" <<'SCRIPT'
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
  attach)
    printf '%s\n' "${ZMX_SESSION_PREFIX-unset}" > "${ZMX_TEST_PREFIX_LOG:?}"
    printf '%s\n' "${@:2}" > "${ZMX_TEST_ATTACH_LOG:?}"
    ;;
  *) exit 2 ;;
esac
SCRIPT
chmod +x "$fake_bin/zmx"
test "$(head -n 1 "$fake_bin/zmx")" = "#!$ORC_TEST_BASH"

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
  ZMX_SESSION_PREFIX=seshy- \
  bash "$provider_script" <<<"$launch_request" >"$test_scope/launch-plan.json"
jq -e --arg zmx "$fake_bin/zmx" '
  .command == ["env", "ZMX_SESSION_PREFIX=", $zmx, "attach", "session-1", "harness", "--flag"]
  and .environment == {BASE: "value"}
' "$test_scope/launch-plan.json" >/dev/null

prefix_log=$test_scope/prefix
attach_log=$test_scope/attached
mapfile -t launch_command < <(jq -r '.command[]' "$test_scope/launch-plan.json")
PATH="$fake_bin:$PATH" \
  ZMX_SESSION_PREFIX=seshy- \
  ZMX_TEST_PREFIX_LOG="$prefix_log" \
  ZMX_TEST_ATTACH_LOG="$attach_log" \
  "${launch_command[@]}"
test -z "$(<"$prefix_log")"
printf '%s\n' session-1 harness --flag | diff -u - "$attach_log"

attach_request=$(jq '
  .action = "attach"
  | .session.providers[0].ref = "explicit-ref@123@456"
' <<<"$launch_request")
PATH="$fake_bin:$PATH" ORC_PROVIDER_LIB="$provider_library" \
  ZMX_SESSION_PREFIX=seshy- \
  bash "$provider_script" <<<"$attach_request" >"$test_scope/attach-plan.json"
jq -e --arg zmx "$fake_bin/zmx" '
  .command == ["env", "ZMX_SESSION_PREFIX=", $zmx, "attach", "explicit-ref"]
' "$test_scope/attach-plan.json" >/dev/null

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
  bash "$provider_script" <<<"$request" >"$plan"
jq -e '
  .command == ["/stable/orc-provider-zmx", "stop", "demo", "123", "456", "123.500000000", "operation-1"]
' "$plan" >/dev/null

preflight=$(jq '.preflight = true' <<<"$request")
preflight_state=$test_scope/preflight-state
PATH="$fake_bin:$PATH" \
  ORC_PROVIDER_LIB="$provider_library" \
  XDG_STATE_HOME="$preflight_state" \
  bash "$provider_script" <<<"$preflight" >"$test_scope/preflight.json"
jq -e '.command == ["true"]' "$test_scope/preflight.json" >/dev/null
test ! -e "$preflight_state"

kill_log=$test_scope/killed
tree_log=$test_scope/tree
PATH="$fake_bin:$PATH" ZMX_TEST_KILL_LOG="$kill_log" ZMX_TEST_TREE_LOG="$tree_log" \
  bash "$provider_script" stop demo 123 456 123.500000000 operation-1
printf '%s\n' stop --zmx "$fake_bin/zmx" --name demo --pid 123 --created 456 --identity 123.500000000 | diff -u - "$tree_log"

set +e
PATH="$fake_bin:$PATH" ZMX_TEST_LIST_FAILURE=true ZMX_TEST_KILL_LOG="$kill_log" \
  bash "$provider_script" stop demo 123 456 123.500000000 operation-1
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
PATH="$fake_bin:$PATH" \
  ORC_PROVIDER_LIB="$provider_library" \
  bash "$provider_script" <<<"$bind_request" >"$test_scope/bind.json"
jq -e '.binding.ref == "demo@123@456@123.500000000"' "$test_scope/bind.json" >/dev/null

set +e
PATH="$fake_bin:$PATH" \
  ORC_PROVIDER_LIB="$provider_library" \
  ZMX_TEST_LIST_FAILURE=true \
  bash "$provider_script" <<<"$bind_request" >/dev/null
failure_code=$?
set -e
test "$failure_code" -eq 2

legacy_request=$(jq '.session.providerRef = "demo" | .operationId = "operation-2"' <<<"$request")
set +e
PATH="$fake_bin:$PATH" \
  ORC_PROVIDER_LIB="$provider_library" \
  XDG_STATE_HOME="$test_scope/legacy-state" \
  ZMX_TEST_LIST_FAILURE=true \
  bash "$provider_script" <<<"$legacy_request" >/dev/null
failure_code=$?
set -e
test "$failure_code" -eq 2
