#!/usr/bin/env python3
"""Positive/negative tests for the frozen runvault schemas.

Usage:
    uv run --with jsonschema --with rfc3339-validator python tools/test_schemas.py

Every rule the design note calls "required" must be enforced by the schema, not
only by prose. A negative case declares *where* it must fail (a JSON path or a
message fragment), so a case cannot pass by failing for an unrelated reason.
"""
from __future__ import annotations

import copy
import json
import sys
from pathlib import Path

from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

SCHEMA_DIR = Path(__file__).resolve().parent.parent / "schema" / "v1"

UID = "01K3QZ8F7H9M2N4P6R8T0V2X4Z"
UID2 = "01K3QZ8F7H9M2N4P6R8T0V2X50"
SLUG = "main_20260830_101500_9f2c41ab_3b1d"
HEX64 = "a" * 64
SHA40 = "c" * 40
TS = "2026-08-30T10:15:00+09:00"

RUN = {
    "schema_version": "1.0", "vocab_version": "1.0", "runvault_version": "0.1.0",
    "run_uid": UID, "run_slug": SLUG,
    "repo_id": "social-simulation-replications", "experiment": "p00000009-schelling",
    "subcommand": "main", "domain": "simulation",
    "config_hash": HEX64, "execution_hash": HEX64,
    "created_at": TS, "origin": "code", "visibility": "internal",
    "code": {"git_commit": SHA40, "git_dirty": False},
    "env": {"env_hash": HEX64, "host": "mbp", "os": "macOS 15.5", "arch": "arm64"},
    "rng": {"master_seed": 42},
    "data": [],
    "research": {"is_replication": False},
}

REPLICATION = {
    "is_replication": True,
    "work": {"work_id": "doi:10.1080/0022250X.1971.9989794",
             "doi": "10.1080/0022250X.1971.9989794",
             "title": "Dynamic Models of Segregation", "year": 1971,
             "source_version": "published"},
    "targets": [{"target_id": "tbl3-r2", "kind": "table", "label": "Table 3", "row": "2"}],
    "obsidian_note": "研究/98_論文レポート/80-再現実験/P00000009/設計書.md",
}

STATUS = {"schema_version": "1.0", "run_uid": UID, "state": "finished",
          "started_at": TS, "finished_at": "2026-08-30T10:20:00+09:00", "duration_sec": 300.0}

METRIC = {"run_uid": UID, "step": None, "step_unit": None, "scope": "run", "name": "asr", "value": 0.21}
REFERENCE = {**METRIC, "target_id": "tbl3-r2", "source": "Table 3 row 2"}
MANIFEST = {"run_uid": UID, "path": "artifacts/final_state.png", "algorithm": "blake3",
            "digest": HEX64, "bytes": 12345}
EVENT_OBS = {"schema": "observation", "run_uid": UID, "ts": TS,
             "unit_id": "t0042", "t": 1, "t_unit": "turn"}
EVENT_TERM = {**EVENT_OBS, "schema": "terminal", "outcome": "refused", "censored": False, "budget": 10}
SYNC = {
    "schema_version": "1.0", "run_uid": UID, "run_key": UID, "generation": 1,
    "synced_at": TS, "verified": True,
    "source": {"host": "mbp", "repo_id": "ssr", "path": "results/schelling/" + SLUG},
    "files": [{"path": "events.jsonl", "stored_path": "events.jsonl.zst", "compression": "zstd",
               "source": {"hash": {"algorithm": "blake3", "value": HEX64}, "bytes": 20_000_000},
               "stored": {"hash": {"algorithm": "blake3", "value": HEX64}, "bytes": 1_000_000}}],
}
VAULT = {"schema_version": "1.0", "visibility": "private", "compress_over_mib": 10}

CONFIG = {"schema_version": "1.0", "run_uid": UID,
          "runvault": {"hash_exclude": ["/output_dir"], "seed_pointers": ["/seed"]},
          "parameters": {"seed": 1}}
REPORT = {
    "schema_version": "1.0", "vocab_version": "1.0", "generated_at": TS, "freshness_hours": 24,
    "experiments": [{"experiment": "p00000009-schelling", "repo_id": "ssr", "n_runs": 3,
                     "n_finished": 3, "last_run_at": TS, "primary_metrics": ["segregation_index"],
                     "git_remote": "git@github.com:akitenkrad/schelling1971.git"}],
    "runs": [{"run_key": UID, "run_uid": UID, "run_slug": SLUG, "repo_id": "ssr",
              "experiment": "p00000009-schelling",
              "subcommand": "main", "state": "finished", "created_at": TS,
              "git_dirty": False, "metrics": {"segregation_index": 0.834}}],
    "warnings": [],
}


def load():
    resources, schemas = [], {}
    for path in sorted(SCHEMA_DIR.glob("*.json")):
        if path.name.startswith("index.columns"):
            continue
        doc = json.loads(path.read_text("utf-8"))
        schemas[path.stem] = doc
        resources.append((doc["$id"], Resource.from_contents(doc)))
    return Registry().with_resources(resources), schemas


def mutate(base, **changes):
    out = copy.deepcopy(base)
    for key, value in changes.items():
        cur, parts = out, key.split(".")
        for p in parts[:-1]:
            cur = cur[p]
        if value is ...:
            cur.pop(parts[-1], None)
        else:
            cur[parts[-1]] = value
    return out


def main() -> int:
    registry, schemas = load()

    def V(name, profile=None):
        schema = {"$ref": f"{schemas[name]['$id']}#/$defs/{profile}"} if profile else schemas[name]
        return Draft202012Validator(schema, registry=registry, format_checker=FormatChecker())

    R = lambda **kw: mutate(RUN, **kw)                                    # noqa: E731
    REP = lambda **kw: R(research=mutate(REPLICATION, **kw))              # noqa: E731

    # (rule, schema, profile, instance, expectation)
    #   expectation True        -> must validate
    #   expectation "substring" -> must fail, and the substring must appear in the errors
    cases = [
        ("最小の run が通る", "run", None, RUN, True),
        ("run_uid は ULID", "run", None, R(run_uid="not-a-ulid"), "run_uid"),
        ("ULID の先頭は 0-7", "run", None, R(run_uid="Z1K3QZ8F7H9M2N4P6R8T0V2X4Z"), "run_uid"),
        ("run_slug はディレクトリ名の形", "run", None, R(run_slug="main_20260830"), "run_slug"),
        ("experiment は slug (大文字不可)", "run", None, R(experiment="P00000009"), "experiment"),
        ("experiment は slug (スラッシュ不可)", "run", None, R(experiment="a/b"), "experiment"),
        ("vocab_version は版番号", "run", None, R(vocab_version="latest"), "vocab_version"),
        ("data の未記入は不可", "run", None, R(data=...), "data"),
        ("domain=simulation は master_seed 必須", "run", None, R(rng=...), "rng"),
        ("master_seed は負にできない", "run", None, R(**{"rng.master_seed": -1}), "master_seed"),
        ("sweep 親は master_seed を持たなくてよい", "run", None,
         R(subcommand="sweep", rng={"master_seed": None},
           lineage={"sweep_id": "s1", "parent_run_uid": None}), True),
        ("sweep の子には master_seed が要る", "run", None,
         R(rng={"master_seed": None},
           lineage={"sweep_id": "s1", "parent_run_uid": UID}), "master_seed"),
        ("sweep でない run に master_seed の免除はない", "run", None,
         R(rng={"master_seed": None}), "master_seed"),
        ("origin=code は code 必須", "run", None, R(code=None), "code"),
        ("origin=manual なら code=null 可", "run", None, R(origin="manual", code=None), True),
        ("git_dirty=true は差分ハッシュ必須", "run", None,
         R(code={"git_commit": SHA40, "git_dirty": True}), "dirty_hash"),
        ("git_dirty=true + dirty_hash は通る", "run", None,
         R(code={"git_commit": SHA40, "git_dirty": True,
                 "dirty_hash": {"algorithm": "blake3", "value": HEX64}}), True),
        ("domain=llm-safety は llm 必須", "run", None, R(domain="llm-safety", rng=..., llm=...), "llm"),
        ("llm.temperature は負にできない", "run", None,
         R(domain="llm-safety", rng=...,
           llm={"provider": "anthropic", "model_snapshot": "opus-5", "temperature": -1}), "temperature"),
        ("domain=anomaly-detection は train/eval 必須", "run", None,
         R(domain="anomaly-detection", rng=...,
           data=[{"role": "train", "name": "cicids2017", "dataset_id": "cicids2017@2017-07"}]), "data"),
        ("train+eval が揃えば通る", "run", None,
         R(domain="anomaly-detection", rng=...,
           data=[{"role": "train", "name": "cicids2017", "dataset_id": "cicids2017@2017-07"},
                 {"role": "eval", "name": "internal-pcap", "dataset_id": "internal-pcap@2026q2"}]), True),
        ("data は識別子いずれか必須", "run", None, R(data=[{"role": "train", "name": "x"}]), "data"),
        ("hash があれば hash_scope 必須", "run", None,
         R(data=[{"role": "train", "name": "x",
                  "hash": {"algorithm": "blake3", "value": HEX64}}]), "hash_scope"),
        ("hash が無いのに hash_scope は不可", "run", None,
         R(data=[{"role": "train", "name": "x", "uri": "s3://a", "hash_scope": "file"}]), "hash_scope"),
        ("parent_run_uid は sweep_id と対", "run", None,
         R(lineage={"parent_run_uid": UID2}), "sweep_id"),
        ("sweep 親子が揃えば通る", "run", None,
         R(lineage={"sweep_id": "sweep-a", "parent_run_uid": UID2}), True),
        ("resume と derive は同時に立たない", "run", None,
         R(lineage={"resumed_from": UID2, "derived_from": UID2}), "derived_from"),
        ("再現実験は work+targets+note で通る", "run", None, REP(), True),
        ("再現実験で論文 ID が無いと落ちる", "run", None,
         REP(work={"work_id": "doi:10.1/x", "title": "t", "source_version": "published"}), "work"),
        ("再現実験は source_version 必須", "run", None,
         REP(work={"work_id": "doi:10.1080/x", "doi": "10.1080/x", "title": "t"}), "source_version"),
        ("work_id は接頭辞つきの正規形", "run", None,
         REP(work={**REPLICATION["work"], "work_id": "x"}), "work_id"),
        ("再現実験で targets 空は落ちる", "run", None, REP(targets=[]), "targets"),
        ("再現実験で obsidian_note=null は落ちる", "run", None, REP(obsidian_note=None), "obsidian_note"),
        ("year は年として妥当な範囲", "run", None, REP(work={**REPLICATION["work"], "year": 99}), "year"),
        ("created_at は date-time", "run", None, R(created_at="2026-08-30 10:15"), "created_at"),

        ("status: finished が通る", "status", None, STATUS, True),
        ("status: failed は error 必須", "status", None, mutate(STATUS, state="failed"), "error"),
        ("status: failed + error は通る", "status", None,
         mutate(STATUS, state="failed", error={"kind": "io", "message": "disk full"}), True),
        ("status: finished に error は置けない", "status", None,
         mutate(STATUS, error={"kind": "io", "message": "x"}), "error"),
        ("status: finished の exit_code は 0", "status", None, mutate(STATUS, exit_code=1), "exit_code"),
        ("status: running という状態は無い", "status", None, mutate(STATUS, state="running"), "state"),
        ("status: collision_index は 2 以上", "status", None, mutate(STATUS, collision_index=1), "collision_index"),

        ("event: observation が通る", "event", None, EVENT_OBS, True),
        ("event: observation は t_unit 必須", "event", None, mutate(EVENT_OBS, t_unit=...), "t_unit"),
        ("event: terminal は budget 必須", "event", None, mutate(EVENT_TERM, budget=...), "budget"),
        ("event: terminal の budget は null 不可", "event", None, mutate(EVENT_TERM, budget=None), "budget"),
        ("event: censored は真偽値", "event", None, mutate(EVENT_TERM, censored="yes"), "censored"),
        ("event: budget は負にできない", "event", None, mutate(EVENT_TERM, budget=-1), "budget"),
        ("event: unit_id は空文字不可", "event", None, mutate(EVENT_OBS, unit_id=""), "unit_id"),
        ("event: 種別は core か x.<repo>.<name>", "event", None, mutate(EVENT_OBS, schema="retry"), "schema"),
        ("event: 名前空間つきなら通る", "event", None,
         mutate(EVENT_OBS, schema="x.rs-jailbreak-bench.retry"), True),
        ("profile: survival_terminal は terminal を要求", "event", "survival_terminal",
         mutate(EVENT_TERM, schema="observation"), "schema"),
        ("profile: survival_terminal が通る", "event", "survival_terminal", EVENT_TERM, True),
        ("profile: survival_observation が通る", "event", "survival_observation", EVENT_OBS, True),

        ("config: エンベロープが通る", "config", None, CONFIG, True),
        ("config: parameters と同階層に run_id を置けない", "config", None,
         {**CONFIG, "run_id": UID}, "run_id"),
        ("config: hash_exclude は JSON Pointer", "config", None,
         mutate(CONFIG, **{"runvault.hash_exclude": ["output_dir"]}), "hash_exclude"),

        ("metrics: 集約値は step/step_unit とも空", "metrics.row", None, METRIC, True),
        ("metrics: step があれば step_unit 必須", "metrics.row", None,
         mutate(METRIC, step=1), "step_unit"),
        ("metrics: step が無いのに step_unit は不可", "metrics.row", None,
         mutate(METRIC, step_unit="step"), "step_unit"),
        ("metrics: scope は必須", "metrics.row", None, mutate(METRIC, scope=...), "scope"),
        ("metrics: target_id は持てない (reference と混ぜない)", "metrics.row", None,
         {**METRIC, "target_id": "tbl3-r2"}, "target_id"),
        ("reference: target_id と source が必須", "reference.row", None,
         mutate(REFERENCE, target_id=...), "target_id"),
        ("reference: 正例", "reference.row", None, REFERENCE, True),

        ("manifest: 正例", "manifest.row", None, MANIFEST, True),
        ("manifest: 絶対パスは不可", "manifest.row", None, mutate(MANIFEST, path="/etc/passwd"), "path"),
        ("manifest: .. を含むパスは不可", "manifest.row", None,
         mutate(MANIFEST, path="../outside.png"), "path"),
        ("manifest: 同期状態は持たない", "manifest.row", None, {**MANIFEST, "synced": True}, "synced"),

        ("runs.report: 正例", "runs.report", None, REPORT, True),
        ("runs.report: generated_at 必須", "runs.report", None, mutate(REPORT, generated_at=...), "generated_at"),
        ("runs.report: 未完了 run の state を表せる", "runs.report", None,
         mutate(REPORT, runs=[{**REPORT["runs"][0], "state": "unfinished"}]), True),
        ("runs.report: primary_metrics は 3 件まで", "runs.report", None,
         mutate(REPORT, experiments=[{**REPORT["experiments"][0],
                                      "primary_metrics": ["a", "b", "c", "d"]}]), "primary_metrics"),
        ("runs.report: legacy run も載せられる", "runs.report", None,
         mutate(REPORT, runs=[{"run_key": "legacy:ssr:20260620_134109", "run_uid": None,
                               "run_slug": None, "repo_id": "ssr",
                               "experiment": "p00000009-schelling",
                               "subcommand": None, "state": "finished", "created_at": TS,
                               "git_dirty": None, "metrics": {}}]), True),
        # legacy run しか無い実験では origin の記録が無い．null を許す．
        ("runs.report: git_remote は null にできる", "runs.report", None,
         mutate(REPORT, experiments=[{**REPORT["experiments"][0], "git_remote": None}]), True),
        ("runs.report: run_key は必須", "runs.report", None,
         mutate(REPORT, runs=[{k: v for k, v in REPORT["runs"][0].items() if k != "run_key"}]), "run_key"),
        # 画面は repo_id から集約先の run ディレクトリを引く．欠けると詳細が開けない．
        ("runs.report: repo_id は必須", "runs.report", None,
         mutate(REPORT, runs=[{k: v for k, v in REPORT["runs"][0].items() if k != "repo_id"}]), "repo_id"),

        ("lineage: sweep_id=null では親を指せない", "run", None,
         R(lineage={"parent_run_uid": UID2, "sweep_id": None}), "sweep_id"),
        ("status: finished の exit_code は null にできる", "status", None,
         mutate(STATUS, exit_code=None), True),

        ("sync: 正例", "sync", None, SYNC, True),
        ("sync: legacy run は run_uid=null で通る", "sync", None,
         mutate(SYNC, run_uid=None, run_key="legacy:ssr:20260620_134109"), True),
        ("sync: generation は 1 以上", "sync", None, mutate(SYNC, generation=0), "generation"),
        ("sync: zstd なら stored_path は .zst", "sync", None,
         mutate(SYNC, files=[{**SYNC["files"][0], "stored_path": "events.jsonl"}]), "stored_path"),
        ("sync: 非圧縮なのに .zst は不可", "sync", None,
         mutate(SYNC, files=[{**SYNC["files"][0], "compression": "none"}]), "stored_path"),
        ("sync: 元と保存後の両方のハッシュが要る", "sync", None,
         mutate(SYNC, files=[{k: v for k, v in SYNC["files"][0].items() if k != "stored"}]), "stored"),

        ("vault: 正例", "vault.config", None, VAULT, True),
        ("vault: public は許さない", "vault.config", None, mutate(VAULT, visibility="public"), "visibility"),
        ("vault: 未知キーは許さない", "vault.config", None, {**VAULT, "public_ok": True}, "public_ok"),
        ("vault: compress_over_mib は正の数", "vault.config", None,
         mutate(VAULT, compress_over_mib=-1), "compress_over_mib"),
    ]

    failures, positives = [], set()
    for rule, name, profile, instance, expect in cases:
        if expect is True:
            positives.add(name)
        errors = list(V(name, profile).iter_errors(instance))
        blob = " ".join(f"{e.json_path} {e.message}" for e in errors)
        if expect is True:
            ok, why = not errors, blob[:160]
        else:
            ok = bool(errors) and expect in blob
            why = "落ちなかった" if not errors else f"期待 '{expect}' が出ない: {blob[:160]}"
        print(f"{'ok  ' if ok else 'NG  '}{rule}")
        if not ok:
            failures.append(f"{rule}: {why}")

    missing = sorted(set(schemas) - positives - {"common"})
    if missing:
        failures.append(f"正例が無いスキーマ: {missing}")

    print(f"\n{len(cases) - len([f for f in failures if ':' in f])}/{len(cases)} 件が期待どおり")
    if failures:
        print("\nNG")
        for f in failures:
            print("  -", f)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
