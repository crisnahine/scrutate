#!/usr/bin/env python3
"""Sweep semantic-model parameters. usage: sem_sweep.py MODEL.npz VARIANT PKG... [--types doc]"""
import os, sys, time, itertools
import numpy as np
from sklearn.feature_extraction.text import TfidfVectorizer
sys.path.insert(0, os.path.dirname(__file__)); sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import first_relevant_rank, summarize, load_queries
from sem_eval import load_chunks, H
from scrutate_ref.semantic import SemanticModel
from scrutate_ref.text import query_terms, tokenize, tokenize_words

def prep(variant, pkgs, types):
    data = []
    for pkg in pkgs:
        chunks = load_chunks(pkg, variant)
        toks, docs, hits = [], [], []
        for rel, c in chunks:
            qual = c.qualname.rsplit(".", 1)[0] if "." in c.qualname else ""
            f = (tokenize_words(c.name), tokenize_words(qual), tokenize(c.sig, 60), tokenize(c.doc, 120), tokenize(c.body, 1500))
            toks.append(f); docs.append(f[0]*3 + f[1] + f[2] + f[3] + f[4]); hits.append(H(rel, c.qualname, c.name))
        tf = TfidfVectorizer(analyzer=lambda x: x, sublinear_tf=True); X = tf.fit_transform(docs)
        qs = load_queries(pkg, types)
        qt = [query_terms(q["query"]) for q in qs]
        Slex = (tf.transform(qt) @ X.T).toarray()
        data.append((qs, qt, toks, hits, Slex))
    return data

def zs(S):  # per-query standardisation
    return (S - S.mean(1, keepdims=True)) / (S.std(1, keepdims=True) + 1e-9)

def evaluate(model, data, lam_list=(0.0, 0.3, 0.6, 1.0)):
    out = {("sem", 0): []}; 
    for lam in lam_list: out[("fuse", lam)] = []
    for qs, qt, toks, hits, Slex in data:
        M = np.vstack([(model.encode_chunk(*f) if True else None) if False else (lambda z: z if z is not None else np.zeros(model.k, np.float32))(model.encode_chunk(*f)) for f in toks])
        Q = np.vstack([(lambda z: z if z is not None else np.zeros(model.k, np.float32))(model.encode_query(t) if t else None) for t in qt])
        Ssem = Q @ M.T
        mats = {("sem", 0): Ssem}
        for lam in lam_list: mats[("fuse", lam)] = zs(Slex) + lam * zs(Ssem)
        for key, S in mats.items():
            order = np.argsort(-S, axis=1)[:, :10]
            out[key] += [first_relevant_rank(q, [hits[i] for i in order[qi]]) for qi, q in enumerate(qs)]
    return {k: summarize(v)["MRR"] for k, v in out.items()}

if __name__ == "__main__":
    a = sys.argv[1:]; types = ("doc",)
    if "--types" in a: i = a.index("--types"); types = tuple(a[i+1].split(",")); del a[i:i+2]
    mp, variant, pkgs = a[0], a[1], a[2:]
    data = prep(variant, pkgs, types)
    model = SemanticModel.load(mp)
    base = evaluate(model, data, (0.0,))[("fuse", 0.0)]
    print(f"lexical-only MRR {base:.3f}   (model {os.path.basename(mp)}, k={model.k})")
    print(f"{'gamma':>5} {'wa':>4} | {'sem':>6} | fuse lam=0.3  0.6  1.0")
    for g, wa in itertools.product((0.0, 0.5, 1.0, 1.5), (0.0, 0.5, 1.0)):
        model.set_params(gamma=g, wa=wa)
        r = evaluate(model, data, (0.3, 0.6, 1.0))
        print(f"{g:5.1f} {wa:4.1f} | {r[('sem',0)]:.3f} | {r[('fuse',0.3)]:.3f}  {r[('fuse',0.6)]:.3f}  {r[('fuse',1.0)]:.3f}", flush=True)
