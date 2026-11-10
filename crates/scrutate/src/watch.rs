//! `scrutate watch` and `serve --watch`: keep the index fresh from filesystem events. Events are
//! filtered with the indexes' own ignore rules, bursts are debounced, and each burst runs the usual
//! incremental update (which takes the indexes' write locks, so a crash mid-update is harmless).

use std::path::Path;
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
        // a lost or overflowing event queue: rescan to be safe
        Err(_) => true,
        Ok(e) if e.need_rescan() => true,
        // reads (the update's own included) are not changes
        Ok(Event { kind: EventKind::Access(_), .. }) => false,
        Ok(e) => e.paths.iter().any(|p| files.may_contain(p)),
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
    let mut files = Membership::new(&root);
    on_ready();
    while !stop.load(Ordering::Relaxed) {
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
