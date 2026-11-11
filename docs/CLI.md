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
(within N edits, default 1, literal, at most 4096 bytes). The default regex engine runs in linear
time and has no look-around or backreferences; `-P` (`--pcre2`) switches to PCRE2 for those. PCRE2
backtracks, so a pattern can take exponential time; its match limit stops the search of a file
then, and `scrutate` prints a warning naming that file. With `-P`, `--index` cannot narrow the
files and searches all of them. `--fuzzy-regex[=N]` matches a regex within N edits
(default 1): byte-oriented, ASCII classes, no anchors or word boundaries, at most 64 character
positions (a bounded repeat counts each copy), and it must not match the empty string. `-z` also searches inside zip and tar archives (members are shown as
`archive.zip{dir/file}`) and compressed files: gzip in process; bzip2, xz, zstd, lz4, brotli and
`.Z` through the system's tools, as ripgrep does. An archive or compressed file stops at 8 GiB
expanded or a million members. Archives inside archives are not opened, and `--index` does not
cover archive members. `-e PATTERN` (repeatable) gives several
patterns; a line matching any of them matches, and the first positional argument is then a path.
Options mirror grep/ripgrep where the meaning is identical: `-i`, `-S` (smart case), `-w`, `-U`
(multiline: each line of a multi-line match is printed with its own number), `-v`, `-l`, `-c`,
`-A/-B/-C`, `-m`, `--max-results`, `-g GLOB` (`!` excludes), `-t TYPE` / `--type-list`, `--hidden`,
`--no-ignore`, `-L` (follow links), `--max-filesize`, `--max-depth`, `-j THREADS` (default: the
cores, at most 12; on macOS the performance cores; more when the files come from the disk, a
cold cache: up to 64 on Linux, elsewhere twice the cores, at most 24), `--sort`,
`--json`, `--stats`, `--symbols` (group lines by the enclosing function using the index, when one
exists). Files that cannot be read, or not to the end, are named in warnings on stderr (the first
ten); they do not change the exit code.

`--index` answers from Engine B's text index (`scrutate index`): the index names the files that
can contain a match (trigram postings), and Engine A's matcher searches only those, so the output
is what the walk prints for the indexed snapshot of `--root`. Run `scrutate index` (or `index
--text`) after changing files; the update is incremental. Options that change which files a walk
visits (`--hidden`, `--no-ignore`, `-L`, `--max-depth`, `--max-filesize`, explicit paths) are
refused with `--index`; `-g` and `-t` filter the indexed files. `--index --update` first brings
the text index up to date (building it if there is none; only new and changed files are read),
so the answer is the walk's for the tree as it is now: the call for an editor that searches
often while files change.

## Engine B

| Command | Purpose |
|---|---|
| `index [--force] [--text \| --no-text]` | Build or incrementally update `.scrutate/`: the code-intelligence index and the text index for `grep --index` (`--text`: only the text index; `--no-text`: skip it). Progress on a terminal. |
| `search QUERY [-k N] [-a ALT]... [--also ROOT]... [--why] [--json]` (alias `s`) | Ranked search by meaning, name or identifier; `--alt` fuses alternative phrasings; `--also` searches other repositories too (paths prefixed with the repository name). |
| `find QUERY` | Unified: regex-looking → `grep`; otherwise ranked search (indexing a small repository on the spot); otherwise identifier grep. |
| `symbol NAME` | Where is it defined (`Class.method` works). |
| `callers NAME` / `callees NAME` | Call graph edges resolved to definitions (callee names outside the repository listed after `external:`). |
| `refs NAME [--def PATH[:LINE]] [--textual] [--max-results N]` | Every whole-word occurrence, each resolved to the definition it refers to, or listed as unresolved with a reason (see below). |
| `outline FILE` | Symbols of a file in source order. |
| `deps FILE` / `rdeps TARGET` | Imports of a file (resolved to repository files when possible) / files importing a module or file. |
| `similar TARGET [-k N]` | Code similar to a symbol or `path:line` (vector cosine + token Jaccard). |
| `dupes [--min-tokens N] [--threshold J] [--limit N]` | Exact and near-duplicate functions. |
| `tsquery LANG QUERY [PATHS] [--limit N] [--no-cache]` | Tree-sitter structural query over every file of a language (no index; per-file results are cached, see below). |
| `check [--files] [--json]` | Verify database integrity, segment consistency and (with `--files`) that indexed files are unchanged. Exit 1 on problems. |
| `stats` | Index statistics (files, chunks, symbols, call edges, imports, segments, and the text index's files, segments and bytes). |
| `languages [--json]` | The verified capability registry. |
| `watch [PATH]` | Keep the index (and the text index, if there is one) up to date from filesystem events (see below). |
| `serve [--refresh-interval S] [--watch]` | MCP server on stdio (see below); `--watch` also runs the watcher in a background thread. |
| `lsp` | Language Server Protocol server on stdio (see below). |
| `config` | Show the configuration file in effect. |
| `batch FILE [--features] [--timing]` | Run requests from a JSON-lines file with one index open. A line `{"query": ..., "k": ...}` is a ranked search; a line `{"op": TOOL, ...}` runs any operation, named and given arguments as the MCP tool is (`{"op": "callers", "name": "parse"}`), and prints `{"op", "result"}` or `{"op", "error"}`. |

### `refs`
```
scrutate refs NAME [--def PATH[:LINE]] [--textual] [--max-results N] [--json]
```
Finds every whole-word occurrence of `NAME` (as `grep -w`) and decides which definition each one
refers to. For Python, JavaScript, TypeScript, Rust and Go the rules are, in order:

1. An occurrence that is the name of a definition, or the name in an import statement, is that
   definition (or what the import resolves to).
2. A qualified occurrence (`obj.N`, `a::b::N`, `pkg.N`) is looked up in what the qualifier names:
   a module bound by an import (`import x`, `import * as x`, `use crate::a`, a Go import), a
   module path (`crate::`, `self::`, `super::`, a workspace crate, a Rust child module), or a
   class or type (`Class.N`, `Type::N`, then its Python and JavaScript base classes). `self.N`,
   `this.N`, `cls.N` and `Self::N` look in the enclosing class or impl. A receiver that is a
   local variable or a parameter (`x.N`) is looked up in the type its declaration states: an
   annotation (`x: T`, `x T`, `x: &T`) or what it is built from (`T(...)`, `new T(...)`,
   `T::new(...)`, `T { .. }`, `&T{}`), the nearest declaration before the use. A Go method is
   found by its receiver type in any file of the package. A variable whose declaration does not
   say its type is unresolved.
3. A plain occurrence is, first, a definition nested in an enclosing function (or directly in an
   enclosing class body); a parameter or local variable of that name hides everything else;
   then a top-level definition of the file; then a name an import binds (`from x import N`,
   `import {N} from './x'`, `use a::N`, `require`); then, in Go, a top-level definition of another
   file of the same package directory; then wildcard imports (`from x import *`, `export * from`,
   `use a::*`, Go dot imports).

Import paths resolve to repository files: Python relative imports, packages (`__init__.py`) and
a repository root inside the package; JavaScript relative paths with the usual extensions and
`index` files (a `.js` import finds the `.ts` source), and the names a project gives its own
folders in `tsconfig.json` or `jsconfig.json` (`paths`, `baseUrl`); Rust module files (`a.rs`, `a/mod.rs`) from
the crate root, workspace crates by their `Cargo.toml` name; Go import paths by the directory
they end with. Re-exports (`from .x import N` in `__init__.py`, `export {N} from`, `pub use`) are
followed. Occurrences in comments and strings, in other languages and in files without a
grammar are unresolved, and so is anything the rules do not reach (method calls on a variable
that got its value from a function call or a field, enum variants, local variables). Imports of modules outside the repository give `external`.

The output lists each definition with its references, then the unresolved occurrences with the
reason. `--def PATH[:LINE]` keeps one definition of `NAME`: the one in `PATH`, or the one
containing `LINE` when the file has several; no such definition is an error. `--textual` gives the
behaviour without resolution: definitions, callers and every occurrence grouped by enclosing
symbol (it never misses anything, and works without an index). With `--json`, each definition has
a `references` list (`path`, `line`, `column` in bytes, `text`, `via`) and the document has an
`unresolved` list with a `reason` (left out with `--def`). Resolution reads the index's
definitions and parses the files that hold occurrences again, so it needs no index change. The
MCP `references` tool gives the same answer (`textual` and `def` are arguments there).

### Answers from the index: one definition for every front end

`symbol`, `callers`, `callees`, `refs`, `outline`, `deps`, `rdeps`, `similar` and `dupes` are
operations (`ops.rs`): the CLI, the MCP server, `batch` and the Python bindings run the same code
with the same defaults (search `k` 8, callers 30, callees 40, references 200, duplicates 50,
structural search 1000) and print the same text or JSON. An empty answer has exit code 1;
`outline`, `deps`, `rdeps`, `similar` and `dupes` then print nothing on standard output and one
sentence on standard error.

`--max-bytes BYTES` (or `SCRUTATE_MAX_BYTES`) limits the size of such an answer. A cut answer
keeps whole items and ends with a line like `(12 more results not shown: call again with
offset=8, or narrow the request)`; `--offset 8` continues it. An agent that runs the CLI through
a shell can set the variable once, so no answer is cut by its harness in the middle.

### `serve` (MCP)

Fourteen tools over stdio, each one an operation above (`search_code`, `grep_code`,
`find_symbol`, `callers`, `callees`, `read_code`, `outline`, `dependencies`, `dependents`,
`references`, `similar_code`, `duplicates`, `structural_search`, `reindex`).

- Every tool but `reindex` is marked read-only (`readOnlyHint`), and all are marked as staying on
  this machine (`openWorldHint: false`), so clients that ask before a write do not ask for these.
- The server returns `instructions`: which tool to start with and that answers are capped.
- Every read-only tool takes `max_bytes` (default 24,000, about 6,000 tokens) and `offset`, as
  `--max-bytes` and `--offset` above.
- `grep_code` is an Engine A search of the files `scrutate grep` searches (case-sensitive regex;
  `(?i)` for any case), matching lines grouped by enclosing symbol; `max_results` (default 100)
  stops it, and the answer says so. An invalid pattern is an error.
- Every answer begins each result with its location (`path:start-end`). A location can be passed
  wherever a tool takes a symbol, to mean that one definition among several of the same name:
  `read_code symbol`, `find_symbol`, `callers`, `callees`, `similar_code target`, `references def`.
- `read_code` reads a line range of a file (`path`, `start`, `end`), or with `symbol` a whole
  definition: a name, `Class.method` or a location. A name with several definitions returns each
  (up to 20), and `path` keeps the ones in that file.
- Every tool declares an `outputSchema`, and every result carries `structuredContent` beside its
  text: `{"schema": 1, "complete": true, "result": ...}`, where `result` is what the matching CLI
  command prints with `--json`. When the text and the JSON together pass one and a half times
  `max_bytes`, `result` is `null` and `complete` is `false`: the text then pages through the answer
  with `offset`, and a larger `max_bytes` gets the whole answer as JSON.
- A call runs on a thread of its own, so `notifications/cancelled` reaches it: a call waiting for
  the index or for its turn stops, and the answer of one already computing is dropped. Nothing
  more is sent for a cancelled request.
- While the first index of a repository is being built, a call waits up to 20 seconds and then
  answers that the index is still being built. A call that carries a `progressToken` waits for
  the index instead, with a `notifications/progress` every two seconds, and gets its answer.
- Protocol: a client that opens with `initialize` is served revisions 2024-11-05, 2025-03-26,
  2025-06-18 and 2025-11-25. A request that names its version in `_meta`
  (`io.modelcontextprotocol/protocolVersion`) is served by revision 2026-07-28, which has no
  handshake: `server/discover` describes the server, every result carries `resultType`, lists
  carry `ttlMs` and `cacheScope`, and another version in `_meta` is answered with error -32022 and
  the supported one. Both eras work on the same pipe.

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
| `textDocument/references` | the occurrences `refs` resolves to the definition under the cursor, and that definition (left out when `includeDeclaration` is false); when the cursor's occurrence is unresolved, every whole-word occurrence, as `refs --textual` |
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
for filesystem events (inotify on Linux, ReadDirectoryChangesW on Windows). On macOS it runs the
index's own incremental check instead, every second or, on a large tree, after a pause ten times as
long as the last check took: the FSEvents backend would link CoreServices into every `scrutate`
command (about 1 ms of start-up), and the check compares sizes and nanosecond times, so it misses
no edit. Events under `.scrutate/` and `.git/` are dropped, and so are paths that
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
