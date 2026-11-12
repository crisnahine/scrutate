#!/usr/bin/env python3
"""Parity of the Rust engine with the Python reference engine, on a real repository.

usage: parity.py REPO_DIR RUST_BINARY [--queries N]

1. Both engines index a copy of REPO_DIR's files in the reference's six languages; chunk tables,
   call edges, postings (per chunk, field and token: tf), document lengths and vectors are compared.
2. Benchmark queries (all five types: the package's query file when one was generated, else made
   from the corpus's own documented functions by the same rules) run through both engines on the *Python-built* index; candidate sets, the 48 features, the
   ranker scores and the final top-10 are compared.
3. The Rust engine searches its own index; final top-10 compared with Python on the Python index.
Exits non-zero on any difference, or when fewer queries than asked can be made.
"""
from __future__ import annotations

import atexit
import json
import os
import shutil
import subprocess
import sys
import tempfile

import numpy as np

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
sys.path.insert(0, os.path.dirname(__file__))
from scrutate_ref.index import Index, unpack_text  # noqa: E402
from scrutate_ref.lex import NF  # noqa: E402
from scrutate_ref.search import FEATURES, Searcher  # noqa: E402

TOL_FEAT = 0.0       # features, compared as the float32 both engines compute
TOL_SCORE = 0.0      # ranker output
TOL_VEC = 0.0        # int8 codes times float32 scales: same quantization in both engines


def identity(row):
    return (row["path"], row["kind"], row["qualname"], row["start"], row["end"])


def chunk_table(ix):
    rows = ix.db.execute("SELECT id,path,kind,name,qualname,name_key,qual_key,start,end,is_test,ndoc,nbody,text,name_tok FROM chunks").fetchall()
    table = {}
    for r in rows:
        table[identity(r)] = tuple(unpack_text(r[k]) if k == "text" else r[k] for k in ("name", "name_key", "qual_key", "is_test", "ndoc", "nbody", "text", "name_tok"))
    refs = set()
    for r in ix.db.execute("SELECT c.path, c.qualname, c.start, r.callee FROM refs r JOIN chunks c ON c.id=r.chunk_id"):
        refs.add(tuple(r))
    return table, refs, {r["id"]: identity(r) for r in rows}


def postings(ix, by_id):
    """{(identity, field, key): tf}, {identity: dl}, {identity: vec}"""
    store = ix.lex
    post, dl, vec = {}, {}, {}
    for s in store.segs:
        ids = np.asarray(s.ids)
        keys = np.asarray(s.keys)
        for c in range(NF):
            ptr = np.asarray(s.ptr[c])
            doc = np.asarray(s.doc[c])
            tf = np.asarray(s.tf[c])
            for t in range(len(keys)):
                for e in range(ptr[t], ptr[t + 1]):
                    cid = int(ids[doc[e]])
                    if cid in by_id:
                        post[(by_id[cid], c, int(keys[t]))] = int(tf[e])
        v_rows = s.vec_in_row_order() if s.vec is not None else None
        for r, cid in enumerate(ids.tolist()):
            if cid in by_id:
                dl[by_id[cid]] = float(s.dl[r])
                if v_rows is not None:
                    vec[by_id[cid]] = v_rows[r]
    return post, dl, vec


def compare_indexes(a: Index, b: Index) -> list[str]:
    problems = []
    ta, ra, ida = chunk_table(a)
    tb, rb, idb = chunk_table(b)
    if set(ta) != set(tb):
        only_a = sorted(set(ta) - set(tb))[:5]
        only_b = sorted(set(tb) - set(ta))[:5]
        problems.append(f"chunk sets differ: {len(set(ta) - set(tb))} only in python, {len(set(tb) - set(ta))} only in rust; e.g. {only_a} / {only_b}")
    diff_cols = [(k, ta[k], tb[k]) for k in set(ta) & set(tb) if ta[k] != tb[k]]
    if diff_cols:
        k, x, y = diff_cols[0]
        cols = ("name", "name_key", "qual_key", "is_test", "ndoc", "nbody", "text", "name_tok")
        bad = [c for c, u, v in zip(cols, x, y) if u != v]
        problems.append(f"{len(diff_cols)} chunks have different columns, e.g. {k}: {bad} -> {[(x[cols.index(c)], y[cols.index(c)]) for c in bad][:2]}")
    if ra != rb:
        problems.append(f"call edges differ: {len(ra - rb)} only in python, {len(rb - ra)} only in rust; e.g. {sorted(ra - rb)[:3]} / {sorted(rb - ra)[:3]}")
    pa, dla, va = postings(a, ida)
    pb, dlb, vb = postings(b, idb)
    if pa != pb:
        only_a = [k for k in pa if k not in pb][:3]
        only_b = [k for k in pb if k not in pa][:3]
        diff = [(k, pa[k], pb[k]) for k in pa if k in pb and pa[k] != pb[k]][:3]
        problems.append(f"postings differ: {len(set(pa) - set(pb))} only python, {len(set(pb) - set(pa))} only rust, {len(diff)} tf diffs; e.g. {only_a} {only_b} {diff}")
    bad_dl = [(k, dla[k], dlb.get(k)) for k in dla if dlb.get(k) != dla[k]]
    if bad_dl:
        problems.append(f"document lengths differ for {len(bad_dl)} chunks, e.g. {bad_dl[:3]}")
    if va and vb:
        worst = max((float(np.abs(va[k] - vb[k]).max()), k) for k in va if k in vb)
        if worst[0] > TOL_VEC:
            problems.append(f"vectors differ: max abs diff {worst[0]:.4g} at {worst[1]}")
    return problems + compare_clusters(a, b, ida, idb)


def first_diff(x: np.ndarray, y: np.ndarray) -> str | None:
    """Where two arrays first differ (bit for bit), or None when they are identical."""
    x, y = np.asarray(x), np.asarray(y)
    if x.shape != y.shape:
        return f"shapes {x.shape} / {y.shape}"
    if not x.size:
        return None
    xb, yb = (np.ascontiguousarray(v).reshape(len(v), -1).view(np.uint8) for v in (x, y))
    bad = np.flatnonzero((xb != yb).any(axis=1))
    if not len(bad):
        return None
    r = bad[0]
    return f"{len(bad)} of {len(x)} rows, first at row {r}: {np.ravel(x[r]).tolist()[:4]} / {np.ravel(y[r]).tolist()[:4]}"


def compare_clusters(a: Index, b: Index, ida: dict, idb: dict) -> list[str]:
    """The clustered layout of every segment that has one: centroids, cluster pointers, cluster order and
    the stored vectors (which are in cluster order) must be identical between the engines."""
    problems = []
    if len(a.lex.segs) != len(b.lex.segs):
        return [f"segment counts differ: {len(a.lex.segs)} in python, {len(b.lex.segs)} in rust"]
    for i, (sa, sb) in enumerate(zip(a.lex.segs, b.lex.segs)):
        if sa.ivf != sb.ivf:
            problems.append(f"segment {i}: clustered in {'python' if sa.ivf else 'rust'} only")
            continue
        if not sa.ivf:
            continue
        rows_a = [ida.get(int(c), c) for c in np.asarray(sa.ids)]
        rows_b = [idb.get(int(c), c) for c in np.asarray(sb.ids)]
        if rows_a != rows_b:
            problems.append(f"segment {i}: rows hold different chunks, clusters not compared")
            continue
        for name, x, y in (("ivf_cent", sa.ivf_cent, sb.ivf_cent), ("ivf_ptr", sa.ivf_ptr, sb.ivf_ptr),
                           ("ivf_order", sa.ivf_order, sb.ivf_order), ("vec", sa.vec, sb.vec),
                           ("vscale", sa.vscale, sb.vscale)):
            d = first_diff(x, y)
            if d:
                problems.append(f"segment {i}: {name} differs: {d}")
                break  # the later arrays follow from the first difference
    return problems


def corpus_queries(repo: str) -> list[str]:
    """The benchmark's five query types, made from the repository's own documented functions."""
    import re

    import make_bench
    import make_bench_code
    from scrutate_ref.parse import language_for
    from scrutate_ref.text import STOPWORDS, is_test_path, split_identifier

    targets = []
    for dp, dns, fns in os.walk(repo):
        dns[:] = sorted(d for d in dns if d not in (".git", ".scrutate", "__pycache__", "node_modules"))
        for fn in sorted(fns):
            rel = os.path.relpath(os.path.join(dp, fn), repo)
            lang = language_for(rel)
            if lang is None or is_test_path(rel):
                continue
            try:
                with open(os.path.join(dp, fn), encoding="utf8") as f:
                    src = f.read()
                if lang == "python":
                    found = make_bench.process_file(src, rel, with_targets=True)[2]
                else:
                    lang = "typescript" if lang in ("typescript", "tsx") else lang
                    found = make_bench_code.process_file(src, rel, lang, with_targets=True)[2]
            except (UnicodeDecodeError, SyntaxError, ValueError):
                continue
            targets += found
    qs = []
    for t in targets:
        qs.append(t["query"])
        words = [w for w in re.findall(r"[A-Za-z][A-Za-z0-9_]{2,}", t["query"]) if w.lower() not in STOPWORDS]
        if len(words) >= 3:
            qs.append(" ".join(words[:3]))
        if len(split_identifier(t["name"])) >= 2:
            qs.append(" ".join(split_identifier(t["name"])))
        qs.append(t["name"])
        if "." in t["qualname"]:
            qs.append(re.sub(r"<[^.]*>", "", ".".join(t["qualname"].split(".")[-2:])))
    return list(dict.fromkeys(qs))


def load_queries(pkg_dir: str, n: int) -> list[str]:
    """The benchmark's query file for this package when one was generated, else queries made from
    the corpus by the same rules; exits when fewer than n are found."""
    import random
    from groups import BENCH

    base = os.path.basename(pkg_dir.rstrip("/"))
    p = os.path.join(BENCH, "queries", f"{base}.json")
    if os.path.exists(p):
        qs, source = list(dict.fromkeys(q["query"] for q in json.load(open(p)))), p
    else:
        qs, source = corpus_queries(pkg_dir), "the corpus doc comments"
    random.Random(7).shuffle(qs)
    if len(qs) < n:
        print(f"\nPARITY FAILED: only {len(qs)} queries from {source}, {n} asked")
        sys.exit(1)
    print(f"queries: {n} of {len(qs)} from {source}")
    return qs[:n]


def python_features(s: Searcher, queries):
    out = {}
    for q in queries:
        qi, rows, X = s.candidates(q)
        sc = s.score(X)
        hits = s._hits(qi, rows, X, sc, 10, with_text=False)
        out[q] = {
            "cands": {identity(r): (X[i].tolist(), float(sc[i])) for i, r in enumerate(rows)},
            "hits": [(h.path, h.qualname, h.start) for h in hits],
            "terms": qi.terms,
        }
    return out


def rust_features(binary: str, root: str, queries, features=True):
    with tempfile.NamedTemporaryFile("w", suffix=".jsonl", delete=False) as f:
        for q in queries:
            f.write(json.dumps({"query": q, "k": 10}) + "\n")
        qf = f.name
    cmd = [binary, "--root", root, "batch", qf] + (["--features"] if features else [])
    r = subprocess.run(cmd, capture_output=True, text=True)
    os.unlink(qf)
    if r.returncode != 0:
        raise SystemExit(f"rust batch failed: {r.stderr[-2000:]}")
    out = {}
    for line in r.stdout.splitlines():
        rec = json.loads(line)
        hits = [(h["path"], h["name"] if h["kind"] not in ("window",) else "", h["start"]) for h in rec["hits"]]
        out[rec["query"]] = {"hits": hits, "raw": rec}
    return out


def compare_search(py, rs, ix: Index) -> list[str]:
    problems = []
    by_id = {r["id"]: identity(r) for r in ix.db.execute("SELECT id,path,kind,qualname,start,end FROM chunks")}
    feat_worst = np.zeros(len(FEATURES))
    score_worst, cand_mismatch, top10_mismatch, top1_mismatch = 0.0, 0, 0, 0
    for q, p in py.items():
        r = rs[q]["raw"]
        rc = {by_id[c["id"]]: (c["x"], c["score"]) for c in r["candidates"] if c["id"] in by_id}
        if set(rc) != set(p["cands"]):
            cand_mismatch += 1
        for k in set(rc) & set(p["cands"]):
            xa, sa = p["cands"][k]
            xb, sb = rc[k]
            feat_worst = np.maximum(feat_worst, np.abs(np.array(xa, np.float32) - np.array(xb, np.float32)))
            if sa > -1e8 or sb > -1e8:
                score_worst = max(score_worst, abs(sa - sb))
        py_hits = [h[0:3:2] for h in p["hits"]]
        rs_hits = [(h["path"], h["start"]) for h in r["hits"]]
        if py_hits != rs_hits:
            top10_mismatch += 1
            if py_hits[:1] != rs_hits[:1]:
                top1_mismatch += 1
        if p["terms"] != r["terms"]:
            problems.append(f"query terms differ for {q!r}: {p['terms']} vs {r['terms']}")
    n = len(py)
    print(f"  queries: {n}; candidate sets identical: {n - cand_mismatch}/{n}; top-10 identical: {n - top10_mismatch}/{n}; top-1 identical: {n - top1_mismatch}/{n}")
    print(f"  max |feature diff| over all candidates: {feat_worst.max():.3g} (feature {FEATURES[int(feat_worst.argmax())]}); max |score diff|: {score_worst:.3g}")
    if cand_mismatch:
        problems.append(f"candidate sets differ for {cand_mismatch} queries")
    if feat_worst.max() > TOL_FEAT:
        problems.append(f"feature difference {feat_worst.max():.3g} > {TOL_FEAT} ({FEATURES[int(feat_worst.argmax())]})")
    if score_worst > TOL_SCORE:
        problems.append(f"score difference {score_worst:.3g} > {TOL_SCORE}")
    if top10_mismatch:
        problems.append(f"top-10 differs for {top10_mismatch} of {n} queries")
    return problems


def reference_files():
    """A copytree ignore that keeps the files of the reference's languages: the Rust engine parses
    others (JSON, Markdown, plain text) with grammars and heuristics the reference does not have."""
    from scrutate_ref.parse import language_for

    def ignore(d, entries):
        skip = {".scrutate", ".git"}
        out = {e for e in entries if e in skip or language_for(e) is None and os.path.isfile(os.path.join(d, e))}
        ignore.left_out |= {os.path.join(d, e) for e in out - skip}
        return out

    ignore.left_out = set()
    return ignore


def main():
    repo, binary = sys.argv[1], sys.argv[2]
    nq = int(sys.argv[sys.argv.index("--queries") + 1]) if "--queries" in sys.argv else 300
    work = tempfile.mkdtemp(prefix="parity_")
    atexit.register(shutil.rmtree, work, ignore_errors=True)  # also when a failed check exits early
    a_root, b_root = os.path.join(work, "py"), os.path.join(work, "rs")
    ignore = reference_files()
    shutil.copytree(repo, a_root, ignore=ignore)
    shutil.copytree(repo, b_root, ignore=ignore)
    if ignore.left_out:
        print(f"left out: {len(ignore.left_out)} files in other languages, e.g. {sorted(os.path.relpath(f, repo) for f in ignore.left_out)[:3]}")
    a = Index(a_root)
    st = a.update()
    print(f"python index: {st.chunks} chunks in {st.seconds:.1f}s")
    r = subprocess.run([binary, "--root", b_root, "index"], capture_output=True, text=True)
    print(f"rust index:   {r.stdout.strip()} {r.stderr.strip()[-300:]}")
    assert r.returncode == 0
    b = Index(b_root)
    problems = compare_indexes(a, b)
    if b.stats()["chunks"] != st.chunks:
        problems.insert(0, f"chunk counts differ: {st.chunks} in python, {b.stats()['chunks']} in rust")
    print("index parity:", "OK" if not problems else "PROBLEMS")
    for p in problems:
        print("   -", p)

    queries = load_queries(repo, nq)
    s = Searcher(a)
    py = python_features(s, queries)
    rs = rust_features(binary, a_root, queries, features=True)
    print("search parity on the python-built index:")
    p2 = compare_search(py, rs, a)
    for p in p2:
        print("   -", p)
    rs_own = rust_features(binary, b_root, queries, features=False)
    differ = [q for q in queries if [h[0:3:2] for h in py[q]["hits"]] != [(h[0], h[2]) for h in rs_own[q]["hits"]]]
    same = len(queries) - len(differ)
    for q in differ[:3]:
        print(f"   {q!r}: python {[h[0:3:2] for h in py[q]['hits']]}\n   rust   {[(h[0], h[2]) for h in rs_own[q]['hits']]}")
    print(f"end-to-end (rust index + rust search vs python): top-10 identical for {same}/{len(queries)} queries")
    problems += p2
    if differ:
        problems.append(f"end-to-end top-10 differs for {len(differ)} queries")
    if problems:
        print(f"\nPARITY FAILED: {len(problems)} problem(s)")
        sys.exit(1)
    print("\nPARITY OK")


if __name__ == "__main__":
    main()
