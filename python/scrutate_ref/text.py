"""Tokenization shared by indexing, pretraining and querying.

Everything is lowercase, identifier-aware (camelCase / snake_case are split) and stemmed,
so `refreshTokens`, `refresh_token` and "refreshing tokens" all meet at `refresh`, `token`.
Compound identifiers also emit one joined token (`sendrequest`) for exact-name matching.
"""
from __future__ import annotations

import re

import snowballstemmer

_STEMMER = snowballstemmer.stemmer("english")

_WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")
_CAMEL = re.compile(r"[A-Z]+(?=[A-Z][a-z])|[A-Z]?[a-z]+|[A-Z]+")

# --- stop lists -------------------------------------------------------------------------
_ENGLISH = """a an the of to in on at by for with from as and or nor but so if then than that this
these those it its is are was were be been being am do does did done has have had having not no
yes can could should would will shall may might must i we you he she they them his her our your
their my me us about into onto over under up down out off also very just only more most such
there here when where which who whom whose why how what whatever any all each both either some
other another same own too via per etc eg ie e g""".split()
_CODE = """self cls this super none null nil true false def class import from return pass lambda
yield async await elif else try except finally raise with assert break continue global nonlocal
del var let const function new void typeof undefined public private protected static final
fn func pub mut impl struct enum trait use mod crate self_ int str bool float""".split()
# words that only matter in questions, never in code
_QUESTION = """find show give get where which implemented implement implementation handled
handle handles handling happens happen code codebase file files function functions method
methods class classes defined define used uses using use look looking search want need
please tell list""".split()

INDEX_STOP = frozenset(_ENGLISH) | frozenset(_CODE)
QUERY_STOP = INDEX_STOP | frozenset(_QUESTION)
STOPWORDS = QUERY_STOP  # public alias (benchmark uses it)

# --- caches -----------------------------------------------------------------------------
_stem_cache: dict[str, str] = {}
_ident_cache: dict[str, tuple[tuple[str, ...], str]] = {}


def stem(w: str) -> str:
    s = _stem_cache.get(w)
    if s is None:
        s = _stem_cache[w] = _STEMMER.stemWord(w)
    return s


def raw_subwords(ident: str) -> list[str]:
    """`parseHTTPResponse_v2` -> ['parse', 'http', 'response', 'v']  (lowercase, unstemmed)."""
    out: list[str] = []
    for part in ident.split("_"):
        if part:
            out.extend(m.group(0).lower() for m in _CAMEL.finditer(part))
    return out


def split_identifier(ident: str) -> list[str]:  # kept for callers that want plain splitting
    return raw_subwords(ident)


def ident_tokens(ident: str) -> tuple[tuple[str, ...], str]:
    """-> (stemmed subwords without stopwords, joined-whole-token or '')."""
    hit = _ident_cache.get(ident)
    if hit is not None:
        return hit
    subs = raw_subwords(ident)
    toks = tuple(stem(w) for w in subs if len(w) > 1 and w not in INDEX_STOP)
    whole = "".join(subs) if len(subs) > 1 and len(toks) > 0 else ""
    if len(_ident_cache) < 2_000_000:
        _ident_cache[ident] = (toks, whole)
    return toks, whole


def tokenize(text: str, cap: int | None = None, whole: bool = True) -> list[str]:
    """Stemmed subword tokens of free text or code (+ joined identifiers when `whole`)."""
    out: list[str] = []
    cache = _ident_cache
    n = 0
    for m in _WORD.finditer(text):
        w = m.group(0)
        hit = cache.get(w) or ident_tokens(w)
        out.extend(hit[0])
        if whole and hit[1]:
            out.append(hit[1])
        n += 1
        if cap is not None and n >= cap:
            break
    return out


def name_key(name: str) -> str:
    """Case- and underscore-insensitive key: `send_request`, `sendRequest` -> `sendrequest`."""
    return name.lower().replace("_", "")


BODY_CAP = 2000  # max identifiers of a body that are indexed / embedded


def tokenize2(text: str, cap: int | None = None) -> tuple[list[str], list[str]]:
    """-> (stemmed subwords, joined whole identifiers) in one pass over the text."""
    subs: list[str] = []
    wholes: list[str] = []
    cache = _ident_cache
    n = 0
    for m in _WORD.finditer(text):
        w = m.group(0)
        hit = cache.get(w) or ident_tokens(w)
        subs.extend(hit[0])
        if hit[1]:
            wholes.append(hit[1])
        n += 1
        if cap is not None and n >= cap:
            break
    return subs, wholes


_TEST_PARTS = {"test", "tests", "testing", "__tests__", "spec", "specs", "e2e", "fixtures", "mocks"}


def is_test_path(path: str) -> bool:
    parts = path.lower().replace("\\", "/").split("/")
    base = parts[-1]
    return (any(p in _TEST_PARTS for p in parts[:-1]) or base.startswith("test_")
            or base.endswith(("_test.py", "_test.go", ".test.js", ".test.ts", ".test.tsx",
                              ".spec.js", ".spec.ts", ".spec.tsx", "test.java")) or base == "conftest.py")


def tokenize_words(text: str, cap: int | None = None) -> list[str]:
    """Subwords only (no joined identifiers) -- used for the semantic model."""
    return tokenize(text, cap=cap, whole=False)


# --- query analysis ---------------------------------------------------------------------
def query_terms(query: str) -> list[str]:
    """Unique stemmed content terms of a query (question words and stopwords removed)."""
    seen: dict[str, None] = {}
    for m in _WORD.finditer(query):
        w = m.group(0)
        subs = raw_subwords(w)
        for s in subs:
            if len(s) > 1 and s not in QUERY_STOP:
                seen.setdefault(stem(s), None)
    return list(seen)


def query_wholes(query: str) -> list[str]:
    """Joined tokens for identifier-like words in the query (`send_request` -> `sendrequest`)."""
    out: dict[str, None] = {}
    for m in _WORD.finditer(query):
        w = m.group(0)
        subs = raw_subwords(w)
        if len(subs) > 1 and (("_" in w) or any(c.isupper() for c in w[1:])):
            out.setdefault("".join(subs), None)
    return list(out)


def literal_candidates(query: str) -> list[str]:
    """Pieces of the query that look like exact code strings worth a literal grep:
    quoted strings and identifier-ish words (snake_case, camelCase, dotted, CONSTANTS)."""
    lits: list[str] = []
    for m in re.finditer(r'"([^"]+)"|\'([^\']+)\'|`([^`]+)`', query):
        lits.append(next(g for g in m.groups() if g))
    stripped = re.sub(r'"[^"]*"|\'[^\']*\'|`[^`]*`', " ", query)
    for w in re.findall(r"[A-Za-z_][\w.]*[\w]", stripped):
        if (
            "_" in w
            or "." in w
            or (any(c.isupper() for c in w[1:]) and any(c.islower() for c in w))
            or (w.isupper() and len(w) > 2)
        ):
            lits.append(w)
    return list(dict.fromkeys(lits))


def split_lines(s: str) -> list[str]:
    """Lines as grep-class tools number them (Rust's ``str::lines``): split at "\\n", a "\\r"
    before it dropped, no empty last line for a trailing newline; form feeds and lone "\\r" are text."""
    parts = s.split("\n")
    last = parts.pop()
    lines = [p[:-1] if p.endswith("\r") else p for p in parts]
    if last:
        lines.append(last)
    return lines
