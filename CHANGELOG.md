# Changelog

All notable changes to Scrutate. The format follows Keep a Changelog; versions follow SemVer.
Compatibility policy: the CLI surface, the JSONL `schema` number, the MCP tool names and the Python
API are versioned interfaces; incompatible changes bump the major version once past 1.0 and are
listed here under "Breaking". The on-disk index is a cache with its own `schema_version`; a new
version rebuilds incompatible indexes automatically and this is never considered breaking.

## [0.1.0] — 2026-11-08

First release.

### Engine A — universal instant search
- Literal, regex (linear time, size-limited), identifier (all casings, digit/acronym aware),
  fuzzy/approximate (Myers bit-parallel, pigeonhole prefilter), multiline, inverted, whole-word,
  smart-case matching; context lines; `-c`, `-l`, `--max-count`, `--max-results`.
- Parallel ignore-aware traversal (`.gitignore`, `.ignore`, global excludes, hidden, symlinks,
  depth and size limits, type globs); per-file batched streaming; Ctrl-C and broken-pipe
  cancellation; deterministic `--sort`; JSONL output (schema 1); exit codes 0/1/2/130.
- Differentially tested against ripgrep on 750 random corpora × patterns with zero disagreements.

### Engine B — code intelligence
- Incremental index (SQLite + memory-mapped segments), content hashing, atomic updates,
  compaction, GC, corruption detection and self-healing rebuild, schema/model versioning,
  multi-process safety, `check` command.
- 61 tree-sitter grammars in the default build (56 snapshot-verified), keyword heuristics for
  grammar-less languages, windows for everything else; imports and call edges.
- Ranked search: field-weighted BM25 + exact lookup + semantic vectors (IVF) + LightGBM
  LambdaRank ranker; bit-identical with the Python reference implementation.
- `symbol`, `callers`, `callees`, `refs`, `outline`, `deps`, `rdeps`, `similar`, `dupes`
  (exact + MinHash/LSH near-duplicates), `tsquery` (tree-sitter structural search),
  cross-repository `search --also`.

### Product
- Unified `find`, MCP server (14 tools), configuration files with environment-variable precedence,
  shell completions, progress reporting, structured diagnostics.
- Python bindings (PyO3 0.30, abi3 wheels, GIL released, streaming iterator, free-threading
  declaration after review).
- Reproducible benchmark harness (`bench/compare_tools.py`) against grep, ripgrep, git grep, ugrep
  and ast-grep; regression gate; CI/release workflows; SBOM; IDE and Git-platform integrations.
