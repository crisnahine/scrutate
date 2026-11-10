# Architecture

Scrutate is one binary with two independent engines and a thin unifying layer.

```
                 scrutate (CLI, MCP server, Python bindings)
                 ┌──────────────────────────────────────────┐
  query ───────► │ unified layer: `find` picks the engine,  │
                 │ `grep --symbols` and `refs` combine them  │
                 └───────┬───────────────────────┬──────────┘
                         │                       │
        ┌────────────────▼──────┐   ┌────────────▼────────────────────────┐
        │ ENGINE A  instant.rs  │   │ ENGINE B  index.rs lex.rs search.rs  │
        │ no index, no parsers  │   │ persistent, incremental index        │
        │ ignore-aware parallel │   │ tree-sitter symbols + call graph +   │
        │ walk, regex/literal/  │   │ imports; BM25 postings; semantic     │
        │ identifier/fuzzy,     │   │ vectors (IVF); LightGBM ranker;      │
        │ streaming, cancel     │   │ dupes/similar/outline/deps/tsquery   │
        └───────────────────────┘   └──────────────────────────────────────┘
```

## Engine A — universal instant search (`crates/scrutate/src/instant.rs`)

Built on the `grep-searcher`, `grep-regex`, `grep-matcher` and `ignore` crates — the published
components ripgrep itself is assembled from (their crates.io pages describe them as ripgrep's
searching, regex, matcher-trait and ignore-aware directory-walking libraries). Scrutate adds the
capabilities those crates do not provide rather than re-implementing what they already do:

- **Matching modes**: regex (Rust `regex`: linear time, no backtracking, size-limited), literal,
  **identifier** (one spelling matches every casing: `refreshToken` ≡ `refresh_token` ≡
  `REFRESH_TOKEN`; digits and acronyms are parts), **fuzzy/approximate** (Myers' bit-parallel
  edit-distance matcher: one 128-bit word for patterns up to 128 bytes, 64-bit blocks with carries up to 4096 bytes, with a pigeonhole prefilter so only candidate lines
  pay for the DP), multiline, inverted, whole-word, smart case.
- **Traversal**: the `ignore` parallel walker (work-stealing across threads, `.gitignore` /
  `.ignore` / global excludes, hidden files off by default, symlink loop detection, max depth and
  size filters, type globs derived from the language registry). The crate is vendored
  (`vendor/ignore`) with four changes. The global git excludes path is read from `~/.gitconfig` by
  a small parser instead of a regex compiled for each run (0.4 ms of a 6 ms search). An idle walker
  thread first waits 10 µs, doubling up to the original 1 ms, before it looks for work again; with a
  fixed 1 ms, a walk of a few hundred files spent whole milliseconds asleep (598 files: 12.3 ms to
  9.3 ms). Gitignore lines of three shapes (`**/` and a name with `*` or `?`, `**/` and a literal
  path, `/` and a name with `*` or `?`) are matched by a byte loop instead of a `GlobSet` regex,
  whose build cost 100 to 400 µs per file, more than the whole walk of a small repository; a
  randomized test checks it against `GlobSet`. And a parallel walk can start more threads while it
  runs (`grow_threads`); their visitors are built at the start and stay idle until then.
- **Thread count**: the cores (at most 12), except on macOS, where a walk uses as many threads as
  performance cores: there, more threads opening and reading files at once wait on each other in
  the kernel (the Linux tree on an M3 Pro: 977 ms on 11 threads, about 845 ms on 5). The open and
  read time of the first files tells the page cache from the disk (about 25 µs a file against
  160 µs or more); when it is the disk, the walk grows to twice the cores, at most 24, because
  the disk serves many reads in flight (the Linux tree cold: 2.9 s on 5 threads, 1.4 s on 22).
  `-j` overrides both.
- **One large file, every core**: a file of 64 MB or more is cut into ranges of whole lines (8 MB
  each). A first pass counts each range's newlines for the line numbers and stops at a NUL byte,
  which sends the file back to the single pass so binary handling is unchanged. The second pass
  searches one window of ranges per thread at a time and writes the windows' output in file order,
  so the output held in memory stays bounded. Context lines, `--max-count`, `--max-results`, `-l`,
  multiline and a byte-order mark keep the single pass.
- **Roots without directories**: when no root holds a directory the walk would enter, a sequential
  walk with the same rules runs first, and three threads search the files as it finds them; when
  those files turn out to come from the disk, it adds threads. Past 256 files it stops, and the
  parallel walk takes over, skipping the files already searched. For three large files this beats
  the parallel walk (40.6 ms against 41.9). Any directory to enter sends the search straight to the
  parallel walk: one thread reading every directory held up the searches (the ripgrep repository,
  60 directories: 5.6 ms in parallel against 6.0).
- **Start-up** (`lazy_bind.rs`, `cli.rs`): a search of a few hundred files is mostly process
  start. Two costs were ours. dyld binds every imported symbol before `main`, about 3.3 µs each for
  symbols that come through libSystem (a C program importing 121 more of them starts 0.4 ms
  later); the executable imported 210, and a search calls about 75. On macOS arm64 the other 133
  are defined in the executable as private stubs that look the function up with `dlsym` on the
  first call and jump to it, keeping every argument register and the stack; `fork` binds all of
  them first, so a child never does a lookup. And clap builds every subcommand's argument
  definitions before parsing, so `scrutate grep` uses a definition of that subcommand alone.
  Start-up went from 2.4 ms to 1.9 ms (a Rust hello-world: 1.5 ms). Measured and left alone: the
  binary's 37,000 pointer fix-ups (applied lazily by the kernel, no cost), its code-signature page
  size (no difference), and its 85 MB size (0.05 to 0.2 ms).
- **What bounds a search on macOS**: opening a file. On APFS `open` costs 8 to 11 µs for a file
  that was opened recently and about 19 µs when the tree is large enough that the kernel's data for
  it left the CPU caches; `close` costs 0.5 µs, a read of the whole file 1.3 µs, `stat` 1 to 2 µs.
  Opens scale to the performance cores and no further (the Linux tree, open and read only: 1.95 s
  on one thread, 0.56 s on five, 0.71 s on six, 0.95 s on eleven; the same for processes as for
  threads), which is why the default thread count is the performance cores. A search of that tree
  takes 0.66 s, so about 0.1 s of it is ours.
- **Files without line breaks**: a large file whose ranges cannot be cut at a newline is read once
  into a buffer of its exact size (checked for NUL bytes as it is read, so binary handling is
  unchanged) and searched as a slice; the stream reader would grow its buffer by doubling.
- **Line anchors**: the regex is always built in multi-line mode, so `^` and `$` mean line start and
  end and the searcher can scan a whole buffer at once; without it, patterns using them were run
  one line at a time.
- **Line-start literals**: when every match of a regex starts a line and begins with one of at most
  64 literals (`^(a|aa)+$`, `^#include`), the candidate lines are the line starts followed by one
  of them, and each is decided by a search anchored there. A match that would cross a line break
  is passed to grep-regex's matcher, which cannot match one.
- **Suffix literals**: when a regex has no literal prefix but every match ends in one of at most 64
  literals of 3 bytes or more (`\w+_(suspend|resume)\(`), candidate lines are found with an
  Aho-Corasick search for those literals and the regex runs only on those lines. Without it the
  regex engine scanned every byte for such patterns (ripgrep, built from the same crates, shows the
  same cost).
- **Streaming**: matches are batched per file and sent over a channel to the caller's sink while
  other threads keep searching; an `AtomicBool` cancels the walk between files (Ctrl-C → exit 130,
  `--max-results`, broken pipe).
- **Output**: path:line:text, `-c`, `-l`, context, colour, deterministic `--sort`, and JSONL
  (`--json`, schema version 1: `match`, `context`, `count`, `file`, `summary`, `symbol_group`).
- **No dependency on Engine B**: `grep` never opens the index. `grep --symbols` is the one
  opt-in bridge (group matches by the enclosing function using the index if one exists).

## Engine B — code intelligence (`index.rs`, `lex.rs`, `parse.rs`, `langs.rs`, `search.rs`, `intel.rs`)

### Parsing (`parse.rs`, `langs.rs`)
Three tiers so every file yields something useful (see docs/LANGUAGES.md): tree-sitter grammars
driven by a **data-driven rule registry** (per language: which node kinds are definitions, how the
name is found, what a call is, how imports look); a keyword heuristic tier; and overlapping line
windows. Each definition becomes a *chunk* with fields name / qualified name / signature /
documentation / body / path, plus call edges and imports. Adding a language is a registry entry
and a sample file, not new code.

### Storage (`index.rs`, `lex.rs`)
- **SQLite** (bundled, WAL, `synchronous=NORMAL`): `files` (size, mtime, SHA-1 content hash),
  `chunks` (metadata + zlib-compressed text), `refs` (call edges), `imports`, `meta`
  (schema version, model signature, segment manifest, data version).
- **Segments** (`.scrutate/seg/sNNNNNN/`): immutable, memory-mapped `.npy` arrays — Lucene-style.
  Per field (name, qualified name, signature, docs, body, path): BM25 postings (`ptr`, `doc`, `tf`)
  over a shared 63-bit token-hash vocabulary; `ids`, `dl`; int8 semantic vectors (`vec`, two
  views per chunk: description and implementation) with one float32 scale per view (`vscale`:
  symmetric quantisation, value = code × max|x| / 127, rounded half to even, identical in both
  engines; half the size of the float16 vectors used up to schema 12, docs/QUALITY.md has the
  measurements) stored in **IVF cluster order** for
  segments ≥ 20 000 rows (spherical k-means, K = clamp(2√n, 32, 1024); a query probes 48
  clusters, measured at ±0.000 MRR against the exhaustive scan).
- **Lifecycle**: content hashing (mtime/size fast path, SHA-1 verification), one `BEGIN IMMEDIATE`
  transaction per update (readers never see a half state), new segments written to a temp
  directory and renamed, deletions as liveness bitmaps, background **compaction** (merge small
  segments / purge dead rows) that aborts if another writer moved the manifest, segment **GC** with
  an age guard, **corruption detection** (`PRAGMA quick_check` on open, structural validation of
  every array on segment open, deep validation in `scrutate check`) with automatic **rebuild**,
  **schema and model versioning** (a different schema or model clears and re-indexes inside the
  database so concurrent processes stay consistent), and multi-process safety (busy-timeout +
  retry on `SQLITE_BUSY`, never deleting a live database).

### Retrieval and ranking (`search.rs`, `semantic.rs`, `rank.rs`)
A query produces candidates from three sources — field-weighted BM25 (name 10, qualified name 3,
signature 3, docs 5, body 1, path 2), exact name/qualified-name lookup, and semantic nearest
neighbours (the query is encoded into the same 256-d space as the chunks by a contrastively
trained code-word model, inference only, bundled in the binary). 47 features per candidate
(lexical scores and ranks per field, z-scored semantic similarities of both views, literal hits,
name coverage, kind, lengths, test-path flags…) go through a LightGBM LambdaRank ensemble
exported to plain arrays and evaluated in Rust. All arithmetic is specified to the bit: the Python
reference implementation (`python/scrutate_ref`, which also trains the models) and the Rust engine
return identical features, scores and rankings (`bench/parity.py`: 300/300 queries; the IVF path
200/200 on a 135k-chunk index). Vector dot products are summed in float64 and rounded once, so
the two engines' different summation orders cannot reorder near-tied chunks.

### Text index (`textindex.rs`)
The indexed counterpart of Engine A, for repeated grep-style queries on large trees (the job Zoekt
and livegrep do). It covers exactly the files Engine A's default walk visits (same walker, same
ignore rules) and stores, per file, the set of byte trigrams (ASCII case folded), as immutable
segments: sorted trigram keys, delta + LEB128 posting lists of document numbers, and per-document
path, size, mtime and flags. Each posting carries two mask bytes: a bit for the low three bits of
each byte that follows the trigram in that file, and a bit for each position of the trigram modulo
8. A query is turned into a boolean trigram condition by walking the pattern's regex-syntax HIR
(literals, small classes and alternations expand to exact string sets; concatenations combine them;
anything else contributes no constraint), evaluated over the postings as bitsets, and the
surviving files are searched by Engine A's own per-file searcher. A string of four bytes or more
needs its trigrams at positions that can line up and each followed by a byte that fits, so a file
holding them apart is not a candidate (`kernel_thread` on the Linux tree: 164 candidate files
instead of 1,781).
The condition is a necessary one, never a sufficient one, so results equal Engine A's on the
indexed snapshot; a property test and a coverage-guided fuzz target check that a text a pattern
matches always satisfies the pattern's condition, including Unicode case folding (the Kelvin sign
matches `k` under `-i`). Files with a NUL byte index only the bytes before it (line-mode search
stops there) and are always searched by multi-line queries; UTF-16 files and files unreadable at
index time are always searched. Updates are incremental (size + mtime), write new segments and a new
liveness file per generation, then swap `manifest.json` atomically, so readers never see a half
state; more than four segments, or more than 30% dead documents, merges them by a streaming k-way
merge. Writers serialise on a lock file (polled with `try_lock`: SMB mounts answer a blocking lock
with EAGAIN). The index stores no file contents (409 MB for the 1.6 GB Linux v7.3 tree; docs/LEADERBOARD.md
compares sizes with Zoekt and livegrep, whose indexes hold the text), so a query reads the
candidate files from the source tree.

### Watching (`watch.rs`)
`scrutate watch` and `serve --watch` turn filesystem events (the `notify` crate: FSEvents,
inotify, ReadDirectoryChangesW) into index updates. Events are filtered with the indexes' own
ignore rules, a burst ends after 200 ms of quiet, and each burst runs the existing incremental
updates (`Index::update`, `textindex::update` when a text index exists). No new write path: the
watcher is one more writer behind the same SQLite transaction and text index lock file, so a crash
mid-update leaves the previous snapshot. Reads by the update itself are not events, so updates do
not trigger themselves. Not a daemon: it runs only while the command (or server) runs.

### Intelligence (`intel.rs`)
Definitions and references, callers/callees, outline, imports with best-effort
module→file resolution and reverse dependencies, **similar code** (implementation-view vectors +
token Jaccard), **duplicates** (exact token-sequence hash and MinHash/LSH near-duplicates over
token 3-grams, nested chunks excluded), and **structural search** (`tsquery`: tree-sitter query
language over all files of a language, parallel, no index needed).

`refs` resolves names (`resolve.rs`). Engine A finds the whole-word occurrences; for each file
that holds one (Python, JavaScript/TypeScript, Rust, Go), the file is parsed again with
tree-sitter to find the identifier node, its qualifier, the enclosing scopes and the names its
imports bind, while definitions and their nesting come from the index's `chunks` rows. An
occurrence goes to the innermost scope that defines the name, then the file's top level, then
an import binding, the Go package directory or a wildcard import; a qualifier goes through the
module it names or the class or type it names. Import paths are mapped onto repository files
(Python packages and relative imports, JavaScript paths and `index` files, Rust `crate`/`self`/
`super` paths, module files and workspace crates from `Cargo.toml`, Go import paths by
directory), following re-exports with a depth limit. It is a query-time step: no schema change,
nothing for the Python reference to mirror, and `refs --textual` keeps the unresolved answer.
The LSP references request uses it for the definition under the cursor.

`tsquery` keeps each file's captures in `.scrutate/tsquery/` (`tscache.rs`): one file per
(version, language, query text, searched paths) mapping a 128-bit BLAKE2b content hash to the
captures, without paths, so files whose content was seen before are read and hashed but not parsed. A file cut off
by `--limit` is never stored, so a cached list is always the complete one and the global limit
gives the same prefix. Files are written to a temporary name and renamed; an unreadable one
counts as empty. Eviction is by query file, least recently used first, above 128 MB in total.

## Unified layer (`cli.rs`, `mcp.rs`, `crates/scrutate-py`)
`find QUERY` chooses: regex-looking → Engine A; otherwise ranked search when an index exists or
the repository is small enough to index on the spot; otherwise identifier grep. `grep --index` sends
an Engine A query through the text index. Both engines stay
independently usable (`grep`, `index`/`search`) and independently tested. The MCP server exposes
fourteen tools over stdio; the Python bindings expose both engines with the GIL released and
streaming results.

`lsp.rs` is a language server for editors (`scrutate lsp`), hand-written like the MCP server: a
`Content-Length` framed JSON-RPC loop on stdio that turns definition, references, workspace symbol
and document symbol requests into the same index lookups as `symbol`, `refs` and `outline`, and
converts between the index's lines and the protocol's UTF-16 columns. The first index build holds
the searcher's lock, so early requests wait for a complete index. A save updates the index on a
second connection owned by a background thread; SQLite WAL lets requests keep reading while it
writes.

## Models (`models/`)
`semantic.npz` (14.6 MB: code-word vocabulary tables and projections) and `ranker.npz` (87 KB)
are embedded in the binary with `include_bytes!`; `SCRUTATE_SEMANTIC` / `SCRUTATE_RANKER` can
point at other files or `none`. The index records the model signature and re-indexes when it
changes. Training (numpy + LightGBM) lives in `bench/train_semantic.py` and the reference package.

## Design rules for the next decade
- Formats are versioned (`schema_version`, `.npy` headers, JSONL `schema`) and migrations are a
  clear-and-rebuild of a cache, never a data-loss event: the source tree is the truth.
- Components are replaceable behind small interfaces: a grammar is a registry row; the ranker is
  an array file; the vector model is an `npz`; the store is a directory of immutable arrays.
- No network, no execution of repository code, no background daemons: the engines are libraries
  that a CLI, an MCP server and Python call directly.
- Everything that can be measured is measured by a script in `bench/` that anyone can rerun.
