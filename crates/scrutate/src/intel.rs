//! Engine B intelligence beyond ranked search: outlines, similar and duplicate code,
//! and tree-sitter structural queries.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Result};

use crate::fmt::{echo, shown};
use crate::index::{ChunkRow, Index};
use crate::search::{Hit, Searcher, CODE_KINDS};
use crate::text::{tokenize2, BODY_CAP};
use crate::tscache::Key;

/// The path and line a location names: `path:line`, or `path:start-end` as every answer prints
/// it, so a location copied from one answer can be handed to the next call.
pub fn location(target: &str) -> Option<(&str, i64)> {
    let (path, tail) = target.rsplit_once(':')?;
    let line = tail.split_once('-').map_or(tail, |(start, _)| start).parse().ok()?;
    Some((path.trim_start_matches("./"), line))
}

/// The chunk a target refers to: a location (a window's too, which has vectors like any chunk), a
/// qualified name, or a plain name (first definition).
pub fn locate(s: &Searcher, target: &str) -> Result<Option<ChunkRow>> {
    if let Some((path, line)) = location(target) {
        return s.ix.chunk_at(&crate::ops::repo_path(s, path), line);
    }
    let hits = s.find_symbol(target, 1)?;
    match hits.first() {
        Some(h) => s.ix.chunk(h.id),
        None => Ok(None),
    }
}

/// Chunks whose implementation vector is closest to the target's, with token-set Jaccard as a second opinion.
pub fn similar(s: &mut Searcher, target: &str, k: usize) -> Result<Vec<(Hit, f32, f32)>> {
    let Some(row) = locate(s, target)? else { bail!("no symbol or location matches {}", echo(target)) };
    if row.kind == "constant" {
        bail!("{} is a constant: constants have no implementation vector and are not compared", shown(&row.qualname));
    }
    let (_, b) = s.ix.lex()?.vec_of(row.id)?.ok_or_else(|| anyhow!("the index has no vectors (semantic model disabled)"))?;
    if b.iter().all(|x| *x == 0.0) {
        bail!("{} has no implementation vector (its body is too short to compare)", shown(&row.qualname));
    }
    let near = s.ix.lex()?.scan_b(&b, k.saturating_mul(3), row.id)?;
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
        let cos = cos.clamp(-1.0, 1.0); // quantized storage can push a unit dot product a hair past 1
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
    pub kind: &'static str, // "exact" (same body text up to whitespace) or "near"
    pub similarity: f32,
    pub members: Vec<(String, String, i64, i64)>, // path, qualname, start, end
    pub tokens: usize,
}

/// Duplicate and near-duplicate functions: the same body text up to whitespace, then MinHash/LSH
/// candidates verified by Jaccard similarity of word 3-gram sets (>= `threshold`; 1 = exact only).
pub fn dupes(ix: &Index, min_tokens: usize, threshold: f32, limit: usize) -> Result<Vec<DupGroup>> {
    let rows = ix.bodies(CODE_KINDS, min_tokens)?;
    let mut exact: HashMap<u64, Vec<usize>> = HashMap::new();
    // the same words in the same order: bodies that differ in operators, numbers or spacing
    let mut same_words: HashMap<u64, Vec<usize>> = HashMap::new();
    let mut word_key: Vec<u64> = Vec::with_capacity(rows.len());
    let mut sets: Vec<HashSet<u64>> = Vec::with_capacity(rows.len());
    let mut sizes: Vec<usize> = Vec::with_capacity(rows.len());
    let mut buckets: HashMap<(usize, u64), Vec<usize>> = HashMap::new();
    for (i, (_, _, _, _, _, text)) in rows.iter().enumerate() {
        // skip the signature line: duplicates differ in name but not in body
        let body = text.split_once('\n').map(|(_, b)| b).unwrap_or("");
        let toks = tokenize2(body, Some(BODY_CAP)).0;
        sizes.push(toks.len());
        // exact: the same body text up to whitespace (the word tokens drop operators and numbers)
        let mut seq = 1469598103934665603u64;
        for w in body.split_whitespace() {
            for b in w.as_bytes() {
                seq = (seq ^ *b as u64).wrapping_mul(1099511628211);
            }
            seq = (seq ^ 0x20).wrapping_mul(1099511628211);
        }
        let mut words = 1469598103934665603u64;
        for t in &toks {
            for b in t.as_bytes() {
                words = (words ^ *b as u64).wrapping_mul(1099511628211);
            }
            words = (words ^ 0x2e).wrapping_mul(1099511628211);
        }
        word_key.push(words);
        if toks.len() >= min_tokens {
            exact.entry(seq).or_default().push(i);
            same_words.entry(words).or_default().push(i);
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
    let mut in_exact: HashMap<usize, u64> = HashMap::new();
    for (&key, members) in exact.iter().filter(|(_, m)| m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort();
        ms.dedup_by(|b, a| rows[*a].1 == rows[*b].1 && rows[*a].3 <= rows[*b].4 && rows[*b].3 <= rows[*a].4);
        if ms.len() < 2 {
            continue;
        }
        in_exact.extend(ms.iter().map(|&i| (i, key)));
        out.push(DupGroup {
            kind: "exact",
            similarity: 1.0,
            tokens: sizes[ms[0]],
            members: ms.iter().map(|&i| (rows[i].1.clone(), rows[i].2.clone(), rows[i].3, rows[i].4)).collect(),
        });
    }
    // a threshold of 1 asks for exact duplicates only
    for members in same_words.values().filter(|m| threshold < 1.0 && m.len() > 1) {
        let mut ms: Vec<usize> = members.clone();
        ms.sort();
        ms.dedup_by(|b, a| rows[*a].1 == rows[*b].1 && rows[*a].3 <= rows[*b].4 && rows[*b].3 <= rows[*a].4);
        // one text for all of them is the exact group above
        if ms.len() < 2 || ms.iter().all(|i| in_exact.contains_key(i) && in_exact.get(i) == in_exact.get(&ms[0])) {
            continue;
        }
        out.push(DupGroup {
            kind: "near",
            similarity: 1.0,
            tokens: sizes[ms[0]],
            members: ms.iter().map(|&i| (rows[i].1.clone(), rows[i].2.clone(), rows[i].3, rows[i].4)).collect(),
        });
    }
    let mut seen_pairs: HashSet<(usize, usize)> = HashSet::new();
    let mut near: Vec<(f32, usize, usize)> = Vec::new();
    for (_, members) in buckets.iter().filter(|(_, m)| threshold < 1.0 && m.len() > 1 && m.len() <= 64) {
        for a in 0..members.len() {
            for b in a + 1..members.len() {
                let (i, j) = (members[a].min(members[b]), members[a].max(members[b]));
                if i == j || word_key[i] == word_key[j] || !seen_pairs.insert((i, j)) {
                    continue;
                }
                // a container and the symbol nested inside it share text: not a duplicate
                if rows[i].1 == rows[j].1 && rows[i].3 <= rows[j].4 && rows[j].3 <= rows[i].4 {
                    continue;
                }
                let inter = sets[i].intersection(&sets[j]).count() as f32;
                let union = sets[i].union(&sets[j]).count().max(1) as f32;
                let jac = inter / union;
                if jac >= threshold {
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
    // members last: the exact groups come from a hash map, and a cut must not depend on its order
    out.sort_by(|a, b| b.tokens.cmp(&a.tokens).then(b.similarity.partial_cmp(&a.similarity).unwrap()).then_with(|| a.members.cmp(&b.members)));
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
/// With `as_given`, paths are printed as `grep PATH...` prints them (the root as given, then the
/// path below it), except under `.`; without, from the root that holds them.
/// Returns the number of captures delivered (stops at `limit`).
pub fn tsquery_each(
    lang: &str,
    query: &str,
    roots: &[PathBuf],
    as_given: bool,
    limit: usize,
    cache_root: Option<&Path>,
    on_capture: &mut dyn FnMut(Capture),
) -> Result<usize> {
    use rayon::prelude::*;
    let spec = crate::langs::by_id(lang).ok_or_else(|| anyhow!("unknown language {} (see `scrutate languages`)", echo(lang)))?;
    if query.trim().is_empty() {
        bail!("empty query: give a tree-sitter pattern with a @capture, such as (identifier) @name");
    }
    let language = (spec.language)();
    let q = tree_sitter::Query::new(&language, query).map_err(|e| anyhow!("bad query: {e}"))?;
    if q.capture_names().is_empty() {
        bail!("the query has no @capture, so it can match nothing to report: add one, such as (identifier) @name");
    }
    // predicates tree-sitter does not evaluate itself would silently match everything
    if let Some(p) = (0..q.pattern_count()).flat_map(|i| q.general_predicates(i)).next() {
        bail!("unknown predicate #{} (supported: #eq?, #match?, #any-of? and their not- and any- forms)", p.operator);
    }
    let names: Vec<String> = q.capture_names().iter().map(|n| n.to_string()).collect();
    // (path, path from its root, path shown, the code index's hash of it when its stat is current)
    let mut files: Vec<(PathBuf, String, String, Option<String>)> = Vec::new();
    // the code index's record saves reading a file whose captures are cached
    let recorded = cache_root.map(crate::stores::recorded).unwrap_or_default();
    let top = cache_root.filter(|_| !recorded.is_empty()).and_then(|r| std::fs::canonicalize(r).ok());
    for root in roots {
        let under = top.as_ref().and_then(|t| std::fs::canonicalize(root).ok()?.strip_prefix(t).ok().map(|p| crate::instant::slashed(p.to_string_lossy())));
        let mut wb = ignore::WalkBuilder::new(root);
        wb.hidden(true).git_ignore(true).add_custom_ignore_filename(crate::instant::RGIGNORE).sort_by_file_path(|a, b| a.cmp(b));
        for entry in wb.build().flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let Some(md) = entry.metadata().ok().filter(|m| m.len() <= crate::index::MAX_FILE_BYTES) else { continue };
            let path = entry.path();
            let rel = crate::instant::slashed(path.strip_prefix(root).unwrap_or(path).to_string_lossy());
            // a root that is the file itself: report it as given (the extension check needs a name)
            let rel = if rel.is_empty() { crate::instant::slashed(root.to_string_lossy()) } else { rel };
            if !crate::parse::is_indexable(&rel) {
                continue;
            }
            let ext_ok = spec.exts.iter().any(|e| rel.ends_with(e)) || spec.files.iter().any(|f| rel.rsplit('/').next() == Some(f));
            if ext_ok || spec.exts.is_empty() {
                let shown = if as_given && root.components().any(|c| c != std::path::Component::CurDir) {
                    crate::instant::slashed(path.to_string_lossy())
                } else {
                    rel.clone()
                };
                let known = under.as_ref().and_then(|u| {
                    let (stat, hash) = recorded.get(&if u.is_empty() { rel.clone() } else { format!("{u}/{rel}") })?;
                    (crate::index::Stat::of(&md) == *stat).then(|| hash.clone())
                });
                files.push((path.to_path_buf(), rel, shown, known));
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
                |(parser, ok, tx), (i, (path, rel, shown, known))| {
                    if stop_ref.load(std::sync::atomic::Ordering::Relaxed) {
                        return;
                    }
                    let mut out = Vec::new();
                    let mut key = None;
                    let mut complete = true;
                    let hit = |key: Option<Key>| {
                        key.and_then(|k| cached.and_then(|c| c.get(&k))).map(|h| h.iter().map(|c| Capture { path: shown.clone(), ..c.clone() }).collect())
                    };
                    // an entry is stored only for content of this language, so a hit needs no read
                    let k = known.as_deref().and_then(|h| cached.map(|_| crate::tscache::key(h, rel)));
                    if let Some(caps) = hit(k) {
                        let _ = tx.send((i, k, caps, true));
                        return;
                    }
                    if *ok {
                        let raw = std::fs::read(path).ok();
                        if let Some((raw, src)) = raw.as_deref().and_then(|raw| Some((raw, crate::index::decode_source(raw)?.into_owned()))) {
                            if crate::parse::spec_for(rel, &src).map(|s| s.id) == Some(spec.id) {
                                key = cached.map(|_| crate::tscache::key(&crate::index::content_hash(raw), rel));
                                if let Some(caps) = hit(key) {
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
                                                path: shown.clone(),
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
    tsquery_each(lang, query, roots, false, limit, cache_root, &mut |c| out.push(c))?;
    Ok(out)
}

/// One group of identifier occurrences inside the same enclosing symbol (or the same file when none).
#[derive(Debug, Clone)]
pub struct RefGroup {
    pub symbol: Option<ChunkRow>,
    pub path: String,
    pub lines: Vec<crate::instant::Line>,
    /// a note on the file after its last group: a NUL byte stopped its search
    pub note: Option<String>,
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

/// Engine A matches and notes in the files a search of the repository covers
/// (`files::repository_files`), in path and line order. It stops when `cancel` is set; the flag
/// returned says the search stopped early.
pub fn occurrences(
    root: &std::path::Path,
    q: &crate::instant::Query,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(Vec<crate::instant::Record>, bool)> {
    occurrences_in(root, &crate::files::repository_files(root), q, cancel)
}

/// `occurrences` in the file set `files` of the repository at `root`; a walk of paths as given
/// (a directory of the repository) names its files from `root`, as the index does.
pub fn occurrences_in(
    root: &std::path::Path,
    files: &crate::instant::Files,
    q: &crate::instant::Query,
    cancel: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(Vec<crate::instant::Record>, bool)> {
    use crate::instant::Record;
    let mut found: Vec<Record> = Vec::new();
    let from_root = |p: &mut std::sync::Arc<str>| {
        if let Ok(rel) = std::path::Path::new(&**p).strip_prefix(root) {
            *p = crate::instant::slashed(rel.to_string_lossy()).into();
        }
    };
    let mut sink = |mut r: Record| {
        if files.paths_as_given {
            match &mut r {
                Record::Match(h) => from_root(&mut h.path),
                Record::Note { path, .. } => from_root(path),
                _ => {}
            }
        }
        if matches!(r, Record::Match(_) | Record::Note { .. }) {
            found.push(r)
        }
    };
    let stats = crate::instant::search(q, files, std::sync::Arc::clone(cancel), crate::instant::Output::Records(&mut sink))?;
    found.sort_by(|a, b| a.path().cmp(b.path()).then(a.order().cmp(&b.order())));
    Ok((found, stats.cancelled))
}

/// Matching lines (in path and line order) grouped by the symbol that encloses them; lines outside
/// any symbol form one group per file. A note on a file goes with its last group.
pub fn group_by_symbol(ix: &Index, records: Vec<crate::instant::Record>) -> Result<Vec<RefGroup>> {
    use crate::instant::Record;
    let mut groups: Vec<RefGroup> = Vec::new();
    for r in records {
        let h = match r {
            Record::Match(h) => h,
            Record::Note { path, text, .. } => {
                match groups.last_mut() {
                    Some(g) if *g.path == *path => g.note = Some(text),
                    _ => groups.push(RefGroup { symbol: None, path: path.to_string(), lines: Vec::new(), note: Some(text) }),
                }
                continue;
            }
            _ => continue,
        };
        let row = ix.chunk_at(&h.path, h.line as i64)?.filter(|r| r.kind != "window");
        match groups.last_mut() {
            Some(g) if g.symbol.as_ref().map(|x| x.id) == row.as_ref().map(|x| x.id) && *g.path == *h.path => g.lines.push(h),
            _ => groups.push(RefGroup { symbol: row, path: h.path.to_string(), lines: vec![h], note: None }),
        }
    }
    Ok(groups)
}

/// An Engine A search of the repository with its matches grouped by enclosing symbol: the grep an
/// agent asks for (MCP `grep_code`). It stops when the call is cancelled.
pub fn grouped_grep(s: &Searcher, q: &crate::instant::Query, files: &crate::instant::Files) -> Result<(Vec<RefGroup>, bool)> {
    let (found, truncated) = occurrences_in(&s.ix.root, files, q, &s.cancel)?;
    Ok((group_by_symbol(&s.ix, found)?, truncated))
}

/// One line per group and one per matching line under it.
pub fn groups_text(groups: &[RefGroup], out: &mut String) {
    for g in groups {
        out.push_str(&group_text(g));
        out.push('\n');
    }
}

/// A group as text: its symbol, then its matching lines.
pub fn group_text(g: &RefGroup) -> String {
    let mut out = match &g.symbol {
        Some(c) => format!("  {}:{}-{}  [{}] {}", shown(&c.path), c.start, c.end, c.kind, shown(if c.qualname.is_empty() { &c.name } else { &c.qualname })),
        None => format!("  {}  (no enclosing symbol)", shown(&g.path)),
    };
    for h in &g.lines {
        out.push_str(&format!("\n    {}: {}", h.line, h.text.trim_end()));
    }
    if let Some(note) = &g.note {
        out.push_str(&format!("\n    note: {note}"));
    }
    out
}

/// Definitions (index), callers (index) and every whole-word occurrence of `name` (Engine A),
/// grouped by the enclosing symbol. Works without an index too (then only occurrences, ungrouped).
pub fn references(s: &Searcher, name: &str, max_results: u64) -> Result<References> {
    let bare = bare_name(s, name)?;
    let definitions = s.ix.definitions(&bare)?;
    let callers = s.callers(name, 200)?;
    let (groups, truncated) = grouped_grep(s, &crate::resolve::name_search(&bare, max_results), &crate::files::repository_files(&s.ix.root))?;
    Ok(References { definitions, callers, groups, truncated })
}

/// Plain-text rendering of `references`.
pub fn references_render(r: &References) -> String {
    let mut out = String::new();
    if !r.definitions.is_empty() {
        out.push_str("definitions:\n");
        for d in &r.definitions {
            out.push_str(&format!("    {}:{}-{}  [{}] {}\n", shown(&d.path), d.start, d.end, d.kind, shown(&d.qualname)));
        }
    }
    if !r.callers.is_empty() {
        out.push_str("callers:\n");
        for h in &r.callers {
            out.push_str(&format!("    {}  [{}] {}\n", crate::fmt::loc(h), h.kind, shown(&h.qualname)));
        }
    }
    out.push_str(&format!("references ({} lines in {} symbols):\n", r.total_lines(), r.groups.len()));
    groups_text(&r.groups, &mut out);
    out
}

/// The name the occurrences of `name` are written with: the stored name of a definition
/// `find_symbol` finds for it that ends the typed text in NFC (`list->vec`, `#secret`, the `call` of
/// `Foo::Bar.call`), else the last part of its dotted form.
pub fn bare_name(s: &Searcher, name: &str) -> Result<String> {
    let typed = name.trim();
    let hits = s.find_symbol(typed, 50)?;
    let typed_nfc: String = unicode_normalization::UnicodeNormalization::nfc(typed).collect();
    let ends = |n: &str| typed.ends_with(n) || typed_nfc.ends_with(&unicode_normalization::UnicodeNormalization::nfc(n).collect::<String>());
    let found = hits.iter().map(|h| h.name.as_str()).filter(|n| !n.is_empty() && ends(n)).max_by_key(|n| n.len());
    Ok(match found {
        Some(n) => n.to_string(),
        None => crate::search::dotted(typed).rsplit('.').next().unwrap_or(typed).to_string(),
    })
}
