# Command-line reference

`scrutate --help` and `scrutate <command> --help` are authoritative; this page explains the model.

## Conventions

- **Exit codes**: `0` results found, `1` no results, `2` error (bad arguments, unreadable index,
  invalid pattern...), `130` interrupted (Ctrl-C, or stopped early for a reason other than
  `--max-results`).
- **Diagnostics**: errors are one line on stderr (`scrutate: error: ...`). With `--json`, or with
  `SCRUTATE_DIAGNOSTICS=json` for any command, an error is one JSON record instead:
  `{"type": "error", "schema": 1, "message": "..."}` (`"internal_error"` for a bug).
- **Streams**: results on stdout, diagnostics and progress on stderr. Output is plain `path:line:text`
  when stdout is not a terminal; colour only on a terminal (`--no-color` / `NO_COLOR` disable it).
- **Determinism**: `--sort` orders Engine A output by path then line (default output is in
  completion order for speed; `-c`, `-l`, `--symbols` are always sorted). Engine B output is fully
  deterministic (ties broken by id).
- **Machine-readable output**: `--json` on every command. Engine A emits JSON Lines with
  `"schema": 1` and `"type"` ∈ {`match`, `context`, `count`, `file`, `summary`, `symbol_group`};
  Engine B commands emit one JSON document. The schema number changes only on incompatible changes.
- **Paths**: relative to the searched root (a root that is a file is shown as given), `/`-separated on every platform.
- **Configuration**: `.scrutate.toml` in the repository, `$XDG_CONFIG_HOME/scrutate/config.toml`,
  or `$SCRUTATE_CONFIG`; `scrutate config` shows what is in effect. Precedence: command line >
  environment variable > configuration file > built-in default. Every configurable option names its
  variable in `--help` (e.g. `--hidden` ↔ `SCRUTATE_GREP_HIDDEN`).
- **Shell completion**: `scrutate completions bash|zsh|fish|powershell|elvish`.
- **Cancellation**: Ctrl-C stops a search between files and exits 130; a closed pipe
  (`| head`) stops it silently.

## Engine A — `grep` (alias `g`)

```
scrutate grep [OPTIONS] PATTERN [PATHS...]
```
`PATTERN` is a regex unless `-F` (literal), `--ident` (identifier in any casing) or `--fuzzy[=N]`
(within N edits, default 1, literal, at most 128 bytes). The default regex engine runs in linear
time and has no look-around or backreferences; `-P` (`--pcre2`) switches to PCRE2 for those. PCRE2
backtracks, so a pattern can take exponential time; its match limit stops the search of a file
then, and `scrutate` prints a warning naming that file. With `-P`, `--index` cannot narrow the
files and searches all of them. `-z` also searches inside zip and tar archives (members are shown as
`archive.zip{dir/file}`) and compressed files: gzip in process; bzip2, xz, zstd, lz4, brotli and
`.Z` through the system's tools, as ripgrep does. An archive or compressed file stops at 8 GiB
expanded or a million members. Archives inside archives are not opened, and `--index` does not
cover archive members. `-e PATTERN` (repeatable) gives several
patterns; a line matching any of them matches, and the first positional argument is then a path.
Options mirror grep/ripgrep where the meaning is identical: `-i`, `-S` (smart case), `-w`, `-U`
(multiline: each line of a multi-line match is printed with its own number), `-v`, `-l`, `-c`,
`-A/-B/-C`, `-m`, `--max-results`, `-g GLOB` (`!` excludes), `-t TYPE` / `--type-list`, `--hidden`,
`--no-ignore`, `-L` (follow links), `--max-filesize`, `--max-depth`, `-j THREADS`, `--sort`,
`--json`, `--stats`, `--symbols` (group lines by the enclosing function using the index, when one
exists). Files that cannot be read, or not to the end, are named in warnings on stderr (the first
ten); they do not change the exit code.

`--index` answers from Engine B's text index (`scrutate index`): the index names the files that
can contain a match (trigram postings), and Engine A's matcher searches only those, so the output
is what the walk prints for the indexed snapshot of `--root`. Run `scrutate index` (or `index
--text`) after changing files; the update is incremental. Options that change which files a walk
visits (`--hidden`, `--no-ignore`, `-L`, `--max-depth`, `--max-filesize`, explicit paths) are
refused with `--index`; `-g` and `-t` filter the indexed files.

## Engine B

| Command | Purpose |
|---|---|
| `index [--force] [--text \| --no-text]` | Build or incrementally update `.scrutate/`: the code-intelligence index and the text index for `grep --index` (`--text`: only the text index; `--no-text`: skip it). Progress on a terminal. |
| `search QUERY [-k N] [-a ALT]... [--also ROOT]... [--why] [--json]` (alias `s`) | Ranked search by meaning, name or identifier; `--alt` fuses alternative phrasings; `--also` searches other repositories too (paths prefixed with the repository name). |
| `find QUERY` | Unified: regex-looking → `grep`; otherwise ranked search (indexing a small repository on the spot); otherwise identifier grep. |
| `symbol NAME` | Where is it defined (`Class.method` works). |
| `callers NAME` / `callees NAME` | Call graph edges resolved to definitions (unresolved callee names listed as external). |
| `refs NAME [--max-results N]` | Definitions + callers + every whole-word occurrence grouped by enclosing symbol (works without an index: occurrences only). |
| `outline FILE` | Symbols of a file in source order. |
| `deps FILE` / `rdeps TARGET` | Imports of a file (resolved to repository files when possible) / files importing a module or file. |
| `similar TARGET [-k N]` | Code similar to a symbol or `path:line` (vector cosine + token Jaccard). |
| `dupes [--min-tokens N] [--threshold J] [--limit N]` | Exact and near-duplicate functions. |
| `tsquery LANG QUERY [PATHS] [--limit N] [--no-cache]` | Tree-sitter structural query over every file of a language (no index; per-file results are cached, see below). |
| `check [--files] [--json]` | Verify database integrity, segment consistency and (with `--files`) that indexed files are unchanged. Exit 1 on problems. |
| `stats` | Index statistics (files, chunks, symbols, call edges, imports, segments, and the text index's files, segments and bytes). |
| `languages [--json]` | The verified capability registry. |
| `watch [PATH]` | Keep the index (and the text index, if there is one) up to date from filesystem events (see below). |
| `serve [--refresh-interval S] [--watch]` | MCP server on stdio; `--watch` also runs the watcher in a background thread. |
| `lsp` | Language Server Protocol server on stdio (see below). |
| `config` | Show the configuration file in effect. |
| `batch FILE [--features] [--timing]` | Run queries from a JSON-lines file (benchmarks, parity tests). |

### `lsp`

```
scrutate lsp
```
A language server for editors (JSON-RPC 2.0 over stdin and stdout, `Content-Length` framing).
The repository is the editor's workspace folder (`rootUri` in `initialize`), or `--root` when the
editor sends none. It answers from the Engine B index:

| Request | Answer |
|---|---|
| `textDocument/definition` | definitions of the identifier under the cursor, as `symbol` finds them; with a qualifier (`Point.norm`, `a::b`), only the definitions whose qualified name matches, when there are any; definitions in the same file first |
| `textDocument/references` | what `refs` finds: definitions (left out when `includeDeclaration` is false) and every whole-word occurrence |
| `workspace/symbol` | symbols whose name contains the query (case and underscores ignored), exact and prefix matches first, at most 200 |
| `textDocument/documentSymbol` | the `outline` of the file, nested (methods under their class) |

It advertises only these four, plus full text sync. Open documents are read from the editor's
text, so an unsaved line still finds the word under the cursor; locations in other files come from
disk. `didSave` updates the index incrementally on a second connection in a background thread,
so requests keep being answered meanwhile. When the repository has no index, `initialize` builds
one (as `scrutate index` does, without the text index) and reports it with `window/logMessage`;
requests that arrive meanwhile wait for it. Columns are UTF-16 code units, as the protocol
requires. The exit code is 0 after `shutdown` then `exit`, 1 otherwise. Logs go to stderr.

### `watch`
```
scrutate watch [PATH]
```
Updates the index of `PATH` (default `--root`) once, prints `watching PATH` on stderr, then waits
for filesystem events (inotify on Linux, ReadDirectoryChangesW on Windows; on macOS it polls every
2 seconds, because the FSEvents backend would link CoreServices into every `scrutate` command and
cost about 1 ms of start-up). Events under `.scrutate/` and `.git/` are dropped, and so are paths that
neither index would hold under its usual rules (ignore files; the skipped build and vendor
directories and `SCRUTATE_INDEX_EXCLUDE_DIRS` for the code index; hidden paths for the text
index). A burst of events is collected until 200 ms pass without one (at
most 2 s), then the usual incremental update runs: the code index, and the text index when
`.scrutate/text/` exists (`watch` never creates one; `scrutate index` does). Each update that
changed something prints one line on stderr. Ctrl-C stops it after the update in progress (exit
130). Other processes can read and update the same index meanwhile; the indexes' own locks order
the writers, and stopping the process mid-update leaves the previous state in place.

`serve --watch` runs the same watcher in a background thread while the server answers requests.
The watcher and the tool calls share one engine, so a code index update and a request never run at
the same time in that process.

### `tsquery` cache
Each file's captures are kept in `--root`/.scrutate/tsquery/, one file per (scrutate version,
language, query text, searched paths), keyed by a hash of the file's content. A file whose content
was seen before is not parsed again; the output is byte-identical to `--no-cache`, which parses everything and
leaves the cache alone. A run that visits every file keeps only the entries it used, so edited and
deleted files do not accumulate. The directory is bounded at 128 MB: beyond it the least recently
used query files are deleted, and a query whose results alone exceed it is not cached. The cache
is safe to delete at any time.

### Environment variables
`SCRUTATE_ROOT`, `SCRUTATE_THREADS`, `SCRUTATE_GREP_{HIDDEN,NO_IGNORE,FOLLOW,SMART_CASE,MAX_FILESIZE,MAX_DEPTH,GLOBS,TYPES}`,
`SCRUTATE_INDEX_EXCLUDE_DIRS`, `SCRUTATE_REFRESH_INTERVAL`, `SCRUTATE_CONFIG`, `SCRUTATE_SEMANTIC`
(path or `none`), `SCRUTATE_RANKER` (path or `none`), `SCRUTATE_PROFILE` (print indexing phase
timings), `SCRUTATE_DIAGNOSTICS` (`json`: errors as JSON records), `NO_COLOR`.

### The index directory
`.scrutate/index.db` (SQLite, WAL), `.scrutate/seg/*/` (memory-mapped arrays) and
`.scrutate/text/` (the text index: a manifest and immutable trigram segments),
`.scrutate/tsquery/` (the `tsquery` cache), plus a `.gitignore`
so it is never committed. It is a cache: deleting it is always safe; `scrutate index
--force` rebuilds it; a damaged index rebuilds itself.
