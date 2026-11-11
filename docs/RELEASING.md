# Releasing

The steps a release takes here, in order. The list comes from what the 0.1.0 release touched
(`git log --stat` on its last commits) and from what the scripts refuse. Tick a step when its
output has been read, not when the command was started. Nothing is published from this
repository yet (docs/ROADMAP.md), so the list ends at local artifacts.

Any change to `crates/scrutate` or `vendor/ignore` after step 2 sends the release back to step 2:
the benchmarks, the test results and the artifacts all name one binary by its SHA-256.

## 1. Version and date

The version is written in `crates/scrutate/Cargo.toml`, `crates/scrutate-py/Cargo.toml`,
`Cargo.lock` (both packages), `integrations/vscode/package.json`,
`integrations/gitlab/.gitlab-ci.yml`, `integrations/bitbucket/bitbucket-pipelines.yml`,
`packaging/homebrew/scrutate.rb`, `packaging/scoop/scrutate.json` and the winget manifests (the
directory name too). The date is in the `CHANGELOG.md` heading, the title of `docs/DELIVERY.md`
and the scope line of `docs/SECURITY.md`.

## 2. Benchmarks on the release binary

Move the previous results out of the way first: the harness resumes from an existing file. Each
goes to `bench/results/<platform>/superseded/` with the first four hex digits of its binary in
the name, a row in that directory's `README.md`, and an entry in `HISTORY` in
`bench/make_dashboard.py`.

macOS, about 4.5 hours:

```sh
cargo build --release -p scrutate --locked && sleep 3600 && bash bench/run_macos.sh
```

- A just-built executable starts about 0.2 ms slower than the same file an hour later. Wait.
- Do not run `cargo test` or `cargo build` until the chain ends: both can replace the binary.
- Do not edit `bench/run_macos.sh` while it runs. bash reads it as it goes.
- Close the browser and the editor. With 18 GB the page cache cannot hold the LLVM tree beside
  them, and a warm run stops being warm (the harness records it as `resident`).

Linux, about 1.5 hours, in a Colima VM with one virtual CPU per performance core
(`colima start --cpu 5 --memory 12`; with 8, three run on efficiency cores and every tool's
medians move by 10 to 30%). The commands are in the headers of
`bench/competitors/linux_bench.sh` and `bench/competitors/linux_indexed.sh`; run the second
after the first. Colima shares only the home directory with containers.

Then read the results before going on:

- Every row's status, and the rows marked "measured again".
- `python3 bench/leaderboard_regression.py OLD.json NEW.json` against the previous run of each
  file. A flagged row that every tool shares is the machine; one that only scrutate shows is
  the code. `/usr/bin/time -l` gives instructions retired, which a busy machine does not move.

## 3. Generated documents

```sh
M=bench/results/macos-arm64 L=bench/results/linux-aarch64
python3 bench/make_leaderboard_md.py --engine-a $M/leaderboard-warm.json --engine-a $M/leaderboard-cold.json \
  --engine-a $L/leaderboard-warm.json --engine-a $L/leaderboard-cold.json --indexed $M/indexed.json --indexed $L/indexed.json
python3 bench/make_benchmarks_md.py $M/compare.json
python3 bench/make_dashboard.py
python3 tools/gen_dependencies_md.py     # when Cargo.lock changed
python3 tools/gen_languages_md.py        # when a language changed
```

## 4. Documents that carry the numbers by hand

`README.md` (Status), `docs/COMPETITIVE.md`, `docs/ROADMAP.md`, `docs/TRACEABILITY.md` (R-4.6,
R-14.6, R-14.16, then `python3 tools/traceability_tally.py`), `docs/RELEASE_READINESS.md` (its
table lists every row that is not PASS), `CHANGELOG.md`, `docs/SECURITY.md` (the `unsafe`
inventory and the crate count), `docs/DELIVERY.md` (binary hashes and wheel names, after step 6).

## 5. Quality gates

In a virtual environment with the reference's dependencies, a copy of networkx 3.4.2 as the
parity repository, and both wheels:

```sh
V=/some/scratch/venv W=/some/scratch/wheels P=/some/scratch/pmid
uv venv --python 3.13 $V && uv pip install --python $V/bin/python -e "python[test]" networkx==3.4.2
cp -r $V/lib/python3.13/site-packages/networkx $P && find $P -name __pycache__ -prune -exec rm -rf {} +
export MACOSX_DEPLOYMENT_TARGET=13.0
maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o $W
maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o $W -i "$(uv python find 3.14t)"
uv pip install --python $V/bin/python --reinstall $W/*abi3*.whl
$V/bin/python tools/gen_test_results.py --repo $P --differential-cases 2000 --reproducible --wheels $W
```

It writes `docs/TEST_RESULTS.md`. Every gate must say PASS, the fuzz regressions included (they
build in a workspace of their own, with its own `[patch]` for `vendor/ignore`). The hash under
"reproducible build" must be the one docs/LEADERBOARD.md names for macOS.

## 6. Platform checks and artifacts

```sh
tools/repro_docker.sh                                   # one hash from two build images
docker run --rm --platform linux/arm64 -v "$PWD":/src:ro -v "$PWD/bench/results/platforms":/out \
  rust:1-slim-bookworm bash /src/tools/linux_verify.sh  # tests with no Python, musl, wheels
tools/linux_x86_cross_verify.sh ~/.cache/scrutate-bench/x86work
tools/windows_cross_check.sh
tools/macos_x86_verify.sh
tools/release_local.sh                                  # dist/ on macOS
```

`dist-linux/` is `tools/release_local.sh` run in a clean `rust:1-slim-bookworm` container on a
copy of the tree at `/build`, with `maturin[patchelf]` installed in a virtual environment.

Check that three hashes agree on each platform: the binary inside the archive, the one
docs/LEADERBOARD.md says was measured, and the reproducible-build hash (`tools/repro_docker.sh`
for Linux, docs/TEST_RESULTS.md for macOS). The archives contain `docs/`, so build them last.

## 7. What a published release adds

A tag `v*` starts `.github/workflows/release.yml` (every platform, signing, attestations); the
package definitions in `packaging/` need the release checksums (packaging/README.md). None of it
has run: the repository is not public.
