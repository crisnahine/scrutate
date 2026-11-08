"""Search parity on an existing index (built by either engine): both engines read the same .scrutate
and must return identical candidates, features, scores and top-10. Exercises the IVF path when the
index is large enough (>= 20,000 chunks in one segment).

usage: parity_shared.py ROOT RUST_BINARY [--queries N]
"""
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "python"))

from parity import compare_search, load_queries, python_features, rust_features  # noqa: E402
from scrutate_ref.index import Index  # noqa: E402
from scrutate_ref.search import Searcher  # noqa: E402


def main():
    root, binary = sys.argv[1], sys.argv[2]
    nq = int(sys.argv[sys.argv.index("--queries") + 1]) if "--queries" in sys.argv else 100
    ix = Index(root)
    st = ix.stats()
    print(f"index: {st['chunks']} chunks, segments: {[s.n for s in ix.lex.segs]}, ivf: {[bool(s.ivf) for s in ix.lex.segs]}")
    queries = load_queries(root, nq)
    s = Searcher(ix)
    py = python_features(s, queries)
    rs = rust_features(binary, root, queries, features=True)
    problems = compare_search(py, rs, ix)
    print("PARITY OK" if not problems else "PARITY PROBLEMS: " + "; ".join(problems))
    return 0 if not problems else 1


if __name__ == "__main__":
    sys.exit(main())
