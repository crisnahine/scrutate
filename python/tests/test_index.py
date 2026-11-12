import os
import time

from conftest import write_repo
from scrutate_ref.index import Index


def test_build_and_stats(repo):
    ix = Index(repo, model=None)
    st = ix.update()
    assert st.files_changed >= 4 and st.chunks > 10
    s = ix.stats()
    assert s["symbols"] >= 8 and s["call_edges"] > 0


def test_second_update_does_no_work(repo):
    ix = Index(repo, model=None)
    ix.update()
    st = ix.update()
    assert st.files_changed == 0 and st.files_removed == 0


def test_index_of_another_content_revision_is_read_again_once(repo):
    from scrutate_ref.index import CONTENT_REVISION
    ix = Index(repo, model=None)
    n = ix.update().files_changed
    assert ix.meta("content_rev") == str(CONTENT_REVISION)
    ix.set_meta("content_rev", "0")
    ix = Index(repo, model=None)
    assert ix.update().files_changed == n
    assert ix.meta("content_rev") == str(CONTENT_REVISION)
    assert ix.update().files_changed == 0


def test_touch_without_change_does_not_reindex(repo):
    ix = Index(repo, model=None)
    ix.update()
    p = os.path.join(repo, "app/auth.py")
    os.utime(p, (time.time() + 100, time.time() + 100))
    assert ix.update().files_changed == 0


def test_modified_file_is_reindexed_and_old_chunks_removed(repo):
    ix = Index(repo, model=None)
    ix.update()
    p = os.path.join(repo, "app/auth.py")
    with open(p, "a") as f:
        f.write("\n\ndef brand_new_function():\n    return 1\n")
    assert ix.update().files_changed == 1
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE name='brand_new_function'").fetchone()[0] == 1
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE name='refresh_token'").fetchone()[0] == 1  # no dupes


def test_deleted_file_is_removed(repo):
    ix = Index(repo, model=None)
    ix.update()
    os.remove(os.path.join(repo, "app/util.js"))
    st = ix.update()
    assert st.files_removed == 1
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE path='app/util.js'").fetchone()[0] == 0
    # nothing orphaned in refs
    assert ix.db.execute("SELECT COUNT(*) FROM refs WHERE chunk_id NOT IN (SELECT id FROM chunks)").fetchone()[0] == 0


def test_binary_and_huge_files_are_skipped_and_remembered(repo):
    with open(os.path.join(repo, "blob.txt"), "wb") as f:
        f.write(b"\x00\x01\x02" * 100)
    ix = Index(repo, model=None)
    ix.update()
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE path='blob.txt'").fetchone()[0] == 0
    assert ix.db.execute("SELECT hash FROM files WHERE path='blob.txt'").fetchone()[0] == "SKIP"
    assert ix.update().files_changed == 0


def test_force_rebuild(repo):
    ix = Index(repo, model=None)
    n = ix.update().chunks
    st = ix.update(force=True)
    assert st.files_changed > 0 and st.chunks == n


def test_parallel_and_serial_indexing_agree(tmp_path):
    files = {f"pkg/m{i}.py": f"def f{i}(x):\n    '''doc {i}'''\n    return g{i}(x)\n" for i in range(60)}
    a, b = tmp_path / "a", tmp_path / "b"
    write_repo(str(a), files)
    write_repo(str(b), files)
    ia, ib = Index(str(a), model=None), Index(str(b), model=None)
    ia.update(workers=1)
    ib.update(workers=2)
    q = "SELECT path, qualname, start, end FROM chunks ORDER BY path, start, qualname"
    assert [tuple(r) for r in ia.db.execute(q)] == [tuple(r) for r in ib.db.execute(q)]


def test_gitignore_is_respected(repo):
    import shutil
    if not shutil.which("rg"):
        return
    os.makedirs(os.path.join(repo, ".git"), exist_ok=True)  # rg only honors .gitignore inside a repo
    with open(os.path.join(repo, ".gitignore"), "w") as f:
        f.write("ignored/\n")
    write_repo(repo, {"ignored/x.py": "def hidden():\n    pass\n"})
    ix = Index(repo, model=None)
    ix.update()
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE name='hidden'").fetchone()[0] == 0


def test_index_from_older_layout_is_rebuilt_not_crashed(repo):
    """An index written by a previous version (other columns, other schema_version) is replaced."""
    import sqlite3

    os.makedirs(os.path.join(repo, ".scrutate"), exist_ok=True)
    con = sqlite3.connect(os.path.join(repo, ".scrutate", "index.db"))
    con.executescript("""
        CREATE TABLE meta(key TEXT PRIMARY KEY, value TEXT);
        INSERT INTO meta VALUES('schema_version','1');
        CREATE TABLE chunks(id INTEGER PRIMARY KEY, path TEXT, name TEXT);
        INSERT INTO chunks VALUES(1,'old.py','old_function');
    """)
    con.commit()
    con.close()
    ix = Index(repo, model=None)  # must not raise "no such column"
    assert ix.update().chunks > 10
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE name='old_function'").fetchone()[0] == 0


def test_damaged_index_file_is_replaced(repo):
    os.makedirs(os.path.join(repo, ".scrutate"), exist_ok=True)
    with open(os.path.join(repo, ".scrutate", "index.db"), "wb") as f:
        f.write(b"this is not a sqlite database" * 100)
    ix = Index(repo, model=None)
    assert ix.update().chunks > 10


def test_switching_semantic_model_rebuilds(repo):
    ix = Index(repo)  # default model
    ix.update()
    ix.db.close()
    ix2 = Index(repo, model=None)  # different model signature -> start over, no stale vectors
    st = ix2.update()
    assert st.files_changed >= 4


def test_store_always_agrees_with_the_chunk_table(repo):
    """Whatever happens to the files, the live rows the search engine scores are exactly the chunks in SQLite
    that ranked search sees (constants are symbols only)."""
    with open(os.path.join(repo, "app/auth.py"), "a") as f:
        f.write("\n\nMAX_RETRIES = 3\n")
    ix = Index(repo, model=None)
    ix.update()

    def check():
        assert ix.lex.n_alive == ix.db.execute("SELECT COUNT(*) FROM chunks WHERE kind != 'constant'").fetchone()[0]

    check()
    with open(os.path.join(repo, "app/auth.py"), "a") as f:
        f.write("\n\ndef brand_new_function():\n    return 1\n")
    ix.update()
    check()
    os.remove(os.path.join(repo, "app/util.js"))
    ix.update()
    check()
    with open(os.path.join(repo, "app/server.py"), "wb") as f:  # a source file that turns into a binary blob
        f.write(b"\x00\x01\x02" * 100)
    ix.update()
    check()
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE path='app/server.py'").fetchone()[0] == 0
    ix.update(force=True)
    check()


def test_a_second_process_sees_changes_made_by_the_first(repo):
    """Two Index objects on one directory (CLI + MCP server): the reader refreshes its statistics."""
    from scrutate_ref.search import QueryInfo, _lex_keys

    a = Index(repo, model=None)
    a.update()
    b = Index(repo, model=None)
    b.lex  # b now holds the statistics of the first state
    with open(os.path.join(repo, "app/auth.py"), "a") as f:
        f.write("\n\ndef another_one():\n    return 2\n")
    a.update()
    n = a.db.execute("SELECT COUNT(*) FROM chunks").fetchone()[0]
    assert b.lex.n_alive == a.lex.n_alive == n
    assert len(b.lex.search(_lex_keys(QueryInfo.parse("another_one")), 3)[0]) >= 1


def test_refresh_is_throttled_but_a_cheap_scan_is_not_held_back_for_long(repo):
    import time

    ix = Index(repo, model=None)
    assert ix.refresh(1e6) is not None  # first call always scans
    assert ix.refresh(1e6) is None  # immediately after: throttled
    with open(os.path.join(repo, "app/auth.py"), "a") as f:
        f.write("\n\ndef fresh_after_edit():\n    return 3\n")
    time.sleep(10 * ix._scan_cost + 0.05)  # far less than the 1e6 s requested interval
    st = ix.refresh(1e6)
    assert st is not None and st.files_changed == 1
    assert ix.refresh(0) is not None  # interval 0 means: always scan


def test_inside_git_the_ignore_rules_decide_for_build_dist_target_and_venv(tmp_path):
    import shutil

    import pytest
    from scrutate_ref.index import list_files

    if not shutil.which("rg"):
        pytest.skip("the ignore files are read through ripgrep")
    files = {"build/maker.py": "def tracked_builder():\n    pass\n", "target/gen.rs": "fn g() {}\n",
             "dist/out.py": "def x():\n    pass\n", "node_modules/m/i.js": "function m() {}\n",
             "src/a.py": "def a():\n    pass\n", ".gitignore": "dist/\n"}
    repo, plain = tmp_path / "repo", tmp_path / "plain"
    for root in (repo, plain):
        root.mkdir()
        write_repo(str(root), files)
    (repo / ".git").mkdir()
    assert list_files(str(repo)) == [".gitignore", "build/maker.py", "src/a.py", "target/gen.rs"]
    assert list_files(str(plain)) == [".gitignore", "src/a.py"]


def test_exclude_dirs_come_from_the_root_s_own_config_unless_the_variable_is_set(tmp_path, monkeypatch):
    from scrutate_ref.index import list_files
    for d in ("src", "gen", "tmp", "sub"):
        (tmp_path / d).mkdir()
        (tmp_path / d / "a.py").write_text("x = 1\n")
    monkeypatch.delenv("SCRUTATE_INDEX_EXCLUDE_DIRS", raising=False)
    assert list_files(str(tmp_path)) == ["gen/a.py", "src/a.py", "sub/a.py", "tmp/a.py"]
    # names only: one with a separator, `.` and `..` exclude nothing
    (tmp_path / ".scrutate.toml").write_text('[index]\nexclude_dirs = ["gen", " tmp ", "..", "sub/a.py", 7]\n')
    assert list_files(str(tmp_path)) == [".scrutate.toml", "src/a.py", "sub/a.py"]
    # a config below the root is not the index's
    (tmp_path / "sub" / ".scrutate.toml").write_text('[index]\nexclude_dirs = ["src"]\n')
    assert "src/a.py" in list_files(str(tmp_path))
    # the variable replaces the file's list, also when it is empty
    monkeypatch.setenv("SCRUTATE_INDEX_EXCLUDE_DIRS", "src")
    assert "gen/a.py" in list_files(str(tmp_path)) and "src/a.py" not in list_files(str(tmp_path))
    monkeypatch.setenv("SCRUTATE_INDEX_EXCLUDE_DIRS", "")
    assert {"gen/a.py", "tmp/a.py", "src/a.py"} <= set(list_files(str(tmp_path)))
    # a file that does not parse excludes nothing
    monkeypatch.delenv("SCRUTATE_INDEX_EXCLUDE_DIRS")
    (tmp_path / ".scrutate.toml").write_text("[index\n")
    assert {"gen/a.py", "tmp/a.py"} <= set(list_files(str(tmp_path)))


def test_an_escaped_comma_is_part_of_an_excluded_directory_name(tmp_path, monkeypatch):
    from scrutate_ref.index import list_files, split_names

    assert split_names(r"gen\,dir, vendor ,,a\b") == ["gen,dir", "vendor", r"a\b"]
    write_repo(str(tmp_path), {"gen,dir/g.py": "x = 1\n", "gen/h.py": "x = 1\n", "dir/i.py": "x = 1\n", "a.py": "x = 1\n"})
    monkeypatch.setenv("SCRUTATE_INDEX_EXCLUDE_DIRS", r"gen\,dir")
    assert list_files(str(tmp_path)) == ["a.py", "dir/i.py", "gen/h.py"]


def test_an_excluded_name_with_a_backslash_excludes_nothing(tmp_path, monkeypatch):
    from scrutate_ref.index import list_files

    files = {"ab/a.py": "x = 1\n", "src/a.py": "x = 1\n"}
    if os.sep == "/":
        files["a\\b/a.py"] = "x = 1\n"
    write_repo(str(tmp_path), files)
    monkeypatch.delenv("SCRUTATE_INDEX_EXCLUDE_DIRS", raising=False)
    (tmp_path / ".scrutate.toml").write_text('[index]\nexclude_dirs = ["a\\\\b", "x\\\\"]\n')
    want = sorted([".scrutate.toml", *files])
    assert list_files(str(tmp_path)) == want
    monkeypatch.setenv("SCRUTATE_INDEX_EXCLUDE_DIRS", r"a\b,x\ ")
    assert list_files(str(tmp_path)) == want


def test_the_root_config_is_read_through_a_link_inside_the_root_up_to_the_file_size_cap(tmp_path, monkeypatch):
    import pytest
    from scrutate_ref.index import config_excludes

    pytest.importorskip("tomllib")
    root, outside = tmp_path / "root", tmp_path / "outside"
    write_repo(str(root), {"conf/real.toml": '[index]\nexclude_dirs = ["gen"]\n'})
    write_repo(str(outside), {"real.toml": '[index]\nexclude_dirs = ["src"]\n'})
    toml = root / ".scrutate.toml"
    if hasattr(os, "symlink") and os.sep == "/":
        os.symlink("conf/real.toml", toml)
        assert config_excludes(str(root)) == ["gen"]
        toml.unlink()
        os.symlink(outside / "real.toml", toml)
        assert config_excludes(str(root)) == []
        toml.unlink()
    for size, want in ((1_000_000, ["gen"]), (1_000_001, [])):
        toml.write_bytes(b'[index]\nexclude_dirs = ["gen"]\n'.ljust(size, b"\n"))
        assert config_excludes(str(root)) == want, size


ENGINE = os.environ.get("SCRUTATE_BIN", os.path.join(os.path.dirname(__file__), "..", "..", "target", "release", "scrutate"))


def _mixed_tree(root):
    for rel in ["a.py", ".hidden.py", ".env.example", "Makefile", "LICENSE", "README", "x.unknownext", "x.PY",
                "notes.txt", "data.csv", "a.min.js", "img.png", "y.Lock", "Dockerfile", ".github/workflows/ci.yml",
                ".config/x.toml", "node_modules/m.js", "build/b.py", "dist/d.py", "target/t.rs", "venv/v.py",
                ".venv/v.py", "gen,dir/g.py", "skipme/s.py", "sub/by_dot_ignore.py", "sub/by_gitignore.py",
                "sub/kept.py", "deep/.git", "src/lib.rs"]:
        p = os.path.join(root, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write("x = 1\n")
    for rel, data in [(".gitignore", b"sub/by_gitignore.py\n"), (".ignore", b"sub/by_dot_ignore.py\n"),
                      ("big.py", b"x = 1\n" * 200000), ("bin.xyz", b"ab\x00cd")]:
        with open(os.path.join(root, rel), "wb") as f:
            f.write(data)
    os.symlink("a.py", os.path.join(root, "link.py"))


def test_the_reference_indexes_the_files_the_engine_indexes(tmp_path, monkeypatch):
    import shutil
    import sqlite3
    import subprocess

    import pytest

    if not os.path.exists(ENGINE):
        pytest.skip("no engine binary (cargo build --release -p scrutate, or SCRUTATE_BIN)")
    if not shutil.which("rg"):
        pytest.skip("the ignore files are read through ripgrep")
    monkeypatch.setenv("SCRUTATE_INDEX_EXCLUDE_DIRS", r"skipme,gen\,dir")
    monkeypatch.setenv("SCRUTATE_SEMANTIC", "none")
    for git in (False, True):
        root = tmp_path / ("git" if git else "plain")
        root.mkdir()
        _mixed_tree(str(root))
        if git:
            (root / ".git").mkdir()
        subprocess.run([ENGINE, "--root", str(root), "index"], check=True, capture_output=True)
        con = sqlite3.connect(str(root / ".scrutate" / "index.db"))
        engine = sorted(r[0] for r in con.execute("SELECT path FROM files"))
        con.close()
        ix = Index(str(root), model=None)
        ix.update()
        ref = sorted(r[0] for r in ix.db.execute("SELECT path FROM files"))
        assert ref == engine, (git, sorted(set(ref) ^ set(engine)))
        assert "bin.xyz" in ref and "big.py" in ref and "gen,dir/g.py" not in ref
