//! Incremental on-disk index: SQLite (chunks, call refs, manifest) + memory-mapped segments
//! (BM25 postings, vectors). Port of python/scrutate_ref/index.py; the layout is identical, so an index built by
//! the Python reference engine opens here and vice versa.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result};
use rayon::prelude::*;
use rusqlite::{params, Connection, OptionalExtension};
use sha1::{Digest, Sha1};

use crate::lex::{merge_segments, pack_bags, quantize, FileLex, LexStore, QVecs, SegmentBuilder};
use crate::models::ModelSet;
use crate::parse::{parse_file_with, PrevParse};
use crate::semantic::SemanticModel;
use crate::text::{is_test_path, name_key, tokenize2, BODY_CAP};

pub const SCHEMA_VERSION: u32 = 13;
/// What the parser and tokenizer make of a file. Bump it with every change of the rules in
/// `langs.rs`, `parse.rs` or `text.rs` (and the same in the reference's `parse.py` and `text.py`):
/// an index of an older revision reads every file again on its next update, and one of a newer
/// revision is read as it is and never updated.
pub const CONTENT_REVISION: u32 = 2;
pub use crate::models::REVISION as MODEL_REVISION;
pub const MAX_SEGMENTS: usize = 6; // more live segments than this triggers a merge of the small ones
pub const SMALL_INDEX_ROWS: usize = 8000; // an index this small is kept as ONE segment
/// Rows per segment flushed during one update: bounds the memory of building the inverted index
/// (the postings of a whole monorepository would not fit) at the price of more, smaller segments,
/// which compaction merges later.
pub const SEGMENT_ROWS: usize = 100_000;
/// Compaction never builds a segment larger than this, so a merge's memory is bounded too.
pub const MAX_MERGE_ROWS: usize = 300_000;
/// Databases up to this size get a full `PRAGMA quick_check` on every open (milliseconds); larger
/// ones are checked on demand (`scrutate check`) and recovered when an operation reports corruption.
pub const QUICK_CHECK_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const GC_AGE_S: u64 = 600; // unreferenced segment directories younger than this may be in flight
pub const MAX_FILE_BYTES: u64 = 1_000_000;
/// Source bytes of the syntax trees an open index keeps for incremental reparsing (the trees
/// themselves take several times this in memory).
pub const TREE_CACHE_BYTES: usize = 2 << 20;
pub const TF_CAP: u32 = 4; // a token counts at most this many times per field

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT);
CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime_ns INTEGER, hash TEXT, ctime_ns INTEGER);
CREATE TABLE IF NOT EXISTS chunks(
  id INTEGER PRIMARY KEY, path TEXT NOT NULL, kind TEXT, name TEXT, qualname TEXT,
  name_key TEXT, qual_key TEXT, start INTEGER, end INTEGER, is_test INTEGER,
  ndoc INTEGER, nbody INTEGER, text BLOB, name_tok TEXT);
CREATE INDEX IF NOT EXISTS chunks_path ON chunks(path, start);
CREATE INDEX IF NOT EXISTS chunks_name ON chunks(name_key);
CREATE INDEX IF NOT EXISTS chunks_qual ON chunks(qual_key);
CREATE TABLE IF NOT EXISTS refs(chunk_id INTEGER NOT NULL, callee TEXT NOT NULL);
CREATE INDEX IF NOT EXISTS refs_callee ON refs(callee);
CREATE INDEX IF NOT EXISTS refs_chunk ON refs(chunk_id);
CREATE TABLE IF NOT EXISTS imports(path TEXT NOT NULL, module TEXT NOT NULL, line INTEGER);
CREATE INDEX IF NOT EXISTS imports_path ON imports(path);
CREATE INDEX IF NOT EXISTS imports_module ON imports(module);
";

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct UpdateStats {
    pub files_total: usize,
    pub files_changed: usize,
    pub files_removed: usize,
    pub chunks: i64,
    pub seconds: f64,
    pub semantic: bool,
}

/// Result of `Index::check`.
#[derive(Debug, serde::Serialize)]
pub struct CheckReport {
    pub database_ok: bool,
    pub segments: usize,
    pub segments_ok: bool,
    /// chunks with a segment row: every chunk but the constants
    pub chunks_db: i64,
    pub chunks_alive: i64,
    pub files_indexed: usize,
    pub files_changed: usize,
    pub files_missing: usize,
    pub problems: Vec<String>,
}

impl Default for CheckReport {
    fn default() -> Self {
        CheckReport {
            database_ok: true,
            segments: 0,
            segments_ok: true,
            chunks_db: 0,
            chunks_alive: 0,
            files_indexed: 0,
            files_changed: 0,
            files_missing: 0,
            problems: Vec::new(),
        }
    }
}

impl CheckReport {
    pub fn ok(&self) -> bool {
        self.database_ok && self.segments_ok && self.chunks_db == self.chunks_alive && self.problems.is_empty()
    }
}

/// One row of the `chunks` table as the search layer reads it.
/// Chunk text as stored: zlib-compressed UTF-8 (2.4x smaller on source code, ~6 us to inflate).
pub fn pack_text(text: &str) -> Vec<u8> {
    thread_local! {
        // a deflate state is ~300 KB to initialise: reuse one per thread instead of paying that per chunk
        static COMP: std::cell::RefCell<flate2::Compress> = std::cell::RefCell::new(flate2::Compress::new(flate2::Compression::new(6), true));
    }
    COMP.with(|c| {
        let mut c = c.borrow_mut();
        c.reset();
        let mut out = Vec::with_capacity(text.len() / 2 + 32);
        let mut input = text.as_bytes();
        loop {
            let before_in = c.total_in();
            let status = c.compress_vec(input, &mut out, flate2::FlushCompress::Finish).expect("in-memory deflate");
            input = &input[(c.total_in() - before_in) as usize..];
            match status {
                flate2::Status::StreamEnd => break,
                _ => out.reserve(out.len() / 2 + 64), // output buffer full: grow and continue
            }
        }
        out
    })
}

/// Inverse of `pack_text`; accepts legacy plain text and NULL.
pub fn unpack_text(v: rusqlite::types::ValueRef<'_>) -> rusqlite::Result<String> {
    use std::io::Read;
    match v {
        rusqlite::types::ValueRef::Null => Ok(String::new()),
        rusqlite::types::ValueRef::Text(t) => Ok(String::from_utf8_lossy(t).into_owned()),
        rusqlite::types::ValueRef::Blob(b) => {
            let mut out = String::new();
            flate2::read::ZlibDecoder::new(b)
                .read_to_string(&mut out)
                .map_err(|e| rusqlite::Error::FromSqlConversionFailure(12, rusqlite::types::Type::Blob, Box::new(e)))?;
            Ok(out)
        }
        other => Err(rusqlite::Error::InvalidColumnType(12, "text".into(), other.data_type())),
    }
}

#[derive(Debug, Clone, Default)]
pub struct ChunkRow {
    pub id: i64,
    pub path: String,
    pub kind: String,
    pub name: String,
    pub qualname: String,
    pub name_key: String,
    pub qual_key: String,
    pub start: i64,
    pub end: i64,
    pub is_test: i64,
    pub ndoc: i64,
    pub nbody: i64,
    pub text: String,
    pub name_tok: String,
}

pub const CHUNK_COLS: &str = "id,path,kind,name,qualname,name_key,qual_key,start,end,is_test,ndoc,nbody,text,name_tok";

impl ChunkRow {
    pub fn from_row(r: &rusqlite::Row) -> rusqlite::Result<ChunkRow> {
        Ok(ChunkRow {
            id: r.get(0)?,
            path: r.get(1)?,
            kind: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            name: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            qualname: r.get::<_, Option<String>>(4)?.unwrap_or_default(),
            name_key: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            qual_key: r.get::<_, Option<String>>(6)?.unwrap_or_default(),
            start: r.get(7)?,
            end: r.get(8)?,
            is_test: r.get::<_, Option<i64>>(9)?.unwrap_or(0),
            ndoc: r.get::<_, Option<i64>>(10)?.unwrap_or(0),
            nbody: r.get::<_, Option<i64>>(11)?.unwrap_or(0),
            text: unpack_text(r.get_ref(12)?)?,
            name_tok: r.get::<_, Option<String>>(13)?.unwrap_or_default(),
        })
    }
}

fn bag(tokens: impl Iterator<Item = String>, cap: Option<u32>) -> HashMap<String, u32> {
    let mut m: HashMap<String, u32> = HashMap::new();
    for t in tokens {
        *m.entry(t).or_insert(0) += 1;
    }
    if let Some(c) = cap {
        for v in m.values_mut() {
            if *v > c {
                *v = c;
            }
        }
    }
    m
}

/// Parse + tokenize one file -> (rows with their callee names, FileLex, imports). Pure function.
/// A chunk ready for insertion: the row, its callees, and the compressed text (packed here, in
/// the parallel stage, so the single-writer insert loop only copies bytes).
pub type NewRow = (ChunkRow, Vec<String>, Vec<u8>);

/// Cumulative nanoseconds per indexing phase, printed by `update()` when SCRUTATE_PROFILE is set.
pub mod prof {
    use std::sync::atomic::{AtomicU64, Ordering};
    pub static PARSE: AtomicU64 = AtomicU64::new(0);
    pub static TREESITTER: AtomicU64 = AtomicU64::new(0);
    pub static TOKENIZE: AtomicU64 = AtomicU64::new(0);
    pub static ENCODE: AtomicU64 = AtomicU64::new(0);
    pub static PACK: AtomicU64 = AtomicU64::new(0);
    pub static INSERT: AtomicU64 = AtomicU64::new(0);
    pub static SEGMENT: AtomicU64 = AtomicU64::new(0);
    pub fn add(c: &AtomicU64, t0: std::time::Instant) {
        c.fetch_add(t0.elapsed().as_nanos() as u64, Ordering::Relaxed);
    }
    pub fn enabled() -> bool {
        std::env::var_os("SCRUTATE_PROFILE").is_some()
    }
    pub fn report(wall: f64) {
        let g = |c: &AtomicU64| c.load(Ordering::Relaxed) as f64 / 1e9;
        eprintln!(
            "[scrutate profile] wall {wall:.2}s | cpu: parse {:.2}s (tree-sitter {:.2}s) tokenize {:.2}s encode {:.2}s pack {:.2}s | serial: insert {:.2}s segment {:.2}s",
            g(&PARSE), g(&TREESITTER), g(&TOKENIZE), g(&ENCODE), g(&PACK), g(&INSERT), g(&SEGMENT)
        );
    }
}

pub type BuiltRows = (Vec<NewRow>, FileLex, Vec<(i64, String)>, Option<PrevParse>);

pub fn build_rows(rel: &str, source: &str, model: Option<&SemanticModel>, prev: Option<PrevParse>) -> BuiltRows {
    let is_test = is_test_path(rel) as i64;
    let path_text = rel.replace(['/', '.', '_'], " ");
    let path_bag = bag(tokenize2(&path_text, None).0.into_iter(), None);
    let mut rows = Vec::new();
    let mut bags: Vec<Vec<HashMap<String, u32>>> = Vec::new();
    let mut vecs = QVecs::default();
    let prof = prof::enabled();
    let t = Instant::now();
    let (parsed, tree) = parse_file_with(rel, source, prev);
    if prof {
        prof::add(&prof::PARSE, t);
    }
    for mut c in parsed.chunks {
        let t = Instant::now();
        c.fill(source.as_bytes());
        let qual = match c.qualname.rfind('.') {
            Some(i) => c.qualname[..i].to_string(),
            None => String::new(),
        };
        let (name_s, name_w) = tokenize2(&c.name, None);
        let (qual_s, qual_w) = tokenize2(&qual, None);
        let (sig_s, sig_w) = tokenize2(&c.sig, Some(80));
        let (doc_s, doc_w) = tokenize2(&c.doc, Some(200));
        let (body_s, body_w) = tokenize2(&c.body, Some(BODY_CAP));
        // a constant is a symbol only: no postings, no vector, never a ranked candidate
        let lexed = c.kind != "constant";
        bags.extend(lexed.then(|| {
            vec![
                bag(name_s.iter().chain(&name_w).cloned(), None),
                bag(qual_s.iter().chain(&qual_w).cloned(), None),
                bag(sig_s.iter().chain(&sig_w).cloned(), Some(TF_CAP)),
                bag(doc_s.iter().chain(&doc_w).cloned(), Some(TF_CAP)),
                bag(body_s.iter().chain(&body_w).cloned(), Some(TF_CAP)),
                path_bag.clone(),
            ]
        }));
        if prof {
            prof::add(&prof::TOKENIZE, t);
        }
        let t = Instant::now();
        if let Some(m) = model.filter(|_| lexed) {
            push_vectors(m, c.kind, [&name_s, &qual_s, &sig_s, &doc_s, &body_s], &mut vecs);
        }
        if prof {
            prof::add(&prof::ENCODE, t);
        }
        let t = Instant::now();
        let mut seen = HashSet::new();
        let name_tok: Vec<&str> = name_s.iter().map(|s| s.as_str()).filter(|s| seen.insert(*s)).collect();
        rows.push((
            ChunkRow {
                id: 0,
                path: rel.to_string(),
                kind: c.kind.to_string(),
                name: c.name.clone(),
                qualname: c.qualname.clone(),
                name_key: name_key(&c.name),
                qual_key: name_key(&c.qualname),
                start: c.start,
                end: c.end,
                is_test,
                ndoc: doc_s.len() as i64,
                nbody: body_s.len() as i64,
                text: String::new(), // the text travels compressed (below); readers inflate it
                name_tok: name_tok.join(" "),
            },
            c.calls.iter().cloned().collect(),
            pack_text(&c.text),
        ));
        if prof {
            prof::add(&prof::PACK, t);
        }
    }
    let vec = if model.is_some() && !rows.is_empty() { Some(vecs) } else { None };
    (rows, pack_bags(&bags, vec), parsed.imports, tree)
}

/// The (A view, B view) vector of one chunk from its stemmed name, qualifier, signature, doc and
/// body words, quantized onto the end of `vecs`.
fn push_vectors(m: &SemanticModel, kind: &str, [name, qual, sig, doc, body]: [&[String]; 5], vecs: &mut QVecs) {
    let k = m.k;
    let mut z = vec![0f32; 2 * k];
    if kind != "window" {
        if let Some(za) = m.encode_chunk(name, qual, sig, doc, &[]) {
            z[..k].copy_from_slice(&za);
        }
    }
    if let Some(zb) = m.encode_chunk(&[], &[], &[], &[], body) {
        z[k..].copy_from_slice(&zb);
    }
    let mut q = vec![0i8; 2 * k];
    let (qa, qb) = q.split_at_mut(k);
    vecs.scale.push(quantize(&z[..k], qa));
    vecs.scale.push(quantize(&z[k..], qb));
    vecs.q.extend(q);
}

/// The vectors `build_rows` makes for the file at `rel`, without its rows and postings; None when
/// its content no longer has the hash `known_hash` (or does not parse), so it must be indexed again.
fn file_vectors(root: &Path, rel: &str, known_hash: &str, m: &SemanticModel) -> Option<QVecs> {
    let raw = std::fs::read(root.join(rel)).ok()?;
    let source = decode_source(&raw)?;
    if format!("{:x}", Sha1::digest(&raw)) != known_hash {
        return None;
    }
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut vecs = QVecs::default();
        for mut c in parse_file_with(rel, &source, None).0.chunks.into_iter().filter(|c| c.kind != "constant") {
            c.fill(source.as_bytes());
            let qual = c.qualname.rfind('.').map_or("", |i| &c.qualname[..i]);
            let toks = [
                tokenize2(&c.name, None).0,
                tokenize2(qual, None).0,
                tokenize2(&c.sig, Some(80)).0,
                tokenize2(&c.doc, Some(200)).0,
                tokenize2(&c.body, Some(BODY_CAP)).0,
            ];
            push_vectors(m, c.kind, [&toks[0], &toks[1], &toks[2], &toks[3], &toks[4]], &mut vecs);
        }
        vecs
    }))
    .ok()
}

enum Processed {
    Gone,
    Skip(Stat),
    Same(Stat),
    New(Stat, String, Vec<NewRow>, Box<FileLex>, Vec<(i64, String)>, Option<PrevParse>),
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

/// What one `stat` says about a file, as the `files` table records it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Stat {
    pub size: i64,
    pub mtime: i64,
    /// status change time: moves on a chmod, which leaves size and mtime alone (0 where unknown)
    pub ctime: i64,
}

impl Stat {
    pub fn of(md: &std::fs::Metadata) -> Stat {
        #[cfg(unix)]
        let ctime = {
            use std::os::unix::fs::MetadataExt;
            md.ctime().saturating_mul(1_000_000_000).saturating_add(md.ctime_nsec())
        };
        #[cfg(not(unix))]
        let ctime = 0;
        Stat { size: md.len() as i64, mtime: mtime_ns(md), ctime }
    }

    /// Whether a recorded row still describes the file; a row without a ctime (written by the
    /// reference engine, or before the column existed) is read again once.
    fn matches(&self, size: i64, mtime: i64, ctime: Option<i64>) -> bool {
        self.size == size && self.mtime == mtime && ctime == Some(self.ctime)
    }
}

fn process(root: &Path, rel: &str, known_hash: Option<&str>, model: Option<&SemanticModel>, prev: Option<PrevParse>) -> Processed {
    let full = root.join(rel);
    let Ok(md) = std::fs::metadata(&full) else {
        return Processed::Gone;
    };
    let stat = Stat::of(&md);
    if md.len() > MAX_FILE_BYTES {
        return Processed::Skip(stat);
    }
    let Ok(raw) = std::fs::read(&full) else {
        return Processed::Gone; // unreadable now: its old chunks must not answer
    };
    let Some(source) = decode_source(&raw) else {
        return Processed::Skip(stat);
    };
    let h = format!("{:x}", Sha1::digest(&raw));
    if Some(h.as_str()) == known_hash {
        return Processed::Same(stat);
    }
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_rows(rel, &source, model, prev)));
    match built {
        Ok((rows, lex, imports, tree)) => Processed::New(stat, h, rows, Box::new(lex), imports, tree),
        Err(_) => {
            eprintln!("[scrutate] skipped {rel}: parser failure");
            Processed::Skip(stat)
        }
    }
}

/// Source text of a file: UTF-16 with a byte-order mark is transcoded (Windows tools write it), a
/// NUL byte in the first 4 KB means binary (None), anything else is lossy UTF-8.
pub fn decode_source(raw: &[u8]) -> Option<std::borrow::Cow<'_, str>> {
    let utf16 = |be: bool| {
        let units = raw[2..].chunks(2).map(|c| match c {
            [a, b] if be => u16::from_be_bytes([*a, *b]),
            [a, b] => u16::from_le_bytes([*a, *b]),
            _ => 0xFFFD, // odd trailing byte
        });
        std::borrow::Cow::Owned(char::decode_utf16(units).map(|c| c.unwrap_or('\u{FFFD}')).collect())
    };
    if raw.starts_with(&[0xFF, 0xFE]) {
        return Some(utf16(false));
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        return Some(utf16(true));
    }
    if raw[..raw.len().min(4096)].contains(&0u8) {
        return None;
    }
    Some(String::from_utf8_lossy(raw))
}

/// The chunk count `chunks_at` ("<data version> <count>") records, when it was written for `ver`.
fn counted(at: Option<&str>, ver: Option<&str>) -> Option<i64> {
    let (v, n) = at?.split_once(' ')?;
    (Some(v) == ver).then(|| n.parse().ok()).flatten()
}

/// SQLite errors that mean the file is not a usable database (as opposed to busy or I/O trouble).
fn is_corruption(e: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode::*;
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if matches!(f.code, DatabaseCorrupt | NotADatabase))
}

/// Write a file under the index directory as a new file: whatever was at `path` (a planted link
/// included) is removed first, so the write never lands outside.
pub fn write_new(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    std::fs::File::options().write(true).create_new(true).open(path)?.write_all(bytes)
}

/// `.gitignore` of the index directory, unless something is there already.
pub(crate) fn write_gitignore(dir: &Path) -> std::io::Result<()> {
    let gi = dir.join(".gitignore");
    if std::fs::symlink_metadata(&gi).is_ok() {
        return Ok(());
    }
    // the index is a cache; never commit it
    match std::fs::File::options().write(true).create_new(true).open(&gi) {
        Ok(mut f) => std::io::Write::write_all(&mut f, b"*\n"),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

/// Whether `root` has an index database (built or being built).
pub fn exists(root: &Path) -> bool {
    root.join(".scrutate").join("index.db").exists()
}

/// Index files that are not what this program wrote; handled like SQLite corruption.
#[derive(Debug)]
pub struct Damaged(pub &'static str);

impl std::fmt::Display for Damaged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Damaged {}

/// Another process holds the write lock.
#[derive(Debug)]
pub struct Busy;

impl std::fmt::Display for Busy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("another process is updating the index")
    }
}

impl std::error::Error for Busy {}

/// A read found no committed file while another process holds the write lock: a first build.
#[derive(Debug)]
pub struct BeingBuilt;

impl std::fmt::Display for BeingBuilt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the index is being built by another process; try again when it finishes")
    }
}

impl std::error::Error for BeingBuilt {}

/// The index was built by a newer scrutate. A newer content revision alone is `readable`: it is
/// answered from as it is and never updated. A newer schema or model revision is refused.
#[derive(Debug)]
pub struct Newer {
    pub readable: bool,
    message: String,
}

impl std::fmt::Display for Newer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Newer {}

impl Newer {
    /// Of the meta values `get` reads, the first one higher than this binary's.
    fn of(get: impl Fn(&str) -> Option<String>) -> Option<Newer> {
        let checks = [
            ("schema_version", "schema version", SCHEMA_VERSION),
            ("model_rev", "model revision", MODEL_REVISION),
            ("content_rev", "content revision", CONTENT_REVISION),
        ];
        checks.into_iter().find_map(|(key, what, ours)| {
            let stored = get(key)?.parse::<u32>().ok().filter(|&v| v > ours)?;
            let readable = key == "content_rev";
            let does = if readable { "reads it and does not update it" } else { "cannot read it" };
            let message = format!(
                "index built by a newer scrutate ({what} {stored}, this version has {ours}): this version {does}; use the newer scrutate, or `scrutate index --force` to rebuild it with this one"
            );
            Some(Newer { readable, message })
        })
    }

    /// Say it on stderr, once per process.
    fn note(&self) {
        static SAID: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if !SAID.swap(true, std::sync::atomic::Ordering::Relaxed) {
            eprintln!("[scrutate] {self}");
        }
    }
}

/// True when an error anywhere in `e`'s chain is SQLite corruption or other damage to the index
/// files (a chunk text that does not inflate, `Damaged`).
pub fn error_is_corruption(e: &anyhow::Error) -> bool {
    e.chain().any(|c| {
        c.is::<Damaged>()
            || match c.downcast_ref::<rusqlite::Error>() {
                Some(rusqlite::Error::FromSqlConversionFailure(_, rusqlite::types::Type::Blob, _)) => true,
                Some(e) => is_corruption(e),
                None => false,
            }
    })
}

/// Delete the index files of `root` (used after an operation reported database corruption on a
/// database too large for the open-time integrity check). The caller reopens afterwards.
pub fn wipe_index(root: &Path) {
    if let Ok(dir) = crate::files::index_dir(root) {
        Index::wipe(&dir, &dir.join("seg"));
    }
}

/// The metadata of the file at `rel` (from the canonical `root`), when it is a file and not a link,
/// as the walk sees it, and resolves inside the root: a linked folder on the way may lead out.
pub(crate) fn file_inside(root: &Path, rel: &str) -> Option<std::fs::Metadata> {
    let full = root.join(rel);
    let md = std::fs::symlink_metadata(&full).ok().filter(|m| m.is_file())?;
    std::fs::canonicalize(&full).ok()?.starts_with(root).then_some(md)
}

/// Run `f` again while it fails with SQLITE_BUSY/LOCKED (statements the busy handler does not cover,
/// such as the journal-mode switch of a brand-new database opened by several processes at once).
fn retry_busy<T>(mut f: impl FnMut() -> rusqlite::Result<T>) -> rusqlite::Result<T> {
    let t0 = Instant::now();
    loop {
        match f() {
            Err(rusqlite::Error::SqliteFailure(e, _))
                if matches!(e.code, rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked)
                    && t0.elapsed() < Duration::from_secs(30) =>
            {
                std::thread::sleep(Duration::from_millis(10));
            }
            other => return other,
        }
    }
}

enum Triage {
    /// use the files as they are
    Reuse,
    /// a readable index of another schema or model: empty it inside the database and re-index
    Rebuild(&'static str),
    /// not a usable database: delete the files
    Garbage(&'static str),
    /// built by a newer scrutate that this one cannot read: leave it alone
    Newer(Newer),
}

pub struct Index {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub seg_dir: PathBuf,
    pub models: Arc<ModelSet>,
    /// the signature of `models`' vector model, as the index stores it
    sig: String,
    pub db: Connection,
    /// print indexing progress to stderr when it is a terminal (set by the CLI)
    pub progress: bool,
    /// set to stop an update between batches of files; the update is rolled back
    pub cancel: Arc<std::sync::atomic::AtomicBool>,
    lex: Option<LexStore>,
    lex_ver: Option<String>,
    last_refresh: Option<Instant>,
    scan_cost: f64,
    /// last syntax tree of recently indexed files, with a use counter for eviction
    trees: HashMap<String, (u64, PrevParse)>,
    tree_tick: u64,
    /// most source bytes the tree cache held at once
    #[cfg(test)]
    trees_peak: usize,
    /// which file `db` has open, to notice another process deleting it and building a new one
    db_id: Option<(u64, u64)>,
}

/// Device and inode of a file. None on Windows, where a file SQLite has open cannot be deleted.
fn file_id(path: &Path) -> Option<(u64, u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

impl Index {
    pub fn open(root: &Path, models: Arc<ModelSet>) -> Result<Index> {
        crate::files::index_dir(root)?;
        let root = std::fs::canonicalize(root).with_context(|| format!("open {}", root.display()))?;
        let dir = crate::files::index_dir(&root)?;
        let seg_dir = dir.join("seg");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        write_gitignore(&dir)?;
        let db_path = dir.join("index.db");
        let sem_sig = models.signature(&dir)?;
        let wrong_type = db_path.is_dir() || std::fs::symlink_metadata(&seg_dir).is_ok_and(|m| !m.is_dir());
        let triage = if wrong_type { Triage::Garbage("index files of the wrong type") } else { Self::triage(&db_path) };
        if let Triage::Newer(n) = triage {
            return Err(n.into());
        }
        if let Triage::Garbage(why) = triage {
            // nobody can be using a database that does not open: deleting the files is safe
            eprintln!("[scrutate] index database unusable ({why}); rebuilding");
            Self::wipe(&dir, &seg_dir);
        }
        std::fs::create_dir_all(&seg_dir)?;
        let setup = |db: &Connection| -> rusqlite::Result<()> {
            db.busy_timeout(Duration::from_secs(30))?;
            retry_busy(|| db.query_row("PRAGMA journal_mode=WAL", [], |_| Ok(())))?;
            // WAL + NORMAL: durable against process crashes, consistent after power loss (at worst the
            // last transaction rolls back); one fsync per checkpoint instead of per transaction
            retry_busy(|| db.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA temp_store=MEMORY; PRAGMA cache_size=-32000;"))
        };
        let db = Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;
        let db = match setup(&db) {
            Ok(()) => retry_busy(|| db.execute_batch(SCHEMA)).map(|()| db),
            Err(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ffi::ErrorCode::ReadOnly && matches!(triage, Triage::Reuse) => {
                drop(db);
                Ok(Self::open_immutable(&db_path, &sem_sig)?)
            }
            Err(e) => return Err(e.into()),
        };
        let db = match db {
            Ok(db) => db,
            Err(e) => {
                // opened but cannot hold the schema: garbage after all
                eprintln!("[scrutate] index database unusable ({e}); rebuilding");
                Self::wipe(&dir, &seg_dir);
                std::fs::create_dir_all(&seg_dir)?;
                let db = Connection::open(&db_path).with_context(|| format!("open {}", db_path.display()))?;
                setup(&db)?;
                retry_busy(|| db.execute_batch(SCHEMA))?;
                db
            }
        };
        let mut ix = Index {
            root,
            dir,
            seg_dir,
            models,
            sig: sem_sig.clone(),
            db,
            progress: false,
            lex: None,
            lex_ver: None,
            last_refresh: None,
            scan_cost: 1.0,
            trees: HashMap::new(),
            tree_tick: 0,
            #[cfg(test)]
            trees_peak: 0,
            cancel: Arc::default(),
            db_id: file_id(&db_path),
        };
        if let Triage::Rebuild(why) = triage {
            // an index of another schema: drop its contents *inside* the database, so
            // other processes using the same index keep a consistent (if briefly empty) view
            eprintln!("[scrutate] {why}; rebuilding the index");
            ix.clear_with(Some(sem_sig.as_str()))?;
        } else {
            // a reader must not need the write lock: another process may hold it for minutes
            if ix.meta("schema_version").as_deref() != Some(SCHEMA_VERSION.to_string().as_str()) {
                ix.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
            }
            // an index with files keeps the vectors of its model until an update makes them anew
            if ix.meta("model_sig").as_deref() != Some(sem_sig.as_str()) && !ix.has_files()? {
                ix.set_meta("model_sig", &sem_sig)?;
                ix.set_meta("model_rev", &MODEL_REVISION.to_string())?;
            }
            if ix.newer().is_none() && ix.meta("content_rev").as_deref() != Some(CONTENT_REVISION.to_string().as_str()) {
                if ix.has_files()? {
                    eprintln!("[scrutate] index built with other parsing rules; every file is read again on the next update");
                } else {
                    ix.set_meta("content_rev", &CONTENT_REVISION.to_string())?;
                }
            }
        }
        Ok(ix)
    }

    /// A WAL database in a read-only directory cannot be read the usual way (its `-shm` cannot be
    /// created). Nobody without root can write it there either, so it is read as a fixed file,
    /// unless a `-wal` holds transactions not yet copied into it.
    fn open_immutable(db_path: &Path, sem_sig: &str) -> Result<Connection> {
        let wal = db_path.with_extension("db-wal");
        if std::fs::metadata(&wal).is_ok_and(|m| m.len() > 0) {
            anyhow::bail!("{}: read-only, and its write-ahead log is not empty", db_path.display());
        }
        let path = db_path.to_string_lossy().replace('%', "%25").replace('?', "%3f").replace('#', "%23");
        let flags = rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_URI;
        let db = Connection::open_with_flags(format!("file:{path}?immutable=1"), flags)?;
        let get = |k: &str| db.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional();
        if let Some(n) = Newer::of(|k| get(k).ok().flatten()).filter(|n| !n.readable) {
            return Err(n.into());
        }
        if get("schema_version")?.as_deref() != Some(SCHEMA_VERSION.to_string().as_str()) || get("model_sig")?.as_deref() != Some(sem_sig) {
            anyhow::bail!("{}: read-only, and built by another version or model", db_path.display());
        }
        Ok(db)
    }

    /// Forget every indexed file and segment (inside one write transaction).
    fn clear(&mut self) -> Result<()> {
        self.clear_with(None)
    }

    /// `clear`; with `stamp_for` (a model signature), of an index triage found to need a rebuild,
    /// stamped with this version in the same transaction. Another process that found the same
    /// index may have done that first and be building it: then nothing is cleared, and its write
    /// lock is not waited for.
    fn clear_with(&mut self, stamp_for: Option<&str>) -> Result<()> {
        let done = |db: &Connection| stamp_for.is_some() && Self::rebuild_reason(db).is_none();
        self.lex = None;
        self.lex_ver = None;
        self.db.busy_timeout(Duration::ZERO)?;
        let t0 = Instant::now();
        let locked = loop {
            if done(&self.db) {
                break Ok(false);
            }
            match self.db.execute_batch("BEGIN IMMEDIATE") {
                Err(rusqlite::Error::SqliteFailure(e, _))
                    if matches!(e.code, rusqlite::ffi::ErrorCode::DatabaseBusy | rusqlite::ffi::ErrorCode::DatabaseLocked)
                        && t0.elapsed() < Duration::from_secs(30) =>
                {
                    std::thread::sleep(Duration::from_millis(10))
                }
                r => break r.map(|()| true),
            }
        };
        self.db.busy_timeout(Duration::from_secs(30))?;
        if !locked? {
            return Ok(());
        }
        if done(&self.db) {
            self.db.execute_batch("ROLLBACK")?;
            return Ok(());
        }
        let old = self.segments();
        self.db.execute_batch("DELETE FROM refs; DELETE FROM imports; DELETE FROM chunks; DELETE FROM files;")?;
        self.set_meta("segments", "[]")?;
        let ver = self.bump_version()?;
        self.set_meta("chunks_at", &format!("{ver} 0"))?;
        if let Some(sig) = stamp_for {
            self.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
            self.set_meta("model_sig", sig)?;
            self.set_meta("model_rev", &MODEL_REVISION.to_string())?;
            self.set_meta("content_rev", &CONTENT_REVISION.to_string())?;
        }
        retry_busy(|| self.db.execute_batch("COMMIT"))?;
        for n in old {
            let _ = std::fs::remove_dir_all(self.seg_dir.join(n));
        }
        Ok(())
    }

    /// What to do with the index files found on disk. Only an unmistakably damaged database is
    /// declared garbage: a locked one (another process writing) is reused, since deleting the files
    /// of a live database would break every other process using it.
    fn triage(db_path: &Path) -> Triage {
        if !db_path.exists() {
            return Triage::Reuse;
        }
        let con = match Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY) {
            Ok(c) => c,
            Err(e) => return if is_corruption(&e) { Triage::Garbage("cannot open") } else { Triage::Reuse },
        };
        let _ = con.busy_timeout(Duration::from_secs(30));
        // a damaged header or a foreign file fails the first real read (NOTADB / CORRUPT)
        if let Err(e) = con.query_row("SELECT count(*) FROM sqlite_master", [], |r| r.get::<_, i64>(0)) {
            return if is_corruption(&e) { Triage::Garbage("not a database") } else { Triage::Reuse };
        }
        // the full page walk is affordable for small databases; large ones rely on the error-driven
        // recovery in `recover_if_corrupt` (and on `scrutate check` for an explicit audit)
        let size = std::fs::metadata(db_path).map(|m| m.len()).unwrap_or(0);
        let limit = std::env::var("SCRUTATE_QUICK_CHECK_MAX_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(QUICK_CHECK_MAX_BYTES);
        if size <= limit {
            match con.query_row("PRAGMA quick_check(1)", [], |r| r.get::<_, String>(0)) {
                Ok(s) if s == "ok" => {}
                Ok(_) => return Triage::Garbage("integrity check failed"),
                Err(e) => return if is_corruption(&e) { Triage::Garbage("integrity check failed") } else { Triage::Reuse },
            }
        }
        let get =
            |k: &str| -> Option<String> { con.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten() };
        if let Some(n) = Newer::of(get).filter(|n| !n.readable) {
            return Triage::Newer(n);
        }
        Self::rebuild_reason(&con).map_or(Triage::Reuse, Triage::Rebuild)
    }

    /// Why a readable index of this or an older version must be emptied and built again. Another
    /// vector model is no reason: an update makes the vectors anew and keeps the rest.
    fn rebuild_reason(db: &Connection) -> Option<&'static str> {
        let get = |k: &str| db.query_row("SELECT value FROM meta WHERE key=?1", [k], |r| r.get::<_, String>(0)).optional().ok().flatten();
        if get("schema_version").as_deref() != Some(SCHEMA_VERSION.to_string().as_str()) {
            return Some("index schema changed");
        }
        if get("segments").is_some_and(|v| serde_json::from_str::<Vec<String>>(&v).is_err()) {
            return Some("segment list unreadable");
        }
        Self::chunks_lost(db).ok().flatten()
    }

    /// The `chunks` table is empty although the commit that wrote the current data version
    /// counted rows in it. O(1); a version written by another engine is not judged.
    fn chunks_lost(db: &Connection) -> rusqlite::Result<Option<&'static str>> {
        let (ver, at, any): (Option<String>, Option<String>, bool) = db.query_row(
            "SELECT (SELECT value FROM meta WHERE key='data_version'), (SELECT value FROM meta WHERE key='chunks_at'), EXISTS(SELECT 1 FROM chunks)",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )?;
        Ok((!any && counted(at.as_deref(), ver.as_deref()).is_some_and(|n| n > 0)).then_some("the chunks table lost its rows"))
    }

    pub(crate) fn wipe(dir: &Path, seg_dir: &Path) {
        for name in ["index.db", "index.db-wal", "index.db-shm", "vectors.npz"] {
            let p = dir.join(name);
            let _ = std::fs::remove_file(&p).or_else(|_| std::fs::remove_dir_all(&p));
        }
        let _ = std::fs::remove_dir_all(seg_dir).or_else(|_| std::fs::remove_file(seg_dir));
    }

    // ---- meta helpers
    pub fn meta(&self, key: &str) -> Option<String> {
        self.db.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        retry_busy(|| self.db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)", params![key, value]))?;
        Ok(())
    }

    /// Whether a newer scrutate built this index (or took it over since it was opened).
    pub fn newer(&self) -> Option<Newer> {
        Newer::of(|k| self.meta(k))
    }

    pub fn segments(&self) -> Vec<String> {
        serde_json::from_str(&self.meta("segments").unwrap_or_else(|| "[]".into())).unwrap_or_default()
    }

    /// Call inside a write transaction: then a directory already under the new name was left by a
    /// writer stopped before COMMIT, since no live writer holds an uncommitted name.
    fn new_segment_name(&self) -> Result<String> {
        let listed = self.segments();
        let mut n: i64 = self.meta("seg_counter").and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
        while listed.contains(&format!("s{n:06}")) {
            n += 1;
        }
        self.set_meta("seg_counter", &n.to_string())?;
        let name = format!("s{n:06}");
        for p in [self.seg_dir.join(&name), self.seg_dir.join(format!("{name}.tmp"))] {
            if p.exists() {
                std::fs::remove_dir_all(&p).with_context(|| format!("remove leftover {}", p.display()))?;
            }
        }
        Ok(name)
    }

    /// Delete segment directories no manifest refers to (left by a crash), unless brand new. Call
    /// inside a write transaction, so no writer of this machine has one in flight uncommitted.
    fn gc_segments(&self) {
        let keep: HashSet<String> = self.segments().into_iter().collect();
        let Ok(rd) = std::fs::read_dir(&self.seg_dir) else {
            return;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if keep.contains(&name) {
                continue;
            }
            let age = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).map(|d| d.as_secs()).unwrap_or(0);
            if age > GC_AGE_S {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }

    /// Which database file this is (device and inode), where the system has such a thing.
    pub(crate) fn db_id(&self) -> Option<(u64, u64)> {
        self.db_id
    }

    /// Open the index again when another process deleted the database this one has open and
    /// built a new one in its place (a rebuild after damage, `index --force`): writing on would go
    /// into the deleted file, and its segments into the new one's directory.
    pub fn reopen_if_replaced(&mut self) -> Result<()> {
        let now = file_id(&self.dir.join("index.db"));
        if self.db_id.is_none() || now.is_none() || now == self.db_id {
            return Ok(());
        }
        eprintln!("[scrutate] index database replaced by another process; opening the new one");
        let mut fresh = Index::open(&self.root, Arc::clone(&self.models))?;
        fresh.progress = self.progress;
        fresh.cancel = Arc::clone(&self.cancel);
        *self = fresh;
        Ok(())
    }

    /// The segment store, with statistics for the chunks that are alive right now.
    pub fn lex(&mut self) -> Result<&mut LexStore> {
        self.reopen_if_replaced()?;
        let ver = self.meta("data_version").unwrap_or_else(|| "0".into());
        let names = self.segments();
        if self.lex.as_ref().map(|l| l.names != names).unwrap_or(true) {
            let vectors = self.meta("model_sig").is_some_and(|s| s != "none");
            let mut names = names;
            let mut attempt = 0;
            let store = loop {
                let opened = LexStore::open(&self.seg_dir, &names).and_then(|s| {
                    if vectors && s.segs.iter().any(|g| g.n > 0 && g.vec.is_none()) {
                        return Err(Damaged("a segment has no vectors").into());
                    }
                    Ok(s)
                });
                match opened {
                    Ok(s) => break s,
                    Err(e) => {
                        // another process may have just compacted: the manifest we read named
                        // segments that are gone. Re-read it a few times before calling it damage.
                        attempt += 1;
                        std::thread::sleep(Duration::from_millis(20 * attempt));
                        let again = self.segments();
                        if again != names && attempt <= 5 {
                            names = again;
                            continue;
                        }
                        if attempt <= 5 {
                            continue;
                        }
                        // a segment vanished or was damaged while we were running: rebuild from the sources
                        eprintln!("[scrutate] index unreadable ({e:#}); rebuilding");
                        self.rebuild()?;
                        break LexStore::open(&self.seg_dir, &self.segments())?;
                    }
                }
            };
            self.lex = Some(store);
            self.lex_ver = None;
        }
        if self.lex_ver.as_deref() != Some(ver.as_str()) {
            let store = self.lex.as_mut().unwrap();
            let n_rows: usize = store.segs.iter().map(|s| s.n).sum();
            // one statement, so the counts and the version they are checked against are of one commit
            let (at_ver, at, n_all, n_live): (Option<String>, Option<String>, i64, i64) = self.db.query_row(
                "SELECT (SELECT value FROM meta WHERE key='data_version'), (SELECT value FROM meta WHERE key='chunks_at'), \
                 (SELECT COUNT(*) FROM chunks), (SELECT COUNT(*) FROM chunks WHERE kind != 'constant')",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            if counted(at.as_deref(), at_ver.as_deref()).is_some_and(|n| n != n_all) {
                return Err(Damaged("the chunks table lost rows").into());
            }
            if n_rows as i64 == n_live {
                store.prepare(None)?; // nothing was ever deleted: no need to read every id
            } else {
                let mut st = self.db.prepare("SELECT id FROM chunks ORDER BY id")?;
                let live: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                store.prepare(Some(&live))?;
            }
            self.lex_ver = Some(ver);
        }
        Ok(self.lex.as_mut().unwrap())
    }

    // ---- update
    /// Inside `watch`'s handling of a burst that named files only, the update looks at those alone.
    pub fn update(&mut self, force: bool) -> Result<UpdateStats> {
        let named = if force { None } else { crate::watch::burst_files().map(|b| b.code) };
        self.update_with(force, true, named.as_deref())
    }

    /// Of `paths` (repository paths the index may hold), the ones whose file is gone or whose
    /// stat differs from what the index recorded. One `stat` and one indexed lookup per path.
    pub fn stale(&self, paths: &[&str]) -> Result<Vec<String>> {
        let ctime = if self.has_ctime() { "ctime_ns" } else { "NULL" };
        let mut q = self.db.prepare_cached(&format!("SELECT size, mtime_ns, {ctime} FROM files WHERE path=?1 AND hash != 'SKIP'"))?;
        let mut out: Vec<String> = Vec::new();
        for &p in paths {
            if out.iter().any(|o| o == p) {
                continue;
            }
            let Some((size, mtime, ct)) = q.query_row([p], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, Option<i64>>(2)?))).optional()? else {
                continue;
            };
            let fresh = std::fs::metadata(self.root.join(p)).ok().filter(|m| m.is_file()).map(|m| Stat::of(&m));
            // a row without a ctime is judged by size and mtime here; the next scan reads it again
            if fresh.is_none_or(|s| !s.matches(size, mtime, ct.or(Some(s.ctime)))) {
                out.push(p.to_string());
            }
        }
        Ok(out)
    }

    /// `update` of just these files, without a walk of the tree; fails at once when another
    /// process holds the write lock.
    pub fn update_paths(&mut self, paths: &[String]) -> Result<UpdateStats> {
        self.update_with(false, false, Some(paths))
    }

    fn has_ctime(&self) -> bool {
        self.db.prepare_cached("SELECT 1 FROM pragma_table_info('files') WHERE name='ctime_ns'").and_then(|mut q| q.exists([])).unwrap_or(false)
    }

    /// `update`; with `wait` false a write lock held by another process is an error at once.
    /// `only`: look at these files alone, instead of every file of the tree.
    fn update_with(&mut self, force: bool, wait: bool, only: Option<&[String]>) -> Result<UpdateStats> {
        self.reopen_if_replaced()?;
        if !self.dir.join("index.db").is_file() {
            return Err(Damaged("index database deleted").into());
        }
        // only a forced update replaces a newer index, and only one this version can read
        if let Some(n) = self.newer().filter(|n| !(force && n.readable)) {
            if only.is_some() && n.readable {
                n.note();
            }
            return Err(n.into());
        }
        let t0 = Instant::now();
        let mut st = UpdateStats { semantic: self.models.semantic_enabled(), ..Default::default() };
        // if the statistics held in memory describe exactly the state we start from, they can be
        // patched afterwards instead of recomputed
        let mut removed: Vec<i64> = Vec::new();
        let mut seg_names: Vec<String> = Vec::new();
        let mut replaced: Vec<String> = Vec::new();
        // a panic in an earlier update may have left its transaction open on this connection
        if !self.db.is_autocommit() {
            let _ = self.db.execute_batch("ROLLBACK");
        }
        // one writer at a time; readers keep working (WAL)
        if wait {
            retry_busy(|| self.db.execute_batch("BEGIN IMMEDIATE"))?;
        } else {
            self.db.busy_timeout(Duration::ZERO)?;
            let r = self.db.execute_batch("BEGIN IMMEDIATE");
            self.db.busy_timeout(Duration::from_secs(30))?;
            match r {
                Err(rusqlite::Error::SqliteFailure(f, _)) if f.code == rusqlite::ffi::ErrorCode::DatabaseBusy => return Err(Busy.into()),
                r => r?,
            }
        }
        self.gc_segments();
        let cur_ver = self.meta("data_version").unwrap_or_else(|| "0".into());
        // vectors made anew replace every segment: what is held in memory is read again
        let in_sync = self.meta("model_sig").as_deref() == Some(self.sig.as_str())
            && self.lex.is_some()
            && self.lex_ver.as_deref() == Some(cur_ver.as_str())
            && self.lex.as_ref().map(|l| l.names == self.segments()).unwrap_or(false);
        let run =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.update_inner(force, only, &mut st, &mut removed, &mut seg_names, &mut replaced)));
        let changed = match run {
            Ok(Ok(c)) => c,
            failed => {
                let _ = self.db.execute_batch("ROLLBACK");
                self.evict_trees();
                for name in &seg_names {
                    let _ = std::fs::remove_dir_all(self.seg_dir.join(name));
                }
                match failed {
                    Ok(r) => return r.map(|_| st),
                    // the database is as it was; what is held in memory may describe half an update
                    Err(panic) => {
                        self.lex = None;
                        self.lex_ver = None;
                        std::panic::resume_unwind(panic)
                    }
                }
            }
        };
        // read under the write lock: after COMMIT another writer's version could be read instead
        let committed = self.meta("data_version");
        retry_busy(|| self.db.execute_batch("COMMIT"))?;
        for name in &replaced {
            let _ = std::fs::remove_dir_all(self.seg_dir.join(name));
        }
        if changed {
            let mut patched = false;
            if in_sync {
                let added: Vec<String> = seg_names.clone();
                if self.lex.as_mut().unwrap().apply(&removed, &added).is_ok() {
                    self.lex_ver = committed;
                    patched = true;
                }
            }
            if !patched {
                self.lex = None;
                self.lex_ver = None;
            }
            // the update is committed: a merge that fails leaves more segments, not a wrong index
            match self.compact() {
                Err(e) if error_is_corruption(&e) => return Err(e),
                Err(e) => {
                    eprintln!("[scrutate] compaction failed ({e:#}); the update is saved, the next one merges again");
                    if !self.db.is_autocommit() {
                        let _ = self.db.execute_batch("ROLLBACK");
                    }
                    self.lex = None;
                    self.lex_ver = None;
                }
                Ok(()) => {}
            }
            let _ = self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            // a long-running process would otherwise keep a build's word caches for its whole life
            rayon::broadcast(|_| crate::text::clear_caches());
            crate::text::clear_caches();
        }
        st.seconds = t0.elapsed().as_secs_f64();
        if only.is_none() {
            self.last_refresh = Some(Instant::now());
        }
        if prof::enabled() {
            prof::report(st.seconds);
        }
        Ok(st)
    }

    /// Body of `update()`, inside the write transaction. -> whether anything changed.
    fn update_inner(
        &mut self,
        force: bool,
        only: Option<&[String]>,
        st: &mut UpdateStats,
        removed: &mut Vec<i64>,
        seg_names: &mut Vec<String>,
        replaced: &mut Vec<String>,
    ) -> Result<bool> {
        if let Some(why) = Self::chunks_lost(&self.db)? {
            return Err(Damaged(why).into());
        }
        if !self.has_ctime() {
            // an index built by the reference engine, or before the column existed
            self.db.execute_batch("ALTER TABLE files ADD COLUMN ctime_ns INTEGER")?;
        }
        let mut changed = false;
        let rev = CONTENT_REVISION.to_string();
        let reparse = only.is_none() && self.meta("content_rev").as_deref() != Some(rev.as_str());
        let force = force || reparse;
        let scan_t0 = Instant::now();
        type Known = HashMap<String, (i64, i64, Option<i64>, String)>;
        let mut known: Known = HashMap::new();
        let row =
            |r: &rusqlite::Row| Ok((r.get::<_, String>(0)?, (r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, Option<i64>>(3)?, r.get::<_, String>(4)?)));
        let files = match only {
            None => {
                let mut q = self.db.prepare("SELECT path,size,mtime_ns,ctime_ns,hash FROM files")?;
                known = q.query_map([], row)?.collect::<rusqlite::Result<Known>>()?;
                let files = crate::files::list_files(&self.root);
                st.files_total = files.len();
                files
            }
            Some(paths) => {
                let mut q = self.db.prepare_cached("SELECT path,size,mtime_ns,ctime_ns,hash FROM files WHERE path=?1")?;
                for p in paths {
                    if let Some((p, k)) = q.query_row([p], row).optional()? {
                        known.insert(p, k);
                    }
                }
                paths.iter().filter(|p| file_inside(&self.root, p).is_some()).cloned().collect()
            }
        };
        let fileset: HashSet<&String> = files.iter().collect();
        let mut gone: Vec<String> = known.keys().filter(|p| !fileset.contains(p)).cloned().collect();
        gone.sort();
        let mut todo: Vec<(String, Option<String>)> = Vec::new();
        for rel in &files {
            let Ok(md) = std::fs::metadata(self.root.join(rel)) else {
                continue;
            };
            if !force {
                if let Some(k) = known.get(rel) {
                    if Stat::of(&md).matches(k.0, k.1, k.2) {
                        continue; // stat unchanged: no read, no hash
                    }
                }
            }
            todo.push((rel.clone(), if force { None } else { known.get(rel).map(|k| k.3.clone()) }));
        }
        if only.is_none() {
            self.scan_cost = scan_t0.elapsed().as_secs_f64();
        }
        let switching = self.meta("model_sig").as_deref() != Some(self.sig.as_str());
        let models = Arc::clone(&self.models);
        let model = if switching || !todo.is_empty() { models.semantic()? } else { None };
        if switching {
            self.vectors_anew(model, &mut todo, &gone, seg_names, replaced)?;
            self.set_meta("model_sig", &self.sig)?;
            self.set_meta("model_rev", &MODEL_REVISION.to_string())?;
            changed = true;
        }
        let mut next_id: i64 = self.meta("next_id").and_then(|v| v.parse().ok()).unwrap_or(1); // ids are never reused
        for g in &gone {
            removed.extend(self.delete_file(g)?);
            st.files_removed += 1;
            changed = true;
        }
        let k = model.map(|m| m.k).unwrap_or(0);
        let mut builder = SegmentBuilder::new(k);
        if let (Some(m), false) = (model, todo.is_empty()) {
            m.warm(); // build the weight tables once, before the parallel pass
        }
        let root = self.root.clone();
        let progress = self.progress && std::io::IsTerminal::is_terminal(&std::io::stderr());
        let mut done = 0usize;
        let mut last_tick = Instant::now();
        // the trees parsed first fill the cache, and the others are dropped at once, so the peak stays bounded
        let room = std::sync::atomic::AtomicIsize::new(TREE_CACHE_BYTES as isize);
        for batch in todo.chunks(256) {
            if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                anyhow::bail!("cancelled");
            }
            let prevs: Vec<Option<PrevParse>> = batch.iter().map(|(rel, _)| self.trees.remove(rel).map(|t| t.1)).collect();
            let results: Vec<(String, Processed)> = batch
                .par_iter()
                .zip(prevs)
                .map(|((rel, kh), prev)| {
                    let mut res = process(&root, rel, kh.as_deref(), model, prev);
                    if let Processed::New(.., tree) = &mut res {
                        let n = tree.as_ref().map_or(0, |t| t.source_len() as isize);
                        if room.fetch_sub(n, std::sync::atomic::Ordering::Relaxed) < n {
                            // a tree that does not fit gives its room back, so smaller ones can still use it
                            room.fetch_add(n, std::sync::atomic::Ordering::Relaxed);
                            *tree = None;
                        }
                    }
                    (rel.clone(), res)
                })
                .collect();
            done += batch.len();
            if progress && (last_tick.elapsed().as_millis() >= 200 || done == todo.len()) {
                eprint!("\r[scrutate] indexing {done}/{} files...", todo.len());
                last_tick = Instant::now();
            }
            for (rel, res) in results {
                match res {
                    Processed::Gone => removed.extend(self.delete_file(&rel)?),
                    Processed::Skip(s) => {
                        // binary / too large / unparsable: remember so we never re-read it
                        removed.extend(self.delete_file(&rel)?);
                        self.db.execute(
                            "INSERT OR REPLACE INTO files(path,size,mtime_ns,hash,ctime_ns) VALUES(?1,?2,?3,'SKIP',?4)",
                            params![rel, s.size, s.mtime, s.ctime],
                        )?;
                    }
                    Processed::Same(s) => {
                        // content identical, only the stat moved
                        self.db.execute("UPDATE files SET size=?1, mtime_ns=?2, ctime_ns=?3 WHERE path=?4", params![s.size, s.mtime, s.ctime, rel])?;
                    }
                    Processed::New(s, h, rows, lex, imports, tree) => {
                        if let Some(tree) = tree.filter(|t| t.source_len() <= TREE_CACHE_BYTES) {
                            self.tree_tick += 1;
                            self.trees.insert(rel.clone(), (self.tree_tick, tree));
                            #[cfg(test)]
                            {
                                self.trees_peak = self.trees_peak.max(self.trees.values().map(|t| t.1.source_len()).sum());
                            }
                        }
                        let t = Instant::now();
                        removed.extend(self.delete_file(&rel)?);
                        let first = next_id;
                        next_id = self.insert(&rel, rows, next_id)?;
                        prof::add(&prof::INSERT, t);
                        {
                            let mut ins = self.db.prepare_cached("INSERT INTO imports(path,module,line) VALUES(?1,?2,?3)")?;
                            for (line, module) in imports {
                                ins.execute(params![rel, module, line])?;
                            }
                        }
                        builder.add_file(first, *lex);
                        self.db.execute(
                            "INSERT OR REPLACE INTO files(path,size,mtime_ns,hash,ctime_ns) VALUES(?1,?2,?3,?4,?5)",
                            params![rel, s.size, s.mtime, h, s.ctime],
                        )?;
                        st.files_changed += 1;
                        changed = true;
                        if builder.n >= SEGMENT_ROWS {
                            // flush: keeps the inverted-index build bounded on huge updates
                            let full = std::mem::replace(&mut builder, SegmentBuilder::new(k));
                            self.flush_segment(full, seg_names)?;
                        }
                    }
                }
            }
            self.evict_trees();
        }
        if progress && done > 0 {
            eprint!("\r{}\r", " ".repeat(60)); // clear the progress line
        }
        if builder.n > 0 {
            self.flush_segment(builder, seg_names)?;
        }
        self.set_meta("next_id", &next_id.to_string())?;
        if reparse {
            self.set_meta("content_rev", &rev)?;
        }
        changed = changed || !removed.is_empty();
        st.chunks = self.db.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        if changed {
            let ver = self.bump_version()?; // same transaction as the manifest: readers never see one without the other
            self.set_meta("chunks_at", &format!("{ver} {}", st.chunks))?;
        }
        Ok(changed)
    }

    /// Write every segment again with the vectors `model` makes (none without a model), and its
    /// other arrays as they are, in place of the old one in the manifest (inside the update's
    /// transaction). One segment's vectors are held at a time. The rows of files in `todo` or
    /// `gone` are left out, as are those of a file whose content is not what the index recorded:
    /// that file joins `todo`, to be indexed whole.
    fn vectors_anew(
        &mut self,
        model: Option<&SemanticModel>,
        todo: &mut Vec<(String, Option<String>)>,
        gone: &[String],
        seg_names: &mut Vec<String>,
        replaced: &mut Vec<String>,
    ) -> Result<()> {
        let mut q = self.db.prepare(
            "SELECT f.path, f.hash, MIN(c.id), SUM(c.kind != 'constant') FROM files f JOIN chunks c ON c.path = f.path \
             WHERE f.hash != 'SKIP' GROUP BY f.path ORDER BY MIN(c.id)",
        )?;
        let files: Vec<(String, String, i64, i64)> = q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?.collect::<rusqlite::Result<_>>()?;
        drop(q);
        let queued: HashSet<&str> = todo.iter().map(|t| t.0.as_str()).chain(gone.iter().map(String::as_str)).collect();
        let files: Vec<_> = files.into_iter().filter(|f| f.3 > 0 && !queued.contains(f.0.as_str())).collect();
        if let (Some(m), false) = (model, files.is_empty()) {
            m.warm();
        }
        let k = model.map_or(0, |m| m.k);
        let mut again = Vec::new();
        let mut names = Vec::new();
        let old = self.segments();
        for name in &old {
            let seg = crate::lex::Segment::open(&self.seg_dir.join(name)).map_err(|_| Damaged("a listed segment does not open"))?;
            let mine: Vec<&(String, String, i64, i64)> = files.iter().filter(|f| seg.row_of(f.2).is_some()).collect();
            let mut vecs = QVecs::default();
            let mut at: HashMap<i64, usize> = HashMap::new();
            let mut alive = vec![false; seg.n];
            for batch in mine.chunks(256) {
                if self.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                    anyhow::bail!("cancelled");
                }
                let root = &self.root;
                let made: Vec<Option<QVecs>> = match model {
                    Some(m) => batch.par_iter().map(|(rel, hash, ..)| file_vectors(root, rel, hash, m)).collect(),
                    None => batch.iter().map(|_| Some(QVecs::default())).collect(),
                };
                for (&(rel, _, first, n), v) in batch.iter().zip(made) {
                    let fits = v.as_ref().filter(|v| model.is_none() || v.scale.len() == 2 * *n as usize);
                    let Some(v) = fits else {
                        again.push((rel.clone(), None));
                        continue;
                    };
                    for i in 0..*n {
                        let r = seg.row_of(first + i).ok_or(Damaged("a chunk has no segment row"))?;
                        alive[r] = true;
                        if model.is_some() {
                            at.insert(first + i, vecs.scale.len() / 2 + i as usize);
                        }
                    }
                    vecs.q.extend_from_slice(&v.q);
                    vecs.scale.extend_from_slice(&v.scale);
                }
            }
            replaced.push(name.clone());
            if !alive.contains(&true) {
                continue;
            }
            let of = |id: i64| {
                let r = *at.get(&id)?;
                Some((&vecs.q[r * 2 * k..(r + 1) * 2 * k], &vecs.scale[r * 2..r * 2 + 2]))
            };
            let vectors = if model.is_some() { crate::lex::Vectors::Made(k, &of) } else { crate::lex::Vectors::Dropped };
            let dest = self.new_segment_name()?;
            seg_names.push(dest.clone());
            crate::lex::merge_segments_with(&self.seg_dir.join(&dest), &[&seg], &[&alive], vectors)?;
            names.push(dest);
        }
        todo.extend(again);
        self.set_meta("segments", &serde_json::to_string(&names)?)?;
        self.lex = None;
        self.lex_ver = None;
        Ok(())
    }

    /// Drop the least recently parsed trees until the kept sources fit in TREE_CACHE_BYTES.
    fn evict_trees(&mut self) {
        let mut total: usize = self.trees.values().map(|t| t.1.source_len()).sum();
        if total <= TREE_CACHE_BYTES {
            return;
        }
        let mut by_age: Vec<(u64, String)> = self.trees.iter().map(|(p, t)| (t.0, p.clone())).collect();
        by_age.sort_unstable();
        for (_, p) in by_age {
            if total <= TREE_CACHE_BYTES {
                break;
            }
            total -= self.trees.remove(&p).map(|t| t.1.source_len()).unwrap_or(0);
        }
    }

    /// Write one segment and register it in the manifest (inside the caller's transaction).
    fn flush_segment(&mut self, builder: SegmentBuilder, seg_names: &mut Vec<String>) -> Result<()> {
        let t = Instant::now();
        let name = self.new_segment_name()?;
        builder.write(&self.seg_dir.join(&name))?;
        prof::add(&prof::SEGMENT, t);
        let mut names = self.segments();
        names.push(name.clone());
        self.set_meta("segments", &serde_json::to_string(&names)?)?;
        seg_names.push(name);
        Ok(())
    }

    /// `update()` for long-running processes: files edited since the last call become searchable.
    /// The directory is scanned at most once per `min_interval` seconds, and a scan that is cheap is
    /// not held back that long: the wait is capped at ten times what looking for changes cost.
    pub fn refresh(&mut self, min_interval: f64) -> Result<Option<UpdateStats>> {
        self.refresh_with(min_interval, true)
    }

    /// `refresh` before a read: when another process holds the write lock it fails at once
    /// instead of waiting, so the read can answer from the last committed index.
    pub fn refresh_or_fail(&mut self, min_interval: f64) -> Result<Option<UpdateStats>> {
        self.refresh_with(min_interval, false)
    }

    fn refresh_with(&mut self, min_interval: f64, wait: bool) -> Result<Option<UpdateStats>> {
        if let Some(last) = self.last_refresh {
            if last.elapsed().as_secs_f64() < min_interval.min(10.0 * self.scan_cost) {
                return Ok(None);
            }
        }
        if let Some(n) = self.newer().filter(|n| n.readable) {
            n.note();
            return Ok(None);
        }
        let st = self.update_with(false, wait, None);
        // a failing update is not retried on every call either; a busy lock costs nothing to try
        if !st.as_ref().is_err_and(|e| e.is::<Busy>()) {
            self.last_refresh = Some(Instant::now());
        }
        st.map(Some)
    }

    /// Forget every indexed file and segment, then index the repository again from scratch.
    pub fn rebuild(&mut self) -> Result<UpdateStats> {
        self.clear()?;
        self.update(true)
    }

    /// A new data version, inside the caller's write transaction; a chunk count recorded for the
    /// old one carries over (a compaction moves segment rows, not chunks).
    fn bump_version(&self) -> Result<String> {
        let old = self.meta("data_version");
        let v = (old.as_deref().and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1).to_string();
        self.set_meta("data_version", &v)?;
        if let Some(n) = counted(self.meta("chunks_at").as_deref(), old.as_deref()) {
            self.set_meta("chunks_at", &format!("{v} {n}"))?;
        }
        Ok(v)
    }

    /// Drop dead segments and merge small ones so queries touch few files.
    fn compact(&mut self) -> Result<()> {
        let (names, counts, sizes, alive): (Vec<String>, Vec<usize>, Vec<usize>, Vec<Vec<bool>>) = {
            let store = self.lex()?;
            (store.names.clone(), store.alive_counts(), store.segs.iter().map(|s| s.n).collect(), store.alive.clone())
        };
        let live: Vec<usize> = (0..counts.len()).filter(|&i| counts[i] > 0).collect();
        let mut merge: Vec<usize> = Vec::new();
        if !live.is_empty() {
            let rows: usize = live.iter().map(|&i| sizes[i]).sum();
            let dead_frac = |i: usize| 1.0 - counts[i] as f64 / sizes[i].max(1) as f64;
            let total_dead = 1.0 - live.iter().map(|&i| counts[i]).sum::<usize>() as f64 / rows as f64;
            if rows <= SMALL_INDEX_ROWS {
                // a small index is kept as one segment
                if live.len() > 1 || total_dead > 0.0 {
                    merge = live.clone();
                }
            } else {
                // smallest-first group that fits under the merge cap (bounded memory per merge)
                let mut order = live.clone();
                order.sort_by_key(|&i| counts[i]);
                let mut group: Vec<usize> = Vec::new();
                let mut sum = 0usize;
                for &i in &order {
                    if sum + counts[i] <= MAX_MERGE_ROWS {
                        group.push(i);
                        sum += counts[i];
                    }
                }
                let group_dead = group.iter().any(|&i| dead_frac(i) > 0.3);
                let many = live.len() > MAX_SEGMENTS;
                // segments too small for an inverted-file index are folded together once they add
                // up to one; everything in the group is merged when there are too many segments or
                // one of them is mostly dead; a lone mostly-dead segment is rewritten to purge it
                // by its size as written: a segment that has an inverted-file index stays out when a
                // few of its rows die, else every save near that size would write it again
                let smalls: Vec<usize> = group.iter().copied().filter(|&i| sizes[i] < crate::lex::IVF_MIN_ROWS).collect();
                let smalls_sum: usize = smalls.iter().map(|&i| counts[i]).sum();
                if group.len() > 1 && (many || group_dead) {
                    merge = group;
                } else if smalls.len() > 1 && smalls_sum >= crate::lex::IVF_MIN_ROWS {
                    merge = smalls;
                } else if let Some(&i) = live.iter().find(|&&i| dead_frac(i) > 0.3 && counts[i] <= MAX_MERGE_ROWS) {
                    merge = vec![i];
                }
            }
        }
        if live.len() == names.len() && merge.is_empty() {
            return Ok(());
        }
        let mut new_names: Vec<String> = live.iter().filter(|i| !merge.contains(i)).map(|&i| names[i].clone()).collect();
        if !merge.is_empty() {
            retry_busy(|| self.db.execute_batch("BEGIN IMMEDIATE"))?;
            let dest = self.new_segment_name()?;
            retry_busy(|| self.db.execute_batch("COMMIT"))?;
            {
                let store = self.lex.as_ref().unwrap();
                let segs: Vec<&crate::lex::Segment> = merge.iter().map(|&i| &store.segs[i]).collect();
                let al: Vec<&[bool]> = merge.iter().map(|&i| alive[i].as_slice()).collect();
                merge_segments(&self.seg_dir.join(&dest), &segs, &al)?;
            }
            new_names.push(dest);
        }
        retry_busy(|| self.db.execute_batch("BEGIN IMMEDIATE"))?;
        if self.segments() != names {
            // another writer changed the manifest while we were merging: our plan is stale. Drop the
            // merged segment (nothing refers to it) and let the next update compact again.
            self.db.execute_batch("ROLLBACK")?;
            for n in new_names.iter().filter(|n| !names.contains(n)) {
                let _ = std::fs::remove_dir_all(self.seg_dir.join(n));
            }
            self.lex = None;
            self.lex_ver = None;
            return Ok(());
        }
        self.set_meta("segments", &serde_json::to_string(&new_names)?)?;
        self.bump_version()?;
        retry_busy(|| self.db.execute_batch("COMMIT"))?;
        self.lex = None;
        self.lex_ver = None;
        let keep: HashSet<&String> = new_names.iter().collect();
        for n in &names {
            if !keep.contains(n) {
                let _ = std::fs::remove_dir_all(self.seg_dir.join(n));
            }
        }
        Ok(())
    }

    fn insert(&self, rel: &str, rows: Vec<NewRow>, mut next_id: i64) -> Result<i64> {
        let mut ins = self.db.prepare_cached(
            "INSERT INTO chunks(id,path,kind,name,qualname,name_key,qual_key,start,end,is_test,ndoc,nbody,text,name_tok) \
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        )?;
        let mut refs = self.db.prepare_cached("INSERT INTO refs(chunk_id,callee) VALUES(?1,?2)")?;
        for (c, calls, blob) in rows {
            let cid = next_id;
            next_id += 1;
            ins.execute(params![cid, rel, c.kind, c.name, c.qualname, c.name_key, c.qual_key, c.start, c.end, c.is_test, c.ndoc, c.nbody, blob, c.name_tok])?;
            for callee in calls {
                refs.execute(params![cid, callee])?;
            }
        }
        Ok(next_id)
    }

    /// Chunks vanish from SQLite; their segment rows simply stop being alive. Returns the ids that died.
    fn delete_file(&self, rel: &str) -> Result<Vec<i64>> {
        let mut q = self.db.prepare_cached("SELECT id FROM chunks WHERE path=?1")?;
        let ids: Vec<i64> = q.query_map([rel], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
        if !ids.is_empty() {
            self.db.execute("DELETE FROM refs WHERE chunk_id IN (SELECT id FROM chunks WHERE path=?1)", [rel])?;
            self.db.execute("DELETE FROM chunks WHERE path=?1", [rel])?;
        }
        self.db.execute("DELETE FROM imports WHERE path=?1", [rel])?;
        self.db.execute("DELETE FROM files WHERE path=?1", [rel])?;
        Ok(ids)
    }

    // ---- lookups
    pub fn chunk(&self, cid: i64) -> Result<Option<ChunkRow>> {
        Ok(self.db.query_row(&format!("SELECT {CHUNK_COLS} FROM chunks WHERE id=?1"), [cid], ChunkRow::from_row).optional()?)
    }

    /// Smallest non-window chunk containing the line (falls back to windows).
    pub fn chunk_at(&self, path: &str, line: i64) -> Result<Option<ChunkRow>> {
        Ok(self
            .db
            .query_row(
                &format!("SELECT {CHUNK_COLS} FROM chunks WHERE path=?1 AND start<=?2 AND end>=?2 ORDER BY (kind='window'), (end-start) LIMIT 1"),
                params![path, line],
                ChunkRow::from_row,
            )
            .optional()?)
    }

    /// Verify the index. Never repairs anything: `update()` / `rebuild()` do that.
    pub fn check(&mut self, files: bool) -> Result<CheckReport> {
        let mut r = CheckReport::default();
        if let Some(n) = self.newer() {
            n.note();
        }
        let integrity: String = self.db.query_row("PRAGMA integrity_check", [], |row| row.get(0)).unwrap_or_else(|e| format!("error: {e}"));
        r.database_ok = integrity == "ok";
        if !r.database_ok {
            r.problems.push(format!("database integrity: {integrity}"));
        }
        r.chunks_db = self.db.query_row("SELECT COUNT(*) FROM chunks WHERE kind != 'constant'", [], |row| row.get(0))?;
        let names = self.segments();
        r.segments = names.len();
        match LexStore::open(&self.seg_dir, &names) {
            Ok(mut store) => {
                for (name, seg) in names.iter().zip(&store.segs) {
                    if let Err(e) = seg.validate_deep() {
                        r.segments_ok = false;
                        r.problems.push(format!("segment {name}: {e}"));
                    }
                }
                let mut live: Vec<i64> = Vec::new();
                let mut st = self.db.prepare("SELECT id FROM chunks ORDER BY id")?;
                for id in st.query_map([], |row| row.get::<_, i64>(0))? {
                    live.push(id?);
                }
                store.prepare(Some(&live))?;
                r.chunks_alive = store.n_alive as i64;
                // every chunk in the database must have a row in exactly one segment
                let in_segments: usize = store.segs.iter().map(|s| s.ids().iter().filter(|id| live.binary_search(id).is_ok()).count()).sum();
                if in_segments as i64 != r.chunks_db {
                    r.problems.push(format!("{} chunks in the database, {} of them present in segments", r.chunks_db, in_segments));
                }
            }
            Err(e) => {
                r.segments_ok = false;
                r.problems.push(format!("segments unreadable: {e:#}"));
            }
        }
        r.files_indexed = self.db.query_row("SELECT COUNT(*) FROM files WHERE hash != 'SKIP'", [], |row| row.get::<_, i64>(0))? as usize;
        if files {
            let mut st = self.db.prepare("SELECT path, hash FROM files WHERE hash != 'SKIP'")?;
            let rows: Vec<(String, String)> = st.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            for (rel, hash) in rows {
                match std::fs::read(self.root.join(&rel)) {
                    Ok(raw) => {
                        if format!("{:x}", Sha1::digest(&raw)) != hash {
                            r.files_changed += 1;
                        }
                    }
                    Err(_) => r.files_missing += 1,
                }
            }
            if r.files_changed + r.files_missing > 0 {
                r.problems.push(format!(
                    "{} files changed and {} missing since indexing (stale, not corrupt: `scrutate index` refreshes them)",
                    r.files_changed, r.files_missing
                ));
            }
        }
        Ok(r)
    }

    /// Whether the committed index holds any file.
    /// Whether the index holds vectors made by this process's vector model, so a query vector
    /// can be compared with them.
    pub fn vectors_fit(&self) -> bool {
        self.sig != "none" && self.meta("model_sig").as_deref() == Some(self.sig.as_str())
    }

    pub fn has_files(&self) -> Result<bool> {
        Ok(self.db.query_row("SELECT EXISTS(SELECT 1 FROM files)", [], |r| r.get(0))?)
    }

    pub fn stats(&self) -> Result<serde_json::Value> {
        let q = |s: &str| -> Result<i64> { Ok(self.db.query_row(s, [], |r| r.get(0))?) };
        Ok(serde_json::json!({
            "files": q("SELECT COUNT(*) FROM files WHERE hash != 'SKIP'")?,
            "files_skipped": q("SELECT COUNT(*) FROM files WHERE hash = 'SKIP'")?,
            "chunks": q("SELECT COUNT(*) FROM chunks")?,
            "symbols": q("SELECT COUNT(*) FROM chunks WHERE kind NOT IN ('window','module')")?,
            "call_edges": q("SELECT COUNT(*) FROM refs")?,
            "imports": q("SELECT COUNT(*) FROM imports")?,
            "semantic": self.models.semantic_enabled(),
            "segments": self.segments().len(),
        }))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_index_another_process_already_cleared_for_this_version_is_left_alone() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_clear_race_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let files = |ix: &super::Index| ix.db.query_row("SELECT COUNT(*) FROM files", [], |r| r.get::<_, i64>(0)).unwrap();
        // what a process that found the old schema meets once another one cleared the index for
        // this version and is building it: this version's stamps, and the write lock held
        let other = rusqlite::Connection::open(ix.dir.join("index.db")).unwrap();
        other.execute_batch("BEGIN IMMEDIATE").unwrap();
        let t0 = std::time::Instant::now();
        ix.clear_with(Some("none")).unwrap();
        assert!(t0.elapsed().as_secs() < 5, "{:?}", t0.elapsed());
        other.execute_batch("ROLLBACK").unwrap();
        ix.clear_with(Some("none")).unwrap();
        assert_eq!(files(&ix), 1);
        ix.set_meta("schema_version", &(super::SCHEMA_VERSION - 1).to_string()).unwrap();
        ix.clear_with(Some("none")).unwrap();
        assert_eq!(files(&ix), 0);
        assert_eq!(ix.meta("schema_version"), Some(super::SCHEMA_VERSION.to_string()));
        drop((ix, other));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn update_counts_as_a_refresh() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_refresh_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def alpha():\n    return 1\n").unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        assert!(ix.last_refresh.is_none());
        ix.update(false).unwrap();
        assert!(ix.last_refresh.is_some());
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn update_frees_the_word_caches_of_every_thread() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_caches_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..40 {
            std::fs::write(root.join(format!("m{i}.py")), format!("def handler_{i}(request):\n    return parse_body(request)\n")).unwrap();
        }
        // a pool of its own: other tests fill the global pool's caches at the same time
        let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
        let left = pool.install(|| {
            let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
            ix.update(false).unwrap();
            rayon::broadcast(|_| crate::text::cache_len()).into_iter().sum::<usize>() + crate::text::cache_len()
        });
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(left, 0);
    }

    #[test]
    fn a_large_segment_is_not_rewritten_when_a_few_of_its_rows_die() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_compact_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let funcs = |from: usize, n: usize| (from..from + n).map(|i| format!("def f{i}():\n    return {i}\n\n")).collect::<String>();
        // one segment just over the size that gets an inverted-file index
        std::fs::write(root.join("big.py"), funcs(0, crate::lex::IVF_MIN_ROWS - 2)).unwrap();
        std::fs::write(root.join("small.py"), funcs(100_000, 10)).unwrap();
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let first = ix.segments();
        let rows = ix.db.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get::<_, i64>(0)).unwrap() as usize;
        assert!(first.len() == 1 && rows >= crate::lex::IVF_MIN_ROWS && rows - 11 < crate::lex::IVF_MIN_ROWS, "{first:?} {rows}");
        // a few of its rows die, and the edit arrives as a small segment of its own
        std::fs::write(root.join("small.py"), funcs(200_000, 10)).unwrap();
        ix.update(false).unwrap();
        let now = ix.segments();
        assert!(now.contains(&first[0]) && now.len() == 2, "the large segment was written again: {first:?} -> {now:?}");
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_watch_burst_of_files_updates_those_files_alone() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_burst_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for name in ["a.py", "b.py", "c.py"] {
            std::fs::write(root.join(name), "def one():\n    return 1\n").unwrap();
        }
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let paths = |ix: &super::Index| {
            ix.db.prepare("SELECT path FROM files ORDER BY path").unwrap().query_map([], |r| r.get(0)).unwrap().collect::<Result<Vec<String>, _>>().unwrap()
        };
        std::fs::write(root.join("a.py"), "def two():\n    return 2\n").unwrap();
        std::fs::write(root.join("b.py"), "def two():\n    return 2\n").unwrap();
        std::fs::remove_file(root.join("c.py")).unwrap();
        std::fs::write(root.join("d.py"), "def new():\n    return 3\n").unwrap();
        let named = crate::watch::BurstFiles { code: vec!["a.py".into(), "c.py".into(), "d.py".into()], text: vec![] };
        let st = crate::watch::with_burst(Some(named), || ix.update(false)).unwrap();
        assert_eq!((st.files_changed, st.files_removed), (2, 1));
        assert_eq!(paths(&ix), ["a.py", "b.py", "d.py"]);
        // b.py was not named, so only a walk sees it
        assert_eq!(ix.stale(&["b.py"]).unwrap(), ["b.py"]);
        assert_eq!(ix.update(false).unwrap().files_changed, 1);
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn files_that_keep_no_tree_take_none_of_the_tree_cache() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_trees_mixed_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for i in 0..20 {
            std::fs::write(root.join(format!("a{i}.c")), format!("int f{i}(int x) {{ return x + {i}; }}\n")).unwrap();
        }
        // listed after the C files, and more of them than the cache holds
        let body: String = (0..400).map(|i| format!("def f{i}(x):\n    return x + {i}\n")).collect();
        for i in 0..300 {
            std::fs::write(root.join(format!("z{i}.py")), &body).unwrap();
        }
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let held = ix.trees.len();
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(held, 20);
    }

    #[test]
    fn a_tree_too_large_for_the_room_left_leaves_that_room_to_smaller_trees() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_trees_large_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // three trees of 0.9 MB, of which two fit; the small trees come in the next batch
        for i in 0..3 {
            std::fs::write(root.join(format!("a{i}.c")), "int g(void);\n".repeat(69_000)).unwrap();
        }
        for i in 0..260 {
            std::fs::write(root.join(format!("b{i:03}.h")), "class B { public: int f(); };\n").unwrap();
        }
        for i in 0..20 {
            std::fs::write(root.join(format!("z{i}.c")), format!("int f{i}(int x) {{ return x + {i}; }}\n")).unwrap();
        }
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let large = ix.trees.keys().filter(|p| p.starts_with('a')).count();
        let small = ix.trees.keys().filter(|p| p.starts_with('z')).count();
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!((large, small), (2, 20));
    }

    #[test]
    fn the_tree_cache_counts_the_language_a_file_parses_as_and_only_files_that_are_parsed() {
        let tree = |name: &str, extra: &dyn Fn(&std::path::Path)| {
            let root = std::env::temp_dir().join(format!("scrutate_ix_trees_{name}_{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(&root).unwrap();
            for i in 0..20 {
                std::fs::write(root.join(format!("a{i}.c")), format!("int f{i}(int x) {{ return x + {i}; }}\n")).unwrap();
            }
            extra(&root);
            let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
            ix.update(false).unwrap();
            let c_trees = ix.trees.keys().filter(|p| p.ends_with(".c")).count();
            let peak = ix.trees_peak;
            drop(ix);
            let _ = std::fs::remove_dir_all(&root);
            (c_trees, peak)
        };
        // C++ headers keep no tree, and a file over the size limit is not parsed at all
        let cpp: String = (0..300).map(|i| format!("class C{i} {{ public: int f(); }};\n")).collect();
        let (c_trees, _) = tree("cpp", &|root| {
            for i in 0..300 {
                std::fs::write(root.join(format!("z{i}.h")), &cpp).unwrap();
            }
            std::fs::write(root.join("zz.c"), "int g(void);\n".repeat(110_000)).unwrap();
        });
        assert_eq!(c_trees, 20);
        // Objective-C `.m` files keep trees, within the bound
        let objc: String = (0..300).map(|i| format!("- (int)f{i} {{ return {i}; }}\n")).collect();
        let (c_trees, peak) = tree("objc", &|root| {
            for i in 0..300 {
                std::fs::write(root.join(format!("m{i}.m")), format!("@implementation M{i}\n{objc}@end\n")).unwrap();
            }
        });
        assert!(peak <= super::TREE_CACHE_BYTES, "peak {peak}");
        assert_eq!(c_trees, 20);
    }

    #[test]
    fn a_build_of_many_c_files_holds_no_more_trees_than_the_cache_bound() {
        let root = std::env::temp_dir().join(format!("scrutate_ix_trees_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let body: String = (0..200).map(|i| format!("int f{i}(int x) {{ return x + {i}; }}\n")).collect();
        for i in 0..1000 {
            std::fs::write(root.join(format!("m{i}.c")), format!("/* {i} */\n{body}")).unwrap();
        }
        let mut ix = super::Index::open(&root, crate::models::ModelSet::none()).unwrap();
        ix.update(false).unwrap();
        let held: usize = ix.trees.values().map(|t| t.1.source_len()).sum();
        let peak = ix.trees_peak;
        drop(ix);
        let _ = std::fs::remove_dir_all(&root);
        assert!(held <= super::TREE_CACHE_BYTES, "{held}");
        assert!(peak <= super::TREE_CACHE_BYTES, "peak {peak}");
    }
}
