#!/usr/bin/env python3
"""Generate crates/scrutate/data/han_fold.txt, the Han fold of ranked search: traditional, simplified
and Japanese shinjitai forms of a character map to one key.

Reads `Unihan_Variants.txt` and `Unihan_Readings.txt` from `Unihan.zip`
(https://www.unicode.org/Public/UCD/latest/ucd/Unihan.zip) and keeps one pair per character whose
`kSimplifiedVariant` differs from itself: the first listed value that is not the character. A
target that is itself folded is followed to the end of its chain.

Then it reads OpenCC's `JPShinjitaiCharacters.txt` at the release pinned below (Apache-2.0), one
shinjitai and its traditional forms per line, and joins the shinjitai to the key of its traditional
form, so 関, 關 and 关 meet. A line is used only when all its values fold to one key: 連 (連 or 聯)
and 闘 (鬭, 鬥) stand for several words. A shinjitai that is a common Chinese character of its own
(in the 2013 standard list, `kTGHZ2013`) with no Mandarin reading in common with its traditional
form is left out too: Chinese 芸 (rue) and 沪 (Shanghai) are other words than 藝 (art) and 濾
(filter). When a join meets two keys, their characters all take one: the simplified form of the
traditional if it has one, else the shinjitai's own. The script checks that folding twice gives the
same result as folding once.

A pair is kept only when both characters are in the scripts the engine cuts into pairs
(`unspaced`), the only place the fold applies. The engine embeds the output file.

usage: gen_han_fold.py PATH/TO/Unihan.zip PATH/TO/JPShinjitaiCharacters.txt
"""
from __future__ import annotations

import hashlib
import os
import re
import sys
import zipfile

OPENCC = "ver.1.4.2"
OPENCC_URL = f"https://raw.githubusercontent.com/BYVoid/OpenCC/{OPENCC}/data/dictionary/JPShinjitaiCharacters.txt"
OPENCC_SHA256 = "12cec7250b873ef52b36d8f92218d4f92c0aaf5d8cd7c58fe42d9785bdcdc43a"


def unspaced(c: str) -> bool:
    """Scripts written without spaces between words, as the engine's text.rs lists them: Han,
    Hiragana, Katakana, and Thai, Lao, Tibetan, Myanmar, Khmer, the Tai scripts, Balinese and Javanese."""
    o = ord(c)
    return (0x0E00 <= o <= 0x109F or 0x1780 <= o <= 0x17FF or 0x1950 <= o <= 0x19DF or 0x1A20 <= o <= 0x1AAF
            or 0x1B00 <= o <= 0x1B7F or 0xA980 <= o <= 0xA9FF or 0xAA60 <= o <= 0xAADF or 0x3005 <= o <= 0x3007 or 0x3040 <= o <= 0x30FF or 0x31F0 <= o <= 0x31FF
            or 0x3400 <= o <= 0x4DBF or 0x4E00 <= o <= 0x9FFF or 0xF900 <= o <= 0xFAFF or 0xFF66 <= o <= 0xFF9F
            or 0x20000 <= o <= 0x323AF)


OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "crates", "scrutate", "data", "han_fold.txt")


def records(text: str):
    """(character, field, value) of each Unihan line."""
    for line in text.splitlines():
        f = line.split("\t")
        if len(f) == 3 and f[0].startswith("U+"):
            yield chr(int(f[0][2:], 16)), f[1], f[2]


def main() -> int:
    if len(sys.argv) != 3:
        print(__doc__.strip().splitlines()[-1], file=sys.stderr)
        return 2
    with zipfile.ZipFile(sys.argv[1]) as z:
        variants = z.read("Unihan_Variants.txt").decode("utf-8")
        readings_text = z.read("Unihan_Readings.txt").decode("utf-8")
    version = re.search(r"^# Unicode Version (\S+)", variants, re.M)
    if not version:
        print("no '# Unicode Version' line in the Unihan header", file=sys.stderr)
        return 1
    raw = open(sys.argv[2], "rb").read()
    if hashlib.sha256(raw).hexdigest() != OPENCC_SHA256:
        print(f"{sys.argv[2]} is not OpenCC {OPENCC}'s file (SHA-256 {OPENCC_SHA256}): {OPENCC_URL}", file=sys.stderr)
        return 1

    fold: dict[str, str] = {}
    for src, field, value in records(variants):
        if field == "kSimplifiedVariant":
            to = next((t for t in (chr(int(v.split("<")[0][2:], 16)) for v in value.split()) if t != src), None)
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

    readings: dict[str, set[str]] = {}
    common = set()
    for c, field, value in records(readings_text):
        if field in ("kMandarin", "kHanyuPinyin", "kXHC1983", "kTGHZ2013"):
            readings.setdefault(c, set()).update(re.sub(r"[0-9.,*:]+", " ", value).split())
        if field == "kTGHZ2013":
            common.add(c)

    members: dict[str, set[str]] = {}
    for s, t in fold.items():
        members.setdefault(t, {t}).add(s)
    def key(c: str) -> str:
        return fold.get(c, c)

    joined, several, apart = 0, 0, []
    for line in raw.decode("utf-8").splitlines():
        if line.startswith("#") or "\t" not in line:
            continue
        s, values = line.split("\t")
        values = values.split()
        if len({key(v) for v in values}) != 1:
            several += 1
            continue
        v = values[0]
        if s in common and not readings.get(s, set()) & (readings.get(v, set()) | readings.get(key(v), set())):
            apart.append(s + v)
            continue
        ks, kv = key(s), key(v)
        if ks == kv:
            continue
        k = kv if v in fold else ks if s in fold else kv
        both = members.pop(ks, {ks}) | members.pop(kv, {kv})
        members[k] = both
        for c in both - {k}:
            fold[c] = k
        joined += 1
    assert all(t not in fold for t in fold.values()), "a fold target is folded again"

    fold = {s: t for s, t in fold.items() if s != t and unspaced(s) and unspaced(t)}
    pairs = "".join(s + fold[s] for s in sorted(fold))
    with open(OUT, "w", encoding="utf-8", newline="\n") as f:
        f.write(f"# Unihan {version.group(1)} kSimplifiedVariant, traditional then simplified, and OpenCC {OPENCC} "
                f"JPShinjitaiCharacters.txt (SHA-256 {OPENCC_SHA256[:16]}), Japanese shinjitai, by tools/gen_han_fold.py\n")
        f.write(pairs + "\n")
    non_bmp = sum(1 for s, t in fold.items() if ord(s) > 0xFFFF or ord(t) > 0xFFFF)
    print(f"wrote {os.path.relpath(OUT)}: {len(fold)} pairs (Unicode {version.group(1)}, OpenCC {OPENCC}), {non_bmp} outside the BMP, "
          f"{chained} chain steps followed; {joined} shinjitai joined, {several} with several keys left out, "
          f"{len(apart)} other Chinese words left out: {' '.join(apart)}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
