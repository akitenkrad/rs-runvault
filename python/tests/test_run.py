"""The writer: one execution, one directory, append-only."""
from __future__ import annotations

import csv
import json
from pathlib import Path
from typing import Any

import pytest

from conftest import start
from runvault import Run, files, ids, lockfile
from runvault import run as run_module
from runvault.errors import SpecError, VerifyError

TERMINAL = {"unit_id": "u1", "t": 4, "t_unit": "turn", "outcome": "jailbroken",
            "censored": False, "budget": 8}


def read_json(path: Path) -> Any:
    return json.loads(path.read_text(encoding="utf-8"))


def rows(path: Path) -> list[dict[str, str]]:
    with path.open(encoding="utf-8", newline="") as handle:
        return list(csv.DictReader(handle))


@pytest.fixture
def lock_when_status_written(monkeypatch: pytest.MonkeyPatch) -> list[bool]:
    """Whether `.runvault.lock` still existed each time `status.json` was written.

    The ordering is the invariant, not the end state: a run whose lock outlives
    its status is a finished run that looks like it is still going.
    """
    seen: list[bool] = []
    original = files.write_atomically

    def spy(path: Path, data: bytes) -> None:
        path = Path(path)
        if path.name == "status.json":
            seen.append((path.parent / lockfile.LOCK_FILE).exists())
        original(path, data)

    monkeypatch.setattr(files, "write_atomically", spy)
    return seen


# --- the happy path -------------------------------------------------------


def test_a_finished_run_writes_every_file_under_one_run_uid(results: Path) -> None:
    run = start(results)
    (run.artifacts / "final.txt").write_text("done", encoding="utf-8")
    run.log_metric("segregation_index", 0.834, step=120, step_unit="step")
    run.log_event("note", {"message": "halfway"})
    directory = run.finish()

    meta = read_json(directory / "run.json")
    uid = meta["run_uid"]
    assert read_json(directory / "config.json")["run_uid"] == uid
    assert read_json(directory / "status.json")["run_uid"] == uid
    assert {row["run_uid"] for row in rows(directory / "metrics.csv")} == {uid}
    assert {row["run_uid"] for row in rows(directory / "manifest.csv")} == {uid}
    events = [json.loads(line) for line in (directory / "events.jsonl").read_text("utf-8").splitlines()]
    assert {event["run_uid"] for event in events} == {uid}

    status = read_json(directory / "status.json")
    assert status["state"] == "finished"
    assert status["counts"] == {"metrics": 1, "events": 1, "artifacts": 1}
    assert not (directory / lockfile.LOCK_FILE).exists()


def test_the_directory_name_carries_the_two_hashes(results: Path) -> None:
    directory = start(results).finish()
    meta = read_json(directory / "run.json")
    assert directory.name == meta["run_slug"]
    cfg8, exec4 = ids.slug_hash_prefixes(meta["run_slug"])
    assert meta["config_hash"].startswith(cfg8)
    assert meta["execution_hash"].startswith(exec4)


def test_origin_and_visibility_are_always_written(results: Path) -> None:
    meta = read_json(start(results).finish() / "run.json")
    assert meta["origin"] == "code"
    assert meta["visibility"] == "internal"


def test_a_metric_row_carries_its_axis_and_scope(results: Path) -> None:
    run = start(results)
    run.log_metric("asr", 0.25)
    run.log_metrics_at(3, "turn", "trial", {"tokens_in": 120, "tokens_out": 34})
    directory = run.finish()
    written = rows(directory / "metrics.csv")
    assert written[0] == {"run_uid": written[0]["run_uid"], "step": "", "step_unit": "",
                          "scope": "run", "name": "asr", "value": "0.25"}
    assert [row["name"] for row in written[1:]] == ["tokens_in", "tokens_out"]
    assert {row["step"] for row in written[1:]} == {"3"}


def test_the_manifest_is_a_snapshot_taken_by_finish(results: Path) -> None:
    run = start(results)
    (run.artifacts / "before.txt").write_text("in", encoding="utf-8")
    directory = run.finish()
    (directory / "artifacts" / "after.txt").write_text("out", encoding="utf-8")
    assert [row["path"] for row in rows(directory / "manifest.csv")] == ["artifacts/before.txt"]


# --- teardown, on every path ----------------------------------------------


def test_finish_removes_the_lock_before_writing_the_status(
    results: Path, lock_when_status_written: list[bool]
) -> None:
    start(results).finish()
    assert lock_when_status_written == [False]


def test_fail_removes_the_lock_before_writing_the_status(
    results: Path, lock_when_status_written: list[bool]
) -> None:
    directory = start(results).fail("budget", "ran out of tokens")
    assert lock_when_status_written == [False]
    status = read_json(directory / "status.json")
    assert status["state"] == "failed"
    assert status["error"] == {"kind": "budget", "message": "ran out of tokens"}


def test_an_exception_out_of_the_with_block_ends_the_run_as_failed(
    results: Path, lock_when_status_written: list[bool]
) -> None:
    with pytest.raises(ZeroDivisionError):
        with start(results) as run:
            directory = run.dir
            1 / 0
    assert lock_when_status_written == [False]
    status = read_json(directory / "status.json")
    assert status["state"] == "failed"
    assert status["error"]["kind"] == "exception"
    assert "ZeroDivisionError" in status["error"]["message"]
    assert not (directory / lockfile.LOCK_FILE).exists()


def test_a_with_block_that_returns_normally_finishes_the_run(results: Path) -> None:
    with start(results) as run:
        directory = run.dir
    assert read_json(directory / "status.json")["state"] == "finished"


def test_a_start_that_fails_after_the_directory_exists_leaves_a_record(
    results: Path, monkeypatch: pytest.MonkeyPatch, lock_when_status_written: list[bool]
) -> None:
    # Without this the directory has neither a status nor a lock, and `gc` walks
    # straight past it: the half-written run stays for good.
    class Broken:
        @staticmethod
        def start(run_dir: Path, record: object) -> None:
            raise OSError("disk full")

    monkeypatch.setattr(run_module, "Heartbeat", Broken)
    with pytest.raises(OSError):
        start(results)

    directory = next((results / "schelling").iterdir())
    status = read_json(directory / "status.json")
    assert status["state"] == "failed"
    assert status["error"]["kind"] == "start"
    assert "disk full" in status["error"]["message"]
    assert lock_when_status_written == [False]
    assert not (directory / lockfile.LOCK_FILE).exists()


def test_the_interpreter_exit_handler_uses_the_same_teardown(
    results: Path, lock_when_status_written: list[bool]
) -> None:
    run = start(results)
    run._on_interpreter_exit()
    assert lock_when_status_written == [False]
    status = read_json(run.dir / "status.json")
    assert status["state"] == "failed"
    assert status["error"]["kind"] == "dropped"


# --- latest_finished ------------------------------------------------------


def test_latest_finished_points_at_the_run_that_finished(results: Path) -> None:
    directory = start(results).finish()
    link = results / "schelling" / "latest_finished"
    assert link.resolve() == directory.resolve()


def test_latest_finished_never_points_at_a_failed_run(results: Path) -> None:
    start(results).fail("budget", "ran out")
    assert not (results / "schelling" / "latest_finished").exists()

    first = start(results).finish()
    start(results, parameters={"n": 4}).fail("budget", "ran out")
    assert (results / "schelling" / "latest_finished").resolve() == first.resolve()


# --- what a row may claim -------------------------------------------------


def test_a_contradictory_terminal_row_never_reaches_the_file(results: Path) -> None:
    run = start(results)
    with pytest.raises(SpecError):
        run.log_event("terminal", {**TERMINAL, "t": 9})
    with pytest.raises(SpecError):
        run.log_event("terminal", {**TERMINAL, "censored": True, "t": 4})
    run.finish()
    assert not (run.dir / "events.jsonl").exists()


def test_a_censored_terminal_row_at_the_budget_is_accepted(results: Path) -> None:
    run = start(results)
    run.log_event("terminal", {**TERMINAL, "censored": True, "t": 8})
    run.finish()
    assert (run.dir / "events.jsonl").read_text("utf-8").count("\n") == 1


def test_an_observation_without_its_unit_is_refused(results: Path) -> None:
    run = start(results)
    with pytest.raises(SpecError, match="t_unit"):
        run.log_event("observation", {"unit_id": "u1", "t": 1})
    run.fail("test", "done")


def test_an_event_kind_outside_the_vocabulary_is_refused(results: Path) -> None:
    run = start(results)
    with pytest.raises(SpecError):
        run.log_event("whatever", {"a": 1})
    run.log_event("x.rs-runvault.probe", {"a": 1})
    run.finish()


def test_a_reserved_metric_at_the_wrong_scope_is_refused(results: Path) -> None:
    run = start(results)
    with pytest.raises(SpecError, match="cost_usd"):
        run.log_metric("cost_usd", 1.5, scope="trial")
    run.log_metric("cost_usd", 1.5)
    run.finish()


def test_a_missing_measurement_is_not_written_as_a_number(results: Path) -> None:
    run = start(results)
    with pytest.raises(SpecError):
        run.log_metric("asr", float("nan"))
    run.fail("test", "done")


def test_a_reference_needs_a_target_that_the_run_declares(results: Path) -> None:
    replication = {
        "is_replication": True,
        "work": {"work_id": "arxiv:2405.01234", "arxiv_id": "2405.01234",
                 "title": "A paper", "source_version": "arXiv v1"},
        "targets": [{"target_id": "tbl3-r2", "kind": "table", "label": "Table 3", "row": "2"}],
        "obsidian_note": "研究/98_論文レポート/80-再現実験/P00000009/設計書.md",
    }
    run = start(results, research=replication)
    with pytest.raises(SpecError):
        run.log_reference("asr", 0.85, target_id="tbl9", source="Table 9")
    run.log_reference("asr", 0.85, target_id="tbl3-r2", source="Table 3 row 2")
    directory = run.finish()
    assert rows(directory / "reference.csv")[0]["source"] == "Table 3 row 2"


# --- what may be left out -------------------------------------------------


def test_a_sweep_parent_may_omit_the_seed_but_a_child_may_not(results: Path) -> None:
    parent = start(results, subcommand="sweep", domain="simulation", sweep_parent=True,
                   seed_pointers=["/seeds"], parameters={"seeds": [1, 2, 3]})
    assert parent.sweep_id == parent.run_slug
    parent_uid = parent.run_uid
    parent.finish()

    with pytest.raises(SpecError, match="master_seed"):
        start(results, domain="simulation", parameters={"seed": 1},
              lineage={"sweep_id": parent.run_slug, "parent_run_uid": parent_uid})

    child = start(results, domain="simulation", parameters={"seed": 1}, master_seed=1,
                  lineage={"sweep_id": parent.run_slug, "parent_run_uid": parent_uid})
    assert read_json(child.finish() / "run.json")["rng"]["master_seed"] == 1


def test_an_analysis_run_records_no_seed_at_all(results: Path) -> None:
    meta = read_json(start(results).finish() / "run.json")
    assert "rng" not in meta


def test_a_domain_that_needs_a_model_refuses_a_run_without_one(results: Path) -> None:
    with pytest.raises(SpecError, match="llm"):
        start(results, domain="llm-safety")


# --- names and collisions -------------------------------------------------


@pytest.mark.parametrize("field", ["repo_id", "experiment", "subcommand", "domain"])
def test_a_path_component_must_match_the_slug_grammar(results: Path, field: str) -> None:
    with pytest.raises(SpecError):
        start(results, **{field: "../escape"})


def test_a_colliding_directory_name_gets_the_next_suffix(
    results: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    # A parallel sweep of the same condition with different seeds starts many
    # runs in the same second, and every one of them wants this same name.
    monkeypatch.setattr(run_module.ids, "timestamp_part", lambda now: "20260830_101500")
    first = start(results).finish()
    second = start(results).finish()
    assert second.name == f"{first.name}-2"
    assert read_json(second / "status.json")["collision_index"] == 2
    assert read_json(first / "status.json")["collision_index"] is None


# --- the code and the environment ----------------------------------------


def test_a_dirty_working_tree_is_recorded_with_the_hash_of_its_diff(
    results: Path, repo: Path
) -> None:
    (repo / "a.txt").write_text("two", encoding="utf-8")
    code = read_json(start(results).finish() / "run.json")["code"]
    assert code["git_dirty"]
    assert len(code["dirty_hash"]["value"]) == 64
    assert len(code["git_commit"]) == 40


def test_a_lock_file_is_copied_into_the_run_and_hashed(results: Path, repo: Path) -> None:
    (repo / "uv.lock").write_text("# lock", encoding="utf-8")
    directory = start(results).finish()
    locks = read_json(directory / "run.json")["code"]["locks"]
    assert [lock["file"] for lock in locks] == ["lock/uv.lock"]
    assert (directory / "lock/uv.lock").read_text(encoding="utf-8") == "# lock"


def test_a_run_that_no_code_produced_needs_no_repository(tmp_path: Path) -> None:
    outside = tmp_path / "notes"
    outside.mkdir()
    run = Run.start("hand-measurement", "main", repo_id="rs-runvault", domain="other",
                    origin="manual", results_root=outside / "results", parameters={"n": 1})
    meta = read_json(run.finish() / "run.json")
    assert meta["code"] is None
    assert meta["env"]["python_version"] is not None


def test_the_environment_is_hashed_without_the_machine_name(results: Path) -> None:
    env = read_json(start(results).finish() / "run.json")["env"]
    assert len(env["env_hash"]) == 64
    assert env["host"]
    assert env["rustc_version"] is None


# --- finish refuses to call a broken run finished -------------------------


def test_finish_marks_the_run_failed_when_the_shallow_checks_do_not_pass(
    results: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    run = start(results)
    run.log_metric("asr", 0.5)
    # A row that claims another run's uid is exactly what the shallow check is for.
    with (run.dir / "metrics.csv").open("a", encoding="utf-8", newline="") as handle:
        handle.write("01K3QZ8F7H9M2N4P6R8T0V2X4Z,,,run,asr,0.5\n")
    with pytest.raises(VerifyError):
        run.finish()
    status = read_json(run.dir / "status.json")
    assert status["state"] == "failed"
    assert status["error"]["kind"] == "verify"
    assert not (results / "schelling" / "latest_finished").exists()
