#!/usr/bin/env python3
"""Speed benchmark on a large real codebase (several big Python packages copied together).

usage: speed.py [--rebuild-corpus] [--json OUT]
"""
from __future__ import annotations

import json
import os
import random
import shutil
import statistics
import subprocess
import sys
import time

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from groups import SP, WORK  # noqa: E402

BIG = os.path.join(WORK, "big")
PKGS = ["pandas", "scipy", "sklearn", "numpy", "matplotlib", "spacy", "mypy", "fontTools", "skimage",
        "networkx", "pydantic", "sympy", "pygments", "reportlab"]


def build_corpus() -> None:
    shutil.rmtree(BIG, ignore_errors=True)
    for p in PKGS:
        src = os.path.join(SP, p)
        if not os.path.isdir(src):
            continue
        for dp, dns, fns in os.walk(src):
            dns[:] = [d for d in dns if d != "__pycache__"]
            for fn in fns:
                if fn.endswith(".py"):
                    dst = os.path.join(BIG, p, os.path.relpath(os.path.join(dp, fn), src))
                    os.makedirs(os.path.dirname(dst), exist_ok=True)
                    shutil.copy2(os.path.join(dp, fn), dst)


def loc() -> tuple[int, int, float]:
    n = lines = 0
    size = 0
    for dp, _, fns in os.walk(BIG):
        for fn in fns:
            if fn.endswith(".py"):
                p = os.path.join(dp, fn)
                n += 1
                size += os.path.getsize(p)
                with open(p, "rb") as f:
                    lines += f.read().count(b"\n")
    return n, lines, size / 1e6


def sh(cmd, cwd=BIG):
    t = time.perf_counter()
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, text=True)
    return time.perf_counter() - t, r


def pct(xs, p):
    xs = sorted(xs)
    return xs[min(len(xs) - 1, int(len(xs) * p))]


def measure(root: str, label: str, lines: int, files: int, mb: float, mid_edit: str) -> dict:
    from scrutate_ref.index import Index
    from scrutate_ref.search import Searcher

    out: dict = {"label": label, "files": files, "lines": lines, "mb": round(mb)}
    shutil.rmtree(os.path.join(root, ".scrutate"), ignore_errors=True)
    t = time.perf_counter()
    ix = Index(root)
    st = ix.update()
    full = time.perf_counter() - t
    dirmb = sum(os.path.getsize(os.path.join(dp, f)) for dp, _, fs in os.walk(ix.dir) for f in fs) / 1e6
    out.update(index_s=round(full, 1), chunks=st.chunks, index_mb=round(dirmb), kloc_per_s=round(lines / full / 1e3))
    t = time.perf_counter(); ix.update(); out["noop_update_ms"] = round((time.perf_counter() - t) * 1000)
    target = os.path.join(root, mid_edit)
    with open(target, "a") as f:
        f.write("\n\ndef _speed_probe():\n    return 1\n")
    t = time.perf_counter(); ix.update(); out["one_file_update_ms"] = round((time.perf_counter() - t) * 1000)
    t = time.perf_counter(); Searcher(ix).search("speed probe", k=3); out["first_search_after_edit_ms"] = round((time.perf_counter() - t) * 1000)

    qs = []
    for pkg in ("pydantic", "pptx", "werkzeug", "seaborn", "networkx", "mcp", "rich"):
        p = os.path.join(WORK, "bench", "queries", f"{pkg}.json")
        if os.path.exists(p):
            qs += json.load(open(p))
    rnd = random.Random(1)
    by_type = {t: [q["query"] for q in qs if q["type"] == t] for t in ("doc", "kw", "name", "id", "qid")}
    s = Searcher(ix)
    s.search("warm up the caches", k=5)
    lat_by_type = {}
    for t_, qlist in by_type.items():
        lat = []
        for q in rnd.sample(qlist, min(200, len(qlist))):
            t = time.perf_counter(); s.search(q, k=10); lat.append((time.perf_counter() - t) * 1000)
        lat_by_type[t_] = {"p50": round(statistics.median(lat), 1), "p95": round(pct(lat, .95), 1), "n": len(lat)}
    out["latency_ms"] = lat_by_type

    ids = rnd.sample(by_type["id"], 40)
    rg, ti = [], []
    for q in ids:
        dt, _ = sh(["rg", "-n", "-w", q, "."], cwd=root); rg.append(dt * 1000)
        t = time.perf_counter(); s.search(q, k=10); ti.append((time.perf_counter() - t) * 1000)
    out["identifier_lookup_ms"] = {"ripgrep_p50": round(statistics.median(rg)), "scrutate_p50": round(statistics.median(ti)),
                                   "ripgrep_p95": round(pct(rg, .95)), "scrutate_p95": round(pct(ti, .95))}

    env = dict(os.environ, PYTHONPATH=os.path.join(os.path.dirname(__file__), "..", "python"))
    cold = []
    for q in ["refresh an expired token", "calculate shortest path between nodes", "parse a csv file header"]:
        t = time.perf_counter()
        r = subprocess.run([sys.executable, "-m", "scrutate_ref", "--root", root, "search", q, "-k", "5"],
                           capture_output=True, text=True, env=env)
        cold.append((time.perf_counter() - t) * 1000)
        assert r.returncode == 0, r.stderr[-500:]
    out["cold_cli_search_ms"] = round(statistics.median(cold))
    out.update(serving_footprint(root, [q for t_ in ("doc", "kw", "name") for q in by_type[t_]][:150]))
    return out


SERVING_PROBE = r"""
import json, sys, time
from scrutate_ref.index import Index
from scrutate_ref.search import Searcher
root, queries = sys.argv[1], json.load(open(sys.argv[2]))
ix = Index(root); s = Searcher(ix)
t = time.perf_counter(); s.search(queries[0], k=10); first = (time.perf_counter() - t) * 1000
for q in queries[1:]:
    s.search(q, k=10)
rss = anon = 0
for line in open("/proc/self/smaps_rollup"):
    k, _, v = line.partition(":")
    if k == "Rss": rss = int(v.split()[0]) // 1024
    if k == "Anonymous": anon = int(v.split()[0]) // 1024
print(json.dumps({"first_query_ms": round(first), "serving_rss_mb": rss, "serving_anon_mb": anon}))
"""


def serving_footprint(root: str, queries: list[str]) -> dict:
    """Memory and first-query latency of a fresh process that only searches (what an MCP server holds)."""
    qf = os.path.join(WORK, "speed_probe_queries.json")
    with open(qf, "w") as f:
        json.dump(queries, f)
    env = dict(os.environ, PYTHONPATH=os.path.join(os.path.dirname(__file__), "..", "python"))
    r = subprocess.run([sys.executable, "-c", SERVING_PROBE, root, qf], capture_output=True, text=True, env=env)
    assert r.returncode == 0, r.stderr[-500:]
    return json.loads(r.stdout.strip().splitlines()[-1])


def show(r: dict) -> None:
    print(f"\n=== {r['label']}: {r['files']:,} files, {r['lines']/1e6:.2f}M lines, {r['mb']} MB of source")
    print(f"full index {r['index_s']}s ({r['kloc_per_s']}k lines/s), {r['chunks']:,} chunks, index on disk {r['index_mb']} MB")
    print(f"no-change update {r['noop_update_ms']} ms | one-file update {r['one_file_update_ms']} ms | "
          f"first search after an edit {r['first_search_after_edit_ms']} ms")
    for t, d in r["latency_ms"].items():
        print(f"  warm query {t:<5} p50 {d['p50']:6.1f} ms  p95 {d['p95']:6.1f} ms  (n={d['n']})")
    i = r["identifier_lookup_ms"]
    print(f"exact identifier: ripgrep p50 {i['ripgrep_p50']} ms p95 {i['ripgrep_p95']} ms | "
          f"scrutate p50 {i['scrutate_p50']} ms p95 {i['scrutate_p95']} ms")
    print(f"cold `scrutate-ref search` from the shell (python start + imports + model + index open): {r['cold_cli_search_ms']} ms")
    print(f"search-only process after {150} queries: {r['serving_rss_mb']} MB resident "
          f"({r['serving_anon_mb']} MB private, the rest is memory-mapped index files); first query {r['first_query_ms']} ms")


def main() -> None:
    results = []
    if "--rebuild-corpus" in sys.argv or not os.path.isdir(BIG):
        print("building corpus ...", flush=True)
        build_corpus()
    files, lines, mb = loc()
    results.append(measure(BIG, "monorepo-scale (14 big Python packages)", lines, files, mb,
                           "networkx/algorithms/shortest_paths/generic.py"))
    show(results[-1])
    # a mid-size repository: one package copied alone (fresh copy so the probe edit does not touch site-packages)
    mid = os.path.join(WORK, "mid")
    shutil.rmtree(mid, ignore_errors=True)
    shutil.copytree(os.path.join(SP, "networkx"), mid, ignore=shutil.ignore_patterns("__pycache__", "tests"))
    n = sum(len(fs) for _, _, fs in os.walk(mid))
    nl = sum(open(os.path.join(dp, f), "rb").read().count(b"\n") for dp, _, fs in os.walk(mid) for f in fs if f.endswith(".py"))
    nmb = sum(os.path.getsize(os.path.join(dp, f)) for dp, _, fs in os.walk(mid) for f in fs) / 1e6
    results.append(measure(mid, "mid-size repository (networkx)", nl, n, nmb, "algorithms/shortest_paths/generic.py"))
    show(results[-1])
    if "--json" in sys.argv:
        with open(sys.argv[sys.argv.index("--json") + 1], "w") as f:
            json.dump(results, f, indent=1)


if __name__ == "__main__":
    main()
