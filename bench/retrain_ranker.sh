#!/usr/bin/env bash
# Retrain the learned ranker on the code of this checkout and check it against the current one.
#
# usage: SCRUTATE_WORK=DIR SCRUTATE_VENV=DIR bench/retrain_ranker.sh
#
# Expects
#   SCRUTATE_WORK  scratch directory for the benchmark, the clones and the features (several GB)
#   SCRUTATE_VENV  Python 3.9+ virtual environment with numpy, lightgbm, scikit-learn, the reference engine's
#                  dependencies (tree-sitter and its grammars, snowballstemmer), the TEST packages
#                  networkx 3.4.2, mcp 1.30.0, rich 15.0.0 and the DEV packages pydantic,
#                  python-pptx, python-docx, werkzeug, seaborn, openpyxl, joblib
#   git, and network access the first time (make_bench_code.py fetch clones about 1.2 GB)
# Optional
#   THREADS=4      LightGBM threads
#   TAG=name       feature tag (default r<short hash of HEAD>); features of an existing tag are reused
#   REBUILD=1      build the benchmark again even when its query files are there
#   CV=1           also run the package-held-out cross-validation on DEV (hyper-parameters)
#   OLD_RANKER=f   the ranker to compare against (default: models/ranker.npz as it is now)
#   RANKER_PARAMS  passed to train_ranker.py
#   TYPE_WEIGHT=w  weight of a type query's rows in training (default 1: every query counts the same)
#
# Stages: benchmark, DEV and TEST features with the reference engine of this checkout, fit on DEV,
# evaluation of the old and new ranker on TEST (function rows and type rows, all languages and
# each one). The new ranker goes to models/ranker.npz only when on TEST no row of 50 queries or
# more is below BM25 and the mean over all queries of a variant is not below the old ranker's,
# each beyond its confidence interval; otherwise the script exits 1 and leaves it in the run
# directory.
# Rebuild the Rust binary afterwards, it embeds models/ranker.npz.
set -euo pipefail

: "${SCRUTATE_WORK:?set SCRUTATE_WORK to the scratch directory}"
: "${SCRUTATE_VENV:?set SCRUTATE_VENV to the virtual environment}"
export SCRUTATE_WORK THREADS="${THREADS:-4}"
PY="$SCRUTATE_VENV/bin/python"
cd "$(dirname "$0")/.."
TAG="${TAG:-r$(git rev-parse --short HEAD)}"
RUN="$SCRUTATE_WORK/retrain/$TAG"
mkdir -p "$RUN"
OLD_RANKER="${OLD_RANKER:-$RUN/ranker_old.npz}"
[ -f "$OLD_RANKER" ] || cp models/ranker.npz "$OLD_RANKER"
log() { echo "[$(date +%H:%M:%S)] $*"; }
stage() { # name, command...: times a stage and keeps its output in $RUN/<name>.log
  local name=$1 t0=$SECONDS
  shift
  log "$name ..."
  if ! "$@" > "$RUN/$name.log" 2>&1; then
    tail -20 "$RUN/$name.log"
    log "$name FAILED, see $RUN/$name.log"
    exit 2
  fi
  log "$name done in $((SECONDS - t0)) s"
}

"$PY" - <<'EOF'
import importlib.metadata as m, sys
need = {"networkx": "3.4.2", "mcp": "1.30.0", "rich": "15.0.0"}
for p in ["lightgbm", "scikit-learn", "numpy", "pydantic", "python-pptx", "python-docx", "werkzeug", "seaborn", "openpyxl", "joblib"]:
    need.setdefault(p, None)
bad = []
for p, v in need.items():
    try:
        got = m.version(p)
    except m.PackageNotFoundError:
        bad.append(f"{p} missing")
        continue
    if v and got != v:
        bad.append(f"{p} {got}, the TEST set is made from {v}")
    print(f"  {p} {got}")
if bad:
    sys.exit("venv: " + "; ".join(bad))
EOF

group() { "$PY" -c "import sys; sys.path.insert(0, 'bench'); import groups; print(' '.join(groups.$1))"; }
TEST_PY=$(group TEST) DEV_PY=$(group DEV) CODE_TEST=$(group CODE_TEST) CODE_DEV=$(group CODE_DEV)

# 1. benchmark: one builder run per group, so each starts its random stream where the published sets did
have_bench() {
  local p
  for p in $TEST_PY $DEV_PY $CODE_TEST $CODE_DEV; do
    grep -q '"target_kind"' "$SCRUTATE_WORK/bench/queries/$p.json" 2>/dev/null || return 1  # a file made before types were answers has none
  done
}
build_bench() {
  "$PY" bench/make_bench_code.py fetch
  "$PY" bench/make_bench.py $TEST_PY
  "$PY" bench/make_bench_code.py $CODE_TEST
  "$PY" bench/make_bench.py $DEV_PY
  "$PY" bench/make_bench_code.py $CODE_DEV
}
if [ -n "${REBUILD:-}" ] || ! have_bench; then
  stage bench build_bench
else
  log "bench: reusing $SCRUTATE_WORK/bench"
fi

# 2. features with the reference engine of this checkout (single-threaded; DEV and TEST side by side)
csv() { echo "$*" | tr ' ' ,; }
have_feat() { # tag, packages...
  local t=$1 p v
  shift
  for p in "$@"; do for v in A B; do [ -f "$SCRUTATE_WORK/feat/$t/${v}_$p.pkl" ] || return 1; done; done
}
features() { # tag, group, packages...
  local t=$1 g=$2
  shift 2
  "$PY" bench/gen_features.py models/semantic.npz "$t" "$g" --pkgs "$(csv "$@")"
}
if have_feat "${TAG}dev" $DEV_PY $CODE_DEV && have_feat "${TAG}test" $TEST_PY $CODE_TEST; then
  log "features: reusing ${TAG}dev and ${TAG}test"
else
  # an index made by another engine version must not be reused
  find "$SCRUTATE_WORK/bench/src" -mindepth 3 -maxdepth 3 -name .scrutate -type d -exec rm -rf {} +
  t0=$SECONDS
  log "features ${TAG}dev and ${TAG}test ..."
  features "${TAG}dev" DEV $DEV_PY $CODE_DEV > "$RUN/features_dev.log" 2>&1 & dev=$!
  features "${TAG}test" TEST $TEST_PY $CODE_TEST > "$RUN/features_test.log" 2>&1 & test=$!
  ok=1
  wait $dev || { tail -20 "$RUN/features_dev.log"; ok=0; }
  wait $test || { tail -20 "$RUN/features_test.log"; ok=0; }
  [ $ok = 1 ] || { log "features FAILED"; exit 2; }
  log "features done in $((SECONDS - t0)) s ($(du -sh "$SCRUTATE_WORK/feat/${TAG}dev" "$SCRUTATE_WORK/feat/${TAG}test" | cut -f1 | tr '\n' ' '))"
fi

# 3. hyper-parameters (optional), fit
[ -z "${CV:-}" ] || stage cv "$PY" bench/train_ranker.py cv "${TAG}dev"
stage fit "$PY" bench/train_ranker.py fit "${TAG}dev" "$RUN/ranker_new.npz"
grep -E "export check|saved" "$RUN/fit.log"

# 4. evaluation on TEST, then the gate
stage eval "$PY" bench/train_ranker.py eval "${TAG}test" "$RUN/ranker_new.npz"
stage eval_python env PYTHON_ONLY=1 "$PY" bench/train_ranker.py eval "${TAG}test" "$RUN/ranker_new.npz"
set +e
"$PY" bench/train_ranker.py compare "${TAG}test" "$OLD_RANKER" "$RUN/ranker_new.npz" "$RUN/compare.json" --gate \
  > "$RUN/compare.log" 2>&1
gate=$?
set -e
cat "$RUN/compare.log"
grep -q "^function rows worse" "$RUN/compare.log" || { log "compare FAILED"; exit 2; }
if [ $gate -ne 0 ]; then
  log "the new ranker is below BM25 on a row or below the old ranker over all queries; models/ranker.npz is unchanged, the candidate is $RUN/ranker_new.npz"
  exit 1
fi
cp "$RUN/ranker_new.npz" models/ranker.npz
log "models/ranker.npz written; logs in $RUN. Rebuild the binary (cargo build --release -p scrutate) to embed it."
