"""The registry the writer checks reserved names against."""
from __future__ import annotations

from pathlib import Path

from runvault import vocabulary

SCHEMA_COPY = Path(__file__).resolve().parents[2] / "schema" / "v1" / "vocabulary.toml"


def test_the_packaged_copy_is_the_schema_file() -> None:
    # The package is installed away from the repository, so it ships its own
    # copy; this is what stops the two from drifting apart unnoticed.
    assert vocabulary.source_bytes() == SCHEMA_COPY.read_bytes()


def test_cost_is_a_run_level_number_only() -> None:
    vocab = vocabulary.get()
    assert vocab.metric_allowed_at("cost_usd", "run")
    assert not vocab.metric_allowed_at("cost_usd", "trial")
    assert vocab.metric_allowed_at("tokens_in", "trial")


def test_a_name_of_the_experiments_own_is_unconstrained() -> None:
    assert vocabulary.get().metric_allowed_at("segregation_index", "agent")


def test_the_registry_carries_a_version_and_the_core_words() -> None:
    vocab = vocabulary.get()
    assert "." in vocab.version
    assert "simulation" in vocab.domains
    assert "observation" in vocab.event_schemas
    assert "run" in vocab.scopes
    assert "turn" in vocab.step_units
    assert "train" in vocab.data_roles
