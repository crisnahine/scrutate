//! `scrutate watch` and `serve --watch`: keep the index fresh from filesystem events. Events are
//! filtered with the indexes' own ignore rules, bursts are debounced, and each burst runs the usual
//! incremental update (which takes the indexes' write locks, so a crash mid-update is harmless).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_os = "macos"))]
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use anyhow::{Context, Result};
use ignore::gitignore::Gitignore;
use notify::{Event, EventKind};
#[cfg(not(target_os = "macos"))]
use notify::{RecursiveMode, Watcher};

/// A burst of events ends after this much quiet.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
/// A burst that never goes quiet is still flushed this often.
#[cfg(not(target_os = "macos"))]
const MAX_WAIT: Duration = Duration::from_secs(2);

/// Decides whether a changed path can change what one of the indexes holds: Engine B indexes the
/// `index::list_files` set (hidden files included, `SKIP_DIRS` pruned), the text index the files of
/// Engine A's default walk (hidden files skipped). Both honour the same ignore files.
#[cfg_attr(target_os = "macos", allow(dead_code))] // the macOS watcher polls the index instead
pub struct Filter {
    root: PathBuf,
    skip: Vec<String>,
    /// top of the git repository holding `root`; `.gitignore` rules only apply inside one
    git_top: Option<PathBuf>,
    exclude: Gitignore,
    global: Gitignore,
    dirs: HashMap<PathBuf, Vec<Gitignore>>,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Filter {
    pub fn new(root: &Path) -> Filter {
        let mut skip: Vec<String> = crate::index::SKIP_DIRS.iter().map(|s| s.to_string()).collect();
        if let Ok(v) = std::env::var("SCRUTATE_INDEX_EXCLUDE_DIRS") {
            skip.extend(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
        }
        let git_top = root.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf);
        let (exclude, global) = match &git_top {
            Some(top) => (Gitignore::new(top.join(".git/info/exclude")).0, Gitignore::global().0),
            None => (Gitignore::empty(), Gitignore::empty()),
        };
        Filter { root: root.to_path_buf(), skip, git_top, exclude, global, dirs: HashMap::new() }
    }

    fn matchers(&mut self, dir: &Path) -> &[Gitignore] {
        let git = self.git_top.is_some();
        self.dirs.entry(dir.to_path_buf()).or_insert_with(|| {
            let mut v = Vec::new();
            let names: &[&str] = if git { &[".ignore", ".gitignore"] } else { &[".ignore"] };
            for name in names {
                let p = dir.join(name);
                if p.is_file() {
                    v.push(Gitignore::new(&p).0);
                }
            }
            v
        })
    }

    fn ignored(&mut self, path: &Path) -> bool {
        let is_dir = path.is_dir();
        let top = self.git_top.clone().unwrap_or_else(|| self.root.clone());
        let mut dir = path.parent();
        while let Some(d) = dir {
            for m in self.matchers(d) {
                let r = m.matched_path_or_any_parents(path, is_dir);
                if !r.is_none() {
                    return r.is_ignore();
                }
            }
            if d == top {
                break;
            }
            dir = d.parent();
        }
        for m in [&self.exclude, &self.global] {
            if path.starts_with(&top) {
                let r = m.matched_path_or_any_parents(path, is_dir);
                if !r.is_none() {
                    return r.is_ignore();
                }
            }
        }
        false
    }

    /// Whether a change of `path` can matter to the code index or (when there is one) the text index.
    pub fn wants(&mut self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else { return false };
        let names: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        if names.first().map(|n| n == ".scrutate" || n == ".git").unwrap_or(false) {
            return false;
        }
        if names.last().map(|n| n == ".gitignore" || n == ".ignore").unwrap_or(false) {
            self.dirs.clear(); // the rules changed: what is indexed may change too
            return true;
        }
        let code = !names.iter().any(|n| self.skip.contains(n));
        let text = !names.iter().any(|n| n.starts_with('.')) && crate::textindex::dir_of(&self.root).join("manifest.json").exists();
        (code || text) && !self.ignored(path)
    }

    fn wants_event(&mut self, ev: &notify::Result<Event>) -> bool {
        match ev {
            // a lost or overflowing event queue: rescan to be safe
            Err(_) => true,
            Ok(e) if e.need_rescan() => true,
            // reads (the update's own included) are not changes
            Ok(Event { kind: EventKind::Access(_), .. }) => false,
            Ok(e) => e.paths.iter().any(|p| self.wants(p)),
        }
    }
}

/// Watch `root` until `stop` is set, calling `on_change` once per debounced burst of relevant
/// events. `on_ready` runs once the watch is in place. Errors from `on_change` are reported and the
/// watch goes on.
#[cfg(not(target_os = "macos"))]
pub fn run(root: &Path, stop: &AtomicBool, on_ready: impl FnOnce(), mut on_change: impl FnMut() -> Result<()>) -> Result<()> {
    let root = std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).context("start the file watcher")?;
    watcher.watch(&root, RecursiveMode::Recursive).with_context(|| format!("watch {}", root.display()))?;
    let mut filter = Filter::new(&root);
    on_ready();
    while !stop.load(Ordering::Relaxed) {
        let ev = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => bail!("the file watcher stopped"),
        };
        if !filter.wants_event(&ev) {
            continue;
        }
        let t0 = Instant::now();
        while t0.elapsed() < MAX_WAIT {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(ev) => {
                    filter.wants_event(&ev); // keeps the ignore rules current
                }
                Err(_) => break,
            }
        }
        if let Err(e) = on_change() {
            eprintln!("[scrutate] update failed: {e:#}");
        }
    }
    Ok(())
}

/// macOS: the FSEvents backend would link CoreServices into every scrutate command (about 1 ms of
/// start-up and 4 MB), and a poller that compares whole-second modification times misses quick
/// edits. So the index's own incremental check runs on a timer: it compares sizes and nanosecond
/// times, and the pause grows with what a check costs (about a tenth of the time at most).
#[cfg(target_os = "macos")]
pub fn run(root: &Path, stop: &AtomicBool, on_ready: impl FnOnce(), mut on_change: impl FnMut() -> Result<()>) -> Result<()> {
    std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    on_ready();
    let mut pause = Duration::from_secs(1);
    while !stop.load(Ordering::Relaxed) {
        let t0 = Instant::now();
        while t0.elapsed() < pause && !stop.load(Ordering::Relaxed) {
            std::thread::sleep(Duration::from_millis(100));
        }
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let t1 = Instant::now();
        if let Err(e) = on_change() {
            eprintln!("[scrutate] update failed: {e:#}");
        }
        pause = (t1.elapsed() * 10).clamp(Duration::from_secs(1), Duration::from_secs(30));
    }
    Ok(())
}

/// Bring the text index up to date when the repository has one.
pub fn update_text(root: &Path) -> Result<Option<crate::textindex::TextStats>> {
    if !crate::textindex::dir_of(root).join("manifest.json").exists() {
        return Ok(None);
    }
    crate::textindex::update(root, false, false).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_follows_the_index_rules() {
        let root = std::env::temp_dir().join(format!("scrutate_watch_filter_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".ignore"), "gen/\n*.log\n").unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let mut f = Filter::new(&root);
        assert!(f.wants(&root.join("src/a.rs")));
        assert!(f.wants(&root.join(".github/ci.yml")), "hidden files are indexed by Engine B");
        assert!(!f.wants(&root.join(".scrutate/index.db")));
        assert!(!f.wants(&root.join("node_modules/x/index.js")), "SKIP_DIRS without a text index");
        assert!(!f.wants(&root.join("gen/out.rs")));
        assert!(!f.wants(&root.join("src/run.log")));
        assert!(!f.wants(Path::new("/elsewhere/a.rs")));
        std::fs::write(root.join(".ignore"), "").unwrap();
        assert!(f.wants(&root.join(".ignore")));
        assert!(f.wants(&root.join("gen/out.rs")), "a changed ignore file is read again");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
