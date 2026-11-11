//! The index survives what a stopped writer leaves behind, and refuses roots and index
//! directories it must not write into.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use scrutate::index::Index;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Tmp(PathBuf);

impl Tmp {
    fn new(name: &str) -> Tmp {
        let p = std::env::temp_dir().join(format!("scrutate_recovery_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&p);
        fs::create_dir_all(&p).unwrap();
        Tmp(fs::canonicalize(&p).unwrap())
    }
}

impl Drop for Tmp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(root: &Path, args: &[&str]) -> (i32, String) {
    let out = Command::new(BIN).arg("--root").arg(root).args(args).stdin(Stdio::null()).output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn leftover_segment_with_the_next_name_does_not_block_an_update() {
    let t = Tmp::new("orphan");
    fs::write(t.0.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    let mut ix = Index::open(&t.0, None).unwrap();
    ix.update(false).unwrap();
    // what a writer stopped before COMMIT leaves: the counter rolled back, the directories stayed
    let next = ix.meta("seg_counter").unwrap().parse::<i64>().unwrap() + 1;
    let seg = t.0.join(".scrutate/seg");
    for name in [format!("s{next:06}"), format!("s{next:06}.tmp")] {
        fs::create_dir_all(seg.join(&name)).unwrap();
        fs::write(seg.join(&name).join("ids.npy"), b"partial").unwrap();
    }
    fs::write(t.0.join("b.py"), "def beta():\n    return 2\n").unwrap();
    let st = ix.update(false).unwrap();
    assert_eq!(st.files_changed, 1);
    drop(ix);
    let mut ix = Index::open(&t.0, None).unwrap();
    fs::write(t.0.join("c.py"), "def gamma():\n    return 3\n").unwrap();
    assert_eq!(ix.update(false).unwrap().files_changed, 1);
    assert!(ix.check(false).unwrap().problems.is_empty());
}

#[test]
fn missing_root_is_an_error_and_nothing_is_created() {
    let t = Tmp::new("missing");
    let missing = t.0.join("typo-root");
    for args in [&["index", "--no-text"][..], &["index", "--text"], &["serve"], &["search", "alpha"]] {
        let (code, err) = run(&missing, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(!missing.exists(), "{args:?} created the root");
    }
    let file = t.0.join("plain.txt");
    fs::write(&file, "x").unwrap();
    let (code, err) = run(&file, &["index", "--no-text"]);
    assert_eq!(code, 2, "{err}");
    assert!(Index::open(&missing, None).is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_index_dir_is_refused() {
    let t = Tmp::new("symlink");
    let repo = t.0.join("repo");
    let outside = t.0.join("outside");
    fs::create_dir_all(&repo).unwrap();
    fs::create_dir_all(outside.join("seg")).unwrap();
    fs::write(outside.join("index.db"), b"keep me").unwrap();
    fs::write(repo.join("a.py"), "def alpha():\n    return 1\n").unwrap();
    std::os::unix::fs::symlink(&outside, repo.join(".scrutate")).unwrap();
    for args in [&["index", "--no-text"][..], &["index", "--text"], &["serve"]] {
        let (code, err) = run(&repo, args);
        assert_eq!(code, 2, "{args:?}: {err}");
        assert!(err.contains(".scrutate"), "{err}");
    }
    scrutate::index::wipe_index(&repo);
    let mut left: Vec<String> = fs::read_dir(&outside).unwrap().map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect();
    left.sort();
    assert_eq!(left, ["index.db", "seg"]);
    assert_eq!(fs::read(outside.join("index.db")).unwrap(), b"keep me");

    // a symlink one level down is refused too
    fs::remove_file(repo.join(".scrutate")).unwrap();
    fs::create_dir_all(repo.join(".scrutate")).unwrap();
    std::os::unix::fs::symlink(outside.join("seg"), repo.join(".scrutate/seg")).unwrap();
    let (code, err) = run(&repo, &["index", "--no-text"]);
    assert_eq!(code, 2, "{err}");
    assert_eq!(fs::read_dir(outside.join("seg")).unwrap().count(), 0);
}

#[test]
fn forced_update_sees_an_edit_that_keeps_size_and_mtime() {
    let t = Tmp::new("samestat");
    let f = t.0.join("a.py");
    fs::write(&f, "def alpha():\n    return 1\n").unwrap();
    let mtime = fs::metadata(&f).unwrap().modified().unwrap();
    let mut ix = Index::open(&t.0, None).unwrap();
    ix.update(false).unwrap();
    fs::write(&f, "def omega():\n    return 1\n").unwrap();
    fs::File::options().write(true).open(&f).unwrap().set_modified(mtime).unwrap();
    assert_eq!(ix.update(false).unwrap().files_changed, 0);
    assert_eq!(ix.update(true).unwrap().files_changed, 1);
    let n: i64 = ix.db.query_row("SELECT COUNT(*) FROM chunks WHERE name='omega'", [], |r| r.get(0)).unwrap();
    assert_eq!(n, 1);
}
