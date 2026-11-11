#!/usr/bin/env python3
"""Generate crates/scrutate/data/han_fold.txt, the traditional-to-simplified Han fold of ranked search.

Reads `Unihan_Variants.txt` (inside https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip) and
keeps one pair per character whose `kSimplifiedVariant` differs from itself: the first listed value
that is not the character. A target that is itself folded is followed to the end of its chain, so
folding twice gives the same result as folding once. A pair is kept only when both characters are in
the scripts the engines cut into pairs (`_unspaced`), the only place the fold applies. Both engines
read the output file.

usage: gen_han_fold.py PATH/TO/Unihan_Variants.txt
"""
from __future__ import annotations

import os
import re
import sys

sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "python"))
from scrutate_ref.text import _unspaced  # noqa: E402

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
    fold = {s: t for s, t in fold.items() if s != t and _unspaced(s) and _unspaced(t)}
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
