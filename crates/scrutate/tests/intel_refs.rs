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
