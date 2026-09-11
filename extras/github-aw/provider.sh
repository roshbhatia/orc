#!/usr/bin/env bash
set -euo pipefail

provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"
provider_init github-aw

if [[ $capability == provider.validate ]]; then
  validate_manifest_requirements
  exit 0
fi
case "$capability" in
  execution.ensure | execution.observe | execution.cancel | execution.logs) ;;
  *) unsupported_capability ;;
esac

if ! jq -e '.resource.spec | has("repository") and has("workflow") and has("ref")' <<< "$request" > /dev/null; then
  emit_declined "resource must define repository, workflow, and ref"
  exit 0
fi

repository=$(jq -er '.resource.spec.repository | select(test("^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$"))' <<< "$request")
workflow=$(jq -er '.resource.spec.workflow | select(test("^[A-Za-z0-9_.-]+[.]lock[.]yml$"))' <<< "$request")
ref=$(jq -er '.resource.spec.ref | strings | select(length > 0)' <<< "$request")
uid=$(jq -er '.resource.metadata.uid | select(test("^[A-Za-z0-9-]+$"))' <<< "$request")
generation=$(jq -er '.resource.metadata.generation | numbers | select(. > 0 and floor == .)' <<< "$request")
if [[ $capability != execution.ensure ]]; then
  observed_generation=$(jq -r '.resource.status.observedGeneration // 0' <<< "$request")
  if [[ $observed_generation =~ ^[1-9][0-9]*$ ]]; then
    generation=$observed_generation
  fi
fi
correlation="${uid}-${generation}"
external_ref=$(jq -r '.resource.status.externalRef // empty' <<< "$request")
if [[ $capability != execution.ensure && $external_ref == "github-aw:${uid}-"* ]]; then
  pending_generation=${external_ref##*-}
  [[ $pending_generation =~ ^[1-9][0-9]*$ ]]
  correlation="${uid}-${pending_generation}"
fi
title="orc:${correlation}"
state_root=${XDG_STATE_HOME:-"${HOME}/.local/state"}
receipt="${state_root}/orc/github-aw/${repository}/${workflow}/${correlation}"
mkdir -p "$(dirname -- "$receipt")"

emit_pending() {
  jq -n --arg message "$1" --arg ref "github-aw:${correlation}" \
    '{version: "orc.provider/v1", status: "Pending", externalRef: $ref, message: $message}'
}

find_run() {
  gh api --paginate "repos/${repository}/actions/workflows/${workflow}/runs?event=workflow_dispatch&per_page=100" |
    jq -sc --arg title "$title" '[.[].workflow_runs[] | select(.display_title == $title)] | sort_by(.id) | last // null'
}

if [[ $capability != execution.ensure && $external_ref == "https://github.com/${repository}/actions/runs/"* ]]; then
  external_id=${external_ref##*/}
  [[ $external_id =~ ^[0-9]+$ ]]
  run=$(gh api "repos/${repository}/actions/runs/${external_id}")
else
  run=$(find_run)
fi
if [[ $capability == execution.cancel ]]; then
  mkdir -p "$receipt"
  touch "$receipt/cancel"
fi

if [[ $run == null && $capability == execution.ensure && ! -d $receipt ]]; then
  inputs=$(jq -ce --arg correlation "$correlation" \
    '(.inputs // .resource.spec.inputs // {}) + {orc_operation_id: $correlation}' <<< "$request")
  if mkdir "$receipt" 2> /dev/null; then
    if ! gh workflow run "$workflow" --repo "$repository" --ref "$ref" --json <<< "$inputs" >&2; then
      emit_pending "Dispatch returned an error. Its receipt is retained because GitHub may have accepted it; inspect the remote runs before retrying."
      exit 0
    fi
  fi
  run=$(find_run)
fi

if [[ $run == null ]]; then
  emit_pending "No correlated run is visible yet. No additional dispatch was sent."
  exit 0
fi

run_id=$(jq -er '.id' <<< "$run")
run_url=$(jq -er '.html_url' <<< "$run")
run_status=$(jq -er '.status' <<< "$run")
if [[ -f $receipt/cancel && $run_status != completed ]]; then
  if [[ ! -f $receipt/cancel-sent ]]; then
    gh run cancel "$run_id" --repo "$repository" >&2
    touch "$receipt/cancel-sent"
  fi
  jq -n --arg url "$run_url" '{version: "orc.provider/v1", status: "Running", externalRef: $url, message: "Cancellation requested; awaiting GitHub confirmation."}'
  exit 0
fi

if [[ $capability == execution.logs ]]; then
  logs=$(gh run view "$run_id" --repo "$repository" --log)
  jq -n --arg url "$run_url" --arg logs "$logs" '{version: "orc.provider/v1", externalRef: $url, outputs: {logs: $logs}}'
  exit 0
fi

jq -n --argjson run "$run" '{
  version: "orc.provider/v1",
  status: (if $run.status != "completed" then "Running"
    elif $run.conclusion == "success" then "Succeeded"
    elif $run.conclusion == "cancelled" then "Cancelled" else "Failed" end),
  externalRef: $run.html_url,
  outputs: {run: {id: $run.id, url: $run.html_url, conclusion: $run.conclusion,
    headSha: $run.head_sha}},
  message: "GitHub Actions run state; a successful run does not prove a pull request was created."
}'
