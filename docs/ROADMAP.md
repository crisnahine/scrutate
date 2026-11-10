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
| One definition of every index operation (R-6.15, R-13) | `ops.rs`: the CLI, the MCP server, `batch` and the Python bindings share arguments, defaults, limits, text and JSON; they had drifted (three limits for `callers`, references textual in MCP and resolved in the CLI) |
| An MCP server that fits how agents work today (R-6.15) | read-only and local hints on every tool, server instructions, answers within a byte budget with an offset to continue, `grep_code` on Engine A, no tool call blocked for more than 20 s by a first index build; from docs/research/agent-facing-search-2026-11.md |
| Index upkeep in one place (R-12.4) | `Repo`: update, and rebuild when the database is corrupt, for every front end that stays open |
| The index never disagrees with the walk about a pattern (R-2B.1) | one translation of the pattern for Engine A's matcher and the text index's trigram condition, checked by an in-process test |
| Linux-tree searches against ripgrep (R-4.6) | a busy `.gitignore` compiles its globs; files up to 1 MB read whole; no size lookup per file: in the test VM from 2 to 6% behind ripgrep to 6 to 12% ahead |

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

- Branch `perf/next` (local) holds changes made after these benchmark runs: one reusable
  small-file buffer per search thread, a page-cache-or-disk test that counts slow reads, the macOS
  13 target, gitignore lines matched without a regex, the parallel walk from the start for any root
  that holds directories (growing its threads when the reads come from the disk), lazy binding of
  133 system functions, and a grep-only argument parser. Checked with the leaderboard harness on
  the same laptop, idle (30 runs each), against the release run's ratios: `ripgrep/literal` 1.051
  to 0.806, `ripgrep/simple regex` 1.076 to 0.853, `ripgrep/case-insensitive` 1.076 to 0.882,
  `rgbuilt/git-aware` 1.022 to 0.750, `patho/backtracking trap` 1.024 to 0.977 (all five now
  PASS); `patho/long line` 0.983 to 0.979, `patho/one-byte lines` 0.885 to 0.888,
  `binheavy/literal` 0.816 to 0.766; `linux/literal` 0.839 and `linux/no match` 0.814 on the
  corpus without hard links (below). Merge the branch with the next full run.
- Small trees (a few hundred files), what the measurements said: Scrutate started sooner than
  ugrep all along (`--version` 2.4 ms against 3.0 ms), so the gap on the ripgrep repository was
  the walk and search of its 211 files. Four causes, fixed on the branch: each ignore file built a
  `GlobSet` regex (100 to 400 µs, the global excludes file 0.3 ms of every run); one thread read
  all 60 directories while the search threads waited; dyld bound 133 imported functions a search
  never calls (0.4 ms); clap built all 24 subcommands (0.1 ms). Ruled out by measurement:
  code-signature page size, the binary's pointer fix-ups, its size (0.05 to 0.2 ms at 85 MB), code
  layout (a linker order file putting the 979 functions a search runs first: 585 page faults
  against 586), and waking idle walker threads through a condition variable instead of short
  sleeps (slower: 3.9 ms against 3.7). Profile-guided optimisation measured 2 to 3.5% on the
  220-file tree; not adopted, since it needs a 13 MB profile in the repository and in every release
  build. What is left of a 3.8 ms search: 1.5 ms is the cost of starting any Rust program, and
  opening 211 files costs 8 µs each.
- Large trees on macOS are bound by `open` (docs/ARCHITECTURE.md): the Linux tree takes 0.56 s to
  open and read on the five performance cores with no search at all, and a search takes 0.65 s.
  The benchmark corpora `linux` and `linux-nodot` shared files through hard links, which doubles
  the cost of every open on macOS for every tool; `bench/corpora.py` now makes copies (APFS
  clones). The published `linux/*` rows were measured with the hard links and need the next full
  run.
- Agents, next: `outputSchema` and `structuredContent` on the MCP tools (Codex turns an output
  schema into a typed return value for its scripts; clients still differ on which form reaches
  the model, so the text stays); the stateless protocol revision 2026-07-28 (`server/discover`,
  `resultType`, cache hints on `tools/list`; today clients fall back to the handshake); honouring
  `notifications/cancelled` (a tool call runs on the thread that reads requests); symbol-scoped
  reads (`read_code` by symbol name) and result ids an agent can pass back. Sources and client
  support are in docs/research/agent-facing-search-2026-11.md.
- Known limits left in place: smart case does not see `\x41` as an uppercase letter; the LSP
  server's save worker updates the index without the corrupt-database rebuild; `-U` is ignored by
  `--fuzzy` and `--fuzzy-regex`.
- Linux: in the 8-CPU Linux VM on the development Mac, Scrutate used about 10% more CPU than
  ripgrep for the same Linux-tree search (993 ms against 910 ms for `PM_RESUME`). Two measured
  steps, both on `perf/next`: read small files into one buffer per thread instead of a new zeroed
  buffer per file (4% of the CPU of a `return` search), and skip the one-thread small-tree phase
  (about 5 ms of a 130 ms search); the branch also drops the regex build for most of the tree's 407
  `.gitignore` files. Then repeat on dedicated Linux hardware,
  where the VM's noise (ripgrep's own medians moved by up to 50% between runs) does not hide them.
- Thread counts on Linux and Windows: the macOS default (performance cores) came from measurement;
  repeat it on dedicated Linux and Windows machines before changing their default. The disk-or-cache
  test (75 µs per small read) also needs a check there: in the VM under load it may misfire.
- References: type inference for method calls on variables; more languages (Java, C#, C/C++
  includes); TypeScript path aliases.
- `cargo install`: int8 weights for the semantic model, with the quality check QUALITY.md uses.
- Benchmark leadership on dedicated idle hardware (R-14.11): every number here comes from a
  developer laptop shared with other work.
