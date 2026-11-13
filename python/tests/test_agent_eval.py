"""The agent eval scores answers, reads Claude Code's stream-json transcript and starts the MCP
server from the config it writes, all without an agent run."""
import json
import os
import shutil
import sys

import pytest

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import agent_eval as ae  # noqa: E402

# a stream-json transcript in the shape the Claude Code headless docs describe: system/init,
# assistant messages with tool_use blocks, user messages with tool_result blocks, a result last
STREAM = [
    {"type": "system", "subtype": "init", "session_id": "s1", "model": "claude-opus-5-5",
     "tools": ["Bash", "Read", "Grep", "Glob", "mcp__scrutate__find_symbol", "mcp__scrutate__callers", "mcp__scrutate__read_code"],
     "mcp_servers": [{"name": "scrutate", "status": "connected"}]},
    {"type": "assistant", "parent_tool_use_id": None, "session_id": "s1", "message": {"role": "assistant", "content": [
        {"type": "text", "text": "Looking for the definition."},
        {"type": "tool_use", "id": "t1", "name": "mcp__scrutate__find_symbol", "input": {"name": "inside"}}],
        "usage": {"input_tokens": 3, "output_tokens": 40}}},
    {"type": "user", "parent_tool_use_id": None, "session_id": "s1", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": "t1", "content": [{"type": "text", "text": "crates/scrutate/src/ops.rs:719-735  [function] inside"}]}]}},
    {"type": "assistant", "parent_tool_use_id": None, "session_id": "s1", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": "t2", "name": "mcp__scrutate__read_code", "input": {}},
        {"type": "tool_use", "id": "t3", "name": "mcp__scrutate__callers", "input": {"name": "crates/scrutate/src/ops.rs:719-735"}}]}},
    {"type": "user", "parent_tool_use_id": None, "session_id": "s1", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": "t2", "is_error": True, "content": "missing path or symbol"},
        {"type": "tool_result", "tool_use_id": "t3", "is_error": False, "content": "crates/scrutate/src/ops.rs:315-334  [function] Scope.files"}]}},
    {"type": "assistant", "parent_tool_use_id": "t9", "session_id": "s1", "message": {"role": "assistant", "content": [
        {"type": "tool_use", "id": "t4", "name": "Grep", "input": {"pattern": "inside\\("}}]}},
    {"type": "user", "parent_tool_use_id": "t9", "session_id": "s1", "message": {"role": "user", "content": [
        {"type": "tool_result", "tool_use_id": "t4", "content": "ops.rs:323"}]}},
    {"type": "result", "subtype": "success", "is_error": False, "duration_ms": 41250, "duration_api_ms": 38000, "num_turns": 6,
     "session_id": "s1", "total_cost_usd": 0.4123,
     "result": "It is `inside`.\nANSWER: {\"definition\": \"crates/scrutate/src/ops.rs:719\", \"callers\": [\"Scope::files\", \"run\", \"read_range\", \"repo_path\"]}",
     "usage": {"input_tokens": 41, "output_tokens": 2210, "cache_creation_input_tokens": 30512, "cache_read_input_tokens": 402113},
     "permission_denials": []},
]


def task(tid):
    with open(ae.TASKS) as f:
        return next(t for t in json.load(f)["tasks"] if t["id"] == tid)


def test_the_task_file_has_twenty_tasks_a_third_held_out_each_with_a_check_and_how_it_was_checked():
    with open(ae.TASKS) as f:
        spec = json.load(f)
    tasks = spec["tasks"]
    assert len(tasks) == 20 and len({t["id"] for t in tasks}) == 20
    assert sum(t["split"] == "heldout" for t in tasks) == 7
    for t in tasks:
        assert t["corpus"] in spec["corpora"] and t["how_checked"] and set(t["check"]) == set(t["answer_shape"]), t["id"]
        for c in t["check"].values():
            assert c["type"] in ("location", "names", "paths", "chain"), t["id"]
    for c in spec["corpora"].values():
        assert len(c["tree_sha256"]) == 64


def test_a_transcript_gives_tokens_cost_turns_and_every_tool_call_with_its_errors():
    parsed = ae.parse_stream(json.dumps(m) + "\n" for m in STREAM)
    m = ae.metrics(parsed)
    assert (m["input_tokens"], m["output_tokens"], m["cache_creation_input_tokens"], m["cache_read_input_tokens"]) == (41, 2210, 30512, 402113)
    assert (m["cost_usd"], m["turns"], m["duration_ms"], m["subtype"]) == (0.4123, 6, 41250, "success")
    assert m["tool_calls"] == 4
    assert m["tools"] == {"mcp__scrutate__find_symbol": 1, "mcp__scrutate__read_code": 1, "mcp__scrutate__callers": 1, "Grep": 1}
    assert m["tool_errors"] == [{"tool": "mcp__scrutate__read_code", "input": {}, "error": "missing path or symbol"}]
    assert [c["subagent"] for c in parsed["calls"]] == [False, False, False, True]
    assert ae.arm_problem("mcp", parsed["init"]) is None
    assert "MCP present" in ae.arm_problem("builtin", parsed["init"])


def test_a_transcript_cut_short_still_parses():
    lines = [json.dumps(m) for m in STREAM[:3]] + ["not json", ""]
    parsed = ae.parse_stream(lines)
    assert parsed["result"] is None and len(parsed["calls"]) == 1
    assert ae.metrics(parsed)["cost_usd"] is None
    assert ae.arm_problem("mcp", None) == "no init message"
    assert "not connected" in ae.arm_problem("mcp", {"mcp_servers": [{"name": "scrutate", "status": "failed"}]})


def test_tool_usage_names_the_tools_never_called_and_the_failed_calls():
    m = ae.metrics(ae.parse_stream(json.dumps(x) for x in STREAM))
    u = ae.tool_usage([{"arm": "mcp", "metrics": m}, {"arm": "builtin", "metrics": m}])
    assert u["per_tool"]["find_symbol"]["calls"] == 1 and u["per_tool"]["callers"]["calls"] == 1
    assert "find_symbol" not in u["never_called"] and "dead_code" in u["never_called"] and len(u["never_called"]) == 16
    assert u["with_errors"]["read_code"]["messages"] == [{"input": {}, "error": "missing path or symbol"}]


def test_the_answer_is_the_json_after_the_last_answer_line():
    assert ae.answer_of('ANSWER: {"a": 1}\nmore\nANSWER: `{"a": 2}`') == {"a": 2}
    assert ae.answer_of("no answer here") is None
    assert ae.answer_of("ANSWER: [1, 2]") is None
    assert ae.answer_of("ANSWER: {broken") is None
    # pretty-printed over several lines, in a fence, with a word after it
    assert ae.answer_of('Done.\nANSWER:\n```json\n{\n  "a": [1,\n 2]\n}\n```\nthanks') == {"a": [1, 2]}


def test_a_right_answer_passes_whatever_its_formatting():
    t = task("scrutate-inside-callers")
    reply = STREAM[-1]["result"]
    assert ae.check(t, reply)["solved"]
    for definition in ["./crates/scrutate/src/ops.rs:719-735", "crates\\scrutate\\src\\ops.rs:722", {"path": "crates/scrutate/src/ops.rs", "line": 717}]:
        ans = {"definition": definition, "callers": ["`Scope.files`", "ops::run()", "read_range", "crates/scrutate/src/ops.rs:856 repo_path"]}
        assert ae.check(t, "ANSWER: " + json.dumps(ans))["solved"], definition


def test_a_wrong_or_partial_answer_fails_the_field_it_gets_wrong():
    t = task("scrutate-inside-callers")
    namesake = {"definition": "crates/scrutate/src/watch.rs:37", "callers": ["files", "run", "read_range", "repo_path"]}
    r = ae.check(t, "ANSWER: " + json.dumps(namesake))
    assert not r["solved"] and r["fields"] == {"definition": False, "callers": True}
    extra = {"definition": "crates/scrutate/src/ops.rs:719", "callers": ["files", "run", "read_range", "repo_path", "note"]}
    assert ae.check(t, "ANSWER: " + json.dumps(extra))["fields"]["callers"] is False
    short = {"definition": "crates/scrutate/src/ops.rs:719", "callers": ["files", "run"]}
    assert ae.check(t, "ANSWER: " + json.dumps(short))["fields"]["callers"] is False
    assert ae.check(t, "ANSWER: " + json.dumps({"definition": "crates/scrutate/src/ops.rs:800"}))["fields"] == {"definition": False, "callers": False}
    assert not ae.check(t, "no answer")["solved"]


def test_a_name_with_alternatives_takes_either_and_a_path_set_ignores_order():
    t = task("networkx-dijkstra-wrapper")
    for nested in ["dist_path", "johnson"]:
        ans = {"wrapper": "_dijkstra", "callers": ["dijkstra_path_length", "dijkstra_predecessor_and_distance", nested]}
        assert ae.check(t, "ANSWER: " + json.dumps(ans))["solved"], nested
    assert not ae.check(t, "ANSWER: " + json.dumps({"wrapper": "_dijkstra", "callers": ["dijkstra_path_length"]}))["solved"]
    t = task("ripgrep-matcher-dependents")
    files = list(reversed(t["check"]["files"]["expected"]))
    assert ae.check(t, "ANSWER: " + json.dumps({"files": ["./" + f for f in files]}))["solved"]
    assert not ae.check(t, "ANSWER: " + json.dumps({"files": files + ["crates/matcher/Cargo.toml"]}))["solved"]


def test_a_chain_keeps_its_order_and_ends_and_lets_wrappers_in_between():
    t = task("scrutate-read-code-chain")
    want = t["check"]["chain"]["expected"]
    assert ae.check(t, "ANSWER: " + json.dumps({"chain": want}))["solved"]
    longer = ["run_call", "call_counted", "Session::run_counted", "waiting_for_others", "Repo::run_as_is", "recovering", "ops::run", "read_range"]
    assert ae.check(t, "ANSWER: " + json.dumps({"chain": longer}))["solved"]
    assert not ae.check(t, "ANSWER: " + json.dumps({"chain": [want[0], want[2], want[1], *want[3:]]}))["solved"]
    assert not ae.check(t, "ANSWER: " + json.dumps({"chain": want[1:]}))["solved"]


def test_the_two_arms_differ_only_in_the_mcp_config(tmp_path):
    class Args:
        model, max_budget_usd, login = "claude-opus-5-5", 1.0, False
    t = task("ripgrep-run-dispatch")
    mcp = ae.command(t, "mcp", str(tmp_path), Args)
    builtin = ae.command(t, "builtin", str(tmp_path), Args)
    assert "--strict-mcp-config" in mcp and "--strict-mcp-config" in builtin and "--bare" in mcp
    assert "--mcp-config" not in builtin and mcp[mcp.index("--mcp-config") + 1] == str(tmp_path / "mcp.json")
    allowed = "Read,Grep,Glob," + ae.READ_ONLY_BASH
    assert builtin[builtin.index("--allowedTools") + 1] == allowed and "Bash(rg:*)" in allowed
    assert [x for x in mcp if x not in builtin] == [allowed + ",mcp__scrutate", "--mcp-config", str(tmp_path / "mcp.json")]
    Args.login = True
    assert "--bare" not in ae.command(t, "builtin", str(tmp_path), Args)


def test_the_cost_estimate_grows_with_turns_and_with_the_tool_list():
    small = ae.cost_estimate("claude-opus-5-5", 10, 20000, 3000, 3000)
    assert 0 < small < ae.cost_estimate("claude-opus-5-5", 20, 20000, 3000, 3000)
    assert small < ae.cost_estimate("claude-opus-5-5", 10, 20000, 3000, 3000, mcp_tokens=4500)
    assert ae.cost_estimate("claude-sonnet-5-5", 10, 20000, 3000, 3000) < small


# read at import: bench/leaderboard.py, which other test files import, removes every SCRUTATE_ variable
BIN = os.environ.get("SCRUTATE_BIN")


@pytest.mark.skipif(not BIN, reason="needs SCRUTATE_BIN, a scrutate binary")
def test_the_server_starts_from_the_mcp_config_the_harness_writes(tmp_path):
    base = tmp_path / "base"
    (base / "pkg").mkdir(parents=True)
    (base / "pkg" / "m.py").write_text("def helper():\n    return 1\n\n\ndef main():\n    return helper()\n")
    t = {"id": "t", "question": "q", "answer_shape": {}}
    run_dir = ae.prepare(t, "mcp", 0, str(base), str(tmp_path), os.path.abspath(BIN))
    with open(os.path.join(run_dir, "mcp.json")) as f:
        cfg = json.load(f)
    assert cfg["mcpServers"]["scrutate"]["args"] == ["--root", os.path.join(run_dir, "repo"), "serve"]
    r = ae.mcp_roundtrip(os.path.join(run_dir, "mcp.json"))
    assert r["server"]["name"] == "scrutate" and r["call_ok"]
    assert sorted(r["tools"]) == sorted(ae.TOOLS)
    shutil.rmtree(run_dir)
