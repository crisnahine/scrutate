"""Source parsing: split files into symbol chunks with separate fields.

A chunk has: name, signature, documentation (docstring / leading comment), body, and the
full text for display. tree-sitter is used for supported languages; everything else becomes
overlapping line windows so it stays searchable.
"""
from __future__ import annotations

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
_TS_DEFS = {**_JS_DEFS, "interface_declaration": "interface", "type_alias_declaration": "type",
            "enum_declaration": "enum"}
_JS_CALLS = {"call_expression": "function", "new_expression": "constructor"}

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
                     "type_spec": "type"},
               calls={"call_expression": "function"}),
    "rust": dict(load=_load("tree_sitter_rust"),
                 defs={"function_item": "function", "struct_item": "struct", "enum_item": "enum",
                       "trait_item": "trait", "impl_item": "impl"},
                 calls={"call_expression": "function", "macro_invocation": "macro"}),
    "java": dict(load=_load("tree_sitter_java"),
                 defs={"class_declaration": "class", "interface_declaration": "interface",
                       "method_declaration": "method", "constructor_declaration": "method",
                       "enum_declaration": "enum"},
                 calls={"method_invocation": "name", "object_creation_expression": "type"}),
}

EXT_LANG = {".py": "python", ".js": "javascript", ".jsx": "javascript", ".mjs": "javascript",
            ".cjs": "javascript", ".ts": "typescript", ".tsx": "tsx", ".go": "go", ".rs": "rust",
            ".java": "java"}

TEXT_EXTS = {
    ".md", ".rst", ".txt", ".json", ".yaml", ".yml", ".toml", ".ini", ".cfg", ".sh", ".bash",
    ".c", ".h", ".cc", ".cpp", ".hpp", ".cs", ".rb", ".php", ".swift", ".kt", ".scala", ".sql",
    ".html", ".css", ".scss", ".vue", ".svelte", ".lua", ".r", ".dart", ".ex", ".exs", ".tf",
    ".proto", ".graphql", ".gradle", ".xml",
}

_parsers: dict[str, Parser] = {}
_CONTAINERS = {"class", "impl", "interface", "trait", "enum", "struct", "type"}
_COMMENT_TYPES = {"comment", "line_comment", "block_comment", "doc_comment"}
MAX_CONTAINER_LINES = 40
MAX_CHUNK_LINES = 200


def language_for(path: str) -> str | None:
    return EXT_LANG.get(os.path.splitext(path)[1].lower())


def is_indexable(path: str) -> bool:
    ext = os.path.splitext(path)[1].lower()
    return ext in EXT_LANG or ext in TEXT_EXTS or os.path.basename(path).lower() in {"dockerfile", "makefile"}


def _parser(lang: str) -> Parser:
    p = _parsers.get(lang)
    if p is None:
        p = _parsers[lang] = Parser(LANGS[lang]["load"]())
    return p


# ---------------------------------------------------------------------- helpers
_PYSTR = re.compile(r'^[rRuUbBfF]{0,2}("""|\'\'\'|"|\')')
_COMMENT_MARK = re.compile(r"^\s*(/\*+|\*+/|\*|//+!?|#+|--)\s?")


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
    lines = [_COMMENT_MARK.sub("", ln).rstrip("*/ ").rstrip() for ln in raw.split("\n")]
    return "\n".join(ln for ln in lines if ln.strip()).strip()


def _head(text: str, n: int) -> str:
    lines = text.split("\n")
    return text if len(lines) <= n else "\n".join(lines[:n]) + "\n..."


def window_chunks(source: str, size: int = 50, overlap: int = 10, first_line: int = 0) -> list[Chunk]:
    """Overlapping line windows of ``source`` from 0-based line ``first_line`` on."""
    lines = split_lines(source)[first_line:]
    out: list[Chunk] = []
    step = max(1, size - overlap)
    for i in range(0, len(lines), step):
        seg = lines[i : i + size]
        if "".join(seg).strip():
            t = "\n".join(seg)
            out.append(Chunk("", "", "window", first_line + i + 1, first_line + i + len(seg), t, body=t))
        if i + size >= len(lines):
            break
    return out


def _callee_name(node, how: str, src: bytes) -> str | None:
    """Last identifier of a call target: a.b.c(x) -> 'c'."""
    target = node.child_by_field_name(how)
    if target is None and node.named_children:
        target = node.named_children[0]
    if target is None:
        return None
    txt = src[target.start_byte : target.end_byte].decode("utf8", "replace")
    for sep in ("::", ".", "->"):
        if sep in txt:
            txt = txt.rsplit(sep, 1)[-1]
    txt = txt.strip().rstrip("!")
    m = re.match(r"[A-Za-z_$][\w$]*", txt)
    return m.group(0) if m else None


def _def_name(node, src: bytes) -> str | None:
    n = node.child_by_field_name("name")
    if n is None and node.type == "impl_item":
        n = node.child_by_field_name("type")
    return src[n.start_byte : n.end_byte].decode("utf8", "replace") if n is not None else None


def _leading_comment(node, src: bytes) -> str:
    """Contiguous comment block directly above a definition (non-Python languages)."""
    anchor = node
    while anchor.parent is not None and anchor.parent.type in (
            "export_statement", "decorated_definition", "lexical_declaration",
            "variable_declaration", "decorator"):
        anchor = anchor.parent
    parts: list[str] = []
    want = anchor.start_point[0]
    prev = anchor.prev_sibling
    while prev is not None and prev.type in _COMMENT_TYPES and prev.end_point[0] >= want - 1:
        parts.append(src[prev.start_byte : prev.end_byte].decode("utf8", "replace"))
        want = prev.start_point[0]
        prev = prev.prev_sibling
    return _clean_comment("\n".join(reversed(parts))) if parts else ""


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
    stack: list[tuple[Chunk, str]] = []  # (current definition, qualname)
    is_py = lang == "python"

    def visit(node):
        t = node.type
        new = None
        kind = defs.get(t)
        name = None
        if kind is None and t == "variable_declarator" and lang in ("javascript", "typescript", "tsx"):
            val = node.child_by_field_name("value")
            if val is not None and val.type in ("arrow_function", "function_expression", "function"):
                kind = "function"
                nn = node.child_by_field_name("name")
                name = txt(nn.start_byte, nn.end_byte) if nn is not None else None
        if kind is not None:
            name = name or _def_name(node, src)
        if kind is not None and name:
            if kind == "function" and stack and stack[-1][0].kind == "class":
                kind = "method"
            parent_q = stack[-1][1] if stack else ""
            q = f"{parent_q}.{name}" if parent_q else name

            outer = node
            if is_py and node.parent is not None and node.parent.type == "decorated_definition":
                outer = node.parent
            body_node = node.child_by_field_name("body")
            sig_end = body_node.start_byte if body_node is not None else min(
                node.end_byte, node.start_byte + 200)
            sig = txt(outer.start_byte, sig_end).strip()
            doc = ""
            body_start = sig_end
            if is_py and body_node is not None:
                s, stmt = _py_docstring_node(body_node)
                if s is not None:
                    doc = _py_string_content(txt(s.start_byte, s.end_byte))
                    body_start = stmt.end_byte
            elif not is_py:
                doc = _leading_comment(node, src)
            full = txt(outer.start_byte, node.end_byte)
            body = txt(body_start, node.end_byte)
            if kind in _CONTAINERS:
                full, body = _head(full, MAX_CONTAINER_LINES), _head(body, MAX_CONTAINER_LINES)
            else:
                full, body = _head(full, MAX_CHUNK_LINES), _head(body, MAX_CHUNK_LINES)
            new = Chunk(name, q, kind, outer.start_point[0] + 1, node.end_point[0] + 1,
                        full, sig, doc, body)
            chunks.append(new)
            stack.append((new, q))
        elif t in calls and stack:
            callee = _callee_name(node, calls[t], src)
            if callee:
                stack[-1][0].calls.add(callee)
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
    if head.strip():
        mdoc = ""
        if is_py:
            s, _ = _py_docstring_node(tree.root_node)
            if s is not None:
                mdoc = _py_string_content(txt(s.start_byte, s.end_byte))
        chunks.insert(0, Chunk(stem_name, stem_name, "module", 1, head_end, head, "", mdoc, head))
    if first_def is None and head_end < len(lines):
        # no definitions: the rest of the file stays searchable as windows
        chunks.extend(window_chunks(source, first_line=head_end))
    if not chunks:
        return window_chunks(source)
    return chunks
