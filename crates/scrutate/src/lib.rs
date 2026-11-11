//! Scrutate engine: incremental code index (BM25 postings + semantic vectors + call graph), a learned
//! ranker, and the search layer used by the CLI and the MCP server.

// Index-driven loops over parallel arrays (postings, feature matrices) read better with explicit
// indices than with zipped iterators; this lint is noise here.
#![allow(clippy::needless_range_loop)]

pub mod archive;
pub mod cli;
pub mod config;
pub mod files;
pub mod fmt;
pub mod fuzzyre;
pub mod index;
pub mod instant;
pub mod intel;
pub mod langs;
pub mod lex;
pub mod lsp;
pub mod mcp;
pub mod npy;
pub mod ops;
pub mod parse;
pub mod rank;
pub mod resolve;
pub mod search;
pub mod semantic;
pub mod text;
pub mod textindex;
pub mod tscache;
pub mod watch;

use std::path::Path;
use std::sync::Arc;

use anyhow::Result;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

static SEMANTIC_NPZ: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../models/semantic.npz"));
static RANKER_NPZ: &[u8] = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/../../models/ranker.npz"));

/// The bundled semantic model, or the one named by `SCRUTATE_SEMANTIC` (`none` disables it).
pub fn default_model() -> Result<Option<Arc<semantic::SemanticModel>>> {
    match std::env::var("SCRUTATE_SEMANTIC").ok().as_deref() {
        Some("none") | Some("") => Ok(None),
        Some(p) => Ok(Some(Arc::new(semantic::SemanticModel::load(Path::new(p))?))),
        None => Ok(Some(Arc::new(semantic::SemanticModel::from_bytes(SEMANTIC_NPZ)?))),
    }
}

/// The bundled ranker, or the one named by `SCRUTATE_RANKER` (`none` disables it: linear blend only).
pub fn default_ranker() -> Result<Option<Arc<rank::TreeEnsemble>>> {
    match std::env::var("SCRUTATE_RANKER").ok().as_deref() {
        Some("none") | Some("") => Ok(None),
        Some(p) => Ok(Some(Arc::new(rank::TreeEnsemble::load(Path::new(p))?))),
        None => Ok(Some(Arc::new(rank::TreeEnsemble::from_bytes(RANKER_NPZ)?))),
    }
}

/// Open (or create) the index of `root` and a searcher on it, with the default models.
pub fn open(root: &Path) -> Result<search::Searcher> {
    let ix = index::Index::open(root, default_model()?)?;
    Ok(search::Searcher::new(ix, default_ranker()?))
}

/// Open and bring the index up to date: `Repo::update` with `force`, else `Repo::refresh`, which
/// answers from the index as it is when it cannot be updated.
pub fn open_updated(root: &Path, force: bool) -> Result<(search::Searcher, index::UpdateStats)> {
    let mut repo = Repo::open(root)?;
    let st = if force { repo.update(true)? } else { repo.refresh(0.0)?.unwrap_or_default() };
    Ok((repo.into_searcher(), st))
}

/// An open repository: the searcher on its index, and the one place that knows how to bring the
/// index up to date and what to do when its database is damaged. It derefs to the searcher.
pub struct Repo {
    /// `None` only while a damaged index is being replaced
    searcher: Option<search::Searcher>,
    /// the last refresh failed and stderr said why
    stale: bool,
}

impl Repo {
    /// Open (or create) the index of `root`, without updating it.
    pub fn open(root: &Path) -> Result<Repo> {
        Ok(Repo { searcher: Some(open(root)?), stale: false })
    }

    pub fn into_searcher(self) -> search::Searcher {
        self.searcher.expect("the index was not reopened after a rebuild")
    }

    /// Bring the index up to date. A corrupt database is deleted and built again: the index is a
    /// cache, the sources are the truth.
    pub fn update(&mut self, force: bool) -> Result<index::UpdateStats> {
        match self.ix.update(force) {
            Err(e) if index::error_is_corruption(&e) => self.rebuild(&e),
            other => other,
        }
    }

    /// `update` before a read, unless the index was checked less than `min_interval` seconds
    /// ago. When it cannot run (another process holds the write lock, the index directory is
    /// read-only) the read answers from the last committed index, and stderr says why once. A
    /// lock held while the committed index has no file is a first build: `index::BeingBuilt`.
    pub fn refresh(&mut self, min_interval: f64) -> Result<Option<index::UpdateStats>> {
        match self.ix.refresh_or_fail(min_interval) {
            Err(e) if index::error_is_corruption(&e) => self.rebuild(&e).map(Some),
            Err(e) if e.is::<index::Busy>() && !self.ix.has_files()? => Err(index::BeingBuilt.into()),
            Err(e) => {
                if !self.stale {
                    eprintln!("[scrutate] index not updated ({e:#}); answering from it as it is");
                    self.stale = true;
                }
                Ok(None)
            }
            Ok(st) => {
                self.stale &= st.is_none();
                Ok(st)
            }
        }
    }

    /// Run one operation on an index refreshed as `refresh` does (operations that do not read
    /// the index skip that).
    pub fn run(&mut self, op: &ops::Op, min_interval: f64) -> Result<ops::Answer> {
        if let ops::Op::Reindex { force } = op {
            return Ok(ops::Answer::Reindexed(self.update(*force)?));
        }
        if !matches!(op, ops::Op::Read { symbol: None, .. }) {
            self.refresh(min_interval)?;
        }
        self.run_as_is(op)
    }

    /// Run one operation on the index as it is, without refreshing it; a damaged database is
    /// still rebuilt and the operation run again.
    pub fn run_as_is(&mut self, op: &ops::Op) -> Result<ops::Answer> {
        match ops::run(self, op) {
            Err(e) if index::error_is_corruption(&e) => {
                self.rebuild(&e)?;
                ops::run(self, op)
            }
            other => other,
        }
    }

    fn rebuild(&mut self, why: &anyhow::Error) -> Result<index::UpdateStats> {
        eprintln!("[scrutate] index database corrupt ({why:#}); rebuilding");
        let root = self.ix.root.clone();
        let progress = self.ix.progress;
        self.searcher = None; // the database is closed before its files are deleted
        index::wipe_index(&root);
        let mut s = open(&root)?;
        s.ix.progress = progress;
        let st = s.ix.update(true);
        self.searcher = Some(s);
        st
    }
}

impl std::ops::Deref for Repo {
    type Target = search::Searcher;
    fn deref(&self) -> &search::Searcher {
        self.searcher.as_ref().expect("the index was not reopened after a rebuild")
    }
}

impl std::ops::DerefMut for Repo {
    fn deref_mut(&mut self) -> &mut search::Searcher {
        self.searcher.as_mut().expect("the index was not reopened after a rebuild")
    }
}
