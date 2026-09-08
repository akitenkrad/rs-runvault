**English** | [日本語](metrics.ja.md)

# What a metric means

A metric name alone does not say what was measured. Coming back to an experiment
a year later, `mean_n_surviving` is a guess and `normal.q_ordinal.0.mean` is not
even that. `metrics.csv` has six columns — `run_uid, step, step_unit, scope,
name, value` — and none of them is a place to write it down.

Descriptions are **optional**, and will stay optional: runs that already exist
have none, and none can be added to them after the fact. What replaces the
requirement is a count — see [the audit](#the-audit).

## Why the meaning is not written per name

One aggregation repository holds **13,589 distinct metric names**. Two
repositories account for 13,528 of them; every metric a person actually sat down
and named, across five other repositories, comes to **74**.

The large sets are not vocabularies. They are products:

```
normal.q_ordinal.0.mean
   │       │      │   └ aggregation   (mean / max)
   │       │      └ component index
   │       └ measure
   └ scenario                          (normal / masquerade / …)
```

Nobody writes 13,589 descriptions. So a declaration says either what one **name**
means, or what the **shape of a family of names** means, one axis at a time. Four
patterns cover eleven thousand names.

## The declaration

A repository declares its metrics in `runvault.toml`, at the repository root:

```toml
# One name at a time. The 74 hand-named metrics need nothing more than this.
[metrics."convergence_rate"]
meaning   = "the share of trials that converged"
unit      = "ratio"
direction = "up"
range     = [0.0, 1.0]
scope     = ["run"]

# A family of names. This is how a generated set is described: per axis.
[[metric_patterns]]
pattern   = "{scenario}.q_ordinal.{index}.{agg}"
meaning   = "the IDES order statistic"
unit      = "count"
[metric_patterns.axes]
scenario = "the traffic scenario that was fed in"
index    = "the component number"
agg      = "how that component was aggregated"
```

| Field | Means |
| --- | --- |
| `meaning` | what is being measured. Required for a name; optional for a pattern |
| `unit` | `ratio`, `count`, `sec`, `usd`, … |
| `direction` | `up`, `down` or `none` — which way is better |
| `range` | the values it can take, if bounded |
| `scope` | the aggregation levels it is meaningful at |

`direction` is what lets a screen say which of two runs improved. **`none` is a
value somebody chose** — "this number is a description, not a score" — and it is
not the same as leaving the field out, which says only that nobody decided.

In `pattern`, a segment written `{name}` is an axis and matches any one segment;
every other segment must match literally. **Segment counts must agree**:
`a.{x}` does not describe `a.b.c`. Matching on a prefix instead would let one
pattern quietly claim a deeper family nobody meant to describe.

An exact name always wins over a pattern that would also match it, so a family
can be described in general and one of its members called out in particular.

## What the run keeps

The declaration belongs to the repository — what a metric means is a property of
the experiment, not of one of its runs. But a run that pointed at a file in the
repository would change meaning whenever that file changed, and lose it whenever
the checkout moved. So the declaration is the source and `metrics.meta.json` in
the run directory is the record, the same split `config.json` already makes for
`parameters`.

`finish()` writes it, and writes **only the part that applies**:

```json
{
  "schema_version": "1.0",
  "metrics": {"convergence_rate": {"meaning": "…", "unit": "ratio", "direction": "up"}},
  "patterns": [{"pattern": "{scenario}.q_ordinal.{index}.{agg}", "meaning": "…", "n_matched": 1270}],
  "undescribed": ["mean_n_surviving"]
}
```

- A run that logged three metrics does not carry descriptions of the other
  seventy, and does not repeat one repository's four patterns in all 298 of its
  runs.
- Names nothing describes are **listed** in `undescribed` rather than dropped, so
  a reader can tell "nobody wrote this down" from "the file is incomplete".
- When nothing at all is described, **no file is written**. A file saying only
  "nothing is documented" makes the same claim the absent file already makes.

A repository without a `runvault.toml` still records runs. Requiring one would
make this a breaking change rather than an addition.

The declaration is read **once, when the run starts**, from the repository root:
the one passed to `RunOptions::repo_root`, or failing that the git root when
`origin = code`. A file edited while the run is in flight does not get to decide
what that run measured. Nothing is read at all when neither is available — the
working directory is not searched.

`runvault sync` carries `metrics.meta.json` to the aggregation repository, and
`runvault query --refresh` flattens it into the `metric_docs` index table.

## The audit

```bash
runvault metrics audit --vault <VAULT>
runvault metrics audit --vault <VAULT> --limit 0 --json
```

Optional without a count is the same as never, so the audit reports, per
experiment, how many distinct metric names its runs recorded, how many of those
something describes, and how many nothing does. (Like the rest of the command
line, it writes its labels in Japanese; `--json` gives the same figures with
English keys.)

- Names covered by a declared family **fold into that family** — one line, the
  pattern and the count. Printing eleven thousand names is not a report, it is a
  wall.
- What is left over is printed **by name**, because a count alone cannot be acted
  on. The list is cut at `--limit` (default 20) and says how many of how many it
  is showing; `--limit 0` prints all of them.
- A description is looked for in the order **name → registry → family**. The
  reserved metric names' meaning belongs to the core vocabulary, not to a
  repository, so a one-segment pattern cannot quietly claim `n_units`.
- The audit **counts, and decides nothing.** It drops nothing, refuses nothing,
  and is deliberately not part of [`verify`](checks.md).

## What the dashboard is given

`runvault report --obsidian` puts the descriptions on `experiments[]`, not on
`runs[]`: a run in the payload carries only its experiment's headline metric
names, so a description attached there would miss most of what is being compared.

```json
{
  "metric_vocabulary": {"n_units": {"meaning": "…", "scope": ["run"]}},
  "experiments": [
    {"repo_id": "…", "experiment": "…",
     "metric_docs": {"names": {}, "patterns": []}}
  ]
}
```

`metric_docs` is always written, empty included: a missing key and an empty one
are different claims, and a screen has to tell "nothing was described" apart from
"not read yet". The reserved names are explained once, at the top level — their
meaning is the vocabulary's, and copied into each experiment they would read as
something that experiment declared.

`range` and `scope` are not carried into the payload. No screen uses them yet,
and adding a field to a contract later is cheaper than freezing one nobody reads.

## The condition, too

A value is no more self-explanatory than a name. The same file describes what
each setting of the condition is, keyed by JSON pointer:

```toml
[parameters."/eps"]
meaning = "the confidence interval; agents further away than this are not consulted"
range   = [0.0, 1.0]

[parameters."/n"]
meaning = "the number of agents"
unit    = "count"
```

Pointers rather than bare names, because `hash_exclude` and `seed_pointers`
already speak in pointers and one file should not hold two spellings of "which
setting". A pointer also reaches a nested setting, and pointing at the parent
describes the whole block.

There is no `direction`: a condition is not a score. What a run *reached* is a
metric; what it was *asked to do* is this.

`finish()` copies the part that applies into `parameters.meta.json` — a second
file rather than a section of the first, because the two are independent and
each is written only when something applies. Settings the run has and nobody
described are listed in `undescribed`; only the top level is counted, since a
described parent already says what the block under it is.

`runvault report --obsidian` carries them on `experiments[].parameter_docs`.

## Not in the Python implementation

The Python package writes runs, but does **not** read `runvault.toml` and does
not write `metrics.meta.json`. A run recorded from Python carries its numbers and
no descriptions of them. This is a gap in the second implementation, not a
difference in the specification — `schema/v1/metrics.declaration.json` fixes the
shape for both.
