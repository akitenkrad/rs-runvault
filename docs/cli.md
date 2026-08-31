**English** | [日本語](cli.ja.md)

# Command line

```bash
cargo install --git https://github.com/akitenkrad/rs-runvault runvault-cli
```

Or, from a checkout, `cargo build --release` puts the binary at
`target/release/runvault`.

```
runvault path     Print run directories: the latest finished one, or the ones sharing a condition
runvault verify   Check a run directory against the invariants that span its files
runvault gc       Turn runs whose process was killed into recorded failures
runvault legacy   Read run directories written before this specification existed
runvault sync     Copy the light half of every run to the aggregation repository
runvault query    Rebuild the index, run SQL against it, or both
runvault report   Summarize the index for the Obsidian dashboard
```

## At a glance

| | |
| --- | --- |
| `runvault path --experiment E --latest` | the last completed run |
| `runvault path --experiment E --config-hash 9f2c41ab` | every run of one condition |
| `runvault path --experiment E --execution-hash 3b1d --finished` | has this exact thing already run? |
| `runvault path --experiment E --latest --subcommand run` | the latest run of one subcommand |
| `runvault path --experiment E --latest --subcommand run --standalone` | …ignoring the ones a sweep started |
| `runvault path --experiment E --children-of <run_uid>` | the runs of one sweep |
| `runvault verify <run>` | the invariants that span a run's files |
| `runvault verify <run> --deep` | …and recompute the hashes, rehash the artifacts, walk `events.jsonl` |
| `runvault gc` | record the runs whose process was killed |
| `runvault sync --repo-id R --vault V --dry-run` | what the aggregation repository would receive |
| `runvault sync --repo-id R --vault V` | copy the light half of every run into it |
| `runvault query --vault V --refresh` | rebuild `index/*.parquet` from that repository |
| `runvault query --vault V "SELECT …"` | ask a question across every repository at once |
| `runvault report --obsidian --vault V -o runs.json` | summarize the index for the dashboard |

## `path`

Prints run directories.

| Flag | Meaning |
| --- | --- |
| `--experiment <EXPERIMENT>` | the experiment to look in (required) |
| `--results-root <RESULTS_ROOT>` | where the experiment directories live (default `results`) |
| `--latest` | resolve the `latest_finished` link |
| `--config-hash <CONFIG_HASH>` | every run whose `config_hash` starts with this prefix — the same condition |
| `--execution-hash <EXECUTION_HASH>` | every run whose `execution_hash` starts with this prefix |
| `--finished` | only runs that finished. A failed run is not a run that happened |
| `--subcommand <SUBCOMMAND>` | only runs of this subcommand |
| `--standalone` | only runs that belong to no sweep |
| `--children-of <RUN_UID>` | only the children of this sweep parent |

The last three exist because of sweeps. A sweep's parent and its children share
an experiment, so `--latest` alone can hand back the parent, which holds no
metrics of its own; and the children run the same subcommand as a run started by
hand, so narrowing by subcommand alone still hands back the last child.

`--execution-hash … --finished` is what answers "has this exact thing already
been run": the same condition, the same seeds, the same commit and the same
environment.

## `verify`

```bash
runvault verify <RUN>
runvault verify <RUN> --deep
```

Without `--deep`, the invariants that span a run's files. With it, the hashes are
recomputed, the artifacts rehashed and `events.jsonl` walked. The deep cost
scales with the size of the run, which is why it is not what every execution does
on its way out — but `sync` runs it before it copies. See [checks](checks.md).

## `gc`

```bash
runvault gc [--results-root <RESULTS_ROOT>] [--dry-run]
```

Turns runs whose process was killed into recorded failures. `--dry-run` reports
what would happen without writing anything.

## `legacy`

```bash
runvault legacy --repo-id <REPO_ID> [--results-root <RESULTS_ROOT>] [--json] [--notes]
```

Reads run directories written before this specification existed. `--json` prints
the runs as JSON instead of a summary; `--notes` also prints what each run could
not convert. Nothing is invented for the fields a legacy run cannot fill.

## `sync`

```bash
runvault sync --repo-id <REPO_ID> --vault <VAULT> [--dry-run] [--allow-internal]
```

| Flag | Meaning |
| --- | --- |
| `--repo-id <REPO_ID>` | the stable repository id. A canonical run whose `run.json` disagrees with it is an error rather than a guess |
| `--vault <VAULT>` | the aggregation repository. It must declare itself private |
| `--results-root <RESULTS_ROOT>` | where the experiment directories live (default `results`) |
| `--dry-run` | list what would be copied, and how large it is, without writing |
| `--allow-internal` | also send runs that did not declare themselves public |

See [preservation](preservation.md) for what is copied and what the destination
has to declare.

## `query`

```bash
runvault query --vault <VAULT> --refresh
runvault query --vault <VAULT> "SELECT experiment, count(*) FROM 'index/runs.parquet' GROUP BY 1"
```

Rebuilds the index, runs SQL against it, or both. The tables are addressed as
parquet paths — `index/<name>.parquet`.

## `report`

```bash
runvault report --vault <VAULT> --obsidian -o runs.json
```

Summarizes the index. `--obsidian` writes the payload the dashboard reads;
`-o`/`--out` says where to write it, defaulting to standard output.
