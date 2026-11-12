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
matches each one. `scrutate grep`. Its one way in is `instant::search`: a **query** (what to match
and how to report it), a **file set** (a walk with its rules, or a list of files) and a cancel flag
it reads and never sets.

**Query**: what an Engine A search asks for (`instant::Query`): the **pattern** (`instant::Pattern`:
the text, its mode, case, whole words, multi-line), whether lines that do not match are the matches,
the report (lines, counts or files), context lines and limits.

**File set**: which files an Engine A search reads (`instant::Files`): the roots of a walk with its
rules (hidden files, ignore files, globs, types, depth, size), a list of files under a root, or
text held in memory with a name for each, and the threads that read them.

**Record**: one Engine A result, of one kind (`instant::Record`): a match, a context line, a count,
a file, or a note on a file (its search stopped at a NUL byte).

**Engine B**: the index and everything answered from it (`index.rs`, `search.rs`, `intel.rs`,
`resolve.rs`): ranked search, symbols, callers, callees, references, structural queries.

**Text index**: Engine B's trigram index over file contents (`textindex.rs`). It tells Engine A
which files can match, so `grep --index` opens only those.

**Chunk**: a piece of a file the index stores as one unit, usually a function, class or method.
**Symbol** is the name for a chunk when the question is "what encloses this line"; in code,
`index::Symbol` is a chunk without its text, what name resolution reads.

**Index**: Engine B's store of chunks, call edges and imports (`index::Index`). Other modules ask
it questions (the chunks of a file, of a name, at a line; the callers of a name) and never see
its tables.

**Hit**: one result. An Engine A hit is a matching line (`instant::Record::Match`); an Engine B hit
is a chunk (`search::Hit`).

**Occurrence**: an Engine A hit for a name, before it is known what the name refers to.
**Reference** is an occurrence together with the definition it resolves to.

**Names**: the one owner of what a name refers to (`names::Names`). Callers, callees, references,
dependencies, dependents and the language server's definition ask it what the occurrence at a
place refers to, and it answers with the definition and how that was found, or why none was. One
request opens one, so each file is read and parsed once. The rules it applies live in `resolve.rs`.

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

**Model set**: the vector model, the ranker and the feature list the ranker reads
(`models::ModelSet`), each loaded on its first use. The index stores the vector model's
signature; another model makes the vectors anew and keeps the rest of the index.

**Translation**: what a pattern asks every matcher of it to find (`Pattern::translate`): the
pattern in the form its mode means, and whether case is folded. Engine A's matcher and the text
index's trigram condition are both built from it.

**Run**: one Engine A search in progress (`instant::Run`): the matcher, query, file set, counters,
stop flag and output target that every file search of it shares. The stop flag is set by the
caller's cancel flag or by the search itself (enough matches, a closed pipe).

**Byte source**: what one Engine A file search reads (`source.rs`): a file on disk, standard
input, an archive member or text held in memory. It gives its size when it knows it, positioned
reads when it has them, and one sequential read; the read rules (first read, byte-order mark, NUL
byte, size thresholds, split, long line, searcher setup) are written once on top of that.

**Repository files**: which files belong to the repository for each index (`files.rs`): the code
index's list (hidden files included, cache and dependency directories skipped), Engine A's walk
(hidden files skipped, the same ignore files), the files a search of the repository covers
(`files::repository_files`: the code index's walk, for `grep_code` and references), and
**membership**, the test of one changed path that the watcher asks.
