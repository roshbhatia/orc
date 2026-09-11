#!/usr/bin/env bash
set -euo pipefail

: "${ORC_FACTORY_RUN_URL:?Set the URL of an existing factory run}"
orc get executions
gh run view "${ORC_FACTORY_RUN_URL##*/}" --repo roshbhatia/sysinit.laurel
