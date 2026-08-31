"""runvault — the Python second implementation of the run identity, and the writer.

The hash primitives are kept independent of the Rust reference on purpose: the
vectors in `schema/v1/testvectors/` only prove something if two implementations
reach them separately. The writer is not a second-class one — `events.jsonl` is
written mostly from Python — so a run recorded here is the same run directory
the Rust reference would have written.
"""
from __future__ import annotations

from . import env, files, git, ids, lockfile, paths, status, verify, vocabulary
from .canonical import CanonicalError, canonicalize
from .errors import GitError, RunvaultError, SpecError, VerifyError
from .framing import length_prefixed
from .hashes import (
    Identity,
    blake3_hex,
    config_hash,
    data_identity,
    env_hash,
    execution_hash,
    identity,
)
from .pointer import (
    Exclusions,
    PointerError,
    parse_pointer,
    prune,
    resolve,
    resolve_exclusions,
)
from .run import Run, RunOptions

__version__ = "0.1.0"

__all__ = [
    "CanonicalError",
    "Exclusions",
    "GitError",
    "Identity",
    "PointerError",
    "Run",
    "RunOptions",
    "RunvaultError",
    "SpecError",
    "VerifyError",
    "__version__",
    "blake3_hex",
    "canonicalize",
    "config_hash",
    "data_identity",
    "env",
    "env_hash",
    "execution_hash",
    "files",
    "git",
    "identity",
    "ids",
    "length_prefixed",
    "lockfile",
    "parse_pointer",
    "paths",
    "prune",
    "resolve",
    "resolve_exclusions",
    "status",
    "verify",
    "vocabulary",
]
