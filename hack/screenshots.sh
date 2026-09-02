#!/usr/bin/env bash
set -euo pipefail

repo_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$repo_dir"

fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT

mkdir -p "$repo_dir/docs"
mkdir -p "$fixture/config/providers" "$fixture/repo" "$fixture/state"

package=$(nix build --accept-flake-config --no-link --print-out-paths)
export ORC_PROVIDER_DIR="$fixture/config/providers"
export ORC_SCREENSHOT_BIN="$package/bin/orc"
export ORC_SCREENSHOT_SCOPE="$fixture/repo"
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
  --depends-on implement \
  --status queued > /dev/null

vhs hack/orc.tape --output "$fixture/orc.gif"
ffmpeg -y -loglevel error -sseof -1 -i "$fixture/orc.gif" -frames:v 1 -update 1 "$repo_dir/docs/orc.png"
