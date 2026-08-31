"""Length-prefixed concatenation (design note §3.3).

A separator can be forged by an input that contains it; a byte length cannot.
Every input occupies a slot, so dropping an empty one changes the result.
"""
from __future__ import annotations

from typing import Iterable

__all__ = ["length_prefixed"]


def length_prefixed(inputs: Iterable[bytes | str]) -> bytes:
    out = bytearray()
    for item in inputs:
        raw = item.encode("utf-8") if isinstance(item, str) else bytes(item)
        out += str(len(raw)).encode("ascii")
        out += b":"
        out += raw
    return bytes(out)
