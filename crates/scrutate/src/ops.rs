//! Every question a front end can ask of the index: its arguments with their defaults and limits,
//! when the index is refreshed first, and a typed answer with one text form and one JSON form.
//! The CLI, the MCP server, `batch` and the Python bindings parse into `Op` and render `Answer`;
//! none of them holds a default, a limit or an output format of its own.

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::fmt::{hit_blocks, hits_json};
use crate::index::{ChunkRow, UpdateStats};
use crate::intel::{Capture, Dependency, DupGroup, RefGroup, References};
use crate::resolve::Resolved;
use crate::search::{Hit, Searcher};
use crate::text::query_words;

/// Version of every JSON record scrutate emits.
pub const SCHEMA: u32 = 1;

pub const SEARCH_K: usize = 8;
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
    Reindex,
}

impl Op {
    /// The operation an MCP tool call or a `batch` line names, defaults filled in and limits
    /// clamped (the caller is a program: an absurd limit is cut, not refused).
    pub fn from_tool(name: &str, args: &Value) -> Result<Op> {
        let text = |k: &str| args.get(k).and_then(Value::as_str).map(str::to_string).ok_or_else(|| anyhow!("missing {k}"));
        let count = |k: &str, default: u64, max: u64| args.get(k).and_then(Value::as_u64).unwrap_or(default).clamp(1, max);
        Ok(match name {
            "search_code" => Op::Search {
                query: text("query")?,
                alt: args
                    .get("alt_queries")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                k: count("k", SEARCH_K as u64, 100) as usize,
            },
            "grep_code" => Op::Grep { pattern: text("pattern")?, max_results: count("max_results", GREP_MAX, 5000) },
            "find_symbol" => Op::Symbol { name: text("name")?, limit: count("limit", SYMBOL_LIMIT as u64, 500) as usize },
            "callers" => Op::Callers { name: text("name")?, limit: count("limit", CALLERS_LIMIT as u64, 500) as usize },
            "callees" => Op::Callees { name: text("name")?, limit: count("limit", CALLEES_LIMIT as u64, 500) as usize },
            "read_code" => {
                let (path, symbol) = (text("path").ok(), text("symbol").ok());
                if path.is_none() && symbol.is_none() {
                    bail!("missing path or symbol");
                }
                Op::Read { path, symbol, start: args.get("start").and_then(Value::as_i64).unwrap_or(1), end: args.get("end").and_then(Value::as_i64) }
            }
            "outline" => Op::Outline { path: text("path")? },
            "dependencies" => Op::Deps { path: text("path")? },
            "dependents" => Op::Rdeps { target: text("target")? },
            "references" => Op::Refs {
                name: text("name")?,
                max_results: count("max_results", REFS_MAX, 5000),
                textual: args.get("textual").and_then(Value::as_bool).unwrap_or(false),
                def: args.get("def").and_then(Value::as_str).map(str::to_string),
            },
            "similar_code" => Op::Similar { target: text("target")?, k: count("k", SIMILAR_K as u64, 50) as usize },
            "duplicates" => Op::Dupes {
                min_tokens: count("min_tokens", DUPES_MIN_TOKENS as u64, 100_000) as usize,
                threshold: args.get("threshold").and_then(Value::as_f64).map_or(DUPES_THRESHOLD, |t| t as f32),
                limit: count("limit", DUPES_LIMIT as u64, 500) as usize,
            },
            "structural_search" => Op::Tsquery { lang: text("lang")?, query: text("query")?, limit: count("limit", TSQUERY_LIMIT as u64, 5000) as usize },
            "reindex" => Op::Reindex,
            _ => bail!("unknown tool {name}"),
        })
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
        "read_code" => obj(json!({"text": ty("string"), "definitions": list(span("qualname"))}), &[]),
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
    let complete =
        json!({"type": "boolean", "description": "false when the answer is larger than max_bytes: result is then null and the text holds the first part"});
    obj(json!({"schema": ty("integer"), "complete": complete, "result": {"anyOf": [result, ty("null")]}}), &["schema", "complete", "result"])
}

pub enum Answer {
    Hits {
        hits: Vec<Hit>,
        words: Vec<String>,
        snippet: bool,
    },
    Callees {
        resolved: Vec<Hit>,
        external: Vec<String>,
    },
    /// `stopped_at`: the limit the search stopped at, when it did.
    Grep {
        groups: Vec<RefGroup>,
        stopped_at: Option<u64>,
    },
    Read(String),
    /// Whole definitions, each with its numbered lines.
    Definitions(Vec<(ChunkRow, String)>),
    Outline {
        path: String,
        rows: Vec<ChunkRow>,
    },
    Deps {
        path: String,
        deps: Vec<Dependency>,
    },
    Rdeps {
        target: String,
        rows: Vec<(String, i64, String)>,
    },
    Refs {
        name: String,
        refs: References,
    },
    /// `only`: the one definition asked for with `def`.
    Resolved {
        name: String,
        refs: Resolved,
        only: Option<i64>,
    },
    Similar(Vec<(Hit, f32, f32)>),
    Dupes(Vec<DupGroup>),
    Captures(Vec<Capture>),
    Reindexed(UpdateStats),
}

/// Run one operation on the index as it is; `Repo::run` brings the index up to date first.
pub fn run(s: &mut Searcher, op: &Op) -> Result<Answer> {
    Ok(match op {
        Op::Search { query, alt, k } => {
            let mut qs = vec![query.clone()];
            qs.extend(alt.iter().filter(|a| !a.is_empty() && *a != query).cloned());
            let hits = if qs.len() == 1 { s.search(query, *k, true)? } else { s.search_many(&qs, *k, true)? };
            Answer::Hits { hits, words: qs.iter().flat_map(|q| query_words(q)).collect(), snippet: true }
        }
        Op::Grep { pattern, max_results } => {
            let o = crate::instant::Options {
                pattern: pattern.clone(),
                mode: crate::instant::Mode::Regex,
                case: crate::instant::Case::Sensitive,
                max_results: Some(*max_results),
                ..crate::instant::Options::default()
            };
            let (groups, stopped) = crate::intel::grouped_grep(s, &o)?;
            Answer::Grep { groups, stopped_at: stopped.then_some(*max_results) }
        }
        Op::Symbol { name, limit } => Answer::Hits { hits: s.find_symbol(&named(s, name)?, *limit)?, words: Vec::new(), snippet: true },
        Op::Callers { name, limit } => Answer::Hits { hits: s.callers(&named(s, name)?, *limit)?, words: Vec::new(), snippet: false },
        Op::Callees { name, limit } => {
            let (resolved, external) = s.callees(&named(s, name)?, *limit)?;
            Answer::Callees { resolved, external }
        }
        Op::Read { path, symbol: Some(symbol), .. } => {
            let mut rows = match crate::intel::location(symbol) {
                Some((path, line)) => s.ix.chunk_at(path, line)?.into_iter().collect(),
                None => {
                    let hits = s.find_symbol(symbol, READ_DEFINITIONS)?;
                    hits.iter().filter_map(|h| s.ix.chunk(h.id).transpose()).collect::<Result<Vec<_>>>()?
                }
            };
            if let Some(path) = path {
                rows.retain(|r| r.path == path.trim_start_matches("./"));
            }
            if rows.is_empty() {
                bail!("no definition matches {symbol}");
            }
            let read = |r: ChunkRow| {
                let mut text = s.read(&r.path, r.start, Some(r.end), READ_LINES);
                if r.end - r.start + 1 > READ_LINES {
                    text.push_str(&format!("\n(the definition goes on to line {}: read_code path={} start={})", r.end, r.path, r.start + READ_LINES));
                }
                (r, text)
            };
            Answer::Definitions(rows.into_iter().map(read).collect())
        }
        Op::Read { path, start, end, .. } => {
            let text = s.read(path.as_deref().unwrap_or(""), *start, *end, READ_LINES);
            match text.strip_prefix("error: ") {
                Some(e) => bail!("{e}"),
                None => Answer::Read(text),
            }
        }
        Op::Outline { path } => Answer::Outline { path: path.clone(), rows: crate::intel::outline(&s.ix, path)? },
        Op::Deps { path } => Answer::Deps { path: path.clone(), deps: crate::intel::deps(&s.ix, path)? },
        Op::Rdeps { target } => Answer::Rdeps { target: target.clone(), rows: crate::intel::rdeps(&s.ix, target)? },
        Op::Refs { name, max_results, textual: true, .. } => Answer::Refs { name: name.clone(), refs: crate::intel::references(s, name, *max_results)? },
        Op::Refs { name, max_results, def, .. } => {
            let refs = crate::resolve::resolved_references(s, name, *max_results)?;
            let only = match def {
                Some(spec) => Some(definition_at(&refs, name, spec)?),
                None => None,
            };
            Answer::Resolved { name: name.clone(), refs, only }
        }
        Op::Similar { target, k } => Answer::Similar(crate::intel::similar(s, target, *k)?),
        Op::Dupes { min_tokens, threshold, limit } => Answer::Dupes(crate::intel::dupes(&s.ix, *min_tokens, *threshold, *limit)?),
        Op::Tsquery { lang, query, limit } => Answer::Captures(crate::intel::tsquery(lang, query, std::slice::from_ref(&s.ix.root), *limit, Some(&s.ix.root))?),
        Op::Reindex => Answer::Reindexed(s.ix.refresh(0.0)?.unwrap_or_default()),
    })
}

/// What a caller meant by `name`: itself, or the symbol at the location it gave (the
/// `path:start-end` an earlier answer printed), so one of several same-named definitions can be
/// followed up.
fn named(s: &Searcher, name: &str) -> Result<String> {
    match crate::intel::location(name) {
        Some((path, line)) => match s.ix.chunk_at(path, line)? {
            Some(c) => Ok(symbol_name(&c).to_string()),
            None => bail!("no symbol at {name}"),
        },
        None => Ok(name.to_string()),
    }
}

/// The definition of `name` that `spec` (a path or a location) points at: the innermost one
/// around the line, or the first in the file.
fn definition_at(refs: &Resolved, name: &str, spec: &str) -> Result<i64> {
    let (path, line) = match crate::intel::location(spec) {
        Some((path, line)) => (path, Some(line)),
        None => (spec.trim_start_matches("./"), None),
    };
    refs.definitions
        .iter()
        .filter(|d| d.path == path && line.is_none_or(|l| d.start <= l && l <= d.end))
        .min_by_key(|d| (line.is_some().then_some(d.end - d.start), d.start))
        .map(|d| d.id)
        .ok_or_else(|| anyhow!("no definition of {name} in {spec}"))
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
}

impl Answer {
    /// Nothing was found (the CLI's exit code 1).
    pub fn is_empty(&self) -> bool {
        match self {
            Answer::Hits { hits, .. } => hits.is_empty(),
            Answer::Callees { resolved, external } => resolved.is_empty() && external.is_empty(),
            Answer::Grep { groups, .. } => groups.is_empty(),
            Answer::Read(text) => text.is_empty(),
            Answer::Definitions(defs) => defs.is_empty(),
            Answer::Outline { rows, .. } => rows.is_empty(),
            Answer::Deps { deps, .. } => deps.is_empty(),
            Answer::Rdeps { rows, .. } => rows.is_empty(),
            Answer::Refs { refs, .. } => refs.is_empty(),
            Answer::Resolved { refs, only, .. } => match only {
                Some(_) => false,
                None => refs.definitions.is_empty() && refs.occurrences.is_empty(),
            },
            Answer::Similar(v) => v.is_empty(),
            Answer::Dupes(v) => v.is_empty(),
            Answer::Captures(v) => v.is_empty(),
            Answer::Reindexed(_) => false,
        }
    }

    /// The answer as text: one item per line or block, and a sentence when there is nothing.
    pub fn text(&self) -> String {
        self.text_within(usize::MAX, 0)
    }

    /// The answer as text of about `max_bytes` at most: whole items from item `offset` on, then
    /// a line saying how many are left and how to ask for them. A harness cuts a tool result that
    /// is too long for it (Codex at 10,000 tokens, from the middle); cutting here keeps whole
    /// items and tells the agent what to do next.
    pub fn text_within(&self, max_bytes: usize, offset: usize) -> String {
        let Parts { head, items, tail, noun } = self.parts();
        let mut out = head;
        let push = |out: &mut String, part: &str| {
            if !out.is_empty() && !part.is_empty() {
                out.push('\n');
            }
            out.push_str(part);
        };
        let mut shown = 0;
        for item in items.iter().skip(offset) {
            if shown > 0 && out.len() + item.len() + 1 > max_bytes {
                break;
            }
            push(&mut out, item);
            shown += 1;
            if out.len() > max_bytes {
                // one item larger than the whole budget: keep its start
                let mut cut = max_bytes;
                while !out.is_char_boundary(cut) {
                    cut -= 1;
                }
                out.truncate(cut);
                push(&mut out, "(this item is cut: pass a larger max_bytes to see all of it)");
                break;
            }
        }
        let next = offset.min(items.len()) + shown;
        if next < items.len() {
            push(&mut out, &format!("({} more {noun} not shown: call again with offset={next}, or narrow the request)", items.len() - next));
        } else {
            if offset > 0 && shown == 0 {
                push(&mut out, &format!("(no more {noun}: {} in all)", items.len()));
            }
            push(&mut out, &tail);
        }
        out
    }

    fn parts(&self) -> Parts {
        let lines = |text: String, noun: &'static str| Parts { items: text.lines().map(str::to_string).collect(), noun, ..Parts::default() };
        let nothing = |sentence: String| Parts { head: sentence, ..Parts::default() };
        match self {
            Answer::Hits { hits, .. } if hits.is_empty() => nothing("(no results)".to_string()),
            Answer::Hits { hits, words, snippet } => Parts { items: hit_blocks(hits, words, *snippet, false), noun: "results", ..Parts::default() },
            Answer::Callees { resolved, external } => Parts {
                head: if resolved.is_empty() { "(no results)".to_string() } else { String::new() },
                items: hit_blocks(resolved, &[], false, false),
                tail: if external.is_empty() {
                    String::new()
                } else {
                    format!("external: {}", external.iter().take(EXTERNAL_CALLEES).cloned().collect::<Vec<_>>().join(", "))
                },
                noun: "callees",
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
            Answer::Read(text) => lines(text.clone(), "lines"),
            Answer::Definitions(defs) => Parts {
                items: defs.iter().map(|(c, text)| format!("{}:{}-{}  [{}] {}\n{text}", c.path, c.start, c.end, c.kind, symbol_name(c))).collect(),
                noun: "definitions",
                ..Parts::default()
            },
            Answer::Outline { path, rows } if rows.is_empty() => nothing(format!("no symbols in {path} (not indexed or no definitions)")),
            Answer::Outline { rows, .. } => Parts {
                items: rows
                    .iter()
                    .map(|r| format!("{:>5}-{:<5} {}{:<9} {}", r.start, r.end, "  ".repeat(r.qualname.matches('.').count()), r.kind, r.qualname))
                    .collect(),
                noun: "symbols",
                ..Parts::default()
            },
            Answer::Deps { path, deps } if deps.is_empty() => nothing(format!("no imports recorded for {path}")),
            Answer::Deps { path, deps } => Parts {
                items: deps
                    .iter()
                    .map(|d| format!("{}:{}  {}{}", path, d.line, d.module, d.resolved.as_ref().map(|r| format!("  -> {r}")).unwrap_or_default()))
                    .collect(),
                noun: "imports",
                ..Parts::default()
            },
            Answer::Rdeps { target, rows } if rows.is_empty() => nothing(format!("nothing imports {target}")),
            Answer::Rdeps { rows, .. } => {
                Parts { items: rows.iter().map(|(p, l, m)| format!("{p}:{l}  {m}")).collect(), noun: "importers", ..Parts::default() }
            }
            Answer::Refs { refs, .. } => lines(crate::intel::references_render(refs), "lines"),
            Answer::Resolved { refs, only, .. } => lines(crate::resolve::resolved_text(refs, *only), "lines"),
            Answer::Similar(v) if v.is_empty() => nothing("(no results)".to_string()),
            Answer::Similar(v) => Parts {
                items: v.iter().map(|(h, cos, jac)| format!("{:.3} cos  {:.2} jaccard  {}  [{}] {}", cos, jac, h.loc(), h.kind, h.qualname)).collect(),
                noun: "results",
                ..Parts::default()
            },
            Answer::Dupes(groups) if groups.is_empty() => nothing("no duplicates found".to_string()),
            Answer::Dupes(groups) => Parts {
                items: groups
                    .iter()
                    .map(|g| {
                        let mut out = format!("{} ({} tokens, similarity {:.2})", g.kind, g.tokens, g.similarity);
                        for (p, q, a, b) in &g.members {
                            out.push_str(&format!("\n    {p}:{a}-{b}  {q}"));
                        }
                        out
                    })
                    .collect(),
                noun: "groups",
                ..Parts::default()
            },
            Answer::Captures(caps) if caps.is_empty() => nothing("no matches".to_string()),
            Answer::Captures(caps) => Parts {
                items: caps.iter().map(|c| format!("{}:{}:{}  @{} ({})  {}", c.path, c.line, c.column, c.capture, c.kind, c.text)).collect(),
                noun: "matches",
                ..Parts::default()
            },
            Answer::Reindexed(st) => nothing(format!("{} files changed, {} removed, {} chunks", st.files_changed, st.files_removed, st.chunks)),
        }
    }

    /// The answer for a client that reads structured results, sent beside a text of `text_len`
    /// bytes that starts at item `offset`: the whole JSON form when both together fit one and a
    /// half times `max_bytes`, else `null` (the text then pages through the answer).
    pub fn structured(&self, text_len: usize, max_bytes: usize, offset: usize) -> Value {
        let result = self.json();
        let complete = offset == 0 && text_len + result.to_string().len() <= max_bytes.saturating_add(max_bytes / 2);
        json!({"schema": SCHEMA, "complete": complete, "result": if complete { result } else { Value::Null }})
    }

    /// The answer as JSON, the shape `--json` prints.
    pub fn json(&self) -> Value {
        match self {
            Answer::Hits { hits, .. } => hits_json(hits),
            Answer::Callees { resolved, external } => json!({"resolved": hits_json(resolved), "external": external}),
            Answer::Grep { groups, stopped_at } => json!({
                "schema": SCHEMA,
                "groups": groups.iter().map(|g| json!({
                    "symbol": g.symbol.as_ref().map(|c| json!({"path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "name": symbol_name(c)})),
                    "path": g.path,
                    "lines": g.lines.iter().map(|h| json!({"line": h.line, "text": h.text})).collect::<Vec<_>>(),
                })).collect::<Vec<_>>(),
                "stopped_at": stopped_at,
            }),
            Answer::Read(text) => json!({"text": text}),
            Answer::Definitions(defs) => json!({
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
            Answer::Refs { name, refs } => json!({
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
            Answer::Resolved { name, refs, only } => {
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
            Answer::Dupes(groups) => serde_json::to_value(groups).unwrap_or_default(),
            Answer::Captures(caps) => serde_json::to_value(caps).unwrap_or_default(),
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
        assert!(matches!(op("reindex", json!({})), Op::Reindex));
    }

    #[test]
    fn limits_are_clamped_and_bad_calls_are_errors() {
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": 100_000})).unwrap(), Op::Search { k: 100, .. }));
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": 0})).unwrap(), Op::Search { k: 1, .. }));
        assert!(matches!(Op::from_tool("search_code", &json!({"query": "q", "k": -3})).unwrap(), Op::Search { k: SEARCH_K, .. }));
        assert!(matches!(Op::from_tool("references", &json!({"name": "n", "max_results": 1_000_000})).unwrap(), Op::Refs { max_results: 5000, .. }));
        assert_eq!(Op::from_tool("callers", &json!({})).err().unwrap().to_string(), "missing name");
        assert_eq!(Op::from_tool("no_such_tool", &json!({})).err().unwrap().to_string(), "unknown tool no_such_tool");
        assert!(read_only("search_code") && read_only("read_code") && !read_only("reindex"));
    }

    #[test]
    fn a_long_answer_is_cut_at_whole_items_and_says_how_to_continue() {
        let hit = |i: usize| Hit { path: format!("src/f{i}.rs"), start: 1, end: 9, name: format!("f{i}"), kind: "function".into(), ..Hit::default() };
        let answer = Answer::Hits { hits: (0..5).map(hit).collect(), words: vec![], snippet: false };
        let all = answer.text();
        assert_eq!(all.lines().count(), 5);
        assert_eq!(answer.text_within(usize::MAX, 0), all);
        // room for two items of 30 bytes each, not three
        let cut = answer.text_within(70, 0);
        assert_eq!(
            cut.lines().collect::<Vec<_>>(),
            ["src/f0.rs:1-9  [function] f0", "src/f1.rs:1-9  [function] f1", "(3 more results not shown: call again with offset=2, or narrow the request)",]
        );
        let rest = answer.text_within(usize::MAX, 2);
        assert_eq!(rest.lines().count(), 3);
        assert!(rest.starts_with("src/f2.rs"));
        assert_eq!(answer.text_within(70, 5), "(no more results: 5 in all)");
        // an item larger than the budget keeps its start
        let one = Answer::Read("x".repeat(500));
        let cut = one.text_within(100, 0);
        assert!(cut.starts_with(&"x".repeat(100)) && cut.contains("this item is cut"), "{cut}");
    }

    #[test]
    fn an_empty_answer_says_so_in_words() {
        assert_eq!(Answer::Outline { path: "a.py".into(), rows: vec![] }.text(), "no symbols in a.py (not indexed or no definitions)");
        assert_eq!(Answer::Rdeps { target: "m".into(), rows: vec![] }.text(), "nothing imports m");
        assert_eq!(Answer::Hits { hits: vec![], words: vec![], snippet: true }.text(), "(no results)");
        assert!(Answer::Dupes(vec![]).is_empty() && Answer::Captures(vec![]).is_empty());
        assert_eq!(
            Answer::Rdeps { target: "m".into(), rows: vec![("a.py".into(), 3, "m".into())] }.json(),
            json!([{"path": "a.py", "line": 3, "module": "m"}])
        );
    }
}
