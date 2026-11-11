//! Hybrid retrieval (port of python/scrutate_ref/search.py).
//!
//! Candidates come from three sources: field-weighted BM25 over the segment store, semantic vectors,
//! and exact name / qualified-name lookup. Each candidate gets 47 features (per-field BM25, semantic
//! similarities, exactness, literal occurrences, structure, query shape) and the learned ranker
//! orders the best 200 by a linear blend. Without a ranker the linear blend itself is used.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use anyhow::Result;
use regex::Regex;
use rusqlite::params_from_iter;

use crate::index::{ChunkRow, Index, CHUNK_COLS};
use crate::lex::tok_key;
use crate::rank::TreeEnsemble;
use crate::text::{literal_candidates, name_key, query_terms, query_wholes, Words, QUERY_STOP};

pub const PRUNE: usize = 200; // the learned ranker sees (at most) this many candidates
pub const LEX_LIMIT: usize = 100;
pub const SEM_LIMIT: usize = 100;
pub const CODE_KINDS: &[&str] = &["function", "method", "class", "struct", "trait", "interface", "type", "enum", "impl"];

pub const FEATURES: [&str; 47] = [
    "lex_log",
    "lex_ratio",
    "lex_rank",
    "lex_z",
    "name_log",
    "name_ratio",
    "qual_log",
    "qual_ratio",
    "sig_log",
    "sig_ratio",
    "doc_log",
    "doc_ratio",
    "body_log",
    "body_ratio",
    "path_log",
    "path_ratio",
    "sem_a",
    "sem_b",
    "sem_mix",
    "sem_a_z",
    "sem_b_z",
    "sem_mix_z",
    "sem_rank",
    "exact_name",
    "exact_qual",
    "name_overlap",
    "name_cover",
    "name_same_set",
    "lit_hits",
    "lit_def",
    "k_function",
    "k_method",
    "k_class",
    "k_module",
    "k_other",
    "is_test",
    "log_nbody",
    "has_doc",
    "log_ndoc",
    "qual_depth",
    "is_private",
    "log_ncallers",
    "n_terms",
    "n_lits",
    "n_words",
    "has_ident",
    "file_frac",
];
pub const NFEAT: usize = FEATURES.len();

pub const fn fi(name: &str) -> usize {
    let mut i = 0;
    while i < NFEAT {
        if const_eq(FEATURES[i], name) {
            return i;
        }
        i += 1;
    }
    panic!("unknown feature")
}

const fn const_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

pub type Features = [f32; NFEAT];

#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct Hit {
    pub id: i64,
    pub path: String,
    pub start: i64,
    pub end: i64,
    pub name: String,
    pub qualname: String,
    pub kind: String,
    #[serde(skip)]
    pub text: String,
    pub score: f64,
    pub why: Vec<(String, serde_json::Value)>,
}

impl Hit {
    pub fn loc(&self) -> String {
        format!("{}:{}-{}", self.path, self.start, self.end)
    }

    pub fn from_row(r: &ChunkRow, with_text: bool) -> Hit {
        Hit {
            id: r.id,
            path: r.path.clone(),
            start: r.start,
            end: r.end,
            name: r.name.clone(),
            qualname: r.qualname.clone(),
            kind: r.kind.clone(),
            text: if with_text { r.text.clone() } else { String::new() },
            score: 0.0,
            why: Vec::new(),
        }
    }

    /// Up to `max_lines` lines of the chunk, starting just above the first line mentioning a query word.
    pub fn snippet(&self, words: &[String], max_lines: usize) -> String {
        let lines: Vec<&str> = self.text.split('\n').collect();
        let mut at = 0usize;
        if !words.is_empty() {
            let low: Vec<String> = words.iter().map(|w| w.to_lowercase()).collect();
            for (i, ln) in lines.iter().enumerate() {
                let l = ln.to_lowercase();
                if low.iter().any(|w| l.contains(w.as_str())) {
                    at = i.saturating_sub(1);
                    break;
                }
            }
        }
        lines[at..(at + max_lines).min(lines.len())].join("\n")
    }
}

#[derive(Debug, Clone)]
pub struct QueryInfo {
    pub text: String,
    pub terms: Vec<String>,
    pub wholes: Vec<String>,
    pub lits: Vec<String>,
    pub words: Vec<String>,  // raw lowercase words
    pub keys: Vec<String>,   // underscore-insensitive keys for exact name lookup
    pub dotted: Vec<String>, // Class.method style keys
}

impl QueryInfo {
    pub fn parse(text: &str) -> QueryInfo {
        let words: Vec<String> = Words::new(text).map(|w| w.to_lowercase()).collect();
        let mut keys = Vec::new();
        let mut seen = HashSet::new();
        for w in &words {
            if w.len() >= 3 && !QUERY_STOP.contains(w.as_str()) {
                let k = name_key(w);
                if seen.insert(k.clone()) {
                    keys.push(k);
                }
            }
        }
        let dotted_re = Regex::new(r"[A-Za-z_]\w*(?:\.[A-Za-z_]\w*)+").unwrap();
        let mut dotted = Vec::new();
        let mut dseen = HashSet::new();
        for m in dotted_re.find_iter(text) {
            let d: Vec<String> = m.as_str().split('.').map(name_key).collect();
            let d = d.join(".");
            if dseen.insert(d.clone()) {
                dotted.push(d);
            }
        }
        QueryInfo { text: text.to_string(), terms: query_terms(text), wholes: query_wholes(text), lits: literal_candidates(text), words, keys, dotted }
    }
}

fn is_lex_token(t: &str) -> bool {
    !t.is_empty() && t.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
}

pub fn lex_keys(q: &QueryInfo) -> Vec<i64> {
    let mut seen = HashSet::new();
    q.terms.iter().chain(&q.wholes).filter(|t| seen.insert((*t).clone())).filter(|t| is_lex_token(t)).map(|t| tok_key(t)).collect()
}

pub struct Searcher {
    pub ix: Index,
    pub ranker: Option<Arc<TreeEnsemble>>,
    ver: Option<String>,
    callers_cache: HashMap<String, i64>,
    /// set when the call being answered was cancelled: `search_many` stops between queries
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
}

pub struct Candidates {
    pub q: QueryInfo,
    pub rows: Vec<ChunkRow>,
    pub x: Vec<Features>,
    pub lex_ids: Vec<i64>,
    pub sem_ids: Vec<i64>,
    pub exact_ids: Vec<i64>,
}

/// A symbol name with every separator a language writes (`::`, `->`, `#`) as a dot. A separator
/// stands between two names, so the arrow of `operator->` stays.
pub(crate) fn dotted(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut rest = name;
    while let Some(c) = rest.chars().next() {
        let sep = ["::", "->", "#"].into_iter().find(|s| rest.starts_with(s));
        let follows = |s: &str| rest[s.len()..].chars().next().is_some_and(|n| n.is_alphanumeric() || n == '_' || n == '~');
        match sep {
            Some(s) if !out.ends_with("operator") && follows(s) => {
                // a leading separator (`::make`) names the top level: nothing before the name
                if !out.is_empty() {
                    out.push('.');
                }
                rest = &rest[s.len()..];
            }
            _ => {
                out.push(c);
                rest = &rest[c.len_utf8()..];
            }
        }
    }
    out
}

impl Searcher {
    pub fn new(ix: Index, ranker: Option<Arc<TreeEnsemble>>) -> Searcher {
        let ranker = ranker.filter(|r| r.feature_names == FEATURES);
        Searcher { ix, ranker, ver: None, callers_cache: HashMap::new(), cancel: Arc::default() }
    }

    fn sync(&mut self) -> Result<()> {
        self.ix.lex()?;
        let ver = self.ix.meta("data_version");
        if ver != self.ver {
            self.ver = ver;
            self.callers_cache.clear();
        }
        Ok(())
    }

    fn exact(&self, q: &QueryInfo, limit: usize) -> Result<Vec<i64>> {
        let mut out: Vec<i64> = Vec::new();
        if !q.keys.is_empty() {
            let ph = vec!["?"; q.keys.len()].join(",");
            let mut st = self.ix.db.prepare(&format!("SELECT id FROM chunks WHERE name_key IN ({ph}) AND kind!='window' LIMIT {limit}"))?;
            let rows = st.query_map(params_from_iter(q.keys.iter()), |r| r.get::<_, i64>(0))?;
            for r in rows {
                out.push(r?);
            }
        }
        for d in &q.dotted {
            let last = d.rsplit('.').next().unwrap_or(d);
            let mut st = self.ix.db.prepare_cached("SELECT id, qual_key FROM chunks WHERE name_key=?1 AND kind!='window' LIMIT 200")?;
            let rows = st.query_map([last], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))?;
            for r in rows {
                let (id, qk) = r?;
                if qk == *d || qk.ends_with(&format!(".{d}")) {
                    out.push(id);
                }
            }
        }
        let mut seen = HashSet::new();
        out.retain(|i| seen.insert(*i));
        Ok(out)
    }

    /// chunk id -> (lines mentioning an identifier-like query token, is that token the chunk's name)
    fn literal_hits(lits: &[String], rows: &[ChunkRow]) -> HashMap<i64, (i64, i64)> {
        let pats: Vec<(String, String)> = lits.iter().take(3).map(|l| (l.to_lowercase(), name_key(l))).collect();
        let mut out = HashMap::new();
        for row in rows {
            let low = row.text.to_lowercase();
            let hits: i64 = low.split('\n').map(|ln| pats.iter().filter(|(p, _)| ln.contains(p.as_str())).count() as i64).sum();
            let is_def = pats.iter().any(|(_, k)| row.name_key == *k) as i64;
            if hits > 0 || is_def > 0 {
                out.insert(row.id, (hits, is_def));
            }
        }
        out
    }

    fn ncallers(&mut self, names: &[String]) -> Result<()> {
        let mut need: Vec<&String> = names.iter().filter(|n| !n.is_empty() && !self.callers_cache.contains_key(*n)).collect();
        need.sort();
        need.dedup();
        for part in need.chunks(400) {
            let ph = vec!["?"; part.len()].join(",");
            let mut st = self.ix.db.prepare(&format!("SELECT callee, COUNT(*) FROM refs WHERE callee IN ({ph}) GROUP BY callee"))?;
            let mut got: HashMap<String, i64> = HashMap::new();
            for r in st.query_map(params_from_iter(part.iter()), |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))? {
                let (n, c) = r?;
                got.insert(n, c);
            }
            for n in part {
                self.callers_cache.insert((*n).clone(), got.get(*n).copied().unwrap_or(0));
            }
        }
        Ok(())
    }

    fn fetch_rows(&self, ids: &[i64]) -> Result<HashMap<i64, ChunkRow>> {
        let mut by_id = HashMap::with_capacity(ids.len());
        for part in ids.chunks(500) {
            let ph = vec!["?"; part.len()].join(",");
            let mut st = self.ix.db.prepare(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE id IN ({ph})"))?;
            for r in st.query_map(params_from_iter(part.iter()), ChunkRow::from_row)? {
                let r = r?;
                by_id.insert(r.id, r);
            }
        }
        Ok(by_id)
    }

    // ------------------------------------------------------------ candidates + features
    pub fn candidates(&mut self, query: &str, use_literal: bool) -> Result<Candidates> {
        let q = QueryInfo::parse(query);
        self.sync()?;
        let keys = lex_keys(&q);
        let lex_ids = self.ix.lex()?.search(&keys, LEX_LIMIT).0;
        let (sem_ids, qz): (Vec<i64>, Option<Vec<f32>>) = match (&self.ix.model, q.terms.is_empty()) {
            (Some(m), false) => match m.encode_query(&q.terms) {
                Some(z) => (self.ix.lex()?.semantic_top(&z, SEM_LIMIT, None), Some(z)),
                None => (Vec::new(), None),
            },
            _ => (Vec::new(), None),
        };
        let exact_ids = self.exact(&q, 60)?;
        let want_lit = use_literal && !q.lits.is_empty();
        let mut ids: Vec<i64> = Vec::new();
        let mut seen = HashSet::new();
        for lst in [&exact_ids, &lex_ids, &sem_ids] {
            for &i in lst {
                if seen.insert(i) {
                    ids.push(i);
                }
            }
        }
        if ids.is_empty() {
            return Ok(Candidates { q, rows: Vec::new(), x: Vec::new(), lex_ids, sem_ids, exact_ids });
        }
        let by_id = self.fetch_rows(&ids)?;
        ids.retain(|i| by_id.contains_key(i));
        let rows: Vec<ChunkRow> = ids.iter().map(|i| by_id[i].clone()).collect();
        let n = ids.len();
        let lit = if want_lit { Self::literal_hits(&q.lits, &rows) } else { HashMap::new() };
        let lex_rank: HashMap<i64, usize> = lex_ids.iter().enumerate().map(|(r, &c)| (c, r + 1)).collect();
        let sem_rank: HashMap<i64, usize> = sem_ids.iter().enumerate().map(|(r, &c)| (c, r + 1)).collect();

        let mut x: Vec<Features> = vec![[0f32; NFEAT]; n];
        // --- lexical
        let s = self.ix.lex()?.field_scores(&keys, &ids);
        let pairs = [
            ("lex_log", "lex_ratio"),
            ("name_log", "name_ratio"),
            ("qual_log", "qual_ratio"),
            ("sig_log", "sig_ratio"),
            ("doc_log", "doc_ratio"),
            ("body_log", "body_ratio"),
            ("path_log", "path_ratio"),
        ];
        for (j, (lg, ra)) in pairs.iter().enumerate() {
            let mx = s.iter().map(|r| r[j]).fold(f32::NEG_INFINITY, f32::max).max(1e-9);
            for (r, row) in s.iter().enumerate() {
                let col = row[j].max(0.0);
                x[r][fi(lg)] = (col as f64).ln_1p() as f32;
                x[r][fi(ra)] = col / mx;
            }
        }
        let lx: Vec<f32> = s.iter().map(|r| r[0]).collect();
        let (mean, std) = mean_std(&lx);
        for r in 0..n {
            x[r][fi("lex_rank")] = lex_rank.get(&ids[r]).map(|&k| 1.0 / (1.0 + k as f32)).unwrap_or(0.0);
            x[r][fi("lex_z")] = (lx[r] - mean) / (std + 1e-9);
        }
        // --- semantic
        if let Some(z) = &qz {
            let (sa, sb) = self.ix.lex()?.sem_pair(&ids, z);
            let mix: Vec<f32> = sa.iter().zip(&sb).map(|(a, b)| 0.5 * (a + b)).collect();
            for (src, dst, zdst) in [(&sa, "sem_a", "sem_a_z"), (&sb, "sem_b", "sem_b_z"), (&mix, "sem_mix", "sem_mix_z")] {
                let (m, sd) = mean_std(src);
                for r in 0..n {
                    x[r][fi(dst)] = src[r];
                    x[r][fi(zdst)] = (src[r] - m) / (sd + 1e-9);
                }
            }
            for r in 0..n {
                x[r][fi("sem_rank")] = sem_rank.get(&ids[r]).map(|&k| 1.0 / (1.0 + k as f32)).unwrap_or(0.0);
            }
        }
        // --- exactness / name overlap / structure
        let tset: HashSet<&str> = q.terms.iter().map(|s| s.as_str()).collect();
        let exact_set: HashSet<i64> = exact_ids.iter().copied().collect();
        let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        for (r, row) in rows.iter().enumerate() {
            let cid = ids[r];
            let name = row.name.as_str();
            let nt: HashSet<&str> = row.name_tok.split_whitespace().collect();
            let inter = nt.intersection(&tset).count();
            let f = &mut x[r];
            f[fi("exact_name")] = (exact_set.contains(&cid) && q.keys.contains(&row.name_key)) as i32 as f32;
            f[fi("exact_qual")] = (exact_set.contains(&cid) && !q.dotted.is_empty()) as i32 as f32;
            f[fi("name_overlap")] = if tset.is_empty() { 0.0 } else { inter as f32 / tset.len() as f32 };
            f[fi("name_cover")] = if nt.is_empty() { 0.0 } else { inter as f32 / nt.len() as f32 };
            f[fi("name_same_set")] = (!nt.is_empty() && nt == tset) as i32 as f32;
            if let Some(&(hits, is_def)) = lit.get(&cid) {
                f[fi("lit_hits")] = (hits as f64).ln_1p() as f32;
                f[fi("lit_def")] = is_def as f32;
            }
            let k = row.kind.as_str();
            f[fi("k_function")] = (k == "function") as i32 as f32;
            f[fi("k_method")] = (k == "method") as i32 as f32;
            f[fi("k_class")] = matches!(k, "class" | "struct" | "trait" | "interface" | "type" | "enum" | "impl") as i32 as f32;
            f[fi("k_module")] = (k == "module") as i32 as f32;
            f[fi("k_other")] = (k == "window") as i32 as f32;
            f[fi("is_test")] = row.is_test as f32;
            f[fi("log_nbody")] = (row.nbody as f64).ln_1p() as f32;
            f[fi("has_doc")] = (row.ndoc > 0) as i32 as f32;
            f[fi("log_ndoc")] = (row.ndoc as f64).ln_1p() as f32;
            f[fi("qual_depth")] = row.qualname.matches('.').count() as f32;
            f[fi("is_private")] = (name.starts_with('_') && !name.starts_with("__")) as i32 as f32;
        }
        self.ncallers(&names)?;
        for (r, nm) in names.iter().enumerate() {
            let c = self.callers_cache.get(nm).copied().unwrap_or(0);
            x[r][fi("log_ncallers")] = (c as f64).ln_1p() as f32;
        }
        // --- query level + file cohesion
        let has_ident = !q.keys.is_empty() && (!q.dotted.is_empty() || q.text.contains('_') || !q.wholes.is_empty());
        let mut top_paths: HashMap<&str, usize> = HashMap::new();
        for cid in lex_ids.iter().take(30) {
            if let Some(r) = by_id.get(cid) {
                *top_paths.entry(r.path.as_str()).or_insert(0) += 1;
            }
        }
        for (r, row) in rows.iter().enumerate() {
            let f = &mut x[r];
            f[fi("n_terms")] = q.terms.len() as f32;
            f[fi("n_lits")] = q.lits.len() as f32;
            f[fi("n_words")] = q.words.len() as f32;
            f[fi("has_ident")] = has_ident as i32 as f32;
            f[fi("file_frac")] = top_paths.get(row.path.as_str()).copied().unwrap_or(0) as f32 / 30.0;
        }
        Ok(Candidates { q, rows, x, lex_ids, sem_ids, exact_ids })
    }

    // ---------------------------------------------------------------------- scoring
    pub fn score(&self, x: &[Features]) -> Vec<f64> {
        let n = x.len();
        if n == 0 {
            return Vec::new();
        }
        let lin: Vec<f32> = x.iter().map(linear_score).collect();
        let Some(ranker) = &self.ranker else {
            return lin.iter().map(|&v| v as f64).collect();
        };
        let keep = prune_indices(&lin, PRUNE);
        let mut out = vec![-1e9f64; n];
        for &i in &keep {
            let row: Vec<f64> = x[i].iter().map(|&v| v as f64).collect();
            out[i] = ranker.predict_row(&row);
        }
        out
    }

    pub fn hits(&self, c: &Candidates, scores: &[f64], k: usize, with_text: bool) -> Result<Vec<Hit>> {
        let mut order: Vec<usize> = (0..c.rows.len()).collect();
        order.sort_by(|&a, &b| scores[b].partial_cmp(&scores[a]).unwrap_or(std::cmp::Ordering::Equal).then(c.rows[a].id.cmp(&c.rows[b].id)));
        let mut picked: Vec<Hit> = Vec::new();
        let mut best_by_key: HashMap<(String, String), usize> = HashMap::new();
        for r in order {
            let row = &c.rows[r];
            let mut h = Hit::from_row(row, false);
            h.score = scores[r];
            h.why = why(&c.x[r]);
            let key = (h.path.clone(), h.qualname.clone());
            let is_code = !h.qualname.is_empty() && CODE_KINDS.contains(&h.kind.as_str());
            if is_code {
                if let Some(&pi) = best_by_key.get(&key) {
                    // overload / redefinition: keep the longer body
                    if (h.end - h.start) > (picked[pi].end - picked[pi].start) {
                        picked[pi] = h;
                    }
                    continue;
                }
            }
            if matches!(h.kind.as_str(), "module" | "window") && picked.iter().any(|p| p.path == h.path && !(h.end < p.start || h.start > p.end)) {
                continue;
            }
            if is_code {
                best_by_key.insert(key, picked.len());
            }
            picked.push(h);
            if picked.len() >= k {
                break;
            }
        }
        if with_text {
            self.fill_text(&mut picked)?;
        }
        Ok(picked)
    }

    fn fill_text(&self, hits: &mut [Hit]) -> Result<()> {
        if hits.is_empty() {
            return Ok(());
        }
        let ids: Vec<i64> = hits.iter().map(|h| h.id).collect();
        let ph = vec!["?"; ids.len()].join(",");
        let mut st = self.ix.db.prepare(&format!("SELECT id, text FROM chunks WHERE id IN ({ph})"))?;
        let mut texts: HashMap<i64, String> = HashMap::new();
        for r in st.query_map(params_from_iter(ids.iter()), |r| Ok((r.get::<_, i64>(0)?, crate::index::unpack_text(r.get_ref(1)?)?)))? {
            let (i, t) = r?;
            texts.insert(i, t);
        }
        for h in hits {
            h.text = texts.remove(&h.id).unwrap_or_default();
        }
        Ok(())
    }

    pub fn search(&mut self, query: &str, k: usize, with_text: bool) -> Result<Vec<Hit>> {
        let c = self.candidates(query, true)?;
        let scores = self.score(&c.x);
        self.hits(&c, &scores, k, with_text)
    }

    /// Several phrasings of one need (synonyms, guessed identifiers): rank-fuse the results.
    pub fn search_many(&mut self, queries: &[String], k: usize, with_text: bool) -> Result<Vec<Hit>> {
        let mut fused: HashMap<i64, f64> = HashMap::new();
        let mut meta: HashMap<i64, Hit> = HashMap::new();
        let mut first_seen: Vec<i64> = Vec::new();
        for q in queries {
            if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                anyhow::bail!("cancelled");
            }
            for (r, h) in self.search(q, 30, false)?.into_iter().enumerate() {
                *fused.entry(h.id).or_insert(0.0) += 1.0 / (60.0 + (r + 1) as f64);
                meta.entry(h.id).or_insert_with(|| {
                    first_seen.push(h.id);
                    h
                });
            }
        }
        let mut ids: Vec<i64> = first_seen;
        ids.sort_by(|a, b| fused[b].partial_cmp(&fused[a]).unwrap());
        ids.truncate(k);
        let mut out: Vec<Hit> = ids
            .iter()
            .map(|i| {
                let mut h = meta.remove(i).unwrap();
                h.score = fused[i];
                h
            })
            .collect();
        if with_text {
            self.fill_text(&mut out)?;
        }
        Ok(out)
    }

    // ----------------------------------------------------------------- navigation
    pub fn find_symbol(&self, name: &str, limit: usize) -> Result<Vec<Hit>> {
        // `Type::method`, `Type#method` and `p->method` are spelled `Type.method` in the index
        let name = &dotted(name);
        let name = name.trim_start_matches('.');
        let key = name_key(name.rsplit('.').next().unwrap_or(name));
        // An empty ?3 means no qualifier; the qualifier filter must run before LIMIT.
        let qual = if name.contains('.') { name_key(name) } else { String::new() };
        let mut st = self.ix.db.prepare(&format!(
            "SELECT {CHUNK_COLS} FROM chunks WHERE name_key=?1 AND kind!='window' \
             AND (?3 = '' OR qual_key = ?3 OR substr(qual_key, -(length(?3) + 1)) = '.' || ?3) \
             ORDER BY is_test, (kind IN ('class','function','method')) DESC LIMIT ?2"
        ))?;
        let rows: Vec<ChunkRow> = st.query_map(rusqlite::params![key, limit as i64, qual], ChunkRow::from_row)?.collect::<rusqlite::Result<_>>()?;
        Ok(rows.iter().map(|r| Hit::from_row(r, true)).collect())
    }

    /// The definitions that call what `name` stands for, as the `callers` tool answers.
    pub fn callers(&self, name: &str, limit: usize) -> Result<Vec<Hit>> {
        Ok(crate::ops::callers(self, name, limit)?.0)
    }

    /// -> (resolved definitions, unresolved external names), as the `callees` tool answers.
    pub fn callees(&self, name: &str, limit: usize) -> Result<(Vec<Hit>, Vec<String>)> {
        crate::ops::callees(self, name, limit).map(|(resolved, external, _)| (resolved, external))
    }

    pub fn read(&self, path: &str, start: i64, end: Option<i64>, max_lines: i64) -> String {
        let end = end.unwrap_or(i64::MAX).min(start.saturating_add(max_lines - 1));
        match self.read_lines(path, start, end) {
            Ok((lines, _)) => lines.join("\n"),
            Err(e) => format!("error: {e}"),
        }
    }

    /// Lines `start..=end` of a repository file, numbered, and how many lines the file has.
    pub fn read_lines(&self, path: &str, start: i64, end: i64) -> Result<(Vec<String>, i64)> {
        let shown = crate::fmt::echo(path);
        let canon = std::fs::canonicalize(self.ix.root.join(path)).map_err(|_| anyhow::anyhow!("cannot read {shown}"))?;
        if !canon.starts_with(&self.ix.root) {
            anyhow::bail!("path outside repo");
        }
        if !canon.is_file() {
            anyhow::bail!("{shown} is not a regular file");
        }
        let bytes = std::fs::read(&canon).map_err(|_| anyhow::anyhow!("cannot read {shown}"))?;
        let text = String::from_utf8_lossy(&bytes);
        let lines = crate::text::split_lines(&text);
        let n = lines.len() as i64;
        let out = (start.max(1)..=end.min(n)).map(|i| format!("{i:>5}  {}", lines[(i - 1) as usize])).collect();
        Ok((out, n))
    }
}

fn mean_std(v: &[f32]) -> (f32, f32) {
    if v.is_empty() {
        return (0.0, 0.0);
    }
    let n = v.len() as f64;
    let mean = v.iter().map(|&x| x as f64).sum::<f64>() / n;
    let var = v.iter().map(|&x| (x as f64 - mean).powi(2)).sum::<f64>() / n;
    (mean as f32, var.sqrt() as f32)
}

/// Human-readable evidence for a hit, from its feature vector.
pub fn why(x: &Features) -> Vec<(String, serde_json::Value)> {
    let f = |n: &str| x[fi(n)];
    let r2 = |v: f32| serde_json::json!(((v as f64) * 100.0).round() / 100.0);
    let mut out = Vec::new();
    if f("exact_name") > 0.0 || f("exact_qual") > 0.0 {
        out.push(("exact name".to_string(), serde_json::json!(1)));
    }
    for (label, key) in [("name", "name_ratio"), ("signature", "sig_ratio"), ("docs", "doc_ratio"), ("body", "body_ratio"), ("path", "path_ratio")] {
        if f(key) >= 0.5 {
            out.push((format!("{label} keywords"), r2(f(key))));
        }
    }
    if f("sem_mix") >= 0.25 {
        out.push(("semantic".to_string(), r2(f("sem_mix"))));
    }
    if f("lit_hits") > 0.0 {
        out.push(("literal match".to_string(), serde_json::json!(((f("lit_hits") as f64).exp_m1()).round() as i64)));
    }
    out
}

pub fn prune_indices(lin: &[f32], n: usize) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..lin.len()).collect();
    if idx.len() > n {
        idx.select_nth_unstable_by(n - 1, |&a, &b| lin[b].partial_cmp(&lin[a]).unwrap_or(std::cmp::Ordering::Equal).then(a.cmp(&b)));
        idx.truncate(n);
    }
    idx
}

/// Hand-set blend used to pick the ranker's candidates (and as the score without a ranker).
pub fn linear_score(x: &Features) -> f32 {
    let f = |n: &str| x[fi(n)];
    f("lex_z") + 0.9 * f("sem_mix_z") + 2.5 * f("exact_name") + 1.5 * f("exact_qual") + 1.2 * f("lit_def") + 0.3 * f("lit_hits") + 1.0 * f("name_overlap")
        - 0.4 * f("is_test")
}
