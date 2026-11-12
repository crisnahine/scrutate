"""The benchmark builders make types answers too, without moving a function query."""
import os
import random
import sys

sys.path.insert(0, os.path.join(os.path.dirname(__file__), "..", "..", "bench"))
import make_bench  # noqa: E402
import make_bench_code  # noqa: E402
from evalcore import is_relevant  # noqa: E402

PY = '''
class Holder:
    """Holds the pillar umbra summit cargo state."""

    class Inner:
        """Keeps the inner river anchor beacon value."""

    def fetch_value(self):
        """Fetches the stored value from the holder."""


class TestHolder:
    """Tests the holder with several cases here."""
'''

RS = '''/// Holds the pillar umbra summit cargo state.
#[derive(Debug)]
pub struct Holder<T> { x: T }

/// The methods of a holder for everyone.
impl<T> Holder<T> {
    /// Fetches the stored value from the holder.
    pub fn fetch_value(&self) -> &T { &self.x }
}

/// Names the languages the parser knows about.
pub enum Lang { Rust, Go }
'''


def test_python_classes_are_type_targets():
    targets = make_bench.process_file(PY, "m.py", with_targets=True)[2]
    got = {(t["qualname"], t["target_kind"]) for t in targets}
    assert got == {("Holder", "type"), ("Holder.Inner", "type"), ("Holder.fetch_value", "function")}


def test_rust_types_are_targets_and_an_impl_is_not():
    targets = make_bench_code.process_file(RS, "src/m.rs", "rust", with_targets=True)[2]
    got = {(t["qualname"], t["target_kind"]) for t in targets}
    assert got == {("Holder", "type"), ("Lang", "type"), ("Holder.fetch_value", "function")}


def _targets(n_func, n_type):
    words = "alpha bravo cargo delta ember fable gamma harbor ivory jungle".split()
    out = []
    for i in range(n_func + n_type):
        kind = "function" if i < n_func else "type"
        name = f"do_thing_{i}" if kind == "function" else f"ThingHolder{i}"
        out.append({"name": name, "qualname": f"Owner.{name}", "path": f"p{i % 3}.py", "target_kind": kind,
                    "query": f"{words[i % 10]} {words[(i * 3) % 10]} {words[(i * 7) % 10]} number {i} here"})
    return out


def test_type_queries_move_no_function_query():
    funcs = _targets(40, 0)
    both = _targets(40, 25)
    a = make_bench.make_queries(funcs, random.Random(1234), random.Random("types x"))
    b = make_bench.make_queries(both, random.Random(1234), random.Random("types x"))
    fb = [q for q in b if q["target_kind"] == "function"]
    assert a == fb and b[: len(fb)] == fb
    types = [q for q in b if q["target_kind"] == "type"]
    assert {q["type"] for q in types} == {"doc", "kw", "name", "id", "qid"}


def test_a_type_query_is_answered_by_the_type_alone():
    q = {"match": "name", "name": "Holder", "target_kind": "type"}
    assert is_relevant(q, "a.rs", "Holder", "Holder", "struct")
    assert not is_relevant(q, "a.rs", "Holder", "Holder", "impl")
    assert not is_relevant(q, "A.java", "Holder.Holder", "Holder", "method")
    f = {"match": "name", "name": "fetch"}
    assert is_relevant(f, "a.py", "fetch", "fetch", "function") and is_relevant(f, "a.py", "fetch", "fetch")


def test_the_groups_are_disjoint():
    import groups

    py = [groups.PRETRAIN, groups.DEV, groups.TEST, groups.SEALED]
    code = [groups.CODE_DEV, groups.CODE_TEST, groups.CODE_SEALED]
    for gs in (py, code):
        names = [p for g in gs for p in g]
        assert len(names) == len(set(names)), sorted(p for p in set(names) if names.count(p) > 1)
    assert {t for g in code for t in g} == set(groups.CODE)
