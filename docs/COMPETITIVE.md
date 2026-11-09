# Competitive positioning: what the evidence supports

This page says what Scrutate 0.1.0 does better than the tools it was measured against, where it
ties, and where it is behind. Every performance statement points at docs/LEADERBOARD.md (generated
from the raw results in `bench/results/`); every capability statement points at a test. Claims
about other tools are limited to what was observed in those runs and to their documented options.
The machine is a developer laptop shared with other work (an Apple M3 Pro); section 14 of the
mission reserves "#1" claims for dedicated idle hardware, so the wins below are measured results,
not publishable leadership claims.

## 1. Against grep-class tools (GNU grep, ripgrep, ugrep, git grep, ag, ack)

SUMMARY_ENGINE_A

**What decides most large-tree benchmarks on macOS is the file system, not the matcher.** Merely
reading every file of the Linux tree (the "read files" baseline, `bench/rawread.c`, eleven threads)
takes about as long as the fastest search tools take to search it, because opening 96,000 files
costs more than scanning their bytes. Several benchmarks are therefore ties at the hardware limit
(BLOCKED), and the margins of the wins there are a few percent. Where the work is not dominated by
opening files, the differences are larger: printing a million matching lines (the search threads
format their own output) and the indexed path below.

**Capabilities the others do not have (each verified by tests):** identifier search across casings
(`--ident`), fuzzy search within N edits (ugrep has `-Z` with a different edit model), results
grouped by enclosing function (`--symbols`), and an index-backed `grep --index` whose output equals
the walk's.

**Where they remain better** (the FAIL and PARTIAL rows of the leaderboard, each with its gap):
WHERE_BEHIND

**Correctness differences found while making the comparison fair**, so the reader can judge them:
ag and ack have no Unicode case folding (`-i KÖNIG` finds nothing); ag, ack and git grep's own `-w`
use ASCII word boundaries (they report `ERR_PTR` glued to Chinese text as a whole word); GNU grep
needs a UTF-8 locale for Unicode word and case rules; tools other than ripgrep and Scrutate skip the
dotfiles a repository's `.gitignore` explicitly re-includes (`!.mailmap` in the Linux tree).

## 2. Against ast-grep (structural search)

STRUCTURAL

## 3. Against Zoekt and livegrep (indexed search)

Measured on the Linux kernel tree (95,578 files, 1.6 GB) on the development Mac, against Zoekt at
commit `a5a933bc` built with `go install` as its README says (docs/LEADERBOARD.md, Engine B
section; raw data in `bench/results/macos-arm64/indexed.json`). Every query was checked against an
independent reference, and both tools answered all 20 the same way as the reference.

- **Building the index**: 2.7 s against 138 s (0.019x), 154 MB on disk against 4,238 MB (0.036x),
  819 MB peak memory against 1,552 MB.
- **After editing 100 files**: 0.28 s, because the text index updates in place. Zoekt's directory
  indexer (`zoekt-index`) rebuilds the whole index (138 s); the corpus is a plain directory, so its
  git indexer was not part of the comparison.
- **Query latency** over 20 queries x 20 runs, each a fresh process: p50 50 ms against 59 ms
  (PASS), p99 195 ms against 879 ms (PASS), p95 150 ms against 265 ms (PARTIAL: the interval is
  wide). Memory per query 14 MB against 274 MB.
- **Throughput** with 11 clients at once: 21 against 22 queries per second, a tie (PARTIAL).

Why it is smaller and quicker to build: the text index stores trigram postings and no file contents,
and reads candidate files from the source tree, which must be present. Zoekt stores the contents
and can answer without the tree. Scrutate offers only the first mode in 0.1.0 (R-14.12).

LIVEGREP

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
