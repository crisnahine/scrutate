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
pub mod running;
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
    /// `None` while a damaged index is being replaced, and after a rebuild that could not reopen it
    searcher: Option<search::Searcher>,
    root: std::path::PathBuf,
    /// the last refresh failed and stderr said why
    stale: bool,
}

impl Repo {
    /// Open (or create) the index of `root`, without updating it.
    pub fn open(root: &Path) -> Result<Repo> {
        let searcher = open(root)?;
        Ok(Repo { root: searcher.ix.root.clone(), searcher: Some(searcher), stale: false })
    }

    pub fn into_searcher(self) -> search::Searcher {
        self.searcher.expect("the index was not reopened after a rebuild")
    }

    /// Whether the last refresh could not update the index, so answers come from it as it was.
    pub fn is_stale(&self) -> bool {
        self.stale
    }

    /// The cancel flag of the next operation; nothing to set while the index is closed.
    pub fn set_cancel(&mut self, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) {
        if let Some(s) = &mut self.searcher {
            // an update the operation runs (a reindex, the scan before a read) stops at its next batch
            s.ix.cancel = std::sync::Arc::clone(&cancel);
            s.cancel = cancel;
        }
    }

    /// An index a failed rebuild left closed is opened and built again before anything reads it.
    fn reopened(&mut self) -> Result<()> {
        if self.searcher.is_none() {
            self.reopen(false)?;
        }
        Ok(())
    }

    /// Bring the index up to date. A corrupt database is deleted and built again: the index is a
    /// cache, the sources are the truth.
    pub fn update(&mut self, force: bool) -> Result<index::UpdateStats> {
        self.reopened()?;
        match self.ix.update(force) {
            Err(e) if index::error_is_corruption(&e) => self.rebuild(&e),
            other => other,
        }
    }

    /// `update` that stops between batches of files once `cancel` is set, and then leaves the
    /// index as it was.
    // shortcut: a rebuild after damage does not see the flag, give `reopen` the flag if that matters
    pub fn update_until(&mut self, force: bool, cancel: std::sync::Arc<std::sync::atomic::AtomicBool>) -> Result<index::UpdateStats> {
        self.reopened()?;
        self.ix.cancel = cancel;
        let r = self.update(force);
        if let Some(s) = &mut self.searcher {
            s.ix.cancel = Default::default();
        }
        r
    }

    /// `update` before a read, unless the index was checked less than `min_interval` seconds
    /// ago. When it cannot run (another process holds the write lock, the index directory is
    /// read-only) the read answers from the last committed index, and stderr says why once. A
    /// lock held while the committed index has no file is a first build: `index::BeingBuilt`.
    pub fn refresh(&mut self, min_interval: f64) -> Result<Option<index::UpdateStats>> {
        self.reopened()?;
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

    /// Run one operation on the index without a scan of the tree. The files the answer cites
    /// (and the ones its arguments name) are checked against the index, one `stat` each: when
    /// one changed or is gone, or a named one is new and a scan would index it, those files
    /// alone are updated and the answer computed again, once. A file no answer cites or names
    /// waits for the next scan. A damaged database is rebuilt and the operation run again.
    pub fn run_as_is(&mut self, op: &ops::Op) -> Result<ops::Answer> {
        let first = self.run_once(op);
        if self.searcher.is_none() {
            return first;
        }
        let named = op.named_paths();
        let mut paths = named.clone();
        if let Ok(a) = &first {
            paths.extend(a.cited().into_iter().map(str::to_string));
        }
        let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
        let stale = match self.ix.stale(&paths) {
            Ok(mut s) => {
                s.extend(self.unindexed(&named));
                s
            }
            Err(_) => return first,
        };
        if stale.is_empty() {
            return first;
        }
        match self.ix.update_paths(&stale) {
            Ok(_) => self.run_once(op),
            Err(e) if index::error_is_corruption(&e) => {
                self.rebuild(&e)?;
                self.run_once(op)
            }
            // another writer holds the lock, or the index cannot be written: answer as it is
            Err(_) => first,
        }
    }

    /// Of `paths` (from the root), the files the index has no row for that a scan would index:
    /// one lookup and one `stat` each, and the ignore rules read for those alone.
    fn unindexed(&self, paths: &[String]) -> Vec<String> {
        let mut membership: Option<files::Membership> = None;
        let skipped = files::skipped_dirs(&self.root);
        let mut out = Vec::new();
        for p in paths {
            let plain = !p.is_empty() && Path::new(p).components().all(|c| matches!(c, std::path::Component::Normal(_)));
            let known = || self.ix.db.query_row("SELECT 1 FROM files WHERE path=?1", [p], |_| Ok(())).is_ok();
            if !plain || !parse::is_indexable(p) || p.split('/').any(|n| skipped.iter().any(|s| s == n)) || known() {
                continue;
            }
            // the walk does not follow symbolic links
            let full = self.root.join(p);
            if !std::fs::symlink_metadata(&full).is_ok_and(|m| m.is_file() && m.len() <= index::MAX_FILE_BYTES) {
                continue;
            }
            if membership.get_or_insert_with(|| files::Membership::new(&self.root)).may_contain(&full) {
                out.push(p.clone());
            }
        }
        out
    }

    fn run_once(&mut self, op: &ops::Op) -> Result<ops::Answer> {
        self.reopened()?;
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
        let progress = self.searcher.as_ref().is_some_and(|s| s.ix.progress);
        self.searcher = None; // the database is closed before its files are deleted
        index::wipe_index(&self.root);
        self.reopen(progress)
    }

    fn reopen(&mut self, progress: bool) -> Result<index::UpdateStats> {
        let mut s = open(&self.root)?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rebuild_that_could_not_reopen_the_index_is_an_error_until_it_can() {
        let root = std::env::temp_dir().join(format!("scrutate_reopen_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let mut repo = Repo { searcher: None, root: root.clone(), stale: false };
        let op = ops::Op::Symbol { name: "f".into(), limit: 5 };
        repo.set_cancel(std::sync::Arc::default());
        assert!(repo.update(false).is_err());
        assert!(repo.refresh(0.0).is_err());
        assert!(repo.run(&op, 0.0).is_err());
        assert!(repo.run_as_is(&op).is_err());
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.py"), "def f():\n    return 1\n").unwrap();
        assert!(matches!(repo.run_as_is(&op), Ok(ops::Answer::Hits { ref hits, .. }) if hits.len() == 1));
        drop(repo);
        let _ = std::fs::remove_dir_all(&root);
    }
}
