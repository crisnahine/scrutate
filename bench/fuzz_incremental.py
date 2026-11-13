#!/usr/bin/env python3
"""Random edit sequences: an incrementally updated index must equal a fresh index of the same tree.

usage: fuzz_incremental.py [--bin SCRUTATE] [--seconds 60] [--seed 1]

Each step applies 1 to 4 random edits (new file, edit, delete, rename, case-only rename, a move
to another directory under the same name, a copy, two files that swap their contents, a
.gitignore written or removed, binary or Latin-1 content, an emptied file) in a tree with a
`.git` directory, runs `scrutate index --no-text`, indexes a copy of the tree from scratch, and
compares chunks (their texts and search fields too), files and the artifacts they name, call
edges and imports of the two databases. The moves, copies and swaps bring content the artifact
store holds to another path. Every 15 steps it runs
`scrutate check`. Needs the sqlite3 command line tool; a failing sqlite3 counts as a problem.
Exit status 1 on any mismatch, or when no step compared a database holding a file; the tree that
differed is kept under the temporary directory printed at the end.
"""
import argparse, os, random, shutil, subprocess, sys, tempfile, time

ap = argparse.ArgumentParser()
ap.add_argument("--bin", default=os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "target", "release", "scrutate"))
ap.add_argument("--seconds", type=float, default=60)
ap.add_argument("--seed", type=int, default=1)
a_ = ap.parse_args()
BIN, SECONDS, SEED = os.path.abspath(a_.bin), a_.seconds, a_.seed
HERE = tempfile.mkdtemp(prefix="scrutate-fuzz-")
T = os.path.join(HERE, f"tree{SEED}")
rnd = random.Random(SEED)
DIRS = ["", "src", "src/a", "lib", "pkg/deep", "build", ".config"]
EXTS = [".py", ".js", ".rs", ".go", ".txt"]
NAMES = ["alpha", "beta", "gamma", "delta", "Mixed", "util", "main"]
IGN = ["*.js", "lib/", "src/a/", "*.txt", "!keep.txt", "pkg/"]

def body(ext):
    n = rnd.randint(0, 5)
    ids = [rnd.randint(1, 40) for _ in range(n)]
    if ext == ".py":
        return "import os\n\n" + "\n\n".join(f"def f_{i}(x):\n    return g_{rnd.randint(1, 40)}(x) + {i}" for i in ids) + "\n"
    if ext == ".js":
        return "\n".join(f"function f_{i}(x) {{\n  return g_{rnd.randint(1, 40)}(x) + {i};\n}}" for i in ids) + "\n"
    if ext == ".rs":
        return "\n".join(f"pub fn f_{i}(x: u32) -> u32 {{\n    g_{rnd.randint(1, 40)}(x) + {i}\n}}" for i in ids) + "\n"
    if ext == ".go":
        return "package p\n\n" + "\n".join(f"func F{i}(x int) int {{\n\treturn G{rnd.randint(1, 40)}(x) + {i}\n}}" for i in ids) + "\n"
    return "\n".join(f"note {i}" for i in ids) + "\n"

def files():
    out = []
    for d, _, fs in os.walk(T):
        if ".scrutate" in d.split(os.sep) or ".git" in d.split(os.sep):
            continue
        out += [os.path.relpath(os.path.join(d, f), T) for f in fs]
    return sorted(out)

def op():
    fs = [f for f in files() if not f.endswith((".gitignore", ".ignore"))]
    kind = rnd.choice(["new", "new", "edit", "edit", "delete", "rename", "move", "copy", "swap", "ignore", "binary", "case", "empty"])
    if kind == "new" or not fs:
        d, ext = rnd.choice(DIRS), rnd.choice(EXTS)
        os.makedirs(os.path.join(T, d), exist_ok=True)
        p = os.path.join(d, rnd.choice(NAMES) + ext)
        open(os.path.join(T, p), "w").write(body(ext))
        return f"new {p}"
    f = rnd.choice(fs)
    full = os.path.join(T, f)
    if kind == "edit":
        open(full, "w").write(body(os.path.splitext(f)[1]))
        return f"edit {f}"
    if kind == "delete":
        os.remove(full)
        return f"delete {f}"
    if kind == "rename":
        d = rnd.choice(DIRS)
        os.makedirs(os.path.join(T, d), exist_ok=True)
        to = os.path.join(d, rnd.choice(NAMES) + rnd.choice(EXTS))
        os.replace(full, os.path.join(T, to))
        return f"rename {f} -> {to}"
    if kind == "move":
        d = rnd.choice(DIRS)
        os.makedirs(os.path.join(T, d), exist_ok=True)
        to = os.path.join(d, os.path.basename(f))
        os.replace(full, os.path.join(T, to))
        return f"move {f} -> {to}"
    if kind == "copy":
        d = rnd.choice(DIRS)
        os.makedirs(os.path.join(T, d), exist_ok=True)
        to = os.path.join(d, rnd.choice([os.path.basename(f), rnd.choice(NAMES) + os.path.splitext(f)[1]]))
        dest = os.path.join(T, to)
        # on a file system that ignores case, a name that differs only in case is the same file
        if os.path.normpath(to) != os.path.normpath(f) and not (os.path.exists(dest) and os.path.samefile(full, dest)):
            shutil.copyfile(full, dest)
        return f"copy {f} -> {to}"
    if kind == "swap":
        g = os.path.join(T, rnd.choice(fs))
        a, b = open(full, "rb").read(), open(g, "rb").read()
        open(full, "wb").write(b)
        open(g, "wb").write(a)
        return f"swap {f} <-> {os.path.relpath(g, T)}"
    if kind == "ignore":
        d = rnd.choice(["", "src"])
        os.makedirs(os.path.join(T, d), exist_ok=True)
        p = os.path.join(T, d, ".gitignore")
        if os.path.exists(p) and rnd.random() < 0.4:
            os.remove(p)
            return f"remove {d}/.gitignore"
        open(p, "w").write("\n".join(rnd.sample(IGN, rnd.randint(1, 3))) + "\n")
        return f"write {d}/.gitignore"
    if kind == "binary":
        open(full, "wb").write(b"def f_1(x):\n    return 1\n\x00\x01binary\n" if rnd.random() < 0.5 else b"# caf\xe9\ndef f_2(x):\n    return 2\n")
        return f"binary-or-latin1 {f}"
    if kind == "case":
        d, b = os.path.split(full)
        to = os.path.join(d, b.swapcase())
        os.replace(full, to)
        return f"case {f} -> {os.path.relpath(to, T)}"
    open(full, "w").write("")
    return f"empty {f}"

Q = ("select 'chunk', path, kind, name, qualname || ' ' || is_test || ' ' || ndoc || ' ' || nbody || ' ' || name_tok || ' ' || hex(text), start, end from chunks union all "
     "select 'file', path, hash, hex(art), '', 0, 0 from files union all "
     "select 'ref', c.path, r.callee, c.qualname, '', c.start, 0 from refs r join chunks c on c.id = r.chunk_id union all "
     "select 'import', path, module, '', '', line, 0 from imports order by 1, 2, 6, 7, 3, 4, 5")

def dump(root):
    p = subprocess.run([BIN, "--root", root, "index", "--no-text"], capture_output=True, text=True)
    if p.returncode != 0:
        return None, p.stderr.strip()[-300:]
    q = subprocess.run(["sqlite3", os.path.join(root, ".scrutate", "index.db"), Q], capture_output=True, text=True)
    if q.returncode != 0:
        return None, f"sqlite3 exit {q.returncode}: {q.stderr.strip()[-300:]}"
    return q.stdout.splitlines(), p.stderr.strip()[-200:]

shutil.rmtree(T, ignore_errors=True)
os.makedirs(os.path.join(T, ".git"))
t0, steps, with_files, bad, log = time.time(), 0, 0, 0, []
while time.time() - t0 < SECONDS and bad < 3:
    ops = [op() for _ in range(rnd.randint(1, 4))]
    log.append(ops)
    steps += 1
    inc, err = dump(T)
    F = os.path.join(HERE, f"fresh{SEED}_{steps}")
    shutil.copytree(T, F, symlinks=True, ignore=shutil.ignore_patterns(".scrutate"))
    fresh, err2 = dump(F)
    if inc is None or fresh is None:
        bad += 1
        print(f"step {steps}: index failed: {err or err2}; ops {ops}")
    elif inc == fresh:
        with_files += any(l.startswith("file|") for l in inc)
    else:
        bad += 1
        only_inc = [l for l in inc if l not in fresh][:5]
        only_fresh = [l for l in fresh if l not in inc][:5]
        print(f"step {steps} MISMATCH after {ops} (previous steps: {log[-4:-1]})")
        for l in only_inc: print("    only incremental:", l[:140])
        for l in only_fresh: print("    only fresh:      ", l[:140])
        shutil.copytree(T, os.path.join(HERE, f"bad{SEED}_{steps}"), symlinks=True)
    if steps % 15 == 0:
        c = subprocess.run([BIN, "--root", T, "check"], capture_output=True, text=True)
        if c.returncode != 0:
            bad += 1
            print(f"step {steps}: check failed: {c.stdout.strip()[-300:]}")
    shutil.rmtree(F, ignore_errors=True)
    sys.stdout.flush()
print(f"{steps} steps in {time.time() - t0:.0f} s (seed {SEED}), {with_files} equal with files indexed: {bad} problems; tree has {len(files())} files" + (f"; kept under {HERE}" if bad else ""))
if not bad:
    shutil.rmtree(HERE, ignore_errors=True)
if not with_files:
    print("no step compared an index holding a file: nothing was really compared")
sys.exit(1 if bad or not with_files else 0)
