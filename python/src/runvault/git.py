"""What the working tree looked like when the run started."""
from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any

from .errors import GitError
from .framing import length_prefixed
from .hashes import blake3_hex

__all__ = ["collect", "dirty_hash", "is_dirty", "repo_root"]


def _git(cwd: Path, *args: str) -> str:
    result = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, encoding="utf-8"
    )
    if result.returncode != 0:
        raise GitError(f"git {' '.join(args)} failed in {cwd}: {result.stderr.strip()}")
    return result.stdout.rstrip("\n")


def _git_optional(cwd: Path, *args: str) -> str | None:
    try:
        return _git(cwd, *args) or None
    except GitError:
        return None


def repo_root(directory: Path) -> Path:
    """The root of the repository `directory` lives in."""
    return Path(_git(Path(directory), "rev-parse", "--show-toplevel"))


def _untracked(root: Path) -> list[str]:
    listed = _git(root, "ls-files", "--others", "--exclude-standard")
    return sorted(line for line in listed.splitlines() if line)


def dirty_hash(root: Path) -> dict[str, Any]:
    """Hash the working-tree difference from `HEAD`.

    `git diff HEAD` does not list untracked files, so experiment code that has
    not been `git add`ed yet would otherwise change the result without changing
    the hash. Their paths and contents are folded in, in path order.
    """
    root = Path(root)
    inputs: list[bytes | str] = [_git(root, "diff", "HEAD")]
    for rel in _untracked(root):
        path = root / rel
        inputs += [rel, path.read_bytes() if path.is_file() else b""]
    return {"algorithm": "blake3", "value": blake3_hex(length_prefixed(inputs))}


def is_dirty(root: Path) -> bool:
    """Whether the working tree differs from `HEAD`, counting untracked files."""
    root = Path(root)
    return bool(_git(root, "diff", "HEAD")) or bool(_untracked(root))


def collect(root: Path, started_from: Path, locks: list[dict[str, Any]]) -> dict[str, Any]:
    """Describe the code that is about to produce a run."""
    root = Path(root)
    dirty = is_dirty(root)
    try:
        relpath = Path(started_from).relative_to(root).as_posix()
        relpath = "" if relpath == "." else relpath
    except ValueError:
        relpath = ""
    return {
        "git_remote": _git_optional(root, "config", "--get", "remote.origin.url"),
        "git_branch": _git_optional(root, "rev-parse", "--abbrev-ref", "HEAD"),
        "git_commit": _git(root, "rev-parse", "HEAD"),
        "git_dirty": dirty,
        "dirty_hash": dirty_hash(root) if dirty else None,
        "repo_relpath": relpath or None,
        "locks": locks,
    }
