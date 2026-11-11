from scrutate_ref.index import Index
from scrutate_ref.search import FEATURES, Searcher


def make(repo):
    ix = Index(repo, model=None)
    ix.update()
    return Searcher(ix, ranker=None)


def names(hits):
    return [h.qualname or h.name for h in hits]


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
