<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-roadmap.svg" alt="Roadmap. What is done, what is blocked and what is next?" width="880">
</p>

# Roadmap

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Dependencies](DEPENDENCIES.md) &nbsp;·&nbsp; Next: [Research](RESEARCH.md)

**Contents**: [Done in this release](#done-in-this-release) &nbsp;·&nbsp; [Blocked: needs the repository published](#blocked-needs-the-repository-published) &nbsp;·&nbsp; [Next](#next)

<!-- End of head. -->

The plan after 0.1.0: done in this release, blocked on publishing, and next. Each item names the
gap it closes in docs/TRACEABILITY.md.

## Done in this release

| Item | Where |
|---|---|
| Searching inside archives (zip, tar) and compressed files (R-2A.2) | `grep -z`, `src/archive.rs`, docs/CLI.md |
| Look-around and backreferences with a step budget (R-2A.2) | `grep -P` (PCRE2, stopped by its match limit with a warning) |
| Fuzzy patterns longer than 128 bytes (R-2A.2) | `--fuzzy`, up to 4096 characters (64-bit blocks with carries) |
| Fuzzy regex (R-2A.2) | `--fuzzy-regex[=N]`, `src/fuzzyre.rs` (bit-parallel Glushkov automaton, up to 64 positions) |
| Small trees (R-4.6) | the parallel walk starts at the first directory of any root that holds directories (one thread used to read all 60 directories of a 211-file repository while the search threads waited); a sequential walk feeding three search threads only for a flat root of up to 256 files; ignore files matched without a regex (each built a `GlobSet`: 100 to 400 µs, the global excludes file 0.3 ms of every run); 133 system functions bound on first use (0.4 ms); a grep-only argument parser (0.1 ms) |
| One very long line (R-4.6) | read into one exactly sized buffer, each thread filling its own part (100 MB: 12.6 ms on one thread, 4.0 ms on five), its UTF-8 checked a piece at a time as it is written: 60.5 ms to 32 ms on a 100 MB line (ugrep 38.8 ms) |
| Incremental re-search for editors (R-4.1) | `grep --index --update` (refresh the text index, then search only candidate files); `watch` and `serve --watch` keep the index fresh |
| Structural search without re-parsing (R-2B.2) | `tsquery` keeps captures per file by content hash: the Linux tree took about 25 s, the repeated query 1.6 s |
| Cross-file reference resolution with scope awareness (R-2B.4) | `refs` resolves occurrences through scopes, imports and qualified names for Python, JavaScript/TypeScript, Rust and Go (60 of 60 hand-checked resolutions right), (`tests/resolve_java.rs`) |
| Language server (R-6.15) | `scrutate lsp`; the VS Code extension starts it; JetBrains through LSP4IJ |
| Sample for `ocaml_interface` (R-3.4) | all 74 grammars verified by samples (docs/LANGUAGES.md) |
| int8 semantic vectors (R-4.3) | schema 13: index about a fifth smaller, ranking quality within 0.001 MRR (docs/QUALITY.md) |
| `scrutate watch` (R-2B.1) | inotify on Linux, ReadDirectoryChangesW on Windows, the index's own check on a timer on macOS |
| Benchmark dashboard (R-4.5) | `bench/make_dashboard.py` writes `docs/dashboard.html` from the results in `bench/results/` |
| Package definitions (R-12.13) | `packaging/`: Homebrew formula (installed and tested in a Linux Homebrew), Scoop and winget manifests (schema-validated), `.deb` and `.rpm` (built and install-tested in clean Debian and Fedora containers) |
| Third-party notices in releases | `tools/third_party_notices.py`; archives, wheels and the `.deb` and `.rpm` packages include `THIRD_PARTY_NOTICES.txt` |
| Incremental parsing of edited files (R-2B.1) | a long-running index (`watch`, `serve`, `lsp`) reparses an edited file against its previous tree, for the 21 grammars without an external scanner (with one, a reparse was seen to differ from a fresh parse); a 66 KB Go file: 5.5 ms to 2.0 ms per edit; `tests/incremental.rs` |
| Ranking quality beyond Python (R-2B.12) | the ranker is fitted on Python plus Rust, Go, Java, JavaScript and TypeScript repositories, with functions and types as answers; held-out results per language in docs/QUALITY.md |
| Indexed search precision (R-2B.1, R-4.6) | text index format 2: next-byte and position masks per posting; concurrent throughput against Zoekt went from 4% behind to 5% ahead and p50 latency to 0.46 of Zoekt's (20% ahead and 0.37 in the final run) |
| Many files on macOS (R-4.6) | as many walking threads as performance cores (kernel contention made 11 threads slower than 5); the Linux and LLVM trees, 200,000 small files and the binary-heavy tree went from behind or level with ugrep to 13 to 21% ahead (15 to 30% in the final run) |
| Regexes anchored at line starts (R-4.6) | only line starts beginning with the regex's prefix literals are examined: `^(a|aa)+$` on the pathological corpus, 52 ms to 18 ms (13 ms since its one-line file is read on several threads) |
| Start-up in git repositories (R-4.6) | `vendor/ignore` reads the global excludes path without compiling a regex: 0.4 ms of a 6 ms search of a 220-file repository |
| One definition of every index operation (R-6.15, R-13) | `ops.rs`: the CLI, the MCP server, `batch` and the Python bindings share arguments, defaults, limits, text and JSON; they had drifted (three limits for `callers`, references textual in MCP and resolved in the CLI) |
| An MCP server that fits how agents work today (R-6.15) | read-only and local hints on every tool, server instructions, answers within a byte budget with an offset to continue, `grep_code` on Engine A, no tool call blocked for more than 20 s by a first index build; from docs/research/agent-facing-search-2026-11.md |
| Index upkeep in one place (R-12.4) | `Repo`: update, and rebuild when the database is corrupt, for every front end that stays open |
| The index never disagrees with the walk about a pattern (R-2B.1) | one translation of the pattern for Engine A's matcher and the text index's trigram condition, checked by an in-process test |
| Linux-tree searches against ripgrep (R-4.6) | a busy `.gitignore` compiles its globs; files up to 1 MB read whole; no size lookup per file: in the test VM from 2 to 6% behind ripgrep to 7 to 16% ahead on 11 of the 13 text rows, 40% ahead with very many matches, 59% ahead on the complex regex |
| Cold trees on Linux (R-4.6) | a read counts as a disk read at 30 µs there (the macOS value, 75 µs, never fired), and a walk reading from the disk grows by a thread for every 16 slow reads, up to 64: the four cold Linux-tree rows went from level with git grep (two lost by under 1%) to 0.28 to 0.30 of the best competitor's time, 18 to 24% above the read-files ceiling |
| Heavy output through a pipe (R-4.6) | output that follows other output within a millisecond waits in a 64 KB buffer and leaves in one write, where every matching file used to cost a write and a wake-up of the reader: `return` in the Linux tree (1.3 million lines) in the Linux VM, from 0.95 of ripgrep's time to 0.60 |
| A read-files ceiling that is one (R-14.8) | `bench/rawread.c` gives each thread 64 neighbouring paths at a time, and the harness keeps the fastest of several thread counts; with one path at a time, its threads shared a directory and read a cold tree at a third of the disk's speed |
| Measurements a busy machine spoiled (R-14.9) | a benchmark where some tool's block medians differ by more than 25% is measured again, once, whatever its status, and keeps its first result beside the second; a warm row records how much of its corpus was in the page cache (on this 18 GB laptop the LLVM tree, 4.9 GB in 16 KB pages, no longer fits once a browser and an editor are open) |
| What agents ask of an MCP server now (R-6.15) | revision 2026-07-28 beside the handshake revisions (`server/discover`, a version in each request, `resultType`, cache hints on lists); with `serve --structured`, an `outputSchema` on every tool and the `--json` form of each answer in `structuredContent` (off by default: Claude Code shows its model that JSON in place of the text); `notifications/cancelled` honoured, each call on a thread of its own; progress reports with files read against the total while the first index is built, `reindex` runs or `structural_search` reads; large structured answers paged with the text (`next_offset`); `read_code` of a whole definition by name; the location at the start of every result accepted wherever a tool takes a symbol |
| Method calls on variables (R-2B.4) | `x.m()` resolves through the type the declaration of `x` states, as an annotation or as the thing it is built from, in Python, JavaScript/TypeScript, Rust and Go; Go methods are found by their receiver type across the files of a package |
| TypeScript path aliases (R-2B.4) | an import through `paths` or `baseUrl` of the nearest `tsconfig.json` or `jsconfig.json` (comments, trailing commas and a relative `extends` included) resolves to its file in `refs`, `deps` and `rdeps`; so does an import of a workspace package by its `package.json` name (`exports`, `module`, `main`, a build output standing for its source under `src/`) |
| Limits left in 0.1.0's first draft (R-2A, R-12.4) | smart case sees a letter written by its code point (`\x41`); the LSP server replaces a damaged index, from a save or from a request; the watcher's test of one path decides in the walk's order (a `.ignore` at any level before every `.gitignore`) |
| Limits left in 0.1.0's second draft (R-2A, R-2B.1, R-2B.4) | calls inside Rust macro arguments are call edges; an MCP answer checks the files it cites and re-reads a changed one; files on `/proc`, `/sys` and the other kernel pseudo filesystems are read to the end; on Unix a `chmod` alone is seen (the index keeps the status-change time); an emptied `chunks` table is damage and is rebuilt; a JavaScript `var` belongs to its function; one file argument prints without its name, as in ripgrep, with `-H` and `-I`; C++ constructors, destructors, member templates and conversion operators declared in a class body are methods; `\,` keeps a comma in `SCRUTATE_INDEX_EXCLUDE_DIRS` |
| Training reads the engine, and the mirror ended (R-5.3) | the ranker and the vector model are fitted on rows the engine writes (`batch --features`, the hidden `views` command), the benchmark builders read files and words through it, and the Python reference engine that mirrored it bit for bit on 7 of the 73 grammars is gone with `bench/parity.py`; `tests/ranking_pins.rs` and `bench/quality.py` watch ranking (docs/QUALITY.md) |
| Benchmark corpora without hard links (R-14) | `bench/corpora.py` copies (APFS clones) where it linked: a hard-linked file costs every tool twice as long to open on macOS |

## Blocked: needs the repository published

Publishing was declined on 2026-11-08 and asked for on 2026-11-11, on one condition: every way to
install and use the tool works first. The repository is public on GitHub
(`github.com/scrutate/scrutate`, `main` only); docs/RELEASE_READINESS.md lists what ran
here and what ran on GitHub. The order: one green run of `ci.yml` (done), the settings in
docs/RELEASING.md, public (done), tag `v0.1.0`.

- The release workflow. `ci.yml` is green since run 43013926055
  ([Releasing](RELEASING.md#7-what-a-published-release-adds)); the changelog has what the three
  failed Windows runs before it found.
- Signed releases with Sigstore attestations (`release.yml` has the steps and a dry run that
  rehearses them; they need one run on GitHub). The free-threaded `cp314t` wheels are built,
  installed and tested in CI on all six runners (`ci.yml`, run 43408907893).
- The packages: tap, bucket and winget pull requests with the release checksums (steps in
  packaging/README.md).
- `cargo install scrutate`: crates.io refuses crates over 10 MB, and the semantic model is 14.6 MB,
  so it needs a smaller model (next: int8 weights, which would halve it, checked as QUALITY.md
  does) or a download on first use. A
  published crate also cannot carry the `[patch]` for `vendor/ignore`: its eleven changes must first
  go upstream or out as a fork.
- A comment on GitLab merge requests (R-12.12; on GitHub the Action's `comment: true` posts
  `scrutate review` on a pull request, and the GitLab template saves it as a Code Quality report),
  and weekly benchmark runs for the dashboard (R-4.5).
- Now that `ci.yml` is green, one job or setting each (docs/research/github-ci-2026-11.md): wheels
  to PyPI by trusted publishing, CodeQL default setup for Rust, OpenSSF Scorecard. For the integrations
  (docs/research/integrations-setup-2026-11.md): an LSP4IJ template so JetBrains needs one import.

## Next

- Small trees and start-up, what is left (the measured state is in
  [Architecture](ARCHITECTURE.md#engine-a-universal-instant-search-cratesscrutatesrcinstantrs)).
  A one-file directory search runs fewer instructions than GNU grep (17 M against 19 M) in the
  same CPU time but touches 20 more pages (a Rust hello-world: 291 page reclaims, 1.9 MB):
  anonymous constant tables the linker cannot order by name (12 pages), relocated data (6) and
  the 64 KB line buffer the searcher zeroes. Wall time against GNU grep depends on load (1.87 ms
  against 1.97 ms in steady runs, behind in interleaved blocks under other work); an idle-machine
  benchmark settles it. Ruled out: binary size (0.07 ms at 85 MB; a file written minutes ago
  costs 0.2 ms more) and a condition variable in place of the idle walker threads' short sleeps
  (3.9 ms against 3.7). A linker order file did nothing under ThinLTO (585 page faults against
  586), since ld64 cannot place the functions ThinLTO promotes; with one object it took 131 page
  reclaims out. Profile-guided optimisation gained 2 to 3.5% on the 220-file tree, not adopted:
  it needs a 13 MB profile in the repository and every release build.
- Peak memory against GNU grep. `-j1` holds less than ugrep and ripgrep on every tree and more
  than GNU grep, a 235 KB C program (macOS):

  | Tree | `-j1` | GNU grep |
  |---|---|---|
  | a one-file directory | 2.5 MB | 1.95 MB |
  | the ripgrep sources | 3.3 MB | 2.3 MB |
  | py-big | 4.6 MB | 2.8 MB |
  | the Linux tree | 6.5 MB | 3.4 MB |
  | LLVM | 8.3 MB | 6.4 MB |

  On a large tree the rest is mostly allocator memory freed and kept (per-CPU magazines:
  `MallocMaxMagazines=1` in the environment takes 1.1 MB off the Linux tree; a process cannot set
  it for itself), the ignore matchers (1.2 MB there) and whole directories read at once. A
  one-line file is held whole (103 MB for 100 MB) for a regex, `-i`, `-w`, `--json` or color; a
  plain `-F` literal with plain output or `-c` reads it in two passes and holds 3 to 5 MB. Under
  the 64 MiB split size such a file still goes through the stream reader, whose buffer grows to
  the line; the two passes could serve it. The default mode holds ordered output on one large file
  with many matches (23 MB); 2 MB ranges in place of 8 MB halve that for 2.4 to 3.5% more
  instructions, not taken.
- Linux has no function order. Of 5.8 MB resident at the start of a small search, 3.5 MB is the
  executable's text, spread over 85 MB with the kernel mapping 64 KB around each fault; 0.5 MB
  is data relocated at load. GNU ld in the build image cannot order functions; lld can, and a
  linker script can list sections, once the unrun release workflow builds with either.
- Large trees on macOS are bound by `open` (docs/ARCHITECTURE.md): the Linux tree takes 0.52 s just to
  open and read on the five performance cores, a search 0.58 s; the rest above that floor is the
  search. The corpora `linux` and `linux-nodot` shared files by hard links, doubling every open's
  cost on macOS for every tool; `bench/corpora.py` now copies them (APFS clones). `openat`
  through the directory's descriptor does not lower the floor: 13.3 µs a file against 12.4 µs by
  path.
- Linux on dedicated hardware: every Linux number comes from a 5-CPU VM on the development Mac,
  on a virtual disk on its SSD. Two values fitted there need checking on NVMe and network
  disks: the read time that counts as a disk read (30 µs) and the most threads for a cold tree
  (64). A cold tree there also costs about 90 MB on 64 threads against 16 MB warm (`-j` bounds
  both).
- Windows: nothing is measured. It takes the macOS values (threads: the cores, at most 12; cold:
  twice the cores, at most 24; a disk read: 75 µs) from the `OTHER` row in
  `crates/scrutate/src/policy.rs`, where a measured row goes.
- References for C# and C/C++ includes; `refs` resolves Python, JavaScript/TypeScript, Rust, Go,
  Java and PHP today. A language is one more rule set under `resolve/`.
- Benchmark leadership on dedicated idle hardware (R-14.11): every number comes from a developer
  laptop shared with other work, and Linux x86_64 has no run; in the last macOS run six of 37 warm
  rows were measured twice for that reason. An x86_64 run emulated on this laptop was dropped:
  gcc crashes building GNU grep there and ripgrep's allocator takes a slower path, so it would
  time the emulator. A run of `bench/run_macos.sh` and
  `bench/competitors/linux_bench.sh` (which takes an x86_64 host as it is) on dedicated machines
  closes it (docs/research/known-limits-2026-11.md prices the hardware).

### Known limits that stay in 0.1.0

- On Windows the index has no status-change time: a file made unreadable with no change of size
  or modification time keeps its chunks until `index --force` or `reindex` with `force` (Unix
  compares that time too). `MetadataExt::change_time` is unstable in Rust 1.99.1 (feature
  `windows_change_time`); another way needs an open handle per file on every scan.
- A method call on a call's result resolves through the callee's declared return type. It stays
  undecided when the callee declares none, returns a type parameter or an outside type holding a
  repository type, or is a method of an outside type (`file.metadata().ok()`).
- In `refs`, a plain name in a Rust macro's arguments that the token rules do not read as a field,
  field name or local is tied by name to a definition: the `text` in `mymac!(text, 3)` counts as a
  reference to a function `text`, whatever the macro does.
- Java: `refs` lists fields, enum constants and record accessors as unresolved (`field or key`,
  [CLI](CLI.md#refs)), a `static final` field too, which `symbol` finds as a constant.
- On Windows the `notify` crate (8.2.2; read in its source, it reports no empty read) drops a
  burst too large for the watch buffer. `watch` indexes it with the first change to an indexed
  file 30 s or more after its last walk; a command run meanwhile scans the tree and finds it.
- On Linux, past `fs.inotify.max_user_watches` the deepest directories get no watch
  ([CLI](CLI.md#watch)). While any directory has none, every burst that changes an indexed file
  walks the tree, so a change in an unwatched directory is indexed with the next change to an
  indexed file anywhere.
- On macOS `watch` checks the tree on a timer ([CLI](CLI.md#watch)), so on a tree of 20,000
  directories a saved file is indexed after 11 to 14 s (debug build). Linux and Windows update the
  saved file alone.
- Call forms the grammars do not tell apart: a Verilog task call without parentheses (`h;`) or on
  a member (`obj.m(1);`) records no call edge, a VHDL indexed signal `a(i)` is recorded as a call,
  and an assembly call under a local label counts for that label and for the global label above.
- A one-line file with 8,000 nested functions takes 542 MB of memory to index (462 MB with
  `SCRUTATE_SEMANTIC=none`) and 3.1 to 3.5 s (release build, macOS arm64): each of the 8,000
  chunks keeps its own text (up to 32 KiB), so the index holds 40 MB of compressed chunk text for
  the file, plus their postings.
- Windows and Intel macOS are tested on GitHub's runners only ([Install](INSTALL.md) has the
  runners and the five Unix-only tests): no release archive is built for them, installed on a
  machine or benchmarked there.
- No benchmark has run on x86_64 Linux or on dedicated idle hardware (R-14.11 above).
- The release binaries carry no platform code signature: no Apple notarization, no Windows
  Authenticode. The integrity check is the Sigstore attestation and `SHA256SUMS`;
  [Install](INSTALL.md#unsigned-binaries-on-macos-and-windows) has the Gatekeeper and SmartScreen
  workarounds. Next: apply to SignPath Foundation (free for open source) after 0.1.0.
