"""Canonical JSON (design note §3.3).

One document must have exactly one byte representation, so that two
implementations hashing the same configuration reach the same digest.
"""
from __future__ import annotations

import math
import unicodedata
from decimal import Decimal
from typing import Any

__all__ = ["CanonicalError", "canonicalize"]

_SHORT_ESCAPES = {0x08: "\\b", 0x09: "\\t", 0x0A: "\\n", 0x0C: "\\f", 0x0D: "\\r"}


class CanonicalError(ValueError):
    """A value that has no canonical form."""


def canonicalize(value: Any) -> str:
    out: list[str] = []
    _write(value, out)
    return "".join(out)


def _write(value: Any, out: list[str]) -> None:
    # bool before int: bool is a subclass of int in Python.
    if value is None:
        out.append("null")
    elif isinstance(value, bool):
        out.append("true" if value else "false")
    elif isinstance(value, int):
        out.append(str(value))
    elif isinstance(value, float):
        out.append(_number(value))
    elif isinstance(value, str):
        out.append(_string(value))
    elif isinstance(value, (list, tuple)):
        out.append("[")
        for i, item in enumerate(value):
            if i:
                out.append(",")
            _write(item, out)
        out.append("]")
    elif isinstance(value, dict):
        out.append("{")
        for i, (key, item) in enumerate(_sorted_items(value)):
            if i:
                out.append(",")
            out.append(_string(key))
            out.append(":")
            _write(item, out)
        out.append("}")
    else:
        raise CanonicalError(f"value of type {type(value).__name__} has no canonical form")


def _sorted_items(obj: dict[Any, Any]) -> list[tuple[str, Any]]:
    normalized: dict[str, Any] = {}
    for key, value in obj.items():
        if not isinstance(key, str):
            raise CanonicalError(f"object key of type {type(key).__name__} is not a string")
        nfc = unicodedata.normalize("NFC", key)
        if nfc in normalized:
            raise CanonicalError(f"keys collide after NFC normalization: {nfc!r}")
        normalized[nfc] = value
    # str comparison in Python is by code point, which is what the spec asks for.
    return sorted(normalized.items(), key=lambda kv: kv[0])


def _string(value: str) -> str:
    out = ['"']
    for ch in unicodedata.normalize("NFC", value):
        code = ord(ch)
        if ch == '"':
            out.append('\\"')
        elif ch == "\\":
            out.append("\\\\")
        elif code < 0x20:
            out.append(_SHORT_ESCAPES.get(code) or f"\\u{code:04x}")
        else:
            out.append(ch)
    out.append('"')
    return "".join(out)


def _number(value: float) -> str:
    if math.isnan(value) or math.isinf(value):
        raise CanonicalError(f"{value!r} has no canonical form")
    if value == 0.0:  # folds -0.0
        return "0.0"
    # repr() is the shortest decimal that round-trips; Decimal spells it out
    # without an exponent.
    text = format(Decimal(repr(value)), "f")
    return text if "." in text else text + ".0"
