#!/usr/bin/env python3
"""Run every quality gate and record the actual output in docs/TEST_RESULTS.md.

Gates: cargo fmt --check, cargo clippy -D warnings (both crates), cargo test (core), cargo audit,
Python reference tests, bindings tests (if the wheel is installed), Rust/Python parity,
differential test against ripgrep, reproducible-build check (optional, slow), core test without
Python on PATH. Nothing is summarised by hand: the exit status and the tail of each command's
output are written as they were produced.

usage: gen_test_results.py [--repo DIR_FOR_PARITY] [--differential-cases N] [--reproducible]
"""
from __future__ import annotations

import argparse
import os
import platform
import re
import subprocess
import sys
import time

# The page head (banner and navigation); tools/gen_doc_assets.py --check compares it with its table.
DOC_HEAD = """<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="assets/banner-test-results.svg" alt="Test results. Did every quality gate pass, and what did it print?" width="880">
</p>

# Test and quality-gate results

[Documentation](README.md) &nbsp;·&nbsp; Previous: [Ranking quality](QUALITY.md) &nbsp;·&nbsp; Next: [Security](SECURITY.md)

<!-- End of head. -->
"""

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, ".."))


def run(cmd, cwd=ROOT, env=None, tail=25, timeout=3600):
    t0 = time.time()
    e = dict(os.environ)
    if env:
        e.update(env)
    try:
        r = subprocess.run(cmd, cwd=cwd, env=e, capture_output=True, text=True, timeout=timeout)
        out = (r.stdout + r.stderr).strip().splitlines()
        rc = r.returncode
    except subprocess.TimeoutExpired:
        out, rc = ["(timed out)"], 124
    dt = time.time() - t0
    shown = out[-tail:] if len(out) > tail else out
    return rc, dt, "\n".join(shown), "\n".join(out)


def ci_nightly() -> str:
    """The nightly toolchain the fuzz job of ci.yml pins."""
    with open(os.path.join(ROOT, ".github", "workflows", "ci.yml")) as f:
        return re.search(r"NIGHTLY: (\S+)", f.read()).group(1)


def fuzz_gates(gate, nightly: str) -> None:
    fuzz = ["cargo", f"+{nightly}", "fuzz"]
    if subprocess.run(fuzz + ["--version"], capture_output=True).returncode != 0:
        gate(f"fuzz regressions (cargo-fuzz on {nightly})", fuzz + ["--version"], tail=4)
        return
    for d in sorted(os.listdir(os.path.join(ROOT, "fuzz", "regressions"))):
        for f in sorted(os.listdir(os.path.join(ROOT, "fuzz", "regressions", d))):
            gate(f"fuzz regression {d}/{f[:18]}", fuzz + ["run", "-O", d, os.path.join("fuzz", "regressions", d, f)], tail=2)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--repo", default="/tmp/pmid", help="repository for the parity harness")
    ap.add_argument("--parity-code", help="the clones `bench/make_bench_code.py fetch` makes, for bench/parity_repos.sh (default: PARITY_CODE, else $SCRUTATE_WORK/code)")
    ap.add_argument("--differential-cases", type=int, default=200)
    ap.add_argument("--reproducible", action="store_true", help="also run tools/reproducible_check.sh (two clean release builds)")
    ap.add_argument("--wheels", help="directory of built wheels to install and test (tools/wheel_matrix.py)")
    ap.add_argument("--pythons", nargs="*", default=["3.10", "3.11", "3.12", "3.13", "3.14t", "3.15", "3.15t"])
    a = ap.parse_args()
    rustc = subprocess.run(["rustc", "--version"], capture_output=True, text=True).stdout.strip()
    cargo = subprocess.run(["cargo", "--version"], capture_output=True, text=True).stdout.strip()
    py = sys.version.split()[0]
    sections = []
    results = []

    def display(arg):
        # local interpreter and scratch paths say nothing to a reader
        if arg == sys.executable:
            return "python"
        if os.path.isabs(arg) and not arg.startswith(ROOT):
            return os.path.basename(arg.rstrip("/"))
        return os.path.relpath(arg, ROOT) if os.path.isabs(arg) else arg

    def gate(name, cmd, **kw):
        rc, dt, shown, full = run(cmd, **kw)
        ok = rc == 0
        results.append((name, ok, dt))
        sections.append(f"### {name}\n\n`{' '.join(display(c) for c in cmd)}`: exit {rc} ({'PASS' if ok else 'FAIL'}, {dt:.1f} s)\n\n```\n{shown}\n```\n")
        print(f"{'PASS' if ok else 'FAIL'} {name} ({dt:.1f}s)", file=sys.stderr)
        return full

    gate("cargo fmt --check", ["cargo", "fmt", "--all", "--check"])
    gate("cargo clippy (core, all targets, -D warnings)", ["cargo", "clippy", "--release", "-p", "scrutate", "--all-targets", "--", "-D", "warnings"], tail=6)
    gate("cargo clippy (python bindings, -D warnings)", ["cargo", "clippy", "--release", "-p", "scrutate-py", "--", "-D", "warnings"], tail=6)
    test_out = gate("cargo test (core crate)", ["cargo", "test", "--release", "-p", "scrutate"], tail=60)
    gate("cargo test --features extra-langs (optional grammars' samples)", ["cargo", "test", "--release", "-p", "scrutate", "--features", "extra-langs", "--test", "samples_extra"], tail=6)
    # core tests with Python removed from PATH (the hybrid-stack rule); where the OS ships its own
    # /usr/bin/python3 (macOS) the gate says so: the Linux container check runs with no Python at all
    stripped = os.path.expanduser("~/.cargo/bin") + ":/usr/bin:/bin"
    has_py = any(os.path.exists(os.path.join(d, "python3")) for d in stripped.split(":"))
    gate("cargo test with Python removed from PATH" + (" (the OS still provides /usr/bin/python3)" if has_py else ""),
         ["cargo", "test", "--release", "-p", "scrutate", "--", "--quiet"], env={"PATH": stripped}, tail=12)
    gate("cargo audit", ["cargo", "audit"], tail=8)
    gate("python reference tests", [sys.executable, "-m", "pytest", "-q", "--ignore=tests/test_bindings.py"], cwd=os.path.join(ROOT, "python"), tail=6)
    gate("python bindings tests (wheel must be installed)", [sys.executable, "-m", "pytest", "-q", "tests/test_bindings.py"], cwd=os.path.join(ROOT, "python"),
         env={"SCRUTATE_BIN": os.path.join(ROOT, "target", "release", "scrutate")}, tail=6)
    gate("rust/python parity (bit-identical features, scores, rankings)", [sys.executable, "bench/parity.py", a.repo, "target/release/scrutate", "--queries", "300"], tail=8)
    gate("rust/python parity on real repositories of the six reference languages", ["bash", "bench/parity_repos.sh", "target/release/scrutate"],
         env={"PARITY_PYTHON": sys.executable, **({"PARITY_CODE": a.parity_code} if a.parity_code else {})}, tail=24)
    gate("differential test against ripgrep, and grep --index against the walk", [sys.executable, "bench/differential.py", "--cases", str(a.differential_cases), "--seed", "11"], tail=4)
    gate("random grep runs against ripgrep (60 s)", [sys.executable, "bench/fuzz_grep.py", "--seconds", "60", "--seed", "11"], tail=2)
    gate("random edit sequences: the incremental index equals a fresh one (60 s)", [sys.executable, "bench/fuzz_incremental.py", "--seconds", "60", "--seed", "11"], tail=2)
    if a.wheels:
        gate("wheels installed and tested on several interpreters", [sys.executable, "tools/wheel_matrix.py", "--wheels", a.wheels, "--cli", os.path.join(ROOT, "target", "release", "scrutate")] + a.pythons, tail=10)
    fuzz_gates(gate, ci_nightly())
    if a.reproducible:
        gate("reproducible build (two clean builds, identical hash)", ["bash", "tools/reproducible_check.sh"], tail=4, timeout=7200)
    # summary counts from cargo test output
    totals = [int(m) for m in re.findall(r"test result: ok\. (\d+) passed", test_out)]
    head = [
        DOC_HEAD,
        f"Generated by `tools/gen_test_results.py` on {time.strftime('%Y-%m-%d %H:%M %z')}, {platform.platform()}, {os.cpu_count()} cores; `{rustc}`, `{cargo}`, Python {py}. "
        "Each block shows the exact command, its exit status and the tail of its output; nothing is transcribed by hand.",
        "",
        "| Gate | Result | Time |",
        "|---|---|---|",
    ]
    for name, ok, dt in results:
        head.append(f"| {name} | {'PASS' if ok else 'FAIL'} | {dt:.1f} s |")
    head.append("")
    head.append(f"Rust tests passed, by binary: {totals} (total {sum(totals)}).")
    head.append("")
    text = "\n".join(head) + "\n" + "\n".join(sections)
    with open(os.path.join(ROOT, "docs", "TEST_RESULTS.md"), "w") as f:
        f.write(text)
    print("wrote docs/TEST_RESULTS.md", file=sys.stderr)
    return 0 if all(ok for _, ok, _ in results) else 1


if __name__ == "__main__":
    sys.exit(main())
