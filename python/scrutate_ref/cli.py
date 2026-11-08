from __future__ import annotations

import argparse
import json
import os
import sys

from .agent import fmt_hits, query_words


def _open(root: str, auto: bool = True):
    from .index import Index
    from .search import Searcher

    ix = Index(root)
    if auto:
        st = ix.update()
        if st.files_changed or st.files_removed:
            print(f"[scrutate-ref] indexed {st.files_changed} changed files "
                  f"({st.chunks} chunks, {st.seconds:.1f}s)", file=sys.stderr)
    return ix, Searcher(ix)


def main(argv: list[str] | None = None) -> None:
    p = argparse.ArgumentParser(prog="scrutate-ref", description="Hybrid AI code search")
    p.add_argument("--root", default=".", help="repository root (default: .)")
    sub = p.add_subparsers(dest="cmd", required=True)

    sub.add_parser("index", help="(re)build the index").add_argument("--force", action="store_true")
    s = sub.add_parser("search", help="search by meaning or name")
    s.add_argument("query")
    s.add_argument("-a", "--alt", action="append", default=[], help="alternative phrasing (repeatable)")
    s.add_argument("-k", type=int, default=8)
    s.add_argument("--why", action="store_true", help="show why each result matched")
    s.add_argument("--json", action="store_true")
    g = sub.add_parser("grep", help="regex search grouped by enclosing symbol")
    g.add_argument("pattern")
    for n in ("symbol", "callers", "callees"):
        sub.add_parser(n).add_argument("name")
    a = sub.add_parser("ask", help="agent answers a question (needs ANTHROPIC_API_KEY)")
    a.add_argument("question")
    a.add_argument("-v", "--verbose", action="store_true")
    sub.add_parser("serve", help="run the MCP server on stdio")
    sub.add_parser("stats")

    args = p.parse_args(argv)
    root = os.path.abspath(args.root)

    if args.cmd == "index":
        from .index import Index

        st = Index(root).update(force=args.force)
        print(f"{st.files_total} files, {st.files_changed} changed, {st.chunks} chunks, "
              f"semantic={st.semantic}, {st.seconds:.1f}s")
    elif args.cmd == "stats":
        ix, _ = _open(root)
        print(ix.stats())
    elif args.cmd == "search":
        _, sr = _open(root)
        qs = [args.query] + args.alt
        hits = sr.search(args.query, k=args.k) if not args.alt else sr.search_many(qs, k=args.k)
        if args.json:
            print(json.dumps([{"path": h.path, "start": h.start, "end": h.end, "name": h.qualname or h.name,
                               "kind": h.kind, "score": h.score, "why": h.layers} for h in hits], indent=2))
        else:
            words = [w for q in qs for w in query_words(q)]
            print(fmt_hits(hits, words, why=args.why))
    elif args.cmd == "grep":
        _, sr = _open(root)
        print(fmt_hits(sr.grep(args.pattern)))
    elif args.cmd == "symbol":
        _, sr = _open(root)
        print(fmt_hits(sr.find_symbol(args.name)))
    elif args.cmd == "callers":
        _, sr = _open(root)
        print(fmt_hits(sr.callers(args.name), snippet=False))
    elif args.cmd == "callees":
        _, sr = _open(root)
        res, ext = sr.callees(args.name)
        print(fmt_hits(res, snippet=False))
        print("external:", ", ".join(ext[:50]))
    elif args.cmd == "ask":
        from .agent import ask

        print(ask(root, args.question, verbose=args.verbose))
    elif args.cmd == "serve":
        from .mcp_server import serve

        serve(root)


if __name__ == "__main__":
    main()
