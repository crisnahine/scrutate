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


def test_calls_inside_rust_macro_arguments_are_calls():
    src = ('fn f() {\n    println!("{}", a::b(x), y.m(z));\n    assert_eq!(vec![g(1)], if k(2) { 1 } else { 0 });\n'
           '    let _ = json!({"tools": tools(structured)});\n    assert!(matches!(first(s), Some(Pat(_)) | Whole(_)));\n'
           '    quote! { fn declared(x: u8) {} #[derive(Debug)] struct S(u8); };\n}\n')
    f = next(c for c in parse_source("m.rs", src) if c.name == "f")
    assert sorted(f.calls) == ["assert", "assert_eq", "b", "first", "g", "json", "k", "m", "matches", "println",
                               "quote", "tools", "vec"]


def test_chunk_text_has_a_byte_cap_on_a_one_line_file_of_nested_functions():
    from scrutate_ref.parse import MAX_CHUNK_BYTES

    n = 3000
    chunks = parse_source("a.js", "function a(){" * n + "}" * n)
    assert len(chunks) == n
    assert max(len(c.text.encode()) for c in chunks) == MAX_CHUNK_BYTES + 4
    assert all(len(c.body.encode()) <= MAX_CHUNK_BYTES + 4 for c in chunks)
    wide = 'function w() { "%s" }' % ("é" * MAX_CHUNK_BYTES)
    w = next(c for c in parse_source("w.js", wide) if c.name == "w")
    assert w.text.endswith("é\n...") and len(w.text.encode()) <= MAX_CHUNK_BYTES + 4


def test_a_rust_comment_a_blank_line_away_is_not_documentation():
    chunks = {c.name: c for c in parse_source("d.rs", "/// stray note\n\nfn f() {}\n/// Doc.\nfn g() {}\n")}
    assert (chunks["f"].doc, chunks["g"].doc) == ("", "Doc.")


def test_a_generic_owner_is_named_by_its_bare_type():
    rs = ("pub struct Stack<T> { items: Vec<T> }\nimpl<T> Stack<T> {\n    pub fn push(&mut self, x: T) {}\n}\n"
          "impl<'a, T: Clone> Wrapper<'a, T> {\n    pub fn get(&self) -> T { todo!() }\n}\n"
          "impl<T: Clone> Clone for Wrapper<'_, T> {\n    fn clone(&self) -> Self { todo!() }\n}\n"
          "impl std::fmt::Display for Wrapper<'_, u8> {\n    fn fmt(&self) {}\n}\nimpl crate::Thing {\n    pub fn run(&self) {}\n}\n")
    chunks = parse_source("more.rs", rs)
    c = by_q(chunks)
    for q, name in [("Stack.push", "push"), ("Wrapper.get", "get"), ("Wrapper.clone", "clone"), ("Wrapper.fmt", "fmt"),
                    ("Thing.run", "run")]:
        assert c[q].name == name
    assert [x.name for x in chunks if x.kind == "impl"] == ["Stack", "Wrapper", "Wrapper", "Wrapper", "Thing"]


def _consts(path, src):
    return [(c.qualname, c.text) for c in parse_source(path, src) if c.kind == "constant"]


def test_constants_are_symbols():
    rs = _consts("c.rs", "/// The cap.\npub const LIMIT: usize = 3;\npub static NAME: &str = \"x\";\nimpl<T> S<T> {\n"
                         "    const N: u8 = 1;\n    fn f() {\n        const IN: u8 = 2;\n    }\n}\nconst _: () = ();\n")
    assert [q for q, _ in rs] == ["LIMIT", "NAME", "S.N"] and rs[0][1] == "pub const LIMIT: usize = 3;"
    go = _consts("c.go", "package p\n\nconst A = 1\nconst (\n\tB = 2\n\tC\n)\nvar X, Y int\nvar (\n\tZ = 3\n)\nvar _ = 4\n"
                         "func f() { const L = 1 }\n")
    assert [q for q, _ in go] == ["A", "B", "C", "X", "Y", "Z"] and (go[0][1], go[1][1]) == ("const A = 1", "B = 2")
    ts = _consts("c.ts", "export const MAX = 3;\nconst f = () => 1, k = 2;\nlet notc = 1;\nconst { a, b } = o;\n"
                         "enum Color { Red, Green = 2 }\nfunction g() { const inner = 1; }\n")
    assert [q for q, _ in ts] == ["MAX", "k", "Color.Red", "Color.Green"] and ts[0][1] == "export const MAX = 3;"
    assert [q for q, _ in _consts("c.js", "export const MAX = 3;\nconst f = function () {};\nvar v = 1;\n")] == ["MAX"]
    java = _consts("C.java", "class C {\n  public static final int MAX = 3;\n  static int n;\n  final int m = 1;\n}\n")
    assert [q for q, _ in java] == ["C.MAX"]
    py = _consts("c.py", "LIMIT = 3\n_PRIVATE_CAP: int = 4\nlower = 5\nA, B = 1, 2\nclass K:\n    INNER = 1\n"
                         "def f():\n    LOCAL = 2\n")
    assert [q for q, _ in py] == ["LIMIT", "_PRIVATE_CAP"]


def test_constants_come_last_and_do_not_move_the_file_header():
    chunks = parse_source("m.rs", "use std::fmt;\n\npub const LIMIT: usize = 3;\n\nfn f() -> usize {\n    LIMIT\n}\n")
    assert [c.kind for c in chunks] == ["module", "function", "constant"]
    assert (chunks[0].start, chunks[0].end) == (1, 4)


def test_a_turbofish_call_is_a_call():
    f = next(c for c in parse_source("t.rs", "fn f() {\n    g::<u8>(1);\n    x.collect::<Vec<_>>();\n}\n") if c.name == "f")
    assert sorted(f.calls) == ["collect", "g"]


def test_every_name_of_a_declaration_is_a_constant_with_the_declaration_as_its_text():
    go = _consts("m.go", "package p\n\nvar X, Y int\nconst A, B = 1, 2\nconst (\n\tC, D = 3, 4\n\tH = 7\n)\nvar (\n\tE, F string\n)\n"
                         "var _, G = 5, 6\n")
    assert go == [("X", "var X, Y int"), ("Y", "var X, Y int"), ("A", "const A, B = 1, 2"), ("B", "const A, B = 1, 2"),
                  ("C", "C, D = 3, 4"), ("D", "C, D = 3, 4"), ("H", "H = 7"), ("E", "E, F string"), ("F", "E, F string"),
                  ("G", "var _, G = 5, 6")]
    java = _consts("M.java", "class M {\n  static final int A = 1, B = 2;\n  static int n, m;\n}\n")
    assert java == [("M.A", "static final int A = 1, B = 2;"), ("M.B", "static final int A = 1, B = 2;")]
    js = _consts("m.js", "const a = 1, b = 2;\nexport const c = 3, d = () => 4;\n")
    assert [q for q, _ in js] == ["a", "b", "c"] and js[1][1] == "const a = 1, b = 2;"


OBJECT_LITERALS = """const api = { get() { return 1; }, post: function () { return 2; }, put: () => 3, gen: function* () {}, "q": () => 5 };
export const store = { count: 0, increment() { return this.total(); }, total() { return this.count; }, sub: { m() {} }, later: async () => go() };
export default { methods: { save() { return 1; }, run() { return this.save(); } } };
module.exports.tool = { build() { return 4; } };
exports.kit = { fix() {} };
f({ cb() {} });
"""


def test_methods_of_an_object_literal_are_owned_by_what_holds_the_literal():
    for path in ("o.js", "o.ts"):
        chunks = parse_source(path, OBJECT_LITERALS)
        assert [(c.name, c.qualname) for c in chunks if c.kind == "method"] == [
            ("get", "api.get"), ("post", "api.post"), ("put", "api.put"), ("gen", "api.gen"),
            ("increment", "store.increment"), ("total", "store.total"), ("m", "store.sub.m"), ("later", "store.later"),
            ("save", "methods.save"), ("run", "methods.run"), ("build", "tool.build"), ("fix", "kit.fix"), ("cb", "cb")], path
        c = by_q(chunks)
        assert c["store.later"].calls == {"go"} and c["store.increment"].calls == {"total"}
