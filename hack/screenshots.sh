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
export ORC_SCREENSHOT_SCOPE="$repo_dir/examples/provider-migration"

animation="$fixture/animations.yaml"
cat > "$animation" << 'YAML'
version: terminal.animation/v1
animations:
  loading:
    full:
      dimensions: { width: 25, height: 3 }
      playback: ping_pong
      easing: ease_in_out
      fps: 5
      frames:
        - { content: "user-owned Orc animation\n⚔ ······················\nassembling the workflow", style: muted }
        - { content: "user-owned Orc animation\n· ⚔ ····················\nassembling the workflow", style: accent }
        - { content: "user-owned Orc animation\n·· ⚔ ···················\nassembling the workflow", style: accent }
        - { content: "user-owned Orc animation\n··· ⚔ ··················\nassembling the workflow", style: success }
    compact:
      dimensions: { width: 3, height: 1 }
      playback: loop
      easing: linear
      fps: 4
      frames:
        - { content: "⚔  ", style: muted }
        - { content: " ⚔ ", style: accent }
        - { content: "  ⚔", style: success }
    reduced_motion:
      dimensions: { width: 25, height: 1 }
      playback: once
      easing: linear
      frames:
        - { content: "⚔ Orc", style: accent, duration_ms: 1000 }
YAML
export ORC_UI_ANIMATION_FILE="$animation"

orchestrator=$(
  "$ORC_SCREENSHOT_BIN" connect \
    --scope "$ORC_SCREENSHOT_SCOPE" \
    --id demo-orchestrator \
    --native-id demo-root \
    --harness codex \
    --role orchestrator \
    --title "Modernize the renderer" \
    --purpose "Own the workflow and verify each stage" \
    --goal "Move the rendering pipeline behind provider contracts" \
    --expected-output "A tested provider API and migrated renderer"
)

workflow="$fixture/provider-migration.yaml"
cat > "$workflow" << 'YAML'
version: orc.workflow/v1
name: renderer-provider-migration
description: Move rendering behind provider contracts
goal: Replace direct rendering calls with provider contracts
expected_output: A passing migration with review evidence
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
    purpose: Find direct renderer dependencies
    goal: List every call site and its owner
    expected_output: A verified dependency map
    value:
      mapped: true
  - name: implement
    type: agent
    role: implementer
    purpose: Implement the provider interface
    goal: Migrate call sites without behavior changes
    expected_output: Passing tests and typed providers
    depends_on: [research]
    review_by: review
    completion: judge
  - name: review
    type: agent
    role: critic
    purpose: Check behavior and contracts
    goal: Reject incomplete provider boundaries
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
    --id demo-implementer \
    --native-id demo-implementer-native \
    --harness codex \
    --role implementer \
    --title "Build provider boundary" \
    --purpose "Replace direct renderer dependencies" \
    --goal "Migrate each renderer call without changing behavior" \
    --expected-output "Typed provider adapters and passing tests" \
    --success "No direct renderer imports remain" \
    --parent "$orchestrator" \
    --run "$run" \
    --node implement \
    --source managed
)

critic=$(
  "$ORC_SCREENSHOT_BIN" connect \
    --scope "$ORC_SCREENSHOT_SCOPE" \
    --id demo-critic \
    --native-id demo-critic-native \
    --harness claude \
    --role critic \
    --title "Review provider migration" \
    --purpose "Challenge the implementation contract" \
    --goal "Find coupling, regressions, and missing evidence" \
    --expected-output "Actionable findings or approval" \
    --success "Every provider boundary has test evidence" \
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
  --title "Map rendering calls" \
  --purpose "Find direct renderer dependencies" \
  --goal "List every call site and its owner" \
  --expected-output "A verified dependency map" \
  --success "Every renderer import is classified" \
  --status "done" > /dev/null

"$ORC_SCREENSHOT_BIN" node upsert implement \
  --scope "$ORC_SCREENSHOT_SCOPE" \
  --run "$run" \
  --role implementer \
  --harness codex \
  --title "Add provider boundary" \
  --purpose "Implement the provider interface" \
  --goal "Migrate call sites without behavior changes" \
  --expected-output "Passing tests and typed providers" \
  --success "All direct imports are removed" \
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
  --title "Review migration" \
  --purpose "Check behavior and contracts" \
  --goal "Reject incomplete provider boundaries" \
  --expected-output "Findings or approval evidence" \
  --success "No renderer coupling remains" \
  --session "$critic" \
  --depends-on implement \
  --status queued > /dev/null

vhs hack/orc.tape --output "$repo_dir/docs/orc.gif"
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
