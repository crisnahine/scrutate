# Glossary

The words this codebase uses for its own concepts. One meaning each; code, docs and reviews use them
the same way.

**Engine A**: instant search of files with no index (`instant.rs`). Walks the repository files and
matches each one. `scrutate grep`.

**Engine B**: the index and everything answered from it (`index.rs`, `search.rs`, `intel.rs`,
`resolve.rs`): ranked search, symbols, callers, callees, references, structural queries.

**Text index**: Engine B's trigram index over file contents (`textindex.rs`). It tells Engine A
which files can match, so `grep --index` opens only those.

**Chunk**: a piece of a file the index stores as one unit, usually a function, class or method.
**Symbol** is the name for a chunk when the question is "what encloses this line".

**Hit**: one result. An Engine A hit is a matching line (`instant::Hit`); an Engine B hit is a chunk
(`search::Hit`).

**Occurrence**: an Engine A hit for a name, before it is known what the name refers to.
**Reference** is an occurrence together with the definition it resolves to.

**Grouped grep**: an Engine A search whose matching lines are grouped by the symbol that encloses
them (`intel::grouped_grep`). The MCP `grep_code` tool, `grep --symbols` and both kinds of
references use it.

**Front end**: one of the four ways in: the CLI, the MCP server, the LSP server, the Python
bindings.

**Operation**: one question a front end can ask of the index (`ops::Op`): ranked search, grep,
symbol, callers, callees, read, outline, dependencies, dependents, references, similar,
duplicates, structural search, reindex. An operation owns its arguments, their defaults and
limits, and the rule for refreshing the index first. Front ends parse into operations; they hold
no defaults of their own.

**Answer**: the typed result of an operation (`ops::Answer`), with one text form and one JSON
form. Every front end renders the same answer.
