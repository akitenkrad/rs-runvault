"""What the writer raises.

`SpecError` means the caller asked for something the specification forbids;
`VerifyError` means a run that was already written contradicts itself. Keeping
them apart is what lets `finish()` mark a run failed for the second while a
mistake in the API surfaces as the first.
"""
from __future__ import annotations

__all__ = ["GitError", "RunvaultError", "SpecError", "VerifyError"]


class RunvaultError(ValueError):
    """Base of everything this package raises on its own account."""


class SpecError(RunvaultError):
    """A value the specification does not allow."""


class VerifyError(RunvaultError):
    """A run directory that disagrees with itself."""


class GitError(RunvaultError):
    """`git` could not answer a question the record needs."""
