"""The cross-implementation vectors are the contract.

Every field of every case in `schema/v1/testvectors/*.json` must be reproducible
from the inputs alone. A mismatch means this implementation is wrong; the
vectors are never adjusted to fit it.
"""
from __future__ import annotations

import json
from pathlib import Path

import pytest

from runvault.canonical import canonicalize
from runvault.framing import length_prefixed
from runvault.hashes import blake3_hex, config_hash, env_hash, execution_hash

VECTORS = Path(__file__).resolve().parents[2] / "schema" / "v1" / "testvectors"


def _cases(filename: str) -> list:
    doc = json.loads((VECTORS / filename).read_text(encoding="utf-8"))
    return doc["cases"]


def _ids(cases: list) -> list[str]:
    return [c["name"] for c in cases]


CANONICALIZE = _cases("canonicalize.json")
LENGTH_PREFIX = _cases("length_prefix.json")
HASHES = _cases("hashes.json")


@pytest.mark.parametrize("case", CANONICALIZE, ids=_ids(CANONICALIZE))
def test_canonicalize(case: dict) -> None:
    got = canonicalize(case["value"])
    assert got == case["canonical"]
    assert blake3_hex(got.encode("utf-8")) == case["blake3"]


@pytest.mark.parametrize("case", LENGTH_PREFIX, ids=_ids(LENGTH_PREFIX))
def test_length_prefix(case: dict) -> None:
    got = length_prefixed(case["inputs"])
    assert got.decode("utf-8") == case["joined"]
    assert blake3_hex(got) == case["blake3"]


@pytest.mark.parametrize("case", HASHES, ids=_ids(HASHES))
def test_env_hash(case: dict) -> None:
    assert env_hash(case["env"]) == case["expect"]["env_hash"]


@pytest.mark.parametrize("case", HASHES, ids=_ids(HASHES))
def test_config_hash(case: dict) -> None:
    canonical, digest = config_hash(case["config"], case["data"])
    assert canonical == case["expect"]["config_canonical"]
    assert digest == case["expect"]["config_hash"]


@pytest.mark.parametrize("case", HASHES, ids=_ids(HASHES))
def test_execution_hash(case: dict) -> None:
    _, cfg = config_hash(case["config"], case["data"])
    got = execution_hash(
        config_hash_hex=cfg,
        config=case["config"],
        code=case["code"],
        env_hash_hex=env_hash(case["env"]),
    )
    assert got == case["expect"]["execution_hash"]
