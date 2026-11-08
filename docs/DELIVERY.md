# Delivery index — Scrutate 0.1.0 (2026-11-08)

The fifteen items the mission asks for (§11), each with where it lives in this repository.

| # | Item | Location |
|---|---|---|
| 1 | Project name and naming research | docs/NAMING.md, docs/name_research.json, tools/name_research.py |
| 2 | Complete source code and architecture | crates/scrutate (engines, CLI, MCP), crates/scrutate-py (bindings), python/scrutate_ref (reference implementation and trainer), models/; docs/ARCHITECTURE.md |
| 3 | Implemented capabilities | README.md, docs/CLI.md, docs/TRACEABILITY.md (every requirement with status) |
| 4 | Rust toolchain and dependency summary | docs/DEPENDENCIES.md, Cargo.lock, rust-toolchain.toml, dist/scrutate-0.1.0.cdx.json (SBOM) |
| 5 | Supported platforms and language capabilities | docs/INSTALL.md (platforms: Linux x86_64 verified; others pending CI), docs/LANGUAGES.md (60 verified grammars + heuristic tier) |
| 6 | Installation and usage instructions | docs/INSTALL.md, docs/CLI.md, README.md, integrations/*/README.md |
| 7 | Automated test results | docs/TEST_RESULTS.md (generated: fmt, clippy, 51 Rust tests, 91 Python tests, audit, parity, differential, reproducible build) |
| 8 | Security assessment | docs/SECURITY.md |
| 9 | Reproducible competitive benchmarks | docs/BENCHMARKS.md (generated), bench/compare_tools.py, bench/results/final.json, bench/baseline.json, bench/regression.py |
| 10 | Performance and memory measurements | docs/BENCHMARKS.md (latency, throughput, CPU, peak RSS, startup, concurrency, cold cache, index build/size, query latency) |
| 11 | Documentation | docs/ (this directory), README.md, CHANGELOG.md, CONTRIBUTING.md |
| 12 | Remaining defects and incomplete requirements | docs/RELEASE_READINESS.md ("Remaining defects and incomplete requirements"), docs/TRACEABILITY.md (PARTIAL/BLOCKED rows) |
| 13 | Release-readiness assessment | docs/RELEASE_READINESS.md |
| 14 | Future engineering roadmap | docs/ROADMAP.md |
| 15 | Evidence-based competitive positioning | docs/COMPETITIVE.md |

Release artifacts built locally for Linux x86_64 (`tools/release_local.sh`): `dist/scrutate-0.1.0-x86_64-unknown-linux-gnu.tar.gz`
(binary, licences, docs, integrations), `dist/scrutate-0.1.0-cp39-abi3-manylinux_2_34_x86_64.whl`,
`dist/scrutate-0.1.0.cdx.json` (CycloneDX SBOM), `dist/SHA256SUMS`. Artifacts for other platforms, the
musl static binary, free-threaded wheels and Sigstore attestations come from `.github/workflows/release.yml`,
which has not been executed yet.

Previous work is preserved untouched in the sibling directory that held the Python prototype ("Titan");
its engine lives on here as `python/scrutate_ref` (the reference implementation the Rust engine is
verified against, and the trainer for the bundled models).
