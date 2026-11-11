#!/usr/bin/env python3
"""Research: small transformer text-embedding models in place of the code-word semantic model.

usage:
  eval_transformers.py features MODEL TAG GROUP[DEV|TEST]   candidate features, same format as gen_features.py
  eval_transformers.py compare  TAG RANKER BASE_TAG BASE_RANKER   MRR per type + paired 95% CI vs the base
  eval_transformers.py speed    MODEL [N]                    CPU embedding cost for N chunks (default 1000)

MODEL is `current` (models/semantic.npz) or a sentence-transformers model id. A transformer embeds two
views per chunk like the shipped model (A: qualified name + signature + docstring, B: body) and the
raw query text; vectors go into the same index slots, so candidates, features and the ranker script
are unchanged. Needs torch and sentence-transformers; set HF_HOME to keep the weights out of the repo.
Train and evaluate the ranker on the result with train_ranker.py (fit TAG OUT.npz, eval TAG OUT.npz).
"""
from __future__ import annotations

import os
import pickle
import random
import resource
import sys
import time
from collections import Counter

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import is_relevant, load_queries, mrr_values, paired_diff_ci, summarize  # noqa: E402
from groups import BENCH, DEV, TEST, WORK  # noqa: E402

import scrutate_ref.index as ixmod  # noqa: E402
from scrutate_ref.index import Index, decode_source, list_files  # noqa: E402
from scrutate_ref.lex import pack_bags  # noqa: E402
from scrutate_ref.parse import parse_source  # noqa: E402
from scrutate_ref.search import FEATURES, Searcher  # noqa: E402
from scrutate_ref.semantic import SemanticModel, default_model_path  # noqa: E402
from scrutate_ref.text import BODY_CAP, is_test_path, name_key, tokenize2  # noqa: E402

THREADS = int(os.environ.get("THREADS", "4"))
MAX_TOKENS = 256  # same truncation for every model
QUERY_PREFIX = {"BAAI/bge-small-en-v1.5": "Represent this sentence for searching relevant passages: "}
SEED = 0


def view_texts(c) -> tuple[str | None, str | None]:
    a = None if c.kind == "window" else "\n".join(x for x in (c.qualname, c.sig, c.doc) if x).strip() or None
    return a, (c.body.strip() or None)


class Transformer:
    """Stands in for SemanticModel inside Index / Searcher (only the attributes they read)."""

    def __init__(self, name: str, device: str = "cpu"):
        import torch
        from sentence_transformers import SentenceTransformer
        torch.set_num_threads(THREADS)
        torch.manual_seed(SEED)
        self.name = name
        self.st = SentenceTransformer(name, device=device)
        self.st.max_seq_length = MAX_TOKENS
        self.k = self.st.get_embedding_dimension()
        self.cfg = {"n_pairs": name}
        self.va, self.vb = {}, {}
        self.cache: dict[str, np.ndarray] = {}

    def warm(self) -> None:
        pass

    def embed(self, texts: list[str], batch: int = 64) -> np.ndarray:
        return self.st.encode(texts, batch_size=batch, normalize_embeddings=True,
                              convert_to_numpy=True, show_progress_bar=False).astype(np.float32)

    def fill(self, texts: set[str]) -> None:
        todo = sorted((t for t in texts if t not in self.cache), key=lambda t: (len(t), t))  # less padding
        for i in range(0, len(todo), 2048):
            part = todo[i : i + 2048]
            for t, v in zip(part, self.embed(part)):
                self.cache[t] = v
            print(f"    embedded {min(i + 2048, len(todo))}/{len(todo)}", flush=True)

    def vec(self, text: str | None) -> np.ndarray | None:
        if text is None:
            return None
        if text not in self.cache:
            self.cache[text] = self.embed([text])[0]
        return self.cache[text]

    def query(self, text: str) -> np.ndarray:
        return self.embed([QUERY_PREFIX.get(self.name, "") + text])[0]


_TM: Transformer | None = None


def build_rows(rel: str, source: str, model):
    """index.build_rows with the vectors taken from the transformer (same rows, bags and layout)."""
    if _TM is None or model is not _TM:
        return _orig_build_rows(rel, source, model)
    is_test = int(is_test_path(rel))
    path_s, _ = tokenize2(rel.replace("/", " ").replace(".", " ").replace("_", " "))
    path_bag = Counter(path_s)
    rows, bags, vecs = [], [], []
    for c in parse_source(rel, source):
        qual = c.qualname.rsplit(".", 1)[0] if "." in c.qualname else ""
        name_s, name_w = tokenize2(c.name)
        qual_s, qual_w = tokenize2(qual)
        sig_s, sig_w = tokenize2(c.sig, 80)
        doc_s, doc_w = tokenize2(c.doc, 200)
        body_s, body_w = tokenize2(c.body, BODY_CAP)
        bags.append((Counter(name_s + name_w), Counter(qual_s + qual_w), ixmod._bag(sig_s + sig_w, ixmod.TF_CAP),
                     ixmod._bag(doc_s + doc_w, ixmod.TF_CAP), ixmod._bag(body_s + body_w, ixmod.TF_CAP), path_bag))
        z = np.zeros((2, model.k), np.float16)
        for j, t in enumerate(view_texts(c)):
            v = model.vec(t)
            if v is not None:
                z[j] = v
        vecs.append(z)
        rows.append(((c.kind, c.name, c.qualname, name_key(c.name), name_key(c.qualname), c.start, c.end,
                      is_test, len(doc_s), len(body_s), ixmod.pack_text(c.text), " ".join(dict.fromkeys(name_s))),
                     sorted(c.calls)))
    return rows, pack_bags(bags, np.stack(vecs) if vecs else None)


_orig_build_rows = ixmod.build_rows
ixmod.build_rows = build_rows
_orig_semantic = Searcher._semantic


def _semantic(self, q, limit=100):
    if _TM is None or self.ix.model is not _TM:
        return _orig_semantic(self, q, limit)
    if not q.text.strip():
        return [], None
    qz = _TM.query(q.text)
    return self._store.semantic_top(qz, limit).tolist(), qz


Searcher._semantic = _semantic


def corpus_texts(root: str) -> set[str]:
    out = set()
    for rel in list_files(root):
        with open(os.path.join(root, rel), "rb") as f:
            src = decode_source(f.read())
        if src is None:
            continue
        for c in parse_source(rel, src):
            out.update(t for t in view_texts(c) if t)
    return out


def load_model(name: str, device: str = "cpu"):
    global _TM
    if name == "current":
        return SemanticModel.load(default_model_path())
    _TM = Transformer(name, device)
    return _TM


def cmd_features(name: str, tag: str, group: str) -> None:
    device = os.environ.get("DEVICE", "cpu")  # bulk embedding may run on "mps"; quality does not depend on it
    model = load_model(name, device)
    outdir = os.path.join(WORK, "feat", tag)
    os.makedirs(outdir, exist_ok=True)
    for variant in ("A", "B"):
        for pkg in (DEV if group == "DEV" else TEST):
            t0 = time.time()
            root = os.path.join(BENCH, "src", variant, pkg)
            if _TM is not None:
                _TM.fill(corpus_texts(root))
            ix = Index(root, model=model)
            st = ix.update(force=True, workers=1)
            s = Searcher(ix, ranker=None)
            recs = []
            for q in load_queries(pkg):
                _, rows, X = s.candidates(q["query"])
                y = np.array([1 if is_relevant(q, r["path"], r["qualname"] or "", r["name"] or "", r["kind"]) else 0
                              for r in rows], np.int8)
                recs.append({"type": q["type"], "X": X, "y": y,
                             "meta": [(r["path"], r["qualname"] or "", r["name"] or "") for r in rows], "q": q})
            with open(os.path.join(outdir, f"{variant}_{pkg}.pkl"), "wb") as f:
                pickle.dump({"features": FEATURES, "recs": recs}, f)
            print(f"{variant} {pkg}: {len(recs)} queries, {st.chunks} chunks, {time.time() - t0:.0f}s", flush=True)


def cmd_compare(tag: str, ranker: str, base_tag: str, base_ranker: str) -> None:
    from train_ranker import evaluate, load
    from scrutate_ref.rank import TreeEnsemble

    def ranks(t, r):
        recs = [x for rs in load(t, TEST).values() for x in rs]
        ens = TreeEnsemble.load(r)
        return {v: evaluate([x for x in recs if x["variant"] == v], ens) for v in ("A", "B")}

    new, base = ranks(tag, ranker), ranks(base_tag, base_ranker)
    print(f"{tag} vs {base_tag}  (MRR@10, hit@1/5/10, diff with 95% paired bootstrap CI)")
    for v in ("A", "B"):
        for t in new[v]:
            for sysname in ("semantic only", "linear blend", "LEARNED ranker"):
                a, b = new[v][t][sysname], base[v][t][sysname]
                assert len(a) == len(b)
                s = summarize(a)
                m, lo, hi = paired_diff_ci(a, b)
                print(f"{v} {t:<4} {sysname:<15} n={s['n']:<5} MRR {s['MRR']:.3f} (base {np.mean(mrr_values(b)):.3f}) "
                      f"hit {s['hit@1']:.3f}/{s['hit@5']:.3f}/{s['hit@10']:.3f}  diff {m:+.3f} [{lo:+.3f},{hi:+.3f}]")


def cmd_speed(name: str, n: int) -> None:
    from scrutate_ref.text import tokenize, tokenize_words
    chunks = []
    for pkg in TEST:
        root = os.path.join(BENCH, "src", "A", pkg)
        for rel in list_files(root):
            with open(os.path.join(root, rel), "rb") as f:
                src = decode_source(f.read())
            if src is not None:
                chunks += [c for c in parse_source(rel, src) if c.kind != "window"]
    random.Random(SEED).shuffle(chunks)
    chunks = chunks[:n]
    rss0 = resource.getrusage(resource.RUSAGE_SELF).ru_maxrss
    t0 = time.perf_counter()
    model = load_model(name)
    t_load = time.perf_counter() - t0
    if name == "current":
        model.warm()

        def run():
            for c in chunks:
                qual = c.qualname.rsplit(".", 1)[0] if "." in c.qualname else ""
                model.encode_chunk(tokenize_words(c.name), tokenize_words(qual), tokenize(c.sig, 80),
                                   tokenize(c.doc, 200), [])
                model.encode_chunk([], [], [], [], tokenize(c.body, BODY_CAP))
    else:
        model.st.float()  # granite loads as bfloat16, which is several times slower on CPU
        texts = sorted((t for c in chunks for t in view_texts(c) if t), key=len)
        model.embed(texts[:64])  # warm-up

        def run():
            model.embed(texts)
    t1 = time.perf_counter()
    run()
    dt = time.perf_counter() - t1
    qs = [q["query"] for q in load_queries(TEST[0], ("doc",))[:100]]
    from scrutate_ref.text import query_terms
    t2 = time.perf_counter()
    for q in qs:
        model.encode_query(query_terms(q)) if name == "current" else model.query(q)
    print(f"{name}: one query at a time: {(time.perf_counter() - t2) * 1000 / len(qs):.2f} ms per query")
    mb = 2**20 if sys.platform == "darwin" else 2**10  # ru_maxrss: bytes on macOS, KB on Linux
    print(f"{name}: {len(chunks)} chunks (2 views) in {dt:.1f}s = {dt * 1000 / len(chunks):.2f} s per 1000 chunks; "
          f"model load {t_load:.1f}s; threads {THREADS}; peak RSS {resource.getrusage(resource.RUSAGE_SELF).ru_maxrss / mb:.0f} MB "
          f"(before loading the model {rss0 / mb:.0f} MB)")


def _selfcheck() -> None:
    from scrutate_ref.parse import Chunk
    a, b = view_texts(Chunk("f", "C.f", "method", 1, 2, "", "def f(x):", "", "  return x"))
    assert a == "C.f\ndef f(x):" and b == "return x"
    assert view_texts(Chunk("", "", "window", 1, 2, "t", body="t")) == (None, "t")


if __name__ == "__main__":
    _selfcheck()
    if len(sys.argv) < 3:
        sys.exit(__doc__)
    cmd, args = sys.argv[1], sys.argv[2:]
    if cmd == "features":
        cmd_features(*args)
    elif cmd == "compare":
        cmd_compare(*args)
    elif cmd == "speed":
        cmd_speed(args[0], int(args[1]) if len(args) > 1 else 1000)
    else:
        sys.exit(__doc__)
