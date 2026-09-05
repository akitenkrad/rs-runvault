**English** | [日本語](rust.ja.md)

# Rust

```toml
[dependencies]
runvault = { git = "https://github.com/akitenkrad/rs-runvault" }
```

Depending on the library to write runs pulls in no database.

## Recording a run

```rust
use runvault::{Run, RunOptions, Target, Work};

let mut run = Run::start(
    RunOptions::new("schelling", "main")          // experiment, subcommand
        .repo_id("social-simulation-replications")
        .domain("simulation")
        .parameters(&cfg)?
        .hash_exclude(["/output_dir", "/log_level"])
        .seed_pointers(["/seed"])
        .master_seed(42)
        .replication(
            Work::doi("10.1080/0022250X.1971.9989794")
                .title("Dynamic Models of Segregation")
                .source_version("published")
                .target(Target::table("tbl3-r2", "Table 3").row("2"))
                .obsidian_note("notes/replications/segregation.md"),
        ),
)?;

run.log_metric("segregation_index", 0.834).step(120, "step").send()?;
run.log_reference("segregation_index", 0.850)
    .target("tbl3-r2")
    .source("Table 3 row 2")
    .send()?;
run.log_event("observation", &record)?;
run.finish()?;
```

A minimal run needs less than that:

```rust
use runvault::{Run, RunOptions};

let cfg = serde_json::json!({ "rows": 13, "cols": 16, "seed": 42 });
let mut run = Run::start(
    RunOptions::new("schelling", "main")
        .repo_id("social-simulation-replications")
        .domain("simulation")
        .parameters(&cfg)?
        .seed_pointers(["/seed"])
        .master_seed(42),
)?;
run.log_metric("segregation_index", 0.834).step(120, "step").send()?;
run.finish()?;
```

`log_reference` records the value the source publication reports, so the
difference against the replication can be computed later instead of remembered.
Its `target` has to be one of the targets declared in the replication.

## What `finish()` records

Write generated files under `run.dir()/artifacts/` and logs under `logs/`:
`finish()` walks exactly those two trees into `manifest.csv`, so anything
written elsewhere in the run directory is not part of the record.

`manifest.csv` is written once, by `finish()`. Anything added to the run
directory afterwards is not in it, so a figure drawn later belongs beside the
run rather than inside it.

## Progress

A subcommand that can run for more than a minute reports what it is doing.
Nothing else in the run directory says whether a long silence is work or a wedge,
and telling the two apart with `ps` is a diagnosis made outside the program.

```rust
let mut stage = run.stage("stage 2", conditions.len());
for condition in &conditions {
    let value = evaluate(condition);
    run.log_metric("segregation_index", value).send()?;
    stage.tick();
}
stage.close();
```

```
progress: stage 2          200/4000     5%  elapsed      12s  eta    3m54s
progress: stage 2         4000/4000   100%  elapsed    4m06s  done
```

Lines go to **standard error**, leaving standard output for the run's
machine-readable results, and the same lines are mirrored into
`logs/progress.log`, which `finish()` hashes into `manifest.csv`. Every line is
flushed as it is written and `isatty` is never consulted: a run whose output is
redirected is exactly the run whose progress is worth having. A stage reports
every five per cent of its total and, whichever comes first, at least every
thirty seconds.

The caller says how much work there is and then says "one more"; it never formats
a line, chooses a stream or decides when to report. A `Stage` borrows nothing, so
the run stays free to record metrics inside the loop being reported on.

Two variants cover the stages a count does not describe:

| Call | For |
| --- | --- |
| `run.weighted_stage(name, costs)` | Conditions that cost different amounts. `costs` is one figure per condition in tick order, in any unit proportional to time; the percentage and the estimate are shares of the cost rather than of the count. A stage whose conditions span orders of magnitude reports "19s" with half an hour left if it counts. |
| `run.unbounded_stage(name)` | Work that cannot be counted first. It reports its tally on a timer and carries neither a percentage nor an estimate, rather than inventing a denominator. |

Progress is not a metric. `metrics.csv` holds the quantities the experiment is
about; how long the run took is `status.json`'s `duration_sec`, and a second
answer to that question in another file is a second answer that can disagree.

Close a stage before `finish()`. The manifest is written there, and a line added
afterwards is a line the manifest disagrees with — so a stage left open past the
end of its run keeps reporting to standard error, says so once, and stops writing
into the directory.

## Sweeps

A sweep parent is driven by a list of seeds rather than one, so it is declared
with `RunOptions::sweep_parent()` — which fills in `lineage.sweep_id` with the
run's own slug — and leaves `master_seed` unset. Its children each carry their
own.

## Failure

A run dropped without `finish()` records itself as failed. A run whose process is
killed leaves a lock behind, and `runvault gc` turns that into a recorded
failure — the presence of the lock alone never means "still running".

## The two crates

| Crate | What it is |
| --- | --- |
| `crates/runvault` | The library. It writes and verifies runs, and carries no DuckDB. |
| `crates/runvault-cli` | The `runvault` binary. It bundles DuckDB for the index behind `query`. |

The split is deliberate. Around two dozen replication repositories depend on the
library in order to record runs; making them compile a database to do that would
be a cost paid by everyone for a feature only the aggregation side uses.

The library also exposes a `schema-gen` feature, which emits JSON Schema from the
Rust types so that tests can compare them against `schema/v1/*.json`. It is off
by default and is only needed to run the schema-parity test.

## Known constraint

`crates/runvault` reads `schema/v1/vocabulary.toml` with `include_str!` from
outside its own directory, so the crate is not publishable to crates.io as it
stands. That matters only when publishing, which is not yet planned.
