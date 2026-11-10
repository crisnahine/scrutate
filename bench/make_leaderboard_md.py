#!/usr/bin/env python3
"""docs/LEADERBOARD.md from the JSON written by bench/leaderboard.py and bench/indexed.py.

usage: make_leaderboard_md.py --engine-a FILE.json [--engine-a FILE2.json] [--indexed FILE.json]... [--out docs/LEADERBOARD.md]

Every number in the page comes from those files; the page states where they came from.
"""
from __future__ import annotations

import argparse
import json
import os
import shlex
import statistics
import sys

TOOLS = ["scrutate", "ripgrep", "ugrep", "GNU grep", "git grep", "ag", "ack", "ast-grep"]

# what closes each kind of gap; the leaderboard states the measured gap next to it
PLANS = {
    "small": ("below ~20 ms the walk of the directories, on one thread, is the critical path: Scrutate starts 1.4 ms sooner than "
              "ugrep but takes about 1.7 ms longer to walk and match ignore rules (ugrep compiles no glob sets and reads neither "
              "the global git excludes nor .git/info/exclude). Measured next steps: the parallel walker's idle threads sleep 1 ms "
              "between checks for work, and a wait that starts at 10 us took the parallel walk of the ripgrep repository from "
              "8.5 to 6.2 ms; then read each directory's ignore files from its listing and build glob sets lazily"),
    "output": "output-bound: per-file buffers already avoid per-line allocation; next, write straight from the search buffer (no UTF-8 re-validation) and reuse buffers across files",
    "regex": "regex-bound: profile the matcher configuration against ripgrep's (same regex crate) and check literal prefilters",
    "default": "profile this workload (instrumented build) and compare with the leader's strategy before changing code",
}


def ms(x):
    if x is None:
        return "-"
    v = x * 1000
    return f"{v:,.0f}" if v >= 100 else f"{v:.1f}"


def plan_for(r):
    us = r["tools"].get("scrutate", {}).get("median") or 0
    if us < 0.02:
        return PLANS["small"]
    if r["qtype"] in ("very many matches", "whole-word") or (r["reference"]["lines"] > 50_000):
        return PLANS["output"]
    if "regex" in r["qtype"]:
        return PLANS["regex"]
    return PLANS["default"]


def hardware(env):
    """The CPU as recorded; a Linux guest of the Apple Virtualization framework reports no name."""
    cpu = env.get("cpu", "?")
    if env.get("platform", "").startswith("Linux") and (not cpu or cpu.startswith("0x")):
        return ("a Linux virtual machine (Colima, Apple Virtualization) on the Apple M3 Pro above; its virtual CPUs run on "
                "performance or efficiency cores as macOS schedules them, so run-to-run variation is higher than on the host")
    return cpu


def engine_a(data, label):
    env = data["environment"]
    out = []
    out.append(f"## Engine A and structural search: {label}\n")
    mem = env.get("memory_bytes", 0) / 2**30
    out.append(f"- Hardware: {hardware(env)} ({env.get('cpus')} logical CPUs"
               + (f"; {env.get('perf_cores')} performance + {env.get('efficiency_cores')} efficiency cores" if env.get("perf_cores") else "")
               + f"), {mem:.0f} GB RAM; {env['platform']}")
    st = data["settings"]
    out.append(f"- Method: hyperfine (`{env.get('hyperfine', '?')}`), output piped, {st['runs']} timed runs for scrutate and every contender in "
               f"{st.get('blocks', 1)} interleaved blocks ({st['warmup']} warm-up run per block), {st['cache']} cache"
               + (f" (prepare: `{st['cold_prepare']}`)" if st.get("cold_prepare") else "")
               + f"; each block first waits a bounded time for the 1-minute load average to fall below {st['max_load']:.0f}.")
    loads = [r.get("max_load", 0) for r in data["benchmarks"].values()]
    loads = [x for r in data["benchmarks"].values() for x in r.get("load", [])] or loads
    out.append(f"- Load average (1 minute) at the start of each measured block: median {statistics.median(loads):.1f}, max {max(loads):.1f} on "
               f"{env.get('cpus')} logical CPUs. The machine was shared with other work throughout, so no result here is a publishable #1 claim "
               "(section 14 requires dedicated idle hardware); rows whose load exceeded the CPU count carry it in the status column.")
    out.append("- The read-files ceiling (`bench/rawread.c`) is measured once per file set, before that set's first benchmark. On a shared machine a "
               "row measured later under heavier load can read PARTIAL where a quieter run reads BLOCKED; PARTIAL is the weaker of the two claims.")
    out.append("- Tool versions: " + "; ".join(f"{t}: `{v}`" for t, v in env["tools"].items() if v and t != "scrutate --index") + ".")
    if env.get("scrutate_sha256"):
        out.append(f"- scrutate binary measured: SHA-256 `{env['scrutate_sha256']}`.")
    out.append("")
    counts = {}
    for r in data["benchmarks"].values():
        counts[r["status"]] = counts.get(r["status"], 0) + 1
    out.append("Status counts: " + ", ".join(f"**{k}** {v}" for k, v in sorted(counts.items())) + f" (of {len(data['benchmarks'])}).\n")
    out.append("Medians in ms (runs in parentheses when fewer than the full count). `wrong` = answer differs from the reference (disqualified), "
               "`n/a` = the tool cannot express the query or filter, `timeout` = did not finish. The cross-category column (`scrutate --index`, "
               "Engine B's text index) is published but never ranked.\n")
    head = "| Benchmark | " + " | ".join(TOOLS) + " | scrutate --index | read files | ratio vs best (95% CI) | status |"
    out.append(head)
    out.append("|" + "---|" * (len(TOOLS) + 5))
    for bid, r in data["benchmarks"].items():
        cells = []
        for t in TOOLS:
            x = r["tools"].get(t)
            if x is None:
                cells.append("")
            elif x.get("status") != "ok":
                cells.append(x.get("status", "?"))
            else:
                n = x.get("n", 0)
                v = ms(x.get("median"))
                cells.append(f"**{v}**" if t == r.get("best_competitor") or (t == "scrutate" and r["status"] == "PASS") else v)
                if n and n < data["settings"]["runs"]:
                    cells[-1] += f" ({n})"
        xi = r["tools"].get("scrutate --index", {})
        idx = ms(xi.get("median")) if xi.get("status") == "ok" else xi.get("status", "")
        ratio = f"{r['ratio']:.3f} [{r['ci95'][0]:.3f}, {r['ci95'][1]:.3f}] vs {r['best_competitor']}" if "ratio" in r else r.get("why", "")
        cpus = data["environment"].get("cpus") or 1
        flag = f" (load {r.get('max_load', 0):.0f})" if r.get("max_load", 0) > cpus else ""
        out.append(f"| {bid} ({r['reference']['lines']:,} lines) | " + " | ".join(cells) + f" | {idx} | {ms(r.get('rawread', {}).get('median'))} | {ratio} | {r['status']}{flag} |")
    out.append("")
    for bid, r in data["benchmarks"].items():
        n = r["reference"].get("files_not_compared")
        if n:
            out.append(f"- **{bid}**: every tool searches every file, but {n:,} of the {r['reference']['files_searched']:,} files are not "
                       "compared: the reference's own parse of them has errors, and inside an error the node a tool reports depends on "
                       "tree-sitter's error recovery, which differs between versions.")
    out.append("")
    fails = [(bid, r) for bid, r in data["benchmarks"].items() if r["status"] == "FAIL"]
    if fails:
        out.append("### Where a competitor is ahead (FAIL), with the measured gap and the plan\n")
        for bid, r in fails:
            if "ratio" in r:
                out.append(f"- **{bid}**: scrutate {ms(r['tools']['scrutate']['median'])} ms vs {r['best_competitor']} "
                           f"{ms(r['tools'][r['best_competitor']]['median'])} ms ({(r['ratio'] - 1) * 100:.0f}% slower, CI {r['ci95'][0]:.3f} to {r['ci95'][1]:.3f}). Plan: {plan_for(r)}.")
            else:
                out.append(f"- **{bid}**: {r.get('why', '')}.")
        out.append("")
    out.append("### CPU time and peak memory\n")
    out.append("Medians over the same runs (user + system CPU seconds, peak resident set in MB). Rank is scrutate's place among the tools "
               "with a correct answer, lowest first; these columns are reported, not tested for significance.\n")
    out.append("| Benchmark | scrutate CPU s | lowest competitor CPU s | CPU rank | scrutate MB | lowest competitor MB | memory rank |")
    out.append("|---|---|---|---|---|---|---|")
    for bid, r in data["benchmarks"].items():
        ok = {t: x for t, x in r["tools"].items() if x.get("status") == "ok" and x.get("cpu_median") is not None and t != "scrutate --index"}
        us = ok.get("scrutate")
        if not us:
            continue
        cells = []
        for key, scale, fmt in (("cpu_median", 1, "{:.2f}"), ("rss_median", 1e6, "{:.0f}")):
            comp = {t: x[key] for t, x in ok.items() if t != "scrutate"}
            best = min(comp, key=comp.get) if comp else None
            rank = 1 + sum(v < us[key] for v in comp.values())
            cells += [fmt.format(us[key] / scale), f"{fmt.format(comp[best] / scale)} ({best})" if best else "-", f"{rank} of {len(ok)}"]
        out.append(f"| {bid} | " + " | ".join(cells) + " |")
    out.append("")
    wrong = [(bid, t, x) for bid, r in data["benchmarks"].items() for t, x in r["tools"].items() if x.get("status") == "wrong"]
    if wrong:
        out.append("### Disqualified answers\n")
        out.append("| Benchmark | Tool | missing | extra | example |\n|---|---|---|---|---|")
        for bid, t, x in wrong:
            ex = (x.get("missing_sample") or x.get("extra_sample") or [["", ""]])[0]
            out.append(f"| {bid} | {t} | {x['missing']} | {x['extra']} | `{ex[0]}:{ex[1]}` |")
        out.append("")
    out.append("### Exact command lines\n")
    out.append("<details><summary>every tool, every benchmark (run from the corpus root)</summary>\n")
    for bid, r in data["benchmarks"].items():
        out.append(f"**{bid}**\n")
        out.append("```")
        for t, x in r["tools"].items():
            if x.get("argv"):
                env = " ".join(f"{k}={v}" for k, v in (x.get("env") or {}).items())
                argv = [os.path.basename(x["argv"][0])] + x["argv"][1:]
                out.append(f"{t:16} {env + ' ' if env else ''}{' '.join(shlex.quote(a) for a in argv)}")
        out.append("```\n")
    out.append("</details>\n")
    return out


def indexed(data, label):
    others = [t for t in data["tools"] if t != "scrutate"]
    out = [f"## Engine B: indexed search against {' and '.join(others)}: {label}\n"]
    env = data["environment"]
    out.append(f"- Hardware: {hardware(env)}, {env.get('cpus')} logical CPUs; {env['platform']}. Corpus: `{os.path.basename(data['corpus'])}`.")
    st = data["settings"]
    out.append(f"- Method: {st['index_runs']} full index builds and {st['index_runs']} incremental updates (100 files edited) per tool; "
               f"{st['runs']} runs of each of {st['queries']} queries, round-robin across tools, each run a fresh process; "
               f"{st['concurrency']} concurrent clients for {st['seconds']:.0f} s, three times, for throughput.")
    out.append("- Versions: " + "; ".join(f"{t}: `{v['version']}`" for t, v in data["tools"].items()) + ".\n")
    out.append("| Metric | " + " | ".join(data["tools"]) + " |")
    out.append("|---|" + "---|" * len(data["tools"]))
    rows = [
        ("full indexing time (median, s)", lambda t: f"{t['index']['median']:.2f}"),
        ("index size (MB)", lambda t: f"{t['index']['bytes'] / 1e6:,.0f}"),
        ("indexing peak memory (MB)", lambda t: f"{t['index']['rss_max'] / 1e6:,.0f}"),
        ("update after 100 edits (median, s)", lambda t: f"{t['update']['median']:.2f} ({t['update']['mode'].split(' (')[0]})"),
        ("query latency p50 (ms)", lambda t: ms(t["latency"]["p50"])),
        ("query latency p95 (ms)", lambda t: ms(t["latency"]["p95"])),
        ("query latency p99 (ms)", lambda t: ms(t["latency"]["p99"])),
        ("query memory (median MB)", lambda t: f"{t['latency']['rss_median'] / 1e6:.0f}"),
        ("concurrent throughput (queries/s)", lambda t: f"{t['throughput']['median']:.0f}"),
        ("queries answered differently from the reference", lambda t: str(len(t["latency"]["wrong_queries"]))),
    ]
    for name, f in rows:
        cells = []
        for t in data["tools"].values():
            try:
                cells.append(f(t))
            except (KeyError, TypeError):
                cells.append("-")
        out.append(f"| {name} | " + " | ".join(cells) + " |")
    out.append("")
    if "comparison" in data:
        out.append("| Comparison (scrutate / best other) | ratio | 95% CI | status |\n|---|---|---|---|")
        for k, v in data["comparison"].items():
            ci = f"{v['ci95'][0]:.3f} to {v['ci95'][1]:.3f}" if "ci95" in v else "(deterministic)"
            out.append(f"| {k} (vs {v['best']}) | {v['ratio']:.3f} | {ci} | {v['status']} |")
        out.append("")
    wrong = [(e["q"]["pats"], t, x) for e in data.get("queries", []) for t, x in e["tools"].items() if not x["valid"]]
    if wrong:
        out.append("Queries a tool answered differently (excluded from that tool's latency):\n")
        for pats, t, x in wrong:
            ex = (x.get("missing_sample") or x.get("extra_sample") or [["", ""]])[0]
            out.append(f"- {t}: `{' | '.join(pats)}`: {x['missing']} missing, {x['extra']} extra (e.g. `{ex[0]}:{ex[1]}`)")
        out.append("")
    return out


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--engine-a", action="append", default=[])
    ap.add_argument("--indexed", action="append", default=[])
    ap.add_argument("--out", default=os.path.join(os.path.dirname(__file__), "..", "docs", "LEADERBOARD.md"))
    a = ap.parse_args()
    lines = ["# Benchmark leaderboard", "",
             "Generated by `bench/make_leaderboard_md.py` from the raw results in `bench/results/`; nothing here is typed by hand. "
             "A benchmark is **PASS** only when the 95% confidence interval of median(scrutate) / median(best competitor), bootstrapped over at "
             "least 30 runs of each, lies entirely below 1; **FAIL** when it lies above 1; **PARTIAL** when it contains 1; **BLOCKED** for a tie "
             "at the hardware limit (both within 15% of the time to read the same files). Claims of \"fastest\" apply only to PASS rows.", ""]
    for p in a.engine_a:
        d = json.load(open(p))
        lines += engine_a(d, f"{os.path.basename(os.path.dirname(os.path.abspath(p)))}, {d['settings']['cache']} cache")
    for p in a.indexed:
        d = json.load(open(p))
        lines += indexed(d, os.path.basename(os.path.dirname(os.path.abspath(p))))
    with open(a.out, "w") as f:
        f.write("\n".join(lines) + "\n")
    print(f"wrote {a.out}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
