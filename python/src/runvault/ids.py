"""The two identifiers.

`run_uid` is the key everything joins on; `run_slug` is the directory name,
which is for people and is not unique (design note §3.1).
"""
from __future__ import annotations

import secrets
from datetime import datetime

from .errors import SpecError

__all__ = [
    "new_run_uid",
    "run_slug",
    "slug_hash_prefixes",
    "timestamp_part",
    "validate_slug",
]

# Crockford base32, as ULID spells it.
_ALPHABET = "0123456789ABCDEFGHJKMNPQRSTVWXYZ"
_HEAD = frozenset("abcdefghijklmnopqrstuvwxyz0123456789")
_TAIL = _HEAD | frozenset("._-")


def validate_slug(field: str, value: str) -> None:
    """Fail unless the value matches the slug grammar of `schema/v1/common.json`.

    The same grammar guards a path element and an index key at once, so `/`,
    `..` and whitespace cannot reach either.
    """
    if (
        not isinstance(value, str)
        or not 1 <= len(value) <= 64
        or value[0] not in _HEAD
        or not set(value[1:]) <= _TAIL
    ):
        raise SpecError(f"{field} is not a slug: {value!r}")


def new_run_uid(now: datetime) -> str:
    """A fresh ULID for the given instant. Sorts by time and is unique across machines."""
    value = (int(now.timestamp() * 1000) << 80) | secrets.randbits(80)
    return "".join(_ALPHABET[(value >> (5 * shift)) & 0x1F] for shift in range(25, -1, -1))


def timestamp_part(now: datetime) -> str:
    """The timestamp part of `run_slug`, in local time."""
    return now.strftime("%Y%m%d_%H%M%S")


def run_slug(
    subcommand: str,
    timestamp: str,
    config_hash: str,
    execution_hash: str,
    collision_index: int | None = None,
) -> str:
    """Build `<subcommand>_<ts>_<cfg8>_<exec4>`, with the collision suffix when needed.

    The condition prefix puts runs of the same condition next to each other, and
    the execution suffix keeps a replicate apart from a run of edited code.
    """
    base = f"{subcommand}_{timestamp}_{config_hash[:8]}_{execution_hash[:4]}"
    return base if collision_index is None else f"{base}-{collision_index}"


def slug_hash_prefixes(slug: str) -> tuple[str, str] | None:
    """Split a `run_slug` back into `(cfg8, exec4)`, or `None` when it is not one."""
    # Only a trailing `-<digits>` is the collision suffix; a subcommand may itself
    # contain a hyphen, so splitting on the first one would cut the name apart.
    head, sep, tail = slug.rpartition("-")
    stem = head if sep and tail.isdigit() else slug
    parts = stem.rsplit("_", 2)
    if len(parts) != 3:
        return None
    _, cfg8, exec4 = parts
    if len(cfg8) != 8 or len(exec4) != 4:
        return None
    if not all(c in "0123456789abcdef" for c in cfg8 + exec4):
        return None
    return cfg8, exec4
