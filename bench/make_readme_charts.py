#!/usr/bin/env python3
"""The README's benchmark charts, docs/assets/bench-{walltime,index}.svg, from the raw results.

usage:
  python3 bench/make_readme_charts.py [--warm FILE.json] [--compare FILE.json] [--indexed FILE.json] [--check]

  --warm      bench/leaderboard.py output, warm cache (the wall-time rows of docs/LEADERBOARD.md)
  --compare   bench/compare_tools.py output (the start-up table of docs/BENCHMARKS.md)
  --indexed   bench/indexed.py output with zoekt and livegrep (the Linux table of docs/LEADERBOARD.md)
  --check     exit 1 when a committed chart differs from what this would write

Each default is the current file under bench/results/ when it exists (macos-arm64/leaderboard-warm.json,
macos-arm64/compare.json, linux-aarch64/indexed.json), else the published run under superseded/.
Values are formatted by the same function as docs/LEADERBOARD.md, so a chart shows that page's numbers.
The faster bar of each pair is the highlighter, whichever tool it belongs to. Ranking quality
(docs/QUALITY.md) is not charted: its numbers live in run logs that are not committed.
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
import gen_assets as G  # noqa: E402  (tokens, parts, SVG wrapper, write/check)
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


def bar(x, y, w, h, win, on, name, label, k):
    """One bar, drawn like a pen stroke: the winner's is the highlighter with its name in ink, the other a dim
    outline; the value follows in mono."""
    c = G.ON[on]
    delay = f"animation-delay:{0.3 + 0.1 * k:.2f}s"
    if win:
        out = [G.marker(x, y, w, h, "wipe", delay)]
    else:
        out = [f'<rect class="wipe" style="{delay}" x="{x + .75:.1f}" y="{y + .75:.1f}" width="{max(w - 1.5, 1):.1f}" '
               f'height="{h - 1.5:.1f}" fill="none" stroke="{c["dim"]}" stroke-width="1.5"/>']
    inside = G.text_w(name, 12, True) + 16 <= w
    out.append(G.mono(x + 8 if inside else x + w + 10, y + h / 2 + 4.5, name, 12,
                      G.INK if inside and win else c["fg"] if inside or win else c["dim"], weight="700" if win else ""))
    vx = x + w + 10 + (0 if inside else G.text_w(name, 12, True) + 10)
    out.append(G.mono(vx, y + h / 2 + 4.5, label, 13, c["fg"], weight="700" if win else ""))
    return "".join(out)


PHONE_W = 440   # the second, narrow version of each chart, which README.md shows to screens under 600 px


def lay(W):
    """Margins and type for the wide and the phone version."""
    pad = G.PAD if W > 600 else 24
    return pad, 28 if W > 600 else 19


def head(n, title, sub, on, W):
    pad, cap = lay(W)
    return G.label(pad, 46, n, sub, on, W - pad) + G.words(title, pad, 98 if W > 600 else 90, cap, G.ON[on]["fg"])


def foot(b, y, W, lines, colour):
    """Source lines in small mono, wrapped to the panel; returns the y after them."""
    pad, _ = lay(W)
    per = int((W - 2 * pad) / (11 * 0.62))
    for ln in lines:
        while ln:
            cut = len(ln) if len(ln) <= per else ln.rfind(" ", 0, per) + 1 or per
            b.append(G.mono(pad, y, ln[:cut].rstrip(), 11, colour))
            ln, y = ln[cut:], y + 17
    return y


def walltime(warm: dict, compare: dict, W=G.W) -> str:
    env = warm["environment"]
    on, wide = "paper", W > 600
    c = G.ON[on]
    pad, _ = lay(W)
    bh, gap = 22, 6
    bx, bw, row_h = (392, 300, 84) if wide else (pad, W - 2 * pad - 120, 128)
    runs = warm["settings"]["runs"]
    b = [head("04", "WALL TIME, WARM CACHE", "lower is better // each row has its own scale" if wide else "lower is better", on, W),
         G.mono(W - pad, 98, "MARKED: THE FASTER OF THE TWO", 11, c["dim"], 0.14, "end") if wide else
         G.mono(pad, 116, "MARKED: THE FASTER OF THE TWO", 11, c["dim"], 0.14)]
    y = 126 if wide else 132
    k = 0

    def row(title, detail, pairs):
        nonlocal y, k
        b.append(f'<path d="M{pad} {y} H{W - pad}" stroke="{c["rule"]}"/>')
        b.append(f'<text x="{pad}" y="{y + (36 if wide else 28)}" class="s" font-size="16" font-weight="700" fill="{c["fg"]}">{G.esc(title)}</text>')
        if detail:
            b.append(G.mono(pad, y + (58 if wide else 48), detail, 11, c["dim"]))
        top = y + (row_h - 2 * bh - gap) / 2 if wide else y + 62
        vmax = max(v for _, v, _ in pairs)
        vmin = min(v for _, v, _ in pairs)
        for i, (tool, v, label) in enumerate(pairs):
            b.append(bar(bx, top + i * (bh + gap), round(max(3.0, bw * v / vmax), 1), bh, v == vmin, on, tool, label, k))
            k += 1
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
    b.append(f'<path d="M{pad} {y} H{W - pad}" stroke="{c["rule"]}"/>')
    y = foot(b, y + 22, W, [f"{env['cpu']}, macOS, shared with other work, {env['date'][:10]}, binary {sha}, "
                            f"{runs} runs, medians. docs/LEADERBOARD.md"], c["dim"]) - 6
    # start-up comes from another harness with another run count: its own block and its own source line
    st = {TOOL_LABEL.get(s["tool"], s["tool"]): s["startup_median_ms"] for s in compare["startup"] if "startup_median_ms" in s}
    other = min((t for t in st if t != "scrutate"), key=st.get)
    # older results measured an empty directory and say nothing; newer ones record the one file
    one = any(s.get("files") for s in compare["startup"])
    row("Start-up, a directory with one file" if one else "Start-up, empty directory", "",
        [("scrutate", st["scrutate"], f"{st['scrutate']:.1f} ms"), (other, st[other], f"{st[other]:.1f} ms")])
    cenv = compare["environment"]
    b.append(f'<path d="M{pad} {y} H{W - pad}" stroke="{c["rule"]}"/>')
    y = foot(b, y + 22, W, [f"{cenv['cpu']}, macOS, {cenv['date'][:10]}, {compare['runs']} runs, medians. docs/BENCHMARKS.md"], c["dim"])
    H = int(y + 10)
    parts = []
    for bid, t in WALL_ROWS:
        r = warm["benchmarks"][bid]
        bc = r["best_competitor"]
        parts.append(f"{t}: scrutate {ms(r['tools']['scrutate']['median'])} ms, {bc} {ms(r['tools'][bc]['median'])} ms")
    desc = (f"Horizontal bars of median wall time, scrutate against the fastest other tool; the faster bar of each pair is "
            f"marked with a highlighter. {'; '.join(parts)}. "
            f"Start-up in {'a directory with one file' if one else 'an empty directory'}: scrutate {st['scrutate']:.1f} ms, "
            f"{other} {st[other]:.1f} ms.")
    return G.svg(W, H, "Wall time against the fastest other tool", desc, G.panel(H, on, w=W) + "\n".join(b),
                 G.wipe_style(0.7), animated=True)


def indexed(data: dict, W=G.W) -> str:
    env = data["environment"]
    tools = list(data["tools"])
    on, wide = "ink", W > 600
    c = G.ON[on]
    pad, _ = lay(W)
    panels = [("FULL INDEX BUILD", lambda t: t["index"]["median"], lambda t: f"{t['index']['median']:.2f} s"),
              ("INDEX SIZE", lambda t: t["index"]["bytes"], lambda t: f"{t['index']['bytes'] / 1e6:,.0f} MB"),
              ("QUERY TIME, MEDIAN", lambda t: t["latency"]["p50"], lambda t: f"{ms(t['latency']['p50'])} ms")]
    q = data["settings"]["queries"]
    b = [head("04", "THE TEXT INDEX", f"the Linux tree, the same {q} queries for every tool // lower is better" if wide
              else "lower is better", on, W),
         G.mono(W - pad, 98, "AGAINST ZOEKT AND LIVEGREP", 11, c["dim"], 0.14, "end") if wide else
         G.mono(pad, 116, f"AGAINST ZOEKT AND LIVEGREP // {q} QUERIES", 11, c["dim"], 0.14)]
    pw, y0 = (252, 132) if wide else (W - 2 * pad, 138)
    ph = 44 + len(tools) * 34 + 12
    k = 0
    for i, (title, val, label) in enumerate(panels):
        x, yp = (pad + i * (pw + 22), y0) if wide else (pad, y0 + i * ph)
        b.append(f'<path d="M{x} {yp} H{x + pw}" stroke="{c["rule"]}"/>')
        b.append(G.mono(x, yp + 26, title, 11, c["fg"], 0.14))
        vals = {t: val(data["tools"][t]) for t in tools}
        vmax, vmin = max(vals.values()), min(vals.values())
        for j, t in enumerate(tools):
            b.append(bar(x, yp + 44 + j * 34, round(max(3.0, (140 if wide else 220) * vals[t] / vmax), 1), 24, vals[t] == vmin,
                         on, t, label(data["tools"][t]), k))
            k += 1
    y = y0 + (ph if wide else 3 * ph)
    st = data["settings"]
    b.append(f'<path d="M{pad} {y} H{W - pad}" stroke="{c["rule"]}"/>')
    y = foot(b, y + 22, W, [f"A Linux VM on the Apple M3 Pro, {env.get('cpus')} CPUs, {env['date'][:10]}, binary "
                            f"{env.get('scrutate_sha256', '')[:8]}, {st['index_runs']} builds, {st['runs']} runs of each query, medians.",
                            "docs/LEADERBOARD.md, Engine B against zoekt and livegrep"], c["dim"])
    H = int(y + 10)
    desc = "; ".join(f"{t}: build {data['tools'][t]['index']['median']:.2f} s, size {data['tools'][t]['index']['bytes'] / 1e6:,.0f} MB, "
                     f"median query {ms(data['tools'][t]['latency']['p50'])} ms" for t in tools)
    return G.svg(W, H, "The text index against zoekt and livegrep", "Three bar charts, lower is better; the smallest bar "
                 "of each is marked with a highlighter. " + desc + ".", G.panel(H, on, w=W) + "\n".join(b),
                 G.wipe_style(0.7), animated=True)


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
    files = {"bench-walltime.svg": walltime(warm, compare), "bench-walltime-phone.svg": walltime(warm, compare, PHONE_W),
             "bench-index.svg": indexed(idx), "bench-index-phone.svg": indexed(idx, PHONE_W)}
    return G.write_or_check(files, G.ASSETS, a.check)


if __name__ == "__main__":
    sys.exit(main())
