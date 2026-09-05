#!/usr/bin/env python3
"""Regression tests for identity-safe Zmx process-tree termination."""

from __future__ import annotations

import importlib.util
import os
import signal
import subprocess
import sys
import time
import unittest
from pathlib import Path
from unittest import mock

import psutil


MODULE_PATH = Path(os.environ["ORC_ZMX_PROCESS_TREE_MODULE"])
SPEC = importlib.util.spec_from_file_location("orc_zmx_process_tree", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
PROCESS_TREE = importlib.util.module_from_spec(SPEC)
sys.modules[SPEC.name] = PROCESS_TREE
SPEC.loader.exec_module(PROCESS_TREE)


class StopTreeTest(unittest.TestCase):
    def setUp(self) -> None:
        environment = os.environ.copy()
        environment["ZMX_SESSION"] = "resume-test"
        self.child = subprocess.Popen(
            [
                sys.executable,
                "-c",
                "import signal,time; signal.signal(signal.SIGTERM, signal.SIG_IGN); time.sleep(60)",
            ],
            env=environment,
        )
        self.process = psutil.Process(self.child.pid)
        self.identity = self.process.create_time()
        self.created = str(int(self.identity))
        time.sleep(0.1)

    def tearDown(self) -> None:
        try:
            process = psutil.Process(self.child.pid)
            if abs(process.create_time() - self.identity) < 0.000_001:
                os.kill(self.child.pid, signal.SIGKILL)
        except psutil.NoSuchProcess:
            pass
        self.child.wait(timeout=2)

    def record(self) -> str:
        return f"name=resume-test\tpid={self.child.pid}\tcreated={self.created}\n"

    def assert_creation_time_is_valid(self, process_created: float) -> None:
        process = mock.Mock()
        process.create_time.return_value = process_created
        process.environ.return_value = {"ZMX_SESSION": "resume-test"}
        with mock.patch.object(PROCESS_TREE.psutil, "Process", return_value=process):
            identity = PROCESS_TREE.inspect_identity("resume-test", 42, "100")
        self.assertEqual(process_created, identity)

    def assert_creation_time_is_rejected(self, process_created: float) -> None:
        process = mock.Mock()
        process.create_time.return_value = process_created
        process.environ.return_value = {"ZMX_SESSION": "resume-test"}
        with mock.patch.object(PROCESS_TREE.psutil, "Process", return_value=process):
            with self.assertRaisesRegex(PROCESS_TREE.StopError, "ambiguous"):
                PROCESS_TREE.inspect_identity("resume-test", 42, "100")

    def test_creation_adjacent_seconds_are_valid(self) -> None:
        self.assert_creation_time_is_valid(99.75)
        self.assert_creation_time_is_valid(100.25)
        self.assert_creation_time_is_valid(101.25)

    def test_creation_outside_adjacent_seconds_is_rejected(self) -> None:
        self.assert_creation_time_is_rejected(98.999)
        self.assert_creation_time_is_rejected(102.0)

    def test_precise_identity_mismatch_signals_nothing(self) -> None:
        with mock.patch.object(PROCESS_TREE, "zmx_records", return_value=self.record()):
            with self.assertRaises(PROCESS_TREE.StopError):
                PROCESS_TREE.stop_tree(
                    "zmx",
                    "resume-test",
                    self.child.pid,
                    self.created,
                    self.identity + 0.01,
                    0.1,
                )
        self.assertTrue(self.process.is_running())
        self.assertNotEqual(self.process.status(), psutil.STATUS_STOPPED)

    def test_signal_failure_resumes_frozen_survivor(self) -> None:
        original_signal_snapshot = PROCESS_TREE.signal_snapshot

        def fail_on_continue(snapshot: set[object], signal_number: int) -> None:
            if signal_number == signal.SIGCONT:
                raise PROCESS_TREE.StopError("injected continue failure")
            original_signal_snapshot(snapshot, signal_number)

        with mock.patch.object(PROCESS_TREE, "zmx_records", return_value=self.record()):
            with mock.patch.object(
                PROCESS_TREE, "signal_snapshot", side_effect=fail_on_continue
            ):
                with self.assertRaisesRegex(PROCESS_TREE.StopError, "injected"):
                    PROCESS_TREE.stop_tree(
                        "zmx",
                        "resume-test",
                        self.child.pid,
                        self.created,
                        self.identity,
                        0.5,
                    )
        time.sleep(0.05)
        self.assertTrue(self.process.is_running())
        self.assertNotEqual(self.process.status(), psutil.STATUS_STOPPED)

    def test_freeze_completes_a_stable_scan_started_before_the_deadline(self) -> None:
        leader = PROCESS_TREE.Identity(42, 100.0)
        process = mock.Mock()
        process.status.return_value = psutil.STATUS_STOPPED
        clock = [0.0]

        def slow_snapshot(_leader: object) -> set[PROCESS_TREE.Identity]:
            clock[0] += 0.6
            return {leader}

        with mock.patch.object(PROCESS_TREE, "process_snapshot", side_effect=slow_snapshot):
            with mock.patch.object(PROCESS_TREE, "current_process", return_value=process):
                with mock.patch.object(
                    PROCESS_TREE.time, "monotonic", side_effect=lambda: clock[0]
                ):
                    snapshot = PROCESS_TREE.freeze_tree(leader, 1.0)

        self.assertEqual(snapshot, {leader})
        process.suspend.assert_called_once_with()

    def test_freeze_does_not_start_a_stability_scan_after_the_deadline(self) -> None:
        leader = PROCESS_TREE.Identity(42, 100.0)
        process = mock.Mock()
        process.status.return_value = psutil.STATUS_STOPPED
        clock = [0.0]

        def snapshot_crossing_deadline(
            _leader: object,
        ) -> set[PROCESS_TREE.Identity]:
            clock[0] += 1.1
            return {leader}

        with mock.patch.object(
            PROCESS_TREE, "process_snapshot", side_effect=snapshot_crossing_deadline
        ) as process_snapshot:
            with mock.patch.object(PROCESS_TREE, "current_process", return_value=process):
                with mock.patch.object(
                    PROCESS_TREE.time, "monotonic", side_effect=lambda: clock[0]
                ):
                    with self.assertRaisesRegex(
                        PROCESS_TREE.StopError, "did not stabilize"
                    ):
                        PROCESS_TREE.freeze_tree(leader, 1.0)

        process_snapshot.assert_called_once_with(leader)
        process.suspend.assert_called_once_with()
        process.resume.assert_called_once_with()

    def test_freeze_retries_when_a_new_descendant_appears(self) -> None:
        leader = PROCESS_TREE.Identity(42, 100.0)
        child = PROCESS_TREE.Identity(43, 101.0)
        processes = {leader: mock.Mock(), child: mock.Mock()}
        for process in processes.values():
            process.status.return_value = psutil.STATUS_STOPPED
        snapshots = [
            {leader},
            {leader, child},
            {leader, child},
            {leader, child},
        ]

        with mock.patch.object(PROCESS_TREE, "process_snapshot", side_effect=snapshots):
            with mock.patch.object(
                PROCESS_TREE,
                "current_process",
                side_effect=lambda identity: processes[identity],
            ):
                snapshot = PROCESS_TREE.freeze_tree(
                    leader, time.monotonic() + 1.0
                )

        self.assertEqual(snapshot, {leader, child})
        processes[leader].suspend.assert_called_once_with()
        processes[child].suspend.assert_called_once_with()

    def test_freeze_resumes_exact_identities_when_they_never_stop(self) -> None:
        leader = PROCESS_TREE.Identity(42, 100.0)
        process = mock.Mock()
        process.status.return_value = psutil.STATUS_RUNNING
        clock = [0.0]

        def advancing_snapshot(_leader: object) -> set[PROCESS_TREE.Identity]:
            clock[0] += 0.01
            return {leader}

        with mock.patch.object(
            PROCESS_TREE, "process_snapshot", side_effect=advancing_snapshot
        ):
            with mock.patch.object(PROCESS_TREE, "current_process", return_value=process):
                with mock.patch.object(
                    PROCESS_TREE.time, "monotonic", side_effect=lambda: clock[0]
                ):
                    with self.assertRaisesRegex(
                        PROCESS_TREE.StopError, "did not stabilize"
                    ):
                        PROCESS_TREE.freeze_tree(leader, 0.025)

        process.suspend.assert_called_once_with()
        process.resume.assert_called_once_with()


if __name__ == "__main__":
    unittest.main()
