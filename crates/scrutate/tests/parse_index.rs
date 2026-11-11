use std::time::Instant;

use scrutate::parse::parse_source;
use scrutate::search::prune_indices;

#[test]
fn candidate_cut_keeps_the_lowest_indices_on_tied_scores() {
    let lin = vec![1.0f32; 300];
    let mut keep = prune_indices(&lin, 200);
    keep.sort_unstable();
    assert_eq!(keep, (0..200).collect::<Vec<_>>());
    let mut lin = vec![0.5f32; 10];
    lin[7] = 2.0;
    let mut keep = prune_indices(&lin, 3);
    keep.sort_unstable();
    assert_eq!(keep, vec![0, 1, 7]);
}

/// Parses `make(n)` and `make(4 * n)` (best of 3 runs each, so a loaded machine does not decide)
/// and fails if the larger input costs 10x or more, where linear work gives 4x and quadratic 16x.
/// The parse is the one the index runs: a chunk's text is cut later, one chunk at a time.
fn assert_parse_scales_linearly(path: &str, n: usize, make: impl Fn(usize) -> String, check: impl Fn(usize, &[scrutate::parse::Chunk])) {
    let best = |k: usize| {
        let src = make(k);
        (0..3)
            .map(|_| {
                let t = Instant::now();
                let chunks = scrutate::parse::parse_file_with(path, &src, None).0.chunks;
                let took = t.elapsed();
                check(k, &chunks);
                took
            })
            .min()
            .unwrap()
    };
    let (small, large) = (best(n), best(4 * n));
    // a floor, so that a run too short to measure does not make the ratio
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.005);
    assert!(ratio < 10.0, "{path}: {n} items took {small:?}, {} took {large:?}", 4 * n);
}

#[test]
fn deeply_nested_functions_index_in_bounded_time_and_size() {
    for (sep, n) in [("\n", 500), ("", 2500)] {
        let make = |n: usize| format!("{}{}", format!("function a(){{{sep}").repeat(n), "}".repeat(n));
        assert_parse_scales_linearly("a.js", n, make, |n, chunks| {
            assert_eq!(chunks.len(), n);
            let deepest = chunks.iter().map(|c| c.qualname.split('.').count()).max().unwrap();
            assert_eq!(deepest, 32);
            let qual_bytes: usize = chunks.iter().map(|c| c.qualname.len()).sum();
            assert!(qual_bytes < 64 * n, "{qual_bytes} bytes of qualified names");
        });
    }
}

#[test]
fn a_node_with_many_children_parses_in_linear_time() {
    assert_parse_scales_linearly("a.scm", 5000, |n| "(f ".repeat(n), |_, _| {});
}

#[test]
fn a_file_of_many_sibling_definitions_parses_in_linear_time() {
    for (path, line) in [
        ("w.js", "function f{i}(x) {{ return x + {i}; }}"),
        ("w.c", "int f{i}(int x) {{ return x + {i}; }}"),
        ("w.go", "func f{i}(x int) int {{ return x + {i} }}"),
    ] {
        let make = |n: usize| {
            let body: String = (0..n).map(|i| line.replace("{i}", &i.to_string()).replace("{{", "{").replace("}}", "}") + "\n").collect();
            if path.ends_with(".go") {
                format!("package p\n{body}")
            } else {
                body
            }
        };
        assert_parse_scales_linearly(path, 1000, make, |n, chunks| {
            assert_eq!(chunks.iter().filter(|c| c.kind == "function").count(), n, "{path}");
        });
    }
}

#[test]
fn the_model_signature_is_a_hash_of_the_model_file() {
    use blake2::digest::{Update, VariableOutput};
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../models/semantic.npz");
    let bytes = std::fs::read(&path).unwrap();
    let mut h = blake2::Blake2bVar::new(16).unwrap();
    h.update(&bytes);
    let mut out = [0u8; 16];
    h.finalize_variable(&mut out).unwrap();
    // the text python/scrutate_ref computes: "blake2b:" + hashlib.blake2b(data, digest_size=16).hexdigest()
    let want = format!("blake2b:{}", out.iter().map(|b| format!("{b:02x}")).collect::<String>());
    assert_eq!(scrutate::semantic::SemanticModel::load(&path).unwrap().signature(), want);
    assert_eq!(scrutate::semantic::SemanticModel::from_bytes(&bytes).unwrap().signature(), want);
}

fn symbols(path: &str, src: &str) -> Vec<(String, String)> {
    let stem = path.rsplit('/').next().unwrap().split('.').next().unwrap();
    let file_chunk = |c: &scrutate::parse::Chunk| c.kind == "window" || (c.kind == "module" && c.qualname == stem);
    parse_source(path, src).into_iter().filter(|c| !file_chunk(c)).map(|c| (c.kind.to_string(), c.qualname)).collect()
}

#[test]
fn the_heuristic_parser_reads_past_modifiers_and_skips_prose() {
    let cr = "abstract class Shape\n  private def area\n    0\n  end\nend\n";
    let got = symbols("shape.cr", cr);
    assert!(got.contains(&("class".into(), "Shape".into())), "{got:?}");
    assert!(got.contains(&("function".into(), "area".into())), "{got:?}");
    if scrutate::langs::by_id("ocaml").is_none() {
        assert_eq!(symbols("m.ml", "let add x y =\n  x + y\n\nlet limit = 5\n"), vec![("function".to_string(), "add".to_string())]);
    }
    let kconfig = "config FOO\n\thelp\n\t  To compile this as a module, choose M here: the\n\t  module will be called foo.\n";
    assert_eq!(symbols("drivers/Kconfig", kconfig), vec![]);
    assert_eq!(symbols("notes.txt", "module will be called foo\nclass of service\n"), vec![]);
}

fn has(got: &[(String, String)], kind: &str, qualname: &str) -> bool {
    got.iter().any(|(k, q)| k == kind && q == qualname)
}

#[test]
fn rust_macros_and_trait_methods_without_a_body_are_symbols() {
    let got = symbols("a.rs", "macro_rules! square {\n    ($x:expr) => { $x * $x };\n}\n\ntrait Runner {\n    fn run(&self) -> u32;\n    fn name(&self) -> String { String::new() }\n}\n");
    assert!(has(&got, "macro", "square") && has(&got, "function", "Runner.run") && has(&got, "function", "Runner.name"), "{got:?}");
}

#[test]
fn go_methods_are_qualified_by_their_receiver_type() {
    let got = symbols("a.go", "package p\n\nfunc (s *Store) Get(key string) (int, bool) {\n\treturn 0, false\n}\n\nfunc (b Box[T]) Put(v T) {}\n");
    assert!(has(&got, "method", "Store.Get") && has(&got, "method", "Box.Put"), "{got:?}");
    let c = parse_source("a.go", "package p\n\nfunc (s *Store) Get() {}\n").into_iter().find(|c| c.qualname == "Store.Get").unwrap();
    assert_eq!(c.name, "Get");
}

#[test]
fn cpp_methods_defined_outside_the_class_are_named_by_the_method() {
    let chunks = parse_source("v.cpp", "void Widget::draw() {}\nvoid ns::Widget::paint() {}\n");
    let draw = chunks.iter().find(|c| c.name == "draw").expect("draw");
    assert_eq!(draw.qualname, "Widget.draw");
    assert!(chunks.iter().any(|c| c.name == "paint" && c.qualname == "ns.Widget.paint"));
}

#[test]
fn cpp_methods_declared_in_a_class_body_are_symbols() {
    let src = "class Shape {\npublic:\n  virtual double area() const = 0;\n  void resize(int w);\n  int x;\n  int (*fp)(int);\n  friend void swap(Shape&, Shape&);\n};\nvoid free_decl(int);\n";
    let got = symbols("s.hpp", src);
    assert_eq!(
        got,
        vec![
            ("class".into(), "Shape".into()),
            ("method".into(), "Shape.area".into()),
            ("method".into(), "Shape.resize".into()),
            ("function".into(), "free_decl".into())
        ]
    );
}

#[test]
fn c_and_cpp_function_like_macros_are_symbols() {
    let src = "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n#define LIMIT 5\n";
    for path in ["m.h", "m.hpp"] {
        let got = symbols(path, src);
        assert_eq!(got, vec![("macro".to_string(), "MAX".to_string()), ("constant".to_string(), "LIMIT".to_string())], "{path}");
    }
}

#[test]
fn js_class_field_functions_and_commonjs_exports_are_symbols() {
    let got = symbols(
        "a.js",
        "class Store {\n  handle = (e) => { this.x = e; };\n  count = 3;\n}\nmodule.exports.legacy = function legacy() {};\nexports.other = () => {};\n",
    );
    assert!(has(&got, "method", "Store.handle") && has(&got, "function", "legacy") && has(&got, "function", "other"), "{got:?}");
    assert!(!got.iter().any(|(_, q)| q.ends_with("count")), "{got:?}");
    let got = symbols("a.ts", "class Store {\n  handle = (e: E) => { };\n}\n");
    assert!(has(&got, "method", "Store.handle"), "{got:?}");
}

#[test]
fn ts_abstract_methods_declared_functions_and_namespaces_are_symbols() {
    let got = symbols(
        "a.ts",
        "abstract class Repo<T> {\n  abstract find(id: Id): T | undefined;\n}\ndeclare function external(x: number): void;\nexport namespace Util {\n  export function clamp(x: number): number { return x; }\n}\n",
    );
    assert!(
        has(&got, "method", "Repo.find") && has(&got, "function", "external") && has(&got, "module", "Util") && has(&got, "function", "Util.clamp"),
        "{got:?}"
    );
}

fn tree(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let root = std::env::temp_dir().join(format!("scrutate_parse_index_{name}_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (rel, body) in files {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
    }
    std::fs::canonicalize(root).unwrap()
}

#[test]
fn inside_git_the_ignore_rules_decide_for_build_dist_target_and_venv() {
    let files = [
        ("build/maker.py", "def tracked_builder():\n    pass\n"),
        ("target/gen.rs", "fn g() {}\n"),
        ("dist/out.py", "def x():\n    pass\n"),
        ("node_modules/m/i.js", "function m() {}\n"),
        ("src/a.py", "def a():\n    pass\n"),
        (".gitignore", "dist/\n"),
    ];
    let repo = tree("repo", &files);
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    assert_eq!(scrutate::files::list_files(&repo), vec![".gitignore", "build/maker.py", "src/a.py", "target/gen.rs"]);
    let mut m = scrutate::files::Membership::new(&repo);
    assert!(m.may_contain(&repo.join("build/new.py")));
    assert!(!m.may_contain(&repo.join("dist/new.py")));
    let plain = tree("plain", &files);
    assert_eq!(scrutate::files::list_files(&plain), vec![".gitignore", "src/a.py"]);
    let mut m = scrutate::files::Membership::new(&plain);
    assert!(!m.may_contain(&plain.join("build/new.py")));
}

#[test]
fn calls_inside_rust_macro_arguments_are_calls() {
    let src = r#"fn f() {
    println!("{}", a::b(x), y.m(z));
    assert_eq!(vec![g(1)], if k(2) { 1 } else { 0 });
    let _ = json!({"tools": tools(structured)});
    assert!(matches!(first(s), Some(Pat(_)) | Whole(_)));
    quote! { fn declared(x: u8) {} #[derive(Debug)] struct S(u8); };
}
"#;
    let chunks = parse_source("m.rs", src);
    let f = chunks.iter().find(|c| c.name == "f").unwrap();
    let calls: Vec<&str> = f.calls.iter().map(|s| s.as_str()).collect();
    assert_eq!(calls, ["assert", "assert_eq", "b", "first", "g", "json", "k", "m", "matches", "println", "quote", "tools", "vec"]);
}

#[test]
fn chunk_text_has_a_byte_cap_on_a_one_line_file_of_nested_functions() {
    use scrutate::parse::MAX_CHUNK_BYTES;
    let n = 8000;
    let src = format!("{}{}", "function a(){".repeat(n), "}".repeat(n));
    let chunks = parse_source("a.js", &src);
    assert_eq!(chunks.len(), n);
    for c in &chunks {
        assert!(c.text.len() <= MAX_CHUNK_BYTES + 4 && c.body.len() <= MAX_CHUNK_BYTES + 4, "{} bytes", c.text.len());
    }
    let total: usize = chunks.iter().map(|c| c.text.len() + c.body.len()).sum();
    assert!(total < 600_000_000, "{total} bytes of chunk text");
    // cut at a character, never inside one
    let wide = format!("function w() {{ \"{}\" }}", "\u{e9}".repeat(MAX_CHUNK_BYTES));
    let w = parse_source("w.js", &wide).into_iter().find(|c| c.name == "w").unwrap();
    assert!(w.text.ends_with("\u{e9}\n...") && w.text.len() <= MAX_CHUNK_BYTES + 4);
}

#[test]
fn cpp_constructors_destructors_member_templates_and_conversions_are_methods() {
    let src = "class Shape {\npublic:\n  Shape();\n  explicit Shape(int w);\n  virtual ~Shape();\n  template <class U> void g(U);\n  operator bool() const;\n  Shape& operator=(const Shape&);\n  Shape* next();\n  int x;\n  void (*fp)(int);\n};\nShape make();\n";
    let got: Vec<(String, String)> = parse_source("s.hpp", src).iter().map(|c| (c.kind.to_string(), c.qualname.clone())).collect();
    let want: Vec<(String, String)> = [
        ("class", "Shape"),
        ("method", "Shape.Shape"),
        ("method", "Shape.Shape"),
        ("method", "Shape.~Shape"),
        ("method", "Shape.g"),
        ("method", "Shape.operator bool"),
        ("method", "Shape.operator="),
        ("method", "Shape.next"),
        ("function", "make"),
    ]
    .iter()
    .map(|(k, q)| (k.to_string(), q.to_string()))
    .collect();
    assert_eq!(got, want);
}

#[test]
fn a_chunk_ends_on_the_last_line_that_holds_its_text() {
    let src = "#include <x.h>\n#define MAX(a, b) ((a) > (b) ? (a) : (b))\ntypedef int T;\n#define LONG(a) \\\n  ((a) + 1)\nint f(void) { return 0; }\n";
    let got: Vec<(String, i64, i64)> = parse_source("m.c", src).iter().map(|c| (c.name.clone(), c.start, c.end)).collect();
    assert_eq!(got[1..], [("MAX".into(), 2, 2), ("T".into(), 3, 3), ("LONG".into(), 4, 5), ("f".into(), 6, 6)]);
    // a Rust line comment ends with its line break: one a blank line away is not documentation
    let rs = parse_source("d.rs", "/// stray note\n\nfn f() {}\n/// Doc.\nfn g() {}\n");
    let doc = |n: &str| rs.iter().find(|c| c.name == n).unwrap().doc.clone();
    assert_eq!((doc("f"), doc("g")), (String::new(), "Doc.".to_string()));
}

#[test]
fn an_escaped_comma_is_part_of_an_excluded_directory_name() {
    assert_eq!(scrutate::files::split_names(r"gen\,dir, vendor ,,a\b"), vec!["gen,dir", "vendor", r"a\b"]);
}

fn find<'a>(chunks: &'a [scrutate::parse::Chunk], qualname: &str) -> &'a scrutate::parse::Chunk {
    chunks.iter().find(|c| c.qualname == qualname).unwrap_or_else(|| panic!("no {qualname} in {:?}", chunks.iter().map(|c| &c.qualname).collect::<Vec<_>>()))
}

#[test]
fn a_generic_owner_is_named_by_its_bare_type() {
    let rs = "pub struct Stack<T> { items: Vec<T> }\nimpl<T> Stack<T> {\n    pub fn push(&mut self, x: T) {}\n}\n\
              impl<'a, T: Clone> Wrapper<'a, T> {\n    pub fn get(&self) -> T { todo!() }\n}\n\
              impl<T: Clone> Clone for Wrapper<'_, T> {\n    fn clone(&self) -> Self { todo!() }\n}\n\
              impl std::fmt::Display for Wrapper<'_, u8> {\n    fn fmt(&self) {}\n}\nimpl crate::Thing {\n    pub fn run(&self) {}\n}\n";
    let chunks = parse_source("more.rs", rs);
    for (q, name) in [("Stack.push", "push"), ("Wrapper.get", "get"), ("Wrapper.clone", "clone"), ("Wrapper.fmt", "fmt"), ("Thing.run", "run")] {
        assert_eq!(find(&chunks, q).name, name);
    }
    let impls: Vec<&str> = chunks.iter().filter(|c| c.kind == "impl").map(|c| c.name.as_str()).collect();
    assert_eq!(impls, ["Stack", "Wrapper", "Wrapper", "Wrapper", "Thing"]);
    let cpp = parse_source("box.cpp", "template <class T>\nvoid Box<T>::put(T x) {}\nvoid ns::Map<K, std::vector<V>>::get() {}\n");
    assert_eq!(find(&cpp, "Box.put").name, "put");
    assert_eq!(find(&cpp, "ns.Map.get").name, "get");
}

#[test]
fn a_dotted_definition_name_is_its_last_part_under_its_owner() {
    let lua =
        parse_source("mod.lua", "local M = {}\n\nfunction M.helper(x)\n  return x\nend\n\nfunction M.sub.deep(x)\n  return x\nend\n\nfunction M:meth()\nend\n");
    for (q, name) in [("M.helper", "helper"), ("M.sub.deep", "deep"), ("M.meth", "meth")] {
        assert_eq!(find(&lua, q).name, name);
    }
    let caller = parse_source("use.lua", "local function go()\n  return M.helper(1)\nend\n");
    assert!(find(&caller, "go").calls.contains("helper"));
    let ex =
        parse_source("mix.ex", "defmodule MyApp.Accounts.User do\n  def changeset(user), do: user\nend\n\ndefprotocol MyApp.Sizeable do\n  def size(x)\nend\n");
    assert_eq!(find(&ex, "MyApp.Accounts.User").name, "User");
    assert_eq!(find(&ex, "MyApp.Accounts.User.changeset").name, "changeset");
    assert_eq!(find(&ex, "MyApp.Sizeable").name, "Sizeable");
    // a dot is part of an R name
    let r = parse_source("stats.R", "read.table2 <- function(x) x\nprint.myclass <- function(x) x\n");
    assert_eq!(find(&r, "read.table2").name, "read.table2");
    assert_eq!(find(&r, "print.myclass").name, "print.myclass");
}

fn constants(path: &str, src: &str) -> Vec<(String, String, String)> {
    parse_source(path, src).into_iter().filter(|c| c.kind == "constant").map(|c| (c.name, c.qualname, c.text)).collect()
}

fn named(got: &[(String, String, String)]) -> Vec<&str> {
    got.iter().map(|(_, q, _)| q.as_str()).collect()
}

#[test]
fn constants_are_symbols_in_every_language_the_reference_has() {
    let rs = constants(
        "c.rs",
        "/// The cap.\npub const LIMIT: usize = 3;\npub static NAME: &str = \"x\";\nimpl<T> S<T> {\n    const N: u8 = 1;\n    fn f() {\n        const IN: u8 = 2;\n    }\n}\nconst _: () = ();\n",
    );
    assert_eq!(named(&rs), ["LIMIT", "NAME", "S.N"]);
    assert_eq!(rs[0].2, "pub const LIMIT: usize = 3;");
    let go = constants("c.go", "package p\n\nconst A = 1\nconst (\n\tB = 2\n\tC\n)\nvar X, Y int\nvar (\n\tZ = 3\n)\nvar _ = 4\nfunc f() { const L = 1 }\n");
    assert_eq!(named(&go), ["A", "B", "C", "X", "Y", "Z"]);
    assert_eq!((go[0].2.as_str(), go[1].2.as_str()), ("const A = 1", "B = 2"));
    let ts_src = "export const MAX = 3;\nconst f = () => 1, k = 2;\nlet notc = 1;\nconst { a, b } = o;\nenum Color { Red, Green = 2 }\nfunction g() { const inner = 1; }\n";
    let ts = constants("c.ts", ts_src);
    assert_eq!(named(&ts), ["MAX", "k", "Color.Red", "Color.Green"]);
    assert_eq!(ts[0].2, "export const MAX = 3;");
    assert_eq!(named(&constants("c.js", "export const MAX = 3;\nconst f = function () {};\nvar v = 1;\n")), ["MAX"]);
    let java = constants("C.java", "class C {\n  public static final int MAX = 3;\n  static int n;\n  final int m = 1;\n}\n");
    assert_eq!(named(&java), ["C.MAX"]);
    let py = constants("c.py", "LIMIT = 3\n_PRIVATE_CAP: int = 4\nlower = 5\nA, B = 1, 2\nclass K:\n    INNER = 1\ndef f():\n    LOCAL = 2\n");
    assert_eq!(named(&py), ["LIMIT", "_PRIVATE_CAP", "A", "B", "K.INNER"]);
    let c = constants("c.h", "#define LIMIT 5\n#define GUARD_H\n#define MAX(a, b) ((a) > (b) ? (a) : (b))\nenum Color { RED, GREEN = 2 };\n");
    assert_eq!(named(&c), ["LIMIT", "Color.RED", "Color.GREEN"]);
    assert_eq!(named(&constants("c.cpp", "#define LIMIT 5\nenum class Mode { On, Off };\n")), ["LIMIT", "Mode.On", "Mode.Off"]);
}

#[test]
fn constants_come_last_and_do_not_move_the_file_header() {
    let src = "use std::fmt;\n\npub const LIMIT: usize = 3;\n\nfn f() -> usize {\n    LIMIT\n}\n";
    let chunks = parse_source("m.rs", src);
    let kinds: Vec<&str> = chunks.iter().map(|c| c.kind).collect();
    assert_eq!(kinds, ["module", "function", "constant"]);
    assert_eq!((chunks[0].start, chunks[0].end), (1, 4));
}

#[test]
fn constants_are_found_by_name_but_never_ranked() {
    let root = tree(
        "constants",
        &[("src/lib.rs", "/// How many retries.\npub const RETRY_LIMIT: usize = 3;\n\n/// Retries a request up to the limit.\npub fn retry_request() -> usize {\n    RETRY_LIMIT\n}\n")],
    );
    let mut repo = scrutate::Repo::open(&root).unwrap();
    repo.update(false).unwrap();
    let hits = repo.find_symbol("RETRY_LIMIT", 5).unwrap();
    assert_eq!(hits.iter().map(|h| (h.kind.as_str(), h.qualname.as_str())).collect::<Vec<_>>(), [("constant", "RETRY_LIMIT")]);
    let outline = scrutate::intel::outline(&repo.ix, "src/lib.rs").unwrap();
    assert!(outline.iter().any(|r| r.kind == "constant" && r.name == "RETRY_LIMIT"));
    for q in ["RETRY_LIMIT", "retry limit", "how many retries"] {
        let cands = repo.candidates(q, true).unwrap();
        assert!(cands.rows.iter().all(|r| r.kind != "constant"), "{q}: {:?}", cands.rows.iter().map(|r| &r.qualname).collect::<Vec<_>>());
    }
    let lexed = |repo: &scrutate::Repo| -> i64 { repo.ix.db.query_row("SELECT COUNT(*) FROM chunks WHERE kind != 'constant'", [], |r| r.get(0)).unwrap() };
    let n = lexed(&repo);
    assert_eq!(repo.ix.lex().unwrap().n_alive as i64, n);
    let report = repo.ix.check(false).unwrap();
    assert!(report.ok(), "{report:?}");
    std::fs::write(root.join("src/lib.rs"), "pub const RETRY_LIMIT: usize = 4;\npub fn retry_request() {}\n").unwrap();
    repo.update(false).unwrap();
    let n = lexed(&repo);
    assert_eq!(repo.ix.lex().unwrap().n_alive as i64, n);
    let report = repo.ix.check(false).unwrap();
    assert!(report.ok(), "{report:?}");
}

#[test]
fn swift_protocol_requirements_kotlin_type_aliases_and_elixir_guards_are_symbols() {
    let swift = symbols("c.swift", "protocol Payable {\n    func pay(amount: Int)\n}\n");
    assert!(has(&swift, "function", "Payable.pay"), "{swift:?}");
    let kt = symbols("b.kt", "package shop\n\ntypealias Handler = (Int) -> Unit\n");
    assert!(has(&kt, "type", "Handler"), "{kt:?}");
    let ex = symbols("f.ex", "defmodule Shop.Invoice do\n  defguard is_big(x) when x > 100\n  def f(x) when is_integer(x), do: x\nend\n");
    assert!(has(&ex, "function", "Shop.Invoice.is_big") && has(&ex, "function", "Shop.Invoice.f"), "{ex:?}");
}

#[test]
fn a_turbofish_call_is_a_call_and_a_struct_body_holds_methods() {
    let rs = parse_source("t.rs", "fn f() {\n    g::<u8>(1);\n    x.collect::<Vec<_>>();\n}\n");
    let calls: Vec<&str> = find(&rs, "f").calls.iter().map(|s| s.as_str()).collect();
    assert_eq!(calls, ["collect", "g"]);
    let got = symbols("p.hpp", "struct P {\n  ~P() = default;\n  P() {}\n};\n");
    assert_eq!(got, vec![("struct".into(), "P".into()), ("method".into(), "P.~P".into()), ("method".into(), "P.P".into())]);
}

#[test]
fn every_name_of_a_declaration_is_a_constant_with_the_declaration_as_its_text() {
    let go = constants("m.go", "package p\n\nvar X, Y int\nconst A, B = 1, 2\nconst (\n\tC, D = 3, 4\n\tH = 7\n)\nvar (\n\tE, F string\n)\nvar _, G = 5, 6\n");
    assert_eq!(named(&go), ["X", "Y", "A", "B", "C", "D", "H", "E", "F", "G"]);
    let texts: Vec<&str> = go.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(
        texts,
        [
            "var X, Y int",
            "var X, Y int",
            "const A, B = 1, 2",
            "const A, B = 1, 2",
            "C, D = 3, 4",
            "C, D = 3, 4",
            "H = 7",
            "E, F string",
            "E, F string",
            "var _, G = 5, 6"
        ]
    );
    let java = constants("M.java", "class M {\n  static final int A = 1, B = 2;\n  static int n, m;\n}\n");
    assert_eq!(named(&java), ["M.A", "M.B"]);
    assert!(java.iter().all(|(_, _, t)| t == "static final int A = 1, B = 2;"), "{java:?}");
    let js = constants("m.js", "const a = 1, b = 2;\nexport const c = 3, d = () => 4;\n");
    assert_eq!(named(&js), ["a", "b", "c"]);
    assert_eq!(js[1].2, "const a = 1, b = 2;");
}

fn calls_of(path: &str, src: &str, qualname: &str) -> Vec<String> {
    find(&parse_source(path, src), qualname).calls.iter().cloned().collect()
}

#[test]
fn a_method_call_records_the_method_not_the_receiver() {
    let cases = [
        ("m.lua", "function f()\n  obj:meth(1)\nend\n", "f"),
        ("m.lua", "function f()\n  obj.meth(1)\nend\n", "f"),
        ("m.php", "<?php\nfunction f() { $obj->meth(); }\n", "f"),
        ("m.php", "<?php\nfunction f() { Obj::meth(); }\n", "f"),
        ("m.kt", "fun f() { obj.meth(1) }\n", "f"),
        ("m.swift", "func f() { obj.meth(1) }\n", "f"),
        ("m.cs", "class C { void F() { obj.meth(1); } }\n", "C.F"),
        ("m.rb", "def f\n  obj.meth(1)\nend\n", "f"),
        ("m.java", "class C { void f() { obj.meth(1); } }\n", "C.f"),
        ("m.go", "package p\nfunc f() { obj.meth(1) }\n", "f"),
        ("m.py", "def f():\n    obj.meth(1)\n", "f"),
        ("m.js", "function f() { obj.meth(1); }\n", "f"),
        ("m.rs", "fn f() { obj.meth(1); }\n", "f"),
        ("m.cpp", "void f() { obj->meth(1); }\n", "f"),
        ("m.scala", "object O { def f() = obj.meth(1) }\n", "O.f"),
        ("m.mm", "void f(void) { [obj meth]; [obj meth:1 with:2]; }\n", "f"),
        ("m.zig", "fn f() void { obj.meth(1); }\n", "f"),
        ("m.jl", "function f()\n  obj.meth(1)\nend\n", "f"),
        ("m.dart", "void f() {\n  obj.meth(1);\n}\n", "f"),
        ("m.dart", "void f() {\n  obj..meth(1);\n}\n", "f"),
        ("m.dart", "void f() {\n  obj?.meth(1);\n}\n", "f"),
        ("m.groovy", "def f() {\n  obj.meth(1)\n}\n", "f"),
        ("m.groovy", "def f() {\n  obj.meth 1\n}\n", "f"),
        ("m.ex", "defmodule M do\n  def f(x), do: Mod.meth(x)\nend\n", "M.f"),
        ("m.ex", "defmodule M do\n  def f(x), do: Mod.Sub.meth(x)\nend\n", "M.f"),
        ("m.ex", "defmodule M do\n  def f(x), do: x |> Mod.meth()\nend\n", "M.f"),
        ("m.php", "<?php\nfunction f() { $obj?->meth(); }\n", "f"),
        ("m.r", "f <- function() {\n  obj$meth(1)\n}\n", "f"),
        ("m.ps1", "function f {\n  $obj.meth(1)\n}\n", "f"),
        ("m.scala", "object O { def f() = obj meth 1 }\n", "O.f"),
        ("m.adb", "procedure F is\nbegin\n  Obj.meth(1);\nend F;\n", "F"),
        ("m.pas", "program P;\nprocedure F;\nbegin\n  Obj.meth(1);\nend;\nbegin end.\n", "F"),
    ];
    let wrong: Vec<String> = cases
        .iter()
        .map(|&(path, src, q)| (path, calls_of(path, src, q)))
        .filter(|(_, got)| got != &["meth"])
        .map(|(p, got)| format!("{p}: {got:?}"))
        .collect();
    assert!(wrong.is_empty(), "{wrong:?}");
}

fn wrong_calls(cases: &[(&str, &str, &str, &[&str])]) -> Vec<String> {
    cases
        .iter()
        .map(|&(path, src, q, want)| (path, calls_of(path, src, q), want))
        .filter(|(_, got, want)| got != want)
        .map(|(p, got, want)| format!("{p}: got {got:?}, want {want:?}"))
        .collect()
}

#[test]
fn a_plain_call_and_a_constructor_call_record_the_called_name() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("m.py", "def f():\n    g(1)\n    Point(1, 2)\n", "f", &["Point", "g"]),
        ("m.js", "function f() { g(1); new Point(1, 2); }\n", "f", &["Point", "g"]),
        ("m.ts", "function f() { g(1); new Point(1, 2); }\n", "f", &["Point", "g"]),
        ("m.tsx", "function f() { g(1); new Point(1, 2); }\n", "f", &["Point", "g"]),
        ("m.go", "package p\nfunc f() { g(1) }\n", "f", &["g"]),
        ("m.rs", "fn f() { g(1); Point::new(1, 2); m!(x); }\n", "f", &["g", "m", "new"]),
        ("m.java", "class C { void f() { g(1); new Point(1, 2); } }\n", "C.f", &["Point", "g"]),
        ("m.c", "void f(void) { g(1); }\n", "f", &["g"]),
        ("m.cpp", "void f() { g(1); new Point(1, 2); Size(3, 4); }\n", "f", &["Point", "Size", "g"]),
        ("m.glsl", "void f() { g(1); vec3 v = vec3(1.0); }\n", "f", &["g", "vec3"]),
        ("m.mm", "void f(void) { g(1); [[Point alloc] init]; }\n", "f", &["alloc", "g", "init"]),
        ("m.cs", "class C { void F() { g(1); new Point(1, 2); } }\n", "C.F", &["Point", "g"]),
        ("m.rb", "def f\n  g(1)\n  Point.new(1, 2)\n  bare 1\nend\n", "f", &["bare", "g", "new"]),
        ("m.php", "<?php\nfunction f() { g(1); new Point(1, 2); }\n", "f", &["Point", "g"]),
        ("m.sh", "f() {\n  g 1\n}\n", "f", &["g"]),
        ("m.fish", "function f\n  g 1\nend\n", "f", &["g"]),
        ("m.ps1", "function f {\n  g 1\n  [Point]::new(1, 2)\n}\n", "f", &["g", "new"]),
        ("m.kt", "fun f() { g(1); Point(1, 2) }\n", "f", &["Point", "g"]),
        ("m.swift", "func f() { g(1); Point(x: 1) }\n", "f", &["Point", "g"]),
        ("m.scala", "object O { def f() = { g(1); new Point(1, 2) } }\n", "O.f", &["Point", "g"]),
        ("m.groovy", "def f() {\n  g(1)\n  new Point(1, 2)\n  println 'x'\n}\n", "f", &["Point", "g", "println"]),
        ("m.lua", "function f()\n  g(1)\nend\n", "f", &["g"]),
        ("m.ex", "defmodule M do\n  def f(x) do\n    g(x)\n    x |> h()\n    bare x\n  end\nend\n", "M.f", &["bare", "g", "h"]),
        ("m.erl", "-module(m).\nf() -> g(1).\n", "f", &["g"]),
        ("m.hs", "f x = g x\n", "f", &["g"]),
        ("m.elm", "f x =\n  g x\n", "f", &["g"]),
        ("m.zig", "fn f() void { g(1); }\n", "f", &["g"]),
        ("m.jl", "function f()\n  g(1)\n  Point(1, 2)\nend\n", "f", &["Point", "g"]),
        ("m.dart", "void f() {\n  g(1);\n  Point(1, 2);\n  new Size(1, 2);\n  const Box(1);\n  Cls.make();\n}\n", "f", &["Box", "Point", "Size", "g", "make"]),
        ("m.dart", "class C {\n  void m() {\n    g(1);\n  }\n}\n", "C.m", &["g"]),
        ("m.r", "f <- function() {\n  g(1)\n}\n", "f", &["g"]),
        ("m.nix", "{ f = x: g x; }\n", "f", &["g"]),
        ("m.tf", "locals {\n  v = g(1)\n}\n", "locals", &["g"]),
        ("m.sol", "contract C { function f() public { g(1); new Point(1, 2); } }\n", "C.f", &["Point", "g"]),
        ("m.pas", "program P;\nprocedure F;\nbegin\n  G(1);\nend;\nbegin end.\n", "F", &["G"]),
        ("m.adb", "procedure F is\n  X : Integer;\nbegin\n  G(1);\n  X := H(2);\nend F;\n", "F", &["G", "H"]),
        ("m.m", "function f()\n  g(1);\nend\n", "f", &["g"]),
        ("m.el", "(defun f (x)\n  (g x))\n", "f", &["g"]),
        ("m.scm", "(define (f x)\n  (g x))\n", "f", &["g"]),
        ("m.rkt", "#lang racket\n(define (f x)\n  (g x))\n", "f", &["g"]),
        ("m.lisp", "(defun f (x)\n  (g x))\n", "f", &["g"]),
        ("m.vim", "function! F()\n  call G(1)\nendfunction\n", "F", &["G"]),
        ("m.gleam", "pub fn f() {\n  g(1)\n  Point(1, 2)\n}\n", "f", &["Point", "g"]),
        ("m.bzl", "def f():\n    g(1)\n", "f", &["g"]),
        ("CMakeLists.txt", "function(f)\n  g(1)\nendfunction()\n", "f", &["g"]),
        ("m.bicep", "func f(x int) int => g(x)\n", "f", &["g"]),
        ("m.pp", "class foo {\n  $x = g(1)\n}\n", "foo", &["g"]),
    ];
    let wrong = wrong_calls(cases);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[cfg(feature = "extra-langs")]
#[test]
fn calls_are_recorded_in_the_optional_grammars() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("m.cu", "void f() { g(1); obj.meth(1); k<<<1, 2>>>(3); }\n", "f", &["g", "k", "meth"]),
        ("m.ml", "let f x =\n  g x;\n  Mod.meth x\n", "f", &["g", "meth"]),
        ("m.fs", "let f x =\n  g x\n  obj.meth(1)\n  Point(1, 2)\n", "f", &["Point", "g", "meth"]),
        ("m.f90", "subroutine f()\n  call g(1)\n  x = h(2)\nend subroutine f\n", "f", &["g", "h"]),
        ("m.d", "void f() { g(1); obj.meth(1); new Point(1, 2); }\n", "f", &["Point", "g", "meth"]),
        ("m.v", "module m;\n  function int f(int x);\n    return g(x);\n  endfunction\nendmodule\n", "m.f", &["g"]),
        ("m.odin", "package p\nf :: proc() {\n  g(1); obj.meth(1)\n}\n", "f", &["g", "meth"]),
        ("m.ll", "define i32 @f() {\n  %1 = call i32 @g(i32 1)\n  ret i32 %1\n}\n", "f", &["g"]),
        ("m.qml", "Item {\n  function f() { g(1); obj.meth(1); }\n}\n", "Item.f", &["g", "meth"]),
        ("m.tla", "---- MODULE m ----\nF(x) == G(x) + 1\n====\n", "m.F", &["G"]),
        ("m.ahk", "f() {\n  g(1)\n  obj.meth(1)\n}\n", "f", &["g", "meth"]),
        ("m.sv", "module m;\n  task t;\n    int y;\n    h(1);\n    h2(1, 2);\n    y = 1;\n  endtask\nendmodule\n", "m.t", &["h", "h2"]),
        ("m.v", "module m;\n  always @(posedge clk) begin\n    h(2);\n  end\n  adder u1 (.a(a), .b(b));\nendmodule\n", "m", &["adder", "h"]),
        ("m.vhd", VHDL, "pkg.f", &["g"]),
        ("m.vhd", VHDL, "pkg.p", &["q"]),
        ("m.vhd", VHDL, "rtl.h", &["f"]),
        ("m.vhd", VHDL, "rtl", &["h", "p"]),
    ];
    let wrong = wrong_calls(cases);
    assert!(wrong.is_empty(), "{wrong:#?}");
}

#[cfg(feature = "extra-langs")]
const VHDL: &str = "package pkg is\n  function f(x : integer) return integer;\nend package;\n\npackage body pkg is\n\
  function f(x : integer) return integer is\n  begin\n    return g(x) + 1;\n  end function;\n\
  procedure p(x : integer) is\n  begin\n    q(x);\n  end procedure;\nend package body;\n\n\
architecture rtl of top is\n  signal s : std_logic_vector(7 downto 0);\n\
  function h(x : integer) return integer is\n  begin\n    return f(x);\n  end function;\n\
begin\n  process\n  begin\n    p(1);\n    s <= h(2);\n  end process;\nend architecture;\n";

#[cfg(feature = "extra-langs")]
#[test]
fn vhdl_subprograms_with_a_body_are_functions_of_their_package_or_architecture() {
    let got = symbols("m.vhd", VHDL);
    let want = [("module", "pkg"), ("module", "pkg"), ("function", "pkg.f"), ("function", "pkg.p"), ("impl", "rtl"), ("function", "rtl.h")];
    assert_eq!(got, want.map(|(k, q)| (k.to_string(), q.to_string())));
}

#[test]
fn calls_are_recorded_in_make_jsonnet_and_assembly() {
    let cases: &[(&str, &str, &str, &[&str])] = &[
        ("Makefile", "define g\n\techo $(1)\nendef\n\nall:\n\t$(call g,1)\n\t@echo $(shell date) $(subst a,b,c)\n", "all", &["g"]),
        ("Makefile", "define outer\n\t$(call g,2) ${call  h-x ,3}\nendef\n", "outer", &["g", "h-x"]),
        ("m.jsonnet", "local f(x) = g(x) + obj.method(x);\nf(1)\n", "f", &["g", "method"]),
        ("m.jsonnet", "{\n  run(y): std.length(h(y)),\n  n: 1,\n}\n", "run", &["h", "length"]),
        (
            "m.s",
            "_start:\n    call helper\n    jmp done\n    bl other\n    jal third\n    b fourth\n    j fifth\nhelper:\n    ret\n",
            "_start",
            &["helper", "other", "third"],
        ),
        ("m.s", "f:\n    callq printf@PLT\n.L2:\n    call g\n    # doc of h\nh:\n    ret\n", "f", &["g", "printf"]),
        ("m.s", "f:\n    callq printf@PLT\n.L2:\n    call g\n    # doc of h\nh:\n    ret\n", ".L2", &["g"]),
    ];
    let wrong = wrong_calls(cases);
    assert!(wrong.is_empty(), "{wrong:#?}");
    let js = symbols("m.jsonnet", "local f(x) = x;\nlocal v = 1;\nlocal o = { m(y): y, n: 2 };\no\n");
    assert_eq!(js, [("function", "f"), ("key", "v"), ("key", "o"), ("method", "o.m")].map(|(k, q)| (k.to_string(), q.to_string())));
}

#[test]
fn an_assembly_label_holds_its_instructions_up_to_the_next_label_or_section() {
    let asm = parse_source("m.s", ".text\nf:\n    mov $1, %eax\n.L2:\n    call g\n    # doc of h\nh:\n    ret\n    .section .rodata\nmsg: .ascii \"x\"\n");
    let span = |q: &str| (find(&asm, q).start, find(&asm, q).end);
    assert_eq!([span("f"), span(".L2"), span("h")], [(2, 5), (4, 5), (7, 8)]);
    assert!(find(&asm, "f").text.contains("call g"), "{}", find(&asm, "f").text);
}

#[test]
fn a_definition_head_and_the_definition_macros_are_not_calls() {
    let ex = parse_source("m.ex", "defmodule M do\n  use GenServer\n  import Enum\n  alias Foo.Bar\n  require Logger\n  def f(x), do: g(x)\n  defp p(x) when x > 0, do: f(x)\n  defguard is_big(x) when x > 100\nend\n");
    assert!(find(&ex, "M").calls.is_empty(), "{:?}", find(&ex, "M").calls);
    assert_eq!(find(&ex, "M.f").calls.iter().collect::<Vec<_>>(), ["g"]);
    assert_eq!(find(&ex, "M.p").calls.iter().collect::<Vec<_>>(), ["f"]);
    assert!(find(&ex, "M.is_big").calls.is_empty());
    assert_eq!(calls_of("m.scm", "(define (f n)\n  (f (- n 1)))\n", "f"), ["f"]);
    assert_eq!(calls_of("m.lisp", "(defmethod m ((x point) y)\n  (h x))\n", "m"), ["h"]);
}

#[test]
fn a_call_records_a_lisp_name_whole_and_a_private_method_with_its_mark() {
    assert_eq!(calls_of("m.scm", "(define (use x)\n  (list->vec (my-string->list x)))\n", "use"), ["list->vec", "my-string->list"]);
    assert_eq!(calls_of("m.el", "(defun use (x)\n  (my-pkg--trim! x))\n", "use"), ["my-pkg--trim!"]);
    let js = parse_source("c.js", "class Cart {\n  #secret() {\n    return 1;\n  }\n  run() {\n    return this.#secret();\n  }\n}\n");
    assert_eq!(find(&js, "Cart.run").calls.iter().collect::<Vec<_>>(), ["#secret"]);
    // a C arrow is still a member access
    assert_eq!(calls_of("m.c", "void use(struct s *p) {\n  p->run(1);\n}\n", "use"), ["run"]);
}

#[test]
fn a_header_is_read_in_the_language_it_is_written_in() {
    let quals = |src: &str| parse_source("shape.h", src).iter().map(|c| format!("{} {}", c.kind, c.qualname)).collect::<Vec<_>>();
    let cpp = quals("// a licence line\n#pragma once\nnamespace geo {\nclass Shape {\npublic:\n  void draw();\n};\nShape make();\n}\n");
    for want in ["class geo.Shape", "method geo.Shape.draw", "function geo.make"] {
        assert!(cpp.iter().any(|q| q == want), "{want} not in {cpp:?}");
    }
    let templ = quals("#ifndef T_H\n#define T_H\ntemplate <typename T>\nT tmax(T a, T b);\n#endif\n");
    assert!(templ.iter().any(|q| q == "function tmax"), "{templ:?}");
    // a prototype inside an include guard is at file scope; one inside a function body is not
    let guarded =
        quals("#ifndef A_H\n#define A_H\nint add(int a, int b);\nstatic int use(void) {\n#ifdef X\n  int inner(int);\n#endif\n  return 0;\n}\n#endif\n");
    assert!(guarded.iter().any(|q| q == "function add") && !guarded.iter().any(|q| q.ends_with("inner")), "{guarded:?}");
    let objc = quals("#import <Foundation/Foundation.h>\n@interface Greeter : NSObject\n- (void)greet;\n@end\n");
    assert!(objc.iter().any(|q| q.ends_with("Greeter")), "{objc:?}");
    // plain C stays C: `class` and `namespace` are ordinary names there
    assert_eq!(scrutate::parse::spec_for("a.h", "struct s { int class; };\nint add(int a, int b);\n/* namespace of the driver */\n").map(|s| s.id), Some("c"));
    assert_eq!(scrutate::parse::spec_for("a.h", "namespace geo { int f(); }\n").map(|s| s.id), Some("cpp"));
}

#[test]
fn an_objc_multi_part_selector_call_is_a_call_of_its_definition() {
    let chunks = parse_source("m.mm", "@implementation Foo\n- (void)a:(int)x b:(int)y {\n}\n- (void)run {\n  [obj a:1 b:2];\n}\n@end\n");
    assert_eq!(find(&chunks, "Foo.a").name, "a");
    assert_eq!(find(&chunks, "Foo.run").calls.iter().collect::<Vec<_>>(), ["a"]);
}

#[test]
fn dart_and_pascal_definitions_hold_their_bodies() {
    let dart = parse_source("m.dart", "void f() {\n  g(1);\n}\n");
    assert_eq!((find(&dart, "f").start, find(&dart, "f").end), (1, 3));
    let pas = parse_source("m.pas", "program P;\nprocedure F;\nbegin\n  G(1);\nend;\nbegin end.\n");
    assert_eq!((find(&pas, "F").start, find(&pas, "F").end), (2, 5));
}

const OBJECT_LITERALS: &str = "const api = { get() { return 1; }, post: function () { return 2; }, put: () => 3, gen: function* () {}, \"q\": () => 5 };
export const store = { count: 0, increment() { return this.total(); }, total() { return this.count; }, sub: { m() {} }, later: async () => go() };
export default { methods: { save() { return 1; }, run() { return this.save(); } } };
module.exports.tool = { build() { return 4; } };
exports.kit = { fix() {} };
f({ cb() {} });
";

#[test]
fn methods_of_an_object_literal_are_owned_by_what_holds_the_literal() {
    for path in ["o.js", "o.ts"] {
        let chunks = parse_source(path, OBJECT_LITERALS);
        let methods: Vec<(&str, &str)> = chunks.iter().filter(|c| c.kind == "method").map(|c| (c.name.as_str(), c.qualname.as_str())).collect();
        assert_eq!(
            methods,
            [
                ("get", "api.get"),
                ("post", "api.post"),
                ("put", "api.put"),
                ("gen", "api.gen"),
                ("increment", "store.increment"),
                ("total", "store.total"),
                ("m", "store.sub.m"),
                ("later", "store.later"),
                ("save", "methods.save"),
                ("run", "methods.run"),
                ("build", "tool.build"),
                ("fix", "kit.fix"),
                ("cb", "cb"),
            ],
            "{path}"
        );
        assert_eq!(find(&chunks, "store.later").calls.iter().collect::<Vec<_>>(), ["go"], "{path}");
        assert_eq!(find(&chunks, "store.increment").calls.iter().collect::<Vec<_>>(), ["total"], "{path}");
    }
}

#[test]
fn c_and_cpp_function_prototypes_are_function_symbols() {
    let c = symbols("p.h", "int add(int a, int b);\nextern int x;\nint y;\nint (*fp)(int);\ntypedef int (*cb)(int);\nstatic char *name(void);\nvoid g(void) { foo(1); int h(int); }\n");
    let want = |v: &[(&str, &str)]| v.iter().map(|&(k, q)| (k.to_string(), q.to_string())).collect::<Vec<_>>();
    assert_eq!(c, want(&[("function", "add"), ("type", "cb"), ("function", "name"), ("function", "g")]));
    let cpp = parse_source(
        "p.hpp",
        "namespace ns {\n  Shape make();\n  int x;\n}\nextern \"C\" {\n  int cfun(int);\n}\nextern \"C\" int cone(int);\nWidget w();\nusing F = int(int);\nclass K { void m(); };\nint& rf();\nint (*fp)(int);\ntemplate <typename T> T tmax(T a, T b);\nvoid g() { foo(1); int h(int); }\n",
    );
    let got: Vec<(&str, &str)> = cpp.iter().map(|c| (c.kind, c.qualname.as_str())).collect();
    assert_eq!(
        got,
        [
            ("module", "ns"),
            ("function", "ns.make"),
            ("function", "cfun"),
            ("function", "cone"),
            ("function", "w"),
            ("type", "F"),
            ("class", "K"),
            ("method", "K.m"),
            ("function", "rf"),
            ("function", "tmax"),
            ("function", "g"),
        ]
    );
    assert_eq!(find(&cpp, "ns.make").text, "Shape make();");
    assert_eq!(find(&cpp, "tmax").text, "template <typename T> T tmax(T a, T b);");
    assert!(find(&cpp, "ns.make").calls.is_empty());
    assert_eq!(find(&cpp, "g").calls.iter().collect::<Vec<_>>(), ["foo"]);
}

const TS_WRAPPED_LITERALS: &str = "export const api = { get() {} } as const;
const store = { inc() {}, sub: ({ deep() {} }) as Sub } satisfies Store;
const box = ({ open() {} });
let tool = ({ run() {} } as const) satisfies Tool;
";

#[test]
fn a_typescript_literal_wrapped_in_as_satisfies_or_parentheses_keeps_its_owner() {
    for path in ["w.ts", "w.tsx"] {
        let chunks = parse_source(path, TS_WRAPPED_LITERALS);
        let methods: Vec<&str> = chunks.iter().filter(|c| c.kind == "method").map(|c| c.qualname.as_str()).collect();
        assert_eq!(methods, ["api.get", "store.inc", "store.sub.deep", "box.open", "tool.run"], "{path}");
    }
}

#[test]
fn c_and_cpp_const_globals_are_constants() {
    let c = constants(
        "k.c",
        "const int MAX_LEN = 80;\nstatic const char *NAME = \"x\";\nextern const int EXT;\nconst int A = 1, B = 2;\nint counter = 0;\n\
         int f(const int p) { const int local = 1; return local; }\nstruct s { const int m; };\n",
    );
    assert_eq!(named(&c), ["MAX_LEN", "NAME", "A", "B"]);
    assert_eq!((c[0].2.as_str(), c[1].2.as_str(), c[3].2.as_str()), ("const int MAX_LEN = 80;", "static const char *NAME = \"x\";", "const int A = 1, B = 2;"));
    let cpp = constants(
        "k.cpp",
        "constexpr int kLimit = 8;\nnamespace cfg { inline constexpr double kPi = 3.14; }\nextern \"C\" { const int kC = 1; }\n\
         class K { public: static const int kSize = 2; };\nvoid g() { constexpr int in = 1; }\n",
    );
    assert_eq!(named(&cpp), ["kLimit", "cfg.kPi", "kC"]);
}

#[test]
fn python_class_constants_and_tuple_targets_are_constants() {
    let py =
        constants("k.py", "A, B = 1, 2\n(C, d) = 3, 4\nclass Config:\n    MAX = 3\n    name = 'x'\n    X, Y = 1, 2\n    def f(self):\n        INNER = 1\n");
    assert_eq!(named(&py), ["A", "B", "C", "Config.MAX", "Config.X", "Config.Y"]);
    let texts: Vec<&str> = py.iter().map(|(_, _, t)| t.as_str()).collect();
    assert_eq!(texts, ["A, B = 1, 2", "A, B = 1, 2", "(C, d) = 3, 4", "MAX = 3", "X, Y = 1, 2", "X, Y = 1, 2"]);
}

#[test]
fn static_readonly_class_members_are_constants() {
    let src = "export default class K {\n  static readonly SIZE = 2;\n  static n = 1;\n  readonly r = 3;\n  private static readonly p: number = 4;\n  static readonly f = () => 1;\n}\n";
    for path in ["k.ts", "k.tsx"] {
        let ts = constants(path, src);
        assert_eq!(named(&ts), ["K.SIZE", "K.p"], "{path}");
        assert_eq!(ts[0].2, "static readonly SIZE = 2", "{path}");
    }
    let js = constants("k.js", "class K {\n  static MAX = 3;\n  static n = 1;\n  static #P = 2;\n  LIMIT = 4;\n}\n");
    assert_eq!(named(&js), ["K.MAX"]);
}

const OBJC_HEADER: &str = "#import <Foundation/Foundation.h>\n@interface Greeter : NSObject\n@property (nonatomic) int count;\n- (void)greet;\n\
                           + (instancetype)make:(int)n with:(int)m;\n@end\n@protocol Speaker\n- (void)speak:(int)x;\n@end\n";

#[test]
fn objc_method_declarations_are_methods_of_their_class_or_protocol() {
    for path in ["g.h", "g.m"] {
        let got = symbols(path, OBJC_HEADER);
        let want = [("class", "Greeter"), ("method", "Greeter.greet"), ("method", "Greeter.make"), ("interface", "Speaker"), ("method", "Speaker.speak")];
        assert_eq!(got, want.map(|(k, q)| (k.to_string(), q.to_string())), "{path}");
        assert_eq!(find(&parse_source(path, OBJC_HEADER), "Greeter.make").text, "+ (instancetype)make:(int)n with:(int)m;", "{path}");
    }
    let root =
        tree("objc_decl", &[("g.h", OBJC_HEADER), ("g.m", "#import \"g.h\"\n@implementation Greeter\n- (void)greet {\n  NSLog(@\"hello there\");\n}\n@end\n")]);
    let mut repo = scrutate::Repo::open(&root).unwrap();
    repo.update(false).unwrap();
    let hits = repo.find_symbol("Greeter.greet", 5).unwrap();
    assert_eq!(hits.iter().map(|h| h.loc()).collect::<Vec<_>>(), ["g.m:3-5", "g.h:4-4"]);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_function_pointer_typedef_is_named_by_its_name() {
    let c = symbols("t.c", "typedef int (*cb)(int);\ntypedef void (*handler_t)(int, void *);\nstruct ops { int (*fp)(int); };\n");
    assert_eq!(c, [("type", "cb"), ("type", "handler_t"), ("struct", "ops")].map(|(k, q)| (k.to_string(), q.to_string())));
    let cpp = symbols("t.cpp", "using cb = int (*)(int);\ntypedef int (*cb2)(int);\nstruct S { int (*fp)(int); };\n");
    assert_eq!(cpp, [("type", "cb"), ("type", "cb2"), ("struct", "S")].map(|(k, q)| (k.to_string(), q.to_string())));
}
