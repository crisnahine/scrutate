#!/usr/bin/env python3
"""Differential testing of Engine A against ripgrep: random corpora x random patterns, exact
agreement on (path, line number, line text) for every match, plus -c and -l modes; and of Engine
B's text index (`grep --index`) against Engine A on the same cases, before and after an update.

usage: differential.py [--cases N] [--seed S] [--keep]
exit status 0 when every case agrees; the first disagreement is printed with a reproduction.

Both tools are run with equivalent flags (line numbers, no colour, same case rules, same word
semantics, multiline when the pattern contains a newline escape). Known, documented semantic
differences are excluded from the generator rather than tolerated in the comparison.
"""
from __future__ import annotations

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile

HERE = os.path.dirname(os.path.abspath(__file__))
SCRUTATE = os.environ.get("SCRUTATE_BIN", os.path.join(HERE, "..", "target", "release", "scrutate"))
RG = shutil.which("rg") or "/usr/bin/rg"

WORDS = ["alpha", "beta", "gamma", "delta", "token", "refresh", "Token", "REFRESH_TOKEN", "refreshToken", "foo_bar", "x", "y1", "self", "def",
         "class", "import", "return", "value", "été", "日本語", "naïve", "Ωmega", "tab\there", "a.b", "a*b", "(x)", "[y]", "end$", "^start", "back\\slash"]


def random_corpus(rng: random.Random, root: str) -> None:
    nfiles = rng.randint(1, 8)
    for i in range(nfiles):
        sub = rng.choice(["", "src/", "src/deep/dir/", "docs/"])
        name = rng.choice([f"f{i}.py", f"f{i}.txt", f"f{i}.rs", f"F{i}.MD", f"noext{i}", f"sp ace{i}.txt"])
        p = os.path.join(root, sub, name)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        lines = []
        for _ in range(rng.randint(0, 60)):
            k = rng.randint(0, 8)
            line = " ".join(rng.choice(WORDS) for _ in range(k))
            if rng.random() < 0.05:
                line = "x" * rng.randint(200, 5000)  # a long line
            lines.append(line)
        sep = rng.choice(["\n", "\n", "\n", "\r\n"])
        text = sep.join(lines)
        if rng.random() < 0.7:
            text += sep
        data = text.encode("utf8")
        if rng.random() < 0.05:
            data = b"\xef\xbb\xbf" + data  # BOM
        with open(p, "wb") as f:
            f.write(data)
    if rng.random() < 0.3:
        with open(os.path.join(root, ".gitignore"), "w") as f:
            f.write("docs/\n*.rs\n")
    if rng.random() < 0.3:
        os.makedirs(os.path.join(root, ".hidden"), exist_ok=True)
        with open(os.path.join(root, ".hidden", "h.txt"), "w") as f:
            f.write("alpha beta\n")


def random_pattern(rng: random.Random) -> tuple[str, list[str], list[str], str]:
    """-> (pattern, scrutate flags, rg flags, mode)"""
    mode = rng.choice(["literal", "literal", "regex", "regex", "icase", "word", "multiline", "pcre2"])
    w = rng.choice(WORDS)
    if mode == "literal":
        pat = rng.choice([w, w[: max(1, len(w) // 2)], w + " " + rng.choice(WORDS)])
        return pat, ["-F"], ["-F"], mode
    if mode == "icase":
        pat = rng.choice([w.upper(), w.lower(), w.capitalize()])
        return pat, ["-F", "-i"], ["-F", "-i"], mode
    if mode == "word":
        return w, ["-F", "-w"], ["-F", "-w"], mode
    if mode == "pcre2":
        w2 = rng.choice(WORDS)
        pat = rng.choice([rf"(?<={w[:2]}){w[2:] or w}", rf"{w}(?=\s+{w2})", rf"(?<!x)\b{w}\b", r"(\w)\1", rf"(?i){w}(?!{w2})", rf"({w}).*\1"])
        return pat, ["-P"], ["-P"], mode
    if mode == "multiline":
        return rng.choice([r"alpha\n\s*beta", r"def\s*\n", r"(?s)gamma.{0,20}delta"]), ["-U"], ["-U"], mode
    atoms = [r"\w+", r"\balpha\b", "(alpha|beta)", "ga+mma", r"\d", "[a-c]+", "tok(en)?", r"^\s*def", r"value$", "é", ".", r"x{3,}", r"refresh[_-]?token", "(?i)TOKEN"]
    pat = "".join(rng.choice(atoms) for _ in range(rng.randint(1, 3)))
    return pat, [], [], "regex"


def run_json(cmd: list[str], cwd: str) -> tuple[int, bytes]:
    r = subprocess.run(cmd, cwd=cwd, capture_output=True, stdin=subprocess.DEVNULL, timeout=120)
    return r.returncode, r.stdout


def scrutate_matches(out: bytes) -> set:
    s = set()
    for line in out.splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get("type") == "match":
            s.add((r["path"], r["line"], r["text"]))
    return s


def rg_matches(out: bytes) -> set:
    s = set()
    for line in out.splitlines():
        try:
            r = json.loads(line)
        except ValueError:
            continue
        if r.get("type") == "match":
            d = r["data"]
            text = d["lines"].get("text")
            if text is None:
                text = bytes.fromhex(d["lines"]["bytes"]).decode("utf8", "replace") if "bytes" in d["lines"] else ""
            text = text.rstrip("\n").rstrip("\r")
            path = d["path"].get("text", "")
            path = path[2:] if path.startswith("./") else path
            s.add((path, d["line_number"], text))
    return s


def indexed_agrees(rng: random.Random, root: str, pat: str, sf: list[str], mode: str) -> str | None:
    """Engine B's text index must give Engine A's answer, before and after an incremental update."""
    for stage in ("fresh", "updated"):
        if stage == "updated":
            files = [os.path.join(d, f) for d, _, fs in os.walk(root) if ".scrutate" not in d for f in fs]
            if not files:
                return None
            victim = rng.choice(sorted(files))
            with open(victim, "a", encoding="utf8") as f:
                f.write(" ".join(rng.choice(WORDS) for _ in range(6)) + "\n")
        if subprocess.run([SCRUTATE, "index", "--text"], cwd=root, capture_output=True).returncode != 0:
            return f"index --text failed (corpus {root})"
        rc1, o1 = run_json([SCRUTATE, "grep", "--sort", "--json", *sf, pat, "."], root)
        rc2, o2 = run_json([SCRUTATE, "grep", "--index", "--sort", "--json", *sf, pat], root)
        if rc1 != rc2 or scrutate_matches(o1) != scrutate_matches(o2):
            only_walk = sorted(scrutate_matches(o1) - scrutate_matches(o2))[:3]
            only_index = sorted(scrutate_matches(o2) - scrutate_matches(o1))[:3]
            return f"--index differs ({stage}) for {pat!r} ({mode}, flags {sf}): exit {rc1}/{rc2}; only walk {only_walk}; only index {only_index} (corpus {root})"
    return None


def one_case(rng: random.Random, keep: bool) -> str | None:
    root = tempfile.mkdtemp(prefix="scrutate_diff_")
    try:
        random_corpus(rng, root)
        pat, sf, rf, mode = random_pattern(rng)
        if mode == "multiline":
            # rg's -U with --json reports multi-line matches as one record spanning lines; compare counts of files instead
            rc1, o1 = run_json([SCRUTATE, "grep", "-l", *sf, pat, "."], root)
            rc2, o2 = run_json([RG, "--no-messages", "-l", *rf, pat, "."], root)
            a = sorted(l.decode() for l in o1.splitlines())
            b = sorted(l.decode()[2:] if l.startswith(b"./") else l.decode() for l in o2.splitlines())
            if a != b:
                return f"multiline -l mismatch for {pat!r}: scrutate={a} rg={b} (corpus {root})"
            return indexed_agrees(rng, root, pat, sf, mode)
        rc1, o1 = run_json([SCRUTATE, "grep", "--json", *sf, pat, "."], root)
        rc2, o2 = run_json([RG, "--json", "--no-messages", *rf, pat, "."], root)
        if rc1 not in (0, 1) or rc2 not in (0, 1):
            if rc1 == 2 and rc2 == 2:
                return None  # both reject the pattern
            return f"exit codes differ for {pat!r} ({mode}): scrutate={rc1} rg={rc2} (corpus {root})"
        a, b = scrutate_matches(o1), rg_matches(o2)
        if a != b:
            only_a = sorted(a - b)[:3]
            only_b = sorted(b - a)[:3]
            return f"match sets differ for {pat!r} ({mode}, flags {sf}): only scrutate {only_a}; only rg {only_b} (corpus {root})"
        # count mode
        rc1, o1 = run_json([SCRUTATE, "grep", "-c", *sf, pat, "."], root)
        rc2, o2 = run_json([RG, "--no-messages", "-c", *rf, pat, "."], root)
        ca = {l.rsplit(b":", 1)[0].decode(): int(l.rsplit(b":", 1)[1]) for l in o1.splitlines() if b":" in l}
        cb = {}
        for l in o2.splitlines():
            if b":" in l:
                p, n = l.rsplit(b":", 1)
                p = p.decode()
                cb[p[2:] if p.startswith("./") else p] = int(n)
        if ca != cb:
            return f"-c differs for {pat!r} ({mode}): scrutate={ca} rg={cb} (corpus {root})"
        return indexed_agrees(rng, root, pat, sf, mode)
    finally:
        if not keep:
            shutil.rmtree(root, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--cases", type=int, default=200)
    ap.add_argument("--seed", type=int, default=1)
    ap.add_argument("--keep", action="store_true", help="keep the corpus of a failing case")
    a = ap.parse_args()
    if not os.path.exists(SCRUTATE) or not os.path.exists(RG):
        print(f"need {SCRUTATE} and ripgrep", file=sys.stderr)
        return 2
    rng = random.Random(a.seed)
    failures = 0
    for i in range(a.cases):
        problem = one_case(rng, a.keep)
        if problem:
            failures += 1
            print(f"case {i}: {problem}")
            if failures >= 5:
                break
    print(f"{a.cases} cases, {failures} disagreements (seed {a.seed})")
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
