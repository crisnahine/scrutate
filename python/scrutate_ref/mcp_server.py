"""MCP server: exposes Titan search tools to Claude Code and other MCP clients.

    claude mcp add scrutate-ref -- python -m scrutate_ref --root /path/to/repo serve
"""
from __future__ import annotations

import functools
import os
import threading

try:  # mcp >= 2 renamed FastMCP -> MCPServer
    from mcp.server.mcpserver import MCPServer as _Server
except ImportError:  # mcp 1.x
    from mcp.server.fastmcp import FastMCP as _Server

from .agent import fmt_hits, run_search
from .index import Index
from .search import Searcher


def build(root: str, refresh_interval: float = 1.0, warm: bool = True):
    """MCP server for `root`. The first indexing runs in the background so the handshake is
    instant; tool calls that arrive meanwhile wait for it. Before every query the index picks up
    files edited since the previous one (at most one directory scan per `refresh_interval`, and on
    a small repository, where a scan costs milliseconds, much more often; see Index.refresh)."""
    root = os.path.abspath(root)
    ix = Index(root)
    s = Searcher(ix)
    mcp = _Server("scrutate-ref")
    lock = threading.RLock()  # the SDK runs sync tools in worker threads; SQLite access is serialized

    def tool(fn, fresh=True):
        @functools.wraps(fn)
        def locked(*a, **kw):
            with lock:
                if fresh:
                    ix.refresh(refresh_interval)
                return fn(*a, **kw)

        return mcp.tool()(locked)

    if warm:
        def first_index():
            with lock:
                ix.refresh(0)

        threading.Thread(target=first_index, daemon=True, name="scrutate-index").start()

    @tool
    def search_code(query: str, alt_queries: list[str] | None = None, k: int = 8) -> str:
        """Search the codebase by meaning or by name. Returns the best matching functions/classes
        with file:line and a snippet. `query` may be natural language ("where do we refresh
        expired tokens") or identifiers (`ClientSession.call_tool`). To improve recall put
        synonyms and likely identifier names in `alt_queries` (e.g. ["renew credentials",
        "refresh_token"]); results of all phrasings are fused."""
        return run_search(s, query, alt_queries, k)

    @tool
    def grep_code(pattern: str) -> str:
        """Exact regex search, results grouped by the enclosing function/class."""
        return fmt_hits(s.grep(pattern))

    @tool
    def find_symbol(name: str) -> str:
        """Where is this function/class/method defined? (`Class.method` works)."""
        return fmt_hits(s.find_symbol(name))

    @tool
    def callers(name: str) -> str:
        """Which functions call this function?"""
        return fmt_hits(s.callers(name), snippet=False)

    @tool
    def callees(name: str) -> str:
        """Which functions does this function call?"""
        res, ext = s.callees(name)
        return fmt_hits(res, snippet=False) + ("\nexternal: " + ", ".join(ext[:40]) if ext else "")

    def read_code(path: str, start: int = 1, end: int | None = None) -> str:
        """Read a line range from a file in the repo."""
        return s.read(path, start, end)

    tool(read_code, fresh=False)

    def reindex() -> str:
        """Force a re-scan for changed files (searches already do this automatically)."""
        st = ix.refresh(0)
        return f"{st.files_changed} files changed, {st.files_removed} removed, {st.chunks} chunks"

    tool(reindex, fresh=False)

    return mcp


def serve(root: str) -> None:
    build(root).run()
