#!/usr/bin/env bash
# Evaluate the ranker in models/ranker.npz once on the sealed hold-out (groups.SEALED and CODE_SEALED:
# packages and repositories never used to pretrain, tune, fit or test).
#
# usage: SCRUTATE_WORK=DIR SCRUTATE_VENV=DIR bench/eval_sealed.sh
#
# Expects what bench/retrain_ranker.sh expects, and in the venv the SEALED packages click 8.5.1,
# httpx 0.28.1 and attrs 26.1.0.
# Optional
#   TAG=name        run directory and feature tag prefix (default r<short hash of HEAD>)
#   THREADS=4       LightGBM threads
#   FORCE_SEALED=1  evaluate although the sealed set was spent on another ranker (recorded as forced)
#
# Writes into $SCRUTATE_WORK/retrain/$TAG: sealed_bench.log (if the benchmark had to be built),
# sealed_features.log, sealed_eval.log (all languages and each one), sealed_eval_python.log (Python
# packages alone), sealed_ranker.npz (the ranker evaluated) and sealed_evaluated.json, the record
# of every evaluation (ranker SHA-256, commit, date). The record is written before the evaluation
# starts. Any record under $SCRUTATE_WORK/retrain that names another ranker makes this script
# refuse. It never changes models/ranker.npz and is no part of the retrain gate.
set -euo pipefail

: "${SCRUTATE_WORK:?set SCRUTATE_WORK to the scratch directory}"
: "${SCRUTATE_VENV:?set SCRUTATE_VENV to the virtual environment}"
export SCRUTATE_WORK THREADS="${THREADS:-4}"
PY="$SCRUTATE_VENV/bin/python"
cd "$(dirname "$0")/.."
TAG="${TAG:-r$(git rev-parse --short HEAD)}"
RUN="$SCRUTATE_WORK/retrain/$TAG"
mkdir -p "$RUN"
log() { echo "[$(date +%H:%M:%S)] $*"; }
stage() { # name, command...: keeps a stage's output in $RUN/<name>.log
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
need = {"click": "8.5.1", "httpx": "0.28.1", "attrs": "26.1.0", "lightgbm": None, "numpy": None}
bad = []
for p, v in need.items():
    try:
        got = m.version(p)
    except m.PackageNotFoundError:
        bad.append(f"{p} missing")
        continue
    if v and got != v:
        bad.append(f"{p} {got}, the SEALED set is made from {v}")
if bad:
    sys.exit("venv: " + "; ".join(bad))
EOF

# a copy, so the file evaluated is the file hashed
cp models/ranker.npz "$RUN/sealed_ranker.npz"
SHA=$(shasum -a 256 "$RUN/sealed_ranker.npz" | cut -d' ' -f1)
COMMIT=$(git rev-parse HEAD)
DIRTY=$([ -z "$(git status --porcelain)" ] && echo false || echo true)

# once per ranker: refuse when any run directory records another ranker
SHA="$SHA" "$PY" - "$SCRUTATE_WORK/retrain" <<'EOF'
import glob, json, os, sys
spent = set()
for f in glob.glob(os.path.join(sys.argv[1], "*", "sealed_evaluated.json")):
    with open(f) as fh:
        spent |= {(e["ranker_sha256"], f) for e in json.load(fh)["evaluations"]}
others = sorted({(h, f) for h, f in spent if h != os.environ["SHA"]})
if others and not os.environ.get("FORCE_SEALED"):
    sys.exit("the sealed set has been spent on another ranker (" + "; ".join(f"{h[:12]} in {f}" for h, f in others)
             + "); a second look is a decision to record: run again with FORCE_SEALED=1")
EOF

SEALED_PY=$("$PY" -c "import sys; sys.path.insert(0, 'bench'); import groups; print(' '.join(groups.SEALED))")
CODE_SEALED=$("$PY" -c "import sys; sys.path.insert(0, 'bench'); import groups; print(' '.join(groups.CODE_SEALED))")

# 1. benchmark: a builder run of its own, so the other groups' random streams are not touched
have_bench() {
  local p
  for p in $SEALED_PY $CODE_SEALED; do
    grep -q '"target_kind"' "$SCRUTATE_WORK/bench/queries/$p.json" 2>/dev/null || return 1
  done
}
build_bench() {
  "$PY" bench/make_bench_code.py fetch
  "$PY" bench/make_bench.py $SEALED_PY
  "$PY" bench/make_bench_code.py $CODE_SEALED
}
if have_bench; then
  log "bench: reusing the sealed queries in $SCRUTATE_WORK/bench"
else
  stage sealed_bench build_bench
fi

# 2. features with the reference engine of this checkout
have_feat() {
  local p v
  for p in $SEALED_PY $CODE_SEALED; do for v in A B; do
    [ -f "$SCRUTATE_WORK/feat/${TAG}sealed/${v}_$p.pkl" ] || return 1
  done; done
}
if have_feat; then
  log "features: reusing ${TAG}sealed"
else
  # an index made by another engine version must not be reused
  for p in $SEALED_PY $CODE_SEALED; do for v in A B; do
    rm -rf "$SCRUTATE_WORK/bench/src/$v/$p/.scrutate"
  done; done
  stage sealed_features "$PY" bench/gen_features.py models/semantic.npz "${TAG}sealed" SEALED \
    --pkgs "$(echo $SEALED_PY $CODE_SEALED | tr ' ' ,)"
fi

# 3. the record, then the evaluation
SHA="$SHA" COMMIT="$COMMIT" DIRTY="$DIRTY" TAG="$TAG" "$PY" - "$RUN/sealed_evaluated.json" <<'EOF'
import datetime, json, os, sys
path = sys.argv[1]
rec = {"evaluations": []}
if os.path.exists(path):
    with open(path) as f:
        rec = json.load(f)
rec["evaluations"].append({
    "ranker_sha256": os.environ["SHA"], "commit": os.environ["COMMIT"],
    "uncommitted_changes": os.environ["DIRTY"] == "true", "tag": os.environ["TAG"],
    "date": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"),
    "forced": bool(os.environ.get("FORCE_SEALED"))})
with open(path, "w") as f:
    json.dump(rec, f, indent=1)
EOF
stage sealed_eval "$PY" bench/train_ranker.py sealed "${TAG}sealed" "$RUN/sealed_ranker.npz"
stage sealed_eval_python env PYTHON_ONLY=1 "$PY" bench/train_ranker.py sealed "${TAG}sealed" "$RUN/sealed_ranker.npz"
grep -h "MRR@10 over all" "$RUN/sealed_eval.log"
log "sealed evaluation of ranker ${SHA:0:12} recorded in $RUN/sealed_evaluated.json; logs in $RUN"
