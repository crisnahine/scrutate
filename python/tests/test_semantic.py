import numpy as np
import pytest

from scrutate_ref.semantic import SemanticModel, pls_svd


def test_pls_svd_matches_dense_svd_on_centered_cross_covariance():
    sp = pytest.importorskip("scipy.sparse")  # only needed to retrain the model
    rng = np.random.default_rng(0)
    XA = sp.csr_matrix((rng.random((200, 30)) > 0.7) * rng.random((200, 30)))
    XB = sp.csr_matrix((rng.random((200, 25)) > 0.7) * rng.random((200, 25)))
    # plant a strong shared factor so the leading singular values are well separated
    shared = rng.random((200, 1))
    XA = sp.csr_matrix(XA.toarray() + shared @ rng.random((1, 30)) * 2)
    XB = sp.csr_matrix(XB.toarray() + shared @ rng.random((1, 25)) * 2)
    U, S, V = pls_svd(XA, XB, k=4, n_iter=5, center=True)
    A, B = XA.toarray(), XB.toarray()
    C = (A - A.mean(0)).T @ (B - B.mean(0))
    s_true = np.linalg.svd(C, compute_uv=False)[:4]
    assert np.allclose(S, s_true, rtol=1e-2)
    assert np.allclose(U.T @ U, np.eye(4), atol=1e-3)


def tiny_model():
    va = ["remov", "delet", "tool", "ping"]
    vb = ["pop", "del", "dict", "send"]
    U = np.array([[1, 0], [0.9, 0.1], [0.2, 0.2], [0, 1]], np.float32)
    V = np.array([[0.9, 0], [1, 0.1], [0.1, 0.3], [0, 1]], np.float32)
    return SemanticModel(va, vb, np.ones(4), np.ones(4), U, V, np.array([2.0, 1.0]), {"k": 2})


def test_synonyms_land_close_and_vectors_are_unit_norm():
    m = tiny_model()
    q = m.encode_query(["remov"])
    c_sim = m.encode_chunk(["delet"], [], [], [], [])
    c_far = m.encode_chunk(["ping"], [], [], [], [])
    assert abs(np.linalg.norm(q) - 1) < 1e-5
    assert q @ c_sim > q @ c_far


def test_unknown_words_give_none():
    m = tiny_model()
    assert m.encode_query(["zzz"]) is None
    assert m.encode_chunk([], [], [], [], ["zzz"]) is None


def test_save_load_roundtrip(tmp_path):
    m = tiny_model()
    p = str(tmp_path / "m.npz")
    m.save(p)
    m2 = SemanticModel.load(p)
    a, b = m.encode_query(["remov", "tool"]), m2.encode_query(["remov", "tool"])
    assert np.allclose(a, b, atol=1e-3)


def test_query_encoding_needs_only_a_few_rows_and_agrees_with_the_full_matrices():
    """A search-only process never builds the weighted float32 matrices; the answer must not depend on it."""
    m = tiny_model()
    terms = ["remov", "tool", "ping"]
    lazy = m.encode_query(terms)
    assert m._U is None and m._V is None
    m.warm()
    assert m._U is not None
    assert np.array_equal(lazy, m.encode_query(terms))
    m.set_params(gamma=0.0)  # changing the weighting drops the cached matrices
    assert m._U is None
    assert not np.array_equal(lazy, m.encode_query(terms))


def test_weight_matrices_are_stored_raw_and_the_file_is_a_plain_npz(tmp_path):
    import zipfile

    p = str(tmp_path / "m.npz")
    tiny_model().save(p)
    kinds = {i.filename: i.compress_type for i in zipfile.ZipFile(p).infolist()}
    assert kinds["U.npy"] == zipfile.ZIP_STORED and kinds["V.npy"] == zipfile.ZIP_STORED
    assert kinds["va.npy"] == zipfile.ZIP_DEFLATED
    z = np.load(p, allow_pickle=False)  # np.load must keep working on it
    assert z["U"].dtype == np.float16 and z["va"].tolist() == ["remov", "delet", "tool", "ping"]


def test_a_retrained_model_of_the_same_shape_rebuilds_the_index(tmp_path):
    from scrutate_ref.index import Index

    a, b = tiny_model(), tiny_model()
    b.U0 = b.U0 * 2  # other weights, same k, n_pairs and vocabulary sizes
    pa, pb = str(tmp_path / "a.npz"), str(tmp_path / "b.npz")
    a.save(pa)
    b.save(pb)
    root = tmp_path / "repo"
    root.mkdir()
    sigs = []
    for p in (pa, pb):
        ix = Index(str(root), model=SemanticModel.load(p))
        sigs.append(ix.meta("model_sig"))
        ix.db.close()
    assert sigs[0] != sigs[1]
