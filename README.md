# Scrutate

**Universal instant code search and persistent code intelligence, in one Rust binary.**

```
scrutate grep "refresh_token" .                 # Engine A: instant, index-free, ignore-aware search
scrutate grep --ident refreshToken .            #   one spelling finds refresh_token / RefreshToken / REFRESH_TOKEN
scrutate grep --fuzzy=1 "shortest_pth" .        #   approximate matching (Myers bit-parallel)
scrutate index && scrutate grep --index "PM_RESUME"   # Engine B text index: the same answer, from a trigram index
scrutate search "where do we renew expired tokens"   # Engine B: ranked search by meaning, learned ranker
scrutate callers refresh_token                  #   call graph, definitions, references, outline
scrutate dupes                                  #   exact and near-duplicate functions
scrutate tsquery python '(class_definition name: (identifier) @c)'   # structural search (tree-sitter)
scrutate serve                                  # MCP server for AI coding agents
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
- **Tests**: 65 Rust tests (unit, 60-language snapshot, robustness, property, end-to-end CLI and
  MCP), 81 Python reference tests, the bindings tests, Rust/Python parity, fuzz regressions and a
  reproducible-build check, all passing (docs/TEST_RESULTS.md). Engine A agrees with ripgrep on
  2,000 random corpora and patterns, run three ways: as shipped, with the parallel walk forced,
  and with every file searched in ranges.
- **Benchmarks**: LEADERBOARD_SUMMARY
- **Engine B** gives bit-identical features, scores and rankings to the Python reference it was
  developed against (300 queries in the parity harness), and its learned ranker beats its own
  lexical layer on every held-out query type (docs/QUALITY.md).
- `cargo clippy -D warnings` and `cargo fmt` are clean; `cargo audit` reports no vulnerabilities;
  an SBOM is generated.

## Install

From source, with Rust 1.89 or newer (`rust-toolchain.toml` pins 1.99.1):

```sh
cargo build --release -p scrutate --locked      # target/release/scrutate
```

No release archive is published yet. `.github/workflows/release.yml` builds archives, checksums,
Sigstore attestations, musl binaries and wheels for every platform once the repository is public
(docs/INSTALL.md).

Python bindings (`pip install` the abi3 wheel built by maturin from `crates/scrutate-py`):

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
| docs/INSTALL.md, docs/DEPENDENCIES.md, docs/ROADMAP.md, CHANGELOG.md, CONTRIBUTING.md | |
| integrations/ | VS Code, Neovim, JetBrains, GitHub Action, GitLab, Bitbucket, MCP, pre-commit |

## Licence

Apache-2.0. See [LICENSE](LICENSE). Every crate compiled into the binary is under a licence
compatible with it (MIT, BSD, Zlib, Unicode-3.0 or Apache-2.0); their licence and notice files are
collected in `THIRD_PARTY_NOTICES.txt`, which release archives and wheels include
(`tools/third_party_notices.py`).
