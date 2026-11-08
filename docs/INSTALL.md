# Installation, update, uninstallation

Scrutate is a single static-friendly binary plus an optional Python package. Nothing runs in the
background and nothing phones home.

## Supported platforms (0.1.0)

| Platform | Status | How it was verified |
|---|---|---|
| Linux x86_64 (glibc) | **tested** | built and the whole test suite run on this platform (docs/TEST_RESULTS.md) |
| Linux x86_64 (musl, fully static) | **not yet verified** | CI job `musl` in `.github/workflows/ci.yml` builds it, checks `file`/`ldd`, compares output byte-for-byte with the glibc build and times both; it has not run yet |
| Linux aarch64, macOS (Intel, Apple Silicon), Windows (x86_64, ARM64) | **not yet verified** | built and tested only by the CI matrix (`ci.yml`, `release.yml`), which has not run yet |

A platform is declared supported only after its binary (and wheel) has been installed and tested
on it. Until the CI matrix has run, Linux x86_64 is the only supported platform.

## From a release archive

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

## From source (any platform with stable Rust ≥ 1.85)

```sh
git clone https://github.com/crisnahine/scrutate && cd scrutate
cargo build --release -p scrutate --locked          # ~2–3 minutes; binary at target/release/scrutate
cargo test --release -p scrutate                     # the Rust test suite
```
Add `--features extra-langs` for the optional heavy grammars (Fortran, OCaml, F#, Verilog, VHDL,
CUDA, TLA+, D, AutoHotkey, Odin, QML, LLVM IR; +~80 MB).

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

Homebrew, Scoop, winget, Debian/RPM packages and a `cargo install scrutate` crates.io release are
planned for a later release (docs/ROADMAP.md); none exists for 0.1.0.
