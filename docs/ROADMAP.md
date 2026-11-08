# Roadmap

Ordered by value to users; each item names the gap it closes in docs/TRACEABILITY.md.

## 0.2 — platforms and packaging
- Run the CI matrix and release workflow; flip Linux aarch64, macOS and Windows rows from PARTIAL
  to PASS only with the recorded run; publish musl static binaries with the measured glibc/musl
  difference (R-13.6, R-6.1).
- Package managers: Homebrew tap, Scoop/winget, `cargo install`, Debian/RPM (R-12.13).
- Free-threaded Python wheel (`cp314t`) built and tested in CI (R-13.5).
- Signed releases actually produced (Sigstore attestations + checksums) (R-12.13).

## 0.3 — Engine A beyond ripgrep's surface
- Parallel output formatting (today a single thread formats; on output-heavy queries scrutate is
  up to 25 % behind ripgrep, see docs/BENCHMARKS.md) (R-4.1).
- Searching inside archives (zip/tar) and compressed files; PCRE-style look-around via an
  optional backtracking engine with a hard step budget (R-2A.2).
- Fuzzy patterns longer than 128 bytes (multi-word Myers blocks) and fuzzy regex (R-2A.2).
- Incremental re-search: reuse the previous walk when only a few files changed (editor
  integration) (R-4.1).

## 0.3 — Engine B depth
- Structural search from the index (store compressed syntax trees or `tsquery` results per file)
  so `tsquery` does not re-parse; today ast-grep is 6–11 % faster (R-2B.2).
- Cross-file reference resolution with scope awareness (imports → definitions) for the grammar
  tier; today `refs` combines call edges with whole-word occurrences (R-2B.4).
- Language-server protocol front end (definition, references, workspace symbols) reusing the index
  (R-6.3 IDE integrations: a real JetBrains plugin and a VS Code extension with inline results).
- More verified grammars: snapshot samples for erb, javascript (covered by parity today but not by
  a sample), matlab, puppet, tsx, and the `extra-langs` set (R-3.4).
- Index size: int8 quantisation of the semantic vectors (another ~70 MB on the 2.9 M-line corpus)
  with a measured recall check; shared-dictionary compression of chunk text (R-4.3).
- Indexing speed: tree-sitter parsing is 67 % of the CPU time; parse only the regions that changed
  (tree-sitter incremental parsing against the previous tree) for edited files (R-4.2).

## 0.4 — ecosystem
- GitHub App / GitLab integration that comments duplicate-code and dependency reports on merge
  requests using the composite action and templates shipped in 0.1.0 (R-12.12).
- `scrutate watch`: filesystem-event driven index refresh for the MCP server (R-2B.1).
- Benchmark dashboard from the weekly `benchmarks.yml` runs (R-4.5).

## Research
- Learned ranker trained on more languages (the current training set is Python-heavy; JS/TS and
  Rust are evaluated, other languages inherit lexical ranking quality only) (R-2B.3).
- Semantic model: evaluate small transformer encoders under the no-network, CPU-only constraint
  against the current code-word model (MRR gain vs. index time and binary size).
