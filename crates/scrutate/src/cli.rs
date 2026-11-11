//! Command-line interface: one binary, two engines.
//!
//! Exit codes follow grep: 0 = something was found, 1 = nothing found, 2 = error, 130 = cancelled.

use std::io::{BufRead, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use termcolor::{Color, ColorChoice, ColorSpec, WriteColor};

use crate::fmt::{fmt_hits, hits_json};
use crate::instant::{self, Case, Mode, Options};
use crate::ops::Op;
use crate::text::query_words;

pub use crate::ops::SCHEMA;

/// Ctrl-C sets a cancellation flag (searches stop between files, exit 130). Direct libc / Win32
/// calls: the `ctrlc` crate links Foundation and libobjc into every macOS process, about 1 ms of
/// start-up on each invocation.
mod interrupt {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, OnceLock};

    static FLAG: OnceLock<Arc<AtomicBool>> = OnceLock::new();

    // async-signal-safe: an atomic load of the cell and an atomic store
    fn fire() {
        if let Some(f) = FLAG.get() {
            f.store(true, Ordering::Relaxed);
        }
    }

    #[cfg(unix)]
    pub fn install(flag: Arc<AtomicBool>) {
        extern "C" fn handler(_: i32) {
            fire()
        }
        extern "C" {
            fn signal(sig: i32, handler: extern "C" fn(i32)) -> usize;
        }
        const SIGINT: i32 = 2;
        if FLAG.set(flag).is_ok() {
            unsafe {
                signal(SIGINT, handler);
            }
        }
    }

    #[cfg(windows)]
    pub fn install(flag: Arc<AtomicBool>) {
        unsafe extern "system" fn handler(_: u32) -> i32 {
            fire();
            1
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn SetConsoleCtrlHandler(handler: Option<unsafe extern "system" fn(u32) -> i32>, add: i32) -> i32;
        }
        if FLAG.set(flag).is_ok() {
            unsafe {
                SetConsoleCtrlHandler(Some(handler), 1);
            }
        }
    }

    #[cfg(not(any(unix, windows)))]
    pub fn install(_flag: Arc<AtomicBool>) {}
}

#[derive(Parser)]
#[command(name = "scrutate", version, about = "Code search: instant grep-class search plus an indexed, ranked code-intelligence engine", long_about = None)]
pub struct Cli {
    /// repository root for the indexed engine (default: current directory)
    #[arg(long, default_value = ".", global = true, env = "SCRUTATE_ROOT")]
    pub root: PathBuf,
    /// never print color (also when NO_COLOR is set and not empty)
    #[arg(long, global = true)]
    pub no_color: bool,
    /// size limit, in bytes, of an answer from the index; a cut answer says how many items are left
    #[arg(long, global = true, value_name = "BYTES", env = "SCRUTATE_MAX_BYTES")]
    pub max_bytes: Option<usize>,
    /// items of an answer to skip, to continue one that --max-bytes cut (not with serve: a tool call takes its own offset)
    #[arg(long, global = true, value_name = "N", default_value_t = 0)]
    pub offset: usize,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Clone, Copy, ValueEnum, Debug)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Powershell,
    Elvish,
}

/// The arguments of `scrutate grep`.
#[derive(clap::Args)]
pub struct GrepArgs {
    /// the pattern (omit it when patterns are given with -e)
    #[arg(required_unless_present = "regexp")]
    pub pattern: Option<String>,
    /// files or directories to search (default: current directory)
    pub paths: Vec<PathBuf>,
    /// a pattern to search for; repeatable, a line matching any of them matches
    #[arg(short = 'e', long = "regexp", value_name = "PATTERN")]
    pub regexp: Vec<String>,
    /// use the text index (Engine B, built by `scrutate index`) to read only the files that can
    /// match; results are those of the indexed snapshot of --root
    #[arg(long)]
    pub index: bool,
    /// with --index: bring the text index up to date first (only changed files are read), so
    /// the answer is the walk's for the tree as it is now
    #[arg(long, requires = "index")]
    pub update: bool,
    /// treat PATTERN as a literal string
    #[arg(short = 'F', long)]
    pub fixed_strings: bool,
    /// identifier search: `refreshToken` also finds refresh_token, RefreshToken, REFRESH_TOKEN
    #[arg(long)]
    pub ident: bool,
    /// approximate search: literal within N edits (insertions, deletions, substitutions)
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "1")]
    pub fuzzy: Option<u8>,
    /// approximate regex: PATTERN within N edits (byte-oriented, ASCII classes, no anchors)
    #[arg(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "1")]
    pub fuzzy_regex: Option<u8>,
    /// PCRE2 syntax: look-around and backreferences (backtracking, stopped by PCRE2's match limit)
    #[arg(short = 'P', long)]
    pub pcre2: bool,
    /// also search inside zip and tar archives and compressed files (gz, bz2, xz, zst, lz4, br, Z)
    #[arg(short = 'z', long)]
    pub search_zip: bool,
    /// case-insensitive
    #[arg(short = 'i', long, overrides_with = "smart_case")]
    pub ignore_case: bool,
    /// case-insensitive unless the pattern has an uppercase letter (of -i and -S the later wins;
    /// SCRUTATE_GREP_SMART_CASE turns it on when neither is given)
    #[arg(short = 'S', long, overrides_with = "ignore_case")]
    pub smart_case: bool,
    /// match whole words only
    #[arg(short = 'w', long)]
    pub word: bool,
    /// let the pattern span lines
    #[arg(short = 'U', long)]
    pub multiline: bool,
    /// select non-matching lines
    #[arg(short = 'v', long)]
    pub invert_match: bool,
    /// print only the paths of files with matches
    #[arg(short = 'l', long)]
    pub files_with_matches: bool,
    /// print only the number of matching lines per file
    #[arg(short = 'c', long)]
    pub count: bool,
    /// print the file's path with each line and count (the default unless one file is the only
    /// path given); of -H and -I the later wins
    #[arg(short = 'H', long, overrides_with = "no_filename")]
    pub with_filename: bool,
    /// never print the file's path with each line and count (the default when one file is the
    /// only path given); -l and --json still print it
    #[arg(short = 'I', long, overrides_with = "with_filename")]
    pub no_filename: bool,
    /// lines of context after each match
    #[arg(short = 'A', long, default_value_t = 0)]
    pub after: usize,
    /// lines of context before each match
    #[arg(short = 'B', long, default_value_t = 0)]
    pub before: usize,
    /// lines of context around each match
    #[arg(short = 'C', long)]
    pub context: Option<usize>,
    /// stop after N matching lines per file
    #[arg(short = 'm', long)]
    pub max_count: Option<u64>,
    /// stop everything after N matches in total
    #[arg(long)]
    pub max_results: Option<u64>,
    /// include/exclude files by glob (prefix ! to exclude); repeatable; SCRUTATE_GREP_GLOBS takes a
    /// comma-separated list (commas inside {} belong to the glob)
    #[arg(short = 'g', long = "glob")]
    pub globs: Vec<String>,
    /// only files of this type (see --type-list); prefix ! to exclude; repeatable
    #[arg(short = 't', long = "type", env = "SCRUTATE_GREP_TYPES", value_delimiter = ',')]
    pub types: Vec<String>,
    /// list the known file types
    #[arg(long)]
    pub type_list: bool,
    /// search hidden files and directories
    #[arg(long, env = "SCRUTATE_GREP_HIDDEN", value_parser = clap::builder::FalseyValueParser::new())]
    pub hidden: bool,
    /// ignore .gitignore / .ignore rules
    #[arg(long, env = "SCRUTATE_GREP_NO_IGNORE", value_parser = clap::builder::FalseyValueParser::new())]
    pub no_ignore: bool,
    /// follow symbolic links
    #[arg(short = 'L', long, env = "SCRUTATE_GREP_FOLLOW", value_parser = clap::builder::FalseyValueParser::new())]
    pub follow: bool,
    /// skip files larger than this (e.g. 10M)
    #[arg(long, value_name = "SIZE", env = "SCRUTATE_GREP_MAX_FILESIZE")]
    pub max_filesize: Option<String>,
    /// descend at most this many directory levels
    #[arg(long, env = "SCRUTATE_GREP_MAX_DEPTH")]
    pub max_depth: Option<usize>,
    /// number of search threads (default: the cores, at most 12; on macOS the performance cores; more when the files come from the disk)
    #[arg(short = 'j', long, default_value_t = 0, env = "SCRUTATE_THREADS")]
    pub threads: usize,
    /// sort results by path (deterministic, waits for the search to finish)
    #[arg(long)]
    pub sort: bool,
    /// one JSON object per line
    #[arg(long)]
    pub json: bool,
    /// print search statistics to stderr
    #[arg(long)]
    pub stats: bool,
    /// group matches by enclosing function/class using the index (Engine B), if one exists
    #[arg(long)]
    pub symbols: bool,
}

/// `Cli` with `grep` as its only subcommand: clap builds every subcommand's arguments before it
/// parses, about 0.2 ms for all of them, and a search needs one.
#[derive(Parser)]
#[command(name = "scrutate", version, about = "Code search: instant grep-class search plus an indexed, ranked code-intelligence engine", long_about = None)]
struct GrepCli {
    /// repository root for the indexed engine (default: current directory)
    #[arg(long, default_value = ".", global = true, env = "SCRUTATE_ROOT")]
    root: PathBuf,
    /// never print color (also when NO_COLOR is set and not empty)
    #[arg(long, global = true)]
    no_color: bool,
    #[command(subcommand)]
    cmd: GrepCmd,
}

#[derive(Subcommand)]
enum GrepCmd {
    /// Engine A: instant search of files (no index). PATTERN is a regex unless -F, --ident or --fuzzy
    #[command(alias = "g")]
    Grep(GrepArgs),
}

#[derive(Subcommand)]
pub enum Cmd {
    /// Engine A: instant search of files (no index). PATTERN is a regex unless -F, --ident or --fuzzy
    #[command(alias = "g")]
    Grep(GrepArgs),
    /// Engine B: build or update the index (code intelligence and the text index for `grep --index`)
    Index {
        #[arg(long)]
        force: bool,
        /// only the text index (trigram postings for `grep --index`)
        #[arg(long, conflicts_with = "no_text")]
        text: bool,
        /// skip the text index
        #[arg(long)]
        no_text: bool,
    },
    /// Engine B: ranked search by meaning, name or identifier
    #[command(alias = "s")]
    Search {
        query: String,
        /// alternative phrasing (repeatable); results of all phrasings are fused
        #[arg(short = 'a', long = "alt")]
        alt: Vec<String>,
        /// also search these repositories (repeatable): cross-repository results, paths prefixed with the repository name
        #[arg(long = "also", value_name = "ROOT")]
        also: Vec<PathBuf>,
        #[arg(short, default_value_t = crate::ops::SEARCH_K)]
        k: usize,
        /// show why each result matched
        #[arg(long)]
        why: bool,
        #[arg(long)]
        json: bool,
    },
    /// Unified: pick the engine from the query (regex-looking -> grep, otherwise ranked search; falls back to identifier grep without an index)
    Find {
        query: String,
        #[arg(short, default_value_t = crate::ops::SEARCH_K)]
        k: usize,
        #[arg(long)]
        json: bool,
    },
    /// where is this function/class/method defined?
    Symbol {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// which functions call this one?
    Callers {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// which functions does this one call?
    Callees {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// modules/files a file imports (Engine B)
    Deps {
        file: String,
        #[arg(long)]
        json: bool,
    },
    /// files that import a module or file (reverse dependencies)
    Rdeps {
        target: String,
        #[arg(long)]
        json: bool,
    },
    /// symbols of a file in source order
    Outline {
        file: String,
        #[arg(long)]
        json: bool,
    },
    /// every occurrence of a name, each resolved to the definition it refers to (or unresolved)
    Refs {
        name: String,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = crate::ops::REFS_MAX)]
        max_results: u64,
        /// only the references of the definition of NAME in PATH (at LINE, when several)
        #[arg(long = "def", value_name = "PATH[:LINE]")]
        def: Option<String>,
        /// no resolution: definitions, callers and every whole-word occurrence grouped by enclosing symbol
        #[arg(long, conflicts_with = "def")]
        textual: bool,
    },
    /// code similar to a symbol or location (`name`, `Class.method` or `path:line`)
    Similar {
        target: String,
        #[arg(short, default_value_t = crate::ops::SIMILAR_K)]
        k: usize,
        #[arg(long)]
        json: bool,
    },
    /// duplicate and near-duplicate functions across the repository
    Dupes {
        /// ignore functions with fewer body tokens than this
        #[arg(long, default_value_t = crate::ops::DUPES_MIN_TOKENS)]
        min_tokens: usize,
        /// near-duplicate threshold (Jaccard similarity of token 3-grams)
        #[arg(long, default_value_t = crate::ops::DUPES_THRESHOLD)]
        threshold: f32,
        #[arg(long, default_value_t = crate::ops::DUPES_LIMIT)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// structural search with a tree-sitter query, e.g. `scrutate tsquery python '(function_definition name: (identifier) @n)'`
    Tsquery {
        lang: String,
        query: String,
        #[arg(default_value = ".")]
        paths: Vec<PathBuf>,
        #[arg(long, default_value_t = crate::ops::TSQUERY_LIMIT)]
        limit: usize,
        #[arg(long)]
        json: bool,
        /// parse every file again instead of reusing the results cached in `--root`/.scrutate/tsquery
        #[arg(long)]
        no_cache: bool,
    },
    /// index statistics
    Stats,
    /// keep the index (and the text index, if there is one) up to date as files change
    Watch {
        /// repository to watch (default: --root)
        path: Option<PathBuf>,
    },
    /// run the MCP server on stdio
    Serve {
        /// at most one directory scan per this many seconds [default: up to 1, less when a scan is cheap]
        #[arg(long, env = "SCRUTATE_REFRESH_INTERVAL")]
        refresh_interval: Option<f64>,
        /// also update the index from filesystem events, in the background
        #[arg(long)]
        watch: bool,
        /// also send each answer as JSON (`structuredContent`, with an `outputSchema` per tool), for
        /// clients whose scripts read tool results; a client that shows its model the JSON in
        /// place of the text loses the text's snippets
        #[arg(long, env = "SCRUTATE_MCP_STRUCTURED", value_parser = clap::builder::FalseyValueParser::new())]
        structured: bool,
    },
    /// run a Language Server Protocol server on stdio (definitions, references, symbols from the index)
    Lsp,
    /// verify the index: database integrity, segment consistency, and (with --files) that every
    /// indexed file still has the content that was indexed
    Check {
        #[arg(long)]
        files: bool,
        #[arg(long)]
        json: bool,
    },
    /// print the configuration file in effect and the variables it sets
    Config,
    /// list the languages this build parses structurally (--json: the capability registry)
    Languages {
        #[arg(long)]
        json: bool,
    },
    /// print shell completions
    Completions { shell: Shell },
    /// print the parse tree of a file (debugging grammar rules)
    #[command(hide = true)]
    Sexp { file: PathBuf },
    /// run many ranked queries from a JSON-lines file ({"query": ..., "k": 10, "alt": [...]})
    Batch {
        file: PathBuf,
        /// include the 47 features and scores of every candidate (parity testing)
        #[arg(long)]
        features: bool,
        #[arg(long)]
        timing: bool,
    },
}

fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim();
    let (num, mult) = match s.chars().last() {
        Some('K') | Some('k') => (&s[..s.len() - 1], 1024u64),
        Some('M') | Some('m') => (&s[..s.len() - 1], 1024 * 1024),
        Some('G') | Some('g') => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        _ => (s, 1),
    };
    num.parse::<u64>().ok().and_then(|n| n.checked_mul(mult)).with_context(|| format!("bad size {s:?}"))
}

fn open_fresh(root: &Path) -> Result<crate::search::Searcher> {
    let (s, st) = crate::open_updated(root, false)?;
    if st.files_changed > 0 || st.files_removed > 0 {
        eprintln!("[scrutate] indexed {} changed files ({} chunks, {:.1}s)", st.files_changed, st.chunks, st.seconds);
    }
    Ok(s)
}

fn color_choice(no_color: bool) -> ColorChoice {
    if no_color || !std::io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty()) {
        ColorChoice::Never
    } else {
        ColorChoice::Auto
    }
}

/// True when the query looks like a regular expression rather than words or an identifier.
pub fn looks_like_regex(q: &str) -> bool {
    q.chars().any(|c| matches!(c, '*' | '+' | '?' | '[' | ']' | '(' | ')' | '|' | '^' | '$' | '\\' | '{' | '}'))
}

/// A comma-separated list of globs, split at the commas outside `{}`.
fn split_globs(list: &str) -> Vec<String> {
    let (mut out, mut cur, mut depth) = (Vec::new(), String::new(), 0usize);
    for c in list.chars() {
        match c {
            '{' => depth += 1,
            '}' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(std::mem::take(&mut cur));
                continue;
            }
            _ => {}
        }
        cur.push(c);
    }
    out.push(cur);
    out.retain(|g| !g.is_empty());
    out
}

pub fn run() -> Result<i32> {
    // configuration file -> environment -> clap (command line still wins)
    let args: Vec<String> = std::env::args_os().map(|a| a.to_string_lossy().into_owned()).collect();
    let cfg_root = crate::config::root_from_args(&args);
    let cfg_path = crate::config::locate(&cfg_root);
    if let Some(p) = &cfg_path {
        let cfg = crate::config::load(p)?;
        crate::config::apply_as_env(&cfg);
    }
    let cli = if matches!(args.get(1).map(String::as_str), Some("grep" | "g")) {
        let GrepCli { root, no_color, cmd: GrepCmd::Grep(grep) } = GrepCli::parse();
        Cli { root, no_color, max_bytes: None, offset: 0, cmd: Cmd::Grep(grep) }
    } else {
        Cli::parse()
    };
    let no_color = cli.no_color;
    let page = (cli.max_bytes.unwrap_or(usize::MAX), cli.offset);
    match cli.cmd {
        Cmd::Grep(GrepArgs {
            pattern,
            mut paths,
            regexp,
            index,
            update,
            fixed_strings,
            pcre2,
            fuzzy_regex,
            search_zip,
            ident,
            fuzzy,
            ignore_case,
            smart_case,
            word,
            multiline,
            invert_match,
            files_with_matches,
            count,
            with_filename,
            no_filename,
            after,
            before,
            context,
            max_count,
            max_results,
            globs,
            types,
            type_list,
            hidden,
            no_ignore,
            follow,
            max_filesize,
            max_depth,
            threads,
            sort,
            json,
            stats,
            symbols,
        }) => {
            // read here, not by clap: with an environment value clap rejects -i as conflicting
            let smart_case = smart_case
                || !ignore_case
                    && std::env::var("SCRUTATE_GREP_SMART_CASE")
                        .is_ok_and(|v| !matches!(v.to_ascii_lowercase().as_str(), "" | "0" | "n" | "no" | "f" | "false" | "off"));
            let globs = if globs.is_empty() { std::env::var("SCRUTATE_GREP_GLOBS").map(|v| split_globs(&v)).unwrap_or_default() } else { globs };
            if type_list {
                for (name, globs) in instant::type_list() {
                    println!("{name}: {}", globs.join(", "));
                }
                return Ok(0);
            }
            if pcre2 && (ident || fuzzy.is_some()) {
                anyhow::bail!("-P/--pcre2 cannot be combined with --ident or --fuzzy");
            }
            if fuzzy_regex.is_some() && (pcre2 || ident || fuzzy.is_some() || fixed_strings) {
                anyhow::bail!("--fuzzy-regex cannot be combined with -P, --ident, --fuzzy or -F (use --fuzzy for a literal)");
            }
            let mode = match (fixed_strings, ident, fuzzy) {
                _ if fuzzy_regex.is_some() => Mode::FuzzyRegex { max_edits: fuzzy_regex.unwrap_or(1) },
                (_, _, Some(n)) => Mode::Fuzzy { max_edits: n },
                (_, true, _) => Mode::Identifier,
                _ if pcre2 => Mode::Pcre2,
                (true, _, _) => Mode::Literal,
                _ => Mode::Regex,
            };
            let (pattern, mode) = if regexp.is_empty() {
                instant::join_patterns(&[pattern.unwrap_or_default()], mode, fixed_strings)?
            } else {
                // with -e, the first positional argument is a path
                if let Some(p) = pattern {
                    paths.insert(0, PathBuf::from(p));
                }
                instant::join_patterns(&regexp, mode, fixed_strings)?
            };
            let mut o = Options {
                pattern,
                mode,
                case: if ignore_case {
                    Case::Insensitive
                } else if smart_case {
                    Case::Smart
                } else {
                    Case::Sensitive
                },
                word,
                multiline,
                invert: invert_match,
                hidden,
                no_ignore,
                follow_links: follow,
                globs,
                types,
                max_filesize: max_filesize.as_deref().map(parse_size).transpose()?,
                max_depth,
                max_count,
                max_results,
                before: context.unwrap_or(before),
                after: context.unwrap_or(after),
                threads,
                count_only: count,
                files_only: files_with_matches,
                want_bytes: stats || json,
                want_spans: json || color_choice(no_color) != ColorChoice::Never,
                search_zip,
                paths_as_given: !paths.is_empty(),
                // as ripgrep: one path that is not a directory is searched without its name
                with_filename: with_filename || (!no_filename && !(paths.len() == 1 && !paths[0].is_dir())),
                small_tree: std::env::var("SCRUTATE_SMALL_TREE").ok().and_then(|v| v.parse().ok()),
                ..Options::default()
            };
            if let Some(n) = std::env::var("SCRUTATE_SPLIT_FILE_BYTES").ok().and_then(|v| v.parse().ok()) {
                o.split_file_bytes = n;
            }
            if index {
                o.check_indexable()?;
                if !paths.is_empty() {
                    anyhow::bail!("--index searches the indexed repository (--root); narrow it with -g/-t instead of paths");
                }
            }
            let roots: Vec<PathBuf> = if paths.is_empty() { vec![PathBuf::from(".")] } else { paths };
            grep_command(roots, o, GrepOutput { files_with_matches, count, sort, json, stats, symbols, root: cli.root, no_color, indexed: index, update })
        }
        Cmd::Index { force, text, no_text } => {
            let progress = std::io::stderr().is_terminal();
            if !text {
                let mut repo = crate::Repo::open(&cli.root)?;
                repo.ix.progress = true;
                let st = repo.update(force)?;
                println!("{} files, {} changed, {} chunks, semantic={}, {:.1}s", st.files_total, st.files_changed, st.chunks, st.semantic, st.seconds);
            }
            if !no_text {
                let root = std::fs::canonicalize(&cli.root).unwrap_or_else(|_| cli.root.clone());
                let t = crate::textindex::update(&root, force, progress)?;
                println!(
                    "text index: {} files, {} changed, {} removed, {} segment(s), {:.1} MB, {:.1}s",
                    t.files,
                    t.files_changed,
                    t.files_removed,
                    t.segments,
                    t.bytes as f64 / 1e6,
                    t.seconds
                );
            }
            Ok(0)
        }
        Cmd::Stats => {
            let s = open_fresh(&cli.root)?;
            let mut v = s.ix.stats()?;
            if let Some(t) = crate::textindex::TextIndex::open(&s.ix.root)? {
                v["text_index"] =
                    serde_json::json!({"files": t.files(), "segments": t.segments(), "bytes": crate::textindex::TextIndex::disk_bytes(&s.ix.root)});
            }
            println!("{}", serde_json::to_string(&v)?);
            Ok(0)
        }
        Cmd::Search { query, alt, also, k, why, json } => {
            let mut qs = vec![query.clone()];
            qs.extend(alt.iter().cloned());
            let mut roots: Vec<PathBuf> = vec![cli.root.clone()];
            roots.extend(also.iter().cloned());
            let mut hits = Vec::new();
            for root in &roots {
                let mut s = open_fresh(root)?;
                let mut part = if alt.is_empty() { s.search(&query, k, true)? } else { s.search_many(&qs, k, true)? };
                if roots.len() > 1 {
                    // cross-repository: the repository name becomes the first path component
                    let label = root
                        .canonicalize()
                        .ok()
                        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
                        .unwrap_or_else(|| root.display().to_string());
                    for h in &mut part {
                        h.path = format!("{label}/{}", h.path);
                    }
                }
                hits.extend(part);
            }
            if roots.len() > 1 {
                // the ranker's scores are comparable across repositories (same model, same features)
                hits.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then(a.path.cmp(&b.path)));
                hits.truncate(k);
            }
            print_hits(&hits, &qs, why, json)?;
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        Cmd::Check { files, json } => check_command(&cli.root, files, json),
        Cmd::Config => {
            match &cfg_path {
                Some(p) => {
                    println!("configuration file: {}", p.display());
                    println!("{}", std::fs::read_to_string(p)?.trim_end());
                }
                None => println!(
                    "no configuration file (looked for $SCRUTATE_CONFIG, {}/.scrutate.toml, $XDG_CONFIG_HOME/scrutate/config.toml)",
                    cfg_root.display()
                ),
            }
            println!("\nenvironment in effect:");
            let mut vars: Vec<(String, String)> = std::env::vars().filter(|(k, _)| k.starts_with("SCRUTATE_")).collect();
            vars.sort();
            for (k, v) in vars {
                println!("  {k}={v}");
            }
            Ok(0)
        }
        Cmd::Find { query, k, json } => find_command(&cli.root, &query, k, json, no_color),
        Cmd::Symbol { name, json } => run_op(&cli.root, Op::Symbol { name, limit: crate::ops::SYMBOL_LIMIT }, json, page),
        Cmd::Callers { name, json } => run_op(&cli.root, Op::Callers { name, limit: crate::ops::CALLERS_LIMIT }, json, page),
        Cmd::Callees { name, json } => run_op(&cli.root, Op::Callees { name, limit: crate::ops::CALLEES_LIMIT }, json, page),
        Cmd::Deps { file, json } => run_op(&cli.root, Op::Deps { path: file }, json, page),
        Cmd::Rdeps { target, json } => run_op(&cli.root, Op::Rdeps { target }, json, page),
        Cmd::Outline { file, json } => run_op(&cli.root, Op::Outline { path: file }, json, page),
        Cmd::Refs { name, json, max_results, def, textual } => run_op(&cli.root, Op::Refs { name, max_results, textual, def }, json, page),
        Cmd::Similar { target, k, json } => run_op(&cli.root, Op::Similar { target, k }, json, page),
        Cmd::Dupes { min_tokens, threshold, limit, json } => run_op(&cli.root, Op::Dupes { min_tokens, threshold, limit }, json, page),
        Cmd::Tsquery { lang, query, paths, limit, json, no_cache } => {
            // streamed: one JSON object or one text line per capture, in path order
            let stdout = std::io::stdout();
            let mut w = std::io::BufWriter::new(stdout.lock());
            let mut err: Option<std::io::Error> = None;
            let cache_root = (!no_cache).then_some(cli.root.as_path());
            let n = crate::intel::tsquery_each(&lang, &query, &paths, limit, cache_root, &mut |c| {
                if err.is_some() {
                    return;
                }
                let r = if json {
                    serde_json::to_writer(&mut w, &c)
                        .map_err(|e| std::io::Error::new(e.io_error_kind().unwrap_or(std::io::ErrorKind::Other), e))
                        .and_then(|_| w.write_all(b"\n"))
                } else {
                    writeln!(w, "{}:{}:{}  @{} ({})  {}", crate::fmt::shown(&c.path), c.line, c.column, c.capture, c.kind, c.text)
                };
                if let Err(e) = r {
                    err = Some(e);
                }
            })?;
            if let Some(e) = err {
                if e.kind() != std::io::ErrorKind::BrokenPipe {
                    return Err(e.into());
                }
            }
            let _ = w.flush();
            Ok(if n == 0 { 1 } else { 0 })
        }
        Cmd::Serve { refresh_interval, watch, structured } => {
            if cli.offset != 0 {
                anyhow::bail!("--offset does nothing with serve: a tool call takes its own offset");
            }
            let s = crate::Repo::open(&cli.root)?;
            let root = s.ix.root.clone();
            let s = Arc::new(Mutex::new(s));
            if watch {
                let s = Arc::clone(&s);
                std::thread::Builder::new().name("scrutate-watch".into()).spawn(move || {
                    static NEVER: AtomicBool = AtomicBool::new(false);
                    if let Err(e) = crate::watch::update_text(&root) {
                        eprintln!("[scrutate] text index update failed: {e:#}");
                    }
                    // the searcher's lock makes this the only writer in the process; the index's own
                    // locks order it with other processes
                    let r = crate::watch::run(
                        &root,
                        &NEVER,
                        || eprintln!("[scrutate] watching {}", root.display()),
                        || {
                            s.lock().unwrap_or_else(|p| p.into_inner()).update(false)?;
                            crate::watch::update_text(&root)?;
                            Ok(())
                        },
                    );
                    if let Err(e) = r {
                        eprintln!("[scrutate] watching stopped: {e:#}");
                    }
                })?;
            }
            crate::mcp::serve(s, crate::mcp::Settings::new(cli.max_bytes, refresh_interval), crate::VERSION, structured)?;
            Ok(0)
        }
        Cmd::Lsp => crate::lsp::serve(&cli.root),
        Cmd::Watch { path } => watch_command(path.as_deref().unwrap_or(&cli.root)),
        Cmd::Languages { json: true } => {
            // the verified capability registry: what each grammar's rules extract
            let v: Vec<serde_json::Value> = crate::langs::LANGS
                .iter()
                .map(|spec| {
                    let mut kinds: Vec<&str> = spec.defs.iter().map(|d| d.kind).collect();
                    kinds.sort();
                    kinds.dedup();
                    serde_json::json!({
                        "id": spec.id, "extensions": spec.exts, "filenames": spec.files,
                        "definitions": !spec.defs.is_empty(), "definition_kinds": kinds,
                        "call_edges": !spec.calls.is_empty(), "imports": !spec.imports.is_empty(),
                        "docstrings": spec.docstring, "tier": "grammar"
                    })
                })
                .collect();
            println!("{}", serde_json::to_string_pretty(&v)?);
            Ok(0)
        }
        Cmd::Languages { json: false } => {
            for spec in crate::langs::LANGS.iter() {
                println!(
                    "{:<16} {}{}",
                    spec.id,
                    spec.exts.join(" "),
                    if spec.files.is_empty() { String::new() } else { format!("  files: {}", spec.files.join(" ")) }
                );
            }
            Ok(0)
        }
        Cmd::Completions { shell } => {
            let mut cmd = Cli::command();
            let name = cmd.get_name().to_string();
            let sh = match shell {
                Shell::Bash => clap_complete::Shell::Bash,
                Shell::Zsh => clap_complete::Shell::Zsh,
                Shell::Fish => clap_complete::Shell::Fish,
                Shell::Powershell => clap_complete::Shell::PowerShell,
                Shell::Elvish => clap_complete::Shell::Elvish,
            };
            clap_complete::generate(sh, &mut cmd, name, &mut std::io::stdout());
            Ok(0)
        }
        Cmd::Sexp { file } => {
            let source = std::fs::read_to_string(&file)?;
            let rel = file.to_string_lossy().into_owned();
            match crate::parse::spec_for(&rel, &source) {
                Some(spec) => {
                    println!("grammar: {}", spec.id);
                    println!("{}", crate::parse::sexp(spec, &source));
                }
                None => println!("no grammar for {rel} (heuristic symbols)"),
            }
            for c in crate::parse::parse_source(&rel, &source) {
                println!(
                    "CHUNK {:<9} {:<30} L{}-{} doc={:?} calls={:?}",
                    c.kind,
                    c.qualname,
                    c.start,
                    c.end,
                    c.doc.lines().next().unwrap_or(""),
                    c.calls.iter().take(6).collect::<Vec<_>>()
                );
            }
            Ok(0)
        }
        Cmd::Batch { file, features, timing } => batch_command(&cli.root, &file, features, timing),
    }
}

/// Run one index operation and print its answer; exit code 1 when nothing was found.
fn run_op(root: &Path, op: Op, json: bool, (max_bytes, offset): (usize, usize)) -> Result<i32> {
    use crate::ops::Answer;
    let mut s = open_fresh(root)?;
    let answer = crate::ops::run(&mut s, &op)?;
    // these print nothing on standard output when empty, so a pipeline sees no lines
    let quiet = matches!(answer, Answer::Outline { .. } | Answer::Deps { .. } | Answer::Rdeps { .. } | Answer::Similar(_) | Answer::Dupes(_));
    if json {
        println!("{}", serde_json::to_string_pretty(&answer.json())?);
    } else if quiet && answer.is_empty() {
        eprintln!("{}", answer.text());
    } else {
        let text = answer.text_within(max_bytes, offset);
        print!("{text}{}", if text.ends_with('\n') { "" } else { "\n" });
    }
    Ok(if answer.is_empty() { 1 } else { 0 })
}

/// `watch`: update the indexes now, then after every burst of file changes until Ctrl-C.
fn watch_command(root: &Path) -> Result<i32> {
    let root = std::fs::canonicalize(root).with_context(|| format!("watch {}", root.display()))?;
    let stop = Arc::new(AtomicBool::new(false));
    interrupt::install(Arc::clone(&stop));
    let mut repo = crate::Repo::open(&root)?;
    let mut update = || -> Result<()> {
        let st = repo.update(false)?;
        let text = crate::watch::update_text(&root)?;
        let text_changed = text.as_ref().map(|t| t.files_changed + t.files_removed).unwrap_or(0);
        if st.files_changed + st.files_removed + text_changed > 0 {
            eprintln!(
                "[scrutate] {} changed, {} removed ({} chunks){}",
                st.files_changed,
                st.files_removed,
                st.chunks,
                text.map(|t| format!("; text index: {} changed, {} removed", t.files_changed, t.files_removed)).unwrap_or_default()
            );
        }
        Ok(())
    };
    update()?;
    crate::watch::run(&root, &stop, || eprintln!("[scrutate] watching {}", root.display()), update)?;
    Ok(if stop.load(Ordering::Relaxed) { 130 } else { 0 })
}

/// `check`: integrity of the database and the segments; with `--files`, re-hash every indexed file.
fn check_command(root: &Path, files: bool, json: bool) -> Result<i32> {
    let mut s = crate::open(root)?;
    let report = s.ix.check(files)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("database: {}", if report.database_ok { "ok" } else { "CORRUPT" });
        println!("segments: {} checked, {}", report.segments, if report.segments_ok { "ok" } else { "DAMAGED" });
        println!(
            "chunks: {} in database, {} alive in segments{}",
            report.chunks_db,
            report.chunks_alive,
            if report.chunks_db == report.chunks_alive { "" } else { "  MISMATCH" }
        );
        if files {
            println!("files: {} indexed, {} changed since indexing, {} missing", report.files_indexed, report.files_changed, report.files_missing);
        }
        for p in &report.problems {
            println!("problem: {p}");
        }
        println!("{}", if report.ok() { "OK" } else { "PROBLEMS FOUND (run `scrutate index` to repair; `scrutate index --force` rebuilds from scratch)" });
    }
    Ok(if report.ok() { 0 } else { 1 })
}

fn print_hits(hits: &[crate::search::Hit], qs: &[String], why: bool, json: bool) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(&hits_json(hits))?);
    } else {
        let words: Vec<String> = qs.iter().flat_map(|q| query_words(q)).collect();
        println!("{}", fmt_hits(hits, &words, true, why));
    }
    Ok(())
}

struct GrepOutput {
    files_with_matches: bool,
    count: bool,
    sort: bool,
    json: bool,
    stats: bool,
    symbols: bool,
    root: PathBuf,
    no_color: bool,
    /// answer from the text index of `root` instead of walking `roots`
    indexed: bool,
    /// refresh that index first
    update: bool,
}

/// The canonical root and the indexed files that can match `o` (after an incremental refresh of
/// the text index when `update`).
fn indexed_candidates(root: &Path, o: &Options, update: bool) -> Result<(PathBuf, Vec<String>)> {
    let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
    if update {
        crate::textindex::update(&root, false, false)?;
    }
    let ti = crate::textindex::TextIndex::open(&root)?
        .ok_or_else(|| anyhow::anyhow!("no text index in {}; build it with `scrutate index --text` (or add --update)", root.display()))?;
    let files = ti.candidates(&crate::textindex::query_for(o)?, o.multiline)?;
    Ok((root, files))
}

fn grep_command(roots: Vec<PathBuf>, o: Options, out: GrepOutput) -> Result<i32> {
    let cancel = Arc::new(AtomicBool::new(false));
    interrupt::install(Arc::clone(&cancel));
    let colour = color_choice(out.no_color) != ColorChoice::Never;
    if !out.json && !colour && !out.sort && !out.symbols && !o.count_only && !o.files_only {
        return grep_plain(roots, o, out, cancel);
    }
    // buffered: one write per 64 KB instead of several per matching line
    let mut w = termcolor::BufferedStandardStream::stdout(color_choice(out.no_color));
    let mut path_color = ColorSpec::new();
    path_color.set_fg(Some(Color::Magenta));
    let mut line_color = ColorSpec::new();
    line_color.set_fg(Some(Color::Green));
    let mut match_color = ColorSpec::new();
    match_color.set_fg(Some(Color::Red)).set_bold(true);
    let mut collected: Vec<instant::Hit> = Vec::new();
    let mut pipe_closed = false; // the reader went away (`| head`): stop quietly, like ripgrep (exit 0)
    let mut counts: std::collections::BTreeMap<Arc<str>, u64> = std::collections::BTreeMap::new();
    let mut files: Vec<Arc<str>> = Vec::new();
    let mut seen_files: std::collections::HashSet<Arc<str>> = std::collections::HashSet::new();
    let defer = out.sort || out.count || out.files_with_matches || out.symbols;
    let bookkeeping = out.count || out.files_with_matches;
    let mut found = false;
    let mut linebuf: Vec<u8> = Vec::with_capacity(512);
    let groups = o.before > 0 || o.after > 0;
    let mut last: Option<(Arc<str>, u64)> = None;
    let mut emit = |h: &instant::Hit, w: &mut termcolor::BufferedStandardStream| -> Result<()> {
        if out.json {
            let rec = serde_json::json!({"type": if h.context { "context" } else { "match" }, "schema": SCHEMA, "path": h.path, "line": h.line,
                "column": h.column, "text": h.text, "spans": h.spans.iter().map(|(a, b)| serde_json::json!({"start": a, "end": b})).collect::<Vec<_>>()});
            writeln!(w, "{rec}")?;
            return Ok(());
        }
        if groups {
            // `--` between lines that are not adjacent in one file, as the plain path's searcher does
            if last.as_ref().is_some_and(|(p, l)| **p != *h.path || h.line > l + 1) {
                w.write_all(b"--\n")?;
            }
            last = Some((Arc::clone(&h.path), h.line + h.text.matches('\n').count() as u64));
        }
        if !colour {
            // plain output: one assembled line, one write (the common piped case)
            linebuf.clear();
            instant::format_plain(&mut linebuf, if o.with_filename { &h.path } else { "" }, h.line, h.text.as_bytes(), h.context);
            w.write_all(&linebuf)?;
            return Ok(());
        }
        if o.with_filename {
            w.set_color(&path_color)?;
            write!(w, "{}", h.path)?;
            w.reset()?;
            write!(w, "{}", if h.context { "-" } else { ":" })?;
        }
        w.set_color(&line_color)?;
        write!(w, "{}", h.line)?;
        w.reset()?;
        write!(w, "{}", if h.context { "-" } else { ":" })?;
        if h.spans.is_empty() {
            writeln!(w, "{}", h.text)?;
        } else {
            let bytes = h.text.as_bytes();
            let mut pos = 0;
            for &(a, b) in &h.spans {
                let (a, b) = (a.min(bytes.len()), b.min(bytes.len()));
                if a < pos {
                    continue;
                }
                write!(w, "{}", String::from_utf8_lossy(&bytes[pos..a]))?;
                w.set_color(&match_color)?;
                write!(w, "{}", String::from_utf8_lossy(&bytes[a..b]))?;
                w.reset()?;
                pos = b;
            }
            writeln!(w, "{}", String::from_utf8_lossy(&bytes[pos..]))?;
        }
        Ok(())
    };
    let stats = {
        let mut sink = |h: instant::Hit| {
            if !h.context {
                found = true;
                if o.count_only || o.files_only {
                    // per-file record: `line` carries the count
                    *counts.entry(Arc::clone(&h.path)).or_insert(0) += h.line;
                    if seen_files.insert(Arc::clone(&h.path)) {
                        files.push(Arc::clone(&h.path));
                    }
                    return;
                }
                if bookkeeping {
                    *counts.entry(Arc::clone(&h.path)).or_insert(0) += 1;
                    if seen_files.insert(Arc::clone(&h.path)) {
                        files.push(Arc::clone(&h.path));
                    }
                }
            }
            if defer {
                collected.push(h);
            } else if let Err(e) = emit(&h, &mut w) {
                if e.downcast_ref::<std::io::Error>().map(|e| e.kind() == std::io::ErrorKind::BrokenPipe).unwrap_or(false) {
                    pipe_closed = true;
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        };
        if out.indexed {
            let (root, files) = indexed_candidates(&out.root, &o, out.update)?;
            instant::search_files(&root, files, &o, Arc::clone(&cancel), &mut sink)?
        } else {
            instant::search(&roots, &o, Arc::clone(&cancel), &mut sink)?
        }
    };
    warn_failures(&stats);
    if pipe_closed {
        return Ok(if found { 0 } else { 1 });
    }
    if out.count {
        for (p, c) in &counts {
            if out.json {
                writeln!(w, "{}", serde_json::json!({"type": "count", "schema": SCHEMA, "path": p, "count": c}))?;
            } else if o.with_filename {
                writeln!(w, "{p}:{c}")?;
            } else {
                writeln!(w, "{c}")?;
            }
        }
    } else if out.files_with_matches {
        files.sort();
        for p in &files {
            if out.json {
                writeln!(w, "{}", serde_json::json!({"type": "file", "schema": SCHEMA, "path": p}))?;
            } else {
                writeln!(w, "{p}")?;
            }
        }
    } else if out.symbols {
        grouped_by_symbol(&out.root, &mut collected, &mut w, out.json)?;
    } else if defer {
        collected.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
        for h in &collected {
            emit(h, &mut w)?;
        }
    }
    if out.stats || out.json {
        let rec = serde_json::json!({"type": "summary", "schema": SCHEMA, "files_searched": stats.files_searched, "files_matched": stats.files_matched,
            "matches": stats.matches, "bytes_searched": stats.bytes_searched, "errors": stats.errors, "elapsed_ms": stats.elapsed_ms, "cancelled": stats.cancelled});
        if out.json {
            writeln!(w, "{rec}")?;
        } else {
            eprintln!(
                "{} matches in {} of {} files ({:.1} MB) in {:.1} ms{}{}",
                stats.matches,
                stats.files_matched,
                stats.files_searched,
                stats.bytes_searched as f64 / 1e6,
                stats.elapsed_ms,
                if stats.errors > 0 { format!(", {} unreadable", stats.errors) } else { String::new() },
                if stats.cancelled { ", stopped early" } else { "" }
            );
        }
    }
    if let Err(e) = w.flush() {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(e.into());
        }
    }
    if stats.cancelled && !o.max_results.map(|m| stats.matches >= m).unwrap_or(false) {
        return Ok(130);
    }
    Ok(if missing_root(&roots, &stats) {
        2
    } else if found {
        0
    } else {
        1
    })
}

/// A path given to search does not exist or cannot be opened (grep's exit 2); files the walk
/// cannot read do not count.
fn missing_root(roots: &[PathBuf], stats: &instant::Stats) -> bool {
    stats.errors > 0 && roots.iter().any(|r| std::fs::metadata(r).is_err())
}

/// The first files a search could not read, or not to the end (PCRE2's match limit, I/O errors).
fn warn_failures(stats: &instant::Stats) {
    for f in &stats.failures {
        diagnostic("warning", f);
    }
    let more = stats.errors.saturating_sub(stats.failures.len() as u64);
    if more > 0 {
        diagnostic("warning", &format!("{more} more files could not be searched"));
    }
}

/// Plain unsorted output, written by the search threads (see `instant::PlainSink`).
fn grep_plain(roots: Vec<PathBuf>, o: Options, out: GrepOutput, cancel: Arc<AtomicBool>) -> Result<i32> {
    let sink = Arc::new(instant::PlainSink::new(Box::new(std::io::stdout())));
    let stats = if out.indexed {
        let (root, files) = indexed_candidates(&out.root, &o, out.update)?;
        instant::search_files_plain(&root, files, &o, Arc::clone(&cancel), Arc::clone(&sink))?
    } else {
        instant::search_plain(&roots, &o, Arc::clone(&cancel), Arc::clone(&sink))?
    };
    warn_failures(&stats);
    let found = stats.matches > 0;
    if sink.broken.load(Ordering::Relaxed) {
        return Ok(if found { 0 } else { 1 }); // the reader went away (`| head`): stop quietly
    }
    if let Err(e) = sink.flush() {
        if e.kind() != std::io::ErrorKind::BrokenPipe {
            return Err(e.into());
        }
    }
    if out.stats {
        eprintln!(
            "{} matches in {} of {} files ({:.1} MB) in {:.1} ms{}{}",
            stats.matches,
            stats.files_matched,
            stats.files_searched,
            stats.bytes_searched as f64 / 1e6,
            stats.elapsed_ms,
            if stats.errors > 0 { format!(", {} unreadable", stats.errors) } else { String::new() },
            if stats.cancelled { ", stopped early" } else { "" }
        );
    }
    if stats.cancelled && !o.max_results.map(|m| stats.matches >= m).unwrap_or(false) {
        return Ok(130);
    }
    Ok(if missing_root(&roots, &stats) {
        2
    } else if found {
        0
    } else {
        1
    })
}

/// Engine B enrichment of Engine A results: group matching lines by the enclosing symbol.
fn grouped_by_symbol(root: &Path, hits: &mut Vec<instant::Hit>, w: &mut termcolor::BufferedStandardStream, json: bool) -> Result<()> {
    let s = open_fresh(root)?;
    let mut lines: Vec<instant::Hit> = std::mem::take(hits).into_iter().filter(|h| !h.context).collect();
    // the index names a file by its path under the root, whatever path the search was given
    let mut named: std::collections::HashMap<std::sync::Arc<str>, std::sync::Arc<str>> = std::collections::HashMap::new();
    for h in &mut lines {
        let under = |p: &str| std::fs::canonicalize(p).ok().and_then(|c| c.strip_prefix(&s.ix.root).ok().map(|r| instant::slashed(r.to_string_lossy())));
        h.path = named.entry(h.path.clone()).or_insert_with_key(|p| under(p).map_or_else(|| p.clone(), Into::into)).clone();
    }
    lines.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    for g in crate::intel::group_by_symbol(&s.ix, lines)? {
        let name =
            g.symbol.as_ref().map(|r| format!("{}:{}-{}  [{}] {}", r.path, r.start, r.end, r.kind, if r.qualname.is_empty() { &r.name } else { &r.qualname }));
        if json {
            writeln!(
                w,
                "{}",
                serde_json::json!({"type": "symbol_group", "schema": SCHEMA, "symbol": name, "path": g.path,
                "matches": g.lines.iter().map(|h| serde_json::json!({"line": h.line, "text": h.text})).collect::<Vec<_>>()})
            )?;
        } else {
            writeln!(w, "{}", name.unwrap_or_else(|| format!("{}  (no symbol)", g.path)))?;
            for h in &g.lines {
                writeln!(w, "    {}: {}", h.line, h.text.trim_end())?;
            }
        }
    }
    Ok(())
}

/// Unified strategy: regex-looking queries go to Engine A; everything else to the ranked engine when
/// an index exists or the repository is small enough to index now; otherwise an identifier grep.
fn find_command(root: &Path, query: &str, k: usize, json: bool, no_color: bool) -> Result<i32> {
    let has_index = crate::index::exists(root);
    let small_repo = || crate::files::list_files(root).len() <= 3000;
    // a question mark ends a question, it does not make the words a regex
    let query = if query.contains(' ') { query.trim_end().strip_suffix('?').unwrap_or(query) } else { query };
    if looks_like_regex(query) {
        let o = Options { pattern: query.to_string(), mode: Mode::Regex, case: Case::Smart, ..Options::default() };
        let r = grep_command(
            vec![root.to_path_buf()],
            o,
            GrepOutput {
                files_with_matches: false,
                count: false,
                sort: true,
                json,
                stats: false,
                symbols: false,
                root: root.to_path_buf(),
                no_color,
                indexed: false,
                update: false,
            },
        );
        // nothing grepped, or not a valid regex: words with punctuation, for the ranked search
        match r {
            Ok(1) if has_index && !json => {}
            Err(_) if has_index => {}
            r => return r,
        }
    }
    if has_index || small_repo() {
        let mut s = open_fresh(root)?;
        let hits = s.search(query, k, true)?;
        print_hits(&hits, &[query.to_string()], false, json)?;
        return Ok(if hits.is_empty() { 1 } else { 0 });
    }
    eprintln!("[scrutate] no index for this large repository: identifier search (run `scrutate index` for ranked results)");
    let o = Options { pattern: query.to_string(), mode: Mode::Identifier, case: Case::Smart, ..Options::default() };
    grep_command(
        vec![root.to_path_buf()],
        o,
        GrepOutput {
            files_with_matches: false,
            count: false,
            sort: true,
            json,
            stats: false,
            symbols: false,
            root: root.to_path_buf(),
            no_color,
            indexed: false,
            update: false,
        },
    )
}

fn batch_command(root: &Path, file: &Path, features: bool, timing: bool) -> Result<i32> {
    let mut s = open_fresh(root)?;
    let f = std::fs::File::open(file)?;
    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    for line in std::io::BufReader::new(f).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: serde_json::Value = serde_json::from_str(&line)?;
        if let Some(tool) = req.get("op").and_then(|v| v.as_str()) {
            // any operation, named and given arguments as an MCP tool call is
            let rec = match Op::from_tool(tool, &req).and_then(|op| crate::ops::run(&mut s, &op)) {
                Ok(answer) => serde_json::json!({"op": tool, "result": answer.json()}),
                Err(e) => serde_json::json!({"op": tool, "error": e.to_string()}),
            };
            writeln!(out, "{rec}")?;
            continue;
        }
        let query = req.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let k = req.get("k").and_then(|v| v.as_u64()).unwrap_or(crate::ops::SEARCH_K as u64) as usize;
        let alt: Vec<String> =
            req.get("alt").and_then(|v| v.as_array()).map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect()).unwrap_or_default();
        let t = Instant::now();
        let mut rec = serde_json::Map::new();
        rec.insert("query".into(), serde_json::Value::String(query.clone()));
        if features {
            let c = s.candidates(&query, true)?;
            let scores = s.score(&c.x);
            let hits = s.hits(&c, &scores, k, false)?;
            rec.insert("hits".into(), hits_json(&hits));
            let cands: Vec<serde_json::Value> = c
                .rows
                .iter()
                .zip(&c.x)
                .zip(&scores)
                .map(|((r, x), sc)| serde_json::json!({"id": r.id, "path": r.path, "qualname": r.qualname, "score": sc, "x": x.to_vec()}))
                .collect();
            rec.insert("candidates".into(), serde_json::Value::Array(cands));
            rec.insert("terms".into(), serde_json::json!(c.q.terms));
            rec.insert("lits".into(), serde_json::json!(c.q.lits));
            rec.insert("lex_ids".into(), serde_json::json!(c.lex_ids));
            rec.insert("sem_ids".into(), serde_json::json!(c.sem_ids));
            rec.insert("exact_ids".into(), serde_json::json!(c.exact_ids));
        } else {
            let mut qs = vec![query.clone()];
            qs.extend(alt.iter().cloned());
            let hits = if alt.is_empty() { s.search(&query, k, false)? } else { s.search_many(&qs, k, false)? };
            rec.insert("hits".into(), hits_json(&hits));
        }
        if timing {
            rec.insert("ms".into(), serde_json::json!(t.elapsed().as_secs_f64() * 1000.0));
        }
        writeln!(out, "{}", serde_json::Value::Object(rec))?;
    }
    Ok(0)
}

/// Errors go to stderr as one line of text, or as one JSON record (`{"type": "error", ...}`) when
/// the command was given `--json` or `SCRUTATE_DIAGNOSTICS=json` is set, so callers that parse
/// the output can parse the failure too.
fn diagnostic(kind: &str, message: &str) {
    let json = std::env::var("SCRUTATE_DIAGNOSTICS").map(|v| v == "json").unwrap_or(false) || std::env::args_os().any(|a| a == "--json");
    if json {
        eprintln!("{}", serde_json::json!({"type": kind, "schema": SCHEMA, "message": message}));
    } else {
        eprintln!("scrutate: {}: {message}", kind.replace('_', " "));
    }
}

pub fn main_exit() {
    // a panic is a bug, but the user still gets a one-line diagnosis and a stable exit code
    // (2 = error) instead of a backtrace dump; RUST_BACKTRACE=1 shows the details
    std::panic::set_hook(Box::new(|info| {
        let msg = info.payload().downcast_ref::<&str>().map(|s| s.to_string()).or_else(|| info.payload().downcast_ref::<String>().cloned()).unwrap_or_default();
        diagnostic(
            "internal_error",
            &format!(
                "{msg} ({}). If this persists run `scrutate index --force`; please report it with RUST_BACKTRACE=1.",
                info.location().map(|l| l.to_string()).unwrap_or_default()
            ),
        );
    }));
    let code = match std::panic::catch_unwind(run) {
        Ok(Ok(c)) => c,
        Ok(Err(e)) => {
            diagnostic("error", &format!("{e:#}"));
            2
        }
        Err(_) => 2,
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    #[test]
    fn sizes_that_overflow_are_refused() {
        assert_eq!(super::parse_size("10M").unwrap(), 10 << 20);
        assert_eq!(super::parse_size("17179869183G").unwrap(), 17179869183 << 30);
        assert!(super::parse_size("17179869184G").is_err());
        assert!(super::parse_size("x").is_err());
    }
}
