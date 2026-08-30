# testvectors

Cross-implementation vectors for the parts of the specification where two
implementations can silently disagree: canonical JSON, the length-prefixed
concatenation, and the three hashes (design note §3.3).

Every implementation — the Rust reference and the Python second implementation —
must reproduce **every** field of every case from the inputs alone. The
`canonical` / `joined` fields exist so a mismatch says *where* the two
implementations diverged, not just that a hash differs.

| File | Pins |
| --- | --- |
| `canonicalize.json` | key ordering, NFC, escaping, float formatting, missing vs `null` |
| `length_prefix.json` | the framing that makes a list of inputs unambiguous |
| `hashes.json` | `env_hash` → `config_hash` → `execution_hash`, including the degenerate cases (no data, no code, no locks) |

These files are inputs to tests, not generated output: change them only when the
specification changes, and change `schema/v1` first.

The vectors are deliberately outside `schema/v1/*.json` (a non-recursive glob),
so the schema validators do not try to read them as JSON Schemas.
