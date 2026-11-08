"""Tests for the PyO3 bindings (`pip install` the wheel from crates/scrutate-py first).

Skipped when the native module is not installed, so the pure-Python reference tests still run.
"""
import json
import os
import shutil
import subprocess
import tempfile
import threading
import time

import pytest

scrutate = pytest.importorskip("scrutate")

CLI = os.environ.get("SCRUTATE_BIN", os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "scrutate"))


@pytest.fixture()
def repo(tmp_path):
    (tmp_path / "pkg").mkdir()
    for i in range(20):
        (tmp_path / "pkg" / f"m{i}.py").write_text(
            f'"""Module {i}: helpers for widget {i}."""\n\nimport os\n\n\ndef make_widget_{i}(size):\n    """Build widget number {i}."""\n    return os.path.join("w", str(size))\n\n\ndef use_{i}():\n    return make_widget_{i}(1)\n'
        )
    (tmp_path / "README.md").write_text("# demo\nneedle in the docs\n")
    return tmp_path


def test_grep_streams_and_matches_cli(repo):
    it = scrutate.grep("import os", [str(repo)], mode="literal")
    first = next(it)
    assert isinstance(first, scrutate.Match)
    rest = list(it)
    got = sorted((m.path, m.line, m.text) for m in [first] + rest)
    assert len(got) == 20
    st = it.stats()
    assert st["matches"] == 20 and st["files_matched"] == 20 and not st["cancelled"]
    if os.path.exists(CLI):
        out = subprocess.run([CLI, "grep", "-F", "--json", "import os", "."], cwd=repo, capture_output=True, text=True).stdout
        cli = sorted((r["path"], r["line"], r["text"]) for r in map(json.loads, out.splitlines()) if r.get("type") == "match")
        assert cli == got


def test_grep_modes_and_errors(repo):
    assert len(list(scrutate.grep(r"def make_widget_\d+", [str(repo)]))) == 20
    # identifier mode: one spelling finds every casing of the whole identifier (def + call in m3.py)
    assert len(list(scrutate.grep("makeWidget3", [str(repo)], mode="identifier"))) == 2
    assert len(list(scrutate.grep("MAKE_WIDGET_3", [str(repo)], mode="identifier"))) == 2
    assert len(list(scrutate.grep("neddle", [str(repo)], mode="fuzzy", fuzzy_edits=1))) == 1
    with pytest.raises(ValueError):
        scrutate.grep("(unclosed", [str(repo)])
    with pytest.raises(ValueError):
        scrutate.grep("x", [str(repo)], mode="nope")


def test_grep_cancel_and_max_results(repo):
    it = scrutate.grep("e", [str(repo)])
    next(it)
    it.cancel()
    assert list(it) == []
    hits = list(scrutate.grep("e", [str(repo)], max_results=5))
    assert len(hits) <= 5


def test_repo_index_and_intelligence(repo):
    r = scrutate.Repo(str(repo))
    st = r.index()
    assert st["files_total"] == 21 and st["chunks"] > 40
    assert r.index()["files_changed"] == 0  # incremental: nothing to do
    hits = r.search("build a widget", k=5)
    assert hits and all(isinstance(h, scrutate.Hit) for h in hits)
    assert any(h.name.startswith("make_widget_") for h in hits)
    sym = r.symbol("make_widget_3")
    assert sym and sym[0].path == "pkg/m3.py"
    callers = r.callers("make_widget_3")
    assert [c.name for c in callers] == ["use_3"]
    res, ext = r.callees("use_3")
    assert [h.name for h in res] == ["make_widget_3"]
    outline = r.outline("pkg/m3.py")
    assert [o["kind"] for o in outline] == ["module", "function", "function"]
    deps = r.dependencies("pkg/m3.py")
    assert deps == [(3, "os", None)]
    assert len(r.dependents("os")) == 20
    sim = r.similar("make_widget_3", k=3)
    assert sim and all(-1.0 <= c <= 1.0 for _, c, _ in sim)
    dup = r.duplicates(min_tokens=3, threshold=0.5, limit=5)
    assert isinstance(dup, list)
    s = r.stats()
    assert s["files"] == 21 and s["symbols"] == 41  # 40 functions + the README heading
    assert "Module 3" in r.read("pkg/m3.py", 1, 1)
    assert r.read("../etc/passwd").startswith("error")


def test_gil_is_released_during_rust_work(repo):
    # a big enough corpus that the search time dominates thread start-up
    big = repo / "big.txt"
    big.write_text("".join(f"line {i} with some words to scan\n" for i in range(400_000)))

    def work():
        for _ in scrutate.grep("words to scan", [str(repo)], mode="literal", max_results=10**9):
            pass

    work()  # warm the cache
    t0 = time.perf_counter()
    for _ in range(4):
        work()
    seq = time.perf_counter() - t0
    threads = [threading.Thread(target=work) for _ in range(4)]
    t0 = time.perf_counter()
    for t in threads:
        t.start()
    for t in threads:
        t.join()
    par = time.perf_counter() - t0
    cpus = os.cpu_count() or 1
    load = os.getloadavg()[0] if hasattr(os, "getloadavg") else 0.0
    if cpus < 2 or load > cpus * 0.5:
        pytest.skip(f"needs idle cores to measure concurrency (cpus={cpus}, load={load:.1f}); sequential {seq:.3f}s parallel {par:.3f}s")
    assert par < seq * 0.9, f"threads did not run concurrently: sequential {seq:.3f}s parallel {par:.3f}s"
