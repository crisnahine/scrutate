//! What a change touches: the definitions a git diff changes, the callers that reach them with
//! their distances, and the test files to run. The diff comes from the `git` program; the
//! definitions of the tree as it is from the index, those of the base from a parse of each file's
//! base version; the callers from `Names`, as `callers` gives them.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{anyhow, bail, Result};
use serde_json::{json, Value};

use crate::fmt::{echo, shown};
use crate::names::Names;
use crate::search::{Hit, Searcher};

/// Steps of callers followed by default: past three, the callers of a much used function grow
/// toward most of the repository and say little about what to test.
pub const DEPTH: usize = 3;
pub const MAX_DEPTH: usize = 10;
/// Callers one step asks for, as `callers --limit 500`.
pub const STEP_CALLERS: usize = 500;
/// Callers one answer lists at most; the walk stops there.
pub const MAX_CALLERS: usize = 2000;

/// A definition the diff changed. `status`: `added`, `edited` or `deleted` (then named, with
/// its lines, as the base has it). Kind `file`: lines outside every definition, with no window.
pub struct Changed {
    pub hit: Hit,
    pub status: &'static str,
}

/// A definition that calls a changed one, `distance` steps away; `via` is the location of what it calls.
pub struct Caller {
    pub hit: Hit,
    pub distance: usize,
    pub via: String,
    /// the resolver could not tell whether it calls that definition or another of its name
    pub undecided: bool,
    /// it calls a definition the change deleted, by a name no other definition answers
    pub calls_deleted: bool,
}

/// A test file to run, and the symbol in it that reached a change (none: the diff changed the file).
pub struct Test {
    pub path: String,
    pub distance: usize,
    pub via: Option<Hit>,
}

pub struct Impact {
    /// the revision as given (`HEAD` by default) and the commit it names
    pub against: String,
    pub base: String,
    pub staged: bool,
    pub depth: usize,
    pub changed: Vec<Changed>,
    pub callers: Vec<Caller>,
    pub tests: Vec<Test>,
    /// (path, why it gives no symbol): a binary file, a submodule, a file the index leaves out
    pub notes: Vec<(String, String)>,
    pub stopped: bool,
}

/// What the working tree (with `staged`, the staged changes) changes against `base` (`HEAD`, a
/// revision, or `A...B` for the merge base of the two), on the index as it is.
pub fn impact(s: &Searcher, base: Option<&str>, staged: bool, depth: usize) -> Result<Impact> {
    let git = Git::new(&s.ix.root)?;
    let against = base.unwrap_or("HEAD").to_string();
    let base = git.base(&against)?;
    let entries = git.diff(&base, staged)?;
    let mut b = Builder { s, staged, changed: Vec::new(), notes: Vec::new(), seen: HashSet::new(), files: Vec::new() };
    let blobs = git.blobs(&entries.iter().flat_map(|e| b.blobs_wanted(e)).collect::<BTreeSet<_>>())?;
    for e in &entries {
        b.entry(e, &blobs)?;
    }
    let Builder { mut changed, mut notes, files, .. } = b;
    changed.sort_by(|a, b| {
        (&a.hit.path, a.hit.start, a.hit.end, &a.hit.kind, &a.hit.qualname).cmp(&(&b.hit.path, b.hit.start, b.hit.end, &b.hit.kind, &b.hit.qualname))
    });
    notes.sort();
    let (callers, stopped) = walk(s, &changed, depth)?;
    let tests = test_files(&changed, &callers, &files);
    Ok(Impact { against, base, staged, depth, changed, callers, tests, notes, stopped })
}

/// Kinds whose callers are not asked for: lines outside every definition, and a module.
fn walkable(h: &Hit) -> bool {
    h.id != 0 && !matches!(h.kind.as_str(), "window" | "module" | "file")
}

/// Callers breadth first, each at the smallest distance it is reached by, in (distance, path, line) order.
fn walk(s: &Searcher, changed: &[Changed], depth: usize) -> Result<(Vec<Caller>, bool)> {
    let mut seen: HashSet<i64> = changed.iter().map(|c| c.hit.id).filter(|id| *id != 0).collect();
    let mut frontier: Vec<Hit> = changed.iter().map(|c| c.hit.clone()).filter(walkable).collect();
    // a deleted definition has no index entry to ask `callers` about: its callers are found by its name
    let gone: Vec<&Hit> =
        changed.iter().filter(|c| c.status == "deleted" && !matches!(c.hit.kind.as_str(), "window" | "module" | "file")).map(|c| &c.hit).collect();
    let mut callers = Vec::new();
    let mut names = Names::new(s)?;
    for distance in 1..=depth {
        let mut next = Vec::new();
        let deleted = if distance == 1 { gone.len() } else { 0 };
        for k in 0..deleted + frontier.len() {
            if s.cancel.load(std::sync::atomic::Ordering::Relaxed) {
                bail!("cancelled");
            }
            let (target, decided, hits) = match k.checked_sub(deleted) {
                None => {
                    let hits = names.callers_of_deleted(gone[k], STEP_CALLERS)?;
                    (gone[k], hits.len(), hits)
                }
                Some(j) => {
                    let (hits, undecided) = names.callers(&frontier[j].loc(), STEP_CALLERS)?;
                    (&frontier[j], hits.len() - undecided, hits)
                }
            };
            for (i, h) in hits.into_iter().enumerate() {
                if !seen.insert(h.id) {
                    continue;
                }
                if callers.len() >= MAX_CALLERS {
                    return Ok((callers, true));
                }
                if walkable(&h) {
                    next.push(h.clone());
                }
                callers.push(Caller { hit: h, distance, via: target.loc(), undecided: i >= decided, calls_deleted: k < deleted });
            }
        }
        next.sort_by(|a, b| (&a.path, a.start, a.end).cmp(&(&b.path, b.start, b.end)));
        frontier = next;
    }
    callers.sort_by(|a, b| (a.distance, &a.hit.path, a.hit.start, a.hit.end).cmp(&(b.distance, &b.hit.path, b.hit.start, b.hit.end)));
    Ok((callers, false))
}

/// The test files (`is_test_path`, the index's test flag) that hold a changed symbol or a caller,
/// and those the diff changed, each once at its smallest distance.
fn test_files(changed: &[Changed], callers: &[Caller], files: &[String]) -> Vec<Test> {
    let mut out: BTreeMap<String, Test> = BTreeMap::new();
    let mut offer = |path: &str, distance: usize, via: Option<&Hit>| {
        if !crate::text::is_test_path(path) {
            return;
        }
        let t = out.entry(path.to_string()).or_insert(Test { path: path.to_string(), distance, via: via.cloned() });
        if distance < t.distance {
            *t = Test { path: path.to_string(), distance, via: via.cloned() };
        }
    };
    for c in changed {
        offer(&c.hit.path, 0, (c.hit.kind != "file").then_some(&c.hit));
    }
    for c in callers {
        offer(&c.hit.path, c.distance, Some(&c.hit));
    }
    for f in files {
        offer(f, 0, None);
    }
    let mut tests: Vec<Test> = out.into_values().collect();
    tests.sort_by(|a, b| (a.distance, &a.path).cmp(&(b.distance, &b.path)));
    tests
}

/// A definition of one side of a file.
#[derive(Clone)]
struct Def {
    kind: String,
    name: String,
    qualname: String,
    start: i64,
    end: i64,
    /// the index's chunk id; 0 for a definition parsed from git's copy
    id: i64,
}

impl Def {
    fn hit(&self, path: &str) -> Hit {
        Hit {
            id: self.id,
            path: path.to_string(),
            start: self.start,
            end: self.end,
            name: self.name.clone(),
            qualname: self.qualname.clone(),
            kind: self.kind.clone(),
            ..Hit::default()
        }
    }

    fn same(&self, other: &Def) -> bool {
        self.kind == other.kind && self.qualname == other.qualname
    }
}

/// The definitions of `src` as the index would hold them, windows aside.
fn parsed(path: &str, src: &[u8]) -> Vec<Def> {
    if src.len() as u64 > crate::index::MAX_FILE_BYTES || !crate::parse::is_indexable(path) {
        return Vec::new();
    }
    let src = String::from_utf8_lossy(src);
    crate::parse::parse_file(path, &src)
        .chunks
        .into_iter()
        .filter(|c| c.kind != "window")
        .map(|c| Def { kind: c.kind.to_string(), name: c.name, qualname: c.qualname, start: c.start, end: c.end, id: 0 })
        .collect()
}

/// Of `defs`, the innermost one around each line of `ranges` (first and last line), and the
/// lines no definition holds.
fn touched(defs: &[Def], ranges: &[(i64, i64)]) -> (BTreeSet<usize>, Vec<i64>) {
    let top = defs.iter().map(|d| d.end).max().unwrap_or(0).max(0) as usize;
    let mut owner = vec![usize::MAX; top + 1];
    let mut order: Vec<usize> = (0..defs.len()).collect();
    // larger first, so an inner definition paints over the one around it
    order.sort_by_key(|&i| std::cmp::Reverse(defs[i].end - defs[i].start));
    for i in order {
        for l in defs[i].start.max(1)..=defs[i].end {
            owner[l as usize] = i;
        }
    }
    let (mut hit, mut outside) = (BTreeSet::new(), Vec::new());
    for &(a, b) in ranges {
        for l in a.max(1)..=b {
            match owner.get(l as usize) {
                Some(&i) if i != usize::MAX => {
                    hit.insert(i);
                }
                _ => outside.push(l),
            }
        }
    }
    (hit, outside)
}

/// The one of `defs` that is `d` (same kind and qualified name), nearest its lines.
fn nearest<'a>(defs: &'a [Def], d: &Def) -> Option<&'a Def> {
    defs.iter().filter(|n| n.same(d)).min_by_key(|n| (n.start - d.start).abs())
}

struct Builder<'a> {
    s: &'a Searcher,
    staged: bool,
    changed: Vec<Changed>,
    notes: Vec<(String, String)>,
    /// chunk ids, and (path, start, kind, qualname) of what has no id, already listed
    seen: HashSet<(i64, String, i64, String, String)>,
    /// every path the diff changed
    files: Vec<String>,
}

impl Builder<'_> {
    fn blobs_wanted(&self, e: &Entry) -> Vec<String> {
        if e.submodule || e.binary {
            return Vec::new();
        }
        let mut v = Vec::new();
        if e.old.is_some() {
            v.push(e.old_blob.clone());
        }
        if self.staged && e.new.is_some() && !e.untracked {
            v.push(e.new_blob.clone());
        }
        v
    }

    fn push(&mut self, hit: Hit, status: &'static str) {
        let key = if hit.id != 0 {
            (hit.id, String::new(), 0, String::new(), String::new())
        } else {
            (0, hit.path.clone(), hit.start, hit.kind.clone(), hit.qualname.clone())
        };
        if self.seen.insert(key) {
            self.changed.push(Changed { hit, status });
        }
    }

    fn entry(&mut self, e: &Entry, blobs: &BTreeMap<String, Vec<u8>>) -> Result<()> {
        let path = e.new.as_deref().or(e.old.as_deref()).unwrap_or_default().to_string();
        self.files.push(path.clone());
        if let (Some(old), Some(new)) = (&e.old, &e.new) {
            if old != new {
                self.notes.push((new.clone(), format!("renamed from {}", shown(old))));
            }
        }
        if e.submodule {
            self.notes.push((path, "a submodule: its commits are not read".into()));
            return Ok(());
        }
        if e.binary {
            self.notes.push((path, "a binary file".into()));
            return Ok(());
        }
        if let Some(new) = e.new.as_deref().filter(|n| !self.s.ix.has_file(n)) {
            self.notes.push((new.to_string(), "not in the index: it has no definitions to name".into()));
            return Ok(());
        }
        let blob = |sha: &str| blobs.get(sha).map(Vec::as_slice).unwrap_or_default();
        let old_defs = match &e.old {
            Some(old) => parsed(old, blob(&e.old_blob)),
            None => Vec::new(),
        };
        let added = e.old.is_none();
        let ix = &self.s.ix;
        // the tree as it is: the index's definitions; the staged copy: a parse of it, matched to the index's
        let (new_defs, indexed): (Vec<Def>, Vec<Def>) = match &e.new {
            Some(new) => {
                let rows: Vec<Def> = ix
                    .outline(new)?
                    .into_iter()
                    .map(|r| Def { kind: r.kind, name: r.name, qualname: r.qualname, start: r.start, end: r.end, id: r.id })
                    .collect();
                if self.staged && !e.untracked {
                    (parsed(new, blob(&e.new_blob)), rows)
                } else {
                    (rows.clone(), rows)
                }
            }
            None => (Vec::new(), Vec::new()),
        };
        let new_ranges = if e.untracked { vec![(1, line_count(&ix.root.join(&path)))] } else { e.new_lines.clone() };
        let (new_hit, new_outside) = touched(&new_defs, &new_ranges);
        let (old_hit, old_outside) = touched(&old_defs, &e.old_lines);
        let new_path = e.new.clone().unwrap_or_default();
        // a staged definition is named where the index holds it now, when it does
        let current = |d: &Def| -> Hit {
            match nearest(&indexed, d).filter(|_| d.id == 0) {
                Some(i) => i.hit(&new_path),
                None => d.hit(&new_path),
            }
        };
        for i in new_hit {
            let d = &new_defs[i];
            let status = if old_defs.iter().any(|o| o.same(d)) { "edited" } else { "added" };
            let h = current(d);
            self.push(h, status);
        }
        for i in old_hit {
            let d = &old_defs[i];
            match nearest(&new_defs, d) {
                Some(n) => {
                    let h = current(n);
                    self.push(h, "edited");
                }
                None => self.push(d.hit(e.old.as_deref().unwrap_or_default()), "deleted"),
            }
        }
        // lines outside every definition: the window that holds them, as callers names such a caller
        let mut bare = !old_outside.is_empty();
        let from_index = e.new.is_some() && (!self.staged || e.untracked);
        let mut after = 0;
        for l in new_outside {
            if !from_index {
                bare = true;
                break;
            }
            if l <= after {
                continue;
            }
            match ix.window_at(&path, l)? {
                Some(w) => {
                    after = w.end;
                    self.push(Hit::from_row(&w, false), if added { "added" } else { "edited" });
                }
                None => bare = true,
            }
        }
        let named = self.changed.iter().any(|c| c.hit.path == path && matches!(c.hit.kind.as_str(), "window" | "module"));
        if bare && !named {
            let status = if e.new.is_none() {
                "deleted"
            } else if added {
                "added"
            } else {
                "edited"
            };
            self.push(Hit { path, kind: "file".into(), ..Hit::default() }, status);
        }
        Ok(())
    }
}

/// Lines of a file, the last one counted without its line break.
fn line_count(p: &Path) -> i64 {
    let b = std::fs::read(p).unwrap_or_default();
    let n = b.iter().filter(|&&c| c == b'\n').count();
    (n + usize::from(b.last().is_some_and(|&c| c != b'\n'))) as i64
}

/// One file of the diff: its paths on each side (None where it does not exist), git's blob ids,
/// and the changed lines of each side as (first, last).
#[derive(Default, Debug)]
struct Entry {
    old: Option<String>,
    new: Option<String>,
    old_blob: String,
    new_blob: String,
    submodule: bool,
    binary: bool,
    untracked: bool,
    old_lines: Vec<(i64, i64)>,
    new_lines: Vec<(i64, i64)>,
}

/// The `git` program run in the repository. Nothing it runs writes: no optional locks, no file
/// system monitor, no external diff or text conversion. A revision is checked before it is
/// passed, so it never reaches git as an option.
struct Git {
    dir: PathBuf,
}

impl Git {
    fn new(root: &Path) -> Result<Git> {
        let git = Git { dir: plain_dir(root) };
        let out = git.command(&["rev-parse", "--is-inside-work-tree"]).output().map_err(not_run)?;
        if !out.status.success() || out.stdout.trim_ascii() != b"true" {
            let why = first_line(&out.stderr);
            if why.is_empty() || why.contains("not a git repository") {
                bail!("{} is not in a git repository: impact reads the changes with git", root.display());
            }
            bail!("git: {why}");
        }
        Ok(git)
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut c = Command::new("git");
        c.current_dir(&self.dir).args(["--no-optional-locks", "-c", "core.fsmonitor=false"]).args(args).stdin(Stdio::null());
        c
    }

    /// (exited with 0, standard output, standard error)
    fn output(&self, args: &[&str]) -> Result<(bool, Vec<u8>, String)> {
        let out = self.command(args).output().map_err(not_run)?;
        Ok((out.status.success(), out.stdout, first_line(&out.stderr)))
    }

    fn run(&self, args: &[&str]) -> Result<Vec<u8>> {
        match self.output(args)? {
            (true, out, _) => Ok(out),
            (false, _, why) => bail!("git {}: {why}", args[0]),
        }
    }

    /// The commit `rev` names; `A...B` is the merge base of the two (an empty side is `HEAD`).
    fn base(&self, rev: &str) -> Result<String> {
        let Some((a, b)) = rev.split_once("...") else { return self.commit(rev) };
        let (a, b) = (if a.is_empty() { "HEAD" } else { a }, if b.is_empty() { "HEAD" } else { b });
        let (x, y) = (self.commit(a)?, self.commit(b)?);
        match self.output(&["merge-base", &x, &y])? {
            (true, out, _) if !out.trim_ascii().is_empty() => Ok(String::from_utf8_lossy(out.trim_ascii()).into_owned()),
            _ => bail!("{} and {} have no merge base", echo(a), echo(b)),
        }
    }

    fn commit(&self, rev: &str) -> Result<String> {
        // a revision is one argument and never an option, so `--output=x` is refused here
        if rev.is_empty() || rev.starts_with('-') {
            bail!("not a revision: {}", echo(rev));
        }
        match self.output(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")])? {
            (true, out, _) => Ok(String::from_utf8_lossy(out.trim_ascii()).into_owned()),
            _ if rev == "HEAD" => bail!("HEAD names no commit yet: commit once, or pass a base revision"),
            _ => bail!("not a revision of this repository: {}", echo(rev)),
        }
    }

    /// The files that differ from `base`, with their changed lines, and the untracked files.
    fn diff(&self, base: &str, staged: bool) -> Result<Vec<Entry>> {
        let common = ["diff", "--no-color", "--no-ext-diff", "--no-textconv", "--relative", "-M", "--src-prefix=a/", "--dst-prefix=b/"];
        let with = |more: &[&str]| -> Vec<String> {
            let mut v: Vec<String> = common.iter().chain(more).map(|s| s.to_string()).collect();
            if staged {
                v.push("--cached".into());
            }
            v.push(base.to_string());
            v.push("--".into());
            v
        };
        let raw = self.run(&with(&["--raw", "-z", "--no-abbrev"]).iter().map(String::as_str).collect::<Vec<_>>())?;
        let patch = self.run(&with(&["-U0", "--inter-hunk-context=0"]).iter().map(String::as_str).collect::<Vec<_>>())?;
        let mut entries = raw_entries(&raw)?;
        let sections = sections(&patch);
        // a file whose stat alone changed is in the raw list and has no section: nothing changed
        entries.retain_mut(|e| {
            let key = e.new.as_deref().or(e.old.as_deref()).unwrap_or_default();
            match sections.get(key) {
                Some(s) => {
                    e.binary = s.binary;
                    e.old_lines = s.old.clone();
                    e.new_lines = s.new.clone();
                    true
                }
                None => e.submodule || e.old != e.new,
            }
        });
        if !staged {
            for p in self.run(&["ls-files", "--others", "--exclude-standard", "-z"])?.split(|&b| b == 0).filter(|p| !p.is_empty()) {
                entries.push(Entry { new: Some(String::from_utf8_lossy(p).into_owned()), untracked: true, ..Entry::default() });
            }
        }
        Ok(entries)
    }

    /// The contents of these blobs, by id, read by one `git cat-file --batch`.
    fn blobs(&self, ids: &BTreeSet<String>) -> Result<BTreeMap<String, Vec<u8>>> {
        let ids: Vec<&String> = ids.iter().filter(|id| !id.is_empty() && id.bytes().any(|b| b != b'0')).collect();
        let mut out = BTreeMap::new();
        if ids.is_empty() {
            return Ok(out);
        }
        let mut child = self.command(&["cat-file", "--batch"]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().map_err(not_run)?;
        let mut stdin = child.stdin.take().ok_or_else(|| anyhow!("git cat-file: no input"))?;
        let asked: String = ids.iter().map(|id| format!("{id}\n")).collect();
        // written beside the read, so a full pipe on either side does not stop both
        let writer = std::thread::spawn(move || stdin.write_all(asked.as_bytes()));
        let mut bytes = Vec::new();
        child.stdout.take().ok_or_else(|| anyhow!("git cat-file: no output"))?.read_to_end(&mut bytes)?;
        let _ = writer.join();
        child.wait()?;
        let mut rest = &bytes[..];
        for id in ids {
            let Some(nl) = rest.iter().position(|&b| b == b'\n') else { break };
            let head = String::from_utf8_lossy(&rest[..nl]).into_owned();
            rest = &rest[nl + 1..];
            let mut parts = head.split(' ');
            let (_, kind, size) = (parts.next(), parts.next(), parts.next().and_then(|n| n.parse::<usize>().ok()));
            if let (Some("blob"), Some(n)) = (kind, size) {
                let n = n.min(rest.len());
                out.insert(id.clone(), rest[..n].to_vec());
                rest = rest.get(n + 1..).unwrap_or_default();
            }
        }
        Ok(out)
    }
}

/// The entries of `git diff --raw -z`: `:mode mode blob blob status`, then one path, or two for a rename.
fn raw_entries(raw: &[u8]) -> Result<Vec<Entry>> {
    let mut fields = raw.split(|&b| b == 0).map(|f| String::from_utf8_lossy(f).into_owned());
    let mut out = Vec::new();
    while let Some(head) = fields.next() {
        let Some(head) = head.strip_prefix(':') else { continue };
        let parts: Vec<&str> = head.split(' ').collect();
        let [old_mode, new_mode, old_blob, new_blob, status] = parts[..] else { bail!("git diff --raw: cannot read {}", echo(head)) };
        let path = fields.next().unwrap_or_default();
        let mut e = Entry { old_blob: old_blob.into(), new_blob: new_blob.into(), submodule: old_mode == "160000" || new_mode == "160000", ..Entry::default() };
        match status.as_bytes().first() {
            Some(b'A') => e.new = Some(path),
            Some(b'D') => e.old = Some(path),
            Some(b'R' | b'C') => {
                e.new = fields.next();
                e.old = Some(path);
            }
            Some(b'U') => bail!("{} is unmerged: finish the merge before asking what it changes", echo(&path)),
            _ => {
                e.old = Some(path.clone());
                e.new = Some(path);
            }
        }
        out.push(e);
    }
    Ok(out)
}

#[derive(Default)]
struct Section {
    binary: bool,
    old: Vec<(i64, i64)>,
    new: Vec<(i64, i64)>,
}

/// The files of a `-U0` patch, by the path they have after the change (before it, when deleted):
/// their changed lines on each side and whether git found them binary.
fn sections(patch: &[u8]) -> BTreeMap<String, Section> {
    let mut out = BTreeMap::new();
    let mut cur: Option<(String, Section)> = None;
    let mut done = |cur: &mut Option<(String, Section)>| {
        if let Some((p, s)) = cur.take() {
            out.insert(p, s);
        }
    };
    for line in patch.split(|&b| b == b'\n') {
        if let Some(head) = line.strip_prefix(b"diff --git ") {
            done(&mut cur);
            cur = Some((header_path(head).unwrap_or_default(), Section::default()));
            continue;
        }
        let Some((path, s)) = cur.as_mut() else { continue };
        if let Some(p) = line.strip_prefix(b"rename to ") {
            *path = unquoted(p);
        } else if line.starts_with(b"Binary files ") || line == b"GIT binary patch" {
            s.binary = true;
        } else if let Some(h) = line.strip_prefix(b"@@ -") {
            let h = String::from_utf8_lossy(h);
            let mut sides = h.split(' ');
            let range = |t: Option<&str>| -> Option<(i64, i64)> {
                let t = t?.trim_start_matches(['-', '+']);
                let (first, n) = match t.split_once(',') {
                    Some((a, n)) => (a.parse().ok()?, n.parse::<i64>().ok()?),
                    None => (t.parse().ok()?, 1),
                };
                (n > 0).then_some((first, first + n - 1))
            };
            if let Some(r) = range(sides.next()) {
                s.old.push(r);
            }
            if let Some(r) = range(sides.next()) {
                s.new.push(r);
            }
        }
    }
    done(&mut cur);
    out
}

/// The path of a `diff --git a/P b/P` line: both halves name the same file unless it was renamed,
/// and then the `rename to` line names it.
fn header_path(head: &[u8]) -> Option<String> {
    if head.first() == Some(&b'"') {
        let (a, rest) = quoted(head)?;
        let b = rest.strip_prefix(b" ").and_then(|r| quoted(r).map(|q| q.0)).unwrap_or(a);
        return Some(String::from_utf8_lossy(b.strip_prefix(b"b/").unwrap_or(&b[..])).into_owned());
    }
    let n = head.len().checked_sub(5)? / 2;
    let (a, b) = (head.get(..n + 2)?, head.get(n + 3..)?);
    match (a.strip_prefix(b"a/"), b.strip_prefix(b"b/")) {
        (Some(x), Some(y)) if x == y => Some(String::from_utf8_lossy(y).into_owned()),
        _ => Some(String::from_utf8_lossy(b.strip_prefix(b"b/").unwrap_or(b)).into_owned()),
    }
}

/// A path git wrote as it is, or in C quotes when it holds special bytes.
fn unquoted(p: &[u8]) -> String {
    match quoted(p) {
        Some((q, _)) => String::from_utf8_lossy(&q).into_owned(),
        None => String::from_utf8_lossy(p).into_owned(),
    }
}

/// A C-quoted string at the start of `s` (`"a\tb\303\251"`), decoded, and what follows it.
fn quoted(s: &[u8]) -> Option<(Vec<u8>, &[u8])> {
    let mut i = 1;
    if s.first() != Some(&b'"') {
        return None;
    }
    let mut out = Vec::new();
    while i < s.len() {
        match s[i] {
            b'"' => return Some((out, &s[i + 1..])),
            b'\\' => {
                let c = *s.get(i + 1)?;
                i += 2;
                match c {
                    b'0'..=b'7' => {
                        let digits = s.get(i - 1..i + 2)?;
                        out.push(u8::from_str_radix(std::str::from_utf8(digits).ok()?, 8).ok()?);
                        i += 2;
                    }
                    b'a' => out.push(7),
                    b'b' => out.push(8),
                    b't' => out.push(b'\t'),
                    b'n' => out.push(b'\n'),
                    b'v' => out.push(11),
                    b'f' => out.push(12),
                    b'r' => out.push(b'\r'),
                    c => out.push(c),
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    None
}

fn not_run(e: std::io::Error) -> anyhow::Error {
    if e.kind() == std::io::ErrorKind::NotFound {
        anyhow!("git is not installed or not on PATH: impact reads the changes with git")
    } else {
        anyhow!("cannot run git: {e}")
    }
}

fn first_line(b: &[u8]) -> String {
    let s = String::from_utf8_lossy(b);
    let line = s.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    line.strip_prefix("fatal: ").or_else(|| line.strip_prefix("error: ")).unwrap_or(line).to_string()
}

/// The root as a directory to start git in: on Windows a verbatim drive path (`\\?\C:\x`, what
/// `canonicalize` gives) in its plain form.
fn plain_dir(root: &Path) -> PathBuf {
    let s = root.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if cfg!(windows) && rest.as_bytes().get(1) == Some(&b':') => PathBuf::from(rest),
        _ => root.to_path_buf(),
    }
}

impl Impact {
    /// The first line: what was compared with what.
    pub fn head(&self) -> String {
        let short = self.base.get(..12).unwrap_or(&self.base);
        format!("{} against {} ({short})", if self.staged { "staged changes" } else { "working tree" }, echo(&self.against))
    }

    pub fn is_empty(&self) -> bool {
        self.changed.is_empty() && self.notes.is_empty() && self.tests.is_empty()
    }

    /// The repository files whose indexed definitions this answer shows.
    pub fn cited(&self) -> Vec<&str> {
        let changed = self.changed.iter().filter(|c| c.status != "deleted").map(|c| c.hit.path.as_str());
        changed.chain(self.callers.iter().map(|c| c.hit.path.as_str())).collect()
    }

    /// One line per section head and per item, as the text pages them.
    pub fn lines(&self) -> Vec<String> {
        let symbol = |h: &Hit| {
            if h.kind == "file" {
                format!("{}  [file]", shown(&h.path))
            } else {
                format!("{}  [{}] {}", crate::fmt::loc(h), h.kind, shown(if h.qualname.is_empty() { &h.name } else { &h.qualname }))
            }
        };
        let mut out = vec![format!("changed: {}", self.changed.len())];
        out.extend(self.changed.iter().map(|c| {
            let at = if c.status == "deleted" && c.hit.kind != "file" { ", lines of the base" } else { "" };
            format!("{}  ({}{at})", symbol(&c.hit), c.status)
        }));
        out.push(format!("callers: {} (depth {})", self.callers.len(), self.depth));
        out.extend(self.callers.iter().map(|c| {
            let mark = if c.calls_deleted {
                "  (calls a deleted definition)"
            } else if c.undecided {
                "  (undecided)"
            } else {
                ""
            };
            format!("{:>2}  {}{mark}", c.distance, symbol(&c.hit))
        }));
        out.push(format!("tests: {}", self.tests.len()));
        out.extend(self.tests.iter().map(|t| match &t.via {
            Some(h) => format!("{}  (distance {}: {})", shown(&t.path), t.distance, symbol(h)),
            None => format!("{}  (changed)", shown(&t.path)),
        }));
        if !self.notes.is_empty() {
            out.push(format!("notes: {}", self.notes.len()));
            out.extend(self.notes.iter().map(|(p, n)| format!("{}  {n}", shown(p))));
        }
        out
    }

    pub fn stop_line(&self) -> String {
        if self.stopped {
            format!("(the walk stopped at {MAX_CALLERS} callers: pass a smaller depth)")
        } else {
            String::new()
        }
    }

    pub fn json(&self) -> Value {
        let symbol = |h: &Hit| {
            if h.kind == "file" {
                json!({"id": h.path, "path": h.path, "start": null, "end": null, "kind": "file", "qualname": ""})
            } else {
                json!({"id": h.loc(), "path": h.path, "start": h.start, "end": h.end, "kind": h.kind, "qualname": if h.qualname.is_empty() { &h.name } else { &h.qualname }})
            }
        };
        let with = |mut v: Value, k: &str, x: Value| {
            v[k] = x;
            v
        };
        json!({
            "against": self.against,
            "base": self.base,
            "staged": self.staged,
            "depth": self.depth,
            "changed": self.changed.iter().map(|c| with(symbol(&c.hit), "status", json!(c.status))).collect::<Vec<_>>(),
            "callers": self.callers.iter().map(|c| {
                let v = with(symbol(&c.hit), "distance", json!(c.distance));
                with(with(with(v, "via", json!(c.via)), "undecided", json!(c.undecided)), "calls_deleted", json!(c.calls_deleted))
            }).collect::<Vec<_>>(),
            "tests": self.tests.iter().map(|t| json!({"path": t.path, "distance": t.distance, "via": t.via.as_ref().map(symbol)})).collect::<Vec<_>>(),
            "notes": self.notes.iter().map(|(p, n)| json!({"path": p, "note": n})).collect::<Vec<_>>(),
            "stopped": self.stopped,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_patch_gives_each_file_its_changed_lines_by_the_path_it_has_now() {
        let patch = b"diff --git a/a.py b/a.py\nindex 1..2 100644\n--- a/a.py\n+++ b/a.py\n@@ -3 +3,2 @@ def f\n-x\n+y\n+z\n@@ -9,2 +10,0 @@\n-p\n-q\n\
diff --git a/old.py b/new.py\nsimilarity index 90%\nrename from old.py\nrename to new.py\n@@ -0,0 +1 @@\n+diff --git a/x b/x\n\
diff --git \"a/t\\tab.py\" \"b/t\\tab.py\"\n@@ -1 +1 @@\n-a\n+b\n\
diff --git a/img.png b/img.png\nBinary files a/img.png and b/img.png differ\n";
        let s = sections(patch);
        assert_eq!(s.keys().map(String::as_str).collect::<Vec<_>>(), ["a.py", "img.png", "new.py", "t\tab.py"]);
        assert_eq!((s["a.py"].old.clone(), s["a.py"].new.clone()), (vec![(3, 3), (9, 10)], vec![(3, 4)]));
        assert_eq!(s["new.py"].new, vec![(1, 1)]);
        assert!(s["img.png"].binary && !s["a.py"].binary);
        assert_eq!(s["t\tab.py"].new, vec![(1, 1)]);
    }

    #[test]
    fn a_line_belongs_to_the_innermost_definition_around_it() {
        let d = |kind: &str, start, end| Def { kind: kind.into(), name: kind.into(), qualname: kind.into(), start, end, id: 0 };
        let defs = [d("class", 1, 10), d("method", 3, 5)];
        let (hit, outside) = touched(&defs, &[(4, 4), (8, 8), (12, 13)]);
        assert_eq!(hit.into_iter().collect::<Vec<_>>(), [0, 1]);
        assert_eq!(outside, [12, 13]);
    }

    #[test]
    fn a_c_quoted_path_is_decoded() {
        assert_eq!(
            quoted(b"\"a\\303\\251\\\"b\" rest").map(|(q, r)| (String::from_utf8(q).unwrap(), r.to_vec())),
            Some(("a\u{e9}\"b".into(), b" rest".to_vec()))
        );
    }
}
