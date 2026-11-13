"""Learned ranker: a LightGBM tree ensemble exported to plain arrays.

Training happens offline (bench/train_ranker.py); at query time only numpy is needed.
"""
from __future__ import annotations

import os

import numpy as np


class TreeEnsemble:
    """Gradient-boosted regression trees, evaluated for all trees at once."""

    def __init__(self, feat, thr, left, right, val, feature_names, base=0.0):
        # arrays shaped (n_trees, max_nodes); leaves have feat == -1 and carry `val`
        self.feat, self.thr, self.left, self.right, self.val = feat, thr, left, right, val
        self.feature_names = list(feature_names)
        self.base = base
        self.depth = self._depth()
        self._flatten()

    def _depth(self) -> int:
        best = 0
        for t in range(self.feat.shape[0]):
            stack = [(0, 0)]
            while stack:
                i, d = stack.pop()
                if self.feat[t, i] < 0:
                    best = max(best, d)
                else:
                    stack.append((int(self.left[t, i]), d + 1))
                    stack.append((int(self.right[t, i]), d + 1))
        return best

    @classmethod
    def from_lightgbm(cls, booster, feature_names) -> "TreeEnsemble":
        dump = booster.dump_model()
        trees = []
        for ti in dump["tree_info"]:
            nodes: list[tuple] = []  # (feat, thr, left, right, val)

            def add(node) -> int:
                i = len(nodes)
                nodes.append(None)  # placeholder
                if "leaf_value" in node:
                    nodes[i] = (-1, 0.0, i, i, float(node["leaf_value"]))
                else:
                    assert node["decision_type"] == "<=", node["decision_type"]
                    l = add(node["left_child"])
                    r = add(node["right_child"])
                    nodes[i] = (int(node["split_feature"]), float(node["threshold"]), l, r, 0.0)
                return i

            add(ti["tree_structure"])
            trees.append(nodes)
        m = max(len(n) for n in trees)
        T = len(trees)
        feat = np.full((T, m), -1, np.int32)
        thr = np.zeros((T, m), np.float64)
        left = np.zeros((T, m), np.int32)
        right = np.zeros((T, m), np.int32)
        val = np.zeros((T, m), np.float32)
        for t, nodes in enumerate(trees):
            for i, (f, th, l, r, v) in enumerate(nodes):
                feat[t, i], thr[t, i], left[t, i], right[t, i], val[t, i] = f, th, l, r, v
        return cls(feat, thr, left, right, val, feature_names)

    def _flatten(self) -> None:
        """One flat array per node attribute, nodes of all trees side by side. Leaves point at
        themselves (and compare against +inf) so a finished path can take part in a step harmlessly."""
        T, m = self.feat.shape
        node = np.arange(T * m, dtype=np.int32).reshape(T, m)
        base = (np.arange(T, dtype=np.int32) * m)[:, None]
        leaf = self.feat < 0
        self._f = np.where(leaf, 0, self.feat).astype(np.int32).ravel()
        self._thr = np.where(leaf, np.inf, self.thr).ravel()
        lr = np.empty((T * m, 2), np.int32)  # children interleaved: one gather picks left or right
        lr[:, 0] = np.where(leaf, node, self.left + base).ravel()
        lr[:, 1] = np.where(leaf, node, self.right + base).ravel()
        self._lr = lr.ravel()
        self._v = self.val.ravel().astype(np.float64)
        self._leaf = leaf.ravel()
        self._root = base.ravel()
        self._T = T

    def predict(self, X: np.ndarray) -> np.ndarray:
        """Scores for the rows of X. All (row, tree) paths advance together, one level per step; once
        most of them have reached a leaf only the unfinished ones are carried on, so the work is
        proportional to the total path length rather than to rows x trees x maximum depth."""
        X = np.ascontiguousarray(X, np.float64)  # LightGBM compares in float64; float32 can flip branches
        n, nf = X.shape
        T = self._T
        if n == 0:
            return np.zeros(0, np.float64)
        flat = X.ravel()
        rowoff = np.repeat(np.arange(n, dtype=np.int32) * nf, T)
        idx = np.tile(self._root, n)
        act = None  # positions whose path has not reached a leaf yet (None: all of them)
        for _ in range(self.depth):
            if act is None:
                right = ~(flat[rowoff + self._f[idx]] <= self._thr[idx])  # NaN goes right, as before
                idx = self._lr[idx * 2 + right]
                unfinished = ~self._leaf[idx]
                if unfinished.mean() < 0.5:
                    act = np.flatnonzero(unfinished)
            else:
                cur = idx[act]
                right = ~(flat[rowoff[act] + self._f[cur]] <= self._thr[cur])
                cur = self._lr[cur * 2 + right]
                idx[act] = cur
                act = act[~self._leaf[cur]]
                if act.size == 0:
                    break
        # sequential (cumsum) rather than pairwise summation: the Rust engine adds tree by tree
        return np.cumsum(self._v[idx].reshape(n, T), axis=1)[:, -1] + self.base

    def save(self, path: str) -> None:
        np.savez_compressed(path, feat=self.feat, thr=self.thr, left=self.left, right=self.right,
                            val=self.val, names=np.array(self.feature_names), base=np.array(self.base))

    @classmethod
    def load(cls, path: str) -> "TreeEnsemble":
        z = np.load(path, allow_pickle=False)
        return cls(z["feat"], z["thr"], z["left"], z["right"], z["val"], z["names"].tolist(),
                   float(z["base"]))


def default_ranker_path() -> str:
    return os.path.join(os.path.dirname(__file__), "..", "..", "crates", "scrutate", "models", "ranker.npz")
