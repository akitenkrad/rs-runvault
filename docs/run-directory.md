**English** | [日本語](run-directory.ja.md)

# The run directory

One execution is one directory, and the directory is the record. Nothing above
it is authoritative.

```
<results_root>/<experiment>/<run_slug>/
├── run.json          ← the run's metadata (identity, code, env, data, lineage, research)
├── config.json       ← an envelope; the conditions sit under ["parameters"]
├── metrics.csv       ← long form: run_uid, step, step_unit, scope, name, value
├── reference.csv     ← the values the source publication reports, for comparison
├── events.jsonl      ← one line per observed unit
├── status.json       ← how the run ended, and when
├── manifest.csv      ← the identity of everything the run wrote
├── lock/             ← copies of the lock files that fixed the environment
├── logs/             ← the run's logs
└── artifacts/        ← what the experiment wrote while it ran
```

`<results_root>` defaults to `results`. An experiment directory also carries a
`latest_finished` symlink to the last run that completed, and a run in progress
carries a `.runvault.lock`.

## The files

### `run.json`

The metadata. It carries `run_uid`, `run_slug`, `repo_id`, `experiment`,
`subcommand`, `domain`, the three hashes, `created_at`, `origin`, `visibility`,
and the blocks `code`, `env`, `rng`, `llm`, `data`, `lineage`, `research` and
`ext`. `schema/v1/run.json` is the specification.

Two fields deserve a note. `data` is the datasets the run used, and an empty
array means "none" — it is distinguished from "not recorded", which is why every
entry needs one of `hash` / `dataset_id` / `uri`. And `research` is what ties a
run to the work it reproduces: the publication, and the specific table or figure
target inside it.

### `config.json`

An envelope, not a bare parameter file. The experimental condition lives under
`parameters`; alongside it sits the `runvault` control block, which is what
declares:

- `hash_exclude` — pointers removed from every hash (`/output_dir`, `/log_level`)
- `seed_pointers` — where the seeds live, so a replicate shares its condition but not its execution
- `determinism.invariant_to` — pointers the experiment *declares* do not change the result
- `sync_include` / `sync_exclude` — what the copy to the aggregation repository adds or removes

`invariant_to` is declared, never guessed. Excluding something like `/threads`
unconditionally would bundle runs whose results genuinely differ as one
condition.

### `metrics.csv` and `reference.csv`

Long form, one number per row: `run_uid, step, step_unit, scope, name, value`.
A metric with no `step` is a single number for the whole run (the default scope
is `run`); one with a `step` sits on a time axis.

`reference.csv` has the same columns plus `target_id` and `source`: it holds the
values the source publication reports, so the difference between the replication
and the original can be computed later rather than remembered. `source` records
where the number was read, and every `target_id` must be one of the targets
declared in `research.targets[]`.

### `events.jsonl`

One line per observed unit. A record that calls itself `observation` or
`terminal` has to carry the reserved keys those kinds mean, so a terminal line
cannot be terminal in name only.

### `status.json`

How the run ended, when it started and finished, and the counts. A run dropped
without finishing records itself as failed.

### `manifest.csv`

`run_uid, path, algorithm, digest, bytes` for everything the run wrote.

It is written **once**, by `finish()`, which walks exactly `artifacts/` and
`logs/`. Two consequences follow, and both matter in practice:

- Anything written elsewhere in the run directory is not part of the record.
- Anything added after `finish()` is not in the manifest, so a figure drawn later
  belongs *beside* the run rather than inside it.

### `lock/`

Copies of the lock files found at the repository root — `Cargo.lock`, `uv.lock`,
`poetry.lock`, `requirements.lock` — each hashed into `env.locks[]` and so into
`env_hash`. They are described before the run directory exists (the directory
name depends on a hash that depends on them) and copied in afterwards.

### `artifacts/` and `logs/`

Where generated files and logs go. These are the two trees `finish()` walks, and
the two `runvault sync` deliberately leaves behind — `manifest.csv` already
carries their identity.

`logs/progress.log` is written by `run.stage(...)`, which mirrors there whatever
it prints to standard error, so how long a subcommand took and where it was at
any point outlives the terminal it was started from.

## Lifetime and failure

- A run in progress holds `.runvault.lock`, refreshed by a heartbeat.
- A run dropped without `finish()` records itself as failed.
- A run whose process is *killed* leaves the lock behind with nothing to write
  the failure. `runvault gc` turns that into a recorded failure. **The presence
  of a lock alone never means "still running".**
- `latest_finished` is only moved forward: when a long run overtakes a short one,
  the link does not walk backwards. The comparison and the replacement happen
  under a directory-wide mutex, so two runs finishing at once cannot install the
  older one last.

## Sweeps

A sweep parent is driven by a list of seeds rather than one. It is declared as a
sweep parent, leaves the master seed unset, and `runvault` fills in
`lineage.sweep_id` with the run's own slug — the caller cannot know it, because
the slug carries the hashes, which are only computed once the run starts. The
children read it back and each carry their own seed.

The parent holds no metrics of its own, which is why `runvault path --latest`
alone can hand back something empty; see [the command line](cli.md) for
`--subcommand`, `--standalone` and `--children-of`.
