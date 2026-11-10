# Competitive positioning: what the evidence supports

This page says what Scrutate 0.1.0 does better than the tools it was measured against, where it
ties, and where it is behind. Every performance statement points at docs/LEADERBOARD.md (generated
from the raw results in `bench/results/`); every capability statement points at a test. Claims
about other tools are limited to what was observed in those runs and to their documented options.
The machine is a developer laptop shared with other work (an Apple M3 Pro); section 14 of the
mission reserves "#1" claims for dedicated idle hardware, so the wins below are measured results,
not publishable leadership claims.

## 1. Against grep-class tools (GNU grep, ripgrep, ugrep, git grep, ag, ack)

On the development Mac, Engine A is significantly fastest in 32 of the 37 warm-cache benchmarks
(30 runs per contender in three interleaved blocks; the 95% interval of the ratio of medians lies
entirely below 1) and in all 5 cold-cache ones. On the Linux kernel tree it is 13 to 38% ahead of
the next tool (ugrep in most rows, ripgrep or git grep where ugrep's answer is wrong), on LLVM 12
to 26%, on 200,000 small files 21%, on a binary-heavy tree 18%, and 42 to 57% on one 740 MB file,
which it searches on every core. With the page cache cleared before every run it is 17 to 23%
ahead of the next tool.

In the 8-CPU Linux virtual machine on the same Mac, where opening files is cheap and run-to-run
variation is high (its virtual CPUs land on performance or efficiency cores), Engine A ties ripgrep
on 11 of the 14 Linux-tree benchmarks (PARTIAL: the intervals contain 1), is significantly faster
on the complex regex (0.54x) and the structural query (0.04x of ast-grep), and loses one, the
case-insensitive Unicode query. With the page cache dropped before every run it wins 3 of 4 (0.56
to 0.70x of ripgrep) and ties the fourth at the hardware limit.

**On macOS, opening files is the expensive part, and more threads make it slower.** Reading every
file of the Linux tree with eleven threads (the "read files" column, `bench/rawread.c`) takes 980
ms; Scrutate searches the same files in 723 ms, because threads opening and reading files at once
wait on each other in the kernel. Scrutate walks with as many threads as performance cores, and
adds threads when its reads show the files coming from the disk rather than the page cache (cold,
the disk serves many reads at once): up to twice the cores on macOS, up to 64 on Linux. The other margins come from the matcher
and the output path: one large file searched on every core, regexes anchored at line starts
examined only there, and search threads that format their own output.

**Capabilities the others do not have (each verified by tests):** identifier search across casings
(`--ident`), fuzzy search within N edits (ugrep has `-Z` with a different edit model), results
grouped by enclosing function (`--symbols`), and an index-backed `grep --index` whose output equals
the walk's.

**Where they remain better** (the FAIL and PARTIAL rows of the leaderboard, each with its gap):
all five on the Mac are runs under 18 ms, and ugrep is ahead in each: the 220-file ripgrep
repository (5.1 ms against 4.7 to 4.8 ms, three queries), the same repository with its build
directory ignored (4.9 against 4.8 ms) and the backtracking trap (17.1 against 16.7 ms). Scrutate
starts 1.4 ms sooner than ugrep but takes longer to walk the directories and apply ignore rules
there; docs/ROADMAP.md has the measured next steps.On Linux, `-i KÖNIG` is
154 ms against ripgrep's 133 ms (1.16x, interval 1.03 to 1.24), and the 11 ties above are ties with
ripgrep; docs/ROADMAP.md (Linux) has the measured CPU difference and the next steps.

**Correctness differences found while making the comparison fair**, so the reader can judge them:
ag and ack have no Unicode case folding (`-i KÖNIG` finds nothing); ag, ack and git grep's own `-w`
use ASCII word boundaries (they report `ERR_PTR` glued to Chinese text as a whole word); GNU grep
needs a UTF-8 locale for Unicode word and case rules; tools other than ripgrep and Scrutate skip the
dotfiles a repository's `.gitignore` explicitly re-includes (`!.mailmap` in the Linux tree).

## 2. Against ast-grep (structural search)

Against ast-grep 0.51.1 with the same question (every call to `kmalloc` in the Linux tree, every
`format!` in ripgrep), both answers checked against a tree-sitter reference: 975 ms against 13.5 s
on the Linux tree (0.072x), 7.5 ms against 40.8 ms on ripgrep (0.184x). For C, ast-grep needs
`--selector call_expression` and a statement as the pattern: alone, `kmalloc($$$A)` parses as a
type macro and matches nothing, which the first comparison mistook for ast-grep being wrong.

## 3. Against Zoekt and livegrep (indexed search)

Measured on the Linux kernel tree (95,578 files, 1.6 GB) on the development Mac, against Zoekt at
commit `a5a933bc` built with `go install` as its README says (docs/LEADERBOARD.md, Engine B
section; raw data in `bench/results/macos-arm64/indexed.json`). Every query was checked against an
independent reference, and both tools answered all 20 the same way as the reference.

- **Building the index**: 5.3 s against 137 s (0.039x), 428 MB on disk against 4,238 MB (0.10x),
  1,118 MB peak memory against 1,302 MB.
- **After editing 100 files**: 0.19 s, because the text index updates in place. Zoekt's directory
  indexer (`zoekt-index`) rebuilds the whole index (135 s); the corpus is a plain directory, so its
  git indexer was not part of the comparison.
- **Query latency** over 20 queries x 20 runs, each a fresh process: p50 28.5 ms against 58.9 ms
  (PASS), p99 225 ms against 875 ms (PASS), p95 155 ms against 263 ms (PARTIAL: the interval, 0.17
  to 1.16, is wide because each tool's slowest query sits right at the 95th percentile). Memory per
  query 12 MB against 273 MB.
- **Throughput** with 11 clients at once: 32.3 against 25.4 queries per second (1.27x, PASS).

Why it is smaller and quicker to build: the text index stores trigram postings and no file contents,
and reads candidate files from the source tree, which must be present. Zoekt stores the contents
and can answer without the tree. Scrutate offers only the first mode in 0.1.0 (R-14.12).

In the Linux virtual machine, against Zoekt and livegrep (revision `923d5ad7`, built with Bazel as
its README says; its `codesearch` server holds the index and a minimal gRPC client prints each
answer), on the same tree and the same 20 queries, every answer checked: Scrutate is ahead on all 8
metrics (`bench/results/linux-aarch64/indexed.json`).

| Metric | Scrutate | Zoekt | livegrep |
|---|---|---|---|
| Building the index | 5.2 s | 127.7 s | 47.8 s |
| Index on disk | 428 MB | 4,238 MB | 7,782 MB |
| After editing 100 files | 0.22 s | 131.9 s (rebuild) | 29.5 s (rebuild) |
| Latency p50 / p95 / p99 | 11.9 / 74.0 / 107 ms | 51.4 / 222 / 851 ms | 72.0 / 317 / 365 ms |
| Throughput, 8 clients | 155.7 queries/s | 26.4 | 48.9 |
| Memory to answer a query | 16 MB | 358 MB | 2,704 MB (the server's resident index included) |

livegrep keeps its index in a long-running server's memory and answers there, which is what makes
its large queries fast; Scrutate and Zoekt start a process per query and map their index files.

## 4. Against "nothing": the code-intelligence engine

None of the measured tools has a code-intelligence index, so this has no competitor here:

- **Ranked search by meaning** with a learned ranker: MRR@10 0.663 for natural-language queries on
  an undocumented held-out Python code base, vs 0.468 for BM25 over the same candidates; 0.93 to
  0.99 for name and identifier queries; held-out Rust, Go, Java, JavaScript and TypeScript
  repositories also gain on every query type (docs/QUALITY.md).
- **Symbols for 60 languages** (72 with the optional grammars), each verified by a sample; call
  graph, imports and dependents, references grouped by symbol, outline, similar code, exact and
  near-duplicate detection, cross-repository search.
- **One interface for people and agents**: JSON everywhere (errors included), an MCP server with 14
  tools, Python bindings with the GIL released.

## 5. The case for adopting it, at the strength the evidence allows

ADOPTION
