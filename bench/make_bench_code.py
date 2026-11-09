#!/usr/bin/env python3
"""Benchmarks for Rust, Go, Java and JavaScript/TypeScript (same protocol as make_bench.py).

Doc comments take the place of docstrings: JSDoc and Javadoc `/** */` blocks, Rust `///` and
`//!` lines, and Go `//` comment blocks directly above a top-level declaration. Targets are
functions and methods with a doc comment directly above them (Rust attributes may sit between);
the first sentence of that comment becomes the query.

  variant A  every doc comment removed
  variant B  first sentence of every doc comment removed

Go doc comments start with the function's name ("NewServer creates ..."); that leading name is
dropped from the query, so a doc query does not contain the identifier.

usage: python bench/make_bench_code.py fetch            clone the repositories in groups.CODE
       python bench/make_bench_code.py [tag ...]        build (default: every tag in groups.CODE)
"""
from __future__ import annotations

import json
import os
import random
import re
import shutil
import subprocess
import sys
from collections import Counter

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from groups import BENCH, CODE, CODE_SRC  # noqa: E402
from make_bench import LIMITS, SECTION_START, clean_rst, split_docstring  # noqa: E402

from scrutate_ref.parse import language_for, parse_source  # noqa: E402
from scrutate_ref.text import STOPWORDS, is_test_path, split_identifier  # noqa: E402

LANG_EXT = {"rust": (".rs",), "go": (".go",), "java": (".java",),
            "javascript": (".js", ".mjs", ".cjs"), "typescript": (".ts", ".js")}
BLOCK = re.compile(r"/\*\*(?!/)(?:[^*]|\*(?!/))*\*/")
INLINE_TAG = re.compile(r"\{@(?:link|linkcode|linkplain|code|literal|value)\s+([^}]*)\}")
MD_LINK = re.compile(r"\[([^\]]+)\](?:\([^)]*\)|\[[^\]]*\])?")
GO_DECL = re.compile(r"(func|type|var|const|package)\b")


def clean_desc(text: str) -> str:
    text = INLINE_TAG.sub(lambda m: m.group(1).split("#")[-1].strip(), text)
    text = re.sub(r"</?[A-Za-z][^>]*>", " ", text)  # Javadoc HTML
    return MD_LINK.sub(r"\1", text).strip()


def doc_blocks(src: str, lang: str):
    """-> [(start, end, description, tag lines, prefix)] for every doc comment of ``src``.

    ``prefix`` is the line-comment marker to rebuild the comment with, or None for a block."""
    out = []
    if lang in ("javascript", "typescript", "java", "rust"):
        for m in BLOCK.finditer(src):
            desc, tags = [], []
            for ln in m.group(0)[3:-2].split("\n"):
                ln = re.sub(r"^\*+ ?", "", ln.strip()).rstrip()
                (tags if ln.startswith("@") or tags else desc).append(ln)
            out.append((m.start(), m.end(), "\n".join(desc), tags, None))
    if lang in ("rust", "go"):
        lines = src.split("\n")
        offs = [0]
        for ln in lines:
            offs.append(offs[-1] + len(ln) + 1)
        mark = re.compile(r"^(\s*)(///?!?)(?!/)") if lang == "rust" else re.compile(r"^()(//)")
        i = 0
        while i < len(lines):
            m = mark.match(lines[i])
            if not m or (lang == "rust" and m.group(2) == "//"):
                i += 1
                continue
            j = i
            while j < len(lines) and (mk := mark.match(lines[j])) and mk.group(2) == m.group(2):
                j += 1
            if lang == "go" and not (j < len(lines) and GO_DECL.match(lines[j])):
                i = j
                continue
            text = "\n".join(mark.sub("", ln, count=1).removeprefix(" ") for ln in lines[i:j])
            out.append((offs[i], offs[j] - 1, text, [], m.group(1) + m.group(2) + " "))
            i = j
    return out


def rebuild(rest: str, tags: list[str], prefix: str | None) -> str:
    body = ([rest] if rest else []) + tags
    if prefix is not None:
        return "\n".join(prefix + ln for b in body for ln in b.split("\n"))
    body = [b.replace("*/", "* /") for b in body]
    return "/**\n" + "".join(f" * {b}\n" for b in body) + " */"


def process_file(src: str, rel: str, lang: str, with_targets: bool):
    """-> (variantA_src, variantB_src, targets)."""
    starts = {}
    for c in parse_source(rel, src):
        if c.kind in ("function", "method"):
            starts.setdefault(c.start, c)
    lines = src.split("\n")
    targets, editsA, editsB = [], [], []
    for a, b, desc, tags, prefix in doc_blocks(src, lang):
        nl = src.count("\n", a, b)
        desc = clean_desc(desc)
        first, rest = split_docstring(desc) if desc else ("", "")
        editsA.append((a, b, "\n" * nl))
        editsB.append((a, b, rebuild(rest, tags, prefix) if (rest or tags) else "\n" * nl))
        line = src.count("\n", 0, b) + 2  # 1-based line after the comment
        while lang == "rust" and line <= len(lines) and lines[line - 1].lstrip().startswith("#["):
            line += 1
        c = starts.get(line)
        if not (with_targets and c is not None and first):
            continue
        name = c.name
        if lang == "go" and first.startswith(name + " "):
            first = first[len(name) + 1:]
        if name == "constructor" or name.startswith("__") or name.startswith("test") \
                or SECTION_START.match(first):
            continue
        q = clean_rst(first).rstrip(".:;, ")
        if 4 <= len(q.split()) <= 30:
            targets.append({"name": name, "qualname": c.qualname, "query": q})

    def apply(edits):
        out, pos = [], 0
        for a, b, repl in sorted(edits):
            if a < pos:  # a `/** */` inside a `///` run: the run already covers it
                continue
            out.append(src[pos:a])
            out.append(repl)
            pos = b
        out.append(src[pos:])
        return "".join(out)

    return apply(editsA), apply(editsB), targets


def source_files(root: str, subs: list[str], exts: tuple[str, ...]):
    for sub in subs:
        p = os.path.join(root, sub)
        if os.path.isfile(p):
            yield p
            continue
        for dp, dns, fns in os.walk(p):
            dns[:] = sorted(d for d in dns if d not in ("node_modules", ".git", "vendor", "testdata"))
            for fn in sorted(fns):
                if fn.endswith(exts) and not fn.endswith((".min.js", ".d.ts")):
                    yield os.path.join(dp, fn)


def build_package(tag: str, rng: random.Random) -> dict:
    lang, _, _, subs, _ = CODE[tag]
    base = {v: os.path.join(BENCH, "src", v, tag) for v in ("A", "B")}
    for d in base.values():
        shutil.rmtree(d, ignore_errors=True)
    all_targets, nfiles = [], 0
    pkg_root = os.path.join(CODE_SRC, tag)
    for full in source_files(pkg_root, subs, LANG_EXT[lang]):
        rel = os.path.relpath(full, pkg_root)
        try:
            with open(full, encoding="utf8") as f:
                src = f.read()
        except (UnicodeDecodeError, OSError):
            continue
        if len(src) > 400_000:
            continue
        nfiles += 1
        flang = language_for(rel)
        a, b, targets = process_file(src, rel, "typescript" if flang in ("typescript", "tsx") else flang,
                                     with_targets=not is_test_path(rel))
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
    # Rust impl names carry generics (`Block<T>.write`); a user types `Block.write`
    add("qid", [(t, re.sub(r"<[^.]*>", "", ".".join(t["qualname"].split(".")[-2:])),
                 {"match": "qualname_suffix"})
                for t in all_targets if "." in t["qualname"]])

    os.makedirs(os.path.join(BENCH, "queries"), exist_ok=True)
    with open(os.path.join(BENCH, "queries", f"{tag}.json"), "w") as f:
        json.dump(queries, f)
    return {"pkg": tag, "files": nfiles, "documented_targets": len(all_targets),
            **Counter(q["type"] for q in queries)}


def fetch() -> None:
    for tag, (_, url, ref, _, _) in CODE.items():
        dst = os.path.join(CODE_SRC, tag)
        if not os.path.isdir(dst):
            subprocess.run(["git", "clone", "-q", "--depth", "1", "--branch", ref, url, dst], check=True)
        head = subprocess.run(["git", "-C", dst, "rev-parse", "HEAD"], capture_output=True, text=True).stdout
        print(tag, ref, head.strip(), flush=True)


def main() -> None:
    if sys.argv[1:] == ["fetch"]:
        return fetch()
    rng = random.Random(1234)
    for tag in sys.argv[1:] or list(CODE):
        print(build_package(tag, rng), flush=True)


if __name__ == "__main__":
    main()
