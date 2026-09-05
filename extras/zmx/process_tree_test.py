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


if __name__ == "__main__":
    unittest.main()
