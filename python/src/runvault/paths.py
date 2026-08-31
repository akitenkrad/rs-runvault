"""Where runs live, and the `latest_finished` link."""
from __future__ import annotations

import itertools
import os
import time
from datetime import datetime
from pathlib import Path

from . import files, lockfile

__all__ = [
    "LATEST_FINISHED",
    "LINK_MUTEX",
    "experiment_dir",
    "is_run_dir",
    "run_dirs",
    "update_latest_finished",
]

#: The link in an experiment directory that points at the last completed run.
LATEST_FINISHED = "latest_finished"

#: Held while `latest_finished` is compared and replaced.
LINK_MUTEX = ".latest_finished.mutex"

#: How long to wait for the mutex before assuming its holder died.
MUTEX_STALE_AFTER = 30.0

_temporaries = itertools.count()


def experiment_dir(results_root: Path, experiment: str) -> Path:
    """`<results_root>/<experiment>`."""
    return Path(results_root) / experiment


def update_latest_finished(experiment_dir: Path, slug: str, finished_at: str) -> bool:
    """Point `latest_finished` at `slug`, but only if that run finished later
    than the one it points at now.

    Two runs started in either order can finish in either order; without the
    comparison the link would walk backwards when a long run overtakes a short one.
    """
    experiment_dir = Path(experiment_dir)
    # Reading the current link and replacing it have to be one step. Two runs
    # finishing at once would otherwise both read the same old link, and the one
    # that finished earlier could install itself last.
    with _LinkMutex(experiment_dir):
        link = experiment_dir / LATEST_FINISHED
        current = _link_finished_at(link)
        if current is not None and not _is_later(finished_at, current):
            return False

        # A symlink cannot be replaced in place, so make a new one and rename over.
        tmp = experiment_dir / f".{LATEST_FINISHED}.tmp-{os.getpid()}-{next(_temporaries)}"
        tmp.unlink(missing_ok=True)
        os.symlink(slug, tmp, target_is_directory=True)
        os.replace(tmp, link)
        return True


def _is_later(candidate: str, current: str) -> bool:
    """Whether one completion happened after another.

    Compared as instants rather than as text: two runs recorded under different
    UTC offsets sort the wrong way round as strings, and the link would then
    move backwards in real time.
    """
    try:
        return datetime.fromisoformat(candidate) > datetime.fromisoformat(current)
    except ValueError:
        return candidate > current


def _link_finished_at(link: Path) -> str | None:
    """When the run `latest_finished` points at finished, when it points anywhere."""
    if not link.is_symlink():
        return None
    status = link.parent / os.readlink(link) / "status.json"
    if not status.is_file():
        return None
    try:
        return files.read_json(status).get("finished_at")
    except (OSError, ValueError):
        return None


class _LinkMutex:
    """A directory-wide mutex, made of a file that only one creator can win."""

    def __init__(self, experiment_dir: Path) -> None:
        self._path = experiment_dir / LINK_MUTEX
        experiment_dir.mkdir(parents=True, exist_ok=True)

    def __enter__(self) -> _LinkMutex:
        deadline = time.monotonic() + MUTEX_STALE_AFTER
        while True:
            try:
                handle = os.open(self._path, os.O_CREAT | os.O_EXCL | os.O_WRONLY)
            except FileExistsError:
                # A holder that died leaves the file behind; take it over once it
                # is older than a whole update could possibly need.
                if self._stale() or time.monotonic() >= deadline:
                    self._path.unlink(missing_ok=True)
                    continue
                time.sleep(0.01)
            else:
                os.write(handle, str(os.getpid()).encode("ascii"))
                os.close(handle)
                return self

    def __exit__(self, *_exc: object) -> None:
        self._path.unlink(missing_ok=True)

    def _stale(self) -> bool:
        try:
            return time.time() - self._path.stat().st_mtime > MUTEX_STALE_AFTER
        except OSError:
            return False


def run_dirs(results_root: Path) -> list[Path]:
    """Every run directory under `results_root`, at any nesting depth."""
    results_root = Path(results_root)
    if not results_root.is_dir():
        return []
    out: list[Path] = []
    stack = [results_root]
    while stack:
        for entry in stack.pop().iterdir():
            if not entry.is_dir() or entry.is_symlink():
                continue
            if is_run_dir(entry):
                out.append(entry)
            else:
                stack.append(entry)
    return sorted(out)


def is_run_dir(path: Path) -> bool:
    """Whether a directory looks like a run rather than a grouping directory.

    A directory counts as a run when it holds `run.json`, `status.json` or the
    lock: that also finds the legacy layouts, which have no `run.json`.
    """
    path = Path(path)
    return (
        (path / "run.json").is_file()
        or (path / "status.json").is_file()
        or (path / lockfile.LOCK_FILE).is_file()
    )
