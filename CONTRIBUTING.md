# Contributing

## Layout

```
crates/scrutate/        the product: Engine A (instant.rs), Engine B (index.rs, lex.rs, parse.rs,
                        langs.rs, search.rs, semantic.rs, rank.rs, intel.rs), CLI (cli.rs,
                        config.rs), MCP server (mcp.rs); tests/ (samples, robustness, properties)
crates/scrutate-py/     optional PyO3 bindings (not built by default)
python/scrutate_ref/    the Python reference implementation of Engine B's index and ranking
                        (trains the models; must stay bit-identical with the Rust engine)
python/tests/           reference tests and bindings tests
models/                 semantic.npz, ranker.npz (embedded into the binary)
bench/                  benchmark harness, parity and differential tests, training scripts
tools/                  name research, SBOM, docs generators, reproducibility check
integrations/           VS Code, Neovim, JetBrains, GitLab, Bitbucket, MCP, pre-commit
docs/                   architecture, security, benchmarks, traceability, …
```

## Quality gates (all must pass before a change is merged)

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --exclude scrutate-py -- -D warnings
cargo test --release -p scrutate                  # unit + samples + robustness + properties
cargo audit
cd python && python -m pytest -q                  # reference engine (+ bindings if installed)
python bench/parity.py <repo> target/release/scrutate --queries 300   # Rust == Python, bit for bit
python bench/differential.py --cases 200          # Engine A == ripgrep on random inputs
```

## Rules that keep the engines honest

- **Parity first.** Any change to tokenization, BM25, vectors, features or the ranker must be made
  in both `python/scrutate_ref` and `crates/scrutate` and must keep `bench/parity.py` at
  "PARITY OK" with zero feature and score differences. Floating-point order matters: both sides
  accumulate in f64 where the reference does.
- **Index format changes bump `SCHEMA_VERSION`** in both engines; old indexes are then rebuilt
  automatically. Never read a field that the version check did not guarantee.
- **Adding a language**: one `LangSpec` in `langs.rs` (node kinds, name rules, call rules, import
  rules) plus `tests/samples/sample.<ext>` and its entry in `expected.json`
  (`UPDATE_EXPECTED=1 cargo test --test samples` records it; review the diff). Then regenerate
  docs/LANGUAGES.md with `tools/gen_languages_md.py`.
- **No new dependency without a reason** written in the PR: size, maintenance, licence, and what
  it replaces. Run `cargo audit` and `tools/sbom.py`.
- **No claims without evidence.** Numbers in docs come from a script in `bench/` or `tools/` that
  anyone can rerun; status words (PASS/PARTIAL/FAIL/BLOCKED) follow docs/TRACEABILITY.md.
- **Never execute repository content.** Parsers parse; nothing in a repository (including
  `.scrutate.toml`) may name a program to run.

## Commit messages

Imperative mood, one logical change per commit, reference the traceability row(s) a change
affects (`R-12.3`), and note benchmark deltas when performance is involved.
