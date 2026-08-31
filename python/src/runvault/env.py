"""What machine and toolchain produced the run.

`os` and `arch` are spelled the way the Rust reference spells them (`macos`,
`aarch64`), not the way Python does: a run recorded from either implementation
on the same machine has to reach the same `env_hash`.
"""
from __future__ import annotations

import platform
import socket
import sys
from pathlib import Path
from typing import Any

from .hashes import blake3_hex, env_hash

__all__ = [
    "arch",
    "collect",
    "host",
    "materialize_locks",
    "os_name",
    "plan_locks",
    "python_version",
    "runvault_version",
]

# The lock files that get copied into the run's `lock/` directory.
LOCK_FILES = (
    ("Cargo.lock", "cargo"),
    ("uv.lock", "uv"),
    ("poetry.lock", "poetry"),
    ("requirements.lock", "pip"),
)

_OS = {"darwin": "macos", "win32": "windows", "cygwin": "windows"}
_ARCH = {"arm64": "aarch64", "AMD64": "x86_64", "amd64": "x86_64", "i386": "x86", "i686": "x86"}


def runvault_version() -> str:
    """The version of `runvault` that is writing the run."""
    from . import __version__  # deferred: the package imports this module

    return __version__


def host() -> str:
    """Machine name. Only recorded, never hashed."""
    return socket.gethostname() or "unknown"


def os_name() -> str:
    return _OS.get(sys.platform, sys.platform)


def arch() -> str:
    machine = platform.machine()
    return _ARCH.get(machine, machine)


def python_version() -> str:
    return platform.python_version()


def plan_locks(repo_root: Path) -> list[tuple[Path, dict[str, Any]]]:
    """Describe the repository's lock files without copying them yet.

    The copies land inside the run directory, whose name depends on
    `execution_hash`, which depends on these hashes: the description has to come
    first, and the copying after the directory exists.
    """
    planned = []
    for name, kind in LOCK_FILES:
        source = Path(repo_root) / name
        if not source.is_file():
            continue
        planned.append(
            (
                source,
                {
                    "kind": kind,
                    "hash": {"algorithm": "blake3", "value": blake3_hex(source.read_bytes())},
                    "file": f"lock/{name}",
                },
            )
        )
    return planned


def materialize_locks(planned: list[tuple[Path, dict[str, Any]]], run_dir: Path) -> None:
    """Copy the planned lock files into the run directory."""
    for source, lock in planned:
        destination = Path(run_dir) / lock["file"]
        destination.parent.mkdir(parents=True, exist_ok=True)
        destination.write_bytes(Path(source).read_bytes())


def collect(python: str | None, locks: list[dict[str, Any]]) -> dict[str, Any]:
    """Collect the environment and compute its hash.

    `origin` does not matter here: a run entered by hand still records which
    machine and toolchain it was entered on. `rustc_version` is null because no
    Rust toolchain took part in a run this implementation wrote.
    """
    env = {
        "env_hash": "",
        "host": host(),
        "os": os_name(),
        "arch": arch(),
        "rustc_version": None,
        "python_version": python,
    }
    env["env_hash"] = env_hash({**env, "locks": locks})
    return env
