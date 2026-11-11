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

#[test]
fn deeply_nested_functions_index_in_bounded_time_and_size() {
    let n = 2000;
    for sep in ["\n", ""] {
        let src = format!("{}{}", format!("function a(){{{sep}").repeat(n), "}".repeat(n));
        let t = Instant::now();
        let chunks = parse_source("a.js", &src);
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(chunks.len(), n);
        let deepest = chunks.iter().map(|c| c.qualname.split('.').count()).max().unwrap();
        assert_eq!(deepest, 32);
        let qual_bytes: usize = chunks.iter().map(|c| c.qualname.len()).sum();
        assert!(qual_bytes < 64 * n, "{qual_bytes} bytes of qualified names");
        assert!(secs < 3.0, "parse took {secs:.1}s");
    }
}

#[test]
fn a_node_with_many_children_parses_in_linear_time() {
    let src = "(f ".repeat(20000);
    let t = Instant::now();
    parse_source("a.scm", &src);
    let secs = t.elapsed().as_secs_f64();
    assert!(secs < 3.0, "parse took {secs:.1}s");
}

#[test]
fn a_file_of_many_sibling_definitions_parses_in_linear_time() {
    let n = 4000;
    for (path, line) in [
        ("w.js", "function f{i}(x) {{ return x + {i}; }}"),
        ("w.c", "int f{i}(int x) {{ return x + {i}; }}"),
        ("w.go", "func f{i}(x int) int {{ return x + {i} }}"),
    ] {
        let body: String = (0..n).map(|i| line.replace("{i}", &i.to_string()).replace("{{", "{").replace("}}", "}") + "\n").collect();
        let src = if path.ends_with(".go") { format!("package p\n{body}") } else { body };
        let t = Instant::now();
        let chunks = parse_source(path, &src);
        let secs = t.elapsed().as_secs_f64();
        assert_eq!(chunks.iter().filter(|c| c.kind == "function").count(), n, "{path}");
        assert!(secs < 3.0, "{path}: parse took {secs:.1}s");
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
    assert_eq!(got, vec![("class".into(), "Shape".into()), ("method".into(), "Shape.area".into()), ("method".into(), "Shape.resize".into())]);
}

#[test]
fn c_and_cpp_function_like_macros_are_symbols() {
    let src = "#define MAX(a, b) ((a) > (b) ? (a) : (b))\n#define LIMIT 5\n";
    for path in ["m.h", "m.hpp"] {
        let got = symbols(path, src);
        assert_eq!(got, vec![("macro".to_string(), "MAX".to_string())], "{path}");
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
