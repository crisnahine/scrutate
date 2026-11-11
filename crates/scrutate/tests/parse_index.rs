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
