#!/usr/bin/env python3
"""Benchmark regression gate: compare a results file from compare_tools.py with a baseline.

usage: regression.py BASELINE.json CURRENT.json [--tolerance 0.15] [--relative]
Fails (exit 1) when scrutate's median wall time for any (corpus, query) is slower than the baseline
by more than the tolerance, when a match count changed, or when the index grew by more than the
tolerance. Only scrutate's own rows are gated; competitor rows are informational.

When both files hold the single run times (`wall_ms`), a row regresses only if the 95% bootstrap
confidence interval of median(current) / median(baseline) lies entirely above 1 + tolerance, so
one noisy median does not fail the gate. An older file has only medians, and those are compared.

--relative is for a machine whose speed moves between the two runs (a shared CI runner): each
median is first divided by the fastest competitor's median for the same query in the same file.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from leaderboard import boot_ratio  # noqa: E402


def rows(d, tool="scrutate"):
    return {(r["corpus"], r["query"]): r for r in d.get("text_search", []) if r.get("tool") == tool and r.get("status") == "ok"}


def machine(d):
    """(corpus, query) -> median of the fastest competitor measured in this file."""
    out = {}
    for r in d.get("text_search", []):
        if r.get("tool") != "scrutate" and r.get("status") == "ok":
            k = (r["corpus"], r["query"])
            out[k] = min(out.get(k, r["wall_median_ms"]), r["wall_median_ms"])
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("baseline")
    ap.add_argument("current")
    ap.add_argument("--tolerance", type=float, default=0.15)
    ap.add_argument("--relative", action="store_true", help="compare medians relative to the fastest competitor in each file")
    a = ap.parse_args()
    base, cur = json.load(open(a.baseline)), json.load(open(a.current))
    b, c = rows(base), rows(cur)
    mb, mc = machine(base), machine(cur)
    problems = []
    for key, br in b.items():
        cr = c.get(key)
        if cr is None:
            problems.append(f"{key}: missing in current results")
            continue
        if cr["matches"] != br["matches"]:
            problems.append(f"{key}: match count {br['matches']} -> {cr['matches']}")
        scale = mb[key] / mc[key] if a.relative and mb.get(key) and mc.get(key) else 1.0
        if br.get("wall_ms") and cr.get("wall_ms"):
            ratio, low, high = boot_ratio([t * scale for t in cr["wall_ms"]], br["wall_ms"])
            shown = f"x{ratio:.2f} (CI {low:.2f} to {high:.2f})"
        else:
            ratio = low = scale * cr["wall_median_ms"] / max(br["wall_median_ms"], 1e-9)
            shown = f"x{ratio:.2f}"
        flag = "  <-- REGRESSION" if low > 1 + a.tolerance else ""
        print(f"{key[0]:10} {key[1]:26} {br['wall_median_ms']:9.1f} -> {cr['wall_median_ms']:9.1f} ms  {shown}{flag}")
        if flag:
            problems.append(f"{key}: {ratio:.2f}x slower")
    for bi in base.get("index", []):
        ci = next((x for x in cur.get("index", []) if x["corpus"] == bi["corpus"]), None)
        if ci and ci["index_mb"] > bi["index_mb"] * (1 + a.tolerance):
            problems.append(f"index {bi['corpus']}: {bi['index_mb']:.0f} MB -> {ci['index_mb']:.0f} MB")
    for p in problems:
        print("PROBLEM:", p)
    print("OK" if not problems else f"{len(problems)} problems")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
