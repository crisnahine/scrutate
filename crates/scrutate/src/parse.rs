//! Source parsing: split files into symbol chunks with separate fields.
//!
//! Three tiers, so every text file is searchable and most code gets real symbols:
//! 1. tree-sitter grammars (langs.rs): precise definitions, signatures, docs, bodies, call edges;
//! 2. heuristic symbols for languages without a grammar: definition lines introduced by the usual
//!    keywords (`def`, `fn`, `function`, `class`, `sub`, ...), with the block below them;
//! 3. overlapping line windows for everything else.
//!
//! For the seven languages of the Python reference engine the output is identical to it.

use std::collections::{BTreeSet, HashMap};
use std::sync::LazyLock;

use regex::Regex;
use tree_sitter::{InputEdit, Node, Parser, Point, Tree};

use crate::langs::{self, head_word, ImportHow, LangSpec, NameRule};
use crate::text::split_lines;

#[derive(Debug, Clone, Default)]
pub struct Chunk {
    pub name: String,
    pub qualname: String,
    pub kind: &'static str,
    pub start: i64, // 1-based inclusive
    pub end: i64,
    pub text: String, // full text for display (containers truncated)
    pub sig: String,
    pub doc: String,
    pub body: String,
    pub calls: BTreeSet<String>,
    /// where `text` and `body` are cut from the source when they are first needed
    pub(crate) lazy: Option<Lazy>,
}

/// Source ranges of a definition's text and body (start bytes, their shared end, the line cap).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Lazy {
    text: usize,
    body: usize,
    end: usize,
    lines: usize,
}

impl Chunk {
    /// Cuts `text` and `body` from `src`. The index does it one chunk at a time, so a file of
    /// thousands of nested definitions never holds all their copies at once.
    pub fn fill(&mut self, src: &[u8]) {
        if let Some(l) = self.lazy.take() {
            self.text = head_bytes(src, l.text, l.end, l.lines);
            self.body = head_bytes(src, l.body, l.end, l.lines);
        }
    }
}

/// Everything extracted from one file.
#[derive(Debug, Default)]
pub struct Parsed {
    pub chunks: Vec<Chunk>,
    /// (1-based line, imported module as written, normalised)
    pub imports: Vec<(i64, String)>,
}

pub const CONTAINERS: &[&str] = &["class", "impl", "interface", "trait", "enum", "struct", "type", "module", "block", "section", "key"];
const COMMENT_TYPES: &[&str] =
    &["comment", "line_comment", "block_comment", "doc_comment", "documentation_comment", "comment_block", "statement_comment", "module_comment", "haddock"];
/// Nodes that list the statements of a body or file: a comment above the first one sits outside the list.
const BODY_LISTS: &[&str] =
    &["declarations", "statement_list", "non_empty_declarative_part", "binding_set", "body", "body_statement", "document", "block_node", "block_mapping"];
pub const MAX_CONTAINER_LINES: usize = 40;
pub const MAX_CHUNK_LINES: usize = 200;
/// Chunk text and body keep at most this many bytes (cut at a character, then "\n..."): a file
/// on one line with thousands of nested functions would otherwise hold its whole line per chunk.
/// The longest chunk of this crate's sources is 9 KB, of networkx 15 KB.
pub const MAX_CHUNK_BYTES: usize = 32 * 1024;

/// `s[..]` cut to `MAX_CHUNK_BYTES` at a character boundary, and whether it was cut.
fn cap_bytes(s: &str) -> (&str, bool) {
    if s.len() <= MAX_CHUNK_BYTES {
        return (s, false);
    }
    (&s[..s.floor_char_boundary(MAX_CHUNK_BYTES)], true)
}

/// Files that are text but not worth indexing (data, generated, lock files).
/// Never indexed by Engine B: generated/data text that is noise for symbol search, and binary formats
/// that would only cost a read before the NUL-byte check rejects them. Engine A has its own rules.
const EXCLUDED_EXTS: &[&str] = &[
    // data and generated text
    ".csv",
    ".tsv",
    ".log",
    ".min.js",
    ".min.css",
    ".map",
    ".lock",
    ".sum",
    ".snap",
    ".pem",
    ".crt",
    ".key",
    ".der",
    ".asc",
    ".sig",
    ".bin",
    ".dat",
    ".parquet",
    ".ipynb_checkpoints",
    // archives and compiled artefacts
    ".gz",
    ".tgz",
    ".zip",
    ".tar",
    ".xz",
    ".bz2",
    ".zst",
    ".7z",
    ".rar",
    ".jar",
    ".war",
    ".whl",
    ".egg",
    ".pyc",
    ".pyo",
    ".so",
    ".dylib",
    ".dll",
    ".exe",
    ".o",
    ".a",
    ".lib",
    ".class",
    ".wasm",
    ".rlib",
    ".pdb",
    // media and documents
    ".png",
    ".jpg",
    ".jpeg",
    ".gif",
    ".bmp",
    ".ico",
    ".icns",
    ".webp",
    ".tif",
    ".tiff",
    ".psd",
    ".mp3",
    ".mp4",
    ".wav",
    ".ogg",
    ".flac",
    ".mov",
    ".avi",
    ".mkv",
    ".webm",
    ".pdf",
    ".woff",
    ".woff2",
    ".ttf",
    ".otf",
    ".eot",
    ".sqlite",
    ".db",
];

pub fn basename(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Every text file is indexable unless its extension marks it as data; binary files are detected
/// by content when read.
pub fn is_indexable(path: &str) -> bool {
    let lower = basename(path).to_lowercase();
    !EXCLUDED_EXTS.iter().any(|e| lower.ends_with(e))
}

/// The grammar that parses this file, if any (shared extensions are decided by the text).
pub fn spec_for(path: &str, source: &str) -> Option<&'static LangSpec> {
    langs::sniff(path, source).or_else(|| langs::spec_for(path))
}

// ------------------------------------------------------------------------- helpers
static PYSTR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^[rRuUbBfF]{0,2}("""|'''|"|')"#).unwrap());
static COMMENT_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^\s*(/\*+|\*+/|\*|//+!?|<#|#>|#+|--\s?[|^]|--|;+|%+|'|!|"|\{-\|?|-\})\s?"#).unwrap());
static IDENT_START: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_$][\w$]*(?:-[A-Za-z_]\w*)*").unwrap());

fn expandtabs(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut col = 0usize;
    for c in s.chars() {
        match c {
            '\t' => {
                let n = 8 - (col % 8);
                out.extend(std::iter::repeat_n(' ', n));
                col += n;
            }
            '\n' | '\r' => {
                out.push(c);
                col = 0;
            }
            _ => {
                out.push(c);
                col += 1;
            }
        }
    }
    out
}

/// inspect.cleandoc: dedent uniformly, drop leading/trailing blank lines.
pub fn cleandoc(doc: &str) -> String {
    let expanded = expandtabs(doc);
    let mut lines: Vec<String> = expanded.split('\n').map(|s| s.to_string()).collect();
    let mut margin = usize::MAX;
    for line in &lines[1..] {
        let content = line.trim_start_matches(' ').chars().count();
        if content > 0 {
            margin = margin.min(line.chars().count() - content);
        }
    }
    if let Some(first) = lines.first_mut() {
        *first = first.trim_start_matches(' ').to_string();
    }
    if margin < usize::MAX {
        for line in lines.iter_mut().skip(1) {
            *line = line.chars().skip(margin).collect();
        }
    }
    while lines.last().map(|l| l.is_empty()).unwrap_or(false) {
        lines.pop();
    }
    while lines.first().map(|l| l.is_empty()).unwrap_or(false) {
        lines.remove(0);
    }
    lines.join("\n")
}

fn py_string_content(raw: &str) -> String {
    let Some(m) = PYSTR.captures(raw) else {
        return raw.to_string();
    };
    let q = m.get(1).unwrap().as_str();
    let mut inner = &raw[m.get(0).unwrap().end()..];
    if inner.ends_with(q) {
        inner = &inner[..inner.len() - q.len()];
    }
    cleandoc(inner)
}

pub fn clean_comment(raw: &str) -> String {
    let mut out = Vec::new();
    for ln in raw.split('\n') {
        let s = COMMENT_MARK.replace(ln, "");
        let s = s.trim_end_matches(['*', '/', ' ']).trim_end();
        if !s.trim().is_empty() {
            out.push(s.to_string());
        }
    }
    out.join("\n").trim().to_string()
}

/// The first `n` lines of `text`, at most `MAX_CHUNK_BYTES` of them, "\n..." when cut.
fn head(text: &str, n: usize) -> String {
    let end = text.match_indices('\n').nth(n - 1).map_or(text.len(), |(i, _)| i);
    let (s, cut) = cap_bytes(&text[..end]);
    if cut || end < text.len() {
        format!("{s}\n...")
    } else {
        s.to_string()
    }
}

/// `text` cut to `MAX_CHUNK_BYTES` ("\n..." when cut).
fn capped(text: String) -> String {
    match cap_bytes(&text) {
        (s, true) => format!("{s}\n..."),
        _ => text,
    }
}

pub fn window_chunks(source: &str) -> Vec<Chunk> {
    window_chunks_from(source, 0)
}

/// Overlapping 50-line windows (10 lines overlap) from 0-based line `first_line` on.
pub fn window_chunks_from(source: &str, first_line: usize) -> Vec<Chunk> {
    let (size, overlap) = (50usize, 10usize);
    let all = split_lines(source);
    let lines = &all[first_line.min(all.len())..];
    let mut out = Vec::new();
    let step = (size - overlap).max(1);
    let mut i = 0;
    while i < lines.len() {
        let seg = &lines[i..(i + size).min(lines.len())];
        if !seg.concat().trim().is_empty() {
            let t = capped(seg.join("\n"));
            out.push(Chunk {
                kind: "window",
                start: (first_line + i) as i64 + 1,
                end: (first_line + i + seg.len()) as i64,
                text: t.clone(),
                body: t,
                ..Default::default()
            });
        }
        if i + size >= lines.len() {
            break;
        }
        i += step;
    }
    out
}

/// Qualified names keep at most this many segments, so deep nesting stays linear.
pub const MAX_QUAL_DEPTH: usize = 32;

fn qualify(parent: &str, name: &str) -> String {
    if parent.is_empty() {
        return name.to_string();
    }
    let parent = if parent.matches('.').count() + 1 >= MAX_QUAL_DEPTH { parent.split_once('.').map_or(parent, |(_, r)| r) } else { parent };
    format!("{parent}.{name}")
}

/// `head` of src[a..b], copying only what it keeps.
fn head_bytes(src: &[u8], a: usize, b: usize, n: usize) -> String {
    let (a, b) = (a.min(src.len()), b.min(src.len()));
    let b = b.max(a);
    // past MAX_CHUNK_BYTES the text is cut anyway: scanning further makes nested chunks quadratic
    let lim = b.min(a + MAX_CHUNK_BYTES + 1);
    // as str, so the search runs on the standard library's memchr; `lim` may split a character
    let s = std::str::from_utf8(&src[a..lim]).unwrap_or_else(|e| std::str::from_utf8(&src[a..a + e.valid_up_to()]).unwrap_or_default());
    let end = s.match_indices('\n').nth(n - 1).map_or(b, |(i, _)| a + i);
    let mut cut = end < b;
    let mut e = end;
    if e - a > MAX_CHUNK_BYTES {
        e = a + MAX_CHUNK_BYTES;
        while e > a && src[e] & 0xC0 == 0x80 {
            e -= 1; // not inside a character
        }
        cut = true;
    }
    if cut {
        txt(src, a, e) + "\n..."
    } else {
        txt(src, a, e)
    }
}

/// End of the first line of src[a..b], at most 1 KB on: keywords sit at the start of a line.
pub fn first_line_end(src: &[u8], a: usize, b: usize) -> usize {
    let (a, b) = (a.min(src.len()), b.min(src.len()).min(a + 1024));
    src[a..b.max(a)].iter().position(|&c| c == b'\n').map_or(b, |i| a + i)
}

/// `s` without its template or generic arguments: `ns::Map<K, std::vector<V>>` gives `ns::Map`.
fn strip_generics(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut depth = 0usize;
    for c in s.chars() {
        match c {
            '<' => depth += 1,
            '>' if depth > 0 => depth -= 1,
            _ if depth == 0 => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

/// The owner a definition names outside its own body: a Go method's receiver type (`*Store[K]`
/// gives `Store`), the class of a C++ method defined out of line (`Widget::draw`), the table of
/// a Lua function (`M.sub.deep`), the namespace of an Elixir module (`MyApp.Accounts.User`).
/// A Rust impl is named by its bare type: `impl<T> crate::Stack<T>` gives `Stack`.
fn split_owner(lang: &str, node: Node, name: String, src: &[u8]) -> (Option<String>, String) {
    // `r#type` is the name `type`, as a call of it is
    let name = match name.strip_prefix("r#") {
        Some(raw) if lang == "rust" => raw.to_string(),
        _ => name,
    };
    if lang == "rust" && node.kind() == "impl_item" {
        let bare = name.split('<').next().unwrap_or(&name).rsplit("::").next().unwrap_or("").trim();
        return (None, if bare.is_empty() { name.clone() } else { bare.to_string() });
    }
    if lang == "lua" && node.kind() == "field" {
        return (lua_table_owner(node, src), name);
    }
    if lang == "lua" || lang == "elixir" {
        if let Some(i) = name.rfind(|c| c == '.' || lang == "lua" && c == ':') {
            if i > 0 && i + 1 < name.len() {
                return (Some(name[..i].replace(':', ".")), name[i + 1..].to_string());
            }
        }
        return (None, name);
    }
    if lang == "julia" {
        // `Base.show` is `show` owned by `Base`, `Base.:+` is `+`
        return match name.rsplit_once('.') {
            Some((owner, last)) if !owner.is_empty() && owner.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '!')) => {
                (Some(owner.to_string()), last.trim_start_matches(':').to_string())
            }
            Some((_, last)) => (None, last.to_string()),
            None => (None, name),
        };
    }
    if lang == "r" && node.kind() == "argument" {
        return (crate::langs::r6_class(node, src), name);
    }
    if matches!(lang, "javascript" | "typescript" | "tsx") && matches!(node.kind(), "method_definition" | "pair") {
        return (object_owner(node, src), name);
    }
    if matches!(lang, "javascript" | "typescript" | "tsx") && node.kind() == "assignment_expression" {
        if let Some((owner, last)) = name.rsplit_once('.') {
            return (Some(owner.to_string()), last.to_string());
        }
    }
    if lang == "go" && node.kind() == "method_declaration" {
        let mut stack: Vec<Node> = node.child_by_field_name("receiver").into_iter().collect();
        while let Some(n) = stack.pop() {
            if n.kind() == "type_identifier" {
                return (Some(txt(src, n.start_byte(), n.end_byte())), name);
            }
            let kids: Vec<Node> = n.children(&mut n.walk()).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    if matches!(lang, "c" | "cpp") {
        // `int& operator*()`: the declarator's reference or pointer mark is not part of the name
        let name = name.trim_start_matches(['&', '*', ' ']).to_string();
        if let Some(i) = name.rfind("::") {
            let owner = strip_generics(&name[..i]).replace("::", ".");
            if !owner.is_empty() {
                return (Some(owner), name[i + 2..].to_string());
            }
        }
        return (None, name);
    }
    (None, name)
}

/// The names that hold the object literal around a JS/TS method: `const store = { sub: { m() {} } }`
/// gives `store.sub`, `module.exports.tool = {...}` gives `tool`. None for a literal nothing names.
fn object_owner<'t>(node: Node<'t>, src: &[u8]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut obj = node.parent().filter(|p| p.kind() == "object")?;
    // `{...} as const`, `{...} satisfies T` and `({...})` are held by the name around them
    let unwrap = |mut n: Node<'t>| {
        while let (true, Some(p)) = (matches!(n.kind(), "as_expression" | "satisfies_expression" | "parenthesized_expression"), n.parent()) {
            n = p;
        }
        n
    };
    while let Some(holder) = obj.parent().map(unwrap) {
        let named = match holder.kind() {
            "pair" => holder.child_by_field_name("key").filter(|k| k.kind() == "property_identifier"),
            "variable_declarator" => holder.child_by_field_name("name").filter(|n| n.kind() == "identifier"),
            // `module.exports = {...}` exports its members under their own names
            "assignment_expression" => {
                holder.child_by_field_name("left").filter(|l| txt(src, l.start_byte(), l.end_byte()) != "module.exports").and_then(|l| match l.kind() {
                    "identifier" => Some(l),
                    "member_expression" => l.child_by_field_name("property"),
                    _ => None,
                })
            }
            _ => None,
        };
        let Some(n) = named else { break };
        parts.push(txt(src, n.start_byte(), n.end_byte()));
        match holder.parent().filter(|p| holder.kind() == "pair" && p.kind() == "object") {
            Some(p) => obj = p,
            None => break,
        }
    }
    parts.reverse();
    (!parts.is_empty()).then(|| parts.join("."))
}

/// The names that hold the Lua table around a field: `local obj = { sub = { f = function ...` gives
/// `obj.sub`. None for a table nothing names.
fn lua_table_owner(node: Node, src: &[u8]) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    let mut table = node.parent().filter(|p| p.kind() == "table_constructor")?;
    while let Some(holder) = table.parent() {
        if holder.kind() == "field" {
            let Some(n) = holder.child_by_field_name("name").filter(|n| n.kind() == "identifier") else { break };
            parts.push(txt(src, n.start_byte(), n.end_byte()));
            match holder.parent().filter(|p| p.kind() == "table_constructor") {
                Some(p) => table = p,
                None => break,
            }
            continue;
        }
        let assignment = holder.parent().filter(|a| holder.kind() == "expression_list" && a.kind() == "assignment_statement");
        if let Some(name) =
            assignment.and_then(crate::langs::lua_single_assignment).filter(|(_, v)| *v == table).and_then(|(t, _)| crate::langs::lua_target(t, src))
        {
            parts.push(name);
        }
        break;
    }
    parts.reverse();
    (!parts.is_empty()).then(|| parts.join("."))
}

/// The name of a file's anonymous default export: the file's stem, or its directory's name for an index file.
pub(crate) fn default_export_name(path: &str) -> String {
    let p = std::path::Path::new(path);
    let stem = p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    match p.parent().and_then(|d| d.file_name()) {
        Some(dir) if stem == "index" => dir.to_string_lossy().into_owned(),
        _ => stem,
    }
}

fn txt(src: &[u8], a: usize, b: usize) -> String {
    String::from_utf8_lossy(&src[a.min(src.len())..b.min(src.len())]).into_owned()
}

/// A Lisp symbol in call position: one name whatever it holds (`list->vec`, `set-car!`, `empty?`).
static LISP_SYMBOL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^[^\s()\[\]{}"'`,;#\d][^\s()\[\]{}"'`,;]*"#).unwrap());

fn callee_name(lang: &str, node: Node, field: &str, src: &[u8]) -> Option<String> {
    let mut target =
        if field.is_empty() { None } else { node.child_by_field_name(field).or_else(|| node.children(&mut node.walk()).find(|c| c.kind() == field)) };
    if target.is_none() && node.named_child_count() > 0 {
        target = node.named_child(0);
    }
    let mut target = target?;
    match (lang, node.kind()) {
        // `$(call g,1)` calls `g`; `$(subst ...)` and the other built-ins are no calls
        ("make", "function_call") => {
            let f = node.child_by_field_name("function")?;
            if txt(src, f.start_byte(), f.end_byte()) != "call" {
                return None;
            }
            let args = txt(src, target.start_byte(), target.end_byte());
            return IDENT_START.find(args.split(',').next()?.trim()).map(|m| m.as_str().to_string());
        }
        ("make", "raw_text") => return None,
        ("verilog", "data_declaration") => {
            // only the typeless declaration that an ERROR holding the arguments follows is a call
            let next = target.next_named_sibling();
            let args = next.filter(|n| n.kind() == "ERROR" && src.get(n.start_byte()) == Some(&b'('));
            if node.child(0) != Some(target) || target.kind() != "list_of_variable_decl_assignments" || target.named_child_count() != 1 || args.is_none() {
                return None;
            }
        }
        _ => {}
    }
    if target.kind() == "method_index_expression" {
        // Lua `obj:meth(x)` calls `meth`
        target = target.child_by_field_name("method")?;
    }
    if target.kind() == "generic_function" {
        // `f::<T>(x)`: the name is before the turbofish
        target = target.child_by_field_name("function")?;
    }
    if target.kind() == "extract_operator" {
        // R `obj$meth(x)` calls `meth`
        target = target.child_by_field_name("rhs")?;
    }
    let mut t = txt(src, target.start_byte(), target.end_byte());
    if matches!(lang, "scheme" | "racket" | "elisp" | "commonlisp") {
        // an operator such as `-` or `<=` is not a name; a Common Lisp package prefix is not part of it
        let name = LISP_SYMBOL.find(t.trim())?.as_str();
        let name = if lang == "commonlisp" { name.rsplit(':').next().unwrap_or(name) } else { name };
        return name.chars().any(char::is_alphabetic).then(|| name.to_string());
    }
    // D and Solidity `new Point(1, 2)` is a call whose callee is `new Point`
    if let Some(rest) = t.strip_prefix("new").filter(|r| r.starts_with(char::is_whitespace)) {
        t = rest.trim_start().to_string();
    }
    // a dot is part of an R name and `::` part of a Bash one
    let seps: &[&str] = match lang {
        "r" => &["::", "->"],
        "bash" => &["->"],
        "php" => &["::", ".", "->", "\\"],
        _ => &["::", ".", "->"],
    };
    for sep in seps {
        if let Some(i) = t.rfind(sep) {
            t = t[i + sep.len()..].to_string();
        }
    }
    let t = t.trim().trim_start_matches('@');
    // a `!` is cut for a Rust macro; in Ruby, Elixir and Julia it ends the name
    let t = if matches!(lang, "ruby" | "elixir" | "julia") { t } else { t.trim_end_matches('!') };
    // a JavaScript private method is named with its `#`
    if let Some(private) = t.strip_prefix('#').filter(|_| matches!(lang, "javascript" | "typescript" | "tsx")) {
        return IDENT_START.find(private).map(|m| format!("#{}", m.as_str()));
    }
    let t = if lang == "rust" { t.strip_prefix("r#").unwrap_or(t) } else { t };
    // Julia `==(a, b)` and `Base.:(==)(a, b)` call the operator a method definition names
    let op = t.trim_start_matches(':').trim_start_matches('(').trim_end_matches(')');
    if lang == "julia" && !op.is_empty() && op.chars().all(|c| c.is_ascii_punctuation()) {
        return Some(op.to_string());
    }
    let name = match lang {
        "ruby" | "elixir" => &*PREDICATE_NAME,
        "julia" => &*BANG_NAME,
        "r" => &*R_NAME,
        "bash" => &*BASH_NAME,
        "haskell" | "ocaml" | "fsharp" | "nix" => &*PRIMED_NAME,
        _ => &*IDENT_START,
    };
    name.find(t).map(|m| m.as_str().to_string())
}

// the name characters some languages allow past a plain identifier, as their definitions keep them
static PREDICATE_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*[?!]?").unwrap());
static BANG_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_]\w*!?").unwrap());
static R_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z.][\w.]*").unwrap());
static BASH_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w$]*(?:(?:-|::|\.)[A-Za-z_]\w*)*").unwrap());
static PRIMED_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][\w']*(?:-[A-Za-z_][\w']*)*").unwrap());

const ASM_CALLS: &[&str] = &["call", "callq", "calll", "bl", "jal"];
const ASM_SECTIONS: &[&str] = &[".text", ".data", ".bss", ".rodata", ".section"];

/// The last node of an assembly label's block, the instructions after it up to the next label
/// or section directive, and the calls among them. A local label (`.L2:`) does not end a global one.
fn asm_block<'t>(label: Node<'t>, siblings: &[Node<'t>], at: usize, src: &[u8], calls: &mut BTreeSet<String>) -> Node<'t> {
    let text_of = |n: Node| txt(src, n.start_byte(), n.end_byte());
    let local = |n: Node| text_of(n).starts_with(|c: char| c == '.' || c.is_ascii_digit());
    let in_local = local(label);
    let mut last = label;
    for &s in siblings.iter().skip(at + 1) {
        let word = || s.child_by_field_name("kind").map(|k| text_of(k).to_lowercase()).unwrap_or_default();
        match s.kind() {
            "label" if in_local || !local(s) => break,
            "meta" if ASM_SECTIONS.contains(&word().as_str()) => break,
            "line_comment" | "block_comment" => continue,
            _ if !s.is_named() => continue,
            "instruction" if ASM_CALLS.contains(&word().as_str()) => {
                let target = s.named_children(&mut s.walk()).find(|c| c.kind() == "ident");
                calls.extend(target.and_then(|t| IDENT_START.find(&text_of(t)).map(|m| m.as_str().to_string())));
            }
            _ => {}
        }
        last = s;
    }
    last
}

/// `$(call g,1)` or `${call g}` in text the Make grammar leaves unparsed.
static MAKE_CALL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\$[({]call\s+([A-Za-z_][\w-]*)").unwrap());

/// The callee of a Jsonnet `suffix_apply`, which follows its callee as a sibling: `g` of `g(x)`,
/// `method` of `obj.method(x)`.
fn sibling_callee(siblings: &[Node], at: usize, src: &[u8]) -> Option<String> {
    let prev = siblings.get(at.checked_sub(1)?)?;
    let name = match prev.kind() {
        "ident_ref" => *prev,
        "suffix_index" => prev.named_child(0).filter(|n| n.kind() == "ident")?,
        _ => return None,
    };
    Some(txt(src, name.start_byte(), name.end_byte()))
}

/// A VHDL name with arguments, `f(x)` or `work.pkg.f(x)`, outside a type (`std_logic_vector(7 downto 0)`).
fn vhdl_callee(node: Node, parent: Node, src: &[u8]) -> Option<String> {
    if parent.kind() == "subtype_indication" {
        return None;
    }
    let kids: Vec<Node> = node.named_children(&mut node.walk()).collect();
    let args = kids.iter().position(|k| k.kind() == "parenthesis_group").filter(|&i| i > 0)?;
    let name = match kids[args - 1].kind() {
        "identifier" => kids[args - 1],
        "selection" => kids[args - 1].named_child(0).filter(|n| n.kind() == "identifier")?,
        _ => return None,
    };
    Some(txt(src, name.start_byte(), name.end_byte()))
}

/// The part of a definition that names it or lists its parameters and that the grammar parses as
/// a call: Elixir `def f(x)`, Scheme `(define (f x) ...)`, a Lisp parameter list `(x y)`.
fn def_head<'t>(lang: &str, node: Node<'t>) -> Option<Node<'t>> {
    match lang {
        "elixir" => {
            let head = node.children(&mut node.walk()).find(|c| c.kind() == "arguments")?.named_child(0)?;
            if head.kind() == "binary_operator" {
                head.child_by_field_name("left")
            } else {
                Some(head)
            }
        }
        "scheme" | "racket" => node.named_child(1).filter(|n| n.kind() == "list"),
        "elisp" => node.child_by_field_name("parameters"),
        "commonlisp" => node.named_child(0)?.child_by_field_name("lambda_list"),
        _ => None,
    }
}

/// 0-based last line holding the node's text: a node that ends with its line break (a C
/// `#define`) ends at column 0 of the next line.
fn last_row(node: Node) -> i64 {
    let end = node.end_position();
    (end.row - usize::from(end.column == 0 && end.row > node.start_position().row)) as i64
}

/// Calls inside a Rust macro's token trees, which tree-sitter does not parse as expressions: a
/// name directly followed by `(...)` (`a::b(x)`, `x.m(y)`), and the name of a nested macro.
fn macro_arg_calls(node: Node, src: &[u8], out: &mut BTreeSet<String>) {
    // after the first comma of `matches!(x, Some(_))` comes a pattern, not calls
    let is_matches = |n: Node| txt(src, n.start_byte(), n.end_byte()).ends_with("matches");
    let pattern_after_comma = node.child_by_field_name("macro").is_some_and(is_matches);
    let mut stack: Vec<(Node, bool)> = node.children(&mut node.walk()).filter(|c| c.kind() == "token_tree").map(|c| (c, pattern_after_comma)).collect();
    while let Some((tt, stop_at_comma)) = stack.pop() {
        let kids: Vec<Node> = tt.children(&mut tt.walk()).collect();
        let kind = |i: usize| kids.get(i).map_or("", |k| k.kind());
        for (i, k) in kids.iter().enumerate() {
            match k.kind() {
                "," if stop_at_comma => break,
                // `#[derive(Debug)]` and `#![...]` are attributes, not calls
                "token_tree" if !(i >= 1 && kind(i - 1) == "#" || i >= 2 && kind(i - 1) == "!" && kind(i - 2) == "#") => {
                    stack.push((*k, i >= 2 && kind(i - 1) == "!" && kind(i - 2) == "identifier" && is_matches(kids[i - 2])));
                }
                "identifier" => {
                    let parens = kind(i + 1) == "token_tree" && kids[i + 1].child(0).is_some_and(|p| p.kind() == "(");
                    let declared = i >= 1 && matches!(kind(i - 1), "fn" | "struct" | "enum" | "union");
                    let nested_macro = kind(i + 1) == "!" && kind(i + 2) == "token_tree";
                    if parens && !declared || nested_macro {
                        out.insert(txt(src, k.start_byte(), k.end_byte()));
                    }
                }
                _ => {}
            }
        }
    }
}

static CAPS_NAME: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^_*[A-Z][A-Z0-9_]*$").unwrap());

/// The constants declared at `node`: their names and the node whose text each chunk shows. Constants are
/// module or type level: Rust `const`/`static`, Go package `const`/`var`, a JS/TS top-level
/// `const` that is not a function, a TS enum member, a Java `static final` field, a Python
/// module- or class-level ALL_CAPS assignment, a TS `static readonly` and JS ALL_CAPS `static`
/// class field, a C/C++ `#define NAME value`, enumerator and file or namespace `const` with a value
/// (GLSL and Objective-C too), a C++ `static const` or `constexpr` class member with a value, and a
/// Ruby assignment to a capitalized name at file, module or class level.
fn constant_of<'t>(lang: &str, node: Node<'t>, ancestors: &[(Node<'t>, Vec<Node<'t>>)], src: &[u8]) -> Option<(Vec<String>, Node<'t>)> {
    let up = |i: usize| ancestors.len().checked_sub(i).map(|j| ancestors[j].0);
    let kind_up = |i: usize| up(i).map_or("", |n| n.kind());
    let text_of = |n: Node| txt(src, n.start_byte(), n.end_byte());
    let field = |n: Node<'t>, f: &str| n.child_by_field_name(f);
    let (names, shown) = match (lang, node.kind()) {
        ("rust", "const_item" | "static_item") if matches!(kind_up(1), "source_file" | "declaration_list") => (vec![text_of(field(node, "name")?)], node),
        ("go", "const_spec" | "var_spec") => {
            let at = if kind_up(1) == "var_spec_list" { 2 } else { 1 };
            if !matches!(kind_up(at), "const_declaration" | "var_declaration") || kind_up(at + 1) != "source_file" {
                return None;
            }
            let decl = up(at)?;
            // `const A, B = 1, 2` declares both names; the field also holds the commas
            let names: Vec<String> = node.children_by_field_name("name", &mut node.walk()).filter(|n| n.kind() == "identifier").map(text_of).collect();
            (names, if at == 1 && decl.named_child_count() == 1 { decl } else { node })
        }
        ("javascript" | "typescript" | "tsx", "variable_declarator") => {
            let decl = up(1).filter(|d| d.kind() == "lexical_declaration" && d.child(0).is_some_and(|k| k.kind() == "const"))?;
            let shown = match kind_up(2) {
                "program" => decl,
                "export_statement" if kind_up(3) == "program" => up(2)?,
                _ => return None,
            };
            let name = field(node, "name").filter(|n| n.kind() == "identifier")?;
            if field(node, "value").is_some_and(|v| crate::langs::JS_FUNCS.contains(&v.kind())) {
                return None;
            }
            (vec![text_of(name)], shown)
        }
        ("typescript" | "tsx", "property_identifier") if kind_up(1) == "enum_body" => (vec![text_of(node)], node),
        ("typescript" | "tsx", "enum_assignment") if kind_up(1) == "enum_body" => (vec![text_of(field(node, "name")?)], node),
        ("java", "field_declaration") => {
            let mods = node.children(&mut node.walk()).find(|c| c.kind() == "modifiers")?;
            let words: Vec<&str> = mods.children(&mut mods.walk()).map(|c| c.kind()).collect();
            if !(words.contains(&"static") && words.contains(&"final")) {
                return None;
            }
            let names: Vec<String> = node.children_by_field_name("declarator", &mut node.walk()).filter_map(|d| field(d, "name")).map(text_of).collect();
            (names, node)
        }
        ("python", "expression_statement")
            if (kind_up(1) == "module" || kind_up(1) == "block" && kind_up(2) == "class_definition") && node.named_child_count() == 1 =>
        {
            let a = node.named_child(0).filter(|a| a.kind() == "assignment")?;
            let left = field(a, "left")?;
            let names = match left.kind() {
                "identifier" => vec![text_of(left)],
                "pattern_list" | "tuple_pattern" | "list_pattern" => {
                    left.named_children(&mut left.walk()).filter(|n| n.kind() == "identifier").map(text_of).collect()
                }
                _ => return None,
            };
            (names, node)
        }
        ("javascript" | "typescript" | "tsx", "field_definition" | "public_field_definition") if kind_up(1) == "class_body" => {
            let words: Vec<&str> = node.children(&mut node.walk()).map(|c| c.kind()).collect();
            // JavaScript has no `readonly`: an ALL_CAPS static field is its constant
            let ts = lang != "javascript";
            if !words.contains(&"static") || ts && !words.contains(&"readonly") {
                return None;
            }
            let name = field(node, "name").or_else(|| field(node, "property")).filter(|n| n.kind() == "property_identifier")?;
            let name = text_of(name);
            if !ts && !CAPS_NAME.is_match(&name) {
                return None;
            }
            (vec![name], node)
        }
        ("c" | "cpp" | "glsl" | "objc", "declaration") => {
            // an include guard or `#if` block is no scope of its own
            let scope = ancestors.iter().rev().map(|(p, _)| p.kind()).find(|k| !k.starts_with("preproc_"));
            if !matches!(scope, Some("translation_unit" | "declaration_list" | "linkage_specification")) {
                return None;
            }
            let is_const = |n: Node| n.children(&mut n.walk()).any(|c| c.kind() == "type_qualifier" && matches!(text_of(c).as_str(), "const" | "constexpr"));
            let const_type = is_const(node);
            // only a declarator with a value: `extern const int X;` defines nothing; `T *const p` is constant too
            let names: Vec<String> = node
                .children_by_field_name("declarator", &mut node.walk())
                .filter(|d| d.kind() == "init_declarator")
                .filter(|d| const_type || field(*d, "declarator").is_some_and(|p| p.kind() == "pointer_declarator" && is_const(p)))
                .filter_map(|d| dig(d, "declarator", &["identifier", "qualified_identifier"]))
                .filter(|n| matches!(n.kind(), "identifier" | "qualified_identifier"))
                .map(text_of)
                .collect();
            (names, node)
        }
        ("cpp", "field_declaration") if kind_up(1) == "field_declaration_list" => {
            let words: Vec<String> =
                node.children(&mut node.walk()).filter(|c| matches!(c.kind(), "storage_class_specifier" | "type_qualifier")).map(text_of).collect();
            if !words.iter().any(|w| w == "static") || !words.iter().any(|w| w == "const" || w == "constexpr") {
                return None;
            }
            // a member declared without its value is defined, and is the constant, outside the class
            let mut names = Vec::new();
            let mut cur = node.walk();
            let mut declarator = None;
            for (i, c) in node.children(&mut cur).enumerate() {
                match node.field_name_for_child(i as u32) {
                    Some("declarator") => declarator = Some(c),
                    Some("default_value") => names.extend(declarator.take().and_then(|d| dig(d, "declarator", &["field_identifier"]).or(Some(d)))),
                    _ => {}
                }
            }
            (names.into_iter().filter(|n| n.kind() == "field_identifier").map(text_of).collect(), node)
        }
        ("c" | "cpp", "preproc_def") if field(node, "value").is_some() => (vec![text_of(field(node, "name")?)], node),
        ("c" | "cpp", "enumerator") => (vec![text_of(field(node, "name")?)], node),
        // a capitalized name is a constant in Ruby
        ("ruby", "assignment") if kind_up(1) == "program" || kind_up(1) == "body_statement" && matches!(kind_up(2), "class" | "module") => {
            (vec![text_of(field(node, "left").filter(|l| l.kind() == "constant")?)], node)
        }
        _ => return None,
    };
    let names: Vec<String> = names.into_iter().filter(|n| n != "_" && (lang != "python" || CAPS_NAME.is_match(n))).collect();
    (!names.is_empty()).then_some((names, shown))
}

/// The chunk kind named by the first declaration keyword on the node's first line.
fn kind_from_head(node: Node, src: &[u8]) -> Option<&'static str> {
    let t = txt(src, node.start_byte(), first_line_end(src, node.start_byte(), node.end_byte()));
    let line = t.lines().next().unwrap_or("");
    for w in line.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '{' | ':' | '<' | ',' | '[')) {
        if let Some((_, k)) = crate::langs::KIND_WORDS.iter().find(|(kw, _)| *kw == w) {
            return Some(k);
        }
    }
    None
}

/// The block of an Objective-C method definition, which the grammar gives no `body` field.
fn objc_method_body(node: Node) -> Option<Node> {
    let def = node.named_child(0).filter(|_| node.kind() == "implementation_definition")?;
    def.children(&mut def.walk()).find(|c| c.kind() == "compound_statement")
}

fn dig<'t>(mut node: Node<'t>, field: &str, kinds: &[&str]) -> Option<Node<'t>> {
    let mut last = None;
    // the `(*cb)` of `typedef int (*cb)(int);` holds its declarator as a plain child
    let inner = |n: Node<'t>| n.named_children(&mut n.walk()).find(|c| c.kind().ends_with("_declarator") || kinds.contains(&c.kind()));
    while let Some(ch) = node.child_by_field_name(field).or_else(|| if node.kind() == "parenthesized_declarator" { inner(node) } else { None }) {
        if kinds.contains(&ch.kind()) {
            return Some(ch);
        }
        last = Some(ch);
        node = ch;
    }
    if kinds.is_empty() {
        return last;
    }
    // the innermost node reached may itself contain the identifier as a child (parenthesized declarators)
    last.and_then(|n| {
        let mut cur = n.walk();
        let found = n.children(&mut cur).find(|c| kinds.contains(&c.kind()));
        found
    })
    .or(last)
}

fn def_name(node: Node, rules: &[NameRule], src: &[u8]) -> Option<String> {
    for rule in rules {
        let got = match rule {
            NameRule::Field(f) => node.child_by_field_name(f).map(|n| txt(src, n.start_byte(), n.end_byte())),
            NameRule::Dig(f, kinds) => dig(node, f, kinds).map(|n| txt(src, n.start_byte(), n.end_byte())),
            NameRule::ChildKind(kinds) => {
                let mut cur = node.walk();
                let found = node.children(&mut cur).find(|c| kinds.contains(&c.kind()));
                found.map(|n| txt(src, n.start_byte(), n.end_byte()))
            }
            NameRule::FirstNamed => node.named_child(0).map(|n| txt(src, n.start_byte(), n.end_byte())),
            NameRule::Own => Some(txt(src, node.start_byte(), node.end_byte())),
            NameRule::NamedChild(i) => node.named_child(*i as u32).map(|n| txt(src, n.start_byte(), n.end_byte())),
            NameRule::Custom(f) => f(node, src),
        };
        if let Some(n) = got {
            let n = n.trim().trim_matches(|c| c == '"' || c == '`');
            // a prime ends a Haskell name (`step'`); only a quoted name starts with one
            let n = if n.starts_with('\'') { n.trim_matches('\'') } else { n };
            let n = if n.starts_with('[') && n.ends_with(']') && n.len() > 2 { &n[1..n.len() - 1] } else { n };
            // the C++ call operator is named with its parentheses
            let call_op = n.ends_with("operator()");
            let n = if call_op { n } else { n.strip_suffix("()").unwrap_or(n) };
            let n = match n.find('(') {
                // a signature slipped in: keep the callee
                Some(i) if i > 0 && !call_op => &n[..i],
                _ => n,
            };
            let n = n.trim().to_string();
            // a long capture with spaces is an expression, never a name
            if !n.is_empty() && (n.len() <= 200 || !n.contains(char::is_whitespace)) {
                return Some(n);
            }
        }
    }
    None
}

fn first_string<'t>(node: Node<'t>) -> Option<Node<'t>> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(
            n.kind(),
            "string"
                | "encapsed_string"
                | "string_literal"
                | "system_lib_string"
                | "interpreted_string_literal"
                | "raw_string_literal"
                | "string_value"
                | "quoted_string"
                | "str_lit"
        ) {
            return Some(n);
        }
        let mut cur = n.walk();
        let kids: Vec<Node> = n.children(&mut cur).collect();
        for k in kids.into_iter().rev() {
            stack.push(k);
        }
    }
    None
}

const IMPORT_WORDS: &[&str] =
    &["import", "using", "use", "static", "qualified", "require", "require_relative", "include", "from", "open", "library", "source", "export"];

/// Module names as people refer to them: quotes, keywords, aliases and brace lists removed.
pub fn normalize_module(raw: &str) -> String {
    let mut t = raw.trim().trim_end_matches(';').trim().to_string();
    if t.starts_with('@') {
        t = t.trim_start_matches("@import").trim_start_matches('(').trim_end_matches(')').to_string();
    }
    // leading keywords
    loop {
        let mut changed = false;
        for w in IMPORT_WORDS {
            if let Some(rest) = t.strip_prefix(w) {
                if rest.starts_with(|c: char| c.is_whitespace() || c == '(') {
                    t = rest.trim_start().trim_start_matches('(').to_string();
                    changed = true;
                }
            }
        }
        if !changed {
            break;
        }
    }
    if let Some(i) = t.find(" as ") {
        t.truncate(i);
    }
    if let Some(i) = t.find(['(', '{']) {
        t.truncate(i);
    }
    let t = t.trim().trim_end_matches("::").trim_end_matches(')').trim();
    let t = if t.chars().all(|c| c == '.') { t } else { t.trim_end_matches('.') }; // `from . import x` keeps its dot
    let t = t.trim_matches(|c| c == '"' || c == '\'' || c == '`' || c == '<' || c == '>');
    // Julia `import Base: show` names the module before the colon
    t.split_whitespace().next().unwrap_or("").trim_end_matches(':').to_string()
}

/// The first argument of a call, when it is a literal of one of `kinds` with no interpolation.
fn literal_arg<'t>(call: Node<'t>, kinds: &[&str]) -> Option<Node<'t>> {
    let args = call.child_by_field_name("arguments").or_else(|| call.named_children(&mut call.walk()).find(|c| c.kind() == "argument_list"))?;
    let mut a = args.named_children(&mut args.walk()).find(|c| !c.kind().contains("comment"))?;
    if a.kind() == "argument" {
        a = a.child_by_field_name("value")?; // R
    }
    let holes = ["template_substitution", "interpolation", "string_interpolation"];
    if !kinds.contains(&a.kind()) || a.named_children(&mut a.walk()).any(|c| holes.contains(&c.kind())) {
        return None;
    }
    // Lua long brackets and R quotes: the content is the module
    Some(a.child_by_field_name("content").unwrap_or(a))
}

/// Each name of a PHP `use`: `use A\B, C;`, `use function A\f;` and `use A\{B, C as D};`.
fn php_use_names(node: Node, src: &[u8], out: &mut Vec<String>) {
    let clause = |cl: Node, prefix: &str| {
        cl.named_children(&mut cl.walk())
            .find(|c| matches!(c.kind(), "qualified_name" | "name"))
            .map(|n| format!("{prefix}{}", txt(src, n.start_byte(), n.end_byte())))
    };
    let mut prefix = String::new();
    for ch in node.named_children(&mut node.walk()) {
        match ch.kind() {
            "namespace_name" => prefix = format!("{}\\", txt(src, ch.start_byte(), ch.end_byte())),
            "namespace_use_clause" => out.extend(clause(ch, "")),
            "namespace_use_group" => {
                out.extend(ch.named_children(&mut ch.walk()).filter(|c| c.kind() == "namespace_use_clause").filter_map(|c| clause(c, &prefix)))
            }
            _ => {}
        }
    }
}

fn imports_of(node: Node, spec: &LangSpec, src: &[u8], out: &mut Vec<(i64, String)>) {
    if !node.is_named() {
        return; // keyword tokens share their kind name with the statement (`import`)
    }
    for rule in spec.imports {
        if rule.node != node.kind() {
            continue;
        }
        if !rule.when_head.is_empty() {
            let head = if rule.head_field.is_empty() { node.named_child(0) } else { node.child_by_field_name(rule.head_field) };
            let Some(h) = head else { continue };
            if node.child_by_field_name("receiver").is_some() {
                continue; // Ruby `YAML.load(...)` is a method call, not `load`
            }
            let ht = txt(src, h.start_byte(), h.end_byte());
            if !rule.when_head.contains(&ht.as_str()) {
                continue;
            }
        }
        let line = node.start_position().row as i64 + 1;
        let mut texts: Vec<String> = Vec::new();
        match rule.how {
            ImportHow::Fields(f) => {
                let mut cur = node.walk();
                for (i, ch) in node.children(&mut cur).enumerate() {
                    if node.field_name_for_child(i as u32) == Some(f) {
                        texts.push(txt(src, ch.start_byte(), ch.end_byte()));
                    }
                }
            }
            ImportHow::FirstString => {
                if let Some(n) = first_string(node) {
                    texts.push(txt(src, n.start_byte(), n.end_byte()));
                }
            }
            ImportHow::ChildKind(kinds) => {
                let mut cur = node.walk();
                for (i, ch) in node.children(&mut cur).enumerate() {
                    // a child named `name` is an alias (C# `using IO = System.IO;`)
                    if kinds.contains(&ch.kind()) && node.field_name_for_child(i as u32) != Some("name") {
                        texts.push(txt(src, ch.start_byte(), ch.end_byte()));
                    }
                }
            }
            ImportHow::Text => texts.push(txt(src, node.start_byte(), node.end_byte())),
            ImportHow::Arg(kinds) => texts.extend(literal_arg(node, kinds).map(|n| txt(src, n.start_byte(), n.end_byte()))),
            ImportHow::UseClauses => php_use_names(node, src, &mut texts),
        }
        for t in texts {
            let m = normalize_module(&t);
            if !m.is_empty() && m.len() <= 300 && !IMPORT_WORDS.contains(&m.as_str()) {
                out.push((line, m));
            }
        }
        break;
    }
}

/// Contiguous comment block directly above a definition.
/// `ancestors` are the node's ancestors, root first, each with its children; `pos[i]` is the
/// position of `ancestors[i]` in its parent and `pos[len]` the node's (Node::parent and
/// prev_sibling walk down from the root, which makes deep nesting quadratic). The `lead`
/// siblings right above the node belong to the definition (a type signature).
fn leading_comment(lang: &str, node: Node, ancestors: &[(Node, Vec<Node>)], pos: &[usize], src: &[u8], wrappers: &[&str], lead: usize) -> String {
    let mut up = ancestors.len();
    let mut at = pos[up] - lead;
    let mut anchor = if lead > 0 { ancestors[up - 1].1[at] } else { node };
    // the definition's own header line: a comment there is not about a nested definition
    let mut header_row = -1;
    while up > 0 {
        let p = ancestors[up - 1].0;
        // a Common Lisp defun is its list's only child; a Jsonnet bind follows the `local` keyword;
        // the comment above the first statement of a body or file sits outside the node that lists them
        let py_body = at == 0 && p.kind() == "block" && up >= 2 && matches!(ancestors[up - 2].0.kind(), "class_definition" | "function_definition");
        let first_in_body = py_body || at == 0 && BODY_LISTS.contains(&p.kind());
        // the list's owner opens on the header line, unless the owner is the file
        if first_in_body && (py_body || up > 2) {
            header_row = ancestors[up - 2].0.start_position().row as i64;
        }
        if !(wrappers.contains(&p.kind())
            || p.kind() == "decorated_definition"
            || first_in_body
            || matches!((anchor.kind(), p.kind()), ("defun", "list_lit") | ("bind", "local_expr")))
        {
            break;
        }
        up -= 1;
        anchor = p;
        at = pos[up];
    }
    let siblings: &[Node] = if up > 0 { &ancestors[up - 1].1 } else { &[] };
    let mut prev_sibling = || -> Option<Node> {
        at = at.checked_sub(1)?;
        Some(siblings[at])
    };
    let mut parts: Vec<String> = Vec::new();
    let mut want = anchor.start_position().row as i64;
    let mut prev = prev_sibling();
    while let Some(p) = prev {
        // fish and assembly put line break tokens, assembly also empty nodes, below a comment
        if p.kind() == "\n" || p.start_byte() == p.end_byte() {
            prev = prev_sibling();
            continue;
        }
        if last_row(p) >= want - 1 && p.start_position().row as i64 != header_row {
            if let Some(doc) = doc_form(lang, p, src) {
                if parts.is_empty() {
                    return doc;
                }
                break;
            }
            if is_attribute(lang, p, src) {
                want = p.start_position().row as i64;
                prev = prev_sibling();
                continue;
            }
        }
        if COMMENT_TYPES.contains(&p.kind()) && last_row(p) >= want - 1 && p.start_position().row as i64 != header_row {
            let t = txt(src, p.start_byte(), p.end_byte());
            if t.starts_with("#!") {
                break; // a shebang is not documentation
            }
            parts.push(t);
            want = p.start_position().row as i64;
            prev = prev_sibling();
        } else {
            break;
        }
    }
    if parts.is_empty() {
        return String::new();
    }
    parts.reverse();
    clean_comment(&parts.join("\n"))
}

/// The name of an Elixir module attribute (`spec` for `@spec ...`) and its call.
fn elixir_attribute<'t>(node: Node<'t>, src: &[u8]) -> Option<(String, Node<'t>)> {
    let call = node.child_by_field_name("operand").filter(|c| node.kind() == "unary_operator" && c.kind() == "call")?;
    let name = call.child_by_field_name("target")?;
    Some((txt(src, name.start_byte(), name.end_byte()), call))
}

/// Text of an Elixir `@doc` or `@moduledoc` given as a string or a sigil.
fn elixir_doc(node: Node, src: &[u8], word: &str) -> Option<String> {
    let (_, call) = elixir_attribute(node, src).filter(|(n, _)| n == word)?;
    let args = call.named_children(&mut call.walk()).find(|c| c.kind() == "arguments")?;
    let arg = args.named_child(0).filter(|a| matches!(a.kind(), "string" | "sigil"))?;
    let text: String = arg.named_children(&mut arg.walk()).filter(|c| c.kind() == "quoted_content").map(|c| txt(src, c.start_byte(), c.end_byte())).collect();
    Some(cleandoc(&text))
}

/// Documentation a language writes above a definition as something other than a comment.
fn doc_form(lang: &str, p: Node, src: &[u8]) -> Option<String> {
    match (lang, p.kind()) {
        ("elixir", "unary_operator") => elixir_doc(p, src, "doc"),
        ("julia", "string_literal") => Some(py_string_content(&txt(src, p.start_byte(), p.end_byte())).trim().to_string()),
        _ => None,
    }
}

/// An attribute, decorator, annotation or type spec written between a definition and its documentation.
fn is_attribute(lang: &str, p: Node, src: &[u8]) -> bool {
    match (lang, p.kind()) {
        (_, "attribute_item" | "decorator" | "annotation") | ("erlang", "spec") => true,
        ("elixir", "unary_operator") => elixir_attribute(p, src).is_some_and(|(n, _)| matches!(n.as_str(), "spec" | "impl" | "deprecated")),
        _ => false,
    }
}

/// Position among `siblings` of the type signature Haskell and Elm write right above function `name`.
fn type_signature(lang: &str, name: &str, siblings: &[Node], at: usize, src: &[u8]) -> Option<usize> {
    let kind = match lang {
        "haskell" => "signature",
        "elm" => "type_annotation",
        _ => return None,
    };
    let i = at.checked_sub(1)?;
    let s = siblings[i];
    (s.kind() == kind && s.child_by_field_name("name").is_some_and(|n| txt(src, n.start_byte(), n.end_byte()) == name)).then_some(i)
}

/// Lisp forms: a string literal that follows the parameter list is documentation.
fn lisp_docstring(node: Node) -> Option<Node> {
    if !matches!(node.kind(), "list_lit" | "list" | "defun" | "function_definition" | "macro_definition") {
        return None;
    }
    let mut cur = node.walk();
    let kids: Vec<Node> = node.children(&mut cur).filter(|c| c.is_named()).collect();
    // (head name (params) "doc" ...) or defun > defun_header, "doc"
    let mut seen_params = false;
    for k in kids {
        if matches!(k.kind(), "defun_header") {
            seen_params = true;
            continue;
        }
        if matches!(k.kind(), "list_lit" | "list") && !seen_params {
            seen_params = true;
            continue;
        }
        if seen_params && matches!(k.kind(), "str_lit" | "string") {
            return Some(k);
        }
    }
    None
}

fn py_docstring_node<'t>(block: Node<'t>) -> Option<(Node<'t>, Node<'t>)> {
    let mut cursor = block.walk();
    for stmt in block.children(&mut cursor) {
        if stmt.kind() == "comment" {
            continue;
        }
        if stmt.kind() == "expression_statement" && stmt.named_child_count() == 1 {
            let s = stmt.named_child(0).unwrap();
            if matches!(s.kind(), "string" | "concatenated_string") {
                return Some((s, stmt));
            }
        }
        return None;
    }
    None
}

thread_local! {
    static PARSERS: std::cell::RefCell<HashMap<&'static str, Option<Parser>>> = std::cell::RefCell::new(HashMap::new());
}

fn with_parser<R>(spec: &'static LangSpec, f: impl FnOnce(&mut Parser) -> R) -> Option<R> {
    PARSERS.with(|p| {
        let mut p = p.borrow_mut();
        let parser = p.entry(spec.id).or_insert_with(|| {
            let mut parser = Parser::new();
            match parser.set_language(&(spec.language)()) {
                Ok(()) => Some(parser),
                Err(e) => {
                    eprintln!("[scrutate] grammar {} unusable: {e}", spec.id);
                    None
                }
            }
        });
        parser.as_mut().map(f)
    })
}

/// Module-level header chunk (module docstring / imports / constants) + fallbacks shared by all tiers.
fn finish(path: &str, source: &str, mut chunks: Vec<Chunk>, module_doc: String) -> Vec<Chunk> {
    let base = basename(path);
    let stem_name = match base.rfind('.') {
        Some(i) if i > 0 => &base[..i],
        _ => base,
    };
    let first_def = chunks.iter().filter(|c| c.kind != "constant").map(|c| c.start).min();
    let lines = split_lines(source);
    let head_end = match first_def {
        Some(fd) => (fd - 1).max(0) as usize,
        None => lines.len(),
    }
    .min(60);
    let head_txt = capped(lines[..head_end.min(lines.len())].join("\n"));
    if !head_txt.trim().is_empty() {
        chunks.insert(
            0,
            Chunk {
                name: stem_name.to_string(),
                qualname: stem_name.to_string(),
                kind: "module",
                start: 1,
                end: head_end as i64,
                text: head_txt.clone(),
                sig: String::new(),
                doc: module_doc,
                body: head_txt,
                calls: BTreeSet::new(),
                lazy: None,
            },
        );
    }
    if first_def.is_none() && head_end < lines.len() {
        // no definitions: the rest of the file stays searchable as windows
        chunks.extend(window_chunks_from(source, head_end));
    }
    // constants last: the index gives the chunks before them their lexical rows (constants get none)
    chunks.sort_by_key(|c| c.kind == "constant");
    if chunks.is_empty() {
        return window_chunks(source);
    }
    chunks
}

// ------------------------------------------------------------------------- tier 1: grammars
/// (chunks, module docstring, imports as (line, module))
type GrammarOutput = (Vec<Chunk>, String, Vec<(i64, String)>);

/// A file's last grammar parse, kept in memory so the next version of the file is parsed
/// incrementally (tree-sitter reuses the unchanged subtrees).
pub struct PrevParse {
    lang: &'static str,
    source: String,
    tree: Tree,
}

impl PrevParse {
    pub fn source_len(&self) -> usize {
        self.source.len()
    }
}

/// Grammars reparsed incrementally: those without an external scanner. With a scanner (Python,
/// JavaScript, Rust, Elm, reStructuredText...) a reparse was seen to differ from a fresh parse.
const INCREMENTAL: &[&str] = &[
    "ada",
    "asm",
    "c",
    "commonlisp",
    "elisp",
    "glsl",
    "go",
    "graphql",
    "groovy",
    "java",
    "json",
    "llvm",
    "make",
    "objc",
    "pascal",
    "prisma",
    "puppet",
    "scheme",
    "solidity",
    "verilog",
    "zig",
];

fn point_at(s: &[u8], at: usize) -> Point {
    let line_start = memchr::memrchr(b'\n', &s[..at]).map(|i| i + 1).unwrap_or(0);
    Point { row: memchr::memchr_iter(b'\n', &s[..at]).count(), column: at - line_start }
}

/// The edit from `old` to `new` as one replaced range: common prefix and suffix are kept.
pub fn single_edit(old: &[u8], new: &[u8]) -> InputEdit {
    let pre = old.iter().zip(new).take_while(|(a, b)| a == b).count();
    let max_suf = old.len().min(new.len()) - pre;
    let suf = old.iter().rev().zip(new.iter().rev()).take(max_suf).take_while(|(a, b)| a == b).count();
    let (old_end, new_end) = (old.len() - suf, new.len() - suf);
    InputEdit {
        start_byte: pre,
        old_end_byte: old_end,
        new_end_byte: new_end,
        start_position: point_at(new, pre),
        old_end_position: point_at(old, old_end),
        new_end_position: point_at(new, new_end),
    }
}

fn parse_tree(spec: &'static LangSpec, source: &str, prev: Option<PrevParse>) -> Option<Tree> {
    let src = source.as_bytes();
    let old = prev.filter(|p| p.lang == spec.id).map(|mut p| {
        p.tree.edit(&single_edit(p.source.as_bytes(), src));
        p.tree
    });
    let tree = with_parser(spec, |p| p.parse(src, old.as_ref()))??;
    if old.is_some() && tree.root_node().has_error() {
        // error recovery can settle differently when it starts from an old tree
        return with_parser(spec, |p| p.parse(src, None))?;
    }
    Some(tree)
}

fn grammar_chunks(spec: &'static LangSpec, path: &str, source: &str, prev: Option<PrevParse>) -> Option<(GrammarOutput, Option<PrevParse>)> {
    let src = source.as_bytes();
    let t = std::time::Instant::now();
    let tree = parse_tree(spec, source, prev)?;
    if crate::index::prof::enabled() {
        crate::index::prof::add(&crate::index::prof::TREESITTER, t);
    }
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut consts: Vec<Chunk> = Vec::new();
    let mut imports: Vec<(i64, String)> = Vec::new();
    let mut def_stack: Vec<usize> = Vec::new();
    let mut heads: std::collections::HashSet<usize> = std::collections::HashSet::new();
    let mut stack: Vec<(Node, bool, bool, usize)> = vec![(tree.root_node(), false, false, 0)];
    let mut ancestors: Vec<(Node, Vec<Node>)> = Vec::new();
    let mut pos: Vec<usize> = Vec::new();
    while let Some((node, exiting, pushed, at)) = stack.pop() {
        if exiting {
            ancestors.pop();
            pos.pop();
            if pushed {
                def_stack.pop();
            }
            continue;
        }
        pos.push(at);
        let t = node.kind();
        if !spec.imports.is_empty() {
            imports_of(node, spec, src, &mut imports);
        }
        let mut matched: Option<(&'static str, String)> = None;
        for def in spec.defs {
            if def.node != t {
                continue;
            }
            if !def.value_kinds.is_empty() {
                let ok = node.child_by_field_name("value").map(|v| def.value_kinds.contains(&v.kind())).unwrap_or(false);
                if !ok {
                    continue;
                }
            }
            if !def.head_words.is_empty() && !def.head_words.contains(&head_word(node, src).as_str()) {
                continue;
            }
            if !def.needs_field.is_empty() && node.child_by_field_name(def.needs_field).is_none() {
                continue;
            }
            if !def.parents.is_empty() {
                // an include guard or `#if` block is no scope of its own
                let parent = ancestors.iter().rev().map(|(p, _)| p.kind()).find(|k| !spec.outer.contains(k) && !k.starts_with("preproc_"));
                if !parent.is_some_and(|k| def.parents.contains(&k)) {
                    continue;
                }
            }
            if let Some(name) = def_name(node, def.name, src) {
                let kind = if def.kind_by_head { kind_from_head(node, src).unwrap_or(def.kind) } else { def.kind };
                matched = Some((kind, name));
                break;
            }
        }
        let js = matches!(spec.id, "javascript" | "typescript" | "tsx");
        let this_member = js && t == "assignment_expression" && matches!(matched, Some(("method", _))) && crate::langs::js_this_member(node, src).is_some();
        if this_member && def_stack.is_empty() {
            matched = None;
        }
        if matched.is_none() && js {
            if let Some(kind) = crate::langs::js_default_export(node, src) {
                let name = node.child_by_field_name("name").map(|n| txt(src, n.start_byte(), n.end_byte()));
                matched = Some((kind, name.unwrap_or_else(|| default_export_name(path))));
            }
        }
        let mut pushed_now = false;
        if matches!(matched, Some(("key", _))) && def_stack.len() >= 2 {
            matched = None; // deeply nested keys of data files are searchable text, not symbols
        }
        if let Some((mut kind, name)) = matched {
            if kind == "function" {
                if let Some(&top) = def_stack.last() {
                    // a C++ struct body is a class body
                    if chunks[top].kind == "class" || spec.id == "cpp" && chunks[top].kind == "struct" {
                        kind = "method";
                    }
                }
            }
            let (prefix, name) = split_owner(spec.id, node, name, src);
            let mut parent_q = def_stack.last().map(|&i| chunks[i].qualname.as_str()).unwrap_or_default();
            if this_member && def_stack.last().is_some_and(|&i| chunks[i].kind == "method") {
                // `this` in a method or a constructor is the method's owner
                parent_q = parent_q.rsplit_once('.').map_or("", |(owner, _)| owner);
            }
            let q = match &prefix {
                Some(p) => qualify(&qualify(parent_q, p), &name),
                None => qualify(parent_q, &name),
            };
            let mut outer = node;
            for (p, _) in ancestors.iter().rev() {
                if spec.outer.contains(&p.kind()) {
                    outer = *p;
                } else {
                    break;
                }
            }
            let mut lead = 0;
            if let Some(i) = ancestors.last().and_then(|(_, kids)| type_signature(spec.id, &name, kids, at, src)) {
                outer = ancestors.last().unwrap().1[i];
                lead = at - i;
            }
            let body_node = if spec.body.is_empty() { None } else { node.child_by_field_name(spec.body).or_else(|| objc_method_body(node)) };
            let sig_end = match body_node {
                Some(b) => b.start_byte(),
                None => node.end_byte().min(node.start_byte() + 200),
            };
            let sig = txt(src, outer.start_byte(), sig_end).trim().to_string();
            let mut body_start = sig_end;
            let mut doc = match node.child_by_field_name("docstring").or_else(|| lisp_docstring(node)) {
                Some(ds) if !spec.docstring => py_string_content(&txt(src, ds.start_byte(), ds.end_byte())),
                _ => leading_comment(spec.id, node, &ancestors, &pos, src, spec.wrappers, lead),
            };
            if doc.is_empty() && spec.id == "elixir" && kind == "module" {
                let body = node.named_children(&mut node.walk()).find(|c| c.kind() == "do_block");
                doc = body.and_then(|b| b.named_children(&mut b.walk()).find_map(|c| elixir_doc(c, src, "moduledoc"))).unwrap_or_default();
            }
            if let Some((s, stmt)) = body_node.filter(|_| spec.docstring).and_then(py_docstring_node) {
                // the comment above comes first, then the docstring
                let ds = py_string_content(&txt(src, s.start_byte(), s.end_byte()));
                doc = if doc.is_empty() { ds } else { format!("{doc}\n{ds}") };
                body_start = stmt.end_byte();
            }
            let cap = if CONTAINERS.contains(&kind) { MAX_CONTAINER_LINES } else { MAX_CHUNK_LINES };
            chunks.push(Chunk {
                name,
                qualname: q,
                kind,
                start: outer.start_position().row as i64 + 1,
                end: last_row(node) + 1,
                sig,
                doc,
                lazy: Some(Lazy { text: outer.start_byte(), body: body_start, end: node.end_byte(), lines: cap }),
                ..Default::default()
            });
            if let (true, Some((_, siblings))) = (spec.id == "asm" && t == "label", ancestors.last()) {
                let c = chunks.last_mut().unwrap();
                let last = asm_block(node, siblings, at, src, &mut c.calls);
                c.end = last_row(last) + 1;
                if let Some(l) = c.lazy.as_mut() {
                    l.end = last.end_byte();
                }
            }
            def_stack.push(chunks.len() - 1);
            pushed_now = true;
            heads.extend(def_head(spec.id, node).map(|h| h.id()));
        } else if let Some((names, shown)) = constant_of(spec.id, node, &ancestors, src) {
            // not on def_stack: calls in the initializer stay with the enclosing definition
            let parent_q = def_stack.last().map(|&i| chunks[i].qualname.as_str()).unwrap_or_default();
            // the comment sits above the node the chunk shows: a Go `const` above its only spec
            let up = ancestors.iter().rposition(|(a, _)| a.id() == shown.id()).unwrap_or(ancestors.len());
            let doc = leading_comment(spec.id, shown, &ancestors[..up], &pos[..=up], src, spec.wrappers, 0);
            let text = head_bytes(src, shown.start_byte(), shown.end_byte(), MAX_CHUNK_LINES);
            for name in names {
                // `const int Config::kSize = 2;` is owned by `Config`
                let (owner, name) = split_owner(spec.id, node, name, src);
                let parent_q = owner.map_or_else(|| parent_q.to_string(), |o| qualify(parent_q, &o));
                consts.push(Chunk {
                    qualname: qualify(&parent_q, &name),
                    name,
                    kind: "constant",
                    start: shown.start_position().row as i64 + 1,
                    end: last_row(shown) + 1,
                    text: text.clone(),
                    doc: doc.clone(),
                    ..Default::default()
                });
            }
        } else if let Some(call) = spec.calls.iter().find(|c| c.node == t) {
            let in_signature = ancestors.last().map(|p| matches!(p.0.kind(), "signature" | "function_signature" | "method_signature")).unwrap_or(false);
            let in_head = !heads.is_empty() && (heads.contains(&node.id()) || ancestors.iter().any(|(a, _)| heads.contains(&a.id())));
            if let (Some(&top), false) = (def_stack.last(), in_signature || in_head) {
                let callee = match (spec.id, t, ancestors.last()) {
                    ("jsonnet", _, Some((_, siblings))) => sibling_callee(siblings, at, src),
                    ("vhdl", _, Some((p, _))) => vhdl_callee(node, *p, src),
                    _ => callee_name(spec.id, node, call.field, src),
                };
                if let Some(c) = callee.filter(|c| !call.skip.contains(&c.as_str())) {
                    chunks[top].calls.insert(c);
                }
                if t == "macro_invocation" && spec.id == "rust" {
                    macro_arg_calls(node, src, &mut chunks[top].calls);
                }
                if t == "raw_text" && spec.id == "make" {
                    let raw = txt(src, node.start_byte(), node.end_byte());
                    chunks[top].calls.extend(MAKE_CALL.captures_iter(&raw).map(|m| m[1].to_string()));
                }
            }
        }
        stack.push((node, true, pushed_now, at));
        let kids: Vec<Node> = node.children(&mut node.walk()).collect();
        stack.extend(kids.iter().enumerate().rev().map(|(i, &ch)| (ch, false, false, i)));
        ancestors.push((node, kids));
    }
    let mut mdoc = String::new();
    if spec.docstring {
        if let Some((s, _)) = py_docstring_node(tree.root_node()) {
            mdoc = py_string_content(&txt(src, s.start_byte(), s.end_byte()));
        }
    }
    // a tree with errors is not kept: the next parse would likely redo it from scratch anyway
    let keep = (!tree.root_node().has_error() && INCREMENTAL.contains(&spec.id)).then(|| PrevParse { lang: spec.id, source: source.to_string(), tree });
    chunks.extend(consts);
    Some(((chunks, mdoc, imports), keep))
}

// ------------------------------------------------------------------------- tier 2: heuristics
const FUNC_WORDS: &[&str] = &[
    "def",
    "fn",
    "func",
    "function",
    "sub",
    "proc",
    "procedure",
    "method",
    "defn",
    "defun",
    "defmacro",
    "fun",
    "macro",
    "iterator",
    "template",
    "task",
    "subroutine",
    "defp",
    "lambda",
    "let",
    "val",
    "var",
    "const",
];
const TYPE_WORDS: &[(&str, &str)] = &[
    ("class", "class"),
    ("struct", "struct"),
    ("interface", "interface"),
    ("trait", "trait"),
    ("enum", "enum"),
    ("module", "module"),
    ("object", "class"),
    ("type", "type"),
    ("record", "struct"),
    ("protocol", "interface"),
    ("extension", "impl"),
    ("actor", "class"),
    ("contract", "class"),
    ("service", "class"),
    ("package", "module"),
    ("namespace", "module"),
    ("union", "struct"),
    ("typedef", "type"),
    ("entity", "class"),
    ("component", "class"),
    ("mixin", "trait"),
    ("impl", "impl"),
    ("instance", "impl"),
    ("data", "type"),
    ("newtype", "type"),
];
const MODIFIERS: &[&str] = &[
    "public",
    "private",
    "protected",
    "internal",
    "static",
    "final",
    "abstract",
    "async",
    "export",
    "default",
    "inline",
    "virtual",
    "override",
    "unsafe",
    "extern",
    "pub",
    "local",
    "global",
    "my",
    "our",
    "open",
    "sealed",
    "partial",
    "declare",
    "native",
    "synchronized",
    "constexpr",
    "shared",
    "mut",
    "operator",
    "friend",
    "readonly",
    "lazy",
    "implicit",
    "suspend",
    "tailrec",
    "infix",
    "inner",
    "data",
    "case",
];
static DEF_LINE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[ \t]*(?:[A-Za-z_@][\w@.()<>\[\]]*[ \t]+){0,4}?([A-Za-z_]\w*)[ \t]+([A-Za-z_][\w.:$'!?-]*)").unwrap());
static SHELL_FN: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]*([A-Za-z_][\w.:-]*)[ \t]*\([ \t]*\)[ \t]*\{?[ \t]*$").unwrap());
static LISP_DEF: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[ \t]*\((def[a-z!*-]*|define(?:-[a-z-]+)?)[ \t]+\(?([^\s()]+)").unwrap());
static SQL_DEF: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^[ \t]*create[ \t]+(?:or[ \t]+replace[ \t]+)?(?:temp(?:orary)?[ \t]+)?(?:materialized[ \t]+)?(function|procedure|table|view|trigger|index|type|schema)[ \t]+(?:if[ \t]+not[ \t]+exists[ \t]+)?([\w.\x22`\[\]]+)").unwrap()
});

fn is_keyword(w: &str) -> bool {
    MODIFIERS.contains(&w) || FUNC_WORDS.contains(&w) || TYPE_WORDS.iter().any(|(t, _)| *t == w)
}

/// `line` without its leading modifier words (`private def x` -> `def x`); a modifier that is
/// followed by a name stays (`data Foo = ...`).
fn strip_modifiers(line: &str) -> &str {
    let mut rest = line.trim_start();
    while let Some((w, after)) = rest.split_once([' ', '\t']) {
        let after = after.trim_start();
        let next = after.split([' ', '\t']).next().unwrap_or("");
        if !(MODIFIERS.contains(&w) && is_keyword(next)) {
            break;
        }
        rest = after;
    }
    rest
}

fn heuristic_def(line: &str) -> Option<(&'static str, String)> {
    let line = strip_modifiers(line);
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with(['#', '/', '*', ';', '-', '%', '\'', '"', '<']) {
        return None;
    }
    if let Some(m) = SQL_DEF.captures(line) {
        let kind = match m[1].to_lowercase().as_str() {
            "function" | "procedure" | "trigger" => "function",
            "table" | "view" | "type" => "type",
            _ => "block",
        };
        return Some((kind, m[2].trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']').to_string()));
    }
    if let Some(m) = LISP_DEF.captures(line) {
        let kind = if m[1].starts_with("defmacro") {
            "macro"
        } else if m[1].contains("struct") || m[1].contains("class") || m[1].contains("record") {
            "struct"
        } else {
            "function"
        };
        return Some((kind, m[2].to_string()));
    }
    if let Some(m) = DEF_LINE.captures(line) {
        let word = &m[1];
        let name = &m[2];
        if MODIFIERS.contains(&name) || name.len() > 100 {
            return None;
        }
        if FUNC_WORDS.contains(&word) {
            // `let x = 5` is not a function: require a following `(` / `=` + lambda-ish for binding words
            if matches!(word, "let" | "val" | "var" | "const" | "lambda") {
                let rest = &line[m.get(0).unwrap().end()..];
                let r = rest.trim_start();
                // OCaml and F#: `let add x y =` takes parameters, `let limit = 5` does not
                let ml_params = word == "let"
                    && r.split_once('=')
                        .is_some_and(|(p, _)| !p.trim().is_empty() && p.split_whitespace().all(|w| w.starts_with(|c: char| c.is_alphabetic() || c == '_')));
                let is_fn = r.starts_with('(') || r.contains("=>") || r.contains("fun ") || r.contains("function") || r.contains("\\") || ml_params;
                if !is_fn {
                    return None;
                }
            }
            return Some(("function", name.trim_end_matches([':', '!']).to_string()));
        }
        if let Some((_, kind)) = TYPE_WORDS.iter().find(|(w, _)| *w == word) {
            return Some((kind, name.trim_end_matches([':', '{', '(']).to_string()));
        }
    }
    if let Some(m) = SHELL_FN.captures(line) {
        let name = &m[1];
        if !["if", "while", "for", "switch", "return", "else", "elif", "catch", "sizeof"].contains(&name) {
            return Some(("function", name.to_string()));
        }
    }
    None
}

fn indent_of(line: &str) -> usize {
    line.chars().take_while(|c| *c == ' ' || *c == '\t').map(|c| if c == '\t' { 4 } else { 1 }).sum()
}

fn is_comment_line(line: &str) -> bool {
    let t = line.trim_start();
    t.starts_with("//")
        || t.starts_with('#')
        || t.starts_with("--")
        || t.starts_with(';')
        || t.starts_with("/*")
        || t.starts_with('*')
        || t.starts_with('%')
        || t.starts_with("///")
}

/// Keyword-introduced definitions for languages without a grammar (or whose grammar found nothing).
pub fn heuristic_chunks(source: &str) -> Vec<Chunk> {
    let lines = split_lines(source);
    let mut defs: Vec<(usize, &'static str, String)> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if let Some((kind, name)) = heuristic_def(line) {
            defs.push((i, kind, name));
        }
    }
    if defs.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    for (di, (start, kind, name)) in defs.iter().enumerate() {
        let my_indent = indent_of(lines[*start]);
        // the block ends before the next definition at the same or a shallower indentation
        let mut end = lines.len();
        for (next, _, _) in &defs[di + 1..] {
            if indent_of(lines[*next]) <= my_indent {
                end = *next;
                break;
            }
        }
        let cap = if CONTAINERS.contains(kind) { MAX_CONTAINER_LINES } else { MAX_CHUNK_LINES };
        // trailing blank/comment lines belong to the next definition
        let mut e = end;
        while e > start + 1 && (lines[e - 1].trim().is_empty() || is_comment_line(lines[e - 1])) {
            e -= 1;
        }
        let seg = &lines[*start..e.max(start + 1)];
        let text = head(&seg.join("\n"), cap);
        let body = head(&seg[1..].join("\n"), cap);
        // doc: comment lines directly above
        let mut d = *start;
        while d > 0 && is_comment_line(lines[d - 1]) {
            d -= 1;
        }
        let doc = if d < *start { clean_comment(&lines[d..*start].join("\n")) } else { String::new() };
        out.push(Chunk {
            name: name.clone(),
            qualname: name.clone(),
            kind,
            start: *start as i64 + 1,
            end: e.max(start + 1) as i64,
            text,
            sig: lines[*start].trim().to_string(),
            doc,
            body,
            calls: BTreeSet::new(),
            lazy: None,
        });
    }
    out
}

/// Indented tree of (kind, field, first line of text) for a file: how grammar rules get written.
pub fn sexp(spec: &'static LangSpec, source: &str) -> String {
    let src = source.as_bytes();
    let Some(Some(tree)) = with_parser(spec, |p| p.parse(src, None)) else {
        return "(parse failed)".into();
    };
    let mut out = String::new();
    let mut stack: Vec<(Node, usize, Option<&'static str>)> = vec![(tree.root_node(), 0, None)];
    while let Some((node, depth, field)) = stack.pop() {
        if !node.is_named() {
            continue;
        }
        let t = txt(src, node.start_byte(), node.end_byte());
        let first = t.lines().next().unwrap_or("");
        let first: String = first.chars().take(60).collect();
        out.push_str(&format!(
            "{}{}{} [{}-{}] {:?}\n",
            "  ".repeat(depth),
            field.map(|f| format!("{f}: ")).unwrap_or_default(),
            node.kind(),
            node.start_position().row + 1,
            node.end_position().row + 1,
            first
        ));
        let n = node.child_count();
        for i in (0..n).rev() {
            if let Some(ch) = node.child(i as u32) {
                stack.push((ch, depth + 1, node.field_name_for_child(i as u32)));
            }
        }
    }
    out
}

// ------------------------------------------------------------------------- main
/// Chunks and imports of one file (grammar tier, then heuristics, then windows).
pub fn parse_file(path: &str, source: &str) -> Parsed {
    let mut parsed = parse_file_with(path, source, None).0;
    parsed.chunks.iter_mut().for_each(|c| c.fill(source.as_bytes()));
    parsed
}

/// `parse_file`, reusing the tree of the file's previous version when there is one; also returns
/// this version's tree for the next call. The result is the same as a fresh parse.
pub fn parse_file_with(path: &str, source: &str, prev: Option<PrevParse>) -> (Parsed, Option<PrevParse>) {
    if let Some(spec) = spec_for(path, source) {
        if let Some(((chunks, mdoc, imports), tree)) = grammar_chunks(spec, path, source, prev) {
            // a parsed file is the grammar's verdict even when it holds no definitions: the
            // keyword heuristics would invent symbols from prose ("namespace so the ...")
            return (Parsed { chunks: finish(path, source, chunks, mdoc), imports }, tree);
        }
    }
    // prose has keyword-looking lines ("module will be called foo" in Kconfig help text)
    let base = basename(path);
    let prose = base.starts_with("Kconfig") || base.to_ascii_lowercase().ends_with(".txt");
    let chunks = if prose { Vec::new() } else { heuristic_chunks(source) };
    (Parsed { chunks: finish(path, source, chunks, String::new()), imports: Vec::new() }, None)
}

pub fn parse_source(path: &str, source: &str) -> Vec<Chunk> {
    parse_file(path, source).chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn python_chunks() {
        let src = "\"\"\"Mod doc.\"\"\"\nimport os\n\n\nclass A:\n    \"\"\"Doc A.\"\"\"\n\n    def m(self, x):\n        '''Do m.\n\n        more'''\n        return os.path.join(x)\n\n\ndef f():\n    return A().m(1)\n";
        let ch = parse_source("a.py", src);
        let names: Vec<&str> = ch.iter().map(|c| c.qualname.as_str()).collect();
        assert_eq!(names, vec!["a", "A", "A.m", "f"]);
        assert_eq!(ch[0].kind, "module");
        assert_eq!(ch[0].doc, "Mod doc.");
        assert_eq!(ch[2].kind, "method");
        assert_eq!(ch[2].doc, "Do m.\n\nmore");
        assert_eq!(ch[2].sig, "def m(self, x):");
        assert!(ch[2].calls.contains("join"));
        assert!(ch[3].calls.contains("A") && ch[3].calls.contains("m"));
        assert_eq!((ch[1].start, ch[1].end), (5, 12));
    }

    #[test]
    fn cleandoc_like_python() {
        assert_eq!(cleandoc("\n    Hello\n      world\n    "), "Hello\n  world");
        assert_eq!(cleandoc("  First line\n\n    indented"), "First line\n\nindented");
    }

    #[test]
    fn js_leading_comment_and_arrow() {
        let src = "// Adds things.\n// Really.\nexport const add = (a, b) => a + b;\n\n/**\n * Greets.\n */\nfunction greet(n) { return hi(n); }\n";
        let ch = parse_source("x.js", src);
        let g = ch.iter().find(|c| c.name == "greet").unwrap();
        assert_eq!(g.doc, "Greets.");
        assert!(g.calls.contains("hi"));
        let a = ch.iter().find(|c| c.name == "add").unwrap();
        assert_eq!(a.doc, "Adds things.\nReally.");
    }

    #[test]
    fn windows() {
        let src: String = (0..120).map(|i| format!("line {i}\n")).collect();
        let w = window_chunks(&src);
        assert_eq!(w.len(), 3);
        assert_eq!((w[0].start, w[0].end), (1, 50));
        assert_eq!((w[1].start, w[1].end), (41, 90));
        assert_eq!((w[2].start, w[2].end), (81, 120));
    }

    #[test]
    fn imports_are_extracted_and_normalised() {
        let py = parse_file("i.py", "import os.path\nfrom a.b import c as d\nimport x, y\nfrom . import z\n");
        let mods: Vec<&str> = py.imports.iter().map(|(_, m)| m.as_str()).collect();
        assert_eq!(mods, vec!["os.path", "a.b", "x", "y", "."]);
        let js = parse_file("i.js", "import {a} from \"./mod\";\nconst b = require(\"fs\");\nexport {c} from \"../c\";\n");
        assert_eq!(js.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["./mod", "fs", "../c"]);
        let rs = parse_file("i.rs", "use std::fmt;\nuse crate::lex::{a, b};\n");
        assert_eq!(rs.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["std::fmt", "crate::lex"]);
        let c = parse_file("i.c", "#include <stdio.h>\n#include \"x.h\"\n");
        assert_eq!(c.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["stdio.h", "x.h"]);
        let rb = parse_file("i.rb", "require \"json\"\nrequire_relative \"lib/x\"\nputs \"no\"\n");
        assert_eq!(rb.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["json", "lib/x"]);
        let go = parse_file("i.go", "package main\nimport (\n  \"fmt\"\n  m \"github.com/x/y\"\n)\n");
        assert_eq!(go.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["fmt", "github.com/x/y"]);
        let java = parse_file("I.java", "import java.util.List;\nimport static java.lang.Math.max;\n");
        assert_eq!(java.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["java.util.List", "java.lang.Math.max"]);
        let hs = parse_file("i.hs", "import qualified Data.Map as M\nimport Data.List (sort)\n");
        assert_eq!(hs.imports.iter().map(|(_, m)| m.as_str()).collect::<Vec<_>>(), vec!["Data.Map", "Data.List"]);
    }

    #[test]
    fn heuristics_find_keyword_definitions() {
        let src = "# A perl-ish file\nuse strict;\n\n# Says hello.\nsub greet {\n    print \"hi\";\n}\n\npackage Foo::Bar;\n\nsub new {\n    return bless {}, shift;\n}\n";
        let ch = parse_source("x.pl", src);
        let names: Vec<(&str, &str)> = ch.iter().map(|c| (c.kind, c.name.as_str())).collect();
        assert_eq!(names, vec![("module", "x"), ("function", "greet"), ("module", "Foo::Bar"), ("function", "new")]);
        assert_eq!(ch[1].doc, "Says hello.");
        let sql = "CREATE OR REPLACE FUNCTION add_one(i int) RETURNS int AS $$ SELECT i + 1 $$;\n\ncreate table if not exists users (\n  id int\n);\n";
        let ch = parse_source("q.sql", sql);
        let names: Vec<(&str, &str)> = ch.iter().filter(|c| c.kind != "module").map(|c| (c.kind, c.name.as_str())).collect();
        assert_eq!(names, vec![("function", "add_one"), ("type", "users")]);
        let sh = "my_func() {\n  echo hi\n}\n";
        let ch = parse_source("x.unknownext", sh);
        assert!(ch.iter().any(|c| c.name == "my_func" && c.kind == "function"));
        let plain = "just some prose\nwithout definitions\n";
        assert!(parse_source("notes.unknownext", plain).iter().all(|c| c.kind == "module"));
    }

    #[test]
    fn form_feeds_and_lone_cr_do_not_shift_line_numbers() {
        // found by fuzzing: Python-style splitlines counted \x0c as a line break
        for c in parse_source("f.ts", "\x0c\x0c\x04") {
            assert!(c.start == 1 && c.end == 1, "{}..{}", c.start, c.end);
        }
        let src = "x = 1\x0c\n\x0c\ndef f():\r    pass\n";
        let total = src.lines().count() as i64;
        for c in parse_source("g.py", src) {
            assert!(c.end <= total, "{} {}..{} of {total}", c.name, c.start, c.end);
        }
    }
}
