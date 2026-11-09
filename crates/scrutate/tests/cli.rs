//! End-to-end tests of the real binary: exit codes, JSON schema, configuration precedence, the
//! unified `find`, cross-repository search, `check`, compaction, the MCP protocol, and the
//! edge cases the traceability matrix promises (UTF-16 BOM, `.git` files, no Python at runtime).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_cli_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Repo { root }
    }
    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
    }
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        run_in(&self.root, args, &[])
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn run_in(cwd: &Path, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(cwd).stdin(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("run scrutate");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// Output lines with the summary's run time removed (it differs on every run).
fn untimed((code, out, err): (i32, String, String)) -> (i32, Vec<String>, String) {
    let out = out
        .lines()
        .map(|l| match l.find("\"elapsed_ms\":") {
            Some(i) => format!("{}{}", &l[..i], &l[i..][l[i..].find(',').unwrap_or(l.len() - i)..]),
            None => l.to_string(),
        })
        .collect();
    (code, out, err)
}

fn python_repo(name: &str) -> Repo {
    let r = Repo::new(name);
    for i in 0..12 {
        r.write(
            &format!("pkg/m{i}.py"),
            format!("\"\"\"Module {i}: widgets.\"\"\"\n\nimport os\nfrom pkg.m{} import make_widget_{}\n\n\ndef make_widget_{i}(size):\n    \"\"\"Build widget number {i} with the given size.\"\"\"\n    return os.path.join(\"w\", str(size))\n\n\ndef use_{i}():\n    return make_widget_{i}(1)\n", (i + 1) % 12, (i + 1) % 12).as_bytes(),
        );
    }
    r.write("README.md", b"# demo\n\nSome docs mention widgets.\n");
    r
}

#[test]
fn exit_codes_and_plain_output() {
    let r = python_repo("exit");
    let (code, out, _) = r.run(&["grep", "-F", "import os", "."]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 12);
    assert!(out.lines().all(|l| l.starts_with("pkg/m") && l.contains(":3:import os")), "{out}");
    let (code, out, _) = r.run(&["grep", "-F", "nothing-like-this", "."]);
    assert_eq!((code, out.as_str()), (1, ""));
    let (code, _, err) = r.run(&["grep", "(unclosed", "."]);
    assert_eq!(code, 2);
    assert!(err.contains("error"), "{err}");
    let (code, _, _) = r.run(&["definitely-not-a-command"]);
    assert_eq!(code, 2);
}

#[test]
fn json_lines_schema_and_modes() {
    let r = python_repo("json");
    let (code, out, _) = r.run(&["grep", "-F", "--json", "--sort", "import os", "."]);
    assert_eq!(code, 0);
    let recs: Vec<serde_json::Value> = out.lines().map(|l| serde_json::from_str(l).expect("json line")).collect();
    let matches: Vec<&serde_json::Value> = recs.iter().filter(|r| r["type"] == "match").collect();
    assert_eq!(matches.len(), 12);
    for m in &matches {
        assert_eq!(m["schema"], 1);
        assert_eq!(m["line"], 3);
        assert!(m["spans"].is_array() && m["column"] == 1, "{m}");
    }
    let summary = recs.iter().find(|r| r["type"] == "summary").expect("summary record");
    assert_eq!(summary["matches"], 12);
    assert_eq!(summary["files_matched"], 12);
    // sorted output is deterministic: two runs, identical match records (the summary carries timings)
    let (_, out2, _) = r.run(&["grep", "-F", "--json", "--sort", "import os", "."]);
    let strip = |s: &str| s.lines().filter(|l| !l.contains("\"summary\"")).collect::<Vec<_>>().join("\n");
    assert_eq!(strip(&out), strip(&out2));
    // counts and file lists
    let (_, out, _) = r.run(&["grep", "-F", "-c", "widget", "."]);
    assert!(out.lines().all(|l| l.rsplit_once(':').map(|(_, n)| n.parse::<u64>().is_ok()).unwrap_or(false)), "{out}");
    let (_, out, _) = r.run(&["grep", "-F", "-l", "widget", "."]);
    assert_eq!(out.lines().count(), 13);
    // identifier mode finds every casing of the whole identifier only
    let (_, out, _) = r.run(&["grep", "--ident", "makeWidget3", "."]);
    assert!(out.lines().all(|l| l.contains("make_widget_3")) && out.lines().count() >= 2, "{out}");
}

#[test]
fn config_file_sets_defaults_but_command_line_wins() {
    let r = python_repo("config");
    r.write(".hidden/h.py", b"def hidden_fn():\n    pass\n");
    r.write(".scrutate.toml", b"[grep]\nhidden = true\nglobs = [\"!pkg/**\"]\n");
    let (code, out, err) = r.run(&["grep", "-F", "-l", "def ", "."]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains(".hidden/h.py") && !out.contains("pkg/"), "config applied: {out}");
    // the command line overrides the file (an explicit glob re-including pkg)
    let (_, out, _) = r.run(&["grep", "-F", "-l", "-g", "pkg/**", "def ", "."]);
    assert!(out.contains("pkg/m1.py"), "{out}");
    // an environment variable beats the file, the command line beats both
    let (_, out, _) = run_in(&r.root, &["grep", "-F", "-l", "def ", "."], &[("SCRUTATE_GREP_HIDDEN", "false")]);
    assert!(!out.contains(".hidden/"), "env beats file: {out}");
    let (_, out, _) = run_in(&r.root, &["grep", "-F", "-l", "--hidden", "def ", "."], &[("SCRUTATE_GREP_HIDDEN", "false")]);
    assert!(out.contains(".hidden/"), "flag beats env: {out}");
    // unknown keys are rejected, loudly
    r.write(".scrutate.toml", b"[grep]\nhiden = true\n");
    let (code, _, err) = r.run(&["grep", "-F", "x", "."]);
    assert_eq!(code, 2);
    assert!(err.contains("hiden"), "{err}");
    r.write(".scrutate.toml", b"[grep]\nhidden = true\n");
    let (code, out, _) = r.run(&["config"]);
    assert_eq!(code, 0);
    assert!(out.contains(".scrutate.toml") && out.contains("SCRUTATE_GREP_HIDDEN=true"), "{out}");
}

#[test]
fn index_search_symbol_refs_outline_deps() {
    let r = python_repo("intel");
    let (code, out, _) = r.run(&["index"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("13 files"), "{out}");
    let (code, out, _) = r.run(&["search", "build a widget with a size", "-k", "3", "--json"]);
    assert_eq!(code, 0);
    let hits: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    assert!(!hits.is_empty() && hits[0]["name"].as_str().unwrap().starts_with("make_widget_"), "{out}");
    let (code, out, _) = r.run(&["symbol", "make_widget_4"]);
    assert_eq!(code, 0);
    assert!(out.contains("pkg/m4.py"), "{out}");
    let (code, out, _) = r.run(&["callers", "make_widget_4", "--json"]);
    assert_eq!(code, 0);
    let callers: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    let names: Vec<&str> = callers.iter().map(|h| h["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"use_4"), "{names:?}");
    let (code, out, _) = r.run(&["refs", "make_widget_4", "--json"]);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["definitions"].as_array().unwrap().len(), 1);
    assert!(v["references"].as_array().unwrap().len() >= 2, "{out}");
    let (code, out, _) = r.run(&["outline", "pkg/m4.py"]);
    assert_eq!(code, 0);
    assert!(out.contains("make_widget_4") && out.contains("use_4"), "{out}");
    let (code, out, _) = r.run(&["deps", "pkg/m4.py"]);
    assert_eq!(code, 0);
    assert!(out.contains("os") && out.contains("pkg.m5") && out.contains("-> pkg/m5.py"), "resolution of a repository module: {out}");
    let (code, out, _) = r.run(&["rdeps", "pkg/m5.py"]);
    assert_eq!(code, 0);
    assert!(out.contains("pkg/m4.py"), "{out}");
    let (code, out, _) = r.run(&["dupes", "--min-tokens", "3", "--threshold", "0.6", "--json"]);
    assert!(code == 0 || code == 1, "{out}");
    let (code, out, _) = r.run(&["tsquery", "python", "(function_definition name: (identifier) @n)", "."]);
    assert_eq!(code, 0);
    assert_eq!(out.lines().count(), 24, "{out}");
    let (code, out, _) = r.run(&["stats"]);
    assert_eq!(code, 0);
    let st: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["files"], 13);
    assert_eq!(st["imports"], 24);
}

#[test]
fn find_routes_by_query_shape() {
    let r = python_repo("find");
    // regex-looking -> Engine A output (path:line:text)
    let (code, out, _) = r.run(&["find", r"def use_\d+"]);
    assert_eq!(code, 0);
    assert!(out.lines().next().unwrap().contains(":def use_"), "{out}");
    // words on a small repository -> ranked results (symbol lines), indexing on the spot
    let (code, out, _) = r.run(&["find", "build widget size"]);
    assert_eq!(code, 0);
    assert!(out.contains("[function]"), "{out}");
}

#[test]
fn search_also_merges_repositories() {
    let a = python_repo("also_a");
    let b = Repo::new("also_b");
    b.write("lib/tokens.py", b"def refresh_expired_token(session):\n    \"\"\"Renew an expired access token.\"\"\"\n    return session.renew()\n");
    let (code, out, _) = run_in(&a.root, &["search", "renew an expired access token", "-k", "3", "--also", b.root.to_str().unwrap(), "--json"], &[]);
    assert_eq!(code, 0, "{out}");
    let hits: Vec<serde_json::Value> = serde_json::from_str(&out).unwrap();
    let label = b.root.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        hits.iter().any(|h| h["path"].as_str().unwrap().starts_with(&format!("{label}/lib/tokens.py"))),
        "cross-repository hit with repository prefix: {out}"
    );
    assert!(hits.iter().any(|h| h["path"].as_str().unwrap().starts_with(&format!("{}/pkg/", a.root.file_name().unwrap().to_string_lossy()))), "{out}");
}

#[test]
fn check_reports_consistency_and_staleness() {
    let r = python_repo("check");
    r.run(&["index"]);
    let (code, out, _) = r.run(&["check", "--files", "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["database_ok"], true);
    assert_eq!(v["chunks_db"], v["chunks_alive"]);
    assert_eq!(v["files_changed"], 0);
    r.write("pkg/m0.py", b"def changed():\n    pass\n");
    let (code, out, _) = r.run(&["check", "--files", "--json"]);
    assert_eq!(code, 1, "stale files are reported: {out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["files_changed"], 1);
    r.run(&["index"]);
    let (code, _, _) = r.run(&["check", "--files"]);
    assert_eq!(code, 0);
}

#[test]
fn compaction_merges_segments_and_keeps_counts() {
    let r = python_repo("compact");
    r.run(&["index"]);
    // six incremental updates each add a segment; compaction must keep the store small and exact
    for round in 0..6 {
        r.write(&format!("pkg/extra{round}.py"), format!("def extra_{round}():\n    return {round}\n").as_bytes());
        let (code, _, _) = r.run(&["index"]);
        assert_eq!(code, 0);
    }
    for i in 0..6 {
        fs::remove_file(r.root.join(format!("pkg/m{i}.py"))).unwrap();
    }
    let (code, _, _) = r.run(&["index"]);
    assert_eq!(code, 0);
    let (code, out, _) = r.run(&["check", "--json"]);
    assert_eq!(code, 0, "{out}");
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["chunks_db"], v["chunks_alive"]);
    assert!(v["segments"].as_u64().unwrap() <= 3, "segments were merged: {out}");
    let (code, out, _) = r.run(&["symbol", "make_widget_2"]);
    assert!(code == 1 && !out.contains("pkg/m2.py"), "deleted file's symbols are gone: {out}");
    let (code, out, _) = r.run(&["symbol", "extra_5"]);
    assert_eq!(code, 0, "{out}");
}

#[test]
fn utf16_bom_is_searched_by_engine_a_and_indexed_by_engine_b() {
    let r = Repo::new("utf16");
    let mut u16le = vec![0xff, 0xfe];
    for u in "def utf16_marker():\n    pass\n".encode_utf16() {
        u16le.extend_from_slice(&u.to_le_bytes());
    }
    r.write("w.py", &u16le);
    r.write("ok.py", b"def plain():\n    pass\n");
    let (code, out, _) = r.run(&["grep", "-F", "utf16_marker", "."]);
    assert_eq!(code, 0, "Engine A transcodes UTF-16 with a BOM: {out}");
    assert!(out.starts_with("w.py:1:"), "{out}");
    r.run(&["index"]);
    let (_, out, _) = r.run(&["stats"]);
    let st: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["files_skipped"], 0, "Engine B transcodes UTF-16 too: {out}");
    let (code, out, _) = r.run(&["symbol", "utf16_marker", "--json"]);
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("\"w.py\"") && out.contains("\"start\": 1"), "{out}");
}

#[test]
fn git_file_of_worktrees_and_submodules_is_not_indexed() {
    let r = Repo::new("gitfile");
    r.write(".git", b"gitdir: /somewhere/else/.git/worktrees/wt\n");
    r.write("vendor/sub/.git", b"gitdir: ../../.git/modules/sub\n");
    r.write("vendor/sub/lib.py", b"def sub_symbol():\n    pass\n");
    r.write("a.py", b"def top():\n    pass\n");
    r.run(&["index"]);
    let (_, out, _) = r.run(&["grep", "-F", "gitdir", "."]);
    assert!(out.is_empty(), "Engine A treats .git files as VCS metadata: {out}");
    let (_, out, _) = r.run(&["stats"]);
    let st: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(st["files"], 2, "{out}");
    let (code, _, _) = r.run(&["symbol", "sub_symbol"]);
    assert_eq!(code, 0);
}

#[test]
fn runs_without_python_on_path() {
    let r = python_repo("nopython");
    let mut c = Command::new(BIN);
    c.args(["grep", "-F", "-c", "import", "."]).current_dir(&r.root).env("PATH", "/nonexistent").env_remove("PYTHONPATH").stdin(Stdio::null());
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let mut c = Command::new(BIN);
    c.args(["index"]).current_dir(&r.root).env("PATH", "/nonexistent").stdin(Stdio::null());
    assert!(c.output().unwrap().status.success());
}

#[test]
fn mcp_protocol_round_trip() {
    let r = python_repo("mcp");
    let mut child = Command::new(BIN)
        .args(["--root", r.root.to_str().unwrap(), "serve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let msgs = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"make_widget_7"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"read_code","arguments":{"path":"../../etc/passwd"}}}"#,
        r#"{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}"#,
        r#"{"jsonrpc":"2.0","id":6,"method":"bogus/method"}"#,
        "this is not json",
    ];
    for m in msgs {
        writeln!(stdin, "{m}").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    assert!(out.status.success());
    let lines: Vec<serde_json::Value> = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    let by_id = |id: i64| lines.iter().find(|v| v["id"] == id).unwrap_or_else(|| panic!("no response {id}"));
    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(by_id(1)["result"]["serverInfo"]["name"], "scrutate");
    let tools = by_id(2)["result"]["tools"].as_array().unwrap();
    assert!(tools.len() >= 14, "{}", tools.len());
    assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object"));
    let text = by_id(3)["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.contains("pkg/m7.py"), "{text}");
    assert_eq!(by_id(3)["result"]["isError"], false);
    let text = by_id(4)["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("error"), "path escape refused: {text}");
    assert_eq!(by_id(5)["result"]["isError"], true);
    assert_eq!(by_id(6)["error"]["code"], -32601);
    assert!(lines.iter().any(|v| v["error"]["code"] == -32700), "parse error for non-JSON input");
}

#[test]
fn corruption_found_during_use_triggers_a_rebuild() {
    // large databases skip the open-time page walk; damage inside a page must still be caught by
    // the first operation that hits it and lead to a rebuild (simulated by disabling the walk)
    let r = python_repo("latecorrupt");
    r.run(&["index"]);
    let db = r.root.join(".scrutate/index.db");
    let mut bytes = fs::read(&db).unwrap();
    assert!(bytes.len() > 8192 * 4);
    // garbage over the fourth page (the header page stays intact)
    for b in bytes[4096 * 3..4096 * 4].iter_mut() {
        *b = 0xA5;
    }
    fs::write(&db, &bytes).unwrap();
    let _ = fs::remove_file(r.root.join(".scrutate/index.db-wal"));
    let _ = fs::remove_file(r.root.join(".scrutate/index.db-shm"));
    let (code, out, err) = run_in(&r.root, &["search", "build a widget", "-k", "2"], &[("SCRUTATE_QUICK_CHECK_MAX_BYTES", "0")]);
    assert_eq!(code, 0, "stdout: {out}\nstderr: {err}");
    assert!(out.contains("make_widget_"), "{out}");
    let (code, out, _) = run_in(&r.root, &["check", "--json"], &[("SCRUTATE_QUICK_CHECK_MAX_BYTES", "0")]);
    assert_eq!(code, 0, "{out}");
}

/// `grep --index` must print exactly what the walk prints, for every kind of query, before and
/// after incremental updates (edits, additions, deletions, renames) and after segment merges.
#[test]
fn indexed_grep_equals_walk() {
    let r = Repo::new("textindex");
    r.write(".gitignore", b"ignored/\n*.log\n");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    for i in 0..40 {
        r.write(
            &format!("src/m{i}.rs"),
            format!("// module {i}\nfn make_widget_{i}(size: usize) -> Widget {{\n    Widget::new(size * {i})\n}}\n\nstruct RefreshToken{i};\nconst KÖNIG_{i}: &str = \"König\";\n").as_bytes(),
        );
    }
    r.write("src/crlf.txt", b"alpha beta\r\nkernel_thread here\r\n");
    r.write("src/bin.dat", b"needle before\0needle after\n");
    r.write("src/late_nul.txt", &[b"needle line\n".repeat(8000), b"\0tail needle\n".to_vec()].concat());
    r.write("src/utf16.txt", &[0xFF, 0xFE, b'n', 0, b'e', 0, b'e', 0, b'd', 0, b'l', 0, b'e', 0, b'\n', 0]);
    r.write("src/multi.c", b"struct file_operations fops = {\n\t.owner = THIS_MODULE,\n};\n");
    r.write("ignored/x.rs", b"fn make_widget_0() {}\n");
    r.write("debug.log", b"make_widget_0\n");
    r.write(".hidden/h.rs", b"fn make_widget_0() {}\n");
    let queries: Vec<Vec<&str>> = vec![
        vec!["-F", "make_widget_1"],
        vec!["make_widget_\\d+"],
        vec!["-i", "-F", "REFRESHTOKEN3"],
        vec!["-w", "-F", "Widget"],
        vec!["--ident", "refreshToken"],
        vec!["--fuzzy=1", "make_widgte"],
        vec!["-i", "könig"],
        vec!["-e", "alpha", "-e", "kernel_thread"],
        vec!["-F", "needle"],
        vec!["-U", "fops = \\{\\n\\s+\\.owner"],
        vec!["-v", "-c", "-F", "Widget"],
        vec!["-l", "-F", "size"],
        vec!["-C", "1", "-F", "struct RefreshToken7"],
        vec!["-g", "*.txt", "-F", "needle"],
        vec!["-t", "rust", "-F", "module"],
        vec!["x{0}"],
        vec!["(ab|cd)?e"],
    ];
    let check = |stage: &str| {
        for q in &queries {
            let mut walk = vec!["grep", "--sort"];
            walk.extend(q.iter());
            let mut idx = walk.clone();
            idx.insert(1, "--index");
            let (c1, o1, e1) = r.run(&walk);
            let (c2, o2, e2) = r.run(&idx);
            assert_eq!((c1, &o1), (c2, &o2), "{stage}: {q:?}\nwalk stderr: {e1}\nindex stderr: {e2}");
        }
    };
    let (code, out, err) = r.run(&["index", "--text"]);
    assert_eq!(code, 0, "{out}{err}");
    assert!(out.contains("text index: 45 files"), "{out}");
    check("fresh");
    // edit, add, delete, rename
    r.write("src/m3.rs", b"fn make_widget_1000() {}\n");
    r.write("src/new.rs", b"fn make_widget_42() {}\nstruct RefreshToken99;\n");
    fs::remove_file(r.root.join("src/m5.rs")).unwrap();
    fs::rename(r.root.join("src/m6.rs"), r.root.join("src/renamed.rs")).unwrap();
    let (_, out, _) = r.run(&["index", "--text"]);
    assert!(out.contains("3 changed, 2 removed"), "{out}");
    check("incremental");
    // many small updates: segments merge, dead documents are purged
    for i in 0..8 {
        r.write(&format!("src/extra{i}.rs"), format!("fn extra_{i}() {{}}\n").as_bytes());
        r.run(&["index", "--text"]);
    }
    let (_, out, _) = r.run(&["index", "--text"]);
    let segs: usize = out.split(" segment(s)").next().and_then(|s| s.rsplit(", ").next()).and_then(|s| s.parse().ok()).unwrap();
    assert!(segs <= 4, "{out}");
    check("merged");
    // a damaged segment is reported with the repair command; rebuilding repairs it
    let seg = fs::read_dir(r.root.join(".scrutate/text")).unwrap().flatten().find(|e| e.path().is_dir()).unwrap().path();
    fs::write(seg.join("ptr.npy"), b"garbage").unwrap();
    let (code, _, err) = r.run(&["grep", "--index", "-F", "make_widget_1"]);
    assert_eq!(code, 2);
    assert!(err.contains("index --text --force"), "{err}");
    let (code, _, _) = r.run(&["index", "--text", "--force"]);
    assert_eq!(code, 0);
    check("rebuilt");
    // flags that change the file set are refused rather than silently ignored
    let (code, _, err) = r.run(&["grep", "--index", "--hidden", "x"]);
    assert_eq!(code, 2, "{err}");
}

/// Each line of a multi-line match is printed with its own number, as grep-class tools do.
#[test]
fn multiline_matches_print_every_line() {
    let r = Repo::new("multiline");
    r.write("a.c", b"int x;\nstruct file_operations fops = {\n\t.owner = THIS_MODULE,\n};\n");
    let (code, out, _) = r.run(&["grep", "-U", "fops = \\{\\n\\s+\\.owner"]);
    assert_eq!(code, 0);
    assert_eq!(out, "a.c:2:struct file_operations fops = {\na.c:3:\t.owner = THIS_MODULE,\n");
}

/// Ctrl-C stops a search and exits 130.
#[cfg(unix)]
#[test]
fn interrupt_exits_130() {
    let r = Repo::new("interrupt");
    let line = "alpha beta gamma delta epsilon zeta eta theta iota kappa lambda mu nu xi omicron\n".repeat(20_000);
    for i in 0..60 {
        r.write(&format!("f{i}.txt"), line.as_bytes());
    }
    // a signal that lands before the handler is installed kills the process: on a loaded machine
    // start-up can take long, so wait longer and try again; a missing handler fails every attempt
    let mut last = None;
    for wait_ms in [200u64, 600, 1500] {
        let mut child = Command::new(BIN)
            .args(["grep", "-j", "1", "--fuzzy=3", "omicron_pi_rho_sigma"])
            .current_dir(&r.root)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(wait_ms));
        if child.try_wait().unwrap().is_some() {
            eprintln!("search finished before the signal; nothing to interrupt on this machine");
            return;
        }
        // the system call, not a `kill` binary: minimal images have none
        extern "C" {
            fn kill(pid: i32, sig: i32) -> i32;
        }
        assert_eq!(unsafe { kill(child.id() as i32, 2) }, 0, "SIGINT");
        let st = child.wait().unwrap();
        if st.code() == Some(130) {
            return;
        }
        last = Some(st);
    }
    panic!("Ctrl-C did not stop the search with exit code 130: {last:?}");
}

/// With --json (or SCRUTATE_DIAGNOSTICS=json) an error is one JSON record on stderr.
#[test]
fn errors_are_json_when_asked() {
    let r = Repo::new("jsonerr");
    r.write("a.txt", b"x\n");
    let (code, _, err) = r.run(&["grep", "--json", "(unclosed"]);
    assert_eq!(code, 2);
    let v: serde_json::Value = serde_json::from_str(err.trim()).unwrap_or_else(|e| panic!("{e}: {err}"));
    assert_eq!(v["type"], "error");
    assert_eq!(v["schema"], 1);
    assert!(v["message"].as_str().unwrap().contains("unclosed"), "{v}");
    let (code, _, err) = run_in(&r.root, &["outline", "missing.py"], &[("SCRUTATE_DIAGNOSTICS", "json")]);
    if code == 2 {
        let v: serde_json::Value = serde_json::from_str(err.trim().lines().last().unwrap()).unwrap();
        assert_eq!(v["type"], "error");
    }
    let (code, _, err) = r.run(&["grep", "(unclosed"]);
    assert_eq!(code, 2);
    assert!(err.starts_with("scrutate: error: "), "{err}");
}

#[test]
fn a_file_searched_in_ranges_equals_one_pass() {
    let r = Repo::new("split");
    let mut big = String::new();
    for i in 0..4000 {
        match i % 7 {
            0 => big.push_str(&format!("alpha {i} König\n")),
            3 => big.push_str(&format!("{}\r\n", "x".repeat(i % 300))),
            5 => big.push('\n'),
            _ => big.push_str(&format!("beta {i} konig gamma\n")),
        }
    }
    big.push_str(&"long ".repeat(400)); // a line longer than a range, and no final newline
    big.push_str("alpha tail");
    r.write("big.txt", big.as_bytes());
    let mut late_nul = big.clone().into_bytes();
    late_nul.splice(late_nul.len() - 500..late_nul.len() - 500, [0u8]);
    r.write("late_nul.txt", &late_nul);
    let mut bom = vec![0xEF, 0xBB, 0xBF];
    bom.extend_from_slice(big.as_bytes());
    r.write("bom.txt", &bom);
    r.write("one_line.txt", format!("{}alpha beta{}", "y".repeat(5000), "z".repeat(5000)).as_bytes());
    r.write("one_line_nul.txt", format!("{}alpha beta{}\0tail alpha", "y".repeat(5000), "z".repeat(5000)).as_bytes());
    for args in [
        &["grep", "alpha"][..],
        &["grep", "-i", "KÖNIG"],
        &["grep", "--json", "beta 2"],
        &["grep", "-c", "konig"],
        &["grep", "-v", "a"],
        &["grep", "--json", "-v", "e"],
        &["grep", "x{20}"],
        &["grep", "nothing here"],
        &["grep", "-j", "3", "gamma"],
    ] {
        for file in ["big.txt", "late_nul.txt", "bom.txt", "one_line.txt", "one_line_nul.txt"] {
            let mut a = args.to_vec();
            a.push(file);
            let one = untimed(run_in(&r.root, &a, &[("SCRUTATE_SPLIT_FILE_BYTES", "999999999999")]));
            let split = untimed(run_in(&r.root, &a, &[("SCRUTATE_SPLIT_FILE_BYTES", "1000")]));
            assert_eq!(one, split, "{a:?}");
            assert!(one.0 == 0 || one.0 == 1, "{a:?}: {one:?}");
            if one.0 == 0 && !a.contains(&"-c") && !a.contains(&"--json") {
                assert!(one.1.iter().all(|l| l.starts_with(&format!("{file}:"))), "{a:?}: {:?}", &one.1[..1]);
            }
        }
    }
}

#[test]
fn small_tree_path_equals_parallel_walk() {
    let r = python_repo("smalltree");
    r.write(".gitignore", b"ignored/\n*.log\n");
    r.write("ignored/x.py", b"make_widget_1 hidden by gitignore\n");
    r.write("a.log", b"make_widget_1 in a log\n");
    r.write(".hidden/h.py", b"make_widget_1 in a hidden dir\n");
    r.write("bin.dat", b"make_widget_1\0binary\n");
    for args in [
        &["grep", "--sort", "make_widget"][..],
        &["grep", "--sort", "--json", "-i", "WIDGET"],
        &["grep", "--sort", "--hidden", "--no-ignore", "make_widget_1"],
        &["grep", "--sort", "-c", "import"],
        &["grep", "--sort", "-l", "widget"],
        &["grep", "--sort", "-g", "*.md", "widgets"],
    ] {
        let small = untimed(run_in(&r.root, args, &[("SCRUTATE_SMALL_TREE", "100000")]));
        let parallel = untimed(run_in(&r.root, args, &[("SCRUTATE_SMALL_TREE", "0")]));
        assert_eq!(small, parallel, "{args:?}");
        assert_eq!(small.0, 0, "{args:?}: {small:?}");
    }
}

#[test]
fn pcre2_look_around_backreferences_and_the_match_limit() {
    let r = Repo::new("pcre2");
    r.write("t.txt", b"foobar\nbazbar\nabba\nabc\n");
    r.write("trap.txt", format!("{}!\n", "a".repeat(40)).repeat(50).as_bytes());
    let (code, out, _) = r.run(&["grep", "-P", "(?<=foo)bar", "t.txt"]);
    assert_eq!((code, out.as_str()), (0, "t.txt:1:foobar\n"));
    let (code, out, _) = r.run(&["grep", "-P", r"(\w)\1", "t.txt"]);
    assert_eq!((code, out.as_str()), (0, "t.txt:1:foobar\nt.txt:3:abba\n"));
    let (code, _, _) = r.run(&["grep", "-P", "-F", "a(b", "t.txt"]);
    assert_eq!(code, 1, "-F makes the pattern literal");
    let (code, out, _) = r.run(&["grep", "-P", "-w", "-e", "ab", "-e", "abc", "t.txt"]);
    assert_eq!((code, out.as_str()), (0, "t.txt:4:abc\n"));
    // exponential backtracking stops at PCRE2's match limit and says so
    let t0 = std::time::Instant::now();
    let (code, out, err) = r.run(&["grep", "-P", "^(a|aa)+$", "trap.txt"]);
    assert!(t0.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!((code, out.as_str()), (1, ""));
    assert!(err.contains("trap.txt") && err.contains("match limit"), "{err}");
    let (code, _, err) = r.run(&["grep", "-P", "(?<=", "t.txt"]);
    assert_eq!(code, 2);
    assert!(err.contains("PCRE2"), "{err}");
}
