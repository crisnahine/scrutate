#!/usr/bin/env python3
"""An agent end to end: each task of agent_eval/tasks.json run by Claude Code with and without
scrutate's MCP server, and scored by a check on the answer.

usage: agent_eval.py --scrutate BIN --dry-run [--only ID ...] [--split tune|heldout|all]
       agent_eval.py --scrutate BIN --yes-spend [--only ID ...] [--split ...] [--model M]
                     [--repeats N] [--max-budget-usd X] [--out DIR] [--login]

Every run is `claude -p` with `--output-format stream-json --verbose` (the last line is the
result object `--output-format json` prints; the lines before it are the transcript, so tool
calls and tool errors are read from the same output, and `--no-session-persistence` keeps the
session off disk). Both arms get the same model, prompt and built-in tools, each run a fresh
session in its own temporary copy of the corpus:

  mcp      --mcp-config RUN/mcp.json --strict-mcp-config, an index built before the clock starts
  builtin  --strict-mcp-config and no --mcp-config: no MCP server at all

The MCP config sits in the run's folder beside the copy (RUN/mcp.json, RUN/repo/), so a search
of the copy never finds it. `--bare` (the default) reads no CLAUDE.md, hooks, plugins, settings
or MCP configuration of the user and needs ANTHROPIC_API_KEY. `--login` drops `--bare` to use the
logged-in account; user settings are then left out with `--setting-sources project`, but the
user's CLAUDE.md and the login itself are still read. Nothing outside the run's folder is
written by the harness.

Every run is a paid agent run, so without --yes-spend (or with --dry-run) no `claude -p` starts:
the dry run builds every copy, index and MCP config, prints each command line, and makes one
initialize and tools/list round trip to each MCP config's server.

Writes OUT/results.json (per run: solved, the check of each answer field, tokens, cost, tool
calls, tool errors, turns, wall time), OUT/results.md (a table per arm and the tool usage: the
scrutate tools never called and the failed calls with their arguments) and OUT/transcripts/.

The tasks: 20 over three pinned corpora (scrutate's own tree by `git archive` of a commit,
ripgrep and networkx from bench/corpora.py's folder, each checked against its tree sha256), each
with the answer derived from the code and a note of how it was checked. Seven are held out
(`split: heldout`): leave them out of any tuning of tool descriptions or prompts, and run them
(`--split heldout`) only to report.

Smoke test, 2 tasks and 4 runs (run the dry run first):

  python3 bench/agent_eval.py --scrutate target/release/scrutate --dry-run --only ripgrep-run-dispatch networkx-multisource-callers
  ANTHROPIC_API_KEY=... python3 bench/agent_eval.py --scrutate target/release/scrutate --yes-spend \
      --only ripgrep-run-dispatch networkx-multisource-callers --out agent-eval-smoke

Full round, 40 runs: `--split all --yes-spend --out DIR` (or `--split tune` then `--split heldout`).

Cost, an estimate from list prices (claude-opus-5-5: $4 and $20 per million input and output
tokens, $0.20 a cache read, $5 a cache write) and a guessed run of 12 turns on a 20,000-token
cached prefix growing 3,000 tokens a turn, with 3,000 output tokens: about $0.43 a run without
scrutate and $0.46 with it (its tool list adds about 4,200 tokens), $0.25 to $0.92 for 6 to 25
turns. So about $1.80 for the smoke test and $18 for a full round (about $10 to $37); --max-budget-usd
($1.00 by default) stops a run that reaches it. claude-sonnet-5-5 costs about half. The real
figure is each run's total_cost_usd, in results.json.
"""
from __future__ import annotations

import argparse
import datetime
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from corpora import tree_hash  # noqa: E402

REPO = os.path.normpath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
TASKS = os.path.join(REPO, "bench", "agent_eval", "tasks.json")
CORPORA = os.path.expanduser("~/.cache/scrutate-bench/corpora")
ARMS = ("mcp", "builtin")
TOOLS = ("search_code", "grep_code", "find_symbol", "callers", "callees", "call_path", "repo_map", "read_code", "outline",
         "dependencies", "dependents", "references", "similar_code", "duplicates", "structural_search", "impact",
         "find_files", "dead_code", "reindex")
BUILTIN_TOOLS = "Bash,Read,Grep,Glob"
FLAGS = ("--print", "--output-format", "--verbose", "--model", "--mcp-config", "--strict-mcp-config", "--tools",
         "--allowedTools", "--permission-mode", "--no-session-persistence", "--max-budget-usd", "--bare",
         "--setting-sources", "--disable-slash-commands")
# per million tokens: input, output, cache read; a cache write is 1.25 times input (5-minute entries)
PRICES = {"claude-opus-5-5": (4.0, 20.0, 0.20), "claude-sonnet-5-5": (2.0, 10.0, 0.10), "claude-haiku-5-5": (0.10, 0.50, 0.01)}

PROMPT = """{question}

Answer from the code in the current directory. Do not change any file. End your reply with one
line that starts with ANSWER: followed by a JSON object of this shape (no code fence):
{shape}
A location is written path:line, with the path relative to the current directory."""


# ---- answers and checks -------------------------------------------------------------------

def answer_of(text: str):
    """The JSON object after the last `ANSWER:` of a reply, or None."""
    for line in reversed((text or "").splitlines()):
        at = line.find("ANSWER:")
        if at >= 0:
            body = line[at + len("ANSWER:"):].strip().strip("`")
            try:
                v = json.loads(body)
            except json.JSONDecodeError:
                return None
            return v if isinstance(v, dict) else None
    return None


def plain_path(p) -> str:
    s = str(p).strip().strip("`'\"").replace("\\", "/")
    while s.startswith("./"):
        s = s[2:]
    return s


def plain_name(n) -> str:
    """A function name as a person might write it, reduced to its last part: `Scope::files`,
    `Core.roll()`, `` `run` `` and `ops.rs:570 run` are files, roll and run."""
    s = re.sub(r"^\S+:\d+(-\d+)?\s+", "", str(n).strip().strip("`'\""))
    m = re.match(r"[\w:.#!?$]+", s.strip("`"))
    return re.split(r"::|\.", m.group(0))[-1].lower() if m else ""


def location_of(v):
    """(path, line) from `path:line`, `path:start-end`, `path:line:col` or {path, line}."""
    if isinstance(v, dict):
        path, line = v.get("path"), v.get("line", v.get("start"))
        return (plain_path(path), int(line)) if path is not None and str(line).lstrip("-").isdigit() else None
    m = re.match(r"^(.+?):(\d+)(?:[-:]\d+)?$", str(v).strip().strip("`"))
    return (plain_path(m.group(1)), int(m.group(2))) if m else None


def check_field(spec: dict, got) -> bool:
    kind = spec["type"]
    if kind == "location":
        loc = location_of(got)
        lo, hi = spec["lines"]
        return loc is not None and loc[0] == spec["path"] and lo <= loc[1] <= hi
    items = got if isinstance(got, list) else [got]
    if kind == "paths":
        return got is not None and {plain_path(x) for x in items} == set(spec["expected"])
    if kind == "names":
        names = {plain_name(x) for x in items if x is not None}
        # an expected entry may be a list of names that each count
        wants = [{plain_name(a) for a in (w if isinstance(w, list) else [w])} for w in spec["expected"]]
        return all(names & w for w in wants) and all(any(n in w for w in wants) for n in names)
    if kind == "chain":
        names, want = [plain_name(x) for x in items], [plain_name(x) for x in spec["expected"]]
        if not names or names[0] != want[0] or names[-1] != want[-1]:
            return False
        it = iter(names)
        return all(w in it for w in want)
    raise ValueError(f"unknown check type {kind}")


def check(task: dict, reply: str) -> dict:
    """Whether the reply solves the task, and the verdict on each field of its answer."""
    ans = answer_of(reply)
    fields = {k: ans is not None and check_field(spec, ans.get(k)) for k, spec in task["check"].items()}
    return {"answer": ans, "fields": fields, "solved": ans is not None and all(fields.values())}


# ---- transcript -----------------------------------------------------------------------------

def tool_text(content) -> str:
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        return "\n".join(c.get("text", "") for c in content if isinstance(c, dict))
    return ""


def parse_stream(lines) -> dict:
    """What a stream-json transcript says: the init message, every tool call with its result,
    and the final result object."""
    init, result, calls, by_id = None, None, [], {}
    for raw in lines:
        raw = raw.strip()
        if not raw:
            continue
        try:
            m = json.loads(raw)
        except json.JSONDecodeError:
            continue
        kind = m.get("type")
        if kind == "system" and m.get("subtype") == "init":
            init = m
        elif kind == "result":
            result = m
        elif kind in ("assistant", "user"):
            for block in (m.get("message") or {}).get("content") or []:
                if not isinstance(block, dict):
                    continue
                if block.get("type") == "tool_use":
                    call = {"id": block.get("id"), "name": block.get("name"), "input": block.get("input"),
                            "subagent": m.get("parent_tool_use_id") is not None, "error": None, "is_error": False}
                    by_id[call["id"]] = call
                    calls.append(call)
                elif block.get("type") == "tool_result" and block.get("tool_use_id") in by_id:
                    call = by_id[block["tool_use_id"]]
                    call["is_error"] = bool(block.get("is_error"))
                    if call["is_error"]:
                        call["error"] = tool_text(block.get("content"))[:500]
    return {"init": init, "result": result, "calls": calls}


def metrics(parsed: dict) -> dict:
    r = parsed["result"] or {}
    u = r.get("usage") or {}
    names = {}
    for c in parsed["calls"]:
        names[c["name"]] = names.get(c["name"], 0) + 1
    return {
        "subtype": r.get("subtype"),
        "is_error": r.get("is_error"),
        "input_tokens": u.get("input_tokens", 0),
        "output_tokens": u.get("output_tokens", 0),
        "cache_creation_input_tokens": u.get("cache_creation_input_tokens", 0),
        "cache_read_input_tokens": u.get("cache_read_input_tokens", 0),
        "cost_usd": r.get("total_cost_usd"),
        "turns": r.get("num_turns"),
        "duration_ms": r.get("duration_ms"),
        "tool_calls": len(parsed["calls"]),
        "tools": names,
        "tool_errors": [{"tool": c["name"], "input": c["input"], "error": c["error"]} for c in parsed["calls"] if c["is_error"]],
        "permission_denials": len(r.get("permission_denials") or []),
    }


def arm_problem(arm: str, init) -> str | None:
    """Why a run does not count for its arm: the MCP arm without a connected scrutate, or the
    builtin arm with any MCP tool."""
    if init is None:
        return "no init message"
    servers = {s.get("name"): s.get("status") for s in init.get("mcp_servers") or []}
    mcp_tools = [t for t in init.get("tools") or [] if str(t).startswith("mcp__")]
    if arm == "mcp" and servers.get("scrutate") != "connected":
        return f"scrutate server not connected: {servers}"
    if arm == "builtin" and (servers or mcp_tools):
        return f"MCP present in the builtin arm: {servers} {mcp_tools[:3]}"
    return None


def tool_usage(runs: list) -> dict:
    """Per scrutate tool, over the MCP runs: calls, failed calls and their messages."""
    usage = {t: {"calls": 0, "errors": 0, "messages": []} for t in TOOLS}
    for run in runs:
        if run["arm"] != "mcp" or "metrics" not in run:
            continue
        for name, n in run["metrics"]["tools"].items():
            t = name.removeprefix("mcp__scrutate__")
            if t in usage:
                usage[t]["calls"] += n
        for e in run["metrics"]["tool_errors"]:
            t = e["tool"].removeprefix("mcp__scrutate__")
            if t in usage:
                usage[t]["errors"] += 1
                usage[t]["messages"].append({"input": e["input"], "error": e["error"]})
    return {"never_called": [t for t, u in usage.items() if u["calls"] == 0],
            "with_errors": {t: u for t, u in usage.items() if u["errors"]}, "per_tool": usage}


# ---- runs -------------------------------------------------------------------------------------

def claude_check() -> dict:
    """The installed version, refusing one whose --help lacks a flag the harness passes."""
    version = subprocess.run(["claude", "--version"], capture_output=True, text=True, check=True).stdout.strip()
    helptext = subprocess.run(["claude", "--help"], capture_output=True, text=True, check=True).stdout
    missing = [f for f in FLAGS if not re.search(re.escape(f) + r"\b", helptext)]
    if missing:
        sys.exit(f"claude {version} lacks {', '.join(missing)}: this harness needs them")
    return {"version": version, "flags_checked": list(FLAGS)}


def corpus_base(name: str, spec: dict, work: str, corpora: str) -> str:
    """A checked copy of the corpus, made once per invocation: its tree hash must be the pinned one."""
    base = os.path.join(work, "base", name)
    if os.path.isdir(base):
        return base
    if spec["source"] == "git-archive":
        tar = os.path.join(work, f"{name}.tar")
        subprocess.run(["git", "-C", REPO, "archive", "-o", tar, spec["commit"]], check=True)
        os.makedirs(base)
        with tarfile.open(tar) as t:
            t.extractall(base, filter="data")
        os.remove(tar)
    else:
        shutil.copytree(os.path.join(corpora, spec["dir"]), base, ignore=shutil.ignore_patterns(".git", ".scrutate"), symlinks=True)
    got = tree_hash(base)[0]
    if got != spec["tree_sha256"]:
        sys.exit(f"corpus {name}: tree sha256 {got}, pinned {spec['tree_sha256']}; the answers hold only for the pinned tree")
    return base


def mcp_config(binary: str, repo: str) -> dict:
    return {"mcpServers": {"scrutate": {"command": binary, "args": ["--root", repo, "serve"]}}}


def command(task: dict, arm: str, run_dir: str, args) -> list:
    prompt = PROMPT.format(question=task["question"], shape=json.dumps(task["answer_shape"]))
    allowed = "Read,Grep,Glob" + (",mcp__scrutate" if arm == "mcp" else "")
    argv = ["claude", "-p", prompt, "--output-format", "stream-json", "--verbose", "--model", args.model,
            "--no-session-persistence", "--tools", BUILTIN_TOOLS, "--allowedTools", allowed,
            "--permission-mode", "dontAsk", "--disable-slash-commands", "--max-budget-usd", str(args.max_budget_usd),
            "--strict-mcp-config"]
    argv += ["--setting-sources", "project"] if args.login else ["--bare"]
    if arm == "mcp":
        argv += ["--mcp-config", os.path.join(run_dir, "mcp.json")]
    return argv


def prepare(task: dict, arm: str, rep: int, base: str, work: str, binary: str) -> str:
    run_dir = os.path.join(work, "runs", f"{task['id']}.{arm}.{rep}")
    repo = os.path.join(run_dir, "repo")
    shutil.copytree(base, repo, symlinks=True)
    if arm == "mcp":
        subprocess.run([binary, "--root", repo, "index"], check=True, capture_output=True)
        with open(os.path.join(run_dir, "mcp.json"), "w") as f:
            json.dump(mcp_config(binary, repo), f, indent=1)
    return run_dir


def mcp_roundtrip(config_path: str, timeout: float = 60) -> dict:
    """Start the server an MCP config names, as a client would, and list its tools."""
    with open(config_path) as f:
        server = json.load(f)["mcpServers"]["scrutate"]
    msgs = [
        {"jsonrpc": "2.0", "id": 1, "method": "initialize",
         "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "agent-eval", "version": "0"}}},
        {"jsonrpc": "2.0", "method": "notifications/initialized"},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/list"},
        {"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "repo_map", "arguments": {"response_format": "concise", "max_bytes": 1000}}},
    ]
    out = subprocess.run([server["command"], *server["args"]], input="".join(json.dumps(m) + "\n" for m in msgs),
                         capture_output=True, text=True, timeout=timeout, env=clean_env(), check=False)
    replies = {r.get("id"): r for r in map(json.loads, filter(None, out.stdout.splitlines()))}
    tools = [t["name"] for t in replies.get(2, {}).get("result", {}).get("tools", [])]
    call = replies.get(3, {}).get("result", {})
    return {"server": replies.get(1, {}).get("result", {}).get("serverInfo"), "tools": tools,
            "call_ok": call.get("isError") is False and bool(call.get("content"))}


def clean_env() -> dict:
    return {k: v for k, v in os.environ.items() if not k.startswith("SCRUTATE_")}


def run_one(argv: list, run_dir: str, timeout: float) -> tuple[list, float, int]:
    t0 = time.monotonic()
    with open(os.path.join(run_dir, "stream.jsonl"), "w") as out:
        p = subprocess.run(argv, cwd=os.path.join(run_dir, "repo"), stdout=out, stderr=subprocess.PIPE, text=True,
                           timeout=timeout, env=clean_env(), stdin=subprocess.DEVNULL, check=False)
    wall = time.monotonic() - t0
    with open(os.path.join(run_dir, "stream.jsonl")) as f:
        return f.readlines(), wall, p.returncode


def cost_estimate(model: str, turns: int, prefix: int, growth: int, output: int, mcp_tokens: int = 0) -> float:
    """Dollars for one run of `turns` requests on a cached prefix of `prefix` tokens that grows by
    `growth` tokens a turn: each request reads the cache and writes what is new, and `output`
    tokens come out in all. An estimate: real runs report total_cost_usd."""
    inp, out, read = PRICES[model]
    prefix += mcp_tokens
    reads = sum(prefix + i * growth for i in range(turns))
    writes = prefix + turns * growth
    return (reads * read + writes * inp * 1.25 + output * out) / 1e6


def table(runs: list) -> str:
    rows = ["| task | split | arm | solved | in | out | cache read | cache write | cost $ | tool calls | tool errors | turns | wall s |",
            "|---|---|---|---|---|---|---|---|---|---|---|---|---|"]
    for r in runs:
        m = r.get("metrics")
        if not m:
            rows.append(f"| {r['task']} | {r['split']} | {r['arm']} | {r.get('problem', 'not run')} |" + " |" * 9)
            continue
        cost = "" if m["cost_usd"] is None else f"{m['cost_usd']:.3f}"
        rows.append(f"| {r['task']} | {r['split']} | {r['arm']} | {'yes' if r['solved'] else 'no'} | {m['input_tokens']} | "
                    f"{m['output_tokens']} | {m['cache_read_input_tokens']} | {m['cache_creation_input_tokens']} | {cost} | "
                    f"{m['tool_calls']} | {len(m['tool_errors'])} | {m['turns']} | {r['wall_s']:.0f} |")
    return "\n".join(rows)


def summary(runs: list) -> str:
    out = ["| arm | split | runs | solved | median cost $ | median tokens (in+out+cache) | median tool calls |", "|---|---|---|---|---|---|---|"]
    for arm in ARMS:
        for split in ("tune", "heldout"):
            rs = [r for r in runs if r["arm"] == arm and r["split"] == split and "metrics" in r and not r.get("problem")]
            if not rs:
                continue

            def med(xs):
                xs = sorted(xs)
                return xs[len(xs) // 2] if xs else 0
            m = [r["metrics"] for r in rs]
            tokens = med([x["input_tokens"] + x["output_tokens"] + x["cache_read_input_tokens"] + x["cache_creation_input_tokens"] for x in m])
            out.append(f"| {arm} | {split} | {len(rs)} | {sum(r['solved'] for r in rs)} | {med([x['cost_usd'] or 0 for x in m]):.3f} | "
                       f"{tokens} | {med([x['tool_calls'] for x in m])} |")
    return "\n".join(out)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--scrutate", required=True, help="the scrutate binary the MCP arm serves")
    ap.add_argument("--tasks", default=TASKS)
    ap.add_argument("--corpora", default=CORPORA, help="the folder of bench/corpora.py")
    ap.add_argument("--only", nargs="*", default=[], help="task ids")
    ap.add_argument("--split", choices=("tune", "heldout", "all"), default="tune",
                    help="held-out tasks stay out of tuning: run them only to report")
    ap.add_argument("--arms", default=",".join(ARMS))
    ap.add_argument("--model", default="claude-opus-5-5")
    ap.add_argument("--repeats", type=int, default=1)
    ap.add_argument("--max-budget-usd", type=float, default=1.0, help="per run, passed to claude")
    ap.add_argument("--timeout", type=float, default=900, help="seconds per run")
    ap.add_argument("--login", action="store_true", help="use the logged-in account in place of --bare and ANTHROPIC_API_KEY")
    ap.add_argument("--out", help="results folder (default: a new temporary folder)")
    ap.add_argument("--keep", action="store_true", help="keep the corpus copies and transcripts")
    ap.add_argument("--dry-run", action="store_true")
    ap.add_argument("--yes-spend", action="store_true", help="every run is a paid agent run; required to start one")
    args = ap.parse_args()

    if not args.dry_run and not args.yes_spend:
        sys.exit("refusing to run: every run is a paid claude -p agent run. Pass --yes-spend, or --dry-run to see the plan.")
    if not args.dry_run and not args.login and not os.environ.get("ANTHROPIC_API_KEY"):
        sys.exit("--bare runs read only ANTHROPIC_API_KEY: set it, or pass --login to use the logged-in account")
    binary = os.path.abspath(args.scrutate)
    with open(args.tasks) as f:
        spec = json.load(f)
    tasks = [t for t in spec["tasks"] if (not args.only or t["id"] in args.only) and (args.only or args.split in ("all", t["split"]))]
    if not tasks:
        sys.exit("no task matches")
    arms = [a for a in args.arms.split(",") if a in ARMS]
    claude = claude_check()
    work = tempfile.mkdtemp(prefix="scrutate-agent-eval-")
    out_dir = args.out or os.path.join(work, "out")
    os.makedirs(out_dir, exist_ok=True)
    print(f"claude {claude['version']}; scrutate {binary}; work folder {work}", file=sys.stderr)

    runs = []
    for task in tasks:
        base = corpus_base(task["corpus"], spec["corpora"][task["corpus"]], work, args.corpora)
        for rep in range(args.repeats):
            for arm in arms:
                run_dir = prepare(task, arm, rep, base, work, binary)
                argv = command(task, arm, run_dir, args)
                run = {"task": task["id"], "split": task["split"], "arm": arm, "repeat": rep, "run_dir": run_dir}
                if args.dry_run:
                    plan = {**run, "argv": argv}
                    if arm == "mcp":
                        with open(os.path.join(run_dir, "mcp.json")) as f:
                            plan["mcp_config"] = json.load(f)
                        plan["roundtrip"] = mcp_roundtrip(os.path.join(run_dir, "mcp.json"))
                    print(json.dumps(plan, indent=1))
                    runs.append(run)
                    continue
                lines, wall, code = run_one(argv, run_dir, args.timeout)
                os.makedirs(os.path.join(out_dir, "transcripts"), exist_ok=True)
                shutil.copy(os.path.join(run_dir, "stream.jsonl"), os.path.join(out_dir, "transcripts", os.path.basename(run_dir) + ".jsonl"))
                parsed = parse_stream(lines)
                reply = (parsed["result"] or {}).get("result", "")
                run.update(check(task, reply), wall_s=wall, exit_code=code, metrics=metrics(parsed),
                           model=(parsed["init"] or {}).get("model"), problem=arm_problem(arm, parsed["init"]))
                runs.append(run)
                print(f"{task['id']} {arm} #{rep}: solved={run['solved']} cost={run['metrics']['cost_usd']} "
                      f"tools={run['metrics']['tool_calls']} {run['problem'] or ''}", file=sys.stderr)
    if args.dry_run:
        if not args.keep:
            shutil.rmtree(work)
        return
    result = {"date": datetime.datetime.now(datetime.timezone.utc).isoformat(timespec="seconds"), "claude": claude,
              "model": args.model, "scrutate": binary, "tasks_file": args.tasks, "runs": runs, "tool_usage": tool_usage(runs)}
    with open(os.path.join(out_dir, "results.json"), "w") as f:
        json.dump(result, f, indent=1)
    usage = result["tool_usage"]
    md = [f"# Agent eval, {result['date']}", "", f"claude {claude['version']}, model {args.model}", "", summary(runs), "", table(runs), "",
          f"Never called: {', '.join(usage['never_called']) or 'none'}", "",
          "Tools with failed calls: " + (", ".join(f"{t} ({u['errors']})" for t, u in usage["with_errors"].items()) or "none")]
    with open(os.path.join(out_dir, "results.md"), "w") as f:
        f.write("\n".join(md) + "\n")
    print(f"wrote {out_dir}/results.json and results.md", file=sys.stderr)
    if not args.keep:
        shutil.rmtree(os.path.join(work, "runs"), ignore_errors=True)
        shutil.rmtree(os.path.join(work, "base"), ignore_errors=True)


if __name__ == "__main__":
    main()
