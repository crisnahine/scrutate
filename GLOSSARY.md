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
rules (hidden files, ignore files, globs, types, depth, size; a type matched before the ignore
rules as a glob is, or after them as ripgrep's `--type` is), a list of files under a root, or
text held in memory with a name for each, and the threads that read them.

**Record**: one Engine A result, of one kind (`instant::Record`): a match, a context line, a count,
a file, or a note on a file (its search stopped at a NUL byte).

**Engine B**: the index and everything answered from it (`index.rs`, `search.rs`, `intel.rs`,
`resolve/`): ranked search, symbols, callers, callees, references, structural queries.

**Text index**: Engine B's trigram index over file contents (`textindex.rs`). It tells Engine A
which files can match, so `grep --index` opens only those.

**Chunk**: a piece of a file the index stores as one unit, usually a function, class or method.
**Symbol** is the name for a chunk when the question is "what encloses this line"; in code,
`index::Symbol` is a chunk without its text, what name resolution reads.

**Index**: Engine B's store of chunks, call edges and imports (`index::Index`). Other modules ask
it questions (the chunks of a file, of a name, at a line; the callers of a name) and never see
its tables.

**Custom language**: a language a `[[language]]` table adds without a build (`custom.rs`), in the
root's `.scrutate.toml` or the user's configuration file: a name, extensions, comment marks and one
regular expression per kind of definition, whose first group is the name. `custom::current` is the
set in effect; its **stamp** (each extension with its language's signature) tells an index update
which files to read again.

**Artifact**: what parsing and tokenizing make of one file, kept in the **artifact store**
(`artifacts.rs`, a table of the index) under a key of its content and the parts of its path the
parser reads (the file name, and for an `index` file its directory's name), so the same bytes
under another path, after a branch switch, or in the main checkout of a linked git work tree are
not parsed again. The **path map** (the `art` column of the `files` table) names each file's
artifact; `artifacts::derive` makes a path's rows and postings from an artifact. The artifacts
no file names are bounded and collected by the update that dropped them.

**Hit**: one result. An Engine A hit is a matching line (`instant::Record::Match`); an Engine B hit
is a chunk (`search::Hit`).

**Occurrence**: an Engine A hit for a name, before it is known what the name refers to.
**Reference** is an occurrence together with the definition it resolves to.

**Names**: the one owner of what a name refers to (`names::Names`). Callers, callees, references,
dependencies, dependents and the language server's definition ask it what the occurrence at a
place refers to, and it answers with the definition and how that was found, or why none was. One
request opens one, so each file is read and parsed once. The rules it applies live in `resolve/`.

**Dead** and **undecided**: what `dead` (`dead.rs`, `Names::dead`) calls a function, method,
class or type that is no entry point of its language (`main`, a test, an export, a public item of
a library, one an attribute or decorator registers). Dead: no line of the repository outside its
own names it. Undecided: some line names it, but the resolver ties none of them to it (a string, a
comment, another definition of the name, a use it cannot place), or none does and its type
extends another whose code may call it. A use that cannot be decided never makes one dead.

**Kept answers**: what `callers`, `callees` and `dead` learned of each call site, stored per request
(`answers.rs`, `.scrutate/answers/`) with what the run read: the index database and its data
version, the executable, the root and the `stat` of every file the resolver read. A later run of
the same request uses them only while all of that is the same, so it gives the answer the stored
run gave; anything else resolves again.

**Repo map**: the files that define code, ranked by PageRank over the edges between files
(`repomap.rs`, `scrutate map`, the MCP tool `repo_map`). The edges come from the index's names
without resolving a call site (`Names::file_edges`): a call goes to the other files of its
language family that define the name, an import to the file its module maps onto. Each file
lists its most called definitions.

**Call path**: the shortest chain of calls from one definition to another (`Names::path`,
`scrutate path`, the MCP tool `call_path`), found breadth first over the calls `callees` resolves,
each step a call site and the definition it reaches.

**Walker**: the part of the resolver every language shares (`resolve/mod.rs`): scopes from the
definitions of the index, the lookup memo, the depth guards and the order of the steps that
decide what a name refers to.

**Rule set**: what one language decides differently when a name is resolved, one file each under
`resolve/` behind the `Rules` trait: what binds a name, which blocks a local stays in, how a
receiver and a base class list are read, how a module maps to files. The walker asks the rule set
of a file's language at each step where languages differ.

**Impact**: what a change touches (`impact.rs`, `scrutate impact`): the definitions a git diff
changes, their callers through `Names` with the distance of each, and the test files to run. The
diff is read by running `git`; the base side of a file is parsed from git's copy.

**Review**: the report of a pull request (`review.rs`, `scrutate review`): the impact between a
base and a head commit, and the **new duplicates**, the `dupes` groups of the head that no group
of the base matches member for member (by path and qualified name, or by body text). It prints as
the PR comment the GitHub Action posts and edits in place (found again by the marker
`<!-- scrutate-review -->`), as JSON, or as a GitLab Code Quality report.

**Rewrite**: a structural search that replaces what it finds (`rewrite.rs`, `scrutate tsquery
--rewrite`): a **plan** (`rewrite::Plan`) holds each file's bytes as read and as rewritten, and
either prints as a diff or writes each file whole by rename while its bytes are still those read.

**Frecency**: how often and how lately a file was read, which `scrutate files` (`finder.rs`, the
MCP `find_files`) ranks by among equal matches: one count per read through `read` or `read_code`
(or a `files` query that is exactly the path), each counting half as much after 7 days. Kept in
`$XDG_STATE_HOME/scrutate/frecency.json`, outside the repository.

**Grouped grep**: an Engine A search whose matching lines are grouped by the symbol that encloses
them (`intel::grouped_grep`). The MCP `grep_code` tool, `grep --symbols` and both kinds of
references use it.

**Front end**: one of the ways in: the CLI (`batch` among its commands), the MCP server, the
LSP server, the Python bindings and the watcher. Each parses its input into operations and
renders their answers, except a plain `scrutate grep`, which goes to Engine A with its `Query`
and `Files` directly, and `watch`, which only writes the stores.

**Operation**: one question a front end can ask of the index (`ops::Op`): ranked search, grep,
symbol, callers, callees, read, outline, dependencies, dependents, references, similar,
duplicates, structural search, impact, files, dead code, reindex. An operation owns its arguments, their defaults and
limits, and the rule for refreshing the index first (`Op::reads_index`). Its grep carries an
Engine A pattern (`instant::Pattern`: text, mode, case, whole words) and a **scope**
(`ops::Scope`: a file or directory of the repository and globs). Front ends parse into
operations (`Op::from_tool` for a tool's JSON arguments) and run them on a `Repo`; they
hold no defaults of their own.

**Answer**: the typed result of an operation (`ops::Answer`), with one text form and one JSON
form. Every front end renders the same answer; the MCP server sends the text, and with
`serve --structured` the JSON form too, as the **structured** result, when it fits the budget
(`Answer::structured`).

**Envelope**: the one JSON shape an answer leaves in (`Answer::envelope`): `schema`
(`ops::ANSWER_SCHEMA`), `result` (the answer's JSON form) and `stopped_at` when it stopped at its
limit. The CLI's `--json` of an index command, a `batch` record (with `op`) and the structured
result (with `complete`) are this envelope.

**Location**: where a symbol is, written `path:start-end` (or `path:line`). Every answer begins
each result with one, and every operation that takes a symbol accepts one in place of a name, to
mean that definition and no other of the same name (`intel::location`).

**Budget**: the size limit of an answer as text (`Answer::text_within`). A cut answer keeps whole
items and ends with how many are left and the `offset` that continues it. `grep --max-bytes`
pages its lines by the same rule (`ops::paged_counted`).

**Form**: how an answer's text is written (`ops::Form`, the MCP argument `response_format`):
**detailed**, the full text, or **concise**, every location and name without what a call can ask
for again (code, snippets, kinds, scores, notes), each directory written once. Both page the same
items (`Answer::text_as`).

**Repo**: an open repository (`scrutate::Repo`, in `stores.rs`): the searcher on its index plus
the stores kept current around it. Front ends that stay open reach one through a session.

**Session**: one repository held open for many calls (`session::Session`): the first-build wait,
the refresh interval, a cancel flag per call, a panic kept inside its call, recovery, and reads
side by side, each on a **reader** of its own (a `Repo` from `Repo::another`), with writes one at
a time. The MCP server, the language server, the Python bindings and `watch` are its adapters. A
command line call may use the session a running `serve` offers on `.scrutate/session.sock`, and
does the work itself when the session is not the same build, settings and root, or would wait.

**Stores**: the derived data under `.scrutate/` (the code index, the text index, the `tsquery`
cache, the kept answers) and the one module that keeps them current (`stores.rs`). A caller names a **change set**
(`stores::Changes`: every file, or the files a watch burst names) and what it does
(`stores::Writer`), never a store; the module holds the rule per operation for when to look and
the one recovery rule for damage.

**Model set**: the vector model, the ranker and the feature list the ranker reads
(`models::ModelSet`), each loaded on its first use. The index stores the vector model's
signature; another model makes the vectors anew from the word counts of the artifacts and keeps
the rest of the index.

**Candidate source**: one way a ranked search finds candidates (`search::Source`): exact names,
lexical (BM25) or semantic (vectors). The candidates are the union of the sources' answers, and
each candidate gets the 48 **features** by name (`search::FEATURES`). `batch --features` prints
them: these are the **training rows** the ranker is fitted on.

**Quality floor**: the lowest MRR@10 and hit@5 the ranking may reach on a frozen copy of this
repository's code before CI fails (`bench/quality.py`, `bench/quality/floor.json`).

**Ranking pins**: every candidate, feature and score of six queries on a small tree with the
bundled models, recorded bit for bit and checked by `cargo test` (`tests/ranking_pins.rs`). With
the quality floor they watch ranking; the Python reference that used to mirror the engine is gone.

**Translation**: what a pattern asks every matcher of it to find (`Pattern::translate`): the
pattern in the form its mode means, and whether case is folded. Engine A's matcher and the text
index's trigram condition are both built from it.

**Run**: one Engine A search in progress (`instant::Run`): the matcher, query, file set, counters,
stop flag and output target that every file search of it shares. The stop flag is set by the
caller's cancel flag or by the search itself (enough matches, a closed pipe).

**Thread rules**: how many threads an Engine A search runs (`instant::policy`): the default
count, the share of each search when several run at once, and the threads a cold disk earns.
They are decisions over readings of the machine (`Machine`: its cores, its performance cores, the
count of searches running, the numbers measured for that system), which the operating system
gives (`System`) or a test does. One scheduler (`instant::schedule`) asks them for every walk and
file list.

**Byte source**: what one Engine A file search reads (`source.rs`): a file on disk, standard
input, an archive member or text held in memory. It gives its size when it knows it, positioned
reads when it has them, and one sequential read; the read rules (first read, byte-order mark, NUL
byte, size thresholds, split, long line, searcher setup) are written once on top of that.

**Repository files**: which files belong to the repository for each index (`files.rs`): the code
index's list (hidden files included, cache and dependency directories skipped), Engine A's walk
(hidden files skipped, the same ignore files), the files a search of the repository covers
(`files::repository_files`: the code index's walk, for `grep_code` and references), and
**membership**, the test of one changed path that the watcher asks. A **look** (`files::look`) is
one walk of the tree and one `stat` per file that gives each of those sets its own files. Nothing
keeps a look between calls: a scan makes one, and the grep of the repository makes its own.

**Setup**: what `scrutate setup <agent>` writes (`setup.rs`): one MCP server entry named
`scrutate` in an agent's configuration, and on request the Grep hook and an instruction block,
each put in and taken out as text, so every other byte of the file stays and `--uninstall` gives
the file back as it was. Each agent has its files, its shape of entry and its scope (the user's
file or, with `--project`, the repository's).

**Grep hook**: Claude Code's `PreToolUse` hook for its Grep tool (`hook.rs`, `scrutate hook
grep`). It answers a Grep call with Engine A's matches, worded as Grep words ripgrep's, or lets
Grep run when it cannot answer exactly (it **fails open**). Its **counter** keeps, per answered
call, the bytes Grep would have returned against the bytes sent (`scrutate hook stats`).
