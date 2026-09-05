#!/usr/bin/env python3
"""Create a Zmx process tree with a detached child for lifecycle tests."""

from __future__ import annotations

import argparse
import os
import signal
import subprocess
import sys
import time
from pathlib import Path


def wait_forever() -> None:
    while True:
        time.sleep(60)


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("directory", type=Path)
    parser.add_argument("--child", action="store_true")
    arguments = parser.parse_args()

    arguments.directory.mkdir(parents=True, exist_ok=True)
    if arguments.child:
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        (arguments.directory / "child.pid").write_text(f"{os.getpid()}\n")
        wait_forever()

    child = subprocess.Popen(
        [sys.executable, __file__, str(arguments.directory), "--child"],
        start_new_session=True,
    )
    (arguments.directory / "parent.pid").write_text(f"{os.getpid()}\n")
    (arguments.directory / "spawned-child.pid").write_text(f"{child.pid}\n")
    wait_forever()


if __name__ == "__main__":
    main()
