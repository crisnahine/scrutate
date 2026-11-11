//! Every question a front end can ask of the index: its arguments with their defaults and limits,
//! when the index is refreshed first, and a typed answer with one text form and one JSON form.
//! The CLI, the MCP server, `batch` and the Python bindings parse into `Op` and render `Answer`;
//! none of them holds a default, a limit or an output format of its own.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::fmt::{hit_blocks, hits_json};
use crate::index::{ChunkRow, UpdateStats};
use crate::intel::{Capture, Dependency, DupGroup, RefGroup, References};
use std::collections::HashSet;

use crate::fmt::{echo, shown};
use crate::resolve::{Resolved, Resolver};
use crate::search::{Hit, Searcher};
use crate::text::query_words;

/// Version of every JSON record scrutate emits.
pub const SCHEMA: u32 = 1;

pub const SEARCH_K: usize = 8;
/// Alternative phrasings one search fuses at most: each costs a full search, and fusion gains
/// little past a handful.
pub const ALT_QUERIES: usize = 10;
pub const SYMBOL_LIMIT: usize = 20;
pub const CALLERS_LIMIT: usize = 30;
pub const CALLEES_LIMIT: usize = 40;
/// Names of callees outside the repository that an answer lists.
pub const EXTERNAL_CALLEES: usize = 50;
pub const GREP_MAX: u64 = 100;
pub const REFS_MAX: u64 = 200;
pub const SIMILAR_K: usize = 10;
pub const DUPES_MIN_TOKENS: usize = 30;
pub const DUPES_THRESHOLD: f32 = 0.75;
pub const DUPES_LIMIT: usize = 50;
pub const TSQUERY_LIMIT: usize = 1000;
/// Lines one `read` returns at most.
pub const READ_LINES: i64 = 400;
/// Bytes of line text one `read_code` of a line range collects at most; a longer line is cut.
pub const READ_BYTES: usize = 1_000_000;
/// Definitions one `read` of a name returns at most.
pub const READ_DEFINITIONS: usize = 20;
/// Bytes of text an agent gets from one call unless it asks for more: about 6,000 tokens, under
/// the 10,000 at which Codex cuts a tool result and Claude Code warns.
pub const AGENT_BYTES: usize = 24_000;

pub enum Op {
    Search {
        query: String,
        alt: Vec<String>,
        k: usize,
    },
    Grep {
        pattern: String,
        max_results: u64,
    },
    Symbol {
        name: String,
        limit: usize,
    },
    Callers {
        name: String,
        limit: usize,
    },
    Callees {
        name: String,
        limit: usize,
    },
    /// A line range of `path`, or with `symbol` (a name or a location) every definition it names,
    /// whole; `path` then keeps the definitions in that file.
    Read {
        path: Option<String>,
        symbol: Option<String>,
        start: i64,
        end: Option<i64>,
    },
    Outline {
        path: String,
    },
    Deps {
        path: String,
    },
    Rdeps {
        target: String,
    },
    /// `textual`: every occurrence of the name grouped by enclosing symbol, instead of each one
    /// resolved to the definition it refers to. `def` (`path` or `path:line`) keeps one definition.
    Refs {
        name: String,
        max_results: u64,
        textual: bool,
        def: Option<String>,
    },
    Similar {
        target: String,
        k: usize,
    },
    Dupes {
        min_tokens: usize,
        threshold: f32,
        limit: usize,
    },
    Tsquery {
        lang: String,
        query: String,
        limit: usize,
    },
    /// `force`: read every file again, also one whose size, modification and change times are unchanged.
    Reindex {
        force: bool,
    },
}

impl Op {
    /// The operation an MCP tool call or a `batch` line names, defaults filled in and limits
    /// clamped (the caller is a program: an absurd limit is cut, not refused).
    pub fn from_tool(name: &str, args: &Value) -> Result<Op> {
        // a JSON null is an argument left out; any other value of the wrong type is an error
        let arg = |k: &str| args.get(k).filter(|v| !v.is_null());
        let wrong = |k: &str, ty: &str| anyhow!("{k} must be {ty}");
        let opt_text = |k: &str| arg(k).map(|v| v.as_str().map(str::to_string).ok_or_else(|| wrong(k, "a string"))).transpose();
        let text = |k: &str| opt_text(k)?.ok_or_else(|| anyhow!("missing {k}"));
        let natural = |k: &str| arg(k).map(|v| v.as_u64().ok_or_else(|| wrong(k, "a non-negative integer"))).transpose();
        let count = |k: &str, default: u64, max: u64| natural(k).map(|n| n.unwrap_or(default).clamp(1, max));
        let line = |k: &str| arg(k).map(|v| v.as_i64().ok_or_else(|| wrong(k, "an integer (a line number)"))).transpose();
        // read by the server, not here, but checked with the rest
        natural("max_bytes")?;
        natural("offset")?;
        Ok(match name {
            "search_code" => Op::Search {
                query: text("query")?,
                alt: match arg("alt_queries") {
                    None => Vec::new(),
                    Some(v) => v
                        .as_array()
                        .and_then(|a| a.iter().map(|x| x.as_str().map(str::to_string)).collect::<Option<Vec<_>>>())
                        .ok_or_else(|| wrong("alt_queries", "an array of strings"))?
                        .into_iter()
                        .take(ALT_QUERIES)
                        .collect(),
                },
                k: count("k", SEARCH_K as u64, 100)? as usize,
            },
            "grep_code" => Op::Grep { pattern: text("pattern")?, max_results: count("max_results", GREP_MAX, 5000)? },
            "find_symbol" => Op::Symbol { name: text("name")?, limit: count("limit", SYMBOL_LIMIT as u64, 500)? as usize },
            "callers" => Op::Callers { name: text("name")?, limit: count("limit", CALLERS_LIMIT as u64, 500)? as usize },
            "callees" => Op::Callees { name: text("name")?, limit: count("limit", CALLEES_LIMIT as u64, 500)? as usize },
            "read_code" => {
                let (path, symbol) = (opt_text("path")?, opt_text("symbol")?);
                if path.is_none() && symbol.is_none() {
                    bail!("missing path or symbol");
                }
                let (start, end) = (line("start")?.unwrap_or(1), line("end")?);
                if let Some(end) = end.filter(|e| *e < start) {
                    bail!("end ({end}) is before start ({start})");
                }
                Op::Read { path, symbol, start, end }
            }
            "outline" => Op::Outline { path: text("path")? },
            "dependencies" => Op::Deps { path: text("path")? },
            "dependents" => Op::Rdeps { target: text("target")? },
            "references" => {
                let name = text("name")?;
                if name.trim().is_empty() {
                    bail!("name is empty");
                }
                Op::Refs {
                    name,
                    max_results: count("max_results", REFS_MAX, 5000)?,
                    textual: arg("textual").map(|v| v.as_bool().ok_or_else(|| wrong("textual", "a boolean (true or false)"))).transpose()?.unwrap_or(false),
                    def: opt_text("def")?,
                }
            }
            "similar_code" => Op::Similar { target: text("target")?, k: count("k", SIMILAR_K as u64, 50)? as usize },
            "duplicates" => Op::Dupes {
                min_tokens: count("min_tokens", DUPES_MIN_TOKENS as u64, 100_000)? as usize,
                threshold: arg("threshold")
                    .map(|v| v.as_f64().ok_or_else(|| wrong("threshold", "a number")))
                    .transpose()?
                    .map_or(DUPES_THRESHOLD, |t| t as f32),
                limit: count("limit", DUPES_LIMIT as u64, 500)? as usize,
            },
            "structural_search" => Op::Tsquery { lang: text("lang")?, query: text("query")?, limit: count("limit", TSQUERY_LIMIT as u64, 5000)? as usize },
            "reindex" => Op::Reindex {
                force: arg("force").map(|v| v.as_bool().ok_or_else(|| wrong("force", "a boolean (true or false)"))).transpose()?.unwrap_or(false),
            },
            _ => bail!("unknown tool {}", echo(name)),
        })
    }
}

impl Op {
    /// Repository files the arguments name (a path, or the file of a location), whose index
    /// entries an answer depends on even when it cites nothing.
    pub fn named_paths(&self) -> Vec<String> {
        let loc = |n: &str| crate::intel::location(n).map(|(p, _)| p.to_string());
        let plain = |p: &str| p.trim_start_matches("./").to_string();
        match self {
            Op::Symbol { name, .. } | Op::Callers { name, .. } | Op::Callees { name, .. } => loc(name).into_iter().collect(),
            Op::Similar { target, .. } => loc(target).into_iter().collect(),
            Op::Read { path, symbol: Some(symbol), .. } => loc(symbol).into_iter().chain(path.as_deref().map(plain)).collect(),
            Op::Outline { path } | Op::Deps { path } => vec![plain(path)],
            Op::Refs { def: Some(def), .. } => vec![loc(def).unwrap_or_else(|| plain(def))],
            _ => Vec::new(),
        }
    }
}

/// Whether the tool leaves everything as it was (`reindex` rewrites the index, a cache).
pub fn read_only(tool: &str) -> bool {
    tool != "reindex"
}

/// The JSON Schema of what `Answer::structured` returns for one tool: the answer's JSON form in
/// `result`, or `null` there when the answer is too large to send whole.
pub fn output_schema(tool: &str) -> Value {
    let ty = |t: &str| json!({"type": t});
    let obj = |properties: Value, required: &[&str]| json!({"type": "object", "properties": properties, "required": required});
    let list = |items: Value| json!({"type": "array", "items": items});
    // a symbol: where it is, and its name under the key this answer uses
    let span = |name: &str| {
        let mut properties = json!({"id": ty("string"), "path": ty("string"), "start": ty("integer"), "end": ty("integer"), "kind": ty("string")});
        properties[name] = ty("string");
        obj(properties, &["path", "start", "end", "kind", name])
    };
    let result = match tool {
        "search_code" | "find_symbol" | "callers" => list(span("name")),
        "callees" => obj(json!({"resolved": list(span("name")), "external": list(ty("string"))}), &["resolved", "external"]),
        "grep_code" => {
            let line = obj(json!({"line": ty("integer"), "text": ty("string")}), &["line", "text"]);
            let group = obj(json!({"symbol": {"anyOf": [span("name"), ty("null")]}, "path": ty("string"), "lines": list(line)}), &["symbol", "path", "lines"]);
            obj(json!({"groups": list(group), "stopped_at": {"type": ["integer", "null"]}}), &["groups", "stopped_at"])
        }
        "read_code" => obj(
            json!({"text": ty("string"), "start": ty("integer"), "end": ty("integer"), "file_lines": {"type": ["integer", "null"]}, "definitions": list(span("qualname"))}),
            &[],
        ),
        "outline" => list(obj(
            json!({"kind": ty("string"), "name": ty("string"), "qualname": ty("string"), "start": ty("integer"), "end": ty("integer")}),
            &["kind", "name", "qualname", "start", "end"],
        )),
        "dependencies" => list(obj(json!({"line": ty("integer"), "module": ty("string"), "resolved": {"type": ["string", "null"]}}), &["line", "module"])),
        "dependents" => list(obj(json!({"path": ty("string"), "line": ty("integer"), "module": ty("string")}), &["path", "line", "module"])),
        "references" => {
            obj(json!({"name": ty("string"), "definitions": list(span("qualname")), "truncated": ty("boolean")}), &["name", "definitions", "truncated"])
        }
        "similar_code" => {
            let mut hit = span("qualname");
            hit["properties"]["cosine"] = ty("number");
            hit["properties"]["jaccard"] = ty("number");
            list(hit)
        }
        "duplicates" => list(obj(
            json!({"kind": ty("string"), "similarity": ty("number"), "tokens": ty("integer"), "members": ty("array")}),
            &["kind", "similarity", "tokens", "members"],
        )),
        "structural_search" => list(obj(
            json!({"path": ty("string"), "line": ty("integer"), "column": ty("integer"), "capture": ty("string"), "kind": ty("string"), "text": ty("string")}),
            &["path", "line", "column", "capture", "kind", "text"],
        )),
        _ => {
            obj(json!({"files_changed": ty("integer"), "files_removed": ty("integer"), "chunks": ty("integer")}), &["files_changed", "files_removed", "chunks"])
        }
    };
    let stopped = json!({"type": "integer", "description": "present when the answer stopped at this limit and more exist: the text's last line says which argument raises it"});
    let complete =
        json!({"type": "boolean", "description": "false when the answer is larger than max_bytes: result is then null and text holds the part that fits"});
    obj(
        json!({"schema": ty("integer"), "complete": complete, "result": {"anyOf": [result, ty("null")]}, "text": ty("string"), "stopped_at": stopped}),
        &["schema", "complete", "result"],
    )
}

/// Where an answer stopped because it reached its limit, and what to pass to see more.
#[derive(Clone, Copy, Debug)]
pub struct Stop {
    pub at: usize,
    pub how: &'static str,
}

pub const RAISE_LIMIT: &str = "raise limit to see more";

/// `items` cut to `limit`, and where they stopped when there were more (ask for `limit + 1`).
fn cut<T>(mut items: Vec<T>, limit: usize, how: &'static str) -> (Vec<T>, Option<Stop>) {
    if items.len() <= limit {
        return (items, None);
    }
    items.truncate(limit);
    (items, Some(Stop { at: limit, how }))
}

pub enum Answer {
    /// `stopped`: the limit the answer stopped at, when there were more.
    Hits {
        hits: Vec<Hit>,
        words: Vec<String>,
        snippet: bool,
        stopped: Option<Stop>,
    },
    /// `undecided`: callers kept although the resolver could not tell whether they call this
    /// definition or another of the same name. `unchecked`: the ones after them that it was not
    /// asked about.
    Callers {
        hits: Vec<Hit>,
        undecided: usize,
        unchecked: usize,
        stopped: Option<Stop>,
    },
    /// `from`: the files of the definitions whose calls these are.
    Callees {
        resolved: Vec<Hit>,
        external: Vec<String>,
        from: Vec<String>,
        stopped: Option<Stop>,
    },
    /// `stopped_at`: the limit the search stopped at, when it did.
    Grep {
        groups: Vec<RefGroup>,
        stopped_at: Option<u64>,
    },
    /// Numbered lines of `path` from line `start` on, and how many lines the file has (None
    /// for a large file read only up to the lines asked for: more lines follow).
    Read {
        path: String,
        lines: Vec<String>,
        start: i64,
        total: Option<i64>,
    },
    /// Whole definitions, each with its numbered lines.
    Definitions(Vec<(ChunkRow, String)>, Option<Stop>),
    Outline {
        path: String,
        rows: Vec<ChunkRow>,
    },
    Deps {
        path: String,
        deps: Vec<Dependency>,
    },
    /// `unresolved`: imports that resolved to no file and may name `target`.
    Rdeps {
        target: String,
        rows: Vec<(String, i64, String)>,
        unresolved: usize,
    },
    Refs {
        name: String,
        refs: References,
        max_results: u64,
    },
    /// `only`: the one definition asked for with `def`.
    Resolved {
        name: String,
        refs: Resolved,
        only: Option<i64>,
        max_results: u64,
    },
    Similar(Vec<(Hit, f32, f32)>),
    Dupes(Vec<DupGroup>, Option<Stop>),
    Captures(Vec<Capture>, Option<Stop>),
    Reindexed(UpdateStats),
}

/// Run one operation on the index as it is; `Repo::run` brings the index up to date first.
pub fn run(s: &mut Searcher, op: &Op) -> Result<Answer> {
    Ok(match op {
        Op::Search { query, alt, k } => {
            let mut qs = vec![query.clone()];
            qs.extend(alt.iter().filter(|a| !a.is_empty() && *a != query).cloned());
            let hits = if qs.len() == 1 { s.search(query, *k, true)? } else { s.search_many(&qs, *k, true)? };
            Answer::Hits { hits, words: qs.iter().flat_map(|q| query_words(q)).collect(), snippet: true, stopped: None }
        }
        Op::Grep { pattern, max_results } => {
            let o = crate::instant::Options {
                pattern: pattern.clone(),
                mode: crate::instant::Mode::Regex,
                case: crate::instant::Case::Sensitive,
                max_results: Some(*max_results),
                // a capped answer is the same on every call, so paging does not repeat or skip lines
                first_in_path_order: true,
                // the files the index holds: hidden ones too, none of the directories it skips
                hidden: true,
                globs: crate::files::skipped_dirs(&s.ix.root).iter().map(|d| format!("!{d}")).collect(),
                ..crate::instant::Options::default()
            };
            let (groups, stopped) = crate::intel::grouped_grep(s, &o)?;
            Answer::Grep { groups, stopped_at: stopped.then_some(*max_results) }
        }
        Op::Symbol { name, limit } => {
            let (hits, stopped) = cut(targets(s, name, limit + 1)?, *limit, RAISE_LIMIT);
            Answer::Hits { hits, words: Vec::new(), snippet: true, stopped }
        }
        Op::Callers { name, limit } => {
            let (hits, mut undecided, mut unchecked) = callers(s, name, limit + 1)?;
            let (hits, stopped) = cut(hits, *limit, RAISE_LIMIT);
            // the unchecked ones come last and the undecided before them, so the one cut off was one of those
            if stopped.is_some() {
                let last = if unchecked > 0 { &mut unchecked } else { &mut undecided };
                *last = last.saturating_sub(1);
            }
            Answer::Callers { hits, undecided, unchecked, stopped }
        }
        Op::Callees { name, limit } => {
            let (resolved, external, from) = callees(s, name, limit + 1)?;
            let (resolved, stopped) = cut(resolved, *limit, RAISE_LIMIT);
            Answer::Callees { resolved, external, from, stopped }
        }
        Op::Read { path, symbol: Some(symbol), .. } => {
            let mut rows = match crate::intel::location(symbol) {
                Some((path, line)) => s.ix.chunk_at(&repo_path(s, path), line)?.into_iter().collect(),
                None => {
                    let hits = s.find_symbol(symbol, READ_DEFINITIONS + 1)?;
                    hits.iter().filter_map(|h| s.ix.chunk(h.id).transpose()).collect::<Result<Vec<_>>>()?
                }
            };
            if let Some(path) = path {
                let path = repo_path(s, path);
                rows.retain(|r| r.path == path);
            }
            if rows.is_empty() {
                bail!("no definition matches {}", echo(symbol));
            }
            let read = |r: ChunkRow| {
                let mut text = s.read(&r.path, r.start, Some(r.end), READ_LINES);
                if r.end - r.start + 1 > READ_LINES {
                    text.push_str(&format!("\n(the definition goes on to line {}: read_code path={} start={})", r.end, r.path, r.start + READ_LINES));
                }
                (r, text)
            };
            let (rows, stopped) = cut(rows, READ_DEFINITIONS, "pass path to keep the ones of one file, or a location (path:start-end) for one");
            Answer::Definitions(rows.into_iter().map(read).collect(), stopped)
        }
        Op::Read { path, start, end, .. } => {
            let path = path.as_deref().unwrap_or("");
            // a path copied from an answer has its control characters written as escapes
            let read_back = crate::fmt::unshown(path);
            let exists = |p: &str| s.ix.root.join(p).exists();
            let printed = read_back != path && crate::fmt::shown(&read_back) == path && exists(&read_back);
            let path = if printed || (read_back != path && !exists(path)) { read_back.as_ref() } else { path };
            let start = (*start).max(1);
            let (lines, total) = read_range(s, path, start, end.unwrap_or(start.saturating_add(READ_LINES - 1)))?;
            Answer::Read { path: path.trim_start_matches("./").to_string(), lines, start, total }
        }
        Op::Outline { path } => {
            let path = repo_path(s, path);
            Answer::Outline { rows: crate::intel::outline(&s.ix, &path)?, path }
        }
        Op::Deps { path } => {
            let path = repo_path(s, path);
            Answer::Deps { deps: crate::intel::deps(&s.ix, &path)?, path }
        }
        Op::Rdeps { target } => {
            let target = repo_path(s, target);
            let (rows, unresolved) = crate::intel::rdeps_counted(&s.ix, &target)?;
            Answer::Rdeps { rows, target, unresolved }
        }
        Op::Refs { name, max_results, textual: true, .. } => {
            let name = &name.trim().to_string();
            Answer::Refs { name: name.clone(), refs: crate::intel::references(s, name, *max_results)?, max_results: *max_results }
        }
        Op::Refs { name, max_results, def, .. } => {
            let name = &name.trim().to_string();
            let refs = crate::resolve::resolved_references(s, name, *max_results)?;
            let only = match def {
                Some(spec) => Some(definition_at(s, &refs, name, spec)?),
                None => None,
            };
            Answer::Resolved { name: name.clone(), refs, only, max_results: *max_results }
        }
        Op::Similar { target, k } => Answer::Similar(crate::intel::similar(s, target, *k)?),
        Op::Dupes { min_tokens, threshold, limit } => {
            let (groups, stopped) = cut(crate::intel::dupes(&s.ix, *min_tokens, *threshold, limit.saturating_add(1))?, *limit, RAISE_LIMIT);
            Answer::Dupes(groups, stopped)
        }
        Op::Tsquery { lang, query, limit } => {
            let caps = crate::intel::tsquery(lang, query, std::slice::from_ref(&s.ix.root), limit + 1, Some(&s.ix.root))?;
            let (caps, stopped) = cut(caps, *limit, RAISE_LIMIT);
            Answer::Captures(caps, stopped)
        }
        Op::Reindex { force } => Answer::Reindexed(s.ix.update(*force)?),
    })
}

/// Lines `start..=end` of a repository file, numbered, read only as far as they go and at most
/// `READ_BYTES` of them; and how many lines the file has, when the read reached its end or the
/// file is small enough to count them.
fn read_range(s: &Searcher, path: &str, start: i64, end: i64) -> Result<(Vec<String>, Option<i64>)> {
    use std::io::BufRead;
    let shown = echo(path);
    let canon = std::fs::canonicalize(s.ix.root.join(path)).map_err(|_| anyhow!("cannot read {shown}"))?;
    if !canon.starts_with(&s.ix.root) {
        bail!("path outside repo");
    }
    if !canon.is_file() {
        bail!("{shown} is not a regular file");
    }
    let file = std::fs::File::open(&canon).map_err(|_| anyhow!("cannot read {shown}"))?;
    let small = file.metadata()?.len() <= crate::index::MAX_FILE_BYTES;
    let mut r = std::io::BufReader::with_capacity(64 * 1024, file);
    // a NUL byte in the first 8 KB, the test grep tools use
    if r.fill_buf().map(|b| b[..b.len().min(8192)].contains(&0))? {
        bail!("{shown} is not a text file (it holds a NUL byte): read_code shows text only");
    }
    // one line into `line`, up to its first `keep` bytes, where the read stops when `keep` is
    // not 0 -> (anything read, the line goes on past what was read)
    let mut next_line = |line: &mut Vec<u8>, keep: usize| -> std::io::Result<(bool, bool)> {
        let mut any = false;
        loop {
            let buf = r.fill_buf()?;
            if buf.is_empty() {
                return Ok((any, false));
            }
            any = true;
            let (body, done) = match buf.iter().position(|&b| b == b'\n') {
                Some(i) => (i, i + 1),
                None => (buf.len(), buf.len()),
            };
            let room = keep.saturating_sub(line.len());
            if keep > 0 && body > room {
                line.extend_from_slice(&buf[..room]);
                r.consume(room);
                return Ok((any, true));
            }
            line.extend_from_slice(&buf[..body.min(room)]);
            let newline = done > body;
            r.consume(done);
            if newline {
                return Ok((any, false));
            }
        }
    };
    let (mut out, mut used, mut n, mut line, mut mid_line) = (Vec::new(), 0usize, 0i64, Vec::new(), false);
    loop {
        if n >= end || (n + 1 >= start && used >= READ_BYTES) {
            break;
        }
        let wanted = n + 1 >= start;
        line.clear();
        let (any, cut) = next_line(&mut line, if wanted { READ_BYTES - used } else { 0 })?;
        if !any {
            return Ok((out, Some(n)));
        }
        n += 1;
        if wanted {
            if !cut && line.last() == Some(&b'\r') {
                line.pop();
            }
            used += line.len().max(1);
            let mut text = format!("{n:>5}  {}", String::from_utf8_lossy(&line));
            if cut {
                text.push_str(&format!("  (line cut at {} bytes)", line.len()));
                mid_line = true;
            }
            out.push(text);
        }
    }
    if !small {
        return Ok((out, (!mid_line && r.fill_buf()?.is_empty()).then_some(n)));
    }
    if mid_line {
        next_line(&mut line, 0)?;
    }
    loop {
        line.clear();
        if !next_line(&mut line, 0)?.0 {
            return Ok((out, Some(n)));
        }
        n += 1;
    }
}

/// One line for a structural match: where, the capture, and its text (the first line of the
/// node, with control characters written as escapes).
pub fn capture_line(c: &Capture) -> String {
    format!("{}:{}:{}  @{} ({})  {}", shown(&c.path), c.line, c.column, c.capture, c.kind, shown(&c.text))
}

/// `path` as the index spells it, when it names a repository file in a spelling `read_code`
/// accepts (`./a.py`, `pkg/../a.py`, another case on a case-insensitive file system); else as given.
fn repo_path(s: &Searcher, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            // above the root: no repository file, and the answer names the path as given
            ".." if parts.pop().is_none() => return path.to_string(),
            ".." => {}
            seg => parts.push(seg),
        }
    }
    let plain = parts.join("/");
    let indexed = |p: &str| s.ix.db.query_row("SELECT 1 FROM files WHERE path=?1", [p], |_| Ok(())).is_ok();
    // a path copied from an answer has its control characters and backslashes written as
    // escapes; the text an answer prints for a file names that file, whatever else exists
    let read_back = crate::fmt::unshown(&plain);
    let escaped = read_back != plain;
    if escaped && crate::fmt::shown(&read_back) == plain && indexed(&read_back) {
        return read_back.into_owned();
    }
    if path.is_empty() || indexed(&plain) {
        return plain;
    }
    if escaped && indexed(&read_back) {
        return read_back.into_owned();
    }
    std::fs::canonicalize(s.ix.root.join(path))
        .ok()
        .filter(|p| p.is_file())
        .and_then(|p| p.strip_prefix(&s.ix.root).ok().map(|r| crate::instant::slashed(r.to_string_lossy())))
        .filter(|p| indexed(p))
        .unwrap_or(plain)
}

/// The definitions `name` stands for: the one at a location (the `path:start-end` an earlier
/// answer printed), else every one `find_symbol` finds.
fn targets(s: &Searcher, name: &str, limit: usize) -> Result<Vec<Hit>> {
    match crate::intel::location(name) {
        Some((path, line)) => match s.ix.chunk_at(&repo_path(s, path), line)? {
            Some(c) => Ok(vec![Hit::from_row(&c, true)]),
            None => bail!("no symbol at {}", echo(name)),
        },
        None => s.find_symbol(name, limit),
    }
}

/// Languages whose code calls each other's definitions directly.
fn family(id: &'static str) -> &'static str {
    match id {
        "javascript" | "typescript" | "tsx" => "js",
        "c" | "cpp" | "cuda" | "objc" => "c",
        "java" | "kotlin" | "scala" | "groovy" => "jvm",
        "erlang" | "elixir" | "gleam" => "beam",
        "ocaml" | "ocaml_interface" => "ocaml",
        "csharp" | "fsharp" => "dotnet",
        id => id,
    }
}

/// The language of each file an answer asks about, by its text where the extension names two.
struct Langs<'a> {
    root: &'a std::path::Path,
    seen: std::cell::RefCell<std::collections::HashMap<String, Option<&'static str>>>,
}

impl Langs<'_> {
    fn id(&self, path: &str) -> Option<&'static str> {
        if let Some(id) = self.seen.borrow().get(path) {
            return *id;
        }
        let lower = path.to_lowercase();
        let shared = crate::langs::BY_CONTENT.iter().any(|(e, _)| lower.ends_with(e));
        let text = shared.then(|| crate::resolve::repo_text(self.root, path)).flatten();
        let id = text.and_then(|t| crate::langs::sniff(path, &t)).or_else(|| crate::langs::spec_for(path)).map(|s| s.id);
        self.seen.borrow_mut().insert(path.to_string(), id);
        id
    }

    /// A file of no known language may call or be called by any.
    fn same_family(&self, a: &str, b: &str) -> bool {
        match (self.id(a), self.id(b)) {
            (Some(x), Some(y)) => family(x) == family(y),
            _ => true,
        }
    }
}

/// What the resolver makes of the occurrences of `name` in lines `start..=end` of `path`.
struct Uses {
    /// the definitions they resolve to
    got: HashSet<i64>,
    /// the ones of `got` a call reaches other than through `super`, where an override may run instead
    dispatched: HashSet<i64>,
    /// one stayed unresolved (or none was found)
    unsure: bool,
    /// one stayed unresolved that may call a free function: not `x.name(..)` where a method
    /// call never reaches a free function
    unsure_free: bool,
    /// every unresolved one is a call without a receiver in a language the resolver does not
    /// read: it runs the definition in its own file when there is one (`own`)
    own_file: bool,
}

/// Whether the definition (`path`, `kind`, `qualname`) is what a call without a receiver in the
/// caller (its path and qualified name) runs: a free function of the caller's file, or a method
/// of the caller's own type.
fn own((from, caller): (&str, &str), path: &str, kind: &str, qualname: &str) -> bool {
    let owner = |q: &str| q.rsplit_once('.').map(|(o, _)| o.to_string());
    path == from && (free(kind, qualname) || owner(qualname) == owner(caller))
}

/// Languages where `x.f()` calls a method, never a free function `f` (in Python `x` may be a
/// module, and the resolver decides those through the import).
fn methods_only(path: &str) -> bool {
    crate::langs::spec_for(path).is_some_and(|s| matches!(s.id, "rust" | "javascript" | "typescript" | "tsx" | "go" | "java"))
}

fn resolve_in(rv: &mut Resolver, s: &Searcher, path: &str, (start, end): (i64, i64), name: &str) -> Uses {
    let mut uses = Uses { got: HashSet::new(), dispatched: HashSet::new(), unsure: true, unsure_free: true, own_file: false };
    let Some(src) = crate::resolve::repo_text(&s.ix.root, path) else { return uses };
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let methods = methods_only(path);
    let (mut unsure, mut unsure_free, mut seen, mut elsewhere) = (false, false, false, false);
    for (i, line) in src.lines().enumerate().skip((start.max(1) - 1) as usize).take((end - start + 1).max(0) as usize) {
        for (col, _) in line.match_indices(name) {
            if ident(line[..col].chars().next_back()) || ident(line[col + name.len()..].chars().next()) {
                continue;
            }
            match rv.resolve_at(path, i as u64 + 1, col, name) {
                // the name of a definition is not a call of it
                Ok((_, "definition")) => {}
                Ok((id, why)) => {
                    seen = true;
                    let all = if why == "overload" { rv.overloads(id) } else { vec![id] };
                    if why != "super" {
                        uses.dispatched.extend(&all);
                    }
                    // a use the arguments do not tie to one overload stands for all of them
                    uses.got.extend(all);
                }
                // an import from outside, a local, a field or a string is decided: not this definition
                Err(why) => {
                    seen = true;
                    let unread = matches!(why, "no grammar" | crate::resolve::UNREAD);
                    if unread || matches!(why, "receiver" | "not found") {
                        unsure = true;
                        unsure_free |= !(methods && line[..col].trim_end().ends_with('.'));
                        let receiver = [".", "->", "::"].iter().any(|r| line[..col].trim_end().ends_with(r));
                        elsewhere |= receiver || !unread;
                    }
                }
            }
        }
    }
    uses.unsure = unsure || !seen;
    uses.unsure_free = unsure_free || !seen;
    uses.own_file = unsure && !elsewhere;
    uses
}

/// A definition outside any class or type: `x.f()` does not call it where `methods_only` holds.
/// A method of an object literal has no owner in its name and is still called through one.
fn free(kind: &str, qualname: &str) -> bool {
    kind != "method" && !qualname.contains('.')
}

/// The directory a file's package starts at: what comes before its `src/`, else its top directory.
fn package(path: &str) -> &str {
    match path.strip_prefix("src/").map(|_| 0).or_else(|| path.find("/src/")) {
        Some(i) => &path[..i],
        None => path.split_once('/').map_or("", |(top, _)| top),
    }
}

/// The definitions whose code calls what `name` stands for, and how many of them were kept only
/// because the resolver could not tell which same-named definition they call. Those come after
/// the decided ones, first the ones whose code names the target's type, then the ones in its package.
/// Call sites the resolver is asked about in one `callers` answer, likeliest first.
const CALLERS_RESOLVED: usize = 400;

/// -> (callers: decided, then undecided, then unchecked; how many are undecided; how many unchecked)
pub(crate) fn callers(s: &Searcher, name: &str, limit: usize) -> Result<(Vec<Hit>, usize, usize)> {
    let langs = Langs { root: &s.ix.root, seen: Default::default() };
    let mut targets = targets(s, name, 500)?;
    // a call of a class runs its constructor: `Point(1)`, `new Widget()`, Ruby `Invoice.new`
    let (classes, made_by_new) = constructed(s, &langs, &targets)?;
    targets.extend(classes);
    let ids: HashSet<i64> = targets.iter().map(|t| t.id).collect();
    let mut names: std::collections::BTreeSet<&str> = targets.iter().map(|t| t.name.as_str()).collect();
    if !made_by_new.is_empty() {
        names.insert("new");
    }
    let owners: HashSet<&str> =
        targets.iter().filter_map(|t| t.qualname.rsplit_once('.').map(|(o, _)| o.rsplit('.').next().unwrap_or(o))).filter(|o| !o.is_empty()).collect();
    let packages: HashSet<&str> = targets.iter().map(|t| package(&t.path)).collect();
    let fits = |path: &str| targets.iter().any(|t| langs.same_family(&t.path, path));
    let cols = crate::index::CHUNK_COLS.split(',').map(|c| format!("c.{c}")).collect::<Vec<_>>().join(",");
    let mut calls = s.ix.db.prepare(&format!("SELECT DISTINCT {cols} FROM refs r JOIN chunks c ON c.id=r.chunk_id WHERE r.callee=?1 ORDER BY c.is_test"))?;
    let mut namesakes = s.ix.db.prepare("SELECT id, path, kind, qualname FROM chunks WHERE name=?1 AND kind != 'window'")?;
    let (mut out, mut unsure_rows, mut unchecked_rows, mut seen) = (Vec::new(), Vec::new(), Vec::new(), HashSet::new());
    let mut rv = Resolver::new(&s.ix)?;
    // a call of a method a target overrides or implements may run the target
    let overridden: HashSet<i64> = targets.iter().flat_map(|t| rv.overridden(t.id)).filter(|id| !ids.contains(id)).collect();
    let names_owner = |text: &str| owners.iter().any(|o| has_word(text, o));
    let likely = |r: &ChunkRow| (!names_owner(&r.text), !packages.contains(package(&r.path)), r.path.clone(), r.start);
    let all_free = |n: &str| targets.iter().filter(|t| t.name == n).all(|t| free(&t.kind, &t.qualname));
    let mut asked = 0;
    for n in names {
        let rows: Vec<(i64, String, String, String)> =
            namesakes.query_map([n], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
        // with another definition of the name about, the likeliest call sites are asked first
        let rivals = rows.iter().any(|(id, path, ..)| !ids.contains(id) && fits(path));
        let mut sites: Vec<ChunkRow> = calls.query_map([n], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        sites.retain(|r| fits(&r.path) && seen.insert(r.id));
        if n == "new" && !targets.iter().any(|t| t.name == "new") {
            sites.retain(|r| made_by_new.iter().any(|class| has_word(&r.text, &format!("{class}.new"))));
        }
        if rivals {
            sites.sort_by_cached_key(&likely);
        }
        for row in sites {
            // a common name has thousands of call sites: the likeliest are resolved, the rest stay undecided
            if asked >= CALLERS_RESOLVED {
                unchecked_rows.push(row);
                continue;
            }
            asked += 1;
            let uses = resolve_in(&mut rv, s, &row.path, (row.start, row.end), n);
            let unsure = (if all_free(n) { uses.unsure_free } else { uses.unsure }) || !uses.dispatched.is_disjoint(&overridden);
            if uses.got.is_disjoint(&ids) {
                // with no other definition of the name, a call the resolver cannot place is this one's
                if unsure && rivals {
                    let mine = |(_, p, k, q): &&(i64, String, String, String)| own((&row.path, &row.qualname), p, k, q);
                    let mine: Vec<i64> = rows.iter().filter(|r| uses.own_file && mine(r)).map(|r| r.0).collect();
                    if mine.is_empty() {
                        unsure_rows.push(row);
                        continue;
                    }
                    if mine.iter().all(|id| !ids.contains(id)) {
                        continue;
                    }
                } else if !unsure {
                    continue;
                }
            }
            out.push(Hit::from_row(&row, true));
            if out.len() >= limit {
                return Ok((out, 0, 0));
            }
        }
    }
    unsure_rows.sort_by_cached_key(&likely);
    let undecided = unsure_rows.len().min(limit - out.len());
    out.extend(unsure_rows.iter().take(undecided).map(|r| Hit::from_row(r, true)));
    let unchecked = unchecked_rows.len().min(limit - out.len());
    out.extend(unchecked_rows.iter().take(unchecked).map(|r| Hit::from_row(r, true)));
    Ok((out, undecided, unchecked))
}

/// The classes whose constructor is among `targets`, and the names of the Ruby ones, which a
/// call of `new` on the class makes.
fn constructed(s: &Searcher, langs: &Langs, targets: &[Hit]) -> Result<(Vec<Hit>, Vec<String>)> {
    let mut class_at =
        s.ix.db.prepare(&format!("SELECT {} FROM chunks WHERE path=?1 AND qualname=?2 AND kind IN ('class','struct') LIMIT 1", crate::index::CHUNK_COLS))?;
    let (mut classes, mut made_by_new) = (Vec::<Hit>::new(), Vec::new());
    for t in targets {
        let Some((owner, name)) = t.qualname.rsplit_once('.') else { continue };
        let lang = langs.id(&t.path).unwrap_or("");
        let constructor = match lang {
            "python" => name == "__init__",
            "javascript" | "typescript" | "tsx" => name == "constructor",
            "ruby" => name == "initialize",
            "php" => name == "__construct",
            "swift" => name == "init",
            // Java, C#, C++, Dart: the method named as its class
            _ => name == owner.rsplit('.').next().unwrap_or(owner),
        };
        if !constructor {
            continue;
        }
        let class: Option<ChunkRow> = rusqlite::OptionalExtension::optional(class_at.query_row((&t.path, owner), ChunkRow::from_row))?;
        let Some(class) = class.filter(|c| !targets.iter().chain(&classes).any(|x| x.id == c.id)) else { continue };
        if lang == "ruby" {
            made_by_new.push(class.name.clone());
        }
        classes.push(Hit::from_row(&class, true));
    }
    Ok((classes, made_by_new))
}

/// Whether `word` is in `text` where it is not part of a longer identifier.
fn has_word(text: &str, word: &str) -> bool {
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    text.match_indices(word).any(|(i, _)| !ident(text[..i].chars().next_back()) && !ident(text[i + word.len()..].chars().next()))
}

/// What the code of the definitions `name` stands for calls: definitions of the repository in
/// the same language family, and the names it calls that have none.
pub(crate) fn callees(s: &Searcher, name: &str, limit: usize) -> Result<(Vec<Hit>, Vec<String>, Vec<String>)> {
    let kinds = crate::search::CODE_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
    let mut named =
        s.ix.db.prepare(&format!("SELECT {} FROM chunks WHERE name=?1 AND kind IN ({kinds},'key','macro') ORDER BY is_test", crate::index::CHUNK_COLS))?;
    let langs = Langs { root: &s.ix.root, seen: Default::default() };
    // a key is what a call runs only in the caller's own language (Nix, whose functions are keys)
    let callable = |from: &str, r: &ChunkRow| langs.same_family(from, &r.path) && (r.kind != "key" || langs.id(from) == langs.id(&r.path));
    let mut calls = s.ix.db.prepare("SELECT DISTINCT callee FROM refs WHERE chunk_id=?1 ORDER BY callee")?;
    let mut rv = Resolver::new(&s.ix)?;
    let (mut resolved, mut external, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
    let defs = targets(s, name, 5)?;
    let from = defs.iter().map(|d| d.path.clone()).collect();
    for d in defs {
        let names: Vec<String> = calls.query_map([d.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for n in names {
            let rows: Vec<ChunkRow> =
                named.query_map([&n], ChunkRow::from_row)?.filter(|r| r.as_ref().map_or(true, |r| callable(&d.path, r))).collect::<rusqlite::Result<_>>()?;
            if rows.is_empty() {
                if !external.contains(&n) {
                    external.push(n);
                }
                continue;
            }
            let uses = resolve_in(&mut rv, s, &d.path, (d.start, d.end), &n);
            let tied: Vec<&ChunkRow> = rows.iter().filter(|r| uses.got.contains(&r.id)).collect();
            let picked: Vec<&ChunkRow> = if !tied.is_empty() {
                tied
            } else if uses.unsure {
                let mut near: Vec<&ChunkRow> = rows.iter().filter(|r| uses.unsure_free || !free(&r.kind, &r.qualname)).collect();
                let mine = |r: &&ChunkRow| own((&d.path, &d.qualname), &r.path, &r.kind, &r.qualname);
                if uses.own_file && near.iter().any(mine) {
                    near.retain(mine);
                }
                // the nearest three: the caller's file, then its directory
                let dir = |p: &str| p.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
                near.sort_by_key(|r| (r.path != d.path, dir(&r.path) != dir(&d.path)));
                near.truncate(3);
                near
            } else {
                Vec::new()
            };
            if picked.is_empty() && !external.contains(&n) {
                external.push(n.clone());
            }
            resolved.extend(picked.into_iter().filter(|r| seen.insert(r.id)).map(|r| Hit::from_row(r, true)));
        }
    }
    external.sort();
    resolved.truncate(limit);
    Ok((resolved, external, from))
}

/// The definition of `name` that `spec` (a path or a location) points at: the innermost one
/// around the line, or the first in the file.
fn definition_at(s: &Searcher, refs: &Resolved, name: &str, spec: &str) -> Result<i64> {
    let (path, line) = match crate::intel::location(spec) {
        Some((path, line)) => (repo_path(s, path), Some(line)),
        None => (repo_path(s, spec), None),
    };
    refs.definitions
        .iter()
        .filter(|d| d.path == path && line.is_none_or(|l| d.start <= l && l <= d.end))
        .min_by_key(|d| (line.is_some().then_some(d.end - d.start), d.start))
        .map(|d| d.id)
        .ok_or_else(|| anyhow!("no definition of {} in {}", echo(name), echo(spec)))
}

/// The closing line of an answer that stopped at its limit.
pub fn stop_line(stopped: Option<Stop>, noun: &str) -> String {
    stopped.map(|s| format!("(stopped at {} {noun}: {})", s.at, s.how)).unwrap_or_default()
}

/// Two closing lines, either of them possibly empty.
fn joined(a: String, b: String) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, _) => b,
        (_, true) => a,
        _ => format!("{a}\n{b}"),
    }
}

/// The closing line of a `references` answer whose occurrence search stopped at `max_results`.
fn stopped_at(truncated: bool, max_results: u64) -> String {
    if truncated {
        format!("(the search stopped at max_results={max_results} occurrences, so these counts cover only those: raise max_results to see all)")
    } else {
        String::new()
    }
}

fn symbol_name(c: &ChunkRow) -> &str {
    if c.qualname.is_empty() {
        &c.name
    } else {
        &c.qualname
    }
}

fn chunk_json(c: &ChunkRow) -> Value {
    json!({"id": format!("{}:{}-{}", c.path, c.start, c.end), "path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "qualname": c.qualname})
}

/// An answer as text: a first line, the items that can be counted and cut, and a last line.
#[derive(Default)]
struct Parts {
    head: String,
    items: Vec<String>,
    tail: String,
    noun: &'static str,
    /// the items are lines of a file: its path, the number of the first, and the file's length
    /// (None: unknown, more lines follow)
    file: Option<(String, i64, Option<i64>)>,
}

const CUT_ITEM: &str = "(this item is cut: pass a larger max_bytes to see all of it)";

/// `items` (one result each, possibly of several lines) as `Answer::text_within` pages them.
pub fn paged(items: Vec<String>, noun: &'static str, max_bytes: usize, offset: usize) -> String {
    Parts { items, noun, ..Parts::default() }.within(max_bytes, offset)
}

impl Parts {
    fn within(&self, max_bytes: usize, offset: usize) -> String {
        let mut room = max_bytes;
        loop {
            let (out, shown, cut) = self.render(max_bytes, room, offset);
            if out.len() <= max_bytes || room == 0 || (shown <= 1 && !cut) {
                return out;
            }
            room = room.saturating_sub(out.len() - max_bytes);
        }
    }

    /// The text with the items from `offset` on that fit `room` bytes (at least one; one longer
    /// than `max_bytes` is cut to fit `room`), how many were shown, and whether one was cut.
    fn render(&self, max_bytes: usize, room: usize, offset: usize) -> (String, usize, bool) {
        let mut out = self.head.clone();
        let push = |out: &mut String, part: &str| {
            if !out.is_empty() && !part.is_empty() {
                out.push('\n');
            }
            out.push_str(part);
        };
        let (mut count, mut cut) = (0, false);
        for item in self.items.iter().skip(offset) {
            if count > 0 && out.len() + item.len() + 1 > room {
                break;
            }
            let before = out.len();
            push(&mut out, item);
            count += 1;
            if out.len() > max_bytes {
                let mut at = room.saturating_sub(CUT_ITEM.len() + 1).max(before);
                while !out.is_char_boundary(at) {
                    at -= 1;
                }
                out.truncate(at);
                push(&mut out, CUT_ITEM);
                cut = true;
                break;
            }
        }
        let next = offset.min(self.items.len()) + count;
        if let Some((path, first, total)) = &self.file {
            let last = first + next as i64 - 1;
            if count > 0 && total.is_none_or(|t| last < t) {
                let path = shown(path);
                let of = match total {
                    Some(t) => format!("of {t} in {path}"),
                    None => format!("of {path}, more lines follow"),
                };
                push(&mut out, &format!("(lines {}-{last} {of}: read_code path={path} start={} for more)", first + offset as i64, last + 1));
            }
        } else if next < self.items.len() {
            push(&mut out, &format!("({} more {} not shown: call again with offset={next}, or narrow the request)", self.items.len() - next, self.noun));
        }
        if next >= self.items.len() {
            if offset > 0 && count == 0 {
                push(&mut out, &format!("(no more {}: {} in all)", self.noun, self.items.len()));
            }
            push(&mut out, &self.tail);
        }
        (out, count, cut)
    }
}

impl Answer {
    /// The repository files whose indexed content or positions this answer shows. A line range
    /// and a structural search read the files themselves and cite none.
    pub fn cited(&self) -> Vec<&str> {
        fn rows(rows: &[ChunkRow]) -> Vec<&str> {
            rows.iter().map(|r| r.path.as_str()).collect()
        }
        match self {
            Answer::Hits { hits, .. } | Answer::Callers { hits, .. } => hits.iter().map(|h| h.path.as_str()).collect(),
            Answer::Callees { resolved, from, .. } => resolved.iter().map(|h| h.path.as_str()).chain(from.iter().map(String::as_str)).collect(),
            Answer::Grep { groups, .. } => groups.iter().map(|g| g.path.as_str()).collect(),
            Answer::Definitions(defs, _) => defs.iter().map(|(c, _)| c.path.as_str()).collect(),
            Answer::Outline { path, .. } | Answer::Deps { path, .. } => vec![path.as_str()],
            Answer::Rdeps { rows, .. } => rows.iter().map(|r| r.0.as_str()).collect(),
            Answer::Refs { refs, .. } => rows(&refs.definitions).into_iter().chain(refs.groups.iter().map(|g| g.path.as_str())).collect(),
            Answer::Resolved { refs, .. } => rows(&refs.definitions).into_iter().chain(refs.occurrences.iter().map(|o| o.path.as_str())).collect(),
            Answer::Similar(v) => v.iter().map(|h| h.0.path.as_str()).collect(),
            Answer::Dupes(groups, _) => groups.iter().flat_map(|g| g.members.iter().map(|m| m.0.as_str())).collect(),
            Answer::Read { .. } | Answer::Captures(..) | Answer::Reindexed(_) => Vec::new(),
        }
    }

    /// Nothing was found (the CLI's exit code 1).
    pub fn is_empty(&self) -> bool {
        match self {
            Answer::Hits { hits, .. } | Answer::Callers { hits, .. } => hits.is_empty(),
            Answer::Callees { resolved, external, .. } => resolved.is_empty() && external.is_empty(),
            Answer::Grep { groups, .. } => groups.is_empty(),
            Answer::Read { lines, .. } => lines.is_empty(),
            Answer::Definitions(defs, _) => defs.is_empty(),
            Answer::Outline { rows, .. } => rows.is_empty(),
            Answer::Deps { deps, .. } => deps.is_empty(),
            Answer::Rdeps { rows, .. } => rows.is_empty(),
            Answer::Refs { refs, .. } => refs.is_empty(),
            Answer::Resolved { refs, only, .. } => match only {
                Some(_) => false,
                None => refs.definitions.is_empty() && refs.occurrences.is_empty(),
            },
            Answer::Similar(v) => v.is_empty(),
            Answer::Dupes(v, _) => v.is_empty(),
            Answer::Captures(v, _) => v.is_empty(),
            Answer::Reindexed(_) => false,
        }
    }

    /// The answer as text: one item per line or block, and a sentence when there is nothing.
    pub fn text(&self) -> String {
        self.text_within(usize::MAX, 0)
    }

    /// The answer as text of `max_bytes` at most: whole items from item `offset` on, then a line
    /// saying how many are left and how to ask for them. A harness cuts a tool result that is
    /// too long for it (Codex at 10,000 tokens, from the middle); cutting here keeps whole items
    /// and tells the agent what to do next. Only a budget smaller than one item and that line
    /// together is exceeded.
    pub fn text_within(&self, max_bytes: usize, offset: usize) -> String {
        self.parts().within(max_bytes, offset)
    }

    fn parts(&self) -> Parts {
        let lines = |text: String, noun: &'static str| Parts { items: text.lines().map(str::to_string).collect(), noun, ..Parts::default() };
        let nothing = |sentence: String| Parts { head: sentence, ..Parts::default() };
        match self {
            Answer::Hits { hits, .. } if hits.is_empty() => nothing("(no results)".to_string()),
            Answer::Hits { hits, words, snippet, stopped } => {
                Parts { items: hit_blocks(hits, words, *snippet, false), tail: stop_line(*stopped, "results"), noun: "results", ..Parts::default() }
            }
            Answer::Callers { hits, .. } if hits.is_empty() => nothing("(no results)".to_string()),
            Answer::Callers { hits, undecided, unchecked, stopped } => Parts {
                items: hit_blocks(hits, &[], false, false),
                tail: joined(
                    joined(
                        match *undecided {
                            0 => String::new(),
                            n if n == hits.len() => {
                                "(the resolver could not tell whether any of these calls this definition or another of the same name)".to_string()
                            }
                            n => format!("({n} of these call a definition of that name the resolver could not identify: it may be another one)"),
                        },
                        match *unchecked {
                            0 => String::new(),
                            n => format!("(the last {n} were not checked: the resolver is asked about the {CALLERS_RESOLVED} likeliest call sites)"),
                        },
                    ),
                    stop_line(*stopped, "results"),
                ),
                noun: "results",
                ..Parts::default()
            },
            Answer::Callees { resolved, external, stopped, .. } => Parts {
                head: if resolved.is_empty() { "(no results)".to_string() } else { String::new() },
                items: hit_blocks(resolved, &[], false, false),
                tail: joined(
                    if external.is_empty() {
                        String::new()
                    } else {
                        format!("external: {}", external.iter().take(EXTERNAL_CALLEES).map(|e| shown(e)).collect::<Vec<_>>().join(", "))
                    },
                    stop_line(*stopped, "callees"),
                ),
                noun: "callees",
                ..Parts::default()
            },
            Answer::Grep { groups, stopped_at } => {
                let lines: usize = groups.iter().map(|g| g.lines.len()).sum();
                let stopped = stopped_at.map(|max| format!(" (stopped at {max} matching lines: narrow the pattern or raise max_results)")).unwrap_or_default();
                Parts {
                    head: format!("{lines} matching lines in {} symbols{stopped}", groups.len()),
                    items: groups.iter().map(crate::intel::group_text).collect(),
                    noun: "symbols",
                    ..Parts::default()
                }
            }
            Answer::Read { path, total: Some(0), .. } => nothing(format!("{} is empty", shown(path))),
            Answer::Read { path, lines, start, total: Some(total) } if lines.is_empty() => {
                nothing(format!("{} has {total} lines: start={start} is past the end", shown(path)))
            }
            Answer::Read { path, lines, start, total: None } if lines.is_empty() => nothing(format!("{}: no lines from start={start}", shown(path))),
            Answer::Read { path, lines, start, total } => {
                Parts { items: lines.clone(), noun: "lines", file: Some((path.clone(), *start, *total)), ..Parts::default() }
            }
            Answer::Definitions(defs, stopped) => Parts {
                items: defs
                    .iter()
                    .map(|(c, text)| format!("{}:{}-{}  [{}] {}\n{text}", shown(&c.path), c.start, c.end, c.kind, shown(symbol_name(c))))
                    .collect(),
                tail: stop_line(*stopped, "definitions"),
                noun: "definitions",
                ..Parts::default()
            },
            Answer::Outline { path, rows } if rows.is_empty() => nothing(format!("no symbols in {} (not indexed or no definitions)", echo(path))),
            Answer::Outline { rows, .. } => Parts {
                items: rows
                    .iter()
                    .map(|r| format!("{:>5}-{:<5} {}{:<9} {}", r.start, r.end, "  ".repeat(r.qualname.matches('.').count()), r.kind, shown(&r.qualname)))
                    .collect(),
                noun: "symbols",
                ..Parts::default()
            },
            Answer::Deps { path, deps } if deps.is_empty() => nothing(format!("no imports recorded for {}", echo(path))),
            Answer::Deps { path, deps } => Parts {
                items: deps
                    .iter()
                    .map(|d| {
                        format!(
                            "{}:{}  {}{}",
                            shown(path),
                            d.line,
                            shown(&d.module),
                            d.resolved.as_ref().map(|r| format!("  -> {}", shown(r))).unwrap_or_default()
                        )
                    })
                    .collect(),
                noun: "imports",
                ..Parts::default()
            },
            Answer::Rdeps { target, rows, unresolved: 0 } if rows.is_empty() => nothing(format!("nothing imports {}", echo(target))),
            Answer::Rdeps { target, rows, unresolved } if rows.is_empty() => nothing(format!(
                "no import resolves to {}, but {unresolved} {} could not be resolved and may name it",
                echo(target),
                if *unresolved == 1 { "import" } else { "imports" }
            )),
            Answer::Rdeps { rows, unresolved, .. } => Parts {
                items: rows.iter().map(|(p, l, m)| format!("{}:{l}  {}", shown(p), shown(m))).collect(),
                tail: match unresolved {
                    0 => String::new(),
                    n => format!("({n} more {} could not be resolved and may name it)", if *n == 1 { "import" } else { "imports" }),
                },
                noun: "importers",
                ..Parts::default()
            },
            Answer::Refs { refs, max_results, .. } => {
                if refs.is_empty() {
                    nothing("(no results)".to_string())
                } else {
                    Parts { tail: stopped_at(refs.truncated, *max_results), ..lines(crate::intel::references_render(refs), "lines") }
                }
            }
            Answer::Resolved { .. } if self.is_empty() => nothing("(no results)".to_string()),
            Answer::Resolved { refs, only, max_results, .. } => {
                Parts { tail: stopped_at(refs.truncated, *max_results), ..lines(crate::resolve::resolved_text(refs, *only), "lines") }
            }
            Answer::Similar(v) if v.is_empty() => nothing("(no results)".to_string()),
            Answer::Similar(v) => Parts {
                items: v
                    .iter()
                    .map(|(h, cos, jac)| format!("{:.3} cos  {:.2} jaccard  {}  [{}] {}", cos, jac, crate::fmt::loc(h), h.kind, shown(&h.qualname)))
                    .collect(),
                noun: "results",
                ..Parts::default()
            },
            Answer::Dupes(groups, _) if groups.is_empty() => nothing("no duplicates found".to_string()),
            Answer::Dupes(groups, stopped) => Parts {
                items: groups
                    .iter()
                    .map(|g| {
                        let mut out = format!("{} ({} tokens, similarity {:.2})", g.kind, g.tokens, g.similarity);
                        for (p, q, a, b) in &g.members {
                            out.push_str(&format!("\n    {}:{a}-{b}  {}", shown(p), shown(q)));
                        }
                        out
                    })
                    .collect(),
                tail: stop_line(*stopped, "groups"),
                noun: "groups",
                ..Parts::default()
            },
            Answer::Captures(caps, _) if caps.is_empty() => nothing("no matches".to_string()),
            Answer::Captures(caps, stopped) => {
                Parts { tail: stop_line(*stopped, "matches"), items: caps.iter().map(capture_line).collect(), noun: "matches", ..Parts::default() }
            }
            Answer::Reindexed(st) => nothing(format!("{} files changed, {} removed, {} chunks", st.files_changed, st.files_removed, st.chunks)),
        }
    }

    /// The answer for a client that reads structured results, sent beside a text of `text_len`
    /// bytes that starts at item `offset`: the whole JSON form when both together fit one and a
    /// half times `max_bytes`, else `null` (the text then pages through the answer).
    pub fn structured(&self, text_len: usize, max_bytes: usize, offset: usize) -> Value {
        let result = self.json();
        let complete = offset == 0 && text_len + result.to_string().len() <= max_bytes.saturating_add(max_bytes / 2);
        let mut v = json!({"schema": SCHEMA, "complete": complete, "result": if complete { result } else { Value::Null }});
        if let Some(stop) = self.stopped() {
            v["stopped_at"] = json!(stop.at);
        }
        v
    }

    /// The limit the answer stopped at, when there were more.
    pub fn stopped(&self) -> Option<Stop> {
        match self {
            Answer::Hits { stopped, .. } | Answer::Callers { stopped, .. } | Answer::Callees { stopped, .. } => *stopped,
            Answer::Definitions(_, stopped) | Answer::Captures(_, stopped) | Answer::Dupes(_, stopped) => *stopped,
            _ => None,
        }
    }

    /// The answer as JSON, the shape `--json` prints.
    pub fn json(&self) -> Value {
        match self {
            Answer::Hits { hits, .. } | Answer::Callers { hits, .. } => hits_json(hits),
            Answer::Callees { resolved, external, .. } => json!({"resolved": hits_json(resolved), "external": external}),
            Answer::Grep { groups, stopped_at } => json!({
                "schema": SCHEMA,
                "groups": groups.iter().map(|g| json!({
                    "symbol": g.symbol.as_ref().map(|c| json!({"path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "name": symbol_name(c)})),
                    "path": g.path,
                    "lines": g.lines.iter().map(|h| json!({"line": h.line, "text": h.text})).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "stopped_at": stopped_at,
            }),
            Answer::Read { lines, start, total, .. } => {
                json!({"text": lines.join("\n"), "start": start, "end": start + lines.len() as i64 - 1, "file_lines": total})
            }
            Answer::Definitions(defs, _) => json!({
                "definitions": defs.iter().map(|(c, text)| {
                    let mut v = chunk_json(c);
                    v["text"] = json!(text);
                    v
                }).collect::<Vec<_>>(),
            }),
            Answer::Outline { rows, .. } => {
                rows.iter().map(|r| json!({"kind": r.kind, "name": r.name, "qualname": r.qualname, "start": r.start, "end": r.end})).collect()
            }
            Answer::Deps { deps, .. } => serde_json::to_value(deps).unwrap_or_default(),
            Answer::Rdeps { rows, .. } => rows.iter().map(|(p, l, m)| json!({"path": p, "line": l, "module": m})).collect(),
            Answer::Refs { name, refs, .. } => json!({
                "schema": SCHEMA,
                "name": name,
                "definitions": refs.definitions.iter().map(chunk_json).collect::<Vec<_>>(),
                "callers": hits_json(&refs.callers),
                "references": refs.groups.iter().map(|g| json!({
                    "symbol": g.symbol.as_ref().map(chunk_json),
                    "path": g.path,
                    "lines": g.lines.iter().map(|h| json!({"line": h.line, "column": h.column, "text": h.text})).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "truncated": refs.truncated,
            }),
            Answer::Resolved { name, refs, only, .. } => {
                let occurrence = |o: &crate::resolve::Occurrence| json!({"path": o.path, "line": o.line, "column": o.column, "text": o.text, "via": o.resolution.ok().map(|x| x.1)});
                let mut v = json!({
                    "schema": SCHEMA,
                    "name": name,
                    "definitions": refs.definitions.iter().filter(|d| only.is_none_or(|id| d.id == id)).map(|c| json!({
                        "path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "qualname": c.qualname,
                        "references": refs.of(c.id).map(occurrence).collect::<Vec<_>>(),
                    })).collect::<Vec<_>>(),
                    "truncated": refs.truncated,
                });
                if only.is_none() {
                    v["unresolved"] = refs
                        .unresolved()
                        .map(|o| json!({"path": o.path, "line": o.line, "column": o.column, "text": o.text, "reason": o.resolution.err()}))
                        .collect();
                }
                v
            }
            Answer::Similar(v) => v
                .iter()
                .map(|(h, cos, jac)| json!({"path": h.path, "start": h.start, "end": h.end, "kind": h.kind, "qualname": h.qualname, "cosine": cos, "jaccard": jac}))
                .collect(),
            Answer::Dupes(groups, _) => serde_json::to_value(groups).unwrap_or_default(),
            Answer::Captures(caps, _) => serde_json::to_value(caps).unwrap_or_default(),
            Answer::Reindexed(st) => json!({"files_changed": st.files_changed, "files_removed": st.files_removed, "chunks": st.chunks}),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_call_without_optional_arguments_gets_the_defaults() {
        let op = |tool: &str, args: Value| Op::from_tool(tool, &args).unwrap();
        assert!(matches!(op("search_code", json!({"query": "q"})), Op::Search { k: SEARCH_K, .. }));
        assert!(matches!(op("grep_code", json!({"pattern": "p"})), Op::Grep { max_results: GREP_MAX, .. }));
        assert!(matches!(op("find_symbol", json!({"name": "n"})), Op::Symbol { limit: SYMBOL_LIMIT, .. }));
        assert!(matches!(op("callers", json!({"name": "n"})), Op::Callers { limit: CALLERS_LIMIT, .. }));
        assert!(matches!(op("callees", json!({"name": "n"})), Op::Callees { limit: CALLEES_LIMIT, .. }));
        assert!(matches!(op("read_code", json!({"path": "p"})), Op::Read { start: 1, end: None, symbol: None, .. }));
        assert!(matches!(op("read_code", json!({"symbol": "Point.norm"})), Op::Read { path: None, symbol: Some(_), .. }));
        assert_eq!(Op::from_tool("read_code", &json!({})).err().unwrap().to_string(), "missing path or symbol");
        assert!(matches!(op("references", json!({"name": "n"})), Op::Refs { max_results: REFS_MAX, textual: false, def: None, .. }));
        assert!(matches!(op("similar_code", json!({"target": "t"})), Op::Similar { k: SIMILAR_K, .. }));
        assert!(matches!(op("duplicates", json!({})), Op::Dupes { min_tokens: DUPES_MIN_TOKENS, limit: DUPES_LIMIT, .. }));
        assert!(matches!(op("structural_search", json!({"lang": "rust", "query": "(x)"})), Op::Tsquery { limit: TSQUERY_LIMIT, .. }));
        assert!(matches!(op("reindex", json!({})), Op::Reindex { force: false }));
    }

    #[test]
    fn limits_are_clamped_and_bad_calls_are_errors() {
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": 100_000})).unwrap(), Op::Search { k: 100, .. }));
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": 0})).unwrap(), Op::Search { k: 1, .. }));
        assert_eq!(Op::from_tool("search_code", &json!({"query": "q", "k": -3})).err().unwrap().to_string(), "k must be a non-negative integer");
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": null})).unwrap(), Op::Search { k: SEARCH_K, .. }));
        assert!(matches!(Op::from_tool("references", &json!({"name": "n", "max_results": 1_000_000})).unwrap(), Op::Refs { max_results: 5000, .. }));
        assert_eq!(Op::from_tool("callers", &json!({})).err().unwrap().to_string(), "missing name");
        assert_eq!(Op::from_tool("no_such_tool", &json!({})).err().unwrap().to_string(), "unknown tool no_such_tool");
        assert!(read_only("search_code") && read_only("read_code") && !read_only("reindex"));
    }

    #[test]
    fn a_long_answer_is_cut_at_whole_items_and_says_how_to_continue() {
        let hit = |i: usize| Hit { path: format!("src/f{i}.rs"), start: 1, end: 9, name: format!("f{i}"), kind: "function".into(), ..Hit::default() };
        let answer = Answer::Hits { hits: (0..5).map(hit).collect(), words: vec![], snippet: false, stopped: None };
        let all = answer.text();
        assert_eq!(all.lines().count(), 5);
        assert_eq!(answer.text_within(usize::MAX, 0), all);
        // room for two items of 30 bytes each and the closing line, not three
        let cut = answer.text_within(140, 0);
        assert!(cut.len() <= 140);
        assert_eq!(
            cut.lines().collect::<Vec<_>>(),
            ["src/f0.rs:1-9  [function] f0", "src/f1.rs:1-9  [function] f1", "(3 more results not shown: call again with offset=2, or narrow the request)",]
        );
        let rest = answer.text_within(usize::MAX, 2);
        assert_eq!(rest.lines().count(), 3);
        assert!(rest.starts_with("src/f2.rs"));
        assert_eq!(answer.text_within(140, 5), "(no more results: 5 in all)");
        // an item larger than the budget keeps its start
        let one = Answer::Read { path: "a.txt".into(), lines: vec!["x".repeat(500)], start: 1, total: Some(1) };
        let cut = one.text_within(100, 0);
        assert!(cut.len() <= 100 && cut.starts_with(&"x".repeat(30)) && cut.ends_with(CUT_ITEM), "{cut}");
    }

    #[test]
    fn an_empty_answer_says_so_in_words() {
        assert_eq!(Answer::Outline { path: "a.py".into(), rows: vec![] }.text(), "no symbols in a.py (not indexed or no definitions)");
        assert_eq!(Answer::Rdeps { target: "m".into(), rows: vec![], unresolved: 0 }.text(), "nothing imports m");
        assert_eq!(Answer::Hits { hits: vec![], words: vec![], snippet: true, stopped: None }.text(), "(no results)");
        assert!(Answer::Dupes(vec![], None).is_empty() && Answer::Captures(vec![], None).is_empty());
        assert_eq!(
            Answer::Rdeps { target: "m".into(), rows: vec![("a.py".into(), 3, "m".into())], unresolved: 0 }.json(),
            json!([{"path": "a.py", "line": 3, "module": "m"}])
        );
    }
}
