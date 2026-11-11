"""Tokenization shared by indexing, pretraining and querying.

Everything is lowercase, identifier-aware (camelCase / snake_case are split) and stemmed,
so `refreshTokens`, `refresh_token` and "refreshing tokens" all meet at `refresh`, `token`.
Compound identifiers also emit one joined token (`sendrequest`) for exact-name matching.
"""
from __future__ import annotations

import functools
import itertools
import os
import re
import unicodedata

import snowballstemmer

_STEMMER = snowballstemmer.stemmer("english")

_WORD = re.compile(r"[A-Za-z_][A-Za-z0-9_]*")


@functools.cache
def _run_re() -> re.Pattern:
    """Maximal runs of letters, marks, decimal digits and `_` (Rust: `[\\p{L}\\p{M}\\p{Nd}_]+`)."""
    ranges, start = [], None
    for cp in range(0x110001):
        cat = unicodedata.category(chr(cp)) if cp < 0x110000 else "Cn"
        ok = cat[0] in "LM" or cat == "Nd" or cp == 0x5F
        if ok and start is None:
            start = cp
        elif not ok and start is not None:
            ranges.append(f"{re.escape(chr(start))}-{re.escape(chr(cp - 1))}")
            start = None
    return re.compile("[" + "".join(ranges) + "]+")


def words(text: str) -> list[str]:
    """Words of a text: `_WORD` for an all-ASCII text; otherwise runs of every script, an all-ASCII
    run losing its leading digits so ASCII words come out as they always did."""
    if text.isascii():
        return _WORD.findall(text)
    out = []
    for w in _run_re().findall(text):
        if w.isascii():
            w = w.lstrip("0123456789")
            if not w:
                continue
        out.append(w)
    return out


def _unspaced(c: str) -> bool:
    """Scripts written without spaces between words: Han, Hiragana, Katakana, and Thai, Lao,
    Tibetan, Myanmar, Khmer, the Tai scripts, Balinese and Javanese."""
    o = ord(c)
    return (0x0E00 <= o <= 0x109F or 0x1780 <= o <= 0x17FF or 0x1950 <= o <= 0x19DF or 0x1A20 <= o <= 0x1AAF
            or 0x1B00 <= o <= 0x1B7F or 0xA980 <= o <= 0xA9FF or 0xAA60 <= o <= 0xAADF or 0x3005 <= o <= 0x3007 or 0x3040 <= o <= 0x30FF or 0x31F0 <= o <= 0x31FF
            or 0x3400 <= o <= 0x4DBF or 0x4E00 <= o <= 0x9FFF or 0xF900 <= o <= 0xFAFF or 0xFF66 <= o <= 0xFF9F
            or 0x20000 <= o <= 0x323AF)


def _fold(s: str) -> str:
    return s.lower().replace("ς", "σ")


def han_fold_path() -> str:
    """The traditional-to-simplified Han table the Rust engine embeds (tools/gen_han_fold.py)."""
    return os.path.join(os.path.dirname(__file__), "..", "..", "crates", "scrutate", "data", "han_fold.txt")


@functools.cache
def _han_fold() -> dict[int, str]:
    with open(han_fold_path(), encoding="utf-8") as f:
        pairs = "".join(line.rstrip("\n") for line in f if not line.startswith("#"))
    return {ord(t): s for t, s in zip(pairs[0::2], pairs[1::2])}


def unicode_tokens(word: str) -> list[str]:
    """Tokens of a word that is not all ASCII: NFC first; runs of unspaced scripts have traditional
    Han folded to simplified and give overlapping character bigrams (a lone character itself),
    other `_`-separated parts are one folded token each, and an ASCII part goes through the
    identifier rules (split, stemmed)."""
    out: list[str] = []
    for u, grp in itertools.groupby(unicodedata.normalize("NFC", word), _unspaced):
        seg = "".join(grp)
        if not u:
            for part in seg.split("_"):
                if part:
                    out.extend(ident_tokens(part)[0] if part.isascii() else (_fold(part),))
            continue
        seg = seg.translate(_han_fold())
        if len(seg) == 1:
            out.append(seg)
        else:
            out.extend(seg[i:i + 2] for i in range(len(seg) - 1))
    return out
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
    """Case- and underscore-insensitive key in NFC: `send_request`, `sendRequest` -> `sendrequest`."""
    if name.isascii():
        return name.lower().replace("_", "")
    return _fold(unicodedata.normalize("NFC", name).replace("_", ""))


BODY_CAP = 2000  # max identifiers of a body that are indexed / embedded


def tokenize2(text: str, cap: int | None = None) -> tuple[list[str], list[str]]:
    """-> (stemmed subwords, joined whole identifiers) in one pass over the text."""
    subs: list[str] = []
    wholes: list[str] = []
    cache = _ident_cache
    n = 0
    for w in (_WORD.findall(text) if text.isascii() else words(text)):
        if not w.isascii():
            subs.extend(unicode_tokens(w))
        else:
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
    for w in words(query):
        if not w.isascii():
            for t in unicode_tokens(w):
                seen.setdefault(t, None)
            continue
        subs = raw_subwords(w)
        for s in subs:
            if len(s) > 1 and s not in QUERY_STOP:
                seen.setdefault(stem(s), None)
    return list(seen)


def query_wholes(query: str) -> list[str]:
    """Joined tokens for identifier-like words in the query (`send_request` -> `sendrequest`)."""
    out: dict[str, None] = {}
    for w in words(query):
        if not w.isascii():
            continue
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
