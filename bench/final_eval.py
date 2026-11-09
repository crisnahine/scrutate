#!/usr/bin/env python3
"""End-to-end evaluation of the shipped artifact (default semantic model + ranker) vs baselines.

usage: final_eval.py [TEST|DEV|CODE] [--variants A,B] [--json OUT]
All systems rank the same chunks of the same masked corpus; hits are de-duplicated the same way.
"""
from __future__ import annotations

import json
import os
import sys
import time

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import bootstrap_ci, first_relevant_rank, load_queries, mrr_values, paired_diff_ci, summarize  # noqa: E402
from groups import BENCH, CODE_TEST, DEV, TEST  # noqa: E402


from scrutate_ref.index import Index  # noqa: E402
from scrutate_ref.search import FI, Searcher  # noqa: E402
from scrutate_ref.text import query_terms, tokenize  # noqa: E402

TYPES = ("doc", "kw", "name", "id", "qid")
CONFIGS = ["grep-style term count", "TF-IDF cosine", "BM25 (field-weighted)", "Titan"]


class H:
    __slots__ = ("path", "qualname", "name")

    def __init__(self, p, q, n):
        self.path, self.qualname, self.name = p, q, n


def baselines(ix: Index):
    from sklearn.feature_extraction.text import TfidfVectorizer

    rows = ix.db.execute("SELECT path, qualname, name, text FROM chunks WHERE kind!='window'").fetchall()
    hits = [H(r[0], r[1] or "", r[2] or "") for r in rows]
    toks = [tokenize(r[3], whole=False) for r in rows]
    sets = [set(t) for t in toks]
    lens = np.array([len(t) for t in toks])
    tf = TfidfVectorizer(analyzer=lambda x: x, sublinear_tf=True)
    X = tf.fit_transform(toks)

    def grep_rank(query):
        terms = query_terms(query)
        if not terms:
            return []
        cnt = np.array([sum(t in s for t in terms) for s in sets])
        order = np.lexsort((lens, -cnt))[:10]  # most terms first, then shortest chunk
        return [hits[i] for i in order if cnt[i] > 0]

    def tfidf_rank(query):
        terms = query_terms(query)
        if not terms:
            return []
        sc = (tf.transform([terms]) @ X.T).toarray()[0]
        return [hits[i] for i in np.argsort(-sc)[:10] if sc[i] > 0]

    return grep_rank, tfidf_rank


def run(group: str, variants: list[str]):
    pkgs = {"DEV": DEV, "TEST": TEST, "CODE": CODE_TEST}[group]
    out: dict = {}
    for variant in variants:
        ranks = {t: {c: [] for c in CONFIGS} for t in TYPES}
        lat = []
        for pkg in pkgs:
            root = os.path.join(BENCH, "src", variant, pkg)
            ix = Index(root)
            ix.update()
            s = Searcher(ix)
            grep_rank, tfidf_rank = baselines(ix)
            for q in load_queries(pkg):
                t0 = time.perf_counter()
                qi, rows, X = s.candidates(q["query"])
                full = s._hits(qi, rows, X, s.score(X), 10, with_text=False)
                lat.append((time.perf_counter() - t0) * 1000)
                bm25 = s._hits(qi, rows, X, X[:, FI["lex_log"]] if len(rows) else np.zeros(0), 10, with_text=False)
                r = ranks[q["type"]]
                r["Titan"].append(first_relevant_rank(q, full))
                r["BM25 (field-weighted)"].append(first_relevant_rank(q, bm25))
                r["TF-IDF cosine"].append(first_relevant_rank(q, tfidf_rank(q["query"])))
                r["grep-style term count"].append(first_relevant_rank(q, grep_rank(q["query"])))
            print(f"  {variant} {pkg} done", flush=True)
        out[variant] = {"ranks": ranks, "latency_ms": lat}
    return out


def report(res, title: str) -> dict:
    summary = {}
    for variant, d in res.items():
        print(f"\n### {title} -- variant {variant}  "
              f"({'docstrings removed' if variant == 'A' else 'first docstring sentence removed'})")
        print(f"search latency incl. feature extraction + ranking: p50 {np.median(d['latency_ms']):.1f} ms, "
              f"p95 {np.percentile(d['latency_ms'], 95):.1f} ms")
        for t in TYPES:
            print(f"\n{t}  (n={len(d['ranks'][t]['Titan'])})")
            print(f"  {'system':<24} {'MRR [95% CI]':<22} {'hit@1':>6} {'hit@5':>6} {'hit@10':>7}")
            for c in CONFIGS:
                rk = d["ranks"][t][c]
                m = summarize(rk)
                lo, hi = bootstrap_ci(mrr_values(rk), 500)
                print(f"  {c:<24} {m['MRR']:.3f} [{lo:.3f},{hi:.3f}]   {m['hit@1']:.3f}  {m['hit@5']:.3f}  {m['hit@10']:.3f}")
                summary.setdefault(variant, {}).setdefault(t, {})[c] = {**m, "ci": [lo, hi]}
            dm, lo, hi = paired_diff_ci(d["ranks"][t]["Titan"], d["ranks"][t]["BM25 (field-weighted)"], 500)
            print(f"  Titan - BM25: {dm:+.3f} [{lo:+.3f},{hi:+.3f}]")
    return summary


if __name__ == "__main__":
    a = sys.argv[1:]
    group = a[0] if a and a[0] in ("TEST", "DEV", "CODE") else "TEST"
    variants = ["A", "B"]
    if "--variants" in a:
        variants = a[a.index("--variants") + 1].split(",")
    res = run(group, variants)
    summ = report(res, f"{group} packages")
    if "--json" in a:
        with open(a[a.index("--json") + 1], "w") as f:
            json.dump({"summary": summ, "latency_ms": {v: res[v]["latency_ms"] for v in res}}, f)
