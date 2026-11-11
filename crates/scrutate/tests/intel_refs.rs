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
        let ix = scrutate::index::Index::open(&fs::canonicalize(&root).unwrap(), None).unwrap();
        let mut s = scrutate::search::Searcher::new(ix, None);
        s.ix.update(false).unwrap();
        for row in scrutate::intel::outline(&s.ix, &name).unwrap() {
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
fn similar_on_a_constant_says_constants_are_not_compared() {
    let r = Repo::new("similar_const", &[("k.py", "MAX_SIZE = 10\n\n\ndef grow(x):\n    return x + MAX_SIZE * 2 + len(str(x))\n")]);
    let (code, out) = r.run(&["similar", "MAX_SIZE"]);
    let (missing, _) = r.run(&["similar", "no_such_name"]);
    assert_eq!(code, missing, "{out}");
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("MAX_SIZE is a constant") && out.contains("not compared") && !out.contains("no vectors"), "{out}");
    assert_eq!(out.trim().lines().count(), 1, "{out}");
}
