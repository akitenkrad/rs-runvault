"""The read side: opening a run directory that the writer just wrote, and the
older layouts that predate it.

The fixtures for the current layout are built with the writer rather than by
hand, so that the two halves are held to the same file format: a reader tested
against a hand-written directory would keep passing after the writer changed.
The legacy fixtures have to be hand-written — nothing writes that layout any
more, and the branches that read it are exactly what would rot unnoticed.
"""
from __future__ import annotations

import json
import subprocess
import sys
from pathlib import Path

import pytest

from conftest import start
from runvault import read

TERMINAL = {"unit_id": "u1", "t": 4, "t_unit": "turn", "outcome": "jailbroken",
            "censored": False, "budget": 8}


def legacy_run(root: Path, *, steps: int = 3) -> Path:
    """A run directory from before runvault: flat config, wide metrics, no run.json."""
    run_dir = root / "legacy_run"
    run_dir.mkdir(parents=True)
    (run_dir / "config.json").write_text(
        json.dumps({"threshold": 0.3, "vacant_rate": 0.1, "seed": 7}), encoding="utf-8"
    )
    rows = ["step,avg_same_ratio,n_moved"]
    rows += [f"{i},{0.5 + i / 100},{10 - i}" for i in range(steps)]
    (run_dir / "metrics.csv").write_text("\n".join(rows) + "\n", encoding="utf-8")
    return run_dir


# --- the current layout ---------------------------------------------------


def test_a_run_written_by_the_writer_reads_back(results: Path) -> None:
    run = start(results, parameters={"threshold": 0.3, "seed": 7})
    run.log_metric("segregation_index", 0.8, step=0, step_unit="step")
    run.log_metric("segregation_index", 0.9, step=1, step_unit="step")
    run.log_metric("final_iteration", 42.0)
    directory = run.finish()

    assert read.config_parameters(directory) == {"threshold": 0.3, "seed": 7}
    assert read.run_subcommand(directory) == "main"
    assert read.load_run_meta(directory)["run_uid"] == run.run_uid
    assert read.run_scope_metrics(directory) == {"final_iteration": 42.0}

    wide = read.metrics_wide(directory / "metrics.csv")
    assert list(wide["step"]) == [0, 1]
    assert list(wide["segregation_index"]) == [0.8, 0.9]


def test_figures_are_drawn_outside_the_run_the_manifest_settled(results: Path) -> None:
    directory = start(results).finish()
    assert read.artifacts_dir(directory) == str(directory / "artifacts")
    assert read.figures_dir(directory) == str(
        results / "schelling" / "figures" / directory.name
    )


def test_a_run_that_recorded_no_run_scope_metrics_is_empty_not_an_error(results: Path) -> None:
    directory = start(results).finish()
    assert read.run_scope_metrics(directory) == {}


def test_a_run_directory_with_no_metrics_file_is_empty_not_an_error(tmp_path: Path) -> None:
    assert read.run_scope_metrics(tmp_path) == {}


# --- the two contracts that must not be merged ----------------------------


def test_the_csv_form_refuses_a_missing_file_where_the_run_form_shrugs(tmp_path: Path) -> None:
    # A caller that located the file itself treats its absence as a failure;
    # answering `{}` there would let a plot draw nothing instead of stopping.
    with pytest.raises(FileNotFoundError):
        read.scope_metrics_from_csv(tmp_path / "metrics.csv")
    assert read.run_scope_metrics(tmp_path) == {}


def test_the_csv_form_reads_the_same_rows_as_the_run_form(results: Path) -> None:
    run = start(results)
    run.log_metric("final_iteration", 12.0)
    directory = run.finish()
    assert read.scope_metrics_from_csv(directory / "metrics.csv") == read.run_scope_metrics(directory)


def test_a_missing_config_is_a_failure_unless_the_caller_says_otherwise(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        read.config_parameters(tmp_path)
    assert read.config_parameters(tmp_path, required=False) is None


def test_a_missing_run_json_is_a_failure_unless_the_caller_says_otherwise(tmp_path: Path) -> None:
    with pytest.raises(FileNotFoundError):
        read.load_run_meta(tmp_path)
    assert read.load_run_meta(tmp_path, required=False) is None


# --- the legacy layout ----------------------------------------------------


def test_a_legacy_flat_config_is_returned_as_it_stands(tmp_path: Path) -> None:
    directory = legacy_run(tmp_path)
    assert read.config_parameters(directory) == {
        "threshold": 0.3, "vacant_rate": 0.1, "seed": 7
    }
    assert read.load_run_meta(directory, required=False) is None


def test_a_legacy_run_keeps_its_output_and_its_figures_inside_itself(tmp_path: Path) -> None:
    directory = legacy_run(tmp_path)
    assert read.artifacts_dir(directory) == str(directory)
    assert read.figures_dir(directory) == str(directory / "figures")


def test_a_legacy_wide_metrics_file_is_already_the_shape_wanted(tmp_path: Path) -> None:
    wide = read.metrics_wide(legacy_run(tmp_path) / "metrics.csv")
    assert list(wide.columns) == ["step", "avg_same_ratio", "n_moved"]
    assert list(wide["step"]) == [0, 1, 2]


def test_a_legacy_run_has_its_final_iteration_rebuilt_from_the_last_row(tmp_path: Path) -> None:
    # `converged` was never recorded in the wide form, so it stays absent rather
    # than being invented.
    assert read.scope_metrics_from_csv(legacy_run(tmp_path) / "metrics.csv") == {
        "final_iteration": 2.0
    }


def test_an_empty_legacy_metrics_file_yields_nothing(tmp_path: Path) -> None:
    directory = tmp_path / "empty"
    directory.mkdir()
    (directory / "metrics.csv").write_text("step,avg_same_ratio\n", encoding="utf-8")
    assert read.scope_metrics_from_csv(directory / "metrics.csv") == {}


# --- events ---------------------------------------------------------------


def test_events_are_read_back_and_filtered_by_kind(results: Path) -> None:
    run = start(results)
    run.log_event("terminal", TERMINAL)
    run.log_event("terminal", {**TERMINAL, "unit_id": "u2", "t": 6})
    run.log_event("note", {"message": "not a trial"})
    directory = run.finish()

    df = read.events_table(directory, kind="terminal")
    assert list(df["unit_id"]) == ["u1", "u2"]
    assert set(df["schema"]) == {"terminal"}
    assert len(read.events_table(directory, kind=None)) == 3


def test_a_run_with_no_events_file_is_an_error(results: Path) -> None:
    directory = start(results).finish()
    with pytest.raises(FileNotFoundError):
        read.events_table(directory)


def test_asking_for_a_kind_no_event_has_stops_rather_than_returning_nothing(results: Path) -> None:
    run = start(results)
    run.log_event("note", {"message": "only notes here"})
    directory = run.finish()
    with pytest.raises(SystemExit):
        read.events_table(directory, kind="terminal")


# --- a sweep --------------------------------------------------------------


@pytest.fixture
def sweep(results: Path) -> Path:
    """A sweep parent and two children, plus a legacy directory beside them.

    The legacy neighbour is there on purpose: the children are found by scanning
    the parent's siblings, and a directory with no `run.json` must be passed over
    rather than crash the scan.
    """
    parent = start(results, subcommand="sweep", sweep_parent=True, parameters={"features": 5})
    parent_uid, sweep_id = parent.run_uid, parent.sweep_id
    parent_dir = parent.finish()

    for features, traits in ((5, 10), (10, 15)):
        child = start(
            results,
            parameters={"features": features, "traits": traits},
            lineage={"parent_run_uid": parent_uid, "sweep_id": sweep_id},
        )
        child.log_metric("convergence_rate", float(features) / 10)
        child.log_event("terminal", {**TERMINAL, "unit_id": f"trial-{features}"})
        child.finish()

    legacy_run(results / "schelling")
    return parent_dir


def test_the_children_are_found_through_their_lineage(sweep: Path) -> None:
    children = read.sweep_children(sweep)
    assert len(children) == 2
    assert all(read.load_run_meta(c)["lineage"]["parent_run_uid"] for c in children)


def test_a_parent_with_no_children_stops_the_caller_that_needs_them(results: Path) -> None:
    parent = start(results, subcommand="sweep", sweep_parent=True).finish()
    assert read.sweep_children(parent) == []
    with pytest.raises(SystemExit):
        read.sweep_summary_table(parent, ["features"])


def test_the_summary_table_is_rebuilt_from_the_children(sweep: Path) -> None:
    df = read.sweep_summary_table(sweep, ["features", "traits"])
    assert list(df["features"]) == [5, 10]
    assert list(df["traits"]) == [10, 15]
    assert list(df["convergence_rate"]) == [0.5, 1.0]
    assert all(Path(d).is_dir() for d in df["run_dir"])


def test_the_summary_table_keeps_a_metric_a_child_never_recorded_as_a_column(sweep: Path) -> None:
    df = read.sweep_summary_table(sweep, ["features"], metric_names=["never_logged"])
    assert list(df.columns) == ["features", "never_logged", "run_dir"]
    assert df["never_logged"].isna().all()


def test_the_events_of_every_child_stack_with_their_conditions(sweep: Path) -> None:
    df = read.sweep_events_table(sweep, ["features", "traits"])
    assert list(df["unit_id"]) == ["trial-5", "trial-10"]
    assert list(df["features"]) == [5, 10]


# --- the writer must not need pandas --------------------------------------


BLOCK_PANDAS = """
import importlib
import pkgutil
import sys


class NoPandas:
    def find_spec(self, name, path=None, target=None):
        if name == "pandas" or name.startswith("pandas."):
            raise ImportError("pandas is not installed")
        return None


sys.meta_path.insert(0, NoPandas())

import runvault

for module in pkgutil.walk_packages(runvault.__path__, "runvault."):
    if module.name == "runvault.read":
        continue
    importlib.import_module(module.name)

try:
    importlib.import_module("runvault.read")
except ImportError:
    print("ok")
else:
    raise SystemExit("the blocker did not block pandas, so this proved nothing")
"""


def test_the_writer_imports_with_pandas_absent() -> None:
    # A repository that only records runs installs `runvault` without the `read`
    # extra. If any writer module grew a module-scope `import pandas`, that
    # install would break, and nothing else here would notice.
    proc = subprocess.run(
        [sys.executable, "-c", BLOCK_PANDAS], capture_output=True, text=True
    )
    assert proc.returncode == 0, proc.stderr
    assert proc.stdout.strip() == "ok"
