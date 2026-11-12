<!-- Head written by tools/gen_doc_assets.py: edit its table, then run it. -->
<p align="center">
<img src="docs/assets/banner-glossary.svg" alt="Glossary. What does each word of this codebase mean?" width="880">
</p>

# Glossary

[Documentation](docs/README.md) &nbsp;·&nbsp; Previous: [Languages](docs/LANGUAGES.md)

<!-- End of head. -->

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
form. Every front end renders the same answer; the MCP server sends the text, and with
`serve --structured` the JSON form too, as the **structured** result, when it fits the budget
(`Answer::structured`).

**Location**: where a symbol is, written `path:start-end` (or `path:line`). Every answer begins
each result with one, and every operation that takes a symbol accepts one in place of a name, to
mean that definition and no other of the same name (`intel::location`).

**Budget**: the size limit of an answer as text (`Answer::text_within`). A cut answer keeps whole
items and ends with how many are left and the `offset` that continues it.

**Repo**: an open repository (`scrutate::Repo`): the searcher on its index plus the rules for
keeping the index usable (bring it up to date, rebuild it when its database is damaged). Front ends
that stay open (the MCP server, the Python bindings, `watch`) hold one.

**Translation**: what a query's options ask every matcher of them to find
(`Options::translate`): the pattern in the form its mode means, and whether case is folded. Engine
A's matcher and the text index's trigram condition are both built from it.

**Run**: one Engine A search in progress (`instant::Run`): the matcher, options, counters, cancel
flag and output target that every file search of it shares.

**Repository files**: which files belong to the repository for each index (`files.rs`): the code
index's list (hidden files included, cache and dependency directories skipped), Engine A's walk
(hidden files skipped, the same ignore files), and **membership**, the test of one changed path
that the watcher asks.
