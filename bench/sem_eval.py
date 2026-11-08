#!/usr/bin/env python3
"""Fast layer-level experiment: lexical TF-IDF vs semantic model vs simple fusion.

usage: sem_eval.py MODEL.npz VARIANT(A|B) PKG [PKG ...] [--types doc,kw] [--gamma G] [--wa W]
"""
from __future__ import annotations

import os
import sys
import time

import numpy as np
from sklearn.feature_extraction.text import TfidfVectorizer

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import first_relevant_rank, fmt_row, load_queries  # noqa: E402
from groups import BENCH  # noqa: E402

from scrutate_ref.parse import parse_source  # noqa: E402
from scrutate_ref.semantic import SemanticModel  # noqa: E402
from scrutate_ref.text import query_terms, tokenize, tokenize_words  # noqa: E402


class H:  # minimal hit
    __slots__ = ("path", "qualname", "name")

    def __init__(self, p, q, n):
        self.path, self.qualname, self.name = p, q, n


def load_chunks(pkg: str, variant: str):
    root = os.path.join(BENCH, "src", variant, pkg)
    chunks = []
    for dp, dns, fns in os.walk(root):
        for fn in fns:
            if fn.endswith(".py"):
                rel = os.path.relpath(os.path.join(dp, fn), root)
                with open(os.path.join(dp, fn), encoding="utf8") as f:
                    for c in parse_source(rel, f.read()):
                        if c.kind != "window":
                            chunks.append((rel, c))
    return chunks


def rrf(score_mats, weights, k=60, top=200):
    nq, nc = score_mats[0].shape
    out = np.zeros((nq, nc), np.float32)
    for S, w in zip(score_mats, weights):
        order = np.argsort(-S, axis=1)[:, :top]
        for qi in range(nq):
            out[qi, order[qi]] += w / (k + np.arange(1, top + 1))
    return out


def main() -> None:
    args = [a for a in sys.argv[1:]]
    opts = {}
    for flag in ("--types", "--gamma", "--wa"):
        if flag in args:
            i = args.index(flag)
            opts[flag] = args[i + 1]
            del args[i : i + 2]
    model_path, variant, pkgs = args[0], args[1], args[2:]
    types = tuple(opts.get("--types", "doc,kw").split(","))
    model = SemanticModel.load(model_path)
    model.set_params(gamma=float(opts["--gamma"]) if "--gamma" in opts else None,
                     wa=float(opts["--wa"]) if "--wa" in opts else None)

    allr: dict[str, dict[str, list]] = {t: {"lexical": [], "semantic": [], "rrf": []} for t in types}
    for pkg in pkgs:
        t0 = time.time()
        chunks = load_chunks(pkg, variant)
        docs, M, hits = [], [], []
        for rel, c in chunks:
            qual = c.qualname.rsplit(".", 1)[0] if "." in c.qualname else ""
            nt, qt, st, dt, bt = (tokenize_words(c.name), tokenize_words(qual), tokenize(c.sig, 60),
                                  tokenize(c.doc, 120), tokenize(c.body, 1500))
            docs.append(nt * 3 + qt + st + dt + bt)
            z = model.encode_chunk(nt, qt, st, dt, bt)
            M.append(z if z is not None else np.zeros(model.k, np.float32))
            hits.append(H(rel, c.qualname, c.name))
        M = np.vstack(M)
        tf = TfidfVectorizer(analyzer=lambda x: x, sublinear_tf=True)
        X = tf.fit_transform(docs)
        for t in types:
            qs = load_queries(pkg, (t,))
            if not qs:
                continue
            qt_terms = [query_terms(q["query"]) for q in qs]
            Qz = np.vstack([(model.encode_query(ts) if ts else None) if ts else np.zeros(model.k, np.float32)
                            if True else None for ts in qt_terms]) if False else None
            zs = []
            for ts in qt_terms:
                z = model.encode_query(ts) if ts else None
                zs.append(z if z is not None else np.zeros(model.k, np.float32))
            Ssem = np.vstack(zs) @ M.T
            Slex = (tf.transform(qt_terms) @ X.T).toarray()
            Sfus = rrf([Slex, Ssem], [1.0, 1.0])
            for name, S in (("lexical", Slex), ("semantic", Ssem), ("rrf", Sfus)):
                order = np.argsort(-S, axis=1)[:, :10]
                for qi, q in enumerate(qs):
                    allr[t][name].append(first_relevant_rank(q, [hits[i] for i in order[qi]]))
        print(f"  {pkg}: {len(chunks)} chunks ({time.time()-t0:.0f}s)", flush=True)
    print(f"\nvariant {variant}  gamma={model.gamma} wa={model.wa}")
    for t in types:
        for name in ("lexical", "semantic", "rrf"):
            print(fmt_row(f"{t}/{name}", allr[t][name]))


if __name__ == "__main__":
    main()
