**English** | [日本語](python.ja.md)

# Python

`python/` is not a thin client over the Rust binary. It is a **full second
implementation**: it computes the identity, it writes run directories, and it
reads them back. A run recorded from Python is the same run directory the Rust
reference would have written.

That is deliberate. The vectors in `schema/v1/testvectors/` only prove something
if two implementations reach them separately, so the hash primitives here are
kept independent of the Rust ones rather than bound to them.

## Install

The package lives in the `python/` subdirectory of this repository, and the
import name is `runvault`:

```toml
[project]
dependencies = ["runvault"]

[tool.uv.sources]
runvault = { git = "https://github.com/akitenkrad/rs-runvault", subdirectory = "python" }
```

The writer depends only on `blake3` and `pydantic`. The analysis helpers in
`runvault.read` need pandas, which is behind the `read` extra:

```toml
dependencies = ["runvault[read]"]
```

A repository that records runs but never analyses them is therefore not made to
install pandas and its wheels.

## Writing a run

```python
from runvault import Run

with Run.start(
    "schelling",                       # experiment
    "main",                            # subcommand
    repo_id="social-simulation-replications",
    domain="simulation",
    parameters=cfg,
    hash_exclude=["/output_dir", "/log_level"],
    seed_pointers=["/seed"],
    master_seed=42,
) as run:
    run.log_metric("segregation_index", 0.834, step=120, step_unit="step")
    run.log_metrics_at(120, "step", "group", {"share_a": 0.51, "share_b": 0.49})
    run.log_event("observation", record)
```

Leaving the block normally finishes the run; leaving it by an exception records
the run as failed, with the exception as the reason. A run that is neither — one
whose interpreter exits without `finish()` — records itself as failed too. Only
a killed process escapes that, which is what the lock file's heartbeat and
`runvault gc` are for.

Every option `RunOptions` carries can be passed to `Run.start` as a keyword;
`Run.from_options(options)` takes a `RunOptions` built separately.

### The recording methods

| Call | Records |
| --- | --- |
| `run.log_metric(name, value, *, step=None, step_unit=None, scope="run")` | one number |
| `run.log_metrics(scope, values)` | several aggregate metrics sharing a scope, flushed once |
| `run.log_metrics_at(step, step_unit, scope, values)` | the same, on a time axis |
| `run.log_reference(name, value, *, target_id, source, step=None, step_unit=None, scope="run")` | the value the source publication reports |
| `run.log_event(kind, payload)` | one line of `events.jsonl` |

`log_metrics` exists because one call per number means one flush per number,
which shows on a run recording a handful of metrics on every step of a long
simulation.

`log_reference` needs a `source` saying where the number was read, and a
`target_id` that is already declared in `research.targets[]`. A value read off a
figure has no place here: recording an estimate as a reported value makes the two
indistinguishable afterwards.

`run.dir`, `run.artifacts`, `run.run_uid`, `run.run_slug`, `run.meta` and
`run.sweep_id` expose what the run turned out to be.

## Reading runs back

`runvault.read` is the other half: which run, and how the envelope opens, in one
place, so that every analysis script agrees on both. It also reads the layouts
that predate runvault — a flat `config.json`, a wide `metrics.csv`, no
`run.json` — because results already on disk do not get rewritten.

```python
from runvault import read

run_dir = read.runvault_path("schelling", subcommand="simulate", standalone=True)
params  = read.config_parameters(run_dir)
scores  = read.run_scope_metrics(run_dir)
events  = read.events_table(run_dir)
```

| Function | Returns |
| --- | --- |
| `runvault_path(experiment, results_root="results", subcommand=None, standalone=False)` | the most recent finished run, via `runvault path --latest` |
| `runvault_binary()` | the `runvault` executable the above will use |
| `config_parameters(run_dir, *, required=True)` | the conditions, out of the envelope |
| `load_run_meta(run_dir, *, required=True)` | `run.json` |
| `run_subcommand(run_dir)` | which subcommand the run was an execution of |
| `artifacts_dir(run_dir)` | where the run's own output went |
| `figures_dir(run_dir)` | where to draw *afterwards* — outside the run directory |
| `metrics_wide(metrics_path)` | `metrics.csv` pivoted wide |
| `run_scope_metrics(run_dir)` | the run's single-number metrics |
| `scope_metrics_from_csv(metrics_path)` | the same, from a path to a `metrics.csv` |
| `events_table(run_dir, kind="terminal")` | `events.jsonl` as a DataFrame |
| `sweep_children(parent_dir)` | a sweep parent's children, by `lineage.parent_run_uid` |
| `sweep_summary_table(sweep_dir, parameter_keys, metric_names=None)` | one row per condition |
| `sweep_events_table(sweep_dir, parameter_keys, kind="terminal")` | one row per trial |

Two of these encode rules worth knowing before writing an analysis script:

- **`standalone=True`.** A sweep child runs the same subcommand as a run started
  by hand, so `subcommand="simulate"` alone returns *the last child that ran*.
  Pass `standalone=True` whenever the run of interest is a single one.
- **`figures_dir` is not `artifacts_dir`.** `manifest.csv` is settled by
  `finish()`. A figure drawn after the run ended would carry no hash and is not
  part of the record, so `figures_dir` puts it beside the run —
  `<results_root>/<experiment>/figures/<run_slug>/` — where it cannot contradict
  the manifest.

`sweep_summary_table` and `sweep_events_table` rebuild tables runvault keeps
nowhere on disk, from the children's `config.json` and `metrics.csv`. Every row
carries its `run_dir`, so a caller never has to compose a directory name out of
the conditions. `sweep_children` scans the parent's neighbours itself rather than
shelling out to `runvault path --children-of`, so that reading a run needs
nothing but the directory — analysis scripts run where the binary has not been
built.

## The Pydantic models

`python/src/runvault/models/` is **generated** from `schema/v1/*.json` by
`tools/gen_pydantic.py`, and committed. CI regenerates them and fails on a diff:
"it can be generated" is not the same as "it matches". Change the schema, then
regenerate — never the other way round.

```bash
uv run --with datamodel-code-generator python tools/gen_pydantic.py
```

## Tests

```bash
cd python && uv run --group dev pytest -q
```

They cover the primitives, the identity, the writer, the read side, the test
vectors, conformance to the schemas, and reading a fixture written by the Rust
implementation.
