from scrutate_ref.text import (is_test_path, ident_tokens, literal_candidates, name_key, query_terms,
                        query_wholes, tokenize, tokenize2)


def test_identifier_splitting_and_stemming():
    toks = tokenize("refreshTokens send_request ClientSession")
    assert {"refresh", "token", "send", "request", "client", "session"} <= set(toks)
    assert "sendrequest" in toks and "clientsession" in toks  # joined whole identifiers


def test_prose_and_code_meet_at_the_same_stem():
    assert set(tokenize("refreshing tokens", whole=False)) == set(tokenize("refresh_token", whole=False))


def test_stopwords_and_digits_are_dropped():
    assert "the" not in tokenize("the self return none")
    assert tokenize("v2", whole=False) == []  # 'v' (1 char) and '2' are noise


def test_tokenize2_separates_wholes():
    subs, wholes = tokenize2("def send_request(self):")
    assert "send" in subs and "request" in subs
    assert wholes == ["sendrequest"]


def test_query_terms_removes_question_words():
    assert query_terms("where is the oauth token refreshed?") == ["oauth", "token", "refresh"]


def test_query_wholes_only_for_identifier_like_words():
    assert query_wholes("call send_request now") == ["sendrequest"]
    assert query_wholes("send request") == []


def test_literal_candidates():
    lits = literal_candidates('find "No refresh token" and ClientSession.call_tool plus MAX_SIZE')
    assert "No refresh token" in lits and "ClientSession.call_tool" in lits and "MAX_SIZE" in lits
    assert literal_candidates("plain words only") == []


def test_name_key_is_case_and_underscore_insensitive():
    assert name_key("send_request") == name_key("sendRequest") == "sendrequest"


def test_ident_tokens_cache_is_consistent():
    assert ident_tokens("parseHTTPResponse") == ident_tokens("parseHTTPResponse")


def test_is_test_path():
    assert is_test_path("pkg/tests/test_a.py") and is_test_path("a/b_test.go") and is_test_path("conftest.py")
    assert not is_test_path("pkg/mod.py") and not is_test_path("src/latest.py")
