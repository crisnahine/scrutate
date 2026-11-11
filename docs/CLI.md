# Command-line reference

`scrutate --help` and `scrutate <command> --help` are authoritative; this page explains the model.

## Conventions

- **Exit codes**: `0` results found, `1` no results, `2` error (bad arguments, unreadable index,
  invalid pattern, a `--root` that is not an existing directory, a search path that does not
  exist...), `130` interrupted (Ctrl-C, or stopped early for a reason other than
  `--max-results`). `grep --type-list` and `grep --version` need no pattern.
- **Diagnostics**: errors are one line on stderr (`scrutate: error: ...`). With `--json`, or with
  `SCRUTATE_DIAGNOSTICS=json` for any command, an error is one JSON record instead:
  `{"type": "error", "schema": 1, "message": "..."}` (`"internal_error"` for a bug).
- **Streams**: results on stdout, diagnostics and progress on stderr. Output is plain `path:line:text`
  when stdout is not a terminal; colour only on a terminal (`--no-color`, or `NO_COLOR` set to
  any non-empty value, disables it).
- **Determinism**: `--sort` orders Engine A output as `rg --sort path` does: the paths in
  command-line order, and under each one its files by their paths compared part by part, then by
  line. So `sub/deep/a.txt` comes before `sub/deep-x/b.txt`, and `foo/mod.rs` before `foo.rs`; a
  file two paths reach comes once under each. Default output is in completion order for speed;
  `-c`, `-l` and `--symbols` are always sorted (without `--sort`, `-c` and `-l` compare whole
  paths as strings, all paths together). Engine B output is fully deterministic (ties broken by
  id).
- **Machine-readable output**: `--json` on every command. Engine A emits JSON Lines with
  `"schema": 1` and `"type"` ∈ {`match`, `context`, `count`, `file`, `summary`, `symbol_group`};
  Engine B commands emit one JSON document. The schema number changes only on incompatible changes.
- **Paths**: `scrutate grep` prints a path as the walk reached it from the path given, as grep and
  ripgrep do: `dir/a.txt` for `dir/`, `./dir/a.txt` for `.`, an absolute path for an absolute one,
  and relative to `.` when no path is given (text, `--json`, `-c` and `-l` alike; with `--symbols`
  a path is the file's path under `--root`, as the index names it). As in ripgrep, each line
  and count carries its file's path unless the only path given is one file; `-H`
  (`--with-filename`) and `-I` (`--no-filename`) choose, and of the two the later wins. `-l` and
  `--json` always carry the path. `find`, the index commands, the MCP tools and the LSP server
  print paths relative to the repository root. On Windows `\` is printed as `/`, and a verbatim
  drive path (`\\?\C:\x`) as `C:/x`; on Unix a backslash is part of a file name and stays. The index commands and the MCP tools show a control
  character in a name as an escape (`\n`, `\x1b`) and a backslash doubled (`\\`); a path or a
  location written that way is read back by every command that takes one.
- **Configuration**: the nearest `.scrutate.toml` from the current directory up to the top of
  the repository (the directory that holds `.git` or `.scrutate`), else
  `$XDG_CONFIG_HOME/scrutate/config.toml`; `$SCRUTATE_CONFIG` names a file, and with `--root` the
  file in that directory is used. The globs of a `.scrutate.toml` match each path from the file's
  directory, whatever path is searched and wherever the command runs. A path outside that
  directory and a path named on the command line are not filtered by them, and `-g` or
  `SCRUTATE_GREP_GLOBS` replaces them. `scrutate config` names the file and shows what is in
  effect, with the directory the globs match from. A file with an unknown field stops every command that reads it (exit 2, naming the
  file and the field); `--help`, `--version`, `config` and `completions` still run. Precedence: command line >
  environment variable > configuration file > built-in default. Every configurable option names its
  variable in `--help` (e.g. `--hidden` ↔ `SCRUTATE_GREP_HIDDEN`). An on/off variable is off when
  it is `0`, `false`, `no`, `off` or empty, and on for any other value (`1`, `true`, `yes`, `on`).
  `--root` (`SCRUTATE_ROOT`) must be an existing directory; otherwise the command exits 2 and
  creates nothing.
- **Running in a subdirectory**: without `--root` (or `SCRUTATE_ROOT`), a command that uses the
  index runs on the top of the repository the current directory is in (the first directory
  upward that holds `.git`, a file or a directory), and says so on stderr
  (`[scrutate] using the index in <path>`). A first index is built there, whichever
  subdirectory the first command runs in, and the top's index wins over one an earlier run left
  in a subdirectory; with no index at the top, an index in a directory between is used. A
  clone, a submodule or a git worktree inside an indexed checkout is its own repository, with
  its own index. Outside a repository the nearest directory above that holds `.scrutate` is
  used, else the current directory. A home directory kept in git is not taken for a top. In
  the home directory or at a filesystem root with no index there, a command that would build a
  first index stops with exit code 2 and says to pass `--root` or run `scrutate index`;
  `scrutate index`, `--root` and `SCRUTATE_ROOT` build there as asked, and an index already
  there is used. A path argument is read from the current
  directory when it exists there, else as given (a path from the repository root, as answers
  print it). A file or a location of another indexed repository, as `search --also` prints it
  (`../two/src/db.py:5-6`), is answered from that repository's index by `symbol`, `callers`,
  `callees`, `refs --def`, `outline`, `deps`, `rdeps` and `similar`; a file under no index is
  answered as one that `is outside the repository`.
  `grep --index` from a subdirectory answers for that subdirectory; `tsquery` walks from the
  current directory and keeps its cache in the root's index; `serve` and `watch` use the index
  above. The language server applies the same rule to the editor's first workspace folder
  (else `rootUri`), so it uses the index at the top.
- **Shell completion**: `scrutate completions bash|zsh|fish|powershell|elvish`.
- **Cancellation**: Ctrl-C stops a search between files and exits 130; a closed pipe
  (`| head`) stops it silently.

## Engine A: `grep` (alias `g`)

```
scrutate grep [OPTIONS] PATTERN [PATHS...]
```
With no path, `grep` searches standard input when it is a pipe or a file, and the current
directory otherwise; `-` names standard input among other paths. Its lines carry the label
`<stdin>` where a path would stand (`-H`, `--json`); `--index` ignores standard input.
`PATTERN` is a regex unless `-F` (literal), `--ident` (identifier in any casing) or `--fuzzy[=N]`
(within N edits, default 1, literal, at most 4096 characters; an edit is one character inserted,
deleted or substituted, a byte of invalid UTF-8 counts as one, and `-i` and `-S` fold case as
they do for a regex). The default regex engine runs in linear
time and has no look-around or backreferences; `-P` (`--pcre2`) switches to PCRE2 for those. PCRE2
backtracks, so a pattern can take exponential time; its match limit stops the search of a file
then, and `scrutate` prints a warning naming that file. With `-P`, `--index` cannot narrow the
files and searches all of them. `--fuzzy-regex[=N]` matches a regex within N edits
(default 1): an edit is one character, as with `--fuzzy`, and a byte of invalid UTF-8 is one unit
that no position matches; classes (`\w`, `\d`, `\s` too) are Unicode, and `-i` and `-S` fold case
as they do for a regex (`-i 'été'` finds `ÉTÉ`). It has no anchors or word boundaries, at most 64
character positions (a bounded repeat counts each copy), and a pattern within N edits of the
empty string is refused. `-z` also searches inside zip and tar archives (members are shown as
`archive.zip{dir/file}`) and compressed files: gzip in process; bzip2, xz, zstd, lz4, brotli and
`.Z` through the system's tools, as ripgrep does. An archive or compressed file stops at 8 GiB
expanded or a million members. Archives inside archives are not opened, and `--index` does not
cover archive members. `-e PATTERN` (repeatable) gives several
patterns; a line matching any of them matches, and the first positional argument is then a path.
Options mirror grep/ripgrep where the meaning is identical: `-i`, `-S` (smart case), `-w`, `-U`
(multiline: each line of a multi-line match is printed with its own number), `-v`, `-l`, `-c`,
`-A/-B/-C`, `-m`, `--max-results`, `-g GLOB` (`!` excludes), `-t TYPE` / `--type-list`, `--hidden`,
`--no-ignore`, `-L` (follow links), `--max-filesize`, `--max-depth`, `-n` / `-N` (line numbers
or none; the later wins, and `--json` keeps them), `-j THREADS` (at most 256, as are
`SCRUTATE_THREADS` and the configuration file's `threads`; default: the cores, at most 12; on
macOS the performance cores; more when the files come from the disk, a cold cache: up to 64 on
Linux, elsewhere twice the cores, at most 24), `--sort`,
`--json`, `--stats`, `--symbols` (group lines by the enclosing function using the index, when one
exists). On macOS, searches of one user that run at the same time split the default threads
between them and take them back as the others end: each gets the default divided by the number
running, at least 1 (py-big, four at once: 244 ms each before, 185 ms now; one alone is
unchanged). A search joins that count once it has searched 1,000 files; one given a thread count
(`-j`, `SCRUTATE_THREADS`, `threads`) keeps it and is not counted, and neither is one waiting on a
reader that does not read or one stopped with Ctrl-Z, until it goes on. A search stopped with
SIGSTOP, which no program can see coming, stays counted until it continues or ends. The count
is a System V semaphore of the user, listed by `ipcs -s` until the machine restarts; a killed
search leaves nothing in it, and where the system gives none a search runs as it would alone.
`SCRUTATE_GREP_DEBUG=1` prints the count and the threads taken on stderr each time they change.
Files that cannot be read, or not to the end, are named in warnings on stderr (the first
ten); they do not change the exit code. A search path that does not exist is named in a warning
and the exit code is 2; matches in the other paths are still printed. On Linux, files on `/proc`,
`/sys` or another kernel pseudo filesystem (debugfs, tracefs, securityfs, configfs, cgroup) are
read until the kernel has no more: such a file gives its size as 0 and can answer the first read
short. The walk finds them by one `statfs` per root and per directory named `proc` or `sys`
below one (with `-L`, a link of another name to such a directory is not checked).

The ignore files are `.gitignore`, `.ignore`, `.rgignore`, the global git excludes and
`.git/info/exclude`, read as ripgrep reads them (`.rgignore` before `.ignore` before
`.gitignore`); `--no-ignore` turns them off. As in ripgrep, `scrutate grep` searches
`node_modules` and the other directories the code index skips by name, unless an ignore file
excludes them.

Where the details differ between tools, scrutate does what ripgrep does. `-m N` stops the search
of a file at its Nth matching line: the trailing context after it is printed, and a match inside
that context is shown as a match. `--max-results N` stops after N matching lines in total, with
`-c` and `-l` too; with `--sort` they are the first N in path order, the same on every run. A
file named on the command line that holds a NUL byte is searched as ripgrep does it: the NUL
ends a line, counts and lists include the file, and line output prints one line in place of its
lines, `binary file matches (found "\0" byte around offset N)` (with `--json`, a record
`{"type": "binary", "path", "text"}`). A file the walk finds with a NUL byte is left out of `-c`
and `-l`, so `-v -l`, `-v` and `-v -c` agree. `-c` and `-l` give one record per path given: a
file that two paths reach is counted or listed twice. Smart case folds only a pattern that has a literal and no uppercase letter in
it, so `-S '\p{Ll}'` stays case-sensitive and matches lowercase letters only. Of `-i` and `-S`
the later one wins, and `SCRUTATE_GREP_SMART_CASE` applies only when neither is given. With
`-A`, `-B` or `-C`, groups that are not adjacent, and different files, are separated by a line
`--` (not in `--json`). A `-g` value is one glob, commas included (`-g '*.{txt,log}'`); the
variable `SCRUTATE_GREP_GLOBS` is split at commas outside braces. One difference: a `-g` glob
with a slash (`-g 'sub/*.rs'`) applies under every searched path, where ripgrep matches it from
the current directory. The globs of a `.scrutate.toml` match from that file's directory (see
Configuration above).

`--index` answers from Engine B's text index (`scrutate index`): the index names the files that
can contain a match (trigram postings), and Engine A's matcher searches only those, so the output
is what the walk prints for the indexed snapshot of `--root`. Run `scrutate index` (or `index
--text`) after changing files; the update is incremental. Options that change which files a walk
visits (`--hidden`, `--no-ignore`, `-L`, `--max-depth`, `--max-filesize`, explicit paths) are
refused with `--index`; `-g` and `-t` filter the indexed files. `--index --update` first brings
the text index up to date (building it if there is none; only new and changed files are read),
so the answer is the walk's for the tree as it is now: the call for an editor that searches
often while files change.

## Engine B

| Command | Purpose |
|---|---|
| `index [--force] [--text \| --no-text]` | Build or incrementally update `.scrutate/`: the code-intelligence index and the text index for `grep --index` (`--text`: only the text index; `--no-text`: skip it). Progress on a terminal. |
| `search QUERY [-k N] [-a ALT]... [--also ROOT]... [--why] [--json]` (alias `s`) | Ranked search by meaning, name or identifier; `--alt` fuses alternative phrasings; `--also` searches other repositories too: their results print by the path given (`../two/src/db.py:5-6`), the repository's own as without it, and a root given twice is searched once; with `--json` each result keeps its path in its repository and gains `root`. A definition is matched by its name, signature, body and documentation; the documentation is the comment block right above it plus any docstring, also when attributes, decorators, annotations or type specs stand between (Rust `#[derive(...)]`, `@Get(...)`, `@override`, Elixir `@spec`, Erlang `-spec`), and the language's own form where it has one (Elixir `@doc`, a Julia docstring, Haskell haddock, PowerShell's help block); a comment a blank line away, a shebang, or a comment on the enclosing definition's header line is not it. A query of one word, or of stop words alone, is looked up as a name too (`search find`). |
| `find QUERY` | Unified: regex-looking → `grep`; otherwise ranked search (indexing a small repository on the spot); otherwise identifier grep. A regex-looking query that finds nothing, or is not a valid regex, goes to ranked search when an index exists. A question with a space and a trailing `?` (`how is the token refreshed?`) is taken as words. |
| `symbol NAME [--limit N]` | Where is it defined (`Class.method` works). The name is looked up as typed first: a whole stored name (`list->vec`, `Foo::Bar`, `#main`), a qualified name, or the end of one. Only when that finds nothing, `Type::method`, `Type#method` and `p->method` are read as `Type.method`; then a stored owner is compared without its generic arguments and path (`Stack<T>.push`, `crate::config::Config::new`, `App\Models\User::find`); last, for a name written as a path, a top-level definition of its last part. A name with a leading dot is looked up without it (`.helper` as `helper`). Names match with case and `_` ignored, and the definitions come in this order: the name spelled as typed before its namesakes in another case, then definitions outside test files, then types (`class`, `struct`, `enum`, `trait`, `interface`, `type`, `impl`) before functions and methods before the rest, and a definition with a body before one without. `callers`, `callees`, `similar`, `refs` and `read_code symbol` take names the same way. A method is named by its owner's bare type: `Stack.push` for `impl<T> Stack<T>` in Rust and for `void Stack<T>::push()` in C++. A dotted Lua or Elixir name is split off its owner: `function M.helper` is `helper` with the qualified name `M.helper`, `defmodule MyApp.Accounts.User` is `User`. A method of a JavaScript or TypeScript object literal, and a property whose value is a function, is named under what holds the literal: `api.get`, `store.sub.m`, `tool.build` for `module.exports.tool`, also through `as`, `satisfies` and parentheses; a literal nothing names keeps the bare name. A C or C++ function declared without a body (at file scope, in a namespace, in `extern "C"`, inside an include guard) is a `function` symbol, so a prototype and its definition are two symbols, the definition listed first; a method declared in its class and defined outside it is likewise two, the declaration (`method`) and the definition (`function`). An Objective-C method declared in an `@interface` is a `method` symbol beside its definition, and a function pointer typedef (`typedef int (*cb)(int)`) is the type `cb`. At most 20 definitions, or `--limit`. |
| `callers NAME` / `callees NAME` (`--limit N`) | Call graph edges resolved to definitions (callee names outside the repository listed after `external:`). A call matches a definition by the name as the call writes it (`Ok(1)` is not a call of `ok`) and in its own language family only: JavaScript and TypeScript; C, C++, CUDA and Objective-C; Java, Kotlin, Scala and Groovy; Erlang, Elixir and Gleam; C# and F#; every other language alone (a file of no known language matches any); an unknown qualifier (`Nope.refresh`) gives no results, and a name with a leading dot answers as the name without it (`callers .helper` as `callers helper`). A call the resolver ties to something else (a function of an imported module such as `subprocess.run`, a local variable, a field) is not a call of a namesake in the repository; `callees` lists such a name after `external:`. In a language the resolver does not read (every one outside Python, JavaScript, TypeScript, Rust, Go and Java), a call without a receiver runs the definition in its own file when there is one, a free function of that file or a method of the caller's own type: `callees` lists that one alone, and `callers` of a namesake in another file leaves the call out (`helper()` in `e.c` calls the `static` `helper` of `e.c`). For any other call the resolver cannot place, `callees` lists up to three definitions of the name, those in the caller's file first, then those in its directory. A file whose extension two languages share (`.m`, `.h`, `.pp`, `.cls`) is put in its family by its text, and a Nix function, which is a key, is listed as a callee, as is a macro of the repository (`#define SQUARE(x)`, `macro_rules! square`, a Lisp `defmacro`). A method call `x.m()` is never a call of a free function `m` in Rust, JavaScript, TypeScript, Go and Java, a receiver whose type comes from outside the repository makes the call external, and a definition's own name is not a call of it. Calls inside the arguments of a Rust macro (`println!("{}", f(x))`, `vec![k(1)]`) count. A method call records the method, not the receiver: Lua `obj:meth()`, Objective-C `[obj meth]`, R `obj$meth()`, PowerShell `$obj.Meth()`, PHP `$o?->m()`; `new Point()` in C++, Scala, Groovy, Dart, D and Solidity records `Point`. A Lisp name is recorded whole (`list->vec`, `set-car!`), and a JavaScript private method with its `#`. A call is recorded under the name its definition has: a Ruby or Elixir name keeps its trailing `?` or `!` (`valid?`, `save!`) and a Julia name its `!` (`update!`); an R name keeps its dots (`is.ready`), a Bash name its `::` (`ns::helper`), and a Haskell, OCaml, F# or Nix name its primes (`step'`); a PHP call through a namespace is recorded by its last part (`\App\Util\helper()` calls `helper`); a Rust raw identifier is recorded without its `r#` (`r#type()` calls `type`), and `symbol` and `callers` take either spelling. Dart, Groovy, Elixir, LLVM IR (`call @g`), TLA+ and Verilog (`g(x)` in an expression) record calls; an Elixir `def`, `use`, `alias` or `if` and the head of a definition are not calls. Verilog task calls and module instances, VHDL calls in a subprogram (a subprogram with a body is a function of its package body or architecture), Jsonnet calls, Make `$(call g,...)` and assembly `call`, `bl` and `jal` targets record calls; an assembly label holds its instructions up to the next label or section. Not recorded: a Verilog task call without parentheses (`h;`) or on a member (`obj.m(1);`). A VHDL indexed signal `a(i)` reads as a call, and an assembly call under a local label counts for that label and for the global one above it. `callers` lists first the callers the resolver ties to that definition, then those it could not decide (the ones that name the owner type or sit in the same package first), and says how many are undecided; for a name with thousands of call sites it asks the resolver about the 400 likeliest, lists the others last and says how many it did not check. A call of a class is a call of its constructor: `callers Point.__init__` lists `Point(1)`, and the same holds for a JavaScript or TypeScript `constructor` (`new Widget()`), a Java, C# or C++ constructor, PHP `__construct`, Swift `init`, and Ruby `initialize` through `Invoice.new`. A Java call that its arguments do not tie to one overload is a caller of each overload. A call through a base class, an interface, a trait or an abstract method that the target overrides or implements, at any depth, is listed among the undecided callers of the target (`b.size()` with `b: Base` for `Big.size`); a call that resolves to a sibling's override is not. A variable declared as the base type and built by a constructor in the same function (`Base b = new Big()`, `const b: Base = new Big()`, `b: Base = Big()`, Go `var s Sizer = Big{}`, Rust `Box::new(Big)`) is typed by the constructor until it is assigned again, so its call is a decided caller of the override. `callees` of a function that calls through the base type lists the base method only. A `super` call (Java, TypeScript and JavaScript `super.m()`, Python `super().m()` and `super(C, self).m()`) is a decided call of the nearest base method that defines it, by the method resolution order in Python (`super(C, self).m()` takes the first class after `C` in the order of the class the call sits in, and stays undecided when `C` is not in that order), and never a call of the override it sits in; with a base from outside the repository it is external. Go `x.Embedded.M()` resolves through the embedded field. |
| `refs NAME [--def PATH[:LINE]] [--textual] [--max-results N]` | Every whole-word occurrence, each resolved to the definition it refers to, or listed as unresolved with a reason (see below). It takes the names `symbol` takes (`Stack::push`). |
| `outline FILE` | Symbols of a file in source order. |
| `deps FILE` / `rdeps TARGET` | Imports of a file (resolved to repository files when possible) / files importing a module or file. Imports resolve as in `refs`: Rust `crate::`, `self::` and `super::` paths and `mod` declarations (`deps` lists a `mod` line too), Go imports through `go.mod`, also from a module in a subdirectory, Python `from . import b`, TypeScript `./lib/c.js` to `c.ts` and `./types` to `types.d.ts`. `deps` gives one line per file a statement names. An `#include "util.h"` resolves to the file next to the including one; `rdeps` matches an import by its base name only when it resolves to no file. When nothing resolves to the target but some imports could not be resolved, `rdeps` says so: `no import resolves to X, but N imports could not be resolved and may name it`. It counts a relative import whose path is the target without its extension, the target or its directory, and, in a file that is not JavaScript or TypeScript, a bare import with a part spelled as the target's module name (`b` for `a/b.py` or `a/b/__init__.py`). A Java import resolves through the package declarations of the files, in any source root (`src/main/java` included): a class import to the class's file, `a.b.*` to the files of the package, a static import to the file of its class. An import is recorded as its statement writes it. JavaScript and TypeScript: the module of `import`, of `export ... from` and of `import x = require()`, and the argument of a `require` or `import()` call when it is a string or a template string without `${}`; an exported declaration is no import. PHP: `require` and `include` with either kind of quote, and `use`, plain, `function` or grouped (`use Foo\{A, B}` gives `Foo\A` and `Foo\B`). R: `library`, `require`, `requireNamespace` and `source`. Julia: each module of a `using` or `import` list, the module before a colon, and `include`. Ruby and Lua: `require` and its kin without a receiver, with a literal argument. Kotlin and C#: the imported name, not its alias. Objective-C `@import` and QML imports are recorded too. |
| `similar TARGET [-k N]` | Code similar to a symbol or `path:line` (vector cosine + token Jaccard). A constant has no vector: `similar` on one is an error that says so. |
| `dupes [--min-tokens N] [--threshold J] [--limit N]` | Exact and near-duplicate functions. `exact` means the same body text once runs of whitespace are squeezed; a pair with the same words written differently is `near` with similarity 1.00. `--threshold 1` prints the exact groups only. At most 50 groups, or `--limit`. |
| `tsquery LANG QUERY [PATHS] [--limit N] [--no-cache]` | Tree-sitter structural query over every file of a language up to 1 MB (no index; per-file results are cached, see below). An empty query, a query without a `@capture` and an unknown predicate are errors. A path that does not exist from the current directory is read under `--root`; one that exists in neither place is named in a warning, the other paths are still searched, and the exit code is 2. Paths print as `grep PATH...` prints them, in plain output and in `--json`: the path as given, then the path below it (`src/a.py` for `src`, `./src/a.py` for `./src`, nothing in front for `.` or no path), so a printed path can be passed to the other commands. |
| `check [--files] [--json]` | Verify database integrity, segment consistency and (with `--files`) that indexed files are unchanged. Exit 1 on problems. The line `chunks: N ranked in the database, M alive in segments, K constants (no postings)` counts the chunks that have segment rows, which leaves out constants, and then the constants; `--json` gives them as `constants`. `--json` counts the indexed files also without `--files` (`files_changed` and `files_missing` are then 0: not checked). |
| `stats` | Index statistics (files, chunks, symbols, call edges, imports, segments, and the text index's files, segments and bytes). |
| `languages [--json]` | The verified capability registry. The JSON form gives each language's extensions, the extensions it shares with another language and takes by the file's text (`extensions_by_content`), its file names, and every kind of definition the index gives it (`definition_kinds`: `method` for a function in a class and `constant` included). |
| `watch [PATH]` | Keep the index (and the text index, if there is one) up to date from filesystem events (see below). |
| `serve [--refresh-interval S] [--max-bytes N] [--watch] [--structured]` | MCP server on stdio (see below); `--watch` also runs the watcher in a background thread; `--structured` also sends each answer as JSON. |
| `lsp` | Language Server Protocol server on stdio (see below). |
| `config [--json]` | Show the configuration file in effect. `--json` prints `{"root": ..., "configuration_file": ...}`: the root an index command run here uses, and the configuration file (`null` when none). It builds no index; the editor plugins read it to find the root. |
| `batch FILE [--features] [--timing]` | Run requests from a JSON-lines file with one index open. A line `{"query": ..., "k": ...}` is a ranked search; a line `{"op": TOOL, ...}` runs any operation, named and given arguments as the MCP tool is (`{"op": "callers", "name": "parse"}`), and prints `{"op", "result"}` or `{"op", "error"}`. A line that is not JSON prints `{"error": "line 3, column 2: not JSON (...)"}` and the other lines still run. |

The code index holds every file the ignore rules let through, hidden files included, except
binary files, files over 1 MB, `.git/`, `.scrutate/`, the directories named in
`SCRUTATE_INDEX_EXCLUDE_DIRS`, and cache and tool directories by name (`node_modules`,
`__pycache__`, `.venv`, `.mypy_cache`, `.pytest_cache`, `.next`, `.tox`, `.idea`, `.vscode`).
`build`, `dist`, `target` and `venv` are skipped by name outside a git repository; inside one,
the ignore files decide, so a `target/` that `.gitignore` does not list is indexed. A comma
inside a name in `SCRUTATE_INDEX_EXCLUDE_DIRS` is written `\,`. The ignore files are the ones
`grep` reads, `.rgignore` included. A `.h` file is read as Objective-C when it holds
`@interface`, `@protocol`, `@implementation` or `@class`, as C++ when a line starts with
`namespace`, `class`, `template <`, `using namespace`, an access label (`public:`), `extern
"C++"` or a standard C++ include (`#include <vector>`), and as C otherwise.

Constants are symbols of kind `constant`:

- Rust: `const` and `static` at file level, in a `mod` block or in an `impl` or trait body.
- Go: package-level `const` and `var`.
- JavaScript and TypeScript: a top-level `const` that is not a function (exported or not). In a
  class body, a TypeScript `static readonly` field and a JavaScript `static` field whose name is
  written in capitals (`static MAX = 1`).
- TypeScript: enum members.
- Java: `static final` fields.
- Python: an assignment to a name written in capitals at module level or in a class body, and
  each such name of a tuple or list target (`A, B = 1, 2`, `[A, B] = 1, 2`).
- C, C++, Objective-C and GLSL: `#define NAME value`, enumerators, and a `const` or `constexpr`
  declaration with a value at file level, in a namespace or in `extern "C"`, also inside an
  include guard (`const int SIZE = 4;`, `NSString *const kName = @"x";`, not
  `extern const int SIZE;`).
- C++: a `static const` or `static constexpr` member with a value in a class body
  (`Config.kSize`), and a member defined with its value outside the class
  (`const int Config::kLater = 5;` is `Config.kLater`).

Every name of a Go or Java declaration with several (`var X, Y int`, `const A, B = 1, 2`) is a
constant with the whole declaration as its text.
`symbol`, `outline`, `read_code symbol` and `refs` find them; ranked search (`search`, `find`,
the MCP `search_code`) never returns one.

Ranked search reads words of every script. Scripts written without spaces (Han, kana, Thai, Lao,
Tibetan, Myanmar, Khmer, the Tai scripts, Balinese and Javanese) become overlapping pairs of
characters, so `天気` finds `東京の天気`; any other script gives one
lowercased token per word. Han characters are folded to their simplified form for matching, in
the index and in the query, so `资料库` finds `資料庫` and the reverse; this applies to Japanese
text too. Names, `symbol`, `grep` and the text shown in results keep the characters as written. Only ASCII words are split at case changes and stemmed. Names
compare in Unicode NFC, so a name typed with a combining accent finds the precomposed spelling.
A definition name longer than 200 bytes is still a symbol when it holds no whitespace.

### `refs`
```
scrutate refs NAME [--def PATH[:LINE]] [--textual] [--max-results N] [--json]
```
Finds every whole-word occurrence of `NAME` (as `grep -w`) and decides which definition each one
refers to. For Python, JavaScript, TypeScript, Rust and Go the rules are, in order:

1. An occurrence that is the name of a definition, or the name in an import statement, is that
   definition (or what the import resolves to).
2. A qualified occurrence (`obj.N`, `a::b::N`, `pkg.N`) is looked up in what the qualifier names:
   a module bound by an import (`import x`, `import * as x`, `use crate::a`, a Go import), a
   module path (`crate::`, `self::`, `super::`, a workspace crate, a Rust child module), or a
   class or type (`Class.N`, `Type::N`, then its Python and JavaScript base classes). `self.N`,
   `this.N`, `cls.N` and `Self::N` look in the enclosing class or impl. A receiver that is a
   local variable or a parameter (`x.N`) is looked up in the type its declaration states: an
   annotation (`x: T`, `x T`, `x: &T`) or what it is built from (`T(...)`, `new T(...)`,
   `T::new(...)`, `T { .. }`, `&T{}`), the nearest declaration before the use. A variable that
   gets its value from a function call takes the function's declared return type (through `?`
   and holders such as `Box`, `Rc` and `Arc` in Rust, `await` in TypeScript); `self.x.N` takes
   the declared type of the field `x`, and a module-level or static variable its declared or
   built type. A Go method is found by its receiver type in any file of the package. A method
   call on the result of a call (`mk().norm()`) goes through the type the callee declares it
   returns. `this` inside a method of a JavaScript or TypeScript object literal is the literal
   (`this.put()` in `api.get` is `api.put`); an arrow function takes `this` from the code around
   it. A call on a value of an outside type is external, except when a trait or interface of the
   repository declares a method of that name: then it stays undecided, since a blanket `impl` or
   a merged interface can give the outside type that method. A variable whose type none of these gives is
   unresolved.
3. A plain occurrence is, first, a definition nested in an enclosing function (or directly in an
   enclosing class body); a parameter or local variable of that name hides everything else
   within its own scope (a lambda's parameter inside the lambda, a block-scoped name inside its
   block). A name bound by a pattern hides only in its own branch (Rust `if let`, `while let`
   and a `match` arm, a Python `case`), a `catch` binding and a Go `:=` in an `if`, `for`,
   `switch` or type switch only in that statement, a Python parameter only in the function body
   (defaults and annotations are outside it), a `let` or `for` not in its own value, and an
   import inside a function only in that function. A closure sees the locals of the function it
   is written in; a Rust `fn` item inside a function does not. Then a top-level definition of the
   file; then a name an import binds (`from x import N`,
   `import {N} from './x'`, `use a::N`, `require`); then, in Go, a top-level definition of another
   file of the same package directory; then wildcard imports (`from x import *`, `export * from`,
   `use a::*`, Go dot imports).

The arguments of a Rust macro are tokens to the grammar, so there a name after `.` is a field,
or a method when `(` follows; `name:` inside `{}` is a field name; and a closure parameter or a
`let` is a local. An occurrence matches the name in either Unicode spelling, composed or
decomposed.

Java has rules of its own. A file's package comes from its `package` declaration, so every
source root works. Imports are single-type, on-demand (`.*`) and `static`. Locals are
parameters, lambda parameters and the variables of `for`, `catch`, an `instanceof` pattern and a
try-with-resources. A plain name is also looked up in the enclosing class and the classes and
interfaces it extends, through nested, inner and anonymous classes, enums with methods, records
and default methods. A receiver goes through its declared type, a chain through declared return
types, and a receiver typed by an interface resolves to the interface's method. The number of
arguments picks one overload; when it does not, the call, like a method reference (`Sq::grow`),
stands for every overload of the name. Undecided: an untyped lambda parameter, an element of a
generic type (`list.get(0).area()`), a value of a type parameter, and a class whose base type is
not in the repository. Fields, enum constants and record accessors are not definitions here:
their occurrences are listed as `field or key`.

Import paths resolve to repository files: Python relative imports, packages (`__init__.py`), and
an absolute import whose first part is the name of the checkout directory (`networkx.classes` in
a checkout named `networkx`), and, when no module from the root has the name and the importing
file's directory is not a package (it holds no `__init__.py`), the module beside the importing
file (`from helpers import make_items` in `tests/` finds `tests/helpers.py`, as Python finds it
when it runs the script; such a sibling wins over a standard library module of the same name,
while `import logging` in a package that has its own `logging.py` stays the standard library);
JavaScript relative paths with the usual extensions and
`index` files (a `.js` import finds the `.ts` source), and the names a project gives its own
folders in `tsconfig.json` or `jsconfig.json` (`paths`, `baseUrl`), then a workspace package by
its `package.json` name: through `exports` (the conditions `types`, `import`, `default`, `require`
and `node` in that order, then any other by name), `module`, `main` or an `index` file; a target
in a build directory the repository does not hold stands for the same path under `src/`
(`./dist/index.js` for `src/index.ts`); a subpath through its `exports` key or pattern, else a
path in the package; of two packages with one name, the one nearest the importing file. A
declaration file (`.d.ts`) stands in only when no implementation has the name (`./m` with `m.js`
and `m.d.ts` side by side is `m.js`). Rust module files (`a.rs`, `a/mod.rs`) from
the crate root, workspace crates by their `Cargo.toml` name; Go import paths through the module
path in `go.mod` (by the directory they end with when there is no `go.mod`). `tsconfig.json`,
`jsconfig.json`, `package.json`, `Cargo.toml` and `go.mod` are read only when they are regular
files inside the repository, of at most 1 MB. Re-exports (`from .x import N` in `__init__.py`, `export {N} from`,
`pub use`) are followed. Occurrences in comments and strings are unresolved; so are those in a
language the resolver does not read (`not resolved in this language`) and in a file without a
grammar (`no grammar`), and so is anything the rules do not reach (method calls on a value
whose type they cannot infer, enum variants, local variables). In JavaScript a `var` belongs to
its function, `let` and `const` to their block. Imports of modules outside the repository give
`external`; a package name that no `package.json` of the repository declares is outside.

The output lists each definition with its references, then the unresolved occurrences with the
reason. `--def PATH[:LINE]` keeps one definition of `NAME`: the one in `PATH`, or the one
containing `LINE` when the file has several; no such definition is an error. `--textual` gives the
behaviour without resolution: definitions, callers and every occurrence grouped by enclosing
symbol (it never misses anything, and works without an index). With `--json`, each definition has
a `references` list (`path`, `line`, `column` in bytes, `text`, `via`) and the document has an
`unresolved` list with a `reason` (left out with `--def`). Resolution reads the index's
definitions and parses the files that hold occurrences again, so it needs no index change. The
MCP `references` tool gives the same answer (`textual` and `def` are arguments there).

### Answers from the index: one definition for every front end

`symbol`, `callers`, `callees`, `refs`, `outline`, `deps`, `rdeps`, `similar` and `dupes` are
operations (`ops.rs`): the CLI, the MCP server, `batch` and the Python bindings run the same code
with the same defaults (search `k` 8, callers 30, callees 40, references 200, duplicates 50,
structural search 1000) and print the same text or JSON. An empty answer has exit code 1;
`outline`, `deps`, `rdeps`, `similar` and `dupes` then print nothing on standard output and one
sentence on standard error. When `symbol`, `callers`, `callees`, `dupes`, `tsquery`,
`read_code symbol`, `duplicates` or `structural_search` stops at its limit with more left, the
answer ends with a line that says so and names the argument that raises it (`(stopped at 30
results: raise limit to see more)`): the MCP argument `limit`, and `--limit` for `symbol`,
`callers`, `callees`, `dupes` and `tsquery`; with `serve --structured` the JSON carries
`stopped_at`.

`--max-bytes BYTES` (or `SCRUTATE_MAX_BYTES`) limits the size of such an answer, and of the
answers of `search` and `tsquery` (`--json` ignores both). A cut answer
keeps whole items and ends with a line like `(12 more results not shown: call again with
offset=8, or narrow the request)`; `--offset 8` continues it. The whole answer, that line
included, fits the budget; only a budget smaller than one item and that line together is
passed. An agent that runs the CLI through a shell can set the variable once, so no answer is cut
by its harness in the middle.

### `serve` (MCP)

Fourteen tools over stdio, each one an operation above (`search_code`, `grep_code`,
`find_symbol`, `callers`, `callees`, `read_code`, `outline`, `dependencies`, `dependents`,
`references`, `similar_code`, `duplicates`, `structural_search`, `reindex`).

Before a call the server checks the repository for changed files and updates the index. Without
`--refresh-interval` the wait between scans is at most a second, and shorter when a scan is
cheap; with `--refresh-interval S` (or `SCRUTATE_REFRESH_INTERVAL`) no scan comes sooner than S
seconds after the last one. With `--watch`, each update the watcher makes counts as a scan.
A scan that could not run, because another process held the index, does not start the
interval. Inside that window an answer still checks the files it cites and the files its
arguments name, one `stat` each: when one changed or is gone, or a named file is new and a scan
would index it, those files alone are indexed again and the answer is computed again, once. A
change in a file the answer does not cite or name waits for the next scan.
At the end of its input (EOF) the server
answers the calls in flight, lets a running index build finish, and exits.

- Every tool but `reindex` is marked read-only (`readOnlyHint`), and all are marked as staying on
  this machine (`openWorldHint: false`), so clients that ask before a write do not ask for these.
- The server returns `instructions`: which tool to start with and that answers are capped.
- Every read-only tool takes `max_bytes` and `offset`, as `--max-bytes` and `--offset` above;
  the whole answer, closing line included, fits `max_bytes`. The default is 24,000 bytes (about
  6,000 tokens); `serve --max-bytes N` (or `SCRUTATE_MAX_BYTES`) sets another default, and
  `tools/list` shows it. Both a call's `max_bytes` and the server's default are held between
  1,000 and 1,000,000, and a call's own value wins. `serve` refuses `--offset`: every tool call takes its own.
- Arguments: a wrong type or a negative number is a tool error that names the argument (`k must be a
  non-negative integer`), and JSON `null` is the same as leaving an argument out. An error quotes
  at most 200 characters of an argument. Every path argument may be written `./x` or `pkg/../x`.
  In text answers, control characters in a path are shown as escapes (`\n`, `\x1b`); the JSON
  form keeps them as they are. A path or a location written that way is read back, so what an
  answer prints can be passed to the next call.
- `grep_code` is an Engine A search of the files the index holds: hidden files such as `.github/`
  are searched; ignored files, `.git/` and the directories the index skips by name
  (`node_modules` and the like) are not (`scrutate grep` skips hidden files unless `--hidden`,
  and reads those directories unless an ignore file excludes them). The pattern is a case-sensitive regex (`(?i)` for any
  case); matching lines are grouped by enclosing symbol; `max_results` (default 100) stops it,
  and the answer says so. A capped answer holds the first matches in path order, so the same call
gives the same lines every time and a second page neither repeats nor skips one; `references`
does the same. An invalid pattern is an error.
- `search_code` takes at most 10 `alt_queries`; a cancelled search stops between them.
- Every answer begins each result with its location (`path:start-end`). A location can be passed
  wherever a tool takes a symbol, to mean exactly that definition among several of the same name:
  `read_code symbol`, `find_symbol`, `callers`, `callees`, `similar_code target`, `references def`.
  For `callers` of a location, the resolver decides when another definition of the same name in
  that language could be meant; a call site it cannot decide is kept, and a closing line counts
  those. `callees` of a location lists the calls inside that definition.
- `read_code` reads a line range of a file (`path`, `start`, `end`), or with `symbol` a whole
  definition: a name, `Class.method` or a location. A name with several definitions returns each
  (up to 20), and `path` keeps the ones in that file. With `end`, every line up to `end` is
  read; without it, 400 lines. The answer closes with `(lines A-B of N in P: read_code path=P
  start=B+1 for more)` while lines are left. An empty file answers `P is empty`, a `start` past
  the end `P has N lines: start=S is past the end`; an `end` before `start` and a path that is
  not a regular file are errors. A file is read through a 64 KB buffer and only as far as the last
  line asked for, with at most 1 MB of line text (a longer line is cut and says so). A file with
  a NUL byte in its first 8 KB is refused. A file over 1 MB that was not read to its end closes
  with `of P, more lines follow` in place of the line count, and the JSON `file_lines` is `null`.
- `references` ends with a closing line when `max_results` cut the search, answers `(no
  results)` when nothing is found, and refuses an empty name.
- `structural_search` refuses an empty query, a query without a `@capture` and an unknown
  predicate, as `tsquery` does.
- `reindex` takes `force` (boolean): every file is read again, also one whose size and
  modification time did not change, and a damaged index is rebuilt.
- `serve --structured` (`SCRUTATE_MCP_STRUCTURED=1`) adds an `outputSchema` to every tool and
  `structuredContent` to every result, beside its text: `{"schema": 1, "complete": true,
  "result": ...}`, where `result` is what the matching CLI command prints with `--json`. When the
  text and the JSON together pass one and a half times `max_bytes`, `result` is `null`,
  `complete` is `false` and `text` holds the page that fits; a larger `max_bytes` gets the whole
  answer as JSON. It is off by default because clients differ in what they show their model:
  Claude Code 2.1.322 shows the JSON in place of the text, and the text is the form with the
  snippets (measured with that client on 2026-11-11). Turn it on for a client whose scripts read
  tool results, such as Codex in Code Mode.
- A call runs on a thread of its own, so `notifications/cancelled` reaches it: a call waiting for
  the index or for its turn stops, an index update it runs (`reindex`, or the scan before a read)
  stops and rolls back at its next batch, and the answer of one already computing is dropped.
  Nothing more is sent for a cancelled request.
- While the first index of a repository is being built, by this server or by another process,
  a call waits up to 20 seconds and then answers that the index is still being built. A call that carries a `progressToken` waits for
  the index instead, with a `notifications/progress` every two seconds, and gets its answer.
- Protocol: a client that opens with `initialize` is served revisions 2024-11-05, 2025-03-26,
  2025-06-18 and 2025-11-25. A request that names its version in `_meta`
  (`io.modelcontextprotocol/protocolVersion`) is served by revision 2026-07-28, which has no
  handshake: `server/discover` describes the server, every result carries `resultType`, lists
  carry `ttlMs` and `cacheScope`, and another version in `_meta` is answered with error -32022 and
  the supported one. Both eras work on the same pipe. An `initialize` with a missing or unknown
  `protocolVersion` is answered with 2025-11-25.
- JSON-RPC: a batch (an array of messages) is answered with one array. A line that is not valid
  JSON, or not UTF-8, gets error -32700, and the server goes on reading. A message that is not an
  object, an empty array and a request without `method` get -32600; an unknown method gets
  -32601; an unknown tool or a `tools/call` without `name` gets -32602. A message without `id`
  (a notification) is never answered; only `notifications/cancelled` does anything. A client's
  response (a `result` or `error` without `method`) is ignored.

### `lsp`

```
scrutate lsp
```
A language server for editors (JSON-RPC 2.0 over stdin and stdout, `Content-Length` framing).
The repository is the top of the repository the editor's first workspace folder is in
(`workspaceFolders`, else `rootUri`), found as for a command run in that folder, so the editor
and a shell there share one index; `--root` when the editor sends no folder. A folder of another
repository is not answered. With no folder and no `--root`, started in the home directory or at
a filesystem root that has no index, the server builds none and says so in the log. It answers
from the Engine B index:

| Request | Answer |
|---|---|
| `textDocument/definition` | definitions of the name under the cursor, as `symbol` finds them: first the run of characters around it (`get-user`, `list->vec`, `Foo::Bar`), then the plain word; with a qualifier (`Point.norm`, `a::b`), only the definitions whose qualified name matches, when there are any; definitions in the same file first |
| `textDocument/references` | the occurrences `refs` resolves to the definition under the cursor, and that definition (left out when `includeDeclaration` is false); when the cursor's occurrence is unresolved, every whole-word occurrence, as `refs --textual` |
| `workspace/symbol` | symbols whose name contains the query (case and underscores ignored), exact and prefix matches first, at most 200 |
| `textDocument/documentSymbol` | the `outline` of the file, nested (methods under their class) |

It advertises only these four, plus full text sync. Open documents are read from the editor's
text, so an unsaved line still finds the word under the cursor; locations in other files come from
disk. `didSave` updates the index incrementally on a second connection in a background thread,
so requests keep being answered meanwhile. When the repository has no index, `initialize` builds
one (as `scrutate index` does, without the text index) and reports it with `window/logMessage`;
requests that arrive meanwhile wait for it. Columns are UTF-16 code units, as the protocol
requires. The exit code is 0 after `shutdown` then `exit`, 1 otherwise. Logs go to stderr.

### `watch`
```
scrutate watch [PATH]
```
Sets up the watch on `PATH` (default `--root`), then updates the index once, so a change made
during that first update is seen, prints `watching PATH` on stderr, and waits for filesystem
events. On Linux each directory the indexes can hold gets one inotify watch of its own, and an
ignored tree gets none (2 watches instead of 7,012 on a tree with an ignored `node_modules` and
`target`); the set is walked again after a burst that creates, removes or moves a directory, so
new, moved and replaced directories are watched. The root is watched first, then shallow
directories before deep ones, by path; when some get no watch (past
`fs.inotify.max_user_watches`, or a directory that cannot be read) stderr says how many.
`.git/info` gets a watch too, for its `exclude` file, also the main repository's when the root
is a linked worktree. Windows uses
one recursive ReadDirectoryChangesW watch on `PATH`. On macOS it runs the index's own incremental check
instead, every second or, on a large tree, after a pause ten times as long as the last check took
(at most 30 s): the FSEvents backend would link CoreServices into every `scrutate` command
(about 1 ms of start-up), and the check compares sizes and nanosecond times, so it misses no
edit. Events under `.scrutate/` and `.git/` are dropped. A file of a burst is kept when the
walk of an index lists it in its directory, so every rule the walks follow decides: ignore files
above the repository top, a nested repository's own rules, a linked worktree's `info/exclude`,
the skipped build and vendor directories and `SCRUTATE_INDEX_EXCLUDE_DIRS` for the code index,
hidden paths for the text index. A burst of
events is collected until 200 ms pass without one (at most 2 s). On Linux and Windows a burst
that names only files (written, created, removed or renamed in directories already watched)
updates those files alone, with no walk of the tree and no new watches, so a saved file costs
the same in a tree of 20,000 directories as in a small one. A burst that creates, removes or
moves a directory, changes an ignore file (`.gitignore`, `.ignore`, `.rgignore`,
`.git/info/exclude`) or `.scrutate.toml`, or reports lost events runs the usual incremental
update of the whole tree and reads the ignore files again. The rule files outside the tree
count too: an `.ignore` or `.rgignore` in any directory above the root, a `.gitignore` above it
up to the repository top, `.git/info/exclude` of each repository nested in the tree, git's
global excludes file and the git configuration files that name it. On Linux each directory
above the root gets one watch of its own for this, and another file written there costs a
filter and nothing more; when such a directory cannot be watched the watcher says so once, and
a change to its rules, like one in a rule directory made after the last walk, applies with the
next walk of the tree. So does a burst that changes an
indexed file while a directory has no watch, or once the last walk is 30 s old, which is how a
change the watcher did not see is found. Either way the code index is
updated, and the text index when `.scrutate/text/` exists (`watch` never creates one;
`scrutate index` does). `SCRUTATE_WATCH_DEBUG=1` prints one line per burst on stderr (the
number of paths, and whether the tree was walked). Each
update that changed something prints one line on stderr. A failed first update stops the
watch; a later one that fails is reported and the watch goes on. Ctrl-C stops it after the
update in progress (exit 130). When `PATH` is removed or moved away, `watch` stops with an error
(exit 2). Other processes can read and update
the same index meanwhile; the indexes' own locks order the writers, and stopping the process
mid-update leaves the previous state in place.

`serve --watch` runs the same watcher in a background thread while the server answers requests;
when the watcher stops, stderr says why and the server goes on answering.
The watcher and the tool calls share one engine, so a code index update and a request never run at
the same time in that process.

### `tsquery` cache
Each file's captures are kept in `--root`/.scrutate/tsquery/, one file per (scrutate version,
language, query text, searched paths), keyed by a hash of the file's content. A file whose content
was seen before is not parsed again; the output is byte-identical to `--no-cache`, which parses everything and
leaves the cache alone. A run that visits every file keeps only the entries it used, so edited and
deleted files do not accumulate. The directory is bounded at 128 MB: beyond it the least recently
used query files are deleted, and a query whose results alone exceed it is not cached. The cache
is safe to delete at any time.

### Environment variables
`SCRUTATE_ROOT`, `SCRUTATE_THREADS`, `SCRUTATE_GREP_{HIDDEN,NO_IGNORE,FOLLOW,SMART_CASE,MAX_FILESIZE,MAX_DEPTH,GLOBS,TYPES}`,
`SCRUTATE_INDEX_EXCLUDE_DIRS`, `SCRUTATE_REFRESH_INTERVAL`, `SCRUTATE_MAX_BYTES`,
`SCRUTATE_MCP_STRUCTURED`, `SCRUTATE_CONFIG`, `SCRUTATE_WATCH_DEBUG`, `SCRUTATE_GREP_DEBUG`, `SCRUTATE_SEMANTIC`
(path or `none`), `SCRUTATE_RANKER` (path or `none`), `SCRUTATE_PROFILE` (print indexing phase
timings), `SCRUTATE_DIAGNOSTICS` (`json`: errors as JSON records), `NO_COLOR` (any non-empty
value). On/off variables follow the rule under Conventions.

### The index directory
`.scrutate/index.db` (SQLite, WAL), `.scrutate/seg/*/` (memory-mapped arrays) and
`.scrutate/text/` (the text index: a manifest and immutable trigram segments),
`.scrutate/tsquery/` (the `tsquery` cache), plus a `.gitignore`
so it is never committed. It is a cache: deleting it is always safe; `scrutate index
--force` rebuilds it; a damaged index rebuilds itself. A command that reads the index never waits
for another process that is writing it: it answers from the last committed index and says so in
one line on stderr. The one exception is a first build by another process, when there is nothing
committed to answer from: the command fails with "the index is being built by another process"
(exit 2), and a `serve` tool call waits up to 20 seconds for it before it answers that the
index is still being built. `scrutate index` (and `--force`) waits for the writer. A read-only index
directory, or a read-only repository with a complete index, is answered from the index as it is.
A `.scrutate` that is a symbolic link, or that holds one, is refused: nothing is written or
deleted outside the repository.
