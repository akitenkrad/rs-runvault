"""The core vocabulary, read from the copy of `schema/v1/vocabulary.toml`
shipped inside the package.

The package is installed away from the repository, so it cannot read the schema
directory at run time; restating the registry in Python instead would let
`vocab_version` and the reserved metric names drift apart from it. A test holds
the packaged copy byte-identical to the schema file.
"""
from __future__ import annotations

import tomllib
from dataclasses import dataclass, field
from functools import lru_cache
from importlib import resources

__all__ = ["ReservedMetric", "Vocabulary", "get", "source_bytes"]


@dataclass(frozen=True)
class ReservedMetric:
    """A reserved metric name and the scopes it is allowed at."""

    scopes: tuple[str, ...]
    meaning: str


@dataclass(frozen=True)
class Vocabulary:
    """The parsed registry."""

    version: str
    domains: tuple[str, ...] = ()
    data_roles: tuple[str, ...] = ()
    scopes: tuple[str, ...] = ()
    step_units: tuple[str, ...] = ()
    event_schemas: tuple[str, ...] = ()
    metric_names: dict[str, ReservedMetric] = field(default_factory=dict)

    def metric_allowed_at(self, name: str, scope: str) -> bool:
        """Whether a metric name may be written at this scope.

        Only the reserved names are constrained: an experiment's own metric can
        be recorded at any scope.
        """
        reserved = self.metric_names.get(name)
        return True if reserved is None else scope in reserved.scopes


def source_bytes() -> bytes:
    """The registry exactly as it is packaged, for the drift test."""
    return (resources.files(__package__) / "data" / "vocabulary.toml").read_bytes()


@lru_cache(maxsize=1)
def get() -> Vocabulary:
    """The core vocabulary."""
    doc = tomllib.loads(source_bytes().decode("utf-8"))

    def values(table: str) -> tuple[str, ...]:
        return tuple(doc.get(table, {}).get("values", ()))

    return Vocabulary(
        version=doc["vocab_version"],
        domains=values("domains"),
        data_roles=values("data_roles"),
        scopes=values("scopes"),
        step_units=values("step_units"),
        event_schemas=values("event_schemas"),
        metric_names={
            name: ReservedMetric(tuple(spec.get("scope", ())), spec.get("meaning", ""))
            for name, spec in doc.get("metric_names", {}).items()
        },
    )
