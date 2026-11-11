#!/usr/bin/env bash
# Run bench/parity.py over real repositories of the six reference languages.
# usage: bench/parity_repos.sh RUST_BINARY
# env: PARITY_CODE (clones dir), PARITY_PYTHON (python with the reference deps),
#      PARITY_NETWORKX (networkx package dir), PARITY_SITE (site-packages with rich and mcp),
#      PARITY_QUERIES (default 40)
set -u
[ $# -eq 1 ] || { echo "usage: $0 RUST_BINARY" >&2; exit 2; }
bin=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
here=$(cd "$(dirname "$0")" && pwd)
code=${PARITY_CODE:-$HOME/.cache/scrutate-eval/work/code}
py=${PARITY_PYTHON:-/private/tmp/claude-502/-Users-crisn-Documents-Projects-scrutate/032db348-b12c-4238-ade7-7c9c75f5a050/scratchpad/venv/bin/python}
nx=${PARITY_NETWORKX:-/private/tmp/claude-502/-Users-crisn-Documents-Projects-scrutate/032db348-b12c-4238-ade7-7c9c75f5a050/scratchpad/pmid}
site=${PARITY_SITE:-$HOME/.cache/scrutate-eval/venv/lib/python3.13/site-packages}
nq=${PARITY_QUERIES:-40}

repos="$code/js_ws/lib $code/js_lodash $code/js_axios/lib $code/js_commander/lib
$code/ts_rxjs/src $code/ts_pixijs/src $code/ts_immutable/src
$code/go_prometheus $code/go_cobra $code/go_zap
$code/java_jsoup/src/main/java $code/java_commons_io/src/main/java $code/java_gson/gson/src/main/java
$code/rs_ripgrep/crates $code/rs_serde_json/src $code/rs_chrono/src
networkx=$nx rich=$site/rich mcp=$site/mcp"

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
