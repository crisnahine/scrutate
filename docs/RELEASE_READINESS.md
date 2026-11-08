# Release-readiness assessment — Scrutate 0.1.0

**Verdict: 0.1.0 is a verified first release for Linux x86_64 developer use. It is not declared
production-ready for mission-critical environments**, because the criteria the mission sets for
that claim are not all met yet (listed below). Nothing in this document is estimated: every
PASS refers to a test, benchmark or audit output in this repository.

## What the release gates say (mission §10)

| Gate | Result |
|---|---|
| Functional universal instant search | **met** — Engine A, differential agreement with ripgrep on 750 random cases |
| Unknown-language generic search | **met** — every readable file, no configuration |
| Reliable filesystem traversal | **met** — ignore rules, symlink loops, dangling links, permissions, 255-byte names, unicode paths |
| Ignore rules and advanced filtering | **met** |
| Literal and regex search | **met** — linear-time regex with size limits |
| Machine-readable output | **met** — JSON Lines (schema 1) and JSON on every command |
| Functioning, tested code-intelligence foundation | **met** — 60 verified grammars, call graph, imports, ranked search with measured quality, 13 end-to-end tests |
| Automated tests | **met** — 51 Rust + 91 Python tests, property and differential suites |
| Reproducible competitive benchmarks | **met** — `bench/compare_tools.py`, results and generator committed |
| Verified builds for declared supported platforms | **met for Linux x86_64 only**; no other platform is declared |
| Security assessment | **met as a self-assessment**; no independent audit |
| Professional documentation | **met** |
| Installation instructions | **met** |

## Why not "production-ready" yet

1. **Platform coverage.** macOS, Windows and Linux aarch64 builds, the musl static binary and the
   wheel matrix exist only as CI definitions that have never run. The mission forbids declaring a
   platform supported before its binary and wheel were installed and tested there.
2. **No independent security review.** The assessment in docs/SECURITY.md is the authors' own;
   there has been no third-party audit, no coverage-guided fuzzing and no sanitizer run.
3. **Release pipeline unexercised.** Signed releases, checksums, SBOM attachment and package
   distribution are implemented in `release.yml` but no release has been produced by it.
4. **Operational experience.** The index lifecycle (compaction, recovery, concurrent writers) is
   tested in minutes-long runs, not weeks of real use; network filesystems are untested and
   documented as unsupported for the index directory.

## Remaining defects and incomplete requirements

Status words as in docs/TRACEABILITY.md. "Resolution" says what closes the item.

| ID | Item | Status | Resolution |
|---|---|---|---|
| R-3.4 (`extra-langs` feature) | the 12 optional grammars have no snapshot samples, so they stay off by default and unverified | PARTIAL | add samples and expectations (ROADMAP 0.3) |
| R-4.1 | Adaptive concurrency (thread count is fixed at min(cores, 12)) | PARTIAL | load-aware scheduling if measurements justify it |
| R-4.3 | Benchmarks against ack and ag | BLOCKED here | run `bench/compare_tools.py` on a machine with ack/ag installed; the harness already supports them |
| R-4.7 | Faster than every competitor on every workload | PARTIAL | ripgrep is 2 % faster on the geometric mean and up to 25 % on output-heavy queries (single output thread); ast-grep is 6–11 % faster on structural search — see ROADMAP 0.3 |
| R-6.1 / R-9.10 / R-12.2 | macOS, Windows, Linux aarch64 verification | PARTIAL | run the CI matrix; flip rows only with the recorded run |
| R-6.11 | Reproducible builds across machines | PARTIAL | local two-build identity verified; add a second-builder check in CI |
| R-6.12 | CI/CD executed | PARTIAL | first push to the repository triggers `ci.yml` |
| R-6.15 / R-12.16 | VS Code, Neovim, JetBrains, GitHub/GitLab/Bitbucket integrations exercised in their hosts | PARTIAL | manual acceptance in each host; publish the VS Code extension; a JetBrains plugin is roadmap work |
| R-7.2 | Domains and trademarks for the name | PARTIAL | WHOIS check and a trademark search by counsel before commercial use |
| R-9.6 | Coverage-guided fuzzing | PARTIAL | `cargo fuzz` on a nightly toolchain in CI (parser, npy reader, fuzzy matcher) |
| R-12.5 | UTF-16 files in Engine B | documented limitation | Engine A searches them (BOM transcoding); Engine B indexes them as binary-skipped; add transcoding to the indexer |
| R-12.10 | Network filesystems | BLOCKED here | document (done) and test on NFS/SMB; consider an index-location override |
| R-12.13 | Structured diagnostics (errors as JSON) | PARTIAL | add `--json` error objects to stderr diagnostics |
| R-12.17 | Package managers, signed releases, published checksums | PARTIAL | first tagged release through `release.yml`; Homebrew/Scoop/winget/cargo-install in 0.2 |
| R-13.6 | abi3 wheel tested on several Python versions | PARTIAL | tested on 3.13 here; 3.10/3.12 in CI |
| R-13.7 | Free-threaded `cp314t` wheel tested | PARTIAL | needs a free-threaded interpreter (CI) |
| R-13.8 | musl static binary, identical output, measured difference | BLOCKED here | `rustup target add x86_64-unknown-linux-musl` cannot download in this environment; the CI job does all three checks |

Defects found and fixed during this release's own testing (recorded so the test value is visible):
a UTF-8 boundary panic in the language sniffer on files with multibyte characters near byte 4000;
definition-less files losing everything after line 60; a prose-derived bogus symbol from the
keyword heuristics on grammar-parsed files; identifier search ignoring digits
(`makeWidget3` ≠ `make_widget_3`); a compaction race that could drop another writer's segment from
the manifest; a transient SQLite lock being misdiagnosed as corruption (which would have deleted a
live database); a 6 GB memory blow-up building one segment for a 1.08 M-chunk corpus (now bounded
by 100 k-row segment flushes and a 300 k-row merge cap); a 1–2 s open-time integrity scan on
gigabyte indexes (now size-gated with error-driven recovery instead); the `zip` crate's four
advisories (replaced) and `pyo3` 0.27's two (upgraded).

## Recommendation

Ship 0.1.0 as a developer release for Linux x86_64 with the documentation as written. Run the
CI/release workflows on the first push; promote platforms to "supported" and the release to
"production-candidate" only as the rows above turn to PASS with recorded evidence.
