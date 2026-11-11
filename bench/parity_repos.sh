#!/usr/bin/env bash
# Run bench/parity.py over real repositories of the six reference languages.
# usage: bench/parity_repos.sh RUST_BINARY
# env: PARITY_CODE (the clones bench/make_bench_code.py fetch makes; default $SCRUTATE_WORK/code),
#      PARITY_PYTHON (a python with the reference's dependencies; default python3),
#      PARITY_NETWORKX (networkx package dir) and PARITY_SITE (site-packages with rich and mcp):
#      default, where that python has them installed; PARITY_QUERIES (default 40)
set -u
[ $# -eq 1 ] || { echo "usage: $0 RUST_BINARY" >&2; exit 2; }
bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
here=$(cd "$(dirname "$0")" && pwd)
code=${PARITY_CODE:-${SCRUTATE_WORK:-$here/../work}/code}
py=${PARITY_PYTHON:-python3}
# networkx, rich and mcp as the python above has them installed, unless a directory is given
pkg() { "$py" -c "import os, $1; print(os.path.dirname($1.__file__))" 2>/dev/null; }
nx=${PARITY_NETWORKX:-$(pkg networkx)}
site=${PARITY_SITE:-}
rich=${site:+$site/rich}; rich=${rich:-$(pkg rich)}
mcp=${site:+$site/mcp}; mcp=${mcp:-$(pkg mcp)}
nq=${PARITY_QUERIES:-40}

repos="$code/js_ws/lib $code/js_lodash $code/js_axios/lib $code/js_commander/lib
$code/ts_rxjs/src $code/ts_pixijs/src $code/ts_immutable/src
$code/go_prometheus $code/go_cobra $code/go_zap
$code/java_jsoup/src/main/java $code/java_commons_io/src/main/java $code/java_gson/gson/src/main/java
$code/rs_ripgrep/crates $code/rs_serde_json/src $code/rs_chrono/src
networkx=${nx:-missing} rich=${rich:-missing} mcp=${mcp:-missing}"

failed=0
log=$(mktemp)
for r in $repos; do
  case $r in
    *=*) name=${r%%=*}; r=${r#*=} ;;
    *) name=${r#"$code/"} ;;
  esac
  if [ ! -d "$r" ]; then
    echo "$name: missing, skipped"
    continue
  fi
  q=$nq
  "$py" "$here/parity.py" "$r" "$bin" --queries "$q" >"$log" 2>&1
  rc=$?
  # A repository that cannot make $nq queries says how many it can; rerun with that many.
  made=$(sed -n 's/^PARITY FAILED: only \([0-9]*\) queries.*/\1/p' "$log")
  if [ $rc -ne 0 ] && [ -n "$made" ] && [ "$made" -gt 0 ]; then
    q=$made
    "$py" "$here/parity.py" "$r" "$bin" --queries "$q" >"$log" 2>&1
    rc=$?
  fi
  if [ $rc -eq 0 ] && grep -q '^PARITY OK' "$log"; then
    echo "$name: ok ($q queries)"
  else
    failed=1
    n=$(sed -n 's/^PARITY FAILED: \([0-9]*\) problem.*/\1/p' "$log")
    first=$(grep -m1 '^   - ' "$log" | cut -c6-400)
    [ -n "$first" ] || first=$(grep -v '^ *$' "$log" | tail -1 | cut -c1-400)
    echo "$name: ${n:-?} problem(s): $first"
  fi
done
rm -f "$log"
exit $failed
