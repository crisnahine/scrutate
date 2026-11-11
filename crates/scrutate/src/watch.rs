//! `scrutate watch` and `serve --watch`: keep the index fresh from filesystem events. Bursts of
//! events are debounced, and each burst runs an incremental update (which takes the indexes' write
//! locks, so a crash mid-update is harmless). The indexes' own walks decide what a burst holds: a
//! burst that names files lists their directories with those walks and updates the files listed.
//! One that touches a directory or a rule file, loses events, or comes while a directory has no
//! watch or the last walk is old walks the tree and updates everything.

use std::cell::RefCell;
#[cfg_attr(target_os = "macos", allow(unused_imports))]
use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(target_os = "macos"))]
use std::sync::mpsc::{self, RecvTimeoutError};
use std::time::{Duration, Instant};

#[cfg(not(target_os = "macos"))]
use anyhow::bail;
use anyhow::{Context, Result};
#[cfg_attr(target_os = "macos", allow(unused_imports))]
use notify::{event::ModifyKind, Event, EventKind};
#[cfg(not(target_os = "macos"))]
use notify::{RecursiveMode, Watcher};

#[cfg_attr(target_os = "macos", allow(unused_imports))]
use crate::files::{CodeWalk, INDEX_DIR};

/// A burst of events ends after this much quiet.
pub const DEBOUNCE: Duration = Duration::from_millis(200);
/// A burst that never goes quiet is still flushed this often.
#[cfg(not(target_os = "macos"))]
const MAX_WAIT: Duration = Duration::from_secs(2);
/// A burst walks the tree once the last walk is this old: no event reports a directory made while
/// its parent's watch was being set, or events the OS or notify dropped without saying so.
#[cfg_attr(target_os = "macos", allow(dead_code))]
const WALK_EVERY: Duration = Duration::from_secs(30);

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

fn has_text_index(root: &Path) -> bool {
    crate::textindex::dir_of(root).join("manifest.json").exists()
}

/// Under the root, and not the root itself, the index directory or `.git`.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn inside(root: &Path, p: &Path) -> bool {
    p.strip_prefix(root).ok().and_then(|r| r.components().next()).is_some_and(|c| c.as_os_str() != INDEX_DIR && c.as_os_str() != ".git")
}

/// git's `info/exclude` for the repository holding `root`. A linked worktree's `.git` is a file
/// naming its git directory, whose `commondir` names the main repository's, as the walk reads it.
#[cfg_attr(target_os = "macos", allow(dead_code))]
fn git_exclude(root: &Path) -> Option<PathBuf> {
    let top = root.ancestors().find(|a| a.join(".git").exists())?;
    let git = top.join(".git");
    let dir = if git.is_dir() {
        git
    } else {
        let text = std::fs::read_to_string(&git).ok()?;
        let gitdir = top.join(text.trim().strip_prefix("gitdir:")?.trim());
        match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(c) => gitdir.join(c.trim()),
            Err(_) => gitdir,
        }
    };
    Some(std::fs::canonicalize(dir).ok()?.join("info").join("exclude"))
}

/// What the last walk of the tree found.
#[cfg_attr(target_os = "macos", allow(dead_code))]
struct Walked {
    /// the directories each index's walk enters; none for the text index when there is none
    code: HashSet<PathBuf>,
    text: HashSet<PathBuf>,
    text_index: bool,
    exclude: Option<PathBuf>,
    at: Instant,
    /// directories that got no watch
    unwatched: usize,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Walked {
    fn new(root: &Path, code: &CodeWalk) -> Walked {
        let text_index = has_text_index(root);
        Walked {
            code: code.builder(root).build().flatten().filter(|e| e.file_type().is_some_and(|t| t.is_dir())).map(|e| e.into_path()).collect(),
            text: if text_index { crate::textindex::walk_entries(root, None).0.into_iter().collect() } else { HashSet::new() },
            text_index,
            exclude: git_exclude(root),
            at: Instant::now(),
            unwatched: 0,
        }
    }

    fn holds(&self, dir: &Path) -> bool {
        self.code.contains(dir) || self.text.contains(dir)
    }

    /// Every directory either walk enters, and the one holding git's exclude file.
    fn watched(&self) -> HashSet<PathBuf> {
        let mut all: HashSet<PathBuf> = self.code.union(&self.text).cloned().collect();
        all.extend(self.exclude.as_deref().and_then(Path::parent).filter(|p| p.is_dir()).map(Path::to_path_buf));
        all
    }

    /// Whether a burst walks the tree whatever it names.
    fn due(&self, root: &Path) -> bool {
        // a change in a directory without a watch sends no event
        self.unwatched > 0 || self.at.elapsed() >= WALK_EVERY || self.text_index != has_text_index(root)
    }

    /// Whether `path` holds rules for what is indexed: an ignore file, git's exclude file, or a
    /// `.scrutate.toml`.
    fn sets_rules(&self, root: &Path, path: &Path) -> bool {
        self.exclude.as_deref() == Some(path)
            || inside(root, path)
                && path.file_name().is_some_and(|n| [".gitignore", ".ignore", crate::instant::RGIGNORE, ".scrutate.toml"].iter().any(|r| n == *r))
    }
}

/// One directory as each index's walk lists it.
#[derive(Default)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
struct Listing {
    dirs: HashSet<PathBuf>,
    code: HashSet<PathBuf>,
    text: HashSet<PathBuf>,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Listing {
    fn of(dir: &Path, code: &CodeWalk, walked: &Walked) -> Listing {
        let mut l = Listing::default();
        if walked.code.contains(dir) {
            for e in code.builder(dir).max_depth(Some(1)).build().flatten().filter(|e| e.depth() == 1) {
                if e.file_type().is_some_and(|t| t.is_dir()) {
                    l.dirs.insert(e.into_path());
                } else if code.keeps(&e).is_some() {
                    l.code.insert(e.into_path());
                }
            }
        }
        if walked.text.contains(dir) {
            let (dirs, files) = crate::textindex::walk_entries(dir, Some(1));
            l.dirs.extend(dirs.into_iter().filter(|d| d != dir));
            l.text.extend(files);
        }
        l
    }
}

/// What the events of one burst ask for.
#[derive(Default)]
#[cfg_attr(target_os = "macos", allow(dead_code))]
struct Burst {
    /// a rule file or lost events: walk the tree and update everything
    full: bool,
    /// changed paths under the root
    paths: BTreeSet<PathBuf>,
    /// those of them created, removed or renamed
    moved: HashSet<PathBuf>,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Burst {
    /// Take one event in; -> whether it names anything that may matter.
    fn note(&mut self, root: &Path, walked: &Walked, ev: &notify::Result<Event>) -> bool {
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
        let moved = !matches!(e.kind, EventKind::Modify(ModifyKind::Data(_) | ModifyKind::Metadata(_) | ModifyKind::Any));
        let mut wanted = false;
        for p in &e.paths {
            if walked.sets_rules(root, p) {
                self.full = true;
                wanted = true;
            } else if inside(root, p) {
                if moved {
                    self.moved.insert(p.clone());
                }
                self.paths.insert(p.clone());
                wanted = true;
            }
        }
        wanted
    }

    /// The burst's files for each index, as the walks list their directories; None when a
    /// directory the walks enter came, went or was replaced, so the tree must be walked.
    fn files(&self, root: &Path, code: &CodeWalk, walked: &Walked) -> Option<BurstFiles> {
        let mut listed: HashMap<&Path, Listing> = HashMap::new();
        let mut out = BurstFiles::default();
        for p in &self.paths {
            let Some(parent) = p.parent() else { continue };
            let md = std::fs::symlink_metadata(p).ok();
            let is_dir = md.as_ref().is_some_and(|m| m.is_dir());
            if walked.holds(p) {
                // a directory's own write time (Windows reports one per entry added or removed):
                // the entry has an event of its own
                if is_dir && !self.moved.contains(p) {
                    continue;
                }
                return None;
            }
            if !walked.holds(parent) {
                continue;
            }
            let l = listed.entry(parent).or_insert_with(|| Listing::of(parent, code, walked));
            if is_dir {
                if l.dirs.contains(p) {
                    return None;
                }
                continue;
            }
            let Ok(rel) = p.strip_prefix(root) else { continue };
            let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
            let gone = !md.is_some_and(|m| m.is_file());
            if gone || l.code.contains(p) {
                out.code.push(rel.clone());
            }
            if gone || l.text.contains(p) {
                out.text.push(rel);
            }
        }
        Some(out)
    }
}

fn root_present(root: &Path) -> Result<()> {
    if !root.is_dir() {
        anyhow::bail!("{} is gone: the watched directory was removed or moved", root.display());
    }
    Ok(())
}

/// The order watches are set in: the root first (the watch ends without it), then shallow
/// directories before deep ones and by path, so at the watch limit a tree keeps the same watches.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn watch_order(root: &Path, dirs: &HashSet<PathBuf>) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = dirs.iter().filter(|d| *d != root).cloned().collect();
    v.sort_by_cached_key(|d| (d.components().count(), d.clone()));
    v.insert(0, root.to_path_buf());
    v
}

/// Linux: walk the tree and set one non-recursive inotify watch per directory the walks enter, so
/// ignored trees cost no watches; new directories get a watch and ones that left lose theirs. Only
/// an error on `root` itself is returned.
#[cfg(target_os = "linux")]
fn sync_watches(watcher: &mut impl Watcher, root: &Path, code: &CodeWalk, old: Option<&Walked>) -> Result<Walked> {
    let mut now = Walked::new(root, code);
    let dirs = now.watched();
    if let Some(old) = old {
        // removals first: a directory moved inside the tree has one inotify watch under both names
        for gone in old.watched().difference(&dirs) {
            let _ = watcher.unwatch(gone);
        }
    }
    // notify drops the watches of moved and deleted directories itself, and a lost queue or the
    // limit may have dropped others, so every directory is watched again (inotify merges repeats)
    let order = watch_order(root, &dirs);
    watcher.watch(root, RecursiveMode::NonRecursive).with_context(|| format!("watch {}", root.display()))?;
    let mut first = None;
    for d in &order[1..] {
        if let Err(e) = watcher.watch(d, RecursiveMode::NonRecursive) {
            now.unwatched += 1;
            first.get_or_insert(e);
        }
    }
    if let Some(e) = first.filter(|_| old.is_none_or(|o| o.unwatched != now.unwatched)) {
        eprintln!(
            "[scrutate] could not watch {} of {} directories ({e}); while any is unwatched, every change to an indexed file walks the tree, so a change in one of them is indexed with the next change elsewhere",
            now.unwatched,
            order.len()
        );
    }
    Ok(now)
}

/// Windows: one recursive watch on the root (`ReadDirectoryChangesW`), plus the directory of git's
/// exclude file when it is outside the root. The walk only records the directories.
#[cfg(all(not(target_os = "linux"), not(target_os = "macos")))]
fn sync_watches(watcher: &mut impl Watcher, root: &Path, code: &CodeWalk, old: Option<&Walked>) -> Result<Walked> {
    let now = Walked::new(root, code);
    if old.is_none() {
        watcher.watch(root, RecursiveMode::Recursive).with_context(|| format!("watch {}", root.display()))?;
    }
    let before = old.map(Walked::watched).unwrap_or_default();
    for d in now.watched().iter().filter(|d| !d.starts_with(root) && !before.contains(*d)) {
        let _ = watcher.watch(d, RecursiveMode::NonRecursive);
    }
    Ok(now)
}

/// `SCRUTATE_WATCH_DEBUG`: one line on stderr per burst, saying what it costs.
#[cfg(not(target_os = "macos"))]
fn debug() -> bool {
    std::env::var_os("SCRUTATE_WATCH_DEBUG").is_some_and(|v| !v.is_empty())
}

/// Watch `root` until `stop` is set. Once the watch is in place `on_change` runs, then `on_ready`;
/// after that `on_change` runs once per debounced burst that changes what an index holds, with
/// `burst_files()` naming the files when the burst does not walk the tree. An error from the first
/// `on_change`, or the root going away, ends the watch; later update errors are reported.
#[cfg(not(target_os = "macos"))]
pub fn run(root: &Path, stop: &AtomicBool, on_ready: impl FnOnce(), mut on_change: impl FnMut() -> Result<()>) -> Result<()> {
    let root = std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    let (tx, rx) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(tx).context("start the file watcher")?;
    let code = CodeWalk::new(&root);
    let mut walked = sync_watches(&mut watcher, &root, &code, None)?;
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
        if !burst.note(&root, &walked, &ev) {
            continue;
        }
        let t0 = Instant::now();
        while t0.elapsed() < MAX_WAIT {
            match rx.recv_timeout(DEBOUNCE) {
                Ok(ev) => {
                    burst.note(&root, &walked, &ev);
                }
                Err(_) => break,
            }
        }
        root_present(&root)?;
        let files = if burst.full { None } else { burst.files(&root, &code, &walked) };
        if files.as_ref().is_some_and(|f| f.code.is_empty() && f.text.is_empty()) {
            continue;
        }
        let named = files.filter(|_| !walked.due(&root));
        if named.is_none() {
            walked = sync_watches(&mut watcher, &root, &code, Some(&walked))?;
        }
        if debug {
            eprintln!("[scrutate] burst: {} paths, full walk: {}", burst.paths.len(), if named.is_none() { "yes" } else { "no" });
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
    if !has_text_index(root) {
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
    use notify::event::{CreateKind, DataChange, ModifyKind, RemoveKind, RenameMode};

    /// A repository with a text index (its manifest is enough for the walks).
    fn tree(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_burst_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for d in [".git/info", "src/deep", "gone_dir", ".scrutate/text"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        std::fs::write(root.join(".scrutate/text/manifest.json"), "{}").unwrap();
        std::fs::write(root.join(".gitignore"), "*.tmp\ngen/\n").unwrap();
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

    /// The walk of the tree as it is now, then these events; -> the burst's files, None when it
    /// walks the tree. `change` runs between the walk and the events.
    fn burst(root: &Path, change: impl FnOnce(), events: &[notify::Result<Event>]) -> Option<BurstFiles> {
        let code = CodeWalk::new(root);
        let walked = Walked::new(root, &code);
        change();
        let mut b = Burst::default();
        for e in events {
            b.note(root, &walked, e);
        }
        if b.full {
            return None;
        }
        b.files(root, &code, &walked)
    }

    fn both(paths: &[&str]) -> Option<BurstFiles> {
        let v: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
        Some(BurstFiles { code: v.clone(), text: v })
    }

    #[test]
    fn a_burst_of_file_events_names_its_files() {
        let root = tree("files");
        let modify = EventKind::Modify(ModifyKind::Data(DataChange::Any));
        let a = root.join("src/a.py");
        assert_eq!(burst(&root, || (), &[ev(modify, &[&a])]), both(&["src/a.py"]));
        // Windows names no kind of entry: the path is looked at
        let created = root.join("src/deep/b.py");
        let write = || std::fs::write(&created, "y\n").unwrap();
        assert_eq!(burst(&root, write, &[ev(EventKind::Create(CreateKind::Any), &[&created])]), both(&["src/deep/b.py"]));
        let tmp = root.join("src/.a.py.swp");
        let rename = EventKind::Modify(ModifyKind::Name(RenameMode::Both));
        assert_eq!(burst(&root, || (), &[ev(rename, &[&tmp, &a])]), both(&["src/.a.py.swp", "src/a.py"]));
        let removed = root.join("src/old.py");
        assert_eq!(burst(&root, || (), &[ev(EventKind::Remove(RemoveKind::Any), &[&removed])]), both(&["src/old.py"]));
        // ignored files, the index's own files, reads and files in directories no walk enters
        // name nothing
        std::fs::write(root.join("src/x.tmp"), "x\n").unwrap();
        std::fs::create_dir_all(root.join("gen")).unwrap();
        std::fs::write(root.join("gen/g.py"), "x\n").unwrap();
        std::fs::create_dir_all(root.join("src/new.tmp")).unwrap();
        for e in [
            ev(modify, &[&root.join("src/x.tmp")]),
            ev(modify, &[&root.join("gen/g.py")]),
            ev(EventKind::Access(notify::event::AccessKind::Any), &[&a]),
            ev(modify, &[&root.join(".scrutate/index.db")]),
            ev(modify, &[&root.join(".git/index")]),
            // Windows: the write time of a directory an entry was added to
            ev(EventKind::Modify(ModifyKind::Any), &[&root.join("src")]),
            // a new directory the walks do not enter
            ev(EventKind::Create(CreateKind::Folder), &[&root.join("src/new.tmp")]),
        ] {
            assert_eq!(burst(&root, || (), &[e]), Some(BurstFiles::default()));
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn directories_rules_and_lost_events_walk_the_tree() {
        let modify = EventKind::Modify(ModifyKind::Any);
        type Change = fn(&Path);
        let cases: [(&str, Change, EventKind, &str); 12] = [
            (
                "a directory moved away and another made in its place",
                |r| std::fs::rename(r.join("src/deep"), r.join("src/deeper")).and_then(|_| std::fs::create_dir(r.join("src/deep"))).unwrap(),
                EventKind::Create(CreateKind::Folder),
                "src/deep",
            ),
            ("a new directory (inotify)", |r| std::fs::create_dir(r.join("src/x")).unwrap(), EventKind::Create(CreateKind::Folder), "src/x"),
            ("a new directory (Windows)", |r| std::fs::create_dir(r.join("src/y")).unwrap(), EventKind::Create(CreateKind::Any), "src/y"),
            ("a removed directory the walk found", |r| std::fs::remove_dir(r.join("gone_dir")).unwrap(), EventKind::Remove(RemoveKind::Any), "gone_dir"),
            (
                "a directory renamed away",
                |r| std::fs::rename(r.join("src/deep"), r.join("src/deeper")).unwrap(),
                EventKind::Modify(ModifyKind::Name(RenameMode::From)),
                "src/deep",
            ),
            (
                "a directory replaced by a file",
                |r| std::fs::remove_dir(r.join("gone_dir")).and_then(|_| std::fs::write(r.join("gone_dir"), "x")).unwrap(),
                modify,
                "gone_dir",
            ),
            ("an ignore file", |_| (), modify, "src/.ignore"),
            ("a .gitignore", |_| (), modify, ".gitignore"),
            ("git's exclude", |_| (), modify, ".git/info/exclude"),
            ("the configuration file", |_| (), modify, ".scrutate.toml"),
            ("a ripgrep ignore file", |_| (), modify, ".rgignore"),
            ("a removed .gitignore", |_| (), EventKind::Remove(RemoveKind::File), "src/deep/.gitignore"),
        ];
        for (what, change, kind, rel) in cases {
            let root = tree("full");
            let a = root.join("src/a.py");
            assert_eq!(burst(&root, || change(&root), &[ev(modify, &[&a]), ev(kind, &[&root.join(rel)])]), None, "{what}");
            std::fs::remove_dir_all(&root).unwrap();
        }
        let root = tree("lost");
        let a = root.join("src/a.py");
        for (what, e) in
            [("a lost queue", Ok(Event::new(EventKind::Other).set_flag(notify::event::Flag::Rescan))), ("a watcher error", Err(notify::Error::generic("test")))]
        {
            assert_eq!(burst(&root, || (), &[ev(modify, &[&a]), e]), None, "{what}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn each_index_gets_the_files_its_walk_lists() {
        let root = tree("split");
        let names = ["src/a.py", "src/.hidden.py", "src/gone.py", "src/a.tmp", "src/img.png"];
        for name in names {
            std::fs::write(root.join(name), "x\n").unwrap();
        }
        std::fs::create_dir_all(root.join("node_modules/m")).unwrap();
        std::fs::write(root.join("node_modules/m/i.js"), "x\n").unwrap();
        let modify = EventKind::Modify(ModifyKind::Any);
        let gone = || std::fs::remove_file(root.join("src/gone.py")).unwrap();
        let mut events: Vec<_> = names.iter().map(|n| ev(modify, &[&root.join(n)])).collect();
        events.push(ev(modify, &[&root.join("node_modules/m/i.js")]));
        let f = burst(&root, gone, &events).unwrap();
        assert_eq!(f.code, ["src/.hidden.py", "src/a.py", "src/gone.py"]);
        assert_eq!(f.text, ["node_modules/m/i.js", "src/a.py", "src/gone.py", "src/img.png"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_rules_of_a_nested_repository_decide_a_burst() {
        let root = tree("nested");
        std::fs::create_dir_all(root.join("vendor/lib/.git")).unwrap();
        std::fs::create_dir_all(root.join("vendor/lib/out")).unwrap();
        std::fs::write(root.join(".gitignore"), "*.gen.py\nout/\n").unwrap();
        std::fs::write(root.join("vendor/lib/x.gen.py"), "x\n").unwrap();
        std::fs::write(root.join("vendor/lib/out/o.py"), "x\n").unwrap();
        std::fs::write(root.join("src/y.gen.py"), "x\n").unwrap();
        let modify = EventKind::Modify(ModifyKind::Any);
        let events: Vec<_> = ["vendor/lib/x.gen.py", "vendor/lib/out/o.py", "src/y.gen.py"].iter().map(|n| ev(modify, &[&root.join(n)])).collect();
        assert_eq!(burst(&root, || (), &events), both(&["vendor/lib/out/o.py", "vendor/lib/x.gen.py"]));
        let code = CodeWalk::new(&root);
        let walked = Walked::new(&root, &code);
        assert!(walked.code.contains(&root.join("vendor/lib/out")) && walked.text.contains(&root.join("vendor/lib/out")));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_linked_worktree_watches_the_exclude_file_of_its_main_repository() {
        let base = tree("wt");
        let main = base.join("main_repo/.git");
        std::fs::create_dir_all(main.join("info")).unwrap();
        std::fs::create_dir_all(main.join("worktrees/wt")).unwrap();
        std::fs::write(main.join("worktrees/wt/commondir"), "../..\n").unwrap();
        let wt = base.join("wt");
        std::fs::create_dir_all(&wt).unwrap();
        std::fs::write(wt.join(".git"), format!("gitdir: {}\n", main.join("worktrees/wt").display())).unwrap();
        assert_eq!(git_exclude(&wt), Some(main.join("info/exclude")));
        assert_eq!(git_exclude(&base.join("src")), Some(base.join(".git/info/exclude")));
        let walked = Walked::new(&wt, &CodeWalk::new(&wt));
        assert!(walked.watched().contains(&main.join("info")));
        assert!(walked.sets_rules(&wt, &main.join("info/exclude")));
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn a_burst_walks_while_a_directory_has_no_watch_and_once_the_last_walk_is_old() {
        let root = tree("due");
        let mut w = Walked::new(&root, &CodeWalk::new(&root));
        assert!(!w.due(&root));
        w.unwatched = 1;
        assert!(w.due(&root), "a directory without a watch");
        w.unwatched = 0;
        if let Some(old) = Instant::now().checked_sub(WALK_EVERY) {
            w.at = old;
            assert!(w.due(&root), "an old walk");
            w.at = Instant::now();
        }
        std::fs::remove_file(root.join(".scrutate/text/manifest.json")).unwrap();
        assert!(w.due(&root), "the text index went away");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn watches_are_set_root_first_then_shallow_directories_by_path() {
        let root = PathBuf::from("/r");
        let dirs: HashSet<PathBuf> = ["/r/b/c", "/r/a/z/y", "/r", "/r/b", "/r/a", "/r/a/z", "/r/c/a"].iter().map(PathBuf::from).collect();
        let order: Vec<String> = watch_order(&root, &dirs).iter().map(|p| p.display().to_string()).collect();
        assert_eq!(order, ["/r", "/r/a", "/r/b", "/r/a/z", "/r/b/c", "/r/c/a", "/r/a/z/y"]);
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
