#!/usr/bin/env python3
"""The README's images in docs/assets/: one file per panel, built from the tokens and parts below.

usage:
  python3 tools/gen_assets.py            write every image
  python3 tools/gen_assets.py --check    exit 1 when a committed image differs from what this writes, when an
                                         SVG breaks a rule below, or when a link or image in README.md is missing
  python3 tools/gen_assets.py demo       write only demo.svg from docs/assets/demo.json
  python3 tools/gen_assets.py heroes DIR write the three hero directions to DIR, to compare them
  python3 tools/gen_assets.py capture --bin PATH/TO/scrutate --repo DIR
                                         run the demo's commands for real in DIR and write demo.json
  python3 tools/gen_assets.py font --otf PATH/TO/GeistMono-Black.otf
                                         write tools/fonts/geist-mono-black.json, the outlines headlines are drawn with

The system: black ink, cream paper and one highlighter colour (MARK) that marks only what was found: a match,
the winning bar, the one word that matters. Every panel carries its own background, so one file serves
GitHub's light and dark pages. Rules each SVG keeps, checked by --check: flat colour only (no gradient, no
filter), no script and nothing loaded from outside, a title and a desc, valid XML, under 150 KB, and complete
with motion off (the still state is the finished state).

Headlines are outlines of Geist Mono Black (SIL Open Font License 1.1, tools/fonts/OFL.txt), read from
tools/fonts/geist-mono-black.json, so they look the same whatever fonts a reader has.

docs/assets/social-preview.png is rendered by hand from social-preview.svg:
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" --headless=new --disable-gpu \
    --hide-scrollbars --window-size=1280,640 --screenshot=docs/assets/social-preview.png \
    docs/assets/social-preview.svg
Plain Python 3.9, standard library only (the font mode too).
"""
from __future__ import annotations

import argparse
import html
import json
import os
import re
import struct
import subprocess
import sys
import xml.dom.minidom

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
ASSETS = os.path.join(ROOT, "docs", "assets")
FONT_JSON = os.path.join(ROOT, "tools", "fonts", "geist-mono-black.json")

# ---------------------------------------------------------------- tokens: every colour and size, once

INK, PAPER, MARK, ALERT = "#0c0c0e", "#f2eee3", "#d9ff3d", "#ff5a36"
ON = {  # what sits on each panel colour
    "ink": {"bg": INK, "fg": PAPER, "rule": "#2a2a2f", "dim": "#8f8f98"},
    "paper": {"bg": PAPER, "fg": INK, "rule": "#cfc9ba", "dim": "#6f6a5e"},
}
W, RADIUS, INSET, PAD = 880, 14, 10, 40        # panel width, corner, frame inset, content margin
SANS = '-apple-system, BlinkMacSystemFont, "Segoe UI", "Noto Sans", Helvetica, Arial, sans-serif'
MONO = 'ui-monospace, SFMono-Regular, "SF Mono", Menlo, Consolas, "Liberation Mono", monospace'
EASE = "cubic-bezier(.2,.7,.2,1)"
REDUCED = "@media (prefers-reduced-motion: reduce) { * { animation: none !important; } }"
BANNER = "<!-- written by tools/gen_assets.py; edit that, not this file -->"
MAX_BYTES = 150 * 1024

# The documents' images (tools/gen_doc_assets.py) still come in a light and a dark file; these are the
# same tokens under the names that script reads.
THEMES = {
    "dark": {"ink": PAPER, "muted": ON["ink"]["dim"], "line": ON["ink"]["rule"], "card": INK, "card2": "#000000",
             "beam": MARK, "beamsoft": "rgba(217,255,61,.16)", "violet": PAPER, "violetsoft": "rgba(242,238,227,.10)",
             "amber": ALERT, "others": ON["ink"]["dim"], "ok": MARK, "bad": ALERT, "page": "#0d1117"},
    "light": {"ink": INK, "muted": ON["paper"]["dim"], "line": ON["paper"]["rule"], "card": PAPER, "card2": "#ffffff",
              "beam": INK, "beamsoft": "rgba(12,12,14,.16)", "violet": INK, "violetsoft": "rgba(12,12,14,.06)",
              "amber": ALERT, "others": ON["paper"]["dim"], "ok": INK, "bad": ALERT, "page": "#ffffff"},
}
ICON_GREY = ON["paper"]["dim"]


def esc(s: str) -> str:
    return html.escape(s, quote=False)


def svg(w, h, title, desc, body, style="", animated=False):
    css = f".s{{font-family:{SANS}}} .m{{font-family:{MONO}}} .pre{{white-space:pre}}\n" + style
    if animated:
        css += "\n" + REDUCED
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {w} {h}" width="{w}" height="{h}" role="img" '
            f'aria-labelledby="t d">\n{BANNER}\n<title id="t">{esc(title)}</title>\n<desc id="d">{esc(desc)}</desc>\n'
            f"<style>\n{css}\n</style>\n{body}\n</svg>\n")


def text_w(s: str, size: float, mono=False, spacing=0.0) -> float:
    """A generous width estimate for system-font text, so a wider fallback font still fits."""
    return len(s) * size * ((0.62 if mono else 0.56) + spacing)


def kf(name: str, total: float, points, prop="opacity") -> str:
    """@keyframes from (seconds, value[, timing function for the next segment]) points."""
    out = [f"@keyframes {name} {{"]
    for p in points:
        t, v = p[0], p[1]
        tf = f" animation-timing-function: {p[2]};" if len(p) > 2 else ""
        out.append(f"  {100 * t / total:.2f}% {{ {prop}: {v};{tf} }}")
    out.append("}")
    return "\n".join(out)


# ---------------------------------------------------------------- headline type: outlines from the font JSON

_FONT: dict = {}


def font() -> dict:
    if not _FONT:
        with open(FONT_JSON, encoding="utf-8") as f:
            _FONT.update(json.load(f))
    return _FONT


def type_w(s: str, cap: float, track=0.0) -> float:
    """Width of outlined text with capital height `cap` px; `track` is extra space per letter, in em."""
    f = font()
    return (len(s) * f["advance"] + (len(s) - 1) * track * f["upm"]) * cap / f["cap"]


def words(s, x, y, cap, fill, track=0.0, anchor="start", hollow=0.0, knock=None, cls="", extra=""):
    """Outlined text: baseline `y`, capital height `cap`. `hollow` draws only the outline, that many px wide;
    `knock` = (colour, px) paints a band of that colour round each letter, so it reads over anything."""
    f = font()
    k = cap / f["cap"]
    step = f["advance"] + track * f["upm"]
    x0 = x - type_w(s, cap, track) * {"start": 0, "middle": 0.5, "end": 1}[anchor]
    if hollow:
        paint = f'fill="none" stroke="{fill}" stroke-width="{hollow:g}" stroke-linejoin="round" vector-effect="non-scaling-stroke"'
    else:
        paint = f'fill="{fill}"'
    if knock:
        paint += f' stroke="{knock[0]}" stroke-width="{knock[1]:g}" stroke-linejoin="round" paint-order="stroke" vector-effect="non-scaling-stroke"'
    c = f' class="{cls}"' if cls else ""
    out = [f'<g{c} transform="translate({x0:.1f} {y:.1f}) scale({k:.5f} {-k:.5f})" {paint}{extra}>']
    for i, ch in enumerate(s):
        if ch == " ":
            continue
        if ch not in f["glyphs"]:
            raise SystemExit(f"the headline font JSON has no {ch!r}; add it to FONT_CHARS and run the font mode")
        tr = f' transform="translate({i * step:g} 0)"' if i else ""
        out.append(f'<path{tr} d="{f["glyphs"][ch]}"/>')
    out.append("</g>")
    return "".join(out)


# ---------------------------------------------------------------- parts

def panel(h, on, frame=True, dots=False, w=W, r=RADIUS):
    """The background every image carries: a flat ink or paper card, a hairline frame, optional dot grid."""
    c = ON[on]
    out = [f'<rect width="{w}" height="{h}" rx="{r}" fill="{c["bg"]}"/>']
    if dots:
        out.insert(0, f'<defs><pattern id="dots" width="22" height="22" patternUnits="userSpaceOnUse">'
                      f'<rect x="10" y="10" width="2" height="2" fill="{c["rule"]}"/></pattern></defs>')
        out.append(f'<rect x="{INSET}" y="{INSET}" width="{w - 2 * INSET}" height="{h - 2 * INSET}" rx="{r - 6}" fill="url(#dots)"/>')
    if frame:
        out.append(f'<rect x="{INSET + .5}" y="{INSET + .5}" width="{w - 2 * INSET - 1}" height="{h - 2 * INSET - 1}" '
                   f'rx="{r - 6}" fill="none" stroke="{c["rule"]}"/>')
    return "".join(out)


def mono(x, y, s, size, fill, spacing=0.0, anchor="start", weight="", cls=""):
    """System mono text; capitals with spacing make the small labels."""
    ls = f' letter-spacing="{spacing * size:.2f}"' if spacing else ""
    an = f' text-anchor="{anchor}"' if anchor != "start" else ""
    wt = f' font-weight="{weight}"' if weight else ""
    return f'<text x="{x:.1f}" y="{y:.1f}" class="m pre {cls}" font-size="{size}" fill="{fill}"{ls}{an}{wt}>{esc(s)}</text>'


def label(x, y, n, s, on, x_end):
    """`// 01  WHAT THIS IS` in spaced capitals, then a hairline to x_end."""
    c = ON[on]
    t = f"// {n}  {s.upper()}" if n else f"// {s.upper()}"
    end = x + text_w(t, 12, True, 0.16) + 14
    out = mono(x, y, t, 12, c["fg"], 0.16)
    if end < x_end:
        out += f'<path d="M{end:.1f} {y - 4:.1f} H{x_end}" stroke="{c["rule"]}"/>'
    return out


def marker(x, y, w, h, cls="", style="", colour=MARK):
    """A highlighter stroke: a solid block whose two ends lean like a chisel tip. Flat, no blur."""
    j = min(4.0, h * 0.14)
    c = f' class="{cls}"' if cls else ""
    st = f' style="{style}"' if style else ""
    return f'<path{c}{st} d="M{x + j:.1f} {y:.1f} H{x + w + j:.1f} L{x + w:.1f} {y + h:.1f} H{x:.1f} Z" fill="{colour}"/>'


def wipe_style(dur=0.5):
    """`.wipe` slabs draw from the left, each after its own animation-delay; with motion off they are simply there."""
    return (f".wipe{{transform-box:fill-box;transform-origin:left center;animation:wipe {dur}s {EASE} both}}\n"
            "@keyframes wipe { from { transform: scaleX(0); } }\n"
            f".hit{{animation:hit {dur}s steps(1, end) both}}\n@keyframes hit {{ from {{ fill: currentColor; }} }}")


def crosshair(cx, cy, r, colour, cls=""):
    c = f' class="{cls}"' if cls else ""
    return (f'<g{c} stroke="{colour}" fill="none" stroke-width="1.5"><circle cx="{cx}" cy="{cy}" r="{r}"/>'
            f'<path d="M{cx - r * 1.7:g} {cy} H{cx - r * .45:g} M{cx + r * .45:g} {cy} H{cx + r * 1.7:g} '
            f'M{cx} {cy - r * 1.7:g} V{cy - r * .45:g} M{cx} {cy + r * .45:g} V{cy + r * 1.7:g}"/></g>')


def ticks(x, y, w, h, colour, n=10):
    """Corner ticks: four small L marks framing a box."""
    return (f'<path d="M{x} {y + n} V{y} H{x + n} M{x + w - n} {y} H{x + w} V{y + n} M{x + w} {y + h - n} V{y + h} '
            f'H{x + w - n} M{x + n} {y + h} H{x} V{y + h - n}" stroke="{colour}" stroke-width="1.5" fill="none"/>')


def side(s, x, y, colour):
    """Tiny capitals running down the panel's right edge."""
    return (f'<text transform="translate({x} {y}) rotate(90)" class="m pre" font-size="10" fill="{colour}" '
            f'letter-spacing="2">{esc(s.upper())}</text>')


def tag(x, y, s, on, h=24):
    """`SCRUTATE // GREP`: a hairline box of spaced capitals; returns (svg, width)."""
    c = ON[on]
    w = text_w(s, 11, True, 0.14) + 18
    return (f'<rect x="{x + .5:.1f}" y="{y + .5:.1f}" width="{w:.0f}" height="{h - 1}" fill="none" stroke="{c["rule"]}"/>'
            + mono(x + 10, y + h / 2 + 4, s.upper(), 11, c["dim"], 0.14)), w


def cut(x, y, w, h, fill, k=12):
    """A block with its top right corner cut off."""
    return f'<path d="M{x} {y} H{x + w - k} L{x + w} {y + k} V{y + h} H{x} Z" fill="{fill}"/>'


def statement(x, y, line1, line2, cap, on, wipe_at=None):
    """Two heavy lines; the second sits on a highlighter slab."""
    c = ON[on]
    lh = cap * 1.55
    w2 = type_w(line2, cap) + cap * 0.7
    slab = marker(x - cap * 0.3, y + lh - cap * 1.32, w2, cap * 1.62,
                  "wipe" if wipe_at is not None else "", f"animation-delay:{wipe_at}s" if wipe_at is not None else "")
    return words(line1, x, y, cap, c["fg"]) + slab + words(line2, x, y + lh, cap, INK)


# ---------------------------------------------------------------- the wordmark and the logo

WORD = "SCRUTATE"
WORD_TRACK = -0.03


def wordmark(x, y, cap, fill, bg, on_slab=MARK, cursor=True, knock=None, cls_cursor=""):
    """SCRUTATE in Geist Mono Black; the bar of the A is a highlighter stroke, and a block cursor follows.
    `bg` (or `knock`) is a band round the letters, so they read over whatever lies behind.
    Returns (svg, width including the cursor)."""
    f = font()
    k = cap / f["cap"]
    knock = knock or (bg, cap * 0.09)
    step = (f["advance"] + WORD_TRACK * f["upm"]) * k
    ax = x + WORD.index("A") * step
    # the A's counter runs to the baseline; a stroke across both legs takes the place of its bar
    bar = marker(ax + 40 * k, y - 360 * k, 500 * k, 170 * k, colour=on_slab)

    def letters(paint):
        out = []
        for i, ch in enumerate(WORD):
            d = A_OPEN if ch == "A" else f["glyphs"][ch]
            out.append(f'<path transform="translate({x + i * step:.1f} {y:.1f}) scale({k:.5f} {-k:.5f})" d="{d}"/>')
        return f'<g {paint}>' + "".join(out) + "</g>"

    body = [letters(f'fill="{knock[0]}" stroke="{knock[0]}" stroke-width="{knock[1]:g}" stroke-linejoin="round" '
                    'vector-effect="non-scaling-stroke"'), letters(f'fill="{fill}"'), bar]
    width = len(WORD) * step
    if cursor:
        cw = step * 0.62
        cl = f' class="{cls_cursor}"' if cls_cursor else ""
        body.append(f'<rect{cl} x="{x + width + step * 0.12:.1f}" y="{y - cap:.1f}" width="{cw:.1f}" height="{cap:.1f}" fill="{on_slab}"/>')
        width += step * 0.12 + cw
    return "".join(body), width


A_OPEN = "M14 0L192 0L300 476L408 0L586 0L406 710L194 710Z"   # Geist Mono's A without its bar


def lens(dx, dy, s, ring, beam):
    """The mark on a 64 grid: a lens crossed by a highlighter beam."""
    return (f'<g transform="translate({dx} {dy}) scale({s})">'
            f'{marker(2, 22, 52, 10, colour=beam)}'
            f'<circle cx="27" cy="27" r="16" fill="none" stroke="{ring}" stroke-width="7"/>'
            f'<path d="M39 39 L55 55" stroke="{ring}" stroke-width="9" stroke-linecap="square"/></g>')


def logo_mark() -> str:
    body = f'<rect width="64" height="64" rx="12" fill="{INK}"/>' + lens(5, 5, 0.86, PAPER, MARK)
    return svg(64, 64, "Scrutate", "The Scrutate mark: a lens crossed by a highlighter beam, on an ink square.", body)


def logo() -> str:
    h, cap = 96, 44
    wm, ww = wordmark(96, 70, cap, PAPER, INK)
    w = int(96 + ww + 30)
    body = (f'<rect width="{w}" height="{h}" rx="14" fill="{INK}"/>' + lens(18, 16, 1.0, PAPER, MARK) + wm)
    return svg(w, h, "Scrutate", "The Scrutate mark, a lens crossed by a highlighter beam, and the wordmark SCRUTATE "
               "whose A has a highlighter stroke for its bar, on ink.", body)


# ---------------------------------------------------------------- the hero: three directions, one chosen

HERO = "listing"   # the direction README.md shows; the others stay here to compare (`heroes DIR`)
HERO_LINE = ("INSTANT CODE SEARCH", "CODE INTELLIGENCE")
HERO_SMALL = "ONE RUST BINARY"
HERO_DESC = "the line 'Instant code search, code intelligence, one Rust binary'"
DEMO_JSON = os.path.join(ASSETS, "demo.json")


def demo_spec():
    with open(DEMO_JSON, encoding="utf-8") as f:
        return json.load(f)


def marked_line(x, y, s, rx, size, fg, pfx, delay=None, mark_cls="", hit_cls=""):
    """One line of real output in system mono: the path prefix in `pfx`, the rest in `fg`, each match of `rx`
    on a highlighter slab with ink text. With `delay` the slabs wipe in one after another (`wipe_style`);
    else they take `mark_cls` and the matched text `hit_cls`. Returns (svg, slabs)."""
    cw = size * 0.6
    m = LOC.match(s)
    head = m.group(1) if m else ""
    rest = s[len(head):]
    slabs, spans = [], []
    if head:
        spans.append(f'<tspan fill="{pfx}">{esc(head)}</tspan>')
    last = 0
    for mm in rx.finditer(rest) if rx else []:
        spans.append(esc(rest[last:mm.start()]))
        st = f"animation-delay:{delay + 0.12 * len(slabs):.2f}s" if delay is not None else ""
        hc = "hit" if delay is not None else hit_cls
        # the matched text keeps the line's colour until its slab lands (`color` is what the animation starts from)
        attrs = (f' class="{hc}" color="{fg}"' if hc else "") + (f' style="{st}"' if st else "")
        spans.append(f'<tspan fill="{INK}"{attrs}>{esc(mm.group(0))}</tspan>')
        a = len(head) + mm.start()
        slabs.append(marker(x + a * cw - 3, y - size * 0.95, len(mm.group(0)) * cw + 4, size * 1.3,
                            "wipe" if delay is not None else mark_cls, st))
        last = mm.end()
    spans.append(esc(rest[last:]))
    return "".join(slabs) + f'<text x="{x}" y="{y}" class="m pre" font-size="{size}" fill="{fg}">{"".join(spans)}</text>', len(slabs)


def hero_listing():
    """Direction 1: real output fills the panel like a printed listing; the wordmark lies over it and the
    highlighter lands on every match, top to bottom."""
    spec = demo_spec()
    cmd = next(c for c in spec["commands"] if c["argv"][1:3] == ["grep", "--ident"])
    rx = re.compile(cmd["highlight"])
    H, c = 500, ON["ink"]
    b = [panel(H, "ink", frame=False), f'<defs><clipPath id="in"><rect x="{INSET}" y="{INSET}" width="{W - 2 * INSET - 40}" '
         f'height="{H - 2 * INSET}"/></clipPath></defs>', '<g clip-path="url(#in)">']
    b.append(mono(PAD, 46, "$ " + shell_join(cmd["argv"]), 14, c["fg"]))
    t0 = 0.4
    for i, ln in enumerate(cmd["output"]):
        s, n = marked_line(PAD, 82 + i * 23.5, ln, rx, 14, c["dim"], c["rule"], delay=t0 + i * 0.16)
        b.append(s)
    b.append("</g>")
    b.append(f'<rect x="{INSET + .5}" y="{INSET + .5}" width="{W - 2 * INSET - 1}" height="{H - 2 * INSET - 1}" rx="{RADIUS - 6}" fill="none" stroke="{c["rule"]}"/>')
    wm, ww = wordmark(0, 0, 112, PAPER, INK, knock=(INK, 22), cls_cursor="blink")
    x0 = (W - ww) / 2
    b.append(f'<g transform="translate({x0:.1f} 268)">{wm}</g>')
    # the statement sits on its own ink block, cut at the corner
    sx, sy = PAD, 352
    b.append(cut(sx - 16, sy - 40, 520, 130, INK, 18))
    b.append(statement(sx, sy, HERO_LINE[0], HERO_LINE[1], 30, "ink", wipe_at=t0 + 17 * 0.16 + 0.2))
    b.append(side(f"SCRUTATE {spec['version'].split()[-1]} // APACHE-2.0 // {HERO_SMALL}", W - 26, 40, c["dim"]))
    style = [wipe_style(), ".blink{animation:blink 1.1s steps(1) 4.2s 6 both}",
             "@keyframes blink { 50% { opacity: 0; } }"]
    desc = (f"The wordmark SCRUTATE over the real output of `{shell_join(cmd['argv'])}` run by {spec['version']} in "
            f"{spec['repo']}: seventeen lines, every match marked with a highlighter, and {HERO_DESC}.")
    return svg(W, H, "Scrutate: instant code search and code intelligence", desc, "\n".join(b), "\n".join(style), animated=True)


def hero_cropped():
    """Direction 2: the wordmark enormous and cropped, one marked line of code running through it."""
    spec = demo_spec()
    cmd = next(c for c in spec["commands"] if c["argv"][1:3] == ["grep", "--ident"])
    rx = re.compile(cmd["highlight"])
    line = next(ln for ln in cmd["output"] if "pub fn index_dir" in ln)
    H, c = 560, ON["paper"]
    b = [panel(H, "paper", frame=False), f'<defs><clipPath id="in"><rect width="{W}" height="{H}" rx="{RADIUS}"/></clipPath></defs>',
         '<g clip-path="url(#in)">']
    cap = 212
    b.append(words("SCRU", 30, 30 + cap, cap, INK, track=-0.02))
    b.append(words("TATE", 30, H - 26, cap, INK, track=-0.02))
    by = 30 + cap + 18
    bh = H - 26 - cap - 18 - by
    b.append(f'<rect x="0" y="{by}" width="{W}" height="{bh}" fill="{INK}"/>')
    s, _ = marked_line(PAD, by + bh / 2 + 7, line, rx, 19, PAPER, ON["ink"]["dim"], delay=0.6)
    b.append(s)
    hit = rx.search(line[len(LOC.match(line).group(1)):])
    hx = PAD + (len(LOC.match(line).group(1)) + hit.start() + len(hit.group(0)) / 2) * 19 * 0.6
    b.append(crosshair(round(hx), round(by + bh / 2), 26, MARK))
    b.append("</g>")
    b.append(side(f"SCRUTATE // {spec['version'].split()[-1]} // APACHE-2.0", W - 22, 40, c["dim"]))
    b.append(mono(W - PAD, H - 18, HERO_LINE[0] + " // " + HERO_LINE[1], 11, c["dim"], 0.14, "end"))
    desc = f"SCRUTATE in two enormous rows, and between them one real line of output with its match marked."
    return svg(W, H, "Scrutate", desc, "\n".join(b), wipe_style(), animated=True)


def hero_split():
    """Direction 3: ink half GREP with no index, paper half KNOW with the index, the wordmark across the join."""
    H = 500
    b = [f'<defs><clipPath id="l"><rect width="{W / 2}" height="{H}"/></clipPath>'
         f'<clipPath id="r"><rect x="{W / 2}" width="{W / 2}" height="{H}"/></clipPath></defs>',
         f'<rect width="{W}" height="{H}" rx="{RADIUS}" fill="{PAPER}"/>',
         f'<path d="M{RADIUS} 0 H{W / 2} V{H} H{RADIUS} A{RADIUS} {RADIUS} 0 0 1 0 {H - RADIUS} V{RADIUS} A{RADIUS} {RADIUS} 0 0 1 {RADIUS} 0 Z" fill="{INK}"/>']
    b.append(label(PAD, 46, "A", "Engine A // no index", "ink", W / 2 - 20))
    b.append(label(W / 2 + 30, 46, "B", "Engine B // the index", "paper", W - PAD))
    b.append(words("GREP", PAD, 190, 104, PAPER, track=-0.02))
    b.append(words("KNOW", W / 2 + 30, 190, 104, INK, track=-0.02))
    wm, ww = wordmark(0, 0, 78, PAPER, INK, cursor=False)
    wm2, _ = wordmark(0, 0, 78, INK, PAPER, cursor=False)
    x0 = (W - ww) / 2
    b.append(f'<g clip-path="url(#l)"><g transform="translate({x0:.1f} 330)">{wm}</g></g>')
    b.append(f'<g clip-path="url(#r)"><g transform="translate({x0:.1f} 330)">{wm2}</g></g>')
    b.append(marker(PAD - 10, 392, W - 2 * PAD + 20, 48, "wipe", "animation-delay:.6s"))
    b.append(words("INSTANT CODE SEARCH + CODE INTELLIGENCE", W / 2, 426, 21, INK, anchor="middle"))
    desc = "Split poster: GREP on ink for Engine A with no index, KNOW on paper for Engine B the index, the wordmark across the join."
    return svg(W, H, "Scrutate", desc, "\n".join(b), wipe_style(), animated=True)


HEROES = {"listing": hero_listing, "cropped": hero_cropped, "split": hero_split}


# ---------------------------------------------------------------- terminal demo

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
    spec = demo_spec()
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


def demo() -> str:
    spec = demo_spec()
    fs, lh = 14, 19.0
    cw = fs * 0.6
    tx, ty, top = 24, 64, 40                 # the terminal window inside the paper panel
    x0 = tx + 22
    rows = []
    for i, c in enumerate(spec["commands"]):
        if i:
            rows.append(("gap", None))
        rows.append(("cmd", c))
        for ln in trim(c["output"], c.get("keep"), c.get("only")):
            rows.append(("out", (ln, c)))
    rows.append(("gap", None))
    rows.append(("prompt", None))
    th = int(top + 18 + lh * sum(0.5 if k == "gap" else 1 for k, _ in rows) + 12)
    H = ty + th + 28
    # timeline: type, Enter, the output at once and its matches marked, a pause; rest, fade and start again
    t, cps, T = 0.5, 40.0, 15.0
    times = []
    for c in spec["commands"]:
        n = len(shell_join(c["argv"]))
        times.append((t, t + n / cps, t + n / cps + 0.35))
        t += n / cps + 0.35 + 1.5
    rest_end = T - 0.9
    assert times[-1][2] + 2.0 <= rest_end, "the demo needs a rest of 2 s; show fewer or shorter commands"
    p, k = ON["paper"], ON["ink"]
    tw = W - 2 * tx
    b = [panel(H, "paper"), label(PAD, 42, "", f"scrutate in a terminal // real output, {spec['version']}", "paper", W - PAD),
         f'<defs><clipPath id="win"><rect x="{tx}" y="{ty}" width="{tw}" height="{th}" rx="8"/></clipPath></defs>',
         f'<rect x="{tx}" y="{ty}" width="{tw}" height="{th}" rx="8" fill="{INK}"/>']
    for i in range(3):
        b.append(f'<rect x="{tx + 18 + i * 16}" y="{ty + 15}" width="8" height="8" fill="{k["rule"]}"/>')
    b.append(mono(tx + tw - 18, ty + 23, spec["title"].upper(), 11, k["dim"], 0.14, "end"))
    b.append(f'<path d="M{tx} {ty + top} H{tx + tw}" stroke="{k["rule"]}"/>')
    b.append('<g clip-path="url(#win)"><g class="screen">')
    style = [f".screen{{animation: screen {T}s linear infinite}}", kf("screen", T, [(0, 1), (rest_end, 1), (rest_end + 0.5, 0), (T, 0)]),
             f".wipe{{transform-box:fill-box;transform-origin:left center}}",
             "@keyframes wipe { from { transform: scaleX(0); } }"]
    y = ty + top + 18 + fs * 0.8
    ci = -1
    for ri, (kind, pl) in enumerate(rows):
        if kind == "gap":
            y += lh * 0.5
            continue
        if kind == "cmd":
            ci += 1
            c = pl
            s = shell_join(c["argv"])
            t0, t1, t2 = times[ci]
            w = text_w(s, fs, mono=True) + 6
            xs = x0 + 2 * cw
            nch = len(s)
            b.append(f'<text x="{x0}" y="{y:.1f}" class="m pre{f" o{ci - 1}" if ci else ""}" font-size="{fs}" fill="{PAPER}">'
                     f'<tspan fill="{MARK}">$ </tspan>{esc(s)}</text>')
            # a cover slides off the command letter by letter (typing); a block cursor rides its edge
            b.append(f'<rect class="cv{ci}" x="{xs - 1:.1f}" y="{y - fs:.1f}" width="{w + 12:.1f}" height="{lh + 2}" fill="{INK}"/>')
            b.append(f'<rect class="cu{ci}" x="{xs:.1f}" y="{y - fs + 1:.1f}" width="{cw:.1f}" height="{fs + 3}" fill="{MARK}" opacity="0"/>')
            style.append(f".cv{ci}{{transform: translateX({w + 14:.1f}px); animation: cv{ci} {T}s linear infinite}}")
            style.append(kf(f"cv{ci}", T, [(0, "translateX(0px)"), (t0, "translateX(0px)", f"steps({nch}, end)"),
                                            (t1, f"translateX({nch * cw:.1f}px)", "linear"), (t1 + 0.01, f"translateX({w + 14:.1f}px)"),
                                            (T, f"translateX({w + 14:.1f}px)")], "transform"))
            style.append(f".cu{ci}{{animation: cu{ci} {T}s linear infinite, cm{ci} {T}s linear infinite}}")
            style.append(kf(f"cu{ci}", T, [(0, 0), (t0 - 0.3, 0), (t0 - 0.29, 1), (t2, 1), (t2 + 0.01, 0), (T, 0)]))
            style.append(kf(f"cm{ci}", T, [(0, "translateX(0px)"), (t0, "translateX(0px)", f"steps({nch}, end)"),
                                            (t1, f"translateX({nch * cw:.1f}px)"), (T, f"translateX({nch * cw:.1f}px)")], "transform"))
            b.append(f'<g class="o{ci}">')
            hl = re.compile(c["highlight"]) if c.get("highlight") else None
        elif kind == "out":
            ln, c = pl
            if ln.strip() == "...":
                b.append(mono(x0, y, ln, fs, k["dim"]))
            elif hl:
                b.append(marked_line(x0, y, ln, hl, fs, PAPER, k["dim"], mark_cls=f"wipe k{ci}", hit_cls=f"h{ci}")[0])
            else:
                b.append(demo_line(x0, y, ln, fs))
        elif kind == "prompt":
            b.append(mono(x0, y, "$ ", fs, MARK, cls=f"o{ci}"))
            b.append(f'<rect class="pc" x="{x0 + 2 * cw:.1f}" y="{y - fs + 1:.1f}" width="{cw:.1f}" height="{fs + 3}" fill="{MARK}"/>')
            style.append(f".pc{{animation: pc {T}s linear infinite}}")
            style.append(kf("pc", T, [(0, 0), (times[-1][2] + 0.4, 0), (times[-1][2] + 0.41, 1), (T, 1)]))
        y += lh
        if kind in ("out", "cmd") and rows[ri + 1][0] == "gap":
            b.append("</g>")
    b.append("</g></g>")
    for i, (t0, t1, t2) in enumerate(times):
        style.append(f".o{i}{{animation: o{i} {T}s linear infinite}}")
        style.append(kf(f"o{i}", T, [(0, 0), (t2, 0), (t2 + 0.01, 1), (T, 1)]))
        # the matches are marked just after the output lands
        style.append(f".k{i}{{animation: k{i} {T}s linear infinite}}")
        style.append(kf(f"k{i}", T, [(0, "scaleX(0)"), (t2 + 0.25, "scaleX(0)", EASE), (t2 + 0.75, "scaleX(1)"), (T, "scaleX(1)")], "transform"))
        style.append(f".h{i}{{animation: h{i} {T}s linear infinite}}")
        style.append(kf(f"h{i}", T, [(0, "currentColor"), (t2 + 0.6, "currentColor"), (t2 + 0.61, INK), (T, INK)], "fill"))
    cmds = "; ".join(shell_join(c["argv"]) for c in spec["commands"])
    desc = (f"A terminal running {spec['version']} in {spec['repo']}: {cmds}. Every line is the binary's own output, "
            "captured by tools/gen_assets.py capture; the matches of each grep are marked with a highlighter, and a line "
            "'...' marks output left out.")
    return svg(W, H, "Scrutate in a terminal", desc, "\n".join(b), "\n".join(style), animated=True)


def demo_line(x, y, s, fs):
    """search and callers output: the location dim, a [kind] dim, the name bold."""
    k = ON["ink"]
    m = LOC.match(s)
    head, rest = (m.group(1), s[len(m.group(1)):]) if m else ("", s)
    parts = [f'<tspan fill="{k["dim"]}">{esc(head)}</tspan>'] if head else []
    kind = re.match(r"^(\s+\[\w+\]\s)(.*)$", rest)
    if kind:
        parts.append(f'<tspan fill="{k["dim"]}">{esc(kind.group(1))}</tspan><tspan font-weight="700">{esc(kind.group(2))}</tspan>')
    else:
        parts.append(esc(rest))
    return f'<text x="{x}" y="{y:.1f}" class="m pre" font-size="{fs}" fill="{PAPER}">{"".join(parts)}</text>'


# ---------------------------------------------------------------- two engines

ENGINES = {
    "asks": [("CLI", "scrutate ..."), ("PYTHON", "import scrutate"), ("MCP", "for agents"), ("LSP", "for editors")],
    "a": ("A", "NO INDEX", "no index, no parsers, no background process", ["walk", "ignore rules", "match", "stream"],
          [("MATCHES", "grep, streamed")]),
    "b": ("B", "THE INDEX", "persistent, incremental, on disk", ["parse", "symbols", "call edges", "text index", "ranker"],
          [("RANKED RESULTS", "search"), ("DEFINITIONS, CALLERS", "symbol, callers"), ("REFERENCES", "refs")]),
}


def engines() -> str:
    c = ON["ink"]
    H = 480
    b = [panel(H, "ink", dots=True), label(PAD, 46, "01", "Two engines, one binary", "ink", W - PAD)]
    # what you ask: four tags down the left
    b.append(mono(PAD, 92, "YOU ASK", 11, c["dim"], 0.16))
    for i, (t, sub) in enumerate(ENGINES["asks"]):
        y = 108 + i * 64
        b.append(f'<rect x="{PAD + .5}" y="{y + .5}" width="139" height="49" fill="{INK}" stroke="{c["rule"]}"/>')
        b.append(words(t, PAD + 12, y + 24, 13, PAPER))
        b.append(mono(PAD + 12, y + 40, sub, 11, c["dim"]))
    b.append(f'<path d="M{PAD + 140} {108 + 25} H{PAD + 160} M{PAD + 140} {172 + 25} H{PAD + 160} M{PAD + 140} {236 + 25} H{PAD + 160} '
             f'M{PAD + 140} {300 + 25} H{PAD + 160} M{PAD + 160} 133 V325" stroke="{c["dim"]}" stroke-width="1.5" fill="none"/>')
    ex, ew = 224, 360
    gx = ex + ew + 22
    b.append(mono(gx, 92, "YOU GET", 11, c["dim"], 0.16))
    t = 0.5
    for key, ey, eh in (("a", 108, 136), ("b", 262, 190)):
        letter, big, line, steps, gets = ENGINES[key]
        b.append(f'<path d="M{PAD + 160} {ey + 46} H{ex - 4}" stroke="{c["dim"]}" stroke-width="1.5"/>'
                 f'<path d="M{ex - 9} {ey + 41} l5 5 -5 5" stroke="{c["dim"]}" stroke-width="1.5" fill="none"/>')
        b.append(cut(ex, ey, ew, eh, PAPER, 16))
        b.append(words(letter, ex + 18, ey + 66, 50, INK))
        b.append(words(big, ex + 72, ey + 40, 26, INK))
        b.append(f'<text x="{ex + 72}" y="{ey + 62}" class="s" font-size="13" fill="{ON["paper"]["dim"]}">{esc(line)}</text>')
        # the steps, as tags in rows that fit the card
        x, y = ex + 18, ey + 92
        tw = [text_w(st.upper(), 10.5, True, 0.1) + 14 for st in steps]
        for i, st in enumerate(steps):
            if x + tw[i] > ex + ew - 14:
                x, y = ex + 18, y + 34
            elif i:   # a connector only between two steps on the same row
                b.append(f'<path d="M{x - 15:.1f} {y + 13} H{x - 3:.1f}" stroke="{INK}" stroke-width="1.5"/>')
            b.append(f'<rect x="{x + .5:.1f}" y="{y + .5}" width="{tw[i]:.0f}" height="25" fill="none" stroke="{INK}"/>')
            b.append(mono(x + 7, y + 17, st.upper(), 10.5, INK, 0.1))
            x += tw[i] + 18
        for j, (g, sub) in enumerate(gets):
            gy = ey + 6 + j * 62
            yc = gy + 28
            b.append(f'<path d="M{ex + ew + 2} {yc} H{gx - 6}" stroke="{c["dim"]}" stroke-width="1.5"/>')
            b.append(marker(gx, gy, type_w(g, 13) + 20, 30, "wipe", f"animation-delay:{t:.2f}s"))
            b.append(words(g, gx + 10, gy + 21, 13, INK, cls="hit", extra=f' color="{PAPER}" style="animation-delay:{t:.2f}s"'))
            b.append(mono(gx + 2, gy + 52, sub, 12, c["dim"]))
            t += 0.35
    desc = ("What you ask (the CLI, Python, an MCP client for agents, an LSP client for editors) goes to two engines in one "
            "binary. Engine A, no index: no index, no parsers, no background process; walk, ignore rules, match, stream; it "
            "returns matches, grep, streamed. Engine B, the index: persistent, incremental, on disk; parse, symbols, call "
            "edges, text index, ranker; it returns ranked results (search), definitions and callers (symbol, callers) and "
            "references (refs). What comes back is marked with a highlighter.")
    return svg(W, H, "Two engines, one binary", desc, "\n".join(b), wipe_style(), animated=True)


# ---------------------------------------------------------------- section strips

# (file, panel, number, words): a band before each README section; the words are outlined, so they read on a phone
STRIPS = [
    ("strip-quick-start", "paper", "02", "FOURTEEN COMMANDS"),
    ("strip-what-it-does", "ink", "03", "FIFTEEN THINGS IT DOES"),
    ("strip-benchmarks", "paper", "04", "EVERYTHING HERE IS MEASURED"),
    ("strip-agents", "ink", "05", "MCP AND LSP ON STDIO"),
    ("strip-install", "paper", "06", "BUILD IT FROM SOURCE"),
    ("strip-python", "ink", "07", "IMPORT SCRUTATE"),
    ("strip-status", "paper", "08", "NOT DECLARED PRODUCTION-READY"),
    ("strip-documentation", "ink", "09", "EVERY DOCUMENT"),
    ("strip-licence", "paper", "10", "APACHE-2.0"),
]


def strip(on, n, s):
    c = ON[on]
    H = 92
    b = [panel(H, on, frame=False)]
    b.append(words("//", PAD, 62, 30, c["dim"]))
    b.append(words(n, PAD + 66, 62, 30, c["fg"]))
    x = PAD + 66 + type_w(n, 30) + 30
    b.append(words(s, x, 58, 20, c["fg"]))
    end = x + type_w(s, 20) + 22
    b.append(f'<path d="M{end:.1f} 48 H{W - PAD - 34}" stroke="{c["rule"]}"/>')
    b.append(crosshair(W - PAD - 12, 48, 8, c["dim"]))
    return svg(W, H, f"{n}: {s.capitalize()}", f"Section {n} of the README: {s.lower()}.", "\n".join(b))


# ---------------------------------------------------------------- icons, badges, social preview

ICONS = {
    # 24 grid, 1.75 stroke, round caps and joins, no fill
    "regex": '<path d="M5.5 18.5h.01" stroke-width="3.2"/><path d="M14.5 4v12M9.3 7l10.4 6M19.7 7 9.3 13"/>',
    "ident": '<path d="M7 4.5c-1.7 0-2.5.8-2.5 2.5v3c0 1.2-.6 2-1.8 2 1.2 0 1.8.8 1.8 2v3c0 1.7.8 2.5 2.5 2.5"/><path d="M10 7h10M10 12h7M10 17h9"/>',
    "fuzzy": '<path d="M3 9c2-2.5 4-2.5 6 0s4 2.5 6 0 4-2.5 6 0"/><path d="M3 15c2-2.5 4-2.5 6 0s4 2.5 6 0 4-2.5 6 0"/>',
    "archive": '<rect x="3.5" y="4" width="17" height="5" rx="1.5"/><path d="M5 9v9.5c0 .8.7 1.5 1.5 1.5h11c.8 0 1.5-.7 1.5-1.5V9"/><path d="M10 13h4"/>',
    "textindex": '<rect x="3" y="4" width="5" height="5" rx="1"/><rect x="9.5" y="4" width="5" height="5" rx="1"/><rect x="16" y="4" width="5" height="5" rx="1"/><path d="M5.5 12.5v7M12 12.5v4M18.5 12.5v6"/>',
    "ranked": '<path d="M4 6h.01M4 12h.01M4 18h.01"/><path d="M8 6h12M8 12h8.5M8 18h5"/>',
    "callgraph": '<circle cx="12" cy="5" r="2.5"/><circle cx="5.5" cy="18.5" r="2.5"/><circle cx="18.5" cy="18.5" r="2.5"/><path d="M10.6 7.2 6.9 16.3M13.4 7.2l3.7 9.1M8 18.5h8"/>',
    "refs": '<circle cx="12" cy="12" r="3"/><path d="M12 3v4M12 17v4M3 12h4M17 12h4"/><circle cx="12" cy="12" r="8"/>',
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
    """An ink chip with the icon in paper strokes, so one file reads on both page colours."""
    body = (f'<rect width="32" height="32" rx="6" fill="{INK}"/><g transform="translate(5 5) scale(.9167)" stroke="{PAPER}" '
            f'stroke-width="1.9" stroke-linecap="round" stroke-linejoin="round" fill="none">{ICONS[name]}</g>')
    return svg(32, 32, ICON_NAMES[name].capitalize(), f"Icon: {ICON_NAMES[name]}.", body)


def rust_version() -> str:
    """The workspace's `rust-version`, so the badge cannot say another one."""
    with open(os.path.join(ROOT, "Cargo.toml"), encoding="utf8") as f:
        return re.search(r'^rust-version = "([^"]+)"', f.read(), re.M).group(1)


BADGES = {"licence": ("licence", "Apache-2.0"), "rust": ("rust", f"{rust_version()} or newer"), "platforms": ("platforms", "macOS  Linux  Windows")}


def badge(key: str) -> str:
    left, right = BADGES[key]
    lw = text_w(left.upper(), 11, True, 0.12) + 20
    rw = text_w(right, 12, True) + 18
    w, h = lw + rw, 24
    body = (f'<rect width="{w:.0f}" height="{h}" fill="{INK}"/>'
            f'<path d="M{lw:.0f} 6 V18" stroke="{ON["ink"]["rule"]}"/>'
            + mono(10, 16, left.upper(), 11, ON["ink"]["dim"], 0.12)
            + mono(lw + 9, 16.5, right, 12, PAPER))
    return svg(round(w), h, f"{left}: {right}", f"Badge: {left} {right}.", body)


def social() -> str:
    """The 1280 by 640 card a link to the repository shows."""
    SW, SH = 1280, 640
    spec = demo_spec()
    cmd = next(c for c in spec["commands"] if c["argv"][1:3] == ["grep", "--ident"])
    rx = re.compile(cmd["highlight"])
    c = ON["ink"]
    b = [f'<rect width="{SW}" height="{SH}" fill="{INK}"/>']
    for i, ln in enumerate(cmd["output"][:16]):
        s, _ = marked_line(64, 70 + i * 36, ln, rx, 21, c["rule"], c["rule"])
        b.append(s)
    wm, ww = wordmark(0, 0, 150, PAPER, INK, knock=(INK, 26))
    b.append(f'<g transform="translate({(SW - ww) / 2:.1f} 330)">{wm}</g>')
    b.append(cut(48, 400, 760, 176, INK, 22))
    b.append(statement(72, 460, HERO_LINE[0], HERO_LINE[1], 40, "ink"))
    b.append(f'<rect x="{SW - 470}" y="{SH - 78}" width="470" height="78" fill="{INK}"/>')
    b.append(mono(SW - 64, SH - 40, HERO_SMALL + " // APACHE-2.0", 18, c["dim"], 0.16, "end"))
    return svg(SW, SH, "Scrutate", f"Social preview: the wordmark SCRUTATE over real grep output with every match marked, and {HERO_DESC}.",
               "\n".join(b))


# ---------------------------------------------------------------- the font mode: Geist Mono Black to JSON

FONT_CHARS = "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789/.,:;-+%$#()'_?&=<>*"


def _tables(data):
    n = struct.unpack(">H", data[4:6])[0]
    out = {}
    for i in range(n):
        tag, _, off, ln = struct.unpack(">4sIII", data[12 + 16 * i:28 + 16 * i])
        out[tag.decode()] = data[off:off + ln]
    return out


def _cmap(tab):
    n = struct.unpack(">H", tab[2:4])[0]
    for i in range(n):
        off = struct.unpack(">I", tab[8 + 8 * i:12 + 8 * i])[0]
        if struct.unpack(">H", tab[off:off + 2])[0] != 4:
            continue
        seg = struct.unpack(">H", tab[off + 6:off + 8])[0] // 2
        e = off + 14
        ends = struct.unpack(f">{seg}H", tab[e:e + 2 * seg])
        starts = struct.unpack(f">{seg}H", tab[e + 2 * seg + 2:e + 4 * seg + 2])
        deltas = struct.unpack(f">{seg}h", tab[e + 4 * seg + 2:e + 6 * seg + 2])
        ro = e + 6 * seg + 2
        ranges = struct.unpack(f">{seg}H", tab[ro:ro + 2 * seg])
        out = {}
        for s in range(seg):
            for ch in range(starts[s], min(ends[s], 0xFFFE) + 1):
                if ranges[s] == 0:
                    g = (ch + deltas[s]) & 0xFFFF
                else:
                    a = ro + 2 * s + ranges[s] + 2 * (ch - starts[s])
                    g = struct.unpack(">H", tab[a:a + 2])[0]
                    g = (g + deltas[s]) & 0xFFFF if g else 0
                if g:
                    out[ch] = g
        return out
    raise SystemExit("the font has no format 4 cmap")


def _index(d, p):
    count = struct.unpack(">H", d[p:p + 2])[0]
    if not count:
        return [], p + 2
    osz = d[p + 2]
    offs = [int.from_bytes(d[p + 3 + i * osz:p + 3 + (i + 1) * osz], "big") for i in range(count + 1)]
    base = p + 2 + (count + 1) * osz
    return [d[base + offs[i]:base + offs[i + 1]] for i in range(count)], base + offs[-1]


def _dict(b):
    out, ops, i = {}, [], 0
    while i < len(b):
        v = b[i]
        if v <= 21:
            key, i = (1200 + b[i + 1], i + 2) if v == 12 else (v, i + 1)
            out[key], ops = ops, []
        elif v == 28:
            ops.append(struct.unpack(">h", b[i + 1:i + 3])[0]); i += 3
        elif v == 29:
            ops.append(struct.unpack(">i", b[i + 1:i + 5])[0]); i += 5
        elif v == 30:   # a real number: skip its nibbles, these keys are not needed
            i += 1
            while (b[i] & 15) != 15 and (b[i] >> 4) != 15:
                i += 1
            i += 1
            ops.append(0)
        elif v <= 246:
            ops.append(v - 139); i += 1
        elif v <= 250:
            ops.append((v - 247) * 256 + b[i + 1] + 108); i += 2
        else:
            ops.append(-(v - 251) * 256 - b[i + 1] - 108); i += 2
    return out


def _charstring(cs, gsubrs, lsubrs):
    """A CFF Type 2 charstring to SVG path data in font units (y up)."""
    st, out, pos, stems = [], [], [0.0, 0.0], [0]

    def go(op, *pts):
        if op == "M" and out:
            out.append("Z")
        out.append(op + " ".join(f"{v:.0f}" for v in pts))

    def move(dx, dy):
        pos[0] += dx; pos[1] += dy; go("M", *pos)

    def line(dx, dy):
        pos[0] += dx; pos[1] += dy; go("L", *pos)

    def curve(a, b, c, d, e, f):
        x1, y1 = pos[0] + a, pos[1] + b
        x2, y2 = x1 + c, y1 + d
        pos[0], pos[1] = x2 + e, y2 + f
        go("C", x1, y1, x2, y2, *pos)

    def bias(subrs):
        return 107 if len(subrs) < 1240 else 1131 if len(subrs) < 33900 else 32768

    def run(code):
        i = 0
        while i < len(code):
            v = code[i]
            if v == 28:
                st.append(struct.unpack(">h", code[i + 1:i + 3])[0]); i += 3; continue
            if v >= 32:
                if v <= 246:
                    st.append(v - 139); i += 1
                elif v <= 250:
                    st.append((v - 247) * 256 + code[i + 1] + 108); i += 2
                elif v <= 254:
                    st.append(-(v - 251) * 256 - code[i + 1] - 108); i += 2
                else:
                    st.append(struct.unpack(">i", code[i + 1:i + 5])[0] / 65536); i += 5
                continue
            i += 1
            a = st[:]
            st.clear()
            if v in (1, 3, 18, 23, 19, 20):            # stems and hint masks: only their count matters
                stems[0] += len(a) // 2
                if v in (19, 20):
                    i += (stems[0] + 7) // 8
            elif v == 21:
                move(*a[-2:])
            elif v == 22:
                move(a[-1], 0)
            elif v == 4:
                move(0, a[-1])
            elif v == 5:
                for k in range(0, len(a), 2):
                    line(a[k], a[k + 1])
            elif v in (6, 7):
                h = v == 6
                for d in a:
                    line(d, 0) if h else line(0, d)
                    h = not h
            elif v == 8:
                for k in range(0, len(a), 6):
                    curve(*a[k:k + 6])
            elif v == 24:
                for k in range(0, len(a) - 2, 6):
                    curve(*a[k:k + 6])
                line(*a[-2:])
            elif v == 25:
                for k in range(0, len(a) - 6, 2):
                    line(a[k], a[k + 1])
                curve(*a[-6:])
            elif v in (26, 27):
                k, first = (1, a[0]) if len(a) % 4 else (0, 0)
                while k < len(a):
                    if v == 26:
                        curve(first, a[k], a[k + 1], a[k + 2], 0, a[k + 3])
                    else:
                        curve(a[k], first, a[k + 1], a[k + 2], a[k + 3], 0)
                    first, k = 0, k + 4
            elif v in (30, 31):
                h, k = v == 31, 0
                while k + 4 <= len(a):
                    last = len(a) - k == 5
                    e = a[k + 4] if last else 0
                    if h:
                        curve(a[k], 0, a[k + 1], a[k + 2], e, a[k + 3])
                    else:
                        curve(0, a[k], a[k + 1], a[k + 2], a[k + 3], e)
                    h, k = not h, k + (5 if last else 4)
            elif v in (10, 29):
                subrs = lsubrs if v == 10 else gsubrs
                st.extend(a[:-1])
                if run(subrs[int(a[-1]) + bias(subrs)]):
                    return True
            elif v == 11:
                st.extend(a)
                return False
            elif v == 14:
                return True
            else:
                raise SystemExit(f"charstring operator {v} is not handled")
        return False

    run(cs)
    return "".join(out) + "Z"


def font_json(otf: str) -> int:
    data = open(otf, "rb").read()
    t = _tables(data)
    upm = struct.unpack(">H", t["head"][18:20])[0]
    cff = t["CFF "]
    _, p = _index(cff, cff[2])
    tops, p = _index(cff, p)
    _, p = _index(cff, p)
    gsubrs, _ = _index(cff, p)
    top = _dict(tops[0])
    if 1230 in top:
        raise SystemExit("a CID-keyed CFF font is not handled")
    chars, _ = _index(cff, top[17][0])
    psize, poff = top[18]
    priv = _dict(cff[poff:poff + psize])
    lsubrs = _index(cff, poff + priv[19][0])[0] if 19 in priv else []
    cm = _cmap(t["cmap"])
    nh = struct.unpack(">H", t["hhea"][34:36])[0]
    ga = min(cm[ord("A")], nh - 1)
    adv = struct.unpack(">H", t["hmtx"][4 * ga:4 * ga + 2])[0]   # a monospace font: every letter's advance
    glyphs = {ch: _charstring(chars[cm[ord(ch)]], gsubrs, lsubrs) for ch in FONT_CHARS}
    out = {"name": "Geist Mono Black 1.700", "source": "https://github.com/vercel/geist-font (GeistMono-Black.otf)",
           "licence": "SIL Open Font License 1.1, tools/fonts/OFL.txt", "upm": upm, "cap": 710, "advance": adv,
           "glyphs": glyphs}
    os.makedirs(os.path.dirname(FONT_JSON), exist_ok=True)
    with open(FONT_JSON, "w", encoding="utf-8") as f:
        json.dump(out, f, indent=0, sort_keys=True)
        f.write("\n")
    print(f"wrote {FONT_JSON}: {len(glyphs)} glyphs", file=sys.stderr)
    return 0


# ---------------------------------------------------------------- write / check

def outputs(only=None):
    out = {}
    if only in (None, "demo"):
        out["demo.svg"] = demo()
    if only is None:
        out["logo-mark.svg"] = logo_mark()
        out["logo.svg"] = logo()
        out["hero.svg"] = HEROES[HERO]()
        out["engines.svg"] = engines()
        for name, on, n, s in STRIPS:
            out[f"{name}.svg"] = strip(on, n, s)
        out["social-preview.svg"] = social()
        for n in ICONS:
            out[f"icon-{n}.svg"] = icon(n)
        for k in BADGES:
            out[f"badge-{k}.svg"] = badge(k)
    return out


def svg_problems(name: str, text: str):
    """The rules every image keeps."""
    bad = []
    try:
        xml.dom.minidom.parseString(text.encode())
    except Exception as e:  # noqa: BLE001
        bad.append(f"not valid XML ({e})")
    if len(text.encode()) > MAX_BYTES:
        bad.append(f"{len(text.encode()) // 1024} KB, over {MAX_BYTES // 1024} KB")
    if "<title" not in text or "<desc" not in text:
        bad.append("no title or desc")
    for word in ("Gradient", "<filter", "filter:", "<script", "@import", "@font-face", "<image", "<foreignObject"):
        if word in text:
            bad.append(f"contains {word!r}")
    if re.search(r"""(?:href|src)\s*=\s*["'](?!#)""", text) or re.search(r"url\((?!#)", text) or \
            "http" in text.replace('xmlns="http://www.w3.org/2000/svg"', ""):
        bad.append("refers to something outside the file")
    if "@keyframes" in text and "prefers-reduced-motion" not in text:
        bad.append("animated but has no reduced-motion rule")
    return [f"{name}: {p}" for p in bad]


def readme_problems():
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import gen_doc_assets  # noqa: E402  (imports this module; its link checker is the one the documents use)
    problems = []
    with open(os.path.join(ROOT, "README.md"), encoding="utf-8") as f:
        gen_doc_assets.check_links("README.md", f.read(), problems)
    return problems


def write_or_check(files: dict, directory: str, check: bool) -> int:
    bad = 0
    for name, text in files.items():
        p = os.path.join(directory, name)
        for prob in svg_problems(name, text) if name.endswith(".svg") else []:
            print(prob, file=sys.stderr)
            bad = 1
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
    ap.add_argument("what", nargs="?", choices=["demo", "capture", "font", "heroes"])
    ap.add_argument("dir", nargs="?")
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--bin")
    ap.add_argument("--repo")
    ap.add_argument("--otf")
    a = ap.parse_args()
    if a.what == "capture":
        if not (a.bin and a.repo):
            ap.error("capture needs --bin and --repo")
        return capture(a.bin, a.repo)
    if a.what == "font":
        if not a.otf:
            ap.error("font needs --otf")
        return font_json(a.otf)
    if a.what == "heroes":
        if not a.dir:
            ap.error("heroes needs a directory")
        return write_or_check({f"hero-{k}.svg": f() for k, f in HEROES.items()}, a.dir, False)
    bad = write_or_check(outputs(a.what), ASSETS, a.check)
    if a.check and a.what is None:
        for p in readme_problems():
            print(p, file=sys.stderr)
            bad = 1
    return bad


if __name__ == "__main__":
    sys.exit(main())
