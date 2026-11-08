# Requirement traceability matrix

Every requirement of the mission (sections 1–13), with its implementation, source location,
verification method, evidence and status. Status words follow the mission's definitions:

- **PASS** — implemented and verified by the evidence named.
- **PARTIAL** — implemented but incompletely verified, or verified only in part.
- **FAIL** — not implemented, or verification failed.
- **BLOCKED** — a genuine, documented technical limitation of the build environment or the
  problem.

Evidence files: `docs/TEST_RESULTS.md` (test and audit output), `docs/BENCHMARKS.md` (generated
from `bench_results/final.json`), `docs/QUALITY.md`, `docs/SECURITY.md`, `docs/LANGUAGES.md`,
`docs/NAMING.md`. Test names refer to `crates/scrutate/tests/*.rs` and `python/tests/*.py`.
Counts: PASS 100 · PARTIAL 17 · BLOCKED 3 · FAIL 0 over 120 rows (computed by `tools/traceability_tally.py`).

## 2A. Engine A — universal instant search

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-2A.1 | Universal plain-text search, no index | `instant.rs` (`search`, `build_walker`, `Collector`) | `cli.rs::exit_codes_and_plain_output`, `json_lines_schema_and_modes`, differential vs ripgrep (750 random cases, 0 disagreements), benchmarks on 3 corpora | PASS |
| R-2A.2 | Literal, regex, fuzzy, approximate matching | `Mode::{Literal,Regex,Fuzzy}`; `FuzzyMatcher` (Myers, pigeonhole prefilter) | unit tests `instant::tests`, proptest `fuzzy_finds_edited_patterns`, bench rows "regex", "fuzzy (1 edit)" | PASS |
| R-2A.3 | Multiline and identifier searching | `-U` (`multi_line`, no line terminator), `--ident` (`ident_parts`, `identifier_regex`) | unit tests, proptest `identifier_regex_matches_all_casings`, bench rows "multiline", "identifier"; differential (multiline `-l`) | PASS |
| R-2A.4 | SIMD acceleration where beneficial | via `regex`/`aho-corasick`/`memchr` (vectorised literal and prefilter scanning in the dependency chain), `half` runtime F16C conversion; own Myers is 128-bit bit-parallel | benchmark parity/advantage vs ripgrep (same SIMD substrate); no hand-written intrinsics | PASS |
| R-2A.5 | Parallel filesystem traversal | `ignore::WalkParallel` (work-stealing), threads = min(cores, 12) or `-j` | bench "concurrency", robustness symlink tests | PASS |
| R-2A.6 | Optimized memory and I/O | `grep-searcher` buffered reader with line-oriented search; per-file batched channel; bounded output buffer; `Arc<str>` paths | CPU/RSS columns in BENCHMARKS.md (peak RSS ≈ ripgrep's) | PASS |
| R-2A.7 | Efficient pattern matching | linear-time regex with literal optimisations; literal fast path; word matcher | bench rows; proptest | PASS |
| R-2A.8 | Streaming results and cancellation | channel-fed sink; `AtomicBool` cancel; Ctrl-C handler (exit 130); broken-pipe stop; `--max-results` | robustness `regex_limits_and_errors_are_clean` (cancel stops early), `cli.rs::exit_codes_and_plain_output` | PASS |
| R-2A.9 | Git-aware and ignore-aware | `.gitignore`/`.ignore`/global/exclude, nested ignore files, `--no-ignore`, `--hidden` | robustness `symlinks_ignores_and_nested_repos`; differential (random `.gitignore`) | PASS |
| R-2A.10 | Advanced filtering and traversal | `-g`, `-t/--type-list`, `--max-filesize`, `--max-depth`, `-L`, `--hidden`, `-m`, `--max-results` | `cli.rs::json_lines_schema_and_modes`, bench | PASS |
| R-2A.11 | Works on unknown languages immediately | no parser involved; any readable file | robustness (noext, UPPER.PY, Makefile), differential | PASS |
| R-2A.12 | Never depends on Engine B / index / parsers / cloud / background processes | `instant.rs` imports nothing from index/parse; `grep` never opens `.scrutate` | code review; `grep` runs in a directory with no index (all tests) | PASS |

## 2B. Engine B — deep code intelligence

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-2B.1 | Incremental repository indexing | `index.rs::update` (mtime/size fast path, SHA-1 hash, per-file delete+insert, new segment per update, compaction) | robustness `incremental_updates_track_edits_renames_and_deletes`; bench "1-file edit update s" | PASS |
| R-2B.2 | AST-aware and structural searching | tree-sitter grammars (`langs.rs`, `parse.rs`); `tsquery` (`intel::tsquery`) | samples snapshot test (56 languages); bench structural rows (identical counts with ast-grep) | PASS |
| R-2B.3 | Symbol and reference discovery | chunks with kinds/qualnames; `symbol`, `refs` (defs + call edges + grouped occurrences) | `cli.rs::index_search_symbol_refs_outline_deps`, bindings tests | PASS |
| R-2B.4 | Find definitions and usages | `find_symbol`, `references()` | bindings test `test_repo_index_and_intelligence`; `cli.rs::index_search_symbol_refs_outline_deps` | PASS |
| R-2B.5 | Function, method, class, type discovery | definition kinds per grammar (function, method, class, struct, enum, trait, interface, type, impl…) | `tests/samples/expected.json` | PASS |
| R-2B.6 | Import and dependency analysis | `imports` table; `intel::deps` (module→file resolution), `rdeps` | unit test `imports_are_extracted_and_normalised`; bindings test (deps/dependents) | PASS |
| R-2B.7 | Call-site discovery | `refs` table (call edges), `callers`, `callees`, `refs` | bindings test (callers == ["use_3"]); Python reference tests | PASS |
| R-2B.8 | Cross-file and cross-repository intelligence | call graph and imports across files; `search --also ROOT` merges ranked results across repositories | `cli.rs::search_also_merges_repositories` | PASS |
| R-2B.9 | Duplicate and similar-code discovery | `intel::dupes` (exact token hash + MinHash/LSH over 3-gram shingles, nested chunks excluded), `intel::similar` (implementation vectors + Jaccard) | bindings tests; manual networkx run (DiGraph/Graph `add_nodes_from` 0.93 similarity found) | PASS |
| R-2B.10 | Language-aware navigation | `outline`, `symbol`, `callers`, `callees`, `refs`, `deps` | bindings tests, `cli.rs::index_search_symbol_refs_outline_deps` | PASS |
| R-2B.11 | Evaluate tree-sitter / compilers / language servers | tree-sitter chosen (63 grammars evaluated, 2 dropped for mis-parsing, 5 for toolchain incompatibility); compiler front-ends and LSP servers rejected for 0.1 (per-language toolchains, repository code execution risk) — see ARCHITECTURE.md | design record | PASS |
| R-2B.12 | Ranking quality (meaning, name, identifier queries) | BM25 + exact + semantic + LightGBM ranker | QUALITY.md: held-out MRR 0.632 (doc) … 0.989 (id), all above the lexical baseline with non-overlapping CIs | PASS |

## 2C. Unified query layer

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-2C.1 | One consistent CLI and query interface | `cli.rs` (clap), shared conventions (exit codes, `--json`, roots) | `cli.rs` (13 end-to-end tests) | PASS |
| R-2C.2 | Automatic strategy selection without sacrificing correctness | `find` (regex → Engine A; indexed/small repo → ranked; else identifier grep); `grep --symbols` enrichment | `cli.rs::find_routes_by_query_shape` | PASS |
| R-2C.3 | Engines independently operational and testable | separate modules, separate test files (`instant` tests need no index; `index` tests build their own) | test suite layout | PASS |

## 3. Universal support

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-3.1 | Every readable text file searchable regardless of language/name/extension | Engine A: all files; Engine B: windows tier for everything not excluded | robustness `hostile_text_shapes_never_panic` (noext, Makefile, UPPER.PY) | PASS |
| R-3.2 | Unknown languages without configuration | heuristic tier + windows | `heuristics_find_keyword_definitions`, samples for grammar-less languages | PASS |
| R-3.3 | Extensible language adapters | data-driven `LangSpec` registry (`langs.rs`) | 61 grammars + optional feature; CONTRIBUTING.md procedure | PASS |
| R-3.4 | Verified language capability registry | `scrutate languages --json`; `tools/gen_languages_md.py` joins it with snapshot evidence | LANGUAGES.md: 60 grammars, all 60 verified by snapshot samples (ERB moved to the heuristic tier) | PASS |
| R-3.5 | Monorepos, multiple repositories, Git and non-Git, mixed languages, legacy, config files, generated code | ignore-aware walk (works without git), `--also`, config/data files indexed (JSON/YAML/TOML/INI keys), `exclude_dirs` for generated code | big corpus (10 packages, 6 525 files), robustness nested-repo test | PASS |

## 4. Extreme performance

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-4.1 | SIMD, algorithms, parallelism, cache-efficient structures, mmap, buffered I/O, low allocation, adaptive concurrency, incremental indexing, optimised storage | see 2A/2B; segments are mmapped contiguous arrays in cluster order; chunk text zlib; IVF | BENCHMARKS.md; profiler (`SCRUTATE_PROFILE`) | PASS, except *adaptive concurrency* (thread count fixed at min(cores,12); no load-adaptive scheduling) → PARTIAL |
| R-4.2 | Benchmark against grep, ripgrep, git grep, ugrep, ast-grep | `bench/compare_tools.py` | BENCHMARKS.md (ugrep from a bundled `+sse2` build — documented) | PASS |
| R-4.3 | Benchmark against ack and ag | harness supports both (`command()` branches) | neither installable here (no CPAN/apt; building fetched source is not permitted in this environment) | BLOCKED |
| R-4.4 | Measure latency, throughput, CPU, memory, startup, indexing time, index size, concurrency | harness measures all eight | BENCHMARKS.md tables | PASS |
| R-4.5 | Small and massive repositories, cold and warm caches, query types, realistic workloads | corpora small/big/massive; cold via `drop_caches`; 10 query types; realistic = the 300-query benchmark set over networkx (docstring-derived) | BENCHMARKS.md; `work/bench/queries` | PASS (realistic workloads are synthetic-from-real queries, not recorded user sessions → noted) |
| R-4.6 | Reproducible benchmarks with versions, hardware, configuration, datasets, commands | results JSON records tool versions, CPU, cores, RAM, exact argv; corpora described | BENCHMARKS.md header; `final.json` | PASS |
| R-4.7 | Outperform competitors across every workload; report where they remain superior | — | BENCHMARKS.md: within 2 % of ripgrep on the geometric mean (faster on 7 of 27 rows, 2.1× GNU grep, 1.7× git grep), slower on output-heavy rows (up to 25 % with 1.5 M matching lines) and on structural search (ast-grep 6–11 % faster); honestly reported | PARTIAL |

## 5. World-class Rust engineering

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-5.1 | Stable Rust, evaluated dependencies | stable 1.97.0, `rust-version` 1.85; DEPENDENCIES.md with purpose per crate | `cargo build` on stable; `cargo audit` 0 | PASS |
| R-5.2 | Modular, cohesive, independently testable | 15 modules with single responsibilities; tests per module | test layout | PASS |
| R-5.3 | Correctness, memory safety, predictable resources, concurrency, maintainability | parity with reference; unsafe inventory (SECURITY.md); bounded buffers; `Mutex`-guarded engine | parity 300/300 + IVF 200/200; robustness; concurrency stress 30× | PASS |
| R-5.4 | Minimal dependencies | 178 shipped crates, 95 of them tree-sitter grammars; `zip` removed in favour of 80 lines | DEPENDENCIES.md | PASS |
| R-5.5 | No dead code, duplicated logic, unnecessary allocations, unjustified abstractions | `-D warnings` (dead code warns), clippy clean; shared `references()` for CLI+MCP; `Arc<str>` paths, per-file batches | `cargo clippy --all-targets -D warnings` clean | PASS |
| R-5.6 | Async only where beneficial | none used (threads + channels suffice for a local tool) | design note | PASS |
| R-5.7 | `cargo fmt`, `cargo clippy`, `cargo test`, dependency auditing | all gates green; `rustfmt.toml` | TEST_RESULTS.md | PASS |

## 6. Enterprise and future-proofing

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-6.1 | Target Linux, macOS, Windows | portable code (`std`, `ignore`, `rusqlite` bundled); CI matrix for 6 OS/arch runners | Linux x86_64 built+tested here; macOS/Windows/aarch64 builds only in CI, **not yet run** | PARTIAL |
| R-6.2 | Secure filesystem access | path confinement in `read`, no following of symlinks by default, index only under root | SECURITY.md; robustness; bindings `read("../etc/passwd")` | PASS |
| R-6.3 | Permission-aware indexing | unreadable files counted and skipped | robustness `unreadable_files_are_reported_not_fatal` (skips when root) | PASS |
| R-6.4 | Repository isolation | one `.scrutate/` per repository; no shared state | design; concurrency test | PASS |
| R-6.5 | Resource exhaustion protection | regex size limits, file size caps, binary skip, bounded channels, `--max-*` | robustness | PASS |
| R-6.6 | Index corruption detection and recovery | triage on open, segment validation, `check`, auto rebuild | robustness `corrupted_index_is_detected_and_rebuilt` (5 scenarios) | PASS |
| R-6.7 | Versioned storage and migration | `schema_version` (11), model signature; mismatch → in-database clear + re-index | robustness; `Triage::Rebuild` | PASS (migration policy = rebuild the cache) |
| R-6.8 | Offline and air-gapped operation | no network code; models embedded | built and run behind an allow-list proxy with no package network | PASS |
| R-6.9 | No unauthorized source-code transmission | nothing leaves the machine | code review (no networking crates) | PASS |
| R-6.10 | Dependency and supply-chain security | `Cargo.lock`, `cargo audit` 0/0, SBOM (CycloneDX), licences reviewed | TEST_RESULTS.md, `tools/sbom.py` | PASS |
| R-6.11 | Reproducible builds | pinned profile, lock file, `tools/reproducible_check.sh` | two clean local builds identical (TEST_RESULTS.md); cross-machine not claimed | PARTIAL |
| R-6.12 | CI/CD integration | `.github/workflows/{ci,release,benchmarks}.yml`, GitLab/Bitbucket templates | YAML validated; **not executed** | PARTIAL |
| R-6.13 | JSON and JSONL output | `--json` everywhere; JSONL schema 1 for Engine A | `cli.rs::json_lines_schema_and_modes`, bindings vs CLI identity | PASS |
| R-6.14 | Stable, versioned integration interfaces | JSONL `schema`, MCP protocol versions, SemVer policy (CHANGELOG.md) | docs | PASS |
| R-6.15 | IDE, automation, Git-platform, MCP integrations (optional) | MCP server; VS Code, Neovim, JetBrains External Tools, GitHub action, GitLab/Bitbucket templates, pre-commit | MCP: automated protocol test `cli.rs::mcp_protocol_round_trip`; others: syntax-validated, not run in their hosts | MCP PASS; others PARTIAL |
| R-6.16 | 10–20-year maintenance horizon | versioned formats, replaceable components, compatibility policy, automated tests (ARCHITECTURE.md "Design rules") | design record | PASS (as a design; time will verify) |

## 7. Project identity

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-7.1 | Original, memorable, commercially suitable name | **Scrutate** | NAMING.md | PASS |
| R-7.2 | ≥ 15 candidates researched (GitHub, crates.io, products, registries, domains, trademarks) | 30 candidates; crates.io, PyPI, npm, GitHub repos, Homebrew, web search | `docs/name_research.json`; domains and trademarks **not** checked (documented) | PARTIAL |
| R-7.3 | Selection by documented evidence; no false uniqueness/legal claims | NAMING.md evidence table and "not verified" section | — | PASS |

## 8. Actually build the product

| ID | Requirement | Status | Evidence |
|---|---|---|---|
| R-8.1–8.10 | Inspect → research → foundation → Engine A → Engine B → unify → benchmark/optimise → security/reliability testing → documentation/installation → deliver 0.1.0 | PASS | this repository; `git log`; TEST_RESULTS.md; BENCHMARKS.md; docs/ |
| R-8.11 | Preserve existing user work | PASS | the previous `titan/` tree is untouched; the reference implementation lives on as `python/scrutate_ref` |
| R-8.12 | Decide autonomously | PASS | decisions recorded in CHANGELOG/ARCHITECTURE; one question asked (stack confirmation) |

## 9. Zero-gap quality assurance

| ID | Requirement | Implementation | Status |
|---|---|---|---|
| R-9.1 | Unit tests | `cargo test` lib tests (text, npy, lex, parse, config, instant…) | PASS |
| R-9.2 | Integration tests | `tests/samples.rs`, `tests/robustness.rs`, `tests/cli.rs` (13 tests) | PASS |
| R-9.3 | End-to-end tests | `tests/cli.rs` runs the real binary (grep/index/search/check/MCP) | PASS |
| R-9.4 | Differential tests | `bench/differential.py` vs ripgrep (750 cases, 0 disagreements); `bench/parity.py` Rust vs Python (bit-identical) | PASS |
| R-9.5 | Property-based tests | `tests/properties.rs` (7 properties, proptest) | PASS |
| R-9.6 | Fuzz tests | `parser_fuzz_smoke` (random bytes/tokens through every parser tier) + proptest; no coverage-guided fuzzing (cargo-fuzz needs nightly) | PARTIAL |
| R-9.7 | Regression tests | snapshot `expected.json`; `bench/regression.py` + `bench/baseline.json` + weekly workflow | PASS |
| R-9.8 | Stress tests | robustness: 5 MB line, 200k lines, 20k nesting; 3 writers + 2 readers × 30 runs | PASS |
| R-9.9 | Security tests | regex limits, path confinement, symlink escape, corruption, panic containment | PASS |
| R-9.10 | Cross-platform tests | CI matrix written, not run; Linux only here | PARTIAL |
| R-9.11 | Traceability matrix with implementation, location, verification, evidence, status | this document | PASS |
| R-9.12 | Continuous gap analysis | RELEASE_READINESS.md "remaining defects"; ROADMAP.md | PASS |

## 10. Release gates (0.1.0)

| Gate | Status | Evidence |
|---|---|---|
| Functional universal instant search | PASS | 2A rows |
| Unknown-language generic search | PASS | R-3.1/3.2 |
| Reliable filesystem traversal | PASS | robustness tests, differential |
| Ignore rules and advanced filtering | PASS | R-2A.9/2A.10 |
| Literal and regex search | PASS | R-2A.2 |
| Machine-readable output | PASS | R-6.13 |
| Functioning, tested code-intelligence foundation | PASS | 2B rows, QUALITY.md |
| Automated tests | PASS | TEST_RESULTS.md |
| Reproducible competitive benchmarks | PASS | BENCHMARKS.md |
| Verified builds for declared supported platforms | PASS for the one declared platform (Linux x86_64); others not declared | INSTALL.md |
| Security assessment | PASS (self-assessment; no external audit) | SECURITY.md |
| Professional documentation | PASS | docs/ |
| Installation instructions | PASS | INSTALL.md |
| Do not declare production readiness until criteria pass | respected: **0.1.0 is not declared production-ready** | RELEASE_READINESS.md |

## 12. Edge cases, reliability, ecosystem

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-12.1 | Filesystem edge cases | long names, spaces/unicode names, dotfiles, no extension, uppercase extensions | robustness `hostile_text_shapes_never_panic` | PASS |
| R-12.2 | Operating-system differences | portable code, CRLF/CR handling, `/`-normalised paths | Linux verified; Windows/macOS via CI (not run) | PARTIAL |
| R-12.3 | Unicode | lossy decoding, `prefix_bytes` boundary safety (bug found and fixed), emoji/Cyrillic/Greek identifiers | robustness, proptest `prefix_bytes_is_valid_and_maximal` | PASS |
| R-12.4 | Binary files | NUL sniff (Engine B), `BinaryDetection::quit` (Engine A), extension exclusions | robustness | PASS |
| R-12.5 | Invalid encodings | invalid UTF-8 lossy; UTF-16 with BOM: Engine A transcodes via BOM sniffing in `grep-searcher`; Engine B skips (binary) | robustness; `cli.rs::utf16_bom_is_searched_by_engine_a_and_skipped_by_engine_b` | PASS (Engine B limitation documented) |
| R-12.6 | Symlinks | not followed by default, loop-safe with `-L` | robustness | PASS |
| R-12.7 | Git worktrees and submodules | `.git` *files* (worktrees, submodules) skipped as VCS metadata; nested repos walked | robustness `symlinks_ignores_and_nested_repos`, `cli.rs::git_file_of_worktrees_and_submodules_is_not_indexed` | PASS |
| R-12.8 | Permissions | counted as errors, never fatal | robustness | PASS |
| R-12.9 | Huge files | Engine A searches any size (5 MB line test); Engine B skips > 1 MB and reports `files_skipped` | robustness | PASS |
| R-12.10 | Network filesystems | not tested; SQLite WAL locking is unreliable on NFS/SMB by SQLite's own documentation → documented as unsupported for the index directory (Engine A unaffected) | INSTALL.md / SECURITY.md note | BLOCKED (cannot be tested here; documented) |
| R-12.11 | Files changing during execution | content read once per file (hash and parse the same bytes); concurrent writers test; mtime-only change detection re-hashes | robustness concurrency + incremental tests | PASS |
| R-12.12 | Index lifecycle: hashing, change detection, atomic updates, concurrent access, crash recovery, corruption detection, compaction, cleanup, rebuilding, versioned migrations | `index.rs`, `lex.rs` (manifest + immutable segments, `BEGIN IMMEDIATE`, temp-dir+rename, GC with age guard, `compact`, `rebuild`, `Triage`) | robustness (incremental, corruption, concurrency), `cli.rs::compaction_merges_segments_and_keeps_counts`, `cli.rs::corruption_found_during_use_triggers_a_rebuild`, multi-process run (4 indexers + reader) | PASS |
| R-12.13 | Professional CLI: completion, configuration, env vars, piping, deterministic output, structured diagnostics, progress, cancellation, exit codes | `completions`, `config.rs`, env on every option, BrokenPipe handling, `--sort`, `--json` summaries, progress on TTY, Ctrl-C, 0/1/2/130 | `cli.rs::config_file_sets_defaults_but_command_line_wins`, `exit_codes_and_plain_output` | PASS, except *structured diagnostics*: errors are plain-text stderr lines (JSON records carry error counts only) → PARTIAL |
| R-12.14 | Protections: regex exhaustion, unauthorized file access, unsafe temp files, dependency vulnerabilities, untrusted repository execution | limits; confinement; temp dirs inside `.scrutate/seg` + rename; audit 0; never executes repository content | SECURITY.md table | PASS |
| R-12.15 | Verification: unit, integration, differential, property, fuzz, stress, security, regression, cross-platform | see section 9 | PASS except fuzz (PARTIAL) and cross-platform (PARTIAL) |
| R-12.16 | Real integrations: VS Code, JetBrains, Neovim, GitHub, GitLab, Bitbucket, CI/CD, MCP | `integrations/` + `.github/` | MCP automated; VS Code (extension.js syntax-checked, not run in VS Code), Neovim (not run), JetBrains (External Tools config, no plugin), GitHub action / GitLab / Bitbucket (YAML validated, not run) | MCP PASS; rest PARTIAL |
| R-12.17 | Installation, updates, uninstallation, packaging, signed releases, checksums, SBOMs, reproducible builds | INSTALL.md; release workflow (tarballs, SHA-256, Sigstore attestations, SBOM); `tools/sbom.py`; local checksums for delivered artifacts | SBOM generated; checksums for the delivered zip; signing + packaging only in the unexecuted workflow; no package-manager packages | PARTIAL |
| R-12.18 | Reproducible benchmarks and regression tracking | `compare_tools.py`, `regression.py`, `baseline.json`, weekly workflow | BENCHMARKS.md | PASS |
| R-12.19 | Every edge case/integration/requirement has explicit status and evidence | this matrix | — | PASS |

## 13. Hybrid Rust + Python stack

| ID | Requirement | Implementation | Verification | Status |
|---|---|---|---|---|
| R-13.1 | Python only in permitted roles | benchmarks/reports (`bench/`), differential/parity scripts, optional PyO3 package (`crates/scrutate-py`), JSONL consumers (`bench/differential.py`), model training (`python/scrutate_ref`, `bench/train_*`) | repository layout | PASS |
| R-13.2 | Engines and CLI in Rust, no Python at runtime | `crates/scrutate` has no Python dependency; models embedded | `cargo tree` shows no `pyo3` under `scrutate`; binary runs with no Python on PATH (`cli.rs::runs_without_python_on_path`) | PASS |
| R-13.3 | Core builds and passes `cargo test` without Python; bindings behind an optional Cargo feature | bindings are a separate workspace member excluded from `default-members` (`cargo build`/`cargo test` never compile pyo3); core has zero Python code paths | `cargo test -p scrutate` with PATH stripped of python (TEST_RESULTS.md) | PASS (implemented as an optional *crate* rather than a feature flag of the core, so the core cannot even optionally depend on Python) |
| R-13.4 | Bindings release the GIL during long work | `Python::detach` around index/search/grep waits | `test_gil_is_released_during_rust_work`; measured 1.82× with 4 threads on 2 cores | PASS |
| R-13.5 | Minimise Rust↔Python conversion; streaming/lazy results | `GrepIter` (bounded channel, per-batch conversion), results converted once | bindings tests; design | PASS |
| R-13.6 | abi3 wheels for standard Python (one wheel per OS/arch) | `pyo3/abi3-py39`, maturin → `scrutate-0.1.0-cp39-abi3-manylinux_2_34_x86_64.whl` | built and tested on Python 3.13 here; 3.10/3.12 only in CI (not run) | PARTIAL |
| R-13.7 | Free-threaded Python: separate `cp314t` wheel; `gil_used = false` only after review | `#[pymodule(gil_used = false)]` with the thread-safety review in `lib.rs`; release workflow builds `cp314t` | no free-threaded interpreter available here; **not tested** | PARTIAL |
| R-13.8 | Linux static musl binaries, identical output, measured difference | CI job `musl` + `bench/musl_vs_glibc.py` | `rustup target add x86_64-unknown-linux-musl` cannot download in this environment (static.rust-lang.org blocked) → not built here | BLOCKED (documented; CI job ready) |
| R-13.9 | Wheels and binaries for Linux/macOS/Windows in CI; declare a platform only after testing there | `release.yml` (8 binary targets, 12 wheel builds); INSTALL.md declares Linux x86_64 only | workflow not run | PARTIAL |
| R-13.10 | Never cite unverified external claims | docs reviewed; external claims limited to crate descriptions, dictionary entries and measured numbers | review | PASS |
| R-13.11 | Hybrid requirements carry PASS/PARTIAL/FAIL/BLOCKED | this section | — | PASS |

## Tally

PASS 100 · PARTIAL 17 · BLOCKED 3 · FAIL 0 over 120 rows. The PARTIAL and BLOCKED rows are listed with their
resolution in RELEASE_READINESS.md ("Remaining defects and incomplete requirements").
