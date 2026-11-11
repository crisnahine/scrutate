# Changelog

All notable changes to Scrutate. The format follows Keep a Changelog; versions follow SemVer.
Compatibility policy: the CLI surface, the JSONL `schema` number, the MCP tool names and the Python
API are versioned interfaces; incompatible changes bump the major version once past 1.0 and are
listed here under "Breaking". The on-disk index is a cache with its own `schema_version`; a new
version rebuilds incompatible indexes automatically and this is never considered breaking.

## [0.1.0] - 2026-11-11

First release.

### Engine A: universal instant search
- Literal, regex (linear time, size-limited), identifier (all casings, digit/acronym aware),
  fuzzy/approximate (Myers bit-parallel, pigeonhole prefilter), multiline, inverted, whole-word,
  smart-case matching; context lines; `-c`, `-l`, `--max-count`, `--max-results`.
- Parallel ignore-aware traversal (`.gitignore`, `.ignore`, global excludes, hidden, symlinks,
  depth and size limits, type globs); per-file batched streaming; Ctrl-C and broken-pipe
  cancellation; deterministic `--sort`; JSONL output (schema 1); exit codes 0/1/2/130.
- Differentially tested against ripgrep on random corpora and patterns with zero disagreements.
- `-e PATTERN` (repeatable) for several patterns; each line of a multi-line match is printed with its
  own line number.
- Plain output is formatted by the search threads into per-file buffers (no per-line allocation or
  channel message); lines over 64 KB are written without being copied, their UTF-8 checked a
  256 KB piece at a time as they are written; UTF-8 is validated with the fast checker before the
  replacing decoder.
- Ctrl-C handling through `signal()` / `SetConsoleCtrlHandler` directly: the `ctrlc` crate linked
  Foundation and libobjc into every macOS process (about 1.5 ms of start-up).
- Gitignore lines of three common shapes (`*.log`, `**/a/b`, `/build*`) are matched without
  building a regex; a `.gitignore` built that way cost 100 to 400 µs to load, the global excludes
  file included, which was more than the whole walk of a small repository.
- A search whose roots hold directories starts the parallel walk at once and adds threads when its
  reads turn out to come from the disk.
- Plain output that follows other output within a millisecond waits in a 64 KB buffer, so the
  lines of many files leave in one write; output after a quiet spell still leaves at once. A write
  per matching file woke the reader of a pipe every time: `return` in the Linux tree (1.3 million
  lines in 88,000 files), on Linux 302 ms to 190 ms (ripgrep 319 ms), on macOS 675 ms to 650 ms.
- macOS on Apple silicon: 133 system functions that a search never calls (SQLite's file calls,
  process spawning, the unwinder) are bound on their first call instead of at launch; dyld spends
  about 3.3 µs on each imported symbol. With the grep-only argument parser (clap otherwise builds
  all 24 subcommands), start-up is 1.9 ms, against 1.5 ms for a Rust hello-world.
- macOS builds need macOS 13 or newer: the binary gets chained fixups, which the kernel applies as
  pages are first touched.
- A `.gitignore` used on many paths compiles its globs into one set after a while (the Linux
  tree's root file has over 100); files up to 1 MB are read whole, and a file's size is no longer
  asked for before reading it. In a Linux VM the Linux-tree search went from 144 ms to 120 ms
  (ripgrep: 135 ms).
- Smart case counts only letters that stand for themselves: `-S '\Sfoo'` is case-insensitive, as
  in ripgrep. `-c` with `--max-count` stops counting at the limit. `-w` with `--fuzzy` or
  `--fuzzy-regex` is an error (it was ignored).
- A file of 64 MB or more is searched on every search thread as ranges of whole lines, with the
  output in file order. Binary data, a byte-order mark, context lines, `--max-count`,
  `--max-results`, `-l` and multiline searches keep the single pass.
- A regex with no literal prefix whose matches all end in one of a few literals
  (`\w+_(suspend|resume)\(`) finds candidate lines by searching for those literals.
- Roots that hold no directory to enter are walked on one thread while three threads search what
  it finds; past 256 files the parallel walk takes over.
- A large file without line breaks to split at is read into one buffer of its exact size (the
  stream reader grew its buffer by doubling), each search thread filling its own part: 100 MB in
  one line took 12.6 ms to read on one thread and 4.0 ms on five. `^(a|aa)+$` over that line and
  two more files: 15.3 ms to 11.0 ms (ugrep 16.5 ms).
- `-P` / `--pcre2`: PCRE2 syntax (look-around, backreferences), stopped by PCRE2's match limit
  on exponential patterns, with a warning naming the file.
- `-z` / `--search-zip`: zip and tar members (`archive.zip{member}`) and compressed files.
- `--fuzzy` patterns up to 4096 characters (64-bit blocks beyond 128); an edit is one character
  (`naïve` is one edit from `naive`), and `-i` and `-S` fold case as for a regex. `grep --index
  --fuzzy` agrees with the walk. `--fuzzy-regex[=N]`: a regex within N edits (bit-parallel
  Glushkov automaton, up to 64 character positions); an edit is one character, classes are
  Unicode, `-i` and `-S` fold case as for a regex, and a pattern within N edits of the empty
  string is refused.
- `grep --index --update`: refresh the text index (changed files only), then search it.
- Files that could not be searched, or not to the end, are named in warnings.
- Regexes are built with `^` and `$` as line anchors in every mode, as ripgrep builds them. Before,
  a pattern using either was matched one line at a time: `^(a|aa)+$` over ten million one-byte
  lines took 198 ms, now 8 ms.
- A regex whose every match starts a line and begins with one of a few literals looks only at the
  line starts that begin with them, each decided by a search anchored there: `^(a|aa)+$` over a
  100 MB line, ten million short lines and 2 MB of near misses, 52 ms to 18 ms.
- On macOS a search of many files runs on as many threads as there are performance cores: more
  threads opening and reading files at once wait on each other in the kernel (the Linux tree on an
  M3 Pro: 977 ms on 11 threads, about 845 ms on 5). One large file split into ranges still uses
  every core, and when the files of a tree take as long to read as a disk read (the page cache
  does not hold them) the walk adds a thread for every 16 slow reads, up to twice the cores, at
  most 24: the Linux tree with a cold cache, 2.9 s on 5 threads, 1.4 s this way, ripgrep 1.8 s.
- On Linux a tree read from the disk gets up to 64 threads, whatever the cores: the Linux tree
  with a cold cache in a 5-CPU VM, 1.22 s on 5 threads, 0.37 s this way (git grep 1.23 s, ugrep
  0.64 s). A small tree stops before many threads start (231 files cold: 5.8 ms, ripgrep 7.5 ms).
- Paths print as given, as in grep and ripgrep: `./a.txt` for `.`, `dir/a.txt` for `dir/`, an
  absolute path for an absolute root. When the only path given is one file, lines and counts
  leave its name out, as in ripgrep; `-H` (`--with-filename`) and `-I` (`--no-filename`) choose,
  the later of the two wins, and `-l` and `--json` always carry the path.
- On Unix a backslash in a file name stays in the printed path; only Windows turns `\` into `/`,
  and prints a verbatim drive path (`\\?\C:\x`) as `C:/x`, a form that can be passed back.
- On Linux, files on `/proc`, `/sys` and the other kernel pseudo filesystems are read to the end
  (the first read of `/proc/kallsyms` returns about 4 KB of 7.3 MB, and its size reads as 0).
- A `-g` glob is taken whole, commas included (`-g '*.{txt,log}'`); `SCRUTATE_GREP_GLOBS` and
  the configuration file's `globs` split at commas outside braces. A `-g` glob with a slash
  applies under every searched path.
- With `-A`, `-B` or `-C`, a line `--` separates groups that are not adjacent and different
  files, as in ripgrep (not in `--json`).
- Of `-i` and `-S` the later one wins; `SCRUTATE_GREP_SMART_CASE` applies only when neither is
  given. Smart case folds only a pattern that has a literal and no uppercase letter.
- `-c` with `-U` counts a match that spans lines as ripgrep counts it. Context lines of CRLF
  files drop the `\r`.
- An argument that is not UTF-8 is an error, and `--max-filesize` refuses a size past 64 bits.
- `ignore` 0.4.34 is vendored (`vendor/ignore`) with two changes: the global git excludes path is
  read from `~/.gitconfig` without compiling a regex (about 0.4 ms of every search in a git
  repository), and an idle walker thread waits 10 µs at first instead of 1 ms before it looks for
  work again (a 598-file package: 10.7 ms to 8.1 ms).
- `--max-results N` stops at exactly N matching lines, also when several threads find matches at
  once and with `-c` and `-l`; `-m N` is a per-file limit of the search, with trailing context.
- A search path that does not exist is a warning and exit code 2; the other paths are searched.
- With no path, a pipe or a file on standard input is searched (`-` names it; label `<stdin>`).
- A file named on the command line that holds a NUL byte is searched as ripgrep does: counts and
  lists include it, and line output prints `binary file matches (found "\0" byte around offset
  N)` (a `binary` record with `--json`). A file the walk finds with a NUL byte is left out of `-c`
  and `-l`, so `-v -l`, `-v` and `-v -c` agree.
- `-c` and `-l` give one record per path given, as ripgrep does, also for a file two paths reach.
- `--sort` with `--max-results N` prints the first N matches in path order, the same on every run.
- `--sort` prints in the order of `rg --sort path`: the paths as given, and under each its files
  by their paths compared part by part (`sub/deep/a.txt` before `sub/deep-x/b.txt`, `foo/mod.rs`
  before `foo.rs`); it ordered all results by the whole path.
- `-n` is accepted and `-N` drops line numbers. `-j`, `SCRUTATE_THREADS` and the configuration
  file's `threads` stop at 256, and a failed thread start says to lower `-j`.
- `.rgignore` files are read by `grep`, the code index, `watch` and `tsquery`, as ripgrep reads them.
- `--type-list` and `--version` need no pattern. A bad `.scrutate.toml` no longer stops `--help`,
  `--version`, `config` and `completions`.
- On Linux a `proc` or `sys` directory on a pseudo filesystem below any search root is read to
  the end.
- On Windows a file that falls back to the stream reader (a NUL byte, a byte order mark, a read
  error) is searched from its first line; it started where the range search had stopped.

### Engine B: code intelligence
- Incremental index (SQLite + memory-mapped segments), content hashing, atomic updates,
  compaction, GC, corruption detection and self-healing rebuild, schema/model versioning,
  multi-process safety, `check` command.
- 60 tree-sitter grammars in the default build, each verified by a snapshot sample; 13 more with
  `--features extra-langs` (12 verified); keyword heuristics for grammar-less languages, windows for
  everything else; imports and call edges.
- Text index for repeated grep-style queries (`scrutate index --text`, `grep --index`): trigram
  postings over exactly the files Engine A's walk visits, trigram conditions derived from the
  pattern's syntax tree, candidates searched by Engine A's own matcher, so the output equals the
  walk's. Incremental (segments merge during updates) and safe for concurrent readers.
- UTF-16 files with a byte-order mark are transcoded and indexed (previously skipped as binary).
- Text index format 2: each posting carries next-byte and position masks, so a literal's trigrams
  must line up in a file for it to be a candidate (the Linux tree: `kernel_thread` 1,781 candidate
  files to 164; concurrent query throughput went from 4% behind Zoekt to 5% ahead). The index is
  409 MB instead of 148 MB; format 1 indexes are rebuilt by the next update.
- Chunk line numbers follow grep's line counting (`\n` only); Python-style splitting had shifted
  them after form feeds (found by fuzzing). Index schema 12.
- The 12 optional grammars (`--features extra-langs`) are verified by samples; writing them fixed
  the D, LLVM IR, AutoHotkey, VHDL and OCaml rules.
- Ranked search: field-weighted BM25 + exact lookup + semantic vectors (IVF) + LightGBM
  LambdaRank ranker; bit-identical with the Python reference implementation.
- `symbol`, `callers`, `callees`, `refs`, `outline`, `deps`, `rdeps`, `similar`, `dupes`
  (exact + MinHash/LSH near-duplicates), `tsquery` (tree-sitter structural search),
  cross-repository `search --also`.
- `refs` resolves each occurrence to the definition it refers to (scopes, imports, qualified
  names) for Python, JavaScript/TypeScript, Rust, Go and Java; `--def` keeps one definition and
  `--textual` gives every whole-word occurrence as before.
- Java in `refs`, `callers`, `callees` and the language server's references: packages from each
  file's `package` declaration in any source root, single, on-demand and static imports, locals,
  fields, inherited members, nested and anonymous classes, records, default methods, calls typed
  by declared types and return types, and method references. The number of arguments picks an
  overload; a call it does not tie to one is a caller of every overload.
- `callers` of an override or an implementation lists a call through its base class, interface
  or trait as undecided, at any depth, and a variable declared as the base type and built by a
  constructor in the same function is typed by the constructor (`Base b = new Big()`), so its
  call is a decided caller of the override. TypeScript resolves inherited members through
  `extends`. A `super` call is a decided call of the nearest base method that defines it
  (Python by its method resolution order, also for `super(C, self)`), never of the override it
  sits in. A bare Python import finds the module beside the importing file when no module from
  the root has the name and the file's directory is not a package (`from helpers import x` in
  `tests/`; `import logging` in a package with its own `logging.py` stays outside).
- A JavaScript or TypeScript import of a workspace package by its `package.json` name resolves
  to the package's file: `exports` (conditions `types`, `import`, `default`, `require`, `node`,
  then the others by name), `module`, `main` or an `index` file, a build output the repository
  does not hold standing for its source under `src/`, a subpath through `exports` or a path in
  the package. A declaration file (`.d.ts`) beside its implementation no longer takes the
  import: `./m` with `m.js` and `m.d.ts` is `m.js`.
- Imports are recorded as their statements write them: a JavaScript or TypeScript exported
  declaration is no import (only `export ... from`), a `require` or `import()` call counts with
  a literal argument, and PHP double quotes and grouped `use`, R, Julia lists and `include`,
  Lua long brackets, Objective-C `@import` and QML imports are recorded; Kotlin and C# record
  the name, not the alias, Ruby only calls without a receiver, and a Java star import is one
  `a.b.*` record that `deps` resolves to the files of the package.
- In a language the resolver does not read, a call without a receiver is a call of the
  definition in its own file: `callees` lists that one alone (it listed the first three
  namesakes of the index), and `callers` of a namesake in another file leaves the call out.
  Other undecided callees are listed nearest first. A Nix function and a macro of the
  repository are listed as callees (they were named after `external:`), and a `.m` file of
  MATLAB is no longer in the C family.
- A search that meets a damaged segment array (a posting row, a pointer or a cluster table that
  points outside the segment) rebuilds the index and answers; it ended with a panic message.
- `refs` finds a name in its composed and decomposed Unicode spelling.
- `tsquery` keeps each file's captures in `.scrutate/tsquery/` by content hash: a repeated query
  re-parses only changed files (the Linux tree: about 25 s, then 1.6 s), output identical to
  `--no-cache`; it also accepts a single file as its path.
- Semantic vectors stored as int8 with a scale per vector (schema 13): the index is about a fifth
  smaller, ranking quality within 0.001 MRR, parity with the Python reference exact.
- `scrutate lsp`: a language server (definitions, resolved references, workspace symbols,
  outlines) for editors; the VS Code extension starts it, JetBrains uses it through LSP4IJ.
- `scrutate watch` and `serve --watch` keep the indexes fresh as files change.
- A long-running index (`watch`, `serve`, `lsp`) reparses an edited file against its previous
  syntax tree (up to 2 MB of source kept) for the 21 grammars without an external scanner: a
  66 KB Go file, 5.5 ms to 2.0 ms per edit.
- Ranker v8, fitted on Python and on 16 repositories in Rust, Go, Java, JavaScript and TypeScript:
  better than v7 on held-out code in 56 of 58 language, variant and query-type cells, never worse
  beyond noise (docs/QUALITY.md).
- Ranked search finds words of every script: scripts written without spaces (Han, kana, Thai,
  Lao, Tibetan, Myanmar, Khmer, the Tai scripts, Balinese and Javanese) become overlapping
  character pairs, other scripts one lowercased token per word; an all-ASCII text gives the tokens it gave
  before. Names compare in NFC (new dependency `unicode-normalization`), and a definition name
  over 200 bytes is a symbol when it has no whitespace. Traditional and simplified Han find each
  other: ranked search folds Han to simplified forms in index and query (a Unihan 18.0.0 table,
  Unicode License v3, listed in the notices and the SBOM); names and stored text are not folded.
- Constants are symbols of kind `constant` (Rust `const` and `static`, Go package `const` and
  `var`, JavaScript and TypeScript top-level `const`, TypeScript enum members and `static
  readonly` fields, JavaScript `static` fields in capitals, Java `static final`, Python
  module-level and class-level capitals and tuple or list targets, C, C++, Objective-C and
  GLSL `#define` values, enumerators and file or namespace `const` and `constexpr` with a value,
  C++ `static const` and `static constexpr` members of a class):
  `symbol`, `outline`, `read_code` and `refs` find them, ranked search never returns them. Every
  name of a Go or Java declaration with several (`var X, Y int`) is a constant.
- A method is named by its owner's bare type (`Stack.push` for `impl<T> Stack<T>`, `Box.put`
  for `Box<T>::put`); a dotted Lua or Elixir name is split off its owner (`M.helper` is
  `helper`, `MyApp.Accounts.User` is `User`); `Account:deposit` in Lua is `Account.deposit`.
  A JavaScript or TypeScript object literal's methods, and its properties whose value is a
  function, are methods named under what holds the literal (`api.get`, `store.sub.m`,
  `tool.build` for `module.exports.tool`), also through `as`, `satisfies` and parentheses; they
  keep their callers and callees.
- New definitions: Rust `mod` and `type` items, `macro_rules!` and trait methods without a body;
  Go methods qualified by their receiver type; C++ methods declared in a class or struct body
  (constructors, destructors, member templates, conversion operators) and out-of-line methods
  split at `::`; C and C++ function-like macros; JavaScript class field functions and
  `module.exports.X`; TypeScript abstract classes and methods, `declare function` and
  namespaces; Java records; Swift protocol requirements; Kotlin type aliases; Elixir guards;
  Objective-C methods and properties declared in an `@interface` or `@protocol`; a VHDL
  subprogram with a body; a Jsonnet function; a function pointer typedef by its own name
  (`cb` in `typedef int (*cb)(int)`); a C
  or C++ function declared without a body, also inside an include guard (`symbol` lists the
  definition before the prototype). A `.h` file written in C++ or Objective-C is parsed with that
  grammar, where it lost its classes and methods as C.
- Calls inside the arguments of a Rust macro (`println!("{}", f(x))`) and turbofish calls
  (`f::<T>(x)`) are call edges.
- Calls in Dart, Groovy, Elixir, LLVM IR, TLA+, Verilog (task calls and module instances too),
  VHDL, Jsonnet, Make (`$(call g,...)`) and assembly (`call`, `bl`, `jal`; a label holds its
  instructions up to the next label or section) are recorded. A method call records
  the method in Lua (`obj:meth()`), Objective-C (`[obj meth]`), R (`obj$meth()`), PowerShell and
  PHP (`?->`); `new Point()` records `Point`; a Lisp name is recorded whole (`list->vec`,
  `set-car!`) and a JavaScript private method with its `#`. A Scheme `(define (f x) ...)` head, a
  Lisp parameter list and Elixir's `def`, `use` and `alias` are not calls. Dart and Pascal
  functions hold their bodies.
- Chunk text and body are cut at 32 KiB, and a chunk ends on the last line that holds its text.
  A file of many sibling definitions or of deep nesting indexes in linear time (20,000 one-line
  JavaScript functions: 58 s to 0.8 s). A segment of 20,000 rows or more is not written again
  when a few of its rows die: compaction judged it by its live rows, so every save near that
  size rewrote it.
- Inside a git repository the ignore files decide for `build`, `dist`, `target` and `venv`;
  outside one they are skipped by name. `\,` keeps a comma in `SCRUTATE_INDEX_EXCLUDE_DIRS`.
- The heuristic tier skips leading modifiers and reads OCaml `let f x =` as a function; Kconfig
  and `.txt` files get no keyword symbols.
- The model signature is a hash of the model file, so a retrained model of the same shape
  rebuilds the vectors.
- A read never waits for another process's writer: it answers from the last committed index and
  says so once on stderr. A read during another process's first build exits 2, and a `serve`
  call waits up to 20 seconds for that build, as for its own.
- Rebuilt as damage: a chunk text that does not inflate, a segment list that is not JSON, a
  segment without vectors, an `index.db` deleted under an open index or that is a directory, a
  `seg` that is a file, an emptied `chunks` table. A writer stopped before its commit no longer
  blocks later writers. A read-only index directory, or a read-only repository with a complete
  index, is answered as it is.
- On Unix a change of status time alone (a `chmod`) is seen: it costs a read and a hash. A file
  that cannot be read loses its chunks.
- A `.scrutate` that is a symbolic link, or that holds one, is refused, and a `--root` that is
  not an existing directory is an error; nothing is written or deleted outside the repository.
- `refs`, `callers` and `callees` infer a receiver's type from declared return types, fields,
  module-level and static variables, constructors, Rust `?` and holders such as `Box`, and
  TypeScript `await`, and a call on the result of a call from the type its callee declares it
  returns; a type from outside the repository makes the call external, unless a trait or
  interface of the repository declares that method. `this` in a JavaScript or TypeScript object
  literal method is the literal. In Rust macro arguments a name after `.` is a field or a method,
  `name:` in braces a field name, and a closure parameter or `let` a local. `x.m()` is
  never a call of a free function in Rust, JavaScript, TypeScript, Go and Java, a definition's
  own name is not a call of it, and a JavaScript `var` belongs to its function.
- A call matches a definition only in its own language family and by the name as written
  (`Ok(1)` is not a call of `ok`); a call the resolver ties to an imported module, a local or a
  field is not a call of a namesake in the repository.
- `symbol`, `callers`, `callees`, `similar`, `refs` and `read_code` look a name up as typed first
  (`list->vec`, `Foo::Bar`, `#main`), then read `Type::method`, `Type#method` and `p->method` as
  `Type.method`, then compare owners without generic arguments and path (`Stack<T>.push`,
  `crate::config::Config::new`), and last, for a path, a top-level definition of its last part;
  they find one method among many namesakes; `operator->` and `operator*` are found by those
  names. A leading dot is dropped (`callers .helper`). The name spelled as typed comes before its
  namesakes in another case, then non-test files, types before functions and methods, and a
  definition with a body before one without. The language server's definition request takes the whole name under the cursor
  (`get-user`, `Foo::Bar`).
- A location (`path:start-end`) names exactly one definition in `symbol`, `callers` and
  `callees`. `callers` lists the callers the resolver ties to the definition first, then the
  undecided ones that name the owner type or share its package, and asks the resolver about at
  most 400 call sites (`callers Builder.new` on 300 crates: 3.2 s to 0.4 to 0.6 s).
- `callers`, `callees`, `symbol`, `dupes`, `tsquery`, `read_code symbol`, `duplicates` and
  `structural_search` end with a line when they stopped at their limit, naming the argument that
  raises it.
- `tsquery` reads a path that does not exist from the current directory under `--root`, and
  names a path that exists in neither place in a warning, exit code 2. It prints paths as
  `grep PATH...` prints them (`src/a.py` for the argument `src`), in plain output and `--json`,
  so a printed path can be passed to the other commands. `dupes` lists groups of equal size in
  one fixed order, so a limit keeps the same groups on every run.
- Imports: Go binds a directory through the module path in `go.mod`; a Python module drops its
  first segment only when that is the checkout's own name; an `#include "util.h"` resolves
  beside the including file; `rdeps` matches by base name only an import that resolves to no
  file. `tsconfig.json`, `jsconfig.json`, `package.json`, `Cargo.toml` and `go.mod` are read
  only when they are regular files inside the repository of at most 1 MB.
- A lambda, nested function, block or comprehension hides a name only inside itself. A name bound
  by a pattern, a `catch` or a Go statement initializer hides a function only in its own branch
  or statement, and a closure sees the locals around it.
  `tsquery` decodes files that are not UTF-8 and skips files over 1 MB, and refuses an empty
  query, a query without a capture and an unknown predicate.
- `dupes`: `exact` means the same body text once whitespace is squeezed; functions with the same
  words that differ in a literal or an operator are one `near` group with similarity 1.00;
  `--threshold 1` prints the exact groups only.
- `grep --symbols` with a path argument finds the file by its path under the root.
- `deps` and `rdeps` resolve imports as `refs` does (Rust `crate::`, `self::`, `super::` and `mod`,
  Go through `go.mod`, Python `from . import`, TypeScript `.js` names and `.d.ts` files), and
  `rdeps` says how many unresolved imports may name the file, counting only those that could:
  a relative import with the file's path, or a bare one with a part spelled as its module name. A Java
  import resolves through the package declarations, in any source root: a class import to the
  class's file, a package import to the package's files, a static import to its class's file.
- An index built with older parsing rules reads every file again once (`content_rev` in `meta`).
- An update can be cancelled and rolls back; an update that panics rolls back at once, and the
  next update clears a transaction left open.
- The index cuts each chunk's text from the source one chunk at a time: a one-line file with
  8,000 nested functions now peaks at 542 MB to index (462 MB with `SCRUTATE_SEMANTIC=none`),
  where it took about 1.1 GB (0.75 GB).
- `similar` on a constant says constants are not compared, and `similar -k 0` returns nothing
  instead of panicking.
- A backslash in a Unix file name stays in `tsquery` output, `read_code` path matching and the
  language server, and `tsquery` prints control characters in a capture escaped. A rebuild that
  cannot reopen the index returns its error, and the next call opens it again.
- Names with control characters print as escapes and a backslash doubled in the index commands;
  what they print is read back as an argument.
- The Python reference indexes the engine's file set (excluded extensions,
  `SCRUTATE_INDEX_EXCLUDE_DIRS`, ripgrep without its configuration), cuts the same chunks, keeps
  the lower index at the 200-candidate cut and refuses `read()` paths outside the repository;
  the parity check fails on any difference.

### Product
- Unified `find`, MCP server (14 tools), configuration files with environment-variable precedence,
  shell completions, progress reporting; errors as JSON records with `--json` or
  `SCRUTATE_DIAGNOSTICS=json`.
- On/off environment variables are off for `0`, `false`, `no`, `off` or empty and on for any other
  value; `NO_COLOR` turns colour off for any non-empty value.
- One definition of every index operation for the CLI, the MCP server, `batch` and the Python
  bindings (`ops.rs`): the same defaults, limits, text and JSON everywhere. `batch` runs any
  operation by name.
- For AI agents. Every MCP tool but `reindex` is marked read-only and local; the server returns
  instructions; `grep_code` is an Engine A search with matches grouped by enclosing symbol;
  `references` resolves each use to its definition; an answer is at most 24,000 bytes by default
  (`max_bytes`), cut at whole items, with the `offset` that continues it; the CLI has the same as
  `--max-bytes` (`SCRUTATE_MAX_BYTES`) and `--offset`, also for `search` and `tsquery`; a first
  index build blocks a tool call for at most 20 seconds. `serve --max-bytes` sets the server's
  default.
- A capped `grep_code` or `references` answer holds the first matches in path order, so a second
  page neither repeats nor skips one.
- `read_code` by path reads only as far as the lines it shows, keeps at most 1 MB of line text,
  refuses a file with a NUL byte in its first 8 KB, and says `more lines follow` when a large
  file was not read to its end.
- Commands run in a subdirectory use the index and the `.scrutate.toml` above it and read path
  arguments from the current directory. `search --also` prints another repository's results by
  the path given, searches a root given twice once, and adds `root` to its JSON. A path or
  location such a result prints is answered from that repository's index. The search for an
  index above stops at the top of a repository, so a nested clone, submodule or worktree does
  not use the outer index. A first index is built at the top of the repository, whichever
  subdirectory the first command runs in, and the top's index wins over one left below it. The globs of a `.scrutate.toml` match each path from the file's
  directory, whatever path is searched and wherever the command runs; a path outside that
  directory and a path named on the command line are not filtered by them.
- Editors opened on a folder below the repository top use the index at the top: the language
  server applies the same rule to the editor's first workspace folder (it built a second,
  partial index in that folder), and the VS Code and Neovim plugins ask `scrutate config --json`
  for the root, so the paths they open exist. `config --json` prints the root an index command
  run there uses and the configuration file, and builds nothing.
- In the home directory or at a filesystem root that has no index, a command that would build
  a first index stops with exit code 2 and names `--root` and `scrutate index`; `serve` does
  not start there, and the language server builds no index when the editor names no folder.
- `languages` lists the extensions a language shares with another and takes by the file's text
  (`.m` for matlab and objc, `.pp` for puppet and pascal, `.h` for cpp and objc); the JSON form
  has them in `extensions_by_content`.
- `symbol`, `callers` and `callees` take `--limit`. `check --json` counts the indexed files
  without `--files`. `check` counts the constants, which have no segment rows (`constants` in
  its JSON). `batch` reports a line that is not JSON by its number and runs the rest.
- `reindex` takes `force`, which reads every file again. `grep_code` searches the files the
  index holds: hidden ones such as `.github/`, not `.git/` or `node_modules/`.
- JSON-RPC answers as the specification gives them: batches answered as one array, -32700 for a
  line that is not JSON or not UTF-8, -32600 for a message that is not an object or has no `method`, -32601 for an
  unknown method, -32602 for an unknown tool; notifications are never answered.
- MCP revision 2026-07-28 beside the handshake revisions: `server/discover`, a protocol version in
  each request's `_meta`, `resultType` on results, `ttlMs` and `cacheScope` on lists. With
  `serve --structured` every tool has an `outputSchema` and every result `structuredContent`
  (the `--json` form, or `null` and the text page when the answer is larger than its budget). `notifications/cancelled` stops a call that is waiting,
  stops and rolls back an index update it runs, and drops the answer of one that is computing; a call with a `progressToken` gets progress
  reports while the first index is built, and then its answer.
- `read_code` reads a whole definition with `symbol` (a name, `Class.method`, or a location). The
  location at the start of every result (`path:start-end`) is accepted wherever a tool takes a
  symbol, so one of several same-named definitions can be followed up; JSON hits carry it as `id`.
- The index recovers from a corrupt database in every front end that stays open (the MCP server,
  the LSP server, the Python bindings, `watch`), not only in the CLI.
- `refs` resolves a method call on a local variable or a parameter through the type its
  declaration states (`x: T`, `x = T()`, `new T()`, `T::new()`, `T {}`, `&T{}`), and finds a Go
  method by its receiver type.
- TypeScript and JavaScript imports through `paths` or `baseUrl` of a `tsconfig.json` or
  `jsconfig.json` resolve to their files: `refs` follows them, and `deps` and `rdeps` list them.
- Smart case sees a letter written by its code point (`\x41`). The watcher's test of one changed
  path decides in the walk's order (a `.ignore` at any level before every `.gitignore`).
- `watch` and `serve --watch` no longer stop on Linux and Windows when `.git/info/exclude` has
  patterns (the matcher was rooted at `.git/info` and panicked on the first changed path).
- `-U` with `--fuzzy` or `--fuzzy-regex` is an error (it found nothing across lines).
- An MCP answer checks the files it cites, and the files its arguments name, with one `stat`
  each; a changed or deleted file, or a new file an argument names, is indexed again alone and
  the answer computed again, once. A scan that could not run does not start the interval.
  Without `--refresh-interval` the server scans at most a second apart, sooner when a scan is
  cheap; an explicit `--refresh-interval` is a floor. `serve` refuses `--offset`.
- At the end of its input the server answers the calls in flight and lets the first index build
  finish before it exits. An `initialize` with a missing or unknown `protocolVersion` is
  answered with 2025-11-25; a request without `method` gets -32600 and a client's response is
  ignored.
- `read_code` honours an explicit `end`, closes with the lines shown, the file's length and the
  `start` for the rest, and answers an empty file and a `start` past the end in words; an `end`
  before `start` and a path that is not a regular file are errors.
- A tool argument of the wrong type, or a negative number, is an error that names the argument;
  an error quotes at most 200 characters of it. Every path argument may be written `./x` or
  `pkg/../x`. `references` says when `max_results` cut it and answers `(no results)`; an empty
  name is an error. `search_code` takes at most 10 `alt_queries`.
- `watch` and `serve --watch` set the watch before the first update, so changes made during it
  are seen. On Linux each directory the indexes can hold gets one inotify watch (2 instead of
  7,012 on a tree with an ignored `node_modules` and `target`); failed watches are named, and
  the watch stops with an error when its root is removed. On Linux and Windows a burst of file
  events updates those files alone, in both indexes, with no walk of the tree; a burst with a
  new, removed or moved directory, a changed ignore file or `.scrutate.toml`, or lost events
  walks the tree, sets the watches again and reads the ignore rules again. `.git/info/exclude`
  is watched, the main repository's too from a linked worktree. The walks of the indexes decide
  what a burst holds, so an ignore file above the repository top and a nested repository's own
  rules apply to a saved file as they do to a scan. Watches are set root first, then shallow
  directories by path; when some directory gets no watch stderr says how many, and a burst
  walks the tree while one is unwatched or once the last walk is 30 s old. A change to a rule
  file outside the tree walks it too: an ignore file above the root, the exclude file of a
  nested repository, git's global excludes file and the configuration that names it.
- `find` takes a question with a space and a trailing `?` as words, and goes to ranked search
  when a regex-looking query finds nothing and an index exists.
- Text files check out with LF line ends on every platform (`.gitattributes`), so a Windows
  build compiles the same data into the binary.
- Licensed under Apache-2.0. Toolchain pinned to Rust 1.99.1 (`rust-version` 1.91).
- Python bindings (PyO3 0.30; abi3, abi3t and cp314t wheels; GIL released, in `stats()` and
  `read()` too; streaming iterator; free-threading declaration after review). Three wheels per
  platform: `cp39-abi3` for standard CPython up to 3.14, `cp314t`, and `cp315-abi3.abi3t` for
  3.15 and newer, standard and free-threaded; the wheel's third-party notices are in
  `dist-info/licenses`. The bindings' `callers`, `callees` and textual references match names
  as the tools do. Ctrl-C stops `grep` and `Repo.index()` within about 100 ms (an interrupted
  update rolls back); `Repo.read()` raises `FileNotFoundError`, `ValueError` or `OSError`;
  `grep` raises its argument errors at call time and takes `sort=True`; a panic no longer leaves
  the `Repo` unusable. `Repo.symbol`, `Repo.callers`, `Repo.duplicates` and the hits of
  `Repo.callees` are a `scrutate.Results` list whose `stopped_at` is the limit when more
  results were left.
- Integrations: one MCP configuration per client (VS Code `servers`, Codex TOML, Zed
  `context_servers`); the GitLab and Bitbucket templates pass on a repository with no
  duplicates and unpack the tool outside the checkout; the pre-commit hook is valid YAML, fails
  when the tool fails, and works through `repo:`; the GitHub Action runs on Windows runners;
  Neovim ships `lsp/scrutate.lua`, so `vim.lsp.enable('scrutate')` is the whole setup, and the
  plugin needs Neovim 0.10 or newer, and its commands work on first use and after an edit; VS Code,
  Neovim and JetBrains put `--` before a query, and VS Code stops without an open folder.
- `release.yml` refuses a tag whose version differs from the packages' (`tools/check_version.py`)
  and runs all of `ci.yml` on the tagged commit before anything is published. Every workflow
  action is pinned to a commit.
- The GitHub Action puts scrutate's exit code in its `exit-code` output and fails its step only on
  an error (2), so a job can gate on results.
- Benchmark suite (`bench/leaderboard.py`, `bench/indexed.py`): grep, ripgrep, ack, ag, git grep,
  ugrep, ast-grep, Zoekt and livegrep on pinned corpora, every answer checked against an
  independent reference, 30+ runs per contender, bootstrap confidence intervals, hardware-ceiling
  baseline, warm and cold cache; a regression gate that times the change and its base in one
  interleaved run and takes their ratio inside each block (`SCRUTATE_BASE_BIN`,
  `leaderboard_regression.py --within`; `--redo-unstable` checks both binaries); CI/release
  workflows; SBOM; IDE and Git-platform integrations.
- Random checks in CI: `bench/fuzz_grep.py` (against ripgrep) and `bench/fuzz_incremental.py`
  (an updated index against a fresh one), 20 seconds each; a check that compared nothing fails.
- Packaging: Homebrew formula, Scoop and winget manifests (schema 1.12.0), `.deb` and `.rpm` packages (built and
  install-tested in clean containers); release archives, wheels and the `.deb` and `.rpm` carry
  `THIRD_PARTY_NOTICES.txt`; `docs/dashboard.html` charts the benchmark runs.
- Fuzz targets (`fuzz/`): the `.npy` reader (three defects fixed: a panic on truncated string
  arrays, a wrapping element count in release builds, a huge allocation for zero-width strings),
  the text index's soundness, the edit-distance matcher and every parser tier.
