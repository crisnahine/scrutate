//! MCP server over stdio (JSON-RPC 2.0, newline-delimited), exposing the search tools to Claude Code
//! and other MCP clients. No SDK: the protocol surface a tools-only server needs is small, and a
//! dependency-free implementation is one less thing to break over the years.
//!
//! Two eras of the protocol are served on the same pipe. A client that opens with `initialize`
//! gets the handshake revisions. A request that names its protocol version in `_meta` is served
//! by revision 2026-07-28, which has no handshake: every result then carries `resultType`, and
//! lists say how long they may be cached.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::{json, Value};

use crate::ops;
use crate::Repo;

/// Revisions that open with the `initialize` handshake.
const HANDSHAKE_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
/// The revision without a handshake.
const STATELESS_VERSION: &str = "2026-07-28";
const VERSION_KEY: &str = "io.modelcontextprotocol/protocolVersion";
const SERVER_INFO_KEY: &str = "io.modelcontextprotocol/serverInfo";
/// How long a client may keep a list, in milliseconds: the tools change only with the binary.
const LIST_TTL_MS: u64 = 3_600_000;

/// What a client shows its model before any tool is loaded; the first sentences stand alone.
const INSTRUCTIONS: &str = "scrutate searches the code of this repository from a local index. \
Start with search_code for a question in words or a name; use grep_code for an exact pattern, find_symbol for a definition, \
references for every use of a name, callers and callees to follow calls, outline and read_code to read a file in pieces \
or one whole definition. \
Every tool only reads, except reindex, which refreshes the index (a cache under .scrutate/). \
Results are capped; a tool says when it stopped early and which argument raises the limit. \
Each result begins with its location (path:start-end). Pass that location wherever a tool takes a symbol name \
to mean that one definition: read_code symbol, find_symbol, callers, callees, similar_code, references def. \
A result of kind window is a stretch of lines outside every definition: callees and similar_code take its location, \
and read_code reads it by path and lines; find_symbol, callers and references def answer that no symbol is there.";

/// The tools, each with the hints clients use to decide whether a call needs approval and, with
/// `structured`, the schema of the JSON its results carry.
pub fn tools(structured: bool, max_bytes: usize) -> Value {
    let mut tools = tool_table();
    for tool in tools.as_array_mut().into_iter().flatten() {
        let name = tool["name"].as_str().unwrap_or("").to_string();
        let read_only = ops::read_only(&name);
        tool["annotations"] = json!({"readOnlyHint": read_only, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
        if structured {
            tool["outputSchema"] = ops::output_schema(&name);
        }
        if read_only {
            let properties = &mut tool["inputSchema"]["properties"];
            properties["max_bytes"] = json!({"type": "integer", "default": max_bytes, "description": "size limit of the answer"});
            properties["offset"] =
                json!({"type": "integer", "default": 0, "description": "items to skip, to continue an answer that said more were not shown"});
        }
    }
    tools
}

fn tool_table() -> Value {
    json!([
        {"name": "search_code",
         "description": "Search the codebase by meaning or by name. Returns the best matching functions/classes with file:line and a snippet. `query` may be natural language (\"where do we refresh expired tokens\") or identifiers (`ClientSession.call_tool`). To improve recall put synonyms and likely identifier names in `alt_queries` (e.g. [\"renew credentials\", \"refresh_token\"], at most 10); results of all phrasings are fused.",
         "inputSchema": {"type": "object", "properties": {
             "query": {"type": "string"},
             "alt_queries": {"type": ["array", "null"], "items": {"type": "string"}, "default": null},
             "k": {"type": "integer", "default": ops::SEARCH_K}}, "required": ["query"]}},
        {"name": "grep_code", "description": "Exact search of the files the index holds: hidden ones such as .github/ included; ignored files, .git/ and dependency or cache directories such as node_modules/ not. Matching lines are grouped by the enclosing function/class. `mode` identifier finds every casing of a name (refreshToken, refresh_token, REFRESH_TOKEN); fuzzy allows `max_edits` typos; `path` keeps one file or directory and `globs` keep files by name (`!` excludes).",
         "inputSchema": {"type": "object", "properties": {
             "pattern": {"type": "string"},
             "mode": {"type": "string", "enum": ["regex", "literal", "identifier", "fuzzy", "fuzzy_regex", "pcre2"], "default": "regex"},
             "case": {"type": "string", "enum": ["sensitive", "insensitive", "smart"], "default": "sensitive"},
             "word": {"type": "boolean", "default": false, "description": "whole words only"},
             "max_edits": {"type": "integer", "default": 1, "description": "edits a fuzzy match may need"},
             "path": {"type": ["string", "null"], "default": null, "description": "a file or directory of the repository to search under"},
             "globs": {"type": ["array", "null"], "items": {"type": "string"}, "default": null, "description": "keep the files these globs match, as grep -g (`!` excludes)"},
             "max_results": {"type": "integer", "default": ops::GREP_MAX}}, "required": ["pattern"]}},
        {"name": "find_symbol", "description": "Where is this function/class/method defined? (`Class.method` works, as do `Class::method`, `Class#method` and a location from another answer).",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::SYMBOL_LIMIT}}, "required": ["name"]}},
        {"name": "callers", "description": "Which functions call this function?",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::CALLERS_LIMIT}}, "required": ["name"]}},
        {"name": "callees", "description": "Which functions does this function call?",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::CALLEES_LIMIT}}, "required": ["name"]}},
        {"name": "read_code", "description": "Read a line range of a file (`path`, `start`, `end`), or one whole definition: `symbol` is a name, `Class.method`, or a location from another answer (`src/a.py:12-40`). A name with several definitions returns each; `path` then keeps the ones in that file.",
         "inputSchema": {"type": "object", "properties": {
             "path": {"type": ["string", "null"], "default": null}, "symbol": {"type": ["string", "null"], "default": null},
             "start": {"type": "integer", "default": 1}, "end": {"type": ["integer", "null"], "default": null}}}},
        {"name": "outline", "description": "Symbols of a file in source order (kind, name, line range).",
         "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}},
        {"name": "dependencies", "description": "Modules a file imports, resolved to repository files where possible.",
         "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}},
        {"name": "dependents", "description": "Files that import a module or repository file (reverse dependencies).",
         "inputSchema": {"type": "object", "properties": {"target": {"type": "string", "description": "module name as imported, or a repository file path"}}, "required": ["target"]}},
        {"name": "references", "description": "Every use of a name, each resolved to the definition it refers to (same-named definitions in other modules are kept apart). `def` (`path` or `path:line`) keeps one definition; `textual` lists every occurrence of the name grouped by the enclosing function/class instead.",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "max_results": {"type": "integer", "default": ops::REFS_MAX},
             "def": {"type": ["string", "null"], "default": null}, "textual": {"type": "boolean", "default": false}}, "required": ["name"]}},
        {"name": "similar_code", "description": "Functions similar to a given symbol or location (`name`, `Class.method` or `path:line`): find duplicated logic or existing implementations.",
         "inputSchema": {"type": "object", "properties": {"target": {"type": "string"}, "k": {"type": "integer", "default": ops::SIMILAR_K}}, "required": ["target"]}},
        {"name": "duplicates", "description": "Duplicate and near-duplicate functions across the repository.",
         "inputSchema": {"type": "object", "properties": {"min_tokens": {"type": "integer", "default": ops::DUPES_MIN_TOKENS}, "threshold": {"type": "number", "default": ops::DUPES_THRESHOLD}, "limit": {"type": "integer", "default": ops::DUPES_LIMIT}}}},
        {"name": "structural_search", "description": "Run a tree-sitter query (S-expression pattern with @captures) over all files of one language, e.g. lang=python query=\"(class_definition name: (identifier) @cls)\".",
         "inputSchema": {"type": "object", "properties": {"lang": {"type": "string"}, "query": {"type": "string"}, "limit": {"type": "integer", "default": ops::TSQUERY_LIMIT}}, "required": ["lang", "query"]}},
        {"name": "reindex", "description": "Re-scan for changed files (searches already do this automatically). `force` reads every file again, also one whose size, modification time and change time did not change.",
         "inputSchema": {"type": "object", "properties": {"force": {"type": "boolean", "default": false}}}}
    ])
}

/// What a tool call returns: text for the model, the same answer as JSON for a client that
/// reads structured results, and whether it failed.
pub struct Called {
    pub text: String,
    pub structured: Value,
    pub is_error: bool,
}

impl Called {
    /// A sentence in place of an answer.
    fn note(text: String, is_error: bool) -> Called {
        let structured = json!({"schema": ops::ANSWER_SCHEMA, "complete": false, "result": null, "text": text});
        Called { text, structured, is_error }
    }
}

/// Run one tool; errors become text for the model.
pub fn call_tool(repo: &mut Repo, name: &str, args: &Value, settings: &Settings) -> Called {
    let max_bytes = args.get("max_bytes").and_then(Value::as_u64).map_or(settings.max_bytes, |n| clamp_bytes(n as usize));
    let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
    let mut scan_and_run = |op: &ops::Op| {
        if !settings.floor {
            return repo.run(op, settings.refresh_interval);
        }
        let mut last = settings.last_scan.lock().unwrap_or_else(|p| p.into_inner());
        if last.is_some_and(|t| t.elapsed().as_secs_f64() < settings.refresh_interval) {
            return repo.run_as_is(op);
        }
        let answer = repo.run(op, 0.0);
        // a scan that could not run (another process holds the write lock) does not start the interval
        if answer.is_ok() && !repo.is_stale() {
            *last = Some(Instant::now());
        }
        answer
    };
    let run = |op: ops::Op| {
        let started = Instant::now();
        loop {
            match scan_and_run(&op) {
                // another process is building the first index: wait for it as for a build of our own
                Err(e) if e.is::<crate::index::BeingBuilt>() && started.elapsed() < BUILD_WAIT => std::thread::sleep(Duration::from_millis(200)),
                answer => return answer,
            }
        }
    };
    match ops::Op::from_tool(name, args).and_then(run) {
        Ok(answer) => {
            let text = answer.text_within(max_bytes, offset);
            let mut structured = answer.structured(text.len(), max_bytes, offset);
            if structured["result"].is_null() {
                // a client that shows only the structured form still gets the part that fits
                structured["text"] = json!(text);
            }
            Called { text, structured, is_error: false }
        }
        Err(e) if e.is::<crate::index::BeingBuilt>() => Called::note(STILL_BUILDING.to_string(), false),
        Err(e) => Called::note(format!("error: {e}"), true),
    }
}

/// The answer to a call that came before the first index of the repository is ready.
const STILL_BUILDING: &str =
    "scrutate is still building its index of this repository (first use, or many files changed). Call the tool again in a little while.";

/// Where messages go: stdout in the server, a list in its tests.
type Wire<'a> = dyn Fn(Value) + Sync + 'a;

/// One line on stdout: a message leaves whole whichever thread writes it.
fn to_stdout(msg: Value) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// `stateless`: the request came under the revision whose results all say what kind they are.
fn respond(send: &Wire, id: &Value, stateless: bool, mut result: Value) {
    if stateless {
        result["resultType"] = json!("complete");
    }
    send(json!({"jsonrpc": "2.0", "id": id, "result": result}));
}

fn respond_err(send: &Wire, id: &Value, code: i64, message: &str, data: Option<Value>) {
    let mut error = json!({"code": code, "message": message});
    if let Some(data) = data {
        error["data"] = data;
    }
    send(json!({"jsonrpc": "2.0", "id": id, "error": error}));
}

/// Tool calls in flight, by request id, each with the flag `notifications/cancelled` sets.
#[derive(Default)]
struct Calls(Mutex<HashMap<String, Arc<AtomicBool>>>);

impl Calls {
    fn map(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<AtomicBool>>> {
        self.0.lock().unwrap_or_else(|p| p.into_inner())
    }
    fn begin(&self, id: &Value) -> Arc<AtomicBool> {
        let cancelled = Arc::new(AtomicBool::new(false));
        self.map().insert(id.to_string(), Arc::clone(&cancelled));
        cancelled
    }
    fn cancel(&self, id: &Value) {
        if let Some(cancelled) = self.map().get(&id.to_string()) {
            cancelled.store(true, Ordering::Relaxed);
        }
    }
}

struct Call {
    id: Value,
    name: String,
    args: Value,
    stateless: bool,
    /// send the answer as JSON too (`serve --structured`)
    structured: bool,
    /// the client's token for progress reports; null when it did not ask for any
    progress: Value,
    cancelled: Arc<AtomicBool>,
}

/// How long a tool call waits for an index that is still being built before it answers that it
/// is (well under the 60 s after which Codex gives a call up). A client that sent a progress
/// token hears every `PROGRESS_SECONDS` that the build goes on, and gets the real answer.
const BUILD_WAIT: Duration = Duration::from_secs(20);
const PROGRESS_SECONDS: u64 = 2;
/// Calls beyond this many in flight run on the thread that reads requests.
const MAX_CALLS: usize = 32;

/// One tool call from its first wait to its answer. A cancelled call sends nothing more: it
/// stops waiting for the index or for its turn, and an answer already being computed is dropped.
fn run_call(send: &Wire, repo: &Mutex<Repo>, building: &AtomicBool, build_failed: &AtomicBool, calls: &Calls, settings: &Settings, call: Call) {
    let Call { id, name, args, stateless, structured, progress, cancelled } = call;
    let (started, mut reported) = (Instant::now(), 0);
    let gone = || cancelled.load(Ordering::Relaxed);
    while building.load(Ordering::Acquire) && !gone() && (!progress.is_null() || started.elapsed() < BUILD_WAIT) {
        std::thread::sleep(Duration::from_millis(20));
        let seconds = started.elapsed().as_secs();
        if !progress.is_null() && seconds >= reported + PROGRESS_SECONDS {
            reported = seconds;
            let params = json!({"progressToken": progress, "progress": seconds, "message": "building the index of this repository"});
            send(json!({"jsonrpc": "2.0", "method": "notifications/progress", "params": params}));
        }
    }
    let called = if gone() {
        None
    } else if building.load(Ordering::Acquire) {
        Some(Called::note(STILL_BUILDING.to_string(), false))
    } else {
        // a panic in an earlier call poisons the lock; the repository behind it is whole
        let mut repo = repo.lock().unwrap_or_else(|p| p.into_inner());
        // a panic inside one tool call must not take the server down: it becomes a tool error
        (!gone()).then(|| {
            repo.set_cancel(Arc::clone(&cancelled));
            let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call_tool(&mut repo, &name, &args, settings)));
            if repo.take_updated() {
                // an update went through, so a later failure is not the first build's
                build_failed.store(false, Ordering::Release);
            }
            let c = run.unwrap_or_else(|_| {
                let text = if build_failed.load(Ordering::Acquire) {
                    "error: the index could not be built: internal failure while indexing (a bug: please report it, with the server's stderr); `scrutate index --force` in the repository may clear it, and the server keeps running".to_string()
                } else {
                    format!("error: internal failure while running {name} (a bug: please report it); the server keeps running")
                };
                Called::note(text, true)
            });
            repo.set_cancel(Arc::default());
            c
        })
    };
    calls.map().remove(&id.to_string());
    if let Some(c) = called.filter(|_| !gone()) {
        let mut result = json!({"content": [{"type": "text", "text": c.text}], "isError": c.is_error});
        if structured {
            result["structuredContent"] = c.structured;
        }
        respond(send, &id, stateless, result);
    }
}

/// Serve until stdin closes. The first indexing runs in the background so a client is answered
/// at once; tool calls that arrive meanwhile wait for it (`BUILD_WAIT`). Each call runs on a
/// thread of its own, so the reader still sees a `notifications/cancelled` for it.
///
/// `structured`: every tool declares an `outputSchema` and every result carries the answer as
/// JSON beside its text. Off unless asked for: Claude Code (2.1.322) shows its model that JSON in
/// place of the text, which has the snippets and says how to page.
pub fn serve(repo: Arc<Mutex<Repo>>, settings: Settings, version: &str, structured: bool) -> Result<()> {
    let (building, build_failed) = (Arc::new(AtomicBool::new(true)), Arc::new(AtomicBool::new(false)));
    {
        let (repo, building, build_failed) = (Arc::clone(&repo), Arc::clone(&building), Arc::clone(&build_failed));
        std::thread::Builder::new().name("scrutate-index".into()).spawn(move || {
            // a panic must still end the wait of every call, and of the exit
            let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| repo.lock().map(|mut g| g.update(false))));
            match built {
                Ok(Ok(Err(e))) => eprintln!("[scrutate] indexing failed: {e:#}"),
                Err(_) => {
                    build_failed.store(true, Ordering::Release);
                    eprintln!("[scrutate] indexing failed: internal failure (a bug: please report it); each tool call tries again");
                }
                _ => {}
            }
            building.store(false, Ordering::Release);
        })?;
    }
    let calls = Calls::default();
    let server = Server { repo: &repo, building: &building, build_failed: &build_failed, calls: &calls, settings: &settings, version, structured };
    let (server, send) = (&server, &to_stdout as &Wire);
    std::thread::scope(|scope| -> Result<()> {
        let mut stdin = std::io::stdin().lock();
        let mut buf = Vec::new();
        loop {
            buf.clear();
            if stdin.read_until(b'\n', &mut buf)? == 0 {
                break;
            }
            let Ok(line) = std::str::from_utf8(&buf) else {
                respond_err(send, &Value::Null, -32700, "parse error: not UTF-8", None);
                continue;
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(Value::Array(batch)) if !batch.is_empty() => {
                    scope.spawn(move || {
                        let replies = Mutex::new(Vec::new());
                        // progress notifications go out at once; the batch's answers go out together
                        let sink = |m: Value| {
                            if m.get("method").is_some() {
                                send(m)
                            } else {
                                replies.lock().unwrap_or_else(|p| p.into_inner()).push(m)
                            }
                        };
                        std::thread::scope(|inner| batch.into_iter().for_each(|m| server.handle(inner, &sink, m)));
                        let replies = replies.into_inner().unwrap_or_else(|p| p.into_inner());
                        if !replies.is_empty() {
                            send(Value::Array(replies));
                        }
                    });
                }
                Ok(msg) => server.handle(scope, send, msg),
                Err(_) => respond_err(send, &Value::Null, -32700, "parse error", None),
            }
        }
        Ok(())
    })?;
    // the first build may not hold the lock yet: let it finish before the exit takes the lock
    while building.load(Ordering::Acquire) {
        std::thread::sleep(Duration::from_millis(20));
    }
    // held until the process exits, so no index writer is inside a transaction the exit would cut
    std::mem::forget(repo.lock().unwrap_or_else(|p| p.into_inner()));
    Ok(())
}

/// The answer size and refresh policy `serve` was started with.
pub struct Settings {
    /// answer size of a call that gives no `max_bytes`
    max_bytes: usize,
    refresh_interval: f64,
    /// the interval was given explicitly: no scan sooner, however cheap scans are
    floor: bool,
    last_scan: Mutex<Option<Instant>>,
}

impl Settings {
    pub fn new(max_bytes: Option<usize>, refresh_interval: Option<f64>) -> Settings {
        Settings {
            max_bytes: max_bytes.map_or(ops::AGENT_BYTES, clamp_bytes),
            refresh_interval: refresh_interval.unwrap_or(1.0),
            floor: refresh_interval.is_some(),
            last_scan: Mutex::new(None),
        }
    }
}

fn clamp_bytes(n: usize) -> usize {
    n.clamp(1_000, 1_000_000)
}

/// What every message is handled with.
struct Server<'a> {
    repo: &'a Mutex<Repo>,
    building: &'a AtomicBool,
    build_failed: &'a AtomicBool,
    calls: &'a Calls,
    settings: &'a Settings,
    version: &'a str,
    structured: bool,
}

impl Server<'_> {
    /// Answer one message, alone or a member of a batch, through `send`.
    fn handle<'s, 'e>(&'e self, scope: &'s std::thread::Scope<'s, 'e>, send: &'e Wire<'e>, msg: Value) {
        if !msg.is_object() {
            return respond_err(send, &Value::Null, -32600, "invalid request: not a JSON object", None);
        }
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let Some(method) = msg.get("method").and_then(Value::as_str) else {
            // a client's response to a request is not answered
            if msg.get("method").is_none() && (msg.get("result").is_some() || msg.get("error").is_some()) {
                return;
            }
            return respond_err(send, &id, -32600, "invalid request: no method", None);
        };
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        if msg.get("id").is_none() {
            if method == "notifications/cancelled" {
                self.calls.cancel(&params["requestId"]);
            }
            return;
        }
        let asked = params.get("_meta").and_then(|m| m.get(VERSION_KEY)).and_then(Value::as_str);
        if let Some(other) = asked.filter(|v| *v != STATELESS_VERSION) {
            return respond_err(send, &id, -32022, "Unsupported protocol version", Some(json!({"supported": [STATELESS_VERSION], "requested": other})));
        }
        let stateless = asked.is_some();
        // a list a client may cache, under the revision that asks every list to say so
        let list = |mut result: Value| {
            if stateless {
                result["ttlMs"] = json!(LIST_TTL_MS);
                result["cacheScope"] = json!("public");
            }
            respond(send, &id, stateless, result);
        };
        match method {
            "server/discover" if stateless => list(json!({
                "supportedVersions": [STATELESS_VERSION],
                "capabilities": {"tools": {}},
                "instructions": INSTRUCTIONS,
                "_meta": {SERVER_INFO_KEY: {"name": "scrutate", "version": self.version}},
            })),
            "server/discover" => respond_err(send, &id, -32602, &format!("_meta has no {VERSION_KEY}"), None),
            "initialize" => {
                let latest = HANDSHAKE_VERSIONS[HANDSHAKE_VERSIONS.len() - 1];
                let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or(latest);
                let ver = if HANDSHAKE_VERSIONS.contains(&asked) { asked } else { latest };
                respond(
                    send,
                    &id,
                    false,
                    json!({
                        "protocolVersion": ver,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "scrutate", "version": self.version},
                        "instructions": INSTRUCTIONS,
                    }),
                );
            }
            "ping" => respond(send, &id, stateless, json!({})),
            "tools/list" => list(json!({"tools": tools(self.structured, self.settings.max_bytes)})),
            "resources/list" => list(json!({"resources": []})),
            "resources/templates/list" => list(json!({"resourceTemplates": []})),
            "prompts/list" => list(json!({"prompts": []})),
            "tools/call" => {
                let Some(name) = params.get("name").and_then(Value::as_str) else {
                    return respond_err(send, &id, -32602, "tools/call has no params.name", None);
                };
                if !tool_table().as_array().into_iter().flatten().any(|t| t["name"] == name) {
                    return respond_err(send, &id, -32602, &format!("unknown tool: {name}"), None);
                }
                let call = Call {
                    name: name.to_string(),
                    args: params.get("arguments").cloned().unwrap_or(json!({})),
                    stateless,
                    structured: self.structured,
                    progress: params.get("_meta").and_then(|m| m.get("progressToken")).cloned().unwrap_or(Value::Null),
                    cancelled: self.calls.begin(&id),
                    id,
                };
                let (repo, building, build_failed, calls, settings) = (self.repo, self.building, self.build_failed, self.calls, self.settings);
                let run = move || run_call(send, repo, building, build_failed, calls, settings, call);
                if calls.map().len() > MAX_CALLS {
                    run();
                } else {
                    scope.spawn(run);
                }
            }
            _ => respond_err(send, &id, -32601, &format!("method not found: {method}"), None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A repository with one function, its index built, and the messages a call sends.
    fn fixture(name: &str) -> (std::path::PathBuf, Mutex<Repo>, Mutex<Vec<Value>>) {
        let root = std::env::temp_dir().join(format!("scrutate_mcp_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let mut repo = Repo::open(&root).unwrap();
        repo.update(false).unwrap();
        (root, Mutex::new(repo), Mutex::new(Vec::new()))
    }

    fn call(calls: &Calls, id: i64, progress: Value) -> Call {
        let id = json!(id);
        Call { name: "find_symbol".into(), args: json!({"name": "make_point"}), stateless: true, structured: true, progress, cancelled: calls.begin(&id), id }
    }

    #[test]
    fn a_cancelled_call_stops_the_reindex_it_runs() {
        let (root, repo, _) = fixture("cancel_reindex");
        let mut repo = repo.into_inner().unwrap();
        let settings = Settings::new(None, Some(0.0));
        // what `notifications/cancelled` sets for the call in flight
        repo.set_cancel(Arc::new(AtomicBool::new(true)));
        let called = call_tool(&mut repo, "reindex", &json!({"force": true}), &settings);
        assert!(called.is_error && called.text.contains("cancelled"), "{}", called.text);
        repo.set_cancel(Arc::default());
        let called = call_tool(&mut repo, "find_symbol", &json!({"name": "make_point"}), &settings);
        assert!(called.text.contains("src/geo.py"), "the index is as it was: {}", called.text);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_call_waits_for_another_process_that_builds_the_first_index() {
        let root = std::env::temp_dir().join(format!("scrutate_mcp_other_build_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/geo.py"), "def make_point():\n    return 1\n").unwrap();
        let mut repo = Repo::open(&root).unwrap();
        // what another process holds while it builds the first index
        let other = rusqlite::Connection::open(root.join(crate::files::INDEX_DIR).join("index.db")).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(600));
            other.execute_batch("ROLLBACK").unwrap();
        });
        let settings = Settings::new(None, Some(3600.0));
        let called = call_tool(&mut repo, "find_symbol", &json!({"name": "make_point"}), &settings);
        release.join().unwrap();
        assert!(called.text.contains("src/geo.py"), "{}", called.text);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_a_call_that_updates_the_index_clears_the_failed_first_build() {
        let (root, repo, sent) = fixture("build_failed_reset");
        // the state a first build that panicked leaves
        repo.lock().unwrap().take_updated();
        let (building, build_failed, calls) = (AtomicBool::new(false), AtomicBool::new(true), Calls::default());
        let send = |m: Value| sent.lock().unwrap().push(m);
        let settings = Settings::new(None, Some(0.0));
        // neither a call with a bad argument nor a read of a file by its path looks at the index
        let read = Call { name: "read_code".into(), args: json!({"path": "src/geo.py"}), ..call(&calls, 5, Value::Null) };
        let bad = Call { args: json!({"name": 7}), ..call(&calls, 6, Value::Null) };
        for c in [read, bad] {
            run_call(&send, &repo, &building, &build_failed, &calls, &settings, c);
            assert!(build_failed.load(Ordering::Acquire));
        }
        run_call(&send, &repo, &building, &build_failed, &calls, &settings, call(&calls, 7, Value::Null));
        let sent = sent.lock().unwrap();
        assert_eq!((&sent[0]["result"]["isError"], &sent[1]["result"]["isError"], &sent[2]["result"]["isError"]), (&json!(false), &json!(true), &json!(false)));
        assert!(!build_failed.load(Ordering::Acquire));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_call_with_a_progress_token_reports_while_the_index_is_built_and_then_answers() {
        let (root, repo, sent) = fixture("progress");
        let (building, calls) = (AtomicBool::new(true), Calls::default());
        let send = |m: Value| sent.lock().unwrap().push(m);
        std::thread::scope(|s| {
            let job =
                s.spawn(|| run_call(&send, &repo, &building, &AtomicBool::new(false), &calls, &Settings::new(None, Some(0.0)), call(&calls, 7, json!("tok"))));
            std::thread::sleep(Duration::from_millis(2300));
            assert!(!job.is_finished(), "the call waits for the index as long as it reports progress");
            building.store(false, Ordering::Release);
        });
        let sent = sent.lock().unwrap().clone();
        let (last, progress) = sent.split_last().unwrap();
        assert!(!progress.is_empty(), "{sent:?}");
        for p in progress {
            assert_eq!(p["method"], "notifications/progress");
            assert_eq!(p["params"]["progressToken"], "tok");
            assert!(p["params"]["progress"].as_u64().unwrap() >= PROGRESS_SECONDS);
        }
        assert_eq!(last["id"], 7);
        assert_eq!(last["result"]["resultType"], "complete");
        assert!(last["result"]["content"][0]["text"].as_str().unwrap().starts_with("src/geo.py:1-2"), "{last}");
        assert_eq!(last["result"]["structuredContent"]["result"][0]["id"], "src/geo.py:1-2");
        assert!(calls.map().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn a_cancelled_call_sends_nothing() {
        let (root, repo, sent) = fixture("cancel");
        let (building, calls) = (AtomicBool::new(true), Calls::default());
        let send = |m: Value| sent.lock().unwrap().push(m);
        std::thread::scope(|s| {
            // waiting for the index: the notice ends the wait
            let waiting = call(&calls, 1, Value::Null);
            let job = s.spawn(|| run_call(&send, &repo, &building, &AtomicBool::new(false), &calls, &Settings::new(None, Some(0.0)), waiting));
            calls.cancel(&json!(1));
            job.join().unwrap();
            building.store(false, Ordering::Release);
            // waiting for its turn: it gives the turn up when it comes
            let turn = repo.lock().unwrap();
            let queued = call(&calls, 2, Value::Null);
            let job = s.spawn(|| run_call(&send, &repo, &building, &AtomicBool::new(false), &calls, &Settings::new(None, Some(0.0)), queued));
            calls.cancel(&json!(2));
            drop(turn);
            job.join().unwrap();
            calls.cancel(&json!(99)); // a notice for a request that is not there is ignored
        });
        assert_eq!(*sent.lock().unwrap(), Vec::<Value>::new());
        assert!(calls.map().is_empty());
        let _ = std::fs::remove_dir_all(root);
    }
}
