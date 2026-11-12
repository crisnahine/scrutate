"""Source parsing: split files into symbol chunks with separate fields.

A chunk has: name, signature, documentation (docstring / leading comment), body, and the
full text for display. tree-sitter is used for supported languages; everything else becomes
overlapping line windows so it stays searchable.
"""
from __future__ import annotations

import bisect
import inspect
import os
import re
import sys
from dataclasses import dataclass, field

from tree_sitter import Language, Parser

from .text import split_lines

sys.setrecursionlimit(20000)


@dataclass(slots=True)
class Chunk:
    name: str
    qualname: str
    kind: str
    start: int  # 1-based inclusive
    end: int
    text: str  # full text for display (containers truncated)
    sig: str = ""
    doc: str = ""
    body: str = ""
    calls: set[str] = field(default_factory=set)


# ---------------------------------------------------------------- language configs
def _load(modname: str, func: str = "language"):
    def get():
        import importlib

        return Language(getattr(importlib.import_module(modname), func)())

    return get


_JS_DEFS = {"function_declaration": "function", "generator_function_declaration": "function",
            "class_declaration": "class", "method_definition": "method"}
_TS_DEFS = {**_JS_DEFS, "abstract_class_declaration": "class", "interface_declaration": "interface",
            "type_alias_declaration": "type", "enum_declaration": "enum",
            "abstract_method_signature": "method", "function_signature": "function", "internal_module": "module"}
_JS_FUNCS = ("arrow_function", "function_expression", "function")
_TS_HOLDERS = {"variable_declarator": "name", "public_field_definition": "name"}
# node -> the field holding its name, when its value is a function
_JS_FUNC_HOLDERS = {"javascript": {"variable_declarator": "name", "field_definition": "property"},
                    "typescript": _TS_HOLDERS, "tsx": _TS_HOLDERS}
_JS_CALLS = {"call_expression": "function", "new_expression": "constructor"}
# the values that make an object literal's property a method: `post: function () {}`, `put: () => 3`
_JS_PROPERTY_FUNCS = (*_JS_FUNCS, "generator_function")

LANGS: dict[str, dict] = {
    "python": dict(load=_load("tree_sitter_python"),
                   defs={"function_definition": "function", "class_definition": "class"},
                   calls={"call": "function"}),
    "javascript": dict(load=_load("tree_sitter_javascript"), defs=_JS_DEFS, calls=_JS_CALLS),
    "typescript": dict(load=_load("tree_sitter_typescript", "language_typescript"),
                       defs=_TS_DEFS, calls=_JS_CALLS),
    "tsx": dict(load=_load("tree_sitter_typescript", "language_tsx"), defs=_TS_DEFS, calls=_JS_CALLS),
    "go": dict(load=_load("tree_sitter_go"),
               defs={"function_declaration": "function", "method_declaration": "method",
                     "type_spec": "type", "type_alias": "type"},
               calls={"call_expression": "function"}),
    "rust": dict(load=_load("tree_sitter_rust"),
                 defs={"function_item": "function", "struct_item": "struct", "enum_item": "enum",
                       "trait_item": "trait", "impl_item": "impl", "mod_item": "module", "type_item": "type",
                       "macro_definition": "macro", "function_signature_item": "function"},
                 calls={"call_expression": "function", "macro_invocation": "macro"}),
    "java": dict(load=_load("tree_sitter_java"),
                 defs={"class_declaration": "class", "interface_declaration": "interface",
                       "method_declaration": "method", "constructor_declaration": "method",
                       "enum_declaration": "enum", "record_declaration": "class"},
                 calls={"method_invocation": "name", "object_creation_expression": "type"}),
}

EXT_LANG = {".py": "python", ".js": "javascript", ".jsx": "javascript", ".mjs": "javascript",
            ".cjs": "javascript", ".ts": "typescript", ".tsx": "tsx", ".go": "go", ".rs": "rust",
            ".java": "java"}

_parsers: dict[str, Parser] = {}
_CONTAINERS = {"class", "impl", "interface", "trait", "enum", "struct", "type", "module"}
_COMMENT_TYPES = {"comment", "line_comment", "block_comment", "doc_comment"}
_ATTRIBUTE_TYPES = {"attribute_item", "decorator", "annotation"}
MAX_CONTAINER_LINES = 40
MAX_QUAL_DEPTH = 32  # qualified names keep at most this many segments, so deep nesting stays linear
MAX_CHUNK_LINES = 200
MAX_CHUNK_BYTES = 32 * 1024  # chunk text and body; a one-line file of nested functions holds its line per chunk otherwise


def language_for(path: str) -> str | None:
    return EXT_LANG.get(os.path.splitext(path)[1].lower())


# data, generated, archive, compiled and media files are never indexed; binary files are found by content
EXCLUDED_EXTS = (
    ".csv", ".tsv", ".log", ".min.js", ".min.css", ".map", ".lock", ".lockb", ".lockfile", ".sum", ".snap", ".pem", ".crt", ".key",
    ".der", ".asc", ".sig", ".bin", ".dat", ".parquet", ".ipynb_checkpoints",
    ".gz", ".tgz", ".zip", ".tar", ".xz", ".bz2", ".zst", ".7z", ".rar", ".jar", ".war", ".whl", ".egg", ".pyc",
    ".pyo", ".so", ".dylib", ".dll", ".exe", ".o", ".a", ".lib", ".class", ".wasm", ".rlib", ".pdb",
    ".png", ".jpg", ".jpeg", ".gif", ".bmp", ".ico", ".icns", ".webp", ".tif", ".tiff", ".psd", ".mp3", ".mp4",
    ".wav", ".ogg", ".flac", ".mov", ".avi", ".mkv", ".webm", ".pdf", ".woff", ".woff2", ".ttf", ".otf", ".eot",
    ".sqlite", ".db",
)


# lock files whose extension is that of hand-written files
EXCLUDED_NAMES = {"package-lock.json", "npm-shrinkwrap.json", "pnpm-lock.yaml", "packages.lock.json", "package.resolved",
                  ".terraform.lock.hcl"}


def is_indexable(path: str) -> bool:
    name = os.path.basename(path).lower()
    return not name.endswith(EXCLUDED_EXTS) and name not in EXCLUDED_NAMES


def _parser(lang: str) -> Parser:
    p = _parsers.get(lang)
    if p is None:
        p = _parsers[lang] = Parser(LANGS[lang]["load"]())
    return p


# ---------------------------------------------------------------------- helpers
_PYSTR = re.compile(r'^[rRuUbBfF]{0,2}("""|\'\'\'|"|\')')
# what Rust's `trim` and `char::is_whitespace` take as white space: Python's own set also has
# U+001C to U+001F, so `strip()` and `\s` would cut windows differently around such a line
WS = "\t\n\x0b\x0c\r \x85\xa0\u1680\u2000\u2001\u2002\u2003\u2004\u2005\u2006\u2007\u2008\u2009\u200a\u2028\u2029\u202f\u205f\u3000"
_COMMENT_MARK = re.compile(rf"^[{WS}]*(/\*+|\*+/|\*|//+!?|#+|--+)[{WS}]?")


def _py_string_content(raw: str) -> str:
    m = _PYSTR.match(raw)
    if not m:
        return raw
    q = m.group(1)
    inner = raw[m.end():]
    if inner.endswith(q):
        inner = inner[: -len(q)]
    return inspect.cleandoc(inner)


def _clean_comment(raw: str) -> str:
    lines = [_COMMENT_MARK.sub("", ln).rstrip("*/ ").rstrip(WS) for ln in raw.split("\n")]
    return "\n".join(ln for ln in lines if ln.strip(WS)).strip(WS)


def _capped(text: str, cut: bool = False) -> str:
    """`text` cut to MAX_CHUNK_BYTES at a character boundary, "\\n..." when cut."""
    b = text.encode("utf8")
    if len(b) > MAX_CHUNK_BYTES:
        e = MAX_CHUNK_BYTES
        while e > 0 and b[e] & 0xC0 == 0x80:
            e -= 1
        text, cut = b[:e].decode("utf8"), True
    return text + "\n..." if cut else text


def _head(text: str, n: int) -> str:
    lines = text.split("\n")
    return _capped(text) if len(lines) <= n else _capped("\n".join(lines[:n]), True)


def _kept(text: str, n: int) -> int:
    """Whole lines that `_head(text, n)` keeps (a huge number when it keeps them all)."""
    b = text.encode("utf8")
    if b[: MAX_CHUNK_BYTES + 1].count(b"\n") >= n:
        return n
    if len(b) <= MAX_CHUNK_BYTES:
        return 1 << 62
    return max(1, b[:MAX_CHUNK_BYTES].count(b"\n"))


def _windows(lines: list[str], a: int, b: int, qualname: str, size: int = 50, overlap: int = 10) -> list[Chunk]:
    """Overlapping line windows of the 0-based lines a..b."""
    out: list[Chunk] = []
    i = a
    while i < b:
        # as many lines as the text cap holds, one at least: a window of very long lines is short
        n = nbytes = 0
        while n < size and i + n < b:
            more = len(lines[i + n].encode()) + (1 if n else 0)
            if n and nbytes + more > MAX_CHUNK_BYTES:
                break
            nbytes += more
            n += 1
        seg = lines[i : i + n]
        if "".join(seg).strip(WS):
            t = _capped("\n".join(seg))
            out.append(Chunk("", qualname, "window", i + 1, i + len(seg), t, body=t))
        if i + n >= b:
            break
        # a window the cap cut short holds no line twice
        i += n if n < size else max(1, size - overlap)
    return out


def window_chunks(source: str, size: int = 50, overlap: int = 10, first_line: int = 0) -> list[Chunk]:
    """Overlapping line windows of ``source`` from 0-based line ``first_line`` on."""
    lines = split_lines(source)
    return _windows(lines, min(first_line, len(lines)), len(lines), "", size, overlap)


def _is_closer(line: str) -> bool:
    """Brackets alone or a block's closing word: nothing to search for."""
    t = line.strip(WS)
    return t.rstrip(";,") in ("end", "fi", "done", "esac") or all(c in "{}[]();," for c in t)


# how an attribute line above a definition starts, where the language writes them outside the node
_ATTR_MARK = {"rust": b"#[", "javascript": b"@", "typescript": b"@", "tsx": b"@", "python": b"@", "java": b"@"}


def _attr_start(src: bytes, at: int, mark: bytes) -> tuple[int, int]:
    """Where a definition's text starts when the attribute lines right above ``at`` are its own,
    and how many lines that adds."""
    above = 0
    if not mark:
        return at, 0
    while True:
        i = at
        while i > 0 and src[i - 1] in b" \t\r\x0c":  # only blanks may stand before `at` on its line
            i -= 1
        if i == 0 or src[i - 1] != 0x0A:
            return at, above
        prev = src.rfind(b"\n", 0, i - 1) + 1
        t = src[prev:i].lstrip(b" \t")
        if not t.startswith(mark):
            return at, above
        at = i - len(t)
        above += 1


def _uncovered_windows(lines: list[str], spans: list[tuple[int, int]], defs: list[Chunk], stem: str) -> list[Chunk]:
    """Windows over every stretch of lines that no span keeps, named after the innermost definition
    holding the stretch (the module outside them). A stretch of brackets and closing words alone makes none."""
    n = len(lines)
    depth = [0] * (n + 1)
    for a, b in spans:  # 1-based, inclusive
        if a <= b and a <= n:
            depth[a - 1] += 1
            depth[min(b, n)] -= 1
    owner = [-1] * n
    stack: list[int] = []
    order = sorted(range(len(defs)), key=lambda k: (defs[k].start, -defs[k].end))
    nxt = 0
    for i in range(n):
        while nxt < len(order) and defs[order[nxt]].start <= i + 1:
            stack.append(order[nxt])
            nxt += 1
        while stack and defs[stack[-1]].end < i + 1:
            stack.pop()
        owner[i] = stack[-1] if stack else -1
    out: list[Chunk] = []
    level, i = 0, 0
    covered = [False] * n
    for k in range(n):
        level += depth[k]
        covered[k] = level > 0
    while i < n:
        if covered[i]:
            i += 1
            continue
        j = i
        while j < n and not covered[j] and owner[j] == owner[i]:
            j += 1
        a, b = i, j
        while a < b and not lines[a].strip(WS):
            a += 1
        while b > a and not lines[b - 1].strip(WS):
            b -= 1
        if any(lines[k].strip(WS) and not _is_closer(lines[k]) for k in range(a, b)):
            out += _windows(lines, a, b, defs[owner[i]].qualname if owner[i] >= 0 else stem)
        i = j
    return out


def _callee_name(lang: str, node, how: str, src: bytes) -> str | None:
    """Last identifier of a call target: a.b.c(x) -> 'c'."""
    target = node.child_by_field_name(how)
    if target is None and node.named_children:
        target = node.named_children[0]
    if target is not None and target.type == "generic_function":  # `f::<T>(x)`: the name is before the turbofish
        target = target.child_by_field_name("function")
    if target is None:
        return None
    txt = src[target.start_byte : target.end_byte].decode("utf8", "replace")
    if txt.startswith("new") and txt[3:4] and txt[3:4] in WS:  # a callee `new Point` calls `Point`
        txt = txt[3:].lstrip(WS)
    for sep in ("::", ".", "->"):
        if sep in txt:
            txt = txt.rsplit(sep, 1)[-1]
    txt = txt.strip(WS).rstrip("!")
    if lang == "rust" and txt.startswith("r#"):  # `r#type` is the name `type`
        txt = txt[2:]
    # a JavaScript private method is named with its `#`
    private = txt.startswith("#") and lang in ("javascript", "typescript", "tsx")
    m = re.match(r"[A-Za-z_$][\w$]*", txt[1:] if private else txt)
    return ("#" if private else "") + m.group(0) if m else None


def _macro_arg_calls(node, src: bytes, out: set[str]) -> None:
    """Calls inside a Rust macro's token trees, which tree-sitter does not parse as expressions: a
    name directly followed by `(...)` (`a::b(x)`, `x.m(y)`), and the name of a nested macro."""
    def is_matches(n) -> bool:  # after the first comma of `matches!(x, Some(_))` comes a pattern
        return src[n.start_byte : n.end_byte].decode("utf8", "replace").endswith("matches")

    head = node.child_by_field_name("macro")
    after_comma = head is not None and is_matches(head)
    stack = [(c, after_comma) for c in node.children if c.type == "token_tree"]
    while stack:
        tt, stop_at_comma = stack.pop()
        kids = tt.children
        kind = [k.type for k in kids] + ["", ""]
        for i, k in enumerate(kids):
            if k.type == "," and stop_at_comma:
                break
            if k.type == "token_tree":
                # `#[derive(Debug)]` and `#![...]` are attributes, not calls
                if not (i >= 1 and kind[i - 1] == "#" or i >= 2 and kind[i - 1] == "!" and kind[i - 2] == "#"):
                    stack.append((k, i >= 2 and kind[i - 1] == "!" and kind[i - 2] == "identifier" and is_matches(kids[i - 2])))
            elif k.type == "identifier":
                parens = kind[i + 1] == "token_tree" and kids[i + 1].child_count > 0 and kids[i + 1].children[0].type == "("
                declared = i >= 1 and kind[i - 1] in ("fn", "struct", "enum", "union")
                nested_macro = kind[i + 1] == "!" and kind[i + 2] == "token_tree"
                if parens and not declared or nested_macro:
                    out.add(src[k.start_byte : k.end_byte].decode("utf8", "replace"))


def _def_name(node, src: bytes) -> str | None:
    n = node.child_by_field_name("name")
    if n is None and node.type == "impl_item":
        n = node.child_by_field_name("type")
    name = src[n.start_byte : n.end_byte].decode("utf8", "replace") if n is not None else None
    if name is not None and node.type == "impl_item":  # the bare type: `impl<T> crate::Stack<T>` is `Stack`
        name = name.split("<", 1)[0].rsplit("::", 1)[-1].strip(WS) or name
    if name is not None:
        # a quoted (`'run'() {}`) or computed (`[kRun]() {}`) name is named by what it holds
        name = name.strip(WS).strip('"`')
        if name.startswith("'"):
            name = name.strip("'")
        if name.startswith("[") and name.endswith("]") and len(name) > 2:
            name = name[1:-1]
    return name


_CAPS_NAME = re.compile(r"_*[A-Z][A-Z0-9_]*\Z")


def _constant_of(lang: str, node, src: bytes):
    """-> (names, node whose text each chunk shows) of the module or type level constants at `node`, or None."""
    def text(n):
        return src[n.start_byte : n.end_byte].decode("utf8", "replace")

    def kind_up(i):
        n = node
        for _ in range(i):
            n = n.parent if n is not None else None
        return n.type if n is not None else ""

    t, shown, name_nodes = node.type, node, []
    if lang == "rust" and t in ("const_item", "static_item") and kind_up(1) in ("source_file", "declaration_list"):
        name_nodes = [node.child_by_field_name("name")]
    elif lang == "go" and t in ("const_spec", "var_spec"):
        at = 2 if kind_up(1) == "var_spec_list" else 1
        if kind_up(at) not in ("const_declaration", "var_declaration") or kind_up(at + 1) != "source_file":
            return None
        if at == 1 and node.parent.named_child_count == 1:
            shown = node.parent
        # `const A, B = 1, 2` declares both names; the field also holds the commas
        name_nodes = [n for n in node.children_by_field_name("name") if n.type == "identifier"]
    elif lang in ("javascript", "typescript", "tsx") and t == "variable_declarator":
        decl = node.parent
        if decl.type != "lexical_declaration" or decl.child_count == 0 or decl.children[0].type != "const":
            return None
        if kind_up(2) == "program":
            shown = decl
        elif kind_up(2) == "export_statement" and kind_up(3) == "program":
            shown = decl.parent
        else:
            return None
        name_node = node.child_by_field_name("name")
        if name_node is None or name_node.type != "identifier":
            return None
        name_nodes = [name_node]
        val = node.child_by_field_name("value")
        if val is not None and val.type in _JS_FUNCS:
            return None
    elif lang in ("typescript", "tsx") and t == "property_identifier" and kind_up(1) == "enum_body":
        name_nodes = [node]
    elif lang in ("typescript", "tsx") and t == "enum_assignment" and kind_up(1) == "enum_body":
        name_nodes = [node.child_by_field_name("name")]
    elif lang == "java" and t == "field_declaration":
        mods = next((c for c in node.children if c.type == "modifiers"), None)
        words = [c.type for c in mods.children] if mods is not None else []
        if "static" not in words or "final" not in words:
            return None
        name_nodes = [d.child_by_field_name("name") for d in node.children_by_field_name("declarator")]
    elif (lang == "python" and t == "expression_statement" and node.named_child_count == 1
          and (kind_up(1) == "module" or (kind_up(1) == "block" and kind_up(2) == "class_definition"))):
        a = node.named_children[0]
        left = a.child_by_field_name("left") if a.type == "assignment" else None
        if left is not None and left.type == "identifier":
            name_nodes = [left]
        elif left is not None and left.type in ("pattern_list", "tuple_pattern", "list_pattern"):
            name_nodes = [n for n in left.named_children if n.type == "identifier"]
    elif lang in ("javascript", "typescript", "tsx") and t in ("field_definition", "public_field_definition") \
            and kind_up(1) == "class_body":
        words = [c.type for c in node.children]
        # JavaScript has no `readonly`: an ALL_CAPS static field is its constant
        ts = lang != "javascript"
        if "static" not in words or (ts and "readonly" not in words):
            return None
        n = node.child_by_field_name("name") or node.child_by_field_name("property")
        if n is not None and n.type == "property_identifier" and (ts or _CAPS_NAME.match(text(n))):
            name_nodes = [n]
    names = [text(n) for n in name_nodes if n is not None]
    names = [n for n in names if n != "_" and (lang != "python" or _CAPS_NAME.match(n))]
    return (names, shown) if names else None


def _object_owner(node, src: bytes) -> str | None:
    """The names that hold the object literal around a JS/TS method: `const store = { sub: { m() {} } }`
    gives `store.sub`, `module.exports.tool = {...}` gives `tool`. None for a literal nothing names."""
    obj = node.parent
    if obj is None or obj.type != "object":
        return None
    parts = []
    while obj.parent is not None:
        holder, named = obj.parent, None
        # `{...} as const`, `{...} satisfies T` and `({...})` are held by the name around them
        while holder.type in ("as_expression", "satisfies_expression", "parenthesized_expression") and holder.parent is not None:
            holder = holder.parent
        if holder.type == "pair":
            named = holder.child_by_field_name("key")
            named = named if named is not None and named.type == "property_identifier" else None
        elif holder.type == "variable_declarator":
            named = holder.child_by_field_name("name")
            named = named if named is not None and named.type == "identifier" else None
        elif holder.type == "assignment_expression":
            left = holder.child_by_field_name("left")
            # `module.exports = {...}` exports its members under their own names
            if left is not None and src[left.start_byte : left.end_byte] == b"module.exports":
                left = None
            if left is not None and left.type == "identifier":
                named = left
            elif left is not None and left.type == "member_expression":
                named = left.child_by_field_name("property")
        if named is None:
            break
        parts.append(src[named.start_byte : named.end_byte].decode("utf8", "replace"))
        if holder.type != "pair" or holder.parent is None or holder.parent.type != "object":
            break
        obj = holder.parent
    return ".".join(reversed(parts)) or None


def _js_member_path(node, src: bytes) -> str | None:
    """`a.b.c` of a member expression made of names only (no `this` or call), `a["b"]` read as `a.b`."""
    if node.type == "identifier":
        return src[node.start_byte : node.end_byte].decode("utf8", "replace")
    if node.type == "member_expression":
        key = node.child_by_field_name("property")
        if key is None or key.type != "property_identifier":
            return None
    elif node.type == "subscript_expression":
        # a computed key (`handlers[i]`) is no name
        index = node.child_by_field_name("index")
        if index is None or index.type != "string" or index.named_child_count != 1:
            return None
        key = index.named_children[0]
    else:
        return None
    key = src[key.start_byte : key.end_byte].decode("utf8", "replace")
    obj = node.child_by_field_name("object")
    if not key or "." in key or any(c in WS for c in key) or obj is None:
        return None
    base = _js_member_path(obj, src)
    return None if base is None else base + "." + key


def _js_this_member(node, src: bytes) -> str | None:
    """`this.x = function ...`: the member `x` of what the enclosing function or class builds."""
    left, right = node.child_by_field_name("left"), node.child_by_field_name("right")
    if left is None or right is None or right.type not in _JS_FUNCS:
        return None
    obj = left.child_by_field_name("object")
    if obj is None or obj.type != "this":
        return None
    if left.type == "member_expression":
        key = left.child_by_field_name("property")
        if key is None or key.type != "property_identifier":
            return None
    elif left.type == "subscript_expression":
        # `this["k"]` is `this.k`
        index = left.child_by_field_name("index")
        if index is None or index.type != "string" or index.named_child_count != 1:
            return None
        key = index.named_children[0]
    else:
        return None
    key = src[key.start_byte : key.end_byte].decode("utf8", "replace")
    return key if key and "." not in key and not any(c in WS for c in key) else None


def _js_default_export(node, src: bytes) -> str | None:
    """The kind of a file's default export written as an expression: `export default function () {}`,
    `export default class {}`, `module.exports = () => ...`. A declaration is a definition already."""
    kind = "class" if node.type == "class" else "function" if node.type in _JS_PROPERTY_FUNCS else None
    if kind is None or not node.is_named:
        return None
    p = node.parent
    if p is None:
        return None
    if p.type == "export_statement":
        value = p.child_by_field_name("value")
        ok = value is not None and value.id == node.id and any(c.type == "default" for c in p.children)
    elif p.type == "assignment_expression":
        right, left = p.child_by_field_name("right"), p.child_by_field_name("left")
        ok = right is not None and right.id == node.id and left is not None and _js_member_path(left, src) == "module.exports"
    else:
        ok = False
    return kind if ok else None


def _default_export_name(path: str) -> str:
    """The name of a file's anonymous default export: the file's stem, or its directory's name for an index file."""
    stem = os.path.splitext(os.path.basename(path))[0]
    folder = os.path.basename(os.path.dirname(path))
    return folder if stem == "index" and folder else stem


def _js_assigned(node, src: bytes):
    """-> (kind, owner, name) of a function assigned to a member: `Foo.prototype.bar = function`
    is the method `bar` of `Foo`, `$.fn.plugin = () => 1` the function `plugin` of `$.fn`, CommonJS
    `exports.NAME` and `module.exports.NAME` the bare `NAME`, `exports.a.b` the function `b` of `a`;
    (None, None, None) otherwise."""
    left, right = node.child_by_field_name("left"), node.child_by_field_name("right")
    if left is None or right is None or right.type not in _JS_FUNCS or left.type == "identifier":
        return None, None, None
    path = _js_member_path(left, src)
    if path is None:
        return None, None, None
    owner, sep, name = path.rpartition(".prototype.")
    if sep and owner and "." not in name:
        return "method", owner, name
    for pre in ("module.exports.", "exports."):
        if path.startswith(pre):
            owner, _, name = path[len(pre):].rpartition(".")
            return "function", owner or None, name
    if path == "module.exports":
        return None, None, None
    owner, _, name = path.rpartition(".")
    return "function", owner, name


def _last_row(node) -> int:
    """0-based last line holding the node's text: a node that ends with its line break ends at
    column 0 of the next line."""
    row, col = node.end_point
    return row - 1 if col == 0 and row > node.start_point[0] else row


def _leading_comment(node, src: bytes) -> tuple[str, tuple[int, int] | None]:
    """Contiguous comment block directly above a definition, and its lines (1-based) when it has text."""
    anchor = node
    header_row = -1  # the definition's own header line: a comment there is not about a nested definition
    while (p := anchor.parent) is not None:
        # the comment above the first statement of a Python body sits outside the body
        if p.type == "block" and anchor.prev_sibling is None and p.parent is not None and p.parent.type in (
                "class_definition", "function_definition"):
            header_row = p.parent.start_point[0]
        elif p.type == "type_declaration" and anchor.prev_sibling is not None and anchor.prev_sibling.type == "type":
            pass  # Go's `type T ...` on its own line: the comment stands above the declaration that holds T
        elif p.type == "statement_list" and anchor.prev_sibling is None:
            # Go: the comment above the first statement of a body sits outside the list
            if p.parent is not None and p.parent.parent is not None:
                header_row = p.parent.start_point[0]
        elif p.type not in ("export_statement", "decorated_definition", "lexical_declaration",
                            "variable_declaration", "decorator", "assignment_expression", "expression_statement"):
            break
        anchor = p
    parts: list[str] = []
    last = -1
    want = anchor.start_point[0]
    prev = anchor.prev_sibling
    while prev is not None:
        if prev.type == "\n" or prev.start_byte == prev.end_byte:
            prev = prev.prev_sibling
            continue
        if prev.type in _ATTRIBUTE_TYPES and _last_row(prev) >= want - 1:
            # a Rust attribute or a decorator between the comment and the definition
            want = prev.start_point[0]
            prev = prev.prev_sibling
            continue
        if prev.type not in _COMMENT_TYPES or _last_row(prev) < want - 1 or prev.start_point[0] == header_row:
            break
        text = src[prev.start_byte : prev.end_byte].decode("utf8", "replace")
        if text.startswith("#!"):
            break  # a shebang is not documentation
        if not parts:
            last = _last_row(prev)
        parts.append(text)
        want = prev.start_point[0]
        prev = prev.prev_sibling
    if not parts:
        return "", None
    doc = _clean_comment("\n".join(reversed(parts)))
    return doc, (want + 1, last + 1) if doc else None


def _py_docstring_node(block):
    """-> (string node, statement node) of a Python body's docstring, if any."""
    for stmt in block.children:
        if stmt.type == "comment":
            continue
        if stmt.type == "expression_statement" and stmt.named_child_count == 1:
            s = stmt.named_children[0]
            if s.type in ("string", "concatenated_string"):
                return s, stmt
        return None, None
    return None, None


def _qualify(parent: str, name: str) -> str:
    if not parent:
        return name
    if parent.count(".") + 1 >= MAX_QUAL_DEPTH:
        parent = parent.split(".", 1)[-1]
    return f"{parent}.{name}"


def _first_of_type(node, kind: str):
    stack = [node] if node is not None else []
    while stack:
        n = stack.pop()
        if n.type == kind:
            return n
        stack.extend(reversed(n.children))
    return None


# ------------------------------------------------------------------------- main
def parse_source(path: str, source: str) -> list[Chunk]:
    lang = language_for(path)
    stem_name = os.path.splitext(os.path.basename(path))[0]
    if lang is None:
        return window_chunks(source)
    cfg = LANGS[lang]
    defs: dict[str, str] = cfg["defs"]
    calls: dict[str, str] = cfg["calls"]
    src = source.encode("utf8")
    try:
        tree = _parser(lang).parse(src)
    except Exception:
        return window_chunks(source)

    def txt(a: int, b: int) -> str:
        return src[a:b].decode("utf8", "replace")

    chunks: list[Chunk] = []
    consts: list[Chunk] = []
    stack: list[tuple[Chunk, str]] = []  # (current definition, qualname)
    spans: list[tuple[int, int]] = []  # the lines each definition's text keeps whole
    loose: list[tuple[int, str]] = []  # calls outside every definition, by line
    is_py = lang == "python"

    def visit(node):
        t = node.type
        new = None
        kind = defs.get(t)
        name = None
        field = _JS_FUNC_HOLDERS.get(lang, {}).get(t)
        if kind is None and field:
            val = node.child_by_field_name("value")
            if val is not None and val.type in _JS_FUNCS:
                kind = "function"
                nn = node.child_by_field_name(field)
                name = txt(nn.start_byte, nn.end_byte) if nn is not None else None
        is_js = lang in ("javascript", "typescript", "tsx")
        if kind is None and t == "pair" and is_js:
            key, val = node.child_by_field_name("key"), node.child_by_field_name("value")
            if key is not None and key.type == "property_identifier" and val is not None and val.type in _JS_PROPERTY_FUNCS:
                kind, name = "method", txt(key.start_byte, key.end_byte)
        member_owner = None
        this_member = False
        if kind is None and t == "assignment_expression" and is_js:
            kind, member_owner, name = _js_assigned(node, src)
            if kind is None and stack and (name := _js_this_member(node, src)):
                kind, this_member = "method", True
        if kind is None and is_js and (kind := _js_default_export(node, src)):
            nn = node.child_by_field_name("name")
            name = txt(nn.start_byte, nn.end_byte) if nn is not None else _default_export_name(path)
        if kind is not None:
            name = name or _def_name(node, src)
            if lang == "rust" and name and name.startswith("r#"):
                name = name[2:]
        if kind is not None and name:
            if kind == "function" and stack and stack[-1][0].kind == "class":
                kind = "method"
            q = _qualify(stack[-1][1] if stack else "", name)
            if t == "method_declaration" and lang == "go":
                owner = _first_of_type(node.child_by_field_name("receiver"), "type_identifier")
                if owner is not None:
                    q = _qualify(_qualify(stack[-1][1] if stack else "", txt(owner.start_byte, owner.end_byte)), name)
            if is_js and t in ("method_definition", "pair") and (owner := _object_owner(node, src)) is not None:
                q = _qualify(_qualify(stack[-1][1] if stack else "", owner), name)
            if member_owner:
                q = _qualify(_qualify(stack[-1][1] if stack else "", member_owner), name)
            if this_member and stack[-1][0].kind == "method":
                # `this` in a method or a constructor is the method's owner
                q = _qualify(stack[-1][1].rpartition(".")[0], name)

            outer = node
            if is_py and node.parent is not None and node.parent.type == "decorated_definition":
                outer = node.parent
            body_node = node.child_by_field_name("body")
            sig_end = body_node.start_byte if body_node is not None else min(
                node.end_byte, node.start_byte + 200)
            text_start, above = _attr_start(src, outer.start_byte, _ATTR_MARK.get(lang, b""))
            sig = txt(text_start, sig_end).strip(WS)
            doc, doc_lines = _leading_comment(node, src)
            if doc_lines:
                # its documentation is searchable in its doc field
                spans.append(doc_lines)
            body_start = sig_end
            if is_py and body_node is not None:
                s, stmt = _py_docstring_node(body_node)
                if s is not None:
                    # the comment above comes first, then the docstring
                    ds = _py_string_content(txt(s.start_byte, s.end_byte))
                    doc = f"{doc}\n{ds}" if doc else ds
                    body_start = stmt.end_byte
            full = txt(text_start, node.end_byte)
            body = txt(body_start, node.end_byte)
            cap = MAX_CONTAINER_LINES if kind in _CONTAINERS else MAX_CHUNK_LINES
            start, end = outer.start_point[0] - above + 1, _last_row(node) + 1
            spans.append((start, min(end, start + _kept(full, cap) - 1)))
            full, body = _head(full, cap), _head(body, cap)
            new = Chunk(name, q, kind, start, end, full, sig, doc, body)
            chunks.append(new)
            stack.append((new, q))
        elif (const := _constant_of(lang, node, src)) is not None:
            # not on the stack: calls in the initializer stay with the enclosing definition
            cnames, shown = const
            for cname in cnames:
                if lang == "rust" and cname.startswith("r#"):
                    cname = cname[2:]
                consts.append(Chunk(cname, _qualify(stack[-1][1] if stack else "", cname), "constant", shown.start_point[0] + 1,
                                    _last_row(shown) + 1, _head(txt(shown.start_byte, shown.end_byte), MAX_CHUNK_LINES),
                                    doc=_leading_comment(shown, src)[0]))
        elif t in calls:
            got = stack[-1][0].calls if stack else set()
            callee = _callee_name(lang, node, calls[t], src)
            if callee:
                got.add(callee)
            if t == "macro_invocation":
                _macro_arg_calls(node, src, got)
            if not stack:
                loose.extend((node.start_point[0] + 1, c) for c in got)
        for ch in node.children:
            visit(ch)
        if new is not None:
            stack.pop()

    visit(tree.root_node)

    # module-level header chunk (module docstring, imports, constants)
    first_def = min((c.start for c in chunks), default=None)
    lines = split_lines(source)
    head_end = min((first_def - 1) if first_def else len(lines), 60)
    head = "\n".join(lines[:head_end])
    defs = list(chunks)
    spans.append((1, min(head_end, _kept(head, 1 << 62))))
    head = _capped(head)
    module = None
    if head.strip(WS):
        mdoc = ""
        if is_py:
            s, _ = _py_docstring_node(tree.root_node)
            if s is not None:
                mdoc = _py_string_content(txt(s.start_byte, s.end_byte))
        module = Chunk(stem_name, stem_name, "module", 1, head_end, head, "", mdoc, head)
        chunks.insert(0, module)
    # every line no chunk keeps (constants have no postings) is searchable as windows
    windows = _uncovered_windows(lines, spans, defs, stem_name)
    # a call outside every definition goes to the module chunk, else the first window holding its line
    ends = [w.end for w in windows]  # they rise with the starts
    for line, callee in loose:
        if line <= head_end:
            if module is not None:
                module.calls.add(callee)
            continue
        i = bisect.bisect_left(ends, line)
        if i < len(windows) and windows[i].start <= line:
            windows[i].calls.add(callee)
    chunks.extend(windows)
    chunks += consts  # last: the index gives the chunks before them their lexical rows (constants get none)
    if not chunks:
        return window_chunks(source)
    return chunks
