"""Segment store: memory-mapped BM25 postings + semantic vectors (Lucene-style, numpy only).

An index is a list of immutable *segments*. Every `update()` that adds chunks writes one new
segment; deletions are not written anywhere (a chunk is alive iff its id is still in the SQLite
`chunks` table), and small segments are merged in the background of `update()`.

Lexical scoring reproduces SQLite FTS5's `bm25()` exactly -- same k1/b, same idf, row length =
tokens over all columns, column weights multiply the term frequency -- so a ranker trained on
FTS5 features keeps working (tests/test_lex.py checks this against a real FTS5 table). Unlike
FTS5 the whole thing is one vectorised pass instead of a per-row aux-function call, which is
what makes it 10-50x faster on large corpora.
"""
from __future__ import annotations

import hashlib
import math
import os
import shutil
from dataclasses import dataclass

import numpy as np

FIELDS = ("name", "qual", "sig", "doc", "body", "path")
NF = len(FIELDS)
K1, B = 1.2, 0.75
WEIGHTS = (10.0, 3.0, 3.0, 5.0, 1.0, 2.0)  # name, qual, sig, doc, body, path
IDF_FLOOR = 1e-6

# Vector scan. Segments with at least IVF_MIN_ROWS chunks get an inverted-file index: the vectors are
# clustered (spherical k-means) and stored in cluster order; a query scores the centroids and scans
# only the IVF_NPROBE closest clusters. Smaller segments are scanned exhaustively (it costs ~1 ms).
IVF_MIN_ROWS = 20_000
IVF_NPROBE = 48
IVF_SAMPLE = 25_000   # rows used to fit the centroids
IVF_ITERS = 10

_KEY_CACHE: dict[str, int] = {}


def tok_key(tok: str) -> int:
    """Stable 63-bit key of a token (no vocabulary file: segments can be merged without remapping)."""
    k = _KEY_CACHE.get(tok)
    if k is None:
        k = int.from_bytes(hashlib.blake2b(tok.encode("utf8", "replace"), digest_size=8).digest(), "little") >> 1
        if len(_KEY_CACHE) < 3_000_000:
            _KEY_CACHE[tok] = k
    return k


@dataclass
class FileLex:
    """Lexical + vector data of the chunks of one file, produced by a worker process."""
    dl: np.ndarray            # float32 [m]    tokens per chunk over all fields (repeats counted)
    keys: list                # per field: int64 [E]   term keys, chunk after chunk
    tf: list                  # per field: uint8 [E]   term frequencies
    cnt: list                 # per field: int32 [m]   distinct terms per chunk
    vec: np.ndarray | None    # float16 [m, 2, k]


def pack_bags(bags: list, vec: np.ndarray | None) -> FileLex:
    """bags[i][c] is a {token: tf} dict of chunk i, field c."""
    m = len(bags)
    dl = np.zeros(m, np.float32)
    keys = [[] for _ in range(NF)]
    tfs = [[] for _ in range(NF)]
    cnt = [np.zeros(m, np.int32) for _ in range(NF)]
    for i, fields in enumerate(bags):
        total = 0
        for c, bag in enumerate(fields):
            if bag:
                keys[c].extend(map(tok_key, bag))
                tfs[c].extend(bag.values())
                cnt[c][i] = len(bag)
                total += sum(bag.values())
        dl[i] = total
    return FileLex(dl, [np.array(k, np.int64) for k in keys],
                   [np.minimum(np.array(t, np.int64), 255).astype(np.uint8) for t in tfs], cnt, vec)


# --------------------------------------------------------------------------- segments
def _load(path: str) -> np.ndarray:
    try:
        return np.load(path, mmap_mode="r")
    except ValueError:  # numpy cannot map an empty file
        return np.load(path)


class Segment:
    def __init__(self, path: str):
        self.path = path
        self.ids = _load(os.path.join(path, "ids.npy"))
        self.dl = _load(os.path.join(path, "dl.npy"))
        self.keys = _load(os.path.join(path, "keys.npy"))
        self.df = _load(os.path.join(path, "df.npy"))
        self.ptr = [_load(os.path.join(path, f"ptr{c}.npy")) for c in range(NF)]
        self.doc = [_load(os.path.join(path, f"doc{c}.npy")) for c in range(NF)]
        self.tf = [_load(os.path.join(path, f"tf{c}.npy")) for c in range(NF)]
        vp = os.path.join(path, "vec.npy")
        # float16 [n, 2, k]; in *cluster order* when the segment has an inverted-file index (see
        # build_ivf), else in row order. `vec_rows` maps segment rows to it.
        self.vec = _load(vp) if os.path.exists(vp) else None
        self.n = len(self.ids)
        self.ivf = os.path.exists(os.path.join(path, "ivf_order.npy"))
        if self.ivf:
            self.ivf_cent = np.load(os.path.join(path, "ivf_cent.npy"))  # small: read, not mapped
            self.ivf_ptr = np.load(os.path.join(path, "ivf_ptr.npy"))
            self.ivf_order = _load(os.path.join(path, "ivf_order.npy"))  # segment row at each sorted position
            self.ivf_inv = _load(os.path.join(path, "ivf_inv.npy"))      # sorted position of each segment row

    def vec_rows(self, rows: np.ndarray) -> np.ndarray:
        """float16 [len(rows), 2, k] vectors of the given segment rows."""
        pos = np.asarray(self.ivf_inv)[rows] if self.ivf else rows
        return np.asarray(self.vec)[pos]

    def vec_in_row_order(self) -> np.ndarray:
        """All vectors, as float16 [n, 2, k] in segment-row order."""
        return self.vec_rows(np.arange(self.n))

    def locate(self, keys: np.ndarray) -> np.ndarray:
        """-> index of each key in this segment's vocabulary, -1 when absent."""
        if len(self.keys) == 0:
            return np.full(len(keys), -1, np.int64)
        j = np.searchsorted(self.keys, keys)
        jc = np.minimum(j, len(self.keys) - 1)
        return np.where(self.keys[jc] == keys, jc, -1)


def write_segment(dest: str, ids: np.ndarray, dl: np.ndarray, fields: list, vec: np.ndarray | None,
                  row_sorted: bool) -> None:
    """fields[c] = (row int32[E], key int64[E], tf uint8[E]); if `row_sorted` the entries are
    already in ascending row order (true for freshly indexed chunks, false when merging)."""
    n = len(ids)
    tmp = dest + ".tmp"
    shutil.rmtree(tmp, ignore_errors=True)
    os.makedirs(tmp)

    def save(name, arr):
        np.save(os.path.join(tmp, name + ".npy"), arr)

    nonempty = [f[1] for f in fields if len(f[1])]
    vocab = np.unique(np.concatenate(nonempty)) if nonempty else np.zeros(0, np.int64)
    nv = len(vocab)
    pairs = []
    for c, (row, key, tf) in enumerate(fields):
        if len(key):
            vi = np.searchsorted(vocab, key).astype(np.int32)
            order = np.argsort(vi, kind="stable") if row_sorted else np.lexsort((row, vi))
            doc, tfs = row[order].astype(np.int32), tf[order].astype(np.uint8)
            counts = np.bincount(vi, minlength=nv)
            pairs.append(vi.astype(np.int64) * n + row)
        else:
            doc, tfs, counts = np.zeros(0, np.int32), np.zeros(0, np.uint8), np.zeros(nv, np.int64)
        ptr = np.zeros(nv + 1, np.int64)
        np.cumsum(counts, out=ptr[1:])
        save(f"ptr{c}", ptr)
        save(f"doc{c}", doc)
        save(f"tf{c}", tfs)
    if pairs:  # documents containing the term in *any* field (what FTS5 calls nHit)
        df = np.bincount(np.unique(np.concatenate(pairs)) // n, minlength=nv).astype(np.int32)
    else:
        df = np.zeros(nv, np.int32)
    save("keys", vocab)
    save("df", df)
    save("ids", np.asarray(ids, np.int64))
    save("dl", np.asarray(dl, np.float32))
    if vec is not None:
        vec = np.asarray(vec, np.float16)
        if n >= IVF_MIN_ROWS:
            order = build_ivf(tmp, vec)
            vec = np.ascontiguousarray(vec[order])  # cluster order: a probe reads one contiguous block
        save("vec", vec)
    os.replace(tmp, dest)


def _nearest(x: np.ndarray, cent: np.ndarray, block: int = 16384) -> np.ndarray:
    out = np.empty(len(x), np.int32)
    for i in range(0, len(x), block):
        out[i:i + block] = np.argmax(x[i:i + block] @ cent.T, axis=1)
    return out


def build_ivf(tmp: str, vec: np.ndarray, seed: int = 0) -> np.ndarray:
    """Cluster the A+B vectors of a segment (spherical k-means on a sample, then one assignment pass
    over every row). Returns the cluster order (segment row at each sorted position); the caller
    stores the vectors in that order so a query can scan a few clusters contiguously. Exact top-k
    over all rows is still available by probing every cluster."""
    n = len(vec)
    mix = vec[:, 0].astype(np.float32)
    mix += vec[:, 1].astype(np.float32)
    unit = mix / np.maximum(np.linalg.norm(mix, axis=1), 1e-6)[:, None]
    K = int(np.clip(round(2 * math.sqrt(n)), 32, 1024))
    rng = np.random.default_rng(seed)
    sample = unit[np.sort(rng.choice(n, min(n, IVF_SAMPLE), replace=False))]
    cent = sample[rng.choice(len(sample), K, replace=False)].copy()
    for _ in range(IVF_ITERS):
        a = _nearest(sample, cent)
        order = np.argsort(a, kind="stable")
        counts = np.bincount(a, minlength=K)
        starts = np.concatenate(([0], np.cumsum(counts)[:-1]))
        nonempty = counts > 0
        sums = np.add.reduceat(sample[order], starts[nonempty], axis=0)
        new = cent.copy()
        new[nonempty] = sums / np.maximum(np.linalg.norm(sums, axis=1), 1e-6)[:, None]
        if not nonempty.all():  # re-seed empty clusters from random rows
            new[~nonempty] = sample[rng.choice(len(sample), int((~nonempty).sum()), replace=False)]
        cent = new
    a = _nearest(unit, cent)
    order = np.argsort(a, kind="stable").astype(np.int32)
    inv = np.empty(n, np.int32)
    inv[order] = np.arange(n, dtype=np.int32)
    ptr = np.zeros(K + 1, np.int64)
    np.cumsum(np.bincount(a, minlength=K), out=ptr[1:])
    np.save(os.path.join(tmp, "ivf_cent.npy"), cent.astype(np.float32))
    np.save(os.path.join(tmp, "ivf_ptr.npy"), ptr)
    np.save(os.path.join(tmp, "ivf_order.npy"), order)
    np.save(os.path.join(tmp, "ivf_inv.npy"), inv)
    return order


class SegmentBuilder:
    """Accumulates FileLex batches (in chunk-id order) and writes one segment."""

    def __init__(self) -> None:
        self.ids: list[np.ndarray] = []
        self.dl: list[np.ndarray] = []
        self.parts: list[list] = [[] for _ in range(NF)]  # per field: (first row, FileLex)
        self.vec: list[np.ndarray] = []
        self.n = 0

    def add_file(self, first_id: int, lex: FileLex) -> None:
        m = len(lex.dl)
        if m == 0:
            return
        self.ids.append(np.arange(first_id, first_id + m, dtype=np.int64))
        self.dl.append(lex.dl)
        for c in range(NF):
            self.parts[c].append((self.n, lex.cnt[c], lex.keys[c], lex.tf[c]))
        if lex.vec is not None:
            self.vec.append(lex.vec)
        self.n += m

    def write(self, dest: str) -> None:
        fields = []
        for c in range(NF):
            rows, keys, tfs = [], [], []
            for first, cnt, k, t in self.parts[c]:
                rows.append(np.repeat(np.arange(first, first + len(cnt), dtype=np.int32), cnt))
                keys.append(k)
                tfs.append(t)
            fields.append((np.concatenate(rows) if rows else np.zeros(0, np.int32),
                           np.concatenate(keys) if keys else np.zeros(0, np.int64),
                           np.concatenate(tfs) if tfs else np.zeros(0, np.uint8)))
        write_segment(dest, np.concatenate(self.ids), np.concatenate(self.dl), fields,
                      np.concatenate(self.vec) if self.vec else None, row_sorted=True)


def merge_segments(dest: str, segs: list, alive: list) -> None:
    """Write one segment holding only the alive documents of `segs` (any order, ids may interleave)."""
    ids = np.concatenate([np.asarray(s.ids)[a] for s, a in zip(segs, alive)])
    order = np.argsort(ids, kind="stable")  # new row of the document at concatenated position p
    rank = np.empty(len(order), np.int64)
    rank[order] = np.arange(len(order))
    dl = np.concatenate([np.asarray(s.dl)[a] for s, a in zip(segs, alive)])[order]
    vecs = [s.vec_in_row_order()[a] for s, a in zip(segs, alive) if s.vec is not None]
    vec = np.concatenate(vecs)[order] if vecs else None
    fields = [([], [], []) for _ in range(NF)]
    offset = 0
    for s, a in zip(segs, alive):
        newrow = np.cumsum(a, dtype=np.int64) - 1 + offset  # position among the concatenated alive docs
        for c in range(NF):
            ptr = np.asarray(s.ptr[c])
            if ptr[-1] == 0:
                continue
            doc = np.asarray(s.doc[c])
            keep = a[doc]
            fields[c][0].append(rank[newrow[doc[keep]]].astype(np.int32))
            fields[c][1].append(np.repeat(np.asarray(s.keys), np.diff(ptr))[keep])
            fields[c][2].append(np.asarray(s.tf[c])[keep])
        offset += int(a.sum())
    cat = [(np.concatenate(r) if r else np.zeros(0, np.int32), np.concatenate(k) if k else np.zeros(0, np.int64),
            np.concatenate(t) if t else np.zeros(0, np.uint8)) for r, k, t in fields]
    write_segment(dest, ids[order], dl, cat, vec, row_sorted=False)


def _top(sc: np.ndarray, limit: int, mask: np.ndarray) -> np.ndarray:
    """Indices of the `limit` best of `sc[mask]`, deterministically: every row tied with the last one
    kept is considered and ties are broken by index, so two engines (or two runs) select the same rows."""
    cand = np.flatnonzero(mask)
    if cand.size <= limit:
        return cand[np.lexsort((cand, -sc[cand]))]
    part = cand[np.argpartition(-sc[cand], limit - 1)[:limit]]
    thresh = sc[part].min()
    cand = cand[sc[cand] >= thresh]
    cand = cand[np.lexsort((cand, -sc[cand]))]
    return cand[:limit]


# ------------------------------------------------------------------------------ store
class LexStore:
    """All segments of an index plus the global statistics needed to score across them."""

    def __init__(self, base: str, names: list[str], weights: tuple[float, ...] = WEIGHTS):
        self.base = base
        self.names = list(names)
        self.segs = [Segment(os.path.join(base, n)) for n in names]
        self.w = np.asarray(weights, np.float32)
        self.alive: list[np.ndarray] = []
        self.has_dead: list[bool] = []
        self._dlsum: list[float] = []  # tokens in the live chunks of each segment
        self._adf: dict[tuple[int, int], int] = {}
        self.norm: list[np.ndarray] = []
        self.mix: list[np.ndarray | None] = []        # exhaustive scan matrix of the small segments
        self.alive_sorted: list[np.ndarray | None] = []  # liveness in cluster order (ivf segments)
        self.nprobe = IVF_NPROBE
        self.n_alive = 0
        self.avgdl = 1.0

    # ---- statistics (a chunk is alive iff its id is still in the SQLite `chunks` table)
    def prepare(self, live_ids: np.ndarray | None) -> None:
        """Statistics from scratch. live_ids: sorted int64 ids of the chunks currently in the index,
        or None when every row of every segment is known to be alive (nothing was ever deleted)."""
        self.alive, self._dlsum = [], []
        for s in self.segs:
            if live_ids is None:
                a = np.ones(s.n, bool)
            elif len(live_ids) and s.n:
                pos = np.minimum(np.searchsorted(live_ids, s.ids), len(live_ids) - 1)
                a = live_ids[pos] == s.ids
            else:
                a = np.zeros(s.n, bool)
            self.alive.append(a)
            self._dlsum.append(float(np.asarray(s.dl, np.float64)[a].sum()))
        self.n_alive = sum(int(a.sum()) for a in self.alive)
        self.has_dead = [not bool(a.all()) for a in self.alive]
        self.mix = [self._mix_of(s, a) for s, a in zip(self.segs, self.alive)]
        self.alive_sorted = [a[np.asarray(s.ivf_order)] if s.ivf else None for s, a in zip(self.segs, self.alive)]
        self._adf = {}
        self._set_norm()

    @staticmethod
    def _mix_of(s: Segment, alive: np.ndarray) -> np.ndarray | None:
        """float32 [n, k] A-view + B-view vector of every row (0 for dead rows): what the exhaustive scan
        multiplies. None for segments that have an inverted-file index (they are scanned from disk)."""
        if s.vec is None or not s.n or s.ivf:
            return None
        v = s.vec_in_row_order()
        m = v[:, 0].astype(np.float32)
        m += v[:, 1].astype(np.float32)
        m[~alive] = 0.0
        return m

    def _set_norm(self) -> None:
        """BM25 length normalisation per row; depends on the average length of the *live* chunks."""
        self.avgdl = sum(self._dlsum) / self.n_alive if self.n_alive else 1.0
        self.norm = [(K1 * (1 - B + B * np.asarray(s.dl, np.float32) / self.avgdl)).astype(np.float32)
                     for s in self.segs]

    def apply(self, removed: np.ndarray, added: list[str]) -> None:
        """Update the statistics after the chunks `removed` died and the segments `added` appeared.
        Gives exactly what `prepare()` over the new live set would, but only touches what changed,
        so an edit costs milliseconds even in an index of a hundred thousand chunks."""
        removed = np.unique(np.asarray(removed, np.int64))
        if removed.size:
            self._adf = {}
            for si, s in enumerate(self.segs):
                if not s.n:
                    continue
                pos = np.minimum(np.searchsorted(s.ids, removed), s.n - 1)
                rows = pos[np.asarray(s.ids)[pos] == removed]
                rows = rows[self.alive[si][rows]]
                if rows.size:
                    self.alive[si][rows] = False
                    self._dlsum[si] -= float(np.asarray(s.dl, np.float64)[rows].sum())
                    self.n_alive -= int(rows.size)
                    self.has_dead[si] = True
                    if self.mix[si] is not None:
                        self.mix[si][rows] = 0.0
                    if self.alive_sorted[si] is not None:
                        self.alive_sorted[si][np.asarray(s.ivf_inv)[rows]] = False
        for name in added:
            s = Segment(os.path.join(self.base, name))
            a = np.ones(s.n, bool)
            self.names.append(name)
            self.segs.append(s)
            self.alive.append(a)
            self.has_dead.append(False)
            self._dlsum.append(float(np.asarray(s.dl, np.float64).sum()))
            self.n_alive += s.n
            self.mix.append(self._mix_of(s, a))
            self.alive_sorted.append(a[np.asarray(s.ivf_order)] if s.ivf else None)
        self._set_norm()

    def alive_counts(self) -> list[int]:
        return [int(a.sum()) for a in self.alive]

    # ---- lexical
    def _alive_df(self, si: int, j: int) -> int:
        """Live documents of segment `si` containing term `j` in any field (cached per data version)."""
        got = self._adf.get((si, j))
        if got is None:
            s = self.segs[si]
            seen = np.zeros(s.n, bool)
            for c in range(NF):
                seen[s.doc[c][s.ptr[c][j]:s.ptr[c][j + 1]]] = True
            got = self._adf[(si, j)] = int(np.count_nonzero(seen & self.alive[si]))
        return got

    def _idf(self, locs: list) -> np.ndarray:
        """idf exactly as FTS5 computes it over the live chunks: log((N - df + 0.5) / (df + 0.5))."""
        df = np.zeros(len(locs[0]) if locs else 0, np.int64)
        for si, (s, j) in enumerate(zip(self.segs, locs)):
            found = np.flatnonzero(j >= 0)
            if self.has_dead[si]:  # the stored df still counts chunks that were edited or deleted since
                for t in found:
                    df[t] += self._alive_df(si, int(j[t]))
            else:
                df[found] += s.df[j[found]]
        with np.errstate(divide="ignore", invalid="ignore"):
            idf = np.log((self.n_alive - df + 0.5) / (df + 0.5))
        return np.where(np.isfinite(idf) & (idf > 0), idf, IDF_FLOOR).astype(np.float32)

    def search(self, keys: list[int], limit: int):
        """Top `limit` chunks by field-weighted BM25 -> (ids int64[], scores float32[]), best first."""
        empty = (np.zeros(0, np.int64), np.zeros(0, np.float32))
        if not keys or not self.n_alive:
            return empty
        karr = np.asarray(keys, np.int64)
        locs = [s.locate(karr) for s in self.segs]
        idf = self._idf(locs)
        out_ids, out_sc = [], []
        for s, j, alive, norm in zip(self.segs, locs, self.alive, self.norm):
            present = np.flatnonzero(j >= 0)
            if not present.size or not s.n:
                continue
            f = np.zeros(s.n, np.float32)
            acc = np.zeros(s.n, np.float32)
            for t in present:
                jt = j[t]
                for c in range(NF):
                    a, b = s.ptr[c][jt], s.ptr[c][jt + 1]
                    if a != b:
                        f[s.doc[c][a:b]] += self.w[c] * s.tf[c][a:b].astype(np.float32)
                u = np.flatnonzero(f)
                fu = f[u]
                f[u] = 0.0
                acc[u] += idf[t] * (K1 + 1.0) * fu / (fu + norm[u])
            acc[~alive] = 0.0
            part = _top(acc, limit, acc > 0)
            out_ids.append(np.asarray(s.ids)[part])
            out_sc.append(acc[part])
        if not out_ids:
            return empty
        ids, sc = np.concatenate(out_ids), np.concatenate(out_sc)
        order = np.lexsort((ids, -sc))[:limit]
        return ids[order], sc[order]

    def field_scores(self, keys: list[int], ids: np.ndarray) -> np.ndarray:
        """-> float32 [len(ids), 1 + NF]: weighted score, then the score of each single field."""
        out = np.zeros((len(ids), 1 + NF), np.float32)
        if not keys or not len(ids) or not self.n_alive:
            return out
        karr = np.asarray(keys, np.int64)
        locs = [s.locate(karr) for s in self.segs]
        idf = self._idf(locs)
        for s, j, alive, norm in zip(self.segs, locs, self.alive, self.norm):
            if not s.n:
                continue
            pos = np.minimum(np.searchsorted(s.ids, ids), s.n - 1)
            sel = np.flatnonzero((np.asarray(s.ids)[pos] == ids) & alive[pos])
            if not sel.size:
                continue
            rows = pos[sel]
            nm = norm[rows]
            acc = np.zeros((sel.size, 1 + NF), np.float32)
            for t in np.flatnonzero(j >= 0):
                jt = j[t]
                tfm = np.zeros((sel.size, NF), np.float32)
                for c in range(NF):
                    a, b = int(s.ptr[c][jt]), int(s.ptr[c][jt + 1])
                    if a == b:
                        continue
                    d = s.doc[c][a:b]
                    p = np.minimum(np.searchsorted(d, rows), b - a - 1)
                    hit = np.asarray(d[p]) == rows
                    tfm[hit, c] = s.tf[c][a:b][p[hit]]
                comb = tfm @ self.w
                acc[:, 0] += idf[t] * (K1 + 1.0) * comb / (comb + nm)
                acc[:, 1:] += idf[t] * (K1 + 1.0) * tfm / (tfm + nm[:, None])
            out[sel] = acc
        return out

    # ---- semantic
    def semantic_top(self, qz: np.ndarray, limit: int, nprobe: int | None = None) -> np.ndarray:
        """Ids of the `limit` chunks closest to the query vector (A-view + B-view similarity).
        Segments with an inverted-file index scan their `nprobe` closest clusters (all of them when
        nprobe >= the number of clusters, which makes the result exact); the others are scanned whole."""
        nprobe = self.nprobe if nprobe is None else nprobe
        out_ids, out_sc = [], []
        for si, s in enumerate(self.segs):
            if s.ivf:
                part, sc = self._ivf_scan(si, qz, limit, nprobe)
            else:
                m = self.mix[si]
                if m is None:
                    continue
                sc = m @ qz
                sc[~self.alive[si]] = -np.inf
                part = _top(sc, limit, np.isfinite(sc))
                sc = sc[part]
            if part.size:
                out_ids.append(np.asarray(s.ids)[part])
                out_sc.append(sc)
        if not out_ids:
            return np.zeros(0, np.int64)
        ids, sc = np.concatenate(out_ids), np.concatenate(out_sc)
        return ids[np.lexsort((ids, -sc))[:limit]]

    def _ivf_scan(self, si: int, qz: np.ndarray, limit: int, nprobe: int):
        """-> (segment rows, scores) of the best `limit` live rows in the `nprobe` closest clusters."""
        s = self.segs[si]
        K = len(s.ivf_ptr) - 1
        if nprobe < K:
            cs = s.ivf_cent @ qz
            probe = np.argpartition(-cs, nprobe - 1)[:nprobe]
            starts, ends = s.ivf_ptr[probe], s.ivf_ptr[probe + 1]
            lens = ends - starts
            total = int(lens.sum())
            if total == 0:
                return np.zeros(0, np.int64), np.zeros(0, np.float32)
            # positions (in cluster order) of every row in the probed clusters, as one gather
            pos = np.repeat(starts - np.concatenate(([0], np.cumsum(lens)[:-1])), lens) + np.arange(total)
            v = np.asarray(s.vec)[pos]
            alive = self.alive_sorted[si][pos]
        else:
            pos = None
            v = np.asarray(s.vec)
            alive = self.alive_sorted[si]
        rows = v[:, 0].astype(np.float32)
        rows += v[:, 1].astype(np.float32)
        sc = rows @ qz
        sc[~alive] = -np.inf
        part = _top(sc, limit, np.isfinite(sc))
        spos = pos[part] if pos is not None else part
        return np.asarray(s.ivf_order)[spos].astype(np.int64), sc[part]

    def sem_pair(self, ids: np.ndarray, qz: np.ndarray):
        """-> (sim of the A view, sim of the B view) for each id; 0 where a chunk has no vector."""
        sa = np.zeros(len(ids), np.float32)
        sb = np.zeros(len(ids), np.float32)
        for s in self.segs:
            if s.vec is None or not s.n:
                continue
            pos = np.minimum(np.searchsorted(s.ids, ids), s.n - 1)
            sel = np.flatnonzero(np.asarray(s.ids)[pos] == ids)
            if sel.size:
                v = s.vec_rows(pos[sel]).astype(np.float64)  # float64 accumulation: same as the Rust engine
                q64 = qz.astype(np.float64)
                sa[sel] = (v[:, 0] @ q64).astype(np.float32)
                sb[sel] = (v[:, 1] @ q64).astype(np.float32)
        return sa, sb
