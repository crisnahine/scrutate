# Delivery index: Scrutate 0.1.0 (2026-11-09)

The fifteen items the mission asks for (section 11), each with where it lives in this repository.

| # | Item | Location |
|---|---|---|
| 1 | Project name and naming research | docs/NAMING.md, docs/name_research.json, tools/name_research.py |
| 2 | Complete source code and architecture | crates/scrutate (engines, CLI, MCP), crates/scrutate-py (bindings), python/scrutate_ref (reference implementation and trainer), models/; docs/ARCHITECTURE.md |
| 3 | Implemented capabilities | README.md, docs/CLI.md, docs/TRACEABILITY.md (every requirement with its status) |
| 4 | Rust toolchain and dependency summary | docs/DEPENDENCIES.md, Cargo.lock, rust-toolchain.toml, the CycloneDX SBOM in `dist/` (tools/sbom.py) |
| 5 | Supported platforms and language capabilities | docs/INSTALL.md (macOS arm64, Linux aarch64 and x86_64 verified; macOS x86_64 and Windows x86_64 cross-built only), docs/LANGUAGES.md (60 verified grammars, 12 more optional, heuristic tier) |
| 6 | Installation and usage instructions | docs/INSTALL.md, docs/CLI.md, README.md, integrations/*/README.md |
| 7 | Automated test results | docs/TEST_RESULTS.md (generated), bench/results/platforms/ (per-platform logs, wheel matrices, musl checks, reproducible build), bench/results/fuzz/ |
| 8 | Security assessment | docs/SECURITY.md |
| 9 | Reproducible competitive benchmarks | docs/LEADERBOARD.md (generated), bench/leaderboard.py, bench/indexed.py, bench/corpora.py (pinned corpora), bench/results/ (raw JSON and logs) |
| 10 | Performance and memory measurements | docs/LEADERBOARD.md (wall time, CPU time, peak memory per benchmark and tool; indexed latency percentiles, index time and size), docs/BENCHMARKS.md (start-up, concurrency, Engine B index metrics) |
| 11 | Documentation | docs/ (this directory), README.md, CHANGELOG.md, CONTRIBUTING.md |
| 12 | Remaining defects and incomplete requirements | docs/RELEASE_READINESS.md, docs/TRACEABILITY.md (PARTIAL, FAIL and BLOCKED rows) |
| 13 | Release-readiness assessment | docs/RELEASE_READINESS.md |
| 14 | Future engineering roadmap | docs/ROADMAP.md |
| 15 | Evidence-based competitive positioning | docs/COMPETITIVE.md |

RELEASE_ARTIFACTS

Previous work is preserved untouched in the sibling directory that held the Python prototype
("Titan"); its engine lives on here as `python/scrutate_ref`, the reference implementation the Rust
engine is verified against and the trainer for the bundled models.
