#!/usr/bin/env bash
set -euo pipefail

provider=$1
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

fake_traces="$fixture/traces"
cat > "$fake_traces" << 'EOF'
#!/usr/bin/env bash
for number in $(seq 1 200); do
  printf 'activity-%03d\n' "$number"
done
EOF
chmod +x "$fake_traces"

output=$("$provider" __activity "$fake_traces" session harness 128 3)
[[ $output == *'… earlier activity omitted'* ]]
[[ $output == *'activity-200'* ]]
[[ $output != *'activity-001'* ]]

cat > "$fake_traces" << 'EOF'
#!/usr/bin/env bash
printf '%0200d' 1
EOF
chmod +x "$fake_traces"

output=$("$provider" __activity "$fake_traces" session harness 128 3)
[[ $output == *'… earlier activity omitted'* ]]
[[ $output == *'0000000000000001'* ]]
