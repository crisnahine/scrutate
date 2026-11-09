#!/usr/bin/env python3
"""Train / evaluate the learned ranker on saved candidate features.

usage:
  train_ranker.py cv      TAG                  3-fold package-held-out CV on DEV features
  train_ranker.py fit     TAG OUT.npz          fit on all DEV features, save ensemble
  train_ranker.py eval    TAG [OUT.npz]        evaluate scorers on TEST features (+ ranker if given)
  train_ranker.py compare TAG OLD.npz NEW.npz [OUT.json]   two rankers on TEST, per language

DEV and TEST include the other-language packages of groups.CODE_DEV / CODE_TEST unless
PYTHON_ONLY=1. RANKER_PARAMS='{"n_estimators": 400}' overrides PARAMS (for tuning with cv).
"""
from __future__ import annotations

import glob
import json
import os
import pickle
import sys

import numpy as np

sys.path.insert(0, os.path.dirname(__file__))
sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "python"))
from evalcore import bootstrap_ci, fmt_row, mrr_values, paired_diff_ci, summarize  # noqa: E402
from groups import CODE_DEV, CODE_TEST, DEV, TEST, WORK, language  # noqa: E402

from scrutate_ref.rank import TreeEnsemble  # noqa: E402
from scrutate_ref.search import FEATURES, FI, linear_score, prune_indices  # noqa: E402

DROP = [p for p in os.environ.get("DROP", "").split(",") if p]  # ablation: zero these feature groups

PARAMS = dict(objective="lambdarank", metric="ndcg", n_estimators=250, learning_rate=0.06,
              num_leaves=31, min_child_samples=40, subsample=0.8, subsample_freq=1,
              colsample_bytree=0.8, reg_lambda=1.0, lambdarank_truncation_level=20,
              verbose=-1, n_jobs=2)
PARAMS.update(json.loads(os.environ.get("RANKER_PARAMS", "{}")))
if not os.environ.get("PYTHON_ONLY"):
    DEV, TEST = DEV + CODE_DEV, TEST + CODE_TEST
TYPES = ("doc", "kw", "name", "id", "qid")


def folds() -> list[list[str]]:
    """The Python folds; each other language's packages dealt round-robin over them."""
    out = [["pydantic", "werkzeug"], ["pptx", "seaborn", "joblib"], ["docx", "openpyxl"]]
    by_lang: dict[str, list[str]] = {}
    for p in DEV:
        if language(p) != "python":
            by_lang.setdefault(language(p).replace("typescript", "javascript"), []).append(p)
    for pkgs in by_lang.values():
        for i, p in enumerate(pkgs):
            out[i % 3].append(p)
    return out


def load(tag: str, pkgs: list[str]):
    out = {}
    for pkg in pkgs:
        recs = []
        for f in sorted(glob.glob(os.path.join(WORK, "feat", tag, f"*_{pkg}.pkl"))):
            with open(f, "rb") as fh:
                d = pickle.load(fh)
            assert d["features"] == FEATURES, "feature set changed; regenerate features"
            drop = [i for i, n in enumerate(FEATURES) if any(n.startswith(p) for p in DROP)]
            for r in d["recs"]:
                r["variant"] = os.path.basename(f).split("_")[0]
                r["lang"] = language(pkg)
                if drop:
                    r["X"] = r["X"].copy()
                    r["X"][:, drop] = 0.0
            recs += d["recs"]
        out[pkg] = recs
    return out


def pruned(rec):
    X, y = rec["X"], rec["y"]
    keep = prune_indices(linear_score(X))
    return X[keep], y[keep], keep


def build(recs):
    Xs, ys, groups = [], [], []
    for r in recs:
        if r["y"].sum() == 0:
            continue
        X, y, _ = pruned(r)
        if y.sum() == 0:
            continue
        Xs.append(X); ys.append(y); groups.append(len(y))
    return np.vstack(Xs), np.concatenate(ys), np.array(groups)


def fit(recs, params=None) -> "lightgbm.LGBMRanker":
    import lightgbm as lgb
    X, y, g = build(recs)
    m = lgb.LGBMRanker(**(params or PARAMS))
    m.fit(X, y, group=g)
    return m


def rank_of_first(scores, y, limit=10):
    order = np.argsort(-scores)[:limit]
    pos = np.nonzero(y[order])[0]
    return int(pos[0]) + 1 if len(pos) else None


SCORERS = {
    "bm25 (lexical)": lambda X: X[:, FI["lex_log"]],
    "semantic only": lambda X: X[:, FI["sem_mix"]],
    "linear blend": linear_score,
}


def evaluate(recs, ranker=None, types=TYPES):
    res = {}
    scorers = dict(SCORERS)
    if ranker is not None:
        def ltr(X):
            keep = prune_indices(linear_score(X))
            out = np.full(len(X), -1e9, np.float32)
            out[keep] = ranker.predict(X[keep])
            return out
        scorers["LEARNED ranker"] = ltr
    for t in types:
        rs = [r for r in recs if r["type"] == t]
        for name, fn in scorers.items():
            res.setdefault(t, {})[name] = [rank_of_first(fn(r["X"]), r["y"]) for r in rs]
    return res


def print_eval(res, title):
    print(f"\n=== {title}")
    for t, d in res.items():
        print(f"-- {t}")
        for name, ranks in d.items():
            print("  " + fmt_row(name, ranks))
        if "LEARNED ranker" in d:
            m, lo, hi = paired_diff_ci(d["LEARNED ranker"], d["bm25 (lexical)"])
            print(f"  learned - bm25 MRR diff: {m:+.3f} [{lo:+.3f},{hi:+.3f}]")


def main() -> None:
    cmd, tag = sys.argv[1], sys.argv[2]
    if cmd == "cv":
        data = load(tag, DEV)
        allres: dict = {}
        print("params:", PARAMS)
        langs = sorted({r["lang"] for rs in data.values() for r in rs})
        for held in folds():
            train = [r for p, rs in data.items() if p not in held for r in rs]
            m = fit(train)
            ens = TreeEnsemble.from_lightgbm(m.booster_, FEATURES)
            for lang in ["all"] + langs:
                res = evaluate([r for p in held for r in data[p] if lang in ("all", r["lang"])], ens)
                for t, d in res.items():
                    for name, ranks in d.items():
                        allres.setdefault(lang, {}).setdefault(t, {}).setdefault(name, []).extend(ranks)
            print(f"held-out {held}: done", flush=True)
        for lang, res in allres.items():
            print_eval(res, f"3-fold package-held-out CV on DEV, {lang}")
            mrr = np.mean([v for d in res.values() for v in mrr_values(d["LEARNED ranker"])])
            print(f"  learned ranker MRR@10 over all {lang} queries: {mrr:.4f}")
    elif cmd == "fit":
        data = load(tag, DEV)
        recs = [r for rs in data.values() for r in rs]
        m = fit(recs)
        ens = TreeEnsemble.from_lightgbm(m.booster_, FEATURES)
        # equivalence check: exported numpy inference == lightgbm
        X = np.vstack([r["X"] for r in recs[:50]])
        d = np.abs(ens.predict(X) - m.booster_.predict(X)).max()
        print(f"export check: max |numpy - lightgbm| = {d:.2e}")
        ens.save(sys.argv[3])
        imp = sorted(zip(m.booster_.feature_importance("gain"), FEATURES), reverse=True)[:12]
        print("top features by gain:", ", ".join(f"{n}" for _, n in imp))
        print("saved", sys.argv[3], f"{os.path.getsize(sys.argv[3])/1e3:.0f} KB")
    elif cmd == "eval":
        data = load(tag, TEST)
        ens = TreeEnsemble.load(sys.argv[3]) if len(sys.argv) > 3 else None
        recs = [r for rs in data.values() for r in rs]
        for variant in ("A", "B"):
            print_eval(evaluate([r for r in recs if r["variant"] == variant], ens),
                       f"TEST (held-out packages), variant {variant}")
    elif cmd == "compare":
        data = load(tag, TEST)
        old, new = TreeEnsemble.load(sys.argv[3]), TreeEnsemble.load(sys.argv[4])
        recs = [r for rs in data.values() for r in rs]
        out: dict = {}
        for lang in sorted({r["lang"] for r in recs}):
            for variant in ("A", "B"):
                sub = [r for r in recs if r["lang"] == lang and r["variant"] == variant]
                a, b = evaluate(sub, old), evaluate(sub, new)
                print(f"\n=== TEST {lang}, variant {variant}: old ranker -> new ranker")
                for t in TYPES:
                    ro, rn = a[t]["LEARNED ranker"], b[t]["LEARNED ranker"]
                    if not ro:
                        continue
                    so, sn = summarize(ro), summarize(rn)
                    (lo1, hi1), (lo2, hi2) = bootstrap_ci(mrr_values(ro)), bootstrap_ci(mrr_values(rn))
                    m, lo, hi = paired_diff_ci(rn, ro)
                    bm = summarize(a[t]["bm25 (lexical)"])["MRR"]
                    print(f"  {t:<5} n={so['n']:<5} bm25 {bm:.3f}  old {so['MRR']:.3f} [{lo1:.3f},{hi1:.3f}]  "
                          f"new {sn['MRR']:.3f} [{lo2:.3f},{hi2:.3f}]  new-old {m:+.3f} [{lo:+.3f},{hi:+.3f}]  "
                          f"hit@1/5/10 old {so['hit@1']:.3f}/{so['hit@5']:.3f}/{so['hit@10']:.3f} "
                          f"new {sn['hit@1']:.3f}/{sn['hit@5']:.3f}/{sn['hit@10']:.3f}")
                    out.setdefault(lang, {}).setdefault(variant, {})[t] = {
                        "n": so["n"], "bm25": bm, "old": {**so, "ci": [lo1, hi1]},
                        "new": {**sn, "ci": [lo2, hi2]}, "new_minus_old": [m, lo, hi]}
        if len(sys.argv) > 5:
            with open(sys.argv[5], "w") as f:
                json.dump(out, f, indent=1)


if __name__ == "__main__":
    main()
