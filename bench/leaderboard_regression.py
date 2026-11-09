#!/usr/bin/env python3
"""Regression gate for the leaderboard: compare scrutate's runs in a new result file with a
baseline file from the same machine, benchmark by benchmark.

usage: leaderboard_regression.py BASELINE.json NEW.json [--tolerance 0.05]

A benchmark regresses when the 95% bootstrap confidence interval of median(new) / median(base)
lies entirely above 1 + tolerance, so ordinary run-to-run noise does not fail the gate. A
benchmark whose answer became wrong always fails. Exit status 1 when anything regressed.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from leaderboard import boot_ratio  # noqa: E402


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("baseline")
    ap.add_argument("new")
    ap.add_argument("--tolerance", type=float, default=0.05)
    a = ap.parse_args()
    base = json.load(open(a.baseline))["benchmarks"]
    new = json.load(open(a.new))["benchmarks"]
    bad = 0
    for bid in sorted(set(base) & set(new)):
        b, n = base[bid]["tools"].get("scrutate", {}), new[bid]["tools"].get("scrutate", {})
        if n.get("status") != "ok":
            print(f"FAIL {bid}: answer is {n.get('status')}")
            bad += 1
            continue
        if b.get("status") != "ok" or len(b.get("times", [])) < 10 or len(n.get("times", [])) < 10:
            print(f"skip {bid}: not enough runs to compare")
            continue
        r, lo, hi = boot_ratio(n["times"], b["times"])
        verdict = "REGRESSION" if lo > 1 + a.tolerance else "faster" if hi < 1 else "same"
        bad += verdict == "REGRESSION"
        print(f"{verdict:10} {bid}: {b['median'] * 1000:.1f} -> {n['median'] * 1000:.1f} ms (ratio {r:.3f}, CI {lo:.3f} to {hi:.3f})")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
