# Competitive positioning — what the evidence supports

This page says what Scrutate 0.1.0 does better than the tools it was benchmarked against, where it
is merely equal, and where it is behind. Every performance statement points at a table in
docs/BENCHMARKS.md (generated from `bench/results/final.json`); every capability statement points
at a test. Claims about other tools are limited to what was observed in those runs on this machine
(2 cores, Linux x86_64, Python-only corpora of 4 MB / 104 MB / 830 MB) and to their own
documented command-line options.

## 1. Against grep-class tools (grep, ripgrep, ugrep, git grep; ack/ag not measured)

**Speed on plain text search: within 2 % of ripgrep on average, ahead of the rest.** Across the
27 comparable rows, the geometric mean of ripgrep's time over Scrutate's is 0.98× (ripgrep 2 %
faster on average); Scrutate is faster on 7 rows and slower on 20, the slowest cases being output-heavy
(`whole word` with 192 k–1.5 M matching lines: 121 vs 77 ms on the big corpus, 769 vs 612 ms on
the massive one; a single output thread versus ripgrep's parallel printers — docs/ROADMAP.md).
Against GNU grep the mean ratio is 2.06×, against git grep 1.70×, against the bundled `+sse2`
ugrep 1.11×. Peak memory for the common-literal query: 8.5 / 9.7 / 11 MB (small / big / massive)
for Scrutate vs 6.9 / 7.4 / 7.8 MB for ripgrep and 2.2–2.3 MB for GNU grep — all small; CPU time
equal to ripgrep's. Startup: 5 ms, the same as ripgrep (GNU grep 1.7 ms, ugrep 3.7 ms).

**Capabilities none of them have (each verified by tests):**

| Capability | Scrutate | grep / ripgrep / ugrep / git grep |
|---|---|---|
| Identifier search across casings (`refreshToken` ≡ `refresh_token` ≡ `REFRESH_TOKEN`, digit- and acronym-aware) | `--ident`; 44–312 ms on big/massive, *faster* than ripgrep running the equivalent hand-written regex (46–339 ms) | needs a hand-written regex per query |
| Multiline search | `-U`: 44 ms (big), 260 ms (massive) — 2–3× faster than ripgrep's `-U` (93 / 770 ms) in these runs | ripgrep `-U` (slower here); ugrep via `-P`; grep/git grep: no |
| Approximate/fuzzy search | `--fuzzy=N` (Myers, prefiltered): 74 ms big, 541 ms massive | ugrep `-Z` (200 / 1 668 ms here; first character must match); others: no |
| Results grouped by enclosing function (`--symbols`) | yes, when an index exists | no |
| Ranked search by meaning, symbols, callers, references, imports, duplicates, structural queries | Engine B (next section) | no |
| Identical results | differential test: 0 disagreements with ripgrep in 750 random cases | — |

**Where they remain better:** GNU grep starts 3 ms faster (single binary with no thread pool);
ripgrep prints very large result sets up to ~25 % faster (`whole word`, massive: 612 vs 769 ms);
a natively built ugrep with AVX2 would likely be faster than the SSE2 build measured here (not
measured — stated as a limitation, not a result); ugrep searches inside archives and compressed
files, which Scrutate does not do in 0.1.0; ack and ag were not measured at all.

## 2. Against ast-grep (structural search)

Same tree-sitter parse, same node kind, identical match counts (2 302 / 114 522 / 916 176).
ast-grep is faster: 241 vs 255 ms (small), 9.9 vs 10.8 s (big), 77.6 vs 85.8 s (massive) —
Scrutate's `tsquery` is 6–11 % slower in these runs; peak memory 14 / 57 / 199 MB vs ast-grep's
16 / 53 / 55 MB (the ordered streaming buffer grows with the number of files on the massive corpus). ast-grep also offers rewriting (`--rewrite`) and a YAML rule language, which
Scrutate does not. What Scrutate adds is everything around the structural query: the structural
search runs inside the same tool that holds the index, the call graph and the ranked search, and
its output follows the same JSON conventions.

## 3. Against "nothing": the code-intelligence engine

None of the benchmarked tools has an index, so the following has no competitor in the comparison:

- **Ranked search by meaning** with a learned ranker: MRR@10 0.632 for natural-language queries on
  an undocumented held-out code base, vs 0.448 for BM25 over the same candidates; 0.93–0.99 for
  name/identifier queries (docs/QUALITY.md). In-process latency 8 ms (small), 15 ms (big),
  48 ms p50 on a 1.08 M-chunk index; the whole process incl. start-up and model load 55 / 129 / 734 ms.
- **Incremental index**: 25 s cold for 2.9 M lines (104 MB → 306 MB index), 0.19 s to absorb a
  one-file edit, 217 s for 23.5 M lines (830 MB → 2.4 GB, peak 2.2 GB RAM); `check` verifies it;
  corruption rebuilds it automatically (robustness tests).
- **Symbols for 60 languages** verified by snapshot samples, heuristics for the rest, every text
  file searchable; call graph, imports/dependents, references grouped by symbol, outline, similar
  code, exact and near-duplicate detection, cross-repository search.
- **One interface for humans and agents**: CLI with JSON everywhere, an MCP server with 14 tools,
  Python bindings with the GIL released.

## 4. The case for adopting it — stated at the strength the evidence allows

Scrutate replaces a grep-class tool without a speed penalty on this machine (within noise of
ripgrep, far ahead of grep and git grep), adds identifier, fuzzy and faster multiline search that
those tools do not offer, and adds a code-intelligence layer whose ranking quality is measured on
held-out code and whose implementation is verified bit-for-bit against a reference. Its outputs
are machine-readable and stable; its behaviour on hostile inputs, corrupted indexes and
concurrent writers is tested; its dependencies are audited and listed in an SBOM.

What the evidence does **not** support, and what this document therefore does not claim:
dominance over ripgrep on every text query (it is a tie, with ripgrep ahead on output-heavy
queries), superiority over ast-grep on structural search (ast-grep is 6–11 % faster), any comparison with
ack or ag, results on hardware other than a 2-core VM, or results on languages other than Python
for the indexing and quality figures. The honest summary is: **ripgrep-class speed (within 2 % on average), more capability,
measured and reproducible** — and a list of the places where the competition is still ahead.
