# Changelog

All notable changes to Scrutate. The format follows Keep a Changelog; versions follow SemVer.
Compatibility policy: the CLI surface, the JSONL `schema` number, the MCP tool names and the Python
API are versioned interfaces; incompatible changes bump the major version once past 1.0 and are
listed here under "Breaking". The on-disk index is a cache with its own `schema_version`; a new
version rebuilds incompatible indexes automatically and this is never considered breaking.

## [0.1.0] - 2026-11-09

First release.

### Engine A — universal instant search
- Literal, regex (linear time, size-limited), identifier (all casings, digit/acronym aware),
  fuzzy/approximate (Myers bit-parallel, pigeonhole prefilter), multiline, inverted, whole-word,
  smart-case matching; context lines; `-c`, `-l`, `--max-count`, `--max-results`.
- Parallel ignore-aware traversal (`.gitignore`, `.ignore`, global excludes, hidden, symlinks,
  depth and size limits, type globs); per-file batched streaming; Ctrl-C and broken-pipe
  cancellation; deterministic `--sort`; JSONL output (schema 1); exit codes 0/1/2/130.
- Differentially tested against ripgrep on random corpora and patterns with zero disagreements.
- `-e PATTERN` (repeatable) for several patterns; each line of a multi-line match is printed with its
  own line number.
- Plain output is formatted by the search threads into per-file buffers (no per-line allocation or
  channel message); lines over 64 KB are written without being copied, their UTF-8 checked a
  256 KB piece at a time as they are written; UTF-8 is validated with the fast checker before the
  replacing decoder.
- Ctrl-C handling through `signal()` / `SetConsoleCtrlHandler` directly: the `ctrlc` crate linked
  Foundation and libobjc into every macOS process (about 1.5 ms of start-up).
- A file of 64 MB or more is searched on every search thread as ranges of whole lines, with the
  output in file order. Binary data, a byte-order mark, context lines, `--max-count`,
  `--max-results`, `-l` and multiline searches keep the single pass.
- A regex with no literal prefix whose matches all end in one of a few literals
  (`\w+_(suspend|resume)\(`) finds candidate lines by searching for those literals.
- A small tree is walked on one thread while three threads search what it finds; past 256 files
  the parallel walk takes over (about a quarter less time on a 220-file repository).
- A large file without line breaks to split at is read into one buffer of its exact size (the
  stream reader grew its buffer by doubling).
- `-P` / `--pcre2`: PCRE2 syntax (look-around, backreferences), stopped by PCRE2's match limit
  on exponential patterns, with a warning naming the file.
- `-z` / `--search-zip`: zip and tar members (`archive.zip{member}`) and compressed files.
- `--fuzzy` patterns up to 4096 bytes (64-bit blocks beyond 128); `--fuzzy-regex[=N]`: a regex
  within N edits (bit-parallel Glushkov automaton, up to 64 character positions).
- `grep --index --update`: refresh the text index (changed files only), then search it.
- Files that could not be searched, or not to the end, are named in warnings.
- Regexes are built with `^` and `$` as line anchors in every mode, as ripgrep builds them. Before,
  a pattern using either was matched one line at a time: `^(a|aa)+$` over ten million one-byte
  lines took 198 ms, now 8 ms.
- Fixed: a file given directly as the search root was printed with an empty path (`:1:text`).
- `ignore` 0.4.34 is vendored (`vendor/ignore`) with one change: the global git excludes path is
  read from `~/.gitconfig` without compiling a regex, about 0.4 ms of every search in a git
  repository.

### Engine B — code intelligence
- Incremental index (SQLite + memory-mapped segments), content hashing, atomic updates,
  compaction, GC, corruption detection and self-healing rebuild, schema/model versioning,
  multi-process safety, `check` command.
- 60 tree-sitter grammars in the default build, each verified by a snapshot sample; 13 more with
  `--features extra-langs` (12 verified); keyword heuristics for grammar-less languages, windows for
  everything else; imports and call edges.
- Text index for repeated grep-style queries (`scrutate index --text`, `grep --index`): trigram
  postings over exactly the files Engine A's walk visits, trigram conditions derived from the
  pattern's syntax tree, candidates searched by Engine A's own matcher, so the output equals the
  walk's. Incremental (segments merge during updates) and safe for concurrent readers.
- UTF-16 files with a byte-order mark are transcoded and indexed (previously skipped as binary).
- Chunk line numbers follow grep's line counting (`\n` only); Python-style splitting had shifted
  them after form feeds (found by fuzzing). Index schema 12.
- The 12 optional grammars (`--features extra-langs`) are verified by samples; writing them fixed
  the D, LLVM IR, AutoHotkey, VHDL and OCaml rules.
- Ranked search: field-weighted BM25 + exact lookup + semantic vectors (IVF) + LightGBM
  LambdaRank ranker; bit-identical with the Python reference implementation.
- `symbol`, `callers`, `callees`, `refs`, `outline`, `deps`, `rdeps`, `similar`, `dupes`
  (exact + MinHash/LSH near-duplicates), `tsquery` (tree-sitter structural search),
  cross-repository `search --also`.
- `refs` resolves each occurrence to the definition it refers to (scopes, imports, qualified
  names) for Python, JavaScript/TypeScript, Rust and Go; `--def` keeps one definition and
  `--textual` gives every whole-word occurrence as before.
- `tsquery` keeps each file's captures in `.scrutate/tsquery/` by content hash: a repeated query
  re-parses only changed files (the Linux tree: about 25 s, then 1.6 s), output identical to
  `--no-cache`; it also accepts a single file as its path.
- Semantic vectors stored as int8 with a scale per vector (schema 13): the index is about a fifth
  smaller, ranking quality within 0.001 MRR, parity with the Python reference exact.
- `scrutate lsp`: a language server (definitions, resolved references, workspace symbols,
  outlines) for editors; the VS Code extension starts it, JetBrains uses it through LSP4IJ.
- `scrutate watch` and `serve --watch` keep the indexes fresh as files change.
- A long-running index (`watch`, `serve`, `lsp`) reparses an edited file against its previous
  syntax tree (up to 2 MB of source kept) for the 21 grammars without an external scanner: a
  66 KB Go file, 5.5 ms to 2.0 ms per edit.
- Ranker v8, fitted on Python and on 16 repositories in Rust, Go, Java, JavaScript and TypeScript:
  better than v7 on held-out code in 56 of 58 language, variant and query-type cells, never worse
  beyond noise (docs/QUALITY.md).

### Product
- Unified `find`, MCP server (14 tools), configuration files with environment-variable precedence,
  shell completions, progress reporting; errors as JSON records with `--json` or
  `SCRUTATE_DIAGNOSTICS=json`.
- Licensed under Apache-2.0. Toolchain pinned to Rust 1.99.1 (`rust-version` 1.89).
- Python bindings (PyO3 0.30, abi3 wheels, GIL released, streaming iterator, free-threading
  declaration after review).
- Benchmark suite (`bench/leaderboard.py`, `bench/indexed.py`): grep, ripgrep, ack, ag, git grep,
  ugrep, ast-grep, Zoekt and livegrep on pinned corpora, every answer checked against an
  independent reference, 30+ runs per contender, bootstrap confidence intervals, hardware-ceiling
  baseline, warm and cold cache; regression gate; CI/release workflows; SBOM; IDE and Git-platform
  integrations.
- Packaging: Homebrew formula, Scoop and winget manifests, `.deb` and `.rpm` packages (built and
  install-tested in clean containers); release archives and wheels carry
  `THIRD_PARTY_NOTICES.txt`; `docs/dashboard.html` charts the benchmark runs.
- Fuzz targets (`fuzz/`): the `.npy` reader (three defects fixed: a panic on truncated string
  arrays, a wrapping element count in release builds, a huge allocation for zero-width strings),
  the text index's soundness, the edit-distance matcher and every parser tier.
