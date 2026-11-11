#!/usr/bin/env python3
"""Run benchmark queries through the engine; save candidate features + relevance labels.

usage: gen_features.py MODEL.npz TAG GROUP[DEV|TEST] [--variants A,B] [--pkgs a,b]
Writes $SCRUTATE_WORK/feat/TAG/{variant}_{pkg}.pkl (gzip-compressed)
"""
from __future__ import annotations

import gzip
import os
import pickle
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import is_relevant, load_queries  # noqa: E402
from groups import BENCH, DEV, TEST, WORK  # noqa: E402

from scrutate_ref.index import Index  # noqa: E402
from scrutate_ref.search import FEATURES, Searcher  # noqa: E402
from scrutate_ref.semantic import SemanticModel  # noqa: E402


def main() -> None:
    a = sys.argv[1:]
    opts = {"--variants": "A,B", "--pkgs": ""}
    for f in list(opts):
        if f in a:
            i = a.index(f)
            opts[f] = a[i + 1]
            del a[i : i + 2]
    model_path, tag, group = a[0], a[1], a[2]
    pkgs = opts["--pkgs"].split(",") if opts["--pkgs"] else (DEV if group == "DEV" else TEST)
    model = SemanticModel.load(model_path) if model_path != "none" else None
    outdir = os.path.join(WORK, "feat", tag)
    os.makedirs(outdir, exist_ok=True)
    for variant in opts["--variants"].split(","):
        for pkg in pkgs:
            t0 = time.time()
            root = os.path.join(BENCH, "src", variant, pkg)
            ix = Index(root, model=model)
            st = ix.update()
            s = Searcher(ix, ranker=None)
            recs = []
            for q in load_queries(pkg):
                _, rows, X = s.candidates(q["query"])
                y = np.array([1 if is_relevant(q, r["path"], r["qualname"] or "", r["name"] or "", r["kind"]) else 0
                              for r in rows], np.int8)
                names = [(r["path"], r["qualname"] or "", r["name"] or "") for r in rows]
                recs.append({"type": q["type"], "X": X, "y": y, "meta": names, "q": q})
            # gzip: the features are about four times smaller (train_ranker.py reads both forms)
            with gzip.open(os.path.join(outdir, f"{variant}_{pkg}.pkl"), "wb", compresslevel=1) as f:
                pickle.dump({"features": FEATURES, "recs": recs}, f)
            dt = time.time() - t0
            print(f"{variant} {pkg}: {len(recs)} queries, {st.chunks} chunks, {dt:.0f}s "
                  f"({dt/max(1,len(recs))*1000:.0f} ms/query incl. indexing)", flush=True)


if __name__ == "__main__":
    main()
