"""The shallow half of §3.10: whether a run contradicts itself."""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from conftest import start
from runvault import lockfile, verify
from runvault.errors import VerifyError
from runvault.pointer import PointerError

OTHER_UID = "01K3QZ8F7H9M2N4P6R8T0V2X4Z"


def edit(path: Path, **changes: object) -> None:
    doc = json.loads(path.read_text(encoding="utf-8"))
    doc.update(changes)
    path.write_text(json.dumps(doc), encoding="utf-8")


def finished_run(results: Path) -> Path:
    run = start(results)
    run.log_metric("asr", 0.5)
    run.log_event("note", {"message": "hello"})
    return run.finish()


def test_a_run_the_writer_produced_passes(results: Path) -> None:
    verify.shallow(finished_run(results))


def test_a_directory_renamed_away_from_its_slug_is_caught(results: Path) -> None:
    directory = finished_run(results)
    moved = directory.parent / "main_20260830_101500_00000000_0000"
    directory.rename(moved)
    with pytest.raises(VerifyError, match="run_slug"):
        verify.shallow(moved)


def test_a_config_belonging_to_another_run_is_caught(results: Path) -> None:
    directory = finished_run(results)
    edit(directory / "config.json", run_uid=OTHER_UID)
    with pytest.raises(VerifyError, match="run_uid"):
        verify.shallow(directory)


def test_an_event_belonging_to_another_run_is_caught(results: Path) -> None:
    directory = finished_run(results)
    path = directory / "events.jsonl"
    line = json.loads(path.read_text(encoding="utf-8").splitlines()[0])
    path.write_text(json.dumps({**line, "run_uid": OTHER_UID}) + "\n", encoding="utf-8")
    with pytest.raises(VerifyError, match="run_uid"):
        verify.shallow(directory)


def test_a_duplicated_metric_key_is_caught(results: Path) -> None:
    directory = finished_run(results)
    with (directory / "metrics.csv").open(encoding="utf-8") as handle:
        last = handle.read().splitlines()[-1]
    with (directory / "metrics.csv").open("a", encoding="utf-8", newline="") as handle:
        handle.write(last + "\n")
    with pytest.raises(VerifyError):
        verify.shallow(directory)


def test_a_lock_left_beside_a_status_is_caught(results: Path) -> None:
    directory = finished_run(results)
    lockfile.write(directory, lockfile.LockRecord.for_this_process())
    with pytest.raises(VerifyError, match="lock"):
        verify.shallow(directory)


def test_a_failed_run_must_say_why(results: Path) -> None:
    directory = finished_run(results)
    edit(directory / "status.json", state="failed", error=None)
    with pytest.raises(VerifyError):
        verify.shallow(directory)


def test_lineage_that_points_at_itself_is_caught(results: Path) -> None:
    directory = finished_run(results)
    meta = json.loads((directory / "run.json").read_text(encoding="utf-8"))
    meta["lineage"] = {"sweep_id": None, "parent_run_uid": None,
                       "resumed_from": meta["run_uid"], "derived_from": None}
    (directory / "run.json").write_text(json.dumps(meta), encoding="utf-8")
    with pytest.raises(VerifyError, match="resumed_from"):
        verify.shallow(directory)


def test_an_exclusion_that_stopped_resolving_is_caught(results: Path) -> None:
    directory = finished_run(results)
    config = json.loads((directory / "config.json").read_text(encoding="utf-8"))
    config["runvault"]["hash_exclude"] = ["/gone"]
    (directory / "config.json").write_text(json.dumps(config), encoding="utf-8")
    with pytest.raises(PointerError):
        verify.shallow(directory)


def test_a_work_id_that_disagrees_with_its_own_fields_is_caught() -> None:
    with pytest.raises(VerifyError, match="work_id"):
        verify.check_research({
            "is_replication": True,
            "work": {"work_id": "arxiv:2405.01234", "arxiv_id": "2405.09999",
                     "title": "A paper", "source_version": "arXiv v1"},
            "targets": [{"target_id": "t1", "kind": "table", "label": "Table 1"}],
            "obsidian_note": "note.md",
        })
