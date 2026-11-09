#!/usr/bin/env bash
# Linux benchmark environment: every competitor at its latest stable release, installed the way
# its maintainers publish it (release binaries where they exist, source builds otherwise), then the
# Engine A leaderboard (warm and cold cache) and the indexed comparison with Zoekt and livegrep.
#
#   docker run --rm --privileged --platform linux/arm64 -v "$PWD":/src:ro \
#     -v "$HOME/.cache/scrutate-bench/corpora":/corpora:ro -v "$HOME/.cache/scrutate-bench/livegrep":/livegrep:ro \
#     -v scrutate-bench-linux:/bench -v "$PWD/bench/results":/results rust:1-slim-bookworm bash /src/bench/competitors/linux_bench.sh
#
# INDEXED_TOOLS (default scrutate,zoekt,livegrep) picks the indexed engines when disk space is short.
#
# The corpora are copied into the container's own disk (/bench) so reads do not go through the
# host file-sharing layer; the cold-cache runs drop the guest page cache before every run.
set -euo pipefail
B=/bench
mkdir -p $B/bin $B/src
export PATH=$B/bin:$B/go/bin:$PATH
arch=$(uname -m)
step() { echo; echo "=== $* ($(date -u +%H:%M:%S))"; }

step "system packages"
apt-get update -qq
apt-get install -y -qq make g++ pkg-config file perl python3 python3-venv silversearcher-ag libpcre2-dev zlib1g-dev libbz2-dev liblzma-dev libzstd-dev liblz4-dev \
  libbrotli-dev unzip xz-utils curl git autoconf automake > /dev/null

fetch() { [ -f "$B/src/$2" ] || curl -fsSL -o "$B/src/$2" "$1"; }
if [ ! -x $B/bin/rg ]; then
  step "ripgrep 15.2.0 (release binary)"
  fetch https://github.com/BurntSushi/ripgrep/releases/download/15.2.0/ripgrep-15.2.0-$arch-unknown-linux-gnu.tar.gz rg.tgz
  tar -C $B/src -xzf $B/src/rg.tgz && cp $B/src/ripgrep-15.2.0-$arch-unknown-linux-gnu/rg $B/bin/
fi
if [ ! -x $B/bin/hyperfine ]; then
  step "hyperfine 2.0.0 (release binary)"
  fetch https://github.com/sharkdp/hyperfine/releases/download/v2.0.0/hyperfine-v2.0.0-$arch-unknown-linux-gnu.tar.gz hf.tgz
  tar -C $B/src -xzf $B/src/hf.tgz && cp $B/src/hyperfine-v2.0.0-$arch-unknown-linux-gnu/hyperfine $B/bin/
fi
if [ ! -x $B/bin/ast-grep ]; then
  step "ast-grep 0.51.1 (release binary)"
  fetch https://github.com/ast-grep/ast-grep/releases/download/0.51.1/app-$arch-unknown-linux-gnu.zip sg.zip
  unzip -o -q $B/src/sg.zip -d $B/src/sg && cp $B/src/sg/ast-grep $B/bin/
fi
if [ ! -x $B/bin/ugrep ]; then
  step "ugrep 7.8.5 (source, ./build.sh as its README says)"
  fetch https://github.com/Genivia/ugrep/archive/refs/tags/v7.8.5.tar.gz ugrep.tgz
  tar -C $B/src -xzf $B/src/ugrep.tgz && (cd $B/src/ugrep-7.8.5 && ./build.sh > /dev/null 2>&1 && cp bin/ugrep $B/bin/)
fi
if [ ! -x $B/bin/grep ]; then
  step "GNU grep 3.12 (source)"
  fetch https://ftp.gnu.org/gnu/grep/grep-3.12.tar.xz grep.txz
  tar -C $B/src -xJf $B/src/grep.txz && (cd $B/src/grep-3.12 && ./configure -q --prefix=$B > /dev/null && make -s -j"$(nproc)" > /dev/null && cp src/grep $B/bin/)
fi
if [ ! -x $B/bin/ack ]; then
  step "ack 3.10.0 (standalone file from beyondgrep.com)"
  fetch https://beyondgrep.com/ack-v3.10.0 ack && cp $B/src/ack $B/bin/ack && chmod +x $B/bin/ack
fi
if [ ! -x $B/go/bin/zoekt ]; then
  step "Go and Zoekt (go install, as its README says)"
  fetch "https://go.dev/dl/go1.25.4.linux-${arch/aarch64/arm64}.tar.gz" go.tgz
  tar -C $B -xzf $B/src/go.tgz
  GOPATH=$B/go GOFLAGS=-mod=mod $B/go/bin/go install github.com/sourcegraph/zoekt/cmd/zoekt-index@a5a933bc043e github.com/sourcegraph/zoekt/cmd/zoekt@a5a933bc043e
  mv $B/go/bin/zoekt $B/go/bin/zoekt-index $B/bin/ 2>/dev/null || true
fi
ln -sf $B/bin/zoekt $B/bin/zoekt-index $B/go/bin/ 2>/dev/null || true

step "scrutate (release build)"
rm -rf $B/scrutate && mkdir -p $B/scrutate
tar -C /src --exclude=./target --exclude=./dist --exclude=.scrutate --exclude=./fuzz -cf - . | tar -C $B/scrutate -xf -
(cd $B/scrutate && cargo build --release -p scrutate --locked -q)
export SCRUTATE_BIN=$B/scrutate/target/release/scrutate ZOEKT_BIN_DIR=$B/bin

step "corpora onto the container disk"
mkdir -p $B/corpora
for c in linux linux-nodot ripgrep; do
  [ -d $B/corpora/$c ] || cp -a /corpora/$c $B/corpora/
done
cp /corpora/manifest.json $B/corpora/
git config --global --add safe.directory '*'

out=/results/linux-$arch
mkdir -p $out
cd $B/scrutate
step "Engine A leaderboard, warm cache (linux corpus)"
python3 bench/leaderboard.py --corpora $B/corpora --out $out/leaderboard-warm.json --only linux/ --max-load "$(nproc)"
step "Engine A leaderboard, cold cache (linux corpus subset)"
python3 bench/leaderboard.py --corpora $B/corpora --out $out/leaderboard-cold.json --cache cold \
  --cold-prepare 'sync; echo 3 > /proc/sys/vm/drop_caches' --only 'linux/literal' 'linux/case-insensitive' 'linux/complex regex' 'linux/no match' --runs 30 --max-load "$(nproc)"
step "indexed: scrutate, Zoekt, livegrep"
python3 bench/indexed.py --corpus $B/corpora/linux-nodot --work $B/indexed-work --out $out/indexed.json --tools "${INDEXED_TOOLS:-scrutate,zoekt,livegrep}" \
  --livegrep-bin /livegrep --max-load "$(nproc)"
rm -rf $B/indexed-work
step "done"
