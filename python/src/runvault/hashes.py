"""The three hashes of a run's identity (design note §3.3).

`env_hash` and `config_hash` say *what was run*; `execution_hash` adds the seeds
and the code, and so says *this particular execution of it*.
"""
from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Mapping, Sequence

from blake3 import blake3

from .canonical import canonicalize
from .framing import length_prefixed
from .pointer import prune, resolve, resolve_exclusions

__all__ = [
    "Identity",
    "blake3_hex",
    "config_hash",
    "data_identity",
    "env_hash",
    "execution_hash",
    "identity",
]


def blake3_hex(data: bytes) -> str:
    return blake3(data).hexdigest()


def env_hash(env: Mapping[str, Any] | None) -> str:
    """os, arch, rustc, python, locks — five inputs. `host` is deliberately not one."""
    env = env or {}
    locks = sorted(env.get("locks") or [], key=lambda lock: (_s(lock.get("kind")), _s(lock.get("file"))))
    lock_inputs: list[str] = []
    for lock in locks:
        digest = lock.get("hash") or {}
        lock_inputs += [
            _s(lock.get("kind")),
            _s(lock.get("file")),
            _s(digest.get("algorithm")),
            _s(digest.get("value")),
        ]
    return blake3_hex(
        length_prefixed(
            [
                _s(env.get("os")),
                _s(env.get("arch")),
                _s(env.get("rustc_version")),
                _s(env.get("python_version")),
                length_prefixed(lock_inputs),
            ]
        )
    )


def data_identity(data: Sequence[Mapping[str, Any]] | None) -> bytes:
    """Ten inputs per dataset, ordered by (role, name)."""
    inputs: list[str] = []
    for entry in sorted(data or [], key=lambda d: (_s(d.get("role")), _s(d.get("name")))):
        digest = entry.get("hash") or {}
        inputs += [
            _s(entry.get("role")),
            _s(entry.get("name")),
            _s(entry.get("dataset_id")),
            _s(entry.get("version")),
            _s(entry.get("split")),
            _s(digest.get("algorithm")),
            _s(digest.get("value")),
            _s(entry.get("hash_scope")),
            _s(entry.get("uri")),
            _s(entry.get("n")),
        ]
    return length_prefixed(inputs)


def config_hash(
    config: Mapping[str, Any],
    data: Sequence[Mapping[str, Any]] | None = None,
) -> tuple[str, str]:
    """Return the canonical string of the pruned parameters and its digest."""
    exclusions = resolve_exclusions(config)
    canonical = canonicalize(prune(config.get("parameters") or {}, exclusions.config_excluded))
    return canonical, blake3_hex(length_prefixed([canonical, data_identity(data)]))


def execution_hash(
    *,
    config_hash_hex: str,
    config: Mapping[str, Any],
    code: Mapping[str, Any] | None,
    env_hash_hex: str,
) -> str:
    """Six inputs; the three from `code` stay as zero-length inputs when there is none."""
    parameters = config.get("parameters") or {}
    seed_inputs: list[str] = []
    for pointer in sorted(resolve_exclusions(config).seed_pointers):
        seed_inputs += [pointer, canonicalize(resolve(parameters, pointer))]

    code = code or {}
    dirty = code.get("git_dirty")
    return blake3_hex(
        length_prefixed(
            [
                config_hash_hex,
                length_prefixed(seed_inputs),
                _s(code.get("git_commit")),
                "" if dirty is None else ("true" if dirty else "false"),
                _s((code.get("dirty_hash") or {}).get("value")),
                env_hash_hex,
            ]
        )
    )


@dataclass(frozen=True)
class Identity:
    config_canonical: str
    env_hash: str
    config_hash: str
    execution_hash: str


def identity(
    config: Mapping[str, Any],
    data: Sequence[Mapping[str, Any]] | None,
    env: Mapping[str, Any] | None,
    code: Mapping[str, Any] | None,
) -> Identity:
    canonical, cfg = config_hash(config, data)
    env_hex = env_hash(env)
    return Identity(
        config_canonical=canonical,
        env_hash=env_hex,
        config_hash=cfg,
        execution_hash=execution_hash(
            config_hash_hex=cfg, config=config, code=code, env_hash_hex=env_hex
        ),
    )


def _s(value: Any) -> str:
    """A missing field collapses to a zero-length input, never to a dropped one."""
    return "" if value is None else str(value)
