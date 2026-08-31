"""JSON Pointer (RFC 6901) and the exclusion resolution built on it.

A pointer that does not resolve is an error: a silently-ineffective exclusion
leaves a volatile field inside the hash, which is the bug this prevents.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Iterable, Sequence

__all__ = [
    "Exclusions",
    "PointerError",
    "parse_pointer",
    "prune",
    "resolve",
    "resolve_exclusions",
]


class PointerError(ValueError):
    """A malformed pointer, or one that does not resolve."""


def parse_pointer(pointer: str) -> list[str]:
    if not pointer.startswith("/"):
        raise PointerError(f"pointer must start with '/': {pointer!r}")
    return [token.replace("~1", "/").replace("~0", "~") for token in pointer.split("/")[1:]]


def resolve(document: Any, pointer: str) -> Any:
    node = document
    for i, token in enumerate(parse_pointer(pointer)):
        if isinstance(node, dict):
            if token not in node:
                raise PointerError(f"pointer does not resolve: {pointer!r} (no key {token!r})")
            node = node[token]
        elif isinstance(node, list):
            node = node[_index(token, len(node), f"pointer does not resolve: {pointer!r}")]
        else:
            here = "/".join([""] + pointer.split("/")[1 : i + 1])
            raise PointerError(f"pointer does not resolve: {pointer!r} ({here!r} is not a container)")
    return node


def prune(document: Any, pointers: Iterable[str]) -> Any:
    """Rebuild `document` without the pointed-at paths.

    Rebuilding rather than deleting in place keeps the result independent of the
    order of the pointers — deleting array elements one at a time does not.
    """
    paths = [parse_pointer(p) for p in pointers]
    if any(not path for path in paths):
        raise PointerError("cannot prune the whole document")
    return _prune(document, paths)


def _prune(node: Any, paths: Sequence[list[str]]) -> Any:
    if not paths:
        return node
    drop: set[str] = set()
    deeper: dict[str, list[list[str]]] = {}
    for path in paths:
        head, rest = path[0], path[1:]
        if rest:
            deeper.setdefault(head, []).append(rest)
        else:
            drop.add(head)
    if isinstance(node, dict):
        return {
            key: _prune(value, deeper.get(key, []))
            for key, value in node.items()
            if key not in drop
        }
    if isinstance(node, list):
        dropped = {_index(token, len(node), "cannot prune") for token in drop}
        nested = {_index(token, len(node), "cannot prune"): rest for token, rest in deeper.items()}
        return [
            _prune(value, nested.get(i, []))
            for i, value in enumerate(node)
            if i not in dropped
        ]
    raise PointerError("pointer descends into a scalar")


def _index(token: str, length: int, context: str) -> int:
    if not token.isdigit() or (len(token) > 1 and token[0] == "0"):
        raise PointerError(f"{context} ({token!r} is not an array index)")
    index = int(token)
    if index >= length:
        raise PointerError(f"{context} (index {index} out of range)")
    return index


@dataclass(frozen=True)
class Exclusions:
    """The three exclusion lists of a config's `runvault` block, resolved."""

    hash_exclude: tuple[str, ...]
    seed_pointers: tuple[str, ...]
    invariant_to: tuple[str, ...]

    @property
    def config_excluded(self) -> tuple[str, ...]:
        """Everything kept out of `config_hash`."""
        return self.hash_exclude + self.seed_pointers + self.invariant_to


def resolve_exclusions(config: dict[str, Any]) -> Exclusions:
    parameters = config.get("parameters") or {}
    block = config.get("runvault") or {}

    hash_exclude = _dedup(block.get("hash_exclude") or [])
    seed_pointers = _dedup(block.get("seed_pointers") or [], seen=set(hash_exclude))
    invariant_to = _dedup(
        (block.get("determinism") or {}).get("invariant_to") or [],
        seen=set(hash_exclude) | set(seed_pointers),
    )

    for pointer in hash_exclude + seed_pointers + invariant_to:
        resolve(parameters, pointer)
    return Exclusions(hash_exclude, seed_pointers, invariant_to)


def _dedup(pointers: Iterable[str], seen: set[str] | None = None) -> tuple[str, ...]:
    seen = set() if seen is None else set(seen)
    out: list[str] = []
    for pointer in pointers:
        if pointer not in seen:
            seen.add(pointer)
            out.append(pointer)
    return tuple(out)
