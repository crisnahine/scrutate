#!/usr/bin/env bash
# macOS x86_64 on an Apple Silicon Mac: build the x86_64 binary and wheels, run the Rust test suite
# and the wheel matrix with x86_64 interpreters, all executed by Rosetta 2. This verifies the
# x86_64 build and its behaviour on macOS; it is not a measurement of Intel hardware. Without
# Rosetta 2 (`softwareupdate --install-rosetta`) only the build and the test compilation run.
#   tools/macos_x86_verify.sh
set -euo pipefail
cd "$(dirname "$0")/.."
out=bench/results/platforms/macos-x86_64
mkdir -p "$out"
exec > >(tee "$out/verify.log") 2>&1
t=x86_64-apple-darwin
export CARGO_TARGET_DIR=target/x86
rustup target add "$t"
echo "=== build"
cargo build --release -p scrutate --locked --target "$t"
file "$CARGO_TARGET_DIR/$t/release/scrutate"
if ! /usr/bin/arch -x86_64 /bin/ls / > /dev/null 2>&1; then
  echo "=== Rosetta 2 is not installed: x86_64 code cannot run here; compiling the tests only"
  cargo test --release -p scrutate --locked --target "$t" --no-run
  echo "=== done (build only)"
  exit 0
fi
"$CARGO_TARGET_DIR/$t/release/scrutate" --version
echo "=== cargo test (x86_64, under Rosetta 2)"
cargo test --release -p scrutate --locked --target "$t" 2>&1 | grep -E '^test result|FAILED|panicked'
echo "=== wheels"
uv python install cpython-3.10-macos-x86_64-none cpython-3.12-macos-x86_64-none cpython-3.13-macos-x86_64-none > /dev/null
rm -rf "$CARGO_TARGET_DIR/wheels"
maturin build --release --strip -m crates/scrutate-py/Cargo.toml --target "$t" -o "$CARGO_TARGET_DIR/wheels" \
  -i "$(uv python find cpython-3.12-macos-x86_64-none)"
ls "$CARGO_TARGET_DIR/wheels"
uv run --no-project --python 3.13 python tools/wheel_matrix.py --wheels "$CARGO_TARGET_DIR/wheels" --cli "$PWD/$CARGO_TARGET_DIR/$t/release/scrutate" \
  --json "$out/wheels.json" cpython-3.10-macos-x86_64-none cpython-3.12-macos-x86_64-none cpython-3.13-macos-x86_64-none | tee "$out/wheels.md"
echo "=== done"
