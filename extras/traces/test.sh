#!/usr/bin/env bash
set -euo pipefail

provider=$1
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

fake_traces="$fixture/traces"
write_fake_traces() {
  printf '#!%s\n' "${ORC_TEST_BASH:?}" >"$fake_traces"
  [[ $(head -n 1 "$fake_traces") == "#!$ORC_TEST_BASH" ]]
}

write_fake_traces
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

write_fake_traces
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
provider_path=$(cd -- "$(dirname -- "$provider")" && pwd)/$(basename -- "$provider")
[[ $(jq -r '.command[0]' <<< "$response") == "$provider_path" ]]
[[ $(jq -c '.command[1:]' <<< "$response") == "[\"__messages\",\"$fake_traces\",\"native-session\",\"codex\"]" ]]

write_fake_traces
cat >> "$fake_traces" << 'EOF'
set -euo pipefail
[[ " $* " == *' --view output '* ]]
[[ " $* " == *' --format jsonl '* ]]
[[ " $* " == *' --once '* ]]
[[ " $* " == *' --session native-session '* ]]
[[ " $* " == *' --service codex '* ]]
[[ " $* " == *' --color never '* ]]
printf '%s\n' '{"version":"traces.message/v1","id":"msg_one","session":"native-session","timestamp":"2026-09-06T12:34:56.123456789Z","body":"finished"}'
EOF
chmod +x "$fake_traces"

output=$("$provider" __messages "$fake_traces" native-session codex)
[[ $(jq -r '.version' <<< "$output") == orc.message/v1 ]]
[[ $(jq -r '.id' <<< "$output") == msg_one ]]
[[ $(jq -r '.body' <<< "$output") == finished ]]

validation_request='{"version":"orc.provider/v1","capability":"provider.validate","scope":"/tmp/orc","manifest":{"requires":{"commands":["jq","tail","traces"]}}}'
write_fake_traces
cat >> "$fake_traces" << 'EOF'
printf '%s\n' 'Usage: traces [-view tree]'
EOF
chmod +x "$fake_traces"
validation=$(PATH="$fixture:$PATH" "$provider" <<< "$validation_request")
[[ $(jq -r '.status' <<< "$validation") == failed ]]
[[ $(jq -r '.checks[] | select(.name == "contract:messages-jsonl") | .status' <<< "$validation") == failed ]]

write_fake_traces
cat >> "$fake_traces" << 'EOF'
set -euo pipefail
printf '%s\n' '{"version":"traces.message/v1","id":"validation-message","session":"orc-validation","timestamp":"not-rfc3339","body":"validation message"}'
EOF
chmod +x "$fake_traces"
validation=$(PATH="$fixture:$PATH" "$provider" <<< "$validation_request")
[[ $(jq -r '.status' <<< "$validation") == failed ]]
[[ $(jq -r '.checks[] | select(.name == "contract:messages-jsonl") | .status' <<< "$validation") == failed ]]

write_fake_traces
cat >> "$fake_traces" << 'EOF'
set -euo pipefail
if [[ ${1:-} == --help ]]; then
  printf '%s\n' 'Usage: traces [-format string]'
  exit
fi
printf '%s\n' 'plain text only'
EOF
chmod +x "$fake_traces"
validation=$(PATH="$fixture:$PATH" "$provider" <<< "$validation_request")
[[ $(jq -r '.status' <<< "$validation") == failed ]]
[[ $(jq -r '.checks[] | select(.name == "contract:messages-jsonl") | .status' <<< "$validation") == failed ]]

write_fake_traces
cat >> "$fake_traces" << 'EOF'
set -euo pipefail
[[ -n ${XDG_CONFIG_HOME:-} ]]
[[ " $* " == *' --file '* ]]
[[ " $* " == *' --view output '* ]]
[[ " $* " == *' --format jsonl '* ]]
[[ " $* " == *' --once '* ]]
[[ " $* " == *' --session orc-validation '* ]]
[[ " $* " == *' --service orc-validation '* ]]
[[ " $* " == *' --color never '* ]]
printf '%s\n' '{"version":"traces.message/v1","id":"validation-message","session":"orc-validation","timestamp":"1970-01-01T00:00:01Z","body":"validation message"}'
EOF
chmod +x "$fake_traces"
validation=$(PATH="$fixture:$PATH" "$provider" <<< "$validation_request")
[[ $(jq -r '.status' <<< "$validation") == ok ]]
[[ $(jq -r '.checks[] | select(.name == "contract:messages-jsonl") | .status' <<< "$validation") == ok ]]
