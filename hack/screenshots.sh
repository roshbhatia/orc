#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"

screenshot_bin=${ORC_SCREENSHOT_BIN:-}
provider_dir=${ORC_PROVIDER_DIR:-${ORC_PROVIDERS_DIRECTORY:-}}
for variable in ${!ORC_@}; do
  unset "$variable"
done

fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

mkdir -p "$repo_dir/docs"
mkdir -p \
  "$fixture/cache" \
  "$fixture/config" \
  "$fixture/data" \
  "$fixture/runtime" \
  "$fixture/state"
chmod 700 "$fixture/runtime"

export XDG_CACHE_HOME="$fixture/cache"
export XDG_CONFIG_HOME="$fixture/config"
export XDG_DATA_DIRS="$fixture/data"
export XDG_DATA_HOME="$fixture/data"
export XDG_RUNTIME_DIR="$fixture/runtime"
export XDG_STATE_HOME="$fixture/state"

if [[ -z $screenshot_bin ]]; then
  package=$(
    nix build \
      --accept-flake-config \
      --option eval-cache false \
      --no-link \
      --print-out-paths \
      ./extras#full
  )
  provider_dir="$package/share/orc/providers"
  screenshot_bin="$package/bin/orc"
fi
if [[ -z $provider_dir ]]; then
  printf 'ORC_PROVIDER_DIR is required when ORC_SCREENSHOT_BIN is set\n' >&2
  exit 2
fi
export ORC_PROVIDER_DIR="$provider_dir"
export ORC_SCREENSHOT_BIN="$screenshot_bin"
screenshot_bin_dir=$(dirname "$ORC_SCREENSHOT_BIN")
export PATH="$screenshot_bin_dir:$PATH"
python3 "$repo_dir/hack/token-fixture.py" "$fixture/checkout-service"
export ORC_SCREENSHOT_SCOPE="$fixture/checkout-service"
(cd "$ORC_SCREENSHOT_SCOPE" && rg -n ValidToken internal/auth && go test -v ./internal/auth) > "$fixture/research-evidence.txt"

orchestrator=$(
  "$ORC_SCREENSHOT_BIN" connect \
    --scope "$ORC_SCREENSHOT_SCOPE" \
    --id token-review-owner \
    --native-id token-review-root \
    --harness codex \
    --role orchestrator \
    --title "Repair authorization headers" \
    --purpose "Own the workflow and verify each stage" \
    --goal "Reject malformed Bearer tokens before session lookup" \
    --expected-output "A token parser repair with regression tests"
)

workflow="$fixture/token-review.yaml"
cat > "$workflow" << 'YAML'
version: orc.workflow/v1
name: token-parser-repair
description: Offline workflow fixture for a tested token parser repair
goal: Reject empty tokens and misplaced Bearer prefixes
expected_output: A parser repair with local test evidence
entry_point: research
approval:
  mode: autonomous
defaults:
  runtime:
    harness: codex
steps:
  - name: research
    type: set
    role: researcher
    purpose: Inspect token parsing and session lookup
    goal: Identify the token prefix boundary
    expected_output: Parser paths and focused test output
    value:
      mapped: true
  - name: implement
    type: agent
    role: implementer
    purpose: Validate the Bearer prefix
    goal: Reject malformed headers before session lookup
    expected_output: Passing token boundary regression tests
    depends_on: [research]
    review_by: review
    completion: judge
  - name: review
    type: agent
    role: critic
    purpose: Check prefix and empty-token behavior
    goal: Reject token parsing regressions
    expected_output: Findings or approval evidence
    routes:
      - to: implement
        when: output.approved == false
      - to: $end
YAML
run=$(
  "$ORC_SCREENSHOT_BIN" workflow start "$workflow" \
    --scope "$ORC_SCREENSHOT_SCOPE" \
    --json | jq -r .id
)
export ORC_SCREENSHOT_RUN=$run

implementer=$(
  "$ORC_SCREENSHOT_BIN" connect \
    --scope "$ORC_SCREENSHOT_SCOPE" \
    --id token-repair \
    --native-id token-repair-native \
    --harness codex \
    --role implementer \
    --title "Repair token validation" \
    --purpose "Replace substring matching with prefix validation" \
    --goal "Reject misplaced prefixes and empty tokens" \
    --expected-output "Token parser changes and regression tests" \
    --success "Misplaced prefixes fail validation" \
    --parent "$orchestrator" \
    --run "$run" \
    --node implement \
    --source managed
)

critic=$(
  "$ORC_SCREENSHOT_BIN" connect \
    --scope "$ORC_SCREENSHOT_SCOPE" \
    --id token-reviewer \
    --native-id token-reviewer-native \
    --harness claude \
    --role critic \
    --title "Review token validation" \
    --purpose "Challenge the parser boundary" \
    --goal "Find coupling, regressions, and missing evidence" \
    --expected-output "Actionable findings or approval" \
    --success "Every rejected header has a regression test" \
    --parent "$orchestrator" \
    --run "$run" \
    --node review \
    --source managed
)

"$ORC_SCREENSHOT_BIN" node upsert research \
  --scope "$ORC_SCREENSHOT_SCOPE" \
  --run "$run" \
  --role researcher \
  --harness codex \
  --title "Inspect token parser" \
  --purpose "Inspect token parsing and session lookup" \
  --goal "Identify the token prefix boundary" \
  --expected-output "Parser paths and focused test output" \
  --success "The parser and its regression tests are located" \
  --status "done" > /dev/null

"$ORC_SCREENSHOT_BIN" node upsert implement \
  --scope "$ORC_SCREENSHOT_SCOPE" \
  --run "$run" \
  --role implementer \
  --harness codex \
  --title "Validate token boundary" \
  --purpose "Validate the Bearer prefix" \
  --goal "Reject malformed headers before session lookup" \
  --expected-output "Passing token boundary regression tests" \
  --success "Malformed tokens fail validation" \
  --session "$implementer" \
  --review-by review \
  --completion judge \
  --depends-on research \
  --status queued > /dev/null

"$ORC_SCREENSHOT_BIN" node upsert review \
  --scope "$ORC_SCREENSHOT_SCOPE" \
  --run "$run" \
  --role critic \
  --harness claude \
  --title "Review parser repair" \
  --purpose "Check prefix and empty-token behavior" \
  --goal "Reject token parsing regressions" \
  --expected-output "Findings or approval evidence" \
  --success "No malformed token reaches session lookup" \
  --session "$critic" \
  --depends-on implement \
  --status queued > /dev/null

vhs examples/token-review/demo.tape --output "$repo_dir/docs/orc.gif"
cp "$repo_dir/docs/orc.gif" "$repo_dir/examples/token-review/demo.gif"
vhs hack/orc-noninteractive.tape --output "$repo_dir/docs/orc-noninteractive.gif"
vhs hack/orc-loading.tape --output "$fixture/orc-loading.gif"
ffmpeg -y \
  -ss 0.2 \
  -i "$fixture/orc-loading.gif" \
  -filter_complex \
  "fps=25,split[frames][palette_source];[palette_source]palettegen=max_colors=256[palette];[frames][palette]paletteuse=dither=bayer:bayer_scale=3" \
  -loop 0 \
  "$repo_dir/docs/orc-loading.gif" \
  > /dev/null 2>&1
