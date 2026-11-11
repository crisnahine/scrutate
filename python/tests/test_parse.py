from scrutate_ref.parse import parse_source, window_chunks

PY = '''"""Module doc about tokens."""
import os

class OAuth:
    """Handles oauth refresh."""
    def refresh_token(self, force: bool = False) -> str:
        """Refresh an expired access token.

        Returns the new token."""
        return self._post(url)

    @property
    def expired(self):
        return True

def helper(x):
    return helper2(x)
'''


def by_q(chunks):
    return {c.qualname: c for c in chunks}


def test_python_fields():
    c = by_q(parse_source("a.py", PY))
    m = c["OAuth.refresh_token"]
    assert m.kind == "method" and m.start == 6 and m.end == 10
    assert m.sig.startswith("def refresh_token(self, force: bool = False)")
    assert m.doc.startswith("Refresh an expired access token.") and "Returns the new token" in m.doc
    assert "Refresh an expired" not in m.body and "_post" in m.body
    assert m.calls == {"_post"}
    assert c["OAuth"].kind == "class" and c["OAuth"].doc == "Handles oauth refresh."
    assert c["helper"].kind == "function" and c["helper"].calls == {"helper2"}


def test_decorators_belong_to_the_signature_and_range():
    c = by_q(parse_source("a.py", PY))["OAuth.expired"]
    assert "@property" in c.sig and c.start == 12


def test_module_chunk_carries_module_docstring():
    mod = [c for c in parse_source("pkg/a.py", PY) if c.kind == "module"][0]
    assert mod.name == "a" and mod.doc == "Module doc about tokens."


def test_javascript_leading_comment_is_documentation():
    js = "/**\n * Fetch the user profile.\n */\nexport async function fetchUser(id) { return get(id); }\n"
    c = by_q(parse_source("a.js", js))["fetchUser"]
    assert "Fetch the user profile" in c.doc and c.calls == {"get"}


def test_unknown_language_falls_back_to_windows():
    chunks = parse_source("notes.md", "\n".join(f"line {i}" for i in range(120)))
    assert chunks and all(c.kind == "window" for c in chunks)
    assert chunks[0].start == 1 and chunks[-1].end == 120


def test_syntax_errors_do_not_crash():
    assert parse_source("bad.py", "def (:\n  <<<\n") is not None


def test_window_chunks_overlap():
    w = window_chunks("\n".join(str(i) for i in range(100)), size=50, overlap=10)
    assert w[1].start == 41


def test_qualified_names_of_deeply_nested_functions_keep_32_segments():
    n = 300
    chunks = parse_source("a.js", "function a(){\n" * n + "}" * n)
    assert len(chunks) == n
    assert max(c.qualname.count(".") + 1 for c in chunks) == 32
    assert chunks[-1].qualname == ".".join(["a"] * 32)


def _syms(path, src):
    return {(c.kind, c.qualname) for c in parse_source(path, src)}


def test_definitions_the_rust_engine_also_finds():
    rs = _syms("a.rs", "macro_rules! square {\n    ($x:expr) => { $x * $x };\n}\n\ntrait Runner {\n"
                       "    fn run(&self) -> u32;\n    fn name(&self) -> String { String::new() }\n}\n")
    assert {("macro", "square"), ("function", "Runner.run"), ("function", "Runner.name")} <= rs
    go = _syms("a.go", "package p\n\nfunc (s *Store) Get(key string) (int, bool) {\n\treturn 0, false\n}\n\n"
                       "func (b Box[T]) Put(v T) {}\n")
    assert {("method", "Store.Get"), ("method", "Box.Put")} <= go
    js = _syms("a.js", "class Store {\n  handle = (e) => { this.x = e; };\n  count = 3;\n}\n"
                       "module.exports.legacy = function legacy() {};\nexports.other = () => {};\n")
    assert {("method", "Store.handle"), ("function", "legacy"), ("function", "other")} <= js
    assert not any(q.endswith("count") for _, q in js)
    ts = _syms("a.ts", "abstract class Repo<T> {\n  abstract find(id: Id): T | undefined;\n  handle = (e: E) => { };\n}\n"
                       "declare function external(x: number): void;\nexport namespace Util {\n"
                       "  export function clamp(x: number): number { return x; }\n}\n")
    assert {("method", "Repo.find"), ("method", "Repo.handle"), ("function", "external"), ("module", "Util"),
            ("function", "Util.clamp")} <= ts
