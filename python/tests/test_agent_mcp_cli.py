import asyncio
import os
import json

import pytest
from types import SimpleNamespace as NS

from scrutate_ref import agent
from scrutate_ref.cli import main
from scrutate_ref.index import Index
from scrutate_ref.search import Searcher


class FakeClient:
    """Scripted stand-in for the Anthropic client: one tool call, then a final answer."""

    def __init__(self):
        self.calls = []
        self.messages = self

    def create(self, **kw):
        self.calls.append(dict(kw, messages=list(kw["messages"])))  # snapshot: the agent keeps appending
        if len(self.calls) == 1:
            return NS(content=[NS(type="tool_use", id="t1", name="search",
                                  input={"query": "delete a tool", "alt_queries": ["remove_tool"]})])
        return NS(content=[NS(type="text", text="It is Server.remove_tool in app/server.py.")])


def test_agent_loop_runs_tools_and_returns_the_answer(repo):
    fake = FakeClient()
    out = agent.ask(repo, "where are tools deleted?", client=fake)
    assert "remove_tool" in out
    # the tool result was threaded back to the model as a tool_result block
    last = fake.calls[1]["messages"][-1]
    assert last["role"] == "user" and last["content"][0]["type"] == "tool_result"
    assert "Server.remove_tool" in last["content"][0]["content"]


def test_run_tool_handles_errors_and_unknown_tools(repo):
    ix = Index(repo, model=None)
    ix.update()
    s = Searcher(ix, ranker=None)
    assert agent.run_tool(s, "nope", {}).startswith("error")
    assert agent.run_tool(s, "read_file", {"path": "../x"}).startswith("error")
    assert "Server.add_tool" in agent.run_tool(s, "find_symbol", {"name": "add_tool"})


def _build(*a, **k):
    pytest.importorskip("mcp")  # optional extra: pip install "scrutate-ref[mcp]"
    from scrutate_ref.mcp_server import build

    return build(*a, **k)


def test_mcp_server_exposes_working_tools(repo):
    mcp = _build(repo)
    tools = {t.name for t in asyncio.run(mcp.list_tools())}
    assert {"search_code", "grep_code", "find_symbol", "callers", "callees", "read_code", "reindex"} <= tools
    res = asyncio.run(mcp.call_tool("search_code", {"query": "send_ping", "k": 2}))
    text = json.dumps(res, default=str)
    assert "send_ping" in text


def test_cli_search_and_json(repo, capsys):
    main(["--root", repo, "search", "delete a tool from the server", "-k", "3"])
    assert "Server.remove_tool" in capsys.readouterr().out
    main(["--root", repo, "search", "send_ping", "--json", "-k", "1"])
    data = json.loads(capsys.readouterr().out)
    assert data[0]["name"] == "send_ping" and data[0]["path"] == "app/server.py"


def test_cli_symbol_callers_grep(repo, capsys):
    main(["--root", repo, "symbol", "calculate_token_expiry"])
    assert "app/auth.py" in capsys.readouterr().out
    main(["--root", repo, "callers", "refresh_token"])
    assert "Server.handle" in capsys.readouterr().out
    main(["--root", repo, "grep", "def remove_tool"])
    assert "Server.remove_tool" in capsys.readouterr().out


def test_mcp_search_sees_files_edited_after_the_server_started(repo):
    mcp = _build(repo, refresh_interval=0)
    asyncio.run(mcp.call_tool("search_code", {"query": "send_ping", "k": 1}))
    with open(os.path.join(repo, "app", "late.py"), "w") as f:
        f.write("def zebra_crossing_checker(x):\n    '''Decide whether a zebra may cross.'''\n    return x\n")
    res = asyncio.run(mcp.call_tool("search_code", {"query": "zebra_crossing_checker", "k": 2}))
    assert "zebra_crossing_checker" in json.dumps(res, default=str)


def test_agent_tools_refresh_the_index_too(repo):
    from scrutate_ref.agent import run_tool

    ix = Index(repo)
    ix.update()
    s = Searcher(ix)
    with open(os.path.join(repo, "app", "late2.py"), "w") as f:
        f.write("def quokka_counter():\n    return 3\n")
    ix._last_refresh = 0.0
    assert "quokka_counter" in run_tool(s, "find_symbol", {"name": "quokka_counter"})


def test_read_refuses_paths_that_leave_the_repo(tmp_path):
    from conftest import write_repo

    root = tmp_path / "repo"
    root.mkdir()
    write_repo(str(root))
    secrets = tmp_path / "repo-secrets"
    secrets.mkdir()
    (secrets / ".env").write_text("TOKEN=hunter2\n")
    os.symlink(secrets / ".env", root / "link.env")
    ix = Index(str(root), model=None)
    ix.update()
    s = Searcher(ix, ranker=None)
    assert s.read("../repo-secrets/.env").startswith("error")
    assert s.read("link.env").startswith("error")
    assert "TokenStore" in s.read("app/auth.py")
