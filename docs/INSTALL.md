# Installation, update, uninstallation

Scrutate is a single static-friendly binary plus an optional Python package. Nothing runs in the
background and nothing phones home.

## Supported platforms (0.1.0)

A platform is declared supported only after its binary and wheel were built, installed and tested
on it. Every row below names the evidence; `bench/results/platforms/` holds the logs.

| Platform | Binary | Wheels | Status | Evidence |
|---|---|---|---|---|
| macOS arm64 (Apple Silicon, macOS 27) | built and tested natively | `cp39-abi3` on CPython 3.10, 3.11, 3.12, 3.13; `cp314t` on free-threaded 3.14 | **supported** | docs/TEST_RESULTS.md; `wheels-macos-arm64.json` |
| Linux aarch64 (glibc and static musl) | built and tested in a clean `rust:1-slim-bookworm` container (no Python on the image); the static musl binary prints byte-identical output | `cp39-abi3` on CPython 3.10, 3.12, 3.13; `cp314t` on free-threaded 3.14 | **supported** | `linux-aarch64/verify.log`, `musl-identical.txt`, `wheels.json` |
| Linux x86_64 (glibc and static musl) | the same script in a `linux/amd64` container, run under emulation on the Apple Silicon host: it verifies behaviour, not x86 speed | same as aarch64 | **supported** | `linux-x86_64/verify.log`, `musl-identical.txt`, `wheels.json` |
| macOS x86_64 | cross-built; the test binaries compile and link, none was run (Rosetta 2 is not installed on the build machine) | not built | **not supported yet** | `macos-x86_64/verify.log` |
| Windows x86_64 | cross-built with MinGW-w64; the test binaries compile and link, none was run | not built | **not supported yet** | `windows-x86_64-cross/build.log` |
| Windows ARM64 | not built | not built | **not supported yet** | no toolchain or machine; the CI matrix (`.github/workflows/ci.yml`, `release.yml`) covers Windows and Intel macOS but has not run, because the repository is not published yet |

## From a release archive

No archive is published for 0.1.0: the repository is not public yet. Once releases exist, the steps
are:

```sh
v=0.1.0; t=x86_64-unknown-linux-gnu
curl -sSfLO "https://github.com/crisnahine/scrutate/releases/download/v$v/scrutate-$v-$t.tar.gz"
curl -sSfLO "https://github.com/crisnahine/scrutate/releases/download/v$v/scrutate-$v-$t.sha256"
sha256sum -c "scrutate-$v-$t.sha256"        # checksum
gh attestation verify "scrutate-$v-$t.tar.gz" --owner crisnahine   # Sigstore provenance (optional)
tar xzf "scrutate-$v-$t.tar.gz"
install -m 0755 "scrutate-$v-$t/scrutate" ~/.local/bin/scrutate
scrutate completions bash > ~/.local/share/bash-completion/completions/scrutate   # or zsh/fish/powershell
```

Windows: unzip, put `scrutate.exe` on `PATH`, `scrutate completions powershell | Out-String | Invoke-Expression`.

## From source (any platform with stable Rust 1.89 or newer)

From a copy of the source tree (the clone URL works once the repository is published):

```sh
git clone https://github.com/crisnahine/scrutate && cd scrutate
cargo build --release -p scrutate --locked          # a few minutes; binary at target/release/scrutate
cargo test --release -p scrutate                     # the Rust test suite
```
`rust-toolchain.toml` pins the compiler (1.99.1) so builds are reproducible; rustup installs it on
first use. Add `--features extra-langs` for the optional heavy grammars (Fortran, OCaml, F#,
Verilog, VHDL, CUDA, TLA+, D, AutoHotkey, Odin, QML, LLVM IR; about 80 MB more).

## Python package

```sh
pip install maturin
maturin build --release --strip -m crates/scrutate-py/Cargo.toml -o dist
pip install dist/scrutate-*.whl
python -c "import scrutate; print(scrutate.__version__)"
```
One `cp39-abi3` wheel per OS/architecture covers CPython 3.9+ (standard builds). Free-threaded
Python needs the separate `cp314t` wheel built by the release workflow.

## Updating

Replace the binary with the new release (same steps). Indexes are caches: a new version that
changes the index schema or the bundled model re-indexes repositories automatically on first use
(the message `index schema changed; rebuilding the index` is normal).

## Uninstalling

```sh
rm ~/.local/bin/scrutate ~/.local/share/bash-completion/completions/scrutate
find ~/code -maxdepth 3 -name .scrutate -type d -prune -exec rm -rf {} +   # the caches, wherever you indexed
rm -rf ~/.config/scrutate                                                  # optional configuration
pip uninstall scrutate                                                     # the Python package, if installed
```
Scrutate writes nothing else: no services, no registry keys, no files outside the repositories you
indexed and the configuration directory you may have created.

## Package managers

None is published for 0.1.0. The package definitions are in `packaging/`: a Homebrew formula, a
Scoop manifest, winget manifests, and `.deb`/`.rpm` builds from `crates/scrutate/Cargo.toml`.
`packaging/README.md` says how each was tested and what is left to do at release time. A
`cargo install scrutate` from crates.io is not possible yet: the bundled models are too large for a
crates.io upload.
