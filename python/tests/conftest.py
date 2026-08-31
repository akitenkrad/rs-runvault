"""Fixtures shared by the writer tests.

A run needs a git repository (`origin=code` is the default), so the tests get a
real one rather than a mocked `git`: what `runvault` records is what `git`
prints, and a stub would only prove the stub.
"""
from __future__ import annotations

import subprocess
from pathlib import Path
from typing import Any

import pytest

from runvault import Run


def git(cwd: Path, *args: str) -> None:
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


@pytest.fixture
def repo(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    root = (tmp_path / "repo").resolve()
    root.mkdir()
    git(root, "init", "-q")
    git(root, "config", "user.email", "t@example.com")
    git(root, "config", "user.name", "t")
    (root / "a.txt").write_text("one", encoding="utf-8")
    # Run directories are outside git (design note §1.4). Left tracked, the first
    # run would dirty the tree and change the next run's execution_hash.
    (root / ".gitignore").write_text("results/\n", encoding="utf-8")
    git(root, "add", "-A")
    git(root, "commit", "-qm", "first")
    monkeypatch.chdir(root)
    return root


@pytest.fixture
def results(repo: Path) -> Path:
    return repo / "results"


def start(results: Path, **overrides: Any) -> Run:
    """A run with the smallest set of options the spec accepts."""
    options: dict[str, Any] = {
        "experiment": "schelling",
        "subcommand": "main",
        "repo_id": "rs-runvault",
        "domain": "analysis",
        "results_root": results,
        "parameters": {"n": 3},
    }
    options.update(overrides)
    return Run.start(options.pop("experiment"), options.pop("subcommand"), **options)
