"""`.runvault.lock` — how a live run is told apart from one that was killed.

An interpreter that is killed runs no handler, so the existence of the lock
cannot mean "running": a run that died would stay running forever. The lock
carries a heartbeat and the identity of the process, and both are checked.
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time
from dataclasses import dataclass
from datetime import datetime
from pathlib import Path
from typing import Any

from . import files
from .env import host

__all__ = [
    "HEARTBEAT_INTERVAL",
    "Heartbeat",
    "LOCK_FILE",
    "LockRecord",
    "RUNNING",
    "STALE",
    "STALE_AFTER",
    "process_start_time",
    "read",
    "remove",
    "write",
]

#: The lock file's name inside a run directory.
LOCK_FILE = ".runvault.lock"

#: How often the heartbeat is refreshed. Short enough against the five-minute
#: threshold that missing one refresh does not make a live run look dead.
HEARTBEAT_INTERVAL = 30.0

#: How long a heartbeat stays believable.
STALE_AFTER = 300.0

#: The process is still there, or the heartbeat is recent enough.
RUNNING = "running"
#: Neither holds: the process died without writing `status.json`.
STALE = "stale"


def process_start_time(pid: int) -> int | None:
    """When a live process started, in seconds since the epoch.

    `None` when this platform cannot say. A run that cannot prove its process is
    alive falls back to the heartbeat, which is weaker but true; inventing a
    start time would make a recycled PID look like the original process.
    """
    try:
        if sys.platform.startswith("linux"):
            with open(f"/proc/{pid}/stat", encoding="utf-8") as handle:
                fields = handle.read().rpartition(")")[2].split()
            ticks = int(fields[19])  # field 22 of proc(5), counting from the state field
            with open("/proc/stat", encoding="utf-8") as handle:
                boot = next(
                    int(line.split()[1]) for line in handle if line.startswith("btime ")
                )
            return boot + ticks // os.sysconf("SC_CLK_TCK")
        if sys.platform == "darwin":
            printed = subprocess.run(
                ["ps", "-o", "lstart=", "-p", str(pid)],
                capture_output=True, text=True, check=True,
            ).stdout.strip()
            if not printed:
                return None
            return int(time.mktime(time.strptime(printed, "%a %b %d %H:%M:%S %Y")))
    except (OSError, ValueError, StopIteration, subprocess.SubprocessError):
        return None
    return None


@dataclass
class LockRecord:
    """What `.runvault.lock` holds."""

    host: str
    pid: int
    process_start_time: int | None
    created_at: str
    heartbeat_at: str

    @classmethod
    def for_this_process(cls, created_at: datetime | None = None) -> LockRecord:
        """A record for the current process."""
        now = (created_at or datetime.now().astimezone()).isoformat()
        pid = os.getpid()
        return cls(host(), pid, process_start_time(pid), now, now)

    def to_json(self) -> dict[str, Any]:
        return {
            "host": self.host,
            "pid": self.pid,
            "process_start_time": self.process_start_time,
            "created_at": self.created_at,
            "heartbeat_at": self.heartbeat_at,
        }

    def liveness(self, now: datetime, this_host: str) -> str:
        """Decide whether the run behind this lock is still alive.

        The PID is only consulted on the machine that wrote the lock; elsewhere
        the heartbeat is all there is.
        """
        if self.host == this_host and self.process_start_time is not None:
            if process_start_time(self.pid) == self.process_start_time:
                return RUNNING
        try:
            age = (now - datetime.fromisoformat(self.heartbeat_at)).total_seconds()
        except ValueError:
            return STALE
        return RUNNING if age <= STALE_AFTER else STALE


def read(run_dir: Path) -> LockRecord | None:
    """Read a run's lock file, or `None` when there is none."""
    path = Path(run_dir) / LOCK_FILE
    if not path.is_file():
        return None
    return LockRecord(**json.loads(path.read_text(encoding="utf-8")))


def remove(run_dir: Path) -> None:
    """Remove a run's lock file. Missing is success."""
    (Path(run_dir) / LOCK_FILE).unlink(missing_ok=True)


def write(run_dir: Path, record: LockRecord) -> None:
    """Write the lock file atomically, so a reader never sees half of it."""
    files.write_json_atomically(Path(run_dir) / LOCK_FILE, record.to_json())


class Heartbeat:
    """The thread that keeps `heartbeat_at` fresh while the run is going."""

    def __init__(self, run_dir: Path, record: LockRecord) -> None:
        self._dir = Path(run_dir)
        self._record = record
        self._stop = threading.Event()
        self._degraded = False
        self._thread = threading.Thread(
            target=self._beat, name="runvault-heartbeat", daemon=True
        )

    @classmethod
    def start(cls, run_dir: Path, record: LockRecord) -> Heartbeat:
        """Write the lock and start refreshing it every `HEARTBEAT_INTERVAL`."""
        beat = cls(run_dir, record)
        write(run_dir, record)
        beat._thread.start()
        return beat

    def _beat(self) -> None:
        while not self._stop.wait(HEARTBEAT_INTERVAL):
            self._record.heartbeat_at = datetime.now().astimezone().isoformat()
            try:
                write(self._dir, self._record)
            except OSError:
                # The run itself is still valid; only the liveness signal is lost.
                self._degraded = True

    @property
    def degraded(self) -> bool:
        """Whether a refresh ever failed. A stopped heartbeat only makes a live
        run look dead, so it is reported rather than raised."""
        return self._degraded

    def stop(self) -> None:
        """Stop the thread and wait for it. Idempotent."""
        self._stop.set()
        if self._thread.is_alive():
            self._thread.join(timeout=HEARTBEAT_INTERVAL)
