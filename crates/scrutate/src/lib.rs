//! Scrutate engine: incremental code index (BM25 postings + semantic vectors + call graph), a learned
//! ranker, and the search layer used by the CLI and the MCP server.

// Index-driven loops over parallel arrays (postings, feature matrices) read better with explicit
// indices than with zipped iterators; this lint is noise here.
#![allow(clippy::needless_range_loop)]

pub mod cli;
pub mod config;
pub mod fmt;
pub mod index;
pub mod instant;
pub mod intel;
pub mod langs;
pub mod lex;
pub mod mcp;
pub mod npy;
pub mod parse;
pub mod rank;
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

/// Open and bring the index up to date; if the database turns out to be corrupt while doing so,
/// delete it and build a fresh one (the index is a cache; the sources are the truth).
pub fn open_updated(root: &Path, force: bool) -> Result<(search::Searcher, index::UpdateStats)> {
    let mut s = open(root)?;
    match s.ix.update(force) {
        Ok(st) => Ok((s, st)),
        Err(e) if index::error_is_corruption(&e) => {
            eprintln!("[scrutate] index database corrupt ({e:#}); rebuilding");
            drop(s);
            index::wipe_index(root);
            let mut s = open(root)?;
            let st = s.ix.update(true)?;
            Ok((s, st))
        }
        Err(e) => Err(e),
    }
}
