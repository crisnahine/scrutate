#!/usr/bin/env python3
"""Write `~` for this machine's home directory in the committed results and generated documents.

usage: scrub_home.py [--check]

Logs and result files record absolute paths of the machine that made them. This rewrites the
tracked text files under bench/results and docs in place; with --check it changes nothing and
exits 1 when a file still names the home directory.
"""
import os
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def main() -> int:
    check = "--check" in sys.argv[1:]
    home = os.path.expanduser("~").rstrip("/").encode()
    assert home.count(b"/") >= 2, home  # never "/" or a one-part path
    files = subprocess.run(["git", "ls-files", "-z", "bench/results", "docs"], cwd=ROOT, capture_output=True, check=True).stdout.split(b"\0")
    hit = 0
    for rel in filter(None, files):
        if rel.startswith(b"docs/research/"):
            continue  # notes that are left as they were written
        path = os.path.join(ROOT.encode(), rel)
        try:
            with open(path, "rb") as f:
                data = f.read()
        except OSError:
            continue
        if b"\0" in data[:8192] or home not in data:
            continue
        hit += 1
        if check:
            print(rel.decode())
        else:
            with open(path, "wb") as f:
                f.write(data.replace(home, b"~"))
    print(f"{hit} file(s) {'name' if check else 'named'} the home directory")
    return 1 if check and hit else 0


if __name__ == "__main__":
    sys.exit(main())
