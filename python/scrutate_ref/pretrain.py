"""Train the semantic model from local source trees.

    python -m scrutate_ref.pretrain --out models/semantic.npz --dirs DIR [DIR ...]

Only functions/methods outside test directories are used. Cache of extracted token lists can
be reused between runs with --cache.
"""
from __future__ import annotations

import argparse
import math
import os
import pickle
import time
from collections import Counter
from multiprocessing import Pool

import numpy as np
import scipy.sparse as sp

from .parse import parse_source
from .semantic import SemanticModel, pls_svd, view_counts
from .text import raw_subwords, tokenize_words

TEST_PARTS = {"test", "tests", "testing", "_tests", "conftest.py", "benchmarks", "asv_bench", "docs",
              "examples", "doc"}


def _is_test(path: str) -> bool:
    parts = path.lower().split(os.sep)
    return any(p in TEST_PARTS or p.startswith("test_") or p.endswith("_test.py") for p in parts)


def _extract(path_root: tuple[str, str]):
    path, root = path_root
    rel = os.path.relpath(path, root)
    try:
        with open(path, encoding="utf8") as f:
            src = f.read()
    except (UnicodeDecodeError, OSError):
        return []
    out = []
    try:
        chunks = parse_source(rel, src)
    except Exception:
        return []
    for c in chunks:
        if c.kind not in ("function", "method"):
            continue
        name_t = tokenize_words(c.name)
        if not name_t:
            continue
        qual = c.qualname.rsplit(".", 1)[0] if "." in c.qualname else ""
        a, b = view_counts(name_t, tokenize_words(qual), tokenize_words(c.sig, 60),
                           tokenize_words(c.doc, 120), tokenize_words(c.body, 1500))
        if len(b) >= 2:
            out.append((a, b))
    return out


def collect(dirs: list[str], workers: int = 2):
    files = []
    for root in dirs:
        for dp, dns, fns in os.walk(root):
            dns[:] = [d for d in dns if d != "__pycache__"]
            for f in fns:
                if f.endswith(".py"):
                    p = os.path.join(dp, f)
                    if not _is_test(os.path.relpath(p, os.path.dirname(root))):
                        files.append((p, os.path.dirname(root)))
    pairs = []
    with Pool(workers) as pool:
        for res in pool.imap_unordered(_extract, files, chunksize=16):
            pairs.extend(res)
    return pairs, len(files)


def build_matrices(pairs, min_df=3, max_vocab=30000, max_df_frac=0.4):
    n = len(pairs)
    vocabs, idfs, mats = [], [], []
    for view in (0, 1):
        df = Counter()
        for p in pairs:
            df.update(p[view].keys())
        keep = [(t, c) for t, c in df.items() if min_df <= c <= max_df_frac * n]
        keep.sort(key=lambda x: -x[1])
        keep = keep[:max_vocab]
        vocab = {t: i for i, (t, _) in enumerate(keep)}
        idf = np.array([math.log(1 + n / c) for _, c in keep], np.float32)
        rows, cols, vals = [], [], []
        for r, p in enumerate(pairs):
            ws = []
            for t, c in p[view].items():
                i = vocab.get(t)
                if i is not None:
                    rows.append(r)
                    cols.append(i)
                    ws.append((1.0 + math.log(c)) * idf[i])
            if ws:
                nrm = math.sqrt(sum(w * w for w in ws))
                vals.extend(w / nrm for w in ws)
            # rows without any vocab entry just stay empty
        X = sp.csr_matrix((np.array(vals, np.float32), (rows, cols)), shape=(n, len(vocab)))
        vocabs.append(vocab)
        idfs.append(idf)
        mats.append(X)
    return vocabs, idfs, mats


def train(pairs, k=192, min_df=3, max_vocab=30000, center=True, n_iter=3, log=print):
    t0 = time.time()
    (va, vb), (ia, ib), (XA, XB) = build_matrices(pairs, min_df, max_vocab)
    log(f"matrices: {XA.shape} x {XB.shape}, nnz A={XA.nnz:,} B={XB.nnz:,}  ({time.time()-t0:.0f}s)")
    t0 = time.time()
    U, S, V = pls_svd(XA, XB, k, n_iter=n_iter, center=center)
    log(f"pls-svd k={k}: top singular values {S[:4].round(1)} ... {S[-1]:.2f}  ({time.time()-t0:.0f}s)")
    cfg = {"k": k, "min_df": min_df, "max_vocab": max_vocab, "center": center, "n_pairs": len(pairs),
           "gamma": 0.5, "wa": 0.5}
    return SemanticModel(sorted(va, key=va.get), sorted(vb, key=vb.get), ia, ib, U, V, S, cfg)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--dirs", nargs="+", required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--k", type=int, default=192)
    ap.add_argument("--min-df", type=int, default=3)
    ap.add_argument("--max-vocab", type=int, default=30000)
    ap.add_argument("--no-center", action="store_true")
    ap.add_argument("--cache", help="pickle file for extracted token counts")
    ap.add_argument("--workers", type=int, default=2)
    a = ap.parse_args()
    if a.cache and os.path.exists(a.cache):
        with open(a.cache, "rb") as f:
            pairs = pickle.load(f)
        print(f"loaded {len(pairs):,} functions from cache")
    else:
        t0 = time.time()
        pairs, nf = collect(a.dirs, a.workers)
        print(f"extracted {len(pairs):,} functions from {nf} files ({time.time()-t0:.0f}s)")
        if a.cache:
            with open(a.cache, "wb") as f:
                pickle.dump(pairs, f)
    model = train(pairs, a.k, a.min_df, a.max_vocab, center=not a.no_center)
    os.makedirs(os.path.dirname(os.path.abspath(a.out)), exist_ok=True)
    model.save(a.out)
    print(f"saved {a.out} ({os.path.getsize(a.out)/1e6:.1f} MB)")
    for w in ("remov", "ping", "token", "enumer", "refresh", "valid", "verbos", "heartbeat"):
        nb = model.neighbors(w, 6)
        if nb:
            print(f"  {w:10s} ~ " + ", ".join(f"{t}" for t, _ in nb))


if __name__ == "__main__":
    main()
