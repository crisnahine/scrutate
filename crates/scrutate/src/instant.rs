//! Engine A: universal instant search. No index, no parsers, no background process.
//!
//! Files are walked in parallel with the same ignore rules as ripgrep (the `ignore` crate: .gitignore,
//! .ignore, hidden files, binary detection), searched line by line with ripgrep's `grep-searcher` and,
//! for literal / regex / identifier queries, Rust's linear-time regex engine (no catastrophic
//! backtracking; compiled size is capped). Two capabilities go beyond grep-class tools:
//! * identifier mode: one spelling finds every casing style (`refreshToken` = `refresh_token` = `REFRESH_TOKEN`);
//! * fuzzy mode: matches within an edit distance (Myers' bit-parallel algorithm, O(n) per file).
//!
//! Results stream to a sink as they are found; a cancellation flag stops the walk between files.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{anyhow, bail, Context, Result};
use grep_matcher::{LineTerminator, Match as MatchSpan, Matcher, NoCaptures, NoError};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use ignore::{overrides::OverrideBuilder, WalkBuilder, WalkState};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Regex,
    Literal,
    /// `refreshToken` matches refresh_token / RefreshToken / REFRESH-TOKEN
    Identifier,
    /// literal within `max_edits` insertions, deletions or substitutions (pattern up to 64 bytes)
    Fuzzy {
        max_edits: u8,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Sensitive,
    Insensitive,
    /// insensitive unless the pattern contains an uppercase letter
    Smart,
}

#[derive(Clone, Debug)]
pub struct Options {
    pub pattern: String,
    pub mode: Mode,
    pub case: Case,
    pub word: bool,
    pub multiline: bool,
    pub invert: bool,
    pub hidden: bool,
    pub no_ignore: bool,
    pub follow_links: bool,
    pub globs: Vec<String>,
    /// language ids / type names from the registry (`py`, `rust`, ...); negative with a leading `!`
    pub types: Vec<String>,
    pub max_filesize: Option<u64>,
    pub max_depth: Option<usize>,
    pub max_count: Option<u64>,
    pub max_results: Option<u64>,
    pub before: usize,
    pub after: usize,
    pub threads: usize,
    /// only report per-file counts / matching files: no line text leaves the worker threads
    pub count_only: bool,
    pub files_only: bool,
    /// collect bytes searched (one stat per file; only when statistics are wanted)
    pub want_bytes: bool,
    /// compute the byte ranges of the matches inside each line (colour / JSON output); off, the
    /// line is reported without a second pass of the matcher over it
    pub want_spans: bool,
    /// regex size limits (resource exhaustion protection)
    pub regex_size_limit: usize,
    pub dfa_size_limit: usize,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            pattern: String::new(),
            mode: Mode::Regex,
            case: Case::Sensitive,
            word: false,
            multiline: false,
            invert: false,
            hidden: false,
            no_ignore: false,
            follow_links: false,
            globs: Vec::new(),
            types: Vec::new(),
            max_filesize: None,
            max_depth: None,
            max_count: None,
            max_results: None,
            before: 0,
            after: 0,
            threads: 0,
            count_only: false,
            files_only: false,
            want_bytes: false,
            want_spans: false,
            regex_size_limit: 20 * (1 << 20),
            dfa_size_limit: 10 * (1 << 20),
        }
    }
}

/// One result record. `line` is 1-based; `column` is the 1-based byte offset of the first match in
/// the line (0 when not applicable, e.g. inverted matches).
#[derive(Clone, Debug, serde::Serialize)]
pub struct Hit {
    /// shared by every hit of one file (a refcount per line instead of a string copy)
    pub path: Arc<str>,
    pub line: u64,
    pub column: u64,
    pub text: String,
    /// byte ranges of the matches within `text`
    pub spans: Vec<(usize, usize)>,
    /// true for context lines (-A/-B), false for matching lines
    pub context: bool,
}

#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct Stats {
    pub files_searched: u64,
    pub files_matched: u64,
    pub matches: u64,
    pub bytes_searched: u64,
    pub errors: u64,
    pub elapsed_ms: f64,
    pub cancelled: bool,
}

// ------------------------------------------------------------------------- fuzzy matcher
/// Approximate matching of a literal (Myers 1999, bit-parallel). Lines are never spanned.
/// A pigeonhole prefilter (one of k+1 pieces of the pattern must occur verbatim) finds candidate
/// lines with a multi-substring scan, so only those lines pay for the edit-distance computation.
#[derive(Clone)]
pub struct FuzzyMatcher {
    peq: [u128; 256],
    m: usize,
    k: usize,
    pattern: Vec<u8>,
    fold: bool,
    prefilter: Option<aho_corasick::AhoCorasick>,
}

impl FuzzyMatcher {
    pub fn new(pattern: &str, max_edits: u8, fold_case: bool) -> Result<FuzzyMatcher> {
        let pat: Vec<u8> = if fold_case { pattern.to_ascii_lowercase().into_bytes() } else { pattern.as_bytes().to_vec() };
        if pat.is_empty() || pat.len() > 128 {
            bail!("fuzzy patterns must be 1..=128 bytes");
        }
        if max_edits as usize >= pat.len() {
            bail!("the number of edits ({max_edits}) must be smaller than the pattern length ({})", pat.len());
        }
        let mut peq = [0u128; 256];
        for (j, &c) in pat.iter().enumerate() {
            peq[c as usize] |= 1u128 << j;
            if fold_case && c.is_ascii_lowercase() {
                peq[c.to_ascii_uppercase() as usize] |= 1u128 << j;
            }
        }
        peq[b'\n' as usize] = 0;
        // k edits can touch at most k of k+1 disjoint pieces: some piece survives verbatim
        let pieces = max_edits as usize + 1;
        let piece_len = pat.len() / pieces;
        let prefilter = if piece_len >= 2 {
            let pats: Vec<&[u8]> = (0..pieces).map(|i| &pat[i * piece_len..if i == pieces - 1 { pat.len() } else { (i + 1) * piece_len }]).collect();
            aho_corasick::AhoCorasick::builder().ascii_case_insensitive(fold_case).build(pats).ok()
        } else {
            None
        };
        Ok(FuzzyMatcher { peq, m: pat.len(), k: max_edits as usize, pattern: pat, fold: fold_case, prefilter })
    }

    /// Byte range of the line containing `pos` within `hay[at..]`.
    fn line_bounds(hay: &[u8], at: usize, pos: usize) -> (usize, usize) {
        let start = hay[at..pos].iter().rposition(|&b| b == b'\n').map(|i| at + i + 1).unwrap_or(at);
        let end = hay[pos..].iter().position(|&b| b == b'\n').map(|i| pos + i).unwrap_or(hay.len());
        (start, end)
    }

    /// First match end in `hay[at..]`: candidate lines from the prefilter, verified with Myers.
    fn first_end_filtered(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        let Some(ac) = &self.prefilter else {
            return self.first_end(hay, at);
        };
        let mut from = at;
        while from < hay.len() {
            let found = ac.find(&hay[from..])?;
            let (ls, le) = Self::line_bounds(hay, from, from + found.start());
            if let Some((end, d)) = self.first_end(&hay[..le], ls) {
                return Some((end, d));
            }
            from = le + 1;
        }
        None
    }

    /// End positions (exclusive) in `hay[at..]` where a match with <= k edits ends, with its distance.
    fn first_end(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        let m = self.m;
        let top = 1u128 << (m - 1);
        let mask = if m == 128 { u128::MAX } else { (1u128 << m) - 1 };
        let mut pv = mask;
        let mut mv = 0u128;
        let mut score = m;
        for (i, &c) in hay[at..].iter().enumerate() {
            if c == b'\n' {
                pv = mask;
                mv = 0;
                score = m;
                continue;
            }
            let eq = self.peq[c as usize];
            let xv = eq | mv;
            let xh = (((eq & pv).wrapping_add(pv)) ^ pv) | eq;
            let mut ph = mv | !(xh | pv);
            let mut mh = pv & xh;
            if ph & top != 0 {
                score += 1;
            } else if mh & top != 0 {
                score -= 1;
            }
            ph = (ph << 1) & mask;
            mh = (mh << 1) & mask;
            pv = (mh | !(xv | ph)) & mask;
            mv = ph & xv;
            if score <= self.k {
                return Some((at + i + 1, score));
            }
        }
        None
    }

    /// Start of the match that ends at `end` with distance `d`: run the algorithm on the reversed
    /// pattern over the reversed window and take the farthest position still within `d` edits (the
    /// longest match, which highlights the whole misspelt word rather than a fragment of it).
    fn start_for(&self, hay: &[u8], end: usize, d: usize) -> usize {
        let m = self.m;
        let lo = end.saturating_sub(m + self.k);
        let window: Vec<u8> = hay[lo..end].iter().rev().copied().collect();
        let mut peq = [0u128; 256];
        for (j, &c) in self.pattern.iter().rev().enumerate() {
            peq[c as usize] |= 1u128 << j;
            if self.fold && c.is_ascii_lowercase() {
                peq[c.to_ascii_uppercase() as usize] |= 1u128 << j;
            }
        }
        let top = 1u128 << (m - 1);
        let mask = if m == 128 { u128::MAX } else { (1u128 << m) - 1 };
        let (mut pv, mut mv, mut score) = (mask, 0u128, m);
        let mut best = None;
        for (i, &c) in window.iter().enumerate() {
            if c == b'\n' {
                break;
            }
            let eq = peq[c as usize];
            let xv = eq | mv;
            let xh = (((eq & pv).wrapping_add(pv)) ^ pv) | eq;
            let mut ph = mv | !(xh | pv);
            let mut mh = pv & xh;
            if ph & top != 0 {
                score += 1;
            } else if mh & top != 0 {
                score -= 1;
            }
            ph = (ph << 1) & mask;
            mh = (mh << 1) & mask;
            pv = (mh | !(xv | ph)) & mask;
            mv = ph & xv;
            if score <= d {
                best = Some(end - (i + 1));
            }
        }
        best.unwrap_or_else(|| end.saturating_sub(m))
    }
}

impl Matcher for FuzzyMatcher {
    type Captures = NoCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<MatchSpan>, NoError> {
        match self.first_end_filtered(haystack, at) {
            Some((end, d)) => {
                let start = self.start_for(haystack, end, d);
                Ok(Some(MatchSpan::new(start.max(at), end)))
            }
            None => Ok(None),
        }
    }

    fn new_captures(&self) -> Result<NoCaptures, NoError> {
        Ok(NoCaptures::new())
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        Some(LineTerminator::default()) // matches never span a line (see first_end)
    }
}

// ------------------------------------------------------------------------- matcher selection
#[derive(Clone)]
pub enum AnyMatcher {
    Regex(grep_regex::RegexMatcher),
    Fuzzy(Box<FuzzyMatcher>),
}

/// The parts of an identifier in any casing style: `XMLHttpRequest2` -> xml, http, request, 2;
/// `make_widget_3` -> make, widget, 3. Boundaries: separators (`_`, `-`, `.`, `:`), lower->upper,
/// letter<->digit, and the last capital of an acronym (`XMLHttp` -> `XML`, `Http`).
pub fn ident_parts(ident: &str) -> Vec<String> {
    let chars: Vec<char> = ident.chars().collect();
    let mut parts: Vec<String> = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(prev) = cur.chars().last() {
            let boundary = (prev.is_lowercase() && c.is_uppercase())
                || (prev.is_ascii_digit() != c.is_ascii_digit())
                || (prev.is_uppercase() && c.is_uppercase() && chars.get(i + 1).map(|n| n.is_lowercase()).unwrap_or(false));
            if boundary {
                parts.push(std::mem::take(&mut cur));
            }
        }
        cur.push(c);
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}

/// Regex for an identifier in any casing style: `refreshToken` -> `(?i)\brefresh[_\-]?token\b`,
/// matching `refresh_token`, `RefreshToken`, `REFRESH_TOKEN` and `refresh-token`, but not
/// `refresh_tokens` or `refreshTokenOnce` (the whole identifier must match).
pub fn identifier_regex(ident: &str) -> String {
    let parts = ident_parts(ident);
    if parts.is_empty() {
        return regex::escape(ident);
    }
    let body = parts.iter().map(|p| regex::escape(p)).collect::<Vec<_>>().join("[_\\-]?");
    format!("(?i)\\b{body}\\b")
}

pub fn build_matcher(o: &Options) -> Result<AnyMatcher> {
    let insensitive = match o.case {
        Case::Sensitive => false,
        Case::Insensitive => true,
        Case::Smart => !o.pattern.chars().any(|c| c.is_uppercase()),
    };
    match o.mode {
        Mode::Fuzzy { max_edits } => Ok(AnyMatcher::Fuzzy(Box::new(FuzzyMatcher::new(&o.pattern, max_edits, insensitive)?))),
        _ => {
            let pat = match o.mode {
                Mode::Literal => regex::escape(&o.pattern),
                Mode::Identifier => identifier_regex(&o.pattern),
                _ => o.pattern.clone(),
            };
            let mut b = RegexMatcherBuilder::new();
            b.case_insensitive(insensitive && o.mode != Mode::Identifier)
                .word(o.word)
                .multi_line(o.multiline)
                .dot_matches_new_line(false)
                .size_limit(o.regex_size_limit)
                .dfa_size_limit(o.dfa_size_limit)
                .line_terminator(if o.multiline { None } else { Some(b'\n') });
            let m = b.build(&pat).with_context(|| format!("invalid pattern {:?}", o.pattern))?;
            Ok(AnyMatcher::Regex(m))
        }
    }
}

// ------------------------------------------------------------------------- file types
/// Named groups of extensions for `--type`, from the language registry plus common aliases.
pub fn type_globs(name: &str) -> Option<Vec<String>> {
    let id = match name {
        "py" => "python",
        "js" => "javascript",
        "ts" => "typescript",
        "rb" => "ruby",
        "rs" => "rust",
        "cs" => "csharp",
        "kt" => "kotlin",
        "hs" => "haskell",
        "ex" => "elixir",
        "sh" | "shell" => "bash",
        "md" => "markdown",
        "yml" => "yaml",
        "tf" | "terraform" => "hcl",
        other => other,
    };
    let spec = crate::langs::by_id(id)?;
    let mut globs: Vec<String> = spec.exts.iter().map(|e| format!("*{e}")).collect();
    globs.extend(spec.files.iter().map(|f| f.to_string()));
    Some(globs)
}

pub fn type_list() -> Vec<(String, Vec<String>)> {
    crate::langs::LANGS.iter().map(|s| (s.id.to_string(), type_globs(s.id).unwrap_or_default())).collect()
}

// ------------------------------------------------------------------------- the search
struct Collector<'a> {
    path: Arc<str>,
    tx: &'a mpsc::Sender<Vec<Hit>>,
    /// hits of the current file, sent in batches (one channel operation per file or 512 lines)
    batch: Vec<Hit>,
    want_spans: bool,
    stats_matches: &'a AtomicU64,
    max_results: Option<u64>,
    matched: bool,
    cancel: &'a AtomicBool,
    matcher: &'a AnyMatcher,
    invert: bool,
    count_only: bool,
    files_only: bool,
    file_count: u64,
}

fn spans_in(matcher: &AnyMatcher, line: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    let find = |at: usize| -> Option<MatchSpan> {
        match matcher {
            AnyMatcher::Regex(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::Fuzzy(m) => m.find_at(line, at).ok().flatten(),
        }
    };
    while at <= line.len() {
        match find(at) {
            Some(m) => {
                out.push((m.start(), m.end()));
                at = if m.end() > m.start() { m.end() } else { m.end() + 1 };
                if out.len() >= 64 {
                    break;
                }
            }
            None => break,
        }
    }
    out
}

impl<'a> Sink for Collector<'a> {
    type Error = std::io::Error;

    fn matched(&mut self, _s: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, std::io::Error> {
        if self.cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if self.files_only || self.count_only {
            self.matched = true;
            self.file_count += 1;
            self.stats_matches.fetch_add(1, Ordering::Relaxed);
            return Ok(!self.files_only); // one match is enough to list the file
        }
        let bytes = mat.bytes();
        let text = String::from_utf8_lossy(bytes.strip_suffix(b"\n").map(|b| b.strip_suffix(b"\r").unwrap_or(b)).unwrap_or(bytes)).into_owned();
        let spans = if self.invert || !self.want_spans { Vec::new() } else { spans_in(self.matcher, bytes) };
        let column = spans.first().map(|s| s.0 as u64 + 1).unwrap_or(0);
        self.matched = true;
        let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
        self.push(Hit { path: Arc::clone(&self.path), line: mat.line_number().unwrap_or(0), column, text, spans, context: false });
        if let Some(max) = self.max_results {
            if n >= max {
                self.cancel.store(true, Ordering::Relaxed);
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn context(&mut self, _s: &Searcher, ctx: &SinkContext<'_>) -> Result<bool, std::io::Error> {
        let bytes = ctx.bytes();
        let text = String::from_utf8_lossy(bytes.strip_suffix(b"\n").unwrap_or(bytes)).into_owned();
        self.push(Hit { path: Arc::clone(&self.path), line: ctx.line_number().unwrap_or(0), column: 0, text, spans: Vec::new(), context: true });
        Ok(true)
    }

    fn finish(&mut self, _s: &Searcher, _f: &grep_searcher::SinkFinish) -> Result<(), std::io::Error> {
        self.flush();
        Ok(())
    }
}

impl Collector<'_> {
    fn push(&mut self, h: Hit) {
        self.batch.push(h);
        if self.batch.len() >= 512 {
            self.flush();
        }
    }
    fn flush(&mut self) {
        if !self.batch.is_empty() {
            let _ = self.tx.send(std::mem::take(&mut self.batch));
        }
    }
}

fn build_walker(roots: &[PathBuf], o: &Options) -> Result<ignore::WalkParallel> {
    let mut wb = WalkBuilder::new(&roots[0]);
    for r in &roots[1..] {
        wb.add(r);
    }
    wb.hidden(!o.hidden)
        .ignore(!o.no_ignore)
        .git_ignore(!o.no_ignore)
        .git_global(!o.no_ignore)
        .git_exclude(!o.no_ignore)
        .parents(!o.no_ignore)
        .follow_links(o.follow_links)
        .max_depth(o.max_depth)
        .max_filesize(o.max_filesize)
        .threads(if o.threads == 0 { std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(12) } else { o.threads });
    // the index directory is never searched
    let mut ob = OverrideBuilder::new(&roots[0]);
    ob.add("!.scrutate/")?;
    for g in &o.globs {
        ob.add(g).with_context(|| format!("invalid glob {g:?}"))?;
    }
    for t in &o.types {
        let (neg, name) = match t.strip_prefix('!') {
            Some(n) => (true, n),
            None => (false, t.as_str()),
        };
        let globs = type_globs(name).ok_or_else(|| anyhow!("unknown file type {name:?} (see --type-list)"))?;
        for g in globs {
            ob.add(&if neg { format!("!{g}") } else { g })?;
        }
    }
    wb.overrides(ob.build()?);
    Ok(wb.build_parallel())
}

/// Search `roots`, sending every hit to `on_hit` as it is found (from the calling thread). Returns
/// statistics; `cancel` may be set from another thread (or by `max_results`) to stop early.
pub fn search(roots: &[PathBuf], o: &Options, cancel: Arc<AtomicBool>, mut on_hit: impl FnMut(Hit)) -> Result<Stats> {
    let t0 = Instant::now();
    if roots.is_empty() {
        bail!("no paths to search");
    }
    let matcher = build_matcher(o)?;
    let walker = build_walker(roots, o)?;
    let (tx, rx) = mpsc::channel::<Vec<Hit>>();
    let files_searched = Arc::new(AtomicU64::new(0));
    let files_matched = Arc::new(AtomicU64::new(0));
    let matches = Arc::new(AtomicU64::new(0));
    let bytes = Arc::new(AtomicU64::new(0));
    let errors = Arc::new(AtomicU64::new(0));
    let roots_owned: Vec<PathBuf> = roots.to_vec();
    let opts = o.clone();
    let matcher_arc = Arc::new(matcher);
    let cancel2 = Arc::clone(&cancel);
    let (fs2, fm2, m2, b2, e2) = (Arc::clone(&files_searched), Arc::clone(&files_matched), Arc::clone(&matches), Arc::clone(&bytes), Arc::clone(&errors));
    let worker = std::thread::spawn(move || {
        walker.run(|| {
            let tx = tx.clone();
            let matcher = Arc::clone(&matcher_arc);
            let cancel = Arc::clone(&cancel2);
            let (fs, fm, m, b, e) = (Arc::clone(&fs2), Arc::clone(&fm2), Arc::clone(&m2), Arc::clone(&b2), Arc::clone(&e2));
            let opts = opts.clone();
            let roots = roots_owned.clone();
            let mut searcher = SearcherBuilder::new()
                .binary_detection(BinaryDetection::quit(0))
                .line_number(!(opts.count_only || opts.files_only)) // line numbers cost a pass over the buffer
                .multi_line(opts.multiline)
                .before_context(opts.before)
                .after_context(opts.after)
                .invert_match(opts.invert)
                .build();
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) {
                    return WalkState::Quit;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => {
                        e.fetch_add(1, Ordering::Relaxed);
                        return WalkState::Continue;
                    }
                };
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    return WalkState::Continue;
                }
                let path = entry.path();
                let rel: Arc<str> = Arc::from(relative(path, &roots));
                fs.fetch_add(1, Ordering::Relaxed);
                if opts.want_bytes {
                    if let Ok(md) = entry.metadata() {
                        b.fetch_add(md.len(), Ordering::Relaxed);
                    }
                }
                let mut sink = Collector {
                    path: Arc::clone(&rel),
                    tx: &tx,
                    batch: Vec::new(),
                    want_spans: opts.want_spans,
                    stats_matches: &m,
                    max_results: opts.max_results,
                    matched: false,
                    cancel: &cancel,
                    matcher: &matcher,
                    invert: opts.invert,
                    count_only: opts.count_only,
                    files_only: opts.files_only,
                    file_count: 0,
                };
                let res = match &*matcher {
                    AnyMatcher::Regex(rm) => searcher.search_path(rm, path, &mut sink),
                    AnyMatcher::Fuzzy(fm_) => searcher.search_path(fm_.as_ref(), path, &mut sink),
                };
                if res.is_err() {
                    e.fetch_add(1, Ordering::Relaxed);
                }
                sink.flush(); // a search aborted by an error or cancellation still reports what it found
                if sink.matched {
                    fm.fetch_add(1, Ordering::Relaxed);
                    if opts.count_only || opts.files_only {
                        // one record per file carries the count in `line`
                        let _ = tx.send(vec![Hit { path: rel, line: sink.file_count, column: 0, text: String::new(), spans: Vec::new(), context: false }]);
                    }
                }
                if cancel.load(Ordering::Relaxed) {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });
    });
    let mut per_file_count: std::collections::HashMap<Arc<str>, u64> = std::collections::HashMap::new();
    for batch in rx {
        for hit in batch {
            if let Some(max) = o.max_count {
                if !hit.context {
                    let c = per_file_count.entry(Arc::clone(&hit.path)).or_insert(0);
                    *c += 1;
                    if *c > max {
                        continue;
                    }
                }
            }
            on_hit(hit);
        }
    }
    worker.join().map_err(|_| anyhow!("search worker panicked"))?;
    Ok(Stats {
        files_searched: files_searched.load(Ordering::Relaxed),
        files_matched: files_matched.load(Ordering::Relaxed),
        matches: matches.load(Ordering::Relaxed),
        bytes_searched: bytes.load(Ordering::Relaxed),
        errors: errors.load(Ordering::Relaxed),
        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
        cancelled: cancel.load(Ordering::Relaxed),
    })
}

fn relative(path: &Path, roots: &[PathBuf]) -> String {
    for r in roots {
        if let Ok(rel) = path.strip_prefix(r) {
            let s = rel.to_string_lossy().replace('\\', "/");
            if roots.len() == 1 && r == Path::new(".") {
                return s;
            }
            if roots.len() == 1 {
                return s;
            }
            return format!("{}/{}", r.to_string_lossy().trim_end_matches('/'), s);
        }
    }
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fuzzy_find(pat: &str, k: u8, hay: &str) -> Option<(usize, usize)> {
        let m = FuzzyMatcher::new(pat, k, false).unwrap();
        m.find_at(hay.as_bytes(), 0).unwrap().map(|m| (m.start(), m.end()))
    }

    #[test]
    fn fuzzy_exact_and_edits() {
        assert_eq!(fuzzy_find("token", 0, "the token here"), Some((4, 9)));
        assert_eq!(fuzzy_find("token", 1, "a tokne b"), Some((2, 6))); // "tokn" is one deletion away
        assert_eq!(fuzzy_find("token", 1, "a tkoen b"), None); // a transposition is two edits
        assert_eq!(fuzzy_find("token", 2, "a tkoen b"), Some((2, 7)));
        assert_eq!(fuzzy_find("token", 1, "a tokn b"), Some((2, 6))); // one deletion
        assert_eq!(fuzzy_find("token", 1, "a tokken b"), Some((2, 8))); // one insertion
        assert_eq!(fuzzy_find("token", 1, "a tomen b"), Some((2, 7))); // one substitution
        assert_eq!(fuzzy_find("token", 0, "nothing"), None);
        assert_eq!(fuzzy_find("token", 1, "tok\nen"), None); // never across lines
    }

    #[test]
    fn fuzzy_case_fold() {
        let m = FuzzyMatcher::new("Token", 0, true).unwrap();
        assert!(m.find_at(b"a TOKEN b", 0).unwrap().is_some());
        let m = FuzzyMatcher::new("Token", 0, false).unwrap();
        assert!(m.find_at(b"a TOKEN b", 0).unwrap().is_none());
    }

    #[test]
    fn identifier_parts_cover_digits_and_acronyms() {
        let p = |s: &str| ident_parts(s);
        assert_eq!(p("makeWidget3"), vec!["make", "Widget", "3"]);
        assert_eq!(p("make_widget_3"), vec!["make", "widget", "3"]);
        assert_eq!(p("XMLHttpRequest2"), vec!["XML", "Http", "Request", "2"]);
        assert_eq!(p("HTTP2Server"), vec!["HTTP", "2", "Server"]);
        assert_eq!(p("MAKE_WIDGET_3"), vec!["MAKE", "WIDGET", "3"]);
        let re = regex::Regex::new(&identifier_regex("makeWidget3")).unwrap();
        assert!(re.is_match("x = make_widget_3(1)") && re.is_match("MAKE_WIDGET_3") && re.is_match("makeWidget3"));
        assert!(!re.is_match("make_widget_30") && !re.is_match("make_widget_3x"));
    }

    #[test]
    fn identifier_regex_matches_every_style() {
        let re = regex::Regex::new(&identifier_regex("refreshToken")).unwrap();
        for s in ["refresh_token", "refreshToken", "RefreshToken", "REFRESH_TOKEN", "refresh-token"] {
            assert!(re.is_match(&format!("x = {s}(1)")), "{s}");
        }
        assert!(!re.is_match("refreshTokens"));
        assert!(!re.is_match("refresh tokens"));
    }
}
