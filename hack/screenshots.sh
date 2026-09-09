#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
if [[ ${1:-} == --check && $# == 1 ]]; then
  if rg -q '!\[.*\]\(docs/orc\.(gif|png)\)' "$root/README.md"; then
    printf 'The withdrawn Orc recording is still embedded in the README.\n' >&2
    exit 1
  fi
  exit 0
fi
printf 'Live recording pending; the previous session simulation was withdrawn.\n' >&2
exit 1
