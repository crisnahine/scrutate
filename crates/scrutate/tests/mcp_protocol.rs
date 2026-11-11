//! The MCP server's JSON-RPC surface on the real binary: what gets an answer, what does not, and
//! what the `serve` flags change.

use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str, files: usize) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_mcpp_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("pkg")).unwrap();
        for i in 0..files {
            let body = format!("import os\n\n\ndef make_widget_{i}(size):\n    return os.path.join(\"w\", str(size))\n");
            fs::write(root.join(format!("pkg/m{i}.py")), body).unwrap();
        }
        Repo { root }
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn spawn(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Child {
    let mut c = Command::new(BIN);
    c.arg("--root").arg(&r.root).args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
    for (k, v) in env {
        c.env(k, v);
    }
    c.spawn().unwrap()
}

/// Send raw lines, close stdin, and return every message the server wrote, and its exit status.
fn exchange(r: &Repo, args: &[&str], lines: &[&[u8]]) -> (Vec<Value>, bool) {
    let mut child = spawn(r, args, &[]);
    let mut stdin = child.stdin.take().unwrap();
    for l in lines {
        stdin.write_all(l).unwrap();
        stdin.write_all(b"\n").unwrap();
    }
    drop(stdin);
    let out = child.wait_with_output().unwrap();
    let msgs = String::from_utf8_lossy(&out.stdout).lines().map(|l| serde_json::from_str(l).unwrap()).collect();
    (msgs, out.status.success())
}

/// A server driven one request at a time.
struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Session {
    fn start(r: &Repo, args: &[&str], env: &[(&str, &str)]) -> Session {
        let mut child = spawn(r, args, env);
        let (stdin, stdout) = (child.stdin.take().unwrap(), BufReader::new(child.stdout.take().unwrap()));
        Session { child, stdin, stdout }
    }
    fn ask(&mut self, msg: Value) -> Value {
        writeln!(self.stdin, "{msg}").unwrap();
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "the server closed stdout");
            let v: Value = serde_json::from_str(&line).unwrap();
            if v["id"] == msg["id"] {
                return v;
            }
        }
    }
    /// The text of a tool call's answer, after the index is built.
    fn text(&mut self, id: i64, name: &str, args: Value) -> String {
        loop {
            let v = self.ask(json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": name, "arguments": args}}));
            let text = v["result"]["content"][0]["text"].as_str().unwrap_or_else(|| panic!("{v}")).to_string();
            if !text.contains("still building its index") {
                return text;
            }
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn an_invalid_utf8_line_is_a_parse_error_and_the_server_keeps_serving() {
    let r = Repo::new("utf8", 2);
    let (msgs, ok) =
        exchange(&r, &["serve"], &[b"{\"jsonrpc\":\"2.0\",\"id\":301,\"method\":\"pi\xff\xfeng\"}", br#"{"jsonrpc":"2.0","id":302,"method":"ping"}"#]);
    assert!(ok);
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert_eq!(msgs[0]["error"]["code"], -32700);
    assert_eq!(msgs[0]["id"], Value::Null);
    assert_eq!(msgs[1]["id"], 302);
    assert_eq!(msgs[1]["result"], json!({}));
}

#[test]
fn a_client_that_disconnects_during_the_first_build_leaves_a_complete_index() {
    let r = Repo::new("eof", 600);
    let init = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    let (msgs, ok) = exchange(&r, &["serve"], &[init]);
    assert!(ok);
    assert_eq!(msgs[0]["id"], 1);
    let db = rusqlite::Connection::open_with_flags(r.root.join(".scrutate/index.db"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let files: i64 = db.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0)).unwrap();
    assert_eq!(files, 600);
}

#[test]
fn notifications_get_no_answer_and_batches_get_an_array() {
    let r = Repo::new("batch", 2);
    let lines: &[&[u8]] = &[
        br#"{"jsonrpc":"2.0","method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"make_widget_1"}}}"#,
        br#"{"jsonrpc":"2.0","method":"ping"}"#,
        b"42",
        br#""x""#,
        b"null",
        b"[]",
        br#"[{"jsonrpc":"2.0","id":11,"method":"ping"},{"jsonrpc":"2.0","method":"notifications/initialized"},{"jsonrpc":"2.0","id":12,"method":"tools/call","params":{"name":"find_symbol","arguments":{"name":"make_widget_1"}}},7]"#,
        br#"[{"jsonrpc":"2.0","method":"notifications/initialized"}]"#,
        br#"{"jsonrpc":"2.0","id":13,"method":"ping"}"#,
    ];
    let (msgs, ok) = exchange(&r, &["serve"], lines);
    assert!(ok);
    let invalid: Vec<&Value> = msgs.iter().filter(|m| m["error"]["code"] == -32600).collect();
    assert_eq!(invalid.len(), 4, "{msgs:?}");
    assert!(invalid.iter().all(|m| m["id"] == Value::Null));
    let batches: Vec<&Value> = msgs.iter().filter(|m| m.is_array()).collect();
    assert_eq!(batches.len(), 1, "{msgs:?}");
    let batch = batches[0].as_array().unwrap();
    assert_eq!(batch.len(), 3, "{batch:?}");
    let by_id = |id: Value| batch.iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no {id} in {batch:?}"));
    assert_eq!(by_id(json!(11))["result"], json!({}));
    assert!(by_id(json!(12))["result"]["content"][0]["text"].is_string());
    assert_eq!(by_id(Value::Null)["error"]["code"], -32600);
    assert!(msgs.iter().any(|m| m["id"] == 13));
    assert_eq!(msgs.len(), 4 + 1 + 1, "{msgs:?}");
}

#[test]
fn a_message_without_a_method_is_a_response_or_an_invalid_request() {
    let r = Repo::new("nomethod", 1);
    let lines: &[&[u8]] = &[
        br#"{"jsonrpc":"2.0","id":81,"result":{}}"#,
        br#"{"jsonrpc":"2.0","id":82,"error":{"code":-1,"message":"no"}}"#,
        br#"{"jsonrpc":"2.0","id":83}"#,
        br#"{"jsonrpc":"2.0","id":84,"method":5}"#,
    ];
    let (msgs, ok) = exchange(&r, &["serve"], lines);
    assert!(ok);
    assert_eq!(msgs.len(), 2, "{msgs:?}");
    assert_eq!(msgs[0]["id"], 83);
    assert_eq!(msgs[0]["error"]["code"], -32600);
    assert_eq!(msgs[1]["id"], 84);
    assert_eq!(msgs[1]["error"]["code"], -32600);
}

#[test]
fn an_unknown_protocol_version_is_answered_with_the_latest() {
    let r = Repo::new("version", 1);
    let init = br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;
    let (msgs, _) = exchange(&r, &["serve"], &[init]);
    assert_eq!(msgs[0]["result"]["protocolVersion"], "2025-11-25");
}

#[test]
fn an_unknown_or_missing_tool_name_is_a_protocol_error() {
    let r = Repo::new("tool", 1);
    let lines: &[&[u8]] = &[
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"no_such_tool","arguments":{}}}"#,
        br#"{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"arguments":{}}}"#,
    ];
    let (msgs, _) = exchange(&r, &["serve"], lines);
    let by_id = |id: i64| msgs.iter().find(|m| m["id"] == id).unwrap_or_else(|| panic!("no {id} in {msgs:?}"));
    assert_eq!(by_id(1)["error"]["code"], -32602);
    assert_eq!(by_id(1)["error"]["message"], "unknown tool: no_such_tool");
    assert_eq!(by_id(2)["error"]["code"], -32602);
    assert_eq!(by_id(2)["error"]["message"], "tools/call has no params.name");
}

#[test]
fn max_bytes_given_to_serve_is_the_default_answer_size() {
    let r = Repo::new("maxbytes", 200);
    let grep = || json!({"pattern": "import os", "max_results": 500});
    let mut wide = Session::start(&r, &["serve"], &[]);
    assert!(wide.text(1, "grep_code", grep()).len() > 5_000);
    for (args, env) in [(&["--max-bytes", "1000", "serve"][..], &[][..]), (&["serve"][..], &[("SCRUTATE_MAX_BYTES", "1000")][..])] {
        let mut s = Session::start(&r, args, env);
        let len = s.text(1, "grep_code", grep()).len();
        assert!(len < 1_200, "{len} bytes under --max-bytes 1000");
        let mut args = grep();
        args["max_bytes"] = json!(5_000);
        assert!(s.text(2, "grep_code", args).len() > 4_000, "a call's own max_bytes still wins");
        let tools = s.ask(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}));
        let find = tools["result"]["tools"].as_array().unwrap().iter().find(|t| t["name"] == "find_symbol").unwrap().clone();
        assert_eq!(find["inputSchema"]["properties"]["max_bytes"]["default"], 1000);
    }
}

#[test]
fn an_explicit_refresh_interval_is_a_floor() {
    let r = Repo::new("refresh", 2);
    let find = |n: &str| json!({"name": n});
    // one after the other: a scan that another server's write holds off does not start the interval
    let mut floor = Session::start(&r, &["serve", "--refresh-interval", "3600"], &[]);
    assert!(floor.text(1, "find_symbol", find("make_widget_1")).contains("pkg/m1.py"));
    let mut default = Session::start(&r, &["serve"], &[]);
    assert!(default.text(1, "find_symbol", find("make_widget_1")).contains("pkg/m1.py"));
    fs::write(r.root.join("pkg/late.py"), "def arrived_late():\n    return 1\n").unwrap();
    std::thread::sleep(std::time::Duration::from_millis(300));
    assert!(!floor.text(2, "find_symbol", find("arrived_late")).contains("pkg/late.py"), "no scan within the interval");
    assert!(default.text(2, "find_symbol", find("arrived_late")).contains("pkg/late.py"), "a cheap scan is not held back by default");
}

#[test]
fn a_new_file_an_argument_names_is_indexed_within_the_refresh_interval() {
    let r = Repo::new("named_new", 2);
    fs::write(r.root.join(".ignore"), "skipped.py\n").unwrap();
    let mut floor = Session::start(&r, &["serve", "--refresh-interval", "3600"], &[]);
    assert!(floor.text(1, "find_symbol", json!({"name": "make_widget_1"})).contains("pkg/m1.py"));
    fs::write(r.root.join("pkg/late.py"), "def arrived_late():\n    return 1\n").unwrap();
    fs::write(r.root.join("skipped.py"), "def ignored_fn():\n    return 1\n").unwrap();
    let outline = floor.text(2, "outline", json!({"path": "pkg/late.py"}));
    assert!(outline.contains("arrived_late"), "{outline}");
    assert!(floor.text(3, "find_symbol", json!({"name": "arrived_late"})).contains("pkg/late.py"), "the file stays indexed");
    // a file a scan would not index is not indexed for naming it either
    let outline = floor.text(4, "outline", json!({"path": "skipped.py"}));
    assert!(outline.contains("not indexed"), "{outline}");
}

#[test]
fn serve_refuses_an_offset_it_would_not_use() {
    let r = Repo::new("offset", 3);
    let out = spawn(&r, &["--offset", "2", "serve"], &[]).wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(2));
}
