#!/usr/bin/env python3
"""Generate crates/scrutate/data/han_fold.txt, the traditional-to-simplified Han fold of ranked search.

Reads `Unihan_Variants.txt` (inside https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip) and
keeps one pair per character whose `kSimplifiedVariant` differs from itself: the first listed value
that is not the character. A target that is itself folded is followed to the end of its chain, so
folding twice gives the same result as folding once. A pair is kept only when both characters are in
the scripts the engine cuts into pairs (`unspaced`), the only place the fold applies. The engine
embeds the output file.

usage: gen_han_fold.py PATH/TO/Unihan_Variants.txt
"""
from __future__ import annotations

import os
import re
import sys


def unspaced(c: str) -> bool:
    """Scripts written without spaces between words, as the engine's text.rs lists them: Han,
    Hiragana, Katakana, and Thai, Lao, Tibetan, Myanmar, Khmer, the Tai scripts, Balinese and Javanese."""
    o = ord(c)
    return (0x0E00 <= o <= 0x109F or 0x1780 <= o <= 0x17FF or 0x1950 <= o <= 0x19DF or 0x1A20 <= o <= 0x1AAF
            or 0x1B00 <= o <= 0x1B7F or 0xA980 <= o <= 0xA9FF or 0xAA60 <= o <= 0xAADF or 0x3005 <= o <= 0x3007 or 0x3040 <= o <= 0x30FF or 0x31F0 <= o <= 0x31FF
            or 0x3400 <= o <= 0x4DBF or 0x4E00 <= o <= 0x9FFF or 0xF900 <= o <= 0xFAFF or 0xFF66 <= o <= 0xFF9F
            or 0x20000 <= o <= 0x323AF)


OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "crates", "scrutate", "data", "han_fold.txt")


def main() -> int:
    if len(sys.argv) != 2:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    text = open(sys.argv[1], encoding="utf-8").read()
    version = re.search(r"^# Unicode Version (\S+)", text, re.M)
    if not version:
        print("no '# Unicode Version' line in the file header", file=sys.stderr)
        return 1
    fold: dict[str, str] = {}
    for line in text.splitlines():
        f = line.split("\t")
        if len(f) != 3 or f[1] != "kSimplifiedVariant":
            continue
        src = chr(int(f[0][2:], 16))
        targets = [chr(int(v.split("<")[0][2:], 16)) for v in f[2].split()]
        to = next((t for t in targets if t != src), None)
        if to is not None:
            fold[src] = to
    chained = 0
    for src in fold:
        seen, to = {src}, fold[src]
        while to in fold and fold[to] not in seen:
            seen.add(to)
            to = fold[to]
            chained += 1
        fold[src] = to
    fold = {s: t for s, t in fold.items() if s != t and unspaced(s) and unspaced(t)}
    pairs = "".join(s + fold[s] for s in sorted(fold))
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        f.write(f"# Unihan {version.group(1)} kSimplifiedVariant, traditional then simplified, by tools/gen_han_fold.py\n")
        f.write(pairs + "\n")
    non_bmp = sum(1 for s, t in fold.items() if ord(s) > 0xFFFF or ord(t) > 0xFFFF)
    print(f"wrote {os.path.relpath(OUT)}: {len(fold)} pairs (Unicode {version.group(1)}), {non_bmp} outside the BMP, "
          f"{chained} chain steps followed", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
