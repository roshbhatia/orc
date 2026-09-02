#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

scope=${1:-$PWD}
run_id=$(
  orc run create \
    --scope "$scope" \
    --name "Externalize renderer integrations" \
    --goal "Move host-specific rendering behind provider contracts" \
    --expected-output "A tested core with optional provider packages" \
    --harness codex
)

orc node upsert research \
  --scope "$scope" \
  --run "$run_id" \
  --role researcher \
  --title "Map direct integrations" \
  --purpose "Find every host-specific import and command" \
  --goal "Classify each dependency by required capability" \
  --expected-output "A verified dependency map" \
  --success "Every direct integration has an owner" \
  --status working > /dev/null

orc node upsert implement \
  --scope "$scope" \
  --run "$run_id" \
  --role implementer \
  --title "Create provider boundary" \
  --purpose "Replace direct integrations with external commands" \
  --goal "Keep the core useful without optional tools" \
  --expected-output "Typed provider contracts and passing tests" \
  --success "The core imports no host integration" \
  --depends-on research \
  --review-by verify \
  --status queued > /dev/null

orc node upsert verify \
  --scope "$scope" \
  --run "$run_id" \
  --role verifier \
  --title "Verify provider isolation" \
  --purpose "Reject coupling and behavior regressions" \
  --goal "Prove standalone and composed operation" \
  --expected-output "Review evidence or actionable feedback" \
  --success "Core and extras pass independently" \
  --depends-on implement \
  --status queued > /dev/null

printf '%s\n' "$run_id"
