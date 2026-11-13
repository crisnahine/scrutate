#!/usr/bin/env python3
"""Fail when the `unsafe` keywords in the Rust code differ from the counts docs/SECURITY.md lists.

Counts each `unsafe` outside comments and string literals in every .rs file under crates/ and
vendor/ignore, per file, and compares them with the rows "| `path` | count |" of the `unsafe`
inventory in docs/SECURITY.md. A new `unsafe` therefore needs a line in that inventory.

usage: unsafe_count.py
"""
import os
import re
import sys

ROOT = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
DIRS = ("crates", "vendor/ignore")
DOC = "docs/SECURITY.md"
# shortcut: a raw string or a block comment that holds the word counts as a use; switch to a lexer if one ever does
STRING = re.compile(r'"(?:\\.|[^"\\\n])*"')
CHAR = re.compile(r"'(?:\\.|[^'\\\n])'")
ROW = re.compile(r"^ *\| `([^`]+\.rs)` \| (\d+) \|$", re.M)


def count(text: str) -> int:
    n = 0
    for line in text.splitlines():
        line = STRING.sub('""', CHAR.sub("''", line)).split("//", 1)[0]
        n += len(re.findall(r"\bunsafe\b", line))
    return n


def found() -> dict[str, int]:
    out = {}
    for d in DIRS:
        for base, dirs, files in os.walk(os.path.join(ROOT, d)):
            dirs[:] = [x for x in dirs if x != "target"]
            for f in files:
                if f.endswith(".rs"):
                    p = os.path.join(base, f)
                    with open(p, encoding="utf-8") as fh:
                        n = count(fh.read())
                    if n:
                        out[os.path.relpath(p, ROOT).replace(os.sep, "/")] = n
    return out


def main() -> int:
    assert count('let s = "unsafe"; // unsafe\nunsafe { f() }; let c = \'"\'; unsafe fn g(x: &\'a str) { h("x") }') == 2
    have = found()
    with open(os.path.join(ROOT, DOC), encoding="utf-8") as fh:
        listed = {p: int(n) for p, n in ROW.findall(fh.read())}
    if have == listed:
        print(f"{sum(have.values())} uses of unsafe in {len(have)} files, as {DOC} lists")
        return 0
    print(f"the uses of unsafe differ from {DOC}; describe each new one in its inventory, then set these rows:", file=sys.stderr)
    for p in sorted(have.keys() | listed.keys()):
        if have.get(p) != listed.get(p):
            print(f"| `{p}` | {have.get(p, 0)} |   (listed: {listed.get(p, 'no row')})", file=sys.stderr)
    return 1


if __name__ == "__main__":
    sys.exit(main())
