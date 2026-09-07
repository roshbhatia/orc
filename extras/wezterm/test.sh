#!/usr/bin/env bash
set -euo pipefail

provider_script=${ORC_PROVIDER_WEZTERM_SCRIPT:?}
provider_library=${ORC_PROVIDER_LIB:?}
expect_script=${ORC_PROVIDER_WEZTERM_EXPECT:?}
packaged_expect_script=${ORC_PROVIDER_WEZTERM_PACKAGED_EXPECT:?}
packaged_provider=${ORC_PROVIDER_WEZTERM_PACKAGED:?}
real_sleep=${ORC_PROVIDER_WEZTERM_REAL_SLEEP:?}
test_scope=${TMPDIR:?}/wezterm-provider-test
mkdir -p "$test_scope"

bind_request() {
  jq -n \
    --arg scope "$test_scope" \
    '{
      version: "orc.provider/v1",
      capability: "session.bind",
      scope: $scope,
      rebindCurrent: true,
      currentSessionId: "orc-session",
      session: {
        id: "orc-session",
        harness: "codex",
        nativeId: "native-session",
        providers: []
      }
    }'
}

assert_bind_status() {
  local expected_ref output
  expected_ref=$1
  output=$2
  if [[ -n $expected_ref ]]; then
    jq -e --arg ref "$expected_ref" '
      .binding.kind == "display"
      and .binding.status == "active"
      and .binding.ref == $ref
    ' <<< "$output" > /dev/null
  else
    jq -e '
      .binding.kind == "display"
      and .binding.status == "available"
      and .binding.ref == null
    ' <<< "$output" > /dev/null
  fi
}

bind_with_tty() {
  local clients panes tty
  clients=$1
  panes=$2
  tty=$3
  bind_request |
    env -u WEZTERM_PANE \
      ORC_PROVIDER_LIB="$provider_library" \
      ORC_HARNESS=codex \
      ORC_TEST_PS_TTY="$tty" \
      ORC_TEST_WEZTERM_CLIENTS="$clients" \
      ORC_TEST_WEZTERM_PANES="$panes" \
      bash "$provider_script"
}

live_clients='[{"pid":123}]'
exact_panes='[{"pane_id":73,"tty_name":"/dev/ttys007"}]'
assert_bind_status 73 "$(bind_with_tty "$live_clients" "$exact_panes" ttys007)"

exact_id_output=$(bind_request |
  env -u ORC_HARNESS -u ORC_NATIVE_SESSION_ID -u WEZTERM_PANE \
    ORC_PROVIDER_LIB="$provider_library" \
    ORC_TEST_PS_TTY=ttys007 \
    ORC_TEST_WEZTERM_CLIENTS="$live_clients" \
    ORC_TEST_WEZTERM_PANES="$exact_panes" \
    bash "$provider_script")
assert_bind_status 73 "$exact_id_output"

other_panes='[{"pane_id":73,"tty_name":"/dev/ttys008"}]'
assert_bind_status '' "$(bind_with_tty "$live_clients" "$other_panes" ttys007)"

ambiguous_panes='[
  {"pane_id":73,"tty_name":"/dev/ttys007"},
  {"pane_id":74,"tty_name":"/dev/ttys007"}
]'
assert_bind_status '' "$(bind_with_tty "$live_clients" "$ambiguous_panes" ttys007)"
assert_bind_status '' "$(bind_with_tty '[]' "$exact_panes" ttys007)"

mismatched_request=$(bind_request | jq '.currentSessionId = "other-session"')
mismatched_output=$(printf '%s\n' "$mismatched_request" |
  env -u ORC_NATIVE_SESSION_ID -u WEZTERM_PANE \
    ORC_PROVIDER_LIB="$provider_library" \
    ORC_HARNESS=codex \
    ORC_TEST_PS_TTY=ttys007 \
    ORC_TEST_WEZTERM_CLIENTS="$live_clients" \
    ORC_TEST_WEZTERM_PANES="$exact_panes" \
    bash "$provider_script")
assert_bind_status '' "$mismatched_output"

harness_mismatched_output=$(bind_request |
  env -u ORC_NATIVE_SESSION_ID -u WEZTERM_PANE \
    ORC_PROVIDER_LIB="$provider_library" \
    ORC_HARNESS=claude \
    ORC_TEST_PS_TTY=ttys007 \
    ORC_TEST_WEZTERM_CLIENTS="$live_clients" \
    ORC_TEST_WEZTERM_PANES="$exact_panes" \
    bash "$provider_script")
assert_bind_status '' "$harness_mismatched_output"

bind_request | jq 'del(.currentSessionId)' |
  ORC_NATIVE_SESSION_ID=native-session \
    ORC_HARNESS=codex \
    ORC_PROVIDER_LIB="$provider_library" \
    ORC_TEST_PS_TTY=ttys007 \
    ORC_TEST_WEZTERM_CLIENTS="$live_clients" \
    ORC_TEST_WEZTERM_PANES='[{"pane_id":42,"tty_name":"/dev/ttys008"}]' \
    WEZTERM_PANE=42 \
    bash "$provider_script" > "$test_scope/environment-pane.json"
assert_bind_status 42 "$(< "$test_scope/environment-pane.json")"

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
      | .receipt == {type: "providerBinding", provider: "wezterm"}
      and $command[0] == $provider
      and $command[1] == "open"
      and $command[3:6] == ["cli", "--no-auto-start", "split-pane"]
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
  WEZTERM_PANE=42 "$packaged_provider" > "$test_scope/packaged.json"
packaged_command=$(jq -er \
  --arg provider "$packaged_provider" \
  '
    .command as $command
    | ($command | index("--")) as $separator
    | select(.receipt == {type: "providerBinding", provider: "wezterm"})
    | select($command[0] == $provider and $command[1] == "open")
    | select($command[$separator + 3] == $provider)
    | select($command[$separator + 4] == "hold")
    | $provider
  ' "$test_scope/packaged.json")
expect "$packaged_expect_script" "$(command -v env)" "$packaged_command"

request right |
  env -u WEZTERM_PANE ORC_PROVIDER_LIB="$provider_library" bash "$provider_script" > "$test_scope/outside.json"
jq -e '
  .receipt == {type: "providerBinding", provider: "wezterm"}
  and .command[1] == "open"
  and .command[3:6] == ["cli", "--no-auto-start", "spawn"]
  and (.command | index("split-pane")) == null
' "$test_scope/outside.json" > /dev/null

open_receipt=$(ORC_PROVIDER_LIB="$provider_library" \
  bash "$provider_script" open sh -c 'printf 88')
jq -e '
  .binding.kind == "display"
  and .binding.status == "active"
  and .binding.ref == "88"
  and .binding.label == "WezTerm pane 88"
' <<< "$open_receipt" > /dev/null

printf '\n' | bash "$provider_script" hold true > "$test_scope/short-success.txt"
grep -Fq 'Command exited with 0. Press Enter to close.' \
  "$test_scope/short-success.txt"

bash "$provider_script" hold false < /dev/null > "$test_scope/noninteractive-failure.txt" 2>&1 ||
  noninteractive_failure_code=$?
test "${noninteractive_failure_code:-0}" -eq 1
grep -Fq 'Command exited with 1. Press Enter to close.' \
  "$test_scope/noninteractive-failure.txt"

pipe_scope=$(mktemp -d "$test_scope/open-pipe.XXXXXX")
open_pipe=$pipe_scope/input
pipe_release=$pipe_scope/release
provider_done=$pipe_scope/done
mkfifo "$open_pipe"
(
  exec 3> "$open_pipe"
  while [[ ! -e $pipe_release ]]; do
    sleep 0.05
  done
) &
pipe_writer=$!
(
  set +e
  bash "$provider_script" hold false < "$open_pipe" > "$pipe_scope/failure.txt" 2>&1
  printf '%s\n' "$?" > "$pipe_scope/code"
  touch "$provider_done"
) &
provider_process=$!
for _ in {1..20}; do
  [[ -e $provider_done ]] && break
  sleep 0.05
done
noninteractive_blocked=true
[[ -e $provider_done ]] && noninteractive_blocked=false
touch "$pipe_release"
wait "$pipe_writer"
wait "$provider_process"
[[ $noninteractive_blocked == false ]]
test "$(< "$pipe_scope/code")" -eq 1

for duration in 0.1 2.5; do
  for status in 0 19; do
    for _ in 1 2 3; do
      expect "$expect_script" "$provider_script" hold "$status" "$duration" "$real_sleep"
    done
  done
done
expect "$expect_script" "$provider_script" run 0 0.1 "$real_sleep"
expect "$expect_script" "$provider_script" run 19 0.1 "$real_sleep"

set +e
printf '\n' | bash "$provider_script" hold false > "$test_scope/failure.txt"
failure_code=$?
set -e
test "$failure_code" -eq 1
grep -Fq 'Command exited with 1. Press Enter to close.' "$test_scope/failure.txt"
