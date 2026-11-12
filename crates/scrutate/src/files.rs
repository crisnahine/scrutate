//! Which files belong to the repository, for each index. Engine A's default walk (which the text
//! index reuses) is `instant::build_walker`; the code index's set is `list_files`, walked by
//! `CodeWalk`; `Membership` asks that walk about one path.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::parse::is_indexable;

/// Where the indexes live under the repository root; no walk enters it.
pub const INDEX_DIR: &str = ".scrutate";

/// `root/.scrutate`, once `root` is an existing directory and nothing the indexes write into is
/// a symbolic link (which would put the index, and its deletions, outside the repository).
pub fn index_dir(root: &Path) -> anyhow::Result<PathBuf> {
    if !root.is_dir() {
        anyhow::bail!("{}: {}", root.display(), if root.exists() { "not a directory" } else { "no such directory" });
    }
    let dir = root.join(INDEX_DIR);
    let refuse = |p: &Path| anyhow::bail!("{} is a symbolic link; the index is only written inside the repository (remove it)", p.display());
    if std::fs::symlink_metadata(&dir).map(|m| m.file_type().is_symlink()).unwrap_or(false) {
        return refuse(&dir);
    }
    // every name the index writes is three levels deep at most (seg/s000001/ids.npy); entry types
    // come from the directory listing, so this costs no stat per file
    let mut todo = vec![(dir.clone(), 0)];
    while let Some((d, depth)) = todo.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_symlink() {
                return refuse(&e.path());
            }
            if ft.is_dir() && depth < 2 {
                todo.push((e.path(), depth + 1));
            }
        }
    }
    Ok(dir)
}

const SKIP_DIRS: &[&str] = &[
    ".git",
    INDEX_DIR,
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    ".mypy_cache",
    ".pytest_cache",
    "target",
    ".next",
    ".tox",
    ".idea",
    ".vscode",
];

/// Build and environment directories some projects commit; inside a git repository the ignore
/// files decide for them.
const SKIP_UNLESS_GIT: &[&str] = &["build", "dist", "target", "venv"];

fn in_git(root: &Path) -> bool {
    root.ancestors().any(|a| a.join(".git").exists())
}

/// Most names `exclude_dirs` of a repository's `.scrutate.toml` adds; the rest are not read.
const MAX_CONFIG_EXCLUDES: usize = 1000;

/// Directory names the code index of `root` never enters: `SKIP_DIRS` (less `SKIP_UNLESS_GIT`
/// inside a git repository) plus those in `SCRUTATE_INDEX_EXCLUDE_DIRS`, or when it is not set,
/// in `[index] exclude_dirs` of `<root>/.scrutate.toml`. A name with a path separator or a
/// backslash, `.` and `..` name no directory and are dropped.
fn skip_dirs(root: &Path) -> Vec<String> {
    let git = in_git(root);
    let mut skip: Vec<String> = SKIP_DIRS.iter().filter(|d| !(git && SKIP_UNLESS_GIT.contains(d))).map(|s| s.to_string()).collect();
    let named = match std::env::var("SCRUTATE_INDEX_EXCLUDE_DIRS") {
        Ok(v) => split_names(&v),
        Err(_) => config_excludes(root),
    };
    // a backslash is a separator on Windows and an escape in the `!name` glob of `grep_code`
    skip.extend(named.into_iter().filter(|n| n != "." && n != ".." && !n.chars().any(|c| std::path::is_separator(c) || c == '\\')));
    skip
}

/// `[index] exclude_dirs` of the root's own `.scrutate.toml`: the setting belongs to the index, so
/// every process that updates it reads the same list, wherever it starts. The file is read as
/// other repository files are: a regular file inside the root, of at most 1 MB.
pub(crate) fn config_excludes(root: &Path) -> Vec<String> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    let Some(text) = crate::resolve::repo_text(&root, ".scrutate.toml") else { return Vec::new() };
    let Ok(table) = text.parse::<toml::Table>() else { return Vec::new() };
    let names = table.get("index").and_then(|i| i.get("exclude_dirs")).and_then(|v| v.as_array());
    names
        .map(|a| a.iter().filter_map(|v| v.as_str()).map(|n| n.trim().to_string()).filter(|n| !n.is_empty()).take(MAX_CONFIG_EXCLUDES).collect())
        .unwrap_or_default()
}

/// A comma-separated list of names; `\,` is a comma inside a name.
pub fn split_names(v: &str) -> Vec<String> {
    let mut out = vec![String::new()];
    let mut chars = v.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&',') => out.last_mut().unwrap().push(chars.next().unwrap()),
            ',' => out.push(String::new()),
            _ => out.last_mut().unwrap().push(c),
        }
    }
    out.into_iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect()
}

/// The directory names the code index of `root` never enters.
pub fn skipped_dirs(root: &Path) -> Vec<String> {
    skip_dirs(root)
}

/// Relative paths ('/' separated, sorted) of the files worth indexing: the same rules as ripgrep's
/// file listing (.gitignore, hidden files) plus the usual build/vendor directories.
pub fn list_files(root: &Path) -> Vec<String> {
    let walk = CodeWalk::new(root);
    let mut out: Vec<String> = walk.builder(root).build().flatten().filter_map(|e| walk.keeps(&e)).collect();
    out.sort();
    out
}

/// The code index's walk of a root. Started at a directory the root's walk enters, it lists that
/// directory as the root's walk does: the ignore files of every directory above it apply.
pub struct CodeWalk {
    root: PathBuf,
    skip: Arc<Vec<String>>,
}

impl CodeWalk {
    pub fn new(root: &Path) -> CodeWalk {
        CodeWalk { root: root.to_path_buf(), skip: Arc::new(skip_dirs(root)) }
    }

    pub fn builder(&self, start: &Path) -> ignore::WalkBuilder {
        // hidden files are indexed (.github/workflows/*.yml, .eslintrc.js, .env.example are code or
        // config people search for); SKIP_DIRS prunes VCS internals and caches
        let skip = self.skip.clone();
        let mut wb = ignore::WalkBuilder::new(start);
        wb.hidden(false)
            .git_ignore(true)
            .git_global(true)
            .git_exclude(true)
            .ignore(true)
            .parents(true)
            .add_custom_ignore_filename(crate::instant::RGIGNORE)
            .follow_links(false)
            .filter_entry(move |e| !(e.file_type().is_some_and(|t| t.is_dir()) && skip.iter().any(|s| *s == *e.file_name().to_string_lossy())));
        wb
    }

    /// The path from the root of a file the walk yielded, when the code index holds that file.
    pub fn keeps(&self, e: &ignore::DirEntry) -> Option<String> {
        // a `.git` *file* (worktree or submodule pointer) is VCS metadata, not source
        if !e.file_type().is_some_and(|t| t.is_file()) || e.file_name() == ".git" {
            return None;
        }
        let rel = slashed(e.path().strip_prefix(&self.root).ok()?);
        self.indexable(&rel).then_some(rel)
    }

    fn indexable(&self, rel: &str) -> bool {
        is_indexable(rel) && !rel.split('/').any(|n| self.skip.iter().any(|s| s == n))
    }
}

fn slashed(rel: &Path) -> String {
    rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/")
}

/// Whether the code index's walk lists one path: each directory from the root down to it is listed
/// with the walk's own rules. A path that does not exist is judged by its directory.
pub struct Membership {
    walk: CodeWalk,
}

impl Membership {
    pub fn new(root: &Path) -> Membership {
        Membership { walk: CodeWalk::new(root) }
    }

    pub fn may_contain(&mut self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.walk.root) else { return false };
        let names: Vec<_> = rel.components().map(|c| c.as_os_str().to_os_string()).collect();
        let mut dir = self.walk.root.clone();
        for (i, name) in names.iter().enumerate() {
            let next = dir.join(name);
            let last = i + 1 == names.len();
            if last && std::fs::symlink_metadata(&next).is_err() {
                return self.walk.indexable(&slashed(rel));
            }
            let Some(e) = self.walk.builder(&dir).max_depth(Some(1)).build().flatten().find(|e| e.depth() == 1 && e.path() == next) else {
                return false;
            };
            if last {
                return e.file_type().is_some_and(|t| t.is_dir()) || self.walk.keeps(&e).is_some();
            }
            dir = next;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tree under the temporary directory; a name ending in `/` is a directory.
    fn tree(name: &str, entries: &[(&str, &str)]) -> PathBuf {
        let root = std::env::temp_dir().join(format!("scrutate_files_{name}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (rel, text) in entries {
            let p = root.join(rel);
            if rel.ends_with('/') {
                std::fs::create_dir_all(&p).unwrap();
            } else {
                std::fs::create_dir_all(p.parent().unwrap()).unwrap();
                std::fs::write(&p, text).unwrap();
            }
        }
        std::fs::canonicalize(&root).unwrap()
    }

    fn every_file(dir: &Path, out: &mut Vec<PathBuf>) {
        for e in std::fs::read_dir(dir).unwrap().flatten() {
            if e.file_type().unwrap().is_dir() {
                every_file(&e.path(), out);
            } else {
                out.push(e.path());
            }
        }
    }

    /// For the index rooted at `root`: `Membership` answers every file of the tree as `list_files`
    /// lists it, and a walk started at each directory the root's walk enters lists that directory's
    /// files as the root's walk does. -> what `list_files` holds.
    fn agrees(root: &Path) -> Vec<String> {
        let walked = list_files(root);
        let mut m = Membership::new(root);
        let mut files = Vec::new();
        every_file(root, &mut files);
        for f in &files {
            let rel = slashed(f.strip_prefix(root).unwrap());
            assert_eq!(m.may_contain(f), walked.contains(&rel), "membership of {rel} ({walked:?})");
        }
        let walk = CodeWalk::new(root);
        for d in walk.builder(root).build().flatten().filter(|e| e.file_type().is_some_and(|t| t.is_dir())) {
            let mut here: Vec<String> = walk.builder(d.path()).max_depth(Some(1)).build().flatten().filter_map(|e| walk.keeps(&e)).collect();
            here.sort();
            let from_root: Vec<String> = walked.iter().filter(|p| root.join(p).parent() == Some(d.path())).cloned().collect();
            assert_eq!(here, from_root, "the walk started at {}", d.path().display());
        }
        walked
    }

    #[test]
    fn an_ignore_file_speaks_before_a_nearer_gitignore_as_in_the_walk() {
        let root = tree(
            "order",
            &[
                (".git/", ""),
                (".ignore", "*.log\nrg.rs\n"),
                (".gitignore", "*.tmp\n"),
                ("sub/.gitignore", "!keep.log\n!keep.tmp\n"),
                (".rgignore", "!rg.rs\n!rg.tmp\n"),
                ("sub/keep.log", "x\n"),
                ("sub/keep.tmp", "x\n"),
                ("sub/a.rs", "x\n"),
                ("sub/other.tmp", "x\n"),
                ("sub/rg.rs", "x\n"),
                ("sub/rg.tmp", "x\n"),
            ],
        );
        let walked = agrees(&root);
        for (name, kept) in
            [("sub/keep.log", false), ("sub/keep.tmp", true), ("sub/a.rs", true), ("sub/other.tmp", false), ("sub/rg.rs", true), ("sub/rg.tmp", true)]
        {
            assert_eq!(walked.iter().any(|p| p == name), kept, "the walk on {name}: {walked:?}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn the_rules_of_directories_above_the_repository_and_of_nested_repositories_apply_as_in_the_walk() {
        let outer = tree(
            "layouts",
            &[
                // an .ignore above the repository top
                (".ignore", "secret.py\n"),
                ("proj/.git/", ""),
                ("proj/secret.py", "x\n"),
                ("proj/a.py", "x\n"),
                // a root outside git holding a repository
                ("ws/repo1/.git/", ""),
                ("ws/repo1/.gitignore", "gen.py\ncov/\n"),
                ("ws/repo1/gen.py", "x\n"),
                ("ws/repo1/cov/x.py", "x\n"),
                ("ws/repo1/a.py", "x\n"),
                // a repository inside one whose ignore patterns would match its files
                ("outer/.git/", ""),
                ("outer/.gitignore", "*.gen.py\nout/\n"),
                ("outer/x.gen.py", "x\n"),
                ("outer/vendor/lib/.git/", ""),
                ("outer/vendor/lib/x.gen.py", "x\n"),
                ("outer/vendor/lib/out/o.py", "x\n"),
                // git's exclude file
                ("ex/.git/info/exclude", "*.tmp\n/gen/\n"),
                ("ex/src/a.tmp", "x\n"),
                ("ex/src/a.rs", "x\n"),
                ("ex/gen/out.rs", "x\n"),
                ("ex/src/gen/out.rs", "x\n"),
            ],
        );
        assert_eq!(agrees(&outer.join("proj")), ["a.py"]);
        assert_eq!(agrees(&outer.join("ws")), ["repo1/.gitignore", "repo1/a.py"]);
        assert_eq!(agrees(&outer.join("outer")), [".gitignore", "vendor/lib/out/o.py", "vendor/lib/x.gen.py"]);
        assert_eq!(agrees(&outer.join("ex")), ["src/a.rs", "src/gen/out.rs"]);
        std::fs::remove_dir_all(&outer).unwrap();
    }

    #[test]
    fn a_linked_worktree_reads_the_exclude_file_of_its_main_repository() {
        let root = tree(
            "worktree",
            &[
                ("main/.git/info/exclude", "private.py\n"),
                ("main/.git/HEAD", "ref: refs/heads/main\n"),
                ("main/.git/worktrees/wt/commondir", "../..\n"),
                ("main/.git/worktrees/wt/HEAD", "ref: refs/heads/wt\n"),
                ("wt/private.py", "x\n"),
                ("wt/a.py", "x\n"),
            ],
        );
        std::fs::write(root.join("wt/.git"), format!("gitdir: {}\n", root.join("main/.git/worktrees/wt").display())).unwrap();
        assert_eq!(agrees(&root.join("wt")), ["a.py"]);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn membership_of_paths_that_are_not_files_of_the_tree() {
        let root = tree("paths", &[(".ignore", "gen/\n"), ("src/a.rs", "x\n"), ("gen/b.rs", "x\n"), (".scrutate/x.py", "x\n"), ("node_modules/x/i.js", "x\n")]);
        let mut m = Membership::new(&root);
        assert!(m.may_contain(&root.join("src/new.rs")), "a missing file is judged by its directory");
        assert!(m.may_contain(&root.join("src")));
        assert!(!m.may_contain(&root.join("gen/new.rs")));
        assert!(!m.may_contain(&root.join("src/new.png")));
        assert!(!m.may_contain(&root.join(".scrutate/x.py")));
        assert!(!m.may_contain(&root.join("node_modules/x/i.js")));
        assert!(!m.may_contain(Path::new("/elsewhere/a.rs")));
        assert!(!m.may_contain(&root));
        std::fs::remove_dir_all(&root).unwrap();
    }
}
