#!/usr/bin/env python3
"""The yardstick: each workload W1 to W10 against its best rival, every answer checked first.

    python3 bench/yardstick.py --corpus DIR --queries FILE --out FILE.json [--runs 20] [--only W3,W4]
    python3 bench/yardstick.py --smoke          # the tiny fixture: scrutate's rows must pass

A tool is an Adapter (bench/rivals): one request per `ask`. A query names its workload and what
the answer must hold (`expect`, or exactly that with `exact: true`). An answer that does not hold
it is reported as wrong and never timed; a tool that cannot run here says why. Passing tools are
timed in turn (A B C, B C A, ...), so load drift does not turn into a difference between them, and
each is compared with scrutate's adapter of the same form: resident with resident (a server that
holds its index), process with process (a program per request).
"""
from __future__ import annotations

import dataclasses
import math
import statistics
import time

import leaderboard as lb
from rivals import Unsupported


def check(workload: str, answer, query: dict) -> str | None:
    """None when `answer` holds what `query` expects, else what is wrong with it."""
    got = {tuple(x) if isinstance(x, (list, tuple)) else x for x in answer}
    want = {tuple(x) if isinstance(x, (list, tuple)) else x for x in query.get("expect", [])}
    if want - got:
        return f"missing {sorted(want - got, key=str)[:3]}"
    if query.get("exact") and got - want:
        return f"extra {sorted(got - want, key=str)[:3]}"
    return None


def measure(adapters: list, queries: list[dict], runs: int, warmup: int, corpus: str, work: str) -> dict:
    """Check, then time, every adapter on every query of a workload it serves."""
    tools: dict = {}
    started = []
    for a in adapters:
        why = a.missing()
        tools[a.name] = {"form": a.form, "missing": why}
        if why:
            continue
        tools[a.name]["version"] = a.version()
        try:
            tools[a.name]["setup"] = dataclasses.asdict(a.start(corpus, work))
            started.append(a)
        except Exception as e:  # noqa: BLE001 - a tool that cannot start is reported, the run goes on
            tools[a.name]["error"] = f"{type(e).__name__}: {e}"
    rows: list[dict] = []
    try:
        for q in queries:
            w = q["workload"]
            if "grep" in q and "expect" not in q:
                # a grep query is checked against leaderboard.py's Python reference, line for line
                found, _ = lb.reference(corpus, lb.file_list(corpus, q.get("mode", "ignore")), q["grep"])
                q = {**q, "expect": sorted(found), "exact": True}
            live = []
            for a in (a for a in adapters if w in a.workloads):
                row = {"workload": w, "query": q["id"], "tool": a.name, "form": a.form}
                rows.append(row)
                if tools[a.name]["missing"]:
                    row["status"] = f"missing: {tools[a.name]['missing']}"
                elif a not in started:
                    row["status"] = f"error: {tools[a.name]['error']}"
                else:
                    try:
                        bad = check(w, a.ask(w, q), q)
                        row["status"] = f"wrong: {bad}" if bad else "ok"
                    except Unsupported as e:
                        row["status"] = f"missing: {e}"
                    except Exception as e:  # noqa: BLE001
                        row["status"] = f"error: {e}"
                    if row["status"] == "ok":
                        live.append((a, row))
            for i in range(warmup + runs):
                k = i % len(live) if live else 0
                for a, row in live[k:] + live[:k]:
                    if row["status"] != "ok":
                        continue
                    t0 = time.perf_counter()
                    try:
                        a.ask(w, q)
                    except Exception as e:  # noqa: BLE001
                        row["status"] = f"error: {e}"
                        row.pop("times", None)
                        continue
                    if i >= warmup:
                        row.setdefault("times", []).append(time.perf_counter() - t0)
            for _, row in live:
                if row.get("times"):
                    row["p50_ms"] = 1000 * statistics.median(row["times"])
                    row["p95_ms"] = 1000 * sorted(row["times"])[max(0, math.ceil(0.95 * len(row["times"])) - 1)]
            ours = {row["form"]: row for _, row in live if row["tool"].startswith("scrutate") and row.get("times")}
            for _, row in live:
                base = ours.get(row["form"])
                if base and base is not row and row.get("times"):
                    row["ratio"], row["ci_low"], row["ci_high"] = lb.boot_ratio(row["times"], base["times"], B=2000)
    finally:
        for a in started:
            a.stop()
    return {"tools": tools, "rows": rows}


def summary(rows: list[dict]) -> dict:
    """Per workload: the rival whose time against scrutate's (geometric mean over the queries) is
    lowest among those that answered every query right, or why there is none."""
    out: dict = {}
    for w in sorted({r["workload"] for r in rows}):
        mine = [r for r in rows if r["workload"] == w and not r["tool"].startswith("scrutate")]
        good = {}
        for tool in {r["tool"] for r in mine}:
            rs = [r for r in mine if r["tool"] == tool]
            if all(r["status"] == "ok" and "ratio" in r for r in rs):
                good[tool] = (math.exp(statistics.mean(math.log(r["ratio"]) for r in rs)), rs[0]["form"])
        if good:
            tool, (ratio, form) = min(good.items(), key=lambda kv: kv[1][0])
            out[w] = {"rival": tool, "form": form, "ratio": ratio}
        else:
            seen = sorted({f"{r['tool']} {r['status']}" for r in mine})
            out[w] = {"reason": "no rival answered right: " + ("; ".join(seen) if seen else "none adapted")}
    return out


def adapters(binary: str | None = None) -> list:
    """Every adapter: scrutate's forms first, then the rivals (each says why when it cannot run)."""
    from rivals import lsp, scrutate
    b = binary or lb.SCRUTATE
    return [scrutate.Cli(b), scrutate.Mcp(b), scrutate.McpCold(b), lsp.Lsp("scrutate-lsp", [b, "lsp"], [b, "--version"], calls=False)]


def main() -> int:
    import argparse
    import json
    import os
    import shutil
    import sys
    import tempfile

    lb.drop_scrutate_settings()
    here = os.path.dirname(os.path.abspath(__file__))
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--smoke", action="store_true", help="the tiny fixture in bench/yardstick_smoke; exit 1 unless scrutate's rows pass")
    ap.add_argument("--corpus")
    ap.add_argument("--queries")
    ap.add_argument("--out")
    ap.add_argument("--runs", type=int, default=20)
    ap.add_argument("--warmup", type=int, default=2)
    ap.add_argument("--only", help="workloads, comma-separated (W3,W4)")
    a = ap.parse_args()
    if a.smoke:
        a.queries, a.runs, a.warmup = os.path.join(here, "yardstick_smoke", "queries.json"), 3, 1
    elif not (a.corpus and a.queries):
        ap.error("give --corpus and --queries, or --smoke")
    queries = json.load(open(a.queries))
    if a.only:
        queries = [q for q in queries if q["workload"] in a.only.split(",")]
    with tempfile.TemporaryDirectory() as work:
        corpus = a.corpus
        if a.smoke:
            # a copy: the tools write their indexes into the tree they search
            corpus = os.path.join(work, "tree")
            shutil.copytree(os.path.join(here, "yardstick_smoke", "tree"), corpus)
        out = measure(adapters(), queries, a.runs, a.warmup, os.path.realpath(corpus), work)
    out["environment"] = lb.environment()
    out["summary"] = summary(out["rows"])
    for r in out["rows"]:
        print(f"{r['workload']:4} {r['query'][:34]:34} {r['tool']:18} {r['form']:8} {r['status'][:60]:60} {r.get('p50_ms', float('nan')):9.2f} ms", file=sys.stderr)
    for w, s in out["summary"].items():
        print(f"{w}: " + (f"best rival {s['rival']} ({s['form']}), {s['ratio']:.2f}x scrutate's time" if "rival" in s else s["reason"]), file=sys.stderr)
    if a.out:
        with open(a.out, "w") as f:
            json.dump(out, f, indent=1)
    ours_ok = all(r["status"] == "ok" or r["status"].startswith("missing") for r in out["rows"] if r["tool"].startswith("scrutate"))
    return 0 if ours_ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
