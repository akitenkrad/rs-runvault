"""The two identifiers (design note §3.1, §4.3)."""
from __future__ import annotations

from datetime import datetime, timedelta, timezone

import pytest

from runvault.errors import SpecError
from runvault.ids import (
    new_run_uid,
    run_slug,
    slug_hash_prefixes,
    timestamp_part,
    validate_slug,
)

JST = timezone(timedelta(hours=9))


@pytest.mark.parametrize("value", ["schelling", "multiturn-jailbreak.v2_1", "a" * 64, "0"])
def test_a_path_element_is_accepted(value: str) -> None:
    validate_slug("experiment", value)


@pytest.mark.parametrize("value", ["..", "a/b", "Schelling", "a b", "", "-leading", "a" * 65])
def test_slugs_reject_path_traversal_and_uppercase(value: str) -> None:
    with pytest.raises(SpecError):
        validate_slug("experiment", value)


def test_a_run_uid_starts_in_the_range_the_schema_allows() -> None:
    uid = new_run_uid(datetime.now(JST))
    assert len(uid) == 26
    assert uid[0] in "01234567"


def test_run_uids_sort_by_time() -> None:
    early = new_run_uid(datetime(2026, 8, 30, 10, 0, 0, tzinfo=JST))
    late = new_run_uid(datetime(2026, 8, 30, 11, 0, 0, tzinfo=JST))
    assert early < late


def test_a_slug_carries_both_hash_prefixes() -> None:
    slug = run_slug("main", "20260830_101500", "9f2c41ab" * 8, "3b1d" * 16)
    assert slug == "main_20260830_101500_9f2c41ab_3b1d"
    assert slug_hash_prefixes(slug) == ("9f2c41ab", "3b1d")


def test_a_collision_suffix_survives_the_round_trip() -> None:
    slug = run_slug("sweep", "20260830_101500", "0" * 64, "1" * 64, 2)
    assert slug == "sweep_20260830_101500_00000000_1111-2"
    assert slug_hash_prefixes(slug) == ("00000000", "1111")


def test_a_hyphen_in_the_subcommand_is_not_a_collision_suffix() -> None:
    slug = run_slug("multi-turn", "20260830_101500", "a" * 64, "b" * 64)
    assert slug_hash_prefixes(slug) == ("aaaaaaaa", "bbbb")


def test_a_legacy_directory_name_is_not_a_slug() -> None:
    assert slug_hash_prefixes("20260620_162729_sweep") is None


def test_the_timestamp_part_is_local_and_sortable() -> None:
    assert timestamp_part(datetime(2026, 8, 30, 10, 15, 0, tzinfo=JST)) == "20260830_101500"
