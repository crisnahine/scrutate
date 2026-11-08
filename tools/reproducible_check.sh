#!/usr/bin/env bash
# Build the release binary twice from clean and compare the hashes. Same toolchain + same source
# path + same flags must give the same bytes; a difference means a reproducibility regression.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo clean -p scrutate --release
cargo build --release -p scrutate --locked -q
h1=$(sha256sum target/release/scrutate | cut -d' ' -f1)
cargo clean -p scrutate --release
cargo build --release -p scrutate --locked -q
h2=$(sha256sum target/release/scrutate | cut -d' ' -f1)
echo "build 1: $h1"
echo "build 2: $h2"
if [ "$h1" = "$h2" ]; then echo "REPRODUCIBLE"; else echo "NOT REPRODUCIBLE"; exit 1; fi
