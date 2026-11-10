# Release-readiness assessment: Scrutate 0.1.0

**Verdict: 0.1.0 is a verified first release for developer use on the platforms INSTALL.md
declares (macOS arm64, Linux aarch64, Linux x86_64). It is not declared production-ready for mission-critical
environments**, because the criteria the mission sets for that claim are not all met (listed
below). Every PASS refers to a test, benchmark or audit output in this repository.

## What the release gates say (mission section 10)

| Gate | Result |
|---|---|
| Functional universal instant search | **met**: Engine A, differential agreement with ripgrep; every leaderboard answer matches an independent reference |
| Unknown-language generic search | **met**: every readable file, no configuration |
| Reliable filesystem traversal | **met**: ignore rules, symlink loops, dangling links, permissions, long and unicode names, case-colliding checkouts, SMB shares |
| Ignore rules and advanced filtering | **met** |
| Literal and regex search | **met**: linear-time regex with size limits |
| Machine-readable output | **met**: JSON Lines (schema 1) and JSON on every command, JSON error records |
| Functioning, tested code-intelligence foundation | **met**: 60 + 12 verified grammars, call graph, imports, ranked search with measured quality, text index equal to Engine A |
| Automated tests | **met**: 104 Rust tests, 82 Python reference tests, bindings tests on four or five interpreters per platform, Rust/Python parity, 2,000 differential cases against ripgrep, fuzz regressions and a reproducible build, all passing (docs/TEST_RESULTS.md) |
| Reproducible competitive benchmarks | **met**: `bench/leaderboard.py`, `bench/indexed.py`, pinned corpora, raw results in `bench/results/` |
| Verified builds for declared supported platforms | **met for the declared platforms**; others are not declared |
| Security assessment | **met as a self-assessment** with fuzzing; no independent audit |
| Professional documentation | **met** |
| Installation instructions | **met** |

## Why not "production-ready" yet

1. **Platform coverage.** Windows and Intel macOS hardware were not available. Both binaries are
   cross-built and their test binaries compile and link, but none of them ran; the CI matrix that
   would run them has not run either, because publishing the repository was declined for this
   release. Linux x86_64 was tested under emulation, not on x86 hardware.
2. **No independent security review**; fuzzing ran in ten-minute rounds on one machine.
3. **Release pipeline unexercised**: signed releases, attestations and package-manager packages exist
   only as workflow definitions.
4. **Benchmark leadership is not proven on dedicated hardware** (section 14 requires it for #1
   claims), and some benchmarks are lost (docs/LEADERBOARD.md).

## Remaining defects and incomplete requirements

Status words as in docs/TRACEABILITY.md. "Resolution" says what closes the item.

| ID | What is missing | Status | What closes it |
|---|---|---|---|
| R-4.6, R-14.16 | #1 on every benchmark: on macOS 5 warm benchmarks under 18 ms are lost to ugrep by 2 to 8% and the indexed p95 is a tie; in the Linux VM 11 benchmarks tie with ripgrep and `-i KÖNIG` is lost (1.16x) | PARTIAL | the measured next steps in docs/ROADMAP.md (small-tree walk, Linux CPU), then a run on dedicated hardware |
| R-14.11 | Results from dedicated, otherwise idle hardware | BLOCKED | a dedicated Mac and Linux machine; the harness and corpora are ready |
| R-14.12 | A second index mode (file contents in the index) to trade size for latency | PARTIAL | implement and benchmark it; the p95 against Zoekt is the case it would decide |
| R-6.1, R-9.10, R-12.2, R-13.9 | Windows and Intel macOS executed, not only cross-built | PARTIAL | a Windows machine or the CI matrix; Intel macOS with Rosetta 2 installed on the build Mac |
| R-6.12, R-14.15, R-12.18 | CI runs, benchmark regression tracking in CI | PARTIAL | publishing the repository (declined for this release) |
| R-12.17 | Signed releases and published packages | PARTIAL | the release workflow's Sigstore steps and the package pull requests, which need the published repository |
| R-6.15, R-12.16 | GitHub App / GitLab integration exercised against a real platform | PARTIAL | the published repository |
| R-12.15 | The cross-platform part of verification | PARTIAL | as R-9.10 |
| R-7.2 | Trademark search by counsel | PARTIAL | counsel's search of the trademark registers |

## Defects found and fixed while preparing this release

Recorded so the value of the testing is visible: four defects found by fuzzing (chunk line numbers
shifted after form feeds; a panic on truncated `.npy` string arrays; an element count that wrapped
in release builds and could have produced an over-long slice; a multi-terabyte allocation request
for zero-width strings); a blocking file lock that fails at once on SMB shares (EAGAIN); broken
symbol rules for D, LLVM IR, AutoHotkey, VHDL architectures and OCaml modules (found by writing
their samples); a test that silently depended on a `git` binary; about 1.5 ms of start-up spent
loading Foundation and libobjc through the `ctrlc` crate; a file given directly as the search root
printed with an empty path (`:1:text`, or `one.txt/` next to a second root); a test that needed a
`kill` binary, which minimal Linux images do not have; and a GIL-release test that could not show
what it claimed, because every line matched and the time went into building Python objects (it
failed on x86_64 and was skipped elsewhere; it now searches one large file for one match, and four
threads take a third of the sequential time); `--max-results N` reporting one or two matches more
than N when several threads found matches at once (caught by the bindings test, now covered by a
CLI test that repeats the search sixty times); and a bundled SQLite with a published CVE
(RUSTSEC-2026-0459, found by `cargo audit` on the release candidate; upgraded). In the benchmark
harness itself: an ast-grep pattern that ast-grep parses as a C type macro, so the Linux structural
benchmark compared Scrutate with a tool that could match nothing; a reference that let
git's case-insensitive ignore matching hide 1,343 files (every tool agreed against it, which is how
it was caught); a reference that used Python's backtracking regex engine on the backtracking-trap
benchmark and never finished; and a musl/glibc comparison that compared the run time recorded in
the JSON summary, and unordered output.

## Recommendation

Use 0.1.0 for development work on macOS arm64 and Linux: both engines are verified there, the
results are reproducible from this repository, and the remaining gaps are listed above with what
closes each. Before calling it production-ready: run the CI matrix (publishing the repository),
execute the Windows and Intel macOS builds, get an independent security review, and repeat the
benchmarks on dedicated hardware.
