//! Scope-aware reference resolution: which definition an occurrence of a name refers to.
//!
//! Query time only. Definitions come from the index (`chunks`); scopes, qualifiers and import
//! bindings come from parsing the files that hold occurrences (and the modules they import) again
//! with tree-sitter. Python, JavaScript/TypeScript, Rust and Go are understood; other languages
//! leave their occurrences unresolved.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use anyhow::Result;
use rusqlite::params;
use tree_sitter::{Node, Parser, Point, Tree};

use crate::fmt::shown;
use crate::index::{ChunkRow, Index};
use crate::search::Searcher;
use crate::text::name_key;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    Python,
    Js,
    Rust,
    Go,
}

fn lang_of(spec_id: &str) -> Option<Lang> {
    match spec_id {
        "python" => Some(Lang::Python),
        "javascript" | "typescript" | "tsx" => Some(Lang::Js),
        "rust" => Some(Lang::Rust),
        "go" => Some(Lang::Go),
        _ => None,
    }
}

/// Definition chosen for an occurrence and how it was found, or why none was.
pub type Resolution = std::result::Result<(i64, &'static str), &'static str>;

#[derive(Debug, Clone)]
struct Def {
    id: i64,
    name: String,
    qualname: String,
    kind: String,
    start: i64,
    end: i64,
    /// innermost enclosing definition (index into the file's defs)
    parent: Option<usize>,
}

/// A name an import statement brings into a file. `item: None` binds the module itself;
/// `local == "*"` is a wildcard import; an empty `module` means the file itself (JS `export {a as b}`).
#[derive(Debug, Clone, PartialEq)]
struct Binding {
    local: String,
    module: String,
    item: Option<String>,
    /// start bytes of the identifiers in the import statement that name this binding
    sites: Vec<usize>,
}

struct FileInfo {
    lang: Option<Lang>,
    src: String,
    tree: Option<Tree>,
    defs: Vec<Def>,
    bindings: Vec<Binding>,
}

/// What a `tsconfig.json` or `jsconfig.json` says about import names, as repository paths.
#[derive(Default)]
struct TsPaths {
    /// `baseUrl`: the directory a name that is not relative is looked up in
    base: Option<String>,
    /// `paths`: a pattern (`@lib/*`) and the places it stands for (`src/lib/*`)
    aliases: Vec<(String, Vec<String>)>,
}

pub struct Resolver<'a> {
    ix: &'a Index,
    files: HashSet<String>,
    cache: HashMap<String, Rc<FileInfo>>,
    parsers: HashMap<&'static str, Parser>,
    crates: Option<HashMap<String, String>>,
    go_dirs: Option<HashMap<String, Vec<String>>>,
    go_imports: Option<HashMap<String, String>>,
    /// by directory: the config found there, or that there is none
    ts: HashMap<String, Option<Rc<TsPaths>>>,
    /// current depth of `member` / `lookup_top` calls: import and base-class cycles stop here
    nest: usize,
    /// (files, name) of the `lookup_top` calls made for the current `resolve_at`
    looked_up: HashSet<(String, String)>,
}

fn text<'s>(n: Node, src: &'s str) -> &'s str {
    n.utf8_text(src.as_bytes()).unwrap_or("")
}

/// The text of `rel` under `root` when it is a regular file inside `root` of at most
/// `MAX_FILE_BYTES`: a FIFO would block the read, and a symlink could lead out of the repository.
pub fn repo_text(root: &std::path::Path, rel: &str) -> Option<String> {
    let p = root.join(rel);
    let meta = std::fs::symlink_metadata(&p).ok()?;
    if !meta.is_file() || meta.len() > crate::index::MAX_FILE_BYTES || !std::fs::canonicalize(&p).ok()?.starts_with(root) {
        return None;
    }
    crate::index::decode_source(&std::fs::read(&p).ok()?).map(|t| t.into_owned())
}

/// The segments of a module path below the repository's own package, when the checkout is that
/// package (`networkx.utils` inside `networkx/` -> `utils`). Any other leading name is a package
/// from outside, even when the rest matches a local file.
pub fn below_own_package<'s>(root: &std::path::Path, segs: &'s [&'s str]) -> Option<&'s [&'s str]> {
    let own = root.file_name()?.to_str()?;
    let first = *segs.first()?;
    (segs.len() > 1 && (first == own || first == own.replace('-', "_"))).then(|| &segs[1..])
}

fn dir_of(p: &str) -> &str {
    p.rsplit_once('/').map(|(d, _)| d).unwrap_or("")
}

fn join(base: &str, rel: &str) -> String {
    let mut parts: Vec<&str> = if base.is_empty() { Vec::new() } else { base.split('/').collect() };
    for seg in rel.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    parts.join("/")
}

/// `Foo<'a, T>.new` -> `Foo.new`: impl blocks carry their generics in the qualified name.
fn strip_generics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.replace(' ', "")
}

fn is_ident(s: &str) -> bool {
    !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$')
}

const CONTAINER_KINDS: &[&str] = &["class", "impl", "trait", "struct", "interface", "enum"];
const SCOPE_KINDS: &[&str] = &["function", "method", "module"];
const FUNCTION_NODES: &[&str] = &[
    "function_definition",
    "lambda",
    "function_declaration",
    "generator_function_declaration",
    "function_expression",
    "function",
    "arrow_function",
    "method_definition",
    "function_item",
    "closure_expression",
    "method_declaration",
    "func_literal",
];
/// (parent kind, field holding the bound name; "" = any child) of local variable and parameter bindings
const LOCAL_BINDERS: &[(&str, &str)] = &[
    ("parameters", ""),
    ("lambda_parameters", ""),
    ("formal_parameters", ""),
    ("closure_parameters", ""),
    ("typed_parameter", ""),
    ("list_splat_pattern", ""),
    ("dictionary_splat_pattern", ""),
    ("pattern_list", ""),
    ("tuple_pattern", ""),
    ("array_pattern", ""),
    ("default_parameter", "name"),
    ("typed_default_parameter", "name"),
    ("assignment", "left"),
    ("for_statement", "left"),
    ("for_in_clause", "left"),
    ("for_in_statement", "left"),
    ("as_pattern_target", ""),
    ("assignment_pattern", "left"),
    ("required_parameter", "pattern"),
    ("optional_parameter", "pattern"),
    ("variable_declarator", "name"),
    ("let_declaration", "pattern"),
    ("parameter", "pattern"),
    ("for_expression", "pattern"),
    ("parameter_declaration", "name"),
    ("variadic_parameter_declaration", "name"),
    ("var_spec", "name"),
    ("const_spec", "name"),
];

fn is_local_binder(n: Node) -> bool {
    let Some(p) = n.parent() else { return false };
    if p.kind() == "expression_list" {
        // Go `a, b := ...` and `for k, v := range`
        return p
            .parent()
            .is_some_and(|g| matches!(g.kind(), "short_var_declaration" | "range_clause") && g.child_by_field_name("left").map(|l| l.id()) == Some(p.id()));
    }
    LOCAL_BINDERS.iter().any(|(k, f)| *k == p.kind() && (f.is_empty() || p.child_by_field_name(f).map(|c| c.id()) == Some(n.id())))
}

/// The receiver or path in front of a member name (`obj` of `obj.N`, `a::b` of `a::b::N`).
fn qualifier(n: Node) -> Option<Node> {
    let p = n.parent()?;
    const PAIRS: &[(&str, &str)] =
        &[("object", "attribute"), ("object", "property"), ("path", "name"), ("value", "field"), ("operand", "field"), ("package", "name"), ("module", "name")];
    for (obj, mem) in PAIRS {
        if p.child_by_field_name(mem).map(|c| c.id()) == Some(n.id()) {
            if let Some(o) = p.child_by_field_name(obj) {
                return Some(o);
            }
        }
    }
    None
}

/// What the binder `b` of a variable is declared with: (its type annotation, its value).
fn declared(b: Node) -> (Option<Node>, Option<Node>) {
    let Some(p) = b.parent() else { return (None, None) };
    if p.kind() == "expression_list" {
        // Go `a, b := x, y`: the value at the binder's place
        let mut cur = p.walk();
        let place = p.named_children(&mut cur).position(|c| c.id() == b.id());
        let value = place.and_then(|i| p.parent()?.child_by_field_name("right")?.named_child(i as u32));
        return (None, value);
    }
    let value = match p.kind() {
        "assignment" => p.child_by_field_name("right"),
        "variable_declarator" | "let_declaration" | "static_item" | "const_item" => p.child_by_field_name("value"),
        "var_spec" | "const_spec" => p.child_by_field_name("value").and_then(|v| if v.kind() == "expression_list" { v.named_child(0) } else { Some(v) }),
        _ => None,
    };
    (p.child_by_field_name("type"), value)
}

/// What the resolver knows of the type of a value.
#[derive(Clone, Debug)]
enum Ty {
    /// a class, struct or other concrete type defined in the repository
    Local(i64),
    /// a concrete type defined outside it (`Vec`, `str`, `list`), with its type arguments
    Foreign(String, Vec<Option<Ty>>),
}

/// Standard Rust types whose methods a call reaches through `Deref` to the type they hold.
const RUST_DEREF: &[&str] = &["Box", "Rc", "Arc", "Ref", "RefMut", "Cow", "MutexGuard", "RwLockReadGuard", "RwLockWriteGuard", "Pin", "ManuallyDrop"];

/// Whether `name` is a type parameter of an item around `n` (`fn f<T>`, `impl<T>`, `class C<T>`).
fn type_parameter(n: Node, name: &str, src: &str) -> bool {
    let mut a = Some(n);
    while let Some(x) = a {
        let mut cur = x.walk();
        let found = x.named_children(&mut cur).any(|c| {
            matches!(c.kind(), "type_parameters" | "type_parameter_list")
                && text(c, src).split(|ch: char| !(ch.is_alphanumeric() || ch == '_')).any(|w| w == name)
        });
        if found {
            return true;
        }
        a = x.parent();
    }
    false
}

/// The identifier that names the type in a type expression, past references, pointers and
/// generic arguments; the last name of a path (`a.b.T`, `a::T`, `pkg.T`).
fn type_name(mut t: Node) -> Option<Node> {
    loop {
        t = match t.kind() {
            "identifier" | "type_identifier" => return Some(t),
            "attribute" => t.child_by_field_name("attribute")?,
            "member_expression" => t.child_by_field_name("property")?,
            "scoped_identifier" | "scoped_type_identifier" | "qualified_type" | "nested_type_identifier" => t.child_by_field_name("name")?,
            "generic_type" => t.child_by_field_name("type").or_else(|| t.child_by_field_name("name"))?,
            "reference_type" => t.child_by_field_name("type")?,
            "type" | "type_annotation" | "pointer_type" | "parenthesized_type" => t.named_child(0)?,
            _ => return None,
        };
    }
}

fn string_content(n: Node, src: &str) -> String {
    text(n, src).trim_matches(|c| c == '"' || c == '\'' || c == '`').to_string()
}

// ------------------------------------------------------------------------- import bindings
fn bindings_of(lang: Lang, root: Node, src: &str) -> Vec<Binding> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        let handled = match lang {
            Lang::Python => py_bindings(n, src, &mut out),
            Lang::Js => js_bindings(n, src, &mut out),
            Lang::Rust => {
                if n.kind() == "use_declaration" {
                    if let Some(a) = n.child_by_field_name("argument") {
                        rust_use(a, "", src, &mut out);
                    }
                    true
                } else {
                    false
                }
            }
            Lang::Go => go_bindings(n, src, &mut out),
        };
        if !handled {
            let mut cur = n.walk();
            let kids: Vec<Node> = n.named_children(&mut cur).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    out
}

fn py_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    match n.kind() {
        "import_statement" => {
            let mut cur = n.walk();
            for ch in n.children_by_field_name("name", &mut cur) {
                if ch.kind() == "aliased_import" {
                    let (Some(name), Some(alias)) = (ch.child_by_field_name("name"), ch.child_by_field_name("alias")) else { continue };
                    out.push(Binding { local: text(alias, src).into(), module: text(name, src).into(), item: None, sites: vec![alias.start_byte()] });
                } else if let Some(first) = ch.named_child(0) {
                    // `import a.b.c` binds `a`
                    let head = text(first, src).to_string();
                    out.push(Binding { local: head.clone(), module: head, item: None, sites: vec![first.start_byte()] });
                }
            }
            true
        }
        "import_from_statement" => {
            let Some(m) = n.child_by_field_name("module_name") else { return true };
            let module: String = text(m, src).chars().filter(|c| !c.is_whitespace()).collect();
            let mut cur = n.walk();
            for ch in n.named_children(&mut cur) {
                if ch.kind() == "wildcard_import" {
                    out.push(Binding { local: "*".into(), module: module.clone(), item: None, sites: vec![] });
                }
            }
            let mut cur = n.walk();
            for ch in n.children_by_field_name("name", &mut cur) {
                let (name, alias) =
                    if ch.kind() == "aliased_import" { (ch.child_by_field_name("name"), ch.child_by_field_name("alias")) } else { (Some(ch), None) };
                let Some(name) = name else { continue };
                let item = text(name, src).to_string();
                let mut sites = vec![name.start_byte()];
                sites.extend(alias.map(|a| a.start_byte()));
                out.push(Binding {
                    local: alias.map(|a| text(a, src).to_string()).unwrap_or_else(|| item.clone()),
                    module: module.clone(),
                    item: Some(item),
                    sites,
                });
            }
            true
        }
        _ => false,
    }
}

fn js_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    match n.kind() {
        "import_statement" => {
            let Some(source) = n.child_by_field_name("source") else { return true };
            let module = string_content(source, src);
            let mut cur = n.walk();
            for clause in n.named_children(&mut cur) {
                let mut c2 = clause.walk();
                let parts: Vec<Node> = if clause.kind() == "import_clause" { clause.named_children(&mut c2).collect() } else { vec![] };
                for p in parts {
                    match p.kind() {
                        "identifier" => {
                            out.push(Binding { local: text(p, src).into(), module: module.clone(), item: Some("default".into()), sites: vec![p.start_byte()] })
                        }
                        "namespace_import" => {
                            if let Some(id) = p.named_child(0) {
                                out.push(Binding { local: text(id, src).into(), module: module.clone(), item: None, sites: vec![id.start_byte()] });
                            }
                        }
                        "named_imports" => js_specifiers(p, &module, src, out),
                        _ => {}
                    }
                }
            }
            true
        }
        "export_statement" => {
            let module = n.child_by_field_name("source").map(|s| string_content(s, src));
            let mut cur = n.walk();
            let kids: Vec<Node> = n.children(&mut cur).collect();
            if kids.iter().any(|k| k.kind() == "default") {
                let target = n
                    .child_by_field_name("declaration")
                    .and_then(|d| d.child_by_field_name("name"))
                    .or_else(|| n.child_by_field_name("value").filter(|v| v.kind() == "identifier"));
                if let Some(t) = target {
                    out.push(Binding { local: "default".into(), module: String::new(), item: Some(text(t, src).into()), sites: vec![] });
                }
                return false; // the declaration inside is still walked
            }
            let clause = kids.iter().find(|k| k.kind() == "export_clause");
            match (clause, &module) {
                (Some(c), m) => js_specifiers(*c, m.as_deref().unwrap_or(""), src, out),
                (None, Some(m)) => match kids.iter().find(|k| k.kind() == "namespace_export").and_then(|k| k.named_child(0)) {
                    Some(id) => out.push(Binding { local: text(id, src).into(), module: m.clone(), item: None, sites: vec![id.start_byte()] }),
                    None => out.push(Binding { local: "*".into(), module: m.clone(), item: None, sites: vec![] }),
                },
                (None, None) => return false,
            }
            true
        }
        "variable_declarator" => {
            // CommonJS: `const x = require('./m')`, `const {a, b: c} = require('./m')`
            let (Some(name), Some(value)) = (n.child_by_field_name("name"), n.child_by_field_name("value")) else { return false };
            if value.kind() != "call_expression" || value.child_by_field_name("function").map(|f| text(f, src)) != Some("require") {
                return false;
            }
            let Some(arg) = value.child_by_field_name("arguments").and_then(|a| a.named_child(0)).filter(|a| a.kind() == "string") else { return false };
            let module = string_content(arg, src);
            match name.kind() {
                "identifier" => out.push(Binding { local: text(name, src).into(), module, item: None, sites: vec![name.start_byte()] }),
                "object_pattern" => {
                    let mut cur = name.walk();
                    for p in name.named_children(&mut cur) {
                        match p.kind() {
                            "shorthand_property_identifier_pattern" => out.push(Binding {
                                local: text(p, src).into(),
                                module: module.clone(),
                                item: Some(text(p, src).into()),
                                sites: vec![p.start_byte()],
                            }),
                            "pair_pattern" => {
                                if let (Some(k), Some(v)) = (p.child_by_field_name("key"), p.child_by_field_name("value")) {
                                    out.push(Binding {
                                        local: text(v, src).into(),
                                        module: module.clone(),
                                        item: Some(text(k, src).into()),
                                        sites: vec![k.start_byte(), v.start_byte()],
                                    });
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            true
        }
        _ => false,
    }
}

fn js_specifiers(list: Node, module: &str, src: &str, out: &mut Vec<Binding>) {
    let mut cur = list.walk();
    for s in list.named_children(&mut cur) {
        let Some(name) = s.child_by_field_name("name") else { continue };
        let alias = s.child_by_field_name("alias");
        let item = text(name, src).to_string();
        let mut sites = vec![name.start_byte()];
        sites.extend(alias.map(|a| a.start_byte()));
        out.push(Binding {
            local: alias.map(|a| text(a, src).to_string()).unwrap_or_else(|| item.clone()),
            module: module.to_string(),
            item: Some(item),
            sites,
        });
    }
}

fn rust_push(full: &str, local: &str, sites: Vec<usize>, out: &mut Vec<Binding>) {
    let (module, item) = full.rsplit_once("::").unwrap_or(("", full));
    out.push(Binding { local: local.to_string(), module: module.to_string(), item: Some(item.to_string()), sites });
}

fn rust_use(n: Node, prefix: &str, src: &str, out: &mut Vec<Binding>) {
    let cat = |p: &str, t: &str| {
        if p.is_empty() {
            t.to_string()
        } else if t.is_empty() {
            p.to_string()
        } else {
            format!("{p}::{t}")
        }
    };
    match n.kind() {
        "self" => {
            let last = prefix.rsplit("::").next().unwrap_or(prefix);
            rust_push(prefix, last, vec![n.start_byte()], out);
        }
        "identifier" | "crate" | "super" | "metavariable" => rust_push(&cat(prefix, text(n, src)), text(n, src), vec![n.start_byte()], out),
        "scoped_identifier" => {
            let name = n.child_by_field_name("name");
            let local = name.map(|x| text(x, src)).unwrap_or("");
            rust_push(&cat(prefix, text(n, src)), local, name.map(|x| vec![x.start_byte()]).unwrap_or_default(), out);
        }
        "use_as_clause" => {
            let (Some(path), Some(alias)) = (n.child_by_field_name("path"), n.child_by_field_name("alias")) else { return };
            let mut sites = vec![alias.start_byte()];
            sites.extend(path.child_by_field_name("name").or(Some(path)).map(|x| x.start_byte()));
            rust_push(&cat(prefix, text(path, src)), text(alias, src), sites, out);
        }
        "scoped_use_list" => {
            let p = n.child_by_field_name("path").map(|x| text(x, src)).unwrap_or("");
            if let Some(list) = n.child_by_field_name("list") {
                rust_use(list, &cat(prefix, p), src, out);
            }
        }
        "use_list" => {
            let mut cur = n.walk();
            for ch in n.named_children(&mut cur) {
                rust_use(ch, prefix, src, out);
            }
        }
        "use_wildcard" => {
            let p = n.named_child(0).map(|x| text(x, src)).unwrap_or("");
            out.push(Binding { local: "*".into(), module: cat(prefix, p), item: None, sites: vec![] });
        }
        _ => {}
    }
}

fn go_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    if n.kind() != "import_spec" {
        return false;
    }
    let Some(path) = n.child_by_field_name("path") else { return true };
    let module = string_content(path, src);
    let segs: Vec<&str> = module.split('/').collect();
    // `example.com/x/v2` is package `x`
    let mut last = *segs.last().unwrap_or(&"");
    if segs.len() > 1 && last.len() > 1 && last.starts_with('v') && last[1..].chars().all(|c| c.is_ascii_digit()) {
        last = segs[segs.len() - 2];
    }
    match n.child_by_field_name("name") {
        Some(a) if a.kind() == "dot" => out.push(Binding { local: "*".into(), module, item: None, sites: vec![] }),
        Some(a) if a.kind() == "blank_identifier" => {}
        Some(a) => out.push(Binding { local: text(a, src).into(), module, item: None, sites: vec![a.start_byte()] }),
        None => out.push(Binding { local: last.to_string(), module, item: None, sites: vec![] }),
    }
    true
}

// ------------------------------------------------------------------------- resolver
impl<'a> Resolver<'a> {
    pub fn new(ix: &'a Index) -> Result<Resolver<'a>> {
        let mut st = ix.db.prepare_cached("SELECT path FROM files WHERE hash != 'SKIP'")?;
        let files = st.query_map([], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<_>>()?;
        Ok(Resolver {
            ix,
            files,
            cache: HashMap::new(),
            parsers: HashMap::new(),
            crates: None,
            go_dirs: None,
            go_imports: None,
            ts: HashMap::new(),
            nest: 0,
            looked_up: HashSet::new(),
        })
    }

    fn file(&mut self, path: &str) -> Rc<FileInfo> {
        if let Some(f) = self.cache.get(path) {
            return Rc::clone(f);
        }
        let fi = Rc::new(self.load(path));
        self.cache.insert(path.to_string(), Rc::clone(&fi));
        fi
    }

    fn load(&mut self, path: &str) -> FileInfo {
        let src = repo_text(&self.ix.root, path).unwrap_or_default();
        let spec = crate::parse::spec_for(path, &src);
        let lang = spec.and_then(|s| lang_of(s.id));
        let tree = match (spec, lang) {
            (Some(spec), Some(_)) => {
                let p = self.parsers.entry(spec.id).or_insert_with(|| {
                    let mut p = Parser::new();
                    let _ = p.set_language(&(spec.language)());
                    p
                });
                p.parse(src.as_bytes(), None)
            }
            _ => None,
        };
        let bindings = match (lang, &tree) {
            (Some(l), Some(t)) => bindings_of(l, t.root_node(), &src),
            _ => Vec::new(),
        };
        let defs = self.defs_of(path).unwrap_or_default();
        FileInfo { lang, src, tree, defs, bindings }
    }

    fn defs_of(&self, path: &str) -> Result<Vec<Def>> {
        let stem = path.rsplit('/').next().unwrap_or(path);
        let stem = stem.split_once('.').map(|(s, _)| s).unwrap_or(stem);
        let mut st = self
            .ix
            .db
            .prepare_cached("SELECT id, name, qualname, kind, start, end FROM chunks WHERE path=?1 AND kind != 'window' ORDER BY start, end DESC, id")?;
        let rows: Vec<(i64, String, String, String, i64, i64)> = st
            .query_map([path], |r| {
                Ok((
                    r.get(0)?,
                    r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                    r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<rusqlite::Result<_>>()?;
        let mut defs: Vec<Def> = Vec::new();
        let mut stack: Vec<usize> = Vec::new();
        for (i, (id, name, qualname, kind, start, end)) in rows.into_iter().enumerate() {
            // the header chunk (docstring and imports) is named after the file, not defined in it
            if i == 0 && kind == "module" && start == 1 && name == stem {
                continue;
            }
            while stack.last().is_some_and(|&t| defs[t].end < start) {
                stack.pop();
            }
            let parent = stack.last().copied().filter(|&t| defs[t].start <= start && end <= defs[t].end);
            defs.push(Def { id, name, qualname, kind, start, end, parent });
            stack.push(defs.len() - 1);
        }
        Ok(defs)
    }

    /// Resolve the occurrence of `name` at 1-based `line`, 0-based byte `col` of `path`.
    pub fn resolve_at(&mut self, path: &str, line: u64, col: usize, name: &str) -> Resolution {
        self.looked_up.clear();
        let fi = self.file(path);
        let (Some(lang), Some(tree)) = (fi.lang, fi.tree.as_ref()) else { return Err("no grammar") };
        let row = line.saturating_sub(1) as usize;
        let Some(node) = tree.root_node().descendant_for_point_range(Point { row, column: col }, Point { row, column: col + name.len() }) else {
            return Err("not found");
        };
        // identifiers inside a string are code only in interpolations (f-strings, template literals)
        let mut a = Some(node);
        while let Some(n) = a {
            if n.kind().contains("comment") || (n.kind().contains("string") && !node.kind().ends_with("identifier")) {
                return Err("comment or string");
            }
            a = n.parent();
        }
        if text(node, &fi.src) != name {
            return Err("not an identifier");
        }
        let line = line as i64;
        if let Some(b) = fi.bindings.iter().find(|b| b.sites.contains(&node.start_byte())) {
            return match self.resolve_binding(path, lang, b, 0) {
                Some(id) => Ok((id, "import")),
                None => Err(self.binding_failure(path, lang, b)),
            };
        }
        if let Some(p) = node.parent() {
            let spec = crate::parse::spec_for(path, &fi.src);
            let is_def_node = spec.is_some_and(|s| s.defs.iter().any(|d| d.node == p.kind()));
            if is_def_node && p.child_by_field_name("name").map(|c| c.id()) == Some(node.id()) {
                if let Some(d) = fi.defs.iter().filter(|d| d.name == name && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start) {
                    return Ok((d.id, "definition"));
                }
            }
            if p.kind() == "keyword_argument" && p.child_by_field_name("name").map(|c| c.id()) == Some(node.id()) {
                return Err("keyword argument");
            }
        }
        if let Some(q) = qualifier(node) {
            return self.resolve_qualified(path, &fi, lang, q, node, name, line);
        }
        if matches!(node.kind(), "property_identifier" | "field_identifier" | "shorthand_property_identifier") {
            return Err("field or key");
        }
        self.resolve_bare(path, &fi, lang, node, name, line)
    }

    fn binding_failure(&mut self, path: &str, lang: Lang, b: &Binding) -> &'static str {
        if b.module.is_empty() || self.module_files(path, lang, &b.module).is_some() {
            "not found"
        } else {
            "external"
        }
    }

    fn resolve_bare(&mut self, path: &str, fi: &FileInfo, lang: Lang, node: Node, name: &str, line: i64) -> Resolution {
        // enclosing scopes, innermost first; a class body is a scope only for code directly in it
        let mut chain: Vec<usize> = fi.defs.iter().enumerate().filter(|(_, d)| d.start <= line && line <= d.end).map(|(i, _)| i).collect();
        chain.sort_by_key(|&i| fi.defs[i].end - fi.defs[i].start);
        for (k, &s) in chain.iter().enumerate() {
            let kind = fi.defs[s].kind.as_str();
            if SCOPE_KINDS.contains(&kind) || (k == 0 && CONTAINER_KINDS.contains(&kind)) {
                if let Some(d) = fi.defs.iter().find(|d| d.name == name && d.parent == Some(s) && d.kind != "impl") {
                    return Ok((d.id, "local"));
                }
            }
        }
        if shadowed(lang, node, name, &fi.src) {
            return Err("local variable");
        }
        if let Some(d) = top_level(fi, name) {
            return Ok((d.id, "module"));
        }
        if let Some(b) = fi.bindings.iter().find(|b| b.local == name).cloned() {
            return match self.resolve_binding(path, lang, &b, 0) {
                Some(id) => Ok((id, "import")),
                None => Err(self.binding_failure(path, lang, &b)),
            };
        }
        if lang == Lang::Go {
            let dir = dir_of(path).to_string();
            for f in self.go_dir(&dir) {
                if f != path {
                    if let Some(d) = top_level(&self.file(&f), name) {
                        return Ok((d.id, "package"));
                    }
                }
            }
        }
        for b in fi.bindings.iter().filter(|b| b.local == "*") {
            if let Some(files) = self.module_files(path, lang, &b.module) {
                if let Some(id) = self.lookup_top(&files, name, 1) {
                    return Ok((id, "import"));
                }
            }
        }
        Err("not found")
    }

    #[allow(clippy::too_many_arguments)]
    fn resolve_qualified(&mut self, path: &str, fi: &FileInfo, lang: Lang, q: Node, node: Node, name: &str, line: i64) -> Resolution {
        let qt = text(q, &fi.src).trim();
        let receiver = match lang {
            Lang::Python => qt == "self" || qt == "cls",
            Lang::Js => qt == "this",
            Lang::Rust => qt == "Self" || (qt == "self" && node.parent().is_some_and(|p| p.kind() == "field_expression")),
            Lang::Go => false,
        };
        if receiver {
            let Some(c) =
                fi.defs.iter().filter(|d| CONTAINER_KINDS.contains(&d.kind.as_str()) && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start)
            else {
                return Err("receiver");
            };
            return self.member(c.id, name, 0).map(|id| (id, "member")).ok_or("not found");
        }
        let sep = if lang == Lang::Rust { "::" } else { "." };
        let stripped = strip_generics(qt);
        let segs: Vec<&str> = stripped.split(sep).collect();
        if segs.iter().any(|s| !is_ident(s)) {
            return self.through_type(path, fi, lang, q, name);
        }
        for k in (1..=segs.len()).rev() {
            let Some(files) = self.module_of_chain(path, fi, lang, &segs[..k]) else { continue };
            let mut cur: Option<i64> = None;
            for seg in &segs[k..] {
                cur = match cur {
                    None => self.lookup_top(&files, seg, 0),
                    Some(id) => self.member(id, seg, 0),
                };
                if cur.is_none() {
                    return Err("not found");
                }
            }
            let hit = match cur {
                None => self.lookup_top(&files, name, 0),
                Some(id) => self.member(id, name, 0),
            };
            return hit.map(|id| (id, "qualified")).ok_or("not found");
        }
        // `Class.N`, `Type::N`: the head names a definition
        let head = if q.kind() == "identifier" || q.named_child_count() == 0 { q } else { first_leaf(q) };
        let Ok((mut cur, _)) = self.resolve_bare(path, fi, lang, head, segs[0], line) else {
            let imported = fi.bindings.iter().any(|b| b.local == segs[0]) || matches!(segs[0], "std" | "core" | "alloc");
            if imported {
                return Err("external");
            }
            // `x.N` with `x` a variable: the type its declaration states, when it states one
            return self.through_type(path, fi, lang, q, name);
        };
        let head_def = cur;
        for seg in segs[1..].iter().copied().chain(std::iter::once(name)) {
            match self.member(cur, seg, 0) {
                Some(id) => cur = id,
                // a module-level variable is a definition too: its type has the member
                None if cur == head_def && !self.def_kind(cur).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str())) => {
                    return self.through_type(path, fi, lang, q, name)
                }
                None => return Err("not found"),
            }
        }
        Ok((cur, "qualified"))
    }

    /// `q.name` decided by the type of `q`: a member of a repository type, or nothing of the
    /// repository when the type is from outside and no definition of the repository extends it.
    fn through_type(&mut self, path: &str, fi: &FileInfo, lang: Lang, q: Node, name: &str) -> Resolution {
        match self.type_of(path, fi, lang, q) {
            Some(Ty::Local(owner)) => self.member(owner, name, 0).map(|id| (id, "variable")).ok_or("not found"),
            Some(Ty::Foreign(t, _)) if !self.extended(&t, name) => Err("external"),
            _ => Err("receiver"),
        }
    }

    /// Whether the repository defines `name` as a member of something named `ty` (a trait
    /// implemented for a type from outside, a prototype extension).
    fn extended(&self, ty: &str, name: &str) -> bool {
        let Ok(mut st) = self.ix.db.prepare_cached("SELECT qualname FROM chunks WHERE name_key=?1 AND name=?2") else { return true };
        let Ok(rows) = st.query_map(params![name_key(name), name], |r| r.get::<_, Option<String>>(0)) else { return true };
        let found = rows
            .flatten()
            .flatten()
            .any(|q| q.rsplit_once('.').is_some_and(|(owner, _)| owner.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == ty)));
        found
    }

    fn def_kind(&self, id: i64) -> Option<String> {
        self.ix.db.query_row("SELECT kind FROM chunks WHERE id=?1", [id], |r| r.get(0)).ok()
    }

    /// The type of the value expression `e` of `path`, when declarations say it: a variable's
    /// annotation or value, a call's declared return type, a field's declared type, a
    /// constructor. None when they do not.
    fn type_of(&mut self, path: &str, fi: &FileInfo, lang: Lang, e: Node) -> Option<Ty> {
        if self.nest > 32 {
            return None;
        }
        self.nest += 1;
        let t = self.type_of_inner(path, fi, lang, e);
        self.nest -= 1;
        t
    }

    fn type_of_inner(&mut self, path: &str, fi: &FileInfo, lang: Lang, e: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let line = e.start_position().row as i64 + 1;
        match e.kind() {
            "self" | "this" => self.container_at(fi, line),
            "identifier" if matches!((lang, text(e, src)), (Lang::Python, "self" | "cls") | (Lang::Rust, "self")) => self.container_at(fi, line),
            "identifier" => self.variable_ty(path, fi, lang, e),
            "parenthesized_expression" | "reference_expression" | "non_null_expression" | "await" | "unary_expression" => {
                self.type_of(path, fi, lang, e.child_by_field_name("operand").or_else(|| e.named_child(0))?)
            }
            "await_expression" => match self.type_of(path, fi, lang, e.named_child(0)?)? {
                Ty::Foreign(n, args) if lang == Lang::Js && n == "Promise" => args.into_iter().next().flatten(),
                t => Some(t),
            },
            // Rust `x?`: the value inside a `Result` or an `Option`
            "try_expression" => match self.type_of(path, fi, lang, e.named_child(0)?)? {
                Ty::Foreign(n, args) if n == "Result" || n == "Option" => args.into_iter().next().flatten(),
                _ => None,
            },
            "new_expression" => self.named_ty(path, fi, lang, e.child_by_field_name("constructor")?),
            "struct_expression" => self.named_ty(path, fi, lang, e.child_by_field_name("name")?),
            "composite_literal" => self.ty_of_type(path, fi, lang, e.child_by_field_name("type")?),
            "call" | "call_expression" => {
                let f = e.child_by_field_name("function")?;
                // `a.m()`, `pkg.f()`, `T::f()`, `f()`: what the callee resolves to says what it returns
                let leaf = match member_parts(f) {
                    Some((_, m)) => m,
                    None if f.kind() == "scoped_identifier" => f.child_by_field_name("name")?,
                    None if f.kind() == "identifier" => f,
                    None => return None,
                };
                let found = match qualifier(leaf) {
                    Some(q) => self.resolve_qualified(path, fi, lang, q, leaf, text(leaf, src), line),
                    None => self.resolve_bare(path, fi, lang, leaf, text(leaf, src), line),
                };
                match found {
                    Ok((id, _)) => self.def_ty(id),
                    // `Type::new()` of a type from outside builds that type
                    Err(_) if f.kind() == "scoped_identifier" && matches!(text(leaf, src), "new" | "default" | "with_capacity" | "from") => {
                        self.named_ty(path, fi, lang, f.child_by_field_name("path")?)
                    }
                    Err(_) => None,
                }
            }
            _ => {
                let (obj, field) = member_parts(e)?;
                match self.type_of(path, fi, lang, obj)? {
                    Ty::Local(owner) => self.field_ty(owner, text(field, src)),
                    Ty::Foreign(..) => None,
                }
            }
        }
    }

    /// The innermost container around `line`, as the type of `self`.
    fn container_at(&self, fi: &FileInfo, line: i64) -> Option<Ty> {
        let c = fi.defs.iter().filter(|d| CONTAINER_KINDS.contains(&d.kind.as_str()) && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start)?;
        Some(Ty::Local(c.id))
    }

    /// What a value of definition `id` is: an instance of a container, or the declared return
    /// value of a function.
    fn def_ty(&mut self, id: i64) -> Option<Ty> {
        let kind = self.def_kind(id)?;
        if CONTAINER_KINDS.contains(&kind.as_str()) || kind == "type" {
            return concrete(&kind).then_some(Ty::Local(id));
        }
        let (path, _) = self.def_row(id)?;
        let fi = self.file(&path);
        let lang = fi.lang?;
        let node = def_node(&fi, id)?;
        let ret = node.child_by_field_name("return_type").or_else(|| node.child_by_field_name("result"))?;
        self.ty_of_type(&path, &fi, lang, ret)
    }

    /// The declared type of the field `name` of container `owner`: a Rust or Go struct field, a
    /// class field with a type or a value, a Python attribute set on `self` in a method.
    fn field_ty(&mut self, owner: i64, name: &str) -> Option<Ty> {
        let (path, _) = self.def_row(owner)?;
        let fi = self.file(&path);
        let lang = fi.lang?;
        let d = fi.defs.iter().find(|d| d.id == owner)?;
        // the fields of a Rust type are on its struct, not on an impl block
        let id = if d.kind == "impl" { top_level(&fi, &strip_generics(&d.name)).filter(|s| s.kind != "impl")?.id } else { owner };
        let node = def_node(&fi, id)?;
        let src = fi.src.as_str();
        let mut stack = vec![node];
        while let Some(n) = stack.pop() {
            let named = |f: &str| n.child_by_field_name(f).is_some_and(|x| text(x, src) == name);
            let hit = match n.kind() {
                "field_declaration" if named("name") => Some((n.child_by_field_name("type"), None)),
                "public_field_definition" | "field_definition" if named("name") || named("property") => {
                    Some((n.child_by_field_name("type"), n.child_by_field_name("value")))
                }
                "assignment" => {
                    let left = n.child_by_field_name("left");
                    let on_self = left.is_some_and(|l| {
                        l.kind() == "attribute"
                            && l.child_by_field_name("object").is_some_and(|o| text(o, src) == "self")
                            && l.child_by_field_name("attribute").is_some_and(|a| text(a, src) == name)
                    });
                    // a class-level `name: T`, not a local of a method
                    let in_body = left.is_some_and(|l| l.kind() == "identifier" && text(l, src) == name) && !has_function_between(n, node);
                    (on_self || in_body).then(|| (n.child_by_field_name("type"), n.child_by_field_name("right")))
                }
                _ => None,
            };
            if let Some((ty, value)) = hit {
                if let Some(t) = ty.and_then(|t| self.ty_of_type(&path, &fi, lang, t)) {
                    return Some(t);
                }
                if let Some(t) = value.and_then(|v| self.type_of(&path, &fi, lang, v)) {
                    return Some(t);
                }
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        None
    }

    /// The type of the variable `at` names: its nearest declaration before the use in the
    /// function around it, else a declaration at the top level of the file.
    fn variable_ty(&mut self, path: &str, fi: &FileInfo, lang: Lang, at: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let name = text(at, src);
        let mut scope = at;
        while let Some(p) = scope.parent() {
            scope = p;
            if FUNCTION_NODES.contains(&p.kind()) {
                break;
            }
        }
        let binds = |n: Node| {
            n.kind() == "identifier"
                && text(n, src) == name
                && (is_local_binder(n) || n.parent().is_some_and(|p| matches!(p.kind(), "static_item" | "const_item")))
        };
        let (mut binder, mut stack) = (None::<Node>, vec![scope]);
        while let Some(n) = stack.pop() {
            if n.start_byte() >= at.start_byte() {
                continue;
            }
            if binds(n) && binder.is_none_or(|b| b.start_byte() < n.start_byte()) {
                binder = Some(n);
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        let root = fi.tree.as_ref()?.root_node();
        if binder.is_none() && scope.id() != root.id() {
            // a module-level or static variable, declared anywhere at the top of the file
            let mut stack = vec![root];
            while let Some(n) = stack.pop() {
                if binds(n) {
                    binder = Some(n);
                    break;
                }
                if n.id() == root.id() || !(FUNCTION_NODES.contains(&n.kind()) || CLASS_NODES.contains(&n.kind())) {
                    let mut cur = n.walk();
                    stack.extend(n.named_children(&mut cur));
                }
            }
        }
        let (ty, value) = declared(binder?);
        if let Some(t) = ty {
            return self.ty_of_type(path, fi, lang, t);
        }
        self.type_of(path, fi, lang, value?)
    }

    /// The type a type expression names, past references, pointers, `Box`-like holders and
    /// `Optional[..]`; None for a trait object, a union, a type parameter or anything unclear.
    fn ty_of_type(&mut self, path: &str, fi: &FileInfo, lang: Lang, t: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        match t.kind() {
            "reference_type" => self.ty_of_type(path, fi, lang, t.child_by_field_name("type")?),
            "type" | "type_annotation" | "parenthesized_type" | "pointer_type" | "readonly_type" => self.ty_of_type(path, fi, lang, t.named_child(0)?),
            "primitive_type" | "predefined_type" => Some(Ty::Foreign(text(t, src).to_string(), Vec::new())),
            "array_type" | "slice_type" | "map_type" | "string_type" => Some(Ty::Foreign(t.kind().to_string(), Vec::new())),
            "generic_type" | "subscript" => {
                let base = t
                    .child_by_field_name("type")
                    .or_else(|| t.child_by_field_name("name"))
                    .or_else(|| t.child_by_field_name("value"))
                    .or_else(|| t.named_child(0))?;
                let mut args = Vec::new();
                let mut cur = t.walk();
                for c in t.named_children(&mut cur).filter(|c| c.id() != base.id()) {
                    let mut c2 = c.walk();
                    let list: Vec<Node> = if matches!(c.kind(), "type_arguments" | "type_parameter") { c.named_children(&mut c2).collect() } else { vec![c] };
                    for a in list {
                        args.push(self.ty_of_type(path, fi, lang, a));
                    }
                }
                match self.named_ty(path, fi, lang, base)? {
                    Ty::Foreign(n, _) if (lang == Lang::Rust && RUST_DEREF.contains(&n.as_str())) || (lang == Lang::Python && n == "Optional") => {
                        args.into_iter().next().flatten()
                    }
                    Ty::Foreign(n, _) => Some(Ty::Foreign(n, args)),
                    local => Some(local),
                }
            }
            "type_identifier"
            | "identifier"
            | "scoped_type_identifier"
            | "scoped_identifier"
            | "qualified_type"
            | "nested_type_identifier"
            | "attribute"
            | "member_expression" => self.named_ty(path, fi, lang, t),
            _ => None,
        }
    }

    /// The type a name of a type stands for.
    fn named_ty(&mut self, path: &str, fi: &FileInfo, lang: Lang, n: Node) -> Option<Ty> {
        let leaf = type_name(n)?;
        let src = fi.src.as_str();
        let name = text(leaf, src);
        let line = leaf.start_position().row as i64 + 1;
        if lang == Lang::Rust && name == "Self" {
            return self.container_at(fi, line);
        }
        if type_parameter(leaf, name, src) {
            return None;
        }
        let q = qualifier(leaf);
        let found = match q {
            Some(q) => self.resolve_qualified(path, fi, lang, q, leaf, name, line),
            None => self.resolve_bare(path, fi, lang, leaf, name, line),
        };
        match found {
            Ok((id, _)) => {
                let kind = self.def_kind(id)?;
                concrete(&kind).then_some(Ty::Local(id))
            }
            Err("external") => Some(Ty::Foreign(name.to_string(), Vec::new())),
            // a path that does not resolve (`Self::Item`, `T::Output`) says nothing
            Err(_) if q.is_some() => None,
            // a type of no file of the repository: the language's own (`Vec`, `str`, `list`)
            Err(_) if !self.defined_anywhere(name) && !(lang == Lang::Python && top_level_binder(fi, name)) => Some(Ty::Foreign(name.to_string(), Vec::new())),
            Err(_) => None,
        }
    }

    fn defined_anywhere(&self, name: &str) -> bool {
        let kinds = CONTAINER_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
        self.ix
            .db
            .query_row(
                &format!("SELECT EXISTS(SELECT 1 FROM chunks WHERE name_key=?1 AND name=?2 AND kind IN ({kinds}, 'type'))"),
                params![name_key(name), name],
                |r| r.get(0),
            )
            .unwrap_or(true)
    }

    fn def_row(&self, id: i64) -> Option<(String, String)> {
        self.ix
            .db
            .query_row("SELECT path, qualname FROM chunks WHERE id=?1", [id], |r| Ok((r.get(0)?, r.get::<_, Option<String>>(1)?.unwrap_or_default())))
            .ok()
    }

    /// `name` as a member of the container definition `id` (methods, nested classes), base classes included.
    fn member(&mut self, id: i64, name: &str, depth: usize) -> Option<i64> {
        if self.nest > 32 {
            return None;
        }
        self.nest += 1;
        let r = self.member_inner(id, name, depth);
        self.nest -= 1;
        r
    }

    fn member_inner(&mut self, id: i64, name: &str, depth: usize) -> Option<i64> {
        let (path, qualname) = self.def_row(id)?;
        let want = format!("{}.{name}", strip_generics(&qualname));
        let fi = self.file(&path);
        if let Some(d) = fi.defs.iter().find(|d| d.name == name && strip_generics(&d.qualname) == want) {
            return Some(d.id);
        }
        // Rust impl blocks may live in another file of the crate
        if fi.lang == Some(Lang::Rust) {
            let mut st = self.ix.db.prepare_cached("SELECT id, path, qualname FROM chunks WHERE name_key=?1 AND name=?2").ok()?;
            let rows: Vec<(i64, String, String)> = st
                .query_map(params![name_key(name), name], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<String>>(2)?.unwrap_or_default())))
                .ok()?
                .flatten()
                .collect();
            let common = |p: &str| p.split('/').zip(path.split('/')).take_while(|(a, b)| a == b).count();
            return rows.into_iter().filter(|(_, _, q)| strip_generics(q) == want).max_by_key(|(_, p, _)| common(p)).map(|(id, _, _)| id);
        }
        // a Go method is declared outside its type, in any file of the package: its receiver names the type
        if fi.lang == Some(Lang::Go) {
            let owner = strip_generics(&qualname);
            for f in self.go_dir(dir_of(&path)) {
                let gf = self.file(&f);
                let Some(tree) = gf.tree.as_ref() else { continue };
                let root = tree.root_node();
                let mut cur = root.walk();
                for m in root.named_children(&mut cur).filter(|n| n.kind() == "method_declaration") {
                    let receiver =
                        m.child_by_field_name("receiver").and_then(|r| r.named_child(0)).and_then(|r| r.child_by_field_name("type")).and_then(type_name);
                    if m.child_by_field_name("name").map(|n| text(n, &gf.src)) == Some(name) && receiver.map(|r| text(r, &gf.src)) == Some(owner.as_str()) {
                        let line = m.start_position().row as i64 + 1;
                        if let Some(d) = gf.defs.iter().find(|d| d.name == name && d.start <= line && line <= d.end) {
                            return Some(d.id);
                        }
                    }
                }
            }
            return None;
        }
        if depth >= 3 {
            return None;
        }
        // Python and JS base classes
        let lang = fi.lang?;
        let tree = fi.tree.as_ref()?;
        let d = fi.defs.iter().find(|d| d.id == id)?;
        let mut bases: Vec<Node> = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            let (s, e) = (n.start_position().row as i64 + 1, n.end_position().row as i64 + 1);
            if e < d.start || s > d.end {
                continue;
            }
            if matches!(n.kind(), "class_definition" | "class_declaration" | "class")
                && n.child_by_field_name("name").map(|x| text(x, &fi.src)) == Some(d.name.as_str())
            {
                let mut cur = n.walk();
                for ch in n.children(&mut cur) {
                    if matches!(ch.kind(), "argument_list" | "class_heritage") {
                        let mut c2 = ch.walk();
                        bases.extend(ch.named_children(&mut c2).filter(|b| matches!(b.kind(), "identifier" | "attribute" | "member_expression")));
                    }
                }
                break;
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        for b in bases {
            let base = if b.kind() == "identifier" {
                self.resolve_bare(&path, &fi, lang, b, text(b, &fi.src), d.start).ok().map(|x| x.0)
            } else {
                let leaf = b.child_by_field_name("attribute").or_else(|| b.child_by_field_name("property"));
                match (leaf, b.child_by_field_name("object")) {
                    (Some(leaf), Some(obj)) => self.resolve_qualified(&path, &fi, lang, obj, leaf, text(leaf, &fi.src), d.start).ok().map(|x| x.0),
                    _ => None,
                }
            };
            if let Some(bid) = base.filter(|&x| x != id) {
                if let Some(m) = self.member(bid, name, depth + 1) {
                    return Some(m);
                }
            }
        }
        None
    }

    /// Definition `name` at the top level of one of `files`, following re-exports and wildcard imports.
    fn lookup_top(&mut self, files: &[String], name: &str, depth: usize) -> Option<i64> {
        // a lookup that comes round again (wildcard import rings) found nothing the first time
        if self.nest > 32 || !self.looked_up.insert((files.join("\n"), name.to_string())) {
            return None;
        }
        self.nest += 1;
        let r = self.lookup_top_inner(files, name, depth);
        self.nest -= 1;
        r
    }

    fn lookup_top_inner(&mut self, files: &[String], name: &str, depth: usize) -> Option<i64> {
        if depth > 6 {
            return None;
        }
        for f in files {
            if let Some(d) = top_level(&self.file(f), name) {
                return Some(d.id);
            }
        }
        for f in files {
            let fi = self.file(f);
            let Some(lang) = fi.lang else { continue };
            for b in fi.bindings.iter().filter(|b| b.local == name) {
                if let Some(id) = self.resolve_binding(f, lang, b, depth + 1) {
                    return Some(id);
                }
            }
        }
        for f in files {
            let fi = self.file(f);
            let Some(lang) = fi.lang else { continue };
            for b in fi.bindings.iter().filter(|b| b.local == "*") {
                if let Some(mf) = self.module_files(f, lang, &b.module) {
                    if mf.iter().all(|m| files.contains(m)) {
                        continue;
                    }
                    if let Some(id) = self.lookup_top(&mf, name, depth + 1) {
                        return Some(id);
                    }
                }
            }
        }
        None
    }

    fn resolve_binding(&mut self, from: &str, lang: Lang, b: &Binding, depth: usize) -> Option<i64> {
        let item = b.item.as_deref()?;
        if b.module.is_empty() && b.local == item {
            return None; // `export {a}` names the file's own `a`, already looked up
        }
        let files = if b.module.is_empty() { vec![from.to_string()] } else { self.module_files(from, lang, &b.module)? };
        self.lookup_top(&files, item, depth + 1)
    }

    /// Files of the module a qualifier chain (`nx.utils`, `crate::a`, `pkg`) names, if it is one.
    fn module_of_chain(&mut self, path: &str, fi: &FileInfo, lang: Lang, segs: &[&str]) -> Option<Vec<String>> {
        let b = fi.bindings.iter().find(|b| b.local == segs[0]);
        match lang {
            Lang::Python => {
                let b = b?;
                let mut m = match &b.item {
                    Some(it) if b.module.ends_with('.') => format!("{}{it}", b.module),
                    Some(it) => format!("{}.{it}", b.module),
                    None => b.module.clone(),
                };
                for s in &segs[1..] {
                    m = format!("{m}.{s}");
                }
                self.module_files(path, lang, &m)
            }
            Lang::Js | Lang::Go => {
                let b = b?;
                if b.item.is_some() || segs.len() > 1 {
                    return None;
                }
                self.module_files(path, lang, &b.module)
            }
            Lang::Rust => {
                let mut full: Vec<String> = Vec::new();
                match b {
                    Some(b) if !matches!(segs[0], "crate" | "self" | "super") => {
                        full.extend(b.module.split("::").filter(|s| !s.is_empty()).map(String::from));
                        full.extend(b.item.clone());
                    }
                    _ => full.push(segs[0].to_string()),
                }
                full.extend(segs[1..].iter().map(|s| s.to_string()));
                self.rust_module(path, &full.iter().map(|s| s.as_str()).collect::<Vec<_>>())
            }
        }
    }

    fn module_files(&mut self, from: &str, lang: Lang, module: &str) -> Option<Vec<String>> {
        match lang {
            Lang::Python => self.py_module(from, module).map(|f| vec![f]),
            Lang::Js => self.js_module(from, module).map(|f| vec![f]),
            Lang::Rust => {
                let segs: Vec<&str> = module.split("::").filter(|s| !s.is_empty()).collect();
                self.rust_module(from, &segs)
            }
            Lang::Go => {
                let best = if self.go_imports().is_empty() {
                    // no go.mod: a GOPATH tree, where the import path ends with the directory
                    self.go_dirs()
                        .keys()
                        .filter(|d| !d.is_empty() && (module == d.as_str() || module.ends_with(&format!("/{d}"))))
                        .max_by_key(|d| d.len())?
                        .clone()
                } else {
                    self.go_imports().get(module)?.clone()
                };
                Some(self.go_dir(&best))
            }
        }
    }

    fn first_file(&self, cands: &[String]) -> Option<String> {
        cands.iter().find(|c| self.files.contains(c.as_str())).cloned()
    }

    fn py_module(&self, from: &str, module: &str) -> Option<String> {
        let try_base =
            |base: &str| self.first_file(&[format!("{base}.py"), format!("{base}/__init__.py"), format!("{base}.pyi"), format!("{base}/__init__.pyi")]);
        if module.starts_with('.') {
            let ups = module.chars().take_while(|c| *c == '.').count();
            let mut base = dir_of(from).to_string();
            for _ in 1..ups {
                base = dir_of(&base).to_string();
            }
            let target = join(&base, &module[ups..].replace('.', "/"));
            return if target.is_empty() { self.first_file(&["__init__.py".to_string()]) } else { try_base(&target) };
        }
        let slashed = module.replace('.', "/");
        if let Some(f) = try_base(&slashed).or_else(|| try_base(&format!("src/{slashed}"))) {
            return Some(f);
        }
        let segs: Vec<&str> = slashed.split('/').collect();
        below_own_package(&self.ix.root, &segs).and_then(|rest| try_base(&rest.join("/")))
    }

    fn js_module(&mut self, from: &str, module: &str) -> Option<String> {
        if module.starts_with("./") || module.starts_with("../") || module == "." || module == ".." {
            return self.js_file(&join(dir_of(from), module));
        }
        // a name for the project's own files (`paths`, `baseUrl`); packages live outside the repository
        let ts = self.ts_paths(from)?;
        // the pattern with the longest text before its `*` wins, as in the TypeScript compiler
        let mut aliases: Vec<&(String, Vec<String>)> = ts.aliases.iter().collect();
        aliases.sort_by_key(|(pattern, _)| std::cmp::Reverse(pattern.split('*').next().map_or(0, str::len)));
        for (pattern, targets) in aliases {
            let star = match pattern.split_once('*') {
                Some((before, after)) => match module.strip_prefix(before).and_then(|m| m.strip_suffix(after)) {
                    Some(star) => star,
                    None => continue,
                },
                None if pattern == module => "",
                None => continue,
            };
            if let Some(f) = targets.iter().find_map(|t| self.js_file(&t.replace('*', star))) {
                return Some(f);
            }
        }
        ts.base.as_ref().and_then(|base| self.js_file(&join(base, module)))
    }

    /// The repository files the import statement of `from` at `line`, recorded as `module`, names:
    /// the files `refs` would look its names up in. None when the resolver does not read the
    /// language of `from`; an empty list when it reads it and finds no file.
    pub fn import_files(&mut self, from: &str, line: i64, module: &str) -> Option<Vec<String>> {
        let fi = self.file(from);
        let lang = fi.lang?;
        let mut out: Vec<String> = Vec::new();
        match lang {
            Lang::Js => out.extend(self.js_module(from, module)),
            Lang::Go => out.extend(self.module_files(from, lang, module).unwrap_or_default()),
            Lang::Python | Lang::Rust => {
                let kinds: &[&str] = if lang == Lang::Python { &["import_from_statement"] } else { &["use_declaration"] };
                let range = statement_at(&fi, line, kinds);
                let named = fi.bindings.iter().filter(|b| range.as_ref().is_some_and(|r| b.sites.first().is_some_and(|s| r.contains(s))));
                for b in named {
                    let found = match (lang, &b.item) {
                        // `from m import x` names the submodule `m.x` when there is one, else `m`
                        (Lang::Python, Some(item)) => {
                            let sub = if b.module.ends_with('.') { format!("{}{item}", b.module) } else { format!("{}.{item}", b.module) };
                            self.py_module(from, &sub).or_else(|| self.py_module(from, &b.module))
                        }
                        (Lang::Python, None) => None,
                        _ => {
                            let full = match &b.item {
                                Some(item) if b.module.is_empty() => item.clone(),
                                Some(item) => format!("{}::{item}", b.module),
                                None => b.module.clone(),
                            };
                            let segs: Vec<&str> = full.split("::").filter(|s| !s.is_empty()).collect();
                            // the longest prefix that is a module: `crate::a::thing` is in `crate::a`
                            (1..=segs.len()).rev().find_map(|n| self.rust_module(from, &segs[..n])).and_then(|v| v.into_iter().next())
                        }
                    };
                    out.extend(found);
                }
                if out.is_empty() {
                    out.extend(match lang {
                        Lang::Python => self.py_module(from, module),
                        _ => self.module_files(from, lang, module).and_then(|v| v.into_iter().next()),
                    });
                }
            }
        }
        out.retain(|f| f != from);
        let mut seen = HashSet::new();
        out.retain(|f| seen.insert(f.clone()));
        Some(out)
    }

    /// The `mod name;` declarations of a Rust file: (line, name, the file of that module).
    pub fn rust_mods(&mut self, from: &str) -> Vec<(i64, String, Option<String>)> {
        let fi = self.file(from);
        let (Some(Lang::Rust), Some(tree)) = (fi.lang, &fi.tree) else { return Vec::new() };
        let mut out = Vec::new();
        let mut stack = vec![(tree.root_node(), Self::rust_mod_dir(from))];
        while let Some((n, dir)) = stack.pop() {
            let mut cur = n.walk();
            for ch in n.named_children(&mut cur) {
                if ch.kind() != "mod_item" {
                    continue;
                }
                let Some(name) = ch.child_by_field_name("name").map(|x| text(x, &fi.src).to_string()) else { continue };
                let sub = join(&dir, &name);
                match ch.child_by_field_name("body") {
                    Some(body) => stack.push((body, sub)),
                    None => out.push((ch.start_position().row as i64 + 1, name, self.rust_mod_file(&sub))),
                }
            }
        }
        out.sort();
        out
    }

    /// The Rust files that may declare `mod` for `path`: the module files of its parent directory.
    pub fn rust_mod_parents(&self, path: &str) -> Vec<String> {
        let d = dir_of(&Self::rust_mod_dir(path)).to_string();
        let mut cands = vec![format!("{d}/mod.rs"), format!("{d}/lib.rs"), format!("{d}/main.rs"), format!("{d}.rs")];
        if d.is_empty() {
            cands = vec!["lib.rs".into(), "main.rs".into()];
        }
        cands.retain(|c| self.files.contains(c) && c != path);
        cands
    }

    /// The file an import path without its extension stands for.
    fn js_file(&self, base: &str) -> Option<String> {
        if self.files.contains(base) && !base.ends_with(".js") {
            return Some(base.to_string());
        }
        // TypeScript sources are imported with the `.js` name they compile to
        let stem = base.strip_suffix(".js").or_else(|| base.strip_suffix(".mjs")).unwrap_or(base);
        let mut cands: Vec<String> = [".ts", ".tsx", ".d.ts", ".js", ".jsx", ".mjs", ".cjs", ".mts", ".cts"].iter().map(|e| format!("{stem}{e}")).collect();
        cands.push(base.to_string());
        cands.extend(["index.ts", "index.tsx", "index.js", "index.jsx", "index.mjs"].iter().map(|i| join(base, i)));
        self.first_file(&cands)
    }

    /// The import names of the nearest `tsconfig.json` or `jsconfig.json` above `from` that sets any.
    fn ts_paths(&mut self, from: &str) -> Option<Rc<TsPaths>> {
        let mut dir = dir_of(from).to_string();
        loop {
            if !self.ts.contains_key(&dir) {
                let found = ["tsconfig.json", "jsconfig.json"].iter().find_map(|name| ts_config(&self.ix.root, &join(&dir, name), 0)).map(Rc::new);
                self.ts.insert(dir.clone(), found);
            }
            if let Some(found) = &self.ts[&dir] {
                return Some(Rc::clone(found));
            }
            if dir.is_empty() {
                return None;
            }
            dir = dir_of(&dir).to_string();
        }
    }

    fn rust_mod_file(&self, dir: &str) -> Option<String> {
        if dir.is_empty() {
            return self.first_file(&["lib.rs".into(), "main.rs".into()]);
        }
        self.first_file(&[format!("{dir}.rs"), format!("{dir}/mod.rs"), format!("{dir}/lib.rs"), format!("{dir}/main.rs")])
    }

    /// Directory that stands for the module of a Rust file (`a/b.rs` and `a/b/mod.rs` -> `a/b`).
    fn rust_mod_dir(path: &str) -> String {
        let base = path.rsplit('/').next().unwrap_or(path);
        if matches!(base, "mod.rs" | "lib.rs" | "main.rs") {
            dir_of(path).to_string()
        } else {
            join(dir_of(path), base.trim_end_matches(".rs"))
        }
    }

    fn rust_crate_root(&self, path: &str) -> Option<String> {
        let mut d = dir_of(path).to_string();
        loop {
            let c = |f: &str| if d.is_empty() { f.to_string() } else { format!("{d}/{f}") };
            if self.files.contains(&c("lib.rs")) || self.files.contains(&c("main.rs")) {
                return Some(d);
            }
            if d.is_empty() {
                return None;
            }
            d = dir_of(&d).to_string();
        }
    }

    /// Workspace crates by their `use` name (`grep-matcher` -> `grep_matcher`) -> crate root directory.
    fn crates(&mut self) -> &HashMap<String, String> {
        if self.crates.is_none() {
            let mut m = HashMap::new();
            let mut roots: Vec<&String> = self.files.iter().filter(|f| f.ends_with("/lib.rs") || *f == "lib.rs").collect();
            roots.sort();
            for f in roots {
                let dir = dir_of(f);
                let pkg = if dir == "src" { "" } else { dir.strip_suffix("/src").unwrap_or(dir) };
                let toml = if pkg.is_empty() { "Cargo.toml".to_string() } else { format!("{pkg}/Cargo.toml") };
                let Some(t) = repo_text(&self.ix.root, &toml) else { continue };
                if let Some(name) = cargo_package_name(&t) {
                    m.entry(name.replace('-', "_")).or_insert_with(|| dir.to_string());
                }
            }
            self.crates = Some(m);
        }
        self.crates.as_ref().unwrap()
    }

    fn rust_module(&mut self, from: &str, segs: &[&str]) -> Option<Vec<String>> {
        let first = *segs.first()?;
        let (mut dir, mut i) = match first {
            "crate" => (self.rust_crate_root(from)?, 1),
            "self" => (Self::rust_mod_dir(from), 1),
            "super" => {
                let mut d = Self::rust_mod_dir(from);
                let mut i = 0;
                while segs.get(i) == Some(&"super") {
                    d = dir_of(&d).to_string();
                    i += 1;
                }
                (d, i)
            }
            other => {
                if let Some(d) = self.crates().get(other).cloned() {
                    (d, 1)
                } else if self.rust_mod_file(&join(&Self::rust_mod_dir(from), other)).is_some() {
                    (Self::rust_mod_dir(from), 0)
                } else {
                    (self.rust_crate_root(from)?, 0) // 2015-edition paths start at the crate root
                }
            }
        };
        while i < segs.len() {
            dir = join(&dir, segs[i]);
            self.rust_mod_file(&dir)?;
            i += 1;
        }
        self.rust_mod_file(&dir).map(|f| vec![f])
    }

    fn go_dirs(&mut self) -> &HashMap<String, Vec<String>> {
        if self.go_dirs.is_none() {
            let mut m: HashMap<String, Vec<String>> = HashMap::new();
            for f in self.files.iter().filter(|f| f.ends_with(".go")) {
                m.entry(dir_of(f).to_string()).or_default().push(f.clone());
            }
            for v in m.values_mut() {
                v.sort();
            }
            self.go_dirs = Some(m);
        }
        self.go_dirs.as_ref().unwrap()
    }

    /// Import path -> directory, for the directories under a `go.mod` (the nearest one above).
    fn go_imports(&mut self) -> &HashMap<String, String> {
        if self.go_imports.is_none() {
            let dirs: Vec<String> = self.go_dirs().keys().cloned().collect();
            let mut declared: HashMap<String, Option<String>> = HashMap::new();
            let mut m = HashMap::new();
            for d in dirs {
                let mut at = d.clone();
                loop {
                    let module = declared.entry(at.clone()).or_insert_with(|| repo_text(&self.ix.root, &join(&at, "go.mod")).and_then(|t| go_module_path(&t)));
                    if let Some(module) = module {
                        let rest = d[at.len()..].trim_start_matches('/');
                        m.insert(if rest.is_empty() { module.clone() } else { format!("{module}/{rest}") }, d.clone());
                        break;
                    }
                    if at.is_empty() {
                        break;
                    }
                    at = dir_of(&at).to_string();
                }
            }
            self.go_imports = Some(m);
        }
        self.go_imports.as_ref().unwrap()
    }

    fn go_dir(&mut self, dir: &str) -> Vec<String> {
        self.go_dirs().get(dir).cloned().unwrap_or_default()
    }
}

/// `baseUrl` and `paths` of the config at `path`; what it does not set comes from the config it
/// extends by a relative path. None when neither is set anywhere.
fn ts_config(root: &std::path::Path, path: &str, depth: usize) -> Option<TsPaths> {
    let text = repo_text(root, path)?;
    let v: serde_json::Value = serde_json::from_str(&plain_json(&text)).ok()?;
    let (dir, options) = (dir_of(path), &v["compilerOptions"]);
    let mut out = TsPaths { base: options["baseUrl"].as_str().map(|b| join(dir, b)), aliases: Vec::new() };
    // the places are relative to `baseUrl`, or to the config when it sets none
    let at = out.base.clone().unwrap_or_else(|| dir.to_string());
    for (pattern, places) in options["paths"].as_object().into_iter().flatten() {
        let places = places.as_array().into_iter().flatten().filter_map(|p| p.as_str()).map(|p| join(&at, p)).collect();
        out.aliases.push((pattern.clone(), places));
    }
    if let Some(parent) = v["extends"].as_str().filter(|e| e.starts_with('.') && depth < 4) {
        let parent = join(dir, parent);
        let parent = if parent.ends_with(".json") { parent } else { format!("{parent}.json") };
        if let Some(inherited) = ts_config(root, &parent, depth + 1) {
            out.base = out.base.or(inherited.base);
            if out.aliases.is_empty() {
                out.aliases = inherited.aliases;
            }
        }
    }
    (out.base.is_some() || !out.aliases.is_empty()).then_some(out)
}

/// JSON with the comments and trailing commas a tsconfig may have taken out.
fn plain_json(text: &str) -> String {
    let (mut out, mut chars) = (String::with_capacity(text.len()), text.chars().peekable());
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                out.push(c);
                while let Some(d) = chars.next() {
                    out.push(d);
                    match d {
                        '\\' => out.extend(chars.next()),
                        '"' => break,
                        _ => {}
                    }
                }
            }
            '/' if chars.peek() == Some(&'/') => {
                chars.by_ref().find(|&d| d == '\n');
                out.push('\n');
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut last = ' ';
                for d in chars.by_ref() {
                    if last == '*' && d == '/' {
                        break;
                    }
                    last = d;
                }
            }
            '}' | ']' => {
                // a comma before a closing bracket is the trailing one
                let kept = out.trim_end().len();
                if out[..kept].ends_with(',') {
                    out.truncate(kept - 1);
                }
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Byte range of the statement of one of `kinds` that starts on (1-based) `line`.
fn statement_at(fi: &FileInfo, line: i64, kinds: &[&str]) -> Option<std::ops::Range<usize>> {
    let row = usize::try_from(line - 1).ok()?;
    let text_line = fi.src.lines().nth(row)?;
    let column = text_line.len() - text_line.trim_start().len();
    let at = Point { row, column };
    let mut n = fi.tree.as_ref()?.root_node().named_descendant_for_point_range(at, at)?;
    loop {
        if kinds.contains(&n.kind()) && n.start_position().row == row {
            return Some(n.byte_range());
        }
        n = n.parent()?;
    }
}

fn first_leaf(mut n: Node) -> Node {
    while let Some(c) = n.named_child(0) {
        n = c;
    }
    n
}

/// (object, member name) of a member access: `a.b`, `a.b` in Rust, `pkg.F` in Go.
fn member_parts(e: Node) -> Option<(Node, Node)> {
    let (obj, mem) = match e.kind() {
        "attribute" => ("object", "attribute"),
        "member_expression" => ("object", "property"),
        "field_expression" => ("value", "field"),
        "selector_expression" => ("operand", "field"),
        _ => return None,
    };
    Some((e.child_by_field_name(obj)?, e.child_by_field_name(mem)?))
}

/// A kind of definition whose values have its members: not a trait or an interface.
fn concrete(kind: &str) -> bool {
    (CONTAINER_KINDS.contains(&kind) || kind == "type") && !matches!(kind, "trait" | "interface")
}

const CLASS_NODES: &[&str] = &["class_definition", "class_declaration", "class", "struct_item", "impl_item", "trait_item"];

/// The syntax node of definition `id` of the file: the first node on its first line that
/// carries its name.
fn def_node(fi: &FileInfo, id: i64) -> Option<Node<'_>> {
    let d = fi.defs.iter().find(|d| d.id == id)?;
    let (first, last) = ((d.start - 1).max(0) as usize, (d.end - 1).max(0) as usize);
    let mut stack = vec![fi.tree.as_ref()?.root_node()];
    while let Some(n) = stack.pop() {
        if n.end_position().row < first || n.start_position().row > last {
            continue;
        }
        let named = n.child_by_field_name("name").is_some_and(|x| text(x, &fi.src) == d.name);
        if named && n.start_position().row >= first {
            return Some(n);
        }
        let mut cur = n.walk();
        let kids: Vec<Node> = n.named_children(&mut cur).collect();
        stack.extend(kids.into_iter().rev());
    }
    None
}

/// Whether a function lies between `n` and its ancestor `top`.
fn has_function_between(n: Node, top: Node) -> bool {
    let mut a = n.parent();
    while let Some(x) = a {
        if x.id() == top.id() {
            return false;
        }
        if FUNCTION_NODES.contains(&x.kind()) {
            return true;
        }
        a = x.parent();
    }
    false
}

/// Whether the file assigns `name` at its top level (a Python `T = TypeVar("T")` or alias).
fn top_level_binder(fi: &FileInfo, name: &str) -> bool {
    let Some(tree) = fi.tree.as_ref() else { return false };
    let root = tree.root_node();
    let mut cur = root.walk();
    let found = root.named_children(&mut cur).any(|s| {
        s.kind() == "expression_statement"
            && s.named_child(0).is_some_and(|a| a.kind() == "assignment" && a.child_by_field_name("left").is_some_and(|l| text(l, &fi.src) == name))
    });
    found
}

fn top_level<'f>(fi: &'f FileInfo, name: &str) -> Option<&'f Def> {
    let mut it = fi.defs.iter().filter(|d| d.name == name && d.parent.is_none());
    it.clone().find(|d| d.kind != "impl").or_else(|| it.next())
}

/// A parameter or local variable named `name` in the function around `node` hides outer definitions;
/// one bound in a nested function, block or comprehension that does not hold `node` does not.
fn shadowed(lang: Lang, node: Node, name: &str, src: &str) -> bool {
    let mut f = node.parent();
    while let Some(n) = f {
        if FUNCTION_NODES.contains(&n.kind()) {
            break;
        }
        f = n.parent();
    }
    let Some(func) = f else { return false };
    let block_scoped: &[&str] = match lang {
        Lang::Python => &["list_comprehension", "set_comprehension", "dictionary_comprehension", "generator_expression"],
        _ => &["statement_block", "block", "compound_statement", "for_statement", "for_in_statement", "for_expression"],
    };
    let at = node.start_byte();
    let holds = |n: Node| n.start_byte() <= at && at < n.end_byte();
    // (node, inside a block that does not hold the use): there only a JS `var` reaches the use
    let mut stack = vec![(func, false)];
    while let Some((n, aside)) = stack.pop() {
        if n.kind() == "identifier" && text(n, src) == name && is_local_binder(n) && (!aside || is_js_var(n)) {
            return true;
        }
        let mut cur = n.walk();
        for c in n.named_children(&mut cur) {
            if holds(c) {
                stack.push((c, aside));
            } else if block_scoped.contains(&c.kind()) {
                if lang == Lang::Js {
                    stack.push((c, true));
                }
            } else if !FUNCTION_NODES.contains(&c.kind()) {
                stack.push((c, aside));
            }
        }
    }
    false
}

/// A binder declared with JavaScript `var`, which belongs to the whole function around it.
fn is_js_var(b: Node) -> bool {
    let mut n = b;
    while let Some(p) = n.parent() {
        match p.kind() {
            "variable_declarator" => return p.parent().is_some_and(|d| d.kind() == "variable_declaration"),
            "for_in_statement" => return p.child_by_field_name("kind").is_some_and(|k| k.kind() == "var"),
            "object_pattern" | "array_pattern" | "pair_pattern" | "assignment_pattern" | "rest_pattern" => n = p,
            _ => return false,
        }
    }
    false
}

fn go_module_path(go_mod: &str) -> Option<String> {
    go_mod
        .lines()
        .find_map(|l| l.trim().strip_prefix("module").filter(|r| r.starts_with(char::is_whitespace)))
        .map(|m| m.split("//").next().unwrap_or("").trim().trim_matches('"').to_string())
        .filter(|m| !m.is_empty())
}

fn cargo_package_name(toml: &str) -> Option<String> {
    let mut in_pkg = false;
    for line in toml.lines() {
        let l = line.trim();
        if l.starts_with('[') {
            in_pkg = l == "[package]";
        } else if in_pkg {
            if let Some(v) = l.strip_prefix("name").map(str::trim_start).and_then(|v| v.strip_prefix('=')) {
                return Some(v.trim().trim_matches('"').to_string());
            }
        }
    }
    None
}

// ------------------------------------------------------------------------- refs
pub struct Occurrence {
    pub path: String,
    pub line: u64,
    /// 1-based byte column
    pub column: u64,
    pub text: String,
    pub resolution: Resolution,
}

pub struct Resolved {
    /// definitions named `name`, plus any other definition an occurrence resolved to
    pub definitions: Vec<ChunkRow>,
    pub occurrences: Vec<Occurrence>,
    pub truncated: bool,
}

impl Resolved {
    pub fn of(&self, id: i64) -> impl Iterator<Item = &Occurrence> {
        self.occurrences.iter().filter(move |o| matches!(o.resolution, Ok((d, _)) if d == id))
    }
    pub fn unresolved(&self) -> impl Iterator<Item = &Occurrence> {
        self.occurrences.iter().filter(|o| o.resolution.is_err())
    }
}

/// Every whole-word occurrence of `name` (Engine A), each resolved to the definition it refers to.
pub fn resolved_references(s: &Searcher, name: &str, max_results: u64) -> Result<Resolved> {
    let bare = name.rsplit('.').next().unwrap_or(name);
    let o = crate::instant::Options {
        pattern: bare.to_string(),
        mode: crate::instant::Mode::Literal,
        word: true,
        want_spans: true,
        max_results: Some(max_results),
        ..crate::instant::Options::default()
    };
    let (hits, truncated) = crate::intel::occurrences(&s.ix.root, &o)?;
    let mut rv = Resolver::new(&s.ix)?;
    let mut occurrences = Vec::new();
    for h in hits {
        let line_text = h.text.trim_end_matches(['\n', '\r']).to_string();
        for &(a, _) in &h.spans {
            let resolution = rv.resolve_at(&h.path, h.line, a, bare);
            occurrences.push(Occurrence { path: h.path.to_string(), line: h.line, column: a as u64 + 1, text: line_text.clone(), resolution });
        }
    }
    let mut definitions: Vec<ChunkRow> = crate::intel::definitions(&s.ix, name)?.into_iter().filter(|d| d.name == bare).collect();
    let mut known: HashSet<i64> = definitions.iter().map(|d| d.id).collect();
    for o in &occurrences {
        if let Ok((id, _)) = o.resolution {
            if known.insert(id) {
                definitions.extend(s.ix.chunk(id)?);
            }
        }
    }
    Ok(Resolved { definitions, occurrences, truncated })
}

/// Plain-text rendering of `resolved_references`, optionally for the one definition `only`.
pub fn resolved_text(r: &Resolved, only: Option<i64>) -> String {
    let mut out = String::new();
    for d in r.definitions.iter().filter(|d| only.is_none_or(|id| d.id == id)) {
        let occ: Vec<&Occurrence> = r.of(d.id).collect();
        out.push_str(&format!("{}:{}-{}  [{}] {}  ({} references)\n", shown(&d.path), d.start, d.end, d.kind, shown(&d.qualname), occ.len()));
        for o in occ {
            out.push_str(&format!("    {}:{}:{}: {}\n", shown(&o.path), o.line, o.column, o.text.trim()));
        }
    }
    let unresolved: Vec<&Occurrence> = r.unresolved().collect();
    match only {
        Some(id) => {
            let elsewhere = r.occurrences.len() - unresolved.len() - r.of(id).count();
            out.push_str(&format!("({elsewhere} other occurrences resolve to other definitions, {} are unresolved)\n", unresolved.len()));
        }
        None => {
            out.push_str(&format!("unresolved ({}):\n", unresolved.len()));
            for o in unresolved {
                out.push_str(&format!("    {}:{}:{}: {}  ({})\n", shown(&o.path), o.line, o.column, o.text.trim(), o.resolution.err().unwrap_or("")));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(lang: Lang, id: &str, src: &str) -> Vec<Binding> {
        let spec = crate::langs::by_id(id).unwrap();
        let mut p = Parser::new();
        p.set_language(&(spec.language)()).unwrap();
        let t = p.parse(src.as_bytes(), None).unwrap();
        bindings_of(lang, t.root_node(), src).into_iter().map(|b| Binding { sites: vec![], ..b }).collect()
    }

    fn b(local: &str, module: &str, item: Option<&str>) -> Binding {
        Binding { local: local.into(), module: module.into(), item: item.map(String::from), sites: vec![] }
    }

    fn check(got: &[Binding], want: &[Binding]) {
        for w in want {
            assert!(got.contains(w), "missing {w:?} in {got:?}");
        }
        assert_eq!(got.len(), want.len(), "{got:?}");
    }

    #[test]
    fn python_bindings() {
        let got = parse(Lang::Python, "python", "from . import x\nfrom ..a.b import c as d\nfrom ..a.b import *\nimport a.b as e, f.g\n");
        check(&got, &[b("*", "..a.b", None), b("x", ".", Some("x")), b("d", "..a.b", Some("c")), b("e", "a.b", None), b("f", "f", None)]);
    }

    #[test]
    fn js_bindings_cover_imports_reexports_and_require() {
        let src = "import {a as b, c} from './x';\nimport d, * as ns from './y';\nexport {e as f} from './z';\nexport * from './w';\nexport default foo;\nconst {g, h: i} = require('./v');\nconst j = require('./u');\n";
        let got = parse(Lang::Js, "javascript", src);
        check(
            &got,
            &[
                b("b", "./x", Some("a")),
                b("c", "./x", Some("c")),
                b("d", "./y", Some("default")),
                b("ns", "./y", None),
                b("f", "./z", Some("e")),
                b("*", "./w", None),
                b("default", "", Some("foo")),
                b("g", "./v", Some("g")),
                b("i", "./v", Some("h")),
                b("j", "./u", None),
            ],
        );
    }

    #[test]
    fn rust_use_trees_are_flattened() {
        let got = parse(Lang::Rust, "rust", "use crate::a::{self, b::C as D, e::*};\nuse super::x;\nuse std::fmt;\n");
        check(
            &got,
            &[
                b("a", "crate", Some("a")),
                b("D", "crate::a::b", Some("C")),
                b("*", "crate::a::e", None),
                b("x", "super", Some("x")),
                b("fmt", "std", Some("fmt")),
            ],
        );
    }

    #[test]
    fn go_imports_bind_package_names() {
        let got = parse(Lang::Go, "go", "package p\nimport (\n  m \"example.com/x/y\"\n  . \"fmt\"\n  _ \"embed\"\n  \"example.com/z/v2\"\n  \"os\"\n)\n");
        assert_eq!(got, vec![b("m", "example.com/x/y", None), b("*", "fmt", None), b("z", "example.com/z/v2", None), b("os", "os", None)]);
    }

    /// Index `files` in a fresh directory and resolve `name` where it appears inside `needle` on the
    /// line of `path` containing it; answers "path qualname" of the definition or the failure reason.
    fn indexed(tag: &str, files: &[(&str, &str)]) -> (std::path::PathBuf, Searcher) {
        let root = std::env::temp_dir().join(format!("scrutate_resolve_{tag}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (p, body) in files {
            let f = root.join(p);
            std::fs::create_dir_all(f.parent().unwrap()).unwrap();
            std::fs::write(f, body).unwrap();
        }
        let (s, _) = crate::open_updated(&root, false).unwrap();
        (root, s)
    }

    fn resolver_case(tag: &str, files: &[(&str, &str)], checks: &[(&str, &str, &str, &str)]) {
        let (root, s) = indexed(tag, files);
        let mut rv = Resolver::new(&s.ix).unwrap();
        for (path, needle, name, want) in checks {
            let src = files.iter().find(|f| f.0 == *path).unwrap().1;
            let (row, l) = src.lines().enumerate().find(|(_, l)| l.contains(needle)).unwrap_or_else(|| panic!("{needle:?} not in {path}"));
            let col = l.find(needle).unwrap() + needle.find(name).unwrap();
            let got = match rv.resolve_at(path, row as u64 + 1, col, name) {
                Ok((id, _)) => rv.def_row(id).map(|(p, q)| format!("{p} {q}")).unwrap(),
                Err(e) => e.to_string(),
            };
            assert_eq!(&got, want, "{path}: {needle}");
        }
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn python_rules() {
        let a = "def f():\n    pass\n\n\nclass C:\n    def m(self):\n        return self.n()\n\n    def n(self):\n        pass\n\n\ndef g():\n    def f():\n        pass\n    return f()\n\n\ndef h(f):\n    return f + 1\n\n\n# f in a comment\n";
        let b = "import a\nfrom a import f as ff\nfrom pkg import w\nimport os\n\n\nclass D(a.C):\n    def k(self):\n        return self.m()\n\n\ndef use():\n    ff()\n    a.f()\n    w()\n    os.f()\n";
        resolver_case(
            "py",
            &[("a.py", a), ("b.py", b), ("pkg/__init__.py", "from .impl import *\n"), ("pkg/impl.py", "def w():\n    pass\n")],
            &[
                ("a.py", "f():", "f", "a.py f"),
                ("a.py", "self.n()", "n", "a.py C.n"),
                ("a.py", "return f()", "f", "a.py g.f"),
                ("a.py", "return f + 1", "f", "local variable"),
                ("a.py", "# f in", "f", "comment or string"),
                ("b.py", "import f as ff", "f", "a.py f"),
                ("b.py", "ff()", "ff", "a.py f"),
                ("b.py", "a.f()", "f", "a.py f"),
                ("b.py", "    w()", "w", "pkg/impl.py w"),
                ("b.py", "self.m()", "m", "a.py C.m"),
                ("b.py", "os.f()", "f", "external"),
            ],
        );
    }

    #[test]
    fn js_ts_rules() {
        resolver_case(
            "js",
            &[
                ("x.js", "export function f() {}\n"),
                ("y.js", "export function f() {}\n"),
                ("index.js", "export {f as g} from './y';\n"),
                (
                    "use.js",
                    "import {f} from './x';\nimport {g} from './index';\nimport * as Y from './y';\nconst {f: h} = require('./y');\nf();\ng();\nY.f();\nh();\n",
                ),
                ("m.ts", "export class K {\n  run() { return this.stop(); }\n  stop() {}\n}\n"),
                ("n.ts", "import {K} from './m.js';\nimport React from 'react';\nnew K();\nReact.f();\n"),
            ],
            &[
                ("use.js", "f();", "f", "x.js f"),
                ("use.js", "g();", "g", "y.js f"),
                ("use.js", "Y.f();", "f", "y.js f"),
                ("use.js", "h();", "h", "y.js f"),
                ("n.ts", "new K()", "K", "m.ts K"),
                ("m.ts", "this.stop()", "stop", "m.ts K.stop"),
                ("n.ts", "React.f()", "f", "external"),
            ],
        );
    }

    #[test]
    fn ts_path_aliases_and_base_url() {
        let config = "{\n  // names for our own folders\n  \"compilerOptions\": {\n    \"baseUrl\": \".\",\n    /* longest prefix wins */\n    \"paths\": { \"@lib/*\": [\"src/lib/*\"], \"@lib/deep/*\": [\"src/deep/*\"], \"@cfg\": [\"src/config.ts\"], },\n  },\n}\n";
        resolver_case(
            "tspaths",
            &[
                ("tsconfig.json", config),
                ("src/lib/math.ts", "export function add(a: number, b: number) { return a + b; }\n"),
                ("src/deep/math.ts", "export function mul(a: number, b: number) { return a * b; }\n"),
                ("src/config.ts", "export function load() { return 1; }\n"),
                ("src/app/util.ts", "export function helper() { return 2; }\n"),
                (
                    "src/app/main.ts",
                    "import { add } from \"@lib/math\";\nimport { mul } from \"@lib/deep/math\";\nimport { load } from \"@cfg\";\nimport { helper } from \"src/app/util\";\nimport { x } from \"left-pad\";\n\nexport function run() {\n  return add(load(), mul(helper(), x()));\n}\n",
                ),
                ("tsconfig.base.json", "{\"compilerOptions\": {\"paths\": {\"@shared/*\": [\"packages/shared/src/*\"]}}}\n"),
                ("packages/web/tsconfig.json", "{\"extends\": \"../../tsconfig.base\"}\n"),
                ("packages/shared/src/id.ts", "export function id() { return 3; }\n"),
                ("packages/web/src/page.ts", "import { id } from \"@shared/id\";\n\nexport function page() {\n  return id();\n}\n"),
            ],
            &[
                ("src/app/main.ts", "add(load()", "add", "src/lib/math.ts add"),
                ("src/app/main.ts", "mul(helper()", "mul", "src/deep/math.ts mul"),
                ("src/app/main.ts", "load()", "load", "src/config.ts load"),
                ("src/app/main.ts", "helper()", "helper", "src/app/util.ts helper"),
                ("src/app/main.ts", "x()", "x", "external"),
                ("packages/web/src/page.ts", "return id()", "id", "packages/shared/src/id.ts id"),
            ],
        );
        // the file-level view of the same imports: dependencies and dependents
        let (root, s) = indexed(
            "tsdeps",
            &[
                ("tsconfig.json", config),
                ("src/lib/math.ts", "export function add(a: number, b: number) { return a + b; }\n"),
                (
                    "src/app/main.ts",
                    "import { add } from \"@lib/math\";\nimport { x } from \"left-pad\";\n\nexport function run() {\n  return add(1, x());\n}\n",
                ),
            ],
        );
        let deps = crate::intel::deps(&s.ix, "src/app/main.ts").unwrap();
        let resolved: Vec<(&str, Option<&str>)> = deps.iter().map(|d| (d.module.as_str(), d.resolved.as_deref())).collect();
        assert_eq!(resolved, [("@lib/math", Some("src/lib/math.ts")), ("left-pad", None)]);
        let importers = crate::intel::rdeps(&s.ix, "src/lib/math.ts").unwrap();
        assert_eq!(importers, [("src/app/main.ts".to_string(), 1, "@lib/math".to_string())]);
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
        assert_eq!(plain_json("{\"a\": \"x // y\", /* c */ \"b\": [1, 2,], // d\n}"), "{\"a\": \"x // y\",  \"b\": [1, 2]}");
    }

    #[test]
    fn a_method_call_on_a_variable_follows_the_type_its_declaration_states() {
        resolver_case(
            "vartype_py",
            &[
                ("shapes.py", "class Point:\n    def norm(self):\n        return 1\n\n\nclass Line:\n    def norm(self):\n        return 2\n"),
                (
                    "use.py",
                    "import shapes\nfrom shapes import Point, Line\n\n\ndef built():\n    p = Point()\n    return p.norm()\n\n\ndef annotated(q: Line, r: shapes.Point):\n    return q.norm() + r.norm()\n\n\ndef unknown(s):\n    t = make()\n    return s.norm() + t.norm()\n\n\ndef rebound():\n    v = Point()\n    v = Line()\n    return v.norm()\n",
                ),
            ],
            &[
                ("use.py", "p.norm()", "norm", "shapes.py Point.norm"),
                ("use.py", "q.norm()", "norm", "shapes.py Line.norm"),
                ("use.py", "r.norm()", "norm", "shapes.py Point.norm"),
                ("use.py", "s.norm()", "norm", "receiver"),
                ("use.py", "t.norm()", "norm", "receiver"),
                ("use.py", "v.norm()", "norm", "shapes.py Line.norm"),
            ],
        );
        resolver_case(
            "vartype_ts",
            &[
                ("shapes.ts", "export class Point {\n  norm() { return 1; }\n}\nexport class Line {\n  norm() { return 2; }\n}\n"),
                (
                    "use.ts",
                    "import { Point, Line } from './shapes';\n\nexport function built() {\n  const p = new Point();\n  return p.norm();\n}\n\nexport function annotated(q: Line) {\n  const r: Point = make();\n  return q.norm() + r.norm();\n}\n",
                ),
            ],
            &[("use.ts", "p.norm()", "norm", "shapes.ts Point.norm"), ("use.ts", "q.norm()", "norm", "shapes.ts Line.norm"), ("use.ts", "r.norm()", "norm", "shapes.ts Point.norm")],
        );
        resolver_case(
            "vartype_rs",
            &[
                ("lib.rs", "mod shapes;\nmod user;\n"),
                ("shapes.rs", "pub struct Point;\n\nimpl Point {\n    pub fn new() -> Point {\n        Point\n    }\n    pub fn norm(&self) -> u32 {\n        1\n    }\n}\n\npub struct Line;\n\nimpl Line {\n    pub fn norm(&self) -> u32 {\n        2\n    }\n}\n"),
                (
                    "user.rs",
                    "use crate::shapes::{Line, Point};\n\npub fn built() -> u32 {\n    let p = Point::new();\n    let l = Line {};\n    p.norm() + l.norm()\n}\n\npub fn annotated(q: &Line, r: &mut crate::shapes::Point) -> u32 {\n    q.norm() + r.norm()\n}\n",
                ),
            ],
            &[
                ("user.rs", "p.norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "l.norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "q.norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "r.norm()", "norm", "shapes.rs Point.norm"),
            ],
        );
        resolver_case(
            "vartype_go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("shapes/point.go", "package shapes\n\ntype Point struct{}\n\nfunc (p *Point) Norm() int {\n\treturn 1\n}\n"),
                ("shapes/line.go", "package shapes\n\ntype Line struct{}\n\nfunc (l Line) Norm() int {\n\treturn 2\n}\n"),
                (
                    "main.go",
                    "package main\n\nimport \"example.com/m/shapes\"\n\nfunc built() int {\n\tp := &shapes.Point{}\n\tl := shapes.Line{}\n\treturn p.Norm() + l.Norm()\n}\n\nfunc annotated(q *shapes.Point) int {\n\tvar r shapes.Line\n\treturn q.Norm() + r.Norm()\n}\n",
                ),
            ],
            &[
                ("main.go", "p.Norm()", "Norm", "shapes/point.go Point.Norm"),
                ("main.go", "l.Norm()", "Norm", "shapes/line.go Line.Norm"),
                ("main.go", "q.Norm()", "Norm", "shapes/point.go Point.Norm"),
                ("main.go", "r.Norm()", "Norm", "shapes/line.go Line.Norm"),
            ],
        );
    }

    #[test]
    fn a_receiver_type_comes_from_return_types_fields_and_module_variables() {
        resolver_case(
            "infer_rs",
            &[
                ("lib.rs", "mod shapes;\nmod user;\n"),
                (
                    "shapes.rs",
                    "pub struct Point;\n\nimpl Point {\n    pub fn norm(&self) -> u32 {\n        1\n    }\n    pub fn twin(&self) -> Point {\n        Point\n    }\n}\n\npub struct Line;\n\nimpl Line {\n    pub fn norm(&self) -> u32 {\n        2\n    }\n}\n\npub fn make() -> Point {\n    Point\n}\n\npub fn boxed() -> Box<Line> {\n    Box::new(Line)\n}\n\npub fn tried() -> Result<Line, String> {\n    Ok(Line)\n}\n\npub struct Holder {\n    pub line: Line,\n}\n\nimpl Holder {\n    pub fn get(&self) -> u32 {\n        self.line.norm()\n    }\n    pub fn line(&self) -> &Line {\n        &self.line\n    }\n}\n\npub static ORIGIN: Point = Point;\n\npub fn origin() -> u32 {\n    ORIGIN.norm()\n}\n",
                ),
                (
                    "user.rs",
                    "use crate::shapes::{boxed, make, tried, Holder};\n\npub fn f(h: &Holder) -> u32 {\n    let v = make();\n    v.norm() + make().norm() + make().twin().norm() + h.line().norm() + boxed().norm()\n}\n\npub fn g(r: Result<u32, String>) -> Result<u32, String> {\n    let t = tried()?;\n    Ok(r.ok().unwrap_or(t.norm()))\n}\n\npub fn h<T: Clone>(x: T, s: &str) -> usize {\n    x.clone();\n    s.len()\n}\n",
                ),
            ],
            &[
                ("shapes.rs", "self.line.norm()", "norm", "shapes.rs Line.norm"),
                ("shapes.rs", "ORIGIN.norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "v.norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "+ make().norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "twin().norm()", "norm", "shapes.rs Point.norm"),
                ("user.rs", "h.line().norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "boxed().norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "t.norm()", "norm", "shapes.rs Line.norm"),
                ("user.rs", "r.ok()", "ok", "external"),
                ("user.rs", "s.len()", "len", "external"),
                ("user.rs", "x.clone()", "clone", "receiver"),
            ],
        );
        resolver_case(
            "infer_ts",
            &[
                (
                    "shapes.ts",
                    "export class Point {\n  norm() { return 1; }\n  twin(): Point { return new Point(); }\n}\nexport class Line {\n  norm() { return 2; }\n}\nexport function make(): Point {\n  return new Point();\n}\nexport class Holder {\n  line: Line = new Line();\n  get() { return this.line.norm(); }\n}\nexport const ORIGIN: Point = new Point();\nexport const BUILT = new Line();\nexport function useAll(xs: string[], n: Array<number>) {\n  const v = make();\n  return v.norm() + make().twin().norm() + ORIGIN.norm() + BUILT.norm() + xs.norm() + n.norm();\n}\n",
                ),
            ],
            &[
                ("shapes.ts", "this.line.norm()", "norm", "shapes.ts Line.norm"),
                ("shapes.ts", "v.norm()", "norm", "shapes.ts Point.norm"),
                ("shapes.ts", "twin().norm()", "norm", "shapes.ts Point.norm"),
                ("shapes.ts", "ORIGIN.norm()", "norm", "shapes.ts Point.norm"),
                ("shapes.ts", "BUILT.norm()", "norm", "shapes.ts Line.norm"),
                ("shapes.ts", "xs.norm()", "norm", "external"),
                ("shapes.ts", "n.norm()", "norm", "external"),
            ],
        );
        resolver_case(
            "infer_py",
            &[(
                "shapes.py",
                "class Point:\n    def norm(self):\n        return 1\n\n\nclass Line:\n    def norm(self):\n        return 2\n\n\ndef make() -> Point:\n    return Point()\n\n\nclass Holder:\n    line: Line\n\n    def __init__(self):\n        self.pt = Point()\n\n    def get(self):\n        return self.line.norm() + self.pt.norm()\n\n\nORIGIN = Point()\nTYPED: Line = build()\n\n\ndef use(items: list):\n    v = make()\n    return v.norm() + make().norm() + ORIGIN.norm() + TYPED.norm() + items.norm()\n",
            )],
            &[
                ("shapes.py", "self.line.norm()", "norm", "shapes.py Line.norm"),
                ("shapes.py", "self.pt.norm()", "norm", "shapes.py Point.norm"),
                ("shapes.py", "v.norm()", "norm", "shapes.py Point.norm"),
                ("shapes.py", "make().norm()", "norm", "shapes.py Point.norm"),
                ("shapes.py", "ORIGIN.norm()", "norm", "shapes.py Point.norm"),
                ("shapes.py", "TYPED.norm()", "norm", "shapes.py Line.norm"),
                ("shapes.py", "items.norm()", "norm", "external"),
            ],
        );
        resolver_case(
            "infer_go",
            &[
                ("go.mod", "module example.com/m\n"),
                (
                    "shapes/shapes.go",
                    "package shapes\n\ntype Point struct{}\n\nfunc (p *Point) Norm() int {\n\treturn 1\n}\n\ntype Line struct{}\n\nfunc (l Line) Norm() int {\n\treturn 2\n}\n\nfunc Make() *Point {\n\treturn &Point{}\n}\n\ntype Holder struct {\n\tline Line\n}\n\nfunc (h *Holder) Get() int {\n\treturn h.line.Norm()\n}\n\nvar Origin = Point{}\n\nfunc Use() int {\n\tv := Make()\n\treturn v.Norm() + Make().Norm() + Origin.Norm()\n}\n",
                ),
            ],
            &[
                ("shapes/shapes.go", "h.line.Norm()", "Norm", "shapes/shapes.go Line.Norm"),
                ("shapes/shapes.go", "v.Norm()", "Norm", "shapes/shapes.go Point.Norm"),
                ("shapes/shapes.go", "Make().Norm()", "Norm", "shapes/shapes.go Point.Norm"),
                ("shapes/shapes.go", "Origin.Norm()", "Norm", "shapes/shapes.go Point.Norm"),
            ],
        );
    }

    #[test]
    fn rust_rules() {
        let lib = "mod a;\nmod b;\npub use b::Thing;\n\nfn top() {}\n";
        let a = "pub fn make() {}\n\npub struct Thing;\n\nimpl Thing {\n    pub fn new() -> Self {\n        Self::helper();\n        Thing\n    }\n    fn helper() {}\n}\n";
        let b = "use crate::a;\nuse crate::a::Thing as AThing;\nuse std::fmt;\n\npub fn make() {}\n\npub struct Thing;\n\nfn use_it() {\n    make();\n    a::make();\n    crate::a::make();\n    super::top();\n    let t = AThing::new();\n    fmt::f();\n}\n";
        let c = "use crate::a::*;\nuse crate::Thing;\n\nfn go(x: Thing) {\n    make();\n    x.make();\n}\n";
        resolver_case(
            "rs",
            &[("Cargo.toml", "[package]\nname = \"demo\"\n"), ("src/lib.rs", lib), ("src/a.rs", a), ("src/b.rs", b), ("src/c.rs", c)],
            &[
                ("src/b.rs", "    make();", "make", "src/b.rs make"),
                ("src/b.rs", "    a::make();", "make", "src/a.rs make"),
                ("src/b.rs", "crate::a::make();", "make", "src/a.rs make"),
                ("src/b.rs", "super::top();", "top", "src/lib.rs top"),
                ("src/b.rs", "AThing::new()", "new", "src/a.rs Thing.new"),
                ("src/b.rs", "AThing::new()", "AThing", "src/a.rs Thing"),
                ("src/a.rs", "Self::helper()", "helper", "src/a.rs Thing.helper"),
                ("src/c.rs", "    make();", "make", "src/a.rs make"),
                ("src/c.rs", "x: Thing", "Thing", "src/b.rs Thing"),
                ("src/c.rs", "x.make()", "make", "not found"), // `x` is a `Thing`, which has no `make`
                ("src/b.rs", "::f()", "f", "external"),
            ],
        );
    }

    #[test]
    fn go_rules() {
        resolver_case(
            "go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("p/a.go", "package p\n\nfunc Make() {}\n"),
                ("p/b.go", "package p\n\nfunc use1() { Make() }\n"),
                ("q/a.go", "package q\n\nfunc Make() {}\n"),
                ("main.go", "package main\n\nimport (\n\t\"example.com/m/p\"\n\tqq \"example.com/m/q\"\n\t\"fmt\"\n)\n\nfunc main() { p.Make(); qq.Make(); fmt.Println() }\n"),
            ],
            &[
                ("p/b.go", "{ Make() }", "Make", "p/a.go Make"),
                ("main.go", "p.Make()", "Make", "p/a.go Make"),
                ("main.go", "qq.Make()", "Make", "q/a.go Make"),
                ("main.go", "fmt.Println()", "Println", "external"),
            ],
        );
    }

    #[test]
    fn helpers() {
        assert_eq!(strip_generics("Foo<'a, T>.new"), "Foo.new");
        assert_eq!(join("a/b", "../c/./d"), "a/c/d");
        assert_eq!(Resolver::rust_mod_dir("src/a/b.rs"), "src/a/b");
        assert_eq!(Resolver::rust_mod_dir("src/a/mod.rs"), "src/a");
        assert_eq!(cargo_package_name("[workspace]\nname = \"no\"\n[package]\nname = \"grep-matcher\"\n"), Some("grep-matcher".into()));
    }
}
