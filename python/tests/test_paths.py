"""`latest_finished` — the link that must only ever move forward."""
from __future__ import annotations

import json
import os
import threading
from pathlib import Path

from runvault import paths


def finished(dir: Path, at: str, state: str = "finished") -> None:
    dir.mkdir(parents=True, exist_ok=True)
    status = {
        "schema_version": "1.0",
        "run_uid": "01K3QZ8F7H9M2N4P6R8T0V2X4Z",
        "state": state,
        "started_at": at,
        "finished_at": at,
        "duration_sec": 1.0,
    }
    (dir / "status.json").write_text(json.dumps(status), encoding="utf-8")


def test_the_link_follows_the_newest_completion(tmp_path: Path) -> None:
    finished(tmp_path / "a", "2026-08-30T10:00:00+09:00")
    assert paths.update_latest_finished(tmp_path, "a", "2026-08-30T10:00:00+09:00")
    finished(tmp_path / "b", "2026-08-30T11:00:00+09:00")
    assert paths.update_latest_finished(tmp_path, "b", "2026-08-30T11:00:00+09:00")
    assert os.readlink(tmp_path / paths.LATEST_FINISHED) == "b"


def test_a_run_that_started_first_but_finished_last_does_not_rewind_the_link(tmp_path: Path) -> None:
    finished(tmp_path / "late", "2026-08-30T12:00:00+09:00")
    paths.update_latest_finished(tmp_path, "late", "2026-08-30T12:00:00+09:00")
    finished(tmp_path / "early", "2026-08-30T09:00:00+09:00")
    assert not paths.update_latest_finished(tmp_path, "early", "2026-08-30T09:00:00+09:00")
    assert os.readlink(tmp_path / paths.LATEST_FINISHED) == "late"


def test_the_mutex_is_released_so_a_later_update_is_not_blocked(tmp_path: Path) -> None:
    finished(tmp_path / "a", "2026-08-30T10:00:00+09:00")
    paths.update_latest_finished(tmp_path, "a", "2026-08-30T10:00:00+09:00")
    assert not (tmp_path / paths.LINK_MUTEX).exists()


def test_runs_are_found_under_a_grouping_directory(tmp_path: Path) -> None:
    finished(tmp_path / "schelling/main_1", "2026-08-30T10:00:00+09:00")
    finished(tmp_path / "schelling/main_2", "2026-08-30T10:00:00+09:00")
    assert len(paths.run_dirs(tmp_path)) == 2


def test_a_missing_results_root_yields_nothing(tmp_path: Path) -> None:
    assert paths.run_dirs(tmp_path / "nope") == []


def test_two_runs_finishing_at_once_do_not_walk_the_link_backwards(tmp_path: Path) -> None:
    # Without the mutex both would read the same empty link, and whichever wrote
    # last would win regardless of which run actually finished later.
    early, late = "2026-08-30T09:00:00+09:00", "2026-08-30T12:00:00+09:00"
    finished(tmp_path / "early", early)
    finished(tmp_path / "late", late)
    ready = threading.Barrier(2)

    def update(slug: str, at: str) -> None:
        ready.wait()
        paths.update_latest_finished(tmp_path, slug, at)

    threads = [
        threading.Thread(target=update, args=("early", early)),
        threading.Thread(target=update, args=("late", late)),
    ]
    for thread in threads:
        thread.start()
    for thread in threads:
        thread.join()
    assert os.readlink(tmp_path / paths.LATEST_FINISHED) == "late"
