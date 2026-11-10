#!/usr/bin/env bash
# The macOS runs behind docs/LEADERBOARD.md and docs/BENCHMARKS.md, in order: the Engine A
# leaderboard with a warm and a cold cache, the tool comparison, the indexed comparison. About
# 4 hours on an M3 Pro; leave the machine idle. Build the binary first and do not run
# `cargo test` in between (it replaces target/release/scrutate). Then wait an hour: macOS starts a
# just-built executable about 0.2 ms slower than the same file later (identical copies, the same
# instructions executed), which is 5% of a small-tree search, and every competitor's binary is old.
#   cargo build --release -p scrutate --locked && sleep 3600 && bash bench/run_macos.sh
set -uo pipefail
cd "$(dirname "$0")/.."
corpora=${CORPORA:-$HOME/.cache/scrutate-bench/corpora}
out=bench/results/macos-$(uname -m)
mkdir -p "$out"
[ -x target/evict ] || cc -O2 -o target/evict bench/evict.c
log() { echo "=== $* $(date '+%Y-%m-%d %H:%M:%S')" | tee -a "$out/chain.log"; }
bin=$(shasum -a 256 target/release/scrutate | cut -c1-16)
: > "$out/chain.log"
log "start $bin, built $(( ($(date +%s) - $(stat -f %m target/release/scrutate)) / 60 )) minutes ago"
python3 bench/leaderboard.py --corpora "$corpora" --out "$out/leaderboard-warm.json" --max-load 40 --contender 1.15 --force \
  > "$out/leaderboard-warm.log" 2>&1
log "warm exit $?"
python3 bench/leaderboard.py --corpora "$corpora" --out "$out/leaderboard-cold.json" --cache cold --cold-prepare "$PWD/target/evict {list}" \
  --only linux/literal linux/case-insensitive llvm/literal ripgrep/literal smallfiles/literal --runs 30 --max-load 40 --contender 1.15 --force \
  > "$out/leaderboard-cold.log" 2>&1
log "cold exit $?"
python3 bench/compare_tools.py --corpus "small=$corpora/py-small" --corpus "big=$corpora/py-big" --corpus "massive=$corpora/py-massive" \
  --runs 6 --out "$out/compare.json" > "$out/compare.log" 2>&1
log "compare_tools exit $?"
python3 bench/indexed.py --corpus "$corpora/linux-nodot" --out "$out/indexed.json" --tools scrutate,zoekt --max-load 40 > "$out/indexed.log" 2>&1
log "indexed exit $?"
log "end $bin"
