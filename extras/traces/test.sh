#!/usr/bin/env bash
set -euo pipefail

provider=$1
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

fake_traces="$fixture/traces"
printf '#!%s\n' "${BASH:?}" > "$fake_traces"
cat >> "$fake_traces" << 'EOF'
for number in $(seq 1 200); do
  printf 'activity-%03d\n' "$number"
done
EOF
chmod +x "$fake_traces"

output=$("$provider" __activity "$fake_traces" session harness 128 3)
[[ $output == *'… earlier activity omitted'* ]]
[[ $output == *'activity-200'* ]]
[[ $output != *'activity-001'* ]]

printf '#!%s\n' "${BASH:?}" > "$fake_traces"
cat >> "$fake_traces" << 'EOF'
printf '%0200d' 1
EOF
chmod +x "$fake_traces"

output=$("$provider" __activity "$fake_traces" session harness 128 3)
[[ $output == *'… earlier activity omitted'* ]]
[[ $output == *'0000000000000001'* ]]

response=$(
  PATH="$fixture:$PATH" "$provider" << 'EOF'
{"version":"orc.provider/v1","action":"output","capability":"messages.read","scope":"/tmp/orc","session":{"id":"orc-session","nativeId":"native-session","harness":"codex"}}
EOF
)
[[ $(jq -r '.command[0]' <<< "$response") == "$fake_traces" ]]
[[ $(jq -c '.command[1:]' <<< "$response") == '["--view","output","--once","--session","native-session","--service","codex","--color","always"]' ]]
