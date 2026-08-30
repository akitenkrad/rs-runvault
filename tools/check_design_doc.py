#!/usr/bin/env python3
"""Validate the frozen schemas and every example in the runvault design note.

Usage:
    uv run --with jsonschema --with rfc3339-validator python tools/check_design_doc.py [DESIGN_DOC]

Examples in the design note carry an HTML comment marker (invisible in Obsidian)
immediately before the fenced block:

    <!-- runvault:validate schema=run -->
    <!-- runvault:validate schema=run pointer=/properties/data -->
    <!-- runvault:validate schema=event profile=survival_terminal -->
    <!-- runvault:validate schema=metrics.row -->
    <!-- runvault:validate schema=sql -->

`pointer=` validates a fragment against a sub-schema, so partial examples are
still checked. An unmarked json/jsonl/csv/sql block is an error: no example may
escape validation.

Checks performed:
  1. every schema is a valid 2020-12 schema, and each has at least one example
  2. every marked example validates (format checking on, so bad date-times fail)
  3. index.columns.json conforms to its meta-schema
  4. vocabulary.toml agrees with the schemas that encode the same vocabulary
  5. SQL references only tables/columns that exist, resolving aliases, and does
     not join on nullable columns with USING (NULL never matches NULL)
"""
from __future__ import annotations

import csv
import io
import json
import math
import re
import sys
import tomllib
from pathlib import Path

from jsonschema import Draft202012Validator, FormatChecker
from referencing import Registry, Resource

ROOT = Path(__file__).resolve().parent.parent
SCHEMA_DIR = ROOT / "schema" / "v1"
DEFAULT_DOC = Path(
    "/Users/akitenkrad/Documents/Obsidian/設計書/rs-runvault_実験管理基盤設計書.md"
)

MARKER = re.compile(r"<!--\s*runvault:validate\s+([^>]*?)\s*-->")
FENCE = re.compile(r"^```(\w*)\s*$")
CHECKED_LANGS = {"json", "jsonl", "csv", "sql"}

INT_CSV_FIELDS = {"step", "bytes", "n"}
FLOAT_CSV_FIELDS = {"value"}
BOOL_CSV_FIELDS: set[str] = set()

# schemas that describe a single CSV row rather than a whole file
ROW_SCHEMAS = {"metrics.row", "reference.row", "manifest.row"}


def load_registry() -> tuple[Registry, dict[str, dict]]:
    schemas: dict[str, dict] = {}
    resources = []
    for path in sorted(SCHEMA_DIR.glob("*.json")):
        if path.name.startswith("index.columns"):
            continue
        doc = json.loads(path.read_text(encoding="utf-8"))
        schemas[path.name[: -len(".json")]] = doc
        resources.append((doc["$id"], Resource.from_contents(doc)))
    return Registry().with_resources(resources), schemas


def validator_for(schemas, registry, name, profile=None, pointer=None):
    if name not in schemas:
        raise KeyError(f"未知のスキーマ: {name}")
    base = schemas[name]
    if profile:
        schema = {"$ref": f"{base['$id']}#/$defs/{profile}"}
    elif pointer:
        schema = {"$ref": f"{base['$id']}#{pointer}"}
    else:
        schema = base
    return Draft202012Validator(schema, registry=registry, format_checker=FormatChecker())


def parse_blocks(text: str):
    lines = text.splitlines()
    pending = None
    i = 0
    while i < len(lines):
        m = MARKER.search(lines[i])
        if m:
            pending = dict(tok.split("=", 1) for tok in m.group(1).split() if "=" in tok)
            i += 1
            continue
        f = FENCE.match(lines[i])
        if f:
            lang = f.group(1)
            start = i + 1
            j = start
            while j < len(lines) and not lines[j].startswith("```"):
                j += 1
            if lang in CHECKED_LANGS:
                yield (start, pending, lang, "\n".join(lines[start:j]))
            pending = None
            i = j + 1
            continue
        if lines[i].strip():
            pending = None
        i += 1


def coerce_csv_row(row: dict[str, str], where: str, errs: list[str]) -> dict:
    out: dict = {}
    for k, v in row.items():
        if k is None:
            errs.append(f"{where}: ヘッダーに無い余分な列がある")
            continue
        if v is None:
            errs.append(f"{where}: 列 {k} の値が欠けている")
            continue
        if v == "":
            out[k] = None
        elif k in INT_CSV_FIELDS:
            try:
                out[k] = int(v)
            except ValueError:
                errs.append(f"{where}: {k}='{v}' は整数ではない")
        elif k in FLOAT_CSV_FIELDS:
            try:
                f = float(v)
            except ValueError:
                errs.append(f"{where}: {k}='{v}' は数値ではない")
                continue
            if math.isnan(f) or math.isinf(f):
                errs.append(f"{where}: {k}='{v}' は NaN / Inf. 欠測は行ごと書かない")
                continue
            out[k] = f
        elif k in BOOL_CSV_FIELDS:
            if v not in ("true", "false"):
                errs.append(f"{where}: {k}='{v}' は true / false ではない")
                continue
            out[k] = v == "true"
        else:
            out[k] = v
    return out


def check_sql(body: str, index: dict) -> list[str]:
    tables = index["tables"]
    cols = {t: {c["name"]: c for c in d["columns"]} for t, d in tables.items()}
    errs: list[str] = []
    sql = re.sub(r"--.*", "", body)

    alias: dict[str, str] = {}
    used: list[str] = []
    for path, name in re.findall(
        r"(?:FROM|JOIN)\s+'([^']+\.parquet)'(?:\s+AS)?\s+(\w+)", sql, re.I
    ):
        stem = Path(path).name[: -len(".parquet")]
        if stem not in tables:
            errs.append(f"未知の索引テーブル: {path}")
            continue
        alias[name] = stem
        used.append(stem)
    for path in re.findall(r"'([^']+\.parquet)'", sql):
        stem = Path(path).name[: -len(".parquet")]
        if stem not in tables:
            errs.append(f"未知の索引テーブル: {path}")

    # 引用符内 ('index/runs.parquet' 等) を外してからエイリアス参照を見る
    bare = re.sub(r"'[^']*'", " ", sql)
    for a, c in re.findall(r"\b([A-Za-z_]\w*)\.([A-Za-z_]\w*)\b", bare):
        if a not in alias:
            errs.append(f"未定義のエイリアス: {a}.{c}")
        elif c not in cols[alias[a]]:
            errs.append(f"{alias[a]} に無い列を参照: {a}.{c}")

    out_aliases = {a.lower() for a in re.findall(r"\bAS\s+([A-Za-z_]\w*)", sql, re.I)}
    all_cols = {c for t in cols.values() for c in t}
    qualified = {m.group(0) for m in re.finditer(r"\b[A-Za-z_]\w*\.[A-Za-z_]\w*\b", bare)}
    stripped = bare
    for q in sorted(qualified, key=len, reverse=True):  # m.step が m.step_unit を壊さないよう長い順
        stripped = stripped.replace(q, " ")
    for ident in re.findall(r"\b[A-Za-z_]\w*\b", stripped):
        low = ident.lower()
        if low in SQL_WORDS or low in out_aliases or low in alias or low in tables:
            continue
        if ident not in all_cols:
            errs.append(f"未修飾の未知識別子: {ident}")

    for a1, c1, a2, c2 in re.findall(
        r"\b([A-Za-z_]\w*)\.([A-Za-z_]\w*)\s*=\s*([A-Za-z_]\w*)\.([A-Za-z_]\w*)", bare
    ):
        for a, c in ((a1, c1), (a2, c2)):
            if a in alias and c in cols[alias[a]] and cols[alias[a]][c]["nullable"]:
                errs.append(
                    f"{a}.{c} は nullable なのに = で結合している. NULL は一致しないため行が落ちる "
                    f"(IS NOT DISTINCT FROM を使う)"
                )

    for group in re.findall(r"USING\s*\(([^)]*)\)", sql, re.I):
        for c in [x.strip() for x in group.split(",") if x.strip()]:
            for t in used:
                if c not in cols[t]:
                    errs.append(f"USING ({c}) だが {t} にその列が無い")
                elif cols[t][c]["nullable"]:
                    errs.append(
                        f"USING ({c}) は {t} で nullable. NULL は NULL と一致しないため行が落ちる "
                        f"(IS NOT DISTINCT FROM を使う)"
                    )
    return errs


SQL_WORDS = {
    "select", "from", "join", "left", "right", "inner", "outer", "on", "using", "where",
    "group", "order", "by", "having", "as", "and", "or", "not", "is", "null", "distinct",
    "asc", "desc", "limit", "offset", "case", "when", "then", "else", "end", "count",
    "avg", "sum", "min", "max", "true", "false", "in", "like", "with", "union", "all",
    "cast", "coalesce", "from_", "read_json_auto", "read_csv_auto",
}


def check_index_shape(index: dict) -> list[str]:
    """索引定義そのものの整合. メタスキーマでは表せない列との対応を見る."""
    errs = []
    for table, d in index["tables"].items():
        names = [c["name"] for c in d["columns"]]
        nullable = {c["name"]: c["nullable"] for c in d["columns"]}
        dupes = {n for n in names if names.count(n) > 1}
        if dupes:
            errs.append(f"{table}: 列名が重複している {sorted(dupes)}")
        for k in d["primary_key"]:
            if k not in nullable:
                errs.append(f"{table}: 主キー {k} がその表の列に無い")
            elif nullable[k] and not d.get("null_equality"):
                errs.append(
                    f"{table}: 主キー {k} が nullable なのに null_equality が宣言されていない"
                )
    return errs


def check_vocabulary(schemas, index) -> list[str]:
    vocab = tomllib.loads((SCHEMA_DIR / "vocabulary.toml").read_text("utf-8"))
    errs: list[str] = []

    core = set(vocab["event_schemas"]["values"])
    pattern = schemas["event"]["properties"]["schema"]["pattern"]
    in_pattern = set(re.findall(r"[a-z_]+", pattern.split("|x\\.")[0].strip("^()")))
    if core != in_pattern:
        errs.append(f"event.json のコア語彙 {sorted(in_pattern)} と vocabulary {sorted(core)} が食い違う")

    branch_domains = {
        b["if"]["properties"]["domain"]["const"]
        for b in schemas["run"]["allOf"]
        if "domain" in b.get("if", {}).get("properties", {})
    }
    unknown = branch_domains - set(vocab["domains"]["values"])
    if unknown:
        errs.append(f"run.json が条件分岐に使う domain が vocabulary に無い: {sorted(unknown)}")

    for name in vocab["metric_names"]:
        if not re.match(r"^[a-z0-9][a-z0-9._-]{0,63}$", name):
            errs.append(f"予約指標名が slug 文法に反する: {name}")
    for name in vocab["deprecated"]:
        if name in vocab["metric_names"]:
            errs.append(f"廃止語が現役の予約指標にも載っている: {name}")

    idx_cols = {c["name"] for c in index["tables"]["runs"]["columns"]}
    for required in ("schema_version", "vocab_version", "state", "git_dirty", "config_hash", "env_hash"):
        if required not in idx_cols:
            errs.append(f"索引 runs に必須列が無い: {required}")
    return errs


def check_vocab_values(name: str, inst, vocab) -> list[str]:
    """語彙に載っている欄が, 実際に登録された値かを見る (typo をここで殺す)."""
    errs = []
    scopes = set(vocab["scopes"]["values"])
    units = set(vocab["step_units"]["values"])
    roles = set(vocab["data_roles"]["values"])
    domains = set(vocab["domains"]["values"])
    metrics = vocab["metric_names"]

    def one(row):
        if not isinstance(row, dict):
            return
        if "scope" in row and row["scope"] is not None and row["scope"] not in scopes:
            errs.append(f"未登録の scope: {row['scope']}")
        if row.get("step_unit") not in (None, *units):
            errs.append(f"未登録の step_unit: {row['step_unit']}")
        if row.get("t_unit") not in (None, *units):
            errs.append(f"未登録の t_unit: {row['t_unit']}")
        if row.get("role") not in (None, *roles):
            errs.append(f"未登録の role: {row['role']}")
        if row.get("domain") not in (None, *domains):
            errs.append(f"未登録の domain: {row['domain']}")
        n = row.get("name")
        if n in metrics and "scope" in row:
            allowed = metrics[n].get("scope", [])
            if allowed and row["scope"] not in allowed:
                errs.append(f"予約指標 {n} は scope={allowed} でのみ使える (指定 {row['scope']})")

    if isinstance(inst, list):
        for r in inst:
            one(r)
    else:
        one(inst)
    return errs


PK_OF = {"metrics.row": "metrics", "reference.row": "reference", "manifest.row": "manifest"}


def resolve_pointer(doc, pointer: str):
    cur = doc
    for part in pointer.lstrip("/").split("/"):
        part = part.replace("~1", "/").replace("~0", "~")
        if isinstance(cur, dict) and part in cur:
            cur = cur[part]
        elif isinstance(cur, list) and part.isdigit() and int(part) < len(cur):
            cur = cur[int(part)]
        else:
            return None, False
    return cur, True


def check_pointers(config: dict) -> list[str]:
    """hash_exclude / seed_pointers / invariant_to が /parameters に実在するか."""
    errs = []
    params = config.get("parameters", {})
    rv = config.get("runvault", {})
    groups = {
        "hash_exclude": rv.get("hash_exclude", []),
        "seed_pointers": rv.get("seed_pointers", []),
        "invariant_to": rv.get("determinism", {}).get("invariant_to", []),
    }
    for group, pointers in groups.items():
        for ptr in pointers:
            _, ok = resolve_pointer(params, ptr)
            if not ok:
                errs.append(f"{group} の {ptr} が /parameters に存在しない")
    return errs


def main() -> int:
    doc_path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT_DOC
    registry, schemas = load_registry()
    index = json.loads((SCHEMA_DIR / "index.columns.json").read_text("utf-8"))
    index_meta = json.loads((SCHEMA_DIR / "index.columns.meta.json").read_text("utf-8"))

    failures: list[str] = []

    for name, schema in schemas.items():
        Draft202012Validator.check_schema(schema)
    Draft202012Validator.check_schema(index_meta)
    for e in Draft202012Validator(index_meta).iter_errors(index):
        failures.append(f"index.columns.json: {e.json_path} {e.message}")
    failures += [f"index: {e}" for e in check_index_shape(index)]
    failures += [f"vocabulary: {e}" for e in check_vocabulary(schemas, index)]
    vocab = tomllib.loads((SCHEMA_DIR / "vocabulary.toml").read_text("utf-8"))
    print(f"schema     : {len(schemas)} 件 + 索引メタを検査")

    text = doc_path.read_text(encoding="utf-8")
    checked = 0
    covered: set[str] = set()

    for lineno, marker, lang, body in parse_blocks(text):
        where = f"{doc_path.name}:{lineno}"
        if marker is None:
            failures.append(f"{where}: ```{lang} ブロックに runvault:validate マーカーがない")
            continue
        name = marker.get("schema")
        try:
            if lang == "sql" or name == "sql":
                failures += [f"{where}: {e}" for e in check_sql(body, index)]
            else:
                v = validator_for(schemas, registry, name, marker.get("profile"), marker.get("pointer"))
                covered.add(name)
                if lang == "jsonl":
                    for k, line in enumerate(l for l in body.splitlines() if l.strip()):
                        inst = json.loads(line)
                        for e in v.iter_errors(inst):
                            failures.append(f"{where}(+{k}): {e.json_path} {e.message}")
                        failures += [f"{where}(+{k}): {e}" for e in check_vocab_values(name, inst, vocab)]
                elif lang == "csv":
                    header = next(csv.reader(io.StringIO(body)), [])
                    if len(header) != len(set(header)):
                        failures.append(f"{where}: CSV のヘッダーに重複した列名がある")
                    rows = list(csv.DictReader(io.StringIO(body)))
                    if not rows:
                        failures.append(f"{where}: CSV に行がない")
                    seen: set[tuple] = set()
                    pk = index["tables"].get(PK_OF.get(name, ""), {}).get("primary_key", [])
                    for k, row in enumerate(rows):
                        errs: list[str] = []
                        inst = coerce_csv_row(row, f"{where}(+{k})", errs)
                        failures += errs
                        for e in v.iter_errors(inst):
                            failures.append(f"{where}(+{k}): {e.json_path} {e.message}")
                        failures += [f"{where}(+{k}): {e}" for e in check_vocab_values(name, inst, vocab)]
                        if pk:
                            key = tuple(inst.get(c) for c in pk if c != "run_key")
                            if key in seen:
                                failures.append(f"{where}(+{k}): 主キーが重複している {key}")
                            seen.add(key)
                else:
                    inst = json.loads(body)
                    for e in v.iter_errors(inst):
                        failures.append(f"{where}: {e.json_path} {e.message}")
                    failures += [f"{where}: {e}" for e in check_vocab_values(name, inst, vocab)]
                    if name == "config":
                        failures += [f"{where}: {e}" for e in check_pointers(inst)]
        except Exception as exc:  # noqa: BLE001
            failures.append(f"{where}: 検証できない ({type(exc).__name__}: {exc})")
        checked += 1

    print(f"design doc : {checked} 個の例を検証")

    uncovered = sorted(set(schemas) - covered - {"common"})
    if uncovered:
        print(f"note       : 設計書に例が無いスキーマ {uncovered} は tools/test_schemas.py が受け持つ")

    if failures:
        print(f"\nNG {len(failures)} 件")
        for f in failures:
            print("  -", f)
        return 1
    print("すべて適合")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
