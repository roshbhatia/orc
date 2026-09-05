#!/usr/bin/env bash
set -euo pipefail

provider=${ORC_PROVIDER_ZMX_COMMAND:?}
python=${ORC_TEST_PYTHON:?}
fixture=${ORC_TEST_TREE_FIXTURE:?}
test_scope=${TMPDIR:?}/zmx-provider-real-test
runtime=$test_scope/runtime
processes=$test_scope/processes
session=orc-zmx-tree-$$
export ZMX_DIR=$runtime/zmx
export ZMX_SESSION_PREFIX=

cleanup() {
  if zmx list 2>/dev/null | grep -Fq "name=$session"; then
    zmx kill "$session" --force >/dev/null 2>&1 || true
  fi
  if [[ -n ${attach_pid:-} ]]; then
    wait "$attach_pid" 2>/dev/null || true
  fi
  if [[ -n ${parent_pid:-} && -n ${parent_identity:-} ]]; then
    "$python" -c 'import os, psutil, signal, sys
pid, identity = int(sys.argv[1]), float(sys.argv[2])
try:
    process = psutil.Process(pid)
    if abs(process.create_time() - identity) < 0.000001:
        os.kill(pid, signal.SIGKILL)
except psutil.NoSuchProcess:
    pass' "$parent_pid" "$parent_identity"
  fi
  if [[ -n ${child_pid:-} && -n ${child_identity:-} ]]; then
    "$python" -c 'import os, psutil, signal, sys
pid, identity = int(sys.argv[1]), float(sys.argv[2])
try:
    process = psutil.Process(pid)
    if abs(process.create_time() - identity) < 0.000001:
        os.kill(pid, signal.SIGKILL)
except psutil.NoSuchProcess:
    pass' "$child_pid" "$child_identity"
  fi
}
trap cleanup EXIT

mkdir -p "$runtime" "$processes"
zmx attach "$session" "$python" "$fixture" "$processes" >/dev/null 2>&1 &
attach_pid=$!

for _ in $(seq 1 100); do
  if [[ -s $processes/parent.pid && -s $processes/child.pid ]] &&
    zmx list 2>/dev/null | grep -Fq "name=$session"; then
    break
  fi
  sleep 0.05
done
test -s "$processes/parent.pid"
test -s "$processes/child.pid"

record=$(zmx list | grep -F "name=$session")
leader_pid=$(sed -E 's/.*pid=([0-9]+).*/\1/' <<<"$record")
created=$(sed -E 's/.*created=([0-9]+).*/\1/' <<<"$record")
parent_pid=$(<"$processes/parent.pid")
child_pid=$(<"$processes/child.pid")
leader_identity=$("$python" -c 'import psutil, sys; print(f"{psutil.Process(int(sys.argv[1])).create_time():.9f}")' "$leader_pid")
parent_identity=$("$python" -c 'import psutil, sys; print(f"{psutil.Process(int(sys.argv[1])).create_time():.9f}")' "$parent_pid")
child_identity=$("$python" -c 'import psutil, sys; print(f"{psutil.Process(int(sys.argv[1])).create_time():.9f}")' "$child_pid")

"$provider" stop "$session" "$leader_pid" "$((created + 60))" "$leader_identity" identity-mismatch
kill -0 "$parent_pid"
kill -0 "$child_pid"
zmx list | grep -Fq "name=$session"

"$provider" stop "$session" "$leader_pid" "$created" "$leader_identity" stop-tree
wait "$attach_pid" 2>/dev/null || true
attach_pid=

if kill -0 "$parent_pid" 2>/dev/null; then
  printf 'parent process %s survived provider stop\n' "$parent_pid" >&2
  exit 1
fi
if kill -0 "$child_pid" 2>/dev/null; then
  printf 'detached child process %s survived provider stop\n' "$child_pid" >&2
  exit 1
fi
if zmx list 2>/dev/null | grep -Fq "name=$session"; then
  printf 'Zmx record %s survived provider stop\n' "$session" >&2
  exit 1
fi
