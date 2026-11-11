# Scrutate

**Universal instant code search and persistent code intelligence, in one Rust binary.**

```
scrutate grep "refresh_token" .                 # Engine A: instant, index-free, ignore-aware search
scrutate grep --ident refreshToken .            #   one spelling finds refresh_token / RefreshToken / REFRESH_TOKEN
scrutate grep --fuzzy=1 "shortest_pth" .        #   approximate matching (Myers bit-parallel)
scrutate grep --fuzzy-regex=1 '(get|set)_valu'  #   a regex within N edits
scrutate grep -P '(?<=self\.)cache\b' .         #   PCRE2: look-around, backreferences
scrutate grep -z "TODO" dist/                   #   inside zip/tar archives and compressed files
scrutate index && scrutate grep --index "PM_RESUME"   # Engine B text index: the same answer, from a trigram index
scrutate search "where do we renew expired tokens"   # Engine B: ranked search by meaning, learned ranker
scrutate callers refresh_token                  #   call graph, definitions, references, outline
scrutate refs Graph --def networkx/classes/graph.py   # references resolved through imports and scopes
scrutate dupes                                  #   exact and near-duplicate functions
scrutate tsquery python '(class_definition name: (identifier) @c)'   # structural search (tree-sitter)
scrutate serve --watch                          # MCP server for AI coding agents, index kept fresh
scrutate lsp                                    # language server for editors (VS Code, JetBrains)
```

Scrutate is two independent engines behind one CLI:

- **Engine A, universal instant search.** No index, no parsers, no background process. Literal,
  regex (linear-time, size-limited), identifier (all casings), fuzzy, multiline, whole-word and
  inverted matching over every readable file, with `.gitignore` awareness, parallel traversal,
  streaming results, cancellation, colour or JSON Lines output. Built on the same `grep-*` /
  `ignore` crates ripgrep is assembled from, and differentially tested against ripgrep.
- **Engine B, code intelligence.** A persistent, incremental index (SQLite + memory-mapped
  segments): tree-sitter symbols for 60 languages (72 with `--features extra-langs`), keyword
  heuristics for the rest, call edges, imports, BM25 postings, semantic vectors and a LightGBM
  ranker; and a trigram text index that answers grep-style queries with Engine A's exact output.
  Ranked search by meaning, name or identifier; definitions, callers/callees, references,
  outlines, dependencies, similar and duplicate code, structural queries, cross-repository search.

Everything below is measured; the numbers come from scripts in `bench/` that you can rerun.

## Status: 0.1.0

This is the first release. It is **not** declared production-ready: docs/RELEASE_READINESS.md says
exactly what is and is not verified.

- **Platforms**: macOS arm64, Linux aarch64 and Linux x86_64 are built and tested (Linux in clean
  containers with no Python on the image, including static musl binaries and wheels for CPython
  3.10 to 3.13 and free-threaded 3.14). macOS x86_64 and Windows x86_64 are cross-built only
  (docs/INSTALL.md).
- **Tests**: 122 Rust tests (unit, 60-language snapshot, robustness, property, end-to-end CLI and
  MCP, incremental parsing), 82 Python reference tests, the bindings tests, Rust/Python parity, fuzz
  regressions and a reproducible-build check, all passing (docs/TEST_RESULTS.md). Engine A agrees
  with ripgrep on 2,000 random corpora and patterns, run three ways: as shipped, with the parallel
  walk forced, and with every file searched in ranges.
- **Benchmarks** (docs/LEADERBOARD.md, generated from the raw results in `bench/results/`; 30 runs
  per contender, 95% intervals): on the development Mac (Apple M3 Pro, shared with other work, so
  none of this is a publishable #1 claim), Engine A is significantly fastest in
  all 37 warm-cache benchmarks and all 5 cold-cache ones: 15 to 30% ahead of the next tool on 12 of
  13 text searches of the Linux kernel tree and 45% ahead on the one with 1.3 million matching
  lines, 15 to 30% on five of six LLVM rows, 24 to 26% on 200,000 small files, 17 to 24% on a
  211-file repository, 43 to 58% on a 740 MB file, 23 to 28% with a cold cache. Six rows that other
  work on the laptop disturbed were measured twice; the table gives both. Structural search is 6 and
  14 times faster than ast-grep. Against Zoekt the text index builds 25 times faster, is a tenth of
  the size, answers in 0.37 of the median time and serves 1.2 times the queries per second; all
  eight indexed metrics lead. In a 5-CPU Linux virtual machine on the same Mac, Engine A is
  significantly fastest on all 14 Linux-tree benchmarks (7 to 16% ahead of ripgrep on 11, 40% with
  very many matches, 59% on the complex regex, 24 times faster than ast-grep on the structural
  query) and on all 4 cold-cache ones, where it takes 0.28 to 0.30 of the next tool's time. There
  the text index leads Zoekt and livegrep on all 8 indexed metrics (3.9 times the throughput of the
  next).
- **Engine B** gives bit-identical features, scores and rankings to the Python reference it was
  developed against (300 queries in the parity harness), and its learned ranker beats its own
  lexical layer on every held-out query type, in Python and five more languages (docs/QUALITY.md).
- `cargo clippy -D warnings` and `cargo fmt` are clean; `cargo audit` reports no vulnerabilities;
  an SBOM is generated.

## Install

From source, with Rust 1.91 or newer (`rust-toolchain.toml` pins 1.99.1):

```sh
cargo build --release -p scrutate --locked      # target/release/scrutate
```

No release archive is published yet. `.github/workflows/release.yml` builds archives, checksums,
Sigstore attestations, musl binaries and wheels for every platform once the repository is public
(docs/INSTALL.md).

Python bindings (`pip install` the wheel built by maturin from `crates/scrutate-py`):

```python
import scrutate
for m in scrutate.grep("TODO", ["."], mode="literal"):      # streamed, GIL released
    print(m.path, m.line, m.text)
repo = scrutate.Repo("."); repo.index()
print(repo.search("parse a date string", k=5))
```

## Documentation

| | |
|---|---|
| docs/CLI.md | every command, option, exit code, environment variable, config file |
| docs/ARCHITECTURE.md | how the engines work and why |
| docs/LEADERBOARD.md | the benchmark leaderboard: every tool, every benchmark, confidence intervals, statuses (generated) |
| docs/BENCHMARKS.md | latency, CPU, memory, start-up, concurrency and index metrics against grep-class tools (generated) |
| docs/QUALITY.md | ranking quality on held-out code (MRR, hit@k) |
| docs/LANGUAGES.md | verified language capability registry (generated) |
| docs/SECURITY.md | threat model, controls, evidence, residual risks |
| docs/TRACEABILITY.md | every mission requirement → implementation → evidence → PASS/PARTIAL/BLOCKED |
| docs/RELEASE_READINESS.md | what 0.1.0 is, remaining defects and incomplete requirements |
| docs/COMPETITIVE.md | evidence-based positioning against existing tools |
| docs/NAMING.md | how the name was chosen and what was (not) checked |
| docs/TEST_RESULTS.md | every quality gate with its command and output (generated) |
| docs/DELIVERY.md | where each deliverable lives, and the release artifacts with their hashes |
| docs/RELEASING.md | the release checklist |
| docs/dashboard.html | every benchmark run so far, charted (generated) |
| GLOSSARY.md | the words this codebase uses for its own concepts |
| docs/INSTALL.md, docs/DEPENDENCIES.md, docs/ROADMAP.md, docs/RESEARCH.md, CHANGELOG.md, CONTRIBUTING.md | |
| integrations/ | VS Code, Neovim, JetBrains, GitHub Action, GitLab, Bitbucket, MCP, pre-commit |

## Licence

Apache-2.0. See [LICENSE](LICENSE). Every crate compiled into the binary is under a licence
compatible with it (MIT, BSD, Zlib, Unicode-3.0 or Apache-2.0); their licence and notice files are
collected in `THIRD_PARTY_NOTICES.txt`, which release archives and wheels include
(`tools/third_party_notices.py`).
