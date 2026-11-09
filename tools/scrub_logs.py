#!/usr/bin/env python3
"""Replace machine-specific paths in the committed logs under bench/results/ (the home directory,
the checkout, temporary and worktree directories) with short placeholders.

usage: scrub_logs.py [DIR...]   (default: bench/results/platforms)
"""
import os
import re
import sys

ROOT = os.path.abspath(os.path.join(os.path.dirname(__file__), ".."))
HOME = os.path.expanduser("~")
RULES = [
    (re.compile(r"/private/tmp/[^\s'\"]*?/scratchpad"), "$TMPDIR"),
    (re.compile(r"/(?:private/)?var/folders/[^\s'\"/]+/[^\s'\"/]+/T"), "$TMPDIR"),
    (re.compile(re.escape(ROOT) + r"/\.claude/worktrees/[^\s'\"/]+"), "<repo>"),
    (re.compile(re.escape(ROOT)), "<repo>"),
    (re.compile(re.escape(HOME)), "~"),
    (re.compile(r"(export 'PATH=|PATH=')[^'\n]*'"), r"\1...'"),
]


def main() -> int:
    dirs = sys.argv[1:] or [os.path.join(ROOT, "bench", "results", "platforms")]
    changed = 0
    for d in dirs:
        for base, _, files in os.walk(d):
            for f in files:
                if not f.endswith((".log", ".txt", ".md")):
                    continue
                p = os.path.join(base, f)
                s = open(p, encoding="utf-8", errors="replace").read()
                t = s
                for rx, rep in RULES:
                    t = rx.sub(rep, t)
                if t != s:
                    open(p, "w", encoding="utf-8").write(t)
                    changed += 1
    print(f"{changed} files scrubbed", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
