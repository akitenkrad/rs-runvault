**English** | [日本語](overview.ja.md)

# Overview

## The problem

A research result is only worth as much as the conditions it was produced under.
Those conditions are usually scattered: a parameter file here, a random seed
passed on the command line there, a toolchain that happened to be installed on
one machine, a dataset that was later regenerated. Six months on, two directories
with similar names cannot be told apart, and nobody can say whether they are the
same experiment or two different ones.

`runvault` records *the conditions and the results together*, in one fixed shape,
so that runs produced by different repositories, in different fields, in
different years can be compared, kept and cited.

## One run is one directory

One execution produces one directory, and that directory is the record. It holds
the condition (`config.json`), the metadata and provenance (`run.json`), the
numbers (`metrics.csv`), the observations (`events.jsonl`), the outcome
(`status.json`) and the identity of everything the run wrote (`manifest.csv`).
See [the run directory](run-directory.md) for the full layout.

Everything above that layer — the DuckDB index behind `runvault query`, the
dashboard payload from `runvault report`, any other tracking UI — is derived. It
can be deleted and rebuilt from the directories, and the experiment code depends
on `runvault` alone.

## Identity is three hashes

The distinguishing idea is that a run's identity is computed, not named:

| Hash | Computed from | Answers |
| --- | --- | --- |
| `env_hash` | os, arch, `rustc` version, Python version, the lock files | Was this the same machine and toolchain? |
| `config_hash` | the experimental condition, pruned of what was declared irrelevant, plus the identity of every dataset used | Is this the same condition? |
| `execution_hash` | `config_hash`, the seeds, the git commit, whether the tree was dirty and the hash of that diff, and `env_hash` | Has this exact thing already been run? |

Because they are hashes over canonicalized inputs, "the same condition" and "a
repeat of the same execution" become machine-decidable rather than a matter of
whether two people named their directories the same way. Replicates of one
condition share a `config_hash` and differ in `execution_hash`; a re-run that
changes nothing at all reproduces both. The rules are in
[identity](identity.md), and the exact inputs are pinned by cross-implementation
test vectors, so a second implementation either agrees or is provably wrong.

## What is in the repository

| Path | Contents |
| --- | --- |
| `schema/v1/` | Frozen JSON Schemas, the flattened index columns, and the core vocabulary. **These files are the specification.** |
| `schema/v1/testvectors/` | Canonicalization and hash vectors, so two implementations can be shown to agree. |
| `crates/runvault/` | The Rust reference implementation. Depending on it to write runs pulls in no database. |
| `crates/runvault-cli/` | The `runvault` binary, and the DuckDB index behind `query`. |
| `python/` | The Python second implementation: the identity, the writer and the read side. |
| `tools/` | The validators and generators that keep the schemas, the models and the vectors honest. |

There are two crates on purpose. The library is what the replication
repositories depend on in order to *write* runs, and it must not make them
compile a database to do it; the DuckDB index therefore lives in the binary
crate. See [Rust](rust.md).

## Status

`schema/v1` is frozen, and the Rust library, the command line (including `sync`,
the DuckDB index and the dashboard payload) and the Python package all exist and
are exercised by CI. The version is `0.1.0`: the shapes are settled, the
surrounding tooling is still growing.

One known constraint: `crates/runvault` reads `schema/v1/vocabulary.toml` with
`include_str!` from outside its own directory, so the crate is not publishable to
crates.io as it stands. That matters only when publishing, which is not yet
planned.

## Where to go next

- Recording a run: [Rust](rust.md), [Python](python.md)
- Reading runs back: [Command line](cli.md), [Python](python.md)
- Keeping the record off one machine: [Preservation](preservation.md)
- What a valid run has to satisfy: [Checks](checks.md), [Schemas](schemas.md)
