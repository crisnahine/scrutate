# Scrutate as an MCP server

`scrutate serve` speaks the Model Context Protocol over stdio (JSON-RPC 2.0, protocol versions
2024-11-05 / 2025-03-26 / 2025-06-18 / 2025-11-25). Tools: `search_code`, `grep_code`,
`find_symbol`, `callers`, `callees`, `read_code`, `outline`, `dependencies`, `dependents`,
`references`, `similar_code`, `duplicates`, `structural_search`, `reindex`. The index refreshes
itself between calls (at most once per `--refresh-interval` seconds), so edits made by the agent
are searchable immediately.

What a client gets (details in docs/CLI.md, `serve`):
- every tool except `reindex` is marked read-only and local, so clients that ask before a write
  do not ask for these;
- server instructions that say which tool to start with;
- answers of at most 24,000 bytes by default (`max_bytes`), cut at whole items, with the
  `offset` that continues them;
- `grep_code` at the speed of `scrutate grep`, with matches grouped by enclosing symbol.

Claude Code:
```sh
claude mcp add scrutate -- scrutate --root /path/to/repo serve
```

Generic MCP client configuration (Cursor, Windsurf, JetBrains AI Assistant, ...):
```json
{ "mcpServers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"] } } }
```

The server never leaves the machine: it reads the repository, writes only `.scrutate/` inside it,
opens no network connections and executes no repository code.
