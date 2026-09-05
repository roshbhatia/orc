#!/usr/bin/env python3
"""Stop one Zmx-owned process tree without signalling reused PIDs."""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from dataclasses import dataclass

import psutil


class StopError(RuntimeError):
    """The process tree could not be stopped safely."""


@dataclass(frozen=True, order=True)
class Identity:
    pid: int
    created: float


def same_created(left: float, right: float) -> bool:
    return abs(left - right) < 0.000_001


def current_process(identity: Identity) -> psutil.Process | None:
    try:
        process = psutil.Process(identity.pid)
        if not same_created(process.create_time(), identity.created):
            return None
        if process.status() == psutil.STATUS_ZOMBIE:
            return None
        return process
    except psutil.NoSuchProcess:
        return None
    except (psutil.AccessDenied, psutil.Error) as error:
        raise StopError(f"cannot verify process {identity.pid}: {error}") from error


def process_snapshot(leader: Identity) -> set[Identity]:
    if current_process(leader) is None:
        raise StopError(f"Zmx leader {leader.pid} changed identity before stop")

    processes: dict[int, tuple[int, float | None]] = {}
    for process in psutil.process_iter(["pid", "ppid", "create_time"], ad_value=None):
        info = process.info
        processes[info["pid"]] = (info["ppid"], info["create_time"])

    descendants = {leader.pid}
    changed = True
    while changed:
        changed = False
        for pid, (parent, _) in processes.items():
            if parent in descendants and pid not in descendants:
                descendants.add(pid)
                changed = True

    snapshot = {leader}
    for pid in descendants - {leader.pid}:
        created = processes[pid][1]
        if created is None:
            raise StopError(f"cannot establish identity for descendant {pid}")
        snapshot.add(Identity(pid, created))
    return snapshot


def resume_snapshot(snapshot: set[Identity]) -> None:
    for identity in snapshot:
        process = current_process(identity)
        if process is None:
            continue
        try:
            process.resume()
        except psutil.NoSuchProcess:
            continue


def snapshot_is_stopped(snapshot: set[Identity]) -> bool:
    for identity in snapshot:
        process = current_process(identity)
        if process is None:
            continue
        try:
            if process.status() != psutil.STATUS_STOPPED:
                return False
        except psutil.NoSuchProcess:
            continue
    return True


def freeze_tree(leader: Identity, deadline: float) -> set[Identity]:
    frozen: set[Identity] = set()
    try:
        while time.monotonic() < deadline:
            snapshot = process_snapshot(leader)
            for identity in sorted(snapshot - frozen):
                process = current_process(identity)
                if process is None:
                    continue
                try:
                    process.suspend()
                except psutil.NoSuchProcess:
                    continue
                frozen.add(identity)

            if not snapshot_is_stopped(snapshot):
                continue
            if time.monotonic() >= deadline:
                break

            next_snapshot = process_snapshot(leader)
            if next_snapshot.issubset(frozen) and snapshot_is_stopped(next_snapshot):
                return frozen
        raise StopError("process tree did not stabilize before the stop deadline")
    except BaseException:
        resume_snapshot(frozen)
        raise


def signal_snapshot(snapshot: set[Identity], signal_number: int) -> None:
    for identity in sorted(snapshot, reverse=True):
        process = current_process(identity)
        if process is None:
            continue
        try:
            os.kill(identity.pid, signal_number)
        except ProcessLookupError:
            continue
        except PermissionError as error:
            raise StopError(f"cannot signal process {identity.pid}: {error}") from error


def live_identities(snapshot: set[Identity]) -> set[Identity]:
    return {identity for identity in snapshot if current_process(identity) is not None}


def wait_for_exit(snapshot: set[Identity], deadline: float) -> set[Identity]:
    while time.monotonic() < deadline:
        survivors = live_identities(snapshot)
        if not survivors:
            return set()
        time.sleep(0.025)
    return live_identities(snapshot)


def zmx_records(zmx: str) -> str:
    environment = os.environ.copy()
    environment["ZMX_SESSION_PREFIX"] = ""
    try:
        result = subprocess.run(
            [zmx, "list"],
            check=False,
            capture_output=True,
            env=environment,
            text=True,
            timeout=2,
        )
    except subprocess.TimeoutExpired as error:
        raise StopError("timed out while inspecting Zmx sessions") from error
    if result.returncode != 0:
        raise StopError(f"cannot inspect Zmx sessions: {result.stderr.strip()}")
    return result.stdout


def matching_zmx_record(records: str, name: str, pid: int, created: str) -> bool:
    for line in records.splitlines():
        fields = {}
        for field in line.split("\t"):
            key, separator, value = field.strip().partition("=")
            if separator:
                fields[key] = value
        if fields.get("name") == name:
            return fields.get("pid") == str(pid) and fields.get("created") == created
    return False


def inspect_identity(name: str, pid: int, created: str) -> float:
    try:
        expected_created = int(created)
    except ValueError as error:
        raise StopError(f"invalid Zmx creation time: {created}") from error
    try:
        process = psutil.Process(pid)
        process_created = process.create_time()
        session_name = process.environ().get("ZMX_SESSION")
    except psutil.NoSuchProcess as error:
        raise StopError(f"Zmx leader {pid} is missing while its record is active") from error
    except (psutil.AccessDenied, psutil.Error) as error:
        raise StopError(f"cannot establish identity for Zmx leader {pid}: {error}") from error
    # Zmx records integer wall time before forking the PTY leader. Linux derives
    # process birth time from an integer boot time and scheduler ticks. These two
    # truncations can place the valid leader in the preceding or following second.
    process_second = int(process_created)
    if process_second not in {
        expected_created - 1,
        expected_created,
        expected_created + 1,
    }:
        raise StopError(
            f"Zmx leader {pid} has an ambiguous creation identity "
            f"(recorded {expected_created}, process {process_created:.9f})"
        )
    if session_name != name:
        raise StopError(f"Zmx leader {pid} does not advertise session {name}")
    return process_created


def stop_tree(
    zmx: str,
    name: str,
    pid: int,
    created: str,
    identity: float,
    grace: float,
) -> None:
    records = zmx_records(zmx)
    if not matching_zmx_record(records, name, pid, created):
        raise StopError("Zmx session identity changed before stop")

    process_created = inspect_identity(name, pid, created)
    if not same_created(process_created, identity):
        raise StopError(f"Zmx leader {pid} changed its precise creation identity")

    leader = Identity(pid, process_created)
    snapshot = freeze_tree(leader, time.monotonic() + grace)
    completed = False
    try:
        signal_snapshot(snapshot, signal.SIGTERM)
        signal_snapshot(snapshot, signal.SIGCONT)

        environment = os.environ.copy()
        environment["ZMX_SESSION_PREFIX"] = ""
        try:
            result = subprocess.run(
                [zmx, "kill", name, "--force"],
                check=False,
                capture_output=True,
                env=environment,
                text=True,
                timeout=grace,
            )
        except subprocess.TimeoutExpired as error:
            raise StopError("timed out while removing the Zmx session") from error

        survivors = wait_for_exit(snapshot, time.monotonic() + grace)
        if survivors:
            signal_snapshot(survivors, signal.SIGKILL)
            survivors = wait_for_exit(survivors, time.monotonic() + grace)

        record_survives = matching_zmx_record(zmx_records(zmx), name, pid, created)
        if record_survives or survivors:
            survivor_pids = ", ".join(str(identity.pid) for identity in sorted(survivors))
            details = []
            if result.returncode != 0:
                details.append(f"zmx kill exited {result.returncode}")
            if record_survives:
                details.append("Zmx record remains")
            if survivors:
                details.append(f"processes remain: {survivor_pids}")
            raise StopError("; ".join(details))
        completed = True
    finally:
        if not completed:
            resume_snapshot(live_identities(snapshot))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    commands = parser.add_subparsers(dest="command", required=True)
    inspect = commands.add_parser("inspect")
    inspect.add_argument("--name", required=True)
    inspect.add_argument("--pid", required=True, type=int)
    inspect.add_argument("--created", required=True)
    stop = commands.add_parser("stop")
    stop.add_argument("--zmx", required=True)
    stop.add_argument("--name", required=True)
    stop.add_argument("--pid", required=True, type=int)
    stop.add_argument("--created", required=True)
    stop.add_argument("--identity", required=True, type=float)
    stop.add_argument("--grace", type=float, default=1.0)
    return parser.parse_args()


def main() -> int:
    arguments = parse_args()
    try:
        if arguments.command == "inspect":
            identity = inspect_identity(arguments.name, arguments.pid, arguments.created)
            print(f"{identity:.9f}")
        else:
            stop_tree(
                arguments.zmx,
                arguments.name,
                arguments.pid,
                arguments.created,
                arguments.identity,
                arguments.grace,
            )
    except StopError as error:
        print(f"orc-provider-zmx: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
