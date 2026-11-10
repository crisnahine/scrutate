//! MCP server over stdio (JSON-RPC 2.0, newline-delimited), exposing the search tools to Claude Code
//! and other MCP clients. No SDK: the protocol surface a tools-only server needs is small, and a
//! dependency-free implementation is one less thing to break over the years.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};

use crate::ops;
use crate::search::Searcher;

const KNOWN_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];

/// What a client shows its model before any tool is loaded; the first sentences stand alone.
const INSTRUCTIONS: &str = "scrutate searches the code of this repository from a local index. \
Start with search_code for a question in words or a name; use grep_code for an exact pattern, find_symbol for a definition, \
references for every use of a name, callers and callees to follow calls, outline and read_code to read a file in pieces. \
Every tool only reads, except reindex, which refreshes the index (a cache under .scrutate/). \
Results are capped; a tool says when it stopped early and which argument raises the limit.";

/// The tools, each with the hints clients use to decide whether a call needs approval.
pub fn tools() -> Value {
    let mut tools = tool_table();
    for tool in tools.as_array_mut().into_iter().flatten() {
        let read_only = ops::read_only(tool["name"].as_str().unwrap_or(""));
        tool["annotations"] = json!({"readOnlyHint": read_only, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false});
    }
    tools
}

fn tool_table() -> Value {
    json!([
        {"name": "search_code",
         "description": "Search the codebase by meaning or by name. Returns the best matching functions/classes with file:line and a snippet. `query` may be natural language (\"where do we refresh expired tokens\") or identifiers (`ClientSession.call_tool`). To improve recall put synonyms and likely identifier names in `alt_queries` (e.g. [\"renew credentials\", \"refresh_token\"]); results of all phrasings are fused.",
         "inputSchema": {"type": "object", "properties": {
             "query": {"type": "string"},
             "alt_queries": {"type": ["array", "null"], "items": {"type": "string"}, "default": null},
             "k": {"type": "integer", "default": ops::SEARCH_K}}, "required": ["query"]}},
        {"name": "grep_code", "description": "Exact regex search of the repository's files (case-sensitive; (?i) in the pattern for any case), matching lines grouped by the enclosing function/class.",
         "inputSchema": {"type": "object", "properties": {"pattern": {"type": "string"}, "max_results": {"type": "integer", "default": ops::GREP_MAX}}, "required": ["pattern"]}},
        {"name": "find_symbol", "description": "Where is this function/class/method defined? (`Class.method` works).",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::SYMBOL_LIMIT}}, "required": ["name"]}},
        {"name": "callers", "description": "Which functions call this function?",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::CALLERS_LIMIT}}, "required": ["name"]}},
        {"name": "callees", "description": "Which functions does this function call?",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "limit": {"type": "integer", "default": ops::CALLEES_LIMIT}}, "required": ["name"]}},
        {"name": "read_code", "description": "Read a line range from a file in the repo.",
         "inputSchema": {"type": "object", "properties": {
             "path": {"type": "string"}, "start": {"type": "integer", "default": 1},
             "end": {"type": ["integer", "null"], "default": null}}, "required": ["path"]}},
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
        {"name": "reindex", "description": "Force a re-scan for changed files (searches already do this automatically).",
         "inputSchema": {"type": "object", "properties": {}}}
    ])
}

/// Run one tool; errors become text for the model.
pub fn call_tool(s: &mut Searcher, name: &str, args: &Value, refresh_interval: f64) -> (String, bool) {
    match ops::Op::from_tool(name, args).and_then(|op| ops::run(s, &op, Some(refresh_interval))) {
        Ok(answer) => (answer.text(), false),
        Err(e) => (format!("error: {e}"), true),
    }
}

fn respond(out: &mut impl Write, id: &Value, result: Value) {
    let msg = json!({"jsonrpc": "2.0", "id": id, "result": result});
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

fn respond_err(out: &mut impl Write, id: &Value, code: i64, message: &str) {
    let msg = json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}});
    let _ = writeln!(out, "{msg}");
    let _ = out.flush();
}

/// Serve until stdin closes. The first indexing runs in the background so the handshake is instant;
/// tool calls that arrive meanwhile wait for it.
pub fn serve(searcher: Arc<Mutex<Searcher>>, refresh_interval: f64, version: &str) -> Result<()> {
    {
        let s = Arc::clone(&searcher);
        std::thread::Builder::new().name("scrutate-index".into()).spawn(move || {
            if let Ok(mut g) = s.lock() {
                let _ = g.ix.refresh(0.0);
            }
        })?;
    }
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => {
                respond_err(&mut out, &Value::Null, -32700, "parse error");
                continue;
            }
        };
        let id = msg.get("id").cloned().unwrap_or(Value::Null);
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let is_notification = msg.get("id").is_none();
        match method {
            "initialize" => {
                let asked = params.get("protocolVersion").and_then(|v| v.as_str()).unwrap_or("2025-06-18");
                let ver = if KNOWN_VERSIONS.contains(&asked) { asked } else { "2025-06-18" };
                respond(
                    &mut out,
                    &id,
                    json!({
                        "protocolVersion": ver,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {"name": "scrutate", "version": version},
                        "instructions": INSTRUCTIONS,
                    }),
                );
            }
            "notifications/initialized" | "notifications/cancelled" | "notifications/roots/list_changed" => {}
            "ping" => respond(&mut out, &id, json!({})),
            "tools/list" => respond(&mut out, &id, json!({"tools": tools()})),
            "resources/list" => respond(&mut out, &id, json!({"resources": []})),
            "resources/templates/list" => respond(&mut out, &id, json!({"resourceTemplates": []})),
            "prompts/list" => respond(&mut out, &id, json!({"prompts": []})),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("").to_string();
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                // a panic inside one tool call must not take the server down: report it as a tool
                // error and keep serving (the mutex is poisoned afterwards; clear it)
                let (text, is_error) = match searcher.lock().or_else(|p| Ok::<_, ()>(p.into_inner())) {
                    Ok(mut s) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| call_tool(&mut s, &name, &args, refresh_interval))) {
                        Ok(r) => r,
                        Err(_) => (format!("error: internal failure while running {name} (a bug: please report it); the server keeps running"), true),
                    },
                    Err(()) => ("error: engine unavailable".to_string(), true),
                };
                respond(&mut out, &id, json!({"content": [{"type": "text", "text": text}], "isError": is_error}));
            }
            _ => {
                if !is_notification {
                    respond_err(&mut out, &id, -32601, &format!("method not found: {method}"));
                }
            }
        }
    }
    Ok(())
}
