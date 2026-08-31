"""Reading a run directory back: choosing a run, and opening the envelope.

The writer puts one execution in one directory:

    <results_root>/<experiment>/<run_slug>/
    ├── run.json          ← the run's metadata (lineage / rng / research …)
    ├── config.json       ← an envelope; the conditions sit under ["parameters"]
    ├── metrics.csv       ← long form (run_uid, step, step_unit, scope, name, value)
    ├── events.jsonl      ← one line per observed unit
    ├── status.json
    ├── manifest.csv
    └── artifacts/        ← what the experiment wrote *while it ran*

This module is the other half: it collects "which run" and "how the envelope
opens" in one place, so that every analysis script agrees on both. It also reads
the layouts that predate runvault (a flat `config.json`, a wide `metrics.csv`,
no `run.json`), because the results already on disk do not get rewritten.

This module needs pandas, which the writer does not; it is installed by the
`read` extra and is never imported by `runvault/__init__.py`. A repository that
only records runs is not made to install it.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
from typing import Iterable, Sequence

import pandas as pd

__all__ = [
    "artifacts_dir",
    "config_parameters",
    "events_table",
    "figures_dir",
    "load_run_meta",
    "metrics_wide",
    "run_scope_metrics",
    "run_subcommand",
    "runvault_binary",
    "runvault_path",
    "scope_metrics_from_csv",
    "sweep_children",
    "sweep_events_table",
    "sweep_summary_table",
]


# --------------------------------------------------------------------------- #
# choosing a run
# --------------------------------------------------------------------------- #

def runvault_binary() -> str:
    """The `runvault` executable, preferring the `RUNVAULT` environment variable.

    An environment where it has not been `cargo install`ed is the normal case,
    so PATH alone is not enough to go on.
    """
    override = os.environ.get("RUNVAULT")
    if override:
        if not os.path.isfile(override) or not os.access(override, os.X_OK):
            raise SystemExit(
                f"error: RUNVAULT does not point at an executable file: {override}"
            )
        return override
    found = shutil.which("runvault")
    if found is None:
        raise SystemExit(
            "error: the `runvault` command was not found.\n"
            "  Use one of:\n"
            "    - put it on PATH: cargo install --path <rs-runvault>/crates/runvault\n"
            "    - point at the binary: RUNVAULT=<rs-runvault>/target/debug/runvault ...\n"
            "    - pass the run directory directly"
        )
    return found


def _runvault_paths(args: list[str], what: str) -> list[str]:
    """Call `runvault path` and return the lines it printed."""
    cmd = [runvault_binary(), "path"] + args
    proc = subprocess.run(cmd, capture_output=True, text=True)
    if proc.returncode != 0:
        raise SystemExit(
            f"error: no {what} ({' '.join(cmd)})\n  {proc.stderr.strip()}"
        )
    lines = [line for line in proc.stdout.strip().splitlines() if line]
    if not lines:
        raise SystemExit(f"error: runvault returned nothing ({' '.join(cmd)})")
    return lines


def runvault_path(
    experiment: str,
    results_root: str = "results",
    subcommand: str | None = None,
    standalone: bool = False,
) -> str:
    """The path of the most recent finished run, via `runvault path --latest`.

    `subcommand` narrows the search to runs of that subcommand. A sweep's parent
    and its children live in the same experiment directory, so without it
    `--latest` can return the parent, which holds no per-step metrics.

    `standalone=True` narrows it further to runs that belong to no sweep. A
    sweep child runs the same subcommand as one started by hand, so
    `--subcommand simulate` alone returns *the last child that ran*. Pass it
    whenever the run of interest is a single one.
    """
    args = [
        "--results-root", str(results_root),
        "--experiment", experiment,
        "--latest",
    ]
    if subcommand is not None:
        args += ["--subcommand", subcommand]
    if standalone:
        args += ["--standalone"]
    return _runvault_paths(args, "finished run")[0]


# --------------------------------------------------------------------------- #
# opening the envelope
# --------------------------------------------------------------------------- #

def _has_run_json(run_dir: str | os.PathLike) -> bool:
    """Whether this directory was written by runvault rather than by a legacy run.

    Deliberately not `paths.is_run_dir`, which answers a different question: that
    one counts `status.json` and the lock as well, so that it *finds* legacy runs.
    Here the point is to tell them apart.
    """
    return os.path.exists(os.path.join(str(run_dir), "run.json"))


def config_parameters(
    run_dir: str | os.PathLike, *, required: bool = True
) -> dict | None:
    """The experiment's conditions, from the run directory's `config.json`.

    runvault's `config.json` is the envelope `{schema_version, run_uid, runvault,
    parameters}`, and the conditions are under `parameters`. A legacy flat
    `config.json` has no envelope and is returned as it stands.

    A missing file raises `FileNotFoundError`. Pass `required=False` to get
    `None` instead — for a caller that displays whatever a directory happens to
    hold, and for which "no config" is an answer rather than a failure.
    """
    path = os.path.join(str(run_dir), "config.json")
    if not os.path.exists(path):
        if required:
            raise FileNotFoundError(f"config.json not found: {path}")
        return None
    with open(path) as f:
        doc = json.load(f)
    if isinstance(doc, dict) and "parameters" in doc:
        return doc["parameters"]
    return doc


def load_run_meta(run_dir: str | os.PathLike, *, required: bool = True) -> dict | None:
    """`run.json`.

    A missing file raises `FileNotFoundError`: a directory without one is not a
    runvault run. Pass `required=False` to get `None` instead, which is how a
    caller asks "is this a runvault run, and if so what does it say".
    """
    path = os.path.join(str(run_dir), "run.json")
    if not os.path.exists(path):
        if required:
            raise FileNotFoundError(
                f"run.json not found (not a runvault run directory): {path}"
            )
        return None
    with open(path) as f:
        return json.load(f)


def run_subcommand(run_dir: str | os.PathLike) -> str:
    """Which subcommand this run was an execution of (`simulate` / `sweep` / …)."""
    meta = load_run_meta(run_dir)
    assert meta is not None  # required=True raises rather than returning None
    return str(meta["subcommand"])


def artifacts_dir(run_dir: str | os.PathLike) -> str:
    """Where the experiment's own output went (snapshots, analysis CSVs).

    Under `artifacts/` for a runvault run. `manifest.csv` is settled by
    `finish()` walking `artifacts/` and `logs/`, so only what was written before
    the run ended belongs here. **A figure drawn afterwards must not go here**:
    it would carry no hash and it is not part of the record. Draw into
    [`figures_dir`] instead.

    A legacy run has no `run.json` and no `artifacts/`, so the run directory
    itself is returned.
    """
    run_dir = str(run_dir)
    if _has_run_json(run_dir):
        return os.path.join(run_dir, "artifacts")
    return run_dir


def figures_dir(run_dir: str | os.PathLike) -> str:
    """Where to draw. What is made after a run ended is not part of its record.

    Returns `<results_root>/<experiment>/figures/<run_slug>/`, outside the run
    directory, so that it cannot disagree with the `manifest.csv` that `finish()`
    settled. A legacy run has no reason to keep figures outside itself, so it
    keeps the older `<run>/figures`.
    """
    run_dir = os.path.abspath(str(run_dir))
    if not _has_run_json(run_dir):
        return os.path.join(run_dir, "figures")
    experiment_dir = os.path.dirname(run_dir)
    return os.path.join(experiment_dir, "figures", os.path.basename(run_dir))


# --------------------------------------------------------------------------- #
# metrics.csv
# --------------------------------------------------------------------------- #

def _is_long(df: pd.DataFrame) -> bool:
    return {"name", "value", "step"}.issubset(df.columns)


def metrics_wide(metrics_path: str | os.PathLike) -> pd.DataFrame:
    """The per-step metrics, one row per step.

    Only the long-form rows that carry a step are used; the ones without a step
    (a `scope=run` `converged` or `final_iteration`) belong to
    [`scope_metrics_from_csv`]. A legacy wide `metrics.csv` is already in this
    shape and is returned unchanged.
    """
    path = str(metrics_path)
    if not os.path.exists(path):
        raise FileNotFoundError(f"metrics.csv not found: {path}")
    df = pd.read_csv(path)
    if not _is_long(df):
        return df
    stepped = df[df["step"].notna()]
    return (
        stepped.pivot_table(index="step", columns="name", values="value", aggfunc="last")
        .reset_index()
        .rename_axis(None, axis=1)
        .astype({"step": int})
        .sort_values("step")
        .reset_index(drop=True)
    )


def run_scope_metrics(run_dir: str | os.PathLike) -> dict[str, float]:
    """The metrics that describe the whole run with one number each.

    Reads the run directory's `metrics.csv` and collects the long-form rows with
    no step. A run that recorded none — or none at all — is `{}`, so a caller
    can ask without first knowing whether the run had any.

    For a path to a `metrics.csv`, and for a legacy wide one, see
    [`scope_metrics_from_csv`]: it answers the same question but insists the
    file exists.
    """
    path = os.path.join(str(run_dir), "metrics.csv")
    if not os.path.exists(path):
        return {}
    df = pd.read_csv(path)
    if df.empty:
        return {}
    rows = df[df["step"].isna()]
    return {str(r["name"]): float(r["value"]) for _, r in rows.iterrows()}


def scope_metrics_from_csv(metrics_path: str | os.PathLike) -> dict[str, float]:
    """The same run-scope metrics, from a `metrics.csv` that must exist.

    Kept apart from [`run_scope_metrics`] on purpose. This one is for a caller
    that already located the file and treats its absence as a failure — turning
    that into an empty dict would let a plot draw nothing instead of stopping.

    A legacy wide `metrics.csv` has no run-scope rows, so the equivalent value is
    rebuilt from the last row (`converged` is not recorded there and is absent).
    """
    path = str(metrics_path)
    if not os.path.exists(path):
        raise FileNotFoundError(f"metrics.csv not found: {path}")
    df = pd.read_csv(path)
    if not _is_long(df):
        if df.empty:
            return {}
        return {"final_iteration": float(df["step"].iloc[-1])}
    rows = df[df["step"].isna()]
    return {str(r["name"]): float(r["value"]) for _, r in rows.iterrows()}


# --------------------------------------------------------------------------- #
# events.jsonl — one line per observed unit
# --------------------------------------------------------------------------- #

def events_table(run_dir: str | os.PathLike, kind: str | None = "terminal") -> pd.DataFrame:
    """`events.jsonl` as a DataFrame.

    `kind` selects on the `schema` column (`None` keeps every kind). The reserved
    keys (`unit_id` / `t` / `t_unit` / `outcome` / `censored` / `budget`) become
    columns under their own names, and an experiment's own fields sit on the same
    row.
    """
    path = os.path.join(str(run_dir), "events.jsonl")
    if not os.path.exists(path):
        raise FileNotFoundError(f"events.jsonl not found: {path}")
    rows: list[dict] = []
    with open(path) as f:
        for line in f:
            if not line.strip():
                continue
            row = json.loads(line)
            if kind is not None and row.get("schema") != kind:
                continue
            rows.append(row)
    if not rows:
        raise SystemExit(f"error: {path} holds no events with schema={kind}.")
    return pd.DataFrame(rows)


# --------------------------------------------------------------------------- #
# a sweep's lineage
# --------------------------------------------------------------------------- #

def sweep_children(parent_dir: str | os.PathLike) -> list[str]:
    """A sweep parent's child runs, matched on `lineage.parent_run_uid`.

    The children are not under the parent but beside it in the experiment
    directory, so the parent's neighbours are scanned and their lineage compared.
    A neighbour with no `run.json` is not a child and is skipped, which is also
    how a legacy directory sitting in the same experiment is passed over.

    The scan is done here rather than by `runvault path --children-of` so that
    reading a run needs nothing but the directory: an analysis script — and this
    package's own tests — run where the binary has not been built.

    Returns the paths in directory-name order, which is start-time order. An
    empty list when the parent has no children; the callers that cannot proceed
    without them say so themselves.
    """
    parent = os.path.abspath(str(parent_dir))
    meta = load_run_meta(parent)
    assert meta is not None  # required=True raises rather than returning None
    parent_uid = meta["run_uid"]

    experiment_dir = os.path.dirname(parent)
    children: list[str] = []
    for name in sorted(os.listdir(experiment_dir)):
        path = os.path.join(experiment_dir, name)
        if path == parent or not os.path.isdir(path) or os.path.islink(path):
            continue
        child = load_run_meta(path, required=False)
        if child is None:
            continue
        lineage = child.get("lineage") or {}
        if lineage.get("parent_run_uid") == parent_uid:
            children.append(path)
    return children


def _children_or_stop(sweep_dir: str | os.PathLike) -> list[str]:
    children = sweep_children(sweep_dir)
    if not children:
        raise SystemExit(
            f"error: no child runs belong to this sweep parent: {sweep_dir}\n"
            "  A child points at its parent through lineage.parent_run_uid;"
            " check that parent and children share a results root."
        )
    return children


def sweep_summary_table(
    sweep_dir: str | os.PathLike,
    parameter_keys: Sequence[str],
    metric_names: Iterable[str] | None = None,
) -> pd.DataFrame:
    """One row per condition.

    runvault keeps no such table on disk. It is rebuilt from the sweep parent's
    children: each child's `config.json` `parameters` and the run-scope metrics
    of its `metrics.csv`.

    Args:
        sweep_dir: the sweep parent's run directory.
        parameter_keys: the `/parameters` keys to make columns of (e.g.
            `["features", "traits"]`). They come out in this order and are the
            sort key.
        metric_names: the run-scope metrics to make columns of. `None` takes
            whatever the children recorded.

    Every row carries a `run_dir`, so a caller never has to compose a directory
    name out of the conditions.
    """
    rows: list[dict] = []
    for child in _children_or_stop(sweep_dir):
        params = config_parameters(child) or {}
        scoped = run_scope_metrics(child)
        wanted = list(scoped) if metric_names is None else list(metric_names)
        row: dict = {key: params.get(key) for key in parameter_keys}
        row.update({name: scoped.get(name) for name in wanted})
        row["run_dir"] = child
        rows.append(row)
    return (
        pd.DataFrame(rows)
        .sort_values(list(parameter_keys))
        .reset_index(drop=True)
    )


def sweep_events_table(
    sweep_dir: str | os.PathLike,
    parameter_keys: Sequence[str],
    kind: str = "terminal",
) -> pd.DataFrame:
    """One row per trial: the children's `events_table`s, with condition columns.

    A spread or a confidence interval needs the individual trials, not the
    condition's mean, and the trials live in `events.jsonl` rather than in the
    aggregate metrics — so the children's events are stacked.
    """
    frames: list[pd.DataFrame] = []
    for child in _children_or_stop(sweep_dir):
        params = config_parameters(child) or {}
        df = events_table(child, kind=kind)
        for key in parameter_keys:
            df[key] = params.get(key)
        df["run_dir"] = child
        frames.append(df)
    return (
        pd.concat(frames, ignore_index=True)
        .sort_values(list(parameter_keys) + ["unit_id"])
        .reset_index(drop=True)
    )
