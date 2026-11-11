//! `scrutate watch` against the real binary: what it sees while it starts, which directories it
//! watches, and what happens when the tree under it moves or goes away.

use std::fs;
use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_watchfiles_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Repo { root: fs::canonicalize(&root).unwrap() }
    }
    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
    }
    fn run(&self, args: &[&str]) -> i32 {
        Command::new(BIN).args(args).current_dir(&self.root).stdin(Stdio::null()).output().unwrap().status.code().unwrap_or(-1)
    }
    fn indexed(&self, word: &str) -> bool {
        self.run(&["grep", "--index", "-F", word]) == 0
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

struct KillOnDrop(Child);

impl Drop for KillOnDrop {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Start `watch` in `root`; stderr lines arrive on the receiver.
fn start_watch(root: &Path) -> (KillOnDrop, Receiver<String>) {
    let mut child = Command::new(BIN)
        .args(["watch", "."])
        .current_dir(root)
        .env("SCRUTATE_WATCH_DEBUG", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for line in std::io::BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    (KillOnDrop(child), rx)
}

fn wait_watching(rx: &Receiver<String>) {
    while !rx.recv_timeout(Duration::from_secs(60)).expect("watch starts").contains("watching") {}
}

fn wait_for(secs: u64, what: &str, mut ok: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !ok() {
        assert!(t0.elapsed().as_secs() < secs, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn files_written_while_the_first_update_runs_are_indexed() {
    let r = Repo::new("startup");
    r.write("a.py", b"def alpha():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    // the first update indexes these, which takes long enough to write during it
    for i in 0..3000 {
        r.write(&format!("src/d{}/m{i}.py", i % 30), format!("def existing_{i}():\n    return {i}\n").as_bytes());
    }
    let (_child, rx) = start_watch(&r.root);
    // write until the watcher says it is watching; nothing is written after that
    let mut n = 0;
    loop {
        r.write(&format!("late/l{n}.py"), format!("def written_late_{n}():\n    return {n}\n").as_bytes());
        n += 1;
        match rx.recv_timeout(Duration::from_millis(30)) {
            Ok(line) if line.contains("watching") => break,
            Ok(_) | Err(std::sync::mpsc::RecvTimeoutError::Timeout) => assert!(n < 3000, "watch never started"),
            Err(e) => panic!("watch exited: {e}"),
        }
    }
    let last = n - 1;
    wait_for(20, "the files written during start-up", || r.indexed(&format!("written_late_{last}()")) && r.run(&["check", "--files"]) == 0);
}

#[test]
fn a_directory_that_replaces_one_moved_away_loses_its_ignore_rules() {
    let r = Repo::new("moved");
    r.write("a.py", b"def alpha():\n    return 1\n");
    r.write("sub2/.ignore", b"b.py\n");
    r.write("sub2/c.py", b"def gamma():\n    return 3\n");
    assert_eq!(r.run(&["index"]), 0);
    let elsewhere = r.root.with_extension("elsewhere");
    let _ = fs::remove_dir_all(&elsewhere);
    let (_child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    // the matcher of sub2/.ignore is cached once an event under sub2 is judged
    r.write("sub2/c.py", b"def gamma_two():\n    return 3\n");
    wait_for(20, "the first change", || r.indexed("gamma_two"));
    fs::rename(r.root.join("sub2"), &elsewhere).unwrap();
    fs::create_dir(r.root.join("sub2")).unwrap();
    // past the burst of the move, so only the write's own event can bring b.py in
    std::thread::sleep(Duration::from_millis(1500));
    r.write("sub2/b.py", b"def beta_after_move():\n    return 2\n");
    let found = Instant::now();
    while !r.indexed("beta_after_move") && found.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    let _ = fs::remove_dir_all(&elsewhere);
    assert!(r.indexed("beta_after_move"), "sub2/b.py is indexed once sub2/.ignore is gone");
}

#[test]
fn new_nested_directories_are_watched() {
    let r = Repo::new("nested");
    r.write("a.py", b"def alpha():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    let (_child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    r.write("x/y/z/n.py", b"def nested_first():\n    return 1\n");
    wait_for(20, "the new nested file", || r.indexed("nested_first"));
    // a later change inside the new directories still sends an event
    r.write("x/y/z/n.py", b"def nested_second():\n    return 2\n");
    wait_for(20, "a change inside the new directories", || r.indexed("nested_second") && !r.indexed("nested_first"));
}

// Windows does not let a directory that is being watched be removed
#[cfg(unix)]
#[test]
fn watch_stops_with_an_error_when_its_root_is_removed() {
    let r = Repo::new("rootgone");
    r.write("a.py", b"def alpha():\n    return 1\n");
    assert_eq!(r.run(&["index"]), 0);
    let (mut child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    fs::remove_dir_all(&r.root).unwrap();
    let t0 = Instant::now();
    let status = loop {
        if let Some(s) = child.0.try_wait().unwrap() {
            break s;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "watch keeps running after its root was removed");
        std::thread::sleep(Duration::from_millis(100));
    };
    let err: Vec<String> = rx.try_iter().collect();
    assert_eq!(status.code(), Some(2), "{err:?}");
    assert!(err.iter().any(|l| l.contains("is gone")), "{err:?}");
}

/// Inotify watches the process holds, from /proc/PID/fdinfo.
#[cfg(target_os = "linux")]
fn inotify_watches(pid: u32) -> usize {
    let mut n = 0;
    for fd in fs::read_dir(format!("/proc/{pid}/fd")).unwrap().flatten() {
        if fs::read_link(fd.path()).map(|l| l.to_string_lossy() == "anon_inode:inotify").unwrap_or(false) {
            let info = fs::read_to_string(format!("/proc/{pid}/fdinfo/{}", fd.file_name().to_string_lossy())).unwrap_or_default();
            n += info.lines().filter(|l| l.starts_with("inotify wd")).count();
        }
    }
    n
}

#[cfg(target_os = "linux")]
#[test]
fn only_directories_the_index_holds_are_watched() {
    let r = Repo::new("count");
    fs::create_dir_all(r.root.join(".git")).unwrap();
    r.write(".gitignore", b"node_modules/\ntarget/\n");
    r.write("src/a.py", b"def alpha():\n    return 1\n");
    for i in 0..200 {
        r.write(&format!("node_modules/pkg{i}/lib/index.js"), b"module.exports = 1;\n");
        fs::create_dir_all(r.root.join(format!("target/debug/build/c{i}/out"))).unwrap();
    }
    assert_eq!(r.run(&["index"]), 0);
    let (child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    assert_eq!(inotify_watches(child.0.id()), 2, "the root and src/");
    r.write("src/new/b.py", b"def beta():\n    return 2\n");
    wait_for(20, "the new file", || r.indexed("beta"));
    assert_eq!(inotify_watches(child.0.id()), 3, "the root, src/ and src/new/");
    fs::remove_dir_all(r.root.join("src/new")).unwrap();
    wait_for(20, "the removal", || !r.indexed("beta"));
    assert_eq!(inotify_watches(child.0.id()), 2, "a removed directory's watch is gone");
}

/// Stderr lines up to and including the first update line that reports `changed` and `removed`
/// files (for each index that has a change).
fn wait_update(rx: &Receiver<String>, changed: usize, removed: usize) -> Vec<String> {
    let want = format!("[scrutate] {changed} changed, {removed} removed");
    let t0 = Instant::now();
    let mut seen = Vec::new();
    loop {
        let left = Duration::from_secs(30).checked_sub(t0.elapsed()).unwrap_or_else(|| panic!("no line {want:?} in {seen:?}"));
        let line = rx.recv_timeout(left).unwrap_or_else(|_| panic!("no line {want:?} in {seen:?}"));
        let done = line.starts_with(&want);
        seen.push(line);
        if done {
            return seen;
        }
    }
}

/// The last burst line among `lines`, where events drive the watcher (macOS checks on a timer).
#[cfg(not(target_os = "macos"))]
fn last_burst(lines: &[String]) -> &str {
    lines.iter().rev().find(|l| l.contains("burst:")).map(String::as_str).unwrap_or_else(|| panic!("no burst line in {lines:?}"))
}

/// Assert that the burst before the update named `paths` (when given) and walked the tree or not.
fn burst_was(_lines: &[String], _paths: Option<usize>, _full: bool) {
    #[cfg(not(target_os = "macos"))]
    {
        let line = last_burst(_lines);
        assert!(line.ends_with(&format!("full walk: {}", if _full { "yes" } else { "no" })), "{_lines:?}");
        if let Some(n) = _paths {
            assert!(line.contains(&format!("burst: {n} paths,")), "{_lines:?}");
        }
    }
}

/// A repository with an index and a watcher that has said it is watching.
fn watched(name: &str, files: &[(&str, &str)]) -> (Repo, KillOnDrop, Receiver<String>) {
    let r = Repo::new(name);
    for (rel, text) in files {
        r.write(rel, text.as_bytes());
    }
    assert_eq!(r.run(&["index"]), 0);
    let (child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    (r, child, rx)
}

#[test]
fn a_save_deep_in_a_wide_tree_updates_that_file_without_a_walk() {
    let r = Repo::new("wide");
    for i in 0..400 {
        r.write(&format!("m{}/p{i}/a.py", i % 20), format!("def f_{i}():\n    return {i}\n").as_bytes());
    }
    assert_eq!(r.run(&["index"]), 0);
    let (_child, rx) = start_watch(&r.root);
    wait_watching(&rx);
    #[cfg(target_os = "linux")]
    let before = inotify_watches(_child.0.id());
    r.write("m3/p123/a.py", b"def saved_deep():\n    return 1\n");
    burst_was(&wait_update(&rx, 1, 0), Some(1), false);
    #[cfg(target_os = "linux")]
    assert_eq!(inotify_watches(_child.0.id()), before, "no directory is watched again");
    assert!(r.indexed("saved_deep"));
}

#[test]
fn a_file_created_next_to_another_is_indexed_without_a_walk() {
    let (r, _child, rx) = watched("created", &[("src/a.py", "def alpha():\n    return 1\n")]);
    r.write("src/b.py", b"def created_next():\n    return 2\n");
    burst_was(&wait_update(&rx, 1, 0), Some(1), false);
    assert!(r.indexed("created_next"));
}

#[test]
fn a_removed_file_leaves_the_index_without_a_walk() {
    let (r, _child, rx) = watched("removed", &[("src/a.py", "def alpha():\n    return 1\n"), ("src/b.py", "def removed_one():\n    return 2\n")]);
    fs::remove_file(r.root.join("src/b.py")).unwrap();
    burst_was(&wait_update(&rx, 0, 1), Some(1), false);
    assert!(!r.indexed("removed_one"));
}

#[test]
fn a_file_renamed_in_its_directory_is_moved_in_the_index_without_a_walk() {
    let (r, _child, rx) = watched("renamed", &[("src/a.py", "def alpha():\n    return 1\n"), ("src/b.py", "def renamed_one():\n    return 2\n")]);
    fs::rename(r.root.join("src/b.py"), r.root.join("src/c.py")).unwrap();
    burst_was(&wait_update(&rx, 1, 1), Some(2), false);
    assert_eq!(r.run(&["grep", "--index", "-l", "renamed_one"]), 0);
    assert_eq!(r.run(&["check", "--files"]), 0);
}

#[test]
fn an_editor_save_through_a_temporary_file_is_indexed_without_a_walk() {
    let (r, _child, rx) = watched("editor", &[("src/a.py", "def before_save():\n    return 1\n")]);
    r.write("src/.a.py.swp", b"def after_save():\n    return 2\n");
    fs::rename(r.root.join("src/.a.py.swp"), r.root.join("src/a.py")).unwrap();
    burst_was(&wait_update(&rx, 1, 0), Some(2), false);
    assert!(r.indexed("after_save") && !r.indexed("before_save"));
}

#[test]
fn a_save_with_a_new_directory_in_the_same_burst_walks_the_tree() {
    let (r, _child, rx) = watched("mixed", &[("src/a.py", "def alpha():\n    return 1\n")]);
    r.write("src/a.py", b"def alpha_saved():\n    return 1\n");
    r.write("src/new/b.py", b"def in_new_dir():\n    return 2\n");
    burst_was(&wait_update(&rx, 2, 0), None, true);
    assert!(r.indexed("alpha_saved") && r.indexed("in_new_dir"));
}

#[test]
fn a_save_of_an_ignored_file_starts_no_update() {
    let (r, _child, rx) = watched("ignored", &[(".ignore", "skip.py\n"), ("src/a.py", "def alpha():\n    return 1\n")]);
    r.write("src/skip.py", b"def ignored_one():\n    return 2\n");
    // longer than a burst's quiet time, and than macOS's first pause between checks
    std::thread::sleep(Duration::from_millis(2500));
    let lines: Vec<String> = rx.try_iter().collect();
    assert!(lines.iter().all(|l| !l.contains("changed")), "{lines:?}");
    #[cfg(not(target_os = "macos"))]
    assert!(lines.iter().all(|l| !l.contains("burst:")), "{lines:?}");
    r.write("src/a.py", b"def alpha_two():\n    return 1\n");
    burst_was(&wait_update(&rx, 1, 0), Some(1), false);
}
