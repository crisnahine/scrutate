#!/bin/sh
# Builds the .deb and .rpm inside rust:1-slim-bookworm. Run from the repository root:
#   docker run --rm -v "$PWD":/src -w /src rust:1-slim-bookworm sh packaging/linux/build-packages.sh
set -eux
apt-get update -qq
apt-get install -y -qq --no-install-recommends dpkg-dev > /dev/null
cargo install --locked -j 3 cargo-deb cargo-generate-rpm
# a separate target directory keeps the Linux build apart from the host's
export CARGO_TARGET_DIR=/src/target/linux
cargo build --release --locked -j 3 -p scrutate
cargo deb -p scrutate --no-build
cargo generate-rpm -p crates/scrutate
mkdir -p dist
cp "$CARGO_TARGET_DIR"/debian/*.deb "$CARGO_TARGET_DIR"/generate-rpm/*.rpm dist/
ls -l dist
