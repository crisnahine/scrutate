"""Scrutate: universal instant code search (Engine A) and persistent code intelligence (Engine B).

Everything here is implemented in Rust (`scrutate._native`); this package only re-exports it.
"""
from ._native import Match, Hit, Repo, GrepIter, grep, version, __version__  # noqa: F401

__all__ = ["Match", "Hit", "Repo", "GrepIter", "grep", "version", "__version__"]
