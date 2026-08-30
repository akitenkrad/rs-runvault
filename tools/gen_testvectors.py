#!/usr/bin/env python3
"""Writes schema/v1/testvectors/ from a second, independent implementation of §3.3.

The point of the vectors is that two implementations agree. This script is that
second implementation: it is written from the design note, not from the Rust
code, and the Rust test asserts every field of every case against it. If the two
disagree, one of them is wrong -- do not regenerate to make a test pass.

    uv run --with blake3 python tools/gen_testvectors.py
"""

from __future__ import annotations

import json
import unicodedata
from decimal import Decimal
from pathlib import Path

import blake3 as blake3_mod

OUT = Path(__file__).resolve().parent.parent / "schema" / "v1" / "testvectors"

SHORT_ESCAPES = {"\b": "\\b", "\f": "\\f", "\n": "\\n", "\r": "\\r", "\t": "\\t"}


# --- canonical JSON -------------------------------------------------------


def nfc(s: str) -> str:
    return unicodedata.normalize("NFC", s)


def enc_string(s: str) -> str:
    out = ['"']
    for ch in nfc(s):
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif ch in SHORT_ESCAPES:
            out.append(SHORT_ESCAPES[ch])
        elif ord(ch) < 0x20:
            out.append(f"\\u{ord(ch):04x}")
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def enc_float(x: float) -> str:
    if x != x or x in (float("inf"), float("-inf")):
        raise ValueError("NaN / Inf cannot be written")
    if x == 0.0:
        x = 0.0  # folds -0.0
    s = format(Decimal(repr(x)), "f")  # shortest round-trip, never an exponent
    return s if "." in s else s + ".0"


def canonicalize(value) -> str:
    if value is None:
        return "null"
    if value is True:
        return "true"
    if value is False:
        return "false"
    if isinstance(value, int):
        return str(value)
    if isinstance(value, float):
        return enc_float(value)
    if isinstance(value, str):
        return enc_string(value)
    if isinstance(value, list):
        return "[" + ",".join(canonicalize(v) for v in value) + "]"
    if isinstance(value, dict):
        items = [(nfc(k), v) for k, v in value.items()]
        keys = [k for k, _ in items]
        if len(set(keys)) != len(keys):
            raise ValueError("keys collide after NFC")
        items.sort(key=lambda kv: kv[0])
        return "{" + ",".join(f"{enc_string(k)}:{canonicalize(v)}" for k, v in items) + "}"
    raise TypeError(type(value))


# --- framing and hashes ---------------------------------------------------


def lp(*inputs: bytes) -> bytes:
    return b"".join(str(len(i)).encode() + b":" + i for i in inputs)


def b3(data: bytes) -> str:
    return blake3_mod.blake3(data).hexdigest()


def utf8(s) -> bytes:
    return ("" if s is None else s).encode("utf-8")


def env_hash(env: dict) -> str:
    locks = sorted(env.get("locks", []), key=lambda l: (l["kind"], l["file"]))
    blob = b"".join(
        lp(utf8(l["kind"]), utf8(l["file"]), utf8(l["hash"]["algorithm"]), utf8(l["hash"]["value"]))
        for l in locks
    )
    return b3(
        lp(
            utf8(env["os"]),
            utf8(env["arch"]),
            utf8(env.get("rustc_version")),
            utf8(env.get("python_version")),
            blob,
        )
    )


def data_identity(data: list) -> bytes:
    blob = b""
    for d in sorted(data, key=lambda d: (d["role"], d["name"])):
        h = d.get("hash") or {}
        n = d.get("n")
        blob += lp(
            utf8(d["role"]),
            utf8(d["name"]),
            utf8(d.get("dataset_id")),
            utf8(d.get("version")),
            utf8(d.get("split")),
            utf8(h.get("algorithm")),
            utf8(h.get("value")),
            utf8(d.get("hash_scope")),
            utf8(d.get("uri")),
            b"" if n is None else str(n).encode(),
        )
    return blob


def parse_pointer(raw: str) -> list[str]:
    if not raw.startswith("/"):
        raise ValueError(raw)
    return [t.replace("~1", "/").replace("~0", "~") for t in raw[1:].split("/")]


def resolve(root, tokens: list[str]):
    cur = root
    for t in tokens:
        if isinstance(cur, dict):
            cur = cur[t]
        elif isinstance(cur, list):
            cur = cur[int(t)]
        else:
            raise KeyError(t)
    return cur


def prune(value, remove: list[list[str]], path=()):
    if isinstance(value, dict):
        out = {}
        for k, v in value.items():
            if list(path + (k,)) not in remove:
                out[k] = prune(v, remove, path + (k,))
        return out
    if isinstance(value, list):
        out = []
        for i, v in enumerate(value):
            if list(path + (str(i),)) not in remove:
                out.append(prune(v, remove, path + (str(i),)))
        return out
    return value


def exclusions(block: dict, parameters: dict):
    def parsed(key_list):
        seen, out = set(), []
        for raw in key_list:
            tokens = parse_pointer(raw)
            resolve(parameters, tokens)  # must exist
            if raw not in seen:
                seen.add(raw)
                out.append(raw)
        return out

    hard = parsed(block.get("hash_exclude", []))
    seeds = [p for p in parsed(block.get("seed_pointers", [])) if p not in hard]
    invariant = [
        p
        for p in parsed(block.get("determinism", {}).get("invariant_to", []))
        if p not in hard and p not in seeds
    ]
    return hard, seeds, invariant


def config_hash(config: dict, data: list) -> tuple[str, str]:
    parameters = config["parameters"]
    hard, seeds, invariant = exclusions(config["runvault"], parameters)
    remove = [parse_pointer(p) for p in hard + seeds + invariant]
    canonical = canonicalize(prune(parameters, remove))
    return canonical, b3(lp(canonical.encode(), data_identity(data)))


def execution_hash(cfg_hash: str, config: dict, code, env_h: str) -> str:
    parameters = config["parameters"]
    hard, seeds, _ = exclusions(config["runvault"], parameters)
    blob = b""
    for raw in sorted(seeds):
        blob += lp(raw.encode(), canonicalize(resolve(parameters, parse_pointer(raw))).encode())
    if code is None:
        commit, dirty, dirty_hash = b"", b"", b""
    else:
        commit = utf8(code["git_commit"])
        dirty = b"true" if code["git_dirty"] else b"false"
        dirty_hash = utf8((code.get("dirty_hash") or {}).get("value"))
    return b3(lp(cfg_hash.encode(), blob, commit, dirty, dirty_hash, utf8(env_h)))


# --- the cases ------------------------------------------------------------

CANONICALIZE_CASES = [
    ("key-order-is-by-code-point", {"b": 1, "a": 2, "A": 3, "あ": 4}),
    ("code-point-order-puts-ascii-before-latin-1", {"é": 1, "z": 2, "e": 3}),
    ("floats-keep-a-fractional-part-and-integers-do-not", {"a": 1.0, "b": -0.0, "c": 0.5, "d": 1, "e": -3}),
    ("floats-never-use-exponent-notation", {"big": 1e21, "small": 1e-7}),
    ("nested-objects-are-sorted-and-arrays-keep-their-order", {"z": [3, 1, {"b": 2, "a": 1}], "y": {"n": None}}),
    ("a-null-is-recorded", {"a": 1, "b": None}),
    ("a-missing-key-is-not-the-same-document", {"a": 1}),
    ("only-quote-backslash-and-control-characters-are-escaped", {"k": 'a"b\\c\ndef'}),
    ("the-short-escapes-are-used-where-json-has-them", {"k": "\b\f\n\r\t"}),
    ("delete-is-not-escaped", {"k": "ab"}),
    ("strings-and-keys-are-nfc-normalized", {"が": "が"}),
    ("non-ascii-is-not-escaped", {"研究": "再現"}),
    ("an-empty-object-and-an-empty-array", {"o": {}, "a": []}),
    ("booleans", {"t": True, "f": False}),
    ("large-integers-stay-integers", {"n": 9007199254740993, "neg": -9007199254740993}),
]

LENGTH_PREFIX_CASES = [
    ("a-separator-would-be-ambiguous-but-a-length-is-not", ["a:b", "c"]),
    ("the-other-split-of-the-same-characters", ["a", "b:c"]),
    ("an-empty-input-still-occupies-a-slot", ["", "x"]),
    ("two-empty-inputs", ["", ""]),
    ("no-inputs-at-all", []),
    ("the-length-is-in-bytes-not-characters", ["あ"]),
]

CARGO_LOCK = {
    "kind": "cargo",
    "file": "lock/Cargo.lock",
    "hash": {"algorithm": "blake3", "value": "c" * 64},
}
UV_LOCK = {
    "kind": "uv",
    "file": "lock/uv.lock",
    "hash": {"algorithm": "blake3", "value": "d" * 64},
}

HASH_CASES = [
    {
        "name": "schelling-main",
        "comment": "The real schelling1971 config carries output_dir, which changes every run.",
        "config": {
            "schema_version": "1.0",
            "run_uid": "01K3QZ8F7H9M2N4P6R8T0V2X4Z",
            "runvault": {
                "hash_exclude": ["/output_dir", "/log_level"],
                "seed_pointers": ["/seed"],
                "determinism": {"invariant_to": ["/threads"]},
                "sync_include": [],
                "sync_exclude": [],
            },
            "parameters": {
                "rows": 13,
                "cols": 16,
                "threshold": 0.5,
                "seed": 42,
                "threads": 8,
                "log_level": "info",
                "output_dir": "results/schelling/main_20260830_101500_9f2c41ab_3b1d",
            },
        },
        "data": [
            {
                "role": "init",
                "name": "schelling-grid",
                "hash": {"algorithm": "blake3", "value": "7b" * 32},
                "hash_scope": "file",
                "n": 208,
            }
        ],
        "env": {
            "os": "macos",
            "arch": "aarch64",
            "rustc_version": "1.94.0",
            "python_version": None,
            "locks": [CARGO_LOCK],
        },
        "code": {
            "git_commit": "a" * 40,
            "git_dirty": True,
            "dirty_hash": {"algorithm": "blake3", "value": "9" * 64},
        },
    },
    {
        "name": "anomaly-detection-two-datasets",
        "comment": "Datasets are sorted by (role, name); split and version are part of the identity.",
        "config": {
            "schema_version": "1.0",
            "run_uid": "01K3QZ8F7H9M2N4P6R8T0V2X50",
            "runvault": {
                "hash_exclude": [],
                "seed_pointers": ["/model/seed", "/split_seed"],
                "determinism": {},
                "sync_include": [],
                "sync_exclude": [],
            },
            "parameters": {
                "window_sec": 60,
                "model": {"kind": "iforest", "trees": 200, "seed": 7},
                "split_seed": 11,
                "features": ["bytes", "pkts", "duration"],
            },
        },
        "data": [
            {
                "role": "train",
                "name": "cicids2017",
                "dataset_id": "cicids2017@2017-07",
                "split": "day1-4",
                "n": 2830743,
                "hash_scope": "dir_manifest",
                "hash": {"algorithm": "blake3", "value": "7b1f1c0d9a4e2f6b8c3d5e7a9b1c3d5e7f9a1b3c5d7e9f1a3b5c7d9e1f3a5b7c"},
            },
            {"role": "eval", "name": "internal-pcap", "dataset_id": "internal-pcap@2026q2", "n": 51200},
        ],
        "env": {
            "os": "linux",
            "arch": "x86_64",
            "rustc_version": None,
            "python_version": "3.13.1",
            "locks": [UV_LOCK, CARGO_LOCK],
        },
        "code": {"git_commit": "b" * 40, "git_dirty": False},
    },
    {
        "name": "manual-origin-no-code-no-data-no-locks",
        "comment": "code = null still contributes three zero-length inputs; dropping them would change the number of inputs.",
        "config": {
            "schema_version": "1.0",
            "run_uid": "01K3QZ8F7H9M2N4P6R8T0V2X51",
            "runvault": {
                "hash_exclude": [],
                "seed_pointers": [],
                "determinism": {},
                "sync_include": [],
                "sync_exclude": [],
            },
            "parameters": {},
        },
        "data": [],
        "env": {
            "os": "macos",
            "arch": "aarch64",
            "rustc_version": None,
            "python_version": None,
            "locks": [],
        },
        "code": None,
    },
]


def main() -> None:
    OUT.mkdir(parents=True, exist_ok=True)

    canon = {
        "description": (
            "Canonical JSON (design note §3.3). `canonical` is the exact string that gets "
            "hashed; `blake3` is BLAKE3 of its UTF-8 bytes."
        ),
        "cases": [],
    }
    for name, value in CANONICALIZE_CASES:
        c = canonicalize(value)
        canon["cases"].append(
            {"name": name, "value": value, "canonical": c, "blake3": b3(c.encode())}
        )

    prefix = {
        "description": (
            "Length-prefixed concatenation: each input becomes "
            "`<byte length in decimal> \":\" <input>`. Inputs are UTF-8 strings here."
        ),
        "cases": [],
    }
    for name, inputs in LENGTH_PREFIX_CASES:
        joined = lp(*[i.encode() for i in inputs])
        prefix["cases"].append(
            {
                "name": name,
                "inputs": inputs,
                "joined": joined.decode(),
                "blake3": b3(joined),
            }
        )

    hashes = {
        "description": (
            "The stack of three hashes. Every field of `expect` must be reproducible from "
            "`config` / `data` / `env` / `code` alone."
        ),
        "cases": [],
    }
    for case in HASH_CASES:
        e = env_hash(case["env"])
        canonical, cfg = config_hash(case["config"], case["data"])
        ex = execution_hash(cfg, case["config"], case["code"], e)
        hashes["cases"].append(
            {
                **case,
                "expect": {
                    "config_canonical": canonical,
                    "env_hash": e,
                    "config_hash": cfg,
                    "execution_hash": ex,
                },
            }
        )

    for name, doc in [
        ("canonicalize.json", canon),
        ("length_prefix.json", prefix),
        ("hashes.json", hashes),
    ]:
        (OUT / name).write_text(
            json.dumps(doc, ensure_ascii=False, indent=2) + "\n", encoding="utf-8"
        )
        print(f"wrote {name}: {len(doc['cases'])} cases")


if __name__ == "__main__":
    main()
