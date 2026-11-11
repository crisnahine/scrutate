//! `scrutate lsp` end to end: the real binary over stdio with Content-Length framing, one request of
//! each kind, UTF-16 columns on a non-ASCII line, and a save picked up by the background update.

use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_lsp_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Repo { root }
    }
    fn write(&self, rel: &str, text: &str) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, text).unwrap();
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn uri(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    if s.starts_with('/') {
        format!("file://{s}")
    } else {
        format!("file:///{s}")
    }
}

struct Client {
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: i64,
    notifications: Vec<Value>,
}

impl Client {
    fn send(&mut self, msg: Value) {
        let body = msg.to_string();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{body}", body.len()).unwrap();
        self.stdin.flush().unwrap();
    }
    fn read(&mut self) -> Value {
        let mut len = 0usize;
        loop {
            let mut line = String::new();
            assert!(self.stdout.read_line(&mut line).unwrap() > 0, "server closed its output");
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some(v) = l.strip_prefix("Content-Length:") {
                len = v.trim().parse().unwrap();
            }
        }
        let mut buf = vec![0u8; len];
        self.stdout.read_exact(&mut buf).unwrap();
        serde_json::from_slice(&buf).unwrap()
    }
    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        loop {
            let m = self.read();
            if m["id"] == id {
                return m;
            }
            self.notifications.push(m);
        }
    }
    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }
}

fn utf16(s: &str) -> u64 {
    s.encode_utf16().count() as u64
}

/// (file name, line, character) of each location.
fn places(v: &Value) -> Vec<(String, u64, u64)> {
    v.as_array()
        .unwrap_or_else(|| panic!("not a location list: {v}"))
        .iter()
        .map(|l| {
            let name = l["uri"].as_str().unwrap().rsplit('/').next().unwrap().to_string();
            (name, l["range"]["start"]["line"].as_u64().unwrap(), l["range"]["start"]["character"].as_u64().unwrap())
        })
        .collect()
}

#[test]
fn lsp_round_trip() {
    let r = Repo::new("rt");
    r.write("src/geo.py", "class Point:\n    def norm(self):\n        return 1\n\n\ndef make_point():\n    return Point()\n");
    r.write("src/other.py", "def norm(x):\n    return x\n");
    let use_line = "    label = \"héllo 🙂 ünïcode\"; p = make_point()";
    r.write("src/app.py", &format!("from src.geo import make_point, Point\n\n\ndef main():\n{use_line}\n    return Point.norm(p)\n"));

    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };

    let init = c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    let caps = &init["result"]["capabilities"];
    assert_eq!(caps["definitionProvider"], true, "{init}");
    assert_eq!(caps["referencesProvider"], true);
    assert_eq!(caps["workspaceSymbolProvider"], true);
    assert_eq!(caps["documentSymbolProvider"], true);
    assert!(caps.get("hoverProvider").is_none(), "only implemented capabilities are advertised");
    assert_eq!(init["result"]["serverInfo"]["name"], "scrutate");
    c.notify("initialized", json!({}));

    let app = uri(&r.root.join("src/app.py"));
    let app_text = fs::read_to_string(r.root.join("src/app.py")).unwrap();
    c.notify("textDocument/didOpen", json!({"textDocument": {"uri": app, "languageId": "python", "version": 1, "text": app_text}}));

    // cursor in the middle of `make_point` on the non-ASCII line: the column is in UTF-16 units
    let col = utf16(&use_line[..use_line.find("make_point").unwrap()]);
    assert_ne!(col as usize, use_line.find("make_point").unwrap(), "the line must have multi-unit characters");
    let def = c.request("textDocument/definition", json!({"textDocument": {"uri": app}, "position": {"line": 4, "character": col + 3}}));
    assert_eq!(places(&def["result"]), vec![("geo.py".to_string(), 5, 4)], "{def}");
    assert!(
        c.notifications.iter().any(|n| n["method"] == "window/logMessage" && n["params"]["message"].as_str().unwrap().contains("building")),
        "{:?}",
        c.notifications
    );

    // qualified name wins over the other `norm`
    let line5 = "    return Point.norm(p)";
    let def = c.request(
        "textDocument/definition",
        json!({"textDocument": {"uri": app}, "position": {"line": 5, "character": utf16(&line5[..line5.find("norm").unwrap()])}}),
    );
    assert_eq!(places(&def["result"]), vec![("geo.py".to_string(), 1, 8)], "{def}");

    let refs = c.request(
        "textDocument/references",
        json!({"textDocument": {"uri": app}, "position": {"line": 4, "character": col}, "context": {"includeDeclaration": true}}),
    );
    let got = places(&refs["result"]);
    for want in [("geo.py", 5, 4), ("app.py", 0, utf16("from src.geo import ")), ("app.py", 4, col)] {
        assert!(got.contains(&(want.0.to_string(), want.1, want.2)), "missing {want:?} in {got:?}");
    }
    assert_eq!(got.len(), 3, "{got:?}");
    let refs = c.request(
        "textDocument/references",
        json!({"textDocument": {"uri": app}, "position": {"line": 4, "character": col}, "context": {"includeDeclaration": false}}),
    );
    assert!(!places(&refs["result"]).contains(&("geo.py".to_string(), 5, 4)), "{refs}");

    // references of the method under the cursor, not of the unrelated `norm` in other.py
    let refs = c.request(
        "textDocument/references",
        json!({"textDocument": {"uri": app}, "position": {"line": 5, "character": utf16(&line5[..line5.find("norm").unwrap()])}, "context": {"includeDeclaration": true}}),
    );
    let got = places(&refs["result"]);
    assert_eq!(got, vec![("geo.py".to_string(), 1, 8), ("app.py".to_string(), 5, utf16(&line5[..line5.find("norm").unwrap()]))], "{refs}");

    let ws = c.request("workspace/symbol", json!({"query": "make_po"}));
    let syms = ws["result"].as_array().unwrap();
    assert_eq!(syms[0]["name"], "make_point", "{ws}");
    assert_eq!(syms[0]["kind"], 12);
    assert!(syms[0]["location"]["uri"].as_str().unwrap().ends_with("/src/geo.py"));

    let outline = c.request("textDocument/documentSymbol", json!({"textDocument": {"uri": uri(&r.root.join("src/geo.py"))}}));
    let top = outline["result"].as_array().unwrap_or_else(|| panic!("{outline}"));
    let names: Vec<&str> = top.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["Point", "make_point"], "{outline}");
    assert_eq!(top[0]["kind"], 5);
    assert_eq!(top[0]["children"][0]["name"], "norm", "methods nest under their class: {outline}");
    assert_eq!(top[0]["children"][0]["selectionRange"]["start"], json!({"line": 1, "character": 8}));

    // a save is indexed in the background; requests keep answering meanwhile
    r.write("src/other.py", "def norm(x):\n    return x\n\n\ndef freshly_saved():\n    return 2\n");
    c.notify("textDocument/didSave", json!({"textDocument": {"uri": uri(&r.root.join("src/other.py"))}}));
    let t0 = Instant::now();
    loop {
        let ws = c.request("workspace/symbol", json!({"query": "freshly_saved"}));
        if ws["result"].as_array().is_some_and(|a| a.iter().any(|s| s["name"] == "freshly_saved")) {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(60), "save never reached the index: {ws}");
        std::thread::sleep(Duration::from_millis(100));
    }

    let unknown = c.request("textDocument/hover", json!({}));
    assert_eq!(unknown["error"]["code"], -32601);
    let bye = c.request("shutdown", Value::Null);
    assert!(bye.get("error").is_none(), "{bye}");
    c.notify("exit", Value::Null);
    drop(c);
    assert_eq!(child.wait().unwrap().code(), Some(0), "exit after shutdown is a clean exit");
}

#[test]
fn definition_takes_the_name_with_dashes_arrows_and_colons_around_the_cursor() {
    let r = Repo::new("names");
    r.write("core.clj", "(ns app.core)\n\n(defn get-user [id]\n  {:id id})\n\n(defn run []\n  (get-user 1))\n");
    r.write("util.el", "(defun my-trim (s)\n  (string-trim s))\n\n(defun use-it (s)\n  (my-trim s))\n");
    r.write("conv.scm", "(define (list->vec l)\n  (list->vector l))\n\n(define (use l)\n  (list->vec l))\n");
    r.write("mod.rb", "class Foo::Bar\n  def call\n    1\n  end\nend\n\ndef use\n  Foo::Bar.new.call\nend\n");
    r.write("app.py", "def user():\n    return 1\n\ndef get():\n    return 2\n");

    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    for (file, line, ch, want) in [
        ("core.clj", 6, 4, ("core.clj", 2)),
        ("core.clj", 6, 8, ("core.clj", 2)),
        ("util.el", 4, 7, ("util.el", 0)),
        ("conv.scm", 4, 5, ("conv.scm", 0)),
        ("mod.rb", 7, 8, ("mod.rb", 0)),
        ("mod.rb", 7, 16, ("mod.rb", 1)),
    ] {
        let def = c.request("textDocument/definition", json!({"textDocument": {"uri": uri(&r.root.join(file))}, "position": {"line": line, "character": ch}}));
        let got: Vec<(String, u64)> = places(&def["result"]).into_iter().map(|(f, l, _)| (f, l)).collect();
        assert_eq!(got, [(want.0.to_string(), want.1)], "{file}:{line}:{ch}: {def}");
    }
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_unix_file_name_stays_in_the_uri() {
    let r = Repo::new("backslash");
    r.write("a\\b.py", "def slash_fn():\n    return 1\n\n\ndef use():\n    return slash_fn()\n");
    let mut child = Command::new(BIN).arg("lsp").current_dir(&r.root).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut c = Client { stdin: child.stdin.take().unwrap(), stdout: BufReader::new(child.stdout.take().unwrap()), next_id: 0, notifications: Vec::new() };
    c.request("initialize", json!({"processId": null, "rootUri": uri(&r.root), "capabilities": {}}));
    c.notify("initialized", json!({}));
    let file = format!("{}/a%5Cb.py", uri(&fs::canonicalize(&r.root).unwrap()));
    let def = c.request("textDocument/definition", json!({"textDocument": {"uri": file}, "position": {"line": 5, "character": 12}}));
    let uris: Vec<&str> = def["result"].as_array().unwrap_or_else(|| panic!("{def}")).iter().map(|l| l["uri"].as_str().unwrap()).collect();
    assert_eq!(uris, [file.as_str()], "{def}");
    c.request("shutdown", Value::Null);
    c.notify("exit", Value::Null);
    drop(c);
    child.wait().unwrap();
}
