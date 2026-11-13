<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="../../docs/assets/banner-mcp.svg" alt="MCP server. How do I give an agent scrutate's tools?" width="880">
</p>

# Scrutate as an MCP server

[Documentation](../../docs/README.md) &nbsp;·&nbsp; Previous: [Command line](../../docs/CLI.md) &nbsp;·&nbsp; Next: [VS Code](../vscode/README.md)

<!-- End of head. -->

`scrutate serve` speaks the Model Context Protocol over stdio (JSON-RPC 2.0): the handshake
revisions 2024-11-05 / 2025-03-26 / 2025-06-18 / 2025-11-25, and revision 2026-07-28, which has
no handshake (`server/discover`, a protocol version in every request's `_meta`). Tools: `search_code`, `grep_code`,
`find_symbol`, `callers`, `callees`, `call_path`, `repo_map`, `read_code`, `outline`, `dependencies`, `dependents`,
`references`, `similar_code`, `duplicates`, `structural_search`, `impact`, `find_files`, `dead_code`, `reindex`. The index refreshes
itself before a call: by default the wait between scans is at most a second, shorter when a scan
is cheap; with `--refresh-interval S`, no scan comes sooner than S seconds after the last one.
Inside that window an answer checks the files it cites and the files its arguments name, and
reads a changed one, or a new one an argument names, first; an edit to another file waits for
the next scan. A call waits up to 20 seconds for a first index build, also one that another
process runs.

What a client gets (details in docs/CLI.md, `serve`):
- every tool except `reindex` is marked read-only and local, so clients that ask before a write
  do not ask for these;
- server instructions that say which tool to start with;
- answers of at most 24,000 bytes by default (`max_bytes`, or `serve --max-bytes` for another
  default), closing line included, cut at whole items, with the `offset` that continues them;
- `grep_code` at the speed of `scrutate grep`, with matches grouped by enclosing symbol; it
  searches the files the index holds: hidden ones such as `.github/`, not ignored files, `.git/`
  or `node_modules/`; a capped answer holds the first matches in path order, so the next page
  neither repeats nor skips a line (`references` too);
- `reindex` with `force: true` to read every file again, also one whose size and modification
  time did not change;
- a location (`path:start-end`) at the start of every result, accepted wherever a tool takes a
  symbol, and `read_code symbol=...` for a whole definition by name or location;
- `read_code path=...` reads only as far as the last line asked for, at most 1 MB of line text,
  and refuses a file with a NUL byte in its first 8 KB;
- with `serve --structured`, `structuredContent` and an `outputSchema` on every tool: the answer
  as the JSON that `--json` prints. Leave it off for Claude Code, which then shows its model the
  JSON in place of the text with the snippets; turn it on for a client whose scripts read tool
  results (Codex in Code Mode);
- cancellation (`notifications/cancelled`), which also stops an index update the call runs
  (`reindex`, or the scan before a read) and rolls it back, and, for a call that sends a
  `progressToken`, progress reports while the first index is built.

## Client configuration

`scrutate setup` writes the entry into the agent's own file and keeps every other line of it:

```sh
scrutate setup claude      # ~/.claude.json, through `claude mcp add-json` when claude is installed
scrutate setup codex       # $CODEX_HOME/config.toml: [mcp_servers.scrutate]
scrutate setup opencode    # ~/.config/opencode/opencode.json (or .jsonc)
scrutate setup cursor      # ~/.cursor/mcp.json
scrutate setup gemini      # ~/.gemini/settings.json
scrutate setup cursor --project            # the repository's .cursor/mcp.json (.mcp.json for Claude Code)
scrutate setup claude --uninstall          # takes out what setup added (hook included), byte for byte
```

The entry runs `scrutate` by its absolute path with `serve`, so the server serves the repository
the agent starts in; a `--project` entry names its repository with `--root`. `--dry-run` shows the
change, `--hook` also answers Claude Code's Grep with scrutate's search, and `--instructions` adds a
short block to `CLAUDE.md`, `AGENTS.md` or `GEMINI.md`. The files, the shapes and the rules are in
[docs/CLI.md](../../docs/CLI.md#setup-and-the-grep-hook). Gemini CLI starts a stdio server only in a
folder you trust.

By hand, the entries look like this (replace `/path/to/repo`; Cursor and VS Code fill in
`${workspaceFolder}` themselves). Start `scrutate serve` with `--root DIR` (or `SCRUTATE_ROOT`),
or from inside a repository: started with no root in the home directory or at a filesystem root
that has no index, it stops with exit code 2 and indexes nothing.

Claude Code (`.mcp.json` in the repository):
```json
{ "mcpServers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"] } } }
```

Codex CLI (`~/.codex/config.toml`):
```toml
[mcp_servers.scrutate]
command = "scrutate"
args = ["--root", "/path/to/repo", "serve"]
```

VS Code (`.vscode/mcp.json`; the key is `servers`):
```json
{ "servers": { "scrutate": { "type": "stdio", "command": "scrutate", "args": ["--root", "${workspaceFolder}", "serve"] } } }
```

Zed (`settings.json`; the key is `context_servers`):
```json
{ "context_servers": { "scrutate": { "command": "scrutate", "args": ["--root", "/path/to/repo", "serve"], "env": {} } } }
```

Tested in a running session: Claude Code. Checked through each client's own listing after
`scrutate setup`, in a scratch home (2026-11-13): Claude Code 2.1.323 (`claude mcp list`:
Connected), Codex CLI 0.155.1 (`codex mcp list`: enabled), cursor-agent 2026.10.22 (`mcp list`:
ready), opencode 1.19.5 and 2.2.7 (`opencode mcp list`: connected) and Gemini CLI 0.68.0 (`gemini
mcp list`: Connected in a trusted folder). VS Code (`code --add-mcp` writes the `servers` entry
above) was checked through its own command; Zed follows its documentation as of November 2026 and
was not started here.

The server never leaves the machine: it reads the repository, writes only `.scrutate/` inside it
and the frecency file of `find_files` under `$XDG_STATE_HOME/scrutate/`, opens no network connections and executes no repository code.
