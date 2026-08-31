**English** | [日本語](preservation.ja.md)

# Keeping the record off one machine

A replication repository ignores `results/`, so nothing but a copy puts the
record anywhere else. The path from a run directory to a queryable, citable
record is four commands: `verify` → `sync` → `query` → `report`.

## `verify`

`runvault verify <run>` checks the invariants that span a run's files; `--deep`
also recomputes the three hashes, rehashes the artifacts and walks
`events.jsonl`. `sync` runs the deep check before it copies anything and refuses
to send a run that fails it, so the aggregation layer never takes in a broken
run. See [checks](checks.md).

## `sync`

```bash
runvault sync --repo-id R --vault <path> --dry-run   # what would be received
runvault sync --repo-id R --vault <path>             # copy it
```

What is copied is the **light half**: the files that reconstruct the condition,
the result, the environment and the provenance — `run.json`, `config.json`,
`status.json`, `metrics.csv`, `reference.csv`, `manifest.csv`, `events.jsonl`,
plus `lock/` and whatever the run's own `sync_include` / `sync_exclude` globs
add or remove.

`artifacts/`, `logs/`, `snapshots/` and `figures/` stay where they are.
`manifest.csv` already carries their identity, and the rule is about what a file
*is* rather than what it is named: a per-step grid dump is the heavy half of the
record whether it is written as `.npy` or as `.csv`. A legacy run contributes
only the small text files a reader can still use — `json`, `jsonl`, `csv`, `tsv`,
`txt`, `md`, `yaml`, `yml`, `toml` — and its figures, checkpoints and pickles
stay behind exactly as `artifacts/` does.

The copy is a copy: the source run directory is never touched. Each destination
run directory gets a `sync.json` receipt.

### The destination has to say what it is

Without a `runvault-vault.toml` at its root declaring `visibility = "private"`,
the command **stops** rather than guessing. A run's `events.jsonl` can hold
prompts, captures and fragments of internal data, and a git history does not
forget, so the failure is closed rather than permissive.

```toml
schema_version = "1.0"
visibility     = "private"
# compress_over_mib = 10
# allow_internal    = false
```

A run that does not declare itself public needs `--allow-internal` (or
`allow_internal = true` on the destination), and one that fails `verify --deep`
is not sent at all.

## `query`

```bash
runvault query --vault <path> --refresh
runvault query --vault <path> "SELECT … FROM 'index/runs.parquet'"
```

`--refresh` walks the aggregation repository and writes seven parquet tables into
`index/`, whose columns are defined by `schema/v1/index.columns.json`:

| Table | One row per |
| --- | --- |
| `runs` | run |
| `run_data` | dataset a run used |
| `run_targets` | replication target a run declared |
| `run_jira` | issue key a run references |
| `metrics` | recorded number |
| `reference` | reported value from the source publication |
| `manifest` | file a run wrote |

The index is derived. It is not tracked by git, and deleting it costs nothing but
the walk. The column definitions are read from `index.columns.json` rather than
transcribed, so a column cannot exist in the SQL examples and be missing from the
writer.

Runs written before this specification existed are indexed beside the rest, keyed
by `legacy:<repo_id>:<path>` and carrying `run_uid IS NULL`. Nothing is invented
for the columns they cannot fill.

## `report`

```bash
runvault report --vault <path> --obsidian -o runs.json
```

Summarizes the index into the dashboard's payload, whose shape is fixed by
`schema/v1/runs.report.json`. It is a summary, not a record: losing it costs one
command.

Nothing is filled in for a run that did not record it. A legacy run has no
`status.json`, so its state is `null` rather than `unfinished` — the latter means
"a run written against this specification that has no `status.json`", which is a
different thing from "we were never told".
