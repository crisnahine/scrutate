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

use crate::lex::{merge_segments, pack_bags, quantize, FileLex, LexStore, QVecs, SegmentBuilder, NF};
use crate::parse::{parse_file_with, PrevParse};
use crate::semantic::SemanticModel;
use crate::text::{is_test_path, name_key, tokenize2, BODY_CAP};

pub const SCHEMA_VERSION: u32 = 13;
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
CREATE TABLE IF NOT EXISTS files(path TEXT PRIMARY KEY, size INTEGER, mtime_ns INTEGER, hash TEXT);
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
    let k = model.map(|m| m.k).unwrap_or(0);
    let prof = prof::enabled();
    let t = Instant::now();
    let (parsed, tree) = parse_file_with(rel, source, prev);
    if prof {
        prof::add(&prof::PARSE, t);
    }
    for c in parsed.chunks {
        let t = Instant::now();
        let qual = match c.qualname.rfind('.') {
            Some(i) => c.qualname[..i].to_string(),
            None => String::new(),
        };
        let (name_s, name_w) = tokenize2(&c.name, None);
        let (qual_s, qual_w) = tokenize2(&qual, None);
        let (sig_s, sig_w) = tokenize2(&c.sig, Some(80));
        let (doc_s, doc_w) = tokenize2(&c.doc, Some(200));
        let (body_s, body_w) = tokenize2(&c.body, Some(BODY_CAP));
        bags.push(vec![
            bag(name_s.iter().chain(&name_w).cloned(), None),
            bag(qual_s.iter().chain(&qual_w).cloned(), None),
            bag(sig_s.iter().chain(&sig_w).cloned(), Some(TF_CAP)),
            bag(doc_s.iter().chain(&doc_w).cloned(), Some(TF_CAP)),
            bag(body_s.iter().chain(&body_w).cloned(), Some(TF_CAP)),
            path_bag.clone(),
        ]);
        if prof {
            prof::add(&prof::TOKENIZE, t);
        }
        let t = Instant::now();
        if let Some(m) = model {
            let mut z = vec![0f32; 2 * k];
            if c.kind != "window" {
                if let Some(za) = m.encode_chunk(&name_s, &qual_s, &sig_s, &doc_s, &[]) {
                    z[..k].copy_from_slice(&za);
                }
            }
            if let Some(zb) = m.encode_chunk(&[], &[], &[], &[], &body_s) {
                z[k..].copy_from_slice(&zb);
            }
            let mut q = vec![0i8; 2 * k];
            let (qa, qb) = q.split_at_mut(k);
            vecs.scale.push(quantize(&z[..k], qa));
            vecs.scale.push(quantize(&z[k..], qb));
            vecs.q.extend(q);
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

enum Processed {
    Gone,
    Skip(u64, i64),
    Same(u64, i64),
    New(u64, i64, String, Vec<NewRow>, Box<FileLex>, Vec<(i64, String)>, Option<PrevParse>),
}

fn mtime_ns(md: &std::fs::Metadata) -> i64 {
    md.modified().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_nanos() as i64).unwrap_or(0)
}

fn process(root: &Path, rel: &str, known_hash: Option<&str>, model: Option<&SemanticModel>, prev: Option<PrevParse>) -> Processed {
    let full = root.join(rel);
    let Ok(md) = std::fs::metadata(&full) else {
        return Processed::Gone;
    };
    let (size, mtime) = (md.len(), mtime_ns(&md));
    if size > MAX_FILE_BYTES {
        return Processed::Skip(size, mtime);
    }
    let Ok(raw) = std::fs::read(&full) else {
        return Processed::Gone;
    };
    let Some(source) = decode_source(&raw) else {
        return Processed::Skip(size, mtime);
    };
    let h = format!("{:x}", Sha1::digest(&raw));
    if Some(h.as_str()) == known_hash {
        return Processed::Same(size, mtime);
    }
    let built = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| build_rows(rel, &source, model, prev)));
    match built {
        Ok((rows, lex, imports, tree)) => Processed::New(size, mtime, h, rows, Box::new(lex), imports, tree),
        Err(_) => {
            eprintln!("[scrutate] skipped {rel}: parser failure");
            Processed::Skip(size, mtime)
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

/// SQLite errors that mean the file is not a usable database (as opposed to busy or I/O trouble).
fn is_corruption(e: &rusqlite::Error) -> bool {
    use rusqlite::ffi::ErrorCode::*;
    matches!(e, rusqlite::Error::SqliteFailure(f, _) if matches!(f.code, DatabaseCorrupt | NotADatabase))
}

/// Whether `root` has an index database (built or being built).
pub fn exists(root: &Path) -> bool {
    root.join(".scrutate").join("index.db").exists()
}

/// True when an error anywhere in `e`'s chain is SQLite corruption.
pub fn error_is_corruption(e: &anyhow::Error) -> bool {
    e.chain().any(|c| c.downcast_ref::<rusqlite::Error>().map(is_corruption).unwrap_or(false))
}

/// Delete the index files of `root` (used after an operation reported database corruption on a
/// database too large for the open-time integrity check). The caller reopens afterwards.
pub fn wipe_index(root: &Path) {
    let dir = root.join(".scrutate");
    Index::wipe(&dir, &dir.join("seg"));
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
}

pub struct Index {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub seg_dir: PathBuf,
    pub model: Option<Arc<SemanticModel>>,
    pub db: Connection,
    /// print indexing progress to stderr when it is a terminal (set by the CLI)
    pub progress: bool,
    lex: Option<LexStore>,
    lex_ver: Option<String>,
    last_refresh: Option<Instant>,
    scan_cost: f64,
    /// last syntax tree of recently indexed files, with a use counter for eviction
    trees: HashMap<String, (u64, PrevParse)>,
    tree_tick: u64,
}

impl Index {
    pub fn open(root: &Path, model: Option<Arc<SemanticModel>>) -> Result<Index> {
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        let dir = root.join(".scrutate");
        let seg_dir = dir.join("seg");
        std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
        let gi = dir.join(".gitignore");
        if !gi.exists() {
            std::fs::write(&gi, "*\n")?; // the index is a cache; never commit it
        }
        let db_path = dir.join("index.db");
        let sem_sig = model.as_ref().map(|m| m.signature()).unwrap_or_else(|| "none".into());
        let triage = Self::triage(&db_path, &sem_sig);
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
        setup(&db)?;
        let db = match retry_busy(|| db.execute_batch(SCHEMA)) {
            Ok(()) => db,
            Err(e) => {
                // opened but cannot hold the schema: garbage after all
                eprintln!("[scrutate] index database unusable ({e}); rebuilding");
                drop(db);
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
            model,
            db,
            progress: false,
            lex: None,
            lex_ver: None,
            last_refresh: None,
            scan_cost: 1.0,
            trees: HashMap::new(),
            tree_tick: 0,
        };
        if let Triage::Rebuild(why) = triage {
            // an index of another schema or model: drop its contents *inside* the database, so
            // other processes using the same index keep a consistent (if briefly empty) view
            eprintln!("[scrutate] {why}; rebuilding the index");
            ix.clear()?;
            ix.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
            ix.set_meta("model_sig", &sem_sig)?;
        } else {
            ix.set_meta("schema_version", &SCHEMA_VERSION.to_string())?;
            ix.set_meta("model_sig", &sem_sig)?;
        }
        ix.gc_segments();
        Ok(ix)
    }

    /// Forget every indexed file and segment (inside one write transaction).
    fn clear(&mut self) -> Result<()> {
        self.lex = None;
        self.lex_ver = None;
        retry_busy(|| self.db.execute_batch("BEGIN IMMEDIATE"))?;
        let old = self.segments();
        self.db.execute_batch("DELETE FROM refs; DELETE FROM imports; DELETE FROM chunks; DELETE FROM files;")?;
        self.set_meta("segments", "[]")?;
        self.bump_version()?;
        retry_busy(|| self.db.execute_batch("COMMIT"))?;
        for n in old {
            let _ = std::fs::remove_dir_all(self.seg_dir.join(n));
        }
        Ok(())
    }

    /// What to do with the index files found on disk. Only an unmistakably damaged database is
    /// declared garbage: a locked one (another process writing) is reused, since deleting the files
    /// of a live database would break every other process using it.
    fn triage(db_path: &Path, sem_sig: &str) -> Triage {
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
        if get("schema_version").as_deref() != Some(SCHEMA_VERSION.to_string().as_str()) {
            return Triage::Rebuild("index schema changed");
        }
        if get("model_sig").as_deref() != Some(sem_sig) {
            return Triage::Rebuild("semantic model changed");
        }
        Triage::Reuse
    }

    pub(crate) fn wipe(dir: &Path, seg_dir: &Path) {
        for name in ["index.db", "index.db-wal", "index.db-shm", "vectors.npz"] {
            let _ = std::fs::remove_file(dir.join(name));
        }
        let _ = std::fs::remove_dir_all(seg_dir);
    }

    // ---- meta helpers
    pub fn meta(&self, key: &str) -> Option<String> {
        self.db.query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0)).optional().ok().flatten()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> Result<()> {
        retry_busy(|| self.db.execute("INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)", params![key, value]))?;
        Ok(())
    }

    pub fn segments(&self) -> Vec<String> {
        serde_json::from_str(&self.meta("segments").unwrap_or_else(|| "[]".into())).unwrap_or_default()
    }

    fn new_segment_name(&self) -> Result<String> {
        let n: i64 = self.meta("seg_counter").and_then(|v| v.parse().ok()).unwrap_or(0) + 1;
        self.set_meta("seg_counter", &n.to_string())?;
        Ok(format!("s{n:06}"))
    }

    /// Delete segment directories no manifest refers to (left by a crash), unless brand new.
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

    /// The segment store, with statistics for the chunks that are alive right now.
    pub fn lex(&mut self) -> Result<&mut LexStore> {
        let ver = self.meta("data_version").unwrap_or_else(|| "0".into());
        let names = self.segments();
        if self.lex.as_ref().map(|l| l.names != names).unwrap_or(true) {
            let mut names = names;
            let mut attempt = 0;
            let store = loop {
                match LexStore::open(&self.seg_dir, &names) {
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
            let n_live: i64 = self.db.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
            if n_rows as i64 == n_live {
                store.prepare(None); // nothing was ever deleted: no need to read every id
            } else {
                let mut st = self.db.prepare("SELECT id FROM chunks ORDER BY id")?;
                let live: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?;
                store.prepare(Some(&live));
            }
            self.lex_ver = Some(ver);
        }
        Ok(self.lex.as_mut().unwrap())
    }

    // ---- update
    pub fn update(&mut self, force: bool) -> Result<UpdateStats> {
        let t0 = Instant::now();
        let mut st = UpdateStats { semantic: self.model.is_some(), ..Default::default() };
        // if the statistics held in memory describe exactly the state we start from, they can be
        // patched afterwards instead of recomputed
        let mut removed: Vec<i64> = Vec::new();
        let mut seg_names: Vec<String> = Vec::new();
        retry_busy(|| self.db.execute_batch("BEGIN IMMEDIATE"))?; // one writer at a time; readers keep working (WAL)
        let cur_ver = self.meta("data_version").unwrap_or_else(|| "0".into());
        let in_sync =
            self.lex.is_some() && self.lex_ver.as_deref() == Some(cur_ver.as_str()) && self.lex.as_ref().map(|l| l.names == self.segments()).unwrap_or(false);
        let changed = match self.update_inner(force, &mut st, &mut removed, &mut seg_names) {
            Ok(c) => c,
            Err(e) => {
                let _ = self.db.execute_batch("ROLLBACK");
                for name in &seg_names {
                    let _ = std::fs::remove_dir_all(self.seg_dir.join(name));
                }
                return Err(e);
            }
        };
        retry_busy(|| self.db.execute_batch("COMMIT"))?;
        if changed {
            let mut patched = false;
            if in_sync {
                let added: Vec<String> = seg_names.clone();
                if self.lex.as_mut().unwrap().apply(&removed, &added).is_ok() {
                    self.lex_ver = self.meta("data_version");
                    patched = true;
                }
            }
            if !patched {
                self.lex = None;
                self.lex_ver = None;
            }
            self.compact()?;
            let _ = self.db.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
        }
        st.chunks = self.db.query_row("SELECT COUNT(*) FROM chunks", [], |r| r.get(0))?;
        st.seconds = t0.elapsed().as_secs_f64();
        if prof::enabled() {
            prof::report(st.seconds);
        }
        Ok(st)
    }

    /// Body of `update()`, inside the write transaction. -> whether anything changed.
    fn update_inner(&mut self, force: bool, st: &mut UpdateStats, removed: &mut Vec<i64>, seg_names: &mut Vec<String>) -> Result<bool> {
        let mut changed = false;
        let scan_t0 = Instant::now();
        let mut known: HashMap<String, (i64, i64, String)> = HashMap::new();
        {
            let mut q = self.db.prepare("SELECT path,size,mtime_ns,hash FROM files")?;
            for r in q.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?)))? {
                let (p, s, m, h) = r?;
                known.insert(p, (s, m, h));
            }
        }
        let files = crate::files::list_files(&self.root);
        st.files_total = files.len();
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
                    if k.0 == md.len() as i64 && k.1 == mtime_ns(&md) {
                        continue; // stat unchanged: no read, no hash
                    }
                }
            }
            todo.push((rel.clone(), if force { None } else { known.get(rel).map(|k| k.2.clone()) }));
        }
        self.scan_cost = scan_t0.elapsed().as_secs_f64();
        let mut next_id: i64 = self.meta("next_id").and_then(|v| v.parse().ok()).unwrap_or(1); // ids are never reused
        for g in &gone {
            removed.extend(self.delete_file(g)?);
            st.files_removed += 1;
            changed = true;
        }
        let k = self.model.as_ref().map(|m| m.k).unwrap_or(0);
        let mut builder = SegmentBuilder::new(k);
        if let (Some(m), false) = (&self.model, todo.is_empty()) {
            m.warm(); // build the weight tables once, before the parallel pass
        }
        let model = self.model.clone();
        let root = self.root.clone();
        let progress = self.progress && std::io::IsTerminal::is_terminal(&std::io::stderr());
        let mut done = 0usize;
        let mut last_tick = Instant::now();
        for batch in todo.chunks(256) {
            let prevs: Vec<Option<PrevParse>> = batch.iter().map(|(rel, _)| self.trees.remove(rel).map(|t| t.1)).collect();
            let results: Vec<(String, Processed)> =
                batch.par_iter().zip(prevs).map(|((rel, kh), prev)| (rel.clone(), process(&root, rel, kh.as_deref(), model.as_deref(), prev))).collect();
            done += batch.len();
            if progress && (last_tick.elapsed().as_millis() >= 200 || done == todo.len()) {
                eprint!("\r[scrutate] indexing {done}/{} files...", todo.len());
                last_tick = Instant::now();
            }
            for (rel, res) in results {
                match res {
                    Processed::Gone => {}
                    Processed::Skip(size, mtime) => {
                        // binary / too large / unparsable: remember so we never re-read it
                        removed.extend(self.delete_file(&rel)?);
                        self.db.execute("INSERT OR REPLACE INTO files(path,size,mtime_ns,hash) VALUES(?1,?2,?3,'SKIP')", params![rel, size as i64, mtime])?;
                    }
                    Processed::Same(size, mtime) => {
                        // content identical, only the mtime moved
                        self.db.execute("UPDATE files SET size=?1, mtime_ns=?2 WHERE path=?3", params![size as i64, mtime, rel])?;
                    }
                    Processed::New(size, mtime, h, rows, lex, imports, tree) => {
                        if let Some(tree) = tree.filter(|t| t.source_len() <= TREE_CACHE_BYTES) {
                            self.tree_tick += 1;
                            self.trees.insert(rel.clone(), (self.tree_tick, tree));
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
                        self.db.execute("INSERT OR REPLACE INTO files(path,size,mtime_ns,hash) VALUES(?1,?2,?3,?4)", params![rel, size as i64, mtime, h])?;
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
        }
        if progress && done > 0 {
            eprint!("\r{}\r", " ".repeat(60)); // clear the progress line
        }
        self.evict_trees();
        if builder.n > 0 {
            self.flush_segment(builder, seg_names)?;
        }
        self.set_meta("next_id", &next_id.to_string())?;
        changed = changed || !removed.is_empty();
        if changed {
            self.bump_version()?; // same transaction as the manifest: readers never see one without the other
        }
        Ok(changed)
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
        if let Some(last) = self.last_refresh {
            if last.elapsed().as_secs_f64() < min_interval.min(10.0 * self.scan_cost) {
                return Ok(None);
            }
        }
        let st = self.update(false)?;
        self.last_refresh = Some(Instant::now());
        Ok(Some(st))
    }

    /// Forget every indexed file and segment, then index the repository again from scratch.
    pub fn rebuild(&mut self) -> Result<UpdateStats> {
        self.clear()?;
        self.update(true)
    }

    fn bump_version(&self) -> Result<String> {
        let v = self.meta("data_version").and_then(|v| v.parse::<i64>().ok()).unwrap_or(0) + 1;
        self.set_meta("data_version", &v.to_string())?;
        Ok(v.to_string())
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
                let smalls: Vec<usize> = group.iter().copied().filter(|&i| counts[i] < crate::lex::IVF_MIN_ROWS).collect();
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
        let integrity: String = self.db.query_row("PRAGMA integrity_check", [], |row| row.get(0)).unwrap_or_else(|e| format!("error: {e}"));
        r.database_ok = integrity == "ok";
        if !r.database_ok {
            r.problems.push(format!("database integrity: {integrity}"));
        }
        r.chunks_db = self.db.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
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
                store.prepare(Some(&live));
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
        if files {
            let mut st = self.db.prepare("SELECT path, hash FROM files WHERE hash != 'SKIP'")?;
            let rows: Vec<(String, String)> = st.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?.collect::<rusqlite::Result<_>>()?;
            r.files_indexed = rows.len();
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

    pub fn stats(&self) -> Result<serde_json::Value> {
        let q = |s: &str| -> Result<i64> { Ok(self.db.query_row(s, [], |r| r.get(0))?) };
        Ok(serde_json::json!({
            "files": q("SELECT COUNT(*) FROM files WHERE hash != 'SKIP'")?,
            "files_skipped": q("SELECT COUNT(*) FROM files WHERE hash = 'SKIP'")?,
            "chunks": q("SELECT COUNT(*) FROM chunks")?,
            "symbols": q("SELECT COUNT(*) FROM chunks WHERE kind NOT IN ('window','module')")?,
            "call_edges": q("SELECT COUNT(*) FROM refs")?,
            "imports": q("SELECT COUNT(*) FROM imports")?,
            "semantic": self.model.is_some(),
            "segments": self.segments().len(),
        }))
    }

    pub fn nf() -> usize {
        NF
    }
}
