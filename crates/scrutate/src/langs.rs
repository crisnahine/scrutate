//! Language registry: which grammar parses which files, and which nodes are definitions and calls.
//!
//! Every entry was derived from the grammar's actual parse tree of a sample file (engine/tests/samples)
//! and is covered by a test that asserts the symbols found in that sample. Languages without an entry
//! here still get heuristic symbols (see parse::heuristic_chunks) and text search.

use tree_sitter::{Language, Node};

/// How to find the name of a definition node.
#[derive(Clone, Copy)]
pub enum NameRule {
    /// text of this field
    Field(&'static str),
    /// follow this field down (C declarators: `declarator` -> `function_declarator` -> ...), then the
    /// text of the innermost node whose kind is in the given list (or the innermost node when empty)
    Dig(&'static str, &'static [&'static str]),
    /// first child (any depth 1) whose kind is in the list
    ChildKind(&'static [&'static str]),
    /// first named child
    FirstNamed,
    /// text of the node itself
    Own,
    /// the n-th named child
    NamedChild(usize),
    /// the first argument of a call-like node (Elixir `def name(args)`, Lisp `(defun name ...)`)
    Custom(fn(Node, &[u8]) -> Option<String>),
}

pub struct Def {
    pub node: &'static str,
    pub kind: &'static str,
    pub name: &'static [NameRule],
    /// only when the `value` field has one of these kinds (JS `const f = () => ...`)
    pub value_kinds: &'static [&'static str],
    /// only when the first child's text is one of these (Elixir `def`/`defmodule` calls)
    pub head_words: &'static [&'static str],
    /// only when this field is present (C `struct point {...}` defines; `struct point p;` does not)
    pub needs_field: &'static str,
    /// the chunk kind comes from the declaration keyword (`struct`/`enum`/`interface`...) found in
    /// the node's first line, falling back to `kind` (grammars that use one node for several kinds)
    pub kind_by_head: bool,
}

pub struct Call {
    pub node: &'static str,
    /// field holding the callee expression ("" = first named child)
    pub field: &'static str,
}

/// Where an import statement keeps the imported module.
#[derive(Clone, Copy)]
pub enum ImportHow {
    /// every child with this field name (Python `import a, b`)
    Fields(&'static str),
    /// the first string literal anywhere below the node (`require("x")`, `#include "x.h"`)
    FirstString,
    /// the first child whose kind is in the list
    ChildKind(&'static [&'static str]),
    /// the node's own text (keywords are stripped afterwards)
    Text,
}

pub struct ImportRule {
    pub node: &'static str,
    pub how: ImportHow,
    /// only when the text of this field (the callee / command name) is one of `when_head`
    pub head_field: &'static str,
    pub when_head: &'static [&'static str],
}

const fn imp(node: &'static str, how: ImportHow) -> ImportRule {
    ImportRule { node, how, head_field: "", when_head: &[] }
}
const fn imp_call(node: &'static str, head_field: &'static str, when_head: &'static [&'static str], how: ImportHow) -> ImportRule {
    ImportRule { node, how, head_field, when_head }
}

pub struct LangSpec {
    pub id: &'static str,
    pub exts: &'static [&'static str],
    pub files: &'static [&'static str],
    pub language: fn() -> Language,
    pub defs: &'static [Def],
    pub calls: &'static [Call],
    /// a string statement that opens a body is documentation (Python, Elixir @doc is not this)
    pub docstring: bool,
    /// wrapper node kinds a leading comment may sit above (decorators, `export`, `const`)
    pub wrappers: &'static [&'static str],
    /// wrapper node kinds whose extent belongs to the chunk itself (decorated definitions, templates)
    pub outer: &'static [&'static str],
    /// field name of the body node ("" = none: the whole node after the name is the body)
    pub body: &'static str,
    /// how imports / includes / requires are recognised (empty: none extracted)
    pub imports: &'static [ImportRule],
}

const fn d(node: &'static str, kind: &'static str, name: &'static [NameRule]) -> Def {
    Def { node, kind, name, value_kinds: &[], head_words: &[], needs_field: "", kind_by_head: false }
}
/// a definition that needs a body to count (declarations/forward references do not)
const fn db(node: &'static str, kind: &'static str, name: &'static [NameRule]) -> Def {
    Def { node, kind, name, value_kinds: &[], head_words: &[], needs_field: "body", kind_by_head: false }
}
/// a definition whose kind is read from its keyword
const fn dk(node: &'static str, kind: &'static str, name: &'static [NameRule]) -> Def {
    Def { node, kind, name, value_kinds: &[], head_words: &[], needs_field: "", kind_by_head: true }
}

/// Declaration keywords and the chunk kind they denote (for `kind_by_head`).
pub const KIND_WORDS: &[(&str, &str)] = &[
    ("class", "class"),
    ("struct", "struct"),
    ("interface", "interface"),
    ("protocol", "interface"),
    ("trait", "trait"),
    ("enum", "enum"),
    ("extension", "impl"),
    ("object", "class"),
    ("actor", "class"),
    ("record", "class"),
    ("union", "struct"),
    ("module", "module"),
    ("namespace", "module"),
    ("type", "type"),
    ("typealias", "type"),
    ("mixin", "trait"),
    ("annotation", "class"),
    // Lisp forms
    ("defun", "function"),
    ("define", "function"),
    ("defmacro", "macro"),
    ("define-syntax", "macro"),
    ("defclass", "class"),
    ("defstruct", "struct"),
    ("define-struct", "struct"),
    ("define-record-type", "struct"),
    ("defvar", "key"),
    ("defparameter", "key"),
    ("defconstant", "key"),
    ("deftype", "type"),
    ("defgeneric", "function"),
    ("defmethod", "function"),
];
const fn c(node: &'static str, field: &'static str) -> Call {
    Call { node, field }
}

const N: &[NameRule] = &[NameRule::Field("name")];
const ID_KINDS: &[&str] = &[
    "identifier",
    "field_identifier",
    "type_identifier",
    "qualified_identifier",
    "scoped_identifier",
    "destructor_name",
    "operator_name",
    "template_function",
    "structured_binding_declarator",
    "constant",
    "scope_resolution",
];

fn text(src: &[u8], a: usize, b: usize) -> String {
    String::from_utf8_lossy(&src[a.min(src.len())..b.min(src.len())]).into_owned()
}

/// name of a Lisp-style `(defun NAME ...)` / Elixir `def NAME(...)` form: first identifier after the head
fn second_symbol(node: Node, src: &[u8]) -> Option<String> {
    // Elixir: call(target: identifier, arguments (call (target: identifier) ...) | alias | identifier)
    let mut cur0 = node.walk();
    let args = node.children(&mut cur0).find(|c| c.kind() == "arguments");
    if let Some(args) = args {
        let mut cur = args.walk();
        for ch in args.children(&mut cur) {
            if !ch.is_named() {
                continue;
            }
            let t = if ch.kind() == "call" {
                ch.child_by_field_name("target").map(|n| text(src, n.start_byte(), n.end_byte()))
            } else if ch.kind() == "alias" || ch.kind() == "identifier" {
                Some(text(src, ch.start_byte(), ch.end_byte()))
            } else {
                None
            };
            return t;
        }
        return None;
    }
    // Lisp: the second named child
    let mut cur = node.walk();
    let mut seen = 0;
    for ch in node.children(&mut cur) {
        if ch.is_named() {
            seen += 1;
            if seen == 2 {
                return Some(text(src, ch.start_byte(), ch.end_byte()));
            }
        }
    }
    None
}

/// R: `name <- function(...)` is a binary_operator; name is its lhs when rhs is a function_definition
fn r_assignment(node: Node, src: &[u8]) -> Option<String> {
    let rhs = node.child_by_field_name("rhs")?;
    if rhs.kind() != "function_definition" && rhs.kind() != "lambda_function" {
        return None;
    }
    let lhs = node.child_by_field_name("lhs")?;
    Some(text(src, lhs.start_byte(), lhs.end_byte()))
}

/// Markdown / RST sections: the heading text
fn heading_text(node: Node, src: &[u8]) -> Option<String> {
    let t = text(src, node.start_byte(), node.end_byte());
    let line = t.lines().next().unwrap_or("").trim().trim_start_matches('#').trim();
    if line.is_empty() {
        None
    } else {
        Some(line.to_string())
    }
}

const C_DEFS: &[Def] = &[
    d("function_definition", "function", &[NameRule::Dig("declarator", ID_KINDS)]),
    db("struct_specifier", "struct", N),
    db("union_specifier", "struct", N),
    db("enum_specifier", "enum", N),
    d("type_definition", "type", &[NameRule::Dig("declarator", ID_KINDS)]),
];
const CPP_DEFS: &[Def] = &[
    d("function_definition", "function", &[NameRule::Dig("declarator", ID_KINDS)]),
    db("class_specifier", "class", N),
    db("struct_specifier", "struct", N),
    db("union_specifier", "struct", N),
    db("enum_specifier", "enum", N),
    d("namespace_definition", "module", N),
    d("type_definition", "type", &[NameRule::Dig("declarator", ID_KINDS)]),
    d("alias_declaration", "type", N),
];
const C_CALLS: &[Call] = &[c("call_expression", "function")];
const CSHARP_DEFS: &[Def] = &[
    d("class_declaration", "class", N),
    d("struct_declaration", "struct", N),
    d("record_declaration", "class", N),
    d("interface_declaration", "interface", N),
    d("enum_declaration", "enum", N),
    d("method_declaration", "method", N),
    d("constructor_declaration", "method", N),
    d("property_declaration", "method", N),
    d("namespace_declaration", "module", N),
    d("delegate_declaration", "type", N),
];
const JS_DEFS: &[Def] = &[
    d("function_declaration", "function", N),
    d("generator_function_declaration", "function", N),
    d("class_declaration", "class", N),
    d("method_definition", "method", N),
    Def {
        node: "variable_declarator",
        kind: "function",
        name: N,
        value_kinds: &["arrow_function", "function_expression", "function"],
        head_words: &[],
        needs_field: "",
        kind_by_head: false,
    },
];
const TS_DEFS: &[Def] = &[
    d("function_declaration", "function", N),
    d("generator_function_declaration", "function", N),
    d("class_declaration", "class", N),
    d("abstract_class_declaration", "class", N),
    d("method_definition", "method", N),
    d("interface_declaration", "interface", N),
    d("type_alias_declaration", "type", N),
    d("enum_declaration", "enum", N),
    Def {
        node: "variable_declarator",
        kind: "function",
        name: N,
        value_kinds: &["arrow_function", "function_expression", "function"],
        head_words: &[],
        needs_field: "",
        kind_by_head: false,
    },
];
const JS_CALLS: &[Call] = &[c("call_expression", "function"), c("new_expression", "constructor")];
const JS_WRAPPERS: &[&str] = &["export_statement", "lexical_declaration", "variable_declaration", "decorator"];
const JS_IMPORTS: &[ImportRule] = &[
    imp("import_statement", ImportHow::FirstString),
    imp("export_statement", ImportHow::FirstString),
    imp_call("call_expression", "function", &["require", "import"], ImportHow::FirstString),
];
const C_IMPORTS: &[ImportRule] = &[imp("preproc_include", ImportHow::FirstString)];
const NO_IMPORTS: &[ImportRule] = &[];

const CORE: &[LangSpec] = &[
    LangSpec {
        id: "python",
        exts: &[".py", ".pyi"],
        files: &[],
        language: || tree_sitter_python::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N), d("class_definition", "class", N)],
        calls: &[c("call", "function")],
        docstring: true,
        wrappers: &["decorated_definition"],
        outer: &["decorated_definition"],
        body: "body",
        imports: &[imp("import_statement", ImportHow::Fields("name")), imp("import_from_statement", ImportHow::Fields("module_name"))],
    },
    LangSpec {
        id: "javascript",
        exts: &[".js", ".jsx", ".mjs", ".cjs"],
        files: &[],
        language: || tree_sitter_javascript::LANGUAGE.into(),
        defs: JS_DEFS,
        calls: JS_CALLS,
        docstring: false,
        wrappers: JS_WRAPPERS,
        outer: &[],
        body: "body",
        imports: JS_IMPORTS,
    },
    LangSpec {
        id: "typescript",
        exts: &[".ts", ".mts", ".cts"],
        files: &[],
        language: || tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        defs: TS_DEFS,
        calls: JS_CALLS,
        docstring: false,
        wrappers: JS_WRAPPERS,
        outer: &[],
        body: "body",
        imports: JS_IMPORTS,
    },
    LangSpec {
        id: "tsx",
        exts: &[".tsx"],
        files: &[],
        language: || tree_sitter_typescript::LANGUAGE_TSX.into(),
        defs: TS_DEFS,
        calls: JS_CALLS,
        docstring: false,
        wrappers: JS_WRAPPERS,
        outer: &[],
        body: "body",
        imports: JS_IMPORTS,
    },
    LangSpec {
        id: "go",
        exts: &[".go"],
        files: &[],
        language: || tree_sitter_go::LANGUAGE.into(),
        defs: &[d("function_declaration", "function", N), d("method_declaration", "method", N), d("type_spec", "type", N)],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import_spec", ImportHow::FirstString)],
    },
    LangSpec {
        id: "rust",
        exts: &[".rs"],
        files: &[],
        language: || tree_sitter_rust::LANGUAGE.into(),
        defs: &[
            d("function_item", "function", N),
            d("struct_item", "struct", N),
            d("enum_item", "enum", N),
            d("trait_item", "trait", N),
            d("impl_item", "impl", &[NameRule::Field("name"), NameRule::Field("type")]),
            d("mod_item", "module", N),
            d("type_item", "type", N),
        ],
        calls: &[c("call_expression", "function"), c("macro_invocation", "macro")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp(
            "use_declaration",
            ImportHow::ChildKind(&[
                "scoped_identifier",
                "scoped_use_list",
                "identifier",
                "use_as_clause",
                "use_list",
                "use_wildcard",
                "crate",
                "self",
                "super",
            ]),
        )],
    },
    LangSpec {
        id: "java",
        exts: &[".java"],
        files: &[],
        language: || tree_sitter_java::LANGUAGE.into(),
        defs: &[
            d("class_declaration", "class", N),
            d("interface_declaration", "interface", N),
            d("method_declaration", "method", N),
            d("constructor_declaration", "method", N),
            d("enum_declaration", "enum", N),
            d("record_declaration", "class", N),
        ],
        calls: &[c("method_invocation", "name"), c("object_creation_expression", "type")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import_declaration", ImportHow::ChildKind(&["scoped_identifier", "identifier", "asterisk"]))],
    },
    LangSpec {
        id: "c",
        exts: &[".c", ".h"],
        files: &[],
        language: || tree_sitter_c::LANGUAGE.into(),
        defs: C_DEFS,
        calls: C_CALLS,
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "cpp",
        exts: &[".cc", ".cpp", ".cxx", ".c++", ".hpp", ".hh", ".hxx", ".h++", ".inl", ".ipp"],
        files: &[],
        language: || tree_sitter_cpp::LANGUAGE.into(),
        defs: CPP_DEFS,
        calls: C_CALLS,
        docstring: false,
        wrappers: &["template_declaration"],
        outer: &["template_declaration"],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "glsl",
        exts: &[".glsl", ".vert", ".frag", ".geom", ".comp", ".tesc", ".tese"],
        files: &[],
        language: || tree_sitter_glsl::LANGUAGE_GLSL.into(),
        defs: C_DEFS,
        calls: C_CALLS,
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "objc",
        exts: &[".mm"],
        files: &[],
        language: || tree_sitter_objc::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", &[NameRule::Dig("declarator", ID_KINDS)]),
            d("class_interface", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier"])]),
            d("class_implementation", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier"])]),
            d("protocol_declaration", "interface", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier"])]),
            d("implementation_definition", "method", &[NameRule::Custom(objc_selector)]),
            db("struct_specifier", "struct", N),
            db("enum_specifier", "enum", N),
        ],
        calls: &[c("call_expression", "function"), c("message_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "csharp",
        exts: &[".cs"],
        files: &[],
        language: || tree_sitter_c_sharp::LANGUAGE.into(),
        defs: CSHARP_DEFS,
        calls: &[c("invocation_expression", "function"), c("object_creation_expression", "type")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("using_directive", ImportHow::ChildKind(&["identifier", "qualified_name", "name_equals"]))],
    },
    LangSpec {
        id: "ruby",
        exts: &[".rb", ".rake", ".gemspec"],
        files: &["Gemfile", "Rakefile"],
        language: || tree_sitter_ruby::LANGUAGE.into(),
        defs: &[d("method", "function", N), d("singleton_method", "function", N), d("class", "class", N), d("module", "module", N)],
        calls: &[c("call", "method")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp_call("call", "method", &["require", "require_relative", "load"], ImportHow::FirstString)],
    },
    LangSpec {
        id: "php",
        exts: &[".php", ".phtml"],
        files: &[],
        language: || tree_sitter_php::LANGUAGE_PHP.into(),
        defs: &[
            d("function_definition", "function", N),
            d("method_declaration", "method", N),
            d("class_declaration", "class", N),
            d("interface_declaration", "interface", N),
            d("trait_declaration", "trait", N),
            d("enum_declaration", "enum", N),
        ],
        calls: &[
            c("function_call_expression", "function"),
            c("member_call_expression", "name"),
            c("scoped_call_expression", "name"),
            c("object_creation_expression", ""),
        ],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[
            imp("namespace_use_declaration", ImportHow::ChildKind(&["namespace_use_clause", "namespace_use_group"])),
            imp("require_expression", ImportHow::FirstString),
            imp("require_once_expression", ImportHow::FirstString),
            imp("include_expression", ImportHow::FirstString),
            imp("include_once_expression", ImportHow::FirstString),
        ],
    },
    LangSpec {
        id: "bash",
        exts: &[".sh", ".bash", ".zsh", ".ksh"],
        files: &[".bashrc", ".zshrc", ".profile"],
        language: || tree_sitter_bash::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N)],
        calls: &[c("command", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp_call("command", "name", &["source", "."], ImportHow::ChildKind(&["word", "string", "raw_string"]))],
    },
    LangSpec {
        id: "fish",
        exts: &[".fish"],
        files: &[],
        language: || tree_sitter_fish::language(),
        defs: &[d("function_definition", "function", N)],
        calls: &[c("command", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "powershell",
        exts: &[".ps1", ".psm1", ".psd1"],
        files: &[],
        language: || tree_sitter_powershell::LANGUAGE.into(),
        defs: &[
            d("function_statement", "function", &[NameRule::ChildKind(&["function_name"])]),
            d("class_statement", "class", &[NameRule::ChildKind(&["simple_name"])]),
            d("class_method_definition", "method", &[NameRule::ChildKind(&["simple_name"])]),
        ],
        calls: &[c("command", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "kotlin",
        exts: &[".kt", ".kts"],
        files: &[],
        language: || tree_sitter_kotlin_ng::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier", "simple_identifier"])]),
            dk("class_declaration", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier", "type_identifier", "simple_identifier"])]),
            d("object_declaration", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["identifier", "type_identifier", "simple_identifier"])]),
        ],
        calls: &[c("call_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import", ImportHow::ChildKind(&["qualified_identifier", "identifier"]))],
    },
    LangSpec {
        id: "swift",
        exts: &[".swift"],
        files: &[],
        language: || tree_sitter_swift::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", &[NameRule::Field("name"), NameRule::ChildKind(&["simple_identifier"])]),
            dk("class_declaration", "class", &[NameRule::Field("name"), NameRule::ChildKind(&["type_identifier", "user_type"])]),
            d("protocol_declaration", "interface", &[NameRule::Field("name"), NameRule::ChildKind(&["type_identifier"])]),
            d("init_declaration", "method", &[NameRule::ChildKind(&["init"])]),
        ],
        calls: &[c("call_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import_declaration", ImportHow::ChildKind(&["identifier"]))],
    },
    LangSpec {
        id: "scala",
        exts: &[".scala", ".sc"],
        files: &[],
        language: || tree_sitter_scala::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", N),
            d("function_declaration", "function", N),
            d("class_definition", "class", N),
            d("object_definition", "class", N),
            d("trait_definition", "trait", N),
            d("enum_definition", "enum", N),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import_declaration", ImportHow::Text)],
    },
    LangSpec {
        id: "groovy",
        exts: &[".groovy", ".gradle", ".gvy"],
        files: &["Jenkinsfile"],
        language: || tree_sitter_groovy::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", N),
            d("method_declaration", "method", N),
            d("class_declaration", "class", N),
            d("interface_declaration", "interface", N),
            d("enum_declaration", "enum", N),
        ],
        calls: &[c("function_call", "function"), c("juxt_function_call", "name"), c("method_call", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import", ImportHow::Text), imp("import_declaration", ImportHow::Text)],
    },
    LangSpec {
        id: "lua",
        exts: &[".lua"],
        files: &[],
        language: || tree_sitter_lua::LANGUAGE.into(),
        defs: &[d("function_declaration", "function", N)],
        calls: &[c("function_call", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp_call("function_call", "name", &["require"], ImportHow::FirstString)],
    },
    LangSpec {
        id: "elixir",
        exts: &[".ex", ".exs"],
        files: &[],
        language: || tree_sitter_elixir::LANGUAGE.into(),
        defs: &[
            Def {
                node: "call",
                kind: "function",
                name: &[NameRule::Custom(second_symbol)],
                value_kinds: &[],
                head_words: &["def", "defp", "defmacro", "defmacrop", "defguard", "defguardp", "defdelegate"],
                needs_field: "",
                kind_by_head: false,
            },
            Def {
                node: "call",
                kind: "module",
                name: &[NameRule::Custom(second_symbol)],
                value_kinds: &[],
                head_words: &["defmodule", "defprotocol", "defimpl"],
                needs_field: "",
                kind_by_head: false,
            },
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp_call("call", "target", &["alias", "import", "use", "require"], ImportHow::ChildKind(&["arguments"]))],
    },
    LangSpec {
        id: "erlang",
        exts: &[".erl", ".hrl"],
        files: &[],
        language: || tree_sitter_erlang::LANGUAGE.into(),
        defs: &[d("fun_decl", "function", &[NameRule::Custom(erlang_fun_name)]), d("record_decl", "struct", N), d("type_alias", "type", N)],
        calls: &[c("call", "expr")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "haskell",
        exts: &[".hs", ".lhs"],
        files: &[],
        language: || tree_sitter_haskell::LANGUAGE.into(),
        defs: &[
            d("function", "function", N),
            d("bind", "function", N),
            d("data_type", "type", N),
            d("newtype", "type", N),
            d("type_synomym", "type", N),
            d("class", "class", N),
            d("instance", "impl", N),
        ],
        calls: &[c("apply", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp("import", ImportHow::Text)],
    },
    LangSpec {
        id: "elm",
        exts: &[".elm"],
        files: &[],
        language: || tree_sitter_elm::LANGUAGE.into(),
        defs: &[
            d("value_declaration", "function", &[NameRule::Custom(elm_value_name)]),
            d("type_declaration", "type", N),
            d("type_alias_declaration", "type", N),
        ],
        calls: &[c("function_call_expr", "target")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp("import_clause", ImportHow::Text)],
    },
    LangSpec {
        id: "zig",
        exts: &[".zig", ".zon"],
        files: &[],
        language: || tree_sitter_zig::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", N),
            d("variable_declaration", "type", &[NameRule::Custom(zig_container_name)]),
            d("test_declaration", "function", &[NameRule::ChildKind(&["string", "identifier"])]),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("builtin_function", ImportHow::FirstString)],
    },
    LangSpec {
        id: "julia",
        exts: &[".jl"],
        files: &[],
        language: || tree_sitter_julia::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", &[NameRule::Custom(julia_name)]),
            d("short_function_definition", "function", &[NameRule::Custom(julia_name)]),
            d("assignment", "function", &[NameRule::Custom(julia_assignment)]),
            d("struct_definition", "struct", &[NameRule::Custom(julia_type_head)]),
            d("abstract_definition", "type", &[NameRule::Custom(julia_type_head)]),
            d("macro_definition", "macro", &[NameRule::Custom(julia_name)]),
            d("module_definition", "module", N),
        ],
        calls: &[c("call_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp("using_statement", ImportHow::Text), imp("import_statement", ImportHow::Text)],
    },
    LangSpec {
        id: "dart",
        exts: &[".dart"],
        files: &[],
        language: || tree_sitter_dart::LANGUAGE.into(),
        defs: &[
            d("class_declaration", "class", N),
            d("function_signature", "function", &[NameRule::Custom(dart_function_name)]),
            d("method_signature", "method", &[NameRule::Custom(dart_method_name)]),
            d("enum_declaration", "enum", N),
            d("mixin_declaration", "trait", N),
            d("extension_declaration", "impl", N),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp("import_or_export", ImportHow::FirstString)],
    },
    LangSpec {
        id: "r",
        exts: &[".r", ".R", ".rmd"],
        files: &[],
        language: || tree_sitter_r::LANGUAGE.into(),
        defs: &[d("binary_operator", "function", &[NameRule::Custom(r_assignment)])],
        calls: &[c("call", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp_call("call", "function", &["library", "require", "requireNamespace"], ImportHow::ChildKind(&["arguments"]))],
    },
    LangSpec {
        id: "nix",
        exts: &[".nix"],
        files: &[],
        language: || tree_sitter_nix::LANGUAGE.into(),
        defs: &[d("binding", "key", &[NameRule::Custom(nix_binding)])],
        calls: &[c("apply_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: &[imp_call("apply_expression", "function", &["import"], ImportHow::Text)],
    },
    LangSpec {
        id: "hcl",
        exts: &[".tf", ".tfvars", ".hcl", ".nomad"],
        files: &[],
        language: || tree_sitter_hcl::LANGUAGE.into(),
        defs: &[d("block", "block", &[NameRule::Custom(hcl_block_name)])],
        calls: &[c("function_call", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "solidity",
        exts: &[".sol"],
        files: &[],
        language: || tree_sitter_solidity::LANGUAGE.into(),
        defs: &[
            d("function_definition", "function", N),
            d("contract_declaration", "class", N),
            d("interface_declaration", "interface", N),
            d("library_declaration", "module", N),
            d("struct_declaration", "struct", N),
            d("event_definition", "type", N),
            d("modifier_definition", "function", N),
            d("enum_declaration", "enum", N),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "pascal",
        exts: &[".pas", ".dpr", ".lpr"],
        files: &[],
        language: || tree_sitter_pascal::LANGUAGE.into(),
        defs: &[
            d("declProc", "function", N),
            d("defProc", "function", &[NameRule::Custom(pascal_proc_name)]),
            d("declType", "type", N),
            d("declClass", "class", N),
        ],
        calls: &[c("exprCall", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "ada",
        exts: &[".adb", ".ads", ".ada"],
        files: &[],
        language: || tree_sitter_ada::LANGUAGE.into(),
        defs: &[
            d("subprogram_body", "function", &[NameRule::Custom(ada_name)]),
            d("subprogram_declaration", "function", &[NameRule::Custom(ada_name)]),
            d("package_declaration", "module", &[NameRule::Custom(ada_name)]),
            d("package_body", "module", &[NameRule::Custom(ada_name)]),
            d("full_type_declaration", "type", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[c("function_call", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "matlab",
        exts: &[],
        files: &[],
        language: || tree_sitter_matlab::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N), d("class_definition", "class", N)],
        calls: &[c("function_call", "name")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "elisp",
        exts: &[".el"],
        files: &[],
        language: || tree_sitter_elisp::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N), d("macro_definition", "macro", N), d("special_form", "key", &[NameRule::Custom(elisp_defvar)])],
        calls: &[c("list", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "scheme",
        exts: &[".scm", ".ss"],
        files: &[],
        language: || tree_sitter_scheme::LANGUAGE.into(),
        defs: &[Def {
            node: "list",
            kind: "function",
            name: &[NameRule::Custom(lisp_define_name)],
            value_kinds: &[],
            head_words: &["define", "define-syntax", "define-record-type"],
            needs_field: "",
            kind_by_head: true,
        }],
        calls: &[c("list", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "racket",
        exts: &[".rkt"],
        files: &[],
        language: || tree_sitter_racket::LANGUAGE.into(),
        defs: &[Def {
            node: "list",
            kind: "function",
            name: &[NameRule::Custom(lisp_define_name)],
            value_kinds: &[],
            head_words: &["define", "define-syntax", "struct", "define-struct"],
            needs_field: "",
            kind_by_head: true,
        }],
        calls: &[c("list", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "commonlisp",
        exts: &[".lisp", ".cl", ".asd"],
        files: &[],
        language: || tree_sitter_commonlisp::LANGUAGE_COMMONLISP.into(),
        defs: &[
            dk("defun", "function", &[NameRule::Custom(cl_defun_name)]),
            Def {
                node: "list_lit",
                kind: "function",
                name: &[NameRule::Custom(lisp_define_name)],
                value_kinds: &[],
                head_words: &["defgeneric", "defmethod", "defclass", "defstruct", "defvar", "defparameter", "defconstant", "deftype"],
                needs_field: "",
                kind_by_head: true,
            },
        ],
        calls: &[c("list_lit", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "vim",
        exts: &[".vim"],
        files: &[".vimrc"],
        language: || tree_sitter_vim::language(),
        defs: &[d("function_definition", "function", &[NameRule::Custom(vim_fn_name)])],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "gleam",
        exts: &[".gleam"],
        files: &[],
        language: || tree_sitter_gleam::LANGUAGE.into(),
        defs: &[
            d("function", "function", N),
            d("type_definition", "type", &[NameRule::ChildKind(&["type_name"])]),
            d("type_alias", "type", &[NameRule::ChildKind(&["type_name"])]),
            d("external_function", "function", N),
        ],
        calls: &[c("function_call", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "body",
        imports: &[imp("import", ImportHow::Text)],
    },
    LangSpec {
        id: "starlark",
        exts: &[".bzl", ".star", ".bazel"],
        files: &["BUILD", "WORKSPACE", "BUILD.bazel", "WORKSPACE.bazel", "MODULE.bazel"],
        language: || tree_sitter_starlark::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N)],
        calls: &[c("call", "function")],
        docstring: true,
        wrappers: &["decorated_definition"],
        outer: &["decorated_definition"],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "cmake",
        exts: &[".cmake"],
        files: &["CMakeLists.txt"],
        language: || tree_sitter_cmake::LANGUAGE.into(),
        defs: &[d("function_def", "function", &[NameRule::Custom(cmake_def_name)]), d("macro_def", "macro", &[NameRule::Custom(cmake_def_name)])],
        calls: &[c("normal_command", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "make",
        exts: &[".mk", ".mak"],
        files: &["Makefile", "makefile", "GNUmakefile"],
        language: || tree_sitter_make::LANGUAGE.into(),
        defs: &[d("rule", "function", &[NameRule::ChildKind(&["targets"])]), d("variable_assignment", "key", N), d("define_directive", "macro", N)],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "graphql",
        exts: &[".graphql", ".gql", ".graphqls"],
        files: &[],
        language: || tree_sitter_graphql::LANGUAGE.into(),
        defs: &[
            d("object_type_definition", "type", &[NameRule::ChildKind(&["name"])]),
            d("interface_type_definition", "interface", &[NameRule::ChildKind(&["name"])]),
            d("enum_type_definition", "enum", &[NameRule::ChildKind(&["name"])]),
            d("input_object_type_definition", "type", &[NameRule::ChildKind(&["name"])]),
            d("union_type_definition", "type", &[NameRule::ChildKind(&["name"])]),
            d("scalar_type_definition", "type", &[NameRule::ChildKind(&["name"])]),
            d("operation_definition", "function", &[NameRule::ChildKind(&["name"])]),
            d("fragment_definition", "function", &[NameRule::ChildKind(&["fragment_name"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "prisma",
        exts: &[".prisma"],
        files: &[],
        language: || tree_sitter_prisma::LANGUAGE.into(),
        defs: &[
            d("model_declaration", "class", &[NameRule::ChildKind(&["identifier"])]),
            d("enum_declaration", "enum", &[NameRule::ChildKind(&["identifier"])]),
            d("datasource_declaration", "block", &[NameRule::ChildKind(&["identifier"])]),
            d("generator_declaration", "block", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "puppet",
        exts: &[],
        files: &[],
        language: || tree_sitter_puppet::LANGUAGE.into(),
        defs: &[
            d("class_definition", "class", &[NameRule::ChildKind(&["identifier", "class_identifier"])]),
            d("defined_resource_type", "function", &[NameRule::ChildKind(&["class_identifier", "identifier"])]),
            d("node_definition", "block", &[NameRule::ChildKind(&["node_name"])]),
            d("function_definition", "function", &[NameRule::ChildKind(&["identifier", "class_identifier"])]),
        ],
        calls: &[c("function_call", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "bicep",
        exts: &[".bicep"],
        files: &[],
        language: || tree_sitter_bicep::LANGUAGE.into(),
        defs: &[
            d("resource_declaration", "block", &[NameRule::ChildKind(&["identifier"])]),
            d("module_declaration", "module", &[NameRule::ChildKind(&["identifier"])]),
            d("parameter_declaration", "type", &[NameRule::ChildKind(&["identifier"])]),
            d("variable_declaration", "type", &[NameRule::ChildKind(&["identifier"])]),
            d("output_declaration", "type", &[NameRule::ChildKind(&["identifier"])]),
            d("user_defined_function", "function", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "asm",
        exts: &[".s", ".S", ".asm"],
        files: &[],
        language: || tree_sitter_asm::LANGUAGE.into(),
        defs: &[d("label", "function", &[NameRule::ChildKind(&["ident", "word", "identifier"])])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "jsonnet",
        exts: &[".jsonnet", ".libsonnet"],
        files: &[],
        language: || tree_sitter_jsonnet::LANGUAGE.into(),
        defs: &[d("bind", "key", &[NameRule::ChildKind(&["ident", "id"])])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "ini",
        exts: &[".ini", ".cfg", ".conf", ".properties", ".editorconfig", ".gitconfig"],
        files: &[".gitconfig", ".npmrc", "setup.cfg"],
        language: || tree_sitter_ini::LANGUAGE.into(),
        defs: &[d("section", "block", &[NameRule::ChildKind(&["section_name"])])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "toml",
        exts: &[".toml"],
        files: &["Cargo.lock", "Pipfile", "poetry.lock"],
        language: || tree_sitter_toml_ng::LANGUAGE.into(),
        defs: &[
            d("table", "block", &[NameRule::ChildKind(&["bare_key", "dotted_key", "quoted_key"])]),
            d("table_array_element", "block", &[NameRule::ChildKind(&["bare_key", "dotted_key", "quoted_key"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "yaml",
        exts: &[".yaml", ".yml"],
        files: &[],
        language: || tree_sitter_yaml::LANGUAGE.into(),
        defs: &[d("block_mapping_pair", "key", &[NameRule::Field("key")])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "value",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "json",
        exts: &[".json", ".jsonc", ".json5", ".webmanifest"],
        files: &[],
        language: || tree_sitter_json::LANGUAGE.into(),
        defs: &[d("pair", "key", &[NameRule::Field("key")])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "value",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "xml",
        exts: &[".xml", ".xsd", ".xsl", ".xslt", ".svg", ".plist", ".csproj", ".pom"],
        files: &["pom.xml"],
        language: || tree_sitter_xml::LANGUAGE_XML.into(),
        defs: &[],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "html",
        exts: &[".html", ".htm", ".xhtml"],
        files: &[],
        language: || tree_sitter_html::LANGUAGE.into(),
        defs: &[],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "css",
        exts: &[".css", ".scss", ".less"],
        files: &[],
        language: || tree_sitter_css::LANGUAGE.into(),
        defs: &[
            d("rule_set", "block", &[NameRule::ChildKind(&["selectors"])]),
            d("keyframes_statement", "block", &[NameRule::ChildKind(&["keyframes_name"])]),
            d("media_statement", "block", &[NameRule::ChildKind(&["feature_query", "keyword_query", "binary_query"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "block",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "markdown",
        exts: &[".md", ".markdown", ".mdx"],
        files: &["README"],
        language: || tree_sitter_md::LANGUAGE.into(),
        defs: &[d("section", "section", &[NameRule::Custom(heading_text)])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "rst",
        exts: &[".rst"],
        files: &[],
        language: || tree_sitter_rst::LANGUAGE.into(),
        defs: &[d("section", "section", &[NameRule::Custom(heading_text)])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
];
// ERB/EJS templates are deliberately not in the registry: the embedded-template grammar only
// delimits directives, so the keyword heuristics (which see the embedded `def`s) do better.

#[cfg(feature = "extra-langs")]
const EXTRA: &[LangSpec] = &[
    LangSpec {
        id: "cuda",
        exts: &[".cu", ".cuh"],
        files: &[],
        language: || tree_sitter_cuda::LANGUAGE.into(),
        defs: CPP_DEFS,
        calls: C_CALLS,
        docstring: false,
        wrappers: &["template_declaration"],
        outer: &["template_declaration"],
        body: "body",
        imports: C_IMPORTS,
    },
    LangSpec {
        id: "ocaml",
        exts: &[".ml"],
        files: &[],
        language: || tree_sitter_ocaml::LANGUAGE_OCAML.into(),
        defs: &[
            d("let_binding", "function", &[NameRule::Field("pattern")]),
            d("type_binding", "type", N),
            d("module_binding", "module", N),
            d("class_binding", "class", N),
        ],
        calls: &[c("application_expression", "function")],
        docstring: false,
        wrappers: &["value_definition", "type_definition", "module_definition"],
        outer: &[],
        body: "body",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "ocaml_interface",
        exts: &[".mli"],
        files: &[],
        language: || tree_sitter_ocaml::LANGUAGE_OCAML_INTERFACE.into(),
        defs: &[
            d("value_specification", "function", &[NameRule::ChildKind(&["value_name"])]),
            d("type_binding", "type", N),
            d("module_declaration", "module", N),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "fsharp",
        exts: &[".fs", ".fsx", ".fsi"],
        files: &[],
        language: || tree_sitter_fsharp::LANGUAGE_FSHARP.into(),
        defs: &[
            d("function_or_value_defn", "function", &[NameRule::Custom(fsharp_name)]),
            d("type_definition", "type", &[NameRule::Custom(fsharp_type_name)]),
            d("module_defn", "module", &[NameRule::ChildKind(&["identifier", "long_identifier"])]),
        ],
        calls: &[c("application_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "fortran",
        exts: &[".f", ".f90", ".f95", ".f03", ".f08", ".for", ".F", ".F90"],
        files: &[],
        language: || tree_sitter_fortran::LANGUAGE.into(),
        defs: &[
            d("function", "function", &[NameRule::Custom(fortran_name)]),
            d("subroutine", "function", &[NameRule::Custom(fortran_name)]),
            d("module", "module", &[NameRule::Custom(fortran_name)]),
            d("program", "module", &[NameRule::Custom(fortran_name)]),
            d("derived_type_definition", "type", &[NameRule::Custom(fortran_name)]),
        ],
        calls: &[c("call_expression", ""), c("subroutine_call", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "d",
        exts: &[".d", ".di"],
        files: &[],
        language: || tree_sitter_d::LANGUAGE.into(),
        defs: &[
            d("function_declaration", "function", N),
            d("class_declaration", "class", N),
            d("struct_declaration", "struct", N),
            d("interface_declaration", "interface", N),
            d("enum_declaration", "enum", N),
            d("template_declaration", "macro", N),
        ],
        calls: &[c("call_expression", "")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "verilog",
        exts: &[".v", ".sv", ".svh", ".vh"],
        files: &[],
        language: || tree_sitter_verilog::LANGUAGE.into(),
        defs: &[
            d("module_declaration", "module", &[NameRule::Custom(verilog_module_name)]),
            d("function_declaration", "function", &[NameRule::Custom(verilog_fn_name)]),
            d("task_declaration", "function", &[NameRule::Custom(verilog_fn_name)]),
            d("class_declaration", "class", &[NameRule::ChildKind(&["class_identifier", "simple_identifier"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "vhdl",
        exts: &[".vhd", ".vhdl"],
        files: &[],
        language: || tree_sitter_vhdl::LANGUAGE.into(),
        defs: &[
            d("entity_declaration", "class", &[NameRule::ChildKind(&["identifier"])]),
            d("architecture_body", "impl", &[NameRule::ChildKind(&["identifier"])]),
            d("function_body", "function", &[NameRule::Custom(vhdl_name)]),
            d("procedure_body", "function", &[NameRule::Custom(vhdl_name)]),
            d("package_declaration", "module", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "odin",
        exts: &[".odin"],
        files: &[],
        language: || tree_sitter_odin::LANGUAGE.into(),
        defs: &[
            d("procedure_declaration", "function", &[NameRule::ChildKind(&["identifier"])]),
            d("struct_declaration", "struct", &[NameRule::ChildKind(&["identifier"])]),
            d("enum_declaration", "enum", &[NameRule::ChildKind(&["identifier"])]),
            d("union_declaration", "struct", &[NameRule::ChildKind(&["identifier"])]),
        ],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "llvm",
        exts: &[".ll"],
        files: &[],
        language: || tree_sitter_llvm::LANGUAGE.into(),
        defs: &[d("define", "function", &[NameRule::ChildKind(&["global_var"])]), d("type_def", "type", &[NameRule::ChildKind(&["local_var"])])],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "qml",
        exts: &[".qml"],
        files: &[],
        language: || tree_sitter_qmljs::LANGUAGE.into(),
        defs: &[d("ui_object_definition", "class", &[NameRule::Field("type_name")]), d("function_declaration", "function", N), d("ui_property", "type", N)],
        calls: JS_CALLS,
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: JS_IMPORTS,
    },
    LangSpec {
        id: "tlaplus",
        exts: &[".tla"],
        files: &[],
        language: || tree_sitter_tlaplus::LANGUAGE.into(),
        defs: &[d("operator_definition", "function", N), d("module", "module", N), d("function_definition", "function", N)],
        calls: &[],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
    LangSpec {
        id: "autohotkey",
        exts: &[".ahk"],
        files: &[],
        language: || tree_sitter_autohotkey::LANGUAGE.into(),
        defs: &[d("function_definition", "function", N), d("class_definition", "class", N)],
        calls: &[c("call_expression", "function")],
        docstring: false,
        wrappers: &[],
        outer: &[],
        body: "",
        imports: NO_IMPORTS,
    },
];

/// All languages this build knows (the `extra-langs` feature adds the heavy grammars).
pub static LANGS: std::sync::LazyLock<Vec<&'static LangSpec>> = std::sync::LazyLock::new(|| {
    #[allow(unused_mut)]
    let mut v: Vec<&'static LangSpec> = CORE.iter().collect();
    #[cfg(feature = "extra-langs")]
    v.extend(EXTRA.iter());
    v
});

// ---- custom name extractors (one per grammar quirk) --------------------------------------------

fn child_text(node: Node, src: &[u8], kinds: &[&str]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if kinds.contains(&ch.kind()) {
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

fn erlang_fun_name(node: Node, src: &[u8]) -> Option<String> {
    // fun_decl > function_clause > (name: atom)
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "function_clause" {
            if let Some(n) = ch.child_by_field_name("name") {
                return Some(text(src, n.start_byte(), n.end_byte()));
            }
        }
    }
    None
}

fn nix_binding(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("attrpath").map(|n| text(src, n.start_byte(), n.end_byte()))
}

#[cfg(feature = "extra-langs")]
fn fsharp_name(node: Node, src: &[u8]) -> Option<String> {
    // function_or_value_defn > function_declaration_left > identifier
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "function_declaration_left" | "value_declaration_left") {
            return child_text(ch, src, &["identifier", "identifier_pattern", "long_identifier"])
                .or_else(|| ch.named_child(0).map(|n| text(src, n.start_byte(), n.end_byte())));
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn fsharp_type_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind().starts_with("type_") || ch.kind().ends_with("_type_defn") {
            if let Some(n) = ch.child_by_field_name("type_name") {
                return Some(text(src, n.start_byte(), n.end_byte()));
            }
            return child_text(ch, src, &["type_name", "identifier"]);
        }
    }
    None
}

fn elm_value_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "function_declaration_left" {
            return child_text(ch, src, &["lower_case_identifier"]);
        }
    }
    None
}

fn zig_container_name(node: Node, src: &[u8]) -> Option<String> {
    // `const Name = struct { ... }` : only when the value is a container
    let mut cur = node.walk();
    let mut has_container = false;
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "struct_declaration" | "enum_declaration" | "union_declaration" | "opaque_declaration") {
            has_container = true;
        }
    }
    if !has_container {
        return None;
    }
    child_text(node, src, &["identifier"])
}

fn julia_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "signature" {
            // signature > call_expression > identifier (or a dotted / typed callee)
            let mut inner = ch.named_child(0)?;
            while inner.kind() != "identifier" && inner.named_child_count() > 0 {
                inner = inner.named_child(0)?;
            }
            let t = text(src, inner.start_byte(), inner.end_byte());
            return Some(t.rsplit('.').next().unwrap_or(&t).to_string());
        }
        if ch.kind() == "identifier" {
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

/// Julia struct / abstract type: the identifier inside `type_head`
fn julia_type_head(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "type_head" {
            return child_text(ch, src, &["identifier"]).or_else(|| Some(text(src, ch.start_byte(), ch.end_byte())));
        }
    }
    None
}

/// Julia `f(x) = ...` (an assignment whose left side is a call)
fn julia_assignment(node: Node, src: &[u8]) -> Option<String> {
    let lhs = node.named_child(0)?;
    if lhs.kind() != "call_expression" {
        return None;
    }
    let f = lhs.named_child(0)?;
    let t = text(src, f.start_byte(), f.end_byte());
    Some(t.rsplit('.').next().unwrap_or(&t).to_string())
}

/// Dart: a function_signature directly inside a method_signature is the method itself (already a chunk)
fn dart_function_name(node: Node, src: &[u8]) -> Option<String> {
    if node.parent().map(|p| p.kind() == "method_signature").unwrap_or(false) {
        return None;
    }
    node.child_by_field_name("name").map(|n| text(src, n.start_byte(), n.end_byte()))
}

fn dart_method_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "function_signature" | "getter_signature" | "setter_signature" | "constructor_signature" | "factory_constructor_signature") {
            return ch.child_by_field_name("name").map(|n| text(src, n.start_byte(), n.end_byte())).or_else(|| child_text(ch, src, &["identifier"]));
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn fortran_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind().ends_with("_statement") {
            if let Some(n) = ch.child_by_field_name("name") {
                return Some(text(src, n.start_byte(), n.end_byte()));
            }
            return child_text(ch, src, &["name", "identifier"]);
        }
    }
    None
}

fn hcl_block_name(node: Node, src: &[u8]) -> Option<String> {
    // block: identifier string_lit* { ... }  -> "resource aws_instance web"
    let mut parts = Vec::new();
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        match ch.kind() {
            "identifier" => parts.push(text(src, ch.start_byte(), ch.end_byte())),
            "string_lit" => parts.push(text(src, ch.start_byte(), ch.end_byte()).trim_matches('"').to_string()),
            "block_start" | "body" | "{" => break,
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" "))
    }
}

fn pascal_proc_name(node: Node, src: &[u8]) -> Option<String> {
    node.child_by_field_name("name")
        .map(|n| text(src, n.start_byte(), n.end_byte()))
        .or_else(|| child_text(node, src, &["identifier", "genericDot", "procName"]))
}

fn ada_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "procedure_specification" | "function_specification") {
            return ch
                .child_by_field_name("name")
                .map(|n| text(src, n.start_byte(), n.end_byte()))
                .or_else(|| child_text(ch, src, &["identifier", "selected_component"]));
        }
        if ch.kind() == "identifier" {
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn verilog_module_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "module_header" | "module_nonansi_header" | "module_ansi_header") {
            return child_text(ch, src, &["simple_identifier", "module_identifier"]).or_else(|| {
                let mut c2 = ch.walk();
                for g in ch.children(&mut c2) {
                    if g.kind() == "module_identifier" || g.kind() == "simple_identifier" {
                        return Some(text(src, g.start_byte(), g.end_byte()));
                    }
                }
                None
            });
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn verilog_fn_name(node: Node, src: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "function_identifier" | "task_identifier") {
            return Some(text(src, n.start_byte(), n.end_byte()));
        }
        let mut cur = n.walk();
        let kids: Vec<Node> = n.children(&mut cur).collect();
        for k in kids.into_iter().rev() {
            if matches!(k.kind(), "function_body_declaration" | "task_body_declaration" | "function_identifier" | "task_identifier") {
                stack.push(k);
            }
        }
    }
    None
}

#[cfg(feature = "extra-langs")]
fn vhdl_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "function_specification" | "procedure_specification") {
            return child_text(ch, src, &["identifier", "operator_symbol"]);
        }
        if ch.kind() == "identifier" {
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

fn lisp_define_name(node: Node, src: &[u8]) -> Option<String> {
    // (define (name args) ...) | (define name ...) | (defun name ...)
    let mut cur = node.walk();
    let mut seen = 0;
    for ch in node.children(&mut cur) {
        if !ch.is_named() {
            continue;
        }
        seen += 1;
        if seen == 2 {
            if matches!(ch.kind(), "list" | "list_lit") {
                return ch.named_child(0).map(|n| text(src, n.start_byte(), n.end_byte()));
            }
            return Some(text(src, ch.start_byte(), ch.end_byte()));
        }
    }
    None
}

/// Objective-C method implementation: the selector (first keyword part or unary selector)
fn objc_selector(node: Node, src: &[u8]) -> Option<String> {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if matches!(n.kind(), "method_selector" | "unary_selector" | "keyword_selector") {
            let t = text(src, n.start_byte(), n.end_byte());
            let first = t.split(':').next().unwrap_or(&t).split_whitespace().last().unwrap_or("").to_string();
            return if first.is_empty() { None } else { Some(first) };
        }
        if n.kind() == "identifier" && n.parent().map(|p| p.kind() == "implementation_definition" || p.kind() == "method_definition").unwrap_or(false) {
            return Some(text(src, n.start_byte(), n.end_byte()));
        }
        let mut cur = n.walk();
        let kids: Vec<Node> = n.children(&mut cur).collect();
        for k in kids.into_iter().rev() {
            if !matches!(k.kind(), "compound_statement") {
                stack.push(k);
            }
        }
    }
    None
}

/// Elisp `(defvar name ...)` / `(defcustom name ...)`: special_form whose first symbol is a def* word
fn elisp_defvar(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    let syms: Vec<Node> = node.children(&mut cur).filter(|c| c.kind() == "symbol").collect();
    if syms.len() < 2 {
        return None;
    }
    let head = text(src, syms[0].start_byte(), syms[0].end_byte());
    if matches!(head.as_str(), "defvar" | "defcustom" | "defconst" | "defgroup" | "defface") {
        Some(text(src, syms[1].start_byte(), syms[1].end_byte()))
    } else {
        None
    }
}

/// Common Lisp `defun` node: defun_header > function_name
fn cl_defun_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "defun_header" {
            return ch.child_by_field_name("function_name").map(|n| text(src, n.start_byte(), n.end_byte()));
        }
    }
    None
}

fn vim_fn_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if ch.kind() == "function_declaration" {
            return child_text(ch, src, &["identifier", "scoped_identifier", "field_expression"]);
        }
    }
    None
}

fn cmake_def_name(node: Node, src: &[u8]) -> Option<String> {
    let mut cur = node.walk();
    for ch in node.children(&mut cur) {
        if matches!(ch.kind(), "function_command" | "macro_command") {
            let mut c2 = ch.walk();
            for g in ch.children(&mut c2) {
                if g.kind() == "argument_list" {
                    return child_text(g, src, &["argument"]);
                }
            }
        }
    }
    None
}

/// The first word of a node's text (for `head_words` matching).
pub fn head_word(node: Node, src: &[u8]) -> String {
    let t = text(src, node.start_byte(), node.end_byte());
    t.trim_start_matches(['(', ' ', '\t']).split(|c: char| c.is_whitespace() || c == '(').next().unwrap_or("").to_string()
}

pub fn by_id(id: &str) -> Option<&'static LangSpec> {
    LANGS.iter().find(|s| s.id == id).copied()
}

/// Extensions shared by several languages are decided by looking at the text.
pub fn sniff(path: &str, source: &str) -> Option<&'static LangSpec> {
    let lower = path.rsplit('/').next().unwrap_or(path).to_lowercase();
    let head = crate::text::prefix_bytes(source, 4000);
    let id = if lower.ends_with(".m") {
        if ["@interface", "@implementation", "#import", "@end", "@protocol"].iter().any(|k| head.contains(k)) {
            "objc"
        } else {
            "matlab"
        }
    } else if lower.ends_with(".pp") {
        if ["procedure ", "function ", "begin", "end;", "unit ", "program "].iter().any(|k| source.contains(k)) && !head.contains("$") {
            "pascal"
        } else {
            "puppet"
        }
    } else if lower.ends_with(".cls") {
        if head.contains("\\NeedsTeXFormat") || head.contains("\\ProvidesClass") {
            "latex"
        } else {
            "apex"
        }
    } else {
        return None;
    };
    by_id(id)
}

pub fn spec_for(path: &str) -> Option<&'static LangSpec> {
    let base = path.rsplit('/').next().unwrap_or(path);
    let lower = base.to_lowercase();
    for spec in LANGS.iter() {
        if spec.files.iter().any(|f| f.eq_ignore_ascii_case(base)) {
            return Some(*spec);
        }
    }
    // longest matching extension first (".d.ts" style suffixes are listed explicitly)
    let mut best: Option<(&'static LangSpec, usize)> = None;
    for spec in LANGS.iter() {
        for ext in spec.exts {
            let e = ext.to_lowercase();
            if lower.ends_with(&e) && lower.len() > e.len() && best.map(|(_, l)| e.len() > l).unwrap_or(true) {
                best = Some((*spec, e.len()));
            }
        }
    }
    // case-sensitive extensions (.R vs .r, .S vs .s) are the same grammar here, so lowercase is fine
    best.map(|(s, _)| s)
}

pub fn language_ids() -> Vec<&'static str> {
    LANGS.iter().map(|s| s.id).collect()
}
