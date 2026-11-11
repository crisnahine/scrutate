"""Hybrid retrieval.

Candidates come from three sources -- field-weighted BM25 over the segment store (lex.py),
semantic vectors, and exact name / qualified-name lookup -- and are scored by a learned ranker
over 47 features (per-field BM25, semantic similarities, exactness, literal occurrences in the
candidate text, structure). Without a trained ranker a hand-set linear blend is used.
"""
from __future__ import annotations

import math
import os
import re
import shutil
import subprocess
from collections import defaultdict
from dataclasses import dataclass, field

import numpy as np

from .index import Index, unpack_text
from .lex import tok_key
from .rank import TreeEnsemble, default_ranker_path
from .text import QUERY_STOP, literal_candidates, name_key, query_terms, query_wholes, raw_subwords, split_lines

PRUNE = 200  # the learned ranker sees (at most) this many candidates, best by the linear blend
LEX_LIMIT = 100
SEM_LIMIT = 100
CODE_KINDS = ("function", "method", "class", "struct", "trait", "interface", "type", "enum", "impl")

FEATURES = [
    "lex_log", "lex_ratio", "lex_rank", "lex_z",
    "name_log", "name_ratio", "qual_log", "qual_ratio", "sig_log", "sig_ratio",
    "doc_log", "doc_ratio", "body_log", "body_ratio", "path_log", "path_ratio",
    "sem_a", "sem_b", "sem_mix", "sem_a_z", "sem_b_z", "sem_mix_z", "sem_rank",
    "exact_name", "exact_qual", "name_overlap", "name_cover", "name_same_set",
    "lit_hits", "lit_def",
    "k_function", "k_method", "k_class", "k_module", "k_other",
    "is_test", "log_nbody", "has_doc", "log_ndoc", "qual_depth", "is_private", "log_ncallers",
    "n_terms", "n_lits", "n_words", "has_ident", "file_frac",
]
FI = {n: i for i, n in enumerate(FEATURES)}


@dataclass
class Hit:
    id: int
    path: str
    start: int
    end: int
    name: str
    qualname: str
    kind: str
    text: str = ""
    score: float = 0.0
    layers: dict = field(default_factory=dict)

    @property
    def loc(self) -> str:
        return f"{self.path}:{self.start}-{self.end}"

    def snippet(self, words: list[str] | None = None, max_lines: int = 8) -> str:
        lines = self.text.split("\n")
        at = 0
        if words:
            low = [w.lower() for w in words]
            for i, ln in enumerate(lines):
                l = ln.lower()
                if any(w in l for w in low):
                    at = max(0, i - 1)
                    break
        return "\n".join(lines[at : at + max_lines])


@dataclass
class QueryInfo:
    text: str
    terms: list[str]
    wholes: list[str]
    lits: list[str]
    words: list[str]  # raw lowercase words (for snippets / exact lookups)
    keys: list[str]  # underscore-insensitive keys for exact name lookup
    dotted: list[str]  # Class.method style keys

    @classmethod
    def parse(cls, text: str) -> "QueryInfo":
        words = [w.lower() for w in re.findall(r"[A-Za-z_][A-Za-z0-9_]*", text)]
        keys = list(dict.fromkeys(name_key(w) for w in words if len(w) >= 3 and w not in QUERY_STOP))
        dotted = [".".join(name_key(p) for p in d.split("."))
                  for d in re.findall(r"[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)+", text)]
        return cls(text, query_terms(text), query_wholes(text), literal_candidates(text), words, keys,
                   list(dict.fromkeys(dotted)))


def _lex_keys(q: QueryInfo) -> list[int]:
    toks = dict.fromkeys(q.terms + q.wholes)
    return [tok_key(t) for t in toks if re.fullmatch(r"[a-z0-9]+", t)]


class Searcher:
    def __init__(self, index: Index, ranker: TreeEnsemble | None | str = "default"):
        self.ix = index
        if ranker == "default":
            p = os.environ.get("SCRUTATE_RANKER", default_ranker_path())
            ranker = TreeEnsemble.load(p) if os.path.exists(p) else None
        self.ranker: TreeEnsemble | None = ranker
        if self.ranker is not None and self.ranker.feature_names != FEATURES:
            self.ranker = None  # trained for a different feature set
        self._ver = None
        self._callers: dict[str, int] = {}

    # ------------------------------------------------------------------ plumbing
    def _sync(self):
        """Segment store for the current data version (caches that depend on the data are reset)."""
        store = self.ix.lex
        ver = self.ix.meta("data_version")
        if ver != self._ver:
            self._ver = ver
            self._callers = {}
        return store

    def _lex(self, q: QueryInfo, limit: int = LEX_LIMIT) -> list[int]:
        keys = _lex_keys(q)
        if not keys:
            return []
        return self._store.search(keys, limit)[0].tolist()

    def _semantic(self, q: QueryInfo, limit: int = SEM_LIMIT):
        """-> (top ids, qz) ; qz None when no semantic model / no known words."""
        if self.ix.model is None or not q.terms:
            return [], None
        qz = self.ix.model.encode_query(q.terms)
        if qz is None:
            return [], None
        return self._store.semantic_top(qz, limit).tolist(), qz

    def _exact(self, q: QueryInfo, limit: int = 60) -> list[int]:
        out: list[int] = []
        db = self.ix.db
        if q.keys:
            ph = ",".join("?" * len(q.keys))
            out += [r[0] for r in db.execute(
                f"SELECT id FROM chunks WHERE name_key IN ({ph}) AND kind!='window' LIMIT ?",
                (*q.keys, limit))]
        for d in q.dotted:
            last = d.rsplit(".", 1)[-1]
            for r in db.execute("SELECT id, qual_key FROM chunks WHERE name_key=? AND kind!='window' "
                                "LIMIT 200", (last,)):
                parts = r[1]
                if parts == d.replace(".", ".") or parts.endswith("." + d) or parts == d:
                    out.append(r[0])
        return list(dict.fromkeys(out))

    @staticmethod
    def _literal_hits(lits: list[str], rows) -> dict[int, tuple[int, int]]:
        """chunk id -> (lines mentioning an identifier-like query token, is that token the chunk's name)."""
        pats = [(lit.lower(), name_key(lit)) for lit in lits[:3]]
        out: dict[int, tuple[int, int]] = {}
        for row in rows:
            low = unpack_text(row["text"]).lower().split("\n")
            hits = sum(1 for ln in low for p, _ in pats if p in ln)
            is_def = int(any(row["name_key"] == k for _, k in pats))
            if hits or is_def:
                out[row["id"]] = (hits, is_def)
        return out

    def _ncallers(self, names: list[str]) -> dict[str, int]:
        need = [n for n in set(names) if n and n not in self._callers]
        for i in range(0, len(need), 400):
            part = need[i : i + 400]
            ph = ",".join("?" * len(part))
            got = dict(self.ix.db.execute(
                f"SELECT callee, COUNT(*) FROM refs WHERE callee IN ({ph}) GROUP BY callee", part).fetchall())
            for n in part:
                self._callers[n] = got.get(n, 0)
        return self._callers

    # ------------------------------------------------------------ candidates + features
    def candidates(self, query: str, use_literal: bool = True):
        """-> (QueryInfo, meta rows, feature matrix) ; rows[i] is the sqlite row of candidate i."""
        q = QueryInfo.parse(query)
        self._store = self._sync()
        lex_ids = self._lex(q)
        sem_ids, qz = self._semantic(q)
        exact_ids = self._exact(q)
        want_lit = bool(use_literal and q.lits)
        order: dict[int, None] = {}
        for lst in (exact_ids, lex_ids, sem_ids):
            for i in lst:
                order.setdefault(i, None)
        ids = list(order)
        if not ids:
            return q, [], np.zeros((0, len(FEATURES)), np.float32)

        rows = []
        for i in range(0, len(ids), 500):
            part = ids[i : i + 500]
            ph = ",".join("?" * len(part))
            rows += self.ix.db.execute(
                f"SELECT id,path,kind,name,qualname,name_key,qual_key,start,end,is_test,ndoc,nbody,name_tok"
                f"{',text' if want_lit else ''} FROM chunks WHERE id IN ({ph})", part).fetchall()
        by_id = {r["id"]: r for r in rows}
        ids = [i for i in ids if i in by_id]
        rows = [by_id[i] for i in ids]
        n = len(ids)
        lit = self._literal_hits(q.lits, rows) if want_lit else {}

        lex_rank = {cid: r for r, cid in enumerate(lex_ids, 1)}
        sem_rank = {cid: r for r, cid in enumerate(sem_ids, 1)}

        X = np.zeros((n, len(FEATURES)), np.float32)
        # --- lexical
        S = self._store.field_scores(_lex_keys(q), np.asarray(ids, np.int64))  # [n, 7]: all, then per field
        mx = np.maximum(S.max(axis=0), 1e-9)
        for j, (lg, ra) in enumerate([("lex_log", "lex_ratio"), ("name_log", "name_ratio"),
                                      ("qual_log", "qual_ratio"), ("sig_log", "sig_ratio"),
                                      ("doc_log", "doc_ratio"), ("body_log", "body_ratio"),
                                      ("path_log", "path_ratio")]):
            col = np.maximum(S[:, j], 0)
            X[:, FI[lg]] = np.log1p(col.astype(np.float64))  # float64 log1p, rounded once (as the Rust engine)
            X[:, FI[ra]] = col / mx[j]
        X[:, FI["lex_rank"]] = [1.0 / (1 + lex_rank[c]) if c in lex_rank else 0.0 for c in ids]
        lx = S[:, 0]
        X[:, FI["lex_z"]] = _zscore(lx)
        # --- semantic
        if qz is not None:
            sa, sb = self._store.sem_pair(np.asarray(ids, np.int64), qz)
            mix = 0.5 * (sa + sb)
            X[:, FI["sem_a"]], X[:, FI["sem_b"]], X[:, FI["sem_mix"]] = sa, sb, mix
            for src, dst in ((sa, "sem_a_z"), (sb, "sem_b_z"), (mix, "sem_mix_z")):
                X[:, FI[dst]] = _zscore(src)
            X[:, FI["sem_rank"]] = [1.0 / (1 + sem_rank[c]) if c in sem_rank else 0.0 for c in ids]
        # --- exactness / name overlap
        tset = set(q.terms)
        exact_set = set(exact_ids)
        names = []
        for r, row in enumerate(rows):
            cid = ids[r]
            name = row["name"] or ""
            names.append(name)
            nt = set((row["name_tok"] or "").split())
            inter = len(nt & tset)
            X[r, FI["exact_name"]] = 1.0 if (cid in exact_set and row["name_key"] in q.keys) else 0.0
            X[r, FI["exact_qual"]] = 1.0 if (cid in exact_set and q.dotted) else 0.0
            X[r, FI["name_overlap"]] = inter / len(tset) if tset else 0.0
            X[r, FI["name_cover"]] = inter / len(nt) if nt else 0.0
            X[r, FI["name_same_set"]] = 1.0 if (nt and nt == tset) else 0.0
            lh = lit.get(cid)
            if lh:
                X[r, FI["lit_hits"]] = math.log1p(lh[0])
                X[r, FI["lit_def"]] = lh[1]
            k = row["kind"]
            X[r, FI["k_function"]] = k == "function"
            X[r, FI["k_method"]] = k == "method"
            X[r, FI["k_class"]] = k in ("class", "struct", "trait", "interface", "type", "enum", "impl")
            X[r, FI["k_module"]] = k == "module"
            X[r, FI["k_other"]] = k == "window"
            X[r, FI["is_test"]] = row["is_test"]
            X[r, FI["log_nbody"]] = math.log1p(row["nbody"] or 0)
            X[r, FI["has_doc"]] = 1.0 if (row["ndoc"] or 0) > 0 else 0.0
            X[r, FI["log_ndoc"]] = math.log1p(row["ndoc"] or 0)
            X[r, FI["qual_depth"]] = (row["qualname"] or "").count(".")
            X[r, FI["is_private"]] = 1.0 if name.startswith("_") and not name.startswith("__") else 0.0
        nc = self._ncallers(names)
        X[:, FI["log_ncallers"]] = [math.log1p(nc.get(nm, 0)) for nm in names]
        # --- query level + file cohesion
        X[:, FI["n_terms"]] = len(q.terms)
        X[:, FI["n_lits"]] = len(q.lits)
        X[:, FI["n_words"]] = len(q.words)
        X[:, FI["has_ident"]] = 1.0 if (q.keys and (q.dotted or "_" in q.text or q.wholes)) else 0.0
        top_paths = defaultdict(int)
        for cid in lex_ids[:30]:
            r = by_id.get(cid)
            if r is not None:
                top_paths[r["path"]] += 1
        X[:, FI["file_frac"]] = [top_paths.get(row["path"], 0) / 30.0 for row in rows]
        return q, rows, X

    # ---------------------------------------------------------------------- scoring
    def score(self, X: np.ndarray) -> np.ndarray:
        n = X.shape[0]
        if n == 0:
            return np.zeros(0, np.float32)
        lin = linear_score(X)
        if self.ranker is None:
            return lin
        keep = prune_indices(lin)
        out = np.full(n, -1e9, np.float64)  # float64 like the Rust engine: identical ties and order
        out[keep] = self.ranker.predict(X[keep])
        return out

    def _hits(self, q: QueryInfo, rows, X, scores, k: int, with_text: bool = True) -> list[Hit]:
        order = np.lexsort((np.array([r["id"] for r in rows]), -np.asarray(scores)))  # ties: lower id first
        picked: list[Hit] = []
        best_by_key: dict[tuple, int] = {}
        for r in order:
            row = rows[r]
            h = Hit(row["id"], row["path"], row["start"], row["end"], row["name"] or "",
                    row["qualname"] or "", row["kind"], "", float(scores[r]), _why(X[r]))
            key = (h.path, h.qualname)
            if h.qualname and h.kind in CODE_KINDS and key in best_by_key:
                prev = picked[best_by_key[key]]  # overload / redefinition: keep the longer body
                if (h.end - h.start) > (prev.end - prev.start):
                    picked[best_by_key[key]] = h
                continue
            if h.kind in ("module", "window") and any(
                    p.path == h.path and not (h.end < p.start or h.start > p.end) for p in picked):
                continue
            if h.qualname and h.kind in CODE_KINDS:
                best_by_key[key] = len(picked)
            picked.append(h)
            if len(picked) >= k:
                break
        if with_text and picked:
            ph = ",".join("?" * len(picked))
            texts = {i: unpack_text(t) for i, t in self.ix.db.execute(f"SELECT id,text FROM chunks WHERE id IN ({ph})",
                                                                      [h.id for h in picked]).fetchall()}
            for h in picked:
                h.text = texts.get(h.id, "")
        return picked

    def search(self, query: str, k: int = 10, with_text: bool = True) -> list[Hit]:
        q, rows, X = self.candidates(query)
        return self._hits(q, rows, X, self.score(X), k, with_text)

    def search_many(self, queries: list[str], k: int = 10, with_text: bool = True) -> list[Hit]:
        """Several phrasings of one need (synonyms, guessed identifiers): rank-fuse the results."""
        fused: dict[int, float] = defaultdict(float)
        meta: dict[int, Hit] = {}
        for qs in queries:
            for r, h in enumerate(self.search(qs, k=30, with_text=False), 1):
                fused[h.id] += 1.0 / (60 + r)
                meta.setdefault(h.id, h)
        top = sorted(fused, key=fused.get, reverse=True)[:k]
        out = [meta[i] for i in top]
        for h in out:
            h.score = fused[h.id]
        if with_text and out:
            ph = ",".join("?" * len(out))
            texts = {i: unpack_text(t) for i, t in self.ix.db.execute(f"SELECT id,text FROM chunks WHERE id IN ({ph})",
                                                                      [h.id for h in out]).fetchall()}
            for h in out:
                h.text = texts.get(h.id, "")
        return out

    # ----------------------------------------------------------------- navigation
    def _row_hit(self, r, with_text=True) -> Hit:
        return Hit(r["id"], r["path"], r["start"], r["end"], r["name"] or "", r["qualname"] or "",
                   r["kind"], unpack_text(r["text"]) if with_text else "")

    def find_symbol(self, name: str, limit: int = 20) -> list[Hit]:
        key = name_key(name.split(".")[-1])
        rows = self.ix.db.execute(
            "SELECT * FROM chunks WHERE name_key=? AND kind!='window' ORDER BY is_test, "
            "(kind IN ('class','function','method')) DESC LIMIT ?", (key, limit * 3)).fetchall()
        if "." in name:
            suffix = name_key(name)
            rows = [r for r in rows if r["qual_key"] == suffix or r["qual_key"].endswith("." + suffix)]
        return [self._row_hit(r) for r in rows[:limit]]

    def callers(self, name: str, limit: int = 30) -> list[Hit]:
        rows = self.ix.db.execute(
            "SELECT DISTINCT c.* FROM refs r JOIN chunks c ON c.id=r.chunk_id WHERE r.callee=? "
            "ORDER BY c.is_test LIMIT ?", (name.split(".")[-1], limit)).fetchall()
        return [self._row_hit(r) for r in rows]

    def callees(self, name: str, limit: int = 40) -> tuple[list[Hit], list[str]]:
        """-> (resolved definitions, unresolved external names)."""
        names: set[str] = set()
        for d in self.find_symbol(name, limit=5):
            names.update(r[0] for r in self.ix.db.execute("SELECT callee FROM refs WHERE chunk_id=?", (d.id,)))
        resolved, external = [], []
        for n in sorted(names):
            rows = self.ix.db.execute(
                "SELECT * FROM chunks WHERE name_key=? AND kind IN (%s) ORDER BY is_test LIMIT 3"
                % ",".join("?" * len(CODE_KINDS)), (name_key(n), *CODE_KINDS)).fetchall()
            if rows:
                resolved.extend(self._row_hit(r) for r in rows)
            else:
                external.append(n)
        return resolved[:limit], external

    def grep(self, pattern: str, k: int = 30) -> list[Hit]:
        """Exact regex search grouped by enclosing symbol (ripgrep when available, else a scan)."""
        seen: dict[int, Hit] = {}
        if shutil.which("rg"):
            try:
                out = subprocess.run(
                    ["rg", "-n", "--no-heading", "--color", "never", "--glob", "!.scrutate", "-e", pattern, "."],
                    cwd=self.ix.root, capture_output=True, text=True, timeout=30).stdout
            except (OSError, subprocess.TimeoutExpired):
                out = ""
            for ln in out.splitlines():
                parts = ln.split(":", 2)
                if len(parts) >= 2 and parts[1].isdigit():
                    path = parts[0][2:] if parts[0].startswith("./") else parts[0]
                    row = self.ix.chunk_at(path, int(parts[1]))
                    if row is not None and row["id"] not in seen:
                        seen[row["id"]] = self._row_hit(row)
                        if len(seen) >= k:
                            break
            return list(seen.values())
        try:
            rx = re.compile(pattern)
        except re.error:
            return []
        for row in self.ix.db.execute("SELECT * FROM chunks ORDER BY is_test, id"):
            if rx.search(unpack_text(row["text"])):
                seen[row["id"]] = self._row_hit(row)
                if len(seen) >= k:
                    break
        return list(seen.values())

    def read(self, path: str, start: int = 1, end: int | None = None, max_lines: int = 400) -> str:
        root = os.path.realpath(self.ix.root)
        full = os.path.realpath(os.path.join(root, path))
        if os.path.commonpath([full, root]) != root:
            return "error: path outside repo"
        try:
            with open(full, encoding="utf8", errors="replace") as f:
                lines = split_lines(f.read())
        except OSError as e:
            return f"error: {e}"
        end = min(end or start + max_lines - 1, len(lines), start + max_lines - 1)
        return "\n".join(f"{i:>5}  {lines[i-1]}" for i in range(max(1, start), end + 1))


def _zscore(v: np.ndarray) -> np.ndarray:
    """(v - mean) / (std + 1e-9) with the statistics accumulated in float64 (the Rust engine does the
    same, so the two produce bit-identical float32 features)."""
    v64 = v.astype(np.float64)
    mean, std = np.float32(v64.mean()), np.float32(v64.std())
    return (v - mean) / (std + 1e-9)


def _why(x: np.ndarray) -> dict:
    """Human-readable evidence for a hit, from its feature vector."""
    f = lambda n: float(x[FI[n]])  # noqa: E731
    why = {}
    if f("exact_name") or f("exact_qual"):
        why["exact name"] = 1
    for label, key in (("name", "name_ratio"), ("signature", "sig_ratio"), ("docs", "doc_ratio"),
                       ("body", "body_ratio"), ("path", "path_ratio")):
        if f(key) >= 0.5:
            why[f"{label} keywords"] = round(f(key), 2)
    if f("sem_mix") >= 0.25:
        why["semantic"] = round(f("sem_mix"), 2)
    if f("lit_hits"):
        why["literal match"] = int(round(math.expm1(f("lit_hits"))))
    return why


def prune_indices(lin: np.ndarray, n: int = PRUNE) -> np.ndarray:
    if len(lin) <= n:
        return np.arange(len(lin))
    return np.argsort(-lin, kind="stable")[:n]  # ties: lower index first, as in the Rust engine


def linear_score(X: np.ndarray) -> np.ndarray:
    """Hand-set blend used when no trained ranker is available."""
    f = lambda n: X[:, FI[n]]  # noqa: E731
    return (f("lex_z") + 0.9 * f("sem_mix_z") + 2.5 * f("exact_name") + 1.5 * f("exact_qual")
            + 1.2 * f("lit_def") + 0.3 * f("lit_hits") + 1.0 * f("name_overlap") - 0.4 * f("is_test"))
