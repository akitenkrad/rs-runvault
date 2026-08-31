**English** | [日本語](checks.ja.md)

# Checks

A JSON Schema sees one file, or one line, at a time. Whether a run contradicts
*itself* is a different question, and that is what `runvault verify` asks.

```bash
runvault verify <run>          # the invariants that span a run's files
runvault verify <run> --deep   # …and the ones whose cost scales with the run
```

## Shallow: the invariants that span files

These are cheap enough to ask of any run, at any time.

| Checked | What it means |
| --- | --- |
| slug ↔ hashes | The directory name, `run_slug` and the two hash prefixes it carries all agree |
| `config.json` | The envelope belongs to this run, and its control block resolves |
| `status.json` | Present, and about this `run_uid` |
| `metrics.csv` | Every row carries this run's `run_uid`, and no key is duplicated |
| `reference.csv` | The same, and every `target_id` is one of the declared `research.targets[]` |
| `manifest.csv` | The same |
| `events.jsonl` | The same, line by line |
| `research` | The replication is identified, and the id it claims agrees with the ids it carries |
| `data` | Every entry has one of `hash` / `dataset_id` / `uri`, and `(role, name)` is unique |
| `lineage` | No self-reference, no cycle, and `resumed_from` points at something it is allowed to |

The cycle check follows all three lineage edges, not just the resume/derive
chain: two runs that name each other as a sweep parent are as unwalkable as a
resume loop. A run may reference one that lives elsewhere; those simply cannot be
checked from here, which is different from being wrong.

## Deep: the ones that cost what the run costs

`--deep` runs everything above, then:

- **Recomputes the three hashes** from the files that claim to summarize them.
  This is what stops a run whose `parameters` were edited afterwards from still
  claiming the old `config_hash`. Each recomputed value is fed into the next hash
  rather than the recorded one — they have just been proved equal, and using the
  recomputed value keeps the chain honest.
- **Rehashes `artifacts/`**, which is what stops a generated file from living
  outside the record.
- **Walks `events.jsonl`** in full.

The cost scales with the size of the run, which is why this is not what every
execution does on its way out. `runvault sync` runs it before it copies anything
and refuses to send a run that fails it, so the aggregation layer never takes in
a broken run.

## Running the repository's own checks

```bash
cargo test --all-features
uv run --with jsonschema --with rfc3339-validator python tools/test_schemas.py
uv run --with blake3 python tools/gen_testvectors.py   # must not change a byte
```

```bash
cd python && uv run --group dev pytest -q
```

`tools/gen_testvectors.py` is a second implementation of the canonicalization and
hashing rules. The vectors it emits are what the tests assert against: when the
two disagree one of them is wrong, and regenerating is not the fix. See
[schemas](schemas.md).

## Test data

The run directories under `crates/runvault/tests/fixtures/legacy/` are outputs of
the author's own replication of Schelling (1971) and of a small opinion-dynamics
model, trimmed to a few rows each. They are test data, not results.
