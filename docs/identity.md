**English** | [日本語](identity.ja.md)

# Identity

A run's identity is computed from what it actually was, not from what someone
named it. That is the whole point: two directories can then be compared by
machine, across repositories and across years.

## The two identifiers

- **`run_uid`** — a ULID. It sorts by time, is unique across machines, and is the
  key everything joins on.
- **`run_slug`** — the directory name, built as
  `<subcommand>_<timestamp>_<cfg8>_<exec4>`: the first 8 hex characters of
  `config_hash` and the first 4 of `execution_hash`. It is for people, and it is
  **not** unique; a collision gets a `-N` suffix.

Because the slug carries hash prefixes, the directory name and the metadata can
be checked against each other, and they are — see [checks](checks.md).

## The three hashes

All three are BLAKE3, printed as hex. Each is computed over a **length-prefixed
concatenation** of its inputs, so that a list of inputs cannot be made ambiguous
by the inputs themselves. A field that is missing collapses to a zero-length
input; it is never a *dropped* input, so "absent" and "empty" stay
distinguishable.

### `env_hash` — five inputs

`os`, `arch`, `rustc_version`, `python_version`, and the lock files (each
contributing `kind`, `file`, hash algorithm and hash value, sorted by kind and
file).

`host` is deliberately **not** an input. The machine's name is recorded, but two
machines that are otherwise identical must not produce two different
environments.

### `config_hash` — the condition, and the data

Take `config.parameters`, prune every pointer the control block keeps out —
`hash_exclude`, `seed_pointers` and `determinism.invariant_to` together —
canonicalize what is left, and hash it with the identity of every dataset used.

The seeds being pruned here is what makes a replicate a replicate: it shares the
condition, so it shares `config_hash`, and differs only in `execution_hash`.

Each dataset contributes ten inputs — `role`, `name`, `dataset_id`, `version`,
`split`, hash algorithm, hash value, `hash_scope`, `uri`, `n` — ordered by
`(role, name)`. This is what makes `config_hash` mean "the same experimental
condition on the same data", rather than "the same parameter file".

### `execution_hash` — six inputs

`config_hash`, the seeds, `git_commit`, whether the tree was dirty, the hash of
that dirty diff, and `env_hash`.

The seeds are collected by resolving each pointer in `seed_pointers` against
`parameters`, in sorted pointer order, contributing the pointer and the
canonicalized value it resolves to. When there is no `code` block at all, its
three inputs stay as zero-length inputs rather than disappearing.

This is the hash that answers *has this exact thing already been run*: the same
condition, the same seeds, the same commit, the same environment.

## Canonicalization

The canonical form is what two implementations have to agree on before they can
agree on a hash. It pins:

- key ordering
- Unicode normalization (NFC)
- string escaping
- float formatting
- missing versus `null`

`schema/v1/testvectors/canonicalize.json` and `length_prefix.json` pin these,
and `hashes.json` pins the three hashes including the degenerate cases (no data,
no code, no locks). Every implementation must reproduce **every** field of every
case from the inputs alone; the intermediate `canonical` / `joined` fields exist
so that a mismatch says *where* two implementations diverged, not merely that a
hash differs. See [schemas](schemas.md).

## What the hashes buy

| Question | How it is answered |
| --- | --- |
| Is this the same condition? | Same `config_hash` |
| Is this a replicate of it? | Same `config_hash`, different `execution_hash` |
| Has this exact thing already been run? | Same `execution_hash` |
| Did the environment change under us? | Same `config_hash`, different `env_hash` |

On the command line that is `runvault path --config-hash …` and
`runvault path --execution-hash … --finished`; see [the command line](cli.md).
