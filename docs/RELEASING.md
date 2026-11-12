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

`python3 tools/check_version.py 0.1.0` fails when any of them differs from the version given:
both `Cargo.toml` files, the VS Code `package.json` and its lock file, the Scoop manifest, the
winget directory and manifests, the Homebrew URL, the GitLab and Bitbucket templates, and the
examples in the GitHub Action and pre-commit READMEs. The release workflow runs it on
the tag (without its `v`) before anything else and refuses a tag that does not match.

The oldest compiler that builds is `rust-version` in the workspace `Cargo.toml`: 1.91, which the
locked `notify-types` needs. A `ci.yml` job runs `cargo check` with that version, so a
dependency update that raises it fails there; README.md and docs/INSTALL.md name it too.

`CONTENT_REVISION` in `crates/scrutate/src/index.rs` and `python/scrutate_ref/index.py` names
the parsing rules an index was built with, and the index keeps it in `meta` as `content_rev`.
After a release, a change to `langs.rs`, `parse.rs`, `text.rs`, `parse.py` or `text.py` raises
it by one in both files, so an index of the older rules reads every file again on its next
update. 0.1.0 ships with 2: builds with the first rules were in use before the release, and
their indexes must not keep chunks cut by those rules. Raise `SCHEMA_VERSION` when the database
layout changes, `MODEL_REVISION` when the embedded semantic model changes, and the text index
`FORMAT` when its files change. Each of the four only ever goes up: a binary leaves an index of
a higher number alone (it reads a newer content revision as it is and refuses the other
three), so two versions on one checkout never rebuild the index in turn.

## 2. Benchmarks on the release binary

Move the previous results out of the way first: the harness resumes from an existing file. Each
goes to `bench/results/<platform>/superseded/` with the first four hex digits of its binary in
the name, a row in that directory's `README.md`, and an entry in `HISTORY` in
`bench/make_dashboard.py`.

macOS, about 4.5 hours:

```sh
cargo build --release -p scrutate --locked && sleep 3600 && bash bench/run_macos.sh
```

- Before the build, on Apple silicon: `python3 tools/order_file.py --check` reports the share of
  the names in `crates/scrutate/startup.order` that the release links still have. Under 80 %, or
  after a toolchain or `Cargo.lock` change, write it again
  (`python3 tools/order_file.py --dirs ~/.cache/scrutate-bench/corpora/startup
  ~/.cache/scrutate-bench/corpora/ripgrep`, about 10 minutes, needs the `llvm-tools` component)
  and commit it. A stale file only costs pages at start; it never breaks the build.
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
- The regression gate measures the change and its base in one run. With
  `SCRUTATE_BASE_BIN=<the previous release binary>` set, `bench/leaderboard.py` times that binary
  as `scrutate base` in the same interleaved blocks as `scrutate` (never ranked, and left out of
  the published tables), and `python3 bench/leaderboard_regression.py --within FILE.json` compares
  the two inside that file. The ratio is taken inside each block: the median over the blocks of
  median(new) / median(base) in that block, with the interval from resampling inside each block.
  A row regresses when that 95% interval lies above 1.10. A slow minute slows both binaries, so
  two runs taken minutes apart are no gate. `--redo-unstable` refuses to continue a run whose
  `scrutate` binary or base binary is not the one it measured.
  `leaderboard_regression.py OLD.json NEW.json` still compares two recorded runs of one machine.
  `/usr/bin/time -l` gives instructions retired, which a busy machine does not move.

## 3. Generated documents

```sh
M=bench/results/macos-arm64 L=bench/results/linux-aarch64
python3 bench/make_leaderboard_md.py --engine-a $M/leaderboard-warm.json --engine-a $M/leaderboard-cold.json \
  --engine-a $L/leaderboard-warm.json --engine-a $L/leaderboard-cold.json --indexed $M/indexed.json --indexed $L/indexed.json
python3 bench/make_benchmarks_md.py $M/compare.json
python3 bench/make_dashboard.py
python3 tools/gen_dependencies_md.py     # when Cargo.lock changed
python3 tools/gen_languages_md.py        # when a language changed
python3 tools/scrub_home.py              # last: logs and results write ~ for the home directory
```

## 4. Documents that carry the numbers by hand

`README.md` (Status), `docs/COMPETITIVE.md`, `docs/ROADMAP.md`, `docs/TRACEABILITY.md` (R-4.6,
R-14.6, R-14.16, then `python3 tools/traceability_tally.py`), `docs/RELEASE_READINESS.md` (its
table lists every row that is not PASS), `CHANGELOG.md`, `docs/SECURITY.md` (the `unsafe`
inventory and the crate count), `docs/DELIVERY.md` (binary hashes and wheel names, after step 6).

## 5. Quality gates

In a virtual environment with the reference's dependencies (the versions in
`python/constraints.txt`), a copy of networkx 3.4.2 as the parity repository, and the three
wheels: `cp39-abi3`, `cp314t` and `cp315-abi3.abi3t`.

```sh
V=/some/scratch/venv W=/some/scratch/wheels P=/some/scratch/pmid
uv venv --python 3.13 $V && uv pip install --python $V/bin/python -e "python[test]" networkx==3.4.2 -c python/constraints.txt
cp -r $V/lib/python3.13/site-packages/networkx $P && find $P -name __pycache__ -prune -exec rm -rf {} +
export MACOSX_DEPLOYMENT_TARGET=13.0
python3 tools/third_party_notices.py --package scrutate-py --out crates/scrutate-py/THIRD_PARTY_NOTICES.txt
for py in 3.13 3.14t 3.15; do
  maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o $W -i "$(uv python find $py)"
done
uv pip install --python $V/bin/python --reinstall $W/*cp39-abi3*.whl
$V/bin/python tools/gen_test_results.py --repo $P --differential-cases 2000 --reproducible --wheels $W
```

The wheel gate installs on 3.10 to 3.13, 3.14t, 3.15 and 3.15t. A final free-threaded 3.15 needs
uv 0.13 or newer; with an older uv, install one elsewhere
(`UV_PYTHON_INSTALL_DIR=/some/scratch uvx uv@latest python install 3.15t --no-bin`) and name every
interpreter: `--pythons 3.10 3.11 3.12 3.13 3.14t 3.15 /some/scratch/.../bin/python3.15t`.

It writes `docs/TEST_RESULTS.md`. Every gate must say PASS, the fuzz regressions included (they
build in a workspace of their own, with its own `[patch]` for `vendor/ignore`). The hash under
"reproducible build" must be the one docs/LEADERBOARD.md names for macOS.

## 6. Platform checks and artifacts

```sh
actionlint && uvx zizmor --offline --persona pedantic .github   # the workflows, when one changed
docker run --rm -i --platform linux/arm64 -v "$PWD":/src:ro python:3.13-slim-bookworm \
  bash /src/tools/ci_rehearsal.sh | tee bench/results/platforms/linux-aarch64/ci-rehearsal.log
docker run --rm --platform linux/arm64 -e BASE=<the previous release> -v "$PWD":/src:ro python:3.13-slim-bookworm \
  bash /src/tools/bench_rehearsal.sh | tee bench/results/platforms/linux-aarch64/benchmarks-rehearsal.log
tools/repro_docker.sh                                   # one hash from two build images
docker run --rm --platform linux/arm64 -v "$PWD":/src:ro -v "$PWD/bench/results/platforms":/out \
  rust:1-slim-bookworm bash /src/tools/linux_verify.sh  # tests with no Python, musl, wheels
tools/linux_x86_cross_verify.sh ~/.cache/scrutate-bench/x86work
tools/windows_cross_check.sh
tools/macos_x86_verify.sh
tools/release_local.sh                                  # dist/ on macOS
uvx --with jsonschema --with pyyaml python packaging/validate_manifests.py --draft   # Scoop, winget
```

`--draft` lets the all-zero sha256 placeholders through; without it the check fails on them, so
it is run without `--draft` only once the manifests carry the release checksums. The `.deb` and
`.rpm` (`packaging/linux/build-packages.sh`) carry `THIRD_PARTY_NOTICES.txt`
(`/usr/share/doc/scrutate/` in the `.deb`, `/usr/share/licenses/scrutate/` in the `.rpm`).

`dist-linux/` is `tools/release_local.sh` run in a clean `rust:1-slim-bookworm` container on a
copy of the tree at `/build`, with `maturin[patchelf]` installed in a virtual environment.

Check that three hashes agree on each platform: the binary inside the archive, the one
docs/LEADERBOARD.md says was measured, and the reproducible-build hash (`tools/repro_docker.sh`
for Linux, docs/TEST_RESULTS.md for macOS). The archives contain `docs/`, so build them last.

## 7. What a published release adds

A tag `v*` starts `.github/workflows/release.yml` (every platform, signing, attestations). It
checks the version first, then runs all of `ci.yml` on the tagged commit (`workflow_call`); the
release job waits for that run and for the binaries and wheels. The package definitions in
`packaging/` need the release checksums (packaging/README.md). The release workflow has not
run. A manual run of it (`gh workflow run release.yml`) is a rehearsal: it builds, tests and
packages the binaries and the three wheel kinds on every platform, installs each wheel, and
publishes and signs nothing. It runs 8 binary jobs and 18 wheel jobs, six of each kind on macOS
and Windows runners, which count ten and two times on a private repository. The repository is on GitHub, private, with `main` only. Run 43013926055 of `ci.yml` (on
`9384c18`) is the first green one: all 12 jobs that run on a push passed, the Rust tests on
macOS arm64, Intel macOS, Linux x86_64, Linux arm64, Windows x64 and Windows arm64, the musl
build, the extra-langs job on the oldest supported Rust, the fuzz regressions, the Python
reference, bindings, parity and differential job, the advisories and the editor integrations.
The dependency review runs only on pull requests. The workflows pass
`actionlint` and `zizmor`, and the Linux jobs of `ci.yml` pass in a container
(`tools/ci_rehearsal.sh`).

Before the first tag, in the repository's settings: a ruleset on `main` (pull request, the `ci`
checks required, no force push), a tag ruleset on `v*`, an environment named `release` with a
required reviewer (the publishing job waits for it), immutable releases, push protection, and
CodeQL default setup. The release job makes a draft, attaches every file, then publishes, which
is the order immutable releases need. Its notes are the version's section of `CHANGELOG.md`.

R-6.1, R-9.10 and R-12.2 in docs/TRACEABILITY.md are PASS from run 43013926055, the first green
run of `ci.yml`. R-6.12 and R-13.9 stay PARTIAL until `release.yml` and `benchmarks.yml` have run
on a runner: flip them from the recorded runs, not before.

After the release is out: take `open-pull-requests-limit: 0` out of `.github/dependabot.yml`.
Version updates are paused until then, because each Dependabot pull request runs `ci.yml` on six
runners, and on a private repository macOS minutes count ten times. A bump of `snowballstemmer`
changes stems: merge it only with a parity run (`bench/parity.py`) that still ends in `PARITY OK`.
