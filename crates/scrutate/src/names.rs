//! What a name refers to. Every question about a use of a name asks `Names`: callers, callees,
//! references, the language server's definition, imports and importers. The rules that decide
//! live in `resolve/`; this module owns who asks them, once per request, with each file read and
//! parsed once. The caller count ranking reads is the index's (`Index::call_counts`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::Result;

use crate::index::{ChunkRow, Symbol};
use crate::resolve::{join, Occurrence, Resolution, Resolved, Resolver};
use crate::search::{Hit, Searcher};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Dependency {
    pub line: i64,
    pub module: String,
    /// repository file the module resolves to, when it can be found
    pub resolved: Option<String>,
}

/// (importing file, line, module as written)
pub type Importer = (String, i64, String);

/// One request's view of what the names of the repository refer to.
pub struct Names<'a> {
    s: &'a Searcher,
    rv: Resolver<'a>,
    /// by file: the byte each line starts at
    line_starts: HashMap<String, Vec<usize>>,
}

impl<'a> Names<'a> {
    pub fn new(s: &'a Searcher) -> Result<Names<'a>> {
        Ok(Names { s, rv: Resolver::new(&s.ix)?, line_starts: HashMap::new() })
    }

    /// The definition the occurrence of `name` at 1-based `line`, 0-based byte `col` of `path`
    /// refers to and how that was found, or why none was.
    pub fn at(&mut self, path: &str, line: u64, col: usize, name: &str) -> Resolution {
        self.rv.resolve_at(path, line, col, name)
    }

    /// Every whole-word occurrence of `name` (Engine A), each resolved to the definition it refers to.
    pub fn references(&mut self, name: &str, max_results: u64) -> Result<Resolved> {
        let s = self.s;
        let bare = &crate::intel::bare_name(s, name)?;
        let q = crate::instant::Query { want_spans: true, ..crate::resolve::name_search(bare, max_results) };
        let walk = crate::files::repository_files(&s.ix.root);
        // the text index, when it holds the files as the last look found them, says which can match
        let files = crate::stores::grep_files(&s.ix, &q, &walk);
        let (found, mut truncated) = crate::intel::occurrences_in(&s.ix.root, files.as_ref().unwrap_or(&walk), &q, &s.cancel)?;
        let mut notes = Vec::new();
        let mut hits = Vec::new();
        for r in found {
            match r {
                crate::instant::Record::Match(l) => hits.push(l),
                crate::instant::Record::Note { path, text, .. } => notes.push((path.to_string(), text)),
                _ => {}
            }
        }
        let mut occurrences = Vec::new();
        // Engine A stops at `max_results` lines; one long line can hold more uses than that
        let total: usize = hits.iter().map(|h| h.spans.len()).sum();
        truncated |= total as u64 > max_results;
        for h in hits {
            let line_text: std::sync::Arc<str> = h.text.trim_end_matches(['\n', '\r']).into();
            for &(a, b) in h.spans.iter().take(max_results.saturating_sub(occurrences.len() as u64) as usize) {
                let ((da, db), col) = self.rv.decoded_span(&h.path, h.line, (a, b));
                // the occurrence as this file spells it
                let resolution = self.rv.resolve_at(&h.path, h.line, col, h.text.get(da..db).unwrap_or(bare));
                occurrences.push(Occurrence { path: h.path.to_string(), line: h.line, column: a as u64 + 1, text: line_text.clone(), at: da, resolution });
            }
        }
        let mut definitions: Vec<ChunkRow> = s.ix.definitions(bare)?.into_iter().filter(|d| crate::resolve::same_name(&d.name, bare)).collect();
        let mut known: HashSet<i64> = definitions.iter().map(|d| d.id).collect();
        for o in &occurrences {
            if let Ok((id, _)) = o.resolution {
                if known.insert(id) {
                    definitions.extend(s.ix.chunk(id)?);
                }
            }
        }
        let mut first_overload = HashMap::new();
        if occurrences.iter().any(|o| matches!(o.resolution, Ok((_, "overload")))) {
            for d in &definitions {
                if let [first, _, ..] = self.rv.overloads(d.id)[..] {
                    first_overload.insert(d.id, first);
                }
            }
        }
        Ok(Resolved { definitions, occurrences, truncated, notes, first_overload })
    }

    /// What the resolver makes of the occurrences of `name` in lines `start..=end` of `path`.
    fn uses(&mut self, path: &str, (start, end): (i64, i64), name: &str) -> Uses {
        let fi = self.rv.file(path);
        let src = &fi.src;
        // a file of thousands of call sites is not walked from its top for each one
        let starts = self
            .line_starts
            .entry(path.to_string())
            .or_insert_with(|| std::iter::once(0).chain(src.bytes().enumerate().filter(|&(_, c)| c == b'\n').map(|(i, _)| i + 1)).collect());
        let first = (start.max(1) - 1) as usize;
        let from = starts.get(first).copied().unwrap_or(src.len());
        let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
        let methods = crate::resolve::methods_only(path);
        let mut uses = Uses { got: HashSet::new(), dispatched: HashSet::new(), unsure: true, unsure_free: true, own_file: false };
        let (mut unsure, mut unsure_free, mut seen, mut elsewhere) = (false, false, false, false);
        for (i, line) in src[from..].lines().enumerate().map(|(i, l)| (i + first, l)).take((end - start + 1).max(0) as usize) {
            for (col, _) in line.match_indices(name) {
                if ident(line[..col].chars().next_back()) || ident(line[col + name.len()..].chars().next()) {
                    continue;
                }
                match self.rv.resolve_at(path, i as u64 + 1, col, name) {
                    // the name of a definition is not a call of it
                    Ok((_, "definition")) => {}
                    Ok((id, why)) => {
                        seen = true;
                        let all = if why == "overload" { self.rv.overloads(id) } else { vec![id] };
                        if why != "super" {
                            uses.dispatched.extend(&all);
                        }
                        // a use the arguments do not tie to one overload stands for all of them
                        uses.got.extend(all);
                    }
                    // an import from outside, a local, a field or a string is decided: not this definition
                    Err(why) => {
                        seen = true;
                        let unread = matches!(why, "no grammar" | crate::resolve::UNREAD | crate::resolve::TOO_DEEP);
                        if unread || matches!(why, "receiver" | "not found") {
                            unsure = true;
                            unsure_free |= !(methods && line[..col].trim_end().ends_with('.'));
                            let receiver = [".", "->", "::"].iter().any(|r| line[..col].trim_end().ends_with(r));
                            elsewhere |= receiver || !unread;
                        }
                    }
                }
            }
        }
        uses.unsure = unsure || !seen;
        uses.unsure_free = unsure_free || !seen;
        uses.own_file = unsure && !elsewhere;
        uses
    }

    /// The definitions whose code calls what `name` stands for, and how many of them were kept only
    /// because the resolver could not tell which same-named definition they call. Those come after
    /// the decided ones, first the ones whose code names the target's type, then the ones in its package.
    /// -> (callers: decided, then undecided, then unchecked; how many are undecided; how many unchecked)
    pub fn callers(&mut self, name: &str, limit: usize) -> Result<(Vec<Hit>, usize, usize)> {
        let s = self.s;
        let langs = Langs { root: &s.ix.root, seen: Default::default() };
        let mut targets = crate::ops::targets(s, name, 500, false)?;
        // a call of a class runs its constructor: `Point(1)`, `new Widget()`, Ruby `Invoice.new`
        let (classes, made_by_new) = constructed(s, &langs, &targets)?;
        targets.extend(classes);
        let ids: HashSet<i64> = targets.iter().map(|t| t.id).collect();
        let mut names: BTreeSet<&str> = targets.iter().map(|t| t.name.as_str()).collect();
        if !made_by_new.is_empty() {
            names.insert("new");
        }
        let owners: HashSet<&str> =
            targets.iter().filter_map(|t| t.qualname.rsplit_once('.').map(|(o, _)| o.rsplit('.').next().unwrap_or(o))).filter(|o| !o.is_empty()).collect();
        let packages: HashSet<&str> = targets.iter().map(|t| package(&t.path)).collect();
        let fits = |path: &str| targets.iter().any(|t| langs.same_family(&t.path, path));
        let (mut out, mut unsure_rows, mut unchecked_rows, mut seen) = (Vec::new(), Vec::new(), Vec::new(), HashSet::new());
        // a call of a method a target overrides or implements may run the target
        let overridden: HashSet<i64> = targets.iter().flat_map(|t| self.rv.overridden(t.id)).filter(|id| !ids.contains(id)).collect();
        let names_owner = |text: &str| owners.iter().any(|o| has_word(text, o));
        let likely = |r: &ChunkRow| (!names_owner(&r.text), !packages.contains(package(&r.path)), r.path.clone(), r.start);
        let all_free = |n: &str| targets.iter().filter(|t| t.name == n).all(|t| free(&t.kind, &t.qualname));
        let mut asked = 0;
        for n in names {
            let rows: Vec<Symbol> = s.ix.named(n)?.into_iter().filter(|r| r.kind != "window").collect();
            // with another definition of the name about, the likeliest call sites are asked first
            let rivals = rows.iter().any(|r| !ids.contains(&r.id) && fits(&r.path));
            let mut sites = s.ix.callers_of(n)?;
            sites.retain(|r| fits(&r.path) && seen.insert(r.id));
            if n == "new" && !targets.iter().any(|t| t.name == "new") {
                sites.retain(|r| made_by_new.iter().any(|class| has_word(&r.text, &format!("{class}.new"))));
            }
            if rivals {
                sites.sort_by_cached_key(&likely);
            }
            for row in sites {
                // a common name has thousands of call sites: the likeliest are resolved, the rest stay undecided
                if asked >= CALLERS_RESOLVED {
                    unchecked_rows.push(row);
                    continue;
                }
                asked += 1;
                let uses = self.uses(&row.path, (row.start, row.end), n);
                let unsure = (if all_free(n) { uses.unsure_free } else { uses.unsure }) || !uses.dispatched.is_disjoint(&overridden);
                if uses.got.is_disjoint(&ids) {
                    // with no other definition of the name, a call the resolver cannot place is this one's
                    if unsure && rivals {
                        let mine = |r: &&Symbol| own((&row.path, &row.qualname), &r.path, &r.kind, &r.qualname);
                        let mine: Vec<i64> = rows.iter().filter(|r| uses.own_file && mine(r)).map(|r| r.id).collect();
                        if mine.is_empty() {
                            unsure_rows.push(row);
                            continue;
                        }
                        if mine.iter().all(|id| !ids.contains(id)) {
                            continue;
                        }
                    } else if !unsure {
                        continue;
                    }
                }
                out.push(Hit::from_row(&row, true));
                if out.len() >= limit {
                    return Ok((out, 0, 0));
                }
            }
        }
        unsure_rows.sort_by_cached_key(&likely);
        let undecided = unsure_rows.len().min(limit - out.len());
        out.extend(unsure_rows.iter().take(undecided).map(|r| Hit::from_row(r, true)));
        let unchecked = unchecked_rows.len().min(limit - out.len());
        out.extend(unchecked_rows.iter().take(unchecked).map(|r| Hit::from_row(r, true)));
        Ok((out, undecided, unchecked))
    }

    /// What the code of the definitions `name` stands for calls: definitions of the repository in
    /// the same language family, and the names it calls that have none.
    /// -> (resolved definitions, external names, the files of the definitions whose calls these are)
    pub fn callees(&mut self, name: &str, limit: usize) -> Result<(Vec<Hit>, Vec<String>, Vec<String>)> {
        let s = self.s;
        let kinds: Vec<&str> = crate::search::CODE_KINDS.iter().copied().chain(["key", "macro"]).collect();
        let langs = Langs { root: &s.ix.root, seen: Default::default() };
        // a key is what a call runs only in the caller's own language (Nix, whose functions are keys)
        let callable = |from: &str, r: &ChunkRow| langs.same_family(from, &r.path) && (r.kind != "key" || langs.id(from) == langs.id(&r.path));
        let (mut resolved, mut external, mut seen) = (Vec::new(), Vec::new(), HashSet::new());
        let defs = crate::ops::targets(s, name, 5, true)?;
        let from = defs.iter().map(|d| d.path.clone()).collect();
        for d in defs {
            let names = s.ix.callees_of(d.id)?;
            for n in names {
                let rows: Vec<ChunkRow> = s.ix.named_of_kinds(&n, &kinds)?.into_iter().filter(|r| callable(&d.path, r)).collect();
                if rows.is_empty() {
                    if !external.contains(&n) {
                        external.push(n);
                    }
                    continue;
                }
                let uses = self.uses(&d.path, (d.start, d.end), &n);
                let tied: Vec<&ChunkRow> = rows.iter().filter(|r| uses.got.contains(&r.id)).collect();
                let picked: Vec<&ChunkRow> = if !tied.is_empty() {
                    tied
                } else if uses.unsure {
                    let mut near: Vec<&ChunkRow> = rows.iter().filter(|r| uses.unsure_free || !free(&r.kind, &r.qualname)).collect();
                    let mine = |r: &&ChunkRow| own((&d.path, &d.qualname), &r.path, &r.kind, &r.qualname);
                    if uses.own_file && near.iter().any(mine) {
                        near.retain(mine);
                    }
                    // the nearest three: the caller's file, then its directory
                    let dir = |p: &str| p.rsplit_once('/').map_or(String::new(), |(d, _)| d.to_string());
                    near.sort_by_key(|r| (r.path != d.path, dir(&r.path) != dir(&d.path)));
                    near.truncate(3);
                    near
                } else {
                    Vec::new()
                };
                if picked.is_empty() && !external.contains(&n) {
                    external.push(n.clone());
                }
                resolved.extend(picked.into_iter().filter(|r| seen.insert(r.id)).map(|r| Hit::from_row(r, true)));
            }
        }
        external.sort();
        resolved.truncate(limit);
        Ok((resolved, external, from))
    }

    /// Modules a file imports, resolved to repository files where the module path maps onto one.
    /// A statement naming several files (`from . import a, b`) is listed once per file.
    pub fn imports(&mut self, path: &str) -> Result<Vec<Dependency>> {
        let rows = self.s.ix.imports_of(path)?;
        let mut out = Vec::new();
        for (line, module) in rows {
            let mut found = self.import_files(&module, path, line);
            if path.ends_with(".go") && found.len() > 1 {
                // a Go import names a package directory: the file named after it stands for it
                let dir = found[0].rsplit_once('/').map_or("", |(d, _)| d);
                let named = format!("{}/{}.go", dir, dir.rsplit('/').next().unwrap_or(dir));
                let pick = found.iter().find(|f| **f == named).or_else(|| found.iter().find(|f| !f.ends_with("_test.go"))).unwrap_or(&found[0]).clone();
                found = vec![pick];
            }
            if found.is_empty() {
                out.push(Dependency { line, module: module.clone(), resolved: None });
            }
            out.extend(found.into_iter().map(|f| Dependency { line, module: module.clone(), resolved: Some(f) }));
        }
        for (line, name, file) in self.rv.rust_mods(path) {
            out.push(Dependency { line, module: name, resolved: file });
        }
        out.sort_by_key(|d| d.line);
        Ok(out)
    }

    /// The files one recorded import names: through the resolver `refs` uses for the languages it
    /// reads, else by mapping the module path onto the repository's files.
    fn import_files(&mut self, module: &str, from: &str, line: i64) -> Vec<String> {
        match self.rv.import_files(from, line, module) {
            Some(found) => found,
            None => module_file(module, from, self.rv.files(), &self.s.ix.root).into_iter().collect(),
        }
    }

    /// Files that import `target`: a module name as written, or a repository file (every file
    /// whose imports resolve to it, plus modules spelled like its path), and how many imports
    /// resolved to no file although they may import `target`: a relative one whose path is the
    /// target's, or a bare one that names the target's module.
    pub fn importers(&mut self, target: &str) -> Result<(Vec<Importer>, usize)> {
        let rows = self.s.ix.imports()?;
        let is_file = self.rv.files().contains(target);
        let stem = target.rsplit_once('.').map(|(s, _)| s).unwrap_or(target);
        let stem = stem.strip_suffix(".d").unwrap_or(stem);
        let dotted: String = stem.replace('/', ".");
        let dotted_pkg =
            dotted.strip_suffix(".__init__").or_else(|| dotted.strip_suffix(".index")).or_else(|| dotted.strip_suffix(".mod")).map(|s| s.to_string());
        // the last name of the module the file is: `a/b.py` -> `b`, `a/b/__init__.py` -> `b`
        let own = {
            let mut parts = stem.rsplit('/');
            let last = parts.next().unwrap_or(stem);
            if matches!(last, "__init__" | "index" | "mod" | "lib" | "main") {
                parts.next().unwrap_or(last)
            } else {
                last
            }
        };
        let target_dir = target.rsplit_once('/').map_or("", |(d, _)| d);
        let (mut out, mut unsure) = (Vec::new(), 0);
        let mut prev = String::new();
        for (path, line, module) in rows {
            // rows come by path: an importer is dropped once read, so memory does not grow with their number
            if path != prev {
                self.rv.forget(&prev);
                prev.clone_from(&path);
            }
            if path == target {
                continue;
            }
            let hit = if is_file {
                let found = self.import_files(&module, &path, line);
                let hit = found.iter().any(|f| f == target)
                    || module == dotted
                    || module.replace("::", ".") == dotted
                    || dotted_pkg.as_deref().map(|p| module == p || module.replace("::", ".") == p).unwrap_or(false)
                    // an include found through a search path the index does not know
                    || (found.is_empty() && module.rsplit('/').next() == Some(target.rsplit('/').next().unwrap_or(target)));
                let may_name = match relative_path(&module, &path).filter(|_| module.starts_with('.')) {
                    // a relative import names one path: the file without its extension, or its directory
                    Some(p) => p == stem || p == target || p == target_dir,
                    // a bare name in JavaScript or TypeScript that the resolver did not map is a package
                    None => {
                        !JS_EXTS.iter().any(|e| path.ends_with(e))
                            && module.split(|c: char| !(c.is_alphanumeric() || c == '_' || c == '-')).any(|seg| seg == own)
                    }
                };
                if !hit && found.is_empty() && may_name {
                    unsure += 1;
                }
                hit
            } else {
                module == target
                    || module.starts_with(&format!("{target}."))
                    || module.starts_with(&format!("{target}::"))
                    || module.starts_with(&format!("{target}/"))
                    || module.ends_with(&format!(".{target}"))
                    || module.ends_with(&format!("::{target}"))
                    || module.ends_with(&format!("/{target}"))
            };
            if hit {
                out.push((path, line, module));
            }
        }
        if is_file && target.ends_with(".rs") {
            for parent in self.rv.rust_mod_parents(target) {
                for (line, name, file) in self.rv.rust_mods(&parent) {
                    if file.as_deref() == Some(target) {
                        out.push((parent.clone(), line, name));
                    }
                }
            }
            out.sort();
            out.dedup();
        }
        Ok((out, unsure))
    }
}

/// Call sites the resolver is asked about in one `callers` answer, likeliest first. Every site of
/// a Java name with 2,000 of them costs about 2.5 s; answers kept per file content would lift it.
pub const CALLERS_RESOLVED: usize = 400;

/// Languages whose code calls each other's definitions directly.
fn family(id: &'static str) -> &'static str {
    match id {
        "javascript" | "typescript" | "tsx" => "js",
        "c" | "cpp" | "cuda" | "objc" => "c",
        "java" | "kotlin" | "scala" | "groovy" => "jvm",
        "erlang" | "elixir" | "gleam" => "beam",
        "ocaml" | "ocaml_interface" => "ocaml",
        "csharp" | "fsharp" => "dotnet",
        id => id,
    }
}

/// The language of each file an answer asks about, by its text where the extension names two.
struct Langs<'a> {
    root: &'a Path,
    seen: std::cell::RefCell<HashMap<String, Option<&'static str>>>,
}

impl Langs<'_> {
    fn id(&self, path: &str) -> Option<&'static str> {
        if let Some(id) = self.seen.borrow().get(path) {
            return *id;
        }
        let lower = path.to_lowercase();
        let shared = crate::langs::BY_CONTENT.iter().any(|(e, _)| lower.ends_with(e));
        let text = shared.then(|| crate::resolve::repo_text(self.root, path)).flatten();
        let id = crate::langs::detect(path, text.as_deref()).map(|s| s.id);
        self.seen.borrow_mut().insert(path.to_string(), id);
        id
    }

    /// A file of no known language may call or be called by any.
    fn same_family(&self, a: &str, b: &str) -> bool {
        match (self.id(a), self.id(b)) {
            (Some(x), Some(y)) => family(x) == family(y),
            _ => true,
        }
    }
}

/// What the resolver makes of the occurrences of a name in some lines of a file.
struct Uses {
    /// the definitions they resolve to
    got: HashSet<i64>,
    /// the ones of `got` a call reaches other than through `super`, where an override may run instead
    dispatched: HashSet<i64>,
    /// one stayed unresolved (or none was found)
    unsure: bool,
    /// one stayed unresolved that may call a free function: not `x.name(..)` where a method
    /// call never reaches a free function
    unsure_free: bool,
    /// every unresolved one is a call without a receiver in a language the resolver does not
    /// read: it runs the definition in its own file when there is one (`own`)
    own_file: bool,
}

/// Whether the definition (`path`, `kind`, `qualname`) is what a call without a receiver in the
/// caller (its path and qualified name) runs: a free function of the caller's file, or a method
/// of the caller's own type.
fn own((from, caller): (&str, &str), path: &str, kind: &str, qualname: &str) -> bool {
    let owner = |q: &str| q.rsplit_once('.').map(|(o, _)| o.to_string());
    path == from && (free(kind, qualname) || owner(qualname) == owner(caller))
}

/// A definition outside any class or type: `x.f()` does not call it where `methods_only` holds.
/// A method of an object literal has no owner in its name and is still called through one.
fn free(kind: &str, qualname: &str) -> bool {
    kind != "method" && !qualname.contains('.')
}

/// The directory a file's package starts at: what comes before its `src/`, else its top directory.
fn package(path: &str) -> &str {
    match path.strip_prefix("src/").map(|_| 0).or_else(|| path.find("/src/")) {
        Some(i) => &path[..i],
        None => path.split_once('/').map_or("", |(top, _)| top),
    }
}

/// Whether `word` is in `text` where it is not part of a longer identifier.
fn has_word(text: &str, word: &str) -> bool {
    let ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    text.match_indices(word).any(|(i, _)| !ident(text[..i].chars().next_back()) && !ident(text[i + word.len()..].chars().next()))
}

/// The classes whose constructor is among `targets`, and the names of the Ruby ones, which a
/// call of `new` on the class makes.
fn constructed(s: &Searcher, langs: &Langs, targets: &[Hit]) -> Result<(Vec<Hit>, Vec<String>)> {
    let (mut classes, mut made_by_new) = (Vec::<Hit>::new(), Vec::new());
    for t in targets {
        let Some((owner, name)) = t.qualname.rsplit_once('.') else { continue };
        let lang = langs.id(&t.path).unwrap_or("");
        let constructor = match lang {
            "python" => name == "__init__",
            "javascript" | "typescript" | "tsx" => name == "constructor",
            "ruby" => name == "initialize",
            "php" => name == "__construct",
            "swift" => name == "init",
            // Java, C#, C++, Dart: the method named as its class
            _ => name == owner.rsplit('.').next().unwrap_or(owner),
        };
        if !constructor {
            continue;
        }
        let class = s.ix.class_at(&t.path, owner)?;
        let Some(class) = class.filter(|c| !targets.iter().chain(&classes).any(|x| x.id == c.id)) else { continue };
        if lang == "ruby" {
            made_by_new.push(class.name.clone());
        }
        classes.push(Hit::from_row(&class, true));
    }
    Ok((classes, made_by_new))
}

const SOURCE_EXTS: &[&str] = &[
    ".py", ".js", ".mjs", ".cjs", ".jsx", ".ts", ".tsx", ".go", ".rs", ".java", ".rb", ".php", ".kt", ".swift", ".scala", ".dart", ".ex", ".exs", ".hs",
    ".lua", ".c", ".h", ".cc", ".cpp", ".hpp", ".cs", ".jl", ".r", ".R", ".sh", ".zig", ".elm",
];

const JS_EXTS: &[&str] = &[".js", ".mjs", ".cjs", ".jsx", ".ts", ".mts", ".cts", ".tsx"];

/// The repository path a relative import of `from` names before an extension or an index file
/// is tried (`./x`, `../x`, Python's `.x`, `..pkg.mod` and `.`); None for any other module.
fn relative_path(module: &str, from: &str) -> Option<String> {
    let dir = from.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    if module.starts_with("./") || module.starts_with("../") {
        Some(join(dir, module))
    } else if module.starts_with('.') {
        Some(crate::resolve::py_relative(from, module))
    } else {
        None
    }
}

/// For the languages the resolver does not read: a best-effort mapping of an import string onto a
/// file of the repository. Handles dotted/`::`-separated module paths, relative paths (`./x`,
/// `../x`, `from . import`), `crate::` roots and index files.
fn module_file(module: &str, from: &str, files: &HashSet<String>, root: &Path) -> Option<String> {
    let dir = from.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
    let mut candidates: Vec<String> = Vec::new();
    if let Some(path) = relative_path(module, from) {
        candidates.push(path);
    } else {
        let m = module.strip_prefix("crate::").map(|m| format!("src/{m}")).unwrap_or_else(|| module.to_string());
        let slashed = m.replace("::", "/").replace('.', "/");
        candidates.push(slashed.clone());
        // java-style: the last segment may be a class inside the file of the previous segment
        if let Some((head, _)) = slashed.rsplit_once('/') {
            candidates.push(head.to_string());
        }
        // same directory first (C includes, sibling modules)
        if !dir.is_empty() {
            candidates.insert(0, join(dir, &slashed));
            candidates.insert(0, join(dir, &m));
        }
        candidates.push(m);
    }
    let try_one = |c: &str| -> Option<String> {
        let c = c.trim_matches('/');
        if files.contains(c) {
            return Some(c.to_string());
        }
        for ext in SOURCE_EXTS {
            let p = format!("{c}{ext}");
            if files.contains(&p) {
                return Some(p);
            }
        }
        for idx in ["__init__.py", "index.js", "index.ts", "index.tsx", "mod.rs", "lib.rs", "index.mjs"] {
            let p = if c.is_empty() { idx.to_string() } else { format!("{c}/{idx}") };
            if files.contains(&p) {
                return Some(p);
            }
        }
        None
    };
    for c in &candidates {
        if let Some(p) = try_one(c) {
            return Some(p);
        }
    }
    if module.starts_with('.') {
        return None;
    }
    let slashed = module.replace("::", "/").replace('.', "/");
    let segs: Vec<&str> = slashed.split('/').filter(|s| !s.is_empty()).collect();
    crate::resolve::below_own_package(root, &segs).and_then(|rest| try_one(&rest.join("/")))
}
