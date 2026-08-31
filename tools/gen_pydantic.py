#!/usr/bin/env python3
"""Writes python/src/runvault/models/ from schema/v1/*.json.

The schemas are the specification; the Pydantic models are a view of them. The
generated files are committed so that a schema change that nobody regenerated
for shows up as a diff in CI rather than as a runtime surprise:

    uv run --with datamodel-code-generator python tools/gen_pydantic.py
    git diff --exit-code python/src/runvault/models/

Only the schemas that Python reads or writes are generated. index.columns.json
and index.columns.meta.json define the DuckDB index that the Rust CLI builds;
they have no object shape, so a model for them would assert nothing.
"""

from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCHEMA = ROOT / "schema" / "v1"
OUT = ROOT / "python" / "src" / "runvault" / "models"

SCHEMAS = [
    "common.json",
    "run.json",
    "config.json",
    "status.json",
    "event.json",
    "metrics.row.json",
    "reference.row.json",
    "manifest.row.json",
    "sync.json",
    "runs.report.json",
    "vault.config.json",
]

HEADER = "# Generated from schema/v1 by tools/gen_pydantic.py. Do not edit.\n"


def main() -> int:
    missing = [n for n in SCHEMAS if not (SCHEMA / n).is_file()]
    if missing:
        print(f"missing schemas: {', '.join(missing)}", file=sys.stderr)
        return 1

    # The generator walks whatever directory it is given, and schema/v1 also
    # holds testvectors/ and vocabulary.toml, which are not JSON Schemas.
    with tempfile.TemporaryDirectory() as tmp:
        staged = Path(tmp) / "v1"
        staged.mkdir()
        for name in SCHEMAS:
            shutil.copy2(SCHEMA / name, staged / name)

        built = Path(tmp) / "models"
        subprocess.run(
            [
                "datamodel-codegen",
                "--input", str(staged),
                "--input-file-type", "jsonschema",
                "--output", str(built),
                "--output-model-type", "pydantic_v2.BaseModel",
                "--target-python-version", "3.11",
                "--use-schema-description",
                "--formatters", "black", "isort",
                # Without this the timestamp comment makes every regeneration a diff,
                # and the CI check below could never mean anything.
                "--disable-timestamp",
            ],
            check=True,
        )

        if OUT.exists():
            shutil.rmtree(OUT)
        OUT.parent.mkdir(parents=True, exist_ok=True)
        shutil.copytree(built, OUT)

    for path in sorted(OUT.glob("*.py")):
        text = path.read_text(encoding="utf-8")
        if not text.startswith(HEADER):
            path.write_text(HEADER + text, encoding="utf-8")
        print(f"wrote {path.relative_to(ROOT)}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
