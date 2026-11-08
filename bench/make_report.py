#!/usr/bin/env python3
"""Turn the raw evaluation logs into the markdown tables of the README (numbers are never typed by hand).

usage: make_report.py final_eval_test.log final_eval_js.log speed.json [haystack.json ivf_recall.json] > RESULTS.md
"""
from __future__ import annotations

import json
import re
import sys

SYSTEMS = ["grep-style term count", "TF-IDF cosine", "BM25 (field-weighted)", "Titan"]
TYPE_LABEL = {
    "doc": "natural-language description (first docstring sentence)",
    "kw": "3 keywords from that description",
    "name": "function name written as words (\"remove tool\")",
    "id": "bare identifier (`remove_tool`)",
    "qid": "qualified name (`Server.remove_tool`)",
}


def parse(path: str) -> dict:
    """-> {variant: {type: {"n": int, system: (mrr, lo, hi, h1, h5, h10), "diff": (d, lo, hi)}}}"""
    out: dict = {}
    variant = typ = None
    for line in open(path):
        m = re.match(r"### .* variant (\w)", line)
        if m:
            variant = m.group(1)
            out[variant] = {}
            continue
        m = re.match(r"(\w+)\s+\(n=(\d+)\)", line)
        if m and variant:
            typ = m.group(1)
            out[variant][typ] = {"n": int(m.group(2))}
            continue
        m = re.match(r"\s+(.+?)\s+([\d.]+) \[([\d.]+),([\d.]+)\]\s+([\d.]+)\s+([\d.]+)\s+([\d.]+)\s*$", line)
        if m and variant and typ and m.group(1) in SYSTEMS:
            out[variant][typ][m.group(1)] = tuple(float(x) for x in m.groups()[1:])
            continue
        m = re.match(r"\s+Titan - BM25: ([+-][\d.]+) \[([+-][\d.]+),([+-][\d.]+)\]", line)
        if m and variant and typ:
            out[variant][typ]["diff"] = tuple(float(x) for x in m.groups())
    return out


def mrr_table(res: dict, variant: str) -> str:
    rows = ["| query type | n | grep-style | TF-IDF | BM25 | **Titan** [95% CI] | Titan − BM25 [95% CI] |",
            "|---|---:|---:|---:|---:|---:|---:|"]
    for t, label in TYPE_LABEL.items():
        d = res[variant][t]
        g, tf, bm, ti = (d[s] for s in SYSTEMS)
        df = d["diff"]
        rows.append(f"| {label} | {d['n']:,} | {g[0]:.3f} | {tf[0]:.3f} | {bm[0]:.3f} | "
                    f"**{ti[0]:.3f}** [{ti[1]:.3f}, {ti[2]:.3f}] | {df[0]:+.3f} [{df[1]:+.3f}, {df[2]:+.3f}] |")
    return "\n".join(rows)


def hit_table(res: dict, variant: str) -> str:
    rows = ["| query type | BM25 hit@1 | Titan hit@1 | BM25 hit@5 | Titan hit@5 | BM25 hit@10 | Titan hit@10 |",
            "|---|---:|---:|---:|---:|---:|---:|"]
    for t, label in TYPE_LABEL.items():
        bm, ti = res[variant][t]["BM25 (field-weighted)"], res[variant][t]["Titan"]
        rows.append(f"| {t} | {bm[3]:.3f} | {ti[3]:.3f} | {bm[4]:.3f} | {ti[4]:.3f} | {bm[5]:.3f} | {ti[5]:.3f} |")
    return "\n".join(rows)


def speed_tables(path: str) -> str:
    data = json.load(open(path))
    head = ["| | " + " | ".join(d["label"] for d in data) + " |", "|---|" + "---:|" * len(data)]
    rows = [
        ("source size", lambda d: f"{d['files']:,} files, {d['lines']/1e6:.2f}M lines, {d['mb']} MB"),
        ("chunks (functions, classes, modules)", lambda d: f"{d['chunks']:,}"),
        ("first full index", lambda d: f"{d['index_s']} s ({d['kloc_per_s']}k lines/s, 2 cores)"),
        ("index size on disk", lambda d: f"{d['index_mb']} MB"),
        ("update, nothing changed", lambda d: f"{d['noop_update_ms']} ms"),
        ("update, one file edited", lambda d: f"{d['one_file_update_ms']} ms"),
        ("cold `scrutate-ref search` from the shell", lambda d: f"{d['cold_cli_search_ms']} ms"),
        ("first query in a fresh process", lambda d: f"{d['first_query_ms']} ms"),
        ("memory of a search-only process (resident)", lambda d: f"{d['serving_rss_mb']} MB"),
    ]
    for t in ("doc", "kw", "name", "id", "qid"):
        rows.append((f"warm query, `{t}`: p50 / p95",
                     lambda d, t=t: f"{d['latency_ms'][t]['p50']} / {d['latency_ms'][t]['p95']} ms"))
    rows.append(("exact identifier: ripgrep p50 / p95",
                 lambda d: f"{d['identifier_lookup_ms']['ripgrep_p50']} / {d['identifier_lookup_ms']['ripgrep_p95']} ms"))
    rows.append(("exact identifier: Titan p50 / p95",
                 lambda d: f"{d['identifier_lookup_ms']['scrutate_p50']} / {d['identifier_lookup_ms']['scrutate_p95']} ms"))
    return "\n".join(head + [f"| {name} | " + " | ".join(fn(d) for d in data) + " |" for name, fn in rows])


def haystack_table(path: str) -> str:
    d = json.load(open(path))
    systems = ["BM25 (field-weighted)", "Titan exact scan"] + [c for c in next(iter(d["types"].values())) if c.startswith("Titan nprobe")]
    head = "| query type | n | " + " | ".join(systems) + " | " + " | ".join(f"{c.split('Titan ')[1]} − exact [95% CI]" for c in systems[2:]) + " |"
    rows = [head, "|---|---:|" + "---:|" * (len(systems) + len(systems) - 2)]
    for t, label in TYPE_LABEL.items():
        r = d["types"][t]
        cells = [f"{r[c]['MRR']:.3f}" for c in systems]
        diffs = [f"{r[c]['diff_vs_exact'][0]:+.3f} [{r[c]['diff_vs_exact'][1]:+.3f}, {r[c]['diff_vs_exact'][2]:+.3f}]" for c in systems[2:]]
        rows.append(f"| {label} | {r['Titan exact scan']['n']:,} | " + " | ".join(cells) + " | " + " | ".join(diffs) + " |")
    lat = d["latency_ms"]
    rows.append("")
    rows.append("| query latency on the haystack | " + " | ".join(c for c in lat) + " |")
    rows.append("|---|" + "---:|" * len(lat))
    rows.append("| p50 / p95 | " + " | ".join(f"{v['p50']:.1f} / {v['p95']:.1f} ms" for v in lat.values()) + " |")
    return "\n".join(rows)


def ivf_table(path: str) -> str:
    d = json.load(open(path))
    rows = [f"| clusters probed (of {d['clusters']}) | recall of the exact top-100 | recall of the exact top-10 | vector scan p50 / p95 |",
            "|---|---:|---:|---:|"]
    for k, v in d["probe"].items():
        label = "all (exact)" if int(k) >= d["clusters"] else k
        rows.append(f"| {label} | {v['recall100']:.3f} | {v['recall10']:.3f} | {v['scan_ms_p50']:.2f} / {v['scan_ms_p95']:.2f} ms |")
    rows.append("")
    rows.append(f"Default: {d['default_nprobe']} clusters. Final top-10 identical to the exact scan for "
                f"{d['final_top10_identical']:.1%} of {d['n_queries']} description/keyword queries; "
                f"whole query p50 {d['query_ms_exact_p50']} ms exact vs {d['query_ms_ivf_p50']} ms default.")
    return "\n".join(rows)


def main() -> None:
    test, js, speed = parse(sys.argv[1]), parse(sys.argv[2]), sys.argv[3]
    print("## Held-out Python packages (networkx, mcp, rich)\n")
    print("**Variant A: every docstring removed from the indexed copy** (an undocumented code base). MRR@10, higher is better.\n")
    print(mrr_table(test, "A"), "\n")
    print(hit_table(test, "A"), "\n")
    print("**Variant B: only the first docstring sentence removed** (partially documented code).\n")
    print(mrr_table(test, "B"), "\n")
    print(hit_table(test, "B"), "\n")
    print("## JavaScript transfer (lodash-es, commander, ws, antlr4; JSDoc removed, never seen in training)\n")
    print("**Variant A: every JSDoc block removed.**\n")
    print(mrr_table(js, "A"), "\n")
    print("**Variant B: first description sentence of every JSDoc block removed.**\n")
    print(mrr_table(js, "B"), "\n")
    print("## Speed\n")
    print(speed_tables(speed), "\n")
    if len(sys.argv) > 5:
        print("## Needle in a haystack: held-out packages inside ~130k chunks of unrelated code\n")
        print("Variant A (docstrings removed). MRR@10. \"nprobe\" = clusters probed by the approximate vector scan; "
              "\"exact\" probes all of them.\n")
        print(haystack_table(sys.argv[4]), "\n")
        print("## Approximate vector scan (clustered layout) on the 132k-chunk tree\n")
        print(ivf_table(sys.argv[5]), "\n")


if __name__ == "__main__":
    main()
