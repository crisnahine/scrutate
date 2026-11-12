#!/usr/bin/env python3
"""Write or check crates/scrutate/startup.order, the linker order file of the macOS executable.

The file lists the functions a small `scrutate grep` runs, so the linker puts them next to each
other and a search touches fewer pages of the 12 MB of code. ld64 places what it finds and
passes over a name it does not find, so a stale file costs pages, never the build.

usage: order_file.py [--dirs DIR...]  regenerate (macOS on Apple silicon, about 10 minutes: one
                                      instrumented build, then two release links with a link map)
       order_file.py --check          the share of the listed names the release links still have

Names carry a hash of how their crate was built, which differs between `cargo build --release`
and `cargo build --release --target aarch64-apple-darwin` (the release workflow): the file holds
the names of both. Needs `rustup component add llvm-tools` and Xcode's c++filt.
"""
import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
ORDER = os.path.join(ROOT, "crates", "scrutate", "startup.order")
WORK = os.path.join(ROOT, "target", "order-file")
TARGET = "aarch64-apple-darwin"
# the searches whose functions go first: the start-up row of the benchmarks, then the shapes
# of the other rows, on one file and on a small tree
SEARCHES = [
    ["-F", "-e", "zqxjkvbw_never_present_symbol"],
    ["-F", "-e", "hello"],
    ["-e", "hel+o"],
    ["-F", "-i", "-e", "hello"],
    ["-F", "-w", "-e", "hello"],
    ["-n", "-l", "hello"],
    ["-c", "hello"],
]
HEADER = (
    "# The functions a small `scrutate grep` runs, for ld64's -order_file (build.rs passes it on\n"
    "# macOS). Written by tools/order_file.py; names a link no longer has are passed over.\n"
)


def run(cmd, **kw):
    print("+", " ".join(cmd), file=sys.stderr)
    return subprocess.run(cmd, check=True, **kw)


def cargo_env(target_dir):
    env = dict(os.environ, CARGO_TARGET_DIR=target_dir)
    env.pop("RUSTFLAGS", None)
    return env


def llvm_profdata():
    sysroot = subprocess.run(["rustc", "--print", "sysroot"], cwd=ROOT, capture_output=True, text=True, check=True).stdout.strip()
    host = re.search(r"host: (\S+)", subprocess.run(["rustc", "-vV"], cwd=ROOT, capture_output=True, text=True, check=True).stdout).group(1)
    path = os.path.join(sysroot, "lib", "rustlib", host, "bin", "llvm-profdata")
    if not os.path.exists(path):
        sys.exit("llvm-profdata is missing: rustup component add llvm-tools")
    return path


def demangle(names):
    out = subprocess.run(["c++filt"], input="\n".join(names), capture_output=True, text=True, check=True).stdout.split("\n")
    return out[: len(names)]


def functions_run(dirs):
    """The functions with a counter above zero in an instrumented build's runs, first runs first."""
    target_dir = os.path.join(WORK, "instrumented")
    prof = os.path.join(WORK, "profiles")
    shutil.rmtree(prof, ignore_errors=True)
    os.makedirs(prof)
    env = cargo_env(target_dir)
    env["RUSTFLAGS"] = f"-Cprofile-generate={prof}"
    run(["cargo", "build", "--release", "-p", "scrutate", "--locked"], cwd=ROOT, env=env)
    exe = os.path.join(target_dir, "release", "scrutate")
    profdata = llvm_profdata()
    names, seen = [], set()

    def searches(dirs):
        for i, args in enumerate(SEARCHES):
            for k, where in enumerate(dirs):
                raw = os.path.join(prof, f"{i}-{k}.profraw")
                subprocess.run([exe, "grep"] + args, cwd=where, stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL,
                               env=dict(os.environ, LLVM_PROFILE_FILE=raw), check=False)
                text = subprocess.run([profdata, "merge", "--text", "-o", "-", raw], capture_output=True, text=True, check=True).stdout
                for name in ran(text):
                    if name not in seen:
                        seen.add(name)
                        names.append(name)

    if dirs:
        searches(dirs)
        return names
    # outside any repository, as the benchmark's directories are: a repository's ignore files
    # would bring in their own code
    with tempfile.TemporaryDirectory() as tree:
        with open(os.path.join(tree, "a.txt"), "w") as f:
            f.write("hello\n")
        os.makedirs(os.path.join(tree, "src", "sub"))
        for i, d in enumerate(["src", "src/sub"]):
            with open(os.path.join(tree, d, f"{i}.rs"), "w") as f:
                f.write("fn main() { println!(\"hello\"); }\n")
        searches([tree, os.path.join(tree, "src")])
    return names


def ran(text):
    lines = text.split("\n")
    i = 0
    while i < len(lines):
        if not lines[i] or lines[i][0] in "#:":
            i += 1
            continue
        name, n = lines[i], int(lines[i + 4])
        counts = lines[i + 6 : i + 6 + n]
        i += 6 + n
        while i < len(lines) and lines[i]:
            i += 1
        if any(c != "0" for c in counts):
            yield name.split(";", 1)[-1]  # a local function's name follows its file's


def link_map(target):
    """{mangled name: [object file leaf names]} of the __text symbols of a release link."""
    tag = target or "host"
    path = os.path.join(WORK, f"{tag}.map")
    cmd = ["cargo", "rustc", "--release", "-p", "scrutate", "--bin", "scrutate", "--locked"] + (["--target", target] if target else [])
    run(cmd + ["--", "-C", f"link-arg=-Wl,-map,{path}"], cwd=ROOT, env=cargo_env(os.path.join(ROOT, "target")))
    objects, symbols, text, mode = {}, {}, None, None
    for line in open(path, errors="replace"):
        if line.startswith("# Object files"):
            mode = "obj"
        elif line.startswith("# Sections"):
            mode = "sec"
        elif line.startswith("# Symbols"):
            mode = "sym"
        elif line.startswith("#"):
            continue
        elif mode == "obj" and line.startswith("["):
            idx, file = line.split("]", 1)
            objects[idx.strip("[ ")] = os.path.basename(file.strip())
        elif mode == "sec":
            p = line.split("\t")
            if len(p) >= 4 and p[3].strip() == "__text":
                text = (int(p[0], 16), int(p[0], 16) + int(p[1], 16))
        elif mode == "sym" and line.startswith("0x"):
            p = line.rstrip("\n").split("\t")
            if text and text[0] <= int(p[0], 16) < text[1]:
                idx, name = p[2].split("]", 1)
                symbols.setdefault(name.strip(), []).append(objects.get(idx.strip("[ "), ""))
    return symbols


def by_meaning(symbols):
    """{demangled name without the LTO suffix: [mangled names]}."""
    mangled = list(symbols)
    plain = demangle([re.sub(r"\.llvm\.\d+$", "", m)[1:] for m in mangled])
    out = {}
    for m, d in zip(mangled, plain):
        out.setdefault(d, []).append(m)
    return out


def generate(dirs):
    os.makedirs(WORK, exist_ok=True)
    names = functions_run(dirs)
    wanted = demangle(names)
    lines, kept = [HEADER], 0
    maps = [link_map(None), link_map(TARGET)]
    meanings = [by_meaning(m) for m in maps]
    for d in dict.fromkeys(wanted):
        found = False
        for symbols, meaning in zip(maps, meanings):
            for m in meaning.get(d, []):
                found = True
                files = symbols[m]
                # ld64 does not place a local function that several objects hold, even named
                # with its object (`file.o:name`)
                if len(files) == 1:
                    lines.append(f"{m}\n")
        kept += found
    with open(ORDER, "w") as f:
        f.writelines(dict.fromkeys(lines))
    print(f"{len(names)} functions ran, {kept} of them are in the links; {ORDER} has {len(set(lines)) - 1} lines")


def check():
    os.makedirs(WORK, exist_ok=True)
    listed = [l.rstrip("\n").split(":")[-1] for l in open(ORDER) if l.strip() and not l.startswith("#")]
    have = set(link_map(None)) | set(link_map(TARGET))
    found = sum(n in have for n in dict.fromkeys(listed))
    total = len(dict.fromkeys(listed))
    print(f"{found} of {total} names of {os.path.relpath(ORDER, ROOT)} are in the release links ({100 * found / total:.0f}%)")
    return 0 if found * 2 >= total else 1


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--dirs", nargs="+", default=[], help="directories to search instead of a small tree made for the purpose")
    if sys.platform != "darwin":
        sys.exit("the order file is made from ld64 link maps: run this on macOS")
    a = ap.parse_args()
    return check() if a.check else generate(a.dirs) or 0


if __name__ == "__main__":
    sys.exit(main())
