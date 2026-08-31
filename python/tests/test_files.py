"""Filesystem helpers the writer and `verify` share."""
from __future__ import annotations

import os
from pathlib import Path

from runvault import files


def test_an_atomic_write_leaves_no_temporary_behind(tmp_path: Path) -> None:
    path = tmp_path / "status.json"
    files.write_atomically(path, b"{}")
    assert path.read_text(encoding="utf-8") == "{}"
    assert [p.name for p in tmp_path.iterdir()] == ["status.json"]


def test_an_atomic_write_replaces_the_previous_content(tmp_path: Path) -> None:
    path = tmp_path / "a.json"
    files.write_atomically(path, b"one")
    files.write_atomically(path, b"two")
    assert path.read_bytes() == b"two"


def test_walking_lists_nested_files_in_order(tmp_path: Path) -> None:
    (tmp_path / "artifacts/figs").mkdir(parents=True)
    (tmp_path / "artifacts/b.txt").write_text("b", encoding="utf-8")
    (tmp_path / "artifacts/figs/a.svg").write_text("a", encoding="utf-8")
    assert files.walk_files(tmp_path / "artifacts", tmp_path) == [
        "artifacts/b.txt",
        "artifacts/figs/a.svg",
    ]


def test_walking_a_missing_directory_is_empty_not_an_error(tmp_path: Path) -> None:
    assert files.walk_files(tmp_path / "logs", tmp_path) == []


def test_symlinks_are_not_followed(tmp_path: Path) -> None:
    outside = tmp_path / "outside.txt"
    outside.write_text("secret", encoding="utf-8")
    inside = tmp_path / "artifacts"
    inside.mkdir()
    os.symlink(outside, inside / "link.txt")
    assert files.walk_files(inside, tmp_path) == []


def test_a_digest_names_the_function_that_produced_it(tmp_path: Path) -> None:
    path = tmp_path / "a.txt"
    path.write_text("one", encoding="utf-8")
    digest, size = files.digest_file(path)
    assert len(digest) == 64
    assert size == 3
    assert files.digest_file_as(path, "sha256")[0] != digest
