"""`status.json` — written last, with an atomic rename.

A run directory without this file is a run that did not complete.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any

__all__ = ["Counts", "FAILED", "FINISHED", "RunStatus", "StatusError"]

#: `finish()` was called and the shallow checks passed.
FINISHED = "finished"
#: The process died, or `finish()` found the run inconsistent.
FAILED = "failed"


@dataclass(frozen=True)
class StatusError:
    """Why the run failed. Required when `state` is `failed`."""

    kind: str
    message: str

    def to_json(self) -> dict[str, Any]:
        return {"kind": self.kind, "message": self.message}


@dataclass
class Counts:
    """How much was recorded. A cheap sanity check for a reader."""

    metrics: int = 0
    events: int = 0
    artifacts: int = 0

    def to_json(self) -> dict[str, Any]:
        return {"metrics": self.metrics, "events": self.events, "artifacts": self.artifacts}


@dataclass(frozen=True)
class RunStatus:
    """`status.json`. Every field is written, so a reader never guesses a default."""

    schema_version: str
    run_uid: str
    state: str
    started_at: str
    finished_at: str
    duration_sec: float
    exit_code: int | None = None
    collision_index: int | None = None
    error: StatusError | None = None
    counts: Counts | None = None

    def to_json(self) -> dict[str, Any]:
        return {
            "schema_version": self.schema_version,
            "run_uid": self.run_uid,
            "state": self.state,
            "started_at": self.started_at,
            "finished_at": self.finished_at,
            "duration_sec": self.duration_sec,
            "exit_code": self.exit_code,
            "collision_index": self.collision_index,
            "error": None if self.error is None else self.error.to_json(),
            "counts": None if self.counts is None else self.counts.to_json(),
        }
