//! Language Server Protocol front end on stdio (JSON-RPC 2.0, `Content-Length` framing): definitions,
//! references, workspace symbols and document outlines answered from the Engine B index. Hand-written
//! like the MCP server: the read-only subset an index-backed server needs is small.

use std::collections::{HashMap, HashSet};
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex, MutexGuard};

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};

use crate::index::{ChunkRow, Index, CHUNK_COLS};
use crate::search::Searcher;
use crate::text::name_key;

type Shared = Arc<Mutex<Option<Searcher>>>;

const MAX_MESSAGE: usize = 256 << 20;

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Put a fresh index in place of a damaged one. The caller has closed its own connection; the
/// readers' one closes here, and requests wait on the lock until the new index is there.
fn replace_damaged(shared: &Shared, root: &Path) {
    let mut g = lock(shared);
    *g = None;
    crate::index::wipe_index(root);
    match crate::open_updated(root, true) {
        Ok((s, _)) => *g = Some(s),
        Err(e) => eprintln!("scrutate: index rebuild failed: {e:#}"),
    }
}

fn send(msg: &Value) {
    let body = msg.to_string();
    // the stdout lock keeps a message whole when the indexing thread logs at the same time
    let mut out = std::io::stdout().lock();
    let _ = write!(out, "Content-Length: {}\r\n\r\n{body}", body.len());
    let _ = out.flush();
}

fn log(kind: u8, message: &str) {
    send(&json!({"jsonrpc": "2.0", "method": "window/logMessage", "params": {"type": kind, "message": message}}));
}

/// One message body, or None at end of input.
pub fn read_message(r: &mut impl BufRead) -> Result<Option<Vec<u8>>> {
    let mut len: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let l = line.trim_end();
        if l.is_empty() {
            if len.is_some() {
                break;
            }
            continue;
        }
        if let Some((k, v)) = l.split_once(':') {
            if k.trim().eq_ignore_ascii_case("content-length") {
                len = Some(v.trim().parse().context("bad Content-Length header")?);
            }
        }
    }
    let n = len.unwrap_or(0);
    if n > MAX_MESSAGE {
        bail!("message of {n} bytes is too large");
    }
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

// ------------------------------------------------------------------------- positions and URIs

/// Byte offset of a UTF-16 column in `line` (past the end clamps to the end).
pub fn utf16_to_byte(line: &str, units: u64) -> usize {
    let mut acc = 0u64;
    for (i, c) in line.char_indices() {
        if acc >= units {
            return i;
        }
        acc += c.len_utf16() as u64;
    }
    line.len()
}

pub fn byte_to_utf16(line: &str, byte: usize) -> u64 {
    line[..byte.min(line.len())].encode_utf16().count() as u64
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Byte range of the identifier at (or just before) `at`.
pub fn word_at(line: &str, at: usize) -> Option<(usize, usize)> {
    let at = at.min(line.len());
    let mut a = at;
    while let Some(c) = line[..a].chars().next_back().filter(|c| is_ident(*c)) {
        a -= c.len_utf8();
    }
    let mut b = at;
    while let Some(c) = line[b..].chars().next().filter(|c| is_ident(*c)) {
        b += c.len_utf8();
    }
    (a < b).then_some((a, b))
}

/// Start and end byte of the run of characters around `a..b` up to a space, bracket, quote or
/// comma: `get-user`, `list->vec`, `Foo::Bar.new`.
fn run_around(line: &str, a: usize, b: usize) -> (usize, usize) {
    let stop = |c: char| c.is_whitespace() || "()[]{}\"'`,".contains(c);
    let start = line[..a].rfind(stop).map_or(0, |i| i + line[i..].chars().next().map_or(1, char::len_utf8));
    let end = line[b..].find(stop).map_or(line.len(), |i| b + i);
    (start, end)
}

/// Byte offsets of `word` in `line` where it is not part of a longer identifier.
fn find_word(line: &str, word: &str) -> Vec<usize> {
    line.match_indices(word)
        .filter(|(i, _)| !line[..*i].chars().next_back().is_some_and(is_ident) && !line[i + word.len()..].chars().next().is_some_and(is_ident))
        .map(|(i, _)| i)
        .collect()
}

/// `Point` in `Point.norm`, `Point::norm` or `p->norm` when the cursor is on `norm`.
fn qualifier(before: &str) -> Option<&str> {
    let b = before.strip_suffix('.').or_else(|| before.strip_suffix("::")).or_else(|| before.strip_suffix("->"))?;
    let (a, e) = word_at(b, b.len())?;
    Some(&b[a..e])
}

pub fn path_to_uri(p: &Path) -> String {
    let s = crate::instant::slashed(p.to_string_lossy());
    let mut out = String::from(if s.starts_with('/') { "file://" } else { "file:///" });
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~/:".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn uri_to_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest).as_bytes();
    let mut raw = Vec::with_capacity(rest.len());
    let mut i = 0;
    while i < rest.len() {
        let hex = rest.get(i + 1..i + 3).and_then(|h| std::str::from_utf8(h).ok()).and_then(|h| u8::from_str_radix(h, 16).ok());
        match hex {
            Some(v) if rest[i] == b'%' => {
                raw.push(v);
                i += 3;
            }
            _ => {
                raw.push(rest[i]);
                i += 1;
            }
        }
    }
    let s = String::from_utf8(raw).ok()?;
    // `/C:/x` on Windows
    let s = match s.as_bytes() {
        [b'/', d, b':', ..] if cfg!(windows) && d.is_ascii_alphabetic() => s[1..].to_string(),
        _ => s,
    };
    Some(PathBuf::from(s))
}

fn symbol_kind(kind: &str) -> u8 {
    match kind {
        "module" => 2,
        "section" | "block" => 3,
        "method" => 6,
        "enum" => 10,
        "interface" | "trait" => 11,
        "function" | "macro" => 12,
        "impl" => 19,
        "key" => 20,
        "struct" => 23,
        "class" | "type" => 5,
        _ => 13,
    }
}

fn range(line: u64, c0: u64, end_line: u64, c1: u64) -> Value {
    json!({"start": {"line": line, "character": c0}, "end": {"line": end_line, "character": c1}})
}

// ------------------------------------------------------------------------- the server

/// File lines for one request: the editor's text for open documents, the disk otherwise.
struct Lines<'a> {
    root: &'a Path,
    docs: &'a HashMap<String, String>,
    cache: HashMap<String, Vec<String>>,
}

impl Lines<'_> {
    fn get(&mut self, rel: &str) -> &[String] {
        if !self.cache.contains_key(rel) {
            let text = match self.docs.get(rel) {
                Some(t) => t.clone(),
                None => std::fs::read(self.root.join(rel)).ok().and_then(|raw| crate::index::decode_source(&raw).map(|c| c.into_owned())).unwrap_or_default(),
            };
            self.cache.insert(rel.to_string(), split_lines(&text));
        }
        &self.cache[rel]
    }

    /// (0-based line, UTF-16 start, UTF-16 end) of `name` in the first lines of a definition.
    fn name_pos(&mut self, rel: &str, start: i64, end: i64, name: &str) -> (u64, u64, u64) {
        let first = (start.max(1) - 1) as usize;
        let lines = self.get(rel);
        if !name.is_empty() {
            for i in first..lines.len().min(end.max(start).min(start + 5) as usize) {
                if let Some(&b) = find_word(&lines[i], name).first() {
                    return (i as u64, byte_to_utf16(&lines[i], b), byte_to_utf16(&lines[i], b + name.len()));
                }
            }
        }
        (first as u64, 0, 0)
    }

    fn location(&mut self, rel: &str, start: i64, end: i64, name: &str) -> Value {
        let (l, a, b) = self.name_pos(rel, start, end, name);
        json!({"uri": path_to_uri(&self.root.join(rel)), "range": range(l, a, l, b)})
    }
}

fn split_lines(text: &str) -> Vec<String> {
    text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).to_string()).collect()
}

struct Server {
    root: PathBuf,
    /// `--root` named the root: without a folder from the editor it is used as given
    root_given: bool,
    searcher: Shared,
    /// open documents by repository-relative path (by URI when outside the root)
    docs: HashMap<String, String>,
    saves: Option<mpsc::Sender<()>>,
    /// a request met a damaged database: the save worker replaces the index
    damaged: Arc<AtomicBool>,
    initialized: bool,
    shutdown: bool,
}

type Reply = std::result::Result<Value, (i64, String)>;

impl Server {
    fn key(&self, uri: &str) -> String {
        self.rel(uri).unwrap_or_else(|| uri.to_string())
    }

    fn rel(&self, uri: &str) -> Option<String> {
        let p = uri_to_path(uri)?;
        let p = std::fs::canonicalize(&p).unwrap_or(p);
        let r = p.strip_prefix(&self.root).ok()?;
        Some(crate::instant::slashed(r.to_string_lossy()))
    }

    fn lines(&self) -> Lines<'_> {
        Lines { root: &self.root, docs: &self.docs, cache: HashMap::new() }
    }

    fn initialize(&mut self, p: &Value) -> Value {
        // the first folder decides: files of another repository's folder are not answered
        let folder =
            p["workspaceFolders"][0]["uri"].as_str().or(p["rootUri"].as_str()).and_then(uri_to_path).or_else(|| p["rootPath"].as_str().map(PathBuf::from));
        let from_editor = folder.is_some();
        let ruled = from_editor || !self.root_given;
        let folder = folder.unwrap_or_else(|| self.root.clone());
        let folder = std::path::absolute(&folder).unwrap_or(folder);
        // the CLI's rule, so the editor and a shell in the same folder share the index at the top
        let top = ruled.then(|| crate::cli::index_above(&folder)).flatten();
        if let Some(top) = &top {
            eprintln!("[scrutate] using the index in {}", top.display().to_string().trim_start_matches(r"\\?\"));
        }
        // a folder from the editor is the user's choice; the server's own directory is not
        let refused = (!from_editor && !self.root_given && top.is_none()).then(|| crate::cli::no_first_build(&folder)).flatten();
        let root = top.unwrap_or(folder);
        self.root = std::fs::canonicalize(&root).unwrap_or(root);
        let existed = crate::index::exists(&self.root);
        let (ready_tx, ready_rx) = mpsc::channel();
        let (shared, root) = (Arc::clone(&self.searcher), self.root.clone());
        let builder = std::thread::Builder::new().name("scrutate-index".into()).spawn(move || {
            let mut g = lock(&shared);
            let _ = ready_tx.send(()); // requests now wait for the index instead of seeing an empty one
            if let Some(why) = refused {
                eprintln!("{why}");
                return log(1, &why);
            }
            if !existed {
                log(3, &format!("scrutate: no index in {}; building it", root.display()));
            }
            // another process's first build: read what it commits as it commits it
            let opened = crate::open_updated(&root, false).or_else(|e| match e.is::<crate::index::BeingBuilt>() {
                true => crate::open(&root).map(|s| (s, Default::default())),
                false => Err(e),
            });
            match opened {
                Ok((s, st)) => {
                    if !existed || st.files_changed > 0 || st.files_removed > 0 {
                        log(
                            3,
                            &format!(
                                "scrutate: index ready ({} files, {} changed, {} chunks, {:.1}s)",
                                st.files_total, st.files_changed, st.chunks, st.seconds
                            ),
                        );
                    }
                    *g = Some(s);
                }
                Err(e) => log(1, &format!("scrutate: indexing failed: {e:#}")),
            }
        });
        if builder.is_ok() {
            let _ = ready_rx.recv();
        }
        self.saves = self.save_worker();
        json!({
            "capabilities": {
                "positionEncoding": "utf-16",
                "textDocumentSync": {"openClose": true, "change": 1, "save": {"includeText": false}},
                "definitionProvider": true,
                "referencesProvider": true,
                "workspaceSymbolProvider": true,
                "documentSymbolProvider": true,
            },
            "serverInfo": {"name": "scrutate", "version": crate::VERSION},
        })
    }

    /// Saves are indexed on a second connection, so requests keep reading meanwhile (WAL).
    fn save_worker(&self) -> Option<mpsc::Sender<()>> {
        let (tx, rx) = mpsc::channel::<()>();
        let (shared, root, damaged) = (Arc::clone(&self.searcher), self.root.clone(), Arc::clone(&self.damaged));
        std::thread::Builder::new()
            .name("scrutate-save".into())
            .spawn(move || {
                let mut ix: Option<Index> = None;
                while rx.recv().is_ok() {
                    while rx.try_recv().is_ok() {} // one update covers every save queued meanwhile
                    if damaged.swap(false, Ordering::Relaxed) {
                        ix = None;
                        replace_damaged(&shared, &root);
                        continue;
                    }
                    if ix.is_none() {
                        // the main index's model: another signature would make the index rebuild itself
                        let Some(model) = lock(&shared).as_ref().map(|s| s.ix.model.clone()) else { continue };
                        ix = Index::open(&root, model).map_err(|e| eprintln!("scrutate: {e:#}")).ok();
                    }
                    match ix.as_mut().and_then(|ix| ix.update(false).err()) {
                        Some(e) if crate::index::error_is_corruption(&e) => {
                            ix = None;
                            replace_damaged(&shared, &root);
                        }
                        Some(e) => eprintln!("scrutate: index update failed: {e:#}"),
                        None => {}
                    }
                }
            })
            .ok()?;
        Some(tx)
    }

    fn notify(&mut self, method: &str, p: &Value) {
        let uri = p["textDocument"]["uri"].as_str().unwrap_or("");
        match method {
            "textDocument/didOpen" => {
                let text = p["textDocument"]["text"].as_str().unwrap_or("").to_string();
                self.docs.insert(self.key(uri), text);
            }
            "textDocument/didChange" => {
                // full sync: the last change holds the whole text
                if let Some(text) = p["contentChanges"].as_array().and_then(|c| c.last()).and_then(|c| c["text"].as_str()) {
                    self.docs.insert(self.key(uri), text.to_string());
                }
            }
            "textDocument/didClose" => {
                self.docs.remove(&self.key(uri));
            }
            "textDocument/didSave" => {
                if let Some(tx) = &self.saves {
                    let _ = tx.send(());
                }
            }
            _ => {}
        }
    }

    fn request(&mut self, method: &str, p: &Value) -> Reply {
        if method == "initialize" {
            if self.initialized {
                return Err((-32600, "already initialized".into()));
            }
            self.initialized = true;
            return Ok(self.initialize(p));
        }
        if !self.initialized {
            return Err((-32002, "server not initialized".into()));
        }
        if self.shutdown {
            return Err((-32600, "server is shutting down".into()));
        }
        if method == "shutdown" {
            self.shutdown = true;
            return Ok(Value::Null);
        }
        let g = lock(&self.searcher);
        let run = |f: &dyn Fn(&Searcher) -> Result<Value>| -> Reply {
            let s = g.as_ref().ok_or((-32603, "the index is not available (see the log)".to_string()))?;
            f(s).map_err(|e| {
                if crate::index::error_is_corruption(&e) {
                    // the index is a cache: the save worker builds it again
                    self.damaged.store(true, Ordering::Relaxed);
                    if let Some(tx) = &self.saves {
                        let _ = tx.send(());
                    }
                    return (-32603, "the index database is damaged and is being rebuilt; ask again".to_string());
                }
                (-32603, format!("{e:#}"))
            })
        };
        match method {
            "textDocument/definition" => run(&|s| self.definition(s, p)),
            "textDocument/references" => run(&|s| self.references(s, p)),
            "textDocument/documentSymbol" => run(&|s| self.document_symbols(s, p)),
            "workspace/symbol" => run(&|s| self.workspace_symbols(s, p)),
            _ => Err((-32601, format!("method not found: {method}"))),
        }
    }

    /// The identifier under the cursor, the text just before it, and the line with its byte range.
    fn word_in_line(&self, p: &Value) -> Option<(String, usize, usize)> {
        let rel = self.key(p["textDocument"]["uri"].as_str()?);
        let line = p["position"]["line"].as_u64()? as usize;
        let mut lines = self.lines();
        let text = lines.get(&rel).get(line)?.clone();
        let (a, b) = word_at(&text, utf16_to_byte(&text, p["position"]["character"].as_u64()?))?;
        Some((text, a, b))
    }

    /// The identifier under the cursor and the text just before it.
    fn ident_at(&self, p: &Value) -> Option<(String, String)> {
        let (text, a, b) = self.word_in_line(p)?;
        Some((text[a..b].to_string(), text[..a].to_string()))
    }

    fn definition(&self, s: &Searcher, p: &Value) -> Result<Value> {
        let Some((text, a, b)) = self.word_in_line(p) else { return Ok(Value::Null) };
        let (ident, before) = (&text[a..b], &text[..a]);
        let here = p["textDocument"]["uri"].as_str().and_then(|u| self.rel(u));
        // the run up to the word (`my-trim`, `Foo::Bar` in `Foo::Bar.new`), then the whole run as
        // one name (`get-user` from `get`) unless the word is the owner of what follows
        let (start, end) = run_around(&text, a, b);
        let mut hits = if &text[start..b] != ident { s.find_symbol(&text[start..b], 50)? } else { Vec::new() };
        let owner = [".", "::", "#"].iter().any(|sep| text[b..].starts_with(sep));
        if hits.is_empty() && end > b && !owner {
            hits = s.symbol_rows(&text[start..end], 50)?;
        }
        if hits.is_empty() {
            let qualified = match qualifier(before) {
                Some(q) => s.find_symbol(&format!("{q}.{ident}"), 50)?,
                None => Vec::new(),
            };
            hits = if qualified.is_empty() { s.find_symbol(ident, 50)? } else { qualified };
        }
        hits.sort_by_key(|h| Some(&h.path) != here.as_ref()); // stable: index order within each group
        let mut lines = self.lines();
        Ok(Value::Array(hits.iter().map(|h| lines.location(&h.path, h.start, h.end, &h.name)).collect()))
    }

    fn references(&self, s: &Searcher, p: &Value) -> Result<Value> {
        let Some((ident, before)) = self.ident_at(p) else { return Ok(Value::Null) };
        let include_decl = p["context"]["includeDeclaration"].as_bool().unwrap_or(true);
        // the definition under the cursor; an unresolved cursor falls back to every textual occurrence
        let here = p["textDocument"]["uri"].as_str().and_then(|u| self.rel(u));
        let line = p["position"]["line"].as_u64().unwrap_or(0);
        if let Some(rel) = here {
            let mut rv = crate::resolve::Resolver::new(&s.ix)?;
            if let Ok((def_id, _)) = rv.resolve_at(&rel, line + 1, before.len(), &ident) {
                let r = crate::resolve::resolved_references(s, &ident, 5000)?;
                let mut lines = self.lines();
                let mut out = Vec::new();
                if let (true, Some(d)) = (include_decl, r.definitions.iter().find(|d| d.id == def_id)) {
                    let (l, a, b) = lines.name_pos(&d.path, d.start, d.end, &d.name);
                    out.push(json!({"uri": path_to_uri(&self.root.join(&d.path)), "range": range(l, a, l, b)}));
                }
                let width = ident.encode_utf16().count() as u64;
                for o in r.of(def_id) {
                    if matches!(o.resolution, Ok((_, "definition"))) {
                        continue;
                    }
                    let (l, a) = (o.line.saturating_sub(1), byte_to_utf16(&o.text, o.column as usize - 1));
                    out.push(json!({"uri": path_to_uri(&self.root.join(&o.path)), "range": range(l, a, l, a + width)}));
                }
                return Ok(Value::Array(out));
            }
        }
        let r = crate::intel::references(s, &ident, 5000)?;
        let mut lines = self.lines();
        let mut seen: HashSet<(String, u64, u64)> = HashSet::new();
        let mut out = Vec::new();
        for d in &r.definitions {
            let (l, a, b) = lines.name_pos(&d.path, d.start, d.end, &d.name);
            seen.insert((d.path.clone(), l, a));
            if include_decl {
                out.push(json!({"uri": path_to_uri(&self.root.join(&d.path)), "range": range(l, a, l, b)}));
            }
        }
        let width = ident.encode_utf16().count() as u64;
        for g in &r.groups {
            for h in &g.lines {
                let text = h.text.trim_end_matches(['\n', '\r']);
                for off in find_word(text, &ident) {
                    let (l, a) = (h.line.saturating_sub(1), byte_to_utf16(text, off));
                    if seen.insert((g.path.clone(), l, a)) {
                        out.push(json!({"uri": path_to_uri(&self.root.join(&g.path)), "range": range(l, a, l, a + width)}));
                    }
                }
            }
        }
        Ok(Value::Array(out))
    }

    fn document_symbols(&self, s: &Searcher, p: &Value) -> Result<Value> {
        let Some(rel) = p["textDocument"]["uri"].as_str().and_then(|u| self.rel(u)) else { return Ok(Value::Null) };
        let rows = crate::intel::outline(&s.ix, &rel)?;
        let mut lines = self.lines();
        // rows come by start line, enclosing symbols first: a stack of open symbols nests them
        let mut roots: Vec<Value> = Vec::new();
        let mut stack: Vec<(i64, Value, Vec<Value>)> = Vec::new();
        let pop = |stack: &mut Vec<(i64, Value, Vec<Value>)>, roots: &mut Vec<Value>| {
            let (_, mut v, children) = stack.pop().unwrap();
            if !children.is_empty() {
                v["children"] = Value::Array(children);
            }
            match stack.last_mut() {
                Some(parent) => parent.2.push(v),
                None => roots.push(v),
            }
        };
        for r in &rows {
            while stack.last().is_some_and(|top| top.0 < r.end) {
                pop(&mut stack, &mut roots);
            }
            stack.push((r.end, document_symbol(&mut lines, &rel, r), Vec::new()));
        }
        while !stack.is_empty() {
            pop(&mut stack, &mut roots);
        }
        Ok(Value::Array(roots))
    }

    fn workspace_symbols(&self, s: &Searcher, p: &Value) -> Result<Value> {
        let q = name_key(p["query"].as_str().unwrap_or(""));
        let mut st = s.ix.db.prepare(&format!(
            "SELECT {CHUNK_COLS} FROM chunks WHERE kind != 'window' AND name != '' AND instr(name_key, ?1) > 0 \
             ORDER BY name_key != ?1, substr(name_key, 1, length(?1)) != ?1, is_test, length(name), path, start LIMIT 200"
        ))?;
        let rows: Vec<ChunkRow> = st.query_map([&q], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        let mut lines = self.lines();
        Ok(Value::Array(
            rows.iter()
                .map(|r| {
                    let container = r.qualname.rsplit_once('.').map(|(c, _)| c).unwrap_or("");
                    json!({"name": r.name, "kind": symbol_kind(&r.kind), "containerName": container,
                           "location": lines.location(&r.path, r.start, r.end, &r.name)})
                })
                .collect(),
        ))
    }
}

fn document_symbol(lines: &mut Lines, rel: &str, r: &ChunkRow) -> Value {
    let (l, a, b) = lines.name_pos(rel, r.start, r.end, &r.name);
    let last = (r.end.max(r.start).max(1) - 1) as usize;
    let end_col = lines.get(rel).get(last).map(|t| t.encode_utf16().count() as u64).unwrap_or(0);
    let name = [&r.name, &r.qualname, &r.kind].into_iter().find(|n| !n.is_empty()).cloned().unwrap_or_else(|| "?".into());
    json!({
        "name": name,
        "detail": r.kind,
        "kind": symbol_kind(&r.kind),
        "range": range((r.start.max(1) - 1) as u64, 0, last as u64, end_col),
        "selectionRange": range(l, a, l, b),
    })
}

/// Serve until `exit` or end of input. -> the process exit code (0 only after `shutdown`).
pub fn serve(root: &Path, root_given: bool) -> Result<i32> {
    let mut srv = Server {
        root: root.to_path_buf(),
        root_given,
        searcher: Arc::new(Mutex::new(None)),
        docs: HashMap::new(),
        saves: None,
        damaged: Arc::new(AtomicBool::new(false)),
        initialized: false,
        shutdown: false,
    };
    let mut input = std::io::stdin().lock();
    while let Some(body) = read_message(&mut input)? {
        let msg: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(_) => {
                send(&json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": "parse error"}}));
                continue;
            }
        };
        let method = msg["method"].as_str().unwrap_or("");
        let params = &msg["params"];
        let Some(id) = msg.get("id") else {
            if method == "exit" {
                break;
            }
            srv.notify(method, params);
            continue;
        };
        if method.is_empty() {
            continue; // a response: this server sends no requests
        }
        // a bug in one request must not take the editor's server down
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| srv.request(method, params)))
            .unwrap_or_else(|_| Err((-32603, format!("internal failure while answering {method} (a bug: please report it)"))));
        send(&match reply {
            Ok(result) => json!({"jsonrpc": "2.0", "id": id, "result": result}),
            Err((code, message)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
        });
    }
    Ok(if srv.shutdown { 0 } else { 1 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf16_columns() {
        let line = "s = \"é🙂\"; p = make_point()";
        let b = line.find("make_point").unwrap();
        let u = byte_to_utf16(line, b);
        assert_eq!(u, line[..b].chars().map(|c| c.len_utf16() as u64).sum::<u64>());
        assert_eq!(utf16_to_byte(line, u), b);
        assert_eq!(word_at(line, utf16_to_byte(line, u + 3)), Some((b, b + "make_point".len())));
        assert_eq!(word_at(line, b + "make_point".len()), Some((b, b + "make_point".len())), "cursor just after the word");
        assert_eq!(utf16_to_byte(line, 999), line.len());
    }

    #[test]
    fn words_qualifiers_and_uris() {
        assert_eq!(find_word("make_point(make_points) make_point", "make_point"), vec![0, 24]);
        assert_eq!(qualifier("    Point."), Some("Point"));
        assert_eq!(qualifier("a::Point::"), Some("Point"));
        assert_eq!(qualifier("x = "), None);
        let p = if cfg!(windows) { PathBuf::from(r"C:\a b\é.rs") } else { PathBuf::from("/a b/é.rs") };
        let uri = path_to_uri(&p);
        assert!(uri.contains("a%20b/%C3%A9.rs"), "{uri}");
        assert_eq!(uri_to_path(&uri).unwrap(), PathBuf::from(p.to_string_lossy().replace('\\', "/")));
    }

    #[test]
    fn a_damaged_index_is_replaced_under_the_readers() {
        let root = std::env::temp_dir().join(format!("scrutate_lsp_damaged_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let shared: Shared = Arc::new(Mutex::new(Some(crate::open_updated(&root, false).unwrap().0)));
        let db = root.join(".scrutate/index.db");
        std::fs::write(&db, vec![0xA5u8; std::fs::metadata(&db).unwrap().len() as usize]).unwrap();
        replace_damaged(&shared, &root);
        let hits = lock(&shared).as_ref().expect("an index after the rebuild").find_symbol("make_point", 5).unwrap();
        assert_eq!(hits.len(), 1, "the rebuilt index answers");
        drop(shared);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn framing() {
        let raw = b"Content-Length: 2\r\nContent-Type: x\r\n\r\n{}Content-Length: 3\r\n\r\n[1]";
        let mut r = std::io::BufReader::new(&raw[..]);
        assert_eq!(read_message(&mut r).unwrap().unwrap(), b"{}");
        assert_eq!(read_message(&mut r).unwrap().unwrap(), b"[1]");
        assert!(read_message(&mut r).unwrap().is_none());
    }
}
