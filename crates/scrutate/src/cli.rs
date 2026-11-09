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
use crate::text::query_words;

pub const SCHEMA: u32 = 1; // version of every JSON record this CLI emits

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
    /// never print color
    #[arg(long, global = true, env = "NO_COLOR")]
    pub no_color: bool,
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

#[derive(Subcommand)]
pub enum Cmd {
    /// Engine A: instant search of files (no index). PATTERN is a regex unless -F, --ident or --fuzzy
    #[command(alias = "g")]
    Grep {
        /// the pattern (omit it when patterns are given with -e)
        #[arg(required_unless_present = "regexp")]
        pattern: Option<String>,
        /// files or directories to search (default: current directory)
        paths: Vec<PathBuf>,
        /// a pattern to search for; repeatable, a line matching any of them matches
        #[arg(short = 'e', long = "regexp", value_name = "PATTERN")]
        regexp: Vec<String>,
        /// use the text index (Engine B, built by `scrutate index`) to read only the files that can
        /// match; results are those of the indexed snapshot of --root
        #[arg(long)]
        index: bool,
        /// treat PATTERN as a literal string
        #[arg(short = 'F', long)]
        fixed_strings: bool,
        /// identifier search: `refreshToken` also finds refresh_token, RefreshToken, REFRESH_TOKEN
        #[arg(long)]
        ident: bool,
        /// approximate search: literal within N edits (insertions, deletions, substitutions)
        #[arg(long, value_name = "N", num_args = 0..=1, default_missing_value = "1")]
        fuzzy: Option<u8>,
        /// PCRE2 syntax: look-around and backreferences (backtracking, stopped by PCRE2's match limit)
        #[arg(short = 'P', long)]
        pcre2: bool,
        /// also search inside zip and tar archives and compressed files (gz, bz2, xz, zst, lz4, br, Z)
        #[arg(short = 'z', long)]
        search_zip: bool,
        /// case-insensitive
        #[arg(short = 'i', long)]
        ignore_case: bool,
        /// case-insensitive unless the pattern has an uppercase letter
        #[arg(short = 'S', long, env = "SCRUTATE_GREP_SMART_CASE")]
        smart_case: bool,
        /// match whole words only
        #[arg(short = 'w', long)]
        word: bool,
        /// let the pattern span lines
        #[arg(short = 'U', long)]
        multiline: bool,
        /// select non-matching lines
        #[arg(short = 'v', long)]
        invert_match: bool,
        /// print only the paths of files with matches
        #[arg(short = 'l', long)]
        files_with_matches: bool,
        /// print only the number of matching lines per file
        #[arg(short = 'c', long)]
        count: bool,
        /// lines of context after each match
        #[arg(short = 'A', long, default_value_t = 0)]
        after: usize,
        /// lines of context before each match
        #[arg(short = 'B', long, default_value_t = 0)]
        before: usize,
        /// lines of context around each match
        #[arg(short = 'C', long)]
        context: Option<usize>,
        /// stop after N matching lines per file
        #[arg(short = 'm', long)]
        max_count: Option<u64>,
        /// stop everything after N matches in total
        #[arg(long)]
        max_results: Option<u64>,
        /// include/exclude files by glob (prefix ! to exclude); repeatable
        #[arg(short = 'g', long = "glob", env = "SCRUTATE_GREP_GLOBS", value_delimiter = ',')]
        globs: Vec<String>,
        /// only files of this type (see --type-list); prefix ! to exclude; repeatable
        #[arg(short = 't', long = "type", env = "SCRUTATE_GREP_TYPES", value_delimiter = ',')]
        types: Vec<String>,
        /// list the known file types
        #[arg(long)]
        type_list: bool,
        /// search hidden files and directories
        #[arg(long, env = "SCRUTATE_GREP_HIDDEN")]
        hidden: bool,
        /// ignore .gitignore / .ignore rules
        #[arg(long, env = "SCRUTATE_GREP_NO_IGNORE")]
        no_ignore: bool,
        /// follow symbolic links
        #[arg(short = 'L', long, env = "SCRUTATE_GREP_FOLLOW")]
        follow: bool,
        /// skip files larger than this (e.g. 10M)
        #[arg(long, value_name = "SIZE", env = "SCRUTATE_GREP_MAX_FILESIZE")]
        max_filesize: Option<String>,
        /// descend at most this many directory levels
        #[arg(long, env = "SCRUTATE_GREP_MAX_DEPTH")]
        max_depth: Option<usize>,
        /// number of search threads (default: cores, max 12)
        #[arg(short = 'j', long, default_value_t = 0, env = "SCRUTATE_THREADS")]
        threads: usize,
        /// sort results by path (deterministic, waits for the search to finish)
        #[arg(long)]
        sort: bool,
        /// one JSON object per line
        #[arg(long)]
        json: bool,
        /// print search statistics to stderr
        #[arg(long)]
        stats: bool,
        /// group matches by enclosing function/class using the index (Engine B), if one exists
        #[arg(long)]
        symbols: bool,
    },
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
        #[arg(short, default_value_t = 8)]
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
        #[arg(short, default_value_t = 8)]
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
    /// every reference to a name: definitions, callers and identifier matches grouped by enclosing symbol
    Refs {
        name: String,
        #[arg(long)]
        json: bool,
        #[arg(long, default_value_t = 200)]
        max_results: usize,
    },
    /// code similar to a symbol or location (`name`, `Class.method` or `path:line`)
    Similar {
        target: String,
        #[arg(short, default_value_t = 10)]
        k: usize,
        #[arg(long)]
        json: bool,
    },
    /// duplicate and near-duplicate functions across the repository
    Dupes {
        /// ignore functions with fewer body tokens than this
        #[arg(long, default_value_t = 30)]
        min_tokens: usize,
        /// near-duplicate threshold (Jaccard similarity of token 3-grams)
        #[arg(long, default_value_t = 0.75)]
        threshold: f32,
        #[arg(long, default_value_t = 50)]
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
        #[arg(long, default_value_t = 1000)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    /// index statistics
    Stats,
    /// run the MCP server on stdio
    Serve {
        /// at most one directory scan per this many seconds
        #[arg(long, default_value_t = 1.0, env = "SCRUTATE_REFRESH_INTERVAL")]
        refresh_interval: f64,
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
    Ok(num.parse::<u64>().with_context(|| format!("bad size {s:?}"))? * mult)
}

fn open_fresh(root: &Path) -> Result<crate::search::Searcher> {
    let (s, st) = crate::open_updated(root, false)?;
    if st.files_changed > 0 || st.files_removed > 0 {
        eprintln!("[scrutate] indexed {} changed files ({} chunks, {:.1}s)", st.files_changed, st.chunks, st.seconds);
    }
    Ok(s)
}

fn color_choice(no_color: bool) -> ColorChoice {
    if no_color || !std::io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some() {
        ColorChoice::Never
    } else {
        ColorChoice::Auto
    }
}

/// True when the query looks like a regular expression rather than words or an identifier.
pub fn looks_like_regex(q: &str) -> bool {
    q.chars().any(|c| matches!(c, '*' | '+' | '?' | '[' | ']' | '(' | ')' | '|' | '^' | '$' | '\\' | '{' | '}'))
}

pub fn run() -> Result<i32> {
    // configuration file -> environment -> clap (command line still wins)
    let args: Vec<String> = std::env::args().collect();
    let cfg_root = crate::config::root_from_args(&args);
    let cfg_path = crate::config::locate(&cfg_root);
    if let Some(p) = &cfg_path {
        let cfg = crate::config::load(p)?;
        crate::config::apply_as_env(&cfg);
    }
    let cli = Cli::parse();
    let no_color = cli.no_color;
    match cli.cmd {
        Cmd::Grep {
            pattern,
            mut paths,
            regexp,
            index,
            fixed_strings,
            pcre2,
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
        } => {
            if type_list {
                for (name, globs) in instant::type_list() {
                    println!("{name}: {}", globs.join(", "));
                }
                return Ok(0);
            }
            if pcre2 && (ident || fuzzy.is_some()) {
                anyhow::bail!("-P/--pcre2 cannot be combined with --ident or --fuzzy");
            }
            let mut mode = match (fixed_strings, ident, fuzzy) {
                (_, _, Some(n)) => Mode::Fuzzy { max_edits: n },
                (_, true, _) => Mode::Identifier,
                _ if pcre2 => Mode::Pcre2,
                (true, _, _) => Mode::Literal,
                _ => Mode::Regex,
            };
            let many = regexp.len() > 1;
            let pattern = if regexp.is_empty() {
                pattern.unwrap_or_default()
            } else {
                // with -e, the first positional argument is a path
                if let Some(p) = pattern {
                    paths.insert(0, PathBuf::from(p));
                }
                match regexp.len() {
                    1 => regexp.into_iter().next().unwrap_or_default(),
                    _ => {
                        if matches!(mode, Mode::Fuzzy { .. }) {
                            anyhow::bail!("--fuzzy takes one pattern");
                        }
                        let each = mode;
                        let part = |p: &str| match each {
                            Mode::Literal => regex::escape(p),
                            Mode::Pcre2 if fixed_strings => regex::escape(p),
                            Mode::Identifier => instant::identifier_regex(p),
                            _ => p.to_string(),
                        };
                        if mode != Mode::Pcre2 {
                            mode = Mode::Regex;
                        }
                        regexp.iter().map(|p| format!("(?:{})", part(p))).collect::<Vec<_>>().join("|")
                    }
                }
            };
            let pattern = if mode == Mode::Pcre2 && fixed_strings && !many { regex::escape(&pattern) } else { pattern };
            if index && search_zip {
                anyhow::bail!("--index does not cover archive members; search with -z and without --index");
            }
            if index {
                if hidden || no_ignore || follow || max_depth.is_some() || max_filesize.is_some() {
                    anyhow::bail!(
                        "--index searches the files of the default walk; drop --hidden/--no-ignore/-L/--max-depth/--max-filesize or search without --index"
                    );
                }
                if !paths.is_empty() {
                    anyhow::bail!("--index searches the indexed repository (--root); narrow it with -g/-t instead of paths");
                }
            }
            let o = Options {
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
                ..Options::default()
            };
            let roots: Vec<PathBuf> = if paths.is_empty() { vec![PathBuf::from(".")] } else { paths };
            grep_command(roots, o, GrepOutput { files_with_matches, count, sort, json, stats, symbols, root: cli.root, no_color, indexed: index })
        }
        Cmd::Index { force, text, no_text } => {
            let progress = std::io::stderr().is_terminal();
            if !text {
                let mut s = crate::open(&cli.root)?;
                s.ix.progress = true;
                let st = match s.ix.update(force) {
                    Ok(st) => st,
                    Err(e) if crate::index::error_is_corruption(&e) => {
                        eprintln!("[scrutate] index database corrupt ({e:#}); rebuilding");
                        drop(s);
                        crate::index::wipe_index(&cli.root);
                        s = crate::open(&cli.root)?;
                        s.ix.progress = true;
                        s.ix.update(true)?
                    }
                    Err(e) => return Err(e),
                };
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
        Cmd::Symbol { name, json } => {
            let s = open_fresh(&cli.root)?;
            let hits = s.find_symbol(&name, 20)?;
            print_hits(&hits, &[], false, json)?;
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        Cmd::Callers { name, json } => {
            let s = open_fresh(&cli.root)?;
            let hits = s.callers(&name, 30)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&hits_json(&hits))?)
            } else {
                println!("{}", fmt_hits(&hits, &[], false, false))
            }
            Ok(if hits.is_empty() { 1 } else { 0 })
        }
        Cmd::Callees { name, json } => {
            let s = open_fresh(&cli.root)?;
            let (res, ext) = s.callees(&name, 40)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({"resolved": hits_json(&res), "external": ext}))?);
            } else {
                println!("{}", fmt_hits(&res, &[], false, false));
                println!("external: {}", ext.iter().take(50).cloned().collect::<Vec<_>>().join(", "));
            }
            Ok(if res.is_empty() && ext.is_empty() { 1 } else { 0 })
        }
        Cmd::Deps { file, json } => {
            let s = open_fresh(&cli.root)?;
            let deps = crate::intel::deps(&s.ix, &file)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&deps)?);
            } else {
                for d in &deps {
                    println!("{}:{}  {}{}", file, d.line, d.module, d.resolved.as_ref().map(|r| format!("  -> {r}")).unwrap_or_default());
                }
            }
            Ok(if deps.is_empty() { 1 } else { 0 })
        }
        Cmd::Rdeps { target, json } => {
            let s = open_fresh(&cli.root)?;
            let rows = crate::intel::rdeps(&s.ix, &target)?;
            if json {
                let v: Vec<serde_json::Value> = rows.iter().map(|(p, l, m)| serde_json::json!({"path": p, "line": l, "module": m})).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for (p, l, m) in &rows {
                    println!("{p}:{l}  {m}");
                }
            }
            Ok(if rows.is_empty() { 1 } else { 0 })
        }
        Cmd::Outline { file, json } => {
            let s = open_fresh(&cli.root)?;
            let rows = crate::intel::outline(&s.ix, &file)?;
            if json {
                let v: Vec<serde_json::Value> =
                    rows.iter().map(|r| serde_json::json!({"kind": r.kind, "name": r.name, "qualname": r.qualname, "start": r.start, "end": r.end})).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for r in &rows {
                    let depth = r.qualname.matches('.').count();
                    println!("{:>5}-{:<5} {}{:<9} {}", r.start, r.end, "  ".repeat(depth), r.kind, r.qualname);
                }
            }
            Ok(if rows.is_empty() { 1 } else { 0 })
        }
        Cmd::Refs { name, json, max_results } => refs_command(&cli.root, &name, max_results, json),
        Cmd::Similar { target, k, json } => {
            let mut s = open_fresh(&cli.root)?;
            let out = crate::intel::similar(&mut s, &target, k)?;
            if json {
                let v: Vec<serde_json::Value> = out.iter().map(|(h, cos, jac)| serde_json::json!({"path": h.path, "start": h.start, "end": h.end, "kind": h.kind, "qualname": h.qualname, "cosine": cos, "jaccard": jac})).collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for (h, cos, jac) in &out {
                    println!("{:.3} cos  {:.2} jaccard  {}  [{}] {}", cos, jac, h.loc(), h.kind, h.qualname);
                }
            }
            Ok(if out.is_empty() { 1 } else { 0 })
        }
        Cmd::Dupes { min_tokens, threshold, limit, json } => {
            let s = open_fresh(&cli.root)?;
            let groups = crate::intel::dupes(&s.ix, min_tokens, threshold, limit)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&groups)?);
            } else {
                for g in &groups {
                    println!("{} ({} tokens, similarity {:.2})", g.kind, g.tokens, g.similarity);
                    for (p, q, a, b) in &g.members {
                        println!("    {p}:{a}-{b}  {q}");
                    }
                }
            }
            Ok(if groups.is_empty() { 1 } else { 0 })
        }
        Cmd::Tsquery { lang, query, paths, limit, json } => {
            // streamed: one JSON object or one text line per capture, in path order
            let stdout = std::io::stdout();
            let mut w = std::io::BufWriter::new(stdout.lock());
            let mut err: Option<std::io::Error> = None;
            let n = crate::intel::tsquery_each(&lang, &query, &paths, limit, &mut |c| {
                if err.is_some() {
                    return;
                }
                let r = if json {
                    serde_json::to_writer(&mut w, &c)
                        .map_err(|e| std::io::Error::new(e.io_error_kind().unwrap_or(std::io::ErrorKind::Other), e))
                        .and_then(|_| w.write_all(b"\n"))
                } else {
                    writeln!(w, "{}:{}:{}  @{} ({})  {}", c.path, c.line, c.column, c.capture, c.kind, c.text)
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
        Cmd::Serve { refresh_interval } => {
            let s = crate::open(&cli.root)?;
            crate::mcp::serve(Arc::new(Mutex::new(s)), refresh_interval, crate::VERSION)?;
            Ok(0)
        }
        Cmd::Lsp => crate::lsp::serve(&cli.root),
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

/// `refs NAME`: definitions, callers and every identifier occurrence grouped by the enclosing symbol.
fn refs_command(root: &Path, name: &str, max_results: usize, json: bool) -> Result<i32> {
    let s = open_fresh(root)?;
    if json {
        let r = crate::intel::references(&s, name, max_results as u64)?;
        let v = serde_json::json!({
            "schema": SCHEMA,
            "name": name,
            "definitions": r.definitions.iter().map(|c| serde_json::json!({"path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "qualname": c.qualname})).collect::<Vec<_>>(),
            "callers": hits_json(&r.callers),
            "references": r.groups.iter().map(|g| serde_json::json!({
                "symbol": g.symbol.as_ref().map(|c| serde_json::json!({"path": c.path, "start": c.start, "end": c.end, "kind": c.kind, "qualname": c.qualname})),
                "path": g.path,
                "lines": g.lines.iter().map(|h| serde_json::json!({"line": h.line, "column": h.column, "text": h.text})).collect::<Vec<_>>()
            })).collect::<Vec<_>>(),
            "truncated": r.truncated,
        });
        println!("{}", serde_json::to_string_pretty(&v)?);
        Ok(if r.is_empty() { 1 } else { 0 })
    } else {
        let r = crate::intel::references(&s, name, max_results as u64)?;
        print!("{}", crate::intel::references_text(&s, name, max_results as u64)?);
        Ok(if r.is_empty() { 1 } else { 0 })
    }
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
}

fn grep_command(roots: Vec<PathBuf>, o: Options, out: GrepOutput) -> Result<i32> {
    let cancel = Arc::new(AtomicBool::new(false));
    interrupt::install(Arc::clone(&cancel));
    let colour = color_choice(out.no_color) != ColorChoice::Never;
    if !out.json && !colour && !out.sort && !out.symbols && !o.count_only && !o.files_only && o.max_count.is_none() {
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
    let mut emit = |h: &instant::Hit, w: &mut termcolor::BufferedStandardStream| -> Result<()> {
        if out.json {
            let rec = serde_json::json!({"type": if h.context { "context" } else { "match" }, "schema": SCHEMA, "path": h.path, "line": h.line,
                "column": h.column, "text": h.text, "spans": h.spans.iter().map(|(a, b)| serde_json::json!({"start": a, "end": b})).collect::<Vec<_>>()});
            writeln!(w, "{rec}")?;
            return Ok(());
        }
        if !colour {
            // plain output: one assembled line, one write (the common piped case)
            linebuf.clear();
            instant::format_plain(&mut linebuf, &h.path, h.line, h.text.as_bytes(), h.context);
            w.write_all(&linebuf)?;
            return Ok(());
        }
        w.set_color(&path_color)?;
        write!(w, "{}", h.path)?;
        w.reset()?;
        write!(w, "{}", if h.context { "-" } else { ":" })?;
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
            let root = std::fs::canonicalize(&out.root).unwrap_or_else(|_| out.root.clone());
            let ti = crate::textindex::TextIndex::open(&root)?
                .ok_or_else(|| anyhow::anyhow!("no text index in {}; build it with `scrutate index --text`", root.display()))?;
            let files = ti.candidates(&crate::textindex::query_for(&o)?, o.multiline)?;
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
            } else {
                writeln!(w, "{p}:{c}")?;
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
    Ok(if found { 0 } else { 1 })
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
        let root = std::fs::canonicalize(&out.root).unwrap_or_else(|_| out.root.clone());
        let ti = crate::textindex::TextIndex::open(&root)?
            .ok_or_else(|| anyhow::anyhow!("no text index in {}; build it with `scrutate index --text`", root.display()))?;
        let files = ti.candidates(&crate::textindex::query_for(&o)?, o.multiline)?;
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
    Ok(if found { 0 } else { 1 })
}

/// Engine B enrichment of Engine A results: group matching lines by the enclosing symbol.
fn grouped_by_symbol(root: &Path, hits: &mut [instant::Hit], w: &mut termcolor::BufferedStandardStream, json: bool) -> Result<()> {
    let s = open_fresh(root)?;
    hits.sort_by(|a, b| a.path.cmp(&b.path).then(a.line.cmp(&b.line)));
    let mut groups: Vec<(Option<crate::index::ChunkRow>, Vec<&instant::Hit>)> = Vec::new();
    for h in hits.iter().filter(|h| !h.context) {
        let row = s.ix.chunk_at(&h.path, h.line as i64)?;
        match groups.last_mut() {
            Some((Some(r), v)) if row.as_ref().map(|x| x.id) == Some(r.id) => v.push(h),
            Some((None, v)) if row.is_none() && v.last().map(|x| x.path == h.path).unwrap_or(false) => v.push(h),
            _ => groups.push((row, vec![h])),
        }
    }
    for (row, lines) in groups {
        let name =
            row.as_ref().map(|r| format!("{}:{}-{}  [{}] {}", r.path, r.start, r.end, r.kind, if r.qualname.is_empty() { &r.name } else { &r.qualname }));
        if json {
            writeln!(
                w,
                "{}",
                serde_json::json!({"type": "symbol_group", "schema": SCHEMA, "symbol": name, "path": lines[0].path,
                "matches": lines.iter().map(|h| serde_json::json!({"line": h.line, "text": h.text})).collect::<Vec<_>>()})
            )?;
        } else {
            writeln!(w, "{}", name.unwrap_or_else(|| format!("{}  (no symbol)", lines[0].path)))?;
            for h in lines {
                writeln!(w, "    {}: {}", h.line, h.text.trim_end())?;
            }
        }
    }
    Ok(())
}

/// Unified strategy: regex-looking queries go to Engine A; everything else to the ranked engine when
/// an index exists or the repository is small enough to index now; otherwise an identifier grep.
fn find_command(root: &Path, query: &str, k: usize, json: bool, no_color: bool) -> Result<i32> {
    let has_index = root.join(".scrutate/index.db").exists();
    let small_repo = || crate::index::list_files(root).len() <= 3000;
    if looks_like_regex(query) {
        let o = Options { pattern: query.to_string(), mode: Mode::Regex, case: Case::Smart, ..Options::default() };
        return grep_command(
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
            },
        );
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
        let query = req.get("query").and_then(|v| v.as_str()).unwrap_or("").to_string();
        let k = req.get("k").and_then(|v| v.as_u64()).unwrap_or(10) as usize;
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
    let json = std::env::var("SCRUTATE_DIAGNOSTICS").map(|v| v == "json").unwrap_or(false) || std::env::args().any(|a| a == "--json");
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
