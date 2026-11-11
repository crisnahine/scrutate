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
use crate::search::{dotted, Hit, Searcher};
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
    /// `force`: read every file again, also one whose size and modification time are unchanged.
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
            json!({"text": ty("string"), "start": ty("integer"), "end": ty("integer"), "file_lines": ty("integer"), "definitions": list(span("qualname"))}),
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
    let complete =
        json!({"type": "boolean", "description": "false when the answer is larger than max_bytes: result is then null and text holds the part that fits"});
    obj(
        json!({"schema": ty("integer"), "complete": complete, "result": {"anyOf": [result, ty("null")]}, "text": ty("string")}),
        &["schema", "complete", "result"],
    )
}

pub enum Answer {
    Hits {
        hits: Vec<Hit>,
        words: Vec<String>,
        snippet: bool,
    },
    /// `undecided`: callers kept although the resolver could not tell whether they call this
    /// definition or another of the same name.
    Callers {
        hits: Vec<Hit>,
        undecided: usize,
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
    /// Numbered lines of `path` from line `start` on, and how many lines the file has.
    Read {
        path: String,
        lines: Vec<String>,
        start: i64,
        total: i64,
    },
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
                // the files the index holds: hidden ones too, none of the directories it skips
                hidden: true,
                globs: crate::files::skipped_dirs(&s.ix.root).iter().map(|d| format!("!{d}")).collect(),
                ..crate::instant::Options::default()
            };
            let (groups, stopped) = crate::intel::grouped_grep(s, &o)?;
            Answer::Grep { groups, stopped_at: stopped.then_some(*max_results) }
        }
        Op::Symbol { name, limit } => Answer::Hits { hits: targets(s, name, *limit)?, words: Vec::new(), snippet: true },
        Op::Callers { name, limit } => {
            let (hits, undecided) = callers(s, name, *limit)?;
            Answer::Callers { hits, undecided }
        }
        Op::Callees { name, limit } => {
            let (resolved, external) = callees(s, name, *limit)?;
            Answer::Callees { resolved, external }
        }
        Op::Read { path, symbol: Some(symbol), .. } => {
            let mut rows = match crate::intel::location(symbol) {
                Some((path, line)) => s.ix.chunk_at(&repo_path(s, path), line)?.into_iter().collect(),
                None => {
                    let hits = s.find_symbol(symbol, READ_DEFINITIONS)?;
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
            Answer::Definitions(rows.into_iter().map(read).collect())
        }
        Op::Read { path, start, end, .. } => {
            let path = path.as_deref().unwrap_or("");
            // a path copied from an answer has its control characters written as escapes
            let read_back = crate::fmt::unshown(path);
            let path = if read_back != path && !s.ix.root.join(path).exists() { read_back.as_ref() } else { path };
            let start = (*start).max(1);
            let (lines, total) = s.read_lines(path, start, end.unwrap_or(start.saturating_add(READ_LINES - 1)))?;
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
            Answer::Rdeps { rows: crate::intel::rdeps(&s.ix, &target)?, target }
        }
        Op::Refs { name, max_results, textual: true, .. } => {
            let name = &dotted(name);
            Answer::Refs { name: name.clone(), refs: crate::intel::references(s, name, *max_results)?, max_results: *max_results }
        }
        Op::Refs { name, max_results, def, .. } => {
            let name = &dotted(name);
            let refs = crate::resolve::resolved_references(s, name, *max_results)?;
            let only = match def {
                Some(spec) => Some(definition_at(s, &refs, name, spec)?),
                None => None,
            };
            Answer::Resolved { name: name.clone(), refs, only, max_results: *max_results }
        }
        Op::Similar { target, k } => Answer::Similar(crate::intel::similar(s, target, *k)?),
        Op::Dupes { min_tokens, threshold, limit } => Answer::Dupes(crate::intel::dupes(&s.ix, *min_tokens, *threshold, *limit)?),
        Op::Tsquery { lang, query, limit } => Answer::Captures(crate::intel::tsquery(lang, query, std::slice::from_ref(&s.ix.root), *limit, Some(&s.ix.root))?),
        Op::Reindex { force } => Answer::Reindexed(s.ix.update(*force)?),
    })
}

/// `path` as the index spells it, when it names a repository file in a spelling `read_code`
/// accepts (`./a.py`, `pkg/../a.py`, another case on a case-insensitive file system); else as given.
fn repo_path(s: &Searcher, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in path.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            seg => parts.push(seg),
        }
    }
    let plain = parts.join("/");
    let indexed = |p: &str| s.ix.db.query_row("SELECT 1 FROM files WHERE path=?1", [p], |_| Ok(())).is_ok();
    if path.is_empty() || indexed(&plain) {
        return plain;
    }
    // a path copied from an answer has its control characters written as escapes
    let read_back = crate::fmt::unshown(&plain);
    if read_back != plain && indexed(&read_back) {
        return read_back.into_owned();
    }
    std::fs::canonicalize(s.ix.root.join(path))
        .ok()
        .filter(|p| p.is_file())
        .and_then(|p| p.strip_prefix(&s.ix.root).ok().map(|r| r.to_string_lossy().replace('\\', "/")))
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

/// Languages whose code calls each other's definitions directly; `None` for a file of no known
/// language, which may call or be called by any.
fn family(path: &str) -> Option<&'static str> {
    Some(match crate::langs::spec_for(path)?.id {
        "javascript" | "typescript" | "tsx" => "js",
        "c" | "cpp" | "cuda" | "objc" => "c",
        "java" | "kotlin" | "scala" | "groovy" => "jvm",
        "erlang" | "elixir" | "gleam" => "beam",
        "ocaml" | "ocaml_interface" => "ocaml",
        "csharp" | "fsharp" => "dotnet",
        id => id,
    })
}

fn same_family(a: &str, b: &str) -> bool {
    match (family(a), family(b)) {
        (Some(x), Some(y)) => x == y,
        _ => true,
    }
}

/// What the resolver makes of the occurrences of `name` in lines `start..=end` of `path`: the
/// definitions they resolve to, and whether one stayed unresolved (or none was found).
fn resolve_in(rv: &mut Resolver, s: &Searcher, path: &str, (start, end): (i64, i64), name: &str) -> (HashSet<i64>, bool) {
    let mut got = HashSet::new();
    let Some(src) = crate::resolve::repo_text(&s.ix.root, path) else { return (got, true) };
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut undecided = false;
    let mut seen = false;
    for (i, line) in src.lines().enumerate().skip((start.max(1) - 1) as usize).take((end - start + 1).max(0) as usize) {
        for (col, _) in line.match_indices(name) {
            if ident(line[..col].chars().next_back()) || ident(line[col + name.len()..].chars().next()) {
                continue;
            }
            seen = true;
            match rv.resolve_at(path, i as u64 + 1, col, name) {
                Ok((id, _)) => {
                    got.insert(id);
                }
                // an import from outside, a local, a field or a string is decided: not this definition
                Err(why) => undecided |= matches!(why, "receiver" | "not found" | "no grammar"),
            }
        }
    }
    (got, undecided || !seen)
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
pub(crate) fn callers(s: &Searcher, name: &str, limit: usize) -> Result<(Vec<Hit>, usize)> {
    /// Call sites the resolver is asked about in one answer, likeliest first.
    const CALLERS_RESOLVED: usize = 400;
    let targets = targets(s, name, 500)?;
    let ids: HashSet<i64> = targets.iter().map(|t| t.id).collect();
    let names: std::collections::BTreeSet<&str> = targets.iter().map(|t| t.name.as_str()).collect();
    let owners: HashSet<&str> =
        targets.iter().filter_map(|t| t.qualname.rsplit_once('.').map(|(o, _)| o.rsplit('.').next().unwrap_or(o))).filter(|o| !o.is_empty()).collect();
    let packages: HashSet<&str> = targets.iter().map(|t| package(&t.path)).collect();
    let fits = |path: &str| targets.iter().any(|t| same_family(&t.path, path));
    let cols = crate::index::CHUNK_COLS.split(',').map(|c| format!("c.{c}")).collect::<Vec<_>>().join(",");
    let mut calls = s.ix.db.prepare(&format!("SELECT DISTINCT {cols} FROM refs r JOIN chunks c ON c.id=r.chunk_id WHERE r.callee=?1 ORDER BY c.is_test"))?;
    let mut namesakes = s.ix.db.prepare("SELECT id, path FROM chunks WHERE name=?1 AND kind != 'window'")?;
    let (mut out, mut unsure_rows, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
    let mut rv = Resolver::new(&s.ix)?;
    let names_owner = |text: &str| owners.iter().any(|o| has_word(text, o));
    let likely = |r: &ChunkRow| (!names_owner(&r.text), !packages.contains(package(&r.path)), r.path.clone(), r.start);
    let mut asked = 0;
    for n in names {
        let rows: Vec<(i64, String)> = namesakes.query_map([n], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
        // with another definition of the name about, the likeliest call sites are asked first
        let rivals = rows.iter().any(|(id, path)| !ids.contains(id) && fits(path));
        let mut sites: Vec<ChunkRow> = calls.query_map([n], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        sites.retain(|r| fits(&r.path) && seen.insert(r.id));
        if rivals {
            sites.sort_by_cached_key(&likely);
        }
        for row in sites {
            // a common name has thousands of call sites: the likeliest are resolved, the rest stay undecided
            if asked >= CALLERS_RESOLVED {
                unsure_rows.push(row);
                continue;
            }
            asked += 1;
            let (got, unsure) = resolve_in(&mut rv, s, &row.path, (row.start, row.end), n);
            if got.is_disjoint(&ids) {
                // with no other definition of the name, a call the resolver cannot place is this one's
                if unsure && rivals {
                    unsure_rows.push(row);
                    continue;
                }
                if !unsure {
                    continue;
                }
            }
            out.push(Hit::from_row(&row, true));
            if out.len() >= limit {
                return Ok((out, 0));
            }
        }
    }
    unsure_rows.sort_by_cached_key(&likely);
    let undecided = unsure_rows.len().min(limit - out.len());
    out.extend(unsure_rows.iter().take(undecided).map(|r| Hit::from_row(r, true)));
    Ok((out, undecided))
}

/// Whether `word` is in `text` where it is not part of a longer identifier.
fn has_word(text: &str, word: &str) -> bool {
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    text.match_indices(word).any(|(i, _)| !ident(text[..i].chars().next_back()) && !ident(text[i + word.len()..].chars().next()))
}

/// What the code of the definitions `name` stands for calls: definitions of the repository in
/// the same language family, and the names it calls that have none.
pub(crate) fn callees(s: &Searcher, name: &str, limit: usize) -> Result<(Vec<Hit>, Vec<String>)> {
    let kinds = crate::search::CODE_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
    let mut named = s.ix.db.prepare(&format!("SELECT {} FROM chunks WHERE name=?1 AND kind IN ({kinds}) ORDER BY is_test", crate::index::CHUNK_COLS))?;
    let mut calls = s.ix.db.prepare("SELECT DISTINCT callee FROM refs WHERE chunk_id=?1 ORDER BY callee")?;
    let mut rv = Resolver::new(&s.ix)?;
    let (mut resolved, mut external, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
    for d in targets(s, name, 5)? {
        let names: Vec<String> = calls.query_map([d.id], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        for n in names {
            let rows: Vec<ChunkRow> = named
                .query_map([&n], ChunkRow::from_row)?
                .filter(|r| r.as_ref().map_or(true, |r| same_family(&d.path, &r.path)))
                .collect::<rusqlite::Result<_>>()?;
            if rows.is_empty() {
                if !external.contains(&n) {
                    external.push(n);
                }
                continue;
            }
            let (got, unsure) = resolve_in(&mut rv, s, &d.path, (d.start, d.end), &n);
            let tied: Vec<&ChunkRow> = rows.iter().filter(|r| got.contains(&r.id)).collect();
            let picked: Vec<&ChunkRow> = if !tied.is_empty() {
                tied
            } else if unsure {
                rows.iter().take(3).collect()
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
    Ok((resolved, external))
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
    file: Option<(String, i64, i64)>,
}

const CUT_ITEM: &str = "(this item is cut: pass a larger max_bytes to see all of it)";

impl Parts {
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
            if count > 0 && last < *total {
                let path = shown(path);
                push(&mut out, &format!("(lines {}-{last} of {total} in {path}: read_code path={path} start={} for more)", first + offset as i64, last + 1));
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
    /// Nothing was found (the CLI's exit code 1).
    pub fn is_empty(&self) -> bool {
        match self {
            Answer::Hits { hits, .. } | Answer::Callers { hits, .. } => hits.is_empty(),
            Answer::Callees { resolved, external } => resolved.is_empty() && external.is_empty(),
            Answer::Grep { groups, .. } => groups.is_empty(),
            Answer::Read { lines, .. } => lines.is_empty(),
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

    /// The answer as text of `max_bytes` at most: whole items from item `offset` on, then a line
    /// saying how many are left and how to ask for them. A harness cuts a tool result that is
    /// too long for it (Codex at 10,000 tokens, from the middle); cutting here keeps whole items
    /// and tells the agent what to do next. Only a budget smaller than one item and that line
    /// together is exceeded.
    pub fn text_within(&self, max_bytes: usize, offset: usize) -> String {
        let parts = self.parts();
        let mut room = max_bytes;
        loop {
            let (out, shown, cut) = parts.render(max_bytes, room, offset);
            if out.len() <= max_bytes || room == 0 || (shown <= 1 && !cut) {
                return out;
            }
            room = room.saturating_sub(out.len() - max_bytes);
        }
    }

    fn parts(&self) -> Parts {
        let lines = |text: String, noun: &'static str| Parts { items: text.lines().map(str::to_string).collect(), noun, ..Parts::default() };
        let nothing = |sentence: String| Parts { head: sentence, ..Parts::default() };
        match self {
            Answer::Hits { hits, .. } if hits.is_empty() => nothing("(no results)".to_string()),
            Answer::Hits { hits, words, snippet } => Parts { items: hit_blocks(hits, words, *snippet, false), noun: "results", ..Parts::default() },
            Answer::Callers { hits, .. } if hits.is_empty() => nothing("(no results)".to_string()),
            Answer::Callers { hits, undecided } => Parts {
                items: hit_blocks(hits, &[], false, false),
                tail: match *undecided {
                    0 => String::new(),
                    n if n == hits.len() => "(the resolver could not tell whether any of these calls this definition or another of the same name)".to_string(),
                    n => format!("({n} of these call a definition of that name the resolver could not identify: it may be another one)"),
                },
                noun: "results",
                ..Parts::default()
            },
            Answer::Callees { resolved, external } => Parts {
                head: if resolved.is_empty() { "(no results)".to_string() } else { String::new() },
                items: hit_blocks(resolved, &[], false, false),
                tail: if external.is_empty() {
                    String::new()
                } else {
                    format!("external: {}", external.iter().take(EXTERNAL_CALLEES).cloned().collect::<Vec<_>>().join(", "))
                },
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
            Answer::Read { path, total: 0, .. } => nothing(format!("{} is empty", shown(path))),
            Answer::Read { path, lines, start, total } if lines.is_empty() => {
                nothing(format!("{} has {total} lines: start={start} is past the end", shown(path)))
            }
            Answer::Read { path, lines, start, total } => {
                Parts { items: lines.clone(), noun: "lines", file: Some((path.clone(), *start, *total)), ..Parts::default() }
            }
            Answer::Definitions(defs) => Parts {
                items: defs.iter().map(|(c, text)| format!("{}:{}-{}  [{}] {}\n{text}", shown(&c.path), c.start, c.end, c.kind, symbol_name(c))).collect(),
                noun: "definitions",
                ..Parts::default()
            },
            Answer::Outline { path, rows } if rows.is_empty() => nothing(format!("no symbols in {} (not indexed or no definitions)", echo(path))),
            Answer::Outline { rows, .. } => Parts {
                items: rows
                    .iter()
                    .map(|r| format!("{:>5}-{:<5} {}{:<9} {}", r.start, r.end, "  ".repeat(r.qualname.matches('.').count()), r.kind, r.qualname))
                    .collect(),
                noun: "symbols",
                ..Parts::default()
            },
            Answer::Deps { path, deps } if deps.is_empty() => nothing(format!("no imports recorded for {}", echo(path))),
            Answer::Deps { path, deps } => Parts {
                items: deps
                    .iter()
                    .map(|d| {
                        format!("{}:{}  {}{}", shown(path), d.line, d.module, d.resolved.as_ref().map(|r| format!("  -> {}", shown(r))).unwrap_or_default())
                    })
                    .collect(),
                noun: "imports",
                ..Parts::default()
            },
            Answer::Rdeps { target, rows } if rows.is_empty() => nothing(format!("nothing imports {}", echo(target))),
            Answer::Rdeps { rows, .. } => {
                Parts { items: rows.iter().map(|(p, l, m)| format!("{}:{l}  {m}", shown(p))).collect(), noun: "importers", ..Parts::default() }
            }
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
                    .map(|(h, cos, jac)| format!("{:.3} cos  {:.2} jaccard  {}  [{}] {}", cos, jac, crate::fmt::loc(h), h.kind, h.qualname))
                    .collect(),
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
                            out.push_str(&format!("\n    {}:{a}-{b}  {q}", shown(p)));
                        }
                        out
                    })
                    .collect(),
                noun: "groups",
                ..Parts::default()
            },
            Answer::Captures(caps) if caps.is_empty() => nothing("no matches".to_string()),
            Answer::Captures(caps) => Parts {
                items: caps.iter().map(|c| format!("{}:{}:{}  @{} ({})  {}", shown(&c.path), c.line, c.column, c.capture, c.kind, c.text)).collect(),
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
            Answer::Hits { hits, .. } | Answer::Callers { hits, .. } => hits_json(hits),
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
            Answer::Read { lines, start, total, .. } => {
                json!({"text": lines.join("\n"), "start": start, "end": start + lines.len() as i64 - 1, "file_lines": total})
            }
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
        let answer = Answer::Hits { hits: (0..5).map(hit).collect(), words: vec![], snippet: false };
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
        let one = Answer::Read { path: "a.txt".into(), lines: vec!["x".repeat(500)], start: 1, total: 1 };
        let cut = one.text_within(100, 0);
        assert!(cut.len() <= 100 && cut.starts_with(&"x".repeat(30)) && cut.ends_with(CUT_ITEM), "{cut}");
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
