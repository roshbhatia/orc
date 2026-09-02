#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
args=(--root "$root")
if [[ ${1:-} == "--check" ]]; then
  args+=(--check)
fi
exec cargo run --quiet --manifest-path "$root/Cargo.toml" -- generate "${args[@]}"
