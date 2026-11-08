"""Code-word semantic model (cross-view SVD), trained offline on real code.

Two views of every function:
  A (descriptive)      name words x3, enclosing-class words, signature words, docstring words
  B (implementation)   identifiers / words appearing in the body

Partial-least-squares SVD of the A x B co-occurrence matrix gives a shared latent space where a
natural-language phrase (view A) lands near code whose *implementation* uses the matching
vocabulary ("remove" ~ delete/pop/unlink, "liveness" ~ ping/heartbeat/health). It is a small
static model (a few MB): no GPU, no downloads, incremental indexing never needs a refit.
"""
from __future__ import annotations

import json
import math
import os
import zipfile
from collections import Counter

import numpy as np

# cached 1+log(c) table
_LOG = [0.0] + [1.0 + math.log(c) for c in range(1, 200)]


def _sublog(c: int) -> float:
    return _LOG[c] if c < 200 else 1.0 + math.log(c)


def view_counts(name_t, qual_t, sig_t, doc_t, body_t):
    """Token lists -> (view A counts, view B counts)."""
    a: dict[str, int] = {}
    for t in name_t:
        a[t] = a.get(t, 0) + 3
    for toks in (qual_t, sig_t, doc_t):
        for t in toks:
            a[t] = a.get(t, 0) + 1
    b: dict[str, int] = {}
    for t in body_t:
        b[t] = b.get(t, 0) + 1
    return a, b


class SemanticModel:
    def __init__(self, va, vb, idf_a, idf_b, U, V, s, cfg=None):
        self.va = {t: i for i, t in enumerate(va)}
        self.vb = {t: i for i, t in enumerate(vb)}
        self.idf_a = np.asarray(idf_a, np.float32)
        self.idf_b = np.asarray(idf_b, np.float32)
        self.s = np.asarray(s, np.float32)
        self.U0 = np.asarray(U)  # kept in the precision they are stored in (float16 on disk)
        self.V0 = np.asarray(V)
        self.cfg = cfg or {}
        self.k = self.U0.shape[1]
        self._U: np.ndarray | None = None
        self._V: np.ndarray | None = None
        self.set_params(gamma=self.cfg.get("gamma", 0.5), wa=self.cfg.get("wa", 0.5))

    def set_params(self, gamma: float | None = None, wa: float | None = None) -> None:
        """gamma: spectral weighting of latent dims; wa: weight of the chunk's own view-A part."""
        if gamma is not None:
            self.gamma = gamma
            self._w = np.power(self.s / self.s[0], gamma).astype(np.float32)
            self._U = self._V = None
        if wa is not None:
            self.wa = wa

    # The weighted float32 matrices are only needed to embed many chunks at once (indexing). Answering
    # a query touches a handful of rows, so a search-only process never builds them: starting up is
    # ~100 ms faster.
    @property
    def U(self) -> np.ndarray:
        if self._U is None:
            self._U = self.U0.astype(np.float32) * self._w
        return self._U

    @property
    def V(self) -> np.ndarray:
        if self._V is None:
            self._V = self.V0.astype(np.float32) * self._w
        return self._V

    def warm(self) -> None:
        """Build the full matrices now (call before forking indexing workers so they share them)."""
        _ = self.U, self.V

    def _rows_a(self, idx) -> np.ndarray:
        return self._U[idx] if self._U is not None else self.U0[idx].astype(np.float32) * self._w

    # ------------------------------------------------------------------ encode
    def _proj(self, counts, vocab, idf, rows):
        """Weighted sum of word vectors, accumulated in float64 (the Rust engine does the same, so
        both produce bit-identical float32 vectors)."""
        idx, w = [], []
        for t, c in counts.items():
            i = vocab.get(t)
            if i is not None:
                idx.append(i)
                w.append(_sublog(c) * float(idf[i]))
        if not idx:
            return None
        w = np.asarray(w, np.float64)
        n = float(np.linalg.norm(w))
        if n == 0:
            return None
        return (w / n) @ rows(idx).astype(np.float64)

    def encode_chunk(self, name_t, qual_t, sig_t, doc_t, body_t) -> np.ndarray | None:
        a, b = view_counts(name_t, qual_t, sig_t, doc_t, body_t)
        U, V = self.U, self.V
        za = self._proj(a, self.va, self.idf_a, U.__getitem__)
        zb = self._proj(b, self.vb, self.idf_b, V.__getitem__)
        if za is None and zb is None:
            return None
        z = (self.wa * za if za is not None else 0) + ((1 - self.wa) * zb if zb is not None else 0)
        n = float(np.linalg.norm(z))
        return (z / n).astype(np.float32) if n > 0 else None

    def encode_query(self, terms: list[str]) -> np.ndarray | None:
        z = self._proj(Counter(terms), self.va, self.idf_a, self._rows_a)
        if z is None:
            return None
        n = float(np.linalg.norm(z))
        return (z / n).astype(np.float32) if n > 0 else None

    def neighbors(self, term: str, n: int = 8, space: str = "a"):
        """Nearest vocabulary terms (for inspection)."""
        vocab, M = (self.va, self.U) if space == "a" else (self.vb, self.V)
        if term not in vocab:
            return []
        Mn = M / (np.linalg.norm(M, axis=1, keepdims=True) + 1e-9)
        sims = Mn @ Mn[vocab[term]]
        inv = {i: t for t, i in vocab.items()}
        return [(inv[i], float(sims[i])) for i in np.argsort(-sims)[1 : n + 1]]

    # ------------------------------------------------------------------- io
    def save(self, path: str) -> None:
        inv_a = sorted(self.va, key=self.va.get)
        inv_b = sorted(self.vb, key=self.vb.get)
        arrays = dict(va=np.array(inv_a), vb=np.array(inv_b), idf_a=self.idf_a, idf_b=self.idf_b,
                      U=np.ascontiguousarray(self.U0, dtype=np.float16), V=np.ascontiguousarray(self.V0, dtype=np.float16),
                      s=self.s, cfg=np.array(json.dumps(self.cfg)))  # C order: the Rust engine maps these
        # an .npz that np.load reads as usual; the weight matrices are stored raw (float16 barely
        # compresses and inflating them took longer than reading them), the word lists deflated
        with zipfile.ZipFile(path, "w") as zf:
            for name, arr in arrays.items():
                info = zipfile.ZipInfo(name + ".npy", date_time=(2020, 1, 1, 0, 0, 0))
                info.compress_type = zipfile.ZIP_DEFLATED if arr.dtype.kind in "US" else zipfile.ZIP_STORED
                with zf.open(info, "w", force_zip64=True) as fh:
                    np.lib.format.write_array(fh, arr, allow_pickle=False)

    @classmethod
    def load(cls, path: str) -> "SemanticModel":
        z = np.load(path, allow_pickle=False)
        return cls(z["va"].tolist(), z["vb"].tolist(), z["idf_a"], z["idf_b"], z["U"], z["V"],
                   z["s"], json.loads(str(z["cfg"])))


def default_model_path() -> str:
    return os.path.join(os.path.dirname(__file__), "..", "..", "models", "semantic.npz")


_default: SemanticModel | None | bool = False


def default_model() -> SemanticModel | None:
    """The bundled model, loaded once (None when absent)."""
    global _default
    if _default is False:
        p = os.environ.get("SCRUTATE_SEMANTIC", default_model_path())
        _default = SemanticModel.load(p) if os.path.exists(p) else None
    return _default  # type: ignore[return-value]


# --------------------------------------------------------------------- training
def pls_svd(XA, XB, k: int, n_iter: int = 3, oversample: int = 24, center: bool = True,
            seed: int = 0):
    """Randomized SVD of the (optionally centered) cross-covariance XA^T XB, never formed."""
    n = XA.shape[0]
    muA = np.asarray(XA.mean(0)).ravel().astype(np.float32)
    muB = np.asarray(XB.mean(0)).ravel().astype(np.float32)

    def C_mul(Q):  # (VB, r) -> (VA, r)
        Y = XA.T @ (XB @ Q)
        if center:
            Y = Y - n * np.outer(muA, muB @ Q)
        return Y.astype(np.float32)

    def Ct_mul(P):  # (VA, r) -> (VB, r)
        Y = XB.T @ (XA @ P)
        if center:
            Y = Y - n * np.outer(muB, muA @ P)
        return Y.astype(np.float32)

    rng = np.random.default_rng(seed)
    r = k + oversample
    Q = rng.standard_normal((XB.shape[1], r)).astype(np.float32)
    Y = C_mul(Q)
    for _ in range(n_iter):
        Y, _ = np.linalg.qr(Y)
        Z, _ = np.linalg.qr(Ct_mul(Y))
        Y = C_mul(Z)
    Qa, _ = np.linalg.qr(Y)
    B = Ct_mul(Qa).T  # (r, VB) = Qa^T C
    Ub, S, Vt = np.linalg.svd(B, full_matrices=False)
    return (Qa @ Ub[:, :k]).astype(np.float32), S[:k].astype(np.float32), Vt[:k].T.astype(np.float32)
