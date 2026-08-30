# runvault

Plain-file experiment tracking for reproducible research.

One run is one directory. The directory is the source of truth; every tracking
UI on top of it (DuckDB index, MLflow, an Obsidian dashboard) is an optional,
removable index layer. Experiment code depends on `runvault` alone.

`runvault` is designed to record simulation (ABM), LLM safety evaluation and
anomaly-detection experiments in the *same* shape, so that runs can be compared
across repositories and across years.

## Status

Phase 0 (frozen schemas) and Phase 1 (the Rust crate) are done. `sync`, the
DuckDB index, the Python implementation and the dashboard are still to come.

## Usage

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
                .obsidian_note("研究/98_論文レポート/80-再現実験/P00000009/設計書.md"),
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

Write generated files under `run.dir()/artifacts/` (and logs under `logs/`):
`finish()` walks exactly those two trees into `manifest.csv`, so anything written
elsewhere in the run directory is not part of the record. A sweep parent is
driven by a list of seeds rather than one, so it declares `lineage.sweep_id` and
leaves `master_seed` unset; its children each carry their own.

A run dropped without `finish()` records itself as failed. A run whose process is
killed leaves a lock behind, and `runvault gc` turns that into a recorded
failure — the presence of the lock alone never means "still running".

### Command line

| | |
| --- | --- |
| `runvault path --experiment E --latest` | the last completed run |
| `runvault path --experiment E --config-hash 9f2c41ab` | every run of one condition |
| `runvault path --experiment E --execution-hash 3b1d --finished` | has this exact thing already run? |
| `runvault path --experiment E --latest --subcommand run` | the latest run of one subcommand |
| `runvault verify <run>` | the invariants that span a run's files |
| `runvault gc` | record the runs whose process was killed |

## Layout

| Path | Contents |
| --- | --- |
| `schema/v1/` | Frozen JSON Schemas, the flattened index columns, and the core vocabulary. These files are the specification. |
| `schema/v1/testvectors/` | Canonicalization and hash vectors, so two implementations can be shown to agree. |
| `crates/runvault/` | The Rust reference implementation and the `runvault` binary. |
| `tools/` | The validators that keep the schemas and the design note honest. |

The design note lives in the author's Obsidian vault:
`設計書/rs-runvault_実験管理基盤設計書.md`. It holds intent and constraints; the
shapes themselves live only in `schema/v1/`, so the two cannot drift apart.

## Checks

```bash
cargo test --all-features
uv run --with jsonschema --with rfc3339-validator python tools/test_schemas.py
uv run --with jsonschema --with rfc3339-validator python tools/check_design_doc.py
uv run --with blake3 python tools/gen_testvectors.py   # must not change a byte
```

`tools/gen_testvectors.py` is a second implementation of the canonicalization and
hashing rules, written from the design note. The vectors it emits are what the
Rust tests assert against: when the two disagree one of them is wrong, and
regenerating is not the fix.

## Known constraints

`crates/runvault` reads `schema/v1/vocabulary.toml` with `include_str!` from
outside its own directory, so the crate is not publishable to crates.io as it
stands. That matters only when publishing, which is not yet planned.

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.

The run directories under `crates/runvault/tests/fixtures/legacy/` are outputs of
the author's own replication of Schelling (1971) and of a small opinion-dynamics
model, trimmed to a few rows each. They are test data, not results.
