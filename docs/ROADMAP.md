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
| Small trees (R-4.6) | the parallel walk from the first directory of any root that holds directories (one thread used to read all 60 directories of a 211-file repository while the search threads waited), and a sequential walk feeding three search threads only for a flat root of up to 256 files; ignore files matched without a regex (each built a `GlobSet`: 100 to 400 µs, the global excludes file 0.3 ms of every run); 133 system functions bound on first use (0.4 ms); a grep-only argument parser (0.1 ms) |
| One very long line (R-4.6) | read into one exactly sized buffer, each thread filling its own part (100 MB: 12.6 ms on one thread, 4.0 ms on five), and its UTF-8 checked a piece at a time as it is written: 60.5 ms to 32 ms on a 100 MB line (ugrep 38.8 ms) |
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
| Indexed search precision (R-2B.1, R-4.6) | text index format 2: next-byte and position masks per posting; with it, concurrent throughput against Zoekt went from 4% behind to 5% ahead and the p50 latency to 0.46 of Zoekt's (20% ahead and 0.37 in the final run) |
| Many files on macOS (R-4.6) | as many threads as performance cores when walking (kernel contention made 11 threads slower than 5); the Linux and LLVM trees, 200,000 small files and the binary-heavy tree went from behind or level with ugrep to 13 to 21% ahead (15 to 30% in the final run) |
| Regexes anchored at line starts (R-4.6) | only line starts beginning with the regex's prefix literals are examined: `^(a|aa)+$` on the pathological corpus, 52 ms to 18 ms (13 ms since its one-line file is read on several threads) |
| Start-up in git repositories (R-4.6) | `vendor/ignore` reads the global excludes path without compiling a regex: 0.4 ms of a 6 ms search of a 220-file repository |
| One definition of every index operation (R-6.15, R-13) | `ops.rs`: the CLI, the MCP server, `batch` and the Python bindings share arguments, defaults, limits, text and JSON; they had drifted (three limits for `callers`, references textual in MCP and resolved in the CLI) |
| An MCP server that fits how agents work today (R-6.15) | read-only and local hints on every tool, server instructions, answers within a byte budget with an offset to continue, `grep_code` on Engine A, no tool call blocked for more than 20 s by a first index build; from docs/research/agent-facing-search-2026-11.md |
| Index upkeep in one place (R-12.4) | `Repo`: update, and rebuild when the database is corrupt, for every front end that stays open |
| The index never disagrees with the walk about a pattern (R-2B.1) | one translation of the pattern for Engine A's matcher and the text index's trigram condition, checked by an in-process test |
| Linux-tree searches against ripgrep (R-4.6) | a busy `.gitignore` compiles its globs; files up to 1 MB read whole; no size lookup per file: in the test VM from 2 to 6% behind ripgrep to 7 to 16% ahead on 11 of the 13 text rows, 40% ahead with very many matches and 59% ahead on the complex regex |
| Cold trees on Linux (R-4.6) | a read counts as a disk read at 30 µs there (the macOS value, 75 µs, never fired), and a walk that reads from the disk grows by a thread for every 16 slow reads, up to 64: the four cold Linux-tree rows went from level with git grep (two lost by under 1%) to 0.28 to 0.30 of the best competitor's time, 18 to 24% above the read-files ceiling |
| Heavy output through a pipe (R-4.6) | output that follows other output within a millisecond waits in a 64 KB buffer and leaves in one write, where every matching file used to cost a write and a wake-up of the reader: `return` in the Linux tree (1.3 million lines) in the Linux VM, from 0.95 of ripgrep's time to 0.60 |
| A read-files ceiling that is one (R-14.8) | `bench/rawread.c` gives each thread 64 neighbouring paths at a time, and the harness keeps the fastest of several thread counts; handing out one path at a time, its threads shared a directory and read a cold tree at a third of the speed the disk allows |
| Measurements a busy machine spoiled (R-14.9) | a benchmark where some tool's block medians differ by more than 25% is measured again, once, whatever its status, and keeps its first result beside the second; a warm row records how much of its corpus was in the page cache (on this 18 GB laptop the LLVM tree, 4.9 GB in 16 KB pages, no longer fits once a browser and an editor are open) |
| What agents ask of an MCP server now (R-6.15) | revision 2026-07-28 beside the handshake revisions (`server/discover`, a version in each request, `resultType`, cache hints on lists); with `serve --structured`, an `outputSchema` on every tool and the `--json` form of each answer in `structuredContent` (off by default: Claude Code shows its model that JSON in place of the text); `notifications/cancelled` honoured, each call on a thread of its own; progress reports while the first index is built; `read_code` of a whole definition by name; the location at the start of every result accepted wherever a tool takes a symbol |
| Method calls on variables (R-2B.4) | `x.m()` resolves through the type the declaration of `x` states, as an annotation or as the thing it is built from, in Python, JavaScript/TypeScript, Rust and Go; Go methods are found by their receiver type across the files of a package |
| TypeScript path aliases (R-2B.4) | an import through `paths` or `baseUrl` of the nearest `tsconfig.json` or `jsconfig.json` (comments, trailing commas and a relative `extends` included) resolves to its file, in `refs`, `deps` and `rdeps` |
| Limits left in 0.1.0's first draft (R-2A, R-12.4) | smart case sees a letter written by its code point (`\x41`); the LSP server replaces a damaged index, from a save or from a request; the watcher's test of one path decides in the walk's order (a `.ignore` at any level before every `.gitignore`) |
| Benchmark corpora without hard links (R-14) | `bench/corpora.py` copies (APFS clones) where it linked: a hard-linked file costs every tool twice as long to open on macOS |

## Blocked: needs the repository published (declined for this release)

- Run the CI matrix and the release workflow; flip the Windows and Intel macOS rows to PASS only
  with a recorded run (R-6.1, R-9.10). Both are cross-built here and their test binaries link.
- Signed releases with Sigstore attestations (`release.yml` has the steps; they need the
  repository's OIDC identity) and the free-threaded wheel built in CI (built and tested locally).
- Publish the packages: tap, bucket and winget pull requests with the release checksums
  (packaging/README.md lists the steps).
- `cargo install scrutate`: crates.io refuses crates over 10 MB, and the semantic model is 14.6 MB.
  It needs a smaller model (int8 weights would halve it) or a download on first use. A published
  crate also cannot carry the `[patch]` that points at `vendor/ignore`: the four changes there
  have to go upstream or out as a fork first.
- The GitHub App / GitLab integration that comments reports on merge requests (R-12.12), and
  weekly benchmark runs for the dashboard (R-4.5).

## Next

- Small trees (a few hundred files), what is left: of a 3.6 ms search of a 211-file repository,
  1.5 ms is the cost of starting any Rust program, and opening the files costs 8 µs each. Ruled
  out by measurement: code-signature page size, the binary's pointer fix-ups, its size (0.05 to
  0.2 ms at 85 MB), code layout (a linker order file putting the 979 functions a search runs
  first: 585 page faults against 586), and waking idle walker threads through a condition
  variable instead of short sleeps (slower: 3.9 ms against 3.7). Profile-guided optimisation
  measured 2 to 3.5% on the 220-file tree; not adopted, since it needs a 13 MB profile in the
  repository and in every release build.
- Large trees on macOS are bound by `open` (docs/ARCHITECTURE.md): the Linux tree takes 0.52 s to
  open and read on the five performance cores with no search at all, and a search takes 0.58 s.
  The benchmark corpora `linux` and `linux-nodot` shared files through hard links, which doubles
  the cost of every open on macOS for every tool; `bench/corpora.py` now makes copies (APFS
  clones). What is left above the floor is the search itself. Opening a file through its
  directory's descriptor (`openat`) does not lower the floor: 13.3 µs a file against 12.4 µs by
  path.
- Agents, next: results an agent can page in the structured form too (today a large answer
  pages as text, and its `structuredContent` carries that text and a `null` result); stopping an Engine A search that is
  already running when its call is cancelled (its answer is dropped today); the count of files
  indexed so far in progress reports; a `response_format` switch for shorter answers. Sources
  and client support are in docs/research/agent-facing-search-2026-11.md.
- The watcher's test of one path (`files::Membership`) still reads the ignore files by hand,
  because the walker's own per-directory matcher is private to the `ignore` crate; a test holds
  it to the walk's answers.
- Linux on dedicated hardware: every Linux number here comes from a 5-CPU VM on the development
  Mac, whose disk is a virtual one on the Mac's SSD. Two values were fitted there and need a
  check on an NVMe disk and on a network disk: the read time that counts as a disk read (30 µs)
  and the most threads for a cold tree (64). A cold tree also costs memory there: about 90 MB
  on 64 threads against 16 MB warm (`-j` bounds both).
- Windows: nothing is measured. Its thread count (the cores, at most 12), its cold thread count
  (twice the cores, at most 24) and its disk-read time (75 µs) are the macOS values.
- References: the type of a variable that got its value from a function call (the function's
  declared return type), of a field (`self.x.m()`) and of a module-level variable; more
  languages (Java, C#, C/C++ includes).
- `cargo install`: int8 weights for the semantic model, with the quality check QUALITY.md uses.
- Benchmark leadership on dedicated idle hardware (R-14.11): every number here comes from a
  developer laptop shared with other work, and Linux x86_64 has no benchmark run at all. In the
  last macOS run six of 37 warm rows had to be measured twice for that reason. An x86_64 run
  under emulation on this laptop was set up and dropped: gcc crashes there building GNU grep,
  and ripgrep's allocator reports that it falls back to a slower path, so the times would
  measure the emulator. `bench/competitors/linux_bench.sh` takes an x86_64 host as it is.
