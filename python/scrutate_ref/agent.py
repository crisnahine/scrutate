"""Agent loop: Claude drives the search tools, reads code, and answers with citations."""
from __future__ import annotations

import json
import os

from .index import Index
from .search import Searcher
from .text import raw_subwords, words

SYSTEM = """You are a code-intelligence agent answering questions about a codebase.
Work like a senior engineer: search broadly, read the real code, follow call chains
(callers/callees), and verify before concluding. The search tool accepts several alternative
phrasings (synonyms, likely identifiers) in `alt_queries` -- use them, they are fused. Prefer
several focused searches over one vague one. Cite every claim as path:line. If you cannot find
something, say so rather than guessing."""

TOOLS = [
    {"name": "search",
     "description": "Hybrid code search (natural language or identifiers). Put synonyms / likely "
                    "identifier names in alt_queries to improve recall.",
     "input_schema": {"type": "object", "properties": {
         "query": {"type": "string"}, "alt_queries": {"type": "array", "items": {"type": "string"}},
         "k": {"type": "integer", "default": 8}}, "required": ["query"]}},
    {"name": "grep", "description": "Exact regex search (ripgrep) mapped to enclosing symbols.",
     "input_schema": {"type": "object", "properties": {"pattern": {"type": "string"}}, "required": ["pattern"]}},
    {"name": "find_symbol", "description": "Find definitions of a function/class/method by name (Class.method ok).",
     "input_schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}},
    {"name": "callers", "description": "Chunks that call the named function.",
     "input_schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}},
    {"name": "callees", "description": "Functions called by the named function.",
     "input_schema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}},
    {"name": "read_file", "description": "Read a line range of a file (1-based, inclusive).",
     "input_schema": {"type": "object", "properties": {"path": {"type": "string"}, "start": {"type": "integer"},
                                                       "end": {"type": "integer"}}, "required": ["path"]}},
]


def fmt_hits(hits, words=None, snippet=True, why=False) -> str:
    if not hits:
        return "(no results)"
    out = []
    for h in hits:
        head = f"{h.loc}  [{h.kind}] {h.qualname or h.name}"
        if why and h.layers:
            head += "  (" + ", ".join(f"{k}" for k in h.layers) + ")"
        out.append(head + ("\n" + "\n".join("    " + l for l in h.snippet(words).split("\n")) if snippet else ""))
    return "\n".join(out)


def query_words(q: str) -> list[str]:
    return [w.lower() for w in words(q) if len(w) > 2]


def run_search(s: Searcher, query: str, alt_queries=None, k: int = 8, why: bool = False) -> str:
    qs = [query] + [a for a in (alt_queries or []) if a and a != query]
    hits = s.search(query, k=k) if len(qs) == 1 else s.search_many(qs, k=k)
    words = [w for q in qs for w in query_words(q)]
    return fmt_hits(hits, words, why=why)


def run_tool(s: Searcher, name: str, args: dict) -> str:
    try:
        if name != "read_file":
            s.ix.refresh()  # pick up files the model (or the user) edited since the last call
        if name == "search":
            return run_search(s, args["query"], args.get("alt_queries"), int(args.get("k", 8)))
        if name == "grep":
            return fmt_hits(s.grep(args["pattern"]))
        if name == "find_symbol":
            return fmt_hits(s.find_symbol(args["name"]))
        if name == "callers":
            return fmt_hits(s.callers(args["name"]), snippet=False)
        if name == "callees":
            res, ext = s.callees(args["name"])
            return fmt_hits(res, snippet=False) + ("\nexternal/unresolved: " + ", ".join(ext[:40]) if ext else "")
        if name == "read_file":
            return s.read(args["path"], int(args.get("start", 1)), args.get("end"))
    except Exception as e:  # tool errors go back to the model
        return f"error: {e}"
    return f"error: unknown tool {name}"


def ask(root: str, question: str, max_turns: int = 14, verbose: bool = False, client=None) -> str:
    if client is None:
        import anthropic

        if not os.environ.get("ANTHROPIC_API_KEY"):
            raise SystemExit("ANTHROPIC_API_KEY is not set; `scrutate-ref ask` needs it (search works without).")
        client = anthropic.Anthropic()
    ix = Index(root)
    ix.update()
    s = Searcher(ix)
    model = os.environ.get("SCRUTATE_MODEL", "claude-sonnet-5-5")
    msgs = [{"role": "user", "content": question}]
    for _ in range(max_turns):
        r = client.messages.create(model=model, max_tokens=4096, system=SYSTEM, tools=TOOLS, messages=msgs)
        msgs.append({"role": "assistant", "content": r.content})
        calls = [b for b in r.content if b.type == "tool_use"]
        if not calls:
            return "".join(b.text for b in r.content if b.type == "text")
        results = []
        for c in calls:
            if verbose:
                print(f"  -> {c.name}({json.dumps(c.input)})")
            results.append({"type": "tool_result", "tool_use_id": c.id,
                            "content": run_tool(s, c.name, c.input)[:12000]})
        msgs.append({"role": "user", "content": results})
    return "(stopped: turn limit reached)"
