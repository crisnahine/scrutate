#!/usr/bin/env python3
"""JavaScript sanity benchmark (same protocol as make_bench.py, JSDoc instead of docstrings).

The ranker and the semantic model were trained on *Python* only, so this measures how much of
the gain survives a language the model never saw. Targets are functions/methods that have a
JSDoc block directly above them; their first description sentence becomes the query.

  variant A  every JSDoc block removed
  variant B  first description sentence of every JSDoc block removed

usage: python bench/make_bench_js.py [NODE_MODULES_DIR]
"""
from __future__ import annotations

import json
import os
import random
import re
import shutil
import sys
from collections import Counter

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from groups import BENCH, is_test_path  # noqa: E402
from make_bench import LIMITS, SECTION_START, clean_rst, split_docstring  # noqa: E402

from scrutate_ref.parse import parse_source  # noqa: E402
from scrutate_ref.text import STOPWORDS, split_identifier  # noqa: E402

# package -> sub-paths that hold exactly one copy of the source (no dist/esm/cjs duplicates)
PACKAGES = {
    "lodash-es": ["."],
    "commander": ["lib", "index.js"],
    "ws": ["lib"],
    "antlr4": ["src"],
}
JSDOC = re.compile(r"/\*\*(?:[^*]|\*(?!/))*\*/")
JS_EXT = (".js", ".mjs", ".cjs")
INLINE_TAG = re.compile(r"\{@(?:link|linkcode|linkplain)\s+([^}|\s]+)[^}]*\}")


def parse_block(block: str) -> tuple[str, list[str]]:
    """JSDoc block -> (description text, tag lines)."""
    lines = []
    for ln in block[3:-2].split("\n"):
        ln = ln.strip()
        ln = re.sub(r"^\*+ ?", "", ln)
        lines.append(ln.rstrip())
    desc, tags = [], []
    for ln in lines:
        if ln.lstrip().startswith("@") or tags:
            tags.append(ln)
        else:
            desc.append(ln)
    text = INLINE_TAG.sub(r"\1", "\n".join(desc)).strip()
    return text, tags


def rebuild(rest: str, tags: list[str]) -> str:
    body = ([rest] if rest else []) + tags
    body = [b.replace("*/", "* /") for b in body]
    return "/**\n" + "".join(f" * {b}\n" for b in body) + " */"


def process_file(src: str, rel: str, with_targets: bool):
    chunks = parse_source(rel, src)
    starts = {}
    for c in chunks:
        if c.kind in ("function", "method"):
            starts.setdefault(c.start, c)
    targets = []
    editsA, editsB = [], []
    for m in JSDOC.finditer(src):
        block = m.group(0)
        end_line = src.count("\n", 0, m.end()) + 1  # 1-based line on which the block ends
        desc, tags = parse_block(block)
        first, rest = split_docstring(desc) if desc else ("", "")
        nl = block.count("\n")
        editsA.append((m.start(), m.end(), "\n" * nl))
        editsB.append((m.start(), m.end(), rebuild(rest, tags) if (rest or tags) else "\n" * nl))
        c = starts.get(end_line + 1)
        if not (with_targets and c is not None and first):
            continue
        name = c.name
        if name in ("constructor",) or name.startswith("__") or SECTION_START.match(first):
            continue
        q = clean_rst(first).rstrip(".:;, ")
        if 4 <= len(q.split()) <= 30:
            targets.append({"name": name, "qualname": c.qualname, "query": q})

    def apply(edits):
        out, pos = [], 0
        for a, b, repl in sorted(edits):
            out.append(src[pos:a])
            out.append(repl)
            pos = b
        out.append(src[pos:])
        return "".join(out)

    return apply(editsA), apply(editsB), targets


def source_files(root: str, subs: list[str]):
    for sub in subs:
        p = os.path.join(root, sub)
        if os.path.isfile(p):
            yield p
            continue
        for dp, dns, fns in os.walk(p):
            dns[:] = [d for d in dns if d not in ("node_modules", "test", "tests", "__tests__", "spec")]
            for fn in fns:
                if fn.endswith(JS_EXT) and not fn.endswith(".min.js"):
                    yield os.path.join(dp, fn)


def build_package(pkg: str, root: str, rng: random.Random) -> dict:
    tag = "js_" + pkg
    base = {v: os.path.join(BENCH, "src", v, tag) for v in ("A", "B")}
    for d in base.values():
        shutil.rmtree(d, ignore_errors=True)
    all_targets, nfiles = [], 0
    pkg_root = os.path.join(root, pkg)
    for full in source_files(pkg_root, PACKAGES[pkg]):
        rel = os.path.relpath(full, pkg_root)
        try:
            with open(full, encoding="utf8") as f:
                src = f.read()
        except (UnicodeDecodeError, OSError):
            continue
        if len(src) > 400_000:
            continue
        nfiles += 1
        a, b, targets = process_file(src, rel, with_targets=not is_test_path(rel))
        for v, text in (("A", a), ("B", b)):
            dst = os.path.join(base[v], rel)
            os.makedirs(os.path.dirname(dst), exist_ok=True)
            with open(dst, "w", encoding="utf8") as f:
                f.write(text)
        for t in targets:
            t["path"] = rel
        all_targets.extend(targets)

    cnt = Counter(t["query"].lower() for t in all_targets)
    all_targets = [t for t in all_targets if cnt[t["query"].lower()] == 1]
    name_cnt = Counter(t["name"] for t in all_targets)
    queries: list[dict] = []

    def add(kind, items):
        rng.shuffle(items)
        for t, q, extra in items[: LIMITS[kind]]:
            queries.append({"type": kind, "query": q, "path": t["path"], "qualname": t["qualname"],
                            "name": t["name"], **extra})

    add("doc", [(t, t["query"], {"match": "qualname"}) for t in all_targets])
    kw = []
    for t in all_targets:
        words = [w for w in re.findall(r"[A-Za-z][A-Za-z0-9_]{2,}", t["query"]) if w.lower() not in STOPWORDS]
        if len(words) >= 3:
            idx = sorted(rng.sample(range(len(words)), 3))
            kw.append((t, " ".join(words[i] for i in idx), {"match": "qualname"}))
    add("kw", kw)
    add("name", [(t, " ".join(split_identifier(t["name"])), {"match": "name"})
                 for t in all_targets if len(split_identifier(t["name"])) >= 2])
    add("id", [(t, t["name"], {"match": "name"}) for t in all_targets
               if len(t["name"]) >= 4 and name_cnt[t["name"]] == 1])
    add("qid", [(t, ".".join(t["qualname"].split(".")[-2:]), {"match": "qualname_suffix"})
                for t in all_targets if "." in t["qualname"]])

    os.makedirs(os.path.join(BENCH, "queries"), exist_ok=True)
    with open(os.path.join(BENCH, "queries", f"{tag}.json"), "w") as f:
        json.dump(queries, f)
    return {"pkg": tag, "files": nfiles, "documented_targets": len(all_targets),
            **Counter(q["type"] for q in queries)}


def main() -> None:
    root = sys.argv[1] if len(sys.argv) > 1 else "/opt/npm-tools/node_modules"
    rng = random.Random(1234)
    for p in PACKAGES:
        print(build_package(p, root, rng), flush=True)


if __name__ == "__main__":
    main()
