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
use tree_sitter::{Node, Parser};

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
    &["comment", "line_comment", "block_comment", "doc_comment", "documentation_comment", "comment_block", "statement_comment", "module_comment"];
pub const MAX_CONTAINER_LINES: usize = 40;
pub const MAX_CHUNK_LINES: usize = 200;

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
static COMMENT_MARK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"^\s*(/\*+|\*+/|\*|//+!?|#+|--|;+|%+|'|!|"|\{-\|?|-\})\s?"#).unwrap());
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

fn head(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.split('\n').collect();
    if lines.len() <= n {
        text.to_string()
    } else {
        let mut s = lines[..n].join("\n");
        s.push_str("\n...");
        s
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
            let t = seg.join("\n");
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

fn txt(src: &[u8], a: usize, b: usize) -> String {
    String::from_utf8_lossy(&src[a.min(src.len())..b.min(src.len())]).into_owned()
}

fn callee_name(node: Node, field: &str, src: &[u8]) -> Option<String> {
    let mut target = if field.is_empty() { None } else { node.child_by_field_name(field) };
    if target.is_none() && node.named_child_count() > 0 {
        target = node.named_child(0);
    }
    let target = target?;
    let mut t = txt(src, target.start_byte(), target.end_byte());
    for sep in ["::", ".", "->"] {
        if let Some(i) = t.rfind(sep) {
            t = t[i + sep.len()..].to_string();
        }
    }
    let t = t.trim().trim_end_matches('!');
    IDENT_START.find(t).map(|m| m.as_str().to_string())
}

/// The chunk kind named by the first declaration keyword on the node's first line.
fn kind_from_head(node: Node, src: &[u8]) -> Option<&'static str> {
    let t = txt(src, node.start_byte(), node.end_byte());
    let line = t.lines().next().unwrap_or("");
    for w in line.split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '{' | ':' | '<' | ',' | '[')) {
        if let Some((_, k)) = crate::langs::KIND_WORDS.iter().find(|(kw, _)| *kw == w) {
            return Some(k);
        }
    }
    None
}

fn dig<'t>(mut node: Node<'t>, field: &str, kinds: &[&str]) -> Option<Node<'t>> {
    let mut last = None;
    while let Some(ch) = node.child_by_field_name(field) {
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
            let n = n.trim().trim_matches(|c| c == '"' || c == '\'' || c == '`');
            let n = if n.starts_with('[') && n.ends_with(']') && n.len() > 2 { &n[1..n.len() - 1] } else { n };
            let n = n.strip_suffix("()").unwrap_or(n);
            let n = match n.find('(') {
                // a signature slipped in: keep the callee
                Some(i) if i > 0 => &n[..i],
                _ => n,
            };
            let n = n.trim().to_string();
            if !n.is_empty() && n.len() <= 200 {
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
    t.split_whitespace().next().unwrap_or("").to_string()
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
            let head = if rule.head_field.is_empty() { None } else { node.child_by_field_name(rule.head_field) };
            let Some(h) = head else { continue };
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
                for ch in node.children(&mut cur) {
                    if kinds.contains(&ch.kind()) {
                        texts.push(txt(src, ch.start_byte(), ch.end_byte()));
                    }
                }
            }
            ImportHow::Text => texts.push(txt(src, node.start_byte(), node.end_byte())),
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
fn leading_comment(node: Node, src: &[u8], wrappers: &[&str]) -> String {
    let mut anchor = node;
    while let Some(p) = anchor.parent() {
        if wrappers.contains(&p.kind()) || p.kind() == "decorated_definition" {
            anchor = p;
        } else {
            break;
        }
    }
    let mut parts: Vec<String> = Vec::new();
    let mut want = anchor.start_position().row as i64;
    let mut prev = anchor.prev_sibling();
    while let Some(p) = prev {
        if COMMENT_TYPES.contains(&p.kind()) && p.end_position().row as i64 >= want - 1 {
            let t = txt(src, p.start_byte(), p.end_byte());
            if t.starts_with("#!") {
                break; // a shebang is not documentation
            }
            parts.push(t);
            want = p.start_position().row as i64;
            prev = p.prev_sibling();
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
    let first_def = chunks.iter().map(|c| c.start).min();
    let lines = split_lines(source);
    let head_end = match first_def {
        Some(fd) => (fd - 1).max(0) as usize,
        None => lines.len(),
    }
    .min(60);
    let head_txt = lines[..head_end.min(lines.len())].join("\n");
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
            },
        );
    }
    if first_def.is_none() && head_end < lines.len() {
        // no definitions: the rest of the file stays searchable as windows
        chunks.extend(window_chunks_from(source, head_end));
    }
    if chunks.is_empty() {
        return window_chunks(source);
    }
    chunks
}

// ------------------------------------------------------------------------- tier 1: grammars
/// (chunks, module docstring, imports as (line, module))
type GrammarOutput = (Vec<Chunk>, String, Vec<(i64, String)>);

fn grammar_chunks(spec: &'static LangSpec, source: &str) -> Option<GrammarOutput> {
    let src = source.as_bytes();
    let t = std::time::Instant::now();
    let tree = with_parser(spec, |p| p.parse(src, None))??;
    if crate::index::prof::enabled() {
        crate::index::prof::add(&crate::index::prof::TREESITTER, t);
    }
    let mut chunks: Vec<Chunk> = Vec::new();
    let mut imports: Vec<(i64, String)> = Vec::new();
    let mut def_stack: Vec<usize> = Vec::new();
    let mut stack: Vec<(Node, bool, bool)> = vec![(tree.root_node(), false, false)];
    while let Some((node, exiting, pushed)) = stack.pop() {
        if exiting {
            if pushed {
                def_stack.pop();
            }
            continue;
        }
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
            if let Some(name) = def_name(node, def.name, src) {
                let kind = if def.kind_by_head { kind_from_head(node, src).unwrap_or(def.kind) } else { def.kind };
                matched = Some((kind, name));
                break;
            }
        }
        let mut pushed_now = false;
        if matches!(matched, Some(("key", _))) && def_stack.len() >= 2 {
            matched = None; // deeply nested keys of data files are searchable text, not symbols
        }
        if let Some((mut kind, name)) = matched {
            if kind == "function" {
                if let Some(&top) = def_stack.last() {
                    if chunks[top].kind == "class" {
                        kind = "method";
                    }
                }
            }
            let parent_q = def_stack.last().map(|&i| chunks[i].qualname.clone()).unwrap_or_default();
            let q = if parent_q.is_empty() { name.clone() } else { format!("{parent_q}.{name}") };
            let mut outer = node;
            while let Some(p) = outer.parent() {
                if spec.outer.contains(&p.kind()) {
                    outer = p;
                } else {
                    break;
                }
            }
            let body_node = if spec.body.is_empty() { None } else { node.child_by_field_name(spec.body) };
            let sig_end = match body_node {
                Some(b) => b.start_byte(),
                None => node.end_byte().min(node.start_byte() + 200),
            };
            let sig = txt(src, outer.start_byte(), sig_end).trim().to_string();
            let mut doc = String::new();
            let mut body_start = sig_end;
            if spec.docstring {
                if let Some(b) = body_node {
                    if let Some((s, stmt)) = py_docstring_node(b) {
                        doc = py_string_content(&txt(src, s.start_byte(), s.end_byte()));
                        body_start = stmt.end_byte();
                    }
                }
            } else {
                if let Some(ds) = node.child_by_field_name("docstring").or_else(|| lisp_docstring(node)) {
                    doc = py_string_content(&txt(src, ds.start_byte(), ds.end_byte()));
                } else {
                    doc = leading_comment(node, src, spec.wrappers);
                }
            }
            let cap = if CONTAINERS.contains(&kind) { MAX_CONTAINER_LINES } else { MAX_CHUNK_LINES };
            let full = head(&txt(src, outer.start_byte(), node.end_byte()), cap);
            let body = head(&txt(src, body_start, node.end_byte()), cap);
            chunks.push(Chunk {
                name,
                qualname: q,
                kind,
                start: outer.start_position().row as i64 + 1,
                end: node.end_position().row as i64 + 1,
                text: full,
                sig,
                doc,
                body,
                calls: BTreeSet::new(),
            });
            def_stack.push(chunks.len() - 1);
            pushed_now = true;
        } else if let Some(call) = spec.calls.iter().find(|c| c.node == t) {
            let in_signature = node.parent().map(|p| matches!(p.kind(), "signature" | "function_signature" | "method_signature")).unwrap_or(false);
            if let (Some(&top), false) = (def_stack.last(), in_signature) {
                if let Some(c) = callee_name(node, call.field, src) {
                    chunks[top].calls.insert(c);
                }
            }
        }
        stack.push((node, true, pushed_now));
        let n = node.child_count();
        for i in (0..n).rev() {
            if let Some(ch) = node.child(i as u32) {
                stack.push((ch, false, false));
            }
        }
    }
    let mut mdoc = String::new();
    if spec.docstring {
        if let Some((s, _)) = py_docstring_node(tree.root_node()) {
            mdoc = py_string_content(&txt(src, s.start_byte(), s.end_byte()));
        }
    }
    Some((chunks, mdoc, imports))
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

fn heuristic_def(line: &str) -> Option<(&'static str, String)> {
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
                let is_fn =
                    r.starts_with('(') || r.contains("=>") || r.contains("fun ") || r.contains("function") || r.contains("\\") || r.starts_with(':') && false;
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
    if let Some(spec) = spec_for(path, source) {
        if let Some((chunks, mdoc, imports)) = grammar_chunks(spec, source) {
            // a parsed file is the grammar's verdict even when it holds no definitions: the
            // keyword heuristics would invent symbols from prose ("namespace so the ...")
            return Parsed { chunks: finish(path, source, chunks, mdoc), imports };
        }
    }
    let chunks = heuristic_chunks(source);
    Parsed { chunks: finish(path, source, chunks, String::new()), imports: Vec::new() }
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
