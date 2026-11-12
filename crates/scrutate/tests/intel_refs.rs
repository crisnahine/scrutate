//! Code intelligence answers on small repositories built for one case each: qualified names
//! among many namesakes, special files where a config is expected, imports of outside packages,
//! scopes, undecodable bytes, duplicates, and the `reindex` and `search_code` tools.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn at(root: PathBuf, files: &[(&str, &[u8])]) -> Repo {
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for (rel, body) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
        }
        Repo { root: fs::canonicalize(&root).unwrap() }
    }

    fn new(name: &str, files: &[(&str, &str)]) -> Repo {
        let files: Vec<(&str, &[u8])> = files.iter().map(|(p, b)| (*p, b.as_bytes())).collect();
        let r = Repo::at(scratch(name), &files);
        r.index();
        r
    }

    fn index(&self) {
        let (code, out) = self.run(&["index", "--no-text"]);
        assert_eq!(code, 0, "{out}");
    }

    /// (exit code, stdout then stderr)
    fn run(&self, args: &[&str]) -> (i32, String) {
        let (code, out, err) = self.exec(args);
        (code, out + &err)
    }

    /// (exit code, stdout, stderr); a run that takes more than 20 s is killed and fails the test.
    fn exec(&self, args: &[&str]) -> (i32, String, String) {
        let mut child =
            Command::new(BIN).args(args).current_dir(&self.root).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
        let start = Instant::now();
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if start.elapsed() > Duration::from_secs(20) {
                let _ = child.kill();
                panic!("scrutate {args:?} did not return in 20 s");
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let out = child.wait_with_output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
    }

    fn json(&self, args: &[&str]) -> Value {
        let (code, out, err) = self.exec(args);
        assert_eq!(code, 0, "{args:?}: {out}{err}");
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }

    /// `path:line` of every occurrence -> path of the definition it resolved to ("-" = unresolved).
    fn resolved(&self, name: &str) -> Vec<(String, String)> {
        let v = self.json(&["refs", name, "--json"]);
        let mut out = Vec::new();
        for d in v["definitions"].as_array().unwrap() {
            for r in d["references"].as_array().unwrap() {
                out.push((format!("{}:{}", r["path"].as_str().unwrap(), r["line"]), d["path"].as_str().unwrap().to_string()));
            }
        }
        for u in v["unresolved"].as_array().unwrap() {
            out.push((format!("{}:{}", u["path"].as_str().unwrap(), u["line"]), "-".to_string()));
        }
        out.sort();
        out
    }

    fn lib(&self) -> scrutate::Repo {
        scrutate::Repo::open(&self.root).unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn scratch(name: &str) -> PathBuf {
    static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("scrutate_intel_{name}_{}_{n}", std::process::id()))
}

fn pairs(want: &[(&str, &str)]) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = want.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect();
    v.sort();
    v
}

fn many_runs() -> Repo {
    let mut src = String::from("def helper():\n    return 1\n\n");
    for i in 0..70 {
        src.push_str(&format!(
            "class C{i}:\n    def run(self):\n        x = {i} * 3 + {i}\n        y = [x, x + 1, x + 2, 'v{i}']\n        return helper()\n\n"
        ));
    }
    Repo::new("runs", &[("many.py", &src)])
}

#[test]
fn class_dot_method_is_found_among_many_namesakes() {
    let r = many_runs();
    for name in ["C69.run", "C5.run", "C40.run"] {
        let (code, out) = r.run(&["symbol", name]);
        assert_eq!(code, 0, "{name}: {out}");
        let class = name.split('.').next().unwrap();
        assert!(out.contains(&format!("{class}.run")) && out.lines().filter(|l| l.contains("[method]")).count() == 1, "{name}: {out}");
    }
    let (code, out) = r.run(&["callees", "C40.run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("helper"), "{out}");
    let (_, out) = r.run(&["similar", "C69.run"]);
    assert!(!out.contains("no symbol or location matches"), "{out}");
}

#[test]
fn lsp_definition_of_a_qualified_call_among_many_namesakes() {
    let r = many_runs();
    fs::write(r.root.join("use.py"), "from many import C69\n\ndef go():\n    return C69.run(None)\n").unwrap();
    r.index();
    let uri = format!("file://{}", r.root.join("use.py").display());
    let msgs = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"rootUri": format!("file://{}", r.root.display()), "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/definition", "params": {"textDocument": {"uri": uri}, "position": {"line": 3, "character": 16}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown"}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ];
    let mut input = Vec::new();
    for m in msgs {
        let body = m.to_string();
        input.extend(format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes());
    }
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned();
    let answer = out.split("Content-Length").find(|m| m.contains("\"id\":2")).unwrap_or_else(|| panic!("{out}"));
    let line_of_c69_run = 1 + 3 + 69 * 6 + 1; // 1-based line of `def run` in class C69
    assert!(answer.contains(&format!("\"line\":{}", line_of_c69_run - 1)), "{answer}");
}

#[cfg(unix)]
fn mkfifo(p: &Path) {
    assert!(Command::new("mkfifo").arg(p).status().unwrap().success());
}

#[cfg(unix)]
#[test]
fn a_fifo_where_a_config_is_expected_does_not_hang() {
    let r = Repo::new(
        "fifo",
        &[
            ("src/a.ts", "export function fromA() { return 1; }\n"),
            ("main.ts", "import { fromA } from \"@x/a\";\nfromA();\n"),
            ("foo/src/lib.rs", "pub fn shared() {}\n"),
            ("app/src/main.rs", "use foo::shared;\n\nfn main() {\n    shared();\n}\n"),
        ],
    );
    mkfifo(&r.root.join("tsconfig.json"));
    mkfifo(&r.root.join("foo/Cargo.toml"));
    mkfifo(&r.root.join("pipe.txt"));
    assert_eq!(r.run(&["deps", "main.ts"]).0, 0);
    assert_eq!(r.run(&["refs", "fromA"]).0, 0);
    assert_eq!(r.run(&["refs", "shared"]).0, 0);
    let mut lib = r.lib();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let c = scrutate::mcp::call_tool(&mut lib, "read_code", &json!({"path": "pipe.txt"}), &scrutate::mcp::Settings::new(None, Some(0.0)));
        let _ = tx.send((c.is_error, c.text));
    });
    let (failed, text) = rx.recv_timeout(Duration::from_secs(20)).expect("read_code of a FIFO returns");
    assert!(failed, "{text}");
}

#[cfg(unix)]
#[test]
fn a_config_symlinked_from_outside_the_repository_is_not_read() {
    let outside = scratch("outside");
    fs::create_dir_all(&outside).unwrap();
    fs::write(outside.join("tsconfig.json"), r#"{"compilerOptions": {"paths": {"@x/*": ["src/*"]}}}"#).unwrap();
    let r = Repo::new("symcfg", &[("src/a.ts", "export function fromA() { return 1; }\n"), ("main.ts", "import { fromA } from \"@x/a\";\nfromA();\n")]);
    std::os::unix::fs::symlink(outside.join("tsconfig.json"), r.root.join("tsconfig.json")).unwrap();
    let deps = r.json(&["deps", "main.ts", "--json"]);
    let _ = fs::remove_dir_all(&outside);
    assert!(deps.to_string().contains("\"resolved\":null"), "{deps}");
    // the same config as a regular file is used
    fs::remove_file(r.root.join("tsconfig.json")).unwrap();
    fs::write(r.root.join("tsconfig.json"), r#"{"compilerOptions": {"paths": {"@x/*": ["src/*"]}}}"#).unwrap();
    let deps = r.json(&["deps", "main.ts", "--json"]);
    assert!(deps.to_string().contains("\"resolved\":\"src/a.ts\""), "{deps}");
}

#[test]
fn go_imports_of_outside_packages_do_not_bind_local_directories() {
    let r = Repo::new(
        "goext",
        &[
            ("go.mod", "module example.com/me\n\ngo 1.21\n"),
            ("errors/errors.go", "package errors\n\nfunc Wrap() {}\n"),
            ("main.go", "package main\n\nimport \"github.com/pkg/errors\"\n\nfunc main() {\n\terrors.Wrap()\n}\n"),
            ("own.go", "package main\n\nimport \"example.com/me/errors\"\n\nfunc own() {\n\terrors.Wrap()\n}\n"),
        ],
    );
    assert_eq!(r.resolved("Wrap"), pairs(&[("errors/errors.go:3", "errors/errors.go"), ("main.go:6", "-"), ("own.go:6", "errors/errors.go")]));
}

#[test]
fn python_imports_of_outside_packages_do_not_bind_local_files() {
    let r = Repo::new(
        "pyext",
        &[("utils.py", "def quote(s):\n    return s\n"), ("app.py", "from requests.utils import quote\n\ndef go():\n    return quote('x')\n")],
    );
    assert_eq!(r.resolved("quote"), pairs(&[("app.py:1", "-"), ("app.py:4", "-"), ("utils.py:1", "utils.py")]));
    let deps = r.json(&["deps", "app.py", "--json"]);
    assert!(!deps.to_string().contains("utils.py"), "{deps}");
}

#[test]
fn python_imports_through_the_checkout_name_still_bind() {
    let parent = scratch("own");
    let r = Repo::at(
        parent.join("mypkg"),
        &[("utils.py", b"def quote(s):\n    return s\n"), ("app.py", b"from mypkg.utils import quote\n\ndef go():\n    return quote('x')\n")],
    );
    r.index();
    assert_eq!(r.resolved("quote"), pairs(&[("app.py:1", "utils.py"), ("app.py:4", "utils.py"), ("utils.py:1", "utils.py")]));
    let deps = r.json(&["deps", "app.py", "--json"]);
    assert!(deps.to_string().contains("\"resolved\":\"utils.py\""), "{deps}");
    drop(r);
    let _ = fs::remove_dir_all(parent);
}

#[test]
fn a_call_the_resolver_does_not_read_runs_the_definition_in_its_own_file() {
    let mut files: Vec<(String, String)> = Vec::new();
    for x in ["a", "b", "c", "d", "e"] {
        files.push((format!("{x}.c"), format!("static int helper(void) {{\n    return 1;\n}}\n\nint run_{x}(void) {{\n    return helper();\n}}\n")));
        files.push((format!("t{x}.rb"), format!("class T{x}\n  def helper\n    1\n  end\n\n  def run_{x}\n    helper()\n  end\nend\n")));
    }
    // no definition in the caller's file: the nearest ones, its directory first
    files.push(("lib/near.c".into(), "int far(void) {\n    return 1;\n}\n".into()));
    files.push(("lib/user.c".into(), "int use(void) {\n    return far();\n}\n".into()));
    for x in ["a", "b", "c"] {
        files.push((format!("aa{x}/far.c"), "int far(void) {\n    return 2;\n}\n".into()));
    }
    files.push(("fns.nix".into(), "{\n  calleeFn = x: x + 1;\n  callerFn = y: calleeFn y;\n}\n".into()));
    files.push(("view.m".into(), "#import <Foundation/Foundation.h>\n\nint shade(int v);\n\nint draw(int v) {\n    return shade(v);\n}\n".into()));
    files.push(("shade.c".into(), "int shade(int v) {\n    return v;\n}\n".into()));
    files.push(("plot.m".into(), "function y = shade(v)\n    y = v;\nend\n".into()));
    let files: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let r = Repo::new("own_file", &files);
    let listed = |spec: &str| -> Vec<String> {
        let (code, out) = r.run(&["callees", spec]);
        assert_eq!(code, 0, "{spec}: {out}");
        out.lines().filter(|l| l.contains("] ")).map(|l| l.split(':').next().unwrap().to_string()).collect()
    };
    assert_eq!(listed("e.c:5"), ["e.c"]);
    assert_eq!(listed("te.rb:6"), ["te.rb"]);
    assert_eq!(listed("lib/user.c:1")[0], "lib/near.c");
    // a Nix function is a key, and a `.m` file of MATLAB is no Objective-C
    assert_eq!(listed("fns.nix:3"), ["fns.nix"]);
    assert_eq!(listed("view.m:5"), ["shade.c"]);
    // and the other way round: a helper is called by the function of its own file alone
    assert_eq!(split_callers(&r, "a.c:1"), (strs(&["run_a"]), strs(&[])));
    assert_eq!(split_callers(&r, "te.rb:2"), (strs(&["Te.run_e"]), strs(&[])));
}

#[test]
fn a_call_of_a_class_is_a_call_of_its_constructor() {
    let r = Repo::new(
        "ctor",
        &[
            ("geo.py", "class Point:\n    def __init__(self, x):\n        self.x = x\n\n\nclass Other:\n    def __init__(self):\n        pass\n\n\ndef make():\n    return Point(1)\n\n\ndef other():\n    return Other()\n"),
            ("w.js", "class Widget {\n  constructor(n) {\n    this.n = n;\n  }\n}\n\nfunction build() {\n  return new Widget(1);\n}\n"),
            ("Box.java", "public class Box {\n    private final int n;\n\n    public Box(int n) {\n        this.n = n;\n    }\n\n    static Box of() {\n        return new Box(1);\n    }\n}\n"),
            ("inv.rb", "class Invoice\n  def initialize(n)\n    @n = n\n  end\nend\n\nclass Receipt\n  def initialize\n  end\nend\n\ndef bill\n  Invoice.new(1)\nend\n\ndef slip\n  Receipt.new\nend\n"),
        ],
    );
    assert_eq!(split_callers(&r, "Point.__init__"), (strs(&["make"]), strs(&[])));
    assert_eq!(split_callers(&r, "Widget.constructor"), (strs(&["build"]), strs(&[])));
    assert_eq!(split_callers(&r, "Box.java:4"), (strs(&["Box.of"]), strs(&[])));
    assert_eq!(split_callers(&r, "Invoice.initialize"), (strs(&["bill"]), strs(&[])));
}

#[test]
fn a_call_outside_every_definition_has_the_chunk_that_holds_it_as_caller() {
    let r = Repo::new(
        "toplevel_calls",
        &[
            ("job.py", "# job.py\ndef helper(x):\n    return x + 1\n\nif __name__ == \"__main__\":\n    total = helper(2)\n"),
            ("run.sh", "main() {\n  echo hi\n}\n\nmain \"$@\"\n"),
            ("a.py", "def load():\n    return 1\n"),
            ("b.py", "def load():\n    return 2\n"),
            ("script.py", "from b import load\nload()\n"),
        ],
    );
    let (code, out) = r.run(&["callers", "helper"]);
    assert_eq!((code, out.trim()), (0, "job.py:5-6  [window] job"));
    let (code, out) = r.run(&["callers", "main"]);
    assert_eq!((code, out.trim()), (0, "run.sh:5-5  [window] run"));
    // the resolver decides a call outside every definition as it does one inside
    assert_eq!(split_callers(&r, "b.py:1"), (strs(&["script"]), strs(&[])));
    assert_eq!(r.run(&["callers", "a.py:1"]).1.trim(), "(no results)");
    let (code, out) = r.run(&["callees", "job.py:6"]);
    assert_eq!((code, out.trim()), (0, "job.py:2-3  [function] helper"));
    let (code, out) = r.run(&["callees", "run.sh:5"]);
    assert_eq!((code, out.trim()), (0, "run.sh:1-3  [function] main"));
    // a window is never a definition elsewhere
    for args in [&["symbol", "job.py:6"][..], &["callers", "job.py:6"], &["refs", "x", "--def", "job.py:6"]] {
        let (code, out) = r.run(args);
        assert_ne!(code, 0, "{args:?}: {out}");
    }
    let (_, out) = r.run(&["outline", "job.py"]);
    assert!(!out.contains("window"), "{out}");
}

#[test]
fn the_outline_of_a_file_with_no_definition_is_its_module_line() {
    let r = Repo::new("outline_text", &[("LICENSE", "MIT License\nCopyright (c) 2026\n"), ("a.py", "x = 1\n")]);
    let (code, out) = r.run(&["outline", "LICENSE"]);
    assert_eq!((code, out.trim()), (0, "1-2     module    LICENSE"));
}

#[test]
fn callers_says_which_call_sites_it_did_not_check() {
    let mut files: Vec<(String, String)> = vec![
        ("a/__init__.py".into(), String::new()),
        ("a/helper.py".into(), "def helper():\n    return 1\n".into()),
        ("b/helper.py".into(), "def helper():\n    return 2\n".into()),
    ];
    for i in 0..410 {
        files.push((format!("u/u{i:03}.py"), format!("from a.helper import helper\n\n\ndef f{i:03}():\n    return helper()\n")));
    }
    let files: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), b.as_str())).collect();
    let r = Repo::new("unchecked", &files);
    let (code, out) = r.run(&["callers", "a/helper.py:1", "--limit", "500"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out.lines().filter(|l| l.contains("] f")).count(), 410, "{out}");
    assert!(out.contains("(the last 10 were not checked: the resolver is asked about the 400 likeliest call sites)"), "{out}");
    assert!(!out.contains("could not"), "{out}");
}

#[test]
fn refs_names_a_language_the_resolver_does_not_read_apart_from_one_without_a_grammar() {
    let r = Repo::new(
        "unread",
        &[
            ("m.c", "int helper(void) {\n    return 1;\n}\n\nint run(void) {\n    return helper();\n}\n"),
            ("m.pl", "sub helper {\n    return 1;\n}\n\nhelper();\n"),
        ],
    );
    let (_, out) = r.run(&["refs", "helper"]);
    let reason_of = |file: &str| out.lines().filter(|l| l.contains(file)).map(|l| l.rsplit('(').next().unwrap_or("").to_string()).collect::<Vec<_>>();
    assert!(reason_of("m.c").iter().any(|w| w.starts_with("not resolved in this language")), "{out}");
    assert!(reason_of("m.c").iter().all(|w| !w.starts_with("no grammar")), "{out}");
    assert!(reason_of("m.pl").iter().any(|w| w.starts_with("no grammar")), "{out}");
}

#[test]
fn a_ruby_predicate_and_a_rust_raw_identifier_have_their_callers() {
    let rb = "class Rec\n  def valid?\n    true\n  end\n\n  def save!\n    1\n  end\n\n  def run\n    save! if valid?\n  end\nend\n";
    let rs = "pub fn r#type() -> i32 {\n    1\n}\n\npub fn kind() -> i32 {\n    r#type()\n}\n";
    let r = Repo::new("name_marks", &[("rec.rb", rb), ("src/lib.rs", rs)]);
    assert_eq!(split_callers(&r, "valid?"), (strs(&["Rec.run"]), strs(&[])));
    assert_eq!(split_callers(&r, "save!"), (strs(&["Rec.run"]), strs(&[])));
    for name in ["type", "r#type"] {
        assert_eq!(split_callers(&r, name), (strs(&["kind"]), strs(&[])), "{name}");
    }
    let (code, out) = r.run(&["callees", "Rec.run"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("Rec.valid?") && out.contains("Rec.save!") && !out.contains("external"), "{out}");
}

#[test]
fn a_macro_of_the_repository_is_a_callee() {
    let c = "#define SQUARE(x) ((x) * (x))\n\nint twice(int v) {\n    return v * 2;\n}\n\nint use(int v) {\n    return SQUARE(v) + twice(v);\n}\n";
    let rs = "macro_rules! square {\n    ($x:expr) => {\n        $x * $x\n    };\n}\n\npub fn area(v: i32) -> i32 {\n    square!(v)\n}\n";
    let r = Repo::new("macro_callee", &[("m.c", c), ("src/lib.rs", rs)]);
    for (spec, name) in [("m.c:8", "SQUARE"), ("src/lib.rs:8", "square")] {
        let (code, out) = r.run(&["callees", spec]);
        assert_eq!(code, 0, "{spec}: {out}");
        assert!(out.lines().any(|l| l.contains("[macro]") && l.ends_with(name)), "{spec}: {out}");
        assert!(!out.lines().any(|l| l.starts_with("external:") && l.contains(name)), "{spec}: {out}");
    }
}

#[test]
fn a_bare_python_import_inside_a_package_is_not_the_module_next_door() {
    // the directory of a package is not on `sys.path`: `import logging` there is the standard library
    let core = "import logging\nimport json\n\n\ndef run():\n    logging.info(\"x\")\n    return json.dumps({})\n";
    let r = Repo::new(
        "pypkg_sibling",
        &[
            ("mypkg/__init__.py", ""),
            ("mypkg/logging.py", "def info(s):\n    return s\n"),
            ("mypkg/json.py", "def dumps(o):\n    return str(o)\n"),
            ("mypkg/core.py", core),
        ],
    );
    for name in ["info", "dumps"] {
        let (code, out) = r.run(&["callers", name]);
        assert_eq!((code, out.trim()), (1, "(no results)"), "{name}");
    }
    let (_, deps) = r.run(&["deps", "mypkg/core.py"]);
    assert!(!deps.contains("mypkg/logging.py") && !deps.contains("mypkg/json.py"), "{deps}");
}

#[test]
fn a_bare_python_import_of_the_module_next_door_binds_it() {
    let r = Repo::new(
        "pysib",
        &[
            ("pyproject.toml", "[project]\nname = \"shop\"\n"),
            ("src/shop/__init__.py", ""),
            ("src/shop/cart.py", "def total(items):\n    return sum(items)\n"),
            ("tests/test_cart.py", "from shop.cart import total\nfrom helpers import make_items\n\n\ndef test_total():\n    assert total(make_items()) == 6\n"),
            ("tests/helpers.py", "def make_items():\n    return [1, 2, 3]\n"),
            ("scripts/run.py", "import common\nfrom common import log_line\n\n\ndef main():\n    log_line(\"x\")\n    return common.fmt(1)\n"),
            ("scripts/common.py", "def log_line(s):\n    print(s)\n\n\ndef fmt(n):\n    return str(n)\n"),
        ],
    );
    assert_eq!(split_callers(&r, "make_items"), (strs(&["test_total"]), strs(&[])));
    assert_eq!(split_callers(&r, "log_line"), (strs(&["main"]), strs(&[])));
    assert_eq!(split_callers(&r, "fmt"), (strs(&["main"]), strs(&[])));
    assert_eq!(split_callers(&r, "total"), (strs(&["test_total"]), strs(&[])));
    assert_eq!(callee_names(&r, "main"), ["fmt", "log_line"]);
    assert_eq!(ref_lines(&r, "make_items", "tests/helpers.py:1"), ["tests/helpers.py:1", "tests/test_cart.py:2", "tests/test_cart.py:6"]);
    assert_eq!(ref_lines(&r, "fmt", "scripts/common.py:5"), ["scripts/common.py:5", "scripts/run.py:7"]);
    assert_eq!(deps_of(&r, "scripts/run.py"), pairs(&[("common", "scripts/common.py"), ("common", "scripts/common.py")]));
    assert_eq!(deps_of(&r, "tests/test_cart.py"), pairs(&[("helpers", "tests/helpers.py"), ("shop.cart", "src/shop/cart.py")]));
    assert_eq!(importers(&r, "scripts/common.py"), ["scripts/run.py"]);
    assert_eq!(importers(&r, "tests/helpers.py"), ["tests/test_cart.py"]);

    // the language server's references of `log_line` at its definition
    let uri = format!("file://{}", r.root.join("scripts/common.py").display());
    let msgs = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"rootUri": format!("file://{}", r.root.display()), "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": "textDocument/references", "params": {"textDocument": {"uri": uri}, "position": {"line": 0, "character": 5}, "context": {"includeDeclaration": false}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown"}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ];
    let mut input = Vec::new();
    for m in msgs {
        let body = m.to_string();
        input.extend(format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes());
    }
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned();
    let answer = out.split("Content-Length").find(|m| m.contains("\"id\":2")).unwrap_or_else(|| panic!("{out}"));
    let body: Value = serde_json::from_str(&answer[answer.find('{').unwrap()..]).unwrap();
    let mut got: Vec<String> = body["result"]
        .as_array()
        .unwrap_or_else(|| panic!("{answer}"))
        .iter()
        .map(|l| format!("{}:{}", l["uri"].as_str().unwrap().rsplit('/').next().unwrap(), l["range"]["start"]["line"]))
        .collect();
    got.sort();
    assert_eq!(got, ["run.py:1", "run.py:5"]);
}

#[test]
fn super_with_a_named_class_follows_the_order_of_the_class_it_sits_in() {
    let base = "class Root:\n    def save(self):\n        return 0\n\n\nclass Left(Root):\n    def save(self):\n        return super().save() + 1\n\n\nclass Right(Root):\n    def save(self):\n        return super().save() + 2\n";
    let leaf = "from base import Left, Right\n\n\nclass Leaf(Left, Right):\n    def save(self):\n        return super().save() + 4\n\n    def load(self):\n        return super(Left, self).save()\n";
    let r = Repo::new("super_named", &[("base.py", base), ("leaf.py", leaf)]);
    assert_eq!(callee_names(&r, "Leaf.load"), ["Right.save"]);
    assert_eq!(callee_names(&r, "Leaf.save"), ["Left.save"]);
    assert_eq!(split_callers(&r, "base.py:12"), (strs(&["Leaf.load"]), strs(&[])));
    assert_eq!(ref_lines(&r, "save", "base.py:2"), ["base.py:13", "base.py:2", "base.py:8"]);
}

#[test]
fn a_lambda_parameter_or_block_name_hides_only_its_own_scope() {
    let r = Repo::new(
        "scope",
        &[
            ("a.py", "def log(msg):\n    pass\n\ndef main(xs):\n    log(\"start\")\n    return sorted(xs, key=lambda log: log.size)\n"),
            ("b.js", "function log(m) {}\n\nfunction f(ok) {\n  if (ok) { const log = 1; }\n  log(\"x\");\n}\n"),
        ],
    );
    let got = r.resolved("log");
    assert!(got.contains(&("a.py:5".to_string(), "a.py".to_string())), "{got:?}");
    assert!(got.contains(&("b.js:5".to_string(), "b.js".to_string())), "{got:?}");
    assert!(got.contains(&("a.py:6".to_string(), "-".to_string())), "{got:?}");
    assert!(got.contains(&("b.js:4".to_string(), "-".to_string())), "{got:?}");
}

#[test]
fn a_file_with_one_bad_byte_is_resolved_and_queried() {
    let files: &[(&str, &[u8])] =
        &[("a.py", b"def helper():\n    return 1\n"), ("b.py", b"# caf\xe9\nfrom a import helper\n\ndef latin_helper():\n    return helper()\n")];
    let r = Repo::at(scratch("latin"), files);
    r.index();
    assert_eq!(r.resolved("helper"), pairs(&[("a.py:1", "a.py"), ("b.py:2", "a.py"), ("b.py:5", "a.py")]));
    let (code, out) = r.run(&["tsquery", "python", "(function_definition name: (identifier) @n)", "."]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("latin_helper"), "{out}");
}

#[test]
fn a_use_after_a_bom_or_a_bad_byte_on_its_line_is_resolved() {
    let files: &[(&str, &[u8])] = &[
        ("helper.py", b"def go():\n    return 1\n"),
        ("bom.py", b"\xef\xbb\xbffrom helper import go\n\ndef run():\n    return go()\n"),
        ("latin.py", b"from helper import go\nx = \"caf\xe9 au lait\"; go()\n"),
        ("both.py", b"\xef\xbb\xbfx = \"caf\xe9\"; from helper import go\ngo()\n"),
    ];
    let r = Repo::at(scratch("bom_latin"), files);
    r.index();
    let want = [
        ("bom.py:1", "helper.py"),
        ("bom.py:4", "helper.py"),
        ("both.py:1", "helper.py"),
        ("both.py:2", "helper.py"),
        ("helper.py:1", "helper.py"),
        ("latin.py:1", "helper.py"),
        ("latin.py:2", "helper.py"),
    ];
    assert_eq!(r.resolved("go"), pairs(&want));
    let v = r.json(&["refs", "go", "--json"]);
    let cols: Vec<u64> =
        v["definitions"][0]["references"].as_array().unwrap().iter().filter(|o| o["path"] == "latin.py").map(|o| o["column"].as_u64().unwrap()).collect();
    // columns stay the byte columns `grep` prints
    assert_eq!(cols, vec![20, 21], "{v}");
}

#[test]
fn every_use_on_one_long_line_is_counted_up_to_max_results() {
    let line = format!("import {{g}} from \"./lib\";{}\n", "g();".repeat(2000));
    let r = Repo::new("one_line", &[("lib.js", "export function g() { return 1 }\n"), ("min.js", &line)]);
    // every reference repeats the long line: more output than a pipe holds, so read it as it comes
    let run = |args: &[&str]| String::from_utf8(Command::new(BIN).args(args).current_dir(&r.root).output().unwrap().stdout).unwrap();
    let count = |v: &Value| v["definitions"][0]["references"].as_array().unwrap().iter().filter(|o| o["path"] == "min.js").count();
    let v: Value = serde_json::from_str(&run(&["refs", "g", "--json", "--max-results", "5000"])).unwrap();
    assert_eq!((count(&v), &v["truncated"]), (2001, &json!(false)));
    let v: Value = serde_json::from_str(&run(&["refs", "g", "--json", "--max-results", "100"])).unwrap();
    assert_eq!(&v["truncated"], &json!(true));
    assert!(count(&v) < 100, "{}", count(&v));
    assert!(run(&["refs", "g", "--max-results", "100"]).contains("stopped at max_results=100"));
    // a use on a long line shows the part of the line around it, not the 8 KB line 2,000 times
    let text = run(&["refs", "g", "--max-results", "5000"]);
    assert!(text.len() < 2001 * 600, "{} bytes", text.len());
    let third = text.lines().find(|l| l.contains("min.js:1:33:")).unwrap();
    assert!(third.contains("...import {g} from \"./lib\";g();g();") || third.contains(": import {g} from \"./lib\";g();g();"), "{third}");
    assert!(third.ends_with("..."), "{third}");
    let v: Value = serde_json::from_str(&run(&["refs", "g", "--json", "--max-results", "5000"])).unwrap();
    let uses = v["definitions"][0]["references"].as_array().unwrap();
    let long = uses.iter().find(|o| o["path"] == "min.js" && o["column"] == 4001).unwrap();
    assert_eq!(long["cut"], json!(true), "{long}");
    let shown = long["text"].as_str().unwrap();
    assert!(shown.len() < 400 && shown.starts_with("...") && shown.ends_with("...") && shown.contains("g();g();"), "{shown}");
    // a short line is whole and says nothing about a cut
    let short = &v["definitions"][0];
    assert!(short["references"].as_array().unwrap().iter().all(|o| o["path"] != "lib.js" || o.get("cut").is_none()), "{short}");
}

/// `refs NAME` on `make(n)` and `make(4 * n)` (best of 3 runs each) fails if the larger costs 10x
/// or more, where linear work gives 4x and quadratic 16x; `want(n)` is the count it must print.
fn assert_refs_scale_linearly(file: &str, other: (&str, &str), name: &str, n: usize, make: impl Fn(usize) -> String, want: impl Fn(usize) -> String) {
    let best = |k: usize| {
        let r = Repo::new("refs_scale", &[other, (file, &make(k))]);
        (0..3)
            .map(|_| {
                let t = Instant::now();
                let out = Command::new(BIN).args(["refs", name, "--max-results", "50000"]).current_dir(&r.root).output().unwrap();
                let took = t.elapsed();
                let out = String::from_utf8_lossy(&out.stdout).into_owned();
                assert!(out.contains(&want(k)), "{}", out.lines().next().unwrap_or(""));
                took
            })
            .min()
            .unwrap()
    };
    let (small, large) = (best(n), best(4 * n));
    let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.005);
    assert!(ratio < 10.0, "{file}: {n} uses took {small:?}, {} took {large:?}", 4 * n);
}

#[test]
fn refs_in_one_big_function_takes_linear_time() {
    assert_refs_scale_linearly(
        "bundle.js",
        ("lib.js", "export function g() { return 1 }\n"),
        "g",
        500,
        |n| format!("import {{g}} from \"./lib\";\nfunction f(){{var a=0;\n{}}}\n", "g();\n".repeat(n)),
        |n| format!("({} references)", n + 2),
    );
    assert_refs_scale_linearly(
        "A.java",
        ("B.java", "class B {}\n"),
        "x",
        500,
        |n| format!("class A {{\n  void f() {{\n    int x = 0;\n{}  }}\n}}\n", "    System.out.println(x);\n".repeat(n)),
        |n| format!("unresolved ({}):", n + 1),
    );
}

#[test]
fn refs_through_a_typed_variable_takes_linear_time() {
    let n = 500;
    let uses = |n: usize, line: &str| line.repeat(n);
    let want = |n: usize| format!("({} references)", n + 1);
    assert_refs_scale_linearly(
        "big.py",
        ("lib.py", "class A:\n    def m(self):\n        pass\n"),
        "m",
        n,
        |n| format!("from lib import A\n\ndef f():\n    x = A()\n{}", uses(n, "    x.m()\n")),
        want,
    );
    assert_refs_scale_linearly(
        "big.js",
        ("lib.js", "export class A {\n  m() {\n    return 1;\n  }\n}\n"),
        "m",
        n,
        |n| format!("import {{ A }} from \"./lib\";\nfunction f() {{\n  const x = new A();\n{}}}\n", uses(n, "  x.m();\n")),
        want,
    );
    assert_refs_scale_linearly(
        "big.ts",
        ("lib.ts", "export class A {\n  m(): number {\n    return 1;\n  }\n}\n"),
        "m",
        n,
        |n| format!("import {{ A }} from \"./lib\";\nfunction f(): void {{\n  const x: A = new A();\n{}}}\n", uses(n, "  x.m();\n")),
        want,
    );
    assert_refs_scale_linearly(
        "src/lib.rs",
        ("Cargo.toml", "[package]\nname = \"t\"\n"),
        "m",
        n,
        |n| format!("pub struct A;\n\nimpl A {{\n    pub fn m(&self) {{}}\n}}\n\npub fn f() {{\n    let x = A {{}};\n{}}}\n", uses(n, "    x.m();\n")),
        want,
    );
    assert_refs_scale_linearly(
        "main.go",
        ("go.mod", "module t\n"),
        "M",
        n,
        |n| format!("package main\n\ntype A struct{{}}\n\nfunc (a A) M() {{}}\n\nfunc f() {{\n\tx := A{{}}\n{}}}\n", uses(n, "\tx.M()\n")),
        want,
    );
}

#[test]
fn refs_of_a_name_bound_in_many_blocks_of_one_function_takes_linear_time() {
    let n = 300;
    let blocks = |n: usize, block: &str| (0..n).map(|i| block.replace('N', &i.to_string())).collect::<String>();
    let want = |n: usize| format!("unresolved ({}):", 2 * n);
    let cases = [
        ("a.py", ("b.py", ""), "def f(c):\n", "    ys = [x + N for x in c]\n", ""),
        ("a.js", ("b.js", ""), "function f() {\n", "  { let x = N; g(x); }\n", "}\n"),
        ("a.ts", ("b.ts", ""), "function f(): void {\n", "  { const x: number = N; g(x); }\n", "}\n"),
        ("src/lib.rs", ("Cargo.toml", "[package]\nname = \"t\"\n"), "pub fn f() {\n", "    { let x = N; g(x); }\n", "}\n"),
        ("main.go", ("go.mod", "module t\n"), "package main\n\nfunc f() {\n", "\t{\n\t\tx := N\n\t\tg(x)\n\t}\n", "}\n"),
        ("A.java", ("B.java", "class B {}\n"), "class A {\n  void f() {\n", "    { int x = N; g(x); }\n", "  }\n}\n"),
        ("c.py", ("b.py", ""), "def f(c):\n", "    if c:\n        x = N\n        g(x)\n", ""),
    ];
    for (file, other, head, block, tail) in cases {
        assert_refs_scale_linearly(file, other, "x", n, |n| format!("{head}{}{tail}", blocks(n, block)), want);
    }
}

#[test]
fn a_method_looked_up_through_a_wide_diamond_of_bases_reads_each_base_once() {
    // three levels of classes that each extend every class of the level below, and calls of a
    // method none of them has: the cost grows with the bases read, not with the paths to them
    let make = |w: usize, py: bool| {
        let names = |l: usize| (0..w).map(|i| format!("C{l}_{i}")).collect::<Vec<_>>().join(", ");
        let mut src = String::new();
        for l in (1..=3).rev() {
            for i in 0..w {
                let below = if l < 3 { names(l + 1) } else { String::new() };
                src += &match (py, below.is_empty()) {
                    (true, _) => format!("class C{l}_{i}({below}):\n    pass\n\n"),
                    (false, true) => format!("class C{l}_{i} {{}}\n"),
                    (false, false) => format!("class C{l}_{i} implements {below} {{}}\n"),
                };
            }
        }
        if py {
            format!("{src}class Top({}):\n    pass\n\ndef f(t: Top):\n{}", names(1), "    t.z()\n".repeat(40))
        } else {
            format!("{src}class Top implements {} {{}}\nfunction f(t: Top) {{\n{}}}\n", names(1), "  t.z();\n".repeat(40))
        }
    };
    for (file, other) in
        [("a.py", ("b.py", "class Other:\n    def z(self):\n        return 2\n")), ("a.ts", ("b.ts", "class Other { z(): number { return 2; } }\n"))]
    {
        let best = |w: usize| {
            let r = Repo::new("diamond", &[other, (file, &make(w, file.ends_with(".py")))]);
            (0..3)
                .map(|_| {
                    let t = Instant::now();
                    let out = Command::new(BIN).args(["refs", "z"]).current_dir(&r.root).output().unwrap();
                    let took = t.elapsed();
                    assert!(String::from_utf8_lossy(&out.stdout).contains("(1 references)"));
                    took
                })
                .min()
                .unwrap()
        };
        // three times as wide: 9 times the bases read, 27 times the paths
        let (small, large) = (best(7), best(21));
        let ratio = large.as_secs_f64() / small.as_secs_f64().max(0.005);
        assert!(ratio < 8.0, "{file}: width 7 took {small:?}, width 21 took {large:?}");
    }
}

#[test]
fn a_file_nested_too_deeply_is_reported_unread_and_one_nested_500_deep_resolves() {
    let class = ("b.py", "class A:\n    def m(self):\n        pass\n");
    let deep = |head: &str, n: usize| format!("from b import A\n\ndef f():\n    {head}{}A(){}\n    x.m()\n", "(".repeat(n), ")".repeat(n));
    // a typed binder with a deep constructor, and a use of `A` deep inside; each run is killed after 20 s
    for (head, name) in [("x: A = ", "m"), ("x = ", "A")] {
        let r = Repo::new("too_deep", &[class, ("a.py", &deep(head, 200_000))]);
        let (code, out) = r.run(&["refs", name, "--max-results", "50"]);
        assert_eq!(code, 0, "{}", &out[..out.len().min(2000)]);
        let line = out.lines().find(|l| l.contains("a.py:") && l.contains("nested too deeply to read")).map(str::to_string);
        assert!(line.is_some(), "{}", &out[out.len().saturating_sub(2000)..]);
    }
    let r = Repo::new("deep_500", &[class, ("a.py", &deep("x: A = ", 500))]);
    assert_eq!(r.resolved("m"), pairs(&[("a.py:5", "b.py"), ("b.py:2", "b.py")]));
    assert_eq!(r.resolved("A"), pairs(&[("a.py:1", "b.py"), ("a.py:4", "b.py"), ("a.py:4", "b.py"), ("b.py:1", "b.py")]));
}

#[test]
fn a_call_in_a_file_nested_too_deeply_is_kept_as_a_caller() {
    let deep = format!("from b import go\n\ndef f():\n    x = {}1{}\n    go()\n", "(".repeat(2100), ")".repeat(2100));
    let r = Repo::new("deep_caller", &[("b.py", "def go():\n    pass\n"), ("a.py", &deep)]);
    // the resolver does not read the file, so the call is not decided to be another `go`
    let (decided, undecided) = split_callers(&r, "go");
    assert_eq!([decided, undecided].concat(), strs(&["f"]));
}

#[test]
fn an_indexed_file_now_holding_a_bom_and_a_nul_byte_does_not_panic_the_resolver() {
    let r = Repo::new("bom_nul", &[("a.py", "from b import foo\nfoo()\n"), ("b.py", "def foo():\n    pass\n")]);
    fs::write(r.root.join("b.py"), b"\xef\xbb\xbfdef foo():\n    pass\n\x00\n").unwrap();
    let got = scrutate::names::Names::new(&r.lib()).unwrap().references("foo", 50).unwrap();
    // the index is not refreshed: its definition of `foo` in b.py still stands
    let a: Vec<_> = got.occurrences.iter().filter(|o| o.path == "a.py").map(|o| (o.line, o.resolution.is_ok())).collect();
    assert_eq!(a, vec![(1, true), (2, true)]);
}

#[test]
fn a_relative_import_that_climbs_above_the_root_names_no_file() {
    let r = Repo::new(
        "above_root",
        &[
            ("lib.js", "export function h() { return 1 }\n"),
            ("src/a.js", "import { h } from '../../lib';\nh();\n"),
            ("src/c.js", "import { h } from '../lib';\nh();\n"),
            ("helper.py", "def go():\n    return 1\n"),
            ("src/__init__.py", ""),
            ("src/b.py", "from ...helper import go\ngo()\n"),
            ("src/d.py", "from ..helper import go\ngo()\n"),
        ],
    );
    let resolved = |path: &str| -> Vec<String> {
        r.json(&["deps", path, "--json"]).as_array().unwrap().iter().filter_map(|d| d["resolved"].as_str().map(str::to_string)).collect()
    };
    assert_eq!(resolved("src/a.js"), Vec::<String>::new());
    assert_eq!(resolved("src/b.py"), Vec::<String>::new());
    assert_eq!(resolved("src/c.js"), vec!["lib.js"]);
    assert_eq!(resolved("src/d.py"), vec!["helper.py"]);
    let got = r.resolved("h");
    assert!(got.contains(&("src/a.js:2".to_string(), "-".to_string())) && got.contains(&("src/c.js:2".to_string(), "lib.js".to_string())), "{got:?}");
    let got = r.resolved("go");
    assert!(got.contains(&("src/b.py:2".to_string(), "-".to_string())) && got.contains(&("src/d.py:2".to_string(), "helper.py".to_string())), "{got:?}");
    let (_, out) = r.run(&["rdeps", "lib.js"]);
    assert!(out.contains("src/c.js") && !out.contains("src/a.js"), "{out}");
}

#[test]
fn tsquery_skips_files_over_the_index_size_limit() {
    let big = format!("def small():\n    pass\n{}", "x = 1\n".repeat(200_000));
    let r = Repo::new("bigts", &[("big.py", &big), ("ok.py", "def fine():\n    pass\n")]);
    let (_, out) = r.run(&["tsquery", "python", "(function_definition name: (identifier) @n)", "."]);
    assert!(out.contains("fine") && !out.contains("small"), "{out}");
}

fn dup_pair(a_op: &str, b_op: &str) -> Repo {
    let body = |op: &str, cmp: &str, logic: &str| {
        format!(
            "    total = 0\n    for item in items:\n        if item.value {cmp} and item.flag {logic} item.ready:\n            total = total {op} item.value\n        else:\n            total = total {op} 1\n    return total\n"
        )
    };
    let a = format!("def first(items):\n{}\n", body(a_op, "> 100", "and"));
    let b = format!("def second(items):\n{}\n", body(b_op, if a_op == b_op { "> 100" } else { "< 5" }, if a_op == b_op { "and" } else { "or" }));
    Repo::new("dupes", &[("a.py", &a), ("b.py", &b)])
}

#[test]
fn dupes_exact_means_the_same_body_text() {
    let r = dup_pair("+", "-");
    let (_, out) = r.run(&["dupes", "--min-tokens", "10", "--threshold", "0.5"]);
    assert!(!out.contains("exact"), "{out}");
    let (_, exact_only) = r.run(&["dupes", "--min-tokens", "10", "--threshold", "1"]);
    assert!(!exact_only.contains("exact") && !exact_only.contains("near"), "{exact_only}");
    let same = dup_pair("+", "+");
    let (_, out) = same.run(&["dupes", "--min-tokens", "10", "--threshold", "1"]);
    assert!(out.contains("exact"), "{out}");
}

#[test]
fn reindex_force_sees_an_edit_that_keeps_size_and_mtime() {
    let r = Repo::new("force", &[("a.py", "def alpha_name():\n    return 1\n")]);
    let p = r.root.join("a.py");
    let mtime = fs::metadata(&p).unwrap().modified().unwrap();
    fs::write(&p, "def omega_name():\n    return 1\n").unwrap();
    fs::File::options().write(true).open(&p).unwrap().set_modified(mtime).unwrap();
    let mut lib = r.lib();
    let plain = scrutate::mcp::call_tool(&mut lib, "reindex", &json!({}), &scrutate::mcp::Settings::new(None, Some(0.0)));
    assert!(!plain.is_error, "{}", plain.text);
    let forced = scrutate::mcp::call_tool(&mut lib, "reindex", &json!({"force": true}), &scrutate::mcp::Settings::new(None, Some(0.0)));
    assert!(!forced.is_error, "{}", forced.text);
    let found = scrutate::mcp::call_tool(&mut lib, "find_symbol", &json!({"name": "omega_name"}), &scrutate::mcp::Settings::new(None, Some(0.0)));
    assert!(found.text.contains("a.py:1-2"), "{}", found.text);
    let wrong = scrutate::mcp::call_tool(&mut lib, "reindex", &json!({"force": "yes"}), &scrutate::mcp::Settings::new(None, Some(0.0)));
    assert!(wrong.is_error, "{}", wrong.text);
}

#[test]
fn wildcard_import_rings_resolve_in_bounded_time() {
    let n = 16;
    let mut files: Vec<(String, String)> = Vec::new();
    for i in 0..n {
        let mut src = String::new();
        for j in 0..n {
            if j != i {
                src.push_str(&format!("from m{j} import *\n"));
            }
        }
        src.push_str(&format!("\ndef f{i}():\n    return missing_name()\n"));
        files.push((format!("m{i}.py"), src));
    }
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let r = Repo::new("ring", &refs);
    let start = Instant::now();
    let (code, out) = r.run(&["refs", "missing_name"]);
    assert_eq!(code, 0, "{out}");
    assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
}

#[test]
fn rdeps_of_a_file_skips_imports_of_a_namesake_file() {
    let r = Repo::new(
        "rdeps",
        &[
            ("a/util.h", "int a_util(void);\n"),
            ("b/util.h", "int b_util(void);\n"),
            ("a/main.c", "#include \"util.h\"\nint main(void) { return a_util(); }\n"),
            ("b/main.c", "#include \"util.h\"\nint main(void) { return b_util(); }\n"),
        ],
    );
    let v = r.json(&["rdeps", "a/util.h", "--json"]);
    let s = v.to_string();
    assert!(s.contains("a/main.c") && !s.contains("b/main.c"), "{s}");
}

#[test]
fn alt_queries_are_clamped_and_a_cancelled_search_stops() {
    let Ok(scrutate::ops::Op::Search { alt, .. }) = scrutate::ops::Op::from_tool("search_code", &json!({"query": "a", "alt_queries": vec!["b"; 300]})) else {
        panic!("search_code parses")
    };
    assert_eq!(alt.len(), scrutate::ops::ALT_QUERIES);
    let r = Repo::new("cancel", &[("a.py", "def refresh_token():\n    return 1\n")]);
    let mut lib = r.lib();
    let settings = scrutate::mcp::Settings::new(None, Some(0.0));
    let args = json!({"query": "refresh", "alt_queries": ["renew token"]});
    assert!(!scrutate::mcp::call_tool(&mut lib, "search_code", &args, &settings).is_error);
    lib.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    let c = scrutate::mcp::call_tool(&mut lib, "search_code", &args, &settings);
    assert!(c.is_error && c.text.contains("cancelled"), "{}", c.text);
}

#[test]
fn searcher_callers_and_callees_keep_namesakes_apart() {
    let r = Repo::new(
        "namesakes",
        &[
            ("helper.py", "def dup_name():\n    return 'A'\n\ndef call_a():\n    return dup_name()\n"),
            ("other.py", "def dup_name():\n    return 'B'\n\ndef call_b():\n    return dup_name()\n"),
        ],
    );
    let lib = r.lib();
    let callers: Vec<String> = lib.callers("helper.py:1-2", 50).unwrap().into_iter().map(|h| h.qualname).collect();
    assert_eq!(callers, ["call_a"]);
    let (resolved, _) = lib.callees("other.py:4-5", 50).unwrap();
    assert_eq!(resolved.iter().map(|h| h.path.as_str()).collect::<Vec<_>>(), ["other.py"]);
    let refs = scrutate::intel::references(&lib, "dup_name", 100).unwrap();
    assert_eq!(refs.callers.len(), 2);
}

#[test]
fn functions_that_differ_only_in_a_literal_are_near_duplicates() {
    let f = |name: &str, k: u32| format!("def {name}(items):\n    total = 0\n    for item in items:\n        total += item * {k}\n    return total\n");
    let (a, b, c) = (f("sum_a", 2), f("sum_b", 5), f("sum_c", 2));
    let two = Repo::new("dupes_literal", &[("a.py", &a), ("b.py", &b)]);
    let (_, out) = two.run(&["dupes", "--min-tokens", "1", "--threshold", "0"]);
    assert!(out.contains("near") && out.contains("1.00") && out.contains("sum_a") && out.contains("sum_b"), "{out}");
    let (_, exact_only) = two.run(&["dupes", "--min-tokens", "1", "--threshold", "1"]);
    assert!(!exact_only.contains("sum_a"), "{exact_only}");
    // one group however many share the words: the pairwise search gives up on large buckets
    let many: Vec<(String, String)> = (0..100).map(|i| (format!("m{i}.py"), f(&format!("sum_{i}"), i))).collect();
    let refs: Vec<(&str, &str)> = many.iter().map(|(p, s)| (p.as_str(), s.as_str())).collect();
    let big = Repo::new("dupes_many", &refs);
    let (_, out) = big.run(&["dupes", "--min-tokens", "1", "--threshold", "0.5", "--limit", "5"]);
    assert!(out.contains("near") && out.matches("sum_").count() >= 100, "{out}");
    let three = Repo::new("dupes_three", &[("a.py", &a), ("b.py", &b), ("c.py", &c)]);
    let (_, out) = three.run(&["dupes", "--min-tokens", "1", "--threshold", "0"]);
    assert!(out.contains("exact") && out.contains("near") && out.contains("sum_b"), "{out}");
}

#[test]
fn callers_of_a_common_name_list_the_resolved_ones_first() {
    let unsure = |f: &str| format!("pub fn {f}(x: &dyn Make) {{\n    x.new();\n}}\n");
    let r = Repo::new(
        "callers_order",
        &[
            ("crates/aaa/src/lib.rs", &unsure("a_far")),
            (
                "crates/one/src/lib.rs",
                "pub struct Id;\n\nimpl Id {\n    pub fn new() -> Id {\n        Id\n    }\n}\n\npub fn z_resolved() -> Id {\n    Id::new()\n}\n",
            ),
            ("crates/one/src/near.rs", &unsure("n_near")),
            ("crates/bbb/src/lib.rs", "pub fn b_names_owner(y: &dyn Make) -> Option<Id> {\n    y.new()\n}\n"),
            ("crates/two/src/lib.rs", "pub struct Other;\n\nimpl Other {\n    pub fn new() -> Other {\n        Other\n    }\n}\n"),
        ],
    );
    let lib = r.lib();
    let names = |name: &str, limit: usize| -> Vec<String> { lib.callers(name, limit).unwrap().into_iter().map(|h| h.qualname).collect() };
    assert_eq!(names("Id.new", 1), ["z_resolved"]);
    assert_eq!(names("Id.new", 4), ["z_resolved", "b_names_owner", "n_near", "a_far"]);
    assert_eq!(names("Other.new", 4), ["a_far", "b_names_owner", "n_near"]);

    let (code, out) = r.run(&["callers", "Other.new"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.trim_end().ends_with("(the resolver could not tell whether any of these calls this definition or another of the same name)"), "{out}");
    let (_, out) = r.run(&["callers", "Id.new"]);
    assert!(out.contains("(3 of these call a definition of that name"), "{out}");
}

#[test]
fn refs_reads_type_colon_colon_method_like_type_dot_method() {
    let src = "pub struct Config;\nimpl Config {\n    pub fn make() -> Config {\n        Config\n    }\n}\npub fn start() -> Config {\n    Config::make()\n}\n";
    let r = Repo::new("refs_colons", &[("src/lib.rs", src)]);
    let (_, dotted) = r.run(&["refs", "Config.make"]);
    let (code, colons) = r.run(&["refs", "Config::make"]);
    assert_eq!(code, 0, "{colons}");
    for out in [&dotted, &colons] {
        assert!(out.contains("Config.make  (2 references)") && out.contains("src/lib.rs:8:13"), "{out}");
    }
}

/// (module, resolved file) of every import `deps` lists for `path`.
fn deps_of(r: &Repo, path: &str) -> Vec<(String, String)> {
    let v = r.json(&["deps", path, "--json"]);
    let mut out: Vec<(String, String)> =
        v.as_array().unwrap().iter().map(|d| (d["module"].as_str().unwrap().to_string(), d["resolved"].as_str().unwrap_or("-").to_string())).collect();
    out.sort();
    out
}

/// The files `rdeps` lists as importing `target`.
fn importers(r: &Repo, target: &str) -> Vec<String> {
    let (_, out, _) = r.exec(&["rdeps", target, "--json"]);
    let v: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
    let mut files: Vec<String> = v.as_array().unwrap().iter().map(|d| d["path"].as_str().unwrap().to_string()).collect();
    files.sort();
    files.dedup();
    files
}

#[test]
fn deps_and_rdeps_resolve_rust_paths_in_a_crate_below_the_root() {
    for base in ["crates/app/src", "src"] {
        let at = |f: &str| format!("{base}/{f}");
        let (lib, a, b) = (at("lib.rs"), at("a.rs"), at("b.rs"));
        let r = Repo::new(
            "deps_rust",
            &[
                (lib.as_str(), "mod a;\nmod b;\nuse crate::a::thing;\nuse self::b::one;\n\npub fn f() {\n    thing();\n    one();\n}\n"),
                (a.as_str(), "pub fn thing() {}\n"),
                (b.as_str(), "use super::a;\n\npub fn one() {\n    a::thing();\n}\n"),
            ],
        );
        let want = |v: &[(&str, &str)]| v.iter().map(|(m, f)| (m.to_string(), f.to_string())).collect::<Vec<_>>();
        assert_eq!(deps_of(&r, &lib), want(&[("a", &a), ("b", &b), ("crate::a::thing", &a), ("self::b::one", &b)]), "{base}");
        assert_eq!(deps_of(&r, &b), want(&[("super::a", &a)]), "{base}");
        assert_eq!(importers(&r, &a), [b.clone(), lib.clone()], "{base}");
        assert_eq!(importers(&r, &b), [lib.as_str()], "{base}");
    }
}

#[test]
fn a_binary_test_example_bench_or_build_script_is_a_crate_root_of_its_own() {
    let r = Repo::new(
        "rust_roots",
        &[
            ("Cargo.toml", "[package]\nname = \"demo\"\n"),
            ("src/lib.rs", "pub mod util;\n\npub fn helper() -> u8 {\n    1\n}\n"),
            ("src/util.rs", "pub fn util() -> u8 {\n    1\n}\n"),
            ("src/bin/tool.rs", "mod util;\n\nfn main() {\n    crate::util::util();\n    util::util();\n}\n"),
            ("src/bin/util.rs", "pub fn util() -> u8 {\n    3\n}\n"),
            ("src/bin/multi/main.rs", "mod part;\n\nfn main() {\n    part::part();\n    crate::part::part();\n}\n"),
            ("src/bin/multi/part.rs", "pub fn part() {}\n"),
            (
                "tests/it.rs",
                "mod common;\n\n#[test]\nfn t() {\n    let n = common::helper();\n    assert_eq!(n, 2);\n}\n\nfn u(other: &str, it: &str) -> usize {\n    other::helper() + it::helper()\n}\n",
            ),
            ("tests/other.rs", "pub fn helper() -> u8 {\n    5\n}\n"),
            ("tests/common/mod.rs", "pub mod helpers;\n\npub fn helper() -> u8 {\n    crate::common::helpers::deep()\n}\n"),
            ("tests/common/helpers.rs", "pub fn deep() -> u8 {\n    2\n}\n"),
            ("tests/suite/main.rs", "mod case;\n\nfn main() {\n    case::case();\n}\n"),
            ("tests/suite/case.rs", "pub fn case() {}\n"),
            ("examples/demo.rs", "mod shared;\n\nfn main() {\n    shared::shared();\n}\n"),
            ("examples/shared/mod.rs", "pub fn shared() {}\n"),
            ("benches/b.rs", "mod common;\n\nfn main() {\n    common::helper();\n}\n"),
            ("benches/common.rs", "pub fn helper() -> u8 {\n    4\n}\n"),
            ("build.rs", "mod codegen;\n\nfn main() {\n    codegen::codegen();\n}\n"),
            ("codegen.rs", "pub fn codegen() {}\n"),
        ],
    );
    let calls = |name: &str| r.resolved(name).into_iter().filter(|(at, def)| !at.starts_with(def.as_str())).collect::<Vec<_>>();
    assert_eq!(calls("util"), pairs(&[("src/bin/tool.rs:4", "src/bin/util.rs"), ("src/bin/tool.rs:5", "src/bin/util.rs")]));
    // a test file is no module of the one beside it, nor of itself
    assert_eq!(
        calls("helper"),
        pairs(&[("benches/b.rs:4", "benches/common.rs"), ("tests/it.rs:10", "-"), ("tests/it.rs:10", "-"), ("tests/it.rs:5", "tests/common/mod.rs")])
    );
    assert_eq!(calls("deep"), pairs(&[("tests/common/mod.rs:4", "tests/common/helpers.rs")]));
    assert_eq!(calls("part"), pairs(&[("src/bin/multi/main.rs:4", "src/bin/multi/part.rs"), ("src/bin/multi/main.rs:5", "src/bin/multi/part.rs")]));
    assert_eq!(calls("case"), pairs(&[("tests/suite/main.rs:4", "tests/suite/case.rs")]));
    assert_eq!(calls("shared"), pairs(&[("examples/demo.rs:4", "examples/shared/mod.rs")]));
    assert_eq!(calls("codegen"), pairs(&[("build.rs:4", "codegen.rs")]));
    let want = |v: &[(&str, &str)]| v.iter().map(|(m, f)| (m.to_string(), f.to_string())).collect::<Vec<_>>();
    assert_eq!(deps_of(&r, "tests/it.rs"), want(&[("common", "tests/common/mod.rs")]));
    assert_eq!(deps_of(&r, "src/bin/tool.rs"), want(&[("util", "src/bin/util.rs")]));
    assert_eq!(deps_of(&r, "src/lib.rs"), want(&[("util", "src/util.rs")]));
    assert_eq!(importers(&r, "tests/common/mod.rs"), ["tests/it.rs"]);
    assert_eq!(importers(&r, "tests/common/helpers.rs"), ["tests/common/mod.rs"]);
    assert_eq!(importers(&r, "src/bin/util.rs"), ["src/bin/tool.rs"]);
    assert_eq!(importers(&r, "benches/common.rs"), ["benches/b.rs"]);
    assert_eq!(importers(&r, "codegen.rs"), ["build.rs"]);
    assert_eq!(importers(&r, "src/util.rs"), ["src/lib.rs"]);
}

#[test]
fn a_tsconfig_extends_list_is_read_in_order_with_the_later_one_winning() {
    let main = "import { helper } from \"@lib/util\";\n\nexport function run(): number {\n  return helper();\n}\n";
    let alias = |to: &str| format!("{{ \"compilerOptions\": {{ \"baseUrl\": \".\", \"paths\": {{ \"@lib/*\": [\"{to}/*\"] }} }} }}\n");
    let (lib, old) = (alias("lib"), alias("old"));
    let cases: [(&str, &str); 6] = [
        ("{ \"extends\": \"./base.json\" }\n", "lib/util.ts"),
        ("{ \"extends\": [\"./base.json\"] }\n", "lib/util.ts"),
        ("{ \"extends\": [\"./old.json\", \"./base.json\"] }\n", "lib/util.ts"),
        ("{ \"extends\": [\"./base.json\", \"./old\"] }\n", "old/util.ts"),
        // a package or a missing file is passed over, as in the string form
        ("{ \"extends\": [\"./base.json\", \"@tsconfig/node16/tsconfig.json\", \"./missing.json\"] }\n", "lib/util.ts"),
        ("{ \"extends\": [\"./base.json\"], \"compilerOptions\": { \"paths\": { \"@lib/*\": [\"old/*\"] } } }\n", "old/util.ts"),
    ];
    for (config, want) in cases {
        let r = Repo::new(
            "tsext",
            &[
                ("tsconfig.json", config),
                ("base.json", &lib),
                ("old.json", &old),
                ("app/main.ts", main),
                ("lib/util.ts", "export function helper(): number {\n  return 2;\n}\n"),
                ("old/util.ts", "export function helper(): number {\n  return 3;\n}\n"),
            ],
        );
        let uses: Vec<(String, String)> = r.resolved("helper").into_iter().filter(|(at, _)| at.starts_with("app/")).collect();
        assert_eq!(uses, pairs(&[("app/main.ts:1", want), ("app/main.ts:4", want)]), "{config}");
    }
}

#[test]
fn tsconfig_paths_are_relative_to_the_final_base_url_after_extends() {
    let main = "import { helper } from \"@lib/util\";\n\nexport function run(): number {\n  return helper();\n}\n";
    let lib = "export function helper(): number {\n  return 2;\n}\n";
    let paths = "\"paths\": { \"@lib/*\": [\"libs/lib/src/*\"] }";
    let (base_url, base_paths) = (r#"{ "compilerOptions": { "baseUrl": "." } }"#.to_string(), format!("{{ \"compilerOptions\": {{ {paths} }} }}"));
    let both = format!("{{ \"compilerOptions\": {{ \"baseUrl\": \".\", {paths} }} }}");
    let child = |extends: &str, options: &str| format!("{{ \"extends\": {extends}, \"compilerOptions\": {{ {options} }} }}");
    let cases = [
        // the base sets `baseUrl`, the child `paths`: the places are relative to the base's folder
        (base_url.clone(), child("\"../../tsconfig.base.json\"", paths), "libs/lib/src/util.ts"),
        // the base sets `paths`, the child `baseUrl`: relative to the child's `baseUrl`
        (base_paths.clone(), child("\"../../tsconfig.base.json\"", "\"baseUrl\": \".\""), "apps/web/libs/lib/src/util.ts"),
        // the child overrides the base's `baseUrl`
        (both, child("\"../../tsconfig.base.json\"", "\"baseUrl\": \"src\""), "apps/web/src/libs/lib/src/util.ts"),
        // one base of a list sets `baseUrl`, the other `paths`
        (base_url, child("[\"../../tsconfig.base.json\", \"../../configs/paths.json\"]", ""), "libs/lib/src/util.ts"),
        // no `baseUrl` anywhere: relative to the config that sets `paths`
        (base_paths, child("\"../../configs/paths.json\"", ""), "configs/libs/lib/src/util.ts"),
    ];
    for (base, web, want) in cases {
        let r = Repo::new(
            "tsbaseurl",
            &[
                ("tsconfig.base.json", &base),
                ("configs/paths.json", &format!("{{ \"compilerOptions\": {{ {paths} }} }}")),
                ("apps/web/tsconfig.json", &web),
                ("apps/web/main.ts", main),
                ("libs/lib/src/util.ts", lib),
                ("apps/web/libs/lib/src/util.ts", lib),
                ("apps/web/src/libs/lib/src/util.ts", lib),
                ("configs/libs/lib/src/util.ts", lib),
            ],
        );
        let uses: Vec<(String, String)> = r.resolved("helper").into_iter().filter(|(at, _)| at.starts_with("apps/web/main")).collect();
        assert_eq!(uses, pairs(&[("apps/web/main.ts:1", want), ("apps/web/main.ts:4", want)]), "{base} {web}");
    }
}

/// The answer of the language server to one request on `rel` at (0-based) `line` and `character`.
fn lsp_answer(r: &Repo, method: &str, rel: &str, line: u32, character: u32) -> String {
    let uri = format!("file://{}", r.root.join(rel).display());
    let msgs = [
        json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"rootUri": format!("file://{}", r.root.display()), "capabilities": {}}}),
        json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        json!({"jsonrpc": "2.0", "id": 2, "method": method, "params": {"textDocument": {"uri": uri}, "position": {"line": line, "character": character}, "context": {"includeDeclaration": false}}}),
        json!({"jsonrpc": "2.0", "id": 3, "method": "shutdown"}),
        json!({"jsonrpc": "2.0", "method": "exit"}),
    ];
    let mut input = Vec::new();
    for m in msgs {
        let body = m.to_string();
        input.extend(format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes());
    }
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    use std::io::Write;
    child.stdin.take().unwrap().write_all(&input).unwrap();
    let out = String::from_utf8_lossy(&child.wait_with_output().unwrap().stdout).into_owned();
    out.split("Content-Length").find(|m| m.contains("\"id\":2")).unwrap_or_else(|| panic!("{out}")).to_string()
}

#[test]
fn workspace_packages_resolve_by_their_package_json_name() {
    let r = Repo::new(
        "workspace",
        &[
            ("package.json", r#"{ "name": "acme", "private": true, "workspaces": ["packages/*"] }"#),
            ("packages/utils/package.json", r#"{ "name": "@acme/utils", "exports": { ".": "./src/index.ts", "./math": "./src/math.ts" } }"#),
            ("packages/utils/src/index.ts", "export function slugify(s: string): string {\n  return s.toLowerCase();\n}\n"),
            ("packages/utils/src/math.ts", "export function half(n: number): number {\n  return n / 2;\n}\n"),
            ("packages/legacy/package.json", r#"{ "name": "legacy", "main": "dist/index.js" }"#),
            ("packages/legacy/src/index.ts", "export function old(n: number): number {\n  return n;\n}\n"),
            ("packages/app/package.json", r#"{ "name": "@acme/app", "dependencies": { "@acme/utils": "workspace:*", "legacy": "workspace:*" } }"#),
            (
                "packages/app/src/main.ts",
                "import { slugify } from \"@acme/utils\";\nimport { half } from \"@acme/utils/math\";\nimport { old } from \"legacy\";\nimport React from \"react\";\n\nexport function render(title) {\n  return slugify(title) + old(half(4)) + React.version;\n}\n",
            ),
        ],
    );
    let app = "packages/app/src/main.ts";
    let (utils, math, legacy) = ("packages/utils/src/index.ts", "packages/utils/src/math.ts", "packages/legacy/src/index.ts");
    for name in ["slugify", "half", "old"] {
        let (code, out) = r.run(&["callers", name]);
        assert_eq!(code, 0, "{out}");
        assert!(out.contains(app) && out.contains("render"), "callers {name}: {out}");
    }
    let (code, out) = r.run(&["callees", "render"]);
    assert_eq!(code, 0, "{out}");
    for file in [utils, math, legacy] {
        assert!(out.contains(file), "callees render: {out}");
    }
    assert_eq!(r.resolved("slugify"), pairs(&[(&format!("{app}:1"), utils), (&format!("{app}:7"), utils), (&format!("{utils}:1"), utils)]));
    assert_eq!(
        deps_of(&r, app),
        [("@acme/utils", utils), ("@acme/utils/math", math), ("legacy", legacy), ("react", "-")].map(|(m, f)| (m.to_string(), f.to_string()))
    );
    for file in [utils, math, legacy] {
        assert_eq!(importers(&r, file), [app], "rdeps {file}");
    }
    let answer = lsp_answer(&r, "textDocument/references", utils, 0, 17);
    assert!(answer.contains(&format!("{app}\"")) && answer.contains("\"line\":6"), "{answer}");
}

/// `path:start` of every definition `find_symbol` returns for `name`.
fn symbol_at(lib: &scrutate::Repo, name: &str) -> Vec<String> {
    let mut v: Vec<String> = lib.find_symbol(name, 50).unwrap().into_iter().map(|h| format!("{}:{}", h.path, h.start)).collect();
    v.sort();
    v
}

#[test]
fn a_name_is_looked_up_as_typed_before_its_dotted_form() {
    let r = Repo::new(
        "sym_typed",
        &[
            ("conv.scm", "(define (list->vec l)\n  (list->vector l))\n"),
            ("core.clj", "(ns app.core)\n\n(defn map->query [m]\n  m)\n"),
            ("util.el", "(defun my-string->list (s)\n  (split-string s))\n"),
            ("mod.rb", "class Foo::Bar\n  def call\n    1\n  end\nend\n\nmodule Outer::Inner\n  def self.go\n    2\n  end\nend\n"),
            ("Pkg.pm", "package Foo::Util;\n\nsub trim {\n    return 1;\n}\n\nsub Foo::Util::extra {\n    return 1;\n}\n\n1;\n"),
            ("style.css", "#main {\n  color: red;\n}\n"),
            ("cart.js", "class Cart {\n  #secret() {\n    return 1;\n  }\n}\n"),
            ("vault.rb", "class Vault\n  def secret\n    2\n  end\nend\n"),
        ],
    );
    let lib = r.lib();
    for (name, want) in [
        ("list->vec", "conv.scm:1"),
        ("map->query", "core.clj:3"),
        ("my-string->list", "util.el:1"),
        ("Foo::Bar", "mod.rb:1"),
        ("Foo::Bar.call", "mod.rb:2"),
        ("Foo::Bar#call", "mod.rb:2"),
        ("Outer::Inner.go", "mod.rb:8"),
        ("Foo::Util", "Pkg.pm:1"),
        ("Foo::Util::extra", "Pkg.pm:7"),
        ("#main", "style.css:1"),
        ("#secret", "cart.js:2"),
        ("Cart.#secret", "cart.js:2"),
        ("Vault#secret", "vault.rb:2"),
    ] {
        assert_eq!(symbol_at(&lib, name), [want], "{name}");
    }
}

#[test]
fn deps_and_rdeps_resolve_go_imports_through_go_mod() {
    let r = Repo::new(
        "deps_go",
        &[
            ("go.mod", "module example.com/app\n\ngo 1.22\n"),
            ("main.go", "package main\n\nimport \"example.com/app/util\"\n\nfunc main() {\n\tutil.Do()\n}\n"),
            ("util/util.go", "package util\n\nfunc Do() {}\n"),
            ("svc/go.mod", "module example.com/svc\n"),
            ("svc/cmd/main.go", "package main\n\nimport (\n\t\"fmt\"\n\t\"example.com/svc/lib\"\n)\n\nfunc main() {\n\tfmt.Println(lib.Run())\n}\n"),
            ("svc/lib/lib.go", "package lib\n\nfunc Run() int { return 1 }\n"),
        ],
    );
    assert_eq!(deps_of(&r, "main.go"), [("example.com/app/util".to_string(), "util/util.go".to_string())]);
    assert_eq!(deps_of(&r, "svc/cmd/main.go"), [("example.com/svc/lib".to_string(), "svc/lib/lib.go".to_string()), ("fmt".to_string(), "-".to_string())]);
    assert_eq!(importers(&r, "util/util.go"), ["main.go"]);
    assert_eq!(importers(&r, "svc/lib/lib.go"), ["svc/cmd/main.go"]);
}

#[test]
fn deps_and_rdeps_resolve_python_names_imported_from_a_package() {
    let r = Repo::new(
        "deps_py",
        &[
            ("pkg/__init__.py", ""),
            ("pkg/a.py", "from . import b\nfrom . import c as cc\nfrom pkg import c\nfrom pkg import helper\n"),
            ("pkg/b.py", "X = 1\n"),
            ("pkg/c.py", "Y = 2\n"),
            ("ns/x.py", "from . import y\n"),
            ("ns/y.py", "Z = 3\n"),
        ],
    );
    let got = deps_of(&r, "pkg/a.py");
    for want in [(".", "pkg/b.py"), (".", "pkg/c.py"), ("pkg", "pkg/c.py"), ("pkg", "pkg/__init__.py")] {
        assert!(got.contains(&(want.0.to_string(), want.1.to_string())), "{want:?} in {got:?}");
    }
    assert!(!got.iter().any(|(m, f)| m == "." && f == "pkg/__init__.py"), "{got:?}");
    assert_eq!(importers(&r, "pkg/c.py"), ["pkg/a.py"]);
    assert_eq!(importers(&r, "pkg/b.py"), ["pkg/a.py"]);
    assert_eq!(deps_of(&r, "ns/x.py"), [(".".to_string(), "ns/y.py".to_string())]);
    assert_eq!(importers(&r, "ns/y.py"), ["ns/x.py"]);
}

#[test]
fn deps_and_rdeps_resolve_typescript_esm_names_and_declaration_files() {
    let r = Repo::new(
        "deps_ts",
        &[
            ("src/main.ts", "import { c } from './lib/c.js';\nimport type { T } from './types';\n\nexport const v: T = c;\n"),
            ("src/lib/c.ts", "export const c = 1;\n"),
            ("src/types.d.ts", "export type T = number;\n"),
        ],
    );
    assert_eq!(deps_of(&r, "src/main.ts"), [("./lib/c.js".to_string(), "src/lib/c.ts".to_string()), ("./types".to_string(), "src/types.d.ts".to_string())]);
    assert_eq!(importers(&r, "src/lib/c.ts"), ["src/main.ts"]);
    assert_eq!(importers(&r, "src/types.d.ts"), ["src/main.ts"]);
}

#[test]
fn rdeps_counts_unresolved_imports_that_may_name_the_file_instead_of_saying_nothing_imports_it() {
    let r = Repo::new("rdeps_unsure", &[("lib/zz.py", "Q = 1\n"), ("app.py", "import zz\nimport os\n")]);
    let (code, out) = r.run(&["rdeps", "lib/zz.py"]);
    assert_ne!(code, 2, "{out}");
    assert!(!out.contains("nothing imports"), "{out}");
    assert!(out.contains("1 import") && out.contains("could not be resolved"), "{out}");
    let lone = Repo::new("rdeps_none", &[("lib/zz.py", "Q = 1\n"), ("app.py", "import os\n")]);
    assert_eq!(lone.run(&["rdeps", "lib/zz.py"]).1.trim(), "nothing imports lib/zz.py");
}

#[test]
fn a_method_that_wraps_a_namesake_is_not_its_own_caller_or_callee() {
    let rs = "pub struct Inner;\n\nimpl Inner {\n    pub fn len(&self) -> usize {\n        0\n    }\n}\n\npub struct Outer {\n    inner: Inner,\n}\n\nimpl Outer {\n    pub fn len(&self) -> usize {\n        self.inner.len()\n    }\n}\n\npub fn use_outer(o: &Outer) -> usize {\n    o.len()\n}\n";
    let py = "class Inner:\n    def close(self):\n        return 1\n\n\nclass Outer:\n    def __init__(self):\n        self.inner = Inner()\n\n    def close(self):\n        return self.inner.close()\n\n\ndef shutdown(o: Outer):\n    return o.close()\n";
    let r = Repo::new("wrapper", &[("src/lib.rs", rs), ("wrap.py", py)]);
    let lib = r.lib();
    let callers = |n: &str| lib.callers(n, 50).unwrap().into_iter().map(|h| h.qualname).collect::<Vec<_>>();
    let callees = |n: &str| lib.callees(n, 50).unwrap().0.into_iter().map(|h| h.qualname).collect::<Vec<_>>();
    assert_eq!(callers("Outer.len"), ["use_outer"]);
    assert_eq!(callees("Outer.len"), ["Inner.len"]);
    assert_eq!(callers("Outer.close"), ["shutdown"]);
    assert_eq!(callees("Outer.close"), ["Inner.close"]);
}

#[test]
fn a_method_of_an_object_literal_keeps_its_callers_and_callees() {
    let ts = "export const store = {\n  count: 0,\n  increment() {\n    this.count += 1;\n    return this.total();\n  },\n  total() {\n    return this.count;\n  },\n};\n\nexport function bump() {\n  return store.increment();\n}\n";
    let js = "export default {\n  methods: {\n    save() {\n      return 1;\n    },\n    run() {\n      return this.save();\n    },\n  },\n};\n";
    let r = Repo::new("literal", &[("vue.ts", ts), ("api.js", js)]);
    let lib = r.lib();
    let callers = |n: &str| lib.callers(n, 50).unwrap().into_iter().map(|h| h.name).collect::<Vec<_>>();
    let callees = |n: &str| lib.callees(n, 50).unwrap().0.into_iter().map(|h| h.name).collect::<Vec<_>>();
    assert_eq!(callers("increment"), ["bump"]);
    assert_eq!(callers("total"), ["increment"]);
    assert_eq!(callers("save"), ["run"]);
    assert_eq!(callees("bump"), ["increment"]);
    assert_eq!(callees("run"), ["save"]);
}

#[test]
fn callers_follow_the_type_a_call_returns() {
    let report = "pub struct Report;\n\nimpl Report {\n    pub fn load() -> Report {\n        Report\n    }\n    pub fn ok(&self) -> bool {\n        true\n    }\n}\n\npub struct Builder;\n\nimpl Builder {\n    pub fn summary(&self) -> Report {\n        Report\n    }\n}\n\npub fn make() -> Builder {\n    Builder\n}\n\npub fn mystery(y: i32) -> Option<i32> {\n    Some(y)\n}\n\npub struct Other;\n\nimpl Other {\n    pub fn ok(&self) -> bool {\n        false\n    }\n}\n";
    let user = "use crate::report::{make, mystery, Report};\n\npub fn a(y: i32) -> bool {\n    mystery(y).ok().is_some()\n}\n\npub fn b() -> bool {\n    Report::load().ok()\n}\n\npub fn c() -> bool {\n    make().summary().ok()\n}\n\npub fn d(file: &std::fs::File) -> bool {\n    file.metadata().ok().is_some()\n}\n";
    let r = Repo::new("returns", &[("src/lib.rs", "mod report;\nmod user;\n"), ("src/report.rs", report), ("src/user.rs", user)]);
    let (code, out) = r.run(&["callers", "Report.ok"]);
    assert_eq!(code, 0, "{out}");
    let names: Vec<&str> = out.lines().filter_map(|l| l.split("] ").nth(1)).collect();
    assert_eq!(names, ["b", "c", "d"], "{out}");
    assert!(out.contains("(1 of these call a definition of that name"), "{out}");
    let (_, other) = r.run(&["callers", "Other.ok"]);
    let names: Vec<&str> = other.lines().filter_map(|l| l.split("] ").nth(1)).collect();
    assert_eq!(names, ["d"], "{other}");
}

#[test]
fn this_in_an_object_literal_method_calls_the_literal_s_method() {
    let js = "export const store = {\n  increment() {\n    return this.total();\n  },\n  total() {\n    return 1;\n  },\n};\n\nexport class Cart {\n  total() {\n    return 2;\n  }\n}\n";
    let r = Repo::new("literal_this", &[("store.js", js)]);
    let lib = r.lib();
    let callers = |n: &str| lib.callers(n, 50).unwrap().into_iter().map(|h| h.qualname).collect::<Vec<_>>();
    assert_eq!(callers("store.total"), ["store.increment"]);
    assert!(callers("Cart.total").is_empty());
    assert_eq!(lib.callees("store.increment", 50).unwrap().0.into_iter().map(|h| h.qualname).collect::<Vec<_>>(), ["store.total"]);
}

#[test]
fn refs_reads_fields_and_methods_inside_rust_macros() {
    let src = "pub fn text() -> u8 {\n    1\n}\n\npub struct Hit {\n    pub text: u8,\n}\n\npub fn run(h: &Hit) -> Vec<Hit> {\n    let s = format!(\"{}\", h.text);\n    assert_eq!(s.len(), 1);\n    println!(\"{}\", text());\n    vec![Hit { text: 1 }]\n}\n";
    let r = Repo::new("macro_refs", &[("src/lib.rs", src)]);
    assert_eq!(
        r.resolved("text"),
        pairs(&[("src/lib.rs:1", "src/lib.rs"), ("src/lib.rs:10", "-"), ("src/lib.rs:12", "src/lib.rs"), ("src/lib.rs:13", "-"), ("src/lib.rs:6", "-")])
    );
    let lib = r.lib();
    assert_eq!(lib.callers("text", 50).unwrap().into_iter().map(|h| h.qualname).collect::<Vec<_>>(), ["run"]);
}

#[test]
fn a_definition_is_listed_before_its_prototype() {
    let h = "#pragma once\nnamespace geo {\nstruct Shape { int w; };\nShape make();\n}\n";
    let cpp = "#include \"a_shape.h\"\nnamespace geo {\nShape make() {\n  Shape s;\n  s.w = 1;\n  return s;\n}\n}\n";
    let user = "#include \"a_shape.h\"\nint use() {\n  return geo::make().w;\n}\n";
    let r = Repo::new("proto_order", &[("a_shape.h", h), ("b_shape.cpp", cpp), ("c_use.cpp", user)]);
    let lib = r.lib();
    let places = |n: &str| lib.find_symbol(n, 10).unwrap().into_iter().map(|h| h.path).collect::<Vec<_>>();
    assert_eq!(places("make"), ["b_shape.cpp", "a_shape.h"]);
    assert_eq!(places("geo::make"), ["b_shape.cpp", "a_shape.h"]);
    assert_eq!(lib.callers("make", 10).unwrap().into_iter().map(|h| h.name).collect::<Vec<_>>(), ["use"]);
}

#[test]
fn a_recursive_function_is_its_own_caller_and_callee() {
    let rs = "pub fn fact(n: u64) -> u64 {\n    if n == 0 {\n        1\n    } else {\n        n * fact(n - 1)\n    }\n}\n\npub fn main_entry() -> u64 {\n    fact(5)\n}\n";
    let py = "def walk(n):\n    if n:\n        return walk(n - 1)\n    return 0\n";
    let r = Repo::new("recursion", &[("src/lib.rs", rs), ("rec.py", py)]);
    let lib = r.lib();
    let mut callers: Vec<String> = lib.callers("fact", 50).unwrap().into_iter().map(|h| h.qualname).collect();
    callers.sort();
    assert_eq!(callers, ["fact", "main_entry"]);
    assert_eq!(lib.callees("fact", 50).unwrap().0.into_iter().map(|h| h.qualname).collect::<Vec<_>>(), ["fact"]);
    assert_eq!(lib.callers("walk", 50).unwrap().into_iter().map(|h| h.qualname).collect::<Vec<_>>(), ["walk"]);
    assert_eq!(lib.callees("walk", 50).unwrap().0.into_iter().map(|h| h.qualname).collect::<Vec<_>>(), ["walk"]);
}

#[test]
fn search_and_tsquery_page_by_max_bytes_and_offset() {
    let src: String = (0..12).map(|i| format!("def parse_header_{i}(value):\n    \"\"\"Parse header value {i}.\"\"\"\n    return value\n\n\n")).collect();
    let r = Repo::new("paging", &[("h.py", &src), ("cr.py", "S = 'a\rb'\n")]);
    let (_, all) = r.run(&["search", "parse header value", "-k", "10"]);
    let (code, cut) = r.run(&["--max-bytes", "400", "search", "parse header value", "-k", "10"]);
    assert_eq!(code, 0, "{cut}");
    assert!(cut.len() <= 401 && cut.len() < all.len() && cut.contains("call again with offset="), "{cut}");
    let (_, rest) = r.run(&["--offset", "9", "search", "parse header value", "-k", "10"]);
    assert_eq!(rest.lines().filter(|l| l.starts_with("h.py:")).count(), 1, "{rest}");

    let q = "(function_definition) @f";
    let (_, whole) = r.run(&["tsquery", "python", q, "."]);
    assert_eq!(whole.lines().count(), 12, "one line per match: {whole}");
    assert!(whole.lines().all(|l| l.ends_with("(value):")), "{whole}");
    let (_, cr) = r.run(&["tsquery", "python", "(string) @s", "cr.py"]);
    assert_eq!(cr.lines().count(), 1, "a lone carriage return is not a line break: {cr:?}");
    assert!(cr.contains("a\\rb"), "{cr:?}");
    let (_, cut) = r.run(&["--max-bytes", "300", "tsquery", "python", q, "."]);
    assert!(cut.len() <= 301 && cut.contains("call again with offset="), "{cut}");
}

#[test]
fn check_counts_files_and_batch_reports_a_bad_line_and_goes_on() {
    let r = Repo::new("check_batch", &[("a.py", "def alpha():\n    return 1\n"), ("b.py", "def beta():\n    return alpha()\n")]);
    assert_eq!(r.json(&["check", "--json"])["files_indexed"], 2);
    let file = r.root.join("calls.jsonl");
    fs::write(&file, "{\"op\": \"find_symbol\", \"name\": \"alpha\"}\n\n{nope\n{\"op\": \"callers\", \"name\": \"alpha\"}\n").unwrap();
    let (code, out, err) = r.exec(&["batch", file.to_str().unwrap()]);
    assert_eq!(code, 0, "{out}{err}");
    let recs: Vec<Value> = out.lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    assert_eq!(recs.len(), 3, "{out}");
    assert!(recs[1]["error"].as_str().unwrap().starts_with("line 3, column 2: not JSON"), "{out}");
    assert_eq!(recs[2]["result"][0]["name"], "beta", "{out}");
}

#[test]
fn a_definition_whose_own_name_has_a_dot_is_found_by_that_name() {
    let r = Repo::new(
        "sym_own_dot",
        &[
            ("stats.R", "read.table2 <- function(f) {\n  f\n}\n\nprint.myclass <- function(x) {\n  x\n}\n"),
            ("conf.toml", "[server.http]\nport = 1\n"),
            ("mod.lua", "local M = {}\n\nfunction M.helper(x)\n  return x\nend\n\nreturn M\n"),
            ("mix.ex", "defmodule MyApp.Accounts.User do\n  def changeset(user), do: user\nend\n"),
        ],
    );
    let lib = r.lib();
    for (name, want) in [
        ("read.table2", "stats.R:1"),
        ("print.myclass", "stats.R:5"),
        ("server.http", "conf.toml:1"),
        ("M.helper", "mod.lua:3"),
        ("MyApp.Accounts.User", "mix.ex:1"),
        ("MyApp.Accounts.User.changeset", "mix.ex:2"),
        ("User.changeset", "mix.ex:2"),
    ] {
        assert_eq!(symbol_at(&lib, name), [want], "{name}");
    }
}

#[test]
fn an_owner_is_matched_without_its_generics_or_its_path() {
    let r = Repo::new(
        "sym_owner",
        &[
            (
                "src/lib.rs",
                "pub struct Stack<T> {\n    v: Vec<T>,\n}\n\nimpl<T> Stack<T> {\n    pub fn push(&mut self, x: T) {\n        self.v.push(x);\n    }\n}\n\npub struct Thing;\n\nimpl crate::Thing {\n    pub fn run(&self) {}\n}\n\npub fn helper() {}\n",
            ),
            (
                "src/config.rs",
                "pub mod config {\n    pub struct Config;\n    impl Config {\n        pub fn new() -> Config {\n            Config\n        }\n    }\n}\n",
            ),
            ("box.cpp", "template <typename T> class Box {\n  void put(T x);\n};\n\ntemplate <typename T> void Box<T>::put(T x) {\n  (void)x;\n}\n"),
            ("User.php", "<?php\nnamespace App\\Models;\n\nclass User {\n    public static function find($id) {\n        return $id;\n    }\n}\n"),
        ],
    );
    let lib = r.lib();
    for (name, want) in [
        ("Stack.push", &["src/lib.rs:6"][..]),
        ("Stack::push", &["src/lib.rs:6"]),
        ("Thing.run", &["src/lib.rs:14"]),
        ("Box.put", &["box.cpp:2", "box.cpp:5"]),
        ("crate::config::Config::new", &["src/config.rs:4"]),
        ("App\\Models\\User::find", &["User.php:5"]),
        ("crate::util::helper", &["src/lib.rs:17"]),
        ("Other.push", &[]),
        ("Thing.helper", &[]),
        ("Other::new", &[]),
    ] {
        assert_eq!(symbol_at(&lib, name), want, "{name}");
    }
}

/// Every name `outline` prints for a sample is found by `find_symbol`, by its qualified name and
/// by its bare name, at the same place (each sample in an index of its own, as a user has it).
#[test]
fn every_outlined_sample_name_is_found_by_symbol() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
    let mut names: Vec<String> = fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    names.retain(|n| n != "expected.json" && !n.starts_with('.'));
    names.sort();
    let (mut misses, mut total) = (Vec::new(), 0);
    for name in names {
        let root = scratch("outline_sample");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::copy(dir.join(&name), root.join(&name)).unwrap();
        let ix = scrutate::index::Index::open(&fs::canonicalize(&root).unwrap(), scrutate::models::ModelSet::none()).unwrap();
        let mut s = scrutate::search::Searcher::new(ix);
        s.ix.update(false).unwrap();
        for row in s.ix.outline(&name).unwrap() {
            total += 1;
            for q in [&row.qualname, &row.name] {
                if !s.find_symbol(q, 50).unwrap().iter().any(|h| h.id == row.id) {
                    misses.push(format!("{name}: [{}] {q}", row.kind));
                }
            }
        }
        drop(s);
        let _ = fs::remove_dir_all(&root);
    }
    assert!(total > 300, "{total} outline entries");
    assert!(misses.is_empty(), "{} of {total} not found:\n{}", misses.len(), misses.join("\n"));
}

#[test]
fn refs_callers_and_callees_take_the_names_symbol_takes() {
    let r = Repo::new(
        "refs_typed",
        &[
            ("conv.scm", "(define (list->vec l)\n  (list->vector l))\n\n(define (use-it)\n  (list->vec (list 1 2)))\n"),
            ("mod.rb", "class Foo::Bar\n  def self.call(x)\n    x\n  end\nend\n\ndef run\n  Foo::Bar.call(1)\nend\n"),
            ("cart.js", "class Cart {\n  #secret() {\n    return 1;\n  }\n  open() {\n    return this.#secret();\n  }\n}\n"),
        ],
    );
    let secret = pairs(&[("cart.js:2", "cart.js"), ("cart.js:6", "cart.js")]);
    assert_eq!(r.resolved("#secret"), secret);
    assert_eq!(r.resolved("Cart.#secret"), secret);
    for (name, def, cols) in [("list->vec", "conv.scm", [10, 4]), ("Foo::Bar.call", "mod.rb", [12, 12])] {
        let v = r.json(&["refs", name, "--json"]);
        let defs: Vec<&str> = v["definitions"].as_array().unwrap().iter().map(|d| d["path"].as_str().unwrap()).collect();
        assert_eq!(defs, [def], "{name}: {v}");
        let at: Vec<u64> = v["unresolved"].as_array().unwrap().iter().map(|u| u["column"].as_u64().unwrap()).collect();
        assert_eq!(at, cols, "{name}: {v}");
        let t = r.json(&["refs", name, "--textual", "--json"]);
        assert_eq!(t["definitions"].as_array().unwrap().len(), 1, "{name}: {t}");
    }
    let (_, out) = r.run(&["callers", "Foo::Bar.call"]);
    assert!(out.contains("mod.rb:7-9  [function] run"), "{out}");
    let (_, out) = r.run(&["callees", "run"]);
    assert!(out.contains("mod.rb:2-4  [method] Foo::Bar.call"), "{out}");
}

#[test]
fn a_window_location_works_where_lines_are_enough_and_is_named_where_a_symbol_is_needed() {
    let app = |name: &str, port: u32| {
        format!(
            "const express = require(\"express\");\nconst {name} = express();\n\nfunction handler(req, res) {{\n  res.send(\"ok\");\n}}\n\n\
             {name}.get(\"/\", handler);\n{name}.listen({port}, () => {{\n  console.log(\"listening on the port\");\n}});\n\nmodule.exports = {name};\n"
        )
    };
    let (a, b) = (app("app", 3000), app("api", 4000));
    let r = Repo::new("window_loc", &[("app.js", a.as_str()), ("api.js", b.as_str())]);
    let (_, found) = r.run(&["search", "-k", "1", "listening on the port"]);
    let loc = found.split_whitespace().next().unwrap().to_string();
    assert!(found.contains("[window]"), "{found}");
    // the window has vectors: the other file's window is like it
    let (code, out) = r.run(&["similar", &loc]);
    assert_eq!(code, 0, "{out}");
    // a tool that needs a symbol says what the location is
    for cmd in ["symbol", "callers"] {
        let (code, out) = r.run(&[cmd, &loc]);
        assert_ne!(code, 0, "{cmd}: {out}");
        assert!(out.contains("outside every definition"), "{cmd}: {out}");
    }
}

#[test]
fn similar_on_a_constant_says_constants_are_not_compared() {
    let r = Repo::new("similar_const", &[("k.py", "MAX_SIZE = 10\n\n\ndef grow(x):\n    return x + MAX_SIZE * 2 + len(str(x))\n")]);
    let (code, out) = r.run(&["similar", "MAX_SIZE"]);
    let (missing, _) = r.run(&["similar", "no_such_name"]);
    assert_eq!(code, missing, "{out}");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("MAX_SIZE is a constant") && out.contains("not compared") && !out.contains("no vectors"), "{out}");
    assert_eq!(out.trim().lines().count(), 1, "{out}");
}

#[test]
fn refs_finds_a_name_in_its_composed_and_decomposed_spelling() {
    let (nfc, nfd) = ("caf\u{e9}_total", "cafe\u{301}_total");
    let r = Repo::new(
        "refs_nfd",
        &[
            ("nfc.py", &format!("def {nfc}():\n    return 1\n\n\ndef caller_nfc():\n    return {nfc}()\n")),
            ("nfd.py", &format!("def caller_nfd():\n    return {nfd}() + {nfd}_two()\n")),
            ("notes.txt", &format!("{nfd}\n")),
        ],
    );
    for typed in [nfc, nfd] {
        let got = r.resolved(typed);
        assert_eq!(got, pairs(&[("nfc.py:1", "nfc.py"), ("nfc.py:6", "nfc.py"), ("nfd.py:2", "-"), ("notes.txt:1", "-")]), "{typed:?}");
        let t = r.json(&["refs", typed, "--textual", "--json"]);
        let lines: Vec<String> = t["references"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|g| g["lines"].as_array().unwrap().iter().map(move |l| format!("{}:{}", g["path"].as_str().unwrap(), l["line"])))
            .collect();
        assert_eq!(lines, ["nfc.py:1", "nfc.py:6", "nfd.py:2", "notes.txt:1"], "{typed:?}: {t}");
    }
}

/// What `callers SPEC` prints: the decided names and the undecided ones after them, each sorted.
fn split_callers(r: &Repo, spec: &str) -> (Vec<String>, Vec<String>) {
    let (code, out) = r.run(&["callers", spec]);
    assert_eq!(code, 0, "{out}");
    let names: Vec<String> = out.lines().filter_map(|l| l.split("] ").nth(1)).map(str::to_string).collect();
    let undecided = if out.contains("could not tell whether any") {
        names.len()
    } else {
        out.split_once(" of these call").and_then(|(a, _)| a.rsplit('(').next()?.parse().ok()).unwrap_or(0)
    };
    let (mut decided, mut unsure) = (names[..names.len() - undecided].to_vec(), names[names.len() - undecided..].to_vec());
    decided.sort();
    unsure.sort();
    (decided, unsure)
}

/// The occurrences `refs NAME --def DEF` ties to that definition, as sorted `path:line`.
fn ref_lines(r: &Repo, name: &str, def: &str) -> Vec<String> {
    let v = r.json(&["refs", name, "--def", def, "--json"]);
    let refs = v["definitions"][0]["references"].as_array().unwrap_or_else(|| panic!("{v}"));
    let mut out: Vec<String> = refs.iter().map(|o| format!("{}:{}", o["path"].as_str().unwrap(), o["line"])).collect();
    out.sort();
    out
}

fn callee_names(r: &Repo, name: &str) -> Vec<String> {
    let v = r.json(&["callees", name, "--json"]);
    v["resolved"].as_array().unwrap().iter().map(|h| h["name"].as_str().unwrap().to_string()).collect()
}

fn strs(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

#[test]
fn a_call_through_a_base_class_is_an_undecided_caller_of_an_override() {
    let ts = "export class Base {\n  size(): number {\n    return 0;\n  }\n}\nexport class Mid extends Base {}\nexport class Big extends Mid {\n  size(): number {\n    return 9;\n  }\n}\nexport class Small extends Base {\n  size(): number {\n    return 1;\n  }\n}\nexport function viaBase(b: Base): number {\n  return b.size();\n}\nexport function viaMid(m: Mid): number {\n  return m.size();\n}\nexport function viaSub(b: Big): number {\n  return b.size();\n}\nexport function viaNew(): number {\n  const b: Base = new Big();\n  return b.size();\n}\nexport function viaSmall(s: Small): number {\n  return s.size();\n}\nexport function viaAgain(): number {\n  let b: Base = new Big();\n  b = new Small();\n  return b.size();\n}\n";
    let r = Repo::new("override_ts", &[("shapes.ts", ts)]);
    assert_eq!(split_callers(&r, "shapes.ts:8"), (strs(&["viaNew", "viaSub"]), strs(&["viaAgain", "viaBase", "viaMid"])));
    assert_eq!(split_callers(&r, "shapes.ts:13"), (strs(&["viaSmall"]), strs(&["viaAgain", "viaBase", "viaMid"])));
    assert_eq!(split_callers(&r, "shapes.ts:2"), (strs(&["viaAgain", "viaBase", "viaMid"]), strs(&[])));
    assert_eq!(callee_names(&r, "viaBase"), ["Base.size"]);
    let built = callee_names(&r, "viaNew");
    assert!(built.contains(&"Big.size".to_string()) && !built.contains(&"Base.size".to_string()), "{built:?}");
    assert_eq!(ref_lines(&r, "size", "shapes.ts:8"), ["shapes.ts:24", "shapes.ts:28", "shapes.ts:8"]);
    assert_eq!(ref_lines(&r, "size", "shapes.ts:2"), ["shapes.ts:18", "shapes.ts:2", "shapes.ts:21", "shapes.ts:36"]);

    let js = "export class Base {\n  size() {\n    return 0;\n  }\n}\nexport class Mid extends Base {}\nexport class Big extends Mid {\n  size() {\n    return 9;\n  }\n}\nexport function viaMid() {\n  return new Mid().size();\n}\nexport function viaSub() {\n  return new Big().size();\n}\n";
    let r = Repo::new("override_js", &[("shapes.js", js)]);
    assert_eq!(split_callers(&r, "shapes.js:8"), (strs(&["viaSub"]), strs(&["viaMid"])));
    assert_eq!(split_callers(&r, "shapes.js:2"), (strs(&["viaMid"]), strs(&[])));

    let py = "class Base:\n    def size(self):\n        return 0\n\n\nclass Mid(Base):\n    pass\n\n\nclass Big(Mid):\n    def size(self):\n        return 9\n\n\nclass Small(Base):\n    def size(self):\n        return 1\n\n\ndef via_base(b: Base):\n    return b.size()\n\n\ndef via_mid(m: Mid):\n    return m.size()\n\n\ndef via_sub(b: Big):\n    return b.size()\n\n\ndef via_new():\n    b: Base = Big()\n    return b.size()\n\n\ndef via_small(s: Small):\n    return s.size()\n\n\ndef via_again():\n    b: Base = Big()\n    b = Small()\n    return b.size()\n";
    let r = Repo::new("override_py", &[("zoo.py", py)]);
    assert_eq!(split_callers(&r, "zoo.py:11"), (strs(&["via_new", "via_sub"]), strs(&["via_base", "via_mid"])));
    // a plain assignment binds the name again in Python: `b = Small()` types the call
    assert_eq!(split_callers(&r, "zoo.py:16"), (strs(&["via_again", "via_small"]), strs(&["via_base", "via_mid"])));
    assert_eq!(split_callers(&r, "zoo.py:2"), (strs(&["via_base", "via_mid"]), strs(&[])));
    assert_eq!(callee_names(&r, "via_base"), ["Base.size"]);
    assert_eq!(ref_lines(&r, "size", "zoo.py:11"), ["zoo.py:11", "zoo.py:29", "zoo.py:34"]);
}

#[test]
fn a_call_through_an_interface_or_a_trait_is_an_undecided_caller_of_an_implementation() {
    let go = "package gp\n\ntype Sizer interface{ Size() int }\n\ntype Big struct{}\n\nfunc (b Big) Size() int { return 9 }\n\ntype Small struct{}\n\nfunc (s Small) Size() int { return 1 }\n\nfunc viaIface(s Sizer) int { return s.Size() }\n\nfunc viaSub(b Big) int { return b.Size() }\n\nfunc viaNew() int {\n\tvar s Sizer = Big{}\n\treturn s.Size()\n}\n\nfunc viaPtr() int {\n\tvar s Sizer = &Big{}\n\treturn s.Size()\n}\n\nfunc viaSmall(s Small) int { return s.Size() }\n\nfunc viaAgain() int {\n\tvar s Sizer = Big{}\n\ts = Small{}\n\treturn s.Size()\n}\n";
    let r = Repo::new("override_go", &[("gp/i.go", go)]);
    assert_eq!(split_callers(&r, "gp/i.go:7"), (strs(&["viaNew", "viaPtr", "viaSub"]), strs(&["viaAgain", "viaIface"])));
    assert_eq!(split_callers(&r, "gp/i.go:11"), (strs(&["viaSmall"]), strs(&["viaAgain", "viaIface"])));
    assert_eq!(ref_lines(&r, "Size", "gp/i.go:7"), ["gp/i.go:15", "gp/i.go:19", "gp/i.go:24", "gp/i.go:7"]);

    let rs = "pub trait Sizer {\n    fn size(&self) -> u8;\n}\n\npub struct Big;\n\nimpl Sizer for Big {\n    fn size(&self) -> u8 {\n        9\n    }\n}\n\npub struct Small;\n\nimpl Sizer for Small {\n    fn size(&self) -> u8 {\n        1\n    }\n}\n\npub fn via_dyn(s: &dyn Sizer) -> u8 {\n    s.size()\n}\n\npub fn via_path() -> u8 {\n    Sizer::size(&Small)\n}\n\npub fn via_sub(b: &Big) -> u8 {\n    b.size()\n}\n\npub fn via_new() -> u8 {\n    let s: Box<dyn Sizer> = Box::new(Big);\n    s.size()\n}\n\npub fn via_ref() -> u8 {\n    let s: &dyn Sizer = &Big;\n    s.size()\n}\n\npub fn via_small(s: &Small) -> u8 {\n    s.size()\n}\n\npub fn via_again() -> u8 {\n    let mut s: Box<dyn Sizer> = Box::new(Big);\n    s = Box::new(Small);\n    s.size()\n}\n";
    let r = Repo::new("override_rs", &[("src/lib.rs", rs)]);
    assert_eq!(split_callers(&r, "src/lib.rs:8"), (strs(&["via_new", "via_ref", "via_sub"]), strs(&["via_again", "via_dyn", "via_path"])));
    assert_eq!(split_callers(&r, "src/lib.rs:16"), (strs(&["via_small"]), strs(&["via_again", "via_dyn", "via_path"])));
    assert_eq!(callee_names(&r, "via_path"), ["Sizer.size"]);
    assert_eq!(ref_lines(&r, "size", "src/lib.rs:8"), ["src/lib.rs:30", "src/lib.rs:35", "src/lib.rs:40", "src/lib.rs:8"]);
}

#[test]
fn functions_assigned_to_names_and_members_are_symbols_callers_and_callees_find() {
    let lua = "local M = {}\n\nlocal helper = function(x)\n  return x\nend\n\nM.assigned = function(x)\n  return helper(x)\nend\n\nlocal obj = {\n  inline = function(x)\n    return x\n  end,\n}\n\nfunction M.use()\n  M.assigned(3)\n  return obj.inline(2)\nend\n\nreturn M\n";
    let js = "function Foo() {}\n\nFoo.prototype.bar = function () {\n  return 1;\n};\n\nFoo.staticFn = function (x) {\n  return x;\n};\n\nFoo.prototype.arrow = (x) => x;\n\nfunction useFoo() {\n  const f = new Foo();\n  return f.bar() + Foo.staticFn(1) + f.arrow(2);\n}\n";
    let r = Repo::new("assigned_fns", &[("a.lua", lua), ("a.js", js)]);
    for (name, at) in [
        ("helper", "a.lua:3"),
        ("M.assigned", "a.lua:7"),
        ("inline", "a.lua:12"),
        ("bar", "a.js:3"),
        ("Foo.bar", "a.js:3"),
        ("Foo.staticFn", "a.js:7"),
        ("arrow", "a.js:11"),
    ] {
        let (code, out) = r.run(&["symbol", name]);
        assert!(code == 0 && out.lines().any(|l| l.starts_with(&format!("{at}-")) || l.starts_with(&format!("{at} "))), "symbol {name}: {out}");
    }
    let mut got = callee_names(&r, "M.use");
    got.sort();
    assert_eq!(got, ["M.assigned", "obj.inline"]);
    assert_eq!(callee_names(&r, "M.assigned"), ["helper"]);
    let mut got = callee_names(&r, "useFoo");
    got.sort();
    assert_eq!(got, ["Foo", "Foo.arrow", "Foo.bar", "Foo.staticFn"]);
    for (def, caller) in [("a.lua:7", "M.use"), ("a.lua:3", "M.assigned"), ("a.js:3", "useFoo"), ("a.js:7", "useFoo")] {
        let (code, out) = r.run(&["callers", def]);
        assert!(code == 0 && out.lines().any(|l| l.ends_with(caller)), "callers {def}: {out}");
    }
}

#[test]
fn a_namespace_import_is_not_the_default_export_of_its_module() {
    let app = "export default function () {\n  return 1;\n}\n\nexport function side() {\n  return 2;\n}\n";
    let main = "import * as ns from \"./app\";\n\nexport function start(run) {\n  return run(ns) + ns.side();\n}\n";
    let r = Repo::new("ns_default", &[("lib/app.js", app), ("lib/main.js", main)]);
    // `ns` is the module's namespace object, not its default export
    assert_eq!(callee_names(&r, "start"), ["side"]);
    let (_, out) = r.run(&["refs", "ns"]);
    assert!(!out.contains("lib/app.js:1"), "{out}");
}

#[test]
fn a_function_set_on_this_and_a_default_export_are_symbols_callers_and_callees_find() {
    let widget = "function Widget() {\n  this.render = function () {\n    return paint();\n  };\n}\n\nfunction paint() {\n  return 1;\n}\n\nfunction useWidget() {\n  const w = new Widget();\n  return w.render();\n}\n";
    let app = "module.exports = function () {\n  return helper();\n};\n\nfunction helper() {\n  return 2;\n}\n";
    let main = "const app = require(\"./app\");\n\nfunction start() {\n  return app();\n}\n";
    let r = Repo::new("this_and_default", &[("lib/widget.js", widget), ("lib/app.js", app), ("lib/main.js", main)]);
    for (name, at) in [("Widget.render", "lib/widget.js:2"), ("render", "lib/widget.js:2"), ("app", "lib/app.js:1")] {
        let (code, out) = r.run(&["symbol", name]);
        assert!(code == 0 && out.lines().any(|l| l.starts_with(&format!("{at}-")) || l.starts_with(&format!("{at} "))), "symbol {name}: {out}");
    }
    let (code, out) = r.run(&["outline", "lib/app.js"]);
    assert!(code == 0 && out.lines().any(|l| l.contains("function") && l.trim_end().ends_with(" app")), "outline: {out}");
    assert!(callee_names(&r, "useWidget").contains(&"Widget.render".to_string()));
    assert_eq!(callee_names(&r, "Widget.render"), ["paint"]);
    assert_eq!(callee_names(&r, "start"), ["app"]);
    assert_eq!(callee_names(&r, "app"), ["helper"]);
    for (def, caller) in [("lib/widget.js:2", "useWidget"), ("lib/app.js:1", "start")] {
        let (code, out) = r.run(&["callers", def]);
        assert!(code == 0 && out.lines().any(|l| l.ends_with(caller)), "callers {def}: {out}");
    }
    let (code, out) = r.run(&["search", "paint", "-k", "20"]);
    assert!(code == 0 && out.contains("Widget.render"), "search: {out}");
}

#[test]
fn a_super_call_is_a_decided_call_of_the_base_method_and_no_call_of_the_override() {
    let ts = "export class Base {\n  size(): number {\n    return 0;\n  }\n}\nexport class Big extends Base {\n  size(): number {\n    return super.size() + 9;\n  }\n  other(): number {\n    return this.size();\n  }\n}\nexport class Bigger extends Big {\n  size(): number {\n    return super.size() + 1;\n  }\n}\nexport function viaBase(b: Base): number {\n  return b.size();\n}\n";
    let far = "import { Outer } from 'outside';\nexport class Far extends Outer {\n  size(): number {\n    return super.size() + 3;\n  }\n}\n";
    let js = ts.replace("(): number", "()").replace("(b: Base): number {\n  return b.size()", "() {\n  return new Base().size()");
    let py = "class Base:\n    def size(self):\n        return 0\n\n\nclass Big(Base):\n    def size(self):\n        return super().size() + 9\n\n    def other(self):\n        return self.size()\n\n\nclass Bigger(Big):\n    def size(self):\n        return super(Bigger, self).size() + 1\n\n\ndef via_base(b: Base):\n    return b.size()\n";
    let py_far = "from outside import Thing\n\n\nclass Far(Thing):\n    def size(self):\n        return super().size() + 3\n";
    let cases = [
        ("ts", ts.to_string(), far.to_string(), "viaBase", 3),
        ("js", js, far.replace("(): number", "()"), "viaBase", 3),
        ("py", py.to_string(), py_far.to_string(), "via_base", 5),
    ];
    for (ext, main, far, via, far_size) in cases {
        let (shapes, outer) = (format!("shapes.{ext}"), format!("far.{ext}"));
        let r = Repo::new(&format!("super_{ext}"), &[(&shapes, &main), (&outer, &far)]);
        let at = |line: u32| format!("{shapes}:{line}");
        // the override is no caller of itself; the base lists it as decided
        assert_eq!(split_callers(&r, &at(7)), (strs(&["Big.other", "Bigger.size"]), strs(&[via])), "{ext}");
        assert_eq!(split_callers(&r, &at(2)), (strs(&["Big.size", via]), strs(&[])), "{ext}");
        assert_eq!(split_callers(&r, &at(15)), (strs(&[]), strs(&["Big.other", via])), "{ext}");
        // a base from outside the repository: the super call is no call of the repository
        let (code, out) = r.run(&["callers", &format!("{outer}:{far_size}")]);
        assert!(code == 1 && out.contains("no results"), "{ext}: {out}");
        assert_eq!(callee_names(&r, "Big.size"), ["Base.size"], "{ext}");
        assert_eq!(callee_names(&r, "Bigger.size"), ["Big.size"], "{ext}");
        assert_eq!(ref_lines(&r, "size", &at(2)), [at(2), at(20), at(8)], "{ext}");
        assert_eq!(ref_lines(&r, "size", &at(7)), [at(11), at(16), at(7)], "{ext}");
    }
}
