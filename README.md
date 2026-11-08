# Scrutate

**Universal instant code search and persistent code intelligence, in one Rust binary.**

```
scrutate grep "refresh_token" .                 # Engine A: instant, index-free, ignore-aware search
scrutate grep --ident refreshToken .            #   one spelling finds refresh_token / RefreshToken / REFRESH_TOKEN
scrutate grep --fuzzy=1 "shortest_pth" .        #   approximate matching (Myers bit-parallel)
scrutate search "where do we renew expired tokens"   # Engine B: ranked search by meaning, learned ranker
scrutate callers refresh_token                  #   call graph, definitions, references, outline
scrutate dupes                                  #   exact and near-duplicate functions
scrutate tsquery python '(class_definition name: (identifier) @c)'   # structural search (tree-sitter)
scrutate serve                                  # MCP server for AI coding agents
```

Scrutate is two independent engines behind one CLI:

- **Engine A — universal instant search.** No index, no parsers, no background process. Literal,
  regex (linear-time, size-limited), identifier (all casings), fuzzy, multiline, whole-word and
  inverted matching over every readable file, with `.gitignore` awareness, parallel traversal,
  streaming results, cancellation, colour or JSON Lines output. Built on the same `grep-*` /
  `ignore` crates ripgrep is assembled from, and differentially tested against ripgrep.
- **Engine B — code intelligence.** A persistent, incremental index (SQLite + memory-mapped
  segments): tree-sitter symbols for 60 languages, keyword heuristics for the rest, call edges,
  imports, BM25 postings, semantic vectors and a LightGBM ranker. Ranked search by meaning, name or
  identifier; definitions, callers/callees, references, outlines, dependencies, similar and
  duplicate code, structural queries, cross-repository search.

Everything below is measured; the numbers come from scripts in `bench/` that you can rerun.

## Status: 0.1.0

This is the first release. It is **not** declared production-ready (docs/RELEASE_READINESS.md
says exactly what is and is not verified). What is verified, on Linux x86_64:

- 51 Rust tests (unit, 60-language snapshot, 8 robustness scenarios incl. corruption and
  concurrency, 7 property-based, 13 end-to-end CLI/MCP), 86 Python reference tests, 5 bindings
  tests — all passing (docs/TEST_RESULTS.md).
- Engine A agrees with ripgrep on 750 random corpora × patterns (0 disagreements) and runs within
  2 % of ripgrep on the geometric mean of 27 text-search workloads (faster on 7, 2.1× faster than
  GNU grep, 1.7× than git grep); where it is slower (output-heavy queries by up to 25 %, structural
  search vs ast-grep by 6–11 %) docs/BENCHMARKS.md says so.
- Engine B's Rust implementation is bit-identical to the Python reference it was developed
  against (300/300 queries; 200/200 on a 135k-chunk IVF index) and beats its own lexical layer on
  every held-out query type (docs/QUALITY.md).
- `cargo clippy -D warnings` and `cargo fmt` clean; `cargo audit`: 0 vulnerabilities in 177
  shipped crates; SBOM generated.

## Install

Linux x86_64: build from source with stable Rust (≥ 1.85):

```sh
cargo build --release -p scrutate --locked      # target/release/scrutate
```

Release archives, checksums, Sigstore attestations, musl static binaries and wheels for macOS and
Windows are produced by `.github/workflows/release.yml`, which has not run yet; those platforms
are therefore not yet declared supported (docs/INSTALL.md).

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
| docs/BENCHMARKS.md | reproducible comparison with grep, ripgrep, git grep, ugrep, ast-grep (generated) |
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

MIT or Apache-2.0, at your option.
