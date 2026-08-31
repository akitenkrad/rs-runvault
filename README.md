<p align="center"><img src="docs/assets/hero.svg" width="100%"></p>

**English** | [日本語](README.ja.md)

# runvault

Plain-file experiment tracking for reproducible research.

One run is one directory, and the directory is the source of truth. Every layer
above it — a DuckDB index, a dashboard, any other tracking UI — reads those files
and can be removed without touching the record. What makes runs comparable is
not a naming convention but three stacked hashes: `env_hash` (the machine and
the toolchain), `config_hash` (the experimental condition and the identity of
the data it used) and `execution_hash` (that condition, plus the seeds, the
commit and the environment). "The same condition" and "a repeat of the same
execution" are therefore decidable by machine. `runvault` records simulation
(ABM), LLM safety evaluation and anomaly-detection experiments in the *same*
shape, so runs can be compared across repositories and across years.

The repository holds a Rust library, a Rust command line, and a Python package
that is a full second implementation — it writes runs as well as reads them. The
schemas under `schema/v1/` are frozen, and both implementations are held to the
same cross-implementation test vectors.

## Install

Record runs from Rust. This pulls in no database:

```toml
[dependencies]
runvault = { git = "https://github.com/akitenkrad/rs-runvault" }
```

Record runs from Python. Use `runvault[read]` instead when the same project also
analyses them:

```toml
[project]
dependencies = ["runvault"]

[tool.uv.sources]
runvault = { git = "https://github.com/akitenkrad/rs-runvault", subdirectory = "python" }
```

The command line, which carries the DuckDB index:

```bash
cargo install --git https://github.com/akitenkrad/rs-runvault runvault-cli
runvault --help
```

## Documentation

| | |
| --- | --- |
| [Overview](docs/overview.md) | What the problem is, and what the three hashes buy |
| [Run directory](docs/run-directory.md) | The layout, and what every file in it is for |
| [Identity](docs/identity.md) | The three hashes, canonicalization, `run_uid` and `run_slug` |
| [Rust](docs/rust.md) | Recording a run from the library, and why there are two crates |
| [Python](docs/python.md) | Recording a run, and reading one back, from Python |
| [Command line](docs/cli.md) | Every subcommand and its flags |
| [Preservation](docs/preservation.md) | `verify` → `sync` → `query` → `report` |
| [Checks](docs/checks.md) | The invariants a run has to satisfy, and how to run them |
| [Schemas](docs/schemas.md) | `schema/v1/`, the test vectors, and what CI enforces |

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
