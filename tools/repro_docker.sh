#!/usr/bin/env bash
# Reproducible build across two different build environments: slim Debian (the Rust image) and full
# Debian (a non-Rust image with rustup installed at the same paths), each a fresh container with the
# same source path. Both build with the toolchain pinned in rust-toolchain.toml. The release
# binaries must have the same SHA-256.
#   tools/repro_docker.sh
set -euo pipefail
cd "$(dirname "$0")/.."
out=bench/results/platforms/repro
mkdir -p "$out"
rm -f "$out"/*.txt
build='
  set -e
  export CARGO_HOME=/usr/local/cargo RUSTUP_HOME=/usr/local/rustup PATH=/usr/local/cargo/bin:$PATH
  if ! command -v rustup > /dev/null; then
    curl -sSf https://sh.rustup.rs | sh -s -- -y -q --profile minimal --default-toolchain none > /dev/null
  fi
  mkdir -p /build && tar -C /src --exclude=./target --exclude=./dist --exclude=.scrutate --exclude=./fuzz -cf - . | tar -C /build -xf -
  cd /build && cargo build --release -p scrutate --locked -q
  sha256sum target/release/scrutate | cut -d" " -f1
  rustc --version
  . /etc/os-release && echo "$PRETTY_NAME"'
for img in rust:1-slim-bookworm golang:1.26-bookworm; do
  tag=${img//[:.]/_}
  docker run --rm --platform linux/arm64 -v "$PWD":/src:ro "$img" bash -c "$build" > "$out/$tag.txt"
  echo "$img: $(tr '\n' ' ' < "$out/$tag.txt")"
done
a=$(head -1 "$out/rust_1-slim-bookworm.txt"); b=$(head -1 "$out/golang_1_26-bookworm.txt")
if [ "$a" = "$b" ]; then echo "REPRODUCIBLE across images: $a"; else echo "NOT REPRODUCIBLE: $a vs $b"; exit 1; fi
