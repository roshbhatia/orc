#!/usr/bin/env bash
set -euo pipefail

provider_script=${ORC_PROVIDER_WEZTERM_SCRIPT:?}
provider_library=${ORC_PROVIDER_LIB:?}
test_scope=${TMPDIR:?}/wezterm-provider-test
mkdir -p "$test_scope"

request() {
  local direction
  direction=$1
  jq -n \
    --arg direction "$direction" \
    --arg scope "$test_scope" \
    '{
      version: "orc.provider/v1",
      capability: "terminal.open",
      scope: $scope,
      direction: $direction,
      plan: {
        version: "orc.provider/v1",
        command: ["printenv", "ORC_COMPOSED_TEST"],
        cwd: $scope,
        environment: {ORC_COMPOSED_TEST: "preserved"},
        successCodes: [0]
      }
    }'
}

for direction in right left top bottom; do
  plan="$test_scope/$direction.json"
  request "$direction" |
    ORC_PROVIDER_LIB="$provider_library" WEZTERM_PANE=42 bash "$provider_script" > "$plan"
  jq -e \
    --arg direction "--$direction" \
    --arg provider "$provider_script" \
    '
      .command as $command
      | ($command | index("--")) as $separator
      | $command[1:4] == ["cli", "--no-auto-start", "split-pane"]
      and ($command | index($direction)) != null
      and ($command | index("--pane-id")) != null
      and ($command | index("42")) != null
      and ($command | index("spawn")) == null
      and .environment.ORC_COMPOSED_TEST == "preserved"
      and ($command[$separator + 1] | endswith("/env"))
      and $command[$separator + 2] == "ORC_COMPOSED_TEST=preserved"
      and $command[$separator + 3] == $provider
      and $command[$separator + 4] == "hold"
      and $command[$separator + 5:] == ["printenv", "ORC_COMPOSED_TEST"]
    ' "$plan" > /dev/null
done

request right |
  env -u WEZTERM_PANE ORC_PROVIDER_LIB="$provider_library" bash "$provider_script" > "$test_scope/outside.json"
jq -e '
  .command[1:4] == ["cli", "--no-auto-start", "spawn"]
  and (.command | index("split-pane")) == null
' "$test_scope/outside.json" > /dev/null

printf '\n' | bash "$provider_script" hold true > "$test_scope/short-success.txt"
grep -Fq 'Command exited before an interactive session was ready. Press Enter to close.' \
  "$test_scope/short-success.txt"

bash "$provider_script" hold sleep 3 > "$test_scope/long-success.txt"
test ! -s "$test_scope/long-success.txt"

set +e
printf '\n' | bash "$provider_script" hold false > "$test_scope/failure.txt"
failure_code=$?
set -e
test "$failure_code" -eq 1
grep -Fq 'Command exited with 1. Press Enter to close.' "$test_scope/failure.txt"
