//! Which files belong to the repository, for each index. Engine A's default walk (which the text
//! index reuses) is `instant::build_walker`; the code index's set is `list_files`; `Membership`
//! answers for one changed path whether it can belong to either.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ignore::gitignore::Gitignore;

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

/// Directory names the code index never enters: `SKIP_DIRS` (less `SKIP_UNLESS_GIT` inside a git
/// repository) plus those in `SCRUTATE_INDEX_EXCLUDE_DIRS`.
fn skip_dirs(git: bool) -> Vec<String> {
    let mut skip: Vec<String> = SKIP_DIRS.iter().filter(|d| !(git && SKIP_UNLESS_GIT.contains(d))).map(|s| s.to_string()).collect();
    if let Ok(v) = std::env::var("SCRUTATE_INDEX_EXCLUDE_DIRS") {
        skip.extend(v.split(',').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()));
    }
    skip
}

/// The directory names the code index of `root` never enters.
pub fn skipped_dirs(root: &Path) -> Vec<String> {
    skip_dirs(in_git(root))
}

/// Relative paths ('/' separated, sorted) of the files worth indexing: the same rules as ripgrep's
/// file listing (.gitignore, hidden files) plus the usual build/vendor directories.
pub fn list_files(root: &Path) -> Vec<String> {
    let mut out = Vec::new();
    // hidden files are indexed (.github/workflows/*.yml, .eslintrc.js, .env.example are code or
    // config people search for); SKIP_DIRS prunes VCS internals and caches
    let skip_list = skip_dirs(in_git(root));
    let skip = move |name: &str| skip_list.iter().any(|s| s == name);
    let skip2 = skip.clone();
    let walker = ignore::WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .ignore(true)
        .parents(true)
        .follow_links(false)
        .filter_entry(move |e| !(e.file_type().map(|t| t.is_dir()).unwrap_or(false) && skip(&e.file_name().to_string_lossy())))
        .build();
    for entry in walker.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        if entry.file_name() == ".git" {
            continue; // a `.git` *file* (worktree or submodule pointer) is VCS metadata, not source
        }
        let Ok(rel) = entry.path().strip_prefix(root) else {
            continue;
        };
        let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
        if is_indexable(&rel) && !rel.split('/').any(&skip2) {
            out.push(rel);
        }
    }
    out.sort();
    out
}

/// Decides whether a changed path can change what one of the indexes holds: Engine B indexes the
/// `list_files` set (hidden files included, `SKIP_DIRS` pruned), the text index the files of
/// Engine A's default walk (hidden files skipped). Both honour the same ignore files.
#[cfg_attr(target_os = "macos", allow(dead_code))] // the macOS watcher polls the index instead
pub struct Membership {
    root: PathBuf,
    skip: Vec<String>,
    /// top of the git repository holding `root`; `.gitignore` rules only apply inside one
    git_top: Option<PathBuf>,
    exclude: Gitignore,
    global: Gitignore,
    /// per directory: its `.ignore`, then its `.gitignore` (inside a git repository)
    dirs: HashMap<PathBuf, [Option<Gitignore>; 2]>,
}

#[cfg_attr(target_os = "macos", allow(dead_code))]
impl Membership {
    pub fn new(root: &Path) -> Membership {
        let git_top = root.ancestors().find(|a| a.join(".git").exists()).map(Path::to_path_buf);
        // both matchers judge paths relative to the repository top, as git does; a matcher rooted
        // anywhere else panics on a path outside its root
        let (exclude, global) = match &git_top {
            Some(top) => {
                let mut exclude = ignore::gitignore::GitignoreBuilder::new(top);
                exclude.add(top.join(".git/info/exclude"));
                (exclude.build().unwrap_or_else(|_| Gitignore::empty()), ignore::gitignore::GitignoreBuilder::new(top).build_global().0)
            }
            None => (Gitignore::empty(), Gitignore::empty()),
        };
        Membership { root: root.to_path_buf(), skip: skip_dirs(git_top.is_some()), git_top, exclude, global, dirs: HashMap::new() }
    }

    fn matchers(&mut self, dir: &Path) -> &[Option<Gitignore>; 2] {
        let git = self.git_top.is_some();
        self.dirs.entry(dir.to_path_buf()).or_insert_with(|| {
            let read = |name: &str| Some(dir.join(name)).filter(|p| p.is_file()).map(|p| Gitignore::new(&p).0);
            [read(".ignore"), if git { read(".gitignore") } else { None }]
        })
    }

    fn ignored(&mut self, path: &Path) -> bool {
        let is_dir = path.is_dir();
        let top = self.git_top.clone().unwrap_or_else(|| self.root.clone());
        // as the walk decides: a `.ignore` at any level speaks before every `.gitignore`, and
        // within one kind the nearest directory speaks first
        for kind in 0..2 {
            let mut dir = path.parent();
            while let Some(d) = dir {
                if let Some(m) = &self.matchers(d)[kind] {
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
    pub fn may_contain(&mut self, path: &Path) -> bool {
        let Ok(rel) = path.strip_prefix(&self.root) else { return false };
        let names: Vec<String> = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect();
        if names.first().map(|n| n == INDEX_DIR || n == ".git").unwrap_or(false) {
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn membership_follows_the_index_rules() {
        let root = std::env::temp_dir().join(format!("scrutate_watch_filter_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".ignore"), "gen/\n*.log\n").unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let mut f = Membership::new(&root);
        assert!(f.may_contain(&root.join("src/a.rs")));
        assert!(f.may_contain(&root.join(".github/ci.yml")), "hidden files are indexed by Engine B");
        assert!(!f.may_contain(&root.join(".scrutate/index.db")));
        assert!(!f.may_contain(&root.join("node_modules/x/index.js")), "SKIP_DIRS without a text index");
        assert!(!f.may_contain(&root.join("gen/out.rs")));
        assert!(!f.may_contain(&root.join("src/run.log")));
        assert!(!f.may_contain(Path::new("/elsewhere/a.rs")));
        std::fs::write(root.join(".ignore"), "").unwrap();
        assert!(f.may_contain(&root.join(".ignore")));
        assert!(f.may_contain(&root.join("gen/out.rs")), "a changed ignore file is read again");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn an_ignore_file_speaks_before_a_nearer_gitignore_as_in_the_walk() {
        let root = std::env::temp_dir().join(format!("scrutate_membership_order_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join(".ignore"), "*.log\n").unwrap();
        std::fs::write(root.join(".gitignore"), "*.tmp\n").unwrap();
        std::fs::write(root.join("sub/.gitignore"), "!keep.log\n!keep.tmp\n").unwrap();
        for name in ["sub/keep.log", "sub/keep.tmp", "sub/a.rs", "sub/other.tmp"] {
            std::fs::write(root.join(name), "x\n").unwrap();
        }
        let root = std::fs::canonicalize(&root).unwrap();
        let walked = list_files(&root);
        let mut f = Membership::new(&root);
        for (name, kept) in [("sub/keep.log", false), ("sub/keep.tmp", true), ("sub/a.rs", true), ("sub/other.tmp", false)] {
            assert_eq!(walked.iter().any(|p| p == name), kept, "the walk on {name}: {walked:?}");
            assert_eq!(f.may_contain(&root.join(name)), kept, "membership of {name}");
        }
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn rules_in_git_info_exclude_apply_from_the_repository_top() {
        let root = std::env::temp_dir().join(format!("scrutate_membership_exclude_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git/info")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join(".git/info/exclude"), "*.tmp\n/gen/\n").unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let mut f = Membership::new(&root);
        assert!(f.may_contain(&root.join("src/a.rs")));
        assert!(!f.may_contain(&root.join("src/a.tmp")));
        assert!(!f.may_contain(&root.join("gen/out.rs")));
        assert!(f.may_contain(&root.join("src/gen/out.rs")), "/gen/ is anchored at the top");
        std::fs::remove_dir_all(&root).unwrap();
    }
}
