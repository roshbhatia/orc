#!/usr/bin/env bash
set -euo pipefail

: "${ORC_FACTORY_SCOPE:?Set the scope containing factory-docs-credentials}"
: "${ORC_FACTORY_RUN_URL:?Set the URL of an existing factory run}"
: "${ORC_FACTORY_PR_URL:?Set the URL of its draft pull request}"
orc get executions factory-docs-credentials --scope "$ORC_FACTORY_SCOPE"
gh run view "${ORC_FACTORY_RUN_URL##*/}" --repo roshbhatia/sysinit.laurel
gh pr view "$ORC_FACTORY_PR_URL" --json title,isDraft,url,files
