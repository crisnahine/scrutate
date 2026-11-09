#!/usr/bin/env bash
# Network file system check: a copy of a repository on a mounted network share (SMB, NFS, ...)
# must give the same Engine A output as the local copy, and Engine B must index it, answer from
# it, survive concurrent updates and pass `check --files`.
#   tools/netfs_check.sh MOUNTED_DIR LOCAL_REPO
set -euo pipefail
S=${SCRUTATE_BIN:-$(cd "$(dirname "$0")/.." && pwd)/target/release/scrutate}
mnt=$1; src=$2
work="$mnt/netfs-check"
rm -rf "$work" && mkdir -p "$work"
(cd "$src" && tar --exclude=.scrutate --exclude=.git -cf - .) | (cd "$work" && tar -xf -)
cd "$work"
echo "file system: $(mount | grep -F " on $(cd "$mnt" && pwd) " | sed 's/.* (//; s/,.*//')"
for q in "-F Searcher" "-i -F error" "[A-Z][a-z]+Builder"; do
  # shellcheck disable=SC2086
  a=$("$S" grep --sort $q | shasum | cut -c1-16); b=$(cd "$src" && "$S" grep --sort $q | shasum | cut -c1-16)
  echo "engine A [$q]: network $a local $b $([ "$a" = "$b" ] && echo same || { echo DIFFERENT; exit 1; })"
done
"$S" index > /dev/null
echo "engine B index: $("$S" stats | head -c 160)"
for round in 1 2 3; do
  f=$(find . -name '*.rs' | sort | head -1)
  echo "// netfs edit $round" >> "$f"
  pids=""
  for k in 1 2 3; do "$S" index > "/tmp/netfs-$k.log" 2>&1 & pids="$pids $!"; done
  for k in 1 2 3; do "$S" search "builder" -k 2 > /dev/null; "$S" grep --index -F Searcher > /dev/null; done
  for p in $pids; do wait "$p" || { echo "concurrent index failed:"; cat /tmp/netfs-*.log; exit 1; }; done
done
"$S" index > /dev/null
"$S" check --files | tail -1
a=$("$S" grep --index --sort -F 'netfs edit 3' | wc -l | tr -d ' '); b=$("$S" grep --sort -F 'netfs edit 3' | wc -l | tr -d ' ')
echo "indexed grep after concurrent updates: $a line(s), walk: $b line(s) $([ "$a" = "$b" ] && echo same || { echo DIFFERENT; exit 1; })"
rm -f /tmp/netfs-*.log
cd / && { rm -rf "$work" 2>/dev/null || echo "(cleanup: the share still holds a lock file; removed on the next run)"; }
echo "NETWORK FS OK"
