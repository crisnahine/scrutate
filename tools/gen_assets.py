#!/usr/bin/env python3
"""The README's images in docs/assets/, light and dark from one source.

usage:
  python3 tools/gen_assets.py            write every image
  python3 tools/gen_assets.py --check    exit 1 when a committed image differs from what this writes
  python3 tools/gen_assets.py demo       write only demo-{light,dark}.svg from docs/assets/demo.json
  python3 tools/gen_assets.py capture --bin PATH/TO/scrutate --repo DIR
                                         run the demo's commands for real in DIR and write demo.json

Each themed image is one template with {{token}} colours, written once per theme, so the two
variants cannot drift. docs/assets/social-preview.png is rendered by hand from social-preview.svg:
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu \
    --hide-scrollbars --window-size=1280,640 --screenshot=docs/assets/social-preview.png \
    docs/assets/social-preview.svg
Plain Python 3.9, standard library only.
"""
from __future__ import annotations

import argparse
import html
import json
import os
import re
import subprocess
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
ASSETS = os.path.join(ROOT, "docs", "assets")

THEMES = {
    "dark": {
        "ink": "#e6edf3", "muted": "#8b949e", "line": "#30363d", "card": "#161b22", "card2": "#0d1117",
        "beam": "#5eead4", "beamsoft": "rgba(94,234,212,.16)", "violet": "#a78bfa", "violetsoft": "rgba(167,139,250,.16)",
        "amber": "#fbbf24", "others": "#6e7681", "ok": "#3fb950", "bad": "#f85149",
        "g1": "#5eead4", "g2": "#a78bfa", "page": "#0d1117", "onbeam": "#0d1117", "onothers": "#ffffff",
        "hl": "rgba(255,255,255,.06)",
    },
    "light": {
        "ink": "#1f2328", "muted": "#59636e", "line": "#d1d9e0", "card": "#f6f8fa", "card2": "#ffffff",
        "beam": "#0f766e", "beamsoft": "rgba(15,118,110,.12)", "violet": "#6d28d9", "violetsoft": "rgba(109,40,217,.10)",
        "amber": "#b45309", "others": "#8c959f", "ok": "#1a7f37", "bad": "#cf222e",
        "g1": "#0f766e", "g2": "#6d28d9", "page": "#ffffff", "onbeam": "#ffffff", "onothers": "#1f2328",
        "hl": "rgba(255,255,255,0)",
    },
}
# one colour that reads on both page colours (3.7:1 on white, 5.1:1 on #0d1117)
MONO_TEAL = "#0d9488"
ICON_GREY = "#8b949e"

SANS = '-apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif'
MONO = 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace'
EASE = "cubic-bezier(.2,.7,.2,1)"
REDUCED = "@media (prefers-reduced-motion: reduce) { * { animation: none !important; } }"
BANNER = "<!-- written by tools/gen_assets.py; edit that, not this file -->"


def esc(s: str) -> str:
    return html.escape(s, quote=False)


def themed(template: str, theme: str) -> str:
    t = THEMES[theme]
    return re.sub(r"\{\{(\w+)\}\}", lambda m: t[m.group(1)], template)


def svg(w, h, title, desc, body, style="", animated=False):
    css = f".s{{font-family:{SANS}}} .m{{font-family:{MONO}}}\n" + style
    if animated:
        css += "\n" + REDUCED
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" '
            f'aria-labelledby="t d" fill="none">\n{BANNER}\n<title id="t">{esc(title)}</title>\n<desc id="d">{esc(desc)}</desc>\n'
            f"<style>\n{css}\n</style>\n{body}\n</svg>\n")


def text_w(s: str, size: float, mono=False) -> float:
    """A generous width estimate, so a wider fallback font still fits."""
    return len(s) * size * (0.62 if mono else 0.56)


def kf(name: str, total: float, points, prop="opacity") -> str:
    """@keyframes from (seconds, value[, timing function for the next segment]) points."""
    out = [f"@keyframes {name} {{"]
    for p in points:
        t, v = p[0], p[1]
        tf = f" animation-timing-function: {p[2]};" if len(p) > 2 else ""
        out.append(f"  {100 * t / total:.2f}% {{ {prop}: {v};{tf} }}")
    out.append("}")
    return "\n".join(out)


# ---------------------------------------------------------------- logo

def mark_paths(stroke: str, beam: str, scale=1.0, dx=0.0, dy=0.0) -> str:
    """The mark on a 64 grid: a lens crossed by a scan beam."""
    tr = f' transform="translate({dx} {dy}) scale({scale})"' if (scale != 1 or dx or dy) else ""
    return (f'<g{tr} stroke-linecap="round" stroke-linejoin="round">'
            f'<circle cx="27" cy="27" r="17" stroke="{stroke}" stroke-width="6"/>'
            f'<path d="M40 40 L55 55" stroke="{stroke}" stroke-width="7.5"/>'
            f'<path d="M4 27 H50" stroke="{beam}" stroke-width="4"/>'
            f'<path d="M18 19.5 H31 M18 34.5 H27" stroke="{stroke}" stroke-width="3" opacity=".55"/>'
            "</g>")


def logo_mark() -> str:
    return svg(64, 64, "Scrutate", "The Scrutate mark: a lens crossed by a scan beam.", mark_paths(MONO_TEAL, MONO_TEAL))


def logo() -> str:
    body = (mark_paths(MONO_TEAL, MONO_TEAL, 1.0, 0, 4)
            + f'<text x="78" y="50" class="m" font-size="40" font-weight="700" fill="{MONO_TEAL}" textLength="200" '
              'lengthAdjust="spacingAndGlyphs">scrutate</text>')
    return svg(290, 72, "Scrutate", "The Scrutate mark, a lens crossed by a scan beam, and the word scrutate.", body)


def grad_defs(gid: str, x1=0, x2=1, user=False) -> str:
    units = ' gradientUnits="userSpaceOnUse"' if user else ""
    return (f'<linearGradient id="{gid}" x1="{x1}" y1="0" x2="{x2}" y2="0"{units}>'
            '<stop offset="0" stop-color="{{g1}}"/><stop offset="1" stop-color="{{g2}}"/></linearGradient>')


# ---------------------------------------------------------------- hero

HERO_CODE = [  # crates/scrutate/src/files.rs, lines 10 to 19
    (10, "/// Where the indexes live under the repository root; no walk enters it."),
    (11, 'pub const INDEX_DIR: &str = ".scrutate";'),
    (12, ""),
    (13, "/// `root/.scrutate`, once `root` is an existing directory and nothing the indexes write into is"),
    (14, "/// a symbolic link (which would put the index, and its deletions, outside the repository)."),
    (15, "pub fn index_dir(root: &Path) -> anyhow::Result<PathBuf> {"),
    (16, "    if !root.is_dir() {"),
    (17, '        anyhow::bail!("{}: {}", root.display(), if root.exists() { "not a directory" } else { "no such directory" });'),
    (18, "    }"),
    (19, "    let dir = root.join(INDEX_DIR);"),
]
HERO_QUERY = re.compile(r"(?i)index_?dir")


def code_line(s: str, query) -> str:
    if s.lstrip().startswith("//"):
        return f'<tspan fill="{{{{muted}}}}">{esc(s)}</tspan>'
    out, last = [], 0
    for m in query.finditer(s):
        out.append(esc(s[last:m.start()]))
        out.append(f'<tspan class="hit">{esc(m.group(0))}</tspan>')
        last = m.end()
    out.append(esc(s[last:]))
    return "".join(out)


def hero() -> str:
    W, H = 1200, 420
    cx, cw = 610, 560
    lh, fs = 27, 18
    sweep0, sweep1 = 0.6, 4.2           # seconds; one pass, then the image rests in its final state
    ch = 66 + lh * (len(HERO_CODE) - 1) + 30
    cy = (H - ch) // 2
    top = cy + 66
    y_first, y_last = top - 6, top + lh * (len(HERO_CODE) - 1) + 12
    body = ["<defs>", grad_defs("hg"), grad_defs("hm", 4, 60, user=True),
            '<linearGradient id="trail" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="{{beam}}" stop-opacity="0"/>'
            '<stop offset="1" stop-color="{{beam}}" stop-opacity=".22"/></linearGradient>',
            f'<clipPath id="code"><rect x="{cx + 1}" y="{cy + 44}" width="{cw - 2}" height="{ch - 45}" rx="11"/></clipPath>',
            "</defs>"]
    # left: mark, wordmark, line, chips
    body.append(mark_paths("url(#hm)", "url(#hm)", 1.25, 36, 52))
    body.append('<text x="128" y="112" class="m" font-size="66" font-weight="700" fill="url(#hg)" textLength="318" '
                'lengthAdjust="spacingAndGlyphs">scrutate</text>')
    body.append('<text class="s" font-size="30" font-weight="600" fill="{{ink}}">'
                '<tspan x="40" y="190">Instant code search and</tspan><tspan x="40" y="230">code intelligence in one Rust binary</tspan></text>')
    chips = [["one binary", "60 languages"], ["grep with no index", "MCP and LSP servers"]]
    for r, row in enumerate(chips):
        x = 40
        for c in row:
            w = text_w(c, 19) + 34
            y = 274 + r * 52
            body.append(f'<rect x="{x}" y="{y}" width="{w:.0f}" height="38" rx="19" fill="{{{{card}}}}" stroke="{{{{line}}}}"/>'
                        f'<circle cx="{x + 18}" cy="{y + 19}" r="4" fill="{{{{beam}}}}"/>'
                        f'<text x="{x + 30}" y="{y + 25.5}" class="s" font-size="19" fill="{{{{ink}}}}">{esc(c)}</text>')
            x += w + 12
    # right: a card of real code, the matches of one query lit
    body.append(f'<rect x="{cx}" y="{cy}" width="{cw}" height="{ch}" rx="12" fill="{{{{card}}}}" stroke="{{{{line}}}}"/>')
    body.append(f'<path d="M{cx + 12} {cy + 1.5} H{cx + cw - 12}" stroke="{{{{hl}}}}"/>')
    for i in range(3):
        body.append(f'<circle cx="{cx + 22 + i * 17}" cy="{cy + 22}" r="5" fill="{{{{line}}}}"/>')
    body.append(f'<text x="{cx + 82}" y="{cy + 28}" class="m" font-size="17"><tspan fill="{{{{beam}}}}">$</tspan>'
                '<tspan fill="{{ink}}"> scrutate grep --ident indexDir</tspan></text>')
    body.append(f'<path d="M{cx} {cy + 44} H{cx + cw}" stroke="{{{{line}}}}"/>')
    body.append('<g clip-path="url(#code)">')
    hits = []
    for i, (n, s) in enumerate(HERO_CODE):
        y = top + i * lh
        if HERO_QUERY.search(s) and not s.lstrip().startswith("//"):
            t_hit = sweep0 + (sweep1 - sweep0) * ((y - 14 - y_first) / (y_last - y_first))
            hits.append((len(hits), t_hit))
            body.append(f'<g class="lit l{len(hits) - 1}"><rect x="{cx + 1}" y="{y - 19}" width="{cw - 2}" height="{lh}" fill="{{{{beamsoft}}}}"/>'
                        f'<rect x="{cx + 1}" y="{y - 19}" width="3" height="{lh}" fill="{{{{beam}}}}"/></g>')
        body.append(f'<text x="{cx + 44}" y="{y}" class="m" font-size="15" fill="{{{{muted}}}}" text-anchor="end" opacity=".8">{n}</text>')
        body.append(f'<text x="{cx + 60}" y="{y}" class="m code" font-size="{fs}" fill="{{{{ink}}}}" xml:space="preserve">{code_line(s, HERO_QUERY)}</text>')
    # the beam: a bright line with a soft trail above it, parked at the bottom when still
    body.append(f'<g class="beam"><rect x="{cx + 1}" y="{y_last - 60}" width="{cw - 2}" height="60" fill="url(#trail)"/>'
                f'<rect x="{cx + 1}" y="{y_last - 1}" width="{cw - 2}" height="2.5" fill="url(#hg)"/></g>')
    body.append("</g>")
    T = sweep1 + 0.8
    dist = y_last - y_first
    style = [".code{white-space:pre} .hit{fill:{{beam}};font-weight:700}",
             f".beam{{animation: sweep {T}s linear 1 both}}",
             kf("sweep", T, [(0, f"translateY({-dist}px)"), (sweep0, f"translateY({-dist}px)"), (sweep1, "translateY(0px)"), (T, "translateY(0px)")], "transform")]
    for k, t in hits:
        style.append(f".l{k}{{animation: lit{k} {T}s {EASE} 1 both}}")
        style.append(kf(f"lit{k}", T, [(0, 0), (t, 0), (t + 0.35, 1), (T, 1)]))
    desc = ("Scrutate's mark and name, the line 'Instant code search and code intelligence in one Rust binary', four facts "
            "(one binary, 60 languages, grep with no index, MCP and LSP servers), and ten real lines of crates/scrutate/src/files.rs "
            "where a scan beam lights the three lines that scrutate grep --ident indexDir matches: INDEX_DIR, index_dir and INDEX_DIR.")
    return svg(W, H, "Scrutate: instant code search and code intelligence", desc, "\n".join(body), "\n".join(style), animated=True)


# ---------------------------------------------------------------- terminal demo

DEMO_JSON = os.path.join(ASSETS, "demo.json")
LOC = re.compile(r"^(\S+?:\d+(?:-\d+)?)(:|\s\s)")


def trim(lines, keep=None, only=None):
    """The lines shown: those matching `only` (each run of the others becomes one '    ...'), then the first `keep`."""
    out = []
    if only:
        rx = re.compile(only)
        for ln in lines:
            if rx.search(ln):
                out.append(ln)
            elif out and out[-1] != "    ...":
                out.append("    ...")
    else:
        out = list(lines)
    if keep and len(out) > keep:
        out = out[:keep] + ["..."]
    return out


def capture(binary: str, repo: str) -> int:
    with open(DEMO_JSON) as f:
        spec = json.load(f)
    env = dict(os.environ, NO_COLOR="1")
    for setup in spec.get("setup", []):
        subprocess.run([binary] + setup[1:], cwd=repo, env=env, check=True, stdin=subprocess.DEVNULL, capture_output=True)
    for c in spec["commands"]:
        argv = [binary] + c["argv"][1:]
        p = subprocess.run(argv, cwd=repo, env=env, stdin=subprocess.DEVNULL, capture_output=True, text=True, timeout=60)
        c["output"] = p.stdout.splitlines()
    spec["version"] = subprocess.run([binary, "--version"], capture_output=True, text=True).stdout.strip()
    with open(DEMO_JSON, "w") as f:
        json.dump(spec, f, indent=2)
        f.write("\n")
    print(f"wrote {DEMO_JSON}", file=sys.stderr)
    return 0


def shell_join(argv) -> str:
    return " ".join(a if re.fullmatch(r"[\w./=:-]+", a) else '"' + a + '"' for a in argv)


def out_line(s: str, hl) -> str:
    m = LOC.match(s)
    head, rest = ("", s)
    if m:
        head, rest = m.group(1), s[len(m.group(1)):]
    parts = [f'<tspan fill="{{{{muted}}}}">{esc(head)}</tspan>'] if head else []
    if s.strip() in ("...",):
        return f'<tspan fill="{{{{muted}}}}">{esc(s)}</tspan>'
    last = 0
    if hl:
        for mm in hl.finditer(rest):
            parts.append(esc(rest[last:mm.start()]))
            parts.append(f'<tspan class="hit">{esc(mm.group(0))}</tspan>')
            last = mm.end()
    tail = rest[last:]
    kind = re.match(r"^(\s+\[\w+\]\s)(.*)$", tail) if not hl else None
    if kind:
        parts.append(f'<tspan fill="{{{{muted}}}}">{esc(kind.group(1))}</tspan><tspan font-weight="700">{esc(kind.group(2))}</tspan>')
    else:
        parts.append(esc(tail))
    return "".join(parts)


def demo() -> str:
    with open(DEMO_JSON) as f:
        spec = json.load(f)
    W, fs, lh, cw = 880, 14, 18.5, 14 * 0.6
    x0, bar = 22, 40
    rows = []  # (kind, payload)
    for i, c in enumerate(spec["commands"]):
        if i:
            rows.append(("gap", None))
        rows.append(("cmd", c))
        for ln in trim(c["output"], c.get("keep"), c.get("only")):
            rows.append(("out", (ln, c)))
    rows.append(("gap", None))
    rows.append(("prompt", None))
    H = int(bar + 20 + lh * sum(0.5 if k == "gap" else 1 for k, _ in rows) + 16)
    # timeline: type, Enter, the output at once, a pause; then everything rests, fades and starts again
    t, cps, T = 0.5, 40.0, 14.0
    times = []
    for c in spec["commands"]:
        n = len(shell_join(c["argv"]))
        t_type = n / cps
        times.append((t, t + t_type, t + t_type + 0.35))
        t += t_type + 0.35 + 1.3
    rest_end = T - 0.9
    assert times[-1][2] + 2.0 <= rest_end, "the demo needs a rest of 2 s; show fewer or shorter commands"
    body = [f'<defs><clipPath id="win"><rect x="1" y="1" width="{W - 2}" height="{H - 2}" rx="11"/></clipPath></defs>',
            f'<rect x=".5" y=".5" width="{W - 1}" height="{H - 1}" rx="12" fill="{{{{card}}}}" stroke="{{{{line}}}}"/>',
            f'<path d="M12 1.5 H{W - 12}" stroke="{{{{hl}}}}"/>']
    for i in range(3):
        body.append(f'<circle cx="{20 + i * 17}" cy="20" r="5" fill="{{{{line}}}}"/>')
    body.append(f'<text x="{W / 2}" y="25" class="s" font-size="14" fill="{{{{muted}}}}" text-anchor="middle">'
                f'{esc(spec["title"])}</text>')
    body.append(f'<path d="M1 {bar} H{W - 1}" stroke="{{{{line}}}}"/>')
    body.append('<g clip-path="url(#win)"><g class="screen">')
    style = [".t{white-space:pre} .hit{fill:{{beam}};font-weight:700}",
             f".screen{{animation: screen {T}s linear infinite}}",
             kf("screen", T, [(0, 1), (rest_end, 1), (rest_end + 0.5, 0), (T, 0)])]
    y = bar + 20 + fs * 0.8
    ci = -1
    for ri, (kind, p) in enumerate(rows):
        if kind == "gap":
            y += lh * 0.5
            continue
        if kind == "cmd":
            ci += 1
            c = p
            s = shell_join(c["argv"])
            t0, t1, t2 = times[ci]
            w = text_w(s, fs, mono=True) + 6
            xs = x0 + 2 * cw
            # a prompt shows once the output before it has
            body.append(f'<text x="{x0}" y="{y:.1f}" class="m t{f" o{ci - 1}" if ci else ""}" font-size="{fs}" fill="{{{{ink}}}}" xml:space="preserve">'
                        f'<tspan fill="{{{{beam}}}}">$ </tspan>{esc(s)}</text>')
            body.append(f'<rect class="cv{ci}" x="{xs - 1:.1f}" y="{y - fs:.1f}" width="{w + 12:.1f}" height="{lh + 2}" fill="{{{{card}}}}"/>')
            body.append(f'<rect class="cu{ci}" x="{xs:.1f}" y="{y - fs + 1:.1f}" width="{cw:.1f}" height="{fs + 3}" fill="{{{{beam}}}}" opacity="0"/>')
            nch = len(s)
            style.append(f".cv{ci}{{transform: translateX({w + 14:.1f}px); animation: cv{ci} {T}s linear infinite}}")
            style.append(kf(f"cv{ci}", T, [(0, "translateX(0px)"), (t0, "translateX(0px)", f"steps({nch}, end)"),
                                            (t1, f"translateX({nch * cw:.1f}px)", "linear"), (t1 + 0.01, f"translateX({w + 14:.1f}px)"),
                                            (T, f"translateX({w + 14:.1f}px)")], "transform"))
            style.append(f".cu{ci}{{animation: cu{ci} {T}s linear infinite, cm{ci} {T}s linear infinite}}")
            style.append(kf(f"cu{ci}", T, [(0, 0), (t0 - 0.3, 0), (t0 - 0.29, 1), (t2, 1), (t2 + 0.01, 0), (T, 0)]))
            style.append(kf(f"cm{ci}", T, [(0, "translateX(0px)"), (t0, "translateX(0px)", f"steps({nch}, end)"),
                                            (t1, f"translateX({nch * cw:.1f}px)"), (T, f"translateX({nch * cw:.1f}px)")], "transform"))
            body.append(f'<g class="o{ci}">')
        elif kind == "out":
            ln, c = p
            hl = re.compile(c["highlight"]) if c.get("highlight") else None
            body.append(f'<text x="{x0}" y="{y:.1f}" class="m t" font-size="{fs}" fill="{{{{ink}}}}" xml:space="preserve">{out_line(ln, hl)}</text>')
        elif kind == "prompt":
            body.append(f'<text x="{x0}" y="{y:.1f}" class="m t o{ci}" font-size="{fs}" fill="{{{{beam}}}}" xml:space="preserve">$ </text>'
                        f'<rect class="pc" x="{x0 + 2 * cw:.1f}" y="{y - fs + 1:.1f}" width="{cw:.1f}" height="{fs + 3}" fill="{{{{beam}}}}"/>')
            style.append(f".pc{{animation: pc {T}s linear infinite}}")
            style.append(kf("pc", T, [(0, 0), (times[-1][2] + 0.4, 0), (times[-1][2] + 0.41, 1), (T, 1)]))
        y += lh
        # close the previous output group before the next command
        if kind in ("out", "cmd") and rows[ri + 1][0] == "gap":
            body.append("</g>")
    body.append("</g></g>")
    for i, (t0, t1, t2) in enumerate(times):
        style.append(f".o{i}{{animation: o{i} {T}s linear infinite}}")
        style.append(kf(f"o{i}", T, [(0, 0), (t2, 0), (t2 + 0.01, 1), (T, 1)]))
    cmds = "; ".join(shell_join(c["argv"]) for c in spec["commands"])
    desc = (f"A terminal running {spec['version']} in {spec['repo']}: {cmds}. Every line is the binary's own output, "
            "captured by tools/gen_assets.py capture; a line '...' marks output left out.")
    return svg(W, H, "Scrutate in a terminal", desc, "\n".join(body), "\n".join(style), animated=True)


# ---------------------------------------------------------------- engines diagram

def card(x, y, w, h, extra=""):
    return f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="12" fill="{{{{card}}}}" stroke="{{{{line}}}}"{extra}/>'


def arrow(d, colour="{{muted}}", flow=None, k=0, T=8.0, length=100):
    """A 1.5 px arrow; with `flow`, a short light travels along it in the direction data flows."""
    out = f'<path d="{d}" stroke="{colour}" stroke-width="1.5" marker-end="url(#ah)"/>'
    if flow:
        out += f'<path class="f{k}" d="{d}" stroke="{flow}" stroke-width="2.5" stroke-linecap="round" pathLength="100" stroke-dasharray="10 100" opacity="0"/>'
    return out


def engines() -> str:
    W, H, T = 880, 384, 8.0
    b = ['<defs><marker id="ah" viewBox="0 0 8 8" refX="7" refY="4" markerWidth="7" markerHeight="7" orient="auto-start-reverse">'
         '<path d="M1 1 L7 4 L1 7" stroke="{{muted}}" stroke-width="1.4" fill="none" stroke-linecap="round" stroke-linejoin="round"/></marker></defs>']
    hdr = [(96, "You ask"), (442, "Two engines, one binary"), (782, "You get")]
    for x, s in hdr:
        b.append(f'<text x="{x}" y="22" class="s" font-size="15" font-weight="600" fill="{{{{muted}}}}" text-anchor="middle">{s}</text>')
    asks = [("CLI", "scrutate ..."), ("Python", "import scrutate"), ("MCP", "for agents"), ("LSP", "for editors")]
    ys = [48, 128, 208, 288]
    for (t, sub), y in zip(asks, ys):
        b.append(card(16, y, 160, 62))
        b.append(f'<text x="34" y="{y + 27}" class="s" font-size="16" font-weight="600" fill="{{{{ink}}}}">{t}</text>')
        b.append(f'<text x="34" y="{y + 48}" class="{"m" if t in ("CLI", "Python") else "s"}" font-size="14" fill="{{{{muted}}}}">{esc(sub)}</text>')
        b.append(f'<path d="M176 {y + 31} H200" stroke="{{{{muted}}}}" stroke-width="1.5"/>')
    b.append(f'<path d="M200 {ys[0] + 31} V{ys[-1] + 31}" stroke="{{{{muted}}}}" stroke-width="1.5"/>')
    ax, aw, ay, ah = 234, 420, 44, 120
    by, bh = 188, 176
    flows = []
    b.append(arrow(f"M200 {ay + ah / 2} H{ax - 3}", flow="{{beam}}", k=0)); flows.append(0.0)
    b.append(arrow(f"M200 {by + bh / 2} H{ax - 3}", flow="{{violet}}", k=1)); flows.append(0.3)
    # Engine A
    b.append(card(ax, ay, aw, ah, ' stroke-width="1"'))
    b.append(f'<rect x="{ax}" y="{ay + 14}" width="3" height="40" rx="1.5" fill="{{{{beam}}}}"/>')
    b.append(f'<text x="{ax + 20}" y="{ay + 32}" class="s" font-size="18" font-weight="600" fill="{{{{ink}}}}">Engine A</text>')
    b.append(f'<text x="{ax + 20}" y="{ay + 54}" class="s" font-size="15" fill="{{{{muted}}}}">no index, no parsers, no background process</text>')

    def chips(x, y, names, colour, soft):
        out = []
        for i, n in enumerate(names):
            w = len(n) * 15 * 0.52 + 24
            out.append(f'<rect x="{x:.0f}" y="{y}" width="{w:.0f}" height="32" rx="8" fill="{soft}" stroke="{colour}" stroke-opacity=".55"/>'
                       f'<text x="{x + w / 2:.0f}" y="{y + 21}" class="s" font-size="15" fill="{{{{ink}}}}" text-anchor="middle">{n}</text>')
            x += w
            if i < len(names) - 1:
                out.append(f'<path d="M{x + 3:.0f} {y + 16} H{x + 15:.0f}" stroke="{{{{muted}}}}" stroke-width="1.5" marker-end="url(#ah)"/>')
                x += 20
        return "".join(out)

    b.append(chips(ax + 20, ay + 72, ["walk", "ignore rules", "match", "stream"], "{{beam}}", "{{beamsoft}}"))
    # Engine B
    b.append(card(ax, by, aw, bh))
    b.append(f'<rect x="{ax}" y="{by + 14}" width="3" height="40" rx="1.5" fill="{{{{violet}}}}"/>')
    b.append(f'<text x="{ax + 20}" y="{by + 32}" class="s" font-size="18" font-weight="600" fill="{{{{ink}}}}">Engine B</text>')
    b.append(f'<text x="{ax + 20}" y="{by + 54}" class="s" font-size="15" fill="{{{{muted}}}}">the index: persistent, incremental, on disk</text>')
    b.append(chips(ax + 20, by + 72, ["parse", "symbols", "call edges"], "{{violet}}", "{{violetsoft}}"))
    b.append(chips(ax + 20, by + 120, ["text index", "ranker"], "{{violet}}", "{{violetsoft}}"))
    # what comes back
    rx, rw = 700, 164
    gets = [(ay + ah / 2, "matches", "{{beam}}", "grep, streamed"), (by + 30, "ranked results", "{{violet}}", "search"),
            (by + 88, "definitions, callers", "{{violet}}", "symbol, callers"), (by + 146, "references", "{{violet}}", "refs")]
    for i, (yc, t, col, sub) in enumerate(gets):
        b.append(card(rx, yc - 26, rw, 52))
        b.append(f'<text x="{rx + 16}" y="{yc - 4}" class="s" font-size="15" font-weight="600" fill="{{{{ink}}}}">{t}</text>')
        b.append(f'<text x="{rx + 16}" y="{yc + 16}" class="m" font-size="14" fill="{{{{muted}}}}">{sub}</text>')
        x_from = ax + aw
        y_from = yc if i == 0 else by + bh / 2 + (i - 2) * 40
        d = f"M{x_from} {y_from} C{x_from + 26} {y_from} {rx - 30} {yc} {rx - 3} {yc}"
        b.append(arrow(d, flow=col, k=i + 2))
        flows.append(1.2 + 0.25 * i)
    style = []
    for k, t0 in enumerate(flows):
        style.append(f".f{k}{{animation: fl{k} {T}s {EASE} infinite}}")
        style.append(kf(f"fl{k}", T, [(0, "10"), (t0, "10"), (t0 + 1.6, "-100"), (T, "-100")], "stroke-dashoffset"))
        style.append(kf(f"fo{k}", T, [(0, 0), (t0, 0), (t0 + 0.05, 1), (t0 + 1.55, 1), (t0 + 1.6, 0), (T, 0)]))
        style[-3] = f".f{k}{{animation: fl{k} {T}s {EASE} infinite, fo{k} {T}s linear infinite}}"
    desc = ("What you ask (the CLI, Python, an MCP client for agents, an LSP client for editors) goes to two engines in one binary. "
            "Engine A searches with no index: walk, ignore rules, match, stream, and returns matches. Engine B is the index: parse, "
            "symbols, call edges, a text index and a ranker; it returns ranked results, definitions and callers, and references.")
    return svg(W, H, "Two engines, one binary", desc, "\n".join(b), "\n".join(style), animated=True)


# ---------------------------------------------------------------- icons

ICONS = {
    # 24 grid, 1.75 stroke, round caps and joins, no fill
    "regex": '<path d="M5.5 18.5h.01" stroke-width="3.2"/><path d="M14.5 4v12M9.3 7l10.4 6M19.7 7 9.3 13"/>',
    "ident": '<path d="M7 4.5c-1.7 0-2.5.8-2.5 2.5v3c0 1.2-.6 2-1.8 2 1.2 0 1.8.8 1.8 2v3c0 1.7.8 2.5 2.5 2.5"/><path d="M10 7h10M10 12h7M10 17h9"/>',
    "fuzzy": '<path d="M3 9c2-2.5 4-2.5 6 0s4 2.5 6 0 4-2.5 6 0"/><path d="M3 15c2-2.5 4-2.5 6 0s4 2.5 6 0 4-2.5 6 0"/>',
    "archive": '<rect x="3.5" y="4" width="17" height="5" rx="1.5"/><path d="M5 9v9.5c0 .8.7 1.5 1.5 1.5h11c.8 0 1.5-.7 1.5-1.5V9"/><path d="M10 13h4"/>',
    "textindex": '<rect x="3" y="4" width="5" height="5" rx="1"/><rect x="9.5" y="4" width="5" height="5" rx="1"/><rect x="16" y="4" width="5" height="5" rx="1"/><path d="M5.5 12.5v7M12 12.5v4M18.5 12.5v6"/>',
    "ranked": '<path d="M4 6h.01M4 12h.01M4 18h.01"/><path d="M8 6h12M8 12h8.5M8 18h5"/>',
    "callgraph": '<circle cx="12" cy="5" r="2.5"/><circle cx="5.5" cy="18.5" r="2.5"/><circle cx="18.5" cy="18.5" r="2.5"/><path d="M10.6 7.2 6.9 16.3M13.4 7.2l3.7 9.1M8 18.5h8"/>',
    "refs": '<circle cx="12" cy="12" r="3"/><path d="M12 3v4M12 17v4M3 12h4M17 12h4"/><circle cx="12" cy="12" r="8" opacity=".55"/>',
    "dupes": '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M16 8V5.5c0-.8-.7-1.5-1.5-1.5h-9C4.7 4 4 4.7 4 5.5v9c0 .8.7 1.5 1.5 1.5H8"/>',
    "structural": '<rect x="9" y="3" width="6" height="5" rx="1.2"/><rect x="3" y="16" width="6" height="5" rx="1.2"/><rect x="15" y="16" width="6" height="5" rx="1.2"/><path d="M12 8v4M6 16v-4h12v4"/>',
    "mcp": '<rect x="4" y="4" width="16" height="7" rx="2"/><rect x="4" y="13" width="16" height="7" rx="2"/><path d="M8 7.5h.01M8 16.5h.01M12 7.5h4M12 16.5h4"/>',
    "lsp": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 8.5h18"/><path d="M7 13h5M7 16.5h8"/><path d="M14.5 11.5v3"/>',
    "python": '<path d="M10 14a4 4 0 0 0 5.7 0l3-3a4 4 0 0 0-5.7-5.7l-1 1"/><path d="M14 10a4 4 0 0 0-5.7 0l-3 3a4 4 0 0 0 5.7 5.7l1-1"/>',
    "watch": '<path d="M20 11a8 8 0 0 0-14.6-4.4L4 8.5"/><path d="M4 4v4.5h4.5"/><path d="M4 13a8 8 0 0 0 14.6 4.4L20 15.5"/><path d="M20 20v-4.5h-4.5"/>',
    "repos": '<path d="M3 7.5V18c0 .8.7 1.5 1.5 1.5h7"/><path d="M7 4h4l1.5 2H19c.8 0 1.5.7 1.5 1.5V14c0 .8-.7 1.5-1.5 1.5H8.5C7.7 15.5 7 14.8 7 14V4Z"/>',
    "doc": '<path d="M14 3H7c-.8 0-1.5.7-1.5 1.5v15c0 .8.7 1.5 1.5 1.5h10c.8 0 1.5-.7 1.5-1.5V7.5Z"/><path d="M14 3v4.5h4.5M9 12h6M9 15.5h6"/>',
    "terminal": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="m7 9.5 3 2.5-3 2.5M12.5 15h4.5"/>',
    "chart": '<path d="M4 4v16h16"/><path d="M8 16v-4M12 16V8M16 16v-6"/>',
    "shield": '<path d="M12 3 5 6v5.5c0 4.3 3 7.9 7 9.5 4-1.6 7-5.2 7-9.5V6Z"/><path d="m9 12 2 2 4-4"/>',
    "check": '<rect x="3.5" y="3.5" width="17" height="17" rx="2.5"/><path d="m8 12.5 2.8 2.7L16.5 9"/>',
    "flag": '<path d="M5 21V4"/><path d="M5 4.5c4-2 6 2 10 0s4 0 4 0v8.5s-1-2-4 0-6-2-10 0"/>',
    "book": '<path d="M4 5.5C4 4.7 4.7 4 5.5 4H11v16H5.5c-.8 0-1.5-.7-1.5-1.5Z"/><path d="M20 5.5c0-.8-.7-1.5-1.5-1.5H13v16h5.5c.8 0 1.5-.7 1.5-1.5Z"/>',
    "plug": '<path d="M9 3v4M15 3v4"/><path d="M6 7h12v3a6 6 0 0 1-12 0Z"/><path d="M12 16v5"/>',
    "language": '<path d="m8 7-5 5 5 5M16 7l5 5-5 5M13.5 4.5l-3 15"/>',
    "tag": '<path d="M3.5 12.6V5c0-.8.7-1.5 1.5-1.5h7.6l8 8a1.5 1.5 0 0 1 0 2.1l-7.5 7.5a1.5 1.5 0 0 1-2.1 0Z"/><path d="M8 8h.01"/>',
    "map": '<circle cx="12" cy="12" r="8.5"/><path d="m15.5 8.5-2 5-5 2 2-5Z"/>',
}
ICON_NAMES = {
    "regex": "literal and regex search", "ident": "identifier search in all casings", "fuzzy": "fuzzy search",
    "archive": "search inside archives", "textindex": "text index", "ranked": "ranked search", "callgraph": "call graph",
    "refs": "references", "dupes": "duplicates", "structural": "structural query", "mcp": "MCP server", "lsp": "language server",
    "python": "Python bindings", "watch": "an index kept fresh", "repos": "cross-repository search", "doc": "document",
    "terminal": "command line", "chart": "benchmarks", "shield": "security", "check": "checked results", "flag": "release",
    "book": "glossary", "plug": "integrations", "language": "languages", "tag": "name", "map": "documentation map",
}


def icon(name: str) -> str:
    body = (f'<g stroke="{ICON_GREY}" stroke-width="1.75" stroke-linecap="round" stroke-linejoin="round" fill="none">'
            f"{ICONS[name]}</g>")
    return svg(24, 24, ICON_NAMES[name].capitalize(), f"Icon: {ICON_NAMES[name]}.", body)


# ---------------------------------------------------------------- badges, divider, social preview

BADGES = {"licence": ("licence", "Apache-2.0"), "rust": ("rust", "1.91 or newer"), "platforms": ("platforms", "macOS  Linux  Windows")}


def badge(key: str) -> str:
    left, right = BADGES[key]
    lw, rw = text_w(left, 12) + 18, text_w(right, 12) + 18
    w, h = lw + rw, 22
    body = (f'<defs><clipPath id="c"><rect width="{w:.0f}" height="{h}" rx="6"/></clipPath></defs>'
            f'<g clip-path="url(#c)"><rect width="{lw:.0f}" height="{h}" fill="#30363d"/>'
            f'<rect x="{lw:.0f}" width="{rw:.0f}" height="{h}" fill="#0f766e"/></g>'
            f'<text x="{lw / 2:.0f}" y="15.5" class="m" font-size="12" fill="#e6edf3" text-anchor="middle">{esc(left)}</text>'
            f'<text x="{lw + rw / 2:.0f}" y="15.5" class="m" font-size="12" fill="#ffffff" text-anchor="middle" xml:space="preserve">{esc(right)}</text>')
    return svg(round(w), h, f"{left}: {right}", f"Badge: {left} {right}.", body)


def divider() -> str:
    W, H, T = 880, 16, 9.0
    body = ('<defs><linearGradient id="dl" x1="0" y1="0" x2="1" y2="0"><stop offset="0" stop-color="{{beam}}" stop-opacity="0"/>'
            '<stop offset=".5" stop-color="{{beam}}"/><stop offset="1" stop-color="{{beam}}" stop-opacity="0"/></linearGradient></defs>'
            f'<path d="M0 8 H{W}" stroke="{{{{line}}}}"/>'
            f'<rect class="b" x="0" y="7" width="140" height="2" rx="1" fill="url(#dl)"/>')
    style = [f".b{{transform: translateX({W / 2 - 70}px); animation: run {T}s {EASE} infinite}}",
             kf("run", T, [(0, "translateX(-160px)"), (0.5, "translateX(-160px)"), (5.5, f"translateX({W + 20}px)"), (T, f"translateX({W + 20}px)")], "transform")]
    return svg(W, H, "Divider", "A hairline with a short scan beam on it.", body, "\n".join(style), animated=True)


def social() -> str:
    W, H = 1280, 640
    b = ["<defs>" + grad_defs("sg") + grad_defs("sm", 4, 60, user=True) + "</defs>", f'<rect width="{W}" height="{H}" fill="{{{{page}}}}"/>']
    b.append(mark_paths("url(#sm)", "url(#sm)", 2.6, 96, 150))
    b.append('<text x="290" y="262" class="m" font-size="118" font-weight="700" fill="url(#sg)" textLength="566" lengthAdjust="spacingAndGlyphs">scrutate</text>')
    b.append('<text class="s" font-size="46" font-weight="600" fill="{{ink}}"><tspan x="100" y="390">Instant code search and code intelligence</tspan>'
             '<tspan x="100" y="450">in one Rust binary</tspan></text>')
    b.append('<text x="100" y="540" class="m" font-size="28" fill="{{muted}}">grep  |  index  |  search  |  callers  |  refs  |  serve  |  lsp</text>')
    b.append(f'<rect x="0" y="{H - 8}" width="{W}" height="8" fill="url(#sg)"/>')
    return svg(W, H, "Scrutate", "Social preview: the Scrutate mark and name, and the line 'Instant code search and code intelligence in one Rust binary'.", "\n".join(b))


# ---------------------------------------------------------------- write / check

def outputs(only=None):
    out = {}
    def both(name, tmpl):
        for th in THEMES:
            out[f"{name}-{th}.svg"] = themed(tmpl, th)
    if only in (None, "demo"):
        both("demo", demo())
    if only is None:
        out["logo-mark.svg"] = logo_mark()
        out["logo.svg"] = logo()
        both("hero", hero())
        both("engines", engines())
        both("divider", divider())
        out["social-preview.svg"] = themed(social(), "dark")
        for n in ICONS:
            out[f"icon-{n}.svg"] = icon(n)
        for k in BADGES:
            out[f"badge-{k}.svg"] = badge(k)
    return out


def write_or_check(files: dict, directory: str, check: bool) -> int:
    bad = 0
    for name, text in files.items():
        p = os.path.join(directory, name)
        if check:
            cur = open(p, encoding="utf-8").read() if os.path.exists(p) else None
            if cur != text:
                print(f"differs: {os.path.relpath(p, ROOT)}", file=sys.stderr)
                bad = 1
        else:
            os.makedirs(directory, exist_ok=True)
            with open(p, "w", encoding="utf-8") as f:
                f.write(text)
    if not check:
        print(f"wrote {len(files)} files in {os.path.relpath(directory, ROOT)}", file=sys.stderr)
    return bad


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("what", nargs="?", choices=["demo", "capture"])
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--bin")
    ap.add_argument("--repo")
    a = ap.parse_args()
    if a.what == "capture":
        if not (a.bin and a.repo):
            ap.error("capture needs --bin and --repo")
        return capture(a.bin, a.repo)
    return write_or_check(outputs(a.what), ASSETS, a.check)


if __name__ == "__main__":
    sys.exit(main())
