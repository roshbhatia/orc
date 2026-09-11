#!/usr/bin/env bash
set -euo pipefail

provider=${1:?provider path required}
test_root=$(mktemp -d)
trap 'rm -rf "$test_root"' EXIT
export XDG_STATE_HOME="$test_root/state"
export AW_TEST_ROOT="$test_root"
export PATH="$test_root/bin:$PATH"
mkdir -p "$test_root/bin"
printf '{"workflow_runs": []}\n' > "$test_root/runs"
cat > "$test_root/bin/gh" << 'SH'
#!/usr/bin/env bash
set -euo pipefail
case "$1 $2" in
  'api --paginate') cat "$AW_TEST_ROOT/runs" ;;
  'api repos/owner/repo/actions/runs/42') jq '.workflow_runs[0]' "$AW_TEST_ROOT/runs" ;;
  'workflow run') cat > "$AW_TEST_ROOT/inputs"; echo dispatch >> "$AW_TEST_ROOT/dispatches"; exit "${AW_TEST_DISPATCH_EXIT:-0}" ;;
  'run cancel') echo cancel >> "$AW_TEST_ROOT/cancels" ;;
  'run view') echo 'acceptance checks passed' ;;
  *) exit 2 ;;
esac
SH
chmod +x "$test_root/bin/gh"
request='{"version":"orc.provider/v1","scope":"/tmp","resource":{"metadata":{"uid":"factory-test","generation":1},"spec":{"repository":"owner/repo","workflow":"factory.lock.yml","ref":"main","inputs":{"task":"Fix README links"}}}}'

invoke() {
  jq --arg capability "$1" '. + {capability: $capability}' <<< "$request" | "$provider" |
    jq -e 'if .version == "orc.provider/v1" and (.status | type == "string") then . else error("invalid observation envelope") end'
}

invoke execution.ensure | jq -e '.status == "Pending"' > /dev/null
invoke execution.ensure | jq -e '.status == "Pending"' > /dev/null
test "$(wc -l < "$test_root/dispatches" | tr -d ' ')" = 1
jq -e '.orc_operation_id == "factory-test-1" and .task == "Fix README links"' "$test_root/inputs" > /dev/null
printf '%s\n' '{"workflow_runs":[{"id":42,"display_title":"orc:factory-test-1","status":"in_progress","html_url":"https://github.com/owner/repo/actions/runs/42","head_sha":"abc"}]}' > "$test_root/runs"
invoke execution.observe | jq -e '.status == "Running" and .outputs.run.id == 42' > /dev/null
request=$(jq '.resource.metadata.generation = 2 | .resource.status = {observedGeneration: 1, externalRef: "https://github.com/owner/repo/actions/runs/42"}' <<< "$request")
invoke execution.cancel | jq -e '.status == "Running"' > /dev/null
request=$(jq '.resource.status.observedGeneration = 2' <<< "$request")
invoke execution.observe | jq -e '.status == "Running"' > /dev/null
test "$(wc -l < "$test_root/cancels" | tr -d ' ')" = 1
jq '.workflow_runs[0] += {status: "completed", conclusion: "cancelled"}' "$test_root/runs" > "$test_root/next"
mv "$test_root/next" "$test_root/runs"
invoke execution.observe | jq -e '.status == "Cancelled"' > /dev/null
invoke execution.logs | jq -e '.logs == "acceptance checks passed"' > /dev/null
jq '.workflow_runs[0].conclusion = "success"' "$test_root/runs" > "$test_root/next"
mv "$test_root/next" "$test_root/runs"
invoke execution.observe | jq -e '.status == "Succeeded"' > /dev/null
test "$(wc -l < "$test_root/dispatches" | tr -d ' ')" = 1

request=$(jq '.resource.metadata.generation = 3 | del(.resource.status)' <<< "$request")
export AW_TEST_DISPATCH_EXIT=1
invoke execution.ensure | jq -e '.status == "Pending" and (.message | contains("error"))' > /dev/null
invoke execution.ensure | jq -e '.status == "Pending"' > /dev/null
test "$(wc -l < "$test_root/dispatches" | tr -d ' ')" = 2
printf 'GitHub AW lifecycle and ambiguous-dispatch checks passed\n'
