# Fuzz targets

Coverage-guided fuzzing with libFuzzer through `cargo fuzz`, which needs a nightly toolchain. This
directory is a workspace of its own, so the product never builds with nightly.

| Target | What it checks |
|---|---|
| `npy` | arbitrary bytes as an `.npy` array: the reader rejects or reads them, never panics or over-allocates |
| `trigram_query` | soundness of the text index: if a regex, literal or identifier pattern matches a line, that line's trigrams satisfy the pattern's trigram condition |
| `fuzzy_matcher` | the edit-distance matcher: no panic, every match inside the haystack and on one line |
| `parse_file` | every parser tier under 60 file extensions: no panic, chunk line ranges inside the file |

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run -O trigram_query -- -max_total_time=600
```

`regressions/` keeps every input that once crashed a target. Replay one with
`cargo +nightly fuzz run -O <target> regressions/<target>/<file>`; `tools/gen_test_results.py`
replays them all. Each also has a unit test in the crate, so `cargo test` covers them on stable.
