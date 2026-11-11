//! `scrutate watch` and `serve --watch`: keep the index fresh from filesystem events. Events are
//! filtered with the indexes' own ignore rules, bursts are debounced, and each burst runs the usual
//! incremental update (which takes the indexes' write locks, so a crash mid-update is harmless).

#[cfg(not(target_os = "macos"))]
use std::collections::HashSet;
use std::path::Path;
#[cfg(not(target_os = "macos"))]
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_os = "macos"))]
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use anyhow::{Context, Result};
#[cfg(not(target_os = "macos"))]
use notify::{Event, EventKind, RecursiveMode, Watcher};

#[cfg(not(target_os = "macos"))]
use crate::files::Membership;

/// A burst of events ends after this much quiet.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
/// A burst that never goes quiet is still flushed this often.
#[cfg(not(target_os = "macos"))]
const MAX_WAIT: Duration = Duration::from_secs(2);

#[cfg(not(target_os = "macos"))]
fn wants_event(files: &mut Membership, ev: &notify::Result<Event>) -> bool {
    match ev {
        // the watch limit or a lost queue: the burst's re-watch and update recover
        Err(e) => {
            eprintln!("[scrutate] file watcher: {e}");
            true
        }
        Ok(e) if e.need_rescan() => true,
        // reads (the update's own included) are not changes
        Ok(Event { kind: EventKind::Access(_), .. }) => false,
        Ok(e) => e.paths.iter().any(|p| files.may_contain(p)),
    }
}

fn root_present(root: &Path) -> Result<()> {
    if !root.is_dir() {
        anyhow::bail!("{} is gone: the watched directory was removed or moved", root.display());
    }
    Ok(())
}

/// Linux: one non-recursive inotify watch per directory the indexes can hold, so ignored trees
/// (node_modules, target) cost no watches. Runs after every burst, so new directories get a watch
/// and ones that left lose theirs. Only an error on `root` itself is returned.
#[cfg(target_os = "linux")]
fn sync_watches(watcher: &mut impl Watcher, root: &Path, files: &mut Membership, watched: &mut HashSet<PathBuf>) -> Result<()> {
    let mut now = HashSet::new();
    let mut todo = vec![root.to_path_buf()];
    while let Some(d) = todo.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                if e.file_type().map(|t| t.is_dir()).unwrap_or(false) && files.may_contain(&e.path()) {
                    todo.push(e.path());
                }
            }
        }
        now.insert(d);
    }
    // removals first: a directory moved inside the tree has one inotify watch under both names
    for gone in watched.difference(&now) {
        let _ = watcher.unwatch(gone);
    }
    // notify drops the watches of moved and deleted directories itself, and a lost queue or the
    // limit may have dropped others, so every directory is watched again (inotify merges repeats)
    let (mut failed, mut first) = (0, None);
    for d in &now {
        if let Err(e) = watcher.watch(d, RecursiveMode::NonRecursive) {
            if d == root {
                return Err(e).with_context(|| format!("watch {}", root.display()));
            }
            failed += 1;
            first.get_or_insert(e);
        }
    }
    if let Some(e) = first {
        eprintln!("[scrutate] could not watch {failed} of {} directories ({e}); changes there are seen with the next other change", now.len());
    }
    *watched = now;
    Ok(())
}

#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
fn sync_watches(watcher: &mut impl Watcher, root: &Path, _: &mut Membership, watched: &mut HashSet<PathBuf>) -> Result<()> {
    if watched.insert(root.to_path_buf()) {
        watcher.watch(root, RecursiveMode::Recursive).with_context(|| format!("watch {}", root.display()))?;
    }
    Ok(())
}

/// Watch `root` until `stop` is set. Once the watch is in place `on_change` runs, then `on_ready`;
/// after that `on_change` runs once per debounced burst of relevant events. An error from the first
/// `on_change`, or the root going away, ends the watch; later update errors are reported.
#[cfg(not(target_os = "macos"))]
pub fn run(root: &Path, stop: &AtomicBool, on_ready: impl FnOnce(), mut on_change: impl FnMut() -> Result<()>) -> Result<()> {
    let root = std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).context("start the file watcher")?;
    let mut files = Membership::new(&root);
    let mut watched = HashSet::new();
    sync_watches(&mut watcher, &root, &mut files, &mut watched)?;
    on_change()?;
    on_ready();
    while !stop.load(Ordering::Relaxed) {
        root_present(&root)?;
        let ev = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => bail!("the file watcher stopped"),
        };
        if !wants_event(&mut files, &ev) {
            continue;
        }
        let t0 = Instant::now();
        while t0.elapsed() < MAX_WAIT {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(ev) => {
                    wants_event(&mut files, &ev); // keeps the ignore rules current
                }
                Err(_) => break,
            }
        }
        root_present(&root)?;
        // the cache keeps the ignore files of directories that were moved away or replaced
        files = Membership::new(&root);
        sync_watches(&mut watcher, &root, &mut files, &mut watched)?;
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
    let root = std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    on_change()?;
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
        root_present(&root)?;
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
