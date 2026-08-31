"""Rules the specification states but the shared vectors do not exercise."""
from __future__ import annotations

import math

import pytest

from runvault.canonical import CanonicalError, canonicalize
from runvault.pointer import PointerError, parse_pointer, prune, resolve, resolve_exclusions


@pytest.mark.parametrize("value", [math.nan, math.inf, -math.inf])
def test_nan_and_inf_have_no_canonical_form(value: float) -> None:
    with pytest.raises(CanonicalError):
        canonicalize({"x": value})


def test_keys_colliding_after_nfc_are_an_error() -> None:
    with pytest.raises(CanonicalError):
        canonicalize({"\u304c": 1, "\u304b\u3099": 2})  # composed and decomposed が


def test_escaped_tokens() -> None:
    assert parse_pointer("/a~1b/c~0d") == ["a/b", "c~d"]


def test_a_pointer_must_start_with_a_slash() -> None:
    with pytest.raises(PointerError):
        parse_pointer("seed")


@pytest.mark.parametrize("pointer", ["/missing", "/a/9", "/a/x", "/a/0/deeper"])
def test_a_pointer_that_does_not_resolve_is_an_error(pointer: str) -> None:
    with pytest.raises(PointerError):
        resolve({"a": [1, 2]}, pointer)


def test_resolves_through_arrays() -> None:
    assert resolve({"a": [{"b": 7}]}, "/a/0/b") == 7


def test_pruning_arrays_does_not_depend_on_pointer_order() -> None:
    document = {"xs": [0, 1, 2, 3]}
    assert prune(document, ["/xs/1", "/xs/2"]) == {"xs": [0, 3]}
    assert prune(document, ["/xs/2", "/xs/1"]) == {"xs": [0, 3]}


def test_pruning_leaves_the_input_untouched() -> None:
    document = {"a": 1, "b": {"c": 2}}
    assert prune(document, ["/b/c"]) == {"a": 1, "b": {}}
    assert document == {"a": 1, "b": {"c": 2}}


def test_a_shallower_exclusion_wins_over_a_deeper_one() -> None:
    assert prune({"a": {"b": 1}}, ["/a", "/a/b"]) == {}


def test_exclusion_precedence_drops_duplicates_from_the_later_lists() -> None:
    config = {
        "runvault": {
            "hash_exclude": ["/out", "/out"],
            "seed_pointers": ["/out", "/seed"],
            "determinism": {"invariant_to": ["/seed", "/threads"]},
        },
        "parameters": {"out": "x", "seed": 1, "threads": 8},
    }
    exclusions = resolve_exclusions(config)
    assert exclusions.hash_exclude == ("/out",)
    assert exclusions.seed_pointers == ("/seed",)
    assert exclusions.invariant_to == ("/threads",)
    assert exclusions.config_excluded == ("/out", "/seed", "/threads")


def test_an_exclusion_that_points_nowhere_is_an_error() -> None:
    config = {"runvault": {"hash_exclude": ["/typo"]}, "parameters": {"seed": 1}}
    with pytest.raises(PointerError):
        resolve_exclusions(config)
