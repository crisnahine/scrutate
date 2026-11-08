#!/usr/bin/env python3
"""Needle-in-a-haystack: the held-out benchmark packages indexed together with ~130k chunks of
unrelated code. Measures (a) how accuracy holds up when the repository is large and (b) what the
clustered (approximate) vector scan costs compared with the exact scan, which is only exercised above
IVF_MIN_ROWS chunks.

usage: haystack_eval.py [--variant A] [--json OUT]
Needs the benchmark (make_bench.py) and the speed corpus (speed.py builds $SCRUTATE_WORK/big).
"""
from __future__ import annotations

import json
import os
import shutil
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import bootstrap_ci, first_relevant_rank, load_queries, mrr_values, paired_diff_ci, summarize  # noqa: E402
from groups import BENCH, TEST, WORK  # noqa: E402

from scrutate_ref.index import Index  # noqa: E402
from scrutate_ref.search import FI, Searcher  # noqa: E402

TYPES = ("doc", "kw", "name", "id", "qid")
PROBES = (48, 128)


class H:
    __slots__ = ("path", "qualname", "name")

    def __init__(self, p, q, n):
        self.path, self.qualname, self.name = p, q, n


def build(variant: str) -> str:
    root = os.path.join(WORK, f"haystack_{variant}")
    if os.path.isdir(root):
        return root
    for pkg in TEST:
        shutil.copytree(os.path.join(BENCH, "src", variant, pkg), os.path.join(root, pkg))
    big = os.path.join(WORK, "big")
    for pkg in sorted(os.listdir(big)):
        if pkg not in TEST and not pkg.startswith("."):
            shutil.copytree(os.path.join(big, pkg), os.path.join(root, "haystack", pkg))
    return root


def main() -> None:
    a = sys.argv[1:]
    variant = a[a.index("--variant") + 1] if "--variant" in a else "A"
    root = build(variant)
    ix = Index(root)
    t = time.perf_counter()
    ix.update()
    print(f"index: {ix.stats()['chunks']:,} chunks ({time.perf_counter() - t:.0f}s)", flush=True)
    s = Searcher(ix)
    store = ix.lex
    K = max((len(sg.ivf_ptr) - 1 for sg in store.segs if sg.ivf), default=0)
    configs = ["BM25 (field-weighted)", "Titan exact scan"] + [f"Titan nprobe={p}" for p in PROBES]
    ranks = {t_: {c: [] for c in configs} for t_ in TYPES}
    lat = {c: [] for c in configs}
    for pkg in TEST:
        for q in load_queries(pkg):
            prefix = pkg + "/"

            def strip(hits):
                return [H(h.path[len(prefix):] if h.path.startswith(prefix) else h.path, h.qualname, h.name) for h in hits]

            r = ranks[q["type"]]
            for c, npb in [("Titan exact scan", max(K, 1))] + [(f"Titan nprobe={p}", p) for p in PROBES]:
                store.nprobe = npb
                t0 = time.perf_counter()
                qi, rows, X = s.candidates(q["query"])
                hits = s._hits(qi, rows, X, s.score(X), 10, with_text=False)
                lat[c].append((time.perf_counter() - t0) * 1000)
                r[c].append(first_relevant_rank(q, strip(hits)))
                if c == "Titan exact scan":
                    bm = s._hits(qi, rows, X, X[:, FI["lex_log"]] if len(rows) else np.zeros(0), 10, with_text=False)
                    r["BM25 (field-weighted)"].append(first_relevant_rank(q, strip(bm)))
        print(f"  {pkg} done", flush=True)
    out = {"chunks": ix.stats()["chunks"], "variant": variant, "clusters": K, "types": {}}
    print(f"\n### haystack, variant {variant}: {ix.stats()['chunks']:,} chunks, {K} clusters")
    for c in configs[1:]:
        print(f"query latency {c:<18} p50 {np.median(lat[c]):.1f} ms  p95 {np.percentile(lat[c], 95):.1f} ms")
    for t_ in TYPES:
        print(f"\n{t_}  (n={len(ranks[t_]['Titan exact scan'])})")
        print(f"  {'system':<24} {'MRR [95% CI]':<22} {'hit@1':>6} {'hit@5':>6} {'hit@10':>7}")
        for c in configs:
            m = summarize(ranks[t_][c])
            lo, hi = bootstrap_ci(mrr_values(ranks[t_][c]), 500)
            print(f"  {c:<24} {m['MRR']:.3f} [{lo:.3f},{hi:.3f}]   {m['hit@1']:.3f}  {m['hit@5']:.3f}  {m['hit@10']:.3f}")
            out["types"].setdefault(t_, {})[c] = {**m, "ci": [lo, hi]}
        for c in configs[2:]:
            dm, lo, hi = paired_diff_ci(ranks[t_][c], ranks[t_]["Titan exact scan"], 500)
            print(f"  {c} - exact: {dm:+.3f} [{lo:+.3f},{hi:+.3f}]")
            out["types"][t_][c]["diff_vs_exact"] = [dm, lo, hi]
    out["latency_ms"] = {c: {"p50": float(np.median(v)), "p95": float(np.percentile(v, 95))} for c, v in lat.items() if v}
    if "--json" in a:
        json.dump(out, open(a[a.index("--json") + 1], "w"), indent=1)


if __name__ == "__main__":
    main()
