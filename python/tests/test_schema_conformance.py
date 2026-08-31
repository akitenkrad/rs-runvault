"""Every file the writer emits, against `schema/v1`.

The Pydantic models do not express the conditional requirements
(`domain=simulation` implies `rng.master_seed`, `origin=code` implies `code`,
`is_replication` implies the research block, `failed` implies `error`), so the
schemas themselves are what the emitted files are held to.
"""
from __future__ import annotations

import csv
import json
from pathlib import Path
from typing import Any

import pytest
from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

from conftest import start

SCHEMA_DIR = Path(__file__).resolve().parents[2] / "schema" / "v1"

REPLICATION = {
    "is_replication": True,
    "work": {"work_id": "doi:10.1080/0022250X.1971.9989794",
             "doi": "10.1080/0022250X.1971.9989794",
             "title": "Dynamic Models of Segregation", "year": 1971,
             "source_version": "published"},
    "targets": [{"target_id": "tbl3-r2", "kind": "table", "label": "Table 3", "row": "2"}],
    "obsidian_note": "研究/98_論文レポート/80-再現実験/P00000009/設計書.md",
}


def validator(name: str) -> Draft202012Validator:
    resources, schemas = [], {}
    for path in sorted(SCHEMA_DIR.glob("*.json")):
        if path.name.startswith("index.columns"):
            continue
        doc = json.loads(path.read_text(encoding="utf-8"))
        schemas[path.stem] = doc
        resources.append((doc["$id"], Resource.from_contents(doc)))
    return Draft202012Validator(
        schemas[name], registry=Registry().with_resources(resources),
        format_checker=FormatChecker(),
    )


def check(name: str, instance: Any) -> None:
    errors = sorted(validator(name).iter_errors(instance), key=str)
    assert not errors, f"{name}: " + "; ".join(f"{list(e.path)}: {e.message}" for e in errors)


def csv_rows(path: Path, numbers: tuple[str, ...] = (), integers: tuple[str, ...] = ()) -> list[dict]:
    """CSV has no types; the row schemas do, so the columns are read back typed."""
    with path.open(encoding="utf-8", newline="") as handle:
        out = []
        for row in csv.DictReader(handle):
            typed: dict[str, Any] = {}
            for key, raw in row.items():
                if raw == "":
                    typed[key] = None
                elif key in numbers:
                    typed[key] = float(raw)
                elif key in integers:
                    typed[key] = int(raw)
                else:
                    typed[key] = raw
            out.append(typed)
        return out


@pytest.fixture
def emitted(results: Path) -> Path:
    run = start(results, domain="simulation", master_seed=42, research=REPLICATION,
                parameters={"n": 3, "seed": 42, "out": "results"},
                seed_pointers=["/seed"], hash_exclude=["/out"],
                data=[{"role": "init", "name": "grid", "dataset_id": "grid-v1"}])
    (run.artifacts / "final.png").write_bytes(b"\x89PNG")
    run.log_metric("segregation_index", 0.834, step=120, step_unit="step")
    run.log_metric("cost_usd", 0.42)
    run.log_reference("segregation_index", 0.85, target_id="tbl3-r2", source="Table 3 row 2")
    run.log_event("observation", {"unit_id": "u1", "t": 1, "t_unit": "step"})
    run.log_event("terminal", {"unit_id": "u1", "t": 1, "t_unit": "step",
                               "outcome": "settled", "censored": False, "budget": 100})
    return run.finish()


def test_the_run_metadata_validates(emitted: Path) -> None:
    check("run", json.loads((emitted / "run.json").read_text(encoding="utf-8")))


def test_the_config_envelope_validates(emitted: Path) -> None:
    check("config", json.loads((emitted / "config.json").read_text(encoding="utf-8")))


def test_the_status_validates(emitted: Path) -> None:
    check("status", json.loads((emitted / "status.json").read_text(encoding="utf-8")))


def test_every_event_line_validates(emitted: Path) -> None:
    for line in (emitted / "events.jsonl").read_text(encoding="utf-8").splitlines():
        check("event", json.loads(line))


def test_every_metric_row_validates(emitted: Path) -> None:
    for row in csv_rows(emitted / "metrics.csv", numbers=("value",), integers=("step",)):
        check("metrics.row", row)


def test_every_reference_row_validates(emitted: Path) -> None:
    for row in csv_rows(emitted / "reference.csv", numbers=("value",), integers=("step",)):
        check("reference.row", row)


def test_every_manifest_row_validates(emitted: Path) -> None:
    for row in csv_rows(emitted / "manifest.csv", integers=("bytes",)):
        check("manifest.row", row)


def test_a_failed_status_validates(results: Path) -> None:
    directory = start(results).fail("budget", "ran out of tokens")
    check("status", json.loads((directory / "status.json").read_text(encoding="utf-8")))


def test_a_sweep_parent_without_a_seed_validates(results: Path) -> None:
    run = start(results, subcommand="sweep", domain="simulation", sweep_parent=True,
                parameters={"seeds": [1, 2, 3]}, seed_pointers=["/seeds"])
    check("run", json.loads((run.finish() / "run.json").read_text(encoding="utf-8")))
