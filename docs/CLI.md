# Command-line reference

`scrutate --help` and `scrutate <command> --help` are authoritative; this page explains the model.

## Conventions

- **Exit codes**: `0` results found, `1` no results, `2` error (bad arguments, unreadable index,
  invalid pattern…), `130` interrupted (Ctrl-C, or stopped early for a reason other than
  `--max-results`).
- **Streams**: results on stdout, diagnostics and progress on stderr. Output is plain `path:line:text`
  when stdout is not a terminal; colour only on a terminal (`--no-color` / `NO_COLOR` disable it).
- **Determinism**: `--sort` orders Engine A output by path then line (default output is in
  completion order for speed; `-c`, `-l`, `--symbols` are always sorted). Engine B output is fully
  deterministic (ties broken by id).
- **Machine-readable output**: `--json` on every command. Engine A emits JSON Lines with
  `"schema": 1` and `"type"` ∈ {`match`, `context`, `count`, `file`, `summary`, `symbol_group`};
  Engine B commands emit one JSON document. The schema number changes only on incompatible changes.
- **Paths**: relative to the searched root, `/`-separated on every platform.
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
(within N edits, default 1, literal, ≤128 bytes). Options mirror grep/ripgrep where the meaning is
identical: `-i`, `-S` (smart case), `-w`, `-U` (multiline), `-v`, `-l`, `-c`, `-A/-B/-C`, `-m`,
`--max-results`, `-g GLOB` (`!` excludes), `-t TYPE` / `--type-list`, `--hidden`, `--no-ignore`,
`-L` (follow links), `--max-filesize`, `--max-depth`, `-j THREADS`, `--sort`, `--json`, `--stats`,
`--symbols` (group lines by the enclosing function using the index, when one exists).

## Engine B

| Command | Purpose |
|---|---|
| `index [--force]` | Build or incrementally update `.scrutate/` (progress on a terminal). |
| `search QUERY [-k N] [-a ALT]... [--also ROOT]... [--why] [--json]` (alias `s`) | Ranked search by meaning, name or identifier; `--alt` fuses alternative phrasings; `--also` searches other repositories too (paths prefixed with the repository name). |
| `find QUERY` | Unified: regex-looking → `grep`; otherwise ranked search (indexing a small repository on the spot); otherwise identifier grep. |
| `symbol NAME` | Where is it defined (`Class.method` works). |
| `callers NAME` / `callees NAME` | Call graph edges resolved to definitions (unresolved callee names listed as external). |
| `refs NAME [--max-results N]` | Definitions + callers + every whole-word occurrence grouped by enclosing symbol (works without an index: occurrences only). |
| `outline FILE` | Symbols of a file in source order. |
| `deps FILE` / `rdeps TARGET` | Imports of a file (resolved to repository files when possible) / files importing a module or file. |
| `similar TARGET [-k N]` | Code similar to a symbol or `path:line` (vector cosine + token Jaccard). |
| `dupes [--min-tokens N] [--threshold J] [--limit N]` | Exact and near-duplicate functions. |
| `tsquery LANG QUERY [PATHS] [--limit N]` | Tree-sitter structural query over every file of a language (no index). |
| `check [--files] [--json]` | Verify database integrity, segment consistency and (with `--files`) that indexed files are unchanged. Exit 1 on problems. |
| `stats` | Index statistics (files, chunks, symbols, call edges, imports, segments). |
| `languages [--json]` | The verified capability registry. |
| `serve [--refresh-interval S]` | MCP server on stdio. |
| `config` | Show the configuration file in effect. |
| `batch FILE [--features] [--timing]` | Run queries from a JSON-lines file (benchmarks, parity tests). |

### Environment variables
`SCRUTATE_ROOT`, `SCRUTATE_THREADS`, `SCRUTATE_GREP_{HIDDEN,NO_IGNORE,FOLLOW,SMART_CASE,MAX_FILESIZE,MAX_DEPTH,GLOBS,TYPES}`,
`SCRUTATE_INDEX_EXCLUDE_DIRS`, `SCRUTATE_REFRESH_INTERVAL`, `SCRUTATE_CONFIG`, `SCRUTATE_SEMANTIC`
(path or `none`), `SCRUTATE_RANKER` (path or `none`), `SCRUTATE_PROFILE` (print indexing phase
timings), `NO_COLOR`.

### The index directory
`.scrutate/index.db` (SQLite, WAL) and `.scrutate/seg/*/` (memory-mapped arrays), plus a
`.gitignore` so it is never committed. It is a cache: deleting it is always safe; `scrutate index
--force` rebuilds it; a damaged index rebuilds itself.
