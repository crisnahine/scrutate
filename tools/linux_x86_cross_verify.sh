#!/usr/bin/env bash
# Linux x86_64 from an arm64 machine whose x86 emulation cannot run the C compiler (gcc crashes
# with SIGSEGV under Rosetta on the large generated grammar sources): build the binary and every
# test binary for x86_64 natively with a cross toolchain, then run them in an x86_64 container
# with the same paths.
#
#   tools/linux_x86_cross_verify.sh WORK_DIR
#
# The log goes to bench/results/platforms/linux-x86_64/cross-verify.log.
set -euo pipefail
cd "$(dirname "$0")/.."
work=${1:?work directory}
out=bench/results/platforms/linux-x86_64
mkdir -p "$work" "$out"
build='
  set -e
  apt-get update -qq && apt-get install -y -qq gcc-x86-64-linux-gnu g++-x86-64-linux-gnu libc6-dev-amd64-cross file > /dev/null
  rm -rf /work/src && mkdir -p /work/src
  tar -C /src --exclude=./.claude --exclude=./target --exclude=./dist --exclude=.scrutate --exclude=./fuzz -cf - . | tar -C /work/src -xf -
  cd /work/src
  rustup target add x86_64-unknown-linux-gnu > /dev/null 2>&1
  export CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc CXX_x86_64_unknown_linux_gnu=x86_64-linux-gnu-g++ \
    CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc
  cargo build --release -p scrutate --locked --target x86_64-unknown-linux-gnu -q
  cargo test --release -p scrutate --locked --target x86_64-unknown-linux-gnu --no-run 2>&1 | grep -o "(target/[^)]*)" | tr -d "()" > /work/tests.txt
  file target/x86_64-unknown-linux-gnu/release/scrutate
  echo "test binaries: $(wc -l < /work/tests.txt)"'
run='
  set -e
  cd /work/src
  uname -m
  target/x86_64-unknown-linux-gnu/release/scrutate --version
  fail=0
  while read -r t; do
    echo "--- $t"
    "$t" --quiet 2>&1 | grep -E "^test result|FAILED|panicked" || fail=1
  done < /work/tests.txt
  exit $fail'
{
  echo "=== cross build on linux/arm64 ($(date -u +%Y-%m-%dT%H:%M:%SZ))"
  docker run --rm --platform linux/arm64 -v "$PWD":/src:ro -v "$work":/work rust:1-slim-bookworm bash -c "$build"
  echo "=== tests on linux/amd64"
  docker run --rm --platform linux/amd64 -v "$work":/work rust:1-slim-bookworm bash -c "$run"
  echo "=== done"
} 2>&1 | tee "$out/cross-verify.log"
