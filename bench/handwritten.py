#!/usr/bin/env python3
"""Anecdotal sanity check: 16 human-style questions about the (unmodified) mcp package.

Written by hand before the learned ranker existed; n is small, so read it as a smoke test,
not as evidence. usage: handwritten.py /path/to/mcp-source-dir-parent
"""
import os
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
import numpy as np  # noqa: E402

from scrutate_ref.index import Index  # noqa: E402
from scrutate_ref.search import FI, Searcher  # noqa: E402

CASES = [
    ("refresh an expired oauth access token", ["_refresh_token", "_handle_refresh_response", "can_refresh_token"]),
    ("parse one line of subprocess stdout into a jsonrpc message", ["_parse_line"]),
    ("work out when a token expires", ["calculate_token_expiry", "update_token_expiry", "is_token_valid"]),
    ("register a new tool with the server", ["add_tool"]),
    ("check a tool result against its declared output schema", ["validate_tool_result"]),
    ("liveness check to see if the peer is still responding", ["send_ping"]),
    ("enumerate every resource the server exposes", ["list_resources"]),
    ("stop receiving notifications about a resource", ["unsubscribe_resource"]),
    ("change how verbose the server logs are", ["set_logging_level"]),
    ("initial handshake that negotiates protocol version", ["initialize"]),
    ("delete a tool from the server by name", ["remove_tool"]),
    ("format a message as a server sent event frame", ["_sse_event"]),
    ("handle an HTTP POST on the streamable http client", ["_handle_post_request"]),
    ("callers of send_request", ["send_ping", "initialize", "list_resources"]),
    ("StreamableHTTPTransport._handle_sse_event", ["_handle_sse_event"]),
    ("where does the stdio server read incoming bytes", ["stdin_reader", "stdio_server"]),
]


def ok(h, targets):
    q = h.qualname or h.name
    return any(q == t or q.endswith("." + t) for t in targets)


def main(root):
    ix = Index(root)
    ix.update()
    s = Searcher(ix)
    res = {"BM25 (field-weighted)": [], "Titan": []}
    for query, targets in CASES:
        qi, rows, X = s.candidates(query)
        for name, scores in (("BM25 (field-weighted)", X[:, FI["lex_log"]]), ("Titan", s.score(X))):
            hits = s._hits(qi, rows, X, scores, 10, with_text=False)
            r = next((i for i, h in enumerate(hits, 1) if ok(h, targets)), None)
            res[name].append(r)
    for name, ranks in res.items():
        mrr = np.mean([1 / r if r else 0 for r in ranks])
        print(f"{name:<24} MRR {mrr:.3f}  hit@1 {np.mean([r == 1 for r in ranks]):.2f}  "
              f"hit@5 {np.mean([bool(r and r <= 5) for r in ranks]):.2f}  found {sum(bool(r) for r in ranks)}/{len(ranks)}")
    for (q, _), a, b in zip(CASES, res["BM25 (field-weighted)"], res["Titan"]):
        print(f"  bm25 {a or '-':>2}  scrutate {b or '-':>2}   {q}")


if __name__ == "__main__":
    main(sys.argv[1])
