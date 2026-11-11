#!/usr/bin/env python3
"""Regression gate for the leaderboard: compare scrutate's runs in a new result file with a
baseline file from the same machine, benchmark by benchmark.

usage: leaderboard_regression.py BASELINE.json NEW.json [--tolerance 0.05] [--relative]

A benchmark regresses when the 95% bootstrap confidence interval of median(new) / median(base)
lies entirely above 1 + tolerance, so ordinary run-to-run noise does not fail the gate. A
benchmark whose answer became wrong always fails. Exit status 1 when anything regressed.

--relative is for a machine whose speed moves between the two runs (a shared CI runner): each
file's times are first divided by the median of the fastest competitor in that same file, which
the harness measures in the same interleaved blocks, so a slower hour slows both and cancels.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from leaderboard import boot_ratio  # noqa: E402


def machine(tools):
    """Median of the fastest competitor with full runs in this file, or None."""
    # "scrutate --index" moves with the change under test, so it cannot be the yardstick
    ms = [t["median"] for name, t in tools.items() if not name.startswith("scrutate") and t.get("status") == "ok" and len(t.get("times", [])) >= 10]
    return min(ms) if ms else None


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("baseline")
    ap.add_argument("new")
    ap.add_argument("--tolerance", type=float, default=0.05)
    ap.add_argument("--relative", action="store_true", help="compare times relative to the fastest competitor in each file")
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
        bt, nt = b["times"], n["times"]
        if a.relative:
            mb, mn = machine(base[bid]["tools"]), machine(new[bid]["tools"])
            if not mb or not mn:
                print(f"skip {bid}: no competitor with full runs in both files")
                continue
            bt, nt = [t / mb for t in bt], [t / mn for t in nt]
        r, lo, hi = boot_ratio(nt, bt)
        verdict = "REGRESSION" if lo > 1 + a.tolerance else "faster" if hi < 1 else "same"
        bad += verdict == "REGRESSION"
        print(f"{verdict:10} {bid}: {b['median'] * 1000:.1f} -> {n['median'] * 1000:.1f} ms (ratio {r:.3f}, CI {lo:.3f} to {hi:.3f})")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
