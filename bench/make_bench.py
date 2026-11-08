#!/usr/bin/env python3
"""Build a leakage-free code-search benchmark from real packages (CodeSearchNet-style).

For every documented function we create queries from its docstring, then index a *masked*
copy of the package in which docstrings no longer contain the answer:

  variant A  every function/class docstring removed        ("undocumented codebase")
  variant B  first sentence of every docstring removed     ("partially documented")

Query types
  doc   first sentence of the docstring                    (natural-language description)
  kw    3 content words sampled from that sentence         (keyword-style search)
  name  the function name written as words                 ("remove tool" for remove_tool)
  id    bare identifier                                    (exact lookup)
  qid   Class.method                                       (qualified exact lookup)

usage: python bench/make_bench.py [pkg ...]      (default: all DEV + TEST packages)
"""
from __future__ import annotations

import ast
import json
import os
import random
import re
import shutil
import sys
from collections import Counter

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from groups import BENCH, DEV, SP, TEST, is_test_path  # noqa: E402

from scrutate_ref.text import STOPWORDS, split_identifier  # noqa: E402

LIMITS = {"doc": 1200, "kw": 600, "name": 500, "id": 300, "qid": 300}
SECTION_START = re.compile(r"^(parameters|returns?|args|arguments|see also|notes?|examples?|"
                           r"deprecated|raises|yields|attributes|\.\. )", re.I)
SENT_END = re.compile(r"(?<=[.!?])\s+(?=[A-Z`(\[])")


def clean_rst(s: str) -> str:
    s = re.sub(r":\w+:`~?([^`]+)`", r"\1", s)
    s = s.replace("``", "").replace("`", "").replace("*", "")
    return " ".join(s.split())


def split_docstring(ds: str) -> tuple[str, str]:
    """-> (first sentence, remainder) of a cleaned docstring."""
    ds = ds.strip()
    para_end = re.search(r"\n\s*\n", ds)
    m = SENT_END.search(ds)
    cut = len(ds)
    if para_end:
        cut = min(cut, para_end.start())
    if m:
        cut = min(cut, m.start())
    return ds[:cut].strip(), ds[cut:].strip()


def collect_defs(tree: ast.AST):
    out = []

    def walk(node, prefix):
        for ch in ast.iter_child_nodes(node):
            if isinstance(ch, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)):
                q = prefix + ch.name
                out.append((ch, q))
                walk(ch, q + ".")
            else:
                walk(ch, prefix)

    walk(tree, "")
    return out


def docstring_stmt(node):
    b = node.body
    if b and isinstance(b[0], ast.Expr) and isinstance(b[0].value, ast.Constant) \
            and isinstance(b[0].value.value, str):
        return b[0]
    return None


def decorator_names(node) -> set[str]:
    names = set()
    for d in getattr(node, "decorator_list", []):
        t = d.func if isinstance(d, ast.Call) else d
        names.add(t.attr if isinstance(t, ast.Attribute) else getattr(t, "id", ""))
    return names


def process_file(src: str, path: str, with_targets: bool):
    """-> (variantA_src, variantB_src, targets)."""
    try:
        tree = ast.parse(src)
    except (SyntaxError, ValueError, RecursionError):
        return src, src, []
    lines = src.split("\n")
    editsA, editsB, targets = [], [], []
    for node, qual in collect_defs(tree):
        st = docstring_stmt(node)
        if st is None:
            continue
        nxt_ok = len(node.body) == 1 or node.body[1].lineno > st.end_lineno
        line = lines[st.lineno - 1]
        indent = line[: len(line) - len(line.lstrip())]
        if not nxt_ok or line[: st.col_offset].strip():
            continue  # docstring shares a line with code; leave it alone, never a target
        ds = ast.get_docstring(node, clean=True) or ""
        first, rest = split_docstring(ds)
        editsA.append((st.lineno, st.end_lineno, [indent + "pass"]))
        if rest:
            esc = rest.replace("\\", "\\\\").replace('"""', '\\"\\"\\"')
            editsB.append((st.lineno, st.end_lineno, [indent + '"""' + esc + '"""']))
        else:
            editsB.append((st.lineno, st.end_lineno, [indent + "pass"]))
        if (with_targets and isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef))
                and not node.name.startswith("__") and not node.name.startswith("test")
                and not (decorator_names(node) & {"overload", "setter", "deleter", "getter"})
                and not SECTION_START.match(first)):
            q = clean_rst(first).rstrip(".:;, ")
            n = len(q.split())
            if 4 <= n <= 30:
                targets.append({"name": node.name, "qualname": qual, "query": q})

    def apply(edits):
        out = list(lines)
        for a, b, repl in sorted(edits, reverse=True):
            out[a - 1 : b] = repl
        return "\n".join(out)

    return apply(editsA), apply(editsB), targets


def build_package(pkg: str, rng: random.Random) -> dict:
    root = os.path.join(SP, pkg)
    base = {v: os.path.join(BENCH, "src", v, pkg) for v in ("A", "B")}
    for d in base.values():
        shutil.rmtree(d, ignore_errors=True)
    all_targets = []
    nfiles = 0
    for dp, dns, fns in os.walk(root):
        dns[:] = [d for d in dns if d != "__pycache__"]
        for fn in fns:
            if not fn.endswith(".py"):
                continue
            full = os.path.join(dp, fn)
            rel = os.path.relpath(full, root)
            try:
                with open(full, encoding="utf8") as f:
                    src = f.read()
            except (UnicodeDecodeError, OSError):
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

    # drop ambiguous queries (same description on several functions)
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
    kw_items = []
    for t in all_targets:
        words = [w for w in re.findall(r"[A-Za-z][A-Za-z0-9_]{2,}", t["query"])
                 if w.lower() not in STOPWORDS]
        if len(words) >= 3:
            idx = sorted(rng.sample(range(len(words)), 3))
            kw_items.append((t, " ".join(words[i] for i in idx), {"match": "qualname"}))
    add("kw", kw_items)
    add("name", [(t, " ".join(split_identifier(t["name"])), {"match": "name"})
                 for t in all_targets if len(split_identifier(t["name"])) >= 2])
    add("id", [(t, t["name"], {"match": "name"}) for t in all_targets
               if len(t["name"]) >= 4 and name_cnt[t["name"]] == 1])
    add("qid", [(t, ".".join(t["qualname"].split(".")[-2:]), {"match": "qualname_suffix"})
                for t in all_targets if "." in t["qualname"]])

    os.makedirs(os.path.join(BENCH, "queries"), exist_ok=True)
    with open(os.path.join(BENCH, "queries", f"{pkg}.json"), "w") as f:
        json.dump(queries, f)
    by_type = Counter(q["type"] for q in queries)
    return {"pkg": pkg, "files": nfiles, "documented_targets": len(all_targets), **by_type}


def main() -> None:
    pkgs = sys.argv[1:] or (DEV + TEST)
    rng = random.Random(1234)
    for p in pkgs:
        print(build_package(p, rng), flush=True)


if __name__ == "__main__":
    main()
