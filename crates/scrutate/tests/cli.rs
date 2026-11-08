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
fn utf16_bom_is_searched_by_engine_a_and_skipped_by_engine_b() {
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
    assert_eq!(st["files_skipped"], 1, "Engine B skips UTF-16 as binary (documented limitation): {out}");
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
