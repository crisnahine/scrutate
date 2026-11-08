#!/usr/bin/env python3
"""Recall of the clustered vector scan against the exact scan, on a large real index.

usage: ivf_recall.py ROOT [--json OUT]
For benchmark queries (description + keyword types) the exact top-100 semantic candidates (every
cluster probed) are compared with the top-100 from `nprobe` clusters; also reports the agreement of
the final top-10 ranking and the scan time. The benchmark packages themselves are far below
IVF_MIN_ROWS chunks, so this is the only place where the approximation is exercised.
"""
from __future__ import annotations

import json
import os
import random
import statistics
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from groups import BENCH  # noqa: E402

from scrutate_ref.index import Index  # noqa: E402
from scrutate_ref.search import SEM_LIMIT, Searcher  # noqa: E402

PROBES = (8, 16, 32, 48, 64, 96, 128)


def main() -> None:
    root = sys.argv[1]
    ix = Index(root)
    ix.update()
    s = Searcher(ix)
    store = ix.lex
    segs = [sg for sg in store.segs if sg.ivf]
    print(f"{ix.stats()['chunks']:,} chunks; ivf segments: {[(sg.n, len(sg.ivf_ptr) - 1) for sg in segs]}")
    qs = []
    for pkg in ("networkx", "mcp", "rich", "pydantic", "werkzeug", "seaborn", "pptx"):
        p = os.path.join(BENCH, "queries", f"{pkg}.json")
        if os.path.exists(p):
            qs += [q["query"] for q in json.load(open(p)) if q["type"] in ("doc", "kw")]
    qs = random.Random(11).sample(qs, 400)
    qzs = []
    for q in qs:
        qi = s._parse(q) if hasattr(s, "_parse") else None
        qzs.append(q)
    model = ix.model
    from scrutate_ref.text import query_terms
    vecs = [model.encode_query(query_terms(q)) for q in qs]
    pairs = [(q, z) for q, z in zip(qs, vecs) if z is not None]
    K = max(len(sg.ivf_ptr) - 1 for sg in segs)
    exact = [set(store.semantic_top(z, SEM_LIMIT, nprobe=K).tolist()) for _, z in pairs]
    exact10 = [store.semantic_top(z, 10, nprobe=K).tolist() for _, z in pairs]
    out = {"chunks": ix.stats()["chunks"], "clusters": K, "n_queries": len(pairs), "probe": {}}
    for npb in PROBES + (K,):
        r100, r10, times = [], [], []
        for (q, z), ex, ex10 in zip(pairs, exact, exact10):
            t = time.perf_counter()
            got = store.semantic_top(z, SEM_LIMIT, nprobe=npb).tolist()
            times.append((time.perf_counter() - t) * 1000)
            r100.append(len(ex & set(got)) / max(1, len(ex)))
            r10.append(len(set(ex10) & set(got[:10])) / max(1, len(ex10)))
        out["probe"][npb] = {"recall100": round(statistics.mean(r100), 4), "recall10": round(statistics.mean(r10), 4),
                             "scan_ms_p50": round(statistics.median(times), 2), "scan_ms_p95": round(sorted(times)[int(len(times) * .95)], 2)}
        label = "exact" if npb >= K else f"nprobe={npb}"
        print(f"  {label:<12} recall@100 {out['probe'][npb]['recall100']:.4f}  top10 recall {out['probe'][npb]['recall10']:.4f}  "
              f"scan p50 {out['probe'][npb]['scan_ms_p50']:.2f} ms  p95 {out['probe'][npb]['scan_ms_p95']:.2f} ms")
    # end-to-end: does the final top-10 change?
    same, lat_exact, lat_ivf = [], [], []
    for q, _ in pairs[:200]:
        store.nprobe = K
        t = time.perf_counter(); a = [h.id for h in s.search(q, k=10)]; lat_exact.append((time.perf_counter() - t) * 1000)
        store.nprobe = out.setdefault("default_nprobe", __import__("scrutate_ref.lex", fromlist=["IVF_NPROBE"]).IVF_NPROBE)
        t = time.perf_counter(); b = [h.id for h in s.search(q, k=10)]; lat_ivf.append((time.perf_counter() - t) * 1000)
        same.append(a == b)
    out["final_top10_identical"] = round(statistics.mean(same), 4)
    out["query_ms_exact_p50"] = round(statistics.median(lat_exact), 1)
    out["query_ms_ivf_p50"] = round(statistics.median(lat_ivf), 1)
    print(f"final top-10 identical to the exact scan for {out['final_top10_identical']:.1%} of {len(same)} queries; "
          f"query p50 exact {out['query_ms_exact_p50']} ms vs default {out['query_ms_ivf_p50']} ms")
    if "--json" in sys.argv:
        json.dump(out, open(sys.argv[sys.argv.index("--json") + 1], "w"), indent=1)


if __name__ == "__main__":
    main()
