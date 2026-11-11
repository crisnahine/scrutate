//! `scrutate watch` and `serve --watch`: keep the index fresh from filesystem events. Events are
//! filtered with the indexes' own ignore rules, bursts are debounced, and each burst runs an
//! incremental update (which takes the indexes' write locks, so a crash mid-update is harmless).
//! A burst that names files only updates those files; one that touches a directory, a rule file
//! or loses events walks the tree.

use std::cell::RefCell;
#[cfg_attr(target_os = "macos", allow(unused_imports))]
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_os = "macos"))]
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use anyhow::{Context, Result};
#[cfg_attr(target_os = "macos", allow(unused_imports))]
use notify::event::{CreateKind, ModifyKind, RemoveKind};
#[cfg_attr(target_os = "macos", allow(unused_imports))]
use notify::{Event, EventKind};
#[cfg(not(target_os = "macos"))]
use notify::{RecursiveMode, Watcher};

#[cfg_attr(target_os = "macos", allow(unused_imports))]
use crate::files::Membership;

/// A burst of events ends after this much quiet.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
/// A burst that never goes quiet is still flushed this often.
#[cfg(not(target_os = "macos"))]
const MAX_WAIT: Duration = Duration::from_secs(2);

/// The files one burst named, relative to the root, as each index's walk would list them; a file
/// that is gone is in both.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BurstFiles {
    pub code: Vec<String>,
    pub text: Vec<String>,
}

thread_local! {
    static BURST: RefCell<Option<BurstFiles>> = const { RefCell::new(None) };
}

/// While `on_change` runs for a burst that named files only: those files. The indexes' updates
/// then look at them alone instead of walking the tree.
pub fn burst_files() -> Option<BurstFiles> {
    BURST.with_borrow(|b| b.clone())
}

/// Run `f` with `burst_files()` answering `files` on this thread.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) fn with_burst<T>(files: Option<BurstFiles>, f: impl FnOnce() -> T) -> T {
    struct Reset;
    impl Drop for Reset {
        fn drop(&mut self) {
            BURST.set(None);
        }
    }
    BURST.set(files);
    let _reset = Reset;
    f()
}

/// What the events of one burst ask for.
#[derive(Default)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
struct Burst {
    /// a directory, a rule file or lost events: walk the tree and update everything
    full: bool,
    /// changed paths that are not directories
    paths: BTreeSet<PathBuf>,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Burst {
    /// Take one event in; -> whether it can change what an index holds. `dirs`: the directories
    /// the last walk found.
    fn note(&mut self, files: &mut Membership, dirs: &HashSet<PathBuf>, ev: &notify::Result<Event>) -> bool {
        let e = match ev {
            // the watch limit or a lost queue: the walk and the full update recover
            Err(e) => {
                eprintln!("[scrutate] file watcher: {e}");
                self.full = true;
                return true;
            }
            // inotify's queue overflowed (IN_Q_OVERFLOW): events were lost
            Ok(e) if e.need_rescan() => {
                self.full = true;
                return true;
            }
            // reads (the update's own included) are not changes
            Ok(Event { kind: EventKind::Access(_), .. }) => return false,
            Ok(e) => e,
        };
        let mut wanted = false;
        for p in &e.paths {
            if files.sets_rules(p) {
                self.full = true;
                wanted = true;
            } else if files.may_contain(p) {
                // a removed or renamed-away directory is only known from the last walk
                let dir = matches!(e.kind, EventKind::Create(CreateKind::Folder) | EventKind::Remove(RemoveKind::Folder))
                    || dirs.contains(p)
                    || std::fs::symlink_metadata(p).is_ok_and(|m| m.is_dir());
                if !dir {
                    self.paths.insert(p.clone());
                    wanted = true;
                } else if !matches!(e.kind, EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any)) {
                    self.full = true;
                    wanted = true;
                }
                // else a directory's own write time (Windows reports one per entry added or
                // removed): the entry has an event of its own
            }
        }
        wanted
    }

    /// The burst's files for each index; a new file goes to an index whose walk would list it.
    fn files(&self, root: &Path, files: &mut Membership) -> BurstFiles {
        let mut out = BurstFiles::default();
        for p in &self.paths {
            let Ok(rel) = p.strip_prefix(root) else { continue };
            let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            let gone = !std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
            if gone || files.code_lists(p) {
                out.code.push(rel.clone());
            }
            if gone || files.text_lists(p) {
                out.text.push(rel);
            }
        }
        out
    }
}

fn root_present(root: &Path) -> Result<()> {
    if !root.is_dir() {
        anyhow::bail!("{} is gone: the watched directory was removed or moved", root.display());
    }
    Ok(())
}

/// The directories the indexes can hold files in (ignored trees such as node_modules and target
/// are not entered), and `.git/info` for its `exclude`.
#[cfg(not(target_os = "macos"))]
fn index_dirs(root: &Path, files: &mut Membership) -> HashSet<PathBuf> {
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
    now.extend(files.git_info());
    now
}

/// Linux: one non-recursive inotify watch per directory of `index_dirs`, so ignored trees cost no
/// watches. Runs when a burst walks the tree, so new directories get a watch and ones that left
/// lose theirs. Only an error on `root` itself is returned.
#[cfg(target_os = "linux")]
fn sync_watches(watcher: &mut impl Watcher, root: &Path, files: &mut Membership, dirs: &mut HashSet<PathBuf>) -> Result<()> {
    let now = index_dirs(root, files);
    // removals first: a directory moved inside the tree has one inotify watch under both names
    for gone in dirs.difference(&now) {
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
    *dirs = now;
    Ok(())
}

/// Windows: one recursive watch on the root (`ReadDirectoryChangesW`), plus `.git/info` when the
/// root is below the repository top. The walk only records the directories.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
fn sync_watches(watcher: &mut impl Watcher, root: &Path, files: &mut Membership, dirs: &mut HashSet<PathBuf>) -> Result<()> {
    let now = index_dirs(root, files);
    if dirs.is_empty() {
        watcher.watch(root, RecursiveMode::Recursive).with_context(|| format!("watch {}", root.display()))?;
    }
    for d in now.iter().filter(|d| !d.starts_with(root) && !dirs.contains(*d)) {
        let _ = watcher.watch(d, RecursiveMode::NonRecursive);
    }
    *dirs = now;
    Ok(())
}

/// `SCRUTATE_WATCH_DEBUG`: one line on stderr per burst, saying what it costs.
#[cfg(not(target_os = "macos"))]
fn debug() -> bool {
    std::env::var_os("SCRUTATE_WATCH_DEBUG").is_some_and(|v| !v.is_empty())
}

/// Watch `root` until `stop` is set. Once the watch is in place `on_change` runs, then `on_ready`;
/// after that `on_change` runs once per debounced burst of relevant events, with `burst_files()`
/// naming the files when the burst named files only. An error from the first `on_change`, or the
/// root going away, ends the watch; later update errors are reported.
#[cfg(not(target_os = "macos"))]
pub fn run(root: &Path, stop: &AtomicBool, on_ready: impl FnOnce(), mut on_change: impl FnMut() -> Result<()>) -> Result<()> {
    let root = std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).context("start the file watcher")?;
    let mut files = Membership::new(&root);
    let mut dirs = HashSet::new();
    sync_watches(&mut watcher, &root, &mut files, &mut dirs)?;
    on_change()?;
    on_ready();
    let debug = debug();
    while !stop.load(Ordering::Relaxed) {
        root_present(&root)?;
        let ev = match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(ev) => ev,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => bail!("the file watcher stopped"),
        };
        let mut burst = Burst::default();
        if !burst.note(&mut files, &dirs, &ev) {
            continue;
        }
        let t0 = Instant::now();
        while t0.elapsed() < MAX_WAIT {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(ev) => {
                    burst.note(&mut files, &dirs, &ev);
                }
                Err(_) => break,
            }
        }
        root_present(&root)?;
        let named = if burst.full {
            // the cache keeps the ignore files of directories that were moved away or replaced
            files = Membership::new(&root);
            sync_watches(&mut watcher, &root, &mut files, &mut dirs)?;
            None
        } else {
            Some(burst.files(&root, &mut files))
        };
        if debug {
            eprintln!("[scrutate] burst: {} paths, full walk: {}", burst.paths.len(), if burst.full { "yes" } else { "no" });
        }
        if let Err(e) = with_burst(named, &mut on_change) {
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

/// Bring the text index up to date when the repository has one: the files of the burst being
/// handled, else every file.
pub fn update_text(root: &Path) -> Result<Option<crate::textindex::TextStats>> {
    if !crate::textindex::dir_of(root).join("manifest.json").exists() {
        return Ok(None);
    }
    match burst_files() {
        Some(b) => crate::textindex::update_paths(root, &b.text),
        None => crate::textindex::update(root, false, false),
    }
    .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{DataChange, RenameMode};

    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_burst_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        std::fs::create_dir_all(root.join("src/deep")).unwrap();
        std::fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::write(root.join("src/a.py"), "x\n").unwrap();
        std::fs::canonicalize(&root).unwrap()
    }

    fn ev(kind: EventKind, paths: &[&Path]) -> notify::Result<Event> {
        let mut e = Event::new(kind);
        for p in paths {
            e = e.add_path(p.to_path_buf());
        }
        Ok(e)
    }

    /// (full, the burst's paths relative to the root) after these events.
    fn burst(root: &Path, events: &[notify::Result<Event>]) -> (bool, Vec<String>) {
        let mut files = Membership::new(root);
        let dirs: HashSet<PathBuf> = [root.to_path_buf(), root.join("src"), root.join("src/deep"), root.join("gone_dir")].into();
        let mut b = Burst::default();
        for e in events {
            b.note(&mut files, &dirs, e);
        }
        (b.full, b.paths.iter().map(|p| p.strip_prefix(root).unwrap().to_string_lossy().replace('\\', "/")).collect())
    }

    #[test]
    fn a_burst_of_file_events_names_its_files() {
        let root = tree("files");
        let modify = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        let a = root.join("src/a.py");
        assert_eq!(burst(&root, &[ev(modify, &[&a])]), (false, vec!["src/a.py".to_string()]));
        // Windows names no kind of entry: the path is looked at
        let created = root.join("src/deep/b.py");
        std::fs::write(&created, "y\n").unwrap();
        assert_eq!(burst(&root, &[ev(EventKind::Create(CreateKind::Any), &[&created])]), (false, vec!["src/deep/b.py".to_string()]));
        let tmp = root.join("src/.a.py.swp");
        let rename = EventKind::Modify(ModifyKind::Name(RenameMode::Both));
        assert_eq!(burst(&root, &[ev(rename, &[&tmp, &a])]), (false, vec!["src/.a.py.swp".to_string(), "src/a.py".to_string()]));
        let removed = root.join("src/old.py");
        assert_eq!(burst(&root, &[ev(EventKind::Remove(RemoveKind::Any), &[&removed])]), (false, vec!["src/old.py".to_string()]));
        // ignored files and reads start no burst
        let mut files = Membership::new(&root);
        let mut b = Burst::default();
        assert!(!b.note(&mut files, &HashSet::new(), &ev(modify, &[&root.join("src/x.tmp")])));
        assert!(!b.note(&mut files, &HashSet::new(), &ev(EventKind::Access(notify::event::AccessKind::Any), &[&a])));
        assert!(!b.note(&mut files, &HashSet::new(), &ev(modify, &[&root.join(".scrutate/index.db")])));
        // Windows: the write time of the directory a file was added to
        assert!(!b.note(&mut files, &HashSet::new(), &ev(EventKind::Modify(ModifyKind::Any), &[&root.join("src")])));
        assert!(!b.full && b.paths.is_empty());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn directories_rules_and_lost_events_walk_the_tree() {
        let root = tree("full");
        let modify = EventKind::Modify(ModifyKind::Any);
        let a = root.join("src/a.py");
        let new_dir = root.join("src/new");
        std::fs::create_dir(&new_dir).unwrap();
        for (what, e) in [
            ("a new directory (inotify)", ev(EventKind::Create(CreateKind::Folder), &[&root.join("src/x")])),
            ("a new directory (Windows)", ev(EventKind::Create(CreateKind::Any), &[&new_dir])),
            ("a removed directory (inotify)", ev(EventKind::Remove(RemoveKind::Folder), &[&root.join("src/y")])),
            ("a removed directory the walk found", ev(EventKind::Remove(RemoveKind::Any), &[&root.join("gone_dir")])),
            ("a directory renamed away", ev(EventKind::Modify(ModifyKind::Name(RenameMode::From)), &[&root.join("src/deep")])),
            ("an ignore file", ev(modify, &[&root.join("src/.ignore")])),
            ("a .gitignore", ev(modify, &[&root.join(".gitignore")])),
            ("git's exclude", ev(modify, &[&root.join(".git/info/exclude")])),
            ("the configuration file", ev(modify, &[&root.join(".scrutate.toml")])),
            ("a lost queue", Ok(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan))),
            ("a watcher error", Err(notify::Error::generic("test"))),
        ] {
            assert!(burst(&root, &[ev(modify, &[&a]), e]).0, "{what}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn each_index_gets_the_files_its_walk_lists() {
        let root = tree("split");
        for name in ["src/a.tmp", "src/.hidden.py"] {
            std::fs::write(root.join(name), "x\n").unwrap();
        }
        let mut files = Membership::new(&root);
        let mut b = Burst::default();
        let modify = EventKind::Modify(ModifyKind::Any);
        for name in ["src/a.py", "src/.hidden.py", "src/gone.py", "src/a.tmp"] {
            b.note(&mut files, &HashSet::new(), &ev(modify, &[&root.join(name)]));
        }
        let f = b.files(&root, &mut files);
        assert_eq!(f.code, ["src/.hidden.py", "src/a.py", "src/gone.py"]);
        assert_eq!(f.text, ["src/a.py", "src/gone.py"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn burst_files_answer_only_inside_with_burst() {
        let f = BurstFiles { code: vec!["a.py".into()], text: vec![] };
        assert_eq!(with_burst(Some(f.clone()), burst_files), Some(f));
        assert_eq!(burst_files(), None);
        let caught = std::panic::catch_unwind(|| with_burst(Some(BurstFiles::default()), || panic!("update")));
        assert!(caught.is_err());
        assert_eq!(burst_files(), None, "reset after a panic too");
    }
}
