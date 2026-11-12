"""Shared evaluation utilities: relevance matching, metrics, bootstrap confidence intervals."""
from __future__ import annotations

import json
import os

import numpy as np

from groups import BENCH


def load_queries(pkg: str, types: tuple[str, ...] | None = None) -> list[dict]:
    with open(os.path.join(BENCH, "queries", f"{pkg}.json")) as f:
        qs = json.load(f)
    return [q for q in qs if types is None or q["type"] in types]


TYPE_KINDS = ("class", "struct", "enum", "trait", "interface", "type")


def is_relevant(q: dict, path: str, qualname: str, name: str, kind: str | None = None) -> bool:
    # a struct's impl block and a class's constructor share its name; the type's own chunk answers
    if q.get("target_kind") == "type" and kind not in TYPE_KINDS:
        return False
    m = q["match"]
    if m == "qualname":
        return path == q["path"] and qualname == q["qualname"]
    if m == "name":
        return name == q["name"]
    if m == "qualname_suffix":
        return qualname == q["qualname"] or qualname.endswith("." + q["qualname"]) \
            or q["qualname"].endswith("." + qualname)
    raise ValueError(m)


def first_relevant_rank(q: dict, hits, limit: int = 10) -> int | None:
    """hits: iterable of objects with path/qualname/name attributes (best first)."""
    for i, h in enumerate(hits, 1):
        if i > limit:
            break
        if is_relevant(q, h.path, h.qualname, h.name, getattr(h, "kind", None)):
            return i
    return None


def summarize(ranks: list[int | None]) -> dict:
    n = len(ranks)
    rr = np.array([1.0 / r if r else 0.0 for r in ranks])
    return {
        "n": n,
        "MRR": float(rr.mean()) if n else 0.0,
        "hit@1": float(np.mean([r == 1 for r in ranks])) if n else 0.0,
        "hit@5": float(np.mean([bool(r and r <= 5) for r in ranks])) if n else 0.0,
        "hit@10": float(np.mean([bool(r) for r in ranks])) if n else 0.0,
    }


def bootstrap_ci(values: list[float], iters: int = 1000, seed: int = 0) -> tuple[float, float]:
    v = np.asarray(values, float)
    if len(v) == 0:
        return 0.0, 0.0
    rng = np.random.default_rng(seed)
    means = np.array([v[rng.integers(0, len(v), len(v))].mean() for _ in range(iters)])
    return float(np.percentile(means, 2.5)), float(np.percentile(means, 97.5))


def mrr_values(ranks: list[int | None]) -> list[float]:
    return [1.0 / r if r else 0.0 for r in ranks]


def fmt_row(label: str, ranks: list[int | None]) -> str:
    s = summarize(ranks)
    lo, hi = bootstrap_ci(mrr_values(ranks))
    return (f"{label:<22} n={s['n']:<5} MRR {s['MRR']:.3f} [{lo:.3f},{hi:.3f}]  "
            f"hit@1 {s['hit@1']:.3f}  hit@5 {s['hit@5']:.3f}  hit@10 {s['hit@10']:.3f}")


def paired_diff_ci(a: list[int | None], b: list[int | None], iters: int = 1000, seed: int = 0):
    """Bootstrap CI of MRR(a) - MRR(b) over the same queries."""
    d = np.array(mrr_values(a)) - np.array(mrr_values(b))
    rng = np.random.default_rng(seed)
    means = np.array([d[rng.integers(0, len(d), len(d))].mean() for _ in range(iters)])
    return float(d.mean()), float(np.percentile(means, 2.5)), float(np.percentile(means, 97.5))
