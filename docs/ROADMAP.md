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
| One very long line (R-4.6) | read into one exactly sized buffer, and its UTF-8 checked a piece at a time as it is written: 60.5 ms to about 42 ms on a 100 MB line, level with ugrep in the same runs |
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
| Incremental parsing of edited files (R-2B.1) | a long-running index (`watch`, `serve`, `lsp`) reparses an edited file against its previous tree, for the 21 grammars without an external scanner (with one, a reparse was seen to differ from a fresh parse); a 66 KB Go file: 5.5 ms to 2.0 ms per edit; `tests/incremental.rs` |
| Ranking quality beyond Python (R-2B.12) | ranker v8 fitted on Python plus Rust, Go, Java, JavaScript and TypeScript repositories; held-out results per language in docs/QUALITY.md |
| Indexed search precision (R-2B.1, R-4.6) | text index format 2: next-byte and position masks per posting; concurrent throughput against Zoekt from 4% behind to 5% ahead, p50 latency 0.46 of Zoekt's |
| Many files on macOS (R-4.6) | as many threads as performance cores when walking (kernel contention made 11 threads slower than 5); the Linux and LLVM trees, 200,000 small files and the binary-heavy tree went from behind or level with ugrep to 13 to 21% ahead |
| Regexes anchored at line starts (R-4.6) | only line starts beginning with the regex's prefix literals are examined: `^(a|aa)+$` on the pathological corpus, 52 ms to 18 ms |
| Start-up in git repositories (R-4.6) | `vendor/ignore` reads the global excludes path without compiling a regex: 0.4 ms of a 6 ms search of a 220-file repository |

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

- Small trees (a few hundred files): Scrutate and ugrep are within about 10% of each other and the
  order changes with machine load. Scrutate starts 1.2 ms sooner; ugrep walks the directories and
  matches ignore rules in less time (it compiles no glob sets and does not read the global git
  excludes or `.git/info/exclude`). Next: in the vendored walker, idle threads sleep 1 ms between
  checks for work; a wait that starts at 10 µs and doubles took the parallel walk of the ripgrep
  repository from 8.5 to 6.2 ms (measured, not shipped: the default one-thread walk was level, and
  the release binary is the one the leaderboard measured). Then read each directory's ignore files
  from its listing on the one-thread walk, and build glob sets lazily. Two more measured options:
  building for macOS 13 or newer gives the binary chained fixups, applied as pages are first
  touched rather than all at launch (start-up 2.6 ms to 2.4 ms; it did not change any of the five
  results, and it would drop macOS 11 and 12); compiling the pattern while the walk starts saved
  nothing measurable. The remaining gap is about 0.2 to 0.3 ms of a 5 ms run, spread over start-up
  (an 85 MB binary: 0.3 ms more than a small one) and the first search in a fresh process (0.4 ms
  more than later ones). Profile-guided optimisation (a profile from the benchmark workloads)
  measured 2 to 3.5% on the 220-file tree, about 1% on matching-bound searches and up to 8% on
  output-bound ones; not adopted for 0.1.0, since it needs a 13 MB profile in the repository and in
  every release build for less than the gaps above.
- Linux: in the 8-CPU Linux VM on the development Mac, Scrutate used about 10% more CPU than
  ripgrep for the same Linux-tree search (993 ms against 910 ms for `PM_RESUME`). Two measured
  steps: read small files into one buffer per thread instead of a new zeroed buffer per file (4% of
  the CPU of a `return` search), and skip the one-thread small-tree phase there (about 5 ms of a
  130 ms search, since files are cheap to open on Linux). Then repeat on dedicated Linux hardware,
  where the VM's noise (ripgrep's own medians moved by up to 50% between runs) does not hide them.
- Thread counts on Linux and Windows: the macOS default (performance cores) came from measurement;
  repeat it on dedicated Linux and Windows machines before changing their default. The disk-or-cache
  test (75 µs per small read) also needs a check there: in the VM under load it may misfire.
- References: type inference for method calls on variables; more languages (Java, C#, C/C++
  includes); TypeScript path aliases.
- `cargo install`: int8 weights for the semantic model, with the quality check QUALITY.md uses.
- Benchmark leadership on dedicated idle hardware (R-14.11): every number here comes from a
  developer laptop shared with other work.
