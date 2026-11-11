"""The numpy segment store must score exactly like SQLite FTS5's bm25() -- the learned ranker was
trained on FTS5 scores -- and must stay correct through updates and merges."""
import os
import shutil
import sqlite3
from collections import Counter

import numpy as np
import pytest

import scrutate_ref
from scrutate_ref.index import TF_CAP, Index
from scrutate_ref.lex import NF, WEIGHTS, LexStore, tok_key
from scrutate_ref.parse import parse_source
from scrutate_ref.search import QueryInfo, _lex_keys
from scrutate_ref.text import BODY_CAP, tokenize2

QUERIES = ["parse source file into chunks", "tokenize identifier", "semantic model encode query",
           "bm25 score", "sqlite connection", "index update changed files", "zzzunknownterm",
           "rank candidates tree ensemble predict"]


def _counted(tokens):
    c = Counter(tokens)
    return " ".join(" ".join([t] * min(n, TF_CAP)) if n > 1 else t for t, n in c.items())


def fts_strings(rel, c):
    """The six column texts the previous FTS5-based index stored for a chunk (the oracle)."""
    path_s, _ = tokenize2(rel.replace("/", " ").replace(".", " ").replace("_", " "))
    qual = c.qualname.rsplit(".", 1)[0] if "." in c.qualname else ""
    name_s, name_w = tokenize2(c.name)
    qual_s, qual_w = tokenize2(qual)
    sig_s, sig_w = tokenize2(c.sig, 80)
    doc_s, doc_w = tokenize2(c.doc, 200)
    body_s, body_w = tokenize2(c.body, BODY_CAP)
    return (" ".join(name_s + name_w), " ".join(qual_s + qual_w), _counted(sig_s + sig_w),
            _counted(doc_s + doc_w), _counted(body_s + body_w), " ".join(path_s))


@pytest.fixture(scope="module")
def corpus(tmp_path_factory):
    root = tmp_path_factory.mktemp("lexcorpus")
    src = os.path.dirname(scrutate_ref.__file__)
    for fn in os.listdir(src):
        if fn.endswith(".py"):
            shutil.copy(os.path.join(src, fn), root / fn)
    ix = Index(str(root), model=None)
    ix.update()
    fts = sqlite3.connect(":memory:")
    fts.execute("CREATE VIRTUAL TABLE fts USING fts5(name, qual, sig, doc, body, path, tokenize='unicode61')")
    n = 0
    for fn in sorted(os.listdir(root)):
        if not fn.endswith(".py"):
            continue
        for c in parse_source(fn, (root / fn).read_text()):
            if c.kind == "constant":  # symbols only, never scored
                continue
            row = ix.db.execute("SELECT id FROM chunks WHERE path=? AND kind=? AND name=? AND start=?",
                                (fn, c.kind, c.name, c.start)).fetchone()
            fts.execute("INSERT INTO fts(rowid,name,qual,sig,doc,body,path) VALUES(?,?,?,?,?,?,?)",
                        (row[0], *fts_strings(fn, c)))
            n += 1
    assert n == ix.db.execute("SELECT COUNT(*) FROM chunks WHERE kind != 'constant'").fetchone()[0] > 100
    return ix, fts, root


def fts_scores(fts, query_keys_tokens, weights):
    w = ",".join(str(x) for x in weights)
    match = " OR ".join(f'"{t}"' for t in query_keys_tokens)
    return {r[0]: -r[1] for r in fts.execute(f"SELECT rowid, bm25(fts,{w}) FROM fts WHERE fts MATCH ?", (match,))}


def query_tokens(text):
    q = QueryInfo.parse(text)
    return [t for t in dict.fromkeys(q.terms + q.wholes) if t.isalnum() and t.islower() or t.isdigit()]


@pytest.mark.parametrize("text", QUERIES)
def test_weighted_and_per_field_scores_match_fts5(corpus, text):
    ix, fts, _ = corpus
    store = ix.lex
    toks = query_tokens(text)
    keys = [tok_key(t) for t in toks]
    all_ids = np.array(sorted(r[0] for r in ix.db.execute("SELECT id FROM chunks WHERE kind != 'constant'")), np.int64)
    got = store.field_scores(keys, all_ids)
    expect = [fts_scores(fts, toks, WEIGHTS)] + [
        fts_scores(fts, toks, [1.0 if j == i else 0.0 for j in range(NF)]) for i in range(NF)]
    for col, want in enumerate(expect):
        for i, cid in enumerate(all_ids):
            assert got[i, col] == pytest.approx(want.get(int(cid), 0.0), rel=2e-4, abs=1e-5), (text, col, cid)


@pytest.mark.parametrize("text", QUERIES)
def test_top_k_matches_fts5_order(corpus, text):
    ix, fts, _ = corpus
    toks = query_tokens(text)
    want = fts_scores(fts, toks, WEIGHTS)
    ids, sc = ix.lex.search([tok_key(t) for t in toks], 25)
    assert len(ids) == min(25, len(want))
    # same set of scores at every rank (ids may swap only between exact ties)
    ref = sorted(want.values(), reverse=True)[: len(ids)]
    assert sc.tolist() == pytest.approx(ref, rel=2e-4)
    for cid, s in zip(ids.tolist(), sc.tolist()):
        assert want[cid] == pytest.approx(s, rel=2e-4)


def test_lex_keys_only_contain_indexable_tokens():
    keys = _lex_keys(QueryInfo.parse("Refresh the OAuth token_expiry, 3x"))
    assert keys and all(isinstance(k, int) and k >= 0 for k in keys)


@pytest.mark.parametrize("small_limit", [8000, 0])  # 0 forces the multi-segment (large index) policy
def test_update_keeps_search_consistent_with_a_fresh_index(tmp_path, monkeypatch, small_limit):
    """Edits, deletions and compaction must give the same ranking as indexing the result from scratch."""
    import scrutate_ref.index as index_mod

    monkeypatch.setattr(index_mod, "SMALL_INDEX_ROWS", small_limit)
    root = tmp_path / "live"
    src = os.path.dirname(scrutate_ref.__file__)
    root.mkdir()
    for fn in ("text.py", "parse.py", "rank.py", "lex.py"):
        shutil.copy(os.path.join(src, fn), root / fn)
    ix = Index(str(root), model=None)
    ix.update()
    # a series of small edits creates several segments and tombstones
    for i in range(9):
        with open(root / "rank.py", "a") as f:
            f.write(f"\n\ndef extra_function_{i}(x):\n    '''Tree ensemble helper number {i}.'''\n    return x\n")
        ix.update()
    os.remove(root / "parse.py")
    ix.update()
    assert len(ix.segments()) <= 6

    fresh_root = tmp_path / "fresh"
    shutil.copytree(root, fresh_root, ignore=shutil.ignore_patterns(".scrutate"))
    fresh = Index(str(fresh_root), model=None)
    fresh.update()

    def top(index, text):
        keys = [tok_key(t) for t in query_tokens(text)]
        ids, sc = index.lex.search(keys, 15)
        names = [index.chunk(int(i))["qualname"] + "@" + index.chunk(int(i))["path"] for i in ids]
        return names, sc

    for text in ("tree ensemble helper", "tokenize identifier", "stem word", "parse source"):
        n1, s1 = top(ix, text)
        n2, s2 = top(fresh, text)
        # statistics are computed over live chunks only, so edits/deletions never skew the scores
        assert s1.tolist() == pytest.approx(s2.tolist(), rel=1e-4), text
        assert n1 == n2, text


def test_full_merge_equals_fresh_build_exactly(tmp_path):
    root = tmp_path / "r"
    root.mkdir()
    src = os.path.dirname(scrutate_ref.__file__)
    for fn in ("text.py", "rank.py"):
        shutil.copy(os.path.join(src, fn), root / fn)
    ix = Index(str(root), model=None)
    ix.update()
    for i in range(3):
        with open(root / "rank.py", "a") as f:
            f.write(f"\n\ndef added_{i}(x):\n    return x + {i}\n")
        ix.update()
    ix.update(force=True)  # everything is rewritten -> old segments are dead and get dropped/merged
    assert len(ix.segments()) == 1
    fresh_root = tmp_path / "f"
    shutil.copytree(root, fresh_root, ignore=shutil.ignore_patterns(".scrutate"))
    fresh = Index(str(fresh_root), model=None)
    fresh.update()
    keys = [tok_key(t) for t in query_tokens("tokenize identifier stem")]
    a_ids, a_sc = ix.lex.search(keys, 20)
    b_ids, b_sc = fresh.lex.search(keys, 20)
    assert a_sc.tolist() == pytest.approx(b_sc.tolist(), rel=1e-6)
    assert [ix.chunk(int(i))["qualname"] for i in a_ids] == [fresh.chunk(int(i))["qualname"] for i in b_ids]


def test_crash_during_update_leaves_a_working_index(tmp_path, monkeypatch):
    root = tmp_path / "c"
    root.mkdir()
    (root / "a.py").write_text("def alpha():\n    return 1\n")
    ix = Index(str(root), model=None)
    ix.update()
    before = ix.segments()
    (root / "b.py").write_text("def beta():\n    return 2\n")

    import scrutate_ref.index as index_mod

    def boom(self, dest):
        raise RuntimeError("disk full")

    monkeypatch.setattr(index_mod.SegmentBuilder, "write", boom)
    with pytest.raises(RuntimeError):
        ix.update()
    monkeypatch.undo()
    assert ix.segments() == before
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE path='b.py'").fetchone()[0] == 0
    ix.update()  # next run simply redoes the work
    assert ix.db.execute("SELECT COUNT(*) FROM chunks WHERE path='b.py'").fetchone()[0] >= 1
    assert len(ix.lex.search([tok_key("beta")], 5)[0]) == 1


def assert_same_statistics(ix):
    """The store `ix` keeps in memory (patched by update()) equals one computed from scratch."""
    got = ix.lex
    live = np.fromiter((r[0] for r in ix.db.execute("SELECT id FROM chunks WHERE kind != 'constant' ORDER BY id")), np.int64)
    ref = LexStore(ix.seg_dir, ix.segments())
    ref.prepare(live)
    assert got.names == ref.names
    assert got.n_alive == ref.n_alive == len(live)
    assert got.avgdl == ref.avgdl
    assert got.has_dead == ref.has_dead
    for a, b in zip(got.alive, ref.alive):
        assert np.array_equal(a, b)
    for a, b in zip(got.norm, ref.norm):
        assert np.array_equal(a, b)
    for a, b in zip(got.mix, ref.mix):
        assert (a is None) == (b is None)
        if a is not None:
            assert np.array_equal(a, b)


@pytest.mark.parametrize("small_limit", [8000, 0])
def test_statistics_patched_by_update_equal_recomputed_ones(tmp_path, monkeypatch, small_limit):
    """update() patches the in-memory statistics (alive rows, average length, vectors to scan) instead of
    recomputing them over the whole index; after any mix of edits and deletions they must be identical."""
    import scrutate_ref.index as index_mod

    monkeypatch.setattr(index_mod, "SMALL_INDEX_ROWS", small_limit)
    applied = []
    real = LexStore.apply
    monkeypatch.setattr(LexStore, "apply", lambda self, *a, **k: (applied.append(1), real(self, *a, **k))[1])
    root = tmp_path / "live"
    root.mkdir()
    src = os.path.dirname(scrutate_ref.__file__)
    for fn in ("text.py", "parse.py", "rank.py", "lex.py", "search.py"):
        shutil.copy(os.path.join(src, fn), root / fn)
    ix = Index(str(root))  # with the real semantic model, so the vectors are exercised too
    ix.update()
    assert_same_statistics(ix)
    for i in range(5):
        with open(root / "rank.py", "a") as f:
            f.write(f"\n\ndef extra_{i}(x):\n    \'\'\'Helper number {i}.\'\'\'\n    return x\n")
        ix.update()
        assert_same_statistics(ix)
    os.remove(root / "parse.py")
    ix.update()
    assert_same_statistics(ix)
    (root / "text.py").write_bytes(b"\x00\x01\x02" * 50)  # turns into a binary file: its chunks must vanish
    ix.update()
    assert_same_statistics(ix)
    assert ix.lex.n_alive == ix.db.execute("SELECT COUNT(*) FROM chunks WHERE kind != 'constant'").fetchone()[0]
    assert len(applied) >= 4  # the incremental path really was the one exercised


def _toy_vectors(n, k, rng):
    """Unit A/B vectors with a few clear directions, quantized as stored."""
    centers = rng.standard_normal((8, k)).astype(np.float32)
    v = centers[rng.integers(0, 8, n)] + 0.3 * rng.standard_normal((n, k)).astype(np.float32)
    a = v / np.linalg.norm(v, axis=1, keepdims=True)
    b = np.roll(a, 1, axis=1)
    from scrutate_ref.lex import quantize

    return quantize(np.stack([a, b], axis=1))


def _mix(vec):
    from scrutate_ref.lex import dequantize

    v = dequantize(*vec)
    return v[:, 0] + v[:, 1]


def test_clustered_scan_is_exact_when_every_cluster_is_probed(tmp_path, monkeypatch):
    """A segment above IVF_MIN_ROWS stores its vectors in cluster order; probing all clusters must give
    exactly the exhaustive ranking, and deleted chunks must never come back."""
    import scrutate_ref.lex as lex
    from scrutate_ref.lex import LexStore, write_segment

    monkeypatch.setattr(lex, "IVF_MIN_ROWS", 100)
    monkeypatch.setattr(lex, "IVF_SAMPLE", 400)
    rng = np.random.default_rng(0)
    n, k = 1500, 16
    vec = _toy_vectors(n, k, rng)
    ids = np.arange(10, 10 + n, dtype=np.int64)
    fields = [(np.zeros(0, np.int32), np.zeros(0, np.int64), np.zeros(0, np.uint8)) for _ in range(NF)]
    seg_dir = tmp_path / "seg"
    seg_dir.mkdir()
    write_segment(str(seg_dir / "s000001"), ids, np.ones(n, np.float32), fields, vec, row_sorted=True)
    assert (seg_dir / "s000001" / "ivf_order.npy").exists()  # vectors are stored in cluster order; no separate mix matrix
    store = LexStore(str(seg_dir), ["s000001"])
    store.prepare(None)
    s = store.segs[0]
    K = len(s.ivf_ptr) - 1
    assert s.ivf and K >= 32
    mix = _mix(vec)
    for qi in range(5):
        q = rng.standard_normal(k).astype(np.float32)
        q /= np.linalg.norm(q)
        exact = mix @ q
        want = ids[np.lexsort((ids, -exact))[:20]].tolist()
        got = store.semantic_top(q, 20, nprobe=K).tolist()
        assert got == want
        few = store.semantic_top(q, 20, nprobe=max(2, K // 3)).tolist()
        assert 0 < len(few) <= 20 and set(few) <= set(ids.tolist())
        assert len(set(few) & set(want)) >= 10  # the closest third of the clusters holds most of the answer
        few_sc = exact[np.searchsorted(ids, few)]
        assert np.all(np.diff(few_sc) <= 1e-6)  # and what it returns is in exact-score order
    # deletions: the dead rows disappear from both the exact and the clustered path
    dead = ids[rng.choice(n, 300, replace=False)]
    store.apply(dead, [])
    q = rng.standard_normal(k).astype(np.float32)
    q /= np.linalg.norm(q)
    alive = ~np.isin(ids, dead)
    exact = np.where(alive, mix @ q, -np.inf)
    want = ids[np.lexsort((ids, -exact))[:20]].tolist()
    assert store.semantic_top(q, 20, nprobe=K).tolist() == want
    assert not set(store.semantic_top(q, 50, nprobe=6).tolist()) & set(dead.tolist())
    # prepare() from the live id list gives the same state as the patched one
    fresh = LexStore(str(seg_dir), ["s000001"])
    fresh.prepare(np.sort(ids[alive]))
    assert np.array_equal(fresh.alive_sorted[0], store.alive_sorted[0])
    assert fresh.semantic_top(q, 20, nprobe=K).tolist() == want


def test_merge_keeps_the_clustered_layout_consistent(tmp_path, monkeypatch):
    import scrutate_ref.lex as lex
    from scrutate_ref.lex import LexStore, Segment, merge_segments, write_segment

    monkeypatch.setattr(lex, "IVF_MIN_ROWS", 100)
    monkeypatch.setattr(lex, "IVF_SAMPLE", 300)
    rng = np.random.default_rng(1)
    k = 16
    fields = [(np.zeros(0, np.int32), np.zeros(0, np.int64), np.zeros(0, np.uint8)) for _ in range(NF)]
    seg_dir = tmp_path / "seg"
    seg_dir.mkdir()
    vecs, idss = [], []
    for i, n in enumerate((400, 50, 300)):  # the middle one is too small for clusters
        ids = np.arange(1000 * i, 1000 * i + n, dtype=np.int64)
        vec = _toy_vectors(n, k, rng)
        write_segment(str(seg_dir / f"s{i}"), ids, np.ones(n, np.float32), fields, vec, row_sorted=True)
        vecs.append(vec)
        idss.append(ids)
    segs = [Segment(str(seg_dir / f"s{i}")) for i in range(3)]
    assert segs[0].ivf and not segs[1].ivf and segs[2].ivf
    alive = [np.ones(len(i), bool) for i in idss]
    alive[0][:100] = False
    merge_segments(str(seg_dir / "merged"), segs, alive)
    store = LexStore(str(seg_dir), ["merged"])
    store.prepare(None)
    assert store.segs[0].ivf and store.n_alive == 400 - 100 + 50 + 300
    all_ids = np.concatenate([i[a] for i, a in zip(idss, alive)])
    all_mix = np.concatenate([_mix(v)[a] for v, a in zip(vecs, alive)])
    q = rng.standard_normal(k).astype(np.float32)
    q /= np.linalg.norm(q)
    sc = all_mix @ q
    want = all_ids[np.lexsort((all_ids, -sc))[:15]].tolist()
    K = len(store.segs[0].ivf_ptr) - 1
    assert store.semantic_top(q, 15, nprobe=K).tolist() == want


def test_quantize_round_trip():
    from scrutate_ref.lex import dequantize, quantize

    rng = np.random.default_rng(3)
    v = rng.standard_normal((50, 2, 256)).astype(np.float32)
    v /= np.linalg.norm(v, axis=-1, keepdims=True)
    v[0, 0] = 0.0
    q, s = quantize(v)
    assert q.dtype == np.int8 and s.dtype == np.float32 and s.shape == (50, 2)
    assert s[0, 0] == 0 and not q[0, 0].any()
    assert np.abs(q).max() == 127
    d = dequantize(q, s)
    assert np.all(np.abs(d - v) <= s[..., None] / 2 * 1.0001)
    assert np.array_equal(quantize(d)[0], q)  # quantizing a decoded vector gives the same codes
