# scrutate-ref

The reference engine of [Scrutate](../README.md), in Python. It is not the product: the product
is the Rust binary in `crates/scrutate`.

It parses, indexes and ranks the same way the Rust engine does, for six languages, and exists for
two jobs:

- **Parity.** `bench/parity.py` runs the same queries through both engines and fails on any
  difference in a feature, a score or a top 10 (docs/QUALITY.md, CONTRIBUTING.md).
- **Training.** `bench/train_ranker.py` and `bench/train_semantic.py` fit the ranker and the
  semantic vectors that the Rust engine ships (`models/`).

Install it for development and run its tests:

```sh
pip install -e "python[test]" -c python/constraints.txt
python -m pytest python/tests -q --ignore=python/tests/test_bindings.py
```

The Python bindings of the product are a separate package, `crates/scrutate-py`.
