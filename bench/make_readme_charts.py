#!/usr/bin/env python3
"""The README's benchmark charts, docs/assets/bench-{walltime,index}-{light,dark}.svg, from the raw results.

usage:
  python3 bench/make_readme_charts.py [--warm FILE.json] [--compare FILE.json] [--indexed FILE.json] [--check]

  --warm      bench/leaderboard.py output, warm cache (the wall-time rows of docs/LEADERBOARD.md)
  --compare   bench/compare_tools.py output (the start-up table of docs/BENCHMARKS.md)
  --indexed   bench/indexed.py output with zoekt and livegrep (the Linux table of docs/LEADERBOARD.md)
  --check     exit 1 when a committed chart differs from what this would write

Each default is the current file under bench/results/ when it exists (macos-arm64/leaderboard-warm.json,
macos-arm64/compare.json, linux-aarch64/indexed.json), else the published run under superseded/.
Values are formatted by the same function as docs/LEADERBOARD.md, so a chart shows that page's numbers.
Ranking quality (docs/QUALITY.md) is not charted: its numbers live in run logs that are not committed.
"""
from __future__ import annotations

import argparse
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.normpath(os.path.join(HERE, ".."))
sys.path.insert(0, os.path.join(ROOT, "tools"))
sys.path.insert(0, HERE)
import gen_assets as G  # noqa: E402  (palette, SVG wrapper, write/check)
from make_leaderboard_md import ms  # noqa: E402

RESULTS = os.path.join(ROOT, "bench", "results")

# (benchmark id in the warm run, what the row says)
WALL_ROWS = [
    ("linux/literal", "Linux kernel tree, literal"),
    ("linux/complex regex", "Linux tree, complex regex"),
    ("linux/very many matches", "Linux tree, 1.3 million matches"),
    ("bigfile/literal", "One 740 MB file, literal"),
    ("smallfiles/literal", "200,000 small files, literal"),
    ("linux/structural", "Linux tree, structural query"),
]
TOOL_LABEL = {"rg": "ripgrep", "grep": "GNU grep"}


def default(platform: str, name: str, published: str) -> str:
    cur = os.path.join(RESULTS, platform, name)
    return cur if os.path.exists(cur) else os.path.join(RESULTS, platform, "superseded", published)


def bar(x, y, w, h, colour, cls):
    # 4 px rounded data end, square at the baseline
    r = min(4, w / 2, h / 2)
    d = f"M{x} {y} H{x + w - r} Q{x + w} {y} {x + w} {y + r} V{y + h - r} Q{x + w} {y + h} {x + w - r} {y + h} H{x} Z"
    return f'<path class="{cls}" d="{d}" fill="{colour}"/>'


def grow_style(T=1.6):
    """Bars grow to their value once and stay; the values follow."""
    return "\n".join([
        f".g{{transform-box: fill-box; transform-origin: left center; animation: grow {T}s {G.EASE} .3s 1 both}}",
        f".v{{animation: show .5s {G.EASE} {T}s 1 both}}",
        "@keyframes grow { from { transform: scaleX(0); } to { transform: scaleX(1); } }",
        "@keyframes show { from { opacity: 0; } to { opacity: 1; } }"])


def tool_name(x, y, t, us, anchor="end"):
    weight = ' font-weight="600"' if us else ""
    return (f'<text x="{x}" y="{y}" class="s" font-size="14" fill="{{{{{"ink" if us else "muted"}}}}}" '
            f'text-anchor="{anchor}"{weight}>{G.esc(t)}</text>')


def walltime(warm: dict, compare: dict) -> str:
    env = warm["environment"]
    W = 880
    lx, tx, bx, bw = 24, 372, 384, 380   # label, tool name (right-aligned), bar start, longest bar
    bh, gap, row_h = 18, 4, 76
    runs = warm["settings"]["runs"]
    b = ['<text x="24" y="30" class="s" font-size="19" font-weight="600" fill="{{ink}}">Wall time, warm cache, lower is better</text>',
         '<text x="24" y="54" class="s" font-size="14" fill="{{muted}}">scrutate against the fastest other tool on each benchmark. '
         'Each row has its own scale.</text>',
         '<rect x="656" y="19" width="14" height="14" rx="3" fill="{{beam}}"/><text x="676" y="31" class="s" font-size="14" fill="{{ink}}">scrutate</text>'
         '<rect x="750" y="19" width="14" height="14" rx="3" fill="{{others}}"/><text x="770" y="31" class="s" font-size="14" fill="{{ink}}">fastest other</text>']
    y = 76

    def row(title, detail, pairs):
        nonlocal y
        b.append(f'<path d="M24 {y} H{W - 24}" stroke="{{{{line}}}}"/>')
        b.append(f'<text x="{lx}" y="{y + 32}" class="s" font-size="15" font-weight="600" fill="{{{{ink}}}}">{G.esc(title)}</text>')
        if detail:
            b.append(f'<text x="{lx}" y="{y + 54}" class="s" font-size="14" fill="{{{{muted}}}}">{G.esc(detail)}</text>')
        top = y + (row_h - 2 * bh - gap) / 2
        vmax = max(v for _, v, _ in pairs)
        for i, (tool, v, label) in enumerate(pairs):
            yy = top + i * (bh + gap)
            w = max(2.0, bw * v / vmax)
            us = tool == "scrutate"
            b.append(tool_name(tx, yy + 14, tool, us))
            b.append(bar(bx, yy, round(w, 1), bh, "{{beam}}" if us else "{{others}}", "g"))
            b.append(f'<text class="m v" x="{bx + w + 8:.1f}" y="{yy + 14}" font-size="14" fill="{{{{ink}}}}">{G.esc(label)}</text>')
        y += row_h

    for bid, title in WALL_ROWS:
        r = warm["benchmarks"][bid]
        best = r["best_competitor"]
        pairs = []
        for t in ("scrutate", best):
            x = r["tools"][t]
            assert x.get("status") == "ok", (bid, t)
            n = x.get("n", 0)
            pairs.append((t, x["median"], f"{ms(x['median'])} ms" + (f" ({n} runs)" if n and n < runs else "")))
        row(title, f"ratio {r['ratio']:.3f}, 95% CI {r['ci95'][0]:.3f} to {r['ci95'][1]:.3f}, {r['status']}", pairs)
    sha = env.get("scrutate_sha256", "")[:8]
    b.append(f'<path d="M24 {y} H{W - 24}" stroke="{{{{line}}}}"/>')
    b.append(f'<text x="24" y="{y + 24}" class="s" font-size="14" fill="{{{{muted}}}}">{G.esc(env["cpu"])}, macOS, shared with other work, '
             f'{env["date"][:10]}, binary {sha}, {runs} runs, medians. docs/LEADERBOARD.md</text>')
    y += 40
    # start-up comes from another harness with another run count: its own block and its own source line
    st = {TOOL_LABEL.get(s["tool"], s["tool"]): s["startup_median_ms"] for s in compare["startup"] if "startup_median_ms" in s}
    other = min((t for t in st if t != "scrutate"), key=st.get)
    # older results measured an empty directory and say nothing; newer ones record the one file
    one = any(s.get("files") for s in compare["startup"])
    row("Start-up, a directory with one file" if one else "Start-up, empty directory", "", [("scrutate", st["scrutate"], f"{st['scrutate']:.1f} ms"), (other, st[other], f"{st[other]:.1f} ms")])
    cenv = compare["environment"]
    b.append(f'<path d="M24 {y} H{W - 24}" stroke="{{{{line}}}}"/>')
    b.append(f'<text x="24" y="{y + 24}" class="s" font-size="14" fill="{{{{muted}}}}">{G.esc(cenv["cpu"])}, macOS, {cenv["date"][:10]}, '
             f'{compare["runs"]} runs, medians. docs/BENCHMARKS.md</text>')
    H = int(y + 40)
    parts = []
    for bid, t in WALL_ROWS:
        r = warm["benchmarks"][bid]
        bc = r["best_competitor"]
        parts.append(f"{t}: scrutate {ms(r['tools']['scrutate']['median'])} ms, {bc} {ms(r['tools'][bc]['median'])} ms")
    desc = (f"Horizontal bars of median wall time, scrutate against the fastest other tool. {'; '.join(parts)}. "
            f"Start-up in {'a directory with one file' if one else 'an empty directory'}: scrutate {st['scrutate']:.1f} ms, {other} {st[other]:.1f} ms.")
    return G.svg(W, H, "Wall time against the fastest other tool", desc, "\n".join(b), grow_style(), animated=True)


def indexed(data: dict) -> str:
    env = data["environment"]
    tools = list(data["tools"])
    W = 880
    panels = [("Full index build", lambda t: t["index"]["median"], lambda t: f"{t['index']['median']:.2f} s"),
              ("Index size", lambda t: t["index"]["bytes"], lambda t: f"{t['index']['bytes'] / 1e6:,.0f} MB"),
              ("Query time, median", lambda t: t["latency"]["p50"], lambda t: f"{ms(t['latency']['p50'])} ms")]
    b = ['<text x="24" y="30" class="s" font-size="19" font-weight="600" fill="{{ink}}">The text index against zoekt and livegrep, lower is better</text>',
         f'<text x="24" y="54" class="s" font-size="14" fill="{{{{muted}}}}">The Linux tree, the same {data["settings"]["queries"]} queries for every tool. '
         'Each panel has its own scale.</text>']
    pw, px0, y0 = 266, 24, 76
    for i, (title, val, label) in enumerate(panels):
        x = px0 + i * (pw + 21)
        b.append(f'<path d="M{x} {y0} H{x + pw}" stroke="{{{{line}}}}"/>')
        b.append(f'<text x="{x}" y="{y0 + 30}" class="s" font-size="15" font-weight="600" fill="{{{{ink}}}}">{title}</text>')
        vmax = max(val(data["tools"][t]) for t in tools)
        for j, t in enumerate(tools):
            yy = y0 + 48 + j * 30
            us = t == "scrutate"
            w = max(2.0, 110 * val(data["tools"][t]) / vmax)
            b.append(tool_name(x, yy + 14, t, us, "start"))
            b.append(bar(x + 70, yy, round(w, 1), 18, "{{beam}}" if us else "{{others}}", "g"))
            b.append(f'<text class="m v" x="{x + 70 + w + 8:.1f}" y="{yy + 14}" font-size="14" fill="{{{{ink}}}}">{label(data["tools"][t])}</text>')
    y = y0 + 48 + len(tools) * 30 + 8
    st = data["settings"]
    b.append(f'<path d="M24 {y} H{W - 24}" stroke="{{{{line}}}}"/>')
    b.append(f'<text x="24" y="{y + 24}" class="s" font-size="14" fill="{{{{muted}}}}">A Linux VM on the Apple M3 Pro, {env.get("cpus")} CPUs, '
             f'{env["date"][:10]}, binary {env.get("scrutate_sha256", "")[:8]}, {st["index_runs"]} builds, {st["runs"]} runs of each query, '
             'medians.</text>')
    b.append(f'<text x="24" y="{y + 44}" class="s" font-size="14" fill="{{{{muted}}}}">docs/LEADERBOARD.md, Engine B against zoekt and livegrep</text>')
    H = y + 60
    desc = "; ".join(f"{t}: build {data['tools'][t]['index']['median']:.2f} s, size {data['tools'][t]['index']['bytes'] / 1e6:,.0f} MB, "
                     f"median query {ms(data['tools'][t]['latency']['p50'])} ms" for t in tools)
    return G.svg(W, H, "The text index against zoekt and livegrep", "Three bar charts, lower is better. " + desc + ".",
                 "\n".join(b), grow_style(), animated=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--warm", default=default("macos-arm64", "leaderboard-warm.json", "leaderboard-warm-9aa0.json"))
    ap.add_argument("--compare", default=default("macos-arm64", "compare.json", "compare-9aa0.json"))
    ap.add_argument("--indexed", default=default("linux-aarch64", "indexed.json", "indexed-261e.json"))
    ap.add_argument("--check", action="store_true")
    a = ap.parse_args()
    with open(a.warm) as f:
        warm = json.load(f)
    with open(a.compare) as f:
        compare = json.load(f)
    with open(a.indexed) as f:
        idx = json.load(f)
    wall, ix = walltime(warm, compare), indexed(idx)
    files = {}
    for th in G.THEMES:
        files[f"bench-walltime-{th}.svg"] = G.themed(wall, th)
        files[f"bench-index-{th}.svg"] = G.themed(ix, th)
    return G.write_or_check(files, G.ASSETS, a.check)


if __name__ == "__main__":
    sys.exit(main())
