#!/usr/bin/env bash
set -euo pipefail

real_sleep=${ORC_WEZTERM_TEST_REAL_SLEEP:?}
state=${ORC_WEZTERM_TEST_WATCHDOG_STATE:?}

if [[ $# -ne 1 || $1 != 2 ]]; then
  exec "$real_sleep" "$@"
fi

case "$state" in
  expired) exit 0 ;;
  running) exec "$real_sleep" 60 ;;
  *)
    printf 'unsupported watchdog state: %s\n' "$state" >&2
    exit 2
    ;;
esac
