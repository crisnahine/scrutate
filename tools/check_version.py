#!/usr/bin/env python3
"""Fail when a release version differs from the one any shipped package declares.

usage: check_version.py VERSION   (release.yml passes the tag without its leading v)
"""
import json
import os
import re
import sys

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")


def declared() -> dict[str, str]:
    out = {}
    for f in ("crates/scrutate/Cargo.toml", "crates/scrutate-py/Cargo.toml"):
        with open(os.path.join(ROOT, f)) as fh:
            # the first `version = "..."` of a manifest is the package's own
            out[f] = re.search(r'^version = "([^"]+)"', fh.read(), re.M).group(1)
    with open(os.path.join(ROOT, "integrations/vscode/package.json")) as fh:
        out["integrations/vscode/package.json"] = json.load(fh)["version"]
    return out


def main() -> int:
    want = sys.argv[1]
    bad = {f: v for f, v in declared().items() if v != want}
    for f, v in bad.items():
        print(f"{f} declares {v}, the tag says {want}")
    if not bad:
        print(f"every package declares {want}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
