# Roadmap

What was planned after 0.1.0, what this release already did, what cannot be done without
publishing the repository, and what is next. Each item names the gap it closes in
docs/TRACEABILITY.md.

## Done in this release

| Item | Where |
|---|---|
| Searching inside archives (zip, tar) and compressed files (R-2A.2) | `grep -z`, `src/archive.rs`, docs/CLI.md |
| Look-around and backreferences with a step budget (R-2A.2) | `grep -P` (PCRE2, stopped by its match limit with a warning) |
| Fuzzy patterns longer than 128 bytes (R-2A.2) | `--fuzzy`, up to 4096 bytes (64-bit blocks with carries) |
| Fuzzy regex (R-2A.2) | `--fuzzy-regex[=N]`, `src/fuzzyre.rs` (bit-parallel Glushkov automaton, up to 64 positions) |
| Small trees (R-4.6) | a sequential walk that hands files to three search threads as it finds them, then the parallel walk past 256 files |
| One very long line (R-4.6) | read into one exactly sized buffer: 60.5 ms to 47.5 ms on a 100 MB line, ugrep 42.5 ms in the same run (with output turned off the two are level, so the rest is printing, including the UTF-8 check) |
| Incremental re-search for editors (R-4.1) | `grep --index --update` (refresh the text index, then search only candidate files); `watch` and `serve --watch` keep the index fresh |
| Structural search without re-parsing (R-2B.2) | `tsquery` keeps captures per file by content hash: the Linux tree took about 25 s, the repeated query 1.6 s |
| Cross-file reference resolution with scope awareness (R-2B.4) | `refs` resolves occurrences through scopes, imports and qualified names for Python, JavaScript/TypeScript, Rust and Go (60 of 60 hand-checked resolutions right) |
| Language server (R-6.15) | `scrutate lsp`; the VS Code extension starts it; JetBrains through LSP4IJ |
| Sample for `ocaml_interface` (R-3.4) | all 73 grammars verified by samples (docs/LANGUAGES.md) |
| int8 semantic vectors (R-4.3) | schema 13: index about a fifth smaller, ranking quality within 0.001 MRR (docs/QUALITY.md) |
| `scrutate watch` (R-2B.1) | inotify on Linux, ReadDirectoryChangesW on Windows, the index's own check on a timer on macOS |
| Benchmark dashboard (R-4.5) | `bench/make_dashboard.py` writes `docs/dashboard.html` from the results in `bench/results/` |
| Package definitions (R-12.13) | `packaging/`: Homebrew formula (installed and tested in a Linux Homebrew), Scoop and winget manifests (schema-validated), `.deb` and `.rpm` (built and install-tested in clean Debian and Fedora containers) |
| Third-party notices in releases | `tools/third_party_notices.py`; archives and wheels include `THIRD_PARTY_NOTICES.txt` |

AGENT_ITEMS

## Blocked: needs the repository published (declined for this release)

- Run the CI matrix and the release workflow; flip the Windows and Intel macOS rows to PASS only
  with a recorded run (R-6.1, R-9.10). Both are cross-built here and their test binaries link.
- Signed releases with Sigstore attestations (`release.yml` has the steps; they need the
  repository's OIDC identity) and the free-threaded wheel built in CI (built and tested locally).
- Publish the packages: tap, bucket and winget pull requests with the release checksums
  (packaging/README.md lists the steps).
- `cargo install scrutate`: crates.io refuses crates over 10 MB, and the semantic model is 14.6 MB.
  It needs a smaller model (int8 weights would halve it) or a download on first use.
- The GitHub App / GitLab integration that comments reports on merge requests (R-12.12), and
  weekly benchmark runs for the dashboard (R-4.5).

## Next

- Case-insensitive literals on many files: ugrep is about a fifth faster on the LLVM tree
  (docs/LEADERBOARD.md); profile its candidate search against the regex engine's.
- The long line: print without re-checking UTF-8 when the bytes are already valid, which needs a
  decision on how invalid bytes are shown (raw, as ripgrep does, or replaced, as now).
- References: type inference for method calls on variables; more languages (Java, C#, C/C++
  includes); TypeScript path aliases.
- `cargo install`: int8 weights for the semantic model, with the quality check QUALITY.md uses.
- Benchmark leadership on dedicated idle hardware (R-14.11): every number here comes from a
  developer laptop shared with other work.
