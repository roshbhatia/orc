#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"

fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

mkdir -p "$repo_dir/docs"
mkdir -p "$fixture/state"

if [[ -z ${ORC_SCREENSHOT_BIN:-} ]]; then
  package=$(
    nix build \
      --accept-flake-config \
      --option eval-cache false \
      --no-link \
      --print-out-paths \
      .#full
  )
  export ORC_PROVIDER_DIR="$package/share/orc/providers"
  export ORC_SCREENSHOT_BIN="$package/bin/orc"
fi
if [[ -z ${ORC_PROVIDER_DIR:-} ]]; then
  printf 'ORC_PROVIDER_DIR is required when ORC_SCREENSHOT_BIN is set\n' >&2
  exit 2
fi
export ORC_SCREENSHOT_SCOPE="$repo_dir/examples/provider-migration"
export XDG_STATE_HOME="$fixture/state"

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

run=$(
  "$ORC_SCREENSHOT_BIN" run create \
    --scope "$ORC_SCREENSHOT_SCOPE" \
    --name "Renderer provider migration" \
    --goal "Replace direct rendering calls with provider contracts" \
    --expected-output "A passing migration with review evidence" \
    --orchestrator "$orchestrator" \
    --harness codex
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
  --depends-on research \
  --status working > /dev/null

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
