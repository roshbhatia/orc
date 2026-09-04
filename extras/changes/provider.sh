#!/usr/bin/env bash
set -euo pipefail

provider_library=${ORC_PROVIDER_LIB:-"$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)/lib/provider.sh"}
# shellcheck source=lib/provider.sh
source "$provider_library"

provider_init "changes"

case "$capability" in
  provider.validate)
    validate_manifest_requirements
    ;;
  changes.inspect)
    executable=$(command -v changes || true)
    if [[ -z $executable ]]; then
      emit_declined "changes is unavailable"
      exit 0
    fi
    emit_plan "$scope" '{}' "$executable" -r -root "$scope" -color always
    ;;
  *) unsupported_capability ;;
esac
