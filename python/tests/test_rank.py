import numpy as np
import pytest

from scrutate_ref.rank import TreeEnsemble

lgb = pytest.importorskip("lightgbm")


def test_numpy_inference_matches_lightgbm(tmp_path):
    rng = np.random.default_rng(0)
    X = rng.random((600, 6)).astype(np.float32)
    X[:, 2] = (X[:, 2] > 0.5)  # a binary feature
    y = ((X[:, 0] + X[:, 1] * X[:, 2] + 0.2 * rng.standard_normal(600)) > 0.9).astype(int)
    m = lgb.LGBMRanker(n_estimators=60, num_leaves=15, min_child_samples=5, verbose=-1, n_jobs=1)
    m.fit(X, y, group=[100] * 6)
    ens = TreeEnsemble.from_lightgbm(m.booster_, [f"f{i}" for i in range(6)])
    Xt = rng.random((300, 6)).astype(np.float32)
    Xt[:, 2] = (Xt[:, 2] > 0.5)
    assert np.abs(ens.predict(Xt) - m.booster_.predict(Xt)).max() < 1e-4
    p = str(tmp_path / "r.npz")
    ens.save(p)
    again = TreeEnsemble.load(p)
    assert again.feature_names == ens.feature_names
    assert np.allclose(again.predict(Xt), ens.predict(Xt))


def test_predict_matches_a_plain_per_tree_walk_on_many_rows():
    """The fast traversal (all paths together, then only the unfinished ones) must equal the obvious
    one-tree-at-a-time walk, including on rows that reach the deepest leaves."""
    import numpy as np

    from scrutate_ref.rank import TreeEnsemble, default_ranker_path

    ens = TreeEnsemble.load(default_ranker_path())
    rng = np.random.default_rng(3)
    X = rng.random((300, len(ens.feature_names)))
    X[:, rng.integers(0, X.shape[1], 10)] *= 50  # a few features far outside [0, 1)
    X[5] = 0.0
    want = np.full(len(X), ens.base, np.float64)
    for t in range(ens.feat.shape[0]):
        for r in range(len(X)):
            i = 0
            while ens.feat[t, i] >= 0:
                i = int(ens.left[t, i]) if X[r, ens.feat[t, i]] <= ens.thr[t, i] else int(ens.right[t, i])
            want[r] += ens.val[t, i]
    got = ens.predict(X)
    assert np.allclose(got, want, atol=1e-9)
    assert ens.predict(X[:0]).shape == (0,)
