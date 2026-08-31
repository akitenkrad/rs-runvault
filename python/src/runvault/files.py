"""Small filesystem helpers shared by the writer and `verify`."""
from __future__ import annotations

import hashlib
import json
import os
from pathlib import Path
from typing import Any

from .canonical import canonicalize
from .errors import SpecError, VerifyError
from .framing import length_prefixed
from .hashes import blake3_hex

__all__ = [
    "digest_dir",
    "digest_file",
    "digest_file_as",
    "format_number",
    "read_json",
    "relative",
    "walk_files",
    "write_atomically",
    "write_json_atomically",
]


def write_atomically(path: Path, data: bytes) -> None:
    """Write bytes by creating a temporary file next to `path` and renaming it.

    A reader must never see a half-written `status.json` and conclude the run
    finished, so the file appears complete or not at all.
    """
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tmp = path.parent / f".{path.name}.tmp-{os.getpid()}"
    tmp.write_bytes(data)
    os.replace(tmp, path)


def write_json_atomically(path: Path, value: Any) -> None:
    """Serialize to pretty JSON and write it atomically, with a trailing newline."""
    text = json.dumps(value, indent=2, ensure_ascii=False) + "\n"
    write_atomically(path, text.encode("utf-8"))


def read_json(path: Path) -> Any:
    """Read and parse a JSON file."""
    path = Path(path)
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except json.JSONDecodeError as error:
        raise VerifyError(f"{path}: not readable as JSON: {error}") from error


def digest_file(path: Path) -> tuple[str, int]:
    """BLAKE3 and byte count of a file. What `runvault` records for its own files."""
    return digest_file_as(path, "blake3")


def digest_file_as(path: Path, algorithm: str) -> tuple[str, int]:
    """The digest of a file under the function a record says it used, and its size.

    `runvault` writes BLAKE3, but `schema/v1` also accepts SHA-256 so a record
    can carry a digest that came from somewhere else. A name that is neither is
    refused rather than skipped: an unchecked row must not pass for a checked one.
    """
    data = Path(path).read_bytes()
    if algorithm == "blake3":
        return blake3_hex(data), len(data)
    if algorithm == "sha256":
        return hashlib.sha256(data).hexdigest(), len(data)
    raise SpecError(f"unknown digest algorithm: {algorithm!r}")


def digest_dir(root: Path) -> str:
    """A directory as the path-ordered list of `(relative path, content hash)`.

    Timestamps and permissions are left out: the same bytes laid out the same
    way must hash the same on another machine.
    """
    root = Path(root)
    inputs: list[str] = []
    for rel in walk_files(root, root):
        inputs += [rel, digest_file(root / rel)[0]]
    return blake3_hex(length_prefixed(inputs))


def walk_files(root: Path, base: Path) -> list[str]:
    """Every regular file under `root`, as paths relative to `base`, sorted.

    Sorting makes `manifest.csv` reproducible; symbolic links are not followed,
    so a link out of the run directory cannot pull foreign files into the record.
    """
    root, base = Path(root), Path(base)
    if not root.exists():
        return []
    out: list[str] = []
    stack = [root]
    while stack:
        for entry in stack.pop().iterdir():
            if entry.is_symlink():
                continue
            if entry.is_dir():
                stack.append(entry)
            elif entry.is_file():
                out.append(relative(entry, base))
    return sorted(out)


def relative(path: Path, base: Path) -> str:
    """`path` relative to `base`, with `/` separators."""
    try:
        return Path(path).relative_to(base).as_posix()
    except ValueError as error:
        raise SpecError(f"{path} is not under {base}") from error


def format_number(value: float) -> str:
    """The shortest decimal that round-trips, without an exponent.

    The same spelling the canonical form uses, so a number written to a CSV and
    the same number inside a hash never disagree about what it was.
    """
    return canonicalize(float(value))
