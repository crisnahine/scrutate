from scrutate_ref.index import Index
from scrutate_ref.search import FEATURES, Searcher


def make(repo):
    ix = Index(repo, model=None)
    ix.update()
    return Searcher(ix, ranker=None)


def names(hits):
    return [h.qualname or h.name for h in hits]


def test_constants_are_found_by_name_but_never_ranked(repo):
    import os

    with open(os.path.join(repo, "app/auth.py"), "a") as f:
        f.write("\n\nMAX_RETRIES = 3\n")
    s = make(repo)
    assert [(h.kind, h.qualname) for h in s.find_symbol("MAX_RETRIES")] == [("constant", "MAX_RETRIES")]
    for q in ("MAX_RETRIES", "max retries", "TokenStore.MAX_RETRIES"):
        _, rows, _ = s.candidates(q)
        assert all(r["kind"] != "constant" for r in rows), q


def test_exact_identifier(repo):
    s = make(repo)
    assert names(s.search("send_request", k=3))[0] == "send_request"
    assert names(s.search("sendRequest", k=3))[0] == "send_request"  # naming-convention insensitive


def test_qualified_name(repo):
    s = make(repo)
    assert names(s.search("TokenStore.refresh_token", k=3))[0] == "TokenStore.refresh_token"


def test_natural_language_finds_the_function(repo):
    s = make(repo)
    assert "Server.remove_tool" in names(s.search("delete a tool from the server by name", k=3))
    assert names(s.search("work out when a token expires", k=3))[0] == "calculate_token_expiry"
    assert names(s.search("refresh expired access token", k=3))[0] == "TokenStore.refresh_token"


def test_non_python_languages_are_searchable(repo):
    s = make(repo)
    assert "formatName" in names(s.search("format a user name for display", k=3))


def test_tests_are_not_preferred_over_implementation(repo):
    s = make(repo)
    top = s.search("calculate token expiry", k=3)
    assert top[0].path == "app/auth.py"


def test_search_many_fuses_phrasings(repo):
    s = make(repo)
    hits = s.search_many(["liveness check", "ping the peer", "send_ping"], k=3)
    assert names(hits)[0] == "send_ping"


def test_find_symbol_callers_callees(repo):
    s = make(repo)
    assert names(s.find_symbol("remove_tool")) == ["Server.remove_tool"]
    assert names(s.find_symbol("Server.add_tool")) == ["Server.add_tool"]
    assert "Server.handle" in names(s.callers("refresh_token"))
    resolved, external = s.callees("Server.handle")
    assert "send_request" in names(resolved) and "TokenStore" in names(resolved)


def test_grep_maps_lines_to_symbols(repo):
    s = make(repo)
    assert "Server.remove_tool" in names(s.grep(r"del self\.tools"))


def test_read_is_sandboxed(repo):
    s = make(repo)
    assert "calculate_token_expiry" in s.read("app/auth.py", 20, 30)
    assert s.read("../../etc/passwd").startswith("error")


def test_empty_and_garbage_queries_do_not_crash(repo):
    s = make(repo)
    assert s.search("", k=3) == []
    assert isinstance(s.search("!!! ??? ***", k=3), list)
    assert isinstance(s.search('"unbalanced quote OR AND NOT', k=3), list)


def test_candidates_feature_matrix_shape(repo):
    s = make(repo)
    q, rows, X = s.candidates("refresh token")
    assert X.shape == (len(rows), len(FEATURES)) and len(rows) > 0


def test_results_have_text_and_explanations(repo):
    s = make(repo)
    h = s.search("send_ping", k=1)[0]
    assert "def send_ping" in h.text and h.layers


def test_index_directory_is_never_indexed(repo):
    s = make(repo)
    s.ix.update(force=True)
    assert s.ix.db.execute("SELECT COUNT(*) FROM chunks WHERE path LIKE '.scrutate%'").fetchone()[0] == 0


def test_works_without_ripgrep(repo, monkeypatch):
    import scrutate_ref.search as ts
    monkeypatch.setattr(ts.shutil, "which", lambda name: None)
    s = make(repo)
    assert "Server.remove_tool" in names(s.grep(r"del self\.tools"))
    assert names(s.search('"/token"', k=3))[0] == "TokenStore._post" or "TokenStore.refresh_token" in names(s.search('"/token"', k=3))
    assert names(s.search("send_ping", k=1)) == ["send_ping"]


def test_candidate_cut_keeps_the_lowest_indices_on_tied_scores():
    import numpy as np
    from scrutate_ref.search import prune_indices

    assert sorted(prune_indices(np.ones(300, np.float32), 200)) == list(range(200))
    lin = np.full(10, 0.5, np.float32)
    lin[7] = 2.0
    assert sorted(prune_indices(lin, 3)) == [0, 1, 7]


def _tree(root, files):
    import os

    for rel, body in files.items():
        p = os.path.join(root, rel)
        os.makedirs(os.path.dirname(p), exist_ok=True)
        with open(p, "w") as f:
            f.write(body)
    return make(root)


def test_find_symbol_puts_the_name_as_typed_before_thirty_case_folded_namesakes(tmp_path):
    files = {f"src/m{i:02}.rs": "pub fn error() -> u8 { 1 }\n" for i in range(30)}
    files["src/zz.rs"] = "pub struct Error { pub code: u8 }\n"
    s = _tree(str(tmp_path), files)
    got = [(h.kind, h.qualname) for h in s.find_symbol("Error", limit=20)]
    assert len(got) == 20 and got[0] == ("struct", "Error")
    assert all(g == ("function", "error") for g in got[1:])
    lower = [h.qualname for h in s.find_symbol("error", limit=40)]
    assert lower == ["error"] * 30 + ["Error"]


def test_find_symbol_puts_a_constant_before_a_method_whose_name_differs_in_case(tmp_path):
    java = "public class Config {\n    public static final int MAX = 5;\n    public int max() { return MAX; }\n}\n"
    s = _tree(str(tmp_path), {"Config.java": java})
    want = [("constant", "Config.MAX"), ("method", "Config.max")]
    assert [(h.kind, h.qualname) for h in s.find_symbol("MAX")] == want
    assert [(h.kind, h.qualname) for h in s.find_symbol("Config.MAX")] == want
    assert [(h.kind, h.qualname) for h in s.find_symbol("max")] == want[::-1]


def test_find_symbol_puts_a_struct_before_a_function_of_the_same_name(tmp_path):
    s = _tree(str(tmp_path), {"a.rs": "pub fn Point() -> u8 { 1 }\n", "b.rs": "pub struct Point { pub x: u8 }\n"})
    assert [(h.kind, h.path) for h in s.find_symbol("Point")] == [("struct", "b.rs"), ("function", "a.rs")]


def test_find_symbol_puts_test_files_last_within_the_exact_and_the_folded_answers(tmp_path):
    s = _tree(str(tmp_path), {"tests/t.rs": "pub struct Error;\npub fn error() {}\n", "src/a.rs": "pub fn error() {}\n",
                              "src/b.rs": "pub enum Error { A }\n"})
    assert [(h.qualname, h.path) for h in s.find_symbol("Error")] == [
        ("Error", "src/b.rs"), ("Error", "tests/t.rs"), ("error", "src/a.rs"), ("error", "tests/t.rs")]


def test_find_symbol_puts_a_definition_before_its_prototype(tmp_path):
    trait = "pub trait Run {\n    fn run(&self);\n}\n"
    body = "impl Run for u8 {\n    fn run(&self) { let total = self.count(); println!(\"{}\", total); }\n}\n"
    s = _tree(str(tmp_path), {"a.rs": trait, "b.rs": body})
    # the trait `Run` matches only after case folding
    assert [(h.path, h.name) for h in s.find_symbol("run")] == [("b.rs", "run"), ("a.rs", "run"), ("a.rs", "Run")]


BURST_RS = ("/// Keeps the files of one burst.\npub struct BurstFiles {\n    n: usize,\n}\n\nimpl BurstFiles {\n"
            "    /// Collects the changed files of a burst into one list.\n    pub fn collect(&self) -> usize {\n"
            "        self.n\n    }\n}\n\n/// The files of one burst.\npub fn burst_files() -> usize {\n    0\n}\n")


def test_a_name_as_typed_is_a_feature_and_an_impl_is_its_own_hit(tmp_path):
    from scrutate_ref.search import FI

    (tmp_path / "src").mkdir()
    (tmp_path / "src" / "lib.rs").write_text(BURST_RS)
    s = make(str(tmp_path))

    def get(q, kind, name, feat):
        _, rows, X = s.candidates(q)
        i = next(i for i, r in enumerate(rows) if r["kind"] == kind and r["name"] == name)
        return float(X[i, FI[feat]])

    assert get("BurstFiles", "struct", "BurstFiles", "exact_case") == 1.0
    assert get("BurstFiles", "function", "burst_files", "exact_case") == 0.0
    # typed in a style neither is written in: both are as near as it gets
    assert get("burstFiles", "struct", "BurstFiles", "exact_case") == 1.0
    assert get("burstFiles", "function", "burst_files", "exact_case") == 1.0
    # the struct and its impl block share a qualified name and are still two hits
    kinds = [h.kind for h in s.search("BurstFiles", k=10, with_text=False) if h.name == "BurstFiles"]
    assert "struct" in kinds and "impl" in kinds, kinds
    # the same name in another Unicode form is the name as typed
    (tmp_path / "nfc").mkdir()
    (tmp_path / "nfc" / "a.py").write_text("def caf\u00e9():\n    return 1\n\n\ndef cafe\u0301_x():\n    return 2\n")
    s = make(str(tmp_path / "nfc"))
    assert get("cafe\u0301", "function", "caf\u00e9", "exact_case") == 1.0
    assert get("cafe\u0301", "function", "cafe\u0301_x", "exact_case") == 0.0
