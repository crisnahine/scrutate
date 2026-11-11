//! Scope-aware reference resolution: which definition an occurrence of a name refers to.
//!
//! Query time only. Definitions come from the index (`chunks`); scopes, qualifiers and import
//! bindings come from parsing the files that hold occurrences (and the modules they import) again
//! with tree-sitter. Python, JavaScript/TypeScript, Rust, Go and Java are understood; other
//! languages leave their occurrences unresolved.

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
    Java,
}

fn lang_of(spec_id: &str) -> Option<Lang> {
    match spec_id {
        "python" => Some(Lang::Python),
        "javascript" | "typescript" | "tsx" => Some(Lang::Js),
        "rust" => Some(Lang::Rust),
        "go" => Some(Lang::Go),
        "java" => Some(Lang::Java),
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
    /// (files, name) of the `lookup_top` calls made for the current `resolve_at`, and the answer
    /// (None while the lookup runs)
    looked_up: HashMap<(String, String), Option<i64>>,
    /// by definition: the type its values have (`def_ty`)
    def_tys: HashMap<i64, Option<Ty>>,
    /// names a trait or interface of the repository declares as methods
    trait_members: Option<HashSet<String>>,
    /// Java: directory -> its `.java` files, and package -> the directories that declare it
    java_dirs: Option<HashMap<String, Vec<String>>>,
    java_pkgs: Option<HashMap<String, Vec<String>>>,
    /// Java: a dotted name (`a.b.C.m`) -> the definition it names
    java_paths: HashMap<String, std::result::Result<i64, &'static str>>,
    /// Java: by class, the types it extends and implements, and the fields it declares
    java_bases: HashMap<i64, Rc<Vec<std::result::Result<i64, &'static str>>>>,
    java_fields: HashMap<i64, Rc<HashSet<String>>>,
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
    "constructor_declaration",
    "lambda_expression",
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
    ("pair_pattern", "value"),
    ("rest_pattern", ""),
    ("catch_clause", "parameter"),
    ("named_expression", "name"),
    ("splat_pattern", ""),
];

/// Rust patterns whose plain names bind; a capitalized one names a constant or a variant.
const RUST_PATTERNS: &[&str] = &[
    "tuple_struct_pattern",
    "field_pattern",
    "mut_pattern",
    "ref_pattern",
    "reference_pattern",
    "captured_pattern",
    "slice_pattern",
    "or_pattern",
    "match_pattern",
    "let_condition",
];

fn is_local_binder(n: Node, src: &str) -> bool {
    if matches!(n.kind(), "shorthand_property_identifier_pattern" | "shorthand_field_identifier") {
        return true;
    }
    let Some(p) = n.parent() else { return false };
    let is = |f: &str, c: Node| p.child_by_field_name(f).map(|x| x.id()) == Some(c.id());
    match p.kind() {
        // Go `a, b := ...`, `for k, v := range`, `case v := <-ch` and `switch v := x.(type)`
        "expression_list" => {
            return p.parent().is_some_and(|g| {
                let field = if g.kind() == "type_switch_statement" { "alias" } else { "left" };
                matches!(g.kind(), "short_var_declaration" | "range_clause" | "receive_statement" | "type_switch_statement")
                    && g.child_by_field_name(field).map(|l| l.id()) == Some(p.id())
            })
        }
        // a Python `case` capture is one plain name; `case Color.RED` compares with a value
        "dotted_name" => return p.named_child_count() == 1 && p.parent().is_some_and(|g| matches!(g.kind(), "case_pattern" | "keyword_pattern")),
        "as_pattern" => return p.parent().is_some_and(|g| g.kind() == "case_pattern") && n.next_named_sibling().is_none(),
        "tuple" => return p.parent().is_some_and(|g| g.kind() == "as_pattern_target"),
        k if RUST_PATTERNS.contains(&k) => {
            let field = if k == "field_pattern" || k == "let_condition" { "pattern" } else { "" };
            return !text(n, src).starts_with(|c: char| c.is_uppercase()) && !is("type", n) && (field.is_empty() || is(field, n));
        }
        _ => {}
    }
    LOCAL_BINDERS.iter().any(|(k, f)| *k == p.kind() && (f.is_empty() || is(f, n)))
}

/// Whether the binder `b` reaches byte `at`: an `if let`, `while let` or `match` arm binds only
/// in its own branch, a `catch` or Go statement initializer only in that statement, a Python
/// parameter only in the function body, and a `let` or `for` not in its own value.
fn visible(b: Node, at: usize) -> bool {
    if b.start_byte() == at {
        return true;
    }
    let within = |n: Option<Node>| n.is_some_and(|n| n.start_byte() <= at && at < n.end_byte());
    let mut n = b;
    while let Some(p) = n.parent() {
        let k = p.kind();
        if k.ends_with("pattern") || matches!(k, "expression_list" | "let_chain" | "default_parameter" | "typed_default_parameter" | "typed_parameter") {
            n = p;
            continue;
        }
        return match k {
            "let_condition" => {
                let mut c = p;
                while let Some(g) = c.parent().filter(|g| g.kind() == "let_chain") {
                    c = g;
                }
                let branch = c.parent();
                !within(p.child_by_field_name("value")) && within(branch) && !within(branch.and_then(|g| g.child_by_field_name("alternative")))
            }
            "let_declaration" => !within(p.child_by_field_name("value")) && !within(p.child_by_field_name("alternative")),
            "for_expression" => !within(p.child_by_field_name("value")),
            "match_arm" | "catch_clause" | "type_switch_statement" => within(Some(p)),
            // Python evaluates defaults and annotations in the scope around the function
            "parameters" | "lambda_parameters" => within(p.parent().and_then(|f| f.child_by_field_name("body"))),
            "receive_statement" | "range_clause" => within(p.parent()),
            "short_var_declaration" => match p.parent() {
                Some(g) if g.kind() == "for_clause" => within(g.parent()),
                Some(g) if g.child_by_field_name("initializer").map(|i| i.id()) == Some(p.id()) => within(Some(g)),
                _ => true,
            },
            _ => true,
        };
    }
    true
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
    if p.kind().ends_with("pattern") {
        // the `type` of `Some(x)` or `P { x }` is the pattern's, not the binder's
        return (None, None);
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
            // a Java `a.b.C` names its parts by position only
            "scoped_identifier" | "scoped_type_identifier" | "qualified_type" | "nested_type_identifier" => {
                t.child_by_field_name("name").or_else(|| t.named_child(t.named_child_count().checked_sub(1)? as u32))?
            }
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
            Lang::Java => java_bindings(n, src, &mut out),
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
            looked_up: HashMap::new(),
            def_tys: HashMap::new(),
            trait_members: None,
            java_dirs: None,
            java_pkgs: None,
            java_paths: HashMap::new(),
            java_bases: HashMap::new(),
            java_fields: HashMap::new(),
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
        let base = path.rsplit('/').next().unwrap_or(path);
        let stem = match base.rfind('.') {
            Some(i) if i > 0 => &base[..i],
            _ => base,
        };
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
        for (id, name, qualname, kind, start, end) in rows {
            // the header chunk (docstring and imports) is named after the file, not defined in it;
            // a definition that starts on line 1 too sorts before it
            if kind == "module" && start == 1 && name == stem && qualname == stem {
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
            if lang == Lang::Java {
                // a static import brings in every overload of the name
                return self.java_binding(b).map(|id| self.java_overload(id, None, "import"));
            }
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
        if lang == Lang::Java {
            return self.java_resolve(path, &fi, node, name);
        }
        if lang == Lang::Rust && node.parent().is_some_and(|p| p.kind() == "token_tree") {
            if let Some(why) = macro_token(node, &fi.src) {
                return Err(why);
            }
        }
        if lang == Lang::Js && node.kind() == "super" {
            // `super(..)` calls the constructor of the base class, which stands for it as `new C()` does
            let called = node.parent().and_then(|p| p.child_by_field_name("function")).is_some_and(|f| f.id() == node.id());
            if !called {
                return Err("keyword");
            }
            let Some(Ty::Local(c)) = self.container_at(&fi, line) else { return Err("receiver") };
            let (order, outside, unknown) = self.linearized(c, 0, &mut HashMap::new());
            return match order.get(1) {
                Some(&b) => Ok((b, "super")),
                None => Err(if outside && !unknown { "external" } else { "not found" }),
            };
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
        if lang == Lang::Java {
            return self.java_bare(fi, node, name);
        }
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
        // an import in the function around the use (Python `import os as name`, a `use` in a Rust body)
        let func = enclosing_function(node).map(|f| f.byte_range());
        let local_import = |b: &&Binding| b.local == name && func.as_ref().is_some_and(|r| b.sites.iter().any(|s| r.contains(s)));
        let local_import = fi.bindings.iter().find(local_import).cloned();
        if local_import.is_none() && shadowed(lang, node, name, &fi.src) {
            return Err("local variable");
        }
        if let Some(d) = top_level(fi, name).filter(|_| local_import.is_none()) {
            return Ok((d.id, "module"));
        }
        if let Some(b) = local_import.or_else(|| fi.bindings.iter().find(|b| b.local == name).cloned()) {
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
        if lang == Lang::Java {
            return self.java_qualified(path, fi, q, node, name);
        }
        let qt = text(q, &fi.src).trim();
        let sup = match lang {
            Lang::Js => q.kind() == "super",
            Lang::Python => q.kind() == "call" && q.child_by_field_name("function").is_some_and(|f| f.kind() == "identifier" && text(f, &fi.src) == "super"),
            _ => false,
        };
        if sup {
            return self.super_member(path, fi, lang, q, name, line);
        }
        let receiver = match lang {
            Lang::Python => qt == "self" || qt == "cls",
            Lang::Js => qt == "this",
            Lang::Rust => qt == "Self" || (qt == "self" && node.parent().is_some_and(|p| p.kind() == "field_expression")),
            Lang::Go | Lang::Java => false,
        };
        if receiver {
            if let Some(owner) = literal_owner(fi, q) {
                if owner.is_empty() {
                    return Err("receiver");
                }
                let want = format!("{owner}.{name}");
                return fi.defs.iter().find(|d| d.qualname == want).map(|d| (d.id, "member")).ok_or("not found");
            }
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
            Some(Ty::Foreign(t, _)) if !self.extended(&t, name) && !self.trait_member(name) => Err("external"),
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

    /// Whether a trait or interface of the repository declares a method `name`: a blanket `impl`
    /// or an interface merged into a type from outside may give that type the method.
    fn trait_member(&mut self, name: &str) -> bool {
        if self.trait_members.is_none() {
            let mut words = HashSet::new();
            if let Ok(mut st) = self.ix.db.prepare("SELECT text FROM chunks WHERE kind IN ('trait', 'interface')") {
                let texts = st.query_map([], |r| crate::index::unpack_text(r.get_ref(0)?)).map(|rows| rows.flatten().collect::<Vec<_>>()).unwrap_or_default();
                for t in texts {
                    let mut rest = t.as_str();
                    while let Some(i) = rest.find(|c: char| c.is_alphanumeric() || c == '_') {
                        let word_len = rest[i..].find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len() - i);
                        let (word, after) = (&rest[i..i + word_len], rest[i + word_len..].trim_start_matches(['?', ' ']));
                        if after.starts_with(['(', '<']) {
                            words.insert(word.to_string());
                        }
                        rest = &rest[i + word_len..];
                    }
                }
            }
            self.trait_members = Some(words);
        }
        self.trait_members.as_ref().is_some_and(|w| w.contains(name))
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
            "identifier" if lang == Lang::Java => self.java_variable_ty(path, fi, e),
            "method_invocation" => {
                let leaf = e.child_by_field_name("name")?;
                let found = match java_qualifier(leaf) {
                    Some(q) => self.java_qualified(path, fi, q, leaf, text(leaf, src)),
                    None => self.java_call(fi, leaf, text(leaf, src)),
                };
                self.def_ty(found.ok()?.0)
            }
            "object_creation_expression" => self.named_ty(path, fi, lang, e.child_by_field_name("type")?),
            "cast_expression" => self.ty_of_type(path, fi, lang, e.child_by_field_name("type")?),
            "string_literal" => Some(Ty::Foreign("String".to_string(), Vec::new())),
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
            "string" | "template_string" | "number" | "array" | "regex" if lang == Lang::Js => {
                let class = match e.kind() {
                    "number" => "Number",
                    "array" => "Array",
                    "regex" => "RegExp",
                    _ => "String",
                };
                Some(Ty::Foreign(class.to_string(), Vec::new()))
            }
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
        if let Some(t) = self.def_tys.get(&id) {
            return t.clone();
        }
        let t = self.def_ty_uncached(id);
        self.def_tys.insert(id, t.clone());
        t
    }

    fn def_ty_uncached(&mut self, id: i64) -> Option<Ty> {
        let kind = self.def_kind(id)?;
        if CONTAINER_KINDS.contains(&kind.as_str()) || kind == "type" {
            return concrete(&kind).then_some(Ty::Local(id));
        }
        let (path, _) = self.def_row(id)?;
        let fi = self.file(&path);
        let lang = fi.lang?;
        let node = def_node(&fi, id)?;
        let java = lang == Lang::Java && node.kind() == "method_declaration";
        let ret = node.child_by_field_name("return_type").or_else(|| node.child_by_field_name(if java { "type" } else { "result" }))?;
        self.ty_of_type(&path, &fi, lang, ret)
    }

    /// The declared type of the field `name` of container `owner`: a Rust or Go struct field, a
    /// class field with a type or a value, a Python attribute set on `self` in a method.
    fn field_ty(&mut self, owner: i64, name: &str) -> Option<Ty> {
        let (path, _) = self.def_row(owner)?;
        let fi = self.file(&path);
        let lang = fi.lang?;
        if lang == Lang::Java {
            return self.java_field_ty(owner, name, 0);
        }
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
                // a Go embedded field is named after its type
                "field_declaration"
                    if lang == Lang::Go
                        && n.child_by_field_name("name").is_none()
                        && n.child_by_field_name("type").and_then(type_name).is_some_and(|t| text(t, src) == name) =>
                {
                    Some((n.child_by_field_name("type"), None))
                }
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
                && (is_local_binder(n, src) || n.parent().is_some_and(|p| matches!(p.kind(), "static_item" | "const_item")))
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
        let binder = binder?;
        let (ty, value) = declared(binder);
        if let Some(t) = ty {
            return self.built_ty(path, fi, lang, binder, value, at).or_else(|| self.ty_of_type(path, fi, lang, t));
        }
        self.type_of(path, fi, lang, value?)
    }

    /// `Base b = new Big()` and its likes: the type the constructor form `value` of the binder
    /// builds, which a use `at` in the same function sees until an assignment to the variable.
    fn built_ty(&mut self, path: &str, fi: &FileInfo, lang: Lang, binder: Node, value: Option<Node>, at: Node) -> Option<Ty> {
        let func = enclosing_function(at);
        if func.map(|f| f.id()) != enclosing_function(binder).map(|f| f.id()) {
            return None;
        }
        let scope = func.or_else(|| fi.tree.as_ref().map(|t| t.root_node()))?;
        if assigned_between(scope, text(at, &fi.src), binder.end_byte(), at.start_byte(), &fi.src) {
            return None;
        }
        self.constructed(path, fi, lang, value?)
    }

    /// The repository type a constructor form builds: `new T()`, `T{}`, `&T{}`, a Python call of a
    /// class, a Rust unit struct and `Box::new(..)` of one of these.
    fn constructed(&mut self, path: &str, fi: &FileInfo, lang: Lang, v: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let line = v.start_position().row as i64 + 1;
        let built = match v.kind() {
            "new_expression" | "object_creation_expression" | "composite_literal" | "struct_expression" => self.type_of(path, fi, lang, v),
            "unary_expression" | "reference_expression" | "parenthesized_expression" => {
                return self.constructed(
                    path,
                    fi,
                    lang,
                    v.child_by_field_name("operand").or_else(|| v.child_by_field_name("value")).or_else(|| v.named_child(0))?,
                )
            }
            "call" | "call_expression" => {
                let f = v.child_by_field_name("function")?;
                if lang == Lang::Rust {
                    let holder = f.kind() == "scoped_identifier"
                        && f.child_by_field_name("name").is_some_and(|n| text(n, src) == "new")
                        && f.child_by_field_name("path").is_some_and(|p| RUST_DEREF.contains(&text(p, src)));
                    return if holder { self.constructed(path, fi, lang, v.child_by_field_name("arguments")?.named_child(0)?) } else { None };
                }
                if lang != Lang::Python {
                    return None;
                }
                let leaf = member_parts(f).map(|(_, m)| m).unwrap_or(f);
                let found = match qualifier(leaf) {
                    Some(q) => self.resolve_qualified(path, fi, lang, q, leaf, text(leaf, src), line),
                    None => self.resolve_bare(path, fi, lang, leaf, text(leaf, src), line),
                };
                found.ok().map(|(id, _)| id).filter(|&id| self.def_kind(id).is_some_and(|k| concrete(&k))).map(Ty::Local)
            }
            "identifier" if lang == Lang::Rust => {
                let id = self.resolve_bare(path, fi, lang, v, text(v, src), line).ok()?.0;
                self.def_kind(id).filter(|k| k == "struct").map(|_| Ty::Local(id))
            }
            _ => None,
        };
        built.filter(|t| matches!(t, Ty::Local(_)))
    }

    /// The type a type expression names, past references, pointers, `Box`-like holders and
    /// `Optional[..]`; None for a trait object, a union, a type parameter or anything unclear.
    fn ty_of_type(&mut self, path: &str, fi: &FileInfo, lang: Lang, t: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        match t.kind() {
            "reference_type" => self.ty_of_type(path, fi, lang, t.child_by_field_name("type")?),
            "type" | "type_annotation" | "parenthesized_type" | "pointer_type" | "readonly_type" => self.ty_of_type(path, fi, lang, t.named_child(0)?),
            "primitive_type" | "predefined_type" | "integral_type" | "floating_point_type" | "boolean_type" => {
                Some(Ty::Foreign(text(t, src).to_string(), Vec::new()))
            }
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
        if lang == Lang::Java {
            // an interface types a value as well as a class does: its methods are declared in it
            return match self.java_type_node(fi, n) {
                Ok(id) => Some(Ty::Local(id)),
                Err("external") => Some(Ty::Foreign(name.to_string(), Vec::new())),
                Err(_) => None,
            };
        }
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

    /// `name` as a member container `id` itself declares.
    fn declared_member(&mut self, id: i64, name: &str) -> Option<i64> {
        let (path, qualname) = self.def_row(id)?;
        let want = format!("{}.{name}", strip_generics(&qualname));
        self.file(&path).defs.iter().find(|d| d.name == name && strip_generics(&d.qualname) == want).map(|d| d.id)
    }

    fn member_inner(&mut self, id: i64, name: &str, depth: usize) -> Option<i64> {
        if let Some(d) = self.declared_member(id, name) {
            return Some(d);
        }
        let (path, qualname) = self.def_row(id)?;
        let want = format!("{}.{name}", strip_generics(&qualname));
        let fi = self.file(&path);
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
        if fi.lang == Some(Lang::Java) {
            if depth >= 8 {
                return None;
            }
            for b in self.java_bases(id).iter() {
                match *b {
                    Ok(bid) if bid != id => {
                        if let Some(m) = self.member(bid, name, depth + 1) {
                            return Some(m);
                        }
                    }
                    _ => {}
                }
            }
            return None;
        }
        if depth >= 3 {
            return None;
        }
        for bid in self.bases(id) {
            if let Some(m) = self.member(bid, name, depth + 1) {
                return Some(m);
            }
        }
        None
    }

    /// The types container `id` extends or implements, each resolved: Java supertypes, Python
    /// and JavaScript base classes, TypeScript `extends` and `implements`, the trait of a Rust
    /// `impl Trait for T`.
    fn bases(&mut self, id: i64) -> Vec<i64> {
        self.base_results(id).into_iter().filter_map(|b| b.ok()).collect()
    }

    /// The bases of `bases`, in header order, with why one is not of the repository: "external"
    /// for an import from outside or a name the repository defines nowhere (`Error`, `object`).
    fn base_results(&mut self, id: i64) -> Vec<std::result::Result<i64, &'static str>> {
        let Some((path, _)) = self.def_row(id) else { return Vec::new() };
        let fi = self.file(&path);
        let (Some(lang), Some(tree), Some(d)) = (fi.lang, fi.tree.as_ref(), fi.defs.iter().find(|d| d.id == id)) else { return Vec::new() };
        if lang == Lang::Java {
            return self.java_bases(id).iter().copied().filter(|&b| b != Ok(id)).collect();
        }
        let mut named: Vec<Node> = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(n) = stack.pop() {
            let (s, e) = (n.start_position().row as i64 + 1, n.end_position().row as i64 + 1);
            if e < d.start || s > d.end {
                continue;
            }
            if n.kind() == "impl_item" && e == d.end {
                named.extend(n.child_by_field_name("trait"));
                break;
            }
            if matches!(n.kind(), "class_definition" | "class_declaration" | "abstract_class_declaration" | "class")
                && n.child_by_field_name("name").map(|x| text(x, &fi.src)) == Some(d.name.as_str())
            {
                let mut cur = n.walk();
                for ch in n.children(&mut cur).filter(|c| matches!(c.kind(), "argument_list" | "class_heritage")) {
                    let mut c2 = ch.walk();
                    for b in ch.named_children(&mut c2) {
                        if matches!(b.kind(), "extends_clause" | "implements_clause") {
                            let mut c3 = b.walk();
                            named.extend(b.named_children(&mut c3));
                        } else {
                            named.push(b);
                        }
                    }
                }
                break;
            }
            let mut cur = n.walk();
            stack.extend(n.named_children(&mut cur));
        }
        let mut out = Vec::new();
        for b in named {
            let Some(leaf) = type_name(b) else { continue };
            let leaf_name = text(leaf, &fi.src);
            let found = match qualifier(leaf) {
                Some(q) => self.resolve_qualified(&path, &fi, lang, q, leaf, leaf_name, d.start),
                None => self.resolve_bare(&path, &fi, lang, leaf, leaf_name, d.start),
            };
            match found {
                Ok((x, _)) if x == id => {}
                Ok((x, _)) => out.push(Ok(x)),
                Err("not found") if !self.defined_anywhere(leaf_name) => out.push(Err("external")),
                Err(e) => out.push(Err(e)),
            }
        }
        out
    }

    /// Container `id` and the classes it inherits from, in Python's method resolution order (C3,
    /// which for one base per class is the chain upwards), and whether a base is from outside and
    /// whether one could not be found.
    fn linearized(&mut self, id: i64, depth: usize, memo: &mut HashMap<i64, (Vec<i64>, bool, bool)>) -> (Vec<i64>, bool, bool) {
        if let Some(l) = memo.get(&id) {
            return l.clone();
        }
        // a class that comes round again through its own bases has none the second time
        memo.insert(id, (vec![id], false, false));
        let (mut seqs, mut direct, mut outside, mut unknown) = (Vec::new(), Vec::new(), false, false);
        for b in if depth < 8 { self.base_results(id) } else { Vec::new() } {
            match b {
                Ok(b) => {
                    let (l, o, u) = self.linearized(b, depth + 1, memo);
                    (outside, unknown) = (outside || o, unknown || u);
                    seqs.push(l);
                    direct.push(b);
                }
                Err("external") => outside = true,
                Err(_) => unknown = true,
            }
        }
        seqs.push(direct);
        let mut out = vec![id];
        loop {
            seqs.retain(|s| !s.is_empty());
            let Some(head) = seqs.iter().map(|s| s[0]).find(|h| !seqs.iter().any(|s| s[1..].contains(h))) else { break };
            out.push(head);
            for s in seqs.iter_mut().filter(|s| s[0] == head) {
                s.remove(0);
            }
        }
        // an order C3 cannot keep (Python refuses the class): the rest in header order
        for b in seqs.into_iter().flatten() {
            if !out.contains(&b) {
                out.push(b);
            }
        }
        let l = (out, outside, unknown);
        memo.insert(id, l.clone());
        l
    }

    /// `super.name` (JavaScript), `super().name` and `super(C, self).name` (Python): the first
    /// class after the one around (or `C`) in its resolution order that declares `name`.
    fn super_member(&mut self, path: &str, fi: &FileInfo, lang: Lang, q: Node, name: &str, line: i64) -> Resolution {
        if literal_owner(fi, q).is_some() {
            return Err("receiver");
        }
        let around = match self.container_at(fi, line) {
            Some(Ty::Local(c)) => Some(c),
            _ => None,
        };
        let named = match q.child_by_field_name("arguments").and_then(|a| a.named_child(0)) {
            Some(c) => {
                let leaf = type_name(c).ok_or("receiver")?;
                let found = match qualifier(leaf) {
                    Some(q) => self.resolve_qualified(path, fi, lang, q, leaf, text(leaf, &fi.src), line),
                    None => self.resolve_bare(path, fi, lang, leaf, text(leaf, &fi.src), line),
                };
                Some(found.map_err(|_| "receiver")?.0)
            }
            None => None,
        };
        // `super(C, self)` searches the order of the class the call sits in, after `C`; outside a
        // class only the order of `C` itself is known
        let (start, after) = match (around, named) {
            (Some(a), Some(c)) => (a, c),
            (None, Some(c)) => (c, c),
            (Some(a), None) => (a, a),
            (None, None) => return Err("receiver"),
        };
        let (order, outside, unknown) = self.linearized(start, 0, &mut HashMap::new());
        let at = order.iter().position(|&c| c == after).ok_or("receiver")?;
        for &c in &order[at + 1..] {
            if let Some(m) = self.declared_member(c, name) {
                return Ok((m, "super"));
            }
        }
        // a class with no base in the repository inherits from outside (`object`, `Error`)
        Err(if !unknown && (outside || order.len() == at + 1) { "external" } else { "not found" })
    }

    /// The methods method `id` overrides or implements: the ones of its name declared in the
    /// types its owner extends or implements, at any depth; a Java overload of another arity is none.
    pub fn overridden(&mut self, id: i64) -> Vec<i64> {
        let Some((path, _)) = self.def_row(id) else { return Vec::new() };
        // an owner whose header names no base needs no parse
        let defs = self.defs_of(&path).unwrap_or_default();
        let Some(owner) = defs.iter().find(|d| d.id == id).and_then(|d| d.parent) else { return Vec::new() };
        let head: String =
            self.ix.db.query_row("SELECT text FROM chunks WHERE id=?1", [defs[owner].id], |r| crate::index::unpack_text(r.get_ref(0)?)).unwrap_or_default();
        let head = &head[..head.find('{').into_iter().chain(head.find(":\n")).min().unwrap_or(head.len())];
        if !head.contains('(') && !head.split(|c: char| !c.is_alphanumeric()).any(|w| matches!(w, "for" | "extends" | "implements")) {
            return Vec::new();
        }
        let fi = self.file(&path);
        let Some(d) = fi.defs.iter().find(|d| d.id == id) else { return Vec::new() };
        let Some(owner) = d.parent.map(|p| &fi.defs[p]).filter(|o| CONTAINER_KINDS.contains(&o.kind.as_str())) else { return Vec::new() };
        let arity = java_params(&fi, id).filter(|_| fi.lang == Some(Lang::Java));
        let (mut out, mut todo, mut seen) = (Vec::new(), self.bases(owner.id), HashSet::from([owner.id]));
        while let Some(b) = todo.pop() {
            if seen.len() > 64 || !seen.insert(b) {
                continue;
            }
            let Some((bpath, _)) = self.def_row(b) else { continue };
            let bf = self.file(&bpath);
            let declared = bf.defs.iter().filter(|m| m.name == d.name && m.parent.is_some_and(|p| bf.defs[p].id == b));
            out.extend(declared.filter(|m| arity.is_none() || java_params(&bf, m.id) == arity).map(|m| m.id));
            todo.extend(self.bases(b));
        }
        out
    }

    /// Definition `name` at the top level of one of `files`, following re-exports and wildcard imports.
    fn lookup_top(&mut self, files: &[String], name: &str, depth: usize) -> Option<i64> {
        // a lookup that comes round again (wildcard import rings) finds nothing; a repeat one
        // (the base of a class named again) gets the first answer
        let key = (files.join("\n"), name.to_string());
        if let Some(r) = self.looked_up.get(&key) {
            return *r;
        }
        if self.nest > 32 {
            return None;
        }
        self.looked_up.insert(key.clone(), None);
        self.nest += 1;
        let r = self.lookup_top_inner(files, name, depth);
        self.nest -= 1;
        self.looked_up.insert(key, r);
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
            Lang::Java => None,
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
            Lang::Java => self.java_package_files(module),
        }
    }

    fn first_file(&self, cands: &[String]) -> Option<String> {
        cands.iter().find(|c| self.files.contains(c.as_str())).cloned()
    }

    fn py_module(&self, from: &str, module: &str) -> Option<String> {
        self.py_rooted(from, module).or_else(|| self.py_beside(from, module))
    }

    fn py_try_base(&self, base: &str) -> Option<String> {
        self.first_file(&[format!("{base}.py"), format!("{base}/__init__.py"), format!("{base}.pyi"), format!("{base}/__init__.pyi")])
    }

    /// An absolute import in the directory of `from`: Python puts a script's own directory first
    /// on `sys.path`, and pytest a test's, so a sibling wins even over a standard library module.
    /// A package's directory is never on `sys.path`, so a file in one gets no such answer.
    fn py_beside(&self, from: &str, module: &str) -> Option<String> {
        let dir = dir_of(from);
        let in_package = || ["py", "pyi"].iter().any(|e| self.files.contains(format!("{dir}/__init__.{e}").as_str()));
        if dir.is_empty() || module.starts_with('.') || in_package() {
            return None;
        }
        self.py_try_base(&format!("{dir}/{}", module.replace('.', "/")))
    }

    fn py_rooted(&self, from: &str, module: &str) -> Option<String> {
        let try_base = |base: &str| self.py_try_base(base);
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
            Lang::Java => {
                // `a.b.*` names a package, or the members of a class (`import static a.b.C.*`)
                let name = module.trim_end_matches(".*");
                if module.ends_with(".*") {
                    out.extend(self.java_package_files(name).unwrap_or_default());
                }
                if out.is_empty() {
                    out.extend(self.java_path(name).ok().and_then(|id| self.def_row(id)).map(|(path, _)| path));
                }
            }
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
                            self.py_rooted(from, &sub)
                                .or_else(|| self.py_rooted(from, &b.module))
                                .or_else(|| self.py_beside(from, &sub))
                                .or_else(|| self.py_beside(from, &b.module))
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
        "field_access" => ("object", "field"),
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

/// What the tokens around a name in a Rust macro argument, which the grammar leaves without
/// syntax, say it is: a field or a method after `.`, a field name in `{ name: value }`.
fn macro_token(n: Node, src: &str) -> Option<&'static str> {
    let next = n.next_sibling();
    if n.prev_sibling().is_some_and(|p| p.kind() == ".") {
        let call = next.is_some_and(|x| x.kind() == "::" || (x.kind() == "token_tree" && text(x, src).starts_with('(')));
        return Some(if call { "receiver" } else { "field or key" });
    }
    if macro_local(n, src) {
        return Some("local variable");
    }
    let braced = n.parent().is_some_and(|p| text(p, src).starts_with('{'));
    (braced && next.is_some_and(|x| x.kind() == ":")).then_some("field or key")
}

/// Whether a name in a Rust macro argument is bound in the tokens before it: a parameter of a
/// closure (`|(a, name)| ..`) or a `let`, in its own token tree or one around it.
fn macro_local(n: Node, src: &str) -> bool {
    let name = text(n, src);
    let binds = |tokens: &[Node]| {
        let mut names = Vec::new();
        for t in tokens {
            token_idents(*t, src, &mut names);
        }
        names.contains(&name)
    };
    let mut child = n;
    while let Some(tt) = child.parent().filter(|p| p.kind() == "token_tree") {
        let mut cur = tt.walk();
        let kids: Vec<Node> = tt.children(&mut cur).collect();
        let at = kids.iter().position(|k| k.id() == child.id()).unwrap_or(kids.len());
        let mut open: Option<usize> = None;
        for (i, k) in kids.iter().enumerate().take(at) {
            match k.kind() {
                "|" => match open.take() {
                    Some(o) if binds(&kids[o + 1..i]) => return true,
                    Some(_) => {}
                    None => open = Some(i),
                },
                "let" => {
                    let bound = i + 1 + usize::from(kids.get(i + 1).is_some_and(|m| m.kind() == "mut"));
                    if bound == at || (bound < at && binds(&kids[bound..bound + 1])) {
                        return true;
                    }
                }
                _ => {}
            }
        }
        // between the bars of a closure: one of its parameters
        if open.is_some() {
            return true;
        }
        child = tt;
    }
    false
}

fn token_idents<'s>(n: Node, src: &'s str, out: &mut Vec<&'s str>) {
    if n.kind() == "identifier" {
        out.push(text(n, src));
    }
    let mut cur = n.walk();
    for c in n.named_children(&mut cur) {
        token_idents(c, src, out);
    }
}

/// When `this` at `n` is bound by a method of an object literal: the literal's qualified name,
/// the method's own before its last dot ("" when the literal has none). An arrow function takes
/// `this` from the code around it.
fn literal_owner(fi: &FileInfo, n: Node) -> Option<String> {
    let f = std::iter::successors(n.parent(), |p| p.parent()).find(|p| FUNCTION_NODES.contains(&p.kind()) && p.kind() != "arrow_function")?;
    let p = f.parent()?;
    let key = match f.kind() {
        "method_definition" if p.kind() == "object" => f.child_by_field_name("name")?,
        "function_expression" | "function" if p.kind() == "pair" && p.parent()?.kind() == "object" => p.child_by_field_name("key")?,
        _ => return None,
    };
    let (name, line) = (text(key, &fi.src), key.start_position().row as i64 + 1);
    let d = fi.defs.iter().filter(|d| d.name == name && d.start <= line && line <= d.end).min_by_key(|d| d.end - d.start);
    Some(d.and_then(|d| d.qualname.rsplit_once('.')).map(|(o, _)| o.to_string()).unwrap_or_default())
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

/// A parameter or local variable named `name` in the functions around `node` (a closure sees the
/// locals of the function it is written in) hides outer definitions; one bound in a nested
/// function, block or comprehension that does not hold `node` does not.
fn shadowed(lang: Lang, node: Node, name: &str, src: &str) -> bool {
    let at = node.start_byte();
    let holds = |n: Node| n.start_byte() <= at && at < n.end_byte();
    let Some(mut func) = enclosing_function(node) else { return false };
    // a Rust `fn` item does not see the locals of the function around it
    while let Some(outer) = enclosing_function(func).filter(|_| func.kind() != "function_item") {
        func = outer;
    }
    let block_scoped: &[&str] = match lang {
        Lang::Python => &["list_comprehension", "set_comprehension", "dictionary_comprehension", "generator_expression"],
        _ => &["statement_block", "block", "compound_statement", "for_statement", "for_in_statement", "for_expression"],
    };
    // (node, inside a block that does not hold the use): there only a JS `var` reaches the use
    let mut stack = vec![(func, false)];
    while let Some((n, aside)) = stack.pop() {
        if matches!(n.kind(), "identifier" | "shorthand_property_identifier_pattern" | "shorthand_field_identifier")
            && text(n, src) == name
            && is_local_binder(n, src)
            && (!aside || is_js_var(n))
            && visible(n, at)
        {
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

/// The innermost function around `node`.
fn enclosing_function(node: Node) -> Option<Node> {
    let mut f = node.parent();
    while let Some(n) = f {
        if FUNCTION_NODES.contains(&n.kind()) {
            return Some(n);
        }
        f = n.parent();
    }
    None
}

/// Whether code of `scope` between bytes `from` and `to` assigns to the variable `name`.
fn assigned_between(scope: Node, name: &str, from: usize, to: usize, src: &str) -> bool {
    let mut stack = vec![scope];
    while let Some(n) = stack.pop() {
        if n.end_byte() <= from || n.start_byte() >= to {
            continue;
        }
        if n.start_byte() >= from && matches!(n.kind(), "assignment_expression" | "assignment" | "assignment_statement") {
            if let Some(l) = n.child_by_field_name("left") {
                let mut cur = l.walk();
                let hit = if l.kind() == "expression_list" { l.named_children(&mut cur).any(|x| text(x, src) == name) } else { text(l, src) == name };
                if hit {
                    return true;
                }
            }
        }
        let mut cur = n.walk();
        stack.extend(n.named_children(&mut cur));
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

// ------------------------------------------------------------------------- Java

/// A class around a Java occurrence: one the index holds, or the body of an anonymous class
/// (whose methods the index holds under the method around it).
#[derive(Clone, Copy)]
enum JClass<'t> {
    Named(i64),
    Anon(Node<'t>),
}

const JAVA_CLASS_NODES: &[&str] = &["class_declaration", "interface_declaration", "enum_declaration", "record_declaration"];
/// Methods a Java class has from `Object`, and an enum from `Enum`: none is a repository definition.
const JAVA_OBJECT_METHODS: &[&str] = &["equals", "hashCode", "toString", "getClass", "notify", "notifyAll", "wait", "clone", "finalize"];
const JAVA_ENUM_METHODS: &[&str] = &["values", "valueOf", "ordinal", "name", "compareTo", "getDeclaringClass", "describeConstable"];

fn java_bindings(n: Node, src: &str, out: &mut Vec<Binding>) -> bool {
    if n.kind() != "import_declaration" {
        // imports come first in a file, outside any class
        return n.kind() != "program";
    }
    let mut cur = n.walk();
    let kids: Vec<Node> = n.named_children(&mut cur).collect();
    let Some(&path) = kids.iter().find(|k| matches!(k.kind(), "scoped_identifier" | "identifier")) else { return true };
    let full: String = text(path, src).chars().filter(|c| !c.is_whitespace()).collect();
    let is_static = text(n, src).trim_start_matches("import").trim_start().starts_with("static");
    if kids.iter().any(|k| k.kind() == "asterisk") {
        // `import static a.C.*` brings in members of a class, `import a.*` the classes of a package
        out.push(Binding { local: "*".into(), module: full, item: is_static.then(|| "*".to_string()), sites: vec![] });
    } else {
        let (module, item) = full.rsplit_once('.').unwrap_or(("", &full));
        let site = path.child_by_field_name("name").unwrap_or(path);
        out.push(Binding { local: item.into(), module: module.into(), item: Some(item.into()), sites: vec![site.start_byte()] });
    }
    true
}

/// The package a Java source declares (`a.b`), "" for the default package.
fn java_package(src: &str) -> String {
    let mut rest = src.trim_start_matches('\u{feff}');
    loop {
        rest = rest.trim_start();
        if let Some(r) = rest.strip_prefix("//") {
            rest = r.split_once('\n').map_or("", |(_, r)| r);
        } else if let Some(r) = rest.strip_prefix("/*") {
            rest = r.split_once("*/").map_or("", |(_, r)| r);
        } else if let Some(r) = rest.strip_prefix('@') {
            // an annotation of the package (package-info.java), with its arguments
            rest = r.trim_start_matches(|c: char| c.is_alphanumeric() || c == '_' || c == '.' || c == '$').trim_start();
            if rest.starts_with('(') {
                let mut depth = 0;
                let end = rest.char_indices().find(|&(_, c)| {
                    depth += match c {
                        '(' => 1,
                        ')' => -1,
                        _ => 0,
                    };
                    depth == 0
                });
                rest = end.map_or("", |(i, _)| &rest[i + 1..]);
            }
        } else {
            break;
        }
    }
    match rest.strip_prefix("package") {
        Some(r) if r.starts_with(char::is_whitespace) => r.split(';').next().unwrap_or("").chars().filter(|c| !c.is_whitespace()).collect(),
        _ => String::new(),
    }
}

/// The first bytes of a repository file: enough for its package declaration.
fn java_head(root: &std::path::Path, rel: &str) -> String {
    use std::io::Read;
    let p = root.join(rel);
    if !std::fs::symlink_metadata(&p).is_ok_and(|m| m.is_file()) {
        return String::new();
    }
    let mut buf = Vec::new();
    if let Ok(f) = std::fs::File::open(&p) {
        let _ = f.take(8192).read_to_end(&mut buf);
    }
    String::from_utf8_lossy(&buf).into_owned()
}

/// The receiver or path in front of a Java member name: `obj` of `obj.m()`, `obj.f`, `T::m`, `a.b` of `a.b.C`.
fn java_qualifier(n: Node) -> Option<Node> {
    let p = n.parent()?;
    let is = |f: &str| p.child_by_field_name(f).map(|c| c.id()) == Some(n.id());
    match p.kind() {
        "method_invocation" if is("name") => p.child_by_field_name("object"),
        "field_access" if is("field") => p.child_by_field_name("object"),
        "scoped_identifier" if is("name") => p.child_by_field_name("scope"),
        "method_reference" => p.named_child(0).filter(|f| f.id() != n.id()),
        "scoped_type_identifier" => n.prev_named_sibling().filter(|s| !s.kind().ends_with("annotation")),
        _ => None,
    }
}

/// The identifiers of a dotted Java name (`a.b.C`, `Outer.Inner<T>`); false when it is not one.
fn java_segments<'t>(n: Node<'t>, out: &mut Vec<Node<'t>>) -> bool {
    match n.kind() {
        "identifier" | "type_identifier" => {
            out.push(n);
            true
        }
        "scoped_type_identifier" | "scoped_identifier" | "field_access" | "generic_type" => {
            let mut cur = n.walk();
            let kids: Vec<Node> = n.named_children(&mut cur).filter(|c| c.kind() != "type_arguments" && !c.kind().ends_with("annotation")).collect();
            kids.into_iter().all(|c| java_segments(c, out))
        }
        _ => false,
    }
}

/// The classes around `node`, innermost first.
fn java_classes<'t>(fi: &FileInfo, node: Node<'t>) -> Vec<JClass<'t>> {
    let mut out = Vec::new();
    let mut a = node.parent();
    while let Some(n) = a {
        if JAVA_CLASS_NODES.contains(&n.kind()) {
            if let Some(id) = java_def(fi, n) {
                out.push(JClass::Named(id));
            }
        } else if n.kind() == "class_body" {
            if let Some(p) = n.parent().filter(|p| p.kind() == "object_creation_expression") {
                out.push(JClass::Anon(p));
            }
        }
        a = n.parent();
    }
    out
}

/// The definition of the index a Java class declaration is.
fn java_def(fi: &FileInfo, n: Node) -> Option<i64> {
    let name = n.child_by_field_name("name")?;
    let (name, line) = (text(name, &fi.src), name.start_position().row as i64 + 1);
    let d = fi.defs.iter().filter(|d| d.name == name && d.start <= line && line <= d.end && CONTAINER_KINDS.contains(&d.kind.as_str()));
    d.min_by_key(|d| d.end - d.start).map(|d| d.id)
}

/// Whether the identifier `n` declares a local variable or a parameter.
fn java_binder(n: Node) -> bool {
    let Some(p) = n.parent() else { return false };
    let is = |f: &str| p.child_by_field_name(f).map(|c| c.id()) == Some(n.id());
    match p.kind() {
        // a record's components are its fields
        "formal_parameter" => is("name") && p.parent().and_then(|l| l.parent()).is_some_and(|o| o.kind() != "record_declaration"),
        "catch_formal_parameter" | "enhanced_for_statement" | "resource" | "instanceof_expression" => is("name"),
        "variable_declarator" => is("name") && p.parent().is_some_and(|d| matches!(d.kind(), "local_variable_declaration" | "spread_parameter")),
        "inferred_parameters" | "type_pattern" | "record_pattern_component" => true,
        "lambda_expression" => is("parameters"),
        _ => false,
    }
}

/// The code a Java local binder `b` is visible in.
fn java_scope(b: Node) -> Option<Node> {
    let p = b.parent()?;
    match p.kind() {
        "formal_parameter" | "resource" => p.parent()?.parent(),
        "variable_declarator" => {
            let d = p.parent()?;
            if d.kind() == "spread_parameter" {
                d.parent()?.parent()
            } else {
                d.parent()
            }
        }
        "catch_formal_parameter" | "inferred_parameters" => p.parent(),
        "enhanced_for_statement" | "lambda_expression" => Some(p),
        // a pattern variable: the block around it, also after an `if (!(x instanceof T t)) return;`
        _ => std::iter::successors(Some(p), |x| x.parent())
            .find(|x| matches!(x.kind(), "block" | "switch_rule" | "switch_block_statement_group" | "lambda_expression" | "method_declaration")),
    }
}

/// The nearest local variable or parameter named `name` that `node` sees, in the method around
/// it and the methods around that (an anonymous class or a lambda sees the locals around it).
fn java_local<'t>(node: Node<'t>, name: &str, src: &str) -> Option<Node<'t>> {
    const SCOPES: &[&str] = &["method_declaration", "constructor_declaration", "compact_constructor_declaration", "lambda_expression", "static_initializer"];
    let at = node.start_byte();
    let top = std::iter::successors(node.parent(), |n| n.parent()).filter(|n| SCOPES.contains(&n.kind())).last()?;
    let (mut best, mut stack): (Option<Node>, Vec<Node>) = (None, vec![top]);
    while let Some(n) = stack.pop() {
        if n.start_byte() > at {
            continue;
        }
        if n.kind() == "identifier"
            && text(n, src) == name
            && java_binder(n)
            && java_scope(n).is_some_and(|s| s.start_byte() <= at && at < s.end_byte())
            && best.is_none_or(|b| b.start_byte() < n.start_byte())
        {
            best = Some(n);
        }
        let mut cur = n.walk();
        stack.extend(n.named_children(&mut cur));
    }
    best
}

/// What a Java local binder is declared with: (its type unless `var`, its value).
fn java_declared<'t>(b: Node<'t>, src: &str) -> (Option<Node<'t>>, Option<Node<'t>>) {
    let Some(p) = b.parent() else { return (None, None) };
    let typed = |n: Option<Node<'t>>| n.and_then(|n| n.child_by_field_name("type")).filter(|t| text(*t, src) != "var");
    match p.kind() {
        "variable_declarator" => (typed(p.parent()), p.child_by_field_name("value")),
        "formal_parameter" | "enhanced_for_statement" => (typed(Some(p)), None),
        "resource" => (typed(Some(p)), p.child_by_field_name("value")),
        "catch_formal_parameter" => {
            let mut cur = p.walk();
            let caught = p.named_children(&mut cur).find(|c| c.kind() == "catch_type").filter(|c| c.named_child_count() == 1);
            (caught.and_then(|c| c.named_child(0)), None)
        }
        "instanceof_expression" => (p.child_by_field_name("right"), None),
        "type_pattern" => (p.named_child(0), None),
        _ => (None, None),
    }
}

impl<'a> Resolver<'a> {
    /// Directory -> its `.java` files; package -> the directories whose first file declares it.
    /// A file's package and its path give its source root, so no list of roots is needed.
    fn java_pkgs(&mut self) -> &HashMap<String, Vec<String>> {
        if self.java_pkgs.is_none() {
            let mut dirs: HashMap<String, Vec<String>> = HashMap::new();
            for f in self.files.iter().filter(|f| f.ends_with(".java")) {
                dirs.entry(dir_of(f).to_string()).or_default().push(f.clone());
            }
            let mut pkgs: HashMap<String, Vec<String>> = HashMap::new();
            for (d, files) in dirs.iter_mut() {
                files.sort();
                pkgs.entry(java_package(&java_head(&self.ix.root, &files[0]))).or_default().push(d.clone());
            }
            for v in pkgs.values_mut() {
                v.sort();
            }
            self.java_dirs = Some(dirs);
            self.java_pkgs = Some(pkgs);
        }
        self.java_pkgs.as_ref().unwrap()
    }

    /// The files of the Java package `pkg`, when the repository holds it.
    fn java_package_files(&mut self, pkg: &str) -> Option<Vec<String>> {
        let dirs = self.java_pkgs().get(pkg)?.clone();
        let by_dir = self.java_dirs.as_ref()?;
        Some(dirs.iter().flat_map(|d| by_dir.get(d).cloned().unwrap_or_default()).collect())
    }

    /// A class, interface, enum or record at the top level of one of `files`.
    fn java_top(&self, files: &[String], name: &str) -> Option<i64> {
        let kinds = CONTAINER_KINDS.iter().map(|k| format!("'{k}'")).collect::<Vec<_>>().join(",");
        let sql = format!("SELECT id, path FROM chunks WHERE name_key=?1 AND name=?2 AND qualname=?2 AND kind IN ({kinds}) ORDER BY path, start");
        let mut st = self.ix.db.prepare_cached(&sql).ok()?;
        let rows: Vec<(i64, String)> = st.query_map(params![name_key(name), name], |r| Ok((r.get(0)?, r.get(1)?))).ok()?.flatten().collect();
        rows.into_iter().find(|(_, p)| files.contains(p)).map(|(id, _)| id)
    }

    /// The definition a dotted Java name stands for: a class of a package (`a.b.C`), a member of
    /// one (`a.b.C.m`, `a.b.C.Inner`). "external" when no package of the repository starts it.
    fn java_path(&mut self, full: &str) -> std::result::Result<i64, &'static str> {
        if let Some(r) = self.java_paths.get(full) {
            return *r;
        }
        let segs: Vec<&str> = full.split('.').collect();
        let mut found = Err("external");
        for k in (0..segs.len()).rev() {
            let Some(files) = self.java_package_files(&segs[..k].join(".")) else { continue };
            let Some(mut cur) = self.java_top(&files, segs[k]) else {
                if k == 0 {
                    break;
                }
                found = Err("not found");
                break;
            };
            found = Ok(cur);
            for s in &segs[k + 1..] {
                match self.member(cur, s, 0) {
                    Some(m) => cur = m,
                    None => {
                        found = Err("not found");
                        break;
                    }
                }
                found = Ok(cur);
            }
            break;
        }
        self.java_paths.insert(full.to_string(), found);
        found
    }

    fn java_binding(&mut self, b: &Binding) -> std::result::Result<i64, &'static str> {
        let item = b.item.as_deref().unwrap_or("");
        let full = if b.module.is_empty() { item.to_string() } else { format!("{}.{item}", b.module) };
        self.java_path(&full)
    }

    fn java_resolve(&mut self, path: &str, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        if !matches!(node.kind(), "identifier" | "type_identifier") {
            return Err("keyword");
        }
        let decl = std::iter::successors(node.parent(), |n| n.parent()).find(|n| matches!(n.kind(), "package_declaration" | "import_declaration"));
        if let Some(decl) = decl {
            // a part of an imported name: the class it names so far, else a package
            let part = node.parent().filter(|p| p.kind() == "scoped_identifier" && decl.kind() == "import_declaration");
            let full: String = part.map(|p| text(p, &fi.src).chars().filter(|c| !c.is_whitespace()).collect()).unwrap_or_default();
            return self.java_path(&full).map(|id| (id, "import")).map_err(|_| "package");
        }
        match java_qualifier(node) {
            Some(q) => self.java_qualified(path, fi, q, node, name),
            None => self.java_bare(fi, node, name),
        }
    }

    /// A Java name with nothing in front: a method of a class around or a static import when
    /// called, else a local, a field, a type.
    fn java_bare(&mut self, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        let p = node.parent();
        if p.is_some_and(|p| p.kind() == "method_invocation") {
            return self.java_call(fi, node, name);
        }
        let src = fi.src.as_str();
        let typed = node.kind() == "type_identifier" || p.is_some_and(|p| p.kind().ends_with("annotation"));
        if !typed {
            if java_local(node, name, src).is_some() {
                return Err("local variable");
            }
            if self.java_is_field(fi, node, name) {
                return Err("field or key");
            }
        }
        match self.java_type(fi, node, name) {
            Ok(id) => Ok((id, "type")),
            // the head of `com.acme.X`: a package
            Err(_) if !typed && p.is_some_and(|p| matches!(p.kind(), "field_access" | "scoped_identifier")) => Err("package"),
            Err(e) => Err(e),
        }
    }

    /// `m(..)` with no receiver: a method of a class around (inherited ones included), else a
    /// statically imported one.
    fn java_call(&mut self, fi: &FileInfo, node: Node, name: &str) -> Resolution {
        let arity = node.parent().and_then(|p| p.child_by_field_name("arguments")).map(|a| a.named_child_count());
        let (mut outside, mut unknown) = (false, false);
        for c in java_classes(fi, node) {
            let found = match c {
                JClass::Named(id) => {
                    let found = self.member(id, name, 0);
                    if found.is_none() {
                        let (o, u) = self.java_base_state(id, 0);
                        outside |= o;
                        unknown |= u;
                    }
                    found.ok_or("not found")
                }
                JClass::Anon(a) => self.java_anon_member(fi, a, name),
            };
            match found {
                Ok(m) => return Ok(self.java_overload(m, arity, "member")),
                Err("external") => outside = true,
                Err(_) => {}
            }
        }
        for b in fi.bindings.iter().filter(|b| b.local == name && b.item.is_some()) {
            match self.java_binding(b) {
                Ok(m) => return Ok(self.java_overload(m, arity, "import")),
                Err("external") => return Err("external"),
                Err(_) => unknown = true,
            }
        }
        for b in fi.bindings.iter().filter(|b| b.local == "*" && b.item.is_some()) {
            match self.java_path(&b.module) {
                Ok(c) => {
                    if let Some(m) = self.member(c, name, 0) {
                        return Ok(self.java_overload(m, arity, "import"));
                    }
                }
                Err("external") => outside = true,
                Err(_) => unknown = true,
            }
        }
        Err(if !unknown && (outside || JAVA_OBJECT_METHODS.contains(&name)) { "external" } else { "not found" })
    }

    /// `q.name`, `q.name(..)`, `q::name`, and the `C` of a type `q.C`.
    fn java_qualified(&mut self, path: &str, fi: &FileInfo, q: Node, node: Node, name: &str) -> Resolution {
        let p = node.parent().map(|p| p.kind()).unwrap_or("");
        let call = matches!(p, "method_invocation" | "method_reference");
        let arity = node.parent().filter(|_| p == "method_invocation").and_then(|p| p.child_by_field_name("arguments")).map(|a| a.named_child_count());
        let around = java_classes(fi, node).first().copied();
        if q.kind() == "super" {
            let Some(JClass::Named(c)) = around else { return Err("receiver") };
            let mut outside = JAVA_OBJECT_METHODS.contains(&name);
            for b in self.java_bases(c).iter() {
                match *b {
                    Ok(bid) => match self.java_member(bid, name) {
                        Ok(m) => return Ok(self.java_overload(m, arity, "super")),
                        Err("external") => outside = true,
                        Err(_) => {}
                    },
                    Err("external") => outside = true,
                    Err(_) => {}
                }
            }
            return Err(if outside { "external" } else { "not found" });
        }
        if let (Some(JClass::Anon(a)), "this") = (around, q.kind()) {
            return self.java_anon_member(fi, a, name).map(|m| self.java_overload(m, arity, "member"));
        }
        let owner = match self.java_owner(path, fi, q) {
            Ok(t) => t,
            // `a.b.C` with `a.b` a package
            Err(e) if !call => {
                let mut segs = Vec::new();
                if !java_segments(q, &mut segs) {
                    return Err(e);
                }
                let full = segs.iter().map(|s| text(*s, &fi.src)).chain(std::iter::once(name)).collect::<Vec<_>>().join(".");
                return self.java_path(&full).map(|id| (id, "qualified"));
            }
            Err(e) => return Err(e),
        };
        match owner {
            Ty::Local(c) if call => self.java_member(c, name).map(|m| self.java_overload(m, arity, "member")),
            Ty::Local(c) => {
                // a nested type; a field or an enum constant is no definition of the index
                let nested = self.member(c, name, 0).filter(|&m| self.def_kind(m).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str())));
                nested.map(|m| (m, "qualified")).ok_or("field or key")
            }
            Ty::Foreign(t, _) if call && (self.extended(&t, name) || self.trait_member(name)) => Err("receiver"),
            Ty::Foreign(..) => Err("external"),
        }
    }

    /// What the qualifier `q` of a Java member access stands for: the type of a value, or a
    /// class (`Util.m()`, `Outer.Inner`, `a.b.C.m()`).
    fn java_owner(&mut self, path: &str, fi: &FileInfo, q: Node) -> std::result::Result<Ty, &'static str> {
        let src = fi.src.as_str();
        if q.kind() == "this" {
            return self.container_at(fi, q.start_position().row as i64 + 1).ok_or("receiver");
        }
        let mut segs = Vec::new();
        if !java_segments(q, &mut segs) {
            return self.type_of(path, fi, Lang::Java, q).ok_or("receiver");
        }
        let (head, hname) = (segs[0], text(segs[0], src));
        // a variable hides a type, a type a package
        if head.kind() == "identifier" && (java_local(head, hname, src).is_some() || self.java_is_field(fi, head, hname)) {
            return self.type_of(path, fi, Lang::Java, q).ok_or("receiver");
        }
        let cur = match self.java_type(fi, head, hname) {
            Ok(id) => id,
            Err("type parameter") => return Err("receiver"),
            Err(e) if segs.len() == 1 => return if e == "external" { Ok(Ty::Foreign(hname.to_string(), Vec::new())) } else { Err(e) },
            Err(_) => {
                let full = segs.iter().map(|s| text(*s, src)).collect::<Vec<_>>().join(".");
                if self.java_package_files(&full).is_some() {
                    return Err("package");
                }
                return match self.java_path(&full) {
                    Ok(id) => Ok(Ty::Local(id)),
                    Err("external") => Ok(Ty::Foreign(text(segs[segs.len() - 1], src).to_string(), Vec::new())),
                    Err(e) => Err(e),
                };
            }
        };
        let mut ty = Ty::Local(cur);
        for s in &segs[1..] {
            let Ty::Local(c) = ty else { return Err("receiver") };
            let s = text(*s, src);
            let nested = self.member(c, s, 0).filter(|&m| self.def_kind(m).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str())));
            ty = match nested {
                Some(m) => Ty::Local(m),
                None => self.java_field_ty(c, s, 0).ok_or("receiver")?,
            };
        }
        Ok(ty)
    }

    /// A member `name` of class `id`, inherited ones included; why there is none otherwise.
    fn java_member(&mut self, id: i64, name: &str) -> std::result::Result<i64, &'static str> {
        if let Some(m) = self.member(id, name, 0) {
            return Ok(m);
        }
        let decl = self.def_row(id).and_then(|(path, _)| {
            let fi = self.file(&path);
            def_node(&fi, id).map(|n| n.kind())
        });
        if JAVA_OBJECT_METHODS.contains(&name) || (decl == Some("enum_declaration") && JAVA_ENUM_METHODS.contains(&name)) {
            return Err("external");
        }
        // the accessor of a record component is implicit
        if decl == Some("record_declaration") && self.java_field_names(id).contains(name) {
            return Err("field or key");
        }
        match self.java_base_state(id, 0) {
            (_, true) => Err("not found"),
            (true, false) => Err("external"),
            _ => Err("not found"),
        }
    }

    /// Whether class `id` extends or implements a type from outside the repository, and whether
    /// one of the types it extends could not be found, at any depth.
    fn java_base_state(&mut self, id: i64, depth: usize) -> (bool, bool) {
        let (mut outside, mut unknown) = (false, false);
        for b in self.java_bases(id).iter() {
            match *b {
                Ok(bid) if bid != id && depth < 8 => {
                    let (o, u) = self.java_base_state(bid, depth + 1);
                    outside |= o;
                    unknown |= u;
                }
                Ok(_) => {}
                Err("external") => outside = true,
                Err(_) => unknown = true,
            }
        }
        (outside, unknown)
    }

    /// The types class `id` extends and implements, each resolved.
    fn java_bases(&mut self, id: i64) -> Rc<Vec<std::result::Result<i64, &'static str>>> {
        if let Some(b) = self.java_bases.get(&id) {
            return Rc::clone(b);
        }
        // a class that comes round again through its own bases has none the second time
        self.java_bases.insert(id, Rc::new(Vec::new()));
        let mut out = Vec::new();
        if let Some((path, _)) = self.def_row(id) {
            let fi = self.file(&path);
            if let Some(node) = def_node(&fi, id) {
                let mut types: Vec<Node> = node.child_by_field_name("superclass").and_then(|s| s.named_child(0)).into_iter().collect();
                let mut cur = node.walk();
                for c in node.children(&mut cur).filter(|c| matches!(c.kind(), "super_interfaces" | "extends_interfaces")) {
                    let mut c2 = c.walk();
                    for list in c.named_children(&mut c2) {
                        let mut c3 = list.walk();
                        types.extend(list.named_children(&mut c3));
                    }
                }
                for t in types {
                    out.push(self.java_type_node(&fi, t));
                }
            }
        }
        let out = Rc::new(out);
        self.java_bases.insert(id, Rc::clone(&out));
        out
    }

    /// A method of an anonymous class body, else a member of the type it extends.
    fn java_anon_member(&mut self, fi: &FileInfo, anon: Node, name: &str) -> std::result::Result<i64, &'static str> {
        let mut cur = anon.walk();
        let Some(body) = anon.named_children(&mut cur).find(|c| c.kind() == "class_body") else { return Err("not found") };
        let (s, e) = (body.start_position().row as i64 + 1, body.end_position().row as i64 + 1);
        if let Some(d) = fi.defs.iter().find(|d| d.name == name && d.kind == "method" && s <= d.start && d.end <= e) {
            return Ok(d.id);
        }
        let t = anon.child_by_field_name("type").ok_or("not found")?;
        let base = self.java_type_node(fi, t)?;
        self.java_member(base, name)
    }

    /// One of the methods that share the qualified name of method `id` (overloads): the one the
    /// number of arguments picks, else the first, marked "overload" (it stands for them all).
    fn java_overload(&mut self, id: i64, arity: Option<usize>, why: &'static str) -> (i64, &'static str) {
        let all = self.overloads(id);
        if all.len() < 2 {
            return (id, why);
        }
        let Some((path, _)) = self.def_row(id) else { return (id, why) };
        let fi = self.file(&path);
        if let Some(n) = arity {
            let fits: Vec<i64> = all.iter().copied().filter(|&o| java_params(&fi, o).is_some_and(|(k, varargs)| k == n || (varargs && n + 1 >= k))).collect();
            if let [one] = fits[..] {
                return (one, why);
            }
        }
        (all[0], "overload")
    }

    /// The methods of the file of `id` with its qualified name, in source order: its overloads,
    /// itself included. Only `id` for a definition that has none.
    pub fn overloads(&mut self, id: i64) -> Vec<i64> {
        let Some((path, qualname)) = self.def_row(id) else { return vec![id] };
        let fi = self.file(&path);
        let all: Vec<i64> = fi.defs.iter().filter(|d| d.qualname == qualname && d.kind == "method").map(|d| d.id).collect();
        if all.contains(&id) {
            all
        } else {
            vec![id]
        }
    }

    /// A type named `name` where `at` is: a type parameter, a class around or nested in one
    /// (inherited ones included), a class of the file, an imported one, one of the package, one
    /// of a package imported with `*`.
    fn java_type(&mut self, fi: &FileInfo, at: Node, name: &str) -> std::result::Result<i64, &'static str> {
        if type_parameter(at, name, &fi.src) {
            return Err("type parameter");
        }
        let container = |s: &Self, id: i64| s.def_kind(id).is_some_and(|k| CONTAINER_KINDS.contains(&k.as_str()));
        for c in java_classes(fi, at) {
            let JClass::Named(id) = c else { continue };
            if fi.defs.iter().any(|d| d.id == id && d.name == name) {
                return Ok(id);
            }
            if let Some(m) = self.member(id, name, 0).filter(|&m| container(self, m)) {
                return Ok(m);
            }
        }
        if let Some(d) = top_level(fi, name).filter(|d| CONTAINER_KINDS.contains(&d.kind.as_str())) {
            return Ok(d.id);
        }
        if let Some(b) = fi.bindings.iter().find(|b| b.local == name && b.item.is_some()) {
            return self.java_binding(b);
        }
        if let Some(files) = self.java_package_files(&java_package(&fi.src)) {
            if let Some(id) = self.java_top(&files, name) {
                return Ok(id);
            }
        }
        for b in fi.bindings.iter().filter(|b| b.local == "*") {
            let found = match self.java_package_files(&b.module) {
                Some(files) => self.java_top(&files, name),
                None => self.java_path(&b.module).ok().and_then(|c| self.member(c, name, 0)).filter(|&m| container(self, m)),
            };
            if let Some(id) = found {
                return Ok(id);
            }
        }
        // `java.lang` and the packages a `*` imports from outside
        Err(if self.defined_anywhere(name) { "not found" } else { "external" })
    }

    /// The class a Java type expression names (`T`, `a.b.T`, `Outer.Inner<X>`).
    fn java_type_node(&mut self, fi: &FileInfo, t: Node) -> std::result::Result<i64, &'static str> {
        let mut segs = Vec::new();
        if !java_segments(t, &mut segs) {
            return Err("not found");
        }
        let (head, hname) = (segs[0], text(segs[0], &fi.src));
        let mut cur = match self.java_type(fi, head, hname) {
            Ok(id) => id,
            Err(e) if segs.len() == 1 => return Err(e),
            Err(_) => return self.java_path(&segs.iter().map(|s| text(*s, &fi.src)).collect::<Vec<_>>().join(".")),
        };
        for s in &segs[1..] {
            cur = self.member(cur, text(*s, &fi.src), 0).ok_or("not found")?;
        }
        Ok(cur)
    }

    /// The type of a Java variable: a local's declaration, else a field of a class around.
    fn java_variable_ty(&mut self, path: &str, fi: &FileInfo, e: Node) -> Option<Ty> {
        let src = fi.src.as_str();
        let name = text(e, src);
        if let Some(b) = java_local(e, name, src) {
            let (ty, value) = java_declared(b, src);
            return match ty {
                Some(t) => self.built_ty(path, fi, Lang::Java, b, value, e).or_else(|| self.ty_of_type(path, fi, Lang::Java, t)),
                None => self.type_of(path, fi, Lang::Java, value?),
            };
        }
        for c in java_classes(fi, e) {
            if let JClass::Named(id) = c {
                if self.java_has_field(id, name, 0) {
                    return self.java_field_ty(id, name, 0);
                }
            }
        }
        None
    }

    /// Whether `name` is a field of a class around `node`, inherited ones included.
    fn java_is_field(&mut self, fi: &FileInfo, node: Node, name: &str) -> bool {
        java_classes(fi, node).into_iter().any(|c| matches!(c, JClass::Named(id) if self.java_has_field(id, name, 0)))
    }

    fn java_has_field(&mut self, id: i64, name: &str, depth: usize) -> bool {
        if self.java_field_names(id).contains(name) {
            return true;
        }
        depth < 8 && self.java_bases(id).iter().any(|b| matches!(*b, Ok(bid) if bid != id && self.java_has_field(bid, name, depth + 1)))
    }

    /// The fields class `id` declares: field declarations, interface constants, enum constants
    /// and record components.
    fn java_field_names(&mut self, id: i64) -> Rc<HashSet<String>> {
        if let Some(f) = self.java_fields.get(&id) {
            return Rc::clone(f);
        }
        let mut names = HashSet::new();
        if let Some((path, _)) = self.def_row(id) {
            let fi = self.file(&path);
            if let Some(node) = def_node(&fi, id) {
                for (n, _) in java_field_decls(node) {
                    names.insert(text(n, &fi.src).to_string());
                }
            }
        }
        let names = Rc::new(names);
        self.java_fields.insert(id, Rc::clone(&names));
        names
    }

    /// The declared type of the field `name` of class `id`, inherited ones included; an enum
    /// constant has the type of its enum.
    fn java_field_ty(&mut self, id: i64, name: &str, depth: usize) -> Option<Ty> {
        let (path, _) = self.def_row(id)?;
        let fi = self.file(&path);
        let node = def_node(&fi, id)?;
        if let Some((_, ty)) = java_field_decls(node).into_iter().find(|(n, _)| text(*n, &fi.src) == name) {
            return match ty {
                Some(t) => self.ty_of_type(&path, &fi, Lang::Java, t),
                None => Some(Ty::Local(id)),
            };
        }
        if depth >= 8 {
            return None;
        }
        for b in self.java_bases(id).iter() {
            if let Ok(bid) = *b {
                if let Some(t) = self.java_field_ty(bid, name, depth + 1).filter(|_| bid != id) {
                    return Some(t);
                }
            }
        }
        None
    }
}

/// The fields a Java class declaration holds directly: (name, declared type; None for an enum constant).
fn java_field_decls(class: Node) -> Vec<(Node, Option<Node>)> {
    let mut out = Vec::new();
    if let Some(params) = class.child_by_field_name("parameters").filter(|_| class.kind() == "record_declaration") {
        let mut cur = params.walk();
        for p in params.named_children(&mut cur) {
            out.extend(p.child_by_field_name("name").map(|n| (n, p.child_by_field_name("type"))));
        }
    }
    let Some(body) = class.child_by_field_name("body") else { return out };
    let mut members: Vec<Node> = Vec::new();
    let mut cur = body.walk();
    for m in body.named_children(&mut cur) {
        if m.kind() == "enum_body_declarations" {
            let mut c2 = m.walk();
            members.extend(m.named_children(&mut c2));
        } else {
            members.push(m);
        }
    }
    for m in members {
        match m.kind() {
            "field_declaration" | "constant_declaration" => {
                let mut c2 = m.walk();
                for d in m.children_by_field_name("declarator", &mut c2) {
                    out.extend(d.child_by_field_name("name").map(|n| (n, m.child_by_field_name("type"))));
                }
            }
            "enum_constant" => out.extend(m.child_by_field_name("name").map(|n| (n, None))),
            _ => {}
        }
    }
    out
}

/// The number of parameters of a Java method or constructor, and whether its last is `...`.
fn java_params(fi: &FileInfo, id: i64) -> Option<(usize, bool)> {
    let params = def_node(fi, id)?.child_by_field_name("parameters")?;
    let mut cur = params.walk();
    let kinds: Vec<&str> = params.named_children(&mut cur).map(|p| p.kind()).filter(|k| *k != "receiver_parameter" && !k.ends_with("comment")).collect();
    Some((kinds.len(), kinds.last() == Some(&"spread_parameter")))
}

// ------------------------------------------------------------------------- refs
/// Whether two spellings are one name: they compare in NFC.
pub fn same_name(a: &str, b: &str) -> bool {
    use unicode_normalization::UnicodeNormalization;
    a == b || a.nfc().eq(b.nfc())
}

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
    /// by overloaded definition: the first of its overloads, which stands for them all where the
    /// arguments of a call do not pick one
    first_overload: HashMap<i64, i64>,
}

impl Resolved {
    pub fn of(&self, id: i64) -> impl Iterator<Item = &Occurrence> {
        let first = self.first_overload.get(&id).copied();
        self.occurrences.iter().filter(move |o| matches!(o.resolution, Ok((d, why)) if d == id || (why == "overload" && Some(d) == first)))
    }
    pub fn unresolved(&self) -> impl Iterator<Item = &Occurrence> {
        self.occurrences.iter().filter(|o| o.resolution.is_err())
    }
}

/// The Engine A search for the whole-word occurrences of `bare`, the first `max_results` in path
/// order, in its composed and its decomposed spelling: names compare in NFC, and a file may hold
/// either.
pub fn name_search(bare: &str, max_results: u64) -> crate::instant::Options {
    use unicode_normalization::UnicodeNormalization;
    let (nfc, nfd): (String, String) = (bare.nfc().collect(), bare.nfd().collect());
    let (pattern, mode) = if nfc == nfd {
        (bare.to_string(), crate::instant::Mode::Literal)
    } else {
        (format!("(?:{}|{})", regex::escape(&nfc), regex::escape(&nfd)), crate::instant::Mode::Regex)
    };
    crate::instant::Options { pattern, mode, word: true, max_results: Some(max_results), first_in_path_order: true, ..crate::instant::Options::default() }
}

/// Every whole-word occurrence of `name` (Engine A), each resolved to the definition it refers to.
pub fn resolved_references(s: &Searcher, name: &str, max_results: u64) -> Result<Resolved> {
    let bare = &crate::intel::bare_name(s, name)?;
    let o = crate::instant::Options { want_spans: true, ..name_search(bare, max_results) };
    let (hits, truncated) = crate::intel::occurrences(&s.ix.root, &o)?;
    let mut rv = Resolver::new(&s.ix)?;
    let mut occurrences = Vec::new();
    for h in hits {
        let line_text = h.text.trim_end_matches(['\n', '\r']).to_string();
        for &(a, b) in &h.spans {
            // the occurrence as this file spells it
            let resolution = rv.resolve_at(&h.path, h.line, a, h.text.get(a..b).unwrap_or(bare));
            occurrences.push(Occurrence { path: h.path.to_string(), line: h.line, column: a as u64 + 1, text: line_text.clone(), resolution });
        }
    }
    let mut definitions: Vec<ChunkRow> = crate::intel::definitions(&s.ix, bare)?.into_iter().filter(|d| same_name(&d.name, bare)).collect();
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
            if let [first, _, ..] = rv.overloads(d.id)[..] {
                first_overload.insert(d.id, first);
            }
        }
    }
    Ok(Resolved { definitions, occurrences, truncated, first_overload })
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
    fn a_bare_python_import_names_the_module_beside_the_file_when_no_root_module_has_the_name() {
        let t = "from helpers import make_items\nimport common\nimport sub.deep\nfrom pkg import w\nimport os\n\n\ndef test_it():\n    make_items()\n    common.fmt()\n    sub.deep.go()\n    w()\n    os.f()\n";
        let run = "import logging\nfrom nowhere import x\n\n\ndef main():\n    logging.info()\n    x()\n";
        let js = "import { u } from 'util';\n\nexport function f() {\n  return u();\n}\n";
        resolver_case(
            "pysib",
            &[
                ("tests/test_it.py", t),
                ("tests/helpers.py", "def make_items():\n    return [1]\n"),
                ("tests/common/__init__.py", "def fmt():\n    pass\n"),
                ("tests/sub/deep.py", "def go():\n    pass\n"),
                ("tests/pkg.py", "def w():\n    pass\n"),
                ("pkg/__init__.py", "def w():\n    pass\n"),
                ("scripts/run.py", run),
                ("scripts/logging.py", "def info():\n    pass\n"),
                ("web/app.js", js),
                ("web/util.js", "export function u() {\n  return 1;\n}\n"),
            ],
            &[
                ("tests/test_it.py", "import make_items", "make_items", "tests/helpers.py make_items"),
                ("tests/test_it.py", "    make_items()", "make_items", "tests/helpers.py make_items"),
                ("tests/test_it.py", "common.fmt()", "fmt", "tests/common/__init__.py fmt"),
                ("tests/test_it.py", "sub.deep.go()", "go", "tests/sub/deep.py go"),
                // a package path from the root wins over a sibling of the same name
                ("tests/test_it.py", "    w()", "w", "pkg/__init__.py w"),
                ("tests/test_it.py", "os.f()", "f", "external"),
                // Python puts the script's own directory first, ahead of the standard library
                ("scripts/run.py", "logging.info()", "info", "scripts/logging.py info"),
                ("scripts/run.py", "    x()", "x", "external"),
                // a bare JavaScript specifier is a package
                ("web/app.js", " u()", "u", "external"),
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
    fn a_call_is_typed_by_the_return_type_its_callee_declares() {
        let report = "pub struct Report;\n\nimpl Report {\n    pub fn load() -> Report {\n        Report\n    }\n    pub fn new() -> Self {\n        Report\n    }\n    pub fn ok(&self) -> bool {\n        true\n    }\n}\n\npub struct Builder;\n\nimpl Builder {\n    pub fn summary(&self) -> Report {\n        Report\n    }\n}\n\npub fn make() -> Builder {\n    Builder\n}\n\npub fn mystery(y: i32) -> Option<i32> {\n    Some(y)\n}\n\npub fn pick<T>(x: T) -> T {\n    x\n}\n\npub fn find() -> Option<Report> {\n    None\n}\n";
        let user = "use crate::report::{find, make, mystery, pick, Report};\n\npub fn a(y: i32) -> bool {\n    mystery(y).ok().is_some()\n}\n\npub fn b() -> bool {\n    Report::load().ok() && Report::new().ok() && make().summary().ok()\n}\n\npub fn c(file: &std::fs::File, v: Vec<Report>) -> bool {\n    file.metadata().ok().is_some() && v.first().unwrap().ok() && pick(Report).ok() && find().unwrap().ok()\n}\n";
        let rs = [("lib.rs", "mod report;\nmod user;\n"), ("report.rs", report), ("user.rs", user)];
        let ok = "report.rs Report.ok";
        resolver_case(
            "ret_rs",
            &rs,
            &[
                ("user.rs", "mystery(y).ok()", "ok", "external"),
                ("user.rs", "Report::load().ok()", "ok", ok),
                ("user.rs", "Report::new().ok()", "ok", ok),
                ("user.rs", "make().summary().ok()", "ok", ok),
                // a method of an outside type may return a repository type: `first` of a `Vec<Report>`
                ("user.rs", "file.metadata().ok()", "ok", "receiver"),
                ("user.rs", "v.first().unwrap().ok()", "ok", "receiver"),
                ("user.rs", "pick(Report).ok()", "ok", "receiver"),
                ("user.rs", "find().unwrap().ok()", "ok", "receiver"),
            ],
        );
        // a trait of the repository may give an outside type the method
        let okish = "pub trait Okish {\n    fn ok(&self) -> bool;\n}\n\nimpl<T: Copy> Okish for T {\n    fn ok(&self) -> bool {\n        true\n    }\n}\n";
        resolver_case(
            "ret_rs_trait",
            &[rs[0], rs[1], rs[2], ("okish.rs", okish)],
            &[("user.rs", "mystery(y).ok()", "ok", "receiver"), ("user.rs", "Report::load().ok()", "ok", ok)],
        );
        resolver_case(
            "ret_ts",
            &[
                ("report.ts", "export class Report {\n  static load(): Report { return new Report(); }\n  ok(): boolean { return true; }\n}\nexport class Builder {\n  summary(): Report { return new Report(); }\n}\nexport function make(): Builder { return new Builder(); }\nexport function mystery(y: number): number { return y; }\nexport function pick<T>(x: T): T { return x; }\n"),
                ("user.ts", "import { Report, make, mystery, pick } from './report';\n\nexport function a(y: number) {\n  return mystery(y).ok() || Report.load().ok() || make().summary().ok() || new Report().ok() || pick(new Report()).ok();\n}\n"),
                ("aug.ts", "export interface Flagged {\n  flag(): boolean;\n}\nexport function b() {\n  return \"s\".ok() || [1].ok() || \"t\".flag();\n}\n"),
            ],
            &[
                ("user.ts", "mystery(y).ok()", "ok", "external"),
                ("user.ts", "Report.load().ok()", "ok", "report.ts Report.ok"),
                ("user.ts", "make().summary().ok()", "ok", "report.ts Report.ok"),
                ("user.ts", "new Report().ok()", "ok", "report.ts Report.ok"),
                ("user.ts", "pick(new Report()).ok()", "ok", "receiver"),
                ("aug.ts", "\"s\".ok()", "ok", "external"),
                ("aug.ts", "[1].ok()", "ok", "external"),
                ("aug.ts", "\"t\".flag()", "flag", "receiver"),
            ],
        );
        resolver_case(
            "ret_js",
            &[
                ("r.js", "export class Report {\n  ok() { return true; }\n}\n"),
                ("use.js", "import { Report } from './r.js';\n\nfunction mystery() { return 1; }\n\nexport function a() {\n  return new Report().ok() + mystery().ok() + `t`.ok();\n}\n"),
            ],
            &[("use.js", "new Report().ok()", "ok", "r.js Report.ok"), ("use.js", "mystery().ok()", "ok", "receiver"), ("use.js", "`t`.ok()", "ok", "external")],
        );
        resolver_case(
            "ret_go",
            &[
                ("go.mod", "module example.com/m\n"),
                ("report/report.go", "package report\n\ntype Report struct{}\n\nfunc (r *Report) Ok() bool { return true }\n\nfunc NewReport() *Report { return &Report{} }\n\ntype Builder struct{}\n\nfunc (b Builder) Summary() Report { return Report{} }\n\nfunc Build() Builder { return Builder{} }\n\nfunc Mystery(y int) int { return y }\n"),
                ("main.go", "package main\n\nimport \"example.com/m/report\"\n\nfunc main() {\n\t_ = report.NewReport().Ok()\n\t_ = report.Build().Summary().Ok()\n\t_ = report.Mystery(1).Ok()\n}\n"),
            ],
            &[
                ("main.go", "NewReport().Ok()", "Ok", "report/report.go Report.Ok"),
                ("main.go", "Summary().Ok()", "Ok", "report/report.go Report.Ok"),
                ("main.go", "Mystery(1).Ok()", "Ok", "external"),
            ],
        );
        resolver_case(
            "ret_py",
            &[
                ("report.py", "class Report:\n    def ok(self):\n        return True\n\n\nclass Builder:\n    def summary(self) -> Report:\n        return Report()\n\n\ndef make() -> Builder:\n    return Builder()\n\n\ndef mystery(y) -> int:\n    return y\n\n\ndef unknown(y):\n    return y\n"),
                ("use.py", "from report import Report, make, mystery, unknown\n\n\ndef a(y):\n    return Report().ok() or make().summary().ok() or mystery(y).ok() or unknown(y).ok()\n"),
            ],
            &[
                ("use.py", "Report().ok()", "ok", "report.py Report.ok"),
                ("use.py", "summary().ok()", "ok", "report.py Report.ok"),
                ("use.py", "mystery(y).ok()", "ok", "external"),
                ("use.py", "unknown(y).ok()", "ok", "receiver"),
            ],
        );
    }

    #[test]
    fn this_in_a_method_of_an_object_literal_is_the_literal() {
        let js = "export const store = {\n  increment() {\n    return this.total();\n  },\n  total() {\n    return 1;\n  },\n  later: function () {\n    return this.total() + 1;\n  },\n  watch() {\n    return [1].map(() => this.total() + 2);\n  },\n  arrow: () => this.total() + 3,\n  sub: {\n    m() {\n      return this.n();\n    },\n    n() {},\n  },\n};\n\nexport default {\n  methods: {\n    save() {},\n    run() {\n      this.save();\n    },\n  },\n};\n";
        let ts = "export class K {\n  run() {\n    const o = { a() { return this.b(); }, b() {} };\n    return this.stop();\n  }\n  stop() {}\n}\n";
        resolver_case(
            "literal_this",
            &[("store.js", js), ("k.ts", ts)],
            &[
                ("store.js", "return this.total();", "total", "store.js store.total"),
                ("store.js", "this.total() + 1", "total", "store.js store.total"),
                ("store.js", "this.total() + 2", "total", "store.js store.total"),
                ("store.js", "this.total() + 3", "total", "receiver"),
                ("store.js", "this.n()", "n", "store.js store.sub.n"),
                ("store.js", "this.save()", "save", "store.js methods.save"),
                ("k.ts", "this.b()", "b", "k.ts K.run.o.b"),
                ("k.ts", "this.stop()", "stop", "k.ts K.stop"),
            ],
        );
    }

    #[test]
    fn names_in_a_rust_macro_follow_the_tokens_around_them() {
        let rs = "pub fn text() -> u8 {\n    1\n}\n\npub struct Hit {\n    pub text: u8,\n}\n\npub fn run(h: &Hit) -> Vec<Hit> {\n    let s = format!(\"{}\", h.text);\n    assert_eq!(h.text(), 1);\n    println!(\"{} {}\", text(), crate::text());\n    vec![Hit { text: 1 }]\n}\n";
        resolver_case(
            "macro_tokens",
            &[("src/lib.rs", rs), ("Cargo.toml", "[package]\nname = \"demo\"\n")],
            &[
                ("src/lib.rs", "h.text);", "text", "field or key"),
                ("src/lib.rs", "h.text()", "text", "receiver"),
                ("src/lib.rs", "{ text: 1 }", "text", "field or key"),
                ("src/lib.rs", "\"{} {}\", text()", "text", "src/lib.rs text"),
                ("src/lib.rs", "crate::text()", "text", "src/lib.rs text"),
            ],
        );
    }

    #[test]
    fn a_closure_parameter_or_let_in_a_macro_is_a_local() {
        let rs = "pub fn text() -> u8 {\n    1\n}\n\npub fn run(rows: &[(u8, u8)]) -> Vec<String> {\n    let a = format!(\"{:?}\", rows.iter().map(|(c, text)| c + text).collect::<Vec<_>>());\n    let b = format!(\"{} {}\", text(), { let text = 2; text });\n    vec![a, b]\n}\n\npub fn param(text: u8) -> String {\n    format!(\"{}\", text)\n}\n";
        resolver_case(
            "macro_locals",
            &[("src/lib.rs", rs), ("Cargo.toml", "[package]\nname = \"demo\"\n")],
            &[
                ("src/lib.rs", "|(c, text)|", "text", "local variable"),
                ("src/lib.rs", "c + text)", "text", "local variable"),
                ("src/lib.rs", "text(), {", "text", "src/lib.rs text"),
                ("src/lib.rs", "let text = 2", "text", "local variable"),
                ("src/lib.rs", "2; text }", "text", "local variable"),
                ("src/lib.rs", "\"{}\", text)", "text", "local variable"),
            ],
        );
    }

    #[test]
    fn a_variable_declared_as_a_base_type_has_the_type_its_constructor_builds() {
        let ts = "export class Base {\n  size() { return 0; }\n}\nexport class Big extends Base {\n  size() { return 9; }\n}\nexport function f(): number {\n  const a: Base = new Big();\n  let b: Base = new Big();\n  b = new Base();\n  const c: Base = pick();\n  return a.size() + b.size() + c.size();\n}\nexport function pick(): Base { return new Big(); }\n";
        let py = "class Base:\n    def size(self):\n        return 0\n\n\nclass Big(Base):\n    def size(self):\n        return 9\n\n\ndef f():\n    a: Base = Big()\n    c: Base = make()\n    return a.size() + c.size()\n\n\ndef make():\n    return Big()\n";
        let go = "package g\n\ntype Sizer interface{ Size() int }\n\ntype Big struct{}\n\nfunc (b *Big) Size() int { return 9 }\n\nfunc f() int {\n\tvar a Sizer = &Big{}\n\tvar b Sizer = Big{}\n\tb = nil\n\treturn a.Size() + b.Size()\n}\n";
        let rs = "pub trait Sizer {\n    fn size(&self) -> u8;\n}\n\npub struct Big;\n\nimpl Sizer for Big {\n    fn size(&self) -> u8 {\n        9\n    }\n}\n\npub fn f() -> u8 {\n    let a: Box<dyn Sizer> = Box::new(Big);\n    let b: &dyn Sizer = &Big {};\n    a.size() + b.size()\n}\n";
        let java = "class Base { int size() { return 0; } }\nclass Big extends Base { int size() { return 9; } }\nclass J {\n    int f() {\n        Base a = new Big();\n        Base b = new Big();\n        b = new Base();\n        return a.size() + b.size();\n    }\n}\n";
        resolver_case(
            "built",
            &[("t.ts", ts), ("p.py", py), ("g/g.go", go), ("src/lib.rs", rs), ("J.java", java)],
            &[
                ("t.ts", "a.size()", "size", "t.ts Big.size"),
                ("t.ts", "b.size()", "size", "t.ts Base.size"),
                ("t.ts", "c.size()", "size", "t.ts Base.size"),
                ("p.py", "a.size()", "size", "p.py Big.size"),
                ("p.py", "c.size()", "size", "p.py Base.size"),
                ("g/g.go", "a.Size()", "Size", "g/g.go Big.Size"),
                ("g/g.go", "b.Size()", "Size", "not found"),
                ("src/lib.rs", "a.size()", "size", "src/lib.rs Big.size"),
                ("src/lib.rs", "b.size()", "size", "src/lib.rs Big.size"),
                ("J.java", "a.size()", "size", "J.java Big.size"),
                ("J.java", "b.size()", "size", "J.java Base.size"),
            ],
        );
    }

    #[test]
    fn a_super_call_is_the_method_of_the_nearest_base_that_defines_it() {
        let ts = "import { Outer } from 'outside';\nexport class Base {\n  constructor(n: number) {}\n  size(): number { return 0; }\n}\nexport class Big extends Base {\n  constructor() { super(1); }\n  size(): number { return super.size() + 9; }\n  other(): number { return this.size(); }\n}\nexport class Bigger extends Big {\n  size(): number { return super.size() + 1; }\n}\nexport class Plain extends Big {\n  other(): number { return super.size() + 2; }\n}\nexport class Far extends Outer {\n  size(): number { return super.size() + 3; }\n}\nexport class Err extends Error {\n  size(): number { return super.size() + 4; }\n}\n";
        let js = "export class A {\n  m() { return 0; }\n}\nexport class B extends A {\n  m() { return super.m() + 1; }\n}\n";
        let py = "from outside import Thing\n\n\nclass Base:\n    def size(self):\n        return 0\n\n\nclass Big(Base):\n    def size(self):\n        return super().size() + 9\n\n    def other(self):\n        return self.size()\n\n\nclass Bigger(Big):\n    def size(self):\n        return super(Bigger, self).size() + 1\n\n    def skip(self):\n        return super(Big, self).size() + 2\n\n\nclass Left(Base):\n    pass\n\n\nclass Right(Base):\n    def size(self):\n        return 5\n\n\nclass Both(Left, Right):\n    def size(self):\n        return super().size() + 3\n\n    def load(self):\n        return super(Left, self).size() + 6\n\n    def odd(self):\n        return super(Bigger, self).size() + 7\n\n\nclass Far(Thing):\n    def size(self):\n        return super().size() + 4\n\n\nclass Alone:\n    def __init__(self):\n        super().__init__()\n";
        let go = "package g\n\ntype Base struct{}\n\nfunc (b *Base) Size() int { return 0 }\n\ntype Big struct {\n\tBase\n}\n\nfunc (b *Big) Size() int { return b.Base.Size() + 9 }\n";
        resolver_case(
            "super",
            &[("t.ts", ts), ("j.js", js), ("p.py", py), ("g/g.go", go)],
            &[
                ("t.ts", "super.size() + 9", "size", "t.ts Base.size"),
                ("t.ts", "super.size() + 1", "size", "t.ts Big.size"),
                ("t.ts", "super.size() + 2", "size", "t.ts Big.size"),
                ("t.ts", "this.size()", "size", "t.ts Big.size"),
                ("t.ts", "super.size() + 3", "size", "external"),
                ("t.ts", "super.size() + 4", "size", "external"),
                ("t.ts", "super(1)", "super", "t.ts Base"),
                ("j.js", "super.m()", "m", "j.js A.m"),
                ("p.py", "super().size() + 9", "size", "p.py Base.size"),
                ("p.py", "self.size()", "size", "p.py Big.size"),
                ("p.py", "super(Bigger, self).size()", "size", "p.py Big.size"),
                ("p.py", "super(Big, self).size()", "size", "p.py Base.size"),
                ("p.py", "super().size() + 3", "size", "p.py Right.size"),
                // the class after `Left` in the order of `Both`, the class the call sits in
                ("p.py", "super(Left, self).size() + 6", "size", "p.py Right.size"),
                // a class that is not in the order of `Both`: Python raises, nothing is decided
                ("p.py", "super(Bigger, self).size() + 7", "size", "receiver"),
                ("p.py", "super().size() + 4", "size", "external"),
                ("p.py", "super().__init__()", "__init__", "external"),
                ("g/g.go", "b.Base.Size()", "Size", "g/g.go Base.Size"),
            ],
        );
    }

    #[test]
    fn the_header_chunk_is_no_definition_wherever_it_sorts() {
        let (root, s) = indexed("header", &[("store.js", "export const store = {\n  total() {\n    return 1;\n  },\n};\n")]);
        let rv = Resolver::new(&s.ix).unwrap();
        let defs: Vec<String> = rv.defs_of("store.js").unwrap().iter().map(|d| format!("{}-{} {} {}", d.start, d.end, d.kind, d.qualname)).collect();
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
        assert!(defs.iter().all(|d| !d.contains(" module ")) && !defs.is_empty(), "{defs:?}");
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
    fn a_local_bound_by_a_pattern_import_or_initializer_hides_the_function_only_in_its_scope() {
        let ts = "export function target(): number {\n  return 1;\n}\nexport const a1 = ({ target }: { target: () => number }) => target() + 1;\nexport function a3({ x: target }: { x: () => number }) {\n  return target() + 3;\n}\nexport function a4({ target = () => 2 }) {\n  return target() + 4;\n}\nexport function a5(...target: Array<() => number>) {\n  return target[0]() + 5;\n}\nexport function a6(obj: { target: () => number }) {\n  const { target } = obj;\n  return target() + 6;\n}\nexport function a7(obj: any) {\n  const { a: { b: target }, ...rest } = obj;\n  return target() + 7;\n}\nexport function a9() {\n  try {\n    return 0;\n  } catch (target) {\n    return target + 9;\n  }\n  return target() + 90;\n}\nexport class K {\n  constructor(private target: () => number) {\n    target() + 10;\n  }\n}\nexport function a12(target: () => number) {\n  return [1].map(() => target() + 12);\n}\nexport function real() {\n  return target() + 99;\n}\n";
        let py = "def target():\n    return 1\n\n\ndef b5():\n    import os as target\n    return target.getcwd()\n\n\ndef b6():\n    from os import getcwd as target\n    return target() + 6\n\n\ndef b7(items):\n    if (target := len(items)) > 1:\n        return target + 7\n    return 0\n\n\ndef b8(target=target, *, k=lambda target=target: target):\n    return target() + 8\n\n\ndef b9(v):\n    match v:\n        case [target, *rest]:\n            return target() + 9\n        case P(k=target) | {\"k\": target}:\n            return target() + 90\n    return 0\n\n\ndef b10(a):\n    with a as (target, b):\n        return target() + 10\n\n\ndef b11(x: target) -> target:\n    return x\n\n\ndef b12(items):\n    out = [target for target in items]\n    return target() + 12\n\n\ndef b13():\n    from c import helper as target\n    return target() + 13\n\n\ndef b14(v):\n    match v:\n        case _ as target:\n            return target() + 14\n\n\ndef b15(target):\n    def inner(x=target, y=1):\n        return target() + 15\n    return inner\n\n\ndef b17():\n    def inner(target=target):\n        return target() + 17\n    return inner\n\n\ndef real():\n    return target() + 99\n";
        let rs = "pub fn target() -> u8 {\n    1\n}\n\npub struct P {\n    pub target: fn() -> u8,\n}\n\npub fn r2(o: Option<fn() -> u8>) -> u8 {\n    if let Some(target) = o {\n        return target() + 2;\n    } else {\n        return target() + 20;\n    }\n}\n\npub fn r3(o: Option<fn() -> u8>) -> u8 {\n    let x = match o {\n        Some(target) => target() + 3,\n        None => 0,\n    };\n    x + target() + 30\n}\n\npub fn r6(p: P) -> u8 {\n    let P { target } = p;\n    target() + 6\n}\n\npub fn r9(o: Option<fn() -> u8>) -> u8 {\n    let Some(target) = o else { return target() + 90 };\n    target() + 9\n}\n\npub fn r10(o: Option<fn() -> u8>) -> u8 {\n    while let Some(target) = o {\n        return target() + 10;\n    }\n    target() + 100\n}\n\npub fn r11(o: Option<(u8, fn() -> u8)>) -> u8 {\n    match o {\n        _w @ Some((_, mut target)) => target() + 11,\n        _ => 0,\n    }\n}\n\npub fn r12(p: &P) -> u8 {\n    let &P { ref target } = p;\n    target() + 12\n}\n\npub fn r13() -> u8 {\n    let target = target() + 13;\n    target\n}\n\npub fn r14(v: &[fn() -> u8]) -> u8 {\n    if let [target, ..] = v {\n        return target() + 14;\n    }\n    0\n}\n\npub fn r15(target: fn() -> u8) -> u8 {\n    let f = || target() + 15;\n    f()\n}\n\npub fn r16(target: u8) -> u8 {\n    fn inner() -> u8 {\n        target() + 16\n    }\n    inner() + target\n}\n\npub fn real() -> u8 {\n    target() + 99\n}\n";
        let go = "package gp\n\nfunc target() int { return 1 }\n\nfunc g4(x interface{}) int {\n\tswitch target := x.(type) {\n\tcase func() int:\n\t\treturn target() + 4\n\t}\n\treturn target() + 40\n}\n\nfunc g6(m map[string]func() int) int {\n\tif target, ok := m[\"a\"]; ok {\n\t\treturn target() + 6\n\t}\n\treturn target() + 60\n}\n\nfunc g7(ch chan func() int) int {\n\tselect {\n\tcase target := <-ch:\n\t\treturn target() + 7\n\t}\n\treturn target() + 70\n}\n\nfunc g8(target func() int) func() int {\n\treturn func() int { return target() + 8 }\n}\n\nfunc real() int { return target() + 99 }\n";
        let (local, ts_fn, py_fn, rs_fn, go_fn) = ("local variable", "b.ts target", "a.py target", "src/lib.rs target", "gp/a.go target");
        resolver_case(
            "locals",
            &[
                ("b.ts", ts),
                ("a.py", py),
                ("c.py", "def helper():\n    pass\n"),
                ("Cargo.toml", "[package]\nname = \"demo\"\n"),
                ("src/lib.rs", rs),
                ("go.mod", "module example.com/m\n"),
                ("gp/a.go", go),
            ],
            &[
                ("b.ts", "({ target }", "target", local),
                ("b.ts", "target() + 1;", "target", local),
                ("b.ts", "target() + 3", "target", local),
                ("b.ts", "target() + 4", "target", local),
                ("b.ts", "target[0]() + 5", "target", local),
                ("b.ts", "target() + 6", "target", local),
                ("b.ts", "target() + 7", "target", local),
                ("b.ts", "catch (target)", "target", local),
                ("b.ts", "target + 9", "target", local),
                ("b.ts", "target() + 90", "target", ts_fn),
                ("b.ts", "target() + 10", "target", local),
                ("b.ts", "target() + 12", "target", local),
                ("b.ts", "target() + 99", "target", ts_fn),
                ("a.py", "target.getcwd()", "target", "external"),
                ("a.py", "target() + 6", "target", "external"),
                ("a.py", "(target :=", "target", local),
                ("a.py", "target + 7", "target", local),
                ("a.py", "=target,", "target", py_fn),
                ("a.py", "=target:", "target", py_fn),
                ("a.py", ": target)", "target", local),
                ("a.py", "target() + 8", "target", local),
                ("a.py", "target() + 9", "target", local),
                ("a.py", "target() + 90", "target", local),
                ("a.py", "target() + 10", "target", local),
                ("a.py", "x: target", "target", py_fn),
                ("a.py", "-> target", "target", py_fn),
                ("a.py", "target() + 12", "target", py_fn),
                ("a.py", "target() + 13", "target", "c.py helper"),
                ("a.py", "target() + 14", "target", local),
                ("a.py", "x=target", "target", local),
                ("a.py", "target() + 15", "target", local),
                ("a.py", "inner(target=", "target", local),
                ("a.py", "=target):", "target", py_fn),
                ("a.py", "target() + 17", "target", local),
                ("a.py", "target() + 99", "target", py_fn),
                ("src/lib.rs", "Some(target) = o {", "target", local),
                ("src/lib.rs", "target() + 2;", "target", local),
                ("src/lib.rs", "target() + 20", "target", rs_fn),
                ("src/lib.rs", "target() + 3,", "target", local),
                ("src/lib.rs", "target() + 30", "target", rs_fn),
                ("src/lib.rs", "P { target }", "target", local),
                ("src/lib.rs", "target() + 6", "target", local),
                ("src/lib.rs", "target() + 90", "target", rs_fn),
                ("src/lib.rs", "    target() + 9", "target", local),
                ("src/lib.rs", "target() + 10", "target", local),
                ("src/lib.rs", "target() + 100", "target", rs_fn),
                ("src/lib.rs", "target() + 11", "target", local),
                ("src/lib.rs", "target() + 12", "target", local),
                ("src/lib.rs", "target() + 13", "target", rs_fn),
                ("src/lib.rs", "target() + 14", "target", local),
                ("src/lib.rs", "target() + 15", "target", local),
                ("src/lib.rs", "target() + 16", "target", rs_fn),
                ("src/lib.rs", "target() + 99", "target", rs_fn),
                ("gp/a.go", "switch target", "target", local),
                ("gp/a.go", "target() + 4", "target", local),
                ("gp/a.go", "target() + 40", "target", go_fn),
                ("gp/a.go", "target() + 6", "target", local),
                ("gp/a.go", "target() + 60", "target", go_fn),
                ("gp/a.go", "target() + 7", "target", local),
                ("gp/a.go", "target() + 70", "target", go_fn),
                ("gp/a.go", "target() + 8", "target", local),
                ("gp/a.go", "target() + 99", "target", go_fn),
            ],
        );
    }

    const JAVA_SHAPE: &str = "package com.acme.shapes;

public interface Shape {
    double area();

    default String label() {
        return \"shape \" + area();
    }
}
";
    const JAVA_CIRCLE: &str = "package com.acme.shapes;

import java.util.List;

public class Circle implements Shape {
    private final double r;
    private Report last;

    public Circle(double r) {
        this.r = r;
    }

    public double area() {
        return Math.PI * r * r;
    }

    public static Circle unit() {
        return new Circle(1);
    }

    public Report summary() {
        return new Report(area());
    }

    public boolean check(List<String> names) {
        names.add(label());
        return last.ok() && names.isEmpty();
    }

    public static class Inner {
        public int depth() { return 1; }
    }

    public enum Kind {
        SMALL, LARGE;

        public boolean big() {
            return this == LARGE;
        }
    }
}
";
    const JAVA_MAIN: &str = "package com.acme.app;

import com.acme.shapes.Circle;
import com.acme.shapes.Shape;
import com.acme.shapes.*;
import static com.acme.app.Util.scale;
import static com.acme.app.Util.*;
import java.util.List;
import java.util.function.Supplier;

public class Main extends Base {
    private Circle held = Circle.unit();

    void note(String s) {}

    void note(String s, int level) {}

    <T> T pick(T x) {
        return x;
    }

    void run(List<Shape> shapes, Shape given) {
        Shape s = Circle.unit();
        s.area();
        given.label();
        given.hashCode();
        var c = new Circle(2);
        c.area();
        held.area();
        this.held.summary();
        Circle.unit().summary().ok();
        new Square().area();
        note(\"a\");
        note(\"a\", 1);
        this.note(\"b\");
        Runnable all = this::note;
        scale(2);
        scale(2, 3);
        log(\"x\");
        shapes.forEach(x -> x.area());
        shapes.size();
        for (Shape each : shapes) {
            each.area();
        }
        Supplier<Circle> make = Circle::unit;
        Runnable r = new Runnable() {
            public void run() {
                note(\"x\");
                helper();
            }
        };
        try {
            inherited();
        } catch (IllegalStateException e) {
            e.getMessage();
        }
        Object o = given;
        if (o instanceof Circle k) {
            k.area();
        }
        new Circle.Inner().depth();
        Circle.Kind.SMALL.big();
        int area = 3;
        String t = \"x\";
        t.length();
        super.inherited();
        com.acme.shapes.Circle.unit();
    }

    void helper() {}
}
";

    #[test]
    fn java_rules() {
        let (shapes, app) = ("src/main/java/com/acme/shapes", "src/main/java/com/acme/app");
        let files: Vec<(String, &str)> = vec![
            (format!("{shapes}/Shape.java"), JAVA_SHAPE),
            (format!("{shapes}/Circle.java"), JAVA_CIRCLE),
            (format!("{shapes}/Square.java"), "package com.acme.shapes;\n\npublic class Square implements Shape {\n    public double area() {\n        return 4;\n    }\n\n    public String toString() {\n        return label();\n    }\n}\n"),
            (format!("{shapes}/Report.java"), "package com.acme.shapes;\n\npublic record Report(double value) {\n    public boolean ok() {\n        return value > 0;\n    }\n}\n"),
            (format!("{app}/Util.java"), "package com.acme.app;\n\npublic class Util {\n    public static int scale(int x) {\n        return x * 2;\n    }\n\n    public static int scale(int x, int y) {\n        return x * y;\n    }\n\n    public static void log(String s) {}\n}\n"),
            (format!("{app}/Base.java"), "package com.acme.app;\n\npublic class Base {\n    void inherited() {}\n}\n"),
            (format!("{app}/Main.java"), JAVA_MAIN),
        ];
        let files: Vec<(&str, &str)> = files.iter().map(|(p, b)| (p.as_str(), *b)).collect();
        let (root, s) = indexed("java", &files);
        let mut rv = Resolver::new(&s.ix).unwrap();
        // (file, text the name is in, name, "file:line qualname (how)" or why there is none)
        let checks: &[(&str, &str, &str, &str)] = &[
            ("Main", "import com.acme.shapes.Circle;", "Circle", "Circle:5 Circle (import)"),
            ("Main", "Util.scale;", "scale", "Util:4 Util.scale (overload)"),
            ("Main", "import java.util.List;", "List", "external"),
            ("Main", "import com.acme.shapes.Shape;", "acme", "package"),
            ("Main", "extends Base", "Base", "Base:3 Base (type)"),
            ("Main", "private Circle held", "Circle", "Circle:5 Circle (type)"),
            ("Main", "s.area()", "area", "Shape:4 Shape.area (member)"),
            ("Main", "given.label()", "label", "Shape:6 Shape.label (member)"),
            ("Main", "given.hashCode()", "hashCode", "external"),
            ("Main", "new Circle(2)", "Circle", "Circle:5 Circle (type)"),
            ("Main", "c.area()", "area", "Circle:13 Circle.area (member)"),
            ("Main", "held.area()", "area", "Circle:13 Circle.area (member)"),
            ("Main", "this.held.summary()", "summary", "Circle:21 Circle.summary (member)"),
            ("Main", "summary().ok()", "ok", "Report:4 Report.ok (member)"),
            ("Main", "new Square().area()", "area", "Square:4 Square.area (member)"),
            ("Main", "note(\"a\");", "note", "Main:14 Main.note (member)"),
            ("Main", "note(\"a\", 1)", "note", "Main:16 Main.note (member)"),
            ("Main", "this.note(\"b\")", "note", "Main:14 Main.note (member)"),
            ("Main", "this::note", "note", "Main:14 Main.note (overload)"),
            ("Main", "scale(2);", "scale", "Util:4 Util.scale (import)"),
            ("Main", "scale(2, 3)", "scale", "Util:8 Util.scale (import)"),
            ("Main", "log(\"x\")", "log", "Util:12 Util.log (import)"),
            ("Main", "x.area()", "area", "receiver"),
            ("Main", "shapes.size()", "size", "external"),
            ("Main", "each.area()", "area", "Shape:4 Shape.area (member)"),
            ("Main", "Circle::unit", "unit", "Circle:17 Circle.unit (member)"),
            ("Main", "note(\"x\")", "note", "Main:14 Main.note (member)"),
            ("Main", "helper();", "helper", "Main:70 Main.helper (member)"),
            ("Main", "    inherited();", "inherited", "Base:4 Base.inherited (member)"),
            ("Main", "e.getMessage()", "getMessage", "external"),
            ("Main", "k.area()", "area", "Circle:13 Circle.area (member)"),
            ("Main", "Circle.Inner()", "Inner", "Circle:30 Circle.Inner (qualified)"),
            ("Main", "Inner().depth()", "depth", "Circle:31 Circle.Inner.depth (member)"),
            ("Main", "SMALL.big()", "big", "Circle:37 Circle.Kind.big (member)"),
            ("Main", "SMALL.big()", "SMALL", "field or key"),
            ("Main", "int area = 3", "area", "local variable"),
            ("Main", "t.length()", "length", "external"),
            ("Main", "super.inherited()", "inherited", "Base:4 Base.inherited (super)"),
            ("Main", "shapes.Circle.unit()", "unit", "Circle:17 Circle.unit (member)"),
            ("Main", "shapes.Circle.unit()", "Circle", "Circle:5 Circle (qualified)"),
            ("Main", "T pick(T x)", "T", "type parameter"),
            ("Main", "return x;", "x", "local variable"),
            ("Circle", "names.add(label())", "add", "external"),
            ("Circle", "names.add(label())", "label", "Shape:6 Shape.label (member)"),
            ("Circle", "last.ok()", "ok", "Report:4 Report.ok (member)"),
            ("Circle", "Math.PI", "PI", "external"),
            ("Circle", "new Report(area())", "area", "Circle:13 Circle.area (member)"),
            ("Circle", "this == LARGE", "LARGE", "field or key"),
            ("Circle", "this.r = r", "r", "field or key"),
            ("Circle", "= r;", "r", "local variable"),
            ("Square", "return label();", "label", "Shape:6 Shape.label (member)"),
            ("Shape", "+ area()", "area", "Shape:4 Shape.area (member)"),
            ("Report", "value > 0", "value", "field or key"),
        ];
        let mut wrong = Vec::new();
        let file_of = |short: &str| files.iter().find(|(p, _)| p.ends_with(&format!("/{short}.java"))).unwrap();
        for (short, needle, name, want) in checks {
            let (path, src) = file_of(short);
            let (row, l) = src.lines().enumerate().find(|(_, l)| l.contains(needle)).unwrap_or_else(|| panic!("{needle:?} not in {short}"));
            let col = l.find(needle).unwrap() + needle.find(name).unwrap();
            let got = match rv.resolve_at(path, row as u64 + 1, col, name) {
                Ok((id, how)) => {
                    let c = s.ix.chunk(id).unwrap().unwrap();
                    let file = c.path.rsplit('/').next().unwrap().trim_end_matches(".java").to_string();
                    format!("{file}:{} {} ({how})", c.start, c.qualname)
                }
                Err(e) => e.to_string(),
            };
            if got != *want {
                wrong.push(format!("{short}: {needle} -> {got}, want {want}"));
            }
        }
        drop(s);
        let _ = std::fs::remove_dir_all(&root);
        assert!(wrong.is_empty(), "{}", wrong.join("\n"));
    }

    #[test]
    fn java_package_declarations() {
        assert_eq!(java_package("// c\n/* d */\n@Generated(\"x(y)\")\npackage a.b ;\nimport c;"), "a.b");
        assert_eq!(java_package("import c;\nclass K {}"), "");
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
