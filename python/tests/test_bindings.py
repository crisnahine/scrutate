"""Tests for the PyO3 bindings (`pip install` the wheel from crates/scrutate-py first).

Skipped when the native module is not installed, so the pure-Python reference tests still run.
"""
import json
import os
import signal
import subprocess
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
    # a binary the caller named must be there: a missing one fails, it does not skip the comparison
    if "SCRUTATE_BIN" in os.environ or os.path.exists(CLI):
        out = subprocess.run([os.path.abspath(CLI), "grep", "-F", "--json", "import os"], cwd=repo, capture_output=True, text=True, stdin=subprocess.DEVNULL).stdout
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
    with pytest.raises(FileNotFoundError):
        r.read("nope.py")
    with pytest.raises(ValueError):
        r.read("/etc/passwd")


def test_a_cut_answer_says_where_it_stopped(repo):
    (repo / "cut.py").write_text(
        "def target():\n    return 1\n\n\n"
        + "".join(f"def caller_{i}():\n    return target()\n\n\n" for i in range(3))
        + "def fan():\n    return caller_0() + caller_1() + caller_2()\n"
    )
    shapes = [
        "    total = compute_alpha(x)\n    shifted = adjust_beta(total)\n    return finalize_gamma(shifted)\n",
        "    items = load_records(x)\n    kept = filter_entries(items)\n    return store_results(kept)\n",
        "    conn = open_socket(x)\n    reply = send_request(conn)\n    return parse_payload(reply)\n",
    ]
    for i, body in enumerate(shapes):
        for copy in "ab":
            (repo / f"dup_{i}{copy}.py").write_text(f"def dup(x):\n{body}")
    r = scrutate.Repo(str(repo))
    r.index()
    calls = {
        "symbol": lambda n: r.symbol("dup", limit=n),
        "callers": lambda n: r.callers("target", limit=n),
        "callees": lambda n: r.callees("fan", limit=n)[0],
        "duplicates": lambda n: r.duplicates(min_tokens=3, threshold=0.95, limit=n),
    }
    def key(items):
        return [x if isinstance(x, dict) else (x.path, x.start) for x in items]

    for name, call in calls.items():
        whole = call(1000)
        assert isinstance(whole, list) and len(whole) >= 3 and whole.stopped_at is None, name
        cut = call(2)
        assert len(cut) == 2 and cut.stopped_at == 2, name
        assert key(cut) == key(whole[:2]), name
        exact = call(len(whole))
        assert key(exact) == key(whole) and exact.stopped_at is None, name


def test_similar_with_k_zero_leaves_the_repo_usable(repo):
    r = scrutate.Repo(str(repo))
    r.index()
    assert r.similar("make_widget_3", 0) == []
    assert r.symbol("make_widget_3")


def test_grep_argument_errors_at_call_time(repo):
    with pytest.raises(ValueError):
        scrutate.grep("x", [])
    with pytest.raises(ValueError):
        scrutate.grep("x", [str(repo)], globs=["a[b"])
    with pytest.raises(ValueError):
        scrutate.grep("x", [str(repo)], types=["no-such-type"])
    with pytest.raises(FileNotFoundError):
        scrutate.grep("x", [str(repo / "missing")])


def test_grep_sort_gives_path_and_line_order(repo):
    got = [(m.path, m.line) for m in scrutate.grep("e", [str(repo)], sort=True)]
    assert len(got) > 100 and got == sorted(got)


def _interrupt_latency(call, delay):
    """Seconds from a SIGINT sent `delay` s into `call()` to the KeyboardInterrupt; None if it finished first."""
    timer = threading.Timer(delay, os.kill, (os.getpid(), signal.SIGINT))
    t0 = time.perf_counter()
    timer.start()
    try:
        call()
    except KeyboardInterrupt:
        return time.perf_counter() - t0 - delay
    finally:
        timer.cancel()
    return None


@pytest.mark.skipif(not hasattr(signal, "SIGINT") or os.name == "nt", reason="needs POSIX signals")
def test_ctrl_c_interrupts_index_and_grep(tmp_path):
    body = "".join(f"def f_{i}_{{n}}(a, b):\n    return [a * {i}, b + {i}, str(a) + str(b)]\n\n" for i in range(60))
    for n in range(3000):  # about 2 s to index on an M-series laptop
        (tmp_path / f"m{n}.py").write_text(body.format(n=n))
    r = scrutate.Repo(str(tmp_path))
    late = _interrupt_latency(lambda: r.index(force=True), 0.2)
    if late is None:
        pytest.skip("index finished before the signal")
    assert late < 1.0, f"KeyboardInterrupt came {late:.2f}s after the signal"
    # the interrupted update stopped and was rolled back: the next call starts from the index as it was
    assert r.index()["files_changed"] == 3000
    assert r.symbol("f_3_7")
    # each line costs PCRE2 about 15 ms of backtracking: a few seconds in all
    slow = tmp_path / "slow"
    slow.mkdir()
    (slow / "s.txt").write_text("".join(f"line {i} with some words to scan\n" for i in range(200)))
    late = _interrupt_latency(lambda: list(scrutate.grep(r"(\w+\s?)*\d{12}$", [str(slow)], mode="pcre2", threads=1)), 0.3)
    if late is None:
        pytest.skip("grep finished before the signal")
    assert late < 1.0, f"KeyboardInterrupt came {late:.2f}s after the signal"


def test_gil_is_released_during_rust_work(repo):
    # one big file (searched by one thread, below the size where a file is split across threads)
    # and a single match, so the time is Rust scanning, not Python building result objects
    big = repo / "big.txt"
    big.write_text("".join(f"line {i} with some words to scan\n" for i in range(1_500_000)))

    def work():
        hits = list(scrutate.grep("line 1499999 with", [str(repo)], mode="literal"))
        assert len(hits) == 1

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
