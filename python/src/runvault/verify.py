"""`runvault verify` — the invariants that span files.

A JSON Schema sees one file, or one line, at a time. Whether a run contradicts
*itself* is a different question, and this is where it is asked (design note
§3.10). Only the shallow checks live here: rehashing the data and walking
`events.jsonl` cost time proportional to the run, and belong before a sync or
before a table is built. The deep half is the CLI's.
"""
from __future__ import annotations

import csv
import json
from pathlib import Path
from typing import Any, Callable, Iterable

from . import files, ids, lockfile, paths
from .errors import SpecError, VerifyError
from .pointer import resolve_exclusions

__all__ = ["check_lineage_shape", "check_research", "shallow"]


def shallow(run_dir: Path) -> None:
    """Run every shallow invariant against a run directory."""
    run_dir = Path(run_dir)
    meta = files.read_json(run_dir / "run.json")
    uid = meta["run_uid"]

    _check_slug_matches_hashes(run_dir, meta)
    _check_config(run_dir, uid)
    _check_status(run_dir, uid)
    _check_metrics(run_dir, uid)
    _check_reference(run_dir, meta)
    _check_manifest(run_dir, uid)
    _check_events(run_dir, uid)
    check_research(meta.get("research") or {})
    _check_data(meta)
    _check_lineage(run_dir, meta)


def _check_slug_matches_hashes(run_dir: Path, meta: dict[str, Any]) -> None:
    """The directory name, `run_slug` and the two hashes must all agree."""
    slug = meta["run_slug"]
    if run_dir.name != slug:
        raise VerifyError(f"directory name `{run_dir.name}` is not the run_slug `{slug}`")
    prefixes = ids.slug_hash_prefixes(slug)
    if prefixes is None:
        raise VerifyError(f"run_slug `{slug}` carries no hash prefixes")
    cfg8, exec4 = prefixes
    if not meta["config_hash"].startswith(cfg8):
        raise VerifyError(f"run_slug cfg8 `{cfg8}` disagrees with config_hash `{meta['config_hash']}`")
    if not meta["execution_hash"].startswith(exec4):
        raise VerifyError(
            f"run_slug exec4 `{exec4}` disagrees with execution_hash `{meta['execution_hash']}`"
        )


def _check_config(run_dir: Path, uid: str) -> None:
    config = files.read_json(run_dir / "config.json")
    if config["run_uid"] != uid:
        raise VerifyError(
            f"config.json run_uid `{config['run_uid']}` is not run.json `{uid}`"
        )
    # A pointer that no longer resolves means an exclusion silently stopped applying.
    resolve_exclusions(config)


def _check_status(run_dir: Path, uid: str) -> None:
    path = run_dir / "status.json"
    if not path.is_file():
        return
    status = files.read_json(path)
    if status["run_uid"] != uid:
        raise VerifyError(f"status.json run_uid `{status['run_uid']}` is not run.json `{uid}`")
    if status["state"] == "failed" and not status.get("error"):
        raise VerifyError("state=failed without an error")
    if (run_dir / lockfile.LOCK_FILE).is_file():
        raise VerifyError(
            f"status.json is present but {lockfile.LOCK_FILE} is still there "
            "(a finished run looks like it is still going)"
        )


def _read_csv(path: Path) -> list[dict[str, str]] | None:
    """Rows of a CSV, or `None` when the file is absent."""
    if not path.is_file():
        return None
    with path.open(encoding="utf-8", newline="") as handle:
        return list(csv.DictReader(handle))


def _check_uid_column(file: str, rows: list[dict[str, str]], uid: str) -> None:
    for number, row in enumerate(rows, start=2):
        if row.get("run_uid") != uid:
            raise VerifyError(
                f"{file} line {number}: run_uid `{row.get('run_uid')}` is not run.json `{uid}`"
            )


def _check_unique(file: str, rows: Iterable[tuple[str, ...]]) -> None:
    seen: set[tuple[str, ...]] = set()
    for key in rows:
        if key in seen:
            raise VerifyError(f"{file} has a duplicated primary key: {', '.join(key)}")
        seen.add(key)


def _key(row: dict[str, str], columns: tuple[str, ...]) -> tuple[str, ...]:
    return tuple(row.get(column) or "" for column in columns)


def _check_metrics(run_dir: Path, uid: str) -> None:
    rows = _read_csv(run_dir / "metrics.csv")
    if rows is None:
        return
    _check_uid_column("metrics.csv", rows, uid)
    columns = ("run_uid", "name", "step", "step_unit", "scope")
    _check_unique("metrics.csv", (_key(row, columns) for row in rows))


def _check_reference(run_dir: Path, meta: dict[str, Any]) -> None:
    rows = _read_csv(run_dir / "reference.csv")
    if rows is None:
        return
    _check_uid_column("reference.csv", rows, meta["run_uid"])
    columns = ("run_uid", "name", "step", "step_unit", "scope", "target_id")
    _check_unique("reference.csv", (_key(row, columns) for row in rows))

    known = {t["target_id"] for t in (meta.get("research") or {}).get("targets") or []}
    for row in rows:
        if row.get("target_id") not in known:
            raise VerifyError(
                f"reference.csv target_id `{row.get('target_id')}` is not in research.targets[]"
            )


def _check_manifest(run_dir: Path, uid: str) -> None:
    rows = _read_csv(run_dir / "manifest.csv")
    if rows is None:
        return
    _check_uid_column("manifest.csv", rows, uid)
    _check_unique("manifest.csv", (_key(row, ("run_uid", "path")) for row in rows))


def _check_events(run_dir: Path, uid: str) -> None:
    def check(number: int, event: dict[str, Any]) -> None:
        if event.get("run_uid") != uid:
            raise VerifyError(
                f"events.jsonl line {number}: run_uid `{event.get('run_uid')}` "
                f"is not run.json `{uid}`"
            )

    for_each_event(run_dir / "events.jsonl", check)


def for_each_event(path: Path, visit: Callable[[int, dict[str, Any]], None]) -> None:
    """Call `visit` with each non-empty line of a JSONL file, one at a time."""
    path = Path(path)
    if not path.is_file():
        return
    with path.open(encoding="utf-8") as handle:
        for number, line in enumerate(handle, start=1):
            if not line.strip():
                continue
            try:
                event = json.loads(line)
            except json.JSONDecodeError as error:
                raise VerifyError(f"events.jsonl line {number} is not JSON: {error}") from error
            visit(number, event)


def _check_data(meta: dict[str, Any]) -> None:
    seen: set[tuple[str, str]] = set()
    for entry in meta.get("data") or []:
        key = (entry.get("role"), entry.get("name"))
        if key in seen:
            raise VerifyError(f"data[] has a duplicated (role, name): {key}")
        seen.add(key)
        if (entry.get("hash") is None) != (entry.get("hash_scope") is None):
            raise VerifyError(
                f"data[] {key} needs hash and hash_scope together "
                "(one alone does not say what was hashed)"
            )


def check_research(research: dict[str, Any]) -> None:
    """The replication is identified, and the id it claims agrees with the ids it carries."""
    seen: set[str] = set()
    for target in research.get("targets") or []:
        ids.validate_slug("target_id", target["target_id"])
        if target["target_id"] in seen:
            raise VerifyError(f"research.targets[] target_id `{target['target_id']}` is duplicated")
        seen.add(target["target_id"])

    if not research.get("is_replication"):
        return

    work = research.get("work")
    if not work:
        raise SpecError("a replication needs work")
    if not (work.get("title") or "").strip():
        raise SpecError("a replication needs work.title")
    if not (work.get("source_version") or "").strip():
        raise SpecError(
            "a replication needs work.source_version (the same table differs between versions)"
        )
    if not research.get("targets"):
        raise SpecError("a replication needs at least one target")
    if not (research.get("obsidian_note") or "").strip():
        raise SpecError("a replication needs obsidian_note")

    # The prefix of work_id names which of the three ids is the canonical one.
    work_id = work.get("work_id") or ""
    prefix, _, rest = work_id.partition(":")
    field = {"doi": "doi", "arxiv": "arxiv_id", "paperid": "paper_id"}.get(prefix)
    if not rest or field is None:
        raise SpecError(f"work_id `{work_id}` has no doi / arxiv / paperid prefix")
    declared = work.get(field)
    if declared is None:
        raise SpecError(f"work_id is `{prefix}:` but {field} is empty")
    if declared != rest:
        raise VerifyError(f"work_id `{work_id}` disagrees with {field} `{declared}`")


def check_lineage_shape(lineage: dict[str, Any] | None) -> None:
    """The two lineage rules a schema can express, checked before the run starts."""
    if not lineage:
        return
    if lineage.get("parent_run_uid") and not lineage.get("sweep_id"):
        raise SpecError(
            "parent_run_uid needs sweep_id as well "
            "(a run cannot name a parent and belong to no sweep)"
        )
    if lineage.get("resumed_from") and lineage.get("derived_from"):
        raise SpecError(
            "resumed_from and derived_from cannot both be set "
            "(continuation and recomputation are different claims)"
        )


def _check_lineage(run_dir: Path, meta: dict[str, Any]) -> None:
    """Self-reference, cycles, and what `resumed_from` is allowed to point at."""
    lineage = meta.get("lineage")
    check_lineage_shape(lineage)
    if not lineage:
        return

    uid = meta["run_uid"]
    for field in ("parent_run_uid", "resumed_from", "derived_from"):
        if lineage.get(field) == uid:
            raise VerifyError(f"lineage.{field} points at the run itself")

    siblings = _sibling_runs(run_dir)

    # A run that cannot be found may live in another repository; that is not the
    # same as one that is found and does not say it failed.
    resumed = lineage.get("resumed_from")
    if resumed and resumed in siblings:
        status = siblings[resumed] / "status.json"
        try:
            state = files.read_json(status)["state"]
        except (OSError, KeyError, ValueError) as error:
            raise VerifyError(
                f"resumed_from `{resumed}` cannot be confirmed to have failed: {error}"
            ) from error
        if state != "failed":
            raise VerifyError(
                f"resumed_from `{resumed}` is finished "
                "(continuing a run that succeeded is a reanalysis, which is derived_from)"
            )

    _check_no_cycle(uid, lineage, siblings)


def _edges(lineage: dict[str, Any]) -> list[str]:
    return [
        lineage[field]
        for field in ("parent_run_uid", "resumed_from", "derived_from")
        if lineage.get(field)
    ]


def _check_no_cycle(uid: str, lineage: dict[str, Any], siblings: dict[str, Path]) -> None:
    """Whether any lineage edge leads back to a run already on the current path.

    All three edges are followed, not just the resume/derive chain: two runs that
    name each other as a sweep parent are as unwalkable as a resume loop.
    """
    marks: dict[str, str] = {}
    stack: list[tuple[str, bool]] = [(uid, False)]
    while stack:
        current, leaving = stack.pop()
        if leaving:
            marks[current] = "done"
            continue
        if marks.get(current) == "on-path":
            raise VerifyError(f"the lineage chain is a cycle ({current})")
        if marks.get(current) == "done":
            continue
        marks[current] = "on-path"
        stack.append((current, True))

        if current == uid:
            edges = _edges(lineage)
        else:
            # A run outside this results root ends the walk; it is not proof of a
            # cycle, and it is not proof of the absence of one either.
            edges = []
            directory = siblings.get(current)
            if directory is not None:
                try:
                    other = files.read_json(directory / "run.json")
                except (OSError, ValueError):
                    other = {}
                edges = _edges(other.get("lineage") or {})
        stack.extend((edge, False) for edge in edges)


def _sibling_runs(run_dir: Path) -> dict[str, Path]:
    """The runs reachable from the same results root, by `run_uid`."""
    results_root = run_dir.parent.parent
    out: dict[str, Path] = {}
    for directory in paths.run_dirs(results_root):
        try:
            meta = files.read_json(directory / "run.json")
        except (OSError, ValueError):
            continue
        if isinstance(meta, dict) and "run_uid" in meta:
            out[meta["run_uid"]] = directory
    return out
