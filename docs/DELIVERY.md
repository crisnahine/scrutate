# Delivery index: Scrutate 0.1.0 (2026-11-11)

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
| 11 | Documentation | docs/ (this directory, with the release checklist in docs/RELEASING.md), README.md, CHANGELOG.md, CONTRIBUTING.md, GLOSSARY.md |
| 12 | Remaining defects and incomplete requirements | docs/RELEASE_READINESS.md, docs/TRACEABILITY.md (PARTIAL, FAIL and BLOCKED rows) |
| 13 | Release-readiness assessment | docs/RELEASE_READINESS.md |
| 14 | Future engineering roadmap | docs/ROADMAP.md |
| 15 | Evidence-based competitive positioning | docs/COMPETITIVE.md |

## Release artifacts (built locally, not published)

`tools/release_local.sh` on the release commit writes them to `dist/` (macOS) and, run in a clean
`rust:1-slim-bookworm` container, to `dist-linux/` (neither is in git; `SHA256SUMS` in each lists
the files' checksums). Every archive holds the binary, LICENSE, README.md, CHANGELOG.md,
THIRD_PARTY_NOTICES.txt, docs/ and integrations/. The `.deb` and `.rpm` packages
(`packaging/linux/build-packages.sh`) carry THIRD_PARTY_NOTICES.txt too.

| Artifact | Platform | Binary SHA-256 |
|---|---|---|
| `scrutate-0.1.0-aarch64-apple-darwin.tar.gz` | macOS arm64 | `9aa0a14ec4b0e10b...`, the binary docs/LEADERBOARD.md measured on macOS |
| `scrutate-0.1.0-cp39-abi3-macosx_13_0_arm64.whl` | macOS arm64 (macOS 13 or newer), standard CPython 3.9 to 3.14 | |
| `scrutate-0.1.0-cp314-cp314t-macosx_13_0_arm64.whl` | macOS arm64, free-threaded CPython 3.14 | |
| `scrutate-0.1.0-cp315-abi3.abi3t-macosx_13_0_arm64.whl` | macOS arm64, CPython 3.15 and newer, standard and free-threaded | |
| `scrutate-0.1.0-aarch64-unknown-linux-gnu.tar.gz` | Linux aarch64 (glibc) | `261ed1a2a4613e19...`, identical from two build images (`bench/results/platforms/repro/`) and the binary measured in the Linux VM |
| `scrutate-0.1.0-cp39-abi3-manylinux_2_34_aarch64.whl` | Linux aarch64, standard CPython 3.9 to 3.14 | |
| `scrutate-0.1.0-cp314-cp314t-manylinux_2_34_aarch64.whl` | Linux aarch64, free-threaded CPython 3.14 | |
| `scrutate-0.1.0-cp315-abi3.abi3t-manylinux_2_34_aarch64.whl` | Linux aarch64, CPython 3.15 and newer, standard and free-threaded | |
| `scrutate-0.1.0.cdx.json` | | CycloneDX SBOM of the binary's dependencies |

`tools/release_local.sh` builds one wheel of each kind when `uv` finds the interpreters (3.12,
3.14t and 3.15), and only the one for `python3` otherwise. The static musl binary is built and
tested by `tools/linux_verify.sh` but not packaged here. Signed archives, attestations and the
other platforms come from `.github/workflows/release.yml` once the repository is public.

Previous work is preserved untouched in the sibling directory that held the Python prototype
("Titan"); its engine lives on here as `python/scrutate_ref`, the reference implementation the Rust
engine is verified against and the trainer for the bundled models.
