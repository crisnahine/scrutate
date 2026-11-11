//! What the `serve` tools answer, called the way the server calls them: arguments as JSON, the
//! text an agent reads, and whether the call failed.

use std::fs;
use std::path::PathBuf;

use serde_json::{json, Value};

struct Repo {
    root: PathBuf,
    repo: scrutate::Repo,
}

impl Repo {
    fn new(name: &str, files: &[(&str, &str)]) -> Repo {
        static N: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!("scrutate_mcp_tools_{}_{}_{n}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        for (rel, body) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
        }
        let root = fs::canonicalize(&root).unwrap();
        let repo = scrutate::Repo::open(&root).unwrap();
        Repo { root, repo }
    }

    /// (failed, text)
    fn call(&mut self, tool: &str, args: Value) -> (bool, String) {
        let c = scrutate::mcp::call_tool(&mut self.repo, tool, &args, &scrutate::mcp::Settings::new(None, Some(0.0)));
        (c.is_error, c.text)
    }

    fn ok(&mut self, tool: &str, args: Value) -> String {
        let (failed, text) = self.call(tool, args.clone());
        assert!(!failed, "{tool} {args}: {text}");
        text
    }

    fn err(&mut self, tool: &str, args: Value) -> String {
        let (failed, text) = self.call(tool, args.clone());
        assert!(failed, "{tool} {args} should fail: {text}");
        text
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The location lines of an answer (each result starts with `path:start-end`).
fn heads(text: &str) -> Vec<&str> {
    text.lines().filter(|l| !l.starts_with(' ') && !l.starts_with('(')).collect()
}

fn two_dup_names() -> Repo {
    Repo::new(
        "dups",
        &[
            ("app.py", "import helper\n\ndef visible_fn():\n    return helper.dup_name()\n"),
            ("helper.py", "def dup_name():\n    return 'A_MARK'\n\ndef call_a():\n    return dup_name()\n"),
            ("other.py", "def dup_name():\n    return 'B_MARK'\n\ndef call_b():\n    return dup_name()\n"),
        ],
    )
}

#[test]
fn a_location_selects_that_one_definition() {
    let mut r = two_dup_names();
    let found = r.ok("find_symbol", json!({"name": "helper.py:1-2"}));
    assert_eq!(heads(&found), ["helper.py:1-2  [function] dup_name"], "{found}");
    assert_eq!(heads(&r.ok("find_symbol", json!({"name": "dup_name"}))).len(), 2);

    let callers = r.ok("callers", json!({"name": "helper.py:1-2"}));
    assert!(callers.contains("call_a") && callers.contains("visible_fn") && !callers.contains("call_b"), "{callers}");
    let callers = r.ok("callers", json!({"name": "./other.py:1-2"}));
    assert!(callers.contains("call_b") && !callers.contains("call_a") && !callers.contains("visible_fn"), "{callers}");
    let all = r.ok("callers", json!({"name": "dup_name"}));
    assert_eq!(heads(&all).len(), 3, "{all}");

    let callees = r.ok("callees", json!({"name": "helper.py:4-5"}));
    assert!(callees.contains("helper.py:1-2") && !callees.contains("other.py"), "{callees}");
    let callees = r.ok("callees", json!({"name": "other.py:4-5"}));
    assert!(callees.contains("other.py:1-2") && !callees.contains("helper.py"), "{callees}");
    assert!(r.err("callers", json!({"name": "helper.py:99"})).contains("no symbol at helper.py:99"));
}

#[test]
fn calls_do_not_cross_languages_or_unknown_qualifiers() {
    let mut r = Repo::new(
        "langs",
        &[
            ("check.py", "class CheckReport:\n    def ok(self):\n        return True\n\ndef py_caller(r):\n    return r.ok()\n"),
            (
                "src/lib.rs",
                "pub struct Index;\n\nimpl Index {\n    pub fn refresh(&self) {}\n}\n\npub fn rs_caller(ix: &Index) -> Option<u8> {\n    ix.refresh();\n    \"1\".parse().ok()\n}\n",
            ),
        ],
    );
    let callers = r.ok("callers", json!({"name": "ok"}));
    assert!(callers.contains("py_caller") && !callers.contains("rs_caller"), "{callers}");
    let callees = r.ok("callees", json!({"name": "rs_caller"}));
    assert!(callees.contains("refresh") && !callees.contains("CheckReport"), "{callees}");
    assert!(r.ok("callers", json!({"name": "Index.refresh"})).contains("rs_caller"));
    assert_eq!(r.ok("callers", json!({"name": "Nope.refresh"})), "(no results)");
    assert_eq!(r.ok("callees", json!({"name": "Nope.rs_caller"})), "(no results)");
}

#[test]
fn the_three_navigation_tools_match_names_alike() {
    let mut r = two_dup_names();
    let dotted_finds_nothing = r.ok("find_symbol", json!({"name": ".dup_name"})) == "(no results)";
    for (tool, name) in [("find_symbol", "dup_name"), ("callers", "dup_name"), ("callees", "call_a")] {
        let plain = r.ok(tool, json!({"name": name}));
        assert_ne!(plain, "(no results)", "{tool}");
        assert_eq!(r.ok(tool, json!({"name": name.to_uppercase()})), plain, "{tool}: case");
        assert_eq!(r.ok(tool, json!({"name": format!(".{name}")})) == "(no results)", dotted_finds_nothing, "{tool}: a leading dot");
    }
}

fn many_lines(n: usize) -> String {
    (1..=n).map(|i| format!("x_{i} = {i}\n")).collect()
}

#[test]
fn read_code_says_where_it_stopped() {
    let mut r = Repo::new("read", &[("many.py", &many_lines(9000)), ("empty.py", ""), ("three.py", "a = 1\nb = 2\nc = 3\n")]);
    let text = r.ok("read_code", json!({"path": "many.py", "max_bytes": 100_000}));
    let last = text.lines().last().unwrap();
    assert_eq!(text.lines().count(), 401, "{last}");
    assert!(last.contains("lines 1-400 of 9000") && last.contains("start=401"), "{last}");

    let text = r.ok("read_code", json!({"path": "many.py", "end": 9000, "max_bytes": 1_000_000}));
    assert!(text.lines().last().unwrap().starts_with(" 9000  x_9000"), "an explicit end is honoured");

    let text = r.ok("read_code", json!({"path": "many.py", "end": 9000, "max_bytes": 1000}));
    let last = text.lines().last().unwrap();
    assert!(text.len() <= 1000 && last.contains("of 9000") && last.contains("start="), "{last}");

    let text = r.ok("read_code", json!({"path": "many.py", "start": 2, "end": 3}));
    assert_eq!(text.lines().count(), 3, "{text}");
    assert!(text.lines().last().unwrap().contains("lines 2-3 of 9000"), "{text}");
    assert_eq!(r.ok("read_code", json!({"path": "three.py"})).lines().count(), 3, "the whole file needs no note");

    assert_eq!(r.ok("read_code", json!({"path": "empty.py"})), "empty.py is empty");
    assert_eq!(r.ok("read_code", json!({"path": "three.py", "start": 10})), "three.py has 3 lines: start=10 is past the end");
    assert!(r.err("read_code", json!({"path": "three.py", "start": 3, "end": 2})).contains("end (2) is before start (3)"));
}

#[test]
fn arguments_of_the_wrong_type_are_errors() {
    let mut r = two_dup_names();
    for (tool, args, name, ty) in [
        ("search_code", json!({"query": "q", "k": "5"}), "k", "integer"),
        ("read_code", json!({"path": "app.py", "start": "2"}), "start", "integer"),
        ("read_code", json!({"path": "app.py", "start": 9223372036854775808u64}), "start", "integer"),
        ("references", json!({"name": "dup_name", "textual": "yes"}), "textual", "boolean"),
        ("find_symbol", json!({"name": "dup_name", "max_bytes": "5000"}), "max_bytes", "integer"),
        ("find_symbol", json!({"name": "dup_name", "offset": -1}), "offset", "integer"),
        ("find_symbol", json!({"name": 7}), "name", "string"),
        ("duplicates", json!({"threshold": "high"}), "threshold", "number"),
    ] {
        let e = r.err(tool, args.clone());
        assert!(e.contains(name) && e.contains(ty), "{tool} {args}: {e}");
    }
    assert_ne!(r.ok("find_symbol", json!({"name": "dup_name", "limit": null})), "", "null is the same as leaving it out");
}

#[test]
fn every_tool_that_takes_a_path_accepts_the_spellings_read_code_does() {
    let mut r = Repo::new("paths", &[("pkg/a.py", "import pkg.b\n\ndef fa():\n    return 1\n"), ("pkg/b.py", "def fb():\n    return 2\n")]);
    for (tool, key, path) in [("outline", "path", "pkg/a.py"), ("dependencies", "path", "pkg/a.py"), ("dependents", "target", "pkg/b.py")] {
        let plain = r.ok(tool, json!({key: path}));
        for spelling in [format!("./{path}"), format!("pkg/../{path}"), format!("pkg/./{}", &path[4..])] {
            assert_eq!(r.ok(tool, json!({key: spelling})), plain, "{tool} {spelling}");
        }
    }
    assert!(r.ok("outline", json!({"path": "pkg/a.py"})).contains("fa"));
}

#[test]
fn a_cut_answer_fits_max_bytes_whole() {
    let files: Vec<(String, String)> = (0..300).map(|i| (format!("m{i}.py"), format!("def widget_{i}():\n    return 'widget {i}'\n"))).collect();
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let mut r = Repo::new("fits", &refs);
    for max in [1000, 1500, 4000] {
        for (tool, args) in
            [("grep_code", json!({"pattern": "widget"})), ("search_code", json!({"query": "widget", "k": 100})), ("references", json!({"name": "widget_1"}))]
        {
            let mut args = args;
            args["max_bytes"] = json!(max);
            let text = r.ok(tool, args);
            assert!(text.len() <= max, "{tool} max_bytes={max}: {} bytes", text.len());
        }
    }
}

#[test]
fn references_say_what_max_results_cut_and_when_nothing_matched() {
    let mut r = two_dup_names();
    let text = r.ok("references", json!({"name": "dup_name", "max_results": 1}));
    assert!(text.contains("max_results=1") && !text.contains("truncated"), "{text}");
    assert_eq!(r.ok("references", json!({"name": "nothing_has_this_name"})), "(no results)");
    assert!(r.err("references", json!({"name": ""})).contains("name is empty"));
}

#[test]
fn errors_echo_a_short_prefix_of_a_long_argument() {
    let mut r = two_dup_names();
    let long = "z".repeat(50_000);
    for (tool, args) in [
        ("similar_code", json!({"target": long})),
        ("read_code", json!({"path": long})),
        ("read_code", json!({"symbol": long})),
        ("callers", json!({"name": format!("{long}:3")})),
        ("structural_search", json!({"lang": long, "query": "(identifier) @x"})),
    ] {
        let (_, text) = r.call(tool, args);
        assert!(text.len() < 400 && text.contains("characters in all"), "{tool}: {} bytes", text.len());
    }
    for (tool, key) in [("outline", "path"), ("dependencies", "path"), ("dependents", "target")] {
        let (_, text) = r.call(tool, json!({key: long}));
        assert!(text.len() < 400, "{tool}: {} bytes", text.len());
    }
}

#[test]
fn control_characters_in_a_file_name_are_shown_escaped() {
    let mut r = Repo::new(
        "evil",
        &[("evil\nfake.py:1-2  [function] injected\n.py", "def evil_fn():\n    return 1\n"), ("esc\x1b[31m.py", "def esc_fn():\n    return 2\n")],
    );
    let text = r.ok("find_symbol", json!({"name": "evil_fn"}));
    assert_eq!(heads(&text), [r"evil\nfake.py:1-2  [function] injected\n.py:1-2  [function] evil_fn"], "{text}");
    let text = r.ok("grep_code", json!({"pattern": "return 2"}));
    assert!(text.contains(r"esc\x1b[31m.py") && !text.contains('\x1b'), "{text}");
}

#[test]
fn structural_search_says_what_is_wrong_with_a_query() {
    let mut r = two_dup_names();
    assert!(r.err("structural_search", json!({"lang": "python", "query": ""})).contains("empty"));
    assert!(r.err("structural_search", json!({"lang": "python", "query": "(identifier)"})).contains("capture"));
    let e = r.err("structural_search", json!({"lang": "python", "query": "((identifier) @x (#nope? @x \"a\"))"}));
    assert!(e.contains("#nope?"), "{e}");
    assert!(!r.ok("structural_search", json!({"lang": "python", "query": "((identifier) @x (#eq? @x \"dup_name\"))"})).contains("no matches"));
}

#[test]
fn grep_code_searches_the_hidden_files_the_index_holds_but_not_git_internals() {
    let mut r = Repo::new(
        "hidden",
        &[
            ("src/a.py", "def needle_fn():\n    return 1\n"),
            (".github/ci.yml", "name: needle_ci\n"),
            (".git/logs/HEAD", "needle commit\n"),
            ("node_modules/pkg/index.js", "function needle_dep() {}\n"),
        ],
    );
    let found = r.ok("grep_code", json!({"pattern": "needle"}));
    assert!(found.contains("src/a.py") && found.contains(".github/ci.yml"), "{found}");
    assert!(!found.contains(".git/") && !found.contains("node_modules"), "{found}");
    assert!(r.ok("search_code", json!({"query": "needle_ci"})).contains(".github/ci.yml"));
}

#[test]
fn callees_match_the_name_as_the_call_writes_it() {
    let src = "pub struct Report;\nimpl Report {\n    pub fn ok(&self) -> bool {\n        true\n    }\n}\npub fn returns_ok() -> Result<i32, ()> {\n    Ok(1)\n}\npub fn some(x: i32) -> i32 {\n    x\n}\npub fn makes_some() -> Option<i32> {\n    Some(1)\n}\npub fn asks(r: &Report) -> bool {\n    r.ok()\n}\n";
    let mut r = Repo::new("callee_case", &[("src/lib.rs", src)]);
    assert!(!r.ok("callees", json!({"name": "returns_ok"})).contains("Report.ok"));
    assert!(!r.ok("callees", json!({"name": "makes_some"})).contains("] some"));
    assert!(r.ok("callees", json!({"name": "asks"})).contains("Report.ok"));
}

#[test]
fn read_code_takes_the_smallest_start_as_line_one() {
    let mut r = Repo::new("read_min", &[("m.py", "a = 1\nb = 2\n")]);
    let text = r.ok("read_code", json!({"path": "m.py", "start": i64::MIN}));
    assert!(text.contains("a = 1") && !text.contains("past the end"), "{text}");
}
