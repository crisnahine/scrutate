#!/usr/bin/env python3
"""Write or check crates/scrutate/startup.order, the linker order file of the macOS executable.

The file lists the functions a small `scrutate grep` runs, so the linker puts them next to each
other and a search touches fewer pages of the 12 MB of code. ld64 places what it finds and
passes over a name it does not find, so a stale file costs pages, never the build.

usage: order_file.py [--dirs DIR...]  regenerate (macOS on Apple silicon, about 15 minutes: two
                                      release links with a link map, each run under lldb)
       order_file.py --check          the share of the listed names the release links still have

The functions are found by running each release link under lldb with a breakpoint on every
function of its link map, so the names are those of that very link, the standard library's
included. Names carry a hash of how their crate was built, which differs between `cargo build
--release` and `cargo build --release --target aarch64-apple-darwin` (the release workflow): the
file holds the names of both. The release profile links one object (fat LTO, one codegen unit),
so ld64 can place every function; with ThinLTO it passes over the promoted `.llvm.N` functions
and the local ones several objects hold. Needs lldb (Xcode's command line tools). On one machine
processes started under lldb came to hang in dyld's open of their own executable when started
from the build's path, so lldb runs a copy of it; the tool gives up after a minute a search
(`ORDER_FILE_SEARCH_SECONDS` raises that on a busy machine).
"""
import argparse
import json
import os
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
# lldb with a breakpoint on every function takes about 20 s a search on an idle machine
SEARCH_SECONDS = int(os.environ.get("ORDER_FILE_SEARCH_SECONDS", "60"))
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


def link_map(target):
    """The executable of a release link and {mangled name: [addresses]} of its __text symbols."""
    tag = target or "host"
    path = os.path.join(WORK, f"{tag}.map")
    cmd = ["cargo", "rustc", "--release", "-p", "scrutate", "--bin", "scrutate", "--locked"] + (["--target", target] if target else [])
    run(cmd + ["--", "-C", f"link-arg=-Wl,-map,{path}"], cwd=ROOT, env=cargo_env(os.path.join(ROOT, "target")))
    exe = os.path.join(ROOT, "target", *([target] if target else []), "release", "scrutate")
    symbols, text, mode = {}, None, None
    for line in open(path, errors="replace"):
        if line.startswith("# Sections"):
            mode = "sec"
        elif line.startswith("# Symbols"):
            mode = "sym"
        elif line.startswith("# Dead Stripped"):
            break
        elif line.startswith("#"):
            continue
        elif mode == "sec":
            p = line.split("\t")
            if len(p) >= 4 and p[3].strip() == "__text":
                text = (int(p[0], 16), int(p[0], 16) + int(p[1], 16))
        elif mode == "sym" and line.startswith("0x"):
            p = line.rstrip("\n").split("\t")
            if text and text[0] <= int(p[0], 16) < text[1]:
                # several objects can hold a local function of one name
                symbols.setdefault(p[2].split("]", 1)[1].strip(), []).append(int(p[0], 16))
    return exe, symbols


def traced(exe, symbols, dirs):
    """The functions of `symbols` that `exe` runs in the searches, first runs first."""
    with tempfile.TemporaryDirectory() as tmp:
        job = os.path.join(tmp, "job.json")
        out = os.path.join(tmp, "out.json")
        runs = [(["grep"] + args, where) for args in SEARCHES for where in dirs]
        # under lldb a start from the build's own path came to hang in dyld's open of the executable,
        # where a copy of the same file starts: the copy is what runs
        copy = os.path.join(tmp, "scrutate")
        shutil.copy2(exe, copy)
        with open(job, "w") as f:
            json.dump({"exe": copy, "out": out, "runs": runs, "addrs": symbols}, f)
        # a fresh executable's first start is the one the system scans; under lldb it can hang there
        subprocess.run([copy, "--version"], stdout=subprocess.DEVNULL, check=True)
        try:
            run(["lldb", "--batch", "--no-lldbinit", "-o", f"command script import {os.path.abspath(__file__)}"], env=dict(os.environ, ORDER_FILE_JOB=job), timeout=SEARCH_SECONDS * len(runs))
        except subprocess.TimeoutExpired:
            sys.exit(f"lldb took over {SEARCH_SECONDS} s a search on {exe}; is the process stuck before main, or the machine busy (ORDER_FILE_SEARCH_SECONDS)?")
        return json.load(open(out))


def __lldb_init_module(debugger, _dict):
    """Inside lldb: run each search with a breakpoint on every function, noting the first hit."""
    import lldb

    job = json.load(open(os.environ["ORDER_FILE_JOB"]))
    names = {addrs[0]: n for n, addrs in job["addrs"].items()}
    debugger.SetAsync(False)
    order, seen = [], set()
    for args, where in job["runs"]:
        target = debugger.CreateTarget(job["exe"])
        info = target.GetLaunchInfo()
        info.SetArguments(args, False)
        info.SetWorkingDirectory(where)
        info.AddOpenFileAction(0, "/dev/null", True, False)
        info.AddOpenFileAction(1, "/dev/null", False, True)
        info.SetLaunchFlags(info.GetLaunchFlags() | lldb.eLaunchFlagDisableASLR | lldb.eLaunchFlagStopAtEntry)
        err = lldb.SBError()
        process = target.Launch(info, err)
        if not err.Success():
            raise SystemExit(f"lldb could not start {job['exe']}: {err}")
        slide = target.GetModuleAtIndex(0).GetObjectFileHeaderAddress().GetLoadAddress(target) - 0x100000000
        at = {}
        for a in names:
            if a not in seen:
                at[target.BreakpointCreateByAddress(a + slide).GetID()] = a
        process.Continue()
        while process.GetState() == lldb.eStateStopped:
            for thread in process:
                if thread.GetStopReason() == lldb.eStopReasonBreakpoint:
                    bid = thread.GetStopReasonDataAtIndex(0)
                    if bid in at:
                        target.FindBreakpointByID(bid).SetEnabled(False)
                        if at[bid] not in seen:
                            seen.add(at[bid])
                            order.append(names[at[bid]])
            process.Continue()
        debugger.DeleteTarget(target)
    json.dump(order, open(job["out"], "w"))


def generate(dirs):
    os.makedirs(WORK, exist_ok=True)

    def searched(dirs):
        lines = [HEADER]
        for target in (None, TARGET):
            exe, symbols = link_map(target)
            names = traced(exe, symbols, dirs)
            # names ld64 would pass over if the profile went back to ThinLTO
            kept = [n for n in names if ".llvm." not in n and len(symbols[n]) == 1]
            print(f"{target or 'host'}: {len(names)} functions ran, {len(kept)} placeable", file=sys.stderr)
            lines += [f"{n}\n" for n in kept]
        with open(ORDER, "w") as f:
            f.writelines(dict.fromkeys(lines))
        print(f"{ORDER} has {len(dict.fromkeys(lines)) - 1} lines")

    if dirs:
        return searched([os.path.abspath(d) for d in dirs])
    # outside any repository, as the benchmark's directories are: a repository's ignore files
    # would bring in their own code
    with tempfile.TemporaryDirectory() as tree:
        with open(os.path.join(tree, "a.txt"), "w") as f:
            f.write("hello\n")
        os.makedirs(os.path.join(tree, "src", "sub"))
        for i, d in enumerate(["src", "src/sub"]):
            with open(os.path.join(tree, d, f"{i}.rs"), "w") as f:
                f.write("fn main() { println!(\"hello\"); }\n")
        return searched([tree, os.path.join(tree, "src")])


def check():
    os.makedirs(WORK, exist_ok=True)
    listed = [l.rstrip("\n").split(":")[-1] for l in open(ORDER) if l.strip() and not l.startswith("#")]
    have = set(link_map(None)[1]) | set(link_map(TARGET)[1])
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
