#!/usr/bin/env bash
# The gate job of .github/workflows/benchmarks.yml in a clean container. The run steps are read
# from the workflow file itself, so this cannot drift from it; the commit to measure against is
# BASE (default: the parent of HEAD). Only what is committed is cloned.
#
#   docker run --rm -e BASE=<commit> -v "$PWD":/src:ro python:3.13-slim-bookworm bash /src/tools/bench_rehearsal.sh
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive CARGO_TERM_COLOR=never
apt-get update -qq && apt-get install -y -qq curl build-essential git sudo > /dev/null
curl -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain none --profile minimal > /dev/null 2>&1
pip install -q --root-user-action=ignore uv pyyaml
export PATH=$HOME/.cargo/bin:$PATH RUNNER_TEMP=/tmp/runner GITHUB_WORKSPACE=/work
mkdir -p "$RUNNER_TEMP"
git config --global --add safe.directory '*'
git clone -q /src /work
cd /work
BASE=${BASE:-$(git rev-parse HEAD~1)}
export BASE
python3 - > "$RUNNER_TEMP/steps.sh" <<'EOF'
import yaml

for s in yaml.safe_load(open(".github/workflows/benchmarks.yml"))["jobs"]["gate"]["steps"]:
    if "run" in s:
        print(f"echo; echo '=== {s.get('name', s['run'].splitlines()[0][:60])} ==='")
        print("bash --noprofile --norc -eo pipefail <<'STEP'\n" + s["run"].rstrip() + "\nSTEP")
EOF
bash -e "$RUNNER_TEMP/steps.sh"
echo
echo "REHEARSAL PASSED"
