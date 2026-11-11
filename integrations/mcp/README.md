# Scrutate as an MCP server

`scrutate serve` speaks the Model Context Protocol over stdio (JSON-RPC 2.0): the handshake
revisions 2024-11-05 / 2025-03-26 / 2025-06-18 / 2025-11-25, and revision 2026-07-28, which has
no handshake (`server/discover`, a protocol version in every request's `_meta`). Tools: `search_code`, `grep_code`,
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
- `grep_code` at the speed of `scrutate grep`, with matches grouped by enclosing symbol;
- a location (`path:start-end`) at the start of every result, accepted wherever a tool takes a
  symbol, and `read_code symbol=...` for a whole definition by name or location;
- with `serve --structured`, `structuredContent` and an `outputSchema` on every tool: the answer
  as the JSON that `--json` prints. Leave it off for Claude Code, which then shows its model the
  JSON in place of the text with the snippets; turn it on for a client whose scripts read tool
  results (Codex in Code Mode);
- cancellation (`notifications/cancelled`) and, for a call that sends a `progressToken`,
  progress reports while the first index is built.

## Client configuration

Each client has its own file and its own key for the list of servers. Replace `/path/to/repo`;
Cursor and VS Code fill in `${workspaceFolder}` themselves.

Claude Code (the command, or the same as `.mcp.json` in the repository):
```sh
claude mcp add scrutate -- scrutate --root /path/to/repo serve
```
```json
{ "mcpServers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"] } } }
```

Codex CLI (`codex mcp add scrutate -- scrutate --root /path/to/repo serve`, or `~/.codex/config.toml`):
```toml
[mcp_servers.scrutate]
command = "scrutate"
args = ["--root", "/path/to/repo", "serve"]
```

Cursor (`.cursor/mcp.json`):
```json
{ "mcpServers": { "scrutate": { "type": "stdio", "command": "scrutate", "args": ["--root", "${workspaceFolder}", "serve"] } } }
```

VS Code (`.vscode/mcp.json`; the key is `servers`):
```json
{ "servers": { "scrutate": { "type": "stdio", "command": "scrutate", "args": ["--root", "${workspaceFolder}", "serve"] } } }
```

Gemini CLI (`.gemini/settings.json`):
```json
{ "mcpServers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"] } } }
```

Zed (`settings.json`; the key is `context_servers`):
```json
{ "context_servers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"], "env": {} } } }
```

Tested in a running session: Claude Code. Checked through the client's own command: Codex CLI
(`codex mcp add` writes the table above, `codex mcp list` shows it enabled) and VS Code
(`code --add-mcp` writes the `servers` entry above). Cursor, Gemini CLI and Zed follow their
documentation as of November 2026 and were not started here.

The server never leaves the machine: it reads the repository, writes only `.scrutate/` inside it,
opens no network connections and executes no repository code.
