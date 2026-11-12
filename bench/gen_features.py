#!/usr/bin/env python3
"""Run benchmark queries through the engine; save candidate features + relevance labels.

usage: gen_features.py MODEL.npz TAG GROUP[DEV|TEST|SEALED] [--variants A,B] [--pkgs a,b]
The binary at $SCRUTATE_BIN (default target/release/scrutate) indexes each package with the vector
model MODEL.npz (`none`: no vectors) and prints the features of every candidate by name
(`batch --features`), and the linear blend that picks the ranker's candidates. Writes
$SCRUTATE_WORK/feat/TAG/{variant}_{pkg}.pkl (gzip-compressed)
"""
from __future__ import annotations

import gzip
import json
import os
import pickle
import subprocess
import sys
import tempfile
import time

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
from evalcore import is_relevant, load_queries  # noqa: E402
from groups import BENCH, DEV, SEALED, TEST, WORK  # noqa: E402

REPO = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")


def engine_features(binary: str, root: str, queries: list[dict], model: str):
    """-> (feature names, [(candidates, X)] per query): the engine's candidates in its order."""
    # the ranker is not needed for features, and one fitted on other features would refuse to load
    env = dict(os.environ, SCRUTATE_SEMANTIC=model, SCRUTATE_RANKER="none")
    ix = subprocess.run([binary, "--root", root, "index", "--no-text"], capture_output=True, text=True, env=env)
    if ix.returncode != 0:
        raise SystemExit(f"{binary} index {root} failed: {ix.stderr[-2000:]}")
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
        f.write("".join(json.dumps({"query": q["query"], "k": 1}) + "\n" for q in queries))
    try:
        p = subprocess.Popen([binary, "--root", root, "batch", "--features", f.name], stdout=subprocess.PIPE, text=True, env=env)
        names, out = [], []
        for line in p.stdout:
            rec = json.loads(line)
            names = rec["feature_names"]
            cands = rec["candidates"]
            # a NaN is written as null
            X = np.array([[np.nan if v is None else v for v in c["x"]] for c in cands], np.float32).reshape(len(cands), len(names))
            # with no ranker the score is the engine's linear blend, which picks the ranker's candidates
            lin = np.array([np.nan if c["score"] is None else c["score"] for c in cands], np.float32)
            out.append((cands, X, lin))
        if p.wait() != 0:
            raise SystemExit(f"{binary} batch --features in {root} failed")
    finally:
        os.unlink(f.name)
    assert len(out) == len(queries), (len(out), len(queries))
    return names, out, ix.stdout.strip().split("\n")[0]


def main() -> None:
    a = sys.argv[1:]
    opts = {"--variants": "A,B", "--pkgs": ""}
    for f in list(opts):
        if f in a:
            i = a.index(f)
            opts[f] = a[i + 1]
            del a[i : i + 2]
    model_path, tag, group = a[0], a[1], a[2]
    pkgs = opts["--pkgs"].split(",") if opts["--pkgs"] else {"DEV": DEV, "TEST": TEST, "SEALED": SEALED}[group]
    model = os.path.abspath(model_path) if model_path != "none" else "none"
    binary = os.environ.get("SCRUTATE_BIN") or os.path.join(REPO, "target", "release", "scrutate")
    outdir = os.path.join(WORK, "feat", tag)
    os.makedirs(outdir, exist_ok=True)
    for variant in opts["--variants"].split(","):
        for pkg in pkgs:
            t0 = time.time()
            root = os.path.join(BENCH, "src", variant, pkg)
            queries = load_queries(pkg)
            names, rows, indexed = engine_features(binary, root, queries, model)
            recs = []
            for q, (cands, X, lin) in zip(queries, rows):
                y = np.array([1 if is_relevant(q, c["path"], c["qualname"], c["name"], c["kind"]) else 0 for c in cands], np.int8)
                meta = [(c["path"], c["qualname"], c["name"]) for c in cands]
                recs.append({"type": q["type"], "X": X, "lin": lin, "y": y, "meta": meta, "q": q})
            # gzip: the features are about four times smaller (train_ranker.py reads both forms)
            with gzip.open(os.path.join(outdir, f"{variant}_{pkg}.pkl"), "wb", compresslevel=1) as f:
                pickle.dump({"features": names, "recs": recs}, f)
            dt = time.time() - t0
            print(f"{variant} {pkg}: {len(recs)} queries, {indexed}, {dt:.0f}s "
                  f"({dt/max(1,len(recs))*1000:.0f} ms/query incl. indexing)", flush=True)


if __name__ == "__main__":
    main()
