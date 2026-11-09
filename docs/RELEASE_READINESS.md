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

REMAINING_TABLE

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
threads take a third of the sequential time). In the benchmark harness itself: a reference that let
git's case-insensitive ignore matching hide 1,343 files (every tool agreed against it, which is how
it was caught); a reference that used Python's backtracking regex engine on the backtracking-trap
benchmark and never finished; and a musl/glibc comparison that compared the run time recorded in
the JSON summary, and unordered output.

## Recommendation

RECOMMENDATION
