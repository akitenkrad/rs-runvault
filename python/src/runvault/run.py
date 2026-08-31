"""`Run` — one execution, one directory, append-only."""
from __future__ import annotations

import atexit
import csv
import io
import json
import math
import os
import sys
from dataclasses import dataclass, field
from datetime import datetime
from pathlib import Path
from typing import Any, Iterable, Mapping, Sequence, TextIO

from . import env as env_module
from . import files, git, ids, paths, verify, vocabulary
from .errors import GitError, SpecError, VerifyError
from .hashes import config_hash, execution_hash
from .lockfile import Heartbeat, LockRecord
from .lockfile import remove as remove_lock
from .status import FAILED, FINISHED, Counts, RunStatus, StatusError

__all__ = ["Run", "RunOptions"]

#: The version of `schema/v1` every file in a run directory carries.
SCHEMA_VERSION = "1.0"

METRICS_HEADER = ["run_uid", "step", "step_unit", "scope", "name", "value"]
REFERENCE_HEADER = METRICS_HEADER + ["target_id", "source"]
MANIFEST_HEADER = ["run_uid", "path", "algorithm", "digest", "bytes"]

#: The default `scope` of a metric: a single number for the whole run.
DEFAULT_SCOPE = "run"

#: How many `-N` suffixes to try before giving up on a colliding directory name.
MAX_COLLISION_INDEX = 999

_WORK = ("work_id", "doi", "arxiv_id", "paper_id", "title", "year", "source_version")
_TARGET = ("target_id", "kind", "label", "panel", "row", "condition")
_UPSTREAM = ("url", "commit")
_LLM = ("provider", "model_snapshot", "temperature", "system_prompt_hash")
_LINEAGE = ("sweep_id", "parent_run_uid", "resumed_from", "derived_from")
_DATASET = ("role", "name", "dataset_id", "version", "hash", "hash_scope", "n", "uri", "split")


def _shape(what: str, raw: Mapping[str, Any], order: Sequence[str], required: Sequence[str] = ()) -> dict[str, Any]:
    """Reorder a caller's block into the shape the schema fixes.

    An unknown key is refused rather than dropped: the schemas forbid extra
    properties, and a silently discarded field is a fact that was recorded
    nowhere.
    """
    unknown = sorted(set(raw) - set(order))
    if unknown:
        raise SpecError(f"{what} has keys the schema does not define: {', '.join(unknown)}")
    missing = [key for key in required if raw.get(key) is None]
    if missing:
        raise SpecError(f"{what} needs {', '.join(missing)}")
    return {key: raw.get(key) for key in order}


def _dataset(raw: Mapping[str, Any]) -> dict[str, Any]:
    """A dataset entry, without the fields it does not have."""
    shaped = _shape("data[]", raw, _DATASET, ("role", "name"))
    return {k: v for k, v in shaped.items() if k in ("role", "name") or v is not None}


def _research(raw: Mapping[str, Any] | None) -> dict[str, Any]:
    raw = dict(raw or {})
    known = ("is_replication", "work", "targets", "upstream_impl", "obsidian_note", "jira")
    unknown = sorted(set(raw) - set(known))
    if unknown:
        raise SpecError(f"research has keys the schema does not define: {', '.join(unknown)}")
    work = raw.get("work")
    upstream = raw.get("upstream_impl")
    return {
        "is_replication": bool(raw.get("is_replication", False)),
        "work": _shape("research.work", work, _WORK, ("work_id", "title")) if work else None,
        "targets": [
            _shape("research.targets[]", t, _TARGET, ("target_id", "kind", "label"))
            for t in raw.get("targets") or []
        ],
        "upstream_impl": _shape("research.upstream_impl", upstream, _UPSTREAM, ("url",))
        if upstream
        else None,
        "obsidian_note": raw.get("obsidian_note"),
        "jira": list(raw.get("jira") or []),
    }


@dataclass
class RunOptions:
    """Everything a run needs to know before it starts."""

    experiment: str
    subcommand: str
    repo_id: str | None = None
    domain: str | None = None
    results_root: Path = Path("results")
    parameters: Mapping[str, Any] = field(default_factory=dict)
    #: Pointers removed from every hash (`/output_dir`, `/log_level`, ...).
    hash_exclude: Sequence[str] = ()
    #: Where the seeds live, so a replicate shares its condition but not its execution.
    seed_pointers: Sequence[str] = ()
    #: Pointers the experiment declares do not change the result. Declared, never
    #: guessed: excluding `/threads` unconditionally would bundle runs whose
    #: results genuinely differ as one condition.
    invariant_to: Sequence[str] = ()
    sync_include: Sequence[str] = ()
    sync_exclude: Sequence[str] = ()
    #: The datasets the run used. An empty list means "none", not "not recorded".
    data: Sequence[Mapping[str, Any]] = ()
    origin: str = "code"
    visibility: str = "internal"
    repo_root: Path | None = None
    started_from: Path | None = None
    master_seed: int | None = None
    replicate_index: int | None = None
    llm: Mapping[str, Any] | None = None
    lineage: Mapping[str, Any] | None = None
    #: Declares this run to be the parent of a sweep. `runvault` fills in
    #: `lineage.sweep_id` with the run's own `run_slug`, because the caller
    #: cannot know it: the slug carries the hashes, which are only computed
    #: inside `Run.start`. The children read it back from `Run.sweep_id`.
    sweep_parent: bool = False
    research: Mapping[str, Any] | None = None
    ext: Mapping[str, Any] | None = None
    cli_args: Sequence[str] | None = None
    python_version: str | None = None

    def control(self) -> dict[str, Any]:
        """The `runvault` block of `config.json`."""
        return {
            "hash_exclude": list(self.hash_exclude),
            "seed_pointers": list(self.seed_pointers),
            "determinism": {"invariant_to": list(self.invariant_to)},
            "sync_include": list(self.sync_include),
            "sync_exclude": list(self.sync_exclude),
        }

    def require(self) -> tuple[str, str, list[dict[str, Any]], dict[str, Any]]:
        """Everything that has to hold before a directory is created."""
        if self.repo_id is None:
            raise SpecError("repo_id is required")
        if self.domain is None:
            raise SpecError("domain is required")
        for name, value in (
            ("repo_id", self.repo_id),
            ("experiment", self.experiment),
            ("subcommand", self.subcommand),
            ("domain", self.domain),
        ):
            ids.validate_slug(name, value)
        if self.origin not in ("code", "manual", "external"):
            raise SpecError(f"origin must be code / manual / external, not {self.origin!r}")
        if self.visibility not in ("internal", "public"):
            raise SpecError(f"visibility must be internal / public, not {self.visibility!r}")
        if not isinstance(self.parameters, Mapping):
            raise SpecError("parameters must be a JSON object")

        lineage = _shape("lineage", self.lineage, _LINEAGE) if self.lineage else None
        # A sweep parent is driven by a list of seeds, not by one; the list lives
        # in `/parameters` and reaches `execution_hash` through `seed_pointers`.
        # Its children still need theirs.
        if self.sweep_parent and lineage:
            if lineage.get("sweep_id"):
                raise SpecError(
                    "leave lineage.sweep_id empty when using sweep_parent "
                    "(runvault fills in the run_slug)"
                )
            if lineage.get("parent_run_uid"):
                raise SpecError("a sweep parent cannot have a parent_run_uid")
        is_sweep_parent = self.sweep_parent or bool(
            lineage and lineage.get("sweep_id") and not lineage.get("parent_run_uid")
        )
        if self.domain == "simulation" and self.master_seed is None and not is_sweep_parent:
            raise SpecError(
                "domain=simulation needs master_seed; a sweep parent driven by a "
                "column of seeds is exempt once it declares lineage.sweep_id"
            )
        if self.domain == "llm-safety" and self.llm is None:
            raise SpecError("domain=llm-safety needs llm")

        data: list[dict[str, Any]] = []
        seen: set[tuple[str, str]] = set()
        for raw in self.data:
            entry = _dataset(raw)
            ids.validate_slug("data[].role", entry["role"])
            ids.validate_slug("data[].name", entry["name"])
            key = (entry["role"], entry["name"])
            if key in seen:
                raise SpecError(f"data[] has a duplicated (role, name): {key}")
            seen.add(key)
            if not any(entry.get(k) is not None for k in ("hash", "dataset_id", "uri")):
                raise SpecError(f"data[] {key} needs one of hash / dataset_id / uri")
            data.append(entry)

        research = _research(self.research)
        verify.check_research(research)
        verify.check_lineage_shape(lineage)
        return self.repo_id, self.domain, data, research


class Run:
    """A run in progress. Every write appends; nothing already written is revised."""

    def __init__(self, options: RunOptions) -> None:
        repo_id, domain, data, research = options.require()
        now = datetime.now().astimezone()

        repo_root: Path | None = None
        if options.origin == "code":
            start_from = Path(options.repo_root or Path.cwd())
            try:
                repo_root = git.repo_root(start_from)
            except (GitError, OSError) as error:
                raise SpecError(
                    f"origin=code but no git repository was found at {start_from}: {error}"
                ) from error

        planned = env_module.plan_locks(repo_root) if repo_root else []
        locks = [lock for _, lock in planned]
        env = env_module.collect(options.python_version or env_module.python_version(), locks)
        code = (
            git.collect(repo_root, options.started_from or Path.cwd(), locks)
            if repo_root
            else None
        )

        envelope = {"runvault": options.control(), "parameters": dict(options.parameters)}
        _, cfg_hash = config_hash(envelope, data)
        exec_hash = execution_hash(
            config_hash_hex=cfg_hash, config=envelope, code=code, env_hash_hex=env["env_hash"]
        )

        run_uid = ids.new_run_uid(now)
        self._experiment_dir = paths.experiment_dir(options.results_root, options.experiment)
        directory, slug, collision_index = _create_run_dir(
            self._experiment_dir,
            options.subcommand,
            ids.timestamp_part(now),
            cfg_hash,
            exec_hash,
        )
        env_module.materialize_locks(planned, directory)

        self._dir = directory
        self._started_at = now
        self._collision_index = collision_index
        self._counts = Counts()
        self._metrics: TextIO | None = None
        self._reference: TextIO | None = None
        self._events: TextIO | None = None
        self._heartbeat: Heartbeat | None = None
        self._finished = False
        self._finished_at = ""
        self._meta = {
            "schema_version": SCHEMA_VERSION,
            "vocab_version": vocabulary.get().version,
            "runvault_version": env_module.runvault_version(),
            "run_uid": run_uid,
            "run_slug": slug,
            "repo_id": repo_id,
            "experiment": options.experiment,
            "subcommand": options.subcommand,
            "domain": domain,
            "config_hash": cfg_hash,
            "execution_hash": exec_hash,
            "created_at": now.isoformat(),
            "cli_args": list(options.cli_args) if options.cli_args is not None else list(sys.argv),
            "origin": options.origin,
            "visibility": options.visibility,
            "code": code,
            "env": env,
        }
        rng = _rng_of(options, domain)
        if rng is not None:
            self._meta["rng"] = rng
        if options.llm is not None:
            self._meta["llm"] = _shape("llm", options.llm, _LLM, ("provider", "model_snapshot"))
        self._meta["data"] = data
        lineage = _lineage_of(options, slug)
        if lineage is not None:
            self._meta["lineage"] = lineage
        self._meta["research"] = research
        if options.ext is not None:
            self._meta["ext"] = dict(options.ext)

        # From here the directory exists, so a failure must not leave something
        # that has neither a status nor a lock: `gc` would see no lock and walk
        # past it, and the half-written run would sit there for good.
        try:
            files.write_json_atomically(
                directory / "config.json",
                {
                    "schema_version": SCHEMA_VERSION,
                    "run_uid": run_uid,
                    "runvault": options.control(),
                    "parameters": dict(options.parameters),
                },
            )
            files.write_json_atomically(directory / "run.json", self._meta)
            self._heartbeat = Heartbeat.start(directory, LockRecord.for_this_process(now))
        except Exception as error:
            _record_failed_start(directory, run_uid, now, collision_index, error)
            raise
        atexit.register(self._on_interpreter_exit)

    @classmethod
    def start(cls, experiment: str, subcommand: str, **options: Any) -> Run:
        """Create the run directory and write `run.json` and `config.json`."""
        return cls(RunOptions(experiment, subcommand, **options))

    @classmethod
    def from_options(cls, options: RunOptions) -> Run:
        """The same, for a caller that built the options separately."""
        return cls(options)

    # --- what the run is --------------------------------------------------

    @property
    def dir(self) -> Path:
        """The run directory."""
        return self._dir

    @property
    def artifacts(self) -> Path:
        """Where the run's own output goes. `finish()` records what is in here."""
        directory = self._dir / "artifacts"
        directory.mkdir(parents=True, exist_ok=True)
        return directory

    @property
    def run_uid(self) -> str:
        """The run's primary key."""
        return self._meta["run_uid"]

    @property
    def run_slug(self) -> str:
        """The directory name."""
        return self._meta["run_slug"]

    @property
    def meta(self) -> dict[str, Any]:
        """Everything `run.json` holds."""
        return self._meta

    @property
    def sweep_id(self) -> str | None:
        """The sweep this run belongs to, which for a sweep parent is its own id."""
        return (self._meta.get("lineage") or {}).get("sweep_id")

    # --- recording --------------------------------------------------------

    def log_metric(
        self,
        name: str,
        value: float,
        *,
        step: int | None = None,
        step_unit: str | None = None,
        scope: str = DEFAULT_SCOPE,
    ) -> None:
        """Record a number."""
        _check_metric(name, scope, step, step_unit, value)
        self._append_metric([
            self.run_uid,
            "" if step is None else str(step),
            step_unit or "",
            scope,
            name,
            files.format_number(value),
        ], flush=True)

    def log_metrics(self, scope: str, values: Mapping[str, float] | Iterable[tuple[str, float]]) -> None:
        """Append several aggregate metrics that share a scope, flushing once.

        One call per number means one flush per number, which shows on a run that
        records a handful of metrics on every step of a long simulation.
        """
        self._log_metric_batch(None, None, scope, values)

    def log_metrics_at(
        self,
        step: int,
        step_unit: str,
        scope: str,
        values: Mapping[str, float] | Iterable[tuple[str, float]],
    ) -> None:
        """The same, for values that sit on a time axis."""
        self._log_metric_batch(step, step_unit, scope, values)

    def _log_metric_batch(
        self,
        step: int | None,
        step_unit: str | None,
        scope: str,
        values: Mapping[str, float] | Iterable[tuple[str, float]],
    ) -> None:
        pairs = list(values.items() if isinstance(values, Mapping) else values)
        for name, value in pairs:
            _check_metric(name, scope, step, step_unit, value)
        for index, (name, value) in enumerate(pairs):
            self._append_metric([
                self.run_uid,
                "" if step is None else str(step),
                step_unit or "",
                scope,
                name,
                files.format_number(value),
            ], flush=index + 1 == len(pairs))

    def log_reference(
        self,
        name: str,
        value: float,
        *,
        target_id: str,
        source: str,
        step: int | None = None,
        step_unit: str | None = None,
        scope: str = DEFAULT_SCOPE,
    ) -> None:
        """Record the value the paper reports, so the difference can be computed later.

        A value read off a figure has no place here: recording an estimate as a
        reported value makes the two indistinguishable afterwards, which is why
        `source` says where the number was read.
        """
        _check_metric(name, scope, step, step_unit, value)
        ids.validate_slug("reference target_id", target_id)
        if not source:
            raise SpecError("a reference needs a source (where the value was read)")
        known = {t["target_id"] for t in self._meta["research"]["targets"]}
        if target_id not in known:
            raise SpecError(f"target_id `{target_id}` is not in research.targets[]")
        if self._reference is None:
            self._reference = self._open_csv("reference.csv", REFERENCE_HEADER)
        _write_row(self._reference, [
            self.run_uid,
            "" if step is None else str(step),
            step_unit or "",
            scope,
            name,
            files.format_number(value),
            target_id,
            source,
        ])

    def log_event(self, kind: str, payload: Mapping[str, Any]) -> None:
        """Append one line to `events.jsonl`.

        A record that calls itself `observation` or `terminal` must carry the
        reserved keys those kinds mean, so a terminal line cannot be terminal in
        name only.
        """
        if not isinstance(payload, Mapping):
            raise SpecError("an event payload must be a JSON object")
        _check_event_kind(kind)
        event = dict(payload)
        event["schema"] = kind
        event["run_uid"] = self.run_uid
        if event.get("ts") is None:
            event["ts"] = datetime.now().astimezone().isoformat()
        _check_event_reserved(kind, event)

        if self._events is None:
            self._events = open(self._dir / "events.jsonl", "a", encoding="utf-8")
        # Keys are sorted so that a line written here and the same line written
        # by the Rust reference are the same bytes.
        self._events.write(
            json.dumps(event, ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n"
        )
        self._events.flush()
        self._counts.events += 1

    # --- ending -----------------------------------------------------------

    def finish(self) -> Path:
        """Write `manifest.csv`, check the run against itself and write `status.json`.

        Only the shallow checks run here: rehashing the data and walking
        `events.jsonl` cost time proportional to the run, and belong before a
        sync or before a table is built, not at the end of every execution.
        """
        if self._finished:
            raise SpecError("this run has already ended")
        self._close_writers()
        self._write_manifest()
        # The lock goes before `status.json`: a completed run must never be found
        # with both, which is one of the invariants `verify` checks.
        self._release_lock()
        try:
            verify.shallow(self._dir)
        except (VerifyError, SpecError) as error:
            self._end(FAILED, StatusError("verify", str(error)), None)
            raise
        self._end(FINISHED, None, 0)
        paths.update_latest_finished(self._experiment_dir, self.run_slug, self._finished_at)
        return self._dir

    def fail(self, kind: str, message: str) -> Path:
        """End the run as failed, with a reason.

        Tears down in the same order as `finish()`: the heartbeat stops and the
        lock goes before `status.json` is written, so an explicit failure never
        leaves the two together.
        """
        if self._finished:
            raise SpecError("this run has already ended")
        if not kind or not message:
            raise SpecError("a failed run states both a kind and a message")
        self._end(FAILED, StatusError(kind, message), None)
        return self._dir

    def __enter__(self) -> Run:
        return self

    def __exit__(self, exc_type: type[BaseException] | None, exc: BaseException | None, _tb: object) -> bool:
        if self._finished:
            return False
        if exc_type is None:
            self.finish()
        else:
            self.fail("exception", f"{exc_type.__name__}: {exc}")
        return False

    def _on_interpreter_exit(self) -> None:
        """A run that was not ended explicitly is a run that failed.

        This does not cover SIGKILL or a power cut, which is why the lock file
        carries a heartbeat and `runvault gc` exists.
        """
        self._end(
            FAILED,
            StatusError("dropped", "finish() was not called before the interpreter exited"),
            None,
        )

    def _end(self, state: str, error: StatusError | None, exit_code: int | None) -> None:
        """The one teardown every path goes through: writers, lock, then status."""
        if self._finished:
            return
        self._close_writers()
        self._release_lock()
        self._write_status(state, error, exit_code)
        self._finished = True
        atexit.unregister(self._on_interpreter_exit)

    def _release_lock(self) -> None:
        """Stop the heartbeat and remove the lock. Always before `status.json`."""
        if self._heartbeat is not None:
            self._heartbeat.stop()
            self._heartbeat = None
        remove_lock(self._dir)

    def _close_writers(self) -> None:
        for name in ("_metrics", "_reference", "_events"):
            handle = getattr(self, name)
            if handle is not None:
                handle.flush()
                handle.close()
                setattr(self, name, None)

    def _write_status(self, state: str, error: StatusError | None, exit_code: int | None) -> None:
        now = datetime.now().astimezone()
        self._finished_at = now.isoformat()
        status = RunStatus(
            schema_version=SCHEMA_VERSION,
            run_uid=self.run_uid,
            state=state,
            started_at=self._started_at.isoformat(),
            finished_at=self._finished_at,
            duration_sec=_duration(self._started_at, now),
            exit_code=exit_code,
            collision_index=self._collision_index,
            error=error,
            counts=self._counts,
        )
        files.write_json_atomically(self._dir / "status.json", status.to_json())

    def _write_manifest(self) -> None:
        rows = []
        for sub in ("artifacts", "logs"):
            for rel in files.walk_files(self._dir / sub, self._dir):
                digest, size = files.digest_file(self._dir / rel)
                rows.append((rel, digest, size))
        rows.sort()

        buffer = io.StringIO()
        writer = csv.writer(buffer, lineterminator="\n")
        writer.writerow(MANIFEST_HEADER)
        for path, digest, size in rows:
            writer.writerow([self.run_uid, path, "blake3", digest, str(size)])
        files.write_atomically(self._dir / "manifest.csv", buffer.getvalue().encode("utf-8"))
        self._counts.artifacts = len(rows)

    def _open_csv(self, name: str, header: Sequence[str]) -> TextIO:
        handle = open(self._dir / name, "a", encoding="utf-8", newline="")
        _write_row(handle, header)
        return handle

    def _append_metric(self, row: Sequence[str], flush: bool) -> None:
        if self._metrics is None:
            self._metrics = self._open_csv("metrics.csv", METRICS_HEADER)
        _write_row(self._metrics, row, flush=flush)
        self._counts.metrics += 1


def _write_row(handle: TextIO, row: Sequence[str], flush: bool = True) -> None:
    csv.writer(handle, lineterminator="\n").writerow(list(row))
    if flush:
        handle.flush()


def _duration(started: datetime, ended: datetime) -> float:
    """Wall-clock seconds, at the millisecond resolution the reference records."""
    return max(0, int((ended - started).total_seconds() * 1000)) / 1000.0


def _rng_of(options: RunOptions, domain: str) -> dict[str, Any] | None:
    if options.master_seed is None and options.replicate_index is None and domain != "simulation":
        return None
    return {"master_seed": options.master_seed, "replicate_index": options.replicate_index}


def _lineage_of(options: RunOptions, run_slug: str) -> dict[str, Any] | None:
    """The lineage as written, with the sweep parent's own id filled in."""
    lineage = _shape("lineage", options.lineage, _LINEAGE) if options.lineage else None
    if not options.sweep_parent:
        return lineage
    return {**(lineage or {key: None for key in _LINEAGE}), "sweep_id": run_slug}


def _create_run_dir(
    experiment_dir: Path, subcommand: str, timestamp: str, config_hash: str, execution_hash: str
) -> tuple[Path, str, int | None]:
    """Create `<experiment>/<slug>`, adding `-N` until the name is free.

    A parallel sweep of the same condition with different seeds starts many runs
    in the same second; without the suffix they would all want one directory.
    """
    experiment_dir.mkdir(parents=True, exist_ok=True)
    for collision_index in [None, *range(2, MAX_COLLISION_INDEX + 1)]:
        slug = ids.run_slug(subcommand, timestamp, config_hash, execution_hash, collision_index)
        directory = experiment_dir / slug
        try:
            os.mkdir(directory)
        except FileExistsError:
            continue
        return directory, slug, collision_index
    raise SpecError(f"no free run directory name left in {experiment_dir}")


def _record_failed_start(
    directory: Path,
    run_uid: str,
    started_at: datetime,
    collision_index: int | None,
    cause: BaseException,
) -> None:
    """Mark a run that could not finish starting, so it is not left in limbo.

    Best effort: this runs on a path that is already failing, and a second
    failure here must not replace the error the caller needs to see.
    """
    try:
        remove_lock(directory)
        now = datetime.now().astimezone()
        status = RunStatus(
            schema_version=SCHEMA_VERSION,
            run_uid=run_uid,
            state=FAILED,
            started_at=started_at.isoformat(),
            finished_at=now.isoformat(),
            duration_sec=_duration(started_at, now),
            exit_code=None,
            collision_index=collision_index,
            error=StatusError("start", f"the run could not be created: {cause}"),
            counts=None,
        )
        files.write_json_atomically(directory / "status.json", status.to_json())
    except OSError:
        pass


def _check_metric(
    name: str, scope: str, step: int | None, step_unit: str | None, value: float
) -> None:
    ids.validate_slug("metric name", name)
    ids.validate_slug("scope", scope)
    if (step is None) != (step_unit is None):
        raise SpecError(
            "step and step_unit go together; one without the other leaves the time axis undefined"
        )
    if step_unit is not None:
        ids.validate_slug("step_unit", step_unit)
    if step is not None and (isinstance(step, bool) or not isinstance(step, int) or step < 0):
        raise SpecError(f"step must be a non-negative integer, not {step!r}")
    if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(value):
        raise SpecError(f"metric `{name}` cannot hold {value!r} (a missing value writes no row)")
    vocab = vocabulary.get()
    if not vocab.metric_allowed_at(name, scope):
        allowed = " / ".join(vocab.metric_names[name].scopes)
        raise SpecError(
            f"the reserved metric `{name}` is only allowed at scope={allowed} "
            f"(tried to write it at scope={scope})"
        )


def _check_event_kind(kind: str) -> None:
    if kind in vocabulary.get().event_schemas:
        return
    if kind.startswith("x."):
        repo, _, name = kind[2:].partition(".")
        if name:
            ids.validate_slug("event kind repo_id", repo)
            ids.validate_slug("event kind name", name)
            return
    raise SpecError(
        f"the event kind `{kind}` is neither core vocabulary nor x.<repo_id>.<name>"
    )


def _check_event_reserved(kind: str, event: Mapping[str, Any]) -> None:
    """The reserved keys each core event kind means."""
    required = {
        "observation": ("unit_id", "t", "t_unit"),
        "terminal": ("unit_id", "t", "t_unit", "outcome", "censored", "budget"),
    }.get(kind)
    if required is None:
        return
    missing = [key for key in required if event.get(key) is None]
    if missing:
        raise SpecError(f"an event calling itself `{kind}` needs {' / '.join(missing)}")
    if kind == "terminal":
        _check_terminal_consistency(event)


def _check_terminal_consistency(event: Mapping[str, Any]) -> None:
    """What a `terminal` line claims about itself has to hold.

    Catching it here rather than in a later deep check means the contradiction
    never reaches the file: a censored observation says the budget ran out, so
    its `t` is the budget, and no observation can run past one.
    """
    t, budget = _number(event.get("t")), _number(event.get("budget"))
    if t is None or budget is None:
        return
    if t > budget:
        raise SpecError(f"terminal t={t} is past budget={budget}")
    if event.get("censored") is True and t != budget:
        raise SpecError(
            f"censored=true but t={t} is not budget={budget} "
            "(the row contradicts the claim that the budget ran out)"
        )


def _number(value: Any) -> float | None:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        return None
    return float(value)
