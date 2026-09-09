#!/usr/bin/env bash
set -euo pipefail
IFS=$'\n\t'

scope=${1:-$PWD}
run_id=$(
  orc run create \
    --scope "$scope" \
    --name "Repair authorization headers" \
    --goal "Reject malformed tokens before session lookup" \
    --expected-output "A parser repair with focused regression tests" \
    --harness codex
)

orc node upsert research \
  --scope "$scope" \
  --run "$run_id" \
  --role researcher \
  --title "Inspect token parser" \
  --purpose "Locate token parsing and session lookup" \
  --goal "Identify prefix and empty-token failure cases" \
  --expected-output "Parser paths and focused test output" \
  --success "The affected parser and tests are located" \
  --status working > /dev/null

orc node upsert implement \
  --scope "$scope" \
  --run "$run_id" \
  --role implementer \
  --title "Repair token validation" \
  --purpose "Reject misplaced prefixes and empty tokens" \
  --goal "Preserve valid token lookup" \
  --expected-output "Parser changes and focused regression tests" \
  --success "Malformed headers fail before session lookup" \
  --depends-on research \
  --review-by verify \
  --status queued > /dev/null

orc node upsert verify \
  --scope "$scope" \
  --run "$run_id" \
  --role verifier \
  --title "Verify token boundaries" \
  --purpose "Reject parsing regressions" \
  --goal "Check valid, empty, and misplaced-prefix headers" \
  --expected-output "Review evidence or actionable feedback" \
  --success "Token boundary tests pass" \
  --depends-on implement \
  --status queued > /dev/null

printf '%s\n' "$run_id"
