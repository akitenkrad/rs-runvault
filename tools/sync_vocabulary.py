#!/usr/bin/env python3
"""Copies schema/v1/vocabulary.toml into the Python package.

The package is installed away from the repository, so it ships its own copy of
the registry; a test holds the two byte-identical. Run this after editing the
registry:

    uv run python tools/sync_vocabulary.py
    git diff --exit-code python/src/runvault/data/vocabulary.toml
"""

from __future__ import annotations

import shutil
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCE = ROOT / "schema" / "v1" / "vocabulary.toml"
TARGET = ROOT / "python" / "src" / "runvault" / "data" / "vocabulary.toml"


def main() -> int:
    if not SOURCE.is_file():
        print(f"missing {SOURCE}", file=sys.stderr)
        return 1
    TARGET.parent.mkdir(parents=True, exist_ok=True)
    shutil.copyfile(SOURCE, TARGET)
    print(f"wrote {TARGET.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
