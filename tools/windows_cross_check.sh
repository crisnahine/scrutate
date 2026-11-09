#!/usr/bin/env bash
# Cross-compile the Windows x86_64 binary (MinGW-w64 toolchain) and its tests: proves the
# Windows-only code (console Ctrl-C handler, path handling) compiles and links. It does not run
# anything on Windows, so it does not make Windows a supported platform.
#   brew install mingw-w64 && tools/windows_cross_check.sh
set -euo pipefail
cd "$(dirname "$0")/.."
out=bench/results/platforms/windows-x86_64-cross
mkdir -p "$out"
exec > >(tee "$out/build.log") 2>&1
t=x86_64-pc-windows-gnu
export CARGO_TARGET_DIR=target/win
rustup target add "$t"
cargo build --release -p scrutate --locked --target "$t"
cargo test --release -p scrutate --locked --target "$t" --no-run
file "$CARGO_TARGET_DIR/$t/release/scrutate.exe"
echo "=== cross build done"
