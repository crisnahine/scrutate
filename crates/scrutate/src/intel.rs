//! Engine B intelligence beyond ranked search: dependencies, outlines, similar and duplicate code,
//! and tree-sitter structural queries.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};
use rusqlite::params;

use crate::index::{ChunkRow, Index, CHUNK_COLS};
use crate::search::{Hit, Searcher, CODE_KINDS};
use crate::text::{name_key, tokenize2, BODY_CAP};
use crate::tscache::Key;

#[derive(Debug, Clone, serde::Serialize)]
pub struct Dependency {
    pub line: i64,
    pub module: String,
    /// repository file the module resolves to, when it can be found
    pub resolved: Option<String>,
}

/// Modules a file imports, resolved to repository files where the module path maps onto one.
pub fn deps(ix: &Index, path: &str) -> Result<Vec<Dependency>> {
    let mut st = ix.db.prepare_cached("SELECT line, module FROM imports WHERE path=?1 ORDER BY line")?;
    let rows: Vec<(i64, String)> = st.query_map([path], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
    let files: HashSet<String> = all_files(ix)?;
    Ok(rows.into_iter().map(|(line, module)| Dependency { resolved: resolve(&module, path, &files), line, module }).collect())
}

fn all_files(ix: &Index) -> Result<HashSet<String>> {
    let mut st = ix.db.prepare_cached("SELECT path FROM files WHERE hash != 'SKIP'")?;
    let files = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
    Ok(files)
}

const SOURCE_EXTS: &[&str] = &[
    ".py", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".go", ".rs", ".java", ".rb", ".php", ".kt", ".swift", ".scala", ".dart", ".ex", ".exs", ".hs",
    ".lua", ".c", ".h", ".cc", ".cpp", ".hpp", ".cs", ".jl", ".r", ".R", ".sh", ".zig", ".elm",
];

/// Best-effort mapping of an import string onto a file of the repository. Handles dotted/`::`-separated
/// module paths, relative paths (`./x`, `../x`, `from . import`), `crate::` roots and index files.
pub fn resolve(module: &str, from: &str, files: &HashSet<String>) -> Option<String> {
    let dir = from.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut candidates: Vec<String> = Vec::new();
    let join = |base: &str, rel: &str| -> String {
        let mut parts: Vec<&str> = if base.is_empty() { Vec::new() } else { base.split('/').collect() };
        for seg in rel.split('/') {
            match seg {
                "" | "." => {}
                ".." => {
                    parts.pop();
                }
                s => parts.push(s),
            }
        }
        parts.join("/")
    };
    if module.starts_with('.') && !module.trim_start_matches('.').is_empty() && !module.starts_with("./") && !module.starts_with("../") {
        // python relative import: `.x` / `..pkg.mod`
        let ups = module.chars().take_while(|c| *c == '.').count();
        let rest = module[ups..].replace('.', "/");
        let mut base = dir.to_string();
        for _ in 1..ups {
            base = base.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        }
        candidates.push(join(&base, &rest));
    } else if module.chars().all(|c| c == '.') {
        let mut base = dir.to_string();
        for _ in 1..module.len() {
            base = base.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default();
        }
        candidates.push(base);
    } else if module.starts_with("./") || module.starts_with("../") {
        candidates.push(join(dir, module));
    } else {
        let m = module.strip_prefix("crate::").map(|m| format!("src/{m}")).unwrap_or_else(|| module.to_string());
        let slashed = m.replace("::", "/").replace('.', "/");
        candidates.push(slashed.clone());
        // java-style: the last segment may be a class inside the file of the previous segment
        if let Some((head, _)) = slashed.rsplit_once('/') {
            candidates.push(head.to_string());
        }
        // same directory first (C includes, sibling modules)
        if !dir.is_empty() {
            candidates.insert(0, join(dir, &slashed));
        }
        candidates.push(m);
    }
    let try_one = |c: &str| -> Option<String> {
        let c = c.trim_matches('/');
        if files.contains(c) {
            return Some(c.to_string());
        }
        for ext in SOURCE_EXTS {
            let p = format!("{c}{ext}");
            if files.contains(&p) {
                return Some(p);
            }
        }
        for idx in ["__init__.py", "index.js", "index.ts", "index.tsx", "mod.rs", "lib.rs", "index.mjs"] {
            let p = if c.is_empty() { idx.to_string() } else { format!("{c}/{idx}") };
            if files.contains(&p) {
                return Some(p);
            }
        }
        None
    };
    for c in &candidates {
        if let Some(p) = try_one(c) {
            return Some(p);
        }
    }
    // the repository root may be the package itself (`networkx.utils.x` inside a checkout of `networkx/`):
    // drop leading segments while at least two remain
    if !module.starts_with('.') {
        let slashed = module.replace("::", "/").replace('.', "/");
        let segs: Vec<&str> = slashed.split('/').filter(|s| !s.is_empty()).collect();
        for i in 1..segs.len().saturating_sub(1) {
            if let Some(p) = try_one(&segs[i..].join("/")) {
                return Some(p);
            }
        }
    }
    None
}

/// Files that import `target`: a module name as written, or a repository file (every file whose
/// imports resolve to it, plus modules spelled like its path).
pub fn rdeps(ix: &Index, target: &str) -> Result<Vec<(String, i64, String)>> {
    let files = all_files(ix)?;
    let mut st = ix.db.prepare("SELECT path, line, module FROM imports ORDER BY path, line")?;
    let rows: Vec<(String, i64, String)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?;
    let is_file = files.contains(target);
    let stem = target.rsplit_once('.').map(|(s, _)| s).unwrap_or(target);
    let dotted: String = stem.replace('/', ".");
    let dotted_pkg = dotted.strip_suffix(".__init__").or_else(|| dotted.strip_suffix(".index")).or_else(|| dotted.strip_suffix(".mod")).map(|s| s.to_string());
    let mut out = Vec::new();
    for (path, line, module) in rows {
        if path == target {
            continue;
        }
        let hit = if is_file {
            resolve(&module, &path, &files).as_deref() == Some(target)
                || module == dotted
                || module.replace("::", ".") == dotted
                || dotted_pkg.as_deref().map(|p| module == p || module.replace("::", ".") == p).unwrap_or(false)
                || module.rsplit('/').next() == Some(target.rsplit('/').next().unwrap_or(target))
        } else {
            module == target
                || module.starts_with(&format!("{target}."))
                || module.starts_with(&format!("{target}::"))
                || module.starts_with(&format!("{target}/"))
                || module.ends_with(&format!(".{target}"))
                || module.ends_with(&format!("::{target}"))
                || module.ends_with(&format!("/{target}"))
        };
        if hit {
            out.push((path, line, module));
        }
    }
    Ok(out)
}

/// Symbols of a file in source order.
pub fn outline(ix: &Index, path: &str) -> Result<Vec<ChunkRow>> {
    let mut st = ix.db.prepare(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE path=?1 AND kind != 'window' ORDER BY start, end DESC"))?;
    let rows = st.query_map([path], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The chunk a target refers to: `path:line`, a qualified name, or a plain name (first definition).
pub fn locate(s: &Searcher, target: &str) -> Result<Option<ChunkRow>> {
    if let Some((p, l)) = target.rsplit_once(':') {
        if let Ok(line) = l.parse::<i64>() {
            return s.ix.chunk_at(p, line);
        }
    }
    let hits = s.find_symbol(target, 1)?;
    match hits.first() {
        Some(h) => s.ix.chunk(h.id),
        None => Ok(None),
    }
}

/// Chunks whose implementation vector is closest to the target's, with token-set Jaccard as a second opinion.
pub fn similar(s: &mut Searcher, target: &str, k: usize) -> Result<Vec<(Hit, f32, f32)>> {
    let Some(row) = locate(s, target)? else { bail!("no symbol or location matches {target:?}") };
    let (_, b) = s.ix.lex()?.vec_of(row.id).ok_or_else(|| anyhow!("the index has no vectors (semantic model disabled)"))?;
    if b.iter().all(|x| *x == 0.0) {
        bail!("{} has no implementation vector (empty body)", row.qualname);
    }
    let near = s.ix.lex()?.scan_b(&b, k * 3, row.id);
    let my_tokens: HashSet<String> = tokenize2(&row.text, Some(BODY_CAP)).0.into_iter().collect();
    let mut out = Vec::new();
    for (id, cos) in near {
        let Some(r) = s.ix.chunk(id)? else { continue };
        if !CODE_KINDS.contains(&r.kind.as_str()) && r.kind != "module" {
            continue;
        }
        let toks: HashSet<String> = tokenize2(&r.text, Some(BODY_CAP)).0.into_iter().collect();
        let inter = my_tokens.intersection(&toks).count() as f32;
        let union = my_tokens.union(&toks).count().max(1) as f32;
        let cos = cos.clamp(-1.0, 1.0); // float16 storage can push a unit dot product a hair past 1
        let mut h = Hit::from_row(&r, true);
        h.score = cos as f64;
        out.push((h, cos, inter / union));
        if out.len() >= k {
            break;
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------------- duplicates (MinHash LSH)
const HASHES: usize = 64;
const BANDS: usize = 16; // 16 bands x 4 rows: pairs with Jaccard >= 0.7 collide with probability ~0.98

fn mix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51afd7ed558ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ceb9fe1a85ec53);
    x ^= x >> 33;
    x
}

fn shingles(tokens: &[String]) -> HashSet<u64> {
    let mut out = HashSet::new();
    if tokens.len() < 3 {
        return out;
    }
    for w in tokens.windows(3) {
        let mut h = 1469598103934665603u64;
        for t in w {
            for b in t.as_bytes() {
                h = (h ^ *b as u64).wrapping_mul(1099511628211);
            }
            h = (h ^ 0x1f).wrapping_mul(1099511628211);
        }
        out.insert(h);
    }
    out
}

fn minhash(sh: &HashSet<u64>) -> [u64; HASHES] {
    let mut mh = [u64::MAX; HASHES];
    for &s in sh {
        for (i, slot) in mh.iter_mut().enumerate() {
            let v = mix64(s ^ (i as u64).wrapping_mul(0x9e3779b97f4a7c15));
            if v < *slot {
                *slot = v;
            }
        }
    }
    mh
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DupGroup {
    pub kind: &'static str, // "exact" (same token sequence) or "near"
    pub similarity: f32,
    pub members: Vec<(String, String, i64, i64)>, // path, qualname, start, end
    pub tokens: usize,
}

/// Duplicate and near-duplicate functions: exact token sequences, then MinHash/LSH candidates
/// verified by Jaccard similarity of token 3-gram sets (>= `threshold`).
pub fn dupes(ix: &Index, min_tokens: usize, threshold: f32, limit: usize) -> Result<Vec<DupGroup>> {
    let kinds = CODE_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
    let mut st = ix.db.prepare(&format!("SELECT id, path, qualname, start, end, text FROM chunks WHERE kind IN ({kinds}) AND nbody >= ?1 ORDER BY id"))?;
    let rows: Vec<(i64, String, String, i64, i64, String)> = st
        .query_map([min_tokens as i64], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default(), r.get(3)?, r.get(4)?, crate::index::unpack_text(r.get_ref(5)?)?))
        })?
        .collect::<rusqlite::Result<_>>()?;
    let mut exact: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut sets: Vec<HashSet<u64>> = Vec::with_capacity(rows.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(rows.len());
    let mut buckets: HashMap<(usize, u64), Vec<usize>> = HashMap::new();
    for (i, (_, _, _, _, _, text)) in rows.iter().enumerate() {
        // skip the signature line: duplicates differ in name but not in body
        let body = text.split_once('\n').map(|(_, b)| b).unwrap_or("");
        let toks = tokenize2(body, Some(BODY_CAP)).0;
        sizes.push(toks.len());
        let mut seq = 1469598103934665603u64;
        for t in &toks {
            for b in t.as_bytes() {
                seq = (seq ^ *b as u64).wrapping_mul(1099511628211);
            }
            seq = (seq ^ 0x2e).wrapping_mul(1099511628211);
        }
        if toks.len() >= min_tokens {
            exact.entry(seq).or_default().push(i);
        }
        let sh = shingles(&toks);
        if sh.len() >= 8 {
            let mh = minhash(&sh);
            let rows_per_band = HASHES / BANDS;
            for b in 0..BANDS {
                let mut key = 0xcbf29ce484222325u64;
                for v in &mh[b * rows_per_band..(b + 1) * rows_per_band] {
                    key = mix64(key ^ *v);
                }
                buckets.entry((b, key)).or_default().push(i);
            }
        }
        sets.push(sh);
    }
    let mut out: Vec<DupGroup> = Vec::new();
    let mut in_exact: HashSet<usize> = HashSet::new();
    for (_, members) in exact.iter().filter(|(_, m)| m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort();
        ms.dedup_by(|b, a| rows[*a].1 == rows[*b].1 && rows[*a].3 <= rows[*b].4 && rows[*b].3 <= rows[*a].4);
        if ms.len() < 2 {
            continue;
        }
        in_exact.extend(ms.iter().copied());
        out.push(DupGroup {
            kind: "exact",
            similarity: 1.0,
            tokens: sizes[ms[0]],
            members: ms.iter().map(|&i| (rows[i].1.clone(), rows[i].2.clone(), rows[i].3, rows[i].4)).collect(),
        });
    }
    let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut near: Vec<(f32, usize, usize)> = Vec::new();
    for (_, members) in buckets.iter().filter(|(_, m)| m.len() > 1 && m.len() <= 64) {
        for a in 0..members.len() {
            for b in a + 1..members.len() {
                let (i, j) = (members[a].min(members[b]), members[a].max(members[b]));
                if i == j || (in_exact.contains(&i) && in_exact.contains(&j)) || !seen_pairs.insert((i, j)) {
                    continue;
                }
                // a container and the symbol nested inside it share text: not a duplicate
                if rows[i].1 == rows[j].1 && rows[i].3 <= rows[j].4 && rows[j].3 <= rows[i].4 {
                    continue;
                }
                let inter = sets[i].intersection(&sets[j]).count() as f32;
                let union = sets[i].union(&sets[j]).count().max(1) as f32;
                let jac = inter / union;
                if jac >= threshold && jac < 1.0 {
                    near.push((jac, i, j));
                }
            }
        }
    }
    near.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap().then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    for (jac, i, j) in near.into_iter().take(limit) {
        out.push(DupGroup {
            kind: "near",
            similarity: jac,
            tokens: sizes[i].min(sizes[j]),
            members: vec![(rows[i].1.clone(), rows[i].2.clone(), rows[i].3, rows[i].4), (rows[j].1.clone(), rows[j].2.clone(), rows[j].3, rows[j].4)],
        });
    }
    out.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(b.similarity.partial_cmp(&a.similarity).unwrap()));
    out.truncate(limit);
    Ok(out)
}

// ------------------------------------------------------------------------- structural queries
#[derive(Debug, Clone, serde::Serialize)]
pub struct Capture {
    pub path: String,
    pub line: i64,
    pub column: i64,
    pub capture: String,
    pub kind: String,
    pub text: String,
}

/// Run a tree-sitter query (S-expression pattern language) over the files of one language under
/// `roots`, delivering every capture to `on_capture` in path order. No index is needed; files are
/// parsed in parallel in bounded chunks, so memory stays flat however many matches there are.
/// With `cache_root`, each file's captures are kept under `cache_root/.scrutate/tsquery/` by
/// content hash and files seen before are not parsed again (see `tscache`); the output is the same.
/// Returns the number of captures delivered (stops at `limit`).
pub fn tsquery_each(lang: &str, query: &str, roots: &[PathBuf], limit: usize, cache_root: Option<&Path>, on_capture: &mut dyn FnMut(Capture)) -> Result<usize> {
    use rayon::prelude::*;
    let spec = crate::langs::by_id(lang).ok_or_else(|| anyhow!("unknown language {lang:?} (see `scrutate languages`)"))?;
    let language = (spec.language)();
    let q = tree_sitter::Query::new(&language, query).map_err(|e| anyhow!("bad query: {e}"))?;
    let names: Vec<String> = q.capture_names().iter().map(|n| n.to_string()).collect();
    let mut files: Vec<(PathBuf, String)> = Vec::new();
    for root in roots {
        let mut wb = ignore::WalkBuilder::new(root);
        wb.hidden(true).git_ignore(true).sort_by_file_path(|a, b| a.cmp(b));
        for entry in wb.build().flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy().replace('\\', "/");
            if !crate::parse::is_indexable(&rel) {
                continue;
            }
            let ext_ok = spec.exts.iter().any(|e| rel.ends_with(e)) || spec.files.iter().any(|f| rel.rsplit('/').next() == Some(f));
            if ext_ok || spec.exts.is_empty() {
                files.push((path.to_path_buf(), rel));
            }
        }
    }
    let cache = cache_root.map(|r| crate::tscache::Cache::open(r, lang, query, roots));
    // parse in parallel, deliver in path order: workers send (file index, content hash, captures,
    // complete) through a bounded channel; the consumer reorders with a small buffer, so memory
    // stays flat and no thread waits at chunk barriers
    let (tx, rx) = std::sync::mpsc::sync_channel::<(usize, Option<Key>, Vec<Capture>, bool)>(512);
    let stop = std::sync::atomic::AtomicBool::new(false);
    let mut fresh: HashMap<Key, Vec<Capture>> = HashMap::new();
    let (mut misses, mut fresh_bytes, mut finished) = (0usize, 0u64, true);
    let delivered = std::thread::scope(|scope| -> usize {
        let files_ref = &files;
        let q_ref = &q;
        let names_ref = &names;
        let language_ref = &language;
        let stop_ref = &stop;
        let cached = cache.as_ref().map(|c| &c.entries);
        scope.spawn(move || {
            files_ref.par_iter().enumerate().for_each_init(
                || {
                    let mut parser = tree_sitter::Parser::new();
                    let ok = parser.set_language(language_ref).is_ok();
                    (parser, ok, tx.clone())
                },
                |(parser, ok, tx), (i, (path, rel))| {
                    if stop_ref.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let mut out = Vec::new();
                    let mut key = None;
                    let mut complete = true;
                    if *ok {
                        if let Ok(src) = std::fs::read_to_string(path) {
                            if crate::parse::spec_for(rel, &src).map(|s| s.id) == Some(spec.id) {
                                key = cached.map(|_| crate::tscache::hash(&[src.as_bytes()]));
                                if let Some(hit) = key.and_then(|k| cached.and_then(|c| c.get(&k))) {
                                    let caps = hit.iter().map(|c| Capture { path: rel.clone(), ..c.clone() }).collect();
                                    let _ = tx.send((i, key, caps, true));
                                    return;
                                }
                                if let Some(tree) = parser.parse(src.as_bytes(), None) {
                                    let mut cursor = tree_sitter::QueryCursor::new();
                                    cursor.set_match_limit(1 << 16);
                                    let mut matches = cursor.matches(q_ref, tree.root_node(), src.as_bytes());
                                    use tree_sitter::StreamingIterator;
                                    while let Some(m) = matches.next() {
                                        for c in m.captures {
                                            let node = c.node;
                                            let text = node.utf8_text(src.as_bytes()).unwrap_or("").lines().next().unwrap_or("").to_string();
                                            out.push(Capture {
                                                path: rel.clone(),
                                                line: node.start_position().row as i64 + 1,
                                                column: node.start_position().column as i64 + 1,
                                                capture: names_ref[c.index as usize].clone(),
                                                kind: node.kind().to_string(),
                                                text,
                                            });
                                        }
                                        if out.len() >= limit {
                                            complete = false; // a cut-off list is not the file's result
                                            break;
                                        }
                                    }
                                } else {
                                    complete = false;
                                }
                            }
                        }
                    }
                    let _ = tx.send((i, key, out, complete));
                },
            );
        });
        let mut next = 0usize;
        let mut pending: std::collections::BTreeMap<usize, (Option<Key>, Vec<Capture>, bool)> = std::collections::BTreeMap::new();
        let mut delivered = 0usize;
        for (i, key, caps, complete) in rx {
            pending.insert(i, (key, caps, complete));
            while let Some((key, caps, complete)) = pending.remove(&next) {
                next += 1;
                if let Some(k) = key {
                    if !complete {
                        finished = false;
                    } else if fresh_bytes <= crate::tscache::MAX_BYTES {
                        misses += usize::from(!cached.is_some_and(|c| c.contains_key(&k)));
                        fresh_bytes += 32 + caps.iter().map(|c| 32 + c.capture.len() + c.kind.len() + c.text.len()).sum::<usize>() as u64;
                        fresh.entry(k).or_insert_with(|| caps.iter().map(|c| Capture { path: String::new(), ..c.clone() }).collect());
                    }
                }
                for cap in caps {
                    if delivered >= limit {
                        stop.store(true, std::sync::atomic::Ordering::Relaxed);
                        break;
                    }
                    on_capture(cap);
                    delivered += 1;
                }
            }
            if delivered >= limit {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
                break;
            }
        }
        finished = finished && next == files_ref.len();
        delivered
    });
    if let Some(mut cache) = cache {
        if fresh_bytes > crate::tscache::MAX_BYTES {
            return Ok(delivered); // too large to cache: the stored entries stay as they were
        }
        let stale = finished && fresh.len() != cache.entries.len();
        if !finished {
            // files this run did not reach keep their entries
            for (k, v) in cache.entries.drain() {
                fresh.entry(k).or_insert(v);
            }
        }
        if misses > 0 || stale {
            let _ = cache.store(&fresh);
        } else {
            cache.touch();
        }
    }
    Ok(delivered)
}

/// `tsquery_each` collected into a vector (MCP and bindings; the CLI streams).
pub fn tsquery(lang: &str, query: &str, roots: &[PathBuf], limit: usize, cache_root: Option<&Path>) -> Result<Vec<Capture>> {
    let mut out = Vec::new();
    tsquery_each(lang, query, roots, limit, cache_root, &mut |c| out.push(c))?;
    Ok(out)
}

/// One group of identifier occurrences inside the same enclosing symbol (or the same file when none).
#[derive(Debug, Clone)]
pub struct RefGroup {
    pub symbol: Option<ChunkRow>,
    pub path: String,
    pub lines: Vec<crate::instant::Hit>,
}

pub struct References {
    pub definitions: Vec<ChunkRow>,
    pub callers: Vec<Hit>,
    pub groups: Vec<RefGroup>,
    pub truncated: bool,
}

impl References {
    pub fn total_lines(&self) -> usize {
        self.groups.iter().map(|g| g.lines.len()).sum()
    }
    pub fn is_empty(&self) -> bool {
        self.definitions.is_empty() && self.callers.is_empty() && self.groups.is_empty()
    }
}

/// Definitions (index), callers (index) and every whole-word occurrence of `name` (Engine A),
/// grouped by the enclosing symbol. Works without an index too (then only occurrences, ungrouped).
pub fn references(s: &Searcher, name: &str, max_results: u64) -> Result<References> {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;
    let definitions = definitions(&s.ix, name)?;
    let callers = s.callers(name, 200)?;
    let o = crate::instant::Options {
        pattern: name.rsplit('.').next().unwrap_or(name).to_string(),
        mode: crate::instant::Mode::Literal,
        word: true,
        max_results: Some(max_results),
        ..crate::instant::Options::default()
    };
    let mut hits: Vec<crate::instant::Hit> = Vec::new();
    let mut sink = |h: crate::instant::Hit| {
        if !h.context {
            hits.push(h)
        }
    };
    let stats = crate::instant::search(std::slice::from_ref(&s.ix.root), &o, Arc::new(AtomicBool::new(false)), &mut sink)?;
    hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    let mut groups: Vec<RefGroup> = Vec::new();
    for h in hits {
        let row = s.ix.chunk_at(&h.path, h.line as i64)?;
        match groups.last_mut() {
            Some(g) if g.symbol.as_ref().map(|x| x.id) == row.as_ref().map(|x| x.id) && *g.path == *h.path => g.lines.push(h),
            _ => groups.push(RefGroup { symbol: row, path: h.path.to_string(), lines: vec![h] }),
        }
    }
    Ok(References { definitions, callers, groups, truncated: stats.cancelled })
}

/// Plain-text rendering of `references` (CLI and MCP).
pub fn references_text(s: &Searcher, name: &str, max_results: u64) -> Result<String> {
    let r = references(s, name, max_results)?;
    let mut out = String::new();
    if !r.definitions.is_empty() {
        out.push_str("definitions:\n");
        for d in &r.definitions {
            out.push_str(&format!("    {}:{}-{}  [{}] {}\n", d.path, d.start, d.end, d.kind, d.qualname));
        }
    }
    if !r.callers.is_empty() {
        out.push_str("callers:\n");
        for h in &r.callers {
            out.push_str(&format!("    {}  [{}] {}\n", h.loc(), h.kind, h.qualname));
        }
    }
    out.push_str(&format!("references ({} lines in {} symbols{}):\n", r.total_lines(), r.groups.len(), if r.truncated { ", truncated" } else { "" }));
    for g in &r.groups {
        match &g.symbol {
            Some(c) => {
                out.push_str(&format!("  {}:{}-{}  [{}] {}\n", c.path, c.start, c.end, c.kind, if c.qualname.is_empty() { &c.name } else { &c.qualname }))
            }
            None => out.push_str(&format!("  {}  (no enclosing symbol)\n", g.path)),
        }
        for h in &g.lines {
            out.push_str(&format!("    {}: {}\n", h.line, h.text.trim_end()));
        }
    }
    Ok(out)
}

/// All symbols named `name` anywhere (for `refs`): definitions by key.
pub fn definitions(ix: &Index, name: &str) -> Result<Vec<ChunkRow>> {
    let bare = name.rsplit('.').next().unwrap_or(name);
    let mut st =
        ix.db.prepare(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE name_key=?1 AND kind != 'window' ORDER BY (name != ?2), is_test, path, start"))?;
    let rows = st.query_map(params![name_key(bare), bare], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
