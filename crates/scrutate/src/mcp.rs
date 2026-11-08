//! MCP server over stdio (JSON-RPC 2.0, newline-delimited), exposing the search tools to Claude Code
//! and other MCP clients. No SDK: the protocol surface a tools-only server needs is small, and a
//! dependency-free implementation is one less thing to break over the years.

use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{json, Value};

use crate::fmt::fmt_hits;
use crate::search::Searcher;
use crate::text::query_words;

const KNOWN_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];

pub fn tools() -> Value {
    json!([
        {"name": "search_code",
         "description": "Search the codebase by meaning or by name. Returns the best matching functions/classes with file:line and a snippet. `query` may be natural language (\"where do we refresh expired tokens\") or identifiers (`ClientSession.call_tool`). To improve recall put synonyms and likely identifier names in `alt_queries` (e.g. [\"renew credentials\", \"refresh_token\"]); results of all phrasings are fused.",
         "inputSchema": {"type": "object", "properties": {
             "query": {"type": "string"},
             "alt_queries": {"type": ["array", "null"], "items": {"type": "string"}, "default": null},
             "k": {"type": "integer", "default": 8}}, "required": ["query"]}},
        {"name": "grep_code", "description": "Exact regex search, results grouped by the enclosing function/class.",
         "inputSchema": {"type": "object", "properties": {"pattern": {"type": "string"}}, "required": ["pattern"]}},
        {"name": "find_symbol", "description": "Where is this function/class/method defined? (`Class.method` works).",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}},
        {"name": "callers", "description": "Which functions call this function?",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}},
        {"name": "callees", "description": "Which functions does this function call?",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}},
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
        {"name": "references", "description": "Every reference to a name: definitions, callers and identifier occurrences grouped by the enclosing function/class.",
         "inputSchema": {"type": "object", "properties": {"name": {"type": "string"}, "max_results": {"type": "integer", "default": 100}}, "required": ["name"]}},
        {"name": "similar_code", "description": "Functions similar to a given symbol or location (`name`, `Class.method` or `path:line`): find duplicated logic or existing implementations.",
         "inputSchema": {"type": "object", "properties": {"target": {"type": "string"}, "k": {"type": "integer", "default": 10}}, "required": ["target"]}},
        {"name": "duplicates", "description": "Duplicate and near-duplicate functions across the repository.",
         "inputSchema": {"type": "object", "properties": {"min_tokens": {"type": "integer", "default": 30}, "threshold": {"type": "number", "default": 0.75}, "limit": {"type": "integer", "default": 30}}}},
        {"name": "structural_search", "description": "Run a tree-sitter query (S-expression pattern with @captures) over all files of one language, e.g. lang=python query=\"(class_definition name: (identifier) @cls)\".",
         "inputSchema": {"type": "object", "properties": {"lang": {"type": "string"}, "query": {"type": "string"}, "limit": {"type": "integer", "default": 200}}, "required": ["lang", "query"]}},
        {"name": "reindex", "description": "Force a re-scan for changed files (searches already do this automatically).",
         "inputSchema": {"type": "object", "properties": {}}}
    ])
}

pub fn run_search(s: &mut Searcher, query: &str, alt: &[String], k: usize, why: bool) -> Result<String> {
    let mut qs: Vec<String> = vec![query.to_string()];
    qs.extend(alt.iter().filter(|a| !a.is_empty() && a.as_str() != query).cloned());
    let hits = if qs.len() == 1 { s.search(query, k, true)? } else { s.search_many(&qs, k, true)? };
    let words: Vec<String> = qs.iter().flat_map(|q| query_words(q)).collect();
    Ok(fmt_hits(&hits, &words, true, why))
}

/// Run one tool; errors become text for the model.
pub fn call_tool(s: &mut Searcher, name: &str, args: &Value, refresh_interval: f64) -> (String, bool) {
    let res: Result<String> = (|| {
        if name != "read_code" && name != "reindex" {
            s.ix.refresh(refresh_interval)?; // pick up files edited since the previous call
        }
        let str_arg = |k: &str| args.get(k).and_then(|v| v.as_str()).map(|v| v.to_string());
        Ok(match name {
            "search_code" => {
                let query = str_arg("query").ok_or_else(|| anyhow::anyhow!("missing query"))?;
                let alt: Vec<String> = args
                    .get("alt_queries")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(|s| s.to_string())).collect())
                    .unwrap_or_default();
                let k = args.get("k").and_then(|v| v.as_i64()).unwrap_or(8).clamp(1, 100) as usize;
                run_search(s, &query, &alt, k, false)?
            }
            "grep_code" => fmt_hits(&s.grep(&str_arg("pattern").ok_or_else(|| anyhow::anyhow!("missing pattern"))?, 30)?, &[], true, false),
            "find_symbol" => fmt_hits(&s.find_symbol(&str_arg("name").ok_or_else(|| anyhow::anyhow!("missing name"))?, 20)?, &[], true, false),
            "callers" => fmt_hits(&s.callers(&str_arg("name").ok_or_else(|| anyhow::anyhow!("missing name"))?, 30)?, &[], false, false),
            "callees" => {
                let (res, ext) = s.callees(&str_arg("name").ok_or_else(|| anyhow::anyhow!("missing name"))?, 40)?;
                let mut out = fmt_hits(&res, &[], false, false);
                if !ext.is_empty() {
                    out.push_str("\nexternal: ");
                    out.push_str(&ext.iter().take(40).cloned().collect::<Vec<_>>().join(", "));
                }
                out
            }
            "read_code" => {
                let path = str_arg("path").ok_or_else(|| anyhow::anyhow!("missing path"))?;
                let start = args.get("start").and_then(|v| v.as_i64()).unwrap_or(1);
                let end = args.get("end").and_then(|v| v.as_i64());
                s.read(&path, start, end, 400)
            }
            "outline" => {
                let path = str_arg("path").ok_or_else(|| anyhow::anyhow!("missing path"))?;
                let rows = crate::intel::outline(&s.ix, &path)?;
                if rows.is_empty() {
                    format!("no symbols in {path} (not indexed or no definitions)")
                } else {
                    rows.iter()
                        .map(|r| format!("{:>5}-{:<5} {}{:<9} {}", r.start, r.end, "  ".repeat(r.qualname.matches('.').count()), r.kind, r.qualname))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "dependencies" => {
                let path = str_arg("path").ok_or_else(|| anyhow::anyhow!("missing path"))?;
                let deps = crate::intel::deps(&s.ix, &path)?;
                if deps.is_empty() {
                    format!("no imports recorded for {path}")
                } else {
                    deps.iter()
                        .map(|d| format!("{}:{}  {}{}", path, d.line, d.module, d.resolved.as_ref().map(|r| format!("  -> {r}")).unwrap_or_default()))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "dependents" => {
                let target = str_arg("target").ok_or_else(|| anyhow::anyhow!("missing target"))?;
                let rows = crate::intel::rdeps(&s.ix, &target)?;
                if rows.is_empty() {
                    format!("nothing imports {target}")
                } else {
                    rows.iter().map(|(p, l, m)| format!("{p}:{l}  {m}")).collect::<Vec<_>>().join("\n")
                }
            }
            "references" => {
                let name = str_arg("name").ok_or_else(|| anyhow::anyhow!("missing name"))?;
                let max = args.get("max_results").and_then(|v| v.as_u64()).unwrap_or(100).clamp(1, 2000);
                crate::intel::references_text(s, &name, max)?
            }
            "similar_code" => {
                let target = str_arg("target").ok_or_else(|| anyhow::anyhow!("missing target"))?;
                let k = args.get("k").and_then(|v| v.as_i64()).unwrap_or(10).clamp(1, 50) as usize;
                let out = crate::intel::similar(s, &target, k)?;
                out.iter()
                    .map(|(h, cos, jac)| format!("{:.3} cos  {:.2} jaccard  {}  [{}] {}", cos, jac, h.loc(), h.kind, h.qualname))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "duplicates" => {
                let min_tokens = args.get("min_tokens").and_then(|v| v.as_u64()).unwrap_or(30) as usize;
                let threshold = args.get("threshold").and_then(|v| v.as_f64()).unwrap_or(0.75) as f32;
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(30).clamp(1, 500) as usize;
                let groups = crate::intel::dupes(&s.ix, min_tokens, threshold, limit)?;
                let mut out = String::new();
                for g in &groups {
                    out.push_str(&format!("{} ({} tokens, similarity {:.2})\n", g.kind, g.tokens, g.similarity));
                    for (p, q, a, b) in &g.members {
                        out.push_str(&format!("    {p}:{a}-{b}  {q}\n"));
                    }
                }
                if out.is_empty() {
                    "no duplicates found".to_string()
                } else {
                    out
                }
            }
            "structural_search" => {
                let lang = str_arg("lang").ok_or_else(|| anyhow::anyhow!("missing lang"))?;
                let query = str_arg("query").ok_or_else(|| anyhow::anyhow!("missing query"))?;
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(200).clamp(1, 5000) as usize;
                let caps = crate::intel::tsquery(&lang, &query, std::slice::from_ref(&s.ix.root), limit)?;
                if caps.is_empty() {
                    "no matches".to_string()
                } else {
                    caps.iter().map(|c| format!("{}:{}:{}  @{} ({})  {}", c.path, c.line, c.column, c.capture, c.kind, c.text)).collect::<Vec<_>>().join("\n")
                }
            }
            "reindex" => {
                let st = s.ix.refresh(0.0)?.unwrap_or_default();
                format!("{} files changed, {} removed, {} chunks", st.files_changed, st.files_removed, st.chunks)
            }
            _ => anyhow::bail!("unknown tool {name}"),
        })
    })();
    match res {
        Ok(t) => (t, false),
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
