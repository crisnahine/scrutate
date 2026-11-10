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

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
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
    /// PCRE2: look-around and backreferences, by backtracking within PCRE2's match limit
    Pcre2,
    /// a regex within `max_edits` errors (byte-oriented, at most 64 character positions)
    FuzzyRegex {
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
    /// search the members of zip/tar archives and the contents of compressed files (`-z`)
    pub search_zip: bool,
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
            search_zip: false,
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
    /// the first few files that could not be searched (or not to the end), with the reason
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<String>,
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
    /// patterns longer than 128 bytes: the pattern and its reverse in 64-bit blocks
    long: Option<Box<(Blocks, Blocks)>>,
}

/// The longest fuzzy pattern accepted (the work per byte grows with its length / 64).
pub const FUZZY_MAX: usize = 4096;

/// Myers' recurrence on 64-bit blocks, for patterns longer than one machine word: the addition
/// and the two shifts carry from block to block (Hyyrö 2003).
#[derive(Clone)]
struct Blocks {
    words: usize,
    m: usize,
    peq: Vec<u64>,
}

impl Blocks {
    fn new(pat: &[u8], fold: bool) -> Blocks {
        let words = pat.len().div_ceil(64);
        let mut peq = vec![0u64; 256 * words];
        for (j, &c) in pat.iter().enumerate() {
            peq[c as usize * words + j / 64] |= 1 << (j % 64);
            if fold && c.is_ascii_lowercase() {
                peq[c.to_ascii_uppercase() as usize * words + j / 64] |= 1 << (j % 64);
            }
        }
        for w in 0..words {
            peq[b'\n' as usize * words + w] = 0;
        }
        Blocks { words, m: pat.len(), peq }
    }

    fn start(&self) -> (Vec<u64>, Vec<u64>) {
        let mut pv = vec![u64::MAX; self.words];
        if !self.m.is_multiple_of(64) {
            pv[self.words - 1] = (1u64 << (self.m % 64)) - 1;
        }
        (pv, vec![0; self.words])
    }

    /// One text byte: updates the column and returns the change of the last row's distance.
    fn step(&self, pv: &mut [u64], mv: &mut [u64], c: u8) -> isize {
        let last = self.words - 1;
        let top = 1u64 << ((self.m - 1) % 64);
        let (mut carry, mut ph_in, mut mh_in, mut delta) = (0u64, 0u64, 0u64, 0isize);
        for w in 0..self.words {
            let eq = self.peq[c as usize * self.words + w];
            let xv = eq | mv[w];
            let (s1, o1) = (eq & pv[w]).overflowing_add(pv[w]);
            let (s2, o2) = s1.overflowing_add(carry);
            carry = (o1 | o2) as u64;
            let xh = (s2 ^ pv[w]) | eq;
            let ph = mv[w] | !(xh | pv[w]);
            let mh = pv[w] & xh;
            if w == last {
                delta = if ph & top != 0 {
                    1
                } else if mh & top != 0 {
                    -1
                } else {
                    0
                };
            }
            let ph_s = (ph << 1) | ph_in;
            let mh_s = (mh << 1) | mh_in;
            ph_in = ph >> 63;
            mh_in = mh >> 63;
            pv[w] = mh_s | !(xv | ph_s);
            mv[w] = ph_s & xv;
        }
        if !self.m.is_multiple_of(64) {
            let mask = (1u64 << (self.m % 64)) - 1;
            pv[last] &= mask;
            mv[last] &= mask;
        }
        delta
    }
}

impl FuzzyMatcher {
    pub fn new(pattern: &str, max_edits: u8, fold_case: bool) -> Result<FuzzyMatcher> {
        let pat: Vec<u8> = if fold_case { pattern.to_ascii_lowercase().into_bytes() } else { pattern.as_bytes().to_vec() };
        if pat.is_empty() || pat.len() > FUZZY_MAX {
            bail!("fuzzy patterns must be 1..={FUZZY_MAX} bytes");
        }
        if max_edits as usize >= pat.len() {
            bail!("the number of edits ({max_edits}) must be smaller than the pattern length ({})", pat.len());
        }
        let mut peq = [0u128; 256];
        for (j, &c) in pat.iter().enumerate().take(128) {
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
        let long = (pat.len() > 128).then(|| {
            let rev: Vec<u8> = pat.iter().rev().copied().collect();
            Box::new((Blocks::new(&pat, fold_case), Blocks::new(&rev, fold_case)))
        });
        Ok(FuzzyMatcher { peq, m: pat.len(), k: max_edits as usize, pattern: pat, fold: fold_case, prefilter, long })
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
        if let Some(b) = &self.long {
            let (fwd, _) = &**b;
            let (mut pv, mut mv) = fwd.start();
            let mut score = self.m as isize;
            for (i, &c) in hay[at..].iter().enumerate() {
                if c == b'\n' {
                    (pv, mv) = fwd.start();
                    score = self.m as isize;
                    continue;
                }
                score += fwd.step(&mut pv, &mut mv, c);
                if score <= self.k as isize {
                    return Some((at + i + 1, score as usize));
                }
            }
            return None;
        }
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
        if let Some(b) = &self.long {
            let (_, rev) = &**b;
            let lo = end.saturating_sub(m + self.k);
            let (mut pv, mut mv) = rev.start();
            let mut score = m as isize;
            let mut best = None;
            for (i, &c) in hay[lo..end].iter().rev().enumerate() {
                if c == b'\n' {
                    break;
                }
                score += rev.step(&mut pv, &mut mv, c);
                if score <= d as isize {
                    best = Some(end - (i + 1));
                }
            }
            return best.unwrap_or_else(|| end.saturating_sub(m));
        }
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
    Regex(LineRegex),
    Fuzzy(Box<FuzzyMatcher>),
    Pcre2(grep_pcre2::RegexMatcher),
    FuzzyRegex(Box<crate::fuzzyre::FuzzyRegex>),
}

/// grep-regex's matcher, except that the regex engine is kept from running its automaton over every
/// byte in two cases: a regex with no usable literal prefix whose every match ends in one of a few
/// literals (`\w+_(suspend|resume)\(`) finds candidate lines by searching for those literals, and a
/// regex whose every match starts a line (`^(a|aa)+$`) looks only at line starts.
#[derive(Clone, Debug)]
pub struct LineRegex {
    inner: grep_regex::RegexMatcher,
    suffixes: Option<regex::bytes::Regex>,
    line_start: Option<LineStart>,
}

/// For a regex whose every match begins at a line start: the line starts that begin with one of its
/// prefix literals, and a search anchored at a line start. Used only on text without a line break
/// past the start (grep-regex removes `\n` from what a line regex can match; this regex does not).
#[derive(Clone, Debug)]
struct LineStart {
    anchored: regex_automata::meta::Regex,
    prefixes: Vec<Vec<u8>>,
    /// a line break followed by one of `prefixes`
    starts: regex::bytes::Regex,
}

impl LineRegex {
    fn new(inner: grep_regex::RegexMatcher, pattern: &str, insensitive: bool) -> LineRegex {
        let lines = inner.line_terminator().is_some();
        let suffixes = if lines { suffix_searcher(pattern, insensitive) } else { None };
        let line_start = if lines && suffixes.is_none() { line_start(pattern, insensitive) } else { None };
        LineRegex { inner, suffixes, line_start }
    }

    fn plain(inner: grep_regex::RegexMatcher) -> LineRegex {
        LineRegex { inner, suffixes: None, line_start: None }
    }

    /// The end of the match anchored at `at`, when `at` starts a line of a haystack without line
    /// breaks after it; Err when the line-start path does not apply.
    fn at_line_start(&self, haystack: &[u8], at: usize, earliest: bool) -> Result<Option<MatchSpan>, ()> {
        let ls = self.line_start.as_ref().ok_or(())?;
        if at > haystack.len() || memchr::memchr(b'\n', &haystack[at..]).is_some() {
            return Err(());
        }
        if at > 0 && haystack[at - 1] != b'\n' {
            return Ok(None);
        }
        let input = regex_automata::Input::new(haystack).range(at..).anchored(regex_automata::Anchored::Yes).earliest(earliest);
        Ok(ls.anchored.search(&input).map(|m| MatchSpan::new(m.start(), m.end())))
    }
}

/// The line-start path for `pattern`, when every match begins at a line start and begins with one
/// of a few literals.
fn line_start(pattern: &str, insensitive: bool) -> Option<LineStart> {
    use regex_syntax::hir::{
        literal::{ExtractKind, Extractor},
        Look,
    };
    let hir = regex_syntax::ParserBuilder::new().case_insensitive(insensitive).multi_line(true).utf8(false).build().parse(pattern).ok()?;
    // `\A` and `\z` mean the line's ends when the searcher checks a line, the buffer's ends here
    let looks = hir.properties().look_set();
    if !hir.properties().look_set_prefix().contains(Look::StartLF) || looks.contains(Look::Start) || looks.contains(Look::End) {
        return None;
    }
    let lits = Extractor::new().kind(ExtractKind::Prefix).extract(&hir);
    let lits = lits.literals()?;
    if lits.is_empty() || lits.len() > 64 || lits.iter().any(|l| l.is_empty() || l.as_bytes().contains(&b'\n')) {
        return None;
    }
    let prefixes: Vec<Vec<u8>> = lits.iter().map(|l| l.as_bytes().to_vec()).collect();
    let alts: Vec<String> = prefixes.iter().map(|l| l.iter().map(|b| format!("\\x{b:02X}")).collect()).collect();
    let starts = regex::bytes::Regex::new(&format!("(?-u:\\n(?:{}))", alts.join("|"))).ok()?;
    let anchored = regex_automata::meta::Regex::builder().configure(regex_automata::meta::Config::new().utf8_empty(false)).build_from_hir(&hir).ok()?;
    Some(LineStart { anchored, prefixes, starts })
}

/// A searcher for the literals every match of `pattern` ends with, when that beats the regex engine.
/// An alternation of literals compiles to the engine's vectorised multi-literal search.
fn suffix_searcher(pattern: &str, insensitive: bool) -> Option<regex::bytes::Regex> {
    use regex_syntax::hir::literal::{ExtractKind, Extractor};
    let hir = regex_syntax::ParserBuilder::new().case_insensitive(insensitive).utf8(false).build().parse(pattern).ok()?;
    let prefixes = Extractor::new().kind(ExtractKind::Prefix).extract(&hir);
    if prefixes.literals().is_some_and(|l| !l.is_empty() && l.iter().all(|x| x.len() >= 2)) {
        return None; // the engine's own prefilter already searches for these
    }
    let suffixes = Extractor::new().kind(ExtractKind::Suffix).extract(&hir);
    let lits = suffixes.literals()?;
    if lits.is_empty() || lits.len() > 64 || lits.iter().any(|l| l.len() < 3 || l.as_bytes().contains(&b'\n')) {
        return None;
    }
    let alts: Vec<String> = lits.iter().map(|l| l.as_bytes().iter().map(|b| format!("\\x{b:02X}")).collect()).collect();
    regex::bytes::Regex::new(&format!("(?-u:{})", alts.join("|"))).ok()
}

impl Matcher for LineRegex {
    type Captures = grep_regex::RegexCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<MatchSpan>, NoError> {
        match self.at_line_start(haystack, at, false) {
            Ok(m) => Ok(m),
            Err(()) => self.inner.find_at(haystack, at),
        }
    }
    fn new_captures(&self) -> Result<Self::Captures, NoError> {
        self.inner.new_captures()
    }
    fn capture_count(&self) -> usize {
        self.inner.capture_count()
    }
    fn capture_index(&self, name: &str) -> Option<usize> {
        self.inner.capture_index(name)
    }
    fn try_find_iter<F, E>(&self, haystack: &[u8], matched: F) -> Result<Result<(), E>, NoError>
    where
        F: FnMut(MatchSpan) -> Result<bool, E>,
    {
        self.inner.try_find_iter(haystack, matched)
    }
    fn captures_at(&self, haystack: &[u8], at: usize, caps: &mut Self::Captures) -> Result<bool, NoError> {
        self.inner.captures_at(haystack, at, caps)
    }
    fn shortest_match_at(&self, haystack: &[u8], at: usize) -> Result<Option<usize>, NoError> {
        match self.at_line_start(haystack, at, true) {
            Ok(m) => Ok(m.map(|m| m.end())),
            Err(()) => self.inner.shortest_match_at(haystack, at),
        }
    }
    fn non_matching_bytes(&self) -> Option<&grep_matcher::ByteSet> {
        self.inner.non_matching_bytes()
    }
    fn line_terminator(&self) -> Option<LineTerminator> {
        self.inner.line_terminator()
    }
    fn find_candidate_line(&self, haystack: &[u8]) -> Result<Option<grep_matcher::LineMatchKind>, NoError> {
        if let Some(ls) = &self.line_start {
            // the searcher calls this at the start of a line, with whole lines
            let mut next = ls.prefixes.iter().any(|p| haystack.starts_with(p)).then_some(0);
            let mut from = 0;
            loop {
                let at = match next.take() {
                    Some(at) => at,
                    None => match ls.starts.find_at(haystack, from) {
                        Some(m) => m.start() + 1,
                        None => return Ok(None),
                    },
                };
                let input = regex_automata::Input::new(haystack).range(at..).anchored(regex_automata::Anchored::Yes).earliest(true);
                match ls.anchored.search_half(&input) {
                    // no match here, so none for grep-regex's line regex either (it matches less)
                    None => from = at,
                    // a match within the line is one for grep-regex too
                    Some(m) if memchr::memchr(b'\n', &haystack[at..m.offset()]).is_none() => return Ok(Some(grep_matcher::LineMatchKind::Confirmed(at))),
                    Some(_) => return Ok(Some(grep_matcher::LineMatchKind::Candidate(at))),
                }
            }
        }
        match &self.suffixes {
            Some(re) => Ok(re.find(haystack).map(|m| grep_matcher::LineMatchKind::Candidate(m.start()))),
            None => self.inner.find_candidate_line(haystack),
        }
    }
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
        Mode::FuzzyRegex { max_edits } => Ok(AnyMatcher::FuzzyRegex(Box::new(crate::fuzzyre::FuzzyRegex::new(&o.pattern, max_edits, insensitive)?))),
        Mode::Pcre2 => {
            let m = grep_pcre2::RegexMatcherBuilder::new()
                .caseless(insensitive)
                .word(o.word)
                .multi_line(true)
                .dotall(false)
                .utf(true)
                .ucp(true)
                .jit_if_available(true)
                .build(&o.pattern)
                .with_context(|| format!("invalid PCRE2 pattern {:?}", o.pattern))?;
            Ok(AnyMatcher::Pcre2(m))
        }
        _ => {
            let pat = match o.mode {
                Mode::Literal => regex::escape(&o.pattern),
                Mode::Identifier => identifier_regex(&o.pattern),
                _ => o.pattern.clone(),
            };
            let mut b = RegexMatcherBuilder::new();
            // ^ and $ are line anchors in every mode, as in ripgrep; without it the searcher has to
            // run the regex one line at a time
            b.case_insensitive(insensitive && o.mode != Mode::Identifier)
                .word(o.word)
                .multi_line(true)
                .dot_matches_new_line(false)
                .size_limit(o.regex_size_limit)
                .dfa_size_limit(o.dfa_size_limit)
                .line_terminator(if o.multiline { None } else { Some(b'\n') });
            let m = b.build(&pat).with_context(|| format!("invalid pattern {:?}", o.pattern))?;
            let suffix_ok = !o.word && o.mode != Mode::Literal;
            Ok(AnyMatcher::Regex(if suffix_ok { LineRegex::new(m, &pat, insensitive && o.mode != Mode::Identifier) } else { LineRegex::plain(m) }))
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
/// Plain `path:line:text` output written by the search threads themselves: each file's lines are
/// formatted into a buffer and written in one call, so no per-line allocation or channel message.
/// Used for the common piped case (no colour, no JSON, no sorting); the bytes are the same as the
/// CLI's own formatting of `Hit`s.
pub struct PlainSink {
    pub out: Mutex<Box<dyn Write + Send>>,
    /// the reader went away (`| head`)
    pub broken: AtomicBool,
}

impl PlainSink {
    pub fn new(out: Box<dyn Write + Send>) -> PlainSink {
        PlainSink { out: Mutex::new(out), broken: AtomicBool::new(false) }
    }

    fn write(&self, buf: &[u8], cancel: &AtomicBool) {
        self.write_parts(&[buf], cancel)
    }

    /// Write several slices as one unit (no other thread's output between them).
    fn write_parts(&self, parts: &[&[u8]], cancel: &AtomicBool) {
        if parts.iter().all(|p| p.is_empty()) || self.broken.load(Ordering::Relaxed) {
            return;
        }
        let res = match self.out.lock() {
            Ok(mut w) => parts.iter().try_for_each(|p| w.write_all(p)),
            Err(_) => Err(std::io::Error::other("output lock poisoned")),
        };
        if res.is_err() {
            self.broken.store(true, Ordering::Relaxed);
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// `before`, `head`, `text` with invalid UTF-8 replaced, and a line break, as one unit.
    fn write_line(&self, before: &[u8], head: &[u8], text: &[u8], cancel: &AtomicBool) {
        if self.broken.load(Ordering::Relaxed) {
            return;
        }
        let res = match self.out.lock() {
            Ok(mut w) => {
                w.write_all(before).and_then(|_| w.write_all(head)).and_then(|_| write_lossy(&mut **w, text, 1 << 18)).and_then(|_| w.write_all(b"\n"))
            }
            Err(_) => Err(std::io::Error::other("output lock poisoned")),
        };
        if res.is_err() {
            self.broken.store(true, Ordering::Relaxed);
            cancel.store(true, Ordering::Relaxed);
        }
    }

    pub fn flush(&self) -> std::io::Result<()> {
        match self.out.lock() {
            Ok(mut w) => w.flush(),
            Err(_) => Err(std::io::Error::other("output lock poisoned")),
        }
    }
}

/// Line text as UTF-8, invalid bytes replaced. Validates with `from_utf8` first: its word-at-a-time
/// check is several times faster than the replacing decoder on the (common) valid input.
fn lossy(b: &[u8]) -> std::borrow::Cow<'_, str> {
    match std::str::from_utf8(b) {
        Ok(s) => std::borrow::Cow::Borrowed(s),
        Err(_) => String::from_utf8_lossy(b),
    }
}

/// `b` written as `lossy(b)` would be, `piece` bytes at a time: each piece is checked just before it
/// is written, while the reader drains the previous one, instead of the whole line up front.
fn write_lossy(w: &mut dyn Write, b: &[u8], piece: usize) -> std::io::Result<()> {
    let mut at = 0;
    while at < b.len() {
        let end = b.len().min(at + piece.max(4));
        match std::str::from_utf8(&b[at..end]) {
            Ok(_) => {
                w.write_all(&b[at..end])?;
                at = end;
            }
            Err(e) => {
                let ok = at + e.valid_up_to();
                w.write_all(&b[at..ok])?;
                match e.error_len() {
                    Some(bad) => {
                        w.write_all("\u{FFFD}".as_bytes())?;
                        at = ok + bad;
                    }
                    // a character cut by the end of the piece: the next piece starts with it
                    None if end < b.len() => at = ok,
                    None => {
                        w.write_all("\u{FFFD}".as_bytes())?;
                        at = b.len();
                    }
                }
            }
        }
    }
    Ok(())
}

/// `path:line:text\n` (`-` separators for context lines), the CLI's plain format. A multi-line
/// match prints each of its lines with its own number, as grep-class tools do.
pub fn format_plain(out: &mut Vec<u8>, path: &str, line: u64, text: &[u8], context: bool) {
    let sep = if context { b'-' } else { b':' };
    for (k, part) in text.split(|&b| b == b'\n').enumerate() {
        out.extend_from_slice(path.as_bytes());
        out.push(sep);
        let mut n = line + k as u64;
        let mut digits = [0u8; 20];
        let mut i = digits.len();
        loop {
            i -= 1;
            digits[i] = b'0' + (n % 10) as u8;
            n /= 10;
            if n == 0 {
                break;
            }
        }
        out.extend_from_slice(&digits[i..]);
        out.push(sep);
        out.extend_from_slice(part);
        out.push(b'\n');
    }
}

struct Collector<'a> {
    path: Arc<str>,
    tx: &'a mpsc::Sender<Vec<Hit>>,
    /// hits of the current file, sent in batches (one channel operation per file or 512 lines)
    batch: Vec<Hit>,
    plain: Option<&'a PlainSink>,
    out: Vec<u8>,
    want_spans: bool,
    stats_matches: &'a AtomicU64,
    max_results: Option<u64>,
    /// `--max-count`, for the count of `-c`; printed lines are cut in `drain`
    max_count: Option<u64>,
    matched: bool,
    cancel: &'a AtomicBool,
    matcher: &'a AnyMatcher,
    invert: bool,
    count_only: bool,
    files_only: bool,
    file_count: u64,
    /// added to every line number (a range of a split file starts mid-file)
    line_base: u64,
    /// keep plain output in `out` instead of writing it (a range of a split file waits its turn)
    hold: bool,
    /// the searcher reported binary data
    binary: bool,
    /// a match may span lines (otherwise a matched line holds no line break)
    multiline: bool,
}

fn spans_in(matcher: &AnyMatcher, line: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let mut at = 0;
    let find = |at: usize| -> Option<MatchSpan> {
        match matcher {
            AnyMatcher::Regex(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::Fuzzy(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::Pcre2(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::FuzzyRegex(m) => m.find_at(line, at).ok().flatten(),
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
            // one match is enough to list the file; a count stops at --max-count
            return Ok(!self.files_only && self.max_count.is_none_or(|max| self.file_count < max));
        }
        let bytes = mat.bytes();
        let line = bytes.strip_suffix(b"\n").map(|b| b.strip_suffix(b"\r").unwrap_or(b)).unwrap_or(bytes);
        if let Some(p) = self.plain {
            let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
            if self.over_limit(n) {
                return Ok(false);
            }
            self.matched = true;
            let number = mat.line_number().unwrap_or(0) + self.line_base;
            if line.len() >= 1 << 16 && !self.hold && !(self.multiline && memchr::memchr(b'\n', line).is_some()) {
                // a very long line is written from where it is instead of being copied into the buffer
                let mut head = Vec::with_capacity(self.path.len() + 24);
                format_plain(&mut head, &self.path, number, b"", false);
                head.pop(); // the newline: the text goes between
                p.write_line(&self.out, &head, line, self.cancel);
                self.out.clear();
            } else {
                format_plain(&mut self.out, &self.path, number, lossy(line).as_bytes(), false);
            }
            if self.out.len() >= 1 << 16 && !self.hold {
                p.write(&self.out, self.cancel);
                self.out.clear();
            }
            if let Some(max) = self.max_results {
                if n >= max {
                    self.cancel.store(true, Ordering::Relaxed);
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
        if self.over_limit(n) {
            return Ok(false);
        }
        let text = lossy(line).into_owned();
        let spans = if self.invert || !self.want_spans { Vec::new() } else { spans_in(self.matcher, bytes) };
        let column = spans.first().map(|s| s.0 as u64 + 1).unwrap_or(0);
        self.matched = true;
        self.push(Hit { path: Arc::clone(&self.path), line: mat.line_number().unwrap_or(0) + self.line_base, column, text, spans, context: false });
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
        let line = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        if self.plain.is_some() {
            format_plain(&mut self.out, &self.path, ctx.line_number().unwrap_or(0) + self.line_base, lossy(line).as_bytes(), true);
            return Ok(true);
        }
        let text = lossy(line).into_owned();
        self.push(Hit {
            path: Arc::clone(&self.path),
            line: ctx.line_number().unwrap_or(0) + self.line_base,
            column: 0,
            text,
            spans: Vec::new(),
            context: true,
        });
        Ok(true)
    }

    fn finish(&mut self, _s: &Searcher, f: &grep_searcher::SinkFinish) -> Result<(), std::io::Error> {
        self.binary = f.binary_byte_offset().is_some();
        self.flush();
        Ok(())
    }
}

impl Collector<'_> {
    /// Match number `n` (counted over all threads) is past `max_results`: another thread got there
    /// first, so this one is not reported.
    fn over_limit(&self, n: u64) -> bool {
        let over = self.max_results.is_some_and(|max| n > max);
        if over {
            self.cancel.store(true, Ordering::Relaxed);
        }
        over
    }

    fn push(&mut self, h: Hit) {
        self.batch.push(h);
        if self.batch.len() >= 512 {
            self.flush();
        }
    }
    fn flush(&mut self) {
        if let (Some(p), false) = (self.plain, self.hold) {
            p.write(&self.out, self.cancel);
            self.out.clear();
        }
        if !self.batch.is_empty() {
            let _ = self.tx.send(std::mem::take(&mut self.batch));
        }
    }
}

/// Default number of threads searching many files: the cores, at most 12 (more only add contention
/// on the directory walk and the output channel); on macOS the performance cores, because threads
/// opening and reading files at once mostly wait on each other in the kernel there (the Linux tree:
/// 977 ms on 11 threads, 843 ms on 6, 850 ms on 4; LLVM: 1649 ms on 11, 1377 ms on 4).
pub fn default_threads(o: &Options) -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    if o.threads != 0 {
        return o.threads;
    }
    *N.get_or_init(|| performance_cores().map_or_else(cores, |p| p.clamp(2, cores())))
}

/// Threads the parallel walk starts with: `default_threads`, unless the files read so far took
/// long enough that they came from the disk rather than the page cache (25 µs or so each warm,
/// 160 µs or more cold on an M3 Pro): a cold tree is read fastest with many reads in flight, so
/// then twice the cores, at most 24 (the Linux tree cold: 2.9 s on 5 threads, 1.7 s on 11, 1.4 s
/// on 24). A walk that starts warm grows to that count when its reads turn slow.
fn walk_threads(o: &Options, c: &Counters) -> usize {
    if o.threads == 0 && reads_from_disk(c) {
        cold_threads()
    } else {
        default_threads(o)
    }
}

/// An open and read of a small file taking this long came from the disk, not the page cache
/// (about 25 us warm, 160 us or more cold on an M3 Pro).
const COLD_READ_NS: u64 = 75_000;

fn reads_from_disk(c: &Counters) -> bool {
    // three quarters slow: a few reads delayed by a busy machine do not make a warm tree look cold
    let reads = c.small_reads.load(Ordering::Relaxed);
    reads >= 16 && c.slow_reads.load(Ordering::Relaxed) * 4 >= reads * 3
}

fn cold_threads() -> usize {
    (2 * std::thread::available_parallelism().map_or(2, |n| n.get())).min(24)
}

/// Threads for one large file searched as ranges: every core (at most 12), since that work is
/// matching rather than opening files.
fn range_threads(o: &Options) -> usize {
    if o.threads != 0 {
        o.threads
    } else {
        cores()
    }
}

fn cores() -> usize {
    static CORES: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    // a system call on some platforms, and asked once per file
    *CORES.get_or_init(|| std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2).min(12))
}

#[cfg(target_os = "macos")]
fn performance_cores() -> Option<usize> {
    extern "C" {
        fn sysctlbyname(name: *const std::ffi::c_char, oldp: *mut std::ffi::c_void, oldlenp: *mut usize, newp: *mut std::ffi::c_void, newlen: usize) -> i32;
    }
    let mut v: i32 = 0;
    let mut len = std::mem::size_of::<i32>();
    // SAFETY: a NUL-terminated name, an i32 to write and its size, no new value
    let rc = unsafe { sysctlbyname(c"hw.perflevel0.physicalcpu".as_ptr(), (&mut v as *mut i32).cast(), &mut len, std::ptr::null_mut(), 0) };
    (rc == 0 && len == std::mem::size_of::<i32>() && v > 0).then_some(v as usize)
}

#[cfg(not(target_os = "macos"))]
fn performance_cores() -> Option<usize> {
    None
}

/// The glob and file-type filters of `o`, plus the index directory, which is never searched.
pub fn overrides(root: &Path, o: &Options) -> Result<ignore::overrides::Override> {
    let mut ob = OverrideBuilder::new(root);
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
    Ok(ob.build()?)
}

pub fn build_walker(roots: &[PathBuf], o: &Options) -> Result<ignore::WalkParallel> {
    Ok(walk_builder(roots, o)?.build_parallel())
}

fn walk_builder(roots: &[PathBuf], o: &Options) -> Result<WalkBuilder> {
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
        .threads(default_threads(o));
    wb.overrides(overrides(&roots[0], o)?);
    Ok(wb)
}

/// How many files the one-thread walk (`small_first`) takes before the parallel walk does the rest;
/// 0 skips it. Roots holding only files get 256: starting the parallel walk costs more there than
/// the search itself (three large files: 40.6 ms against 41.9). Any directory to enter gets 0: one
/// thread reading every directory then holds up the searches (the ripgrep repository, 60
/// directories: 5.6 ms in parallel against 6.0). `SCRUTATE_SMALL_TREE` sets the number for any tree
/// (the tests use both walks).
fn small_tree_files(roots: &[PathBuf], o: &Options) -> usize {
    static V: std::sync::OnceLock<Option<usize>> = std::sync::OnceLock::new();
    let forced = *V.get_or_init(|| std::env::var("SCRUTATE_SMALL_TREE").ok().and_then(|v| v.parse().ok()));
    forced.unwrap_or_else(|| if has_dirs(roots, o) { 0 } else { 256 })
}

/// Whether a root holds a directory the walk would enter (a hidden one only with `--hidden`).
fn has_dirs(roots: &[PathBuf], o: &Options) -> bool {
    roots.iter().any(|r| {
        std::fs::read_dir(r).is_ok_and(|mut entries| {
            entries.any(|e| {
                e.is_ok_and(|e| {
                    let dir = if o.follow_links { e.path().is_dir() } else { e.file_type().is_ok_and(|t| t.is_dir()) };
                    dir && (o.hidden || !e.file_name().as_encoded_bytes().starts_with(b"."))
                })
            })
        })
    })
}

enum Small {
    Done,
    /// more files than the budget: the paths already searched, for the parallel walk to skip
    Big(std::collections::HashSet<PathBuf>),
}

/// Walks on one thread while a few threads search the files it finds, so a small tree pays
/// neither for a parallel walk nor for walking before searching. Past `small_tree_files` files the
/// walk stops; the caller's parallel walk then skips what was searched here. Walk errors count only
/// when this walk finished (the parallel walk meets them again otherwise).
#[allow(clippy::too_many_arguments)]
fn small_first(
    roots: &[PathBuf],
    o: &Options,
    matcher: &AnyMatcher,
    c: &Counters,
    cancel: &AtomicBool,
    plain: Option<&PlainSink>,
    on_hit: &mut impl FnMut(Hit),
) -> Small {
    let limit = small_tree_files(roots, o);
    if limit == 0 {
        return Small::Big(Default::default());
    }
    let Ok(wb) = walk_builder(roots, o) else { return Small::Big(Default::default()) };
    let (work_tx, work_rx) = mpsc::channel::<(PathBuf, Arc<str>, Option<u64>)>();
    let work_rx = Mutex::new(work_rx);
    let (tx, rx) = mpsc::channel::<Vec<Hit>>();
    let workers = default_threads(o).min(3);
    let walked = std::thread::scope(|s| {
        let work_rx = &work_rx;
        let worker = move |tx: mpsc::Sender<Vec<Hit>>| {
            // a clone per thread: each gets its own regex cache pool instead of contending for one
            let matcher = matcher.clone();
            s.spawn(move || {
                let mut fw = file_worker(o);
                loop {
                    let job = match work_rx.lock() {
                        Ok(r) => r.recv(),
                        Err(_) => break,
                    };
                    let Ok((path, rel, size)) = job else { break };
                    if !cancel.load(Ordering::Relaxed) {
                        search_one(&mut fw, &matcher, &path, rel, size, o, c, cancel, &tx, plain);
                    }
                }
            });
        };
        for _ in 0..workers {
            worker(tx.clone());
        }
        // for the workers added when the files turn out to come from the disk
        let mut more = (o.threads == 0).then(|| tx.clone());
        drop(tx);
        let walker = s.spawn(move || {
            let mut seen = std::collections::HashSet::new();
            let mut errors = Vec::new();
            for entry in wb.build() {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        errors.push(e.to_string());
                        continue;
                    }
                };
                if !entry.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                if seen.len() == limit {
                    return (seen, errors, true);
                }
                let size = if o.want_bytes { entry.metadata().ok().map(|m| m.len()) } else { None };
                let rel: Arc<str> = Arc::from(relative(entry.path(), roots));
                let path = entry.into_path();
                seen.insert(path.clone());
                if work_tx.send((path, rel, size)).is_err() {
                    break;
                }
                if let Some(tx) = more.take_if(|_| reads_from_disk(c)) {
                    for _ in workers..cold_threads() {
                        worker(tx.clone());
                    }
                }
            }
            (seen, errors, false)
        });
        drain(rx, o, &mut *on_hit);
        walker.join()
    });
    match walked {
        Ok((seen, _, true)) if !cancel.load(Ordering::Relaxed) => Small::Big(seen),
        Ok((_, errors, _)) => {
            for e in errors {
                c.failed(e);
            }
            Small::Done
        }
        Err(_) => Small::Done,
    }
}

#[derive(Default)]
struct Counters {
    files_searched: AtomicU64,
    /// small files opened and read in one call
    small_reads: AtomicU64,
    /// of those, the reads that took as long as a disk read (`COLD_READ_NS` or more)
    slow_reads: AtomicU64,
    files_matched: AtomicU64,
    matches: AtomicU64,
    bytes: AtomicU64,
    errors: AtomicU64,
    failures: Mutex<Vec<String>>,
}

impl Counters {
    fn failed(&self, what: String) {
        self.errors.fetch_add(1, Ordering::Relaxed);
        if let Ok(mut v) = self.failures.lock() {
            if v.len() < 10 {
                v.push(what);
            }
        }
    }
}

/// What one search thread keeps from file to file: its searcher and the buffer small files are
/// read into (a new zeroed buffer per file cost about 4% of the CPU of a Linux-tree search on Linux).
struct FileWorker {
    searcher: Searcher,
    small: Vec<u8>,
}

fn file_worker(o: &Options) -> FileWorker {
    let searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(!(o.count_only || o.files_only)) // line numbers cost a pass over the buffer
        .multi_line(o.multiline)
        .before_context(o.before)
        .after_context(o.after)
        .invert_match(o.invert)
        .build();
    FileWorker { searcher, small: Vec::new() }
}

/// Search one file and send its hits to `tx`.
#[allow(clippy::too_many_arguments)]
fn search_one(
    w: &mut FileWorker,
    matcher: &AnyMatcher,
    path: &Path,
    rel: Arc<str>,
    size: Option<u64>,
    o: &Options,
    c: &Counters,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<Vec<Hit>>,
    plain: Option<&PlainSink>,
) {
    c.files_searched.fetch_add(1, Ordering::Relaxed);
    if let Some(n) = size {
        c.bytes.fetch_add(n, Ordering::Relaxed);
    }
    if o.search_zip {
        if let Some(kind) = crate::archive::Kind::of(&rel) {
            search_archive(&mut w.searcher, matcher, path, &rel, kind, o, c, cancel, tx, plain);
            return;
        }
    }
    let searcher = &mut w.searcher;
    let opened = Instant::now();
    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            c.failed(format!("{rel}: {e}"));
            return;
        }
    };
    let mut owned = None;
    let mut small = None;
    if splittable(o) {
        // most files are small: one read of up to SMALL_FILE + 1 bytes takes such a file whole and
        // tells a larger one apart, with no call for the size (a `statx` per file otherwise: 96,000
        // of them on the Linux tree)
        let first = if size.is_none_or(|n| n <= SMALL_FILE) { split::first(&file, &mut w.small) } else { split::First::Larger };
        if matches!(first, split::First::Whole(_) | split::First::Other) {
            c.small_reads.fetch_add(1, Ordering::Relaxed);
            if opened.elapsed().as_nanos() as u64 >= COLD_READ_NS {
                c.slow_reads.fetch_add(1, Ordering::Relaxed);
            }
        }
        match first {
            split::First::Whole(n) => small = Some(n),
            split::First::Other => {}
            split::First::Larger => {
                let len = size.or_else(|| file.metadata().ok().map(|m| m.len())).unwrap_or(0);
                let min = split_file_bytes();
                if len >= min {
                    match split::search(&file, len, (min / 8).max(1), &rel, matcher, o, c, cancel, tx, plain) {
                        split::Outcome::Done => return,
                        split::Outcome::Stream => {}
                        split::Outcome::Buffer(b) => owned = Some(b),
                    }
                } else if len <= MID_FILE {
                    small = split::rest(&file, len as usize, &mut w.small);
                }
            }
        }
    }
    let whole: Option<&[u8]> = owned.as_deref().or(small.map(|n| &w.small[..n]));
    let mut sink = collector(Arc::clone(&rel), matcher, o, c, cancel, tx, plain);
    let res = match (matcher, &whole) {
        (AnyMatcher::Regex(rm), Some(b)) => searcher.search_slice(rm, b, &mut sink),
        (AnyMatcher::Fuzzy(fm), Some(b)) => searcher.search_slice(fm.as_ref(), b, &mut sink),
        (AnyMatcher::Pcre2(pm), Some(b)) => searcher.search_slice(pm, b, &mut sink),
        (AnyMatcher::FuzzyRegex(fm), Some(b)) => searcher.search_slice(fm.as_ref(), b, &mut sink),
        (AnyMatcher::Regex(rm), None) => searcher.search_file(rm, &file, &mut sink),
        (AnyMatcher::Fuzzy(fm), None) => searcher.search_file(fm.as_ref(), &file, &mut sink),
        (AnyMatcher::Pcre2(pm), None) => searcher.search_file(pm, &file, &mut sink),
        (AnyMatcher::FuzzyRegex(fm), None) => searcher.search_file(fm.as_ref(), &file, &mut sink),
    };
    if let Err(e) = res {
        c.failed(format!("{rel}: {e}"));
    }
    sink.flush(); // a search aborted by an error or cancellation still reports what it found
    if sink.matched {
        c.files_matched.fetch_add(1, Ordering::Relaxed);
        if o.count_only || o.files_only {
            // one record per file carries the count in `line`
            let _ = tx.send(vec![Hit { path: rel, line: sink.file_count, column: 0, text: String::new(), spans: Vec::new(), context: false }]);
        }
    }
}

fn collector<'a>(
    path: Arc<str>,
    matcher: &'a AnyMatcher,
    o: &Options,
    c: &'a Counters,
    cancel: &'a AtomicBool,
    tx: &'a mpsc::Sender<Vec<Hit>>,
    plain: Option<&'a PlainSink>,
) -> Collector<'a> {
    Collector {
        path,
        tx,
        batch: Vec::new(),
        plain,
        out: Vec::new(),
        want_spans: o.want_spans,
        stats_matches: &c.matches,
        max_results: o.max_results,
        max_count: o.max_count,
        matched: false,
        cancel,
        matcher,
        invert: o.invert,
        count_only: o.count_only,
        files_only: o.files_only,
        file_count: 0,
        line_base: 0,
        hold: false,
        binary: false,
        multiline: o.multiline,
    }
}

/// `-z`: every member of an archive, or the decompressed file, as a file of its own, reported as
/// `archive.zip{member}` (the decompressed file keeps its own name).
#[allow(clippy::too_many_arguments)]
fn search_archive(
    searcher: &mut Searcher,
    matcher: &AnyMatcher,
    path: &Path,
    rel: &Arc<str>,
    kind: crate::archive::Kind,
    o: &Options,
    c: &Counters,
    cancel: &AtomicBool,
    tx: &mpsc::Sender<Vec<Hit>>,
    plain: Option<&PlainSink>,
) {
    let mut each = |member: Option<&str>, r: &mut dyn std::io::Read| -> bool {
        let label: Arc<str> = match member {
            Some(m) => Arc::from(format!("{rel}{{{m}}}")),
            None => Arc::clone(rel),
        };
        let mut sink = collector(Arc::clone(&label), matcher, o, c, cancel, tx, plain);
        let res = match matcher {
            AnyMatcher::Regex(m) => searcher.search_reader(m, r, &mut sink),
            AnyMatcher::Fuzzy(m) => searcher.search_reader(m.as_ref(), r, &mut sink),
            AnyMatcher::Pcre2(m) => searcher.search_reader(m, r, &mut sink),
            AnyMatcher::FuzzyRegex(m) => searcher.search_reader(m.as_ref(), r, &mut sink),
        };
        if let Err(e) = res {
            c.failed(format!("{label}: {e}"));
        }
        sink.flush();
        if sink.matched {
            c.files_matched.fetch_add(1, Ordering::Relaxed);
            if o.count_only || o.files_only {
                let _ = tx.send(vec![Hit { path: label, line: sink.file_count, column: 0, text: String::new(), spans: Vec::new(), context: false }]);
            }
        }
        !cancel.load(Ordering::Relaxed)
    };
    let mut failed = |what: String| c.failed(format!("{rel}: {what}"));
    if let Err(e) = crate::archive::walk(path, kind, &mut each, &mut failed) {
        c.failed(format!("{rel}: {e}"));
    }
}

/// Files at least this large are searched as several ranges of whole lines at once (an eighth of
/// it each): one huge file would otherwise keep a single core busy while the others idle.
/// `SCRUTATE_SPLIT_FILE_BYTES` overrides it (the tests use small files).
/// Files up to this size are read with one call (`split::first`) instead of reading until a
/// zero-length read; within it the searcher's binary check covers the whole file.
const SMALL_FILE: u64 = 64 << 10;
/// Files up to this size are read whole into the thread's buffer too (`split::rest`): the stream
/// reader takes one read per 64 KB, moves the rest of each buffer down and counts the lines of
/// everything it passes. Half the bytes of the Linux tree are in files between the two sizes.
const MID_FILE: u64 = 1 << 20;

fn split_file_bytes() -> u64 {
    static V: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *V.get_or_init(|| std::env::var("SCRUTATE_SPLIT_FILE_BYTES").ok().and_then(|v| v.parse().ok()).unwrap_or(64 << 20))
}

/// Line-at-a-time searches whose results do not depend on neighbouring lines or on a running count.
/// `-l` stays sequential: it stops at the first match.
fn splittable(o: &Options) -> bool {
    !o.multiline && o.before == 0 && o.after == 0 && o.max_count.is_none() && o.max_results.is_none() && !o.files_only && range_threads(o) > 1
}

mod split {
    use super::*;
    use std::fs::File;
    use std::io::Read;
    use std::sync::atomic::AtomicUsize;

    #[cfg(unix)]
    fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
        std::os::unix::fs::FileExt::read_at(f, buf, off)
    }
    #[cfg(windows)]
    fn read_at(f: &File, buf: &mut [u8], off: u64) -> std::io::Result<usize> {
        std::os::windows::fs::FileExt::seek_read(f, buf, off)
    }
    #[cfg(not(any(unix, windows)))]
    fn read_at(_f: &File, _buf: &mut [u8], _off: u64) -> std::io::Result<usize> {
        Err(std::io::Error::other("positional reads unavailable"))
    }

    /// Bytes `pos..end` of a file, read without moving a shared file offset.
    struct Range<'f> {
        file: &'f File,
        pos: u64,
        end: u64,
    }

    impl Read for Range<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            if self.pos >= self.end {
                return Ok(0);
            }
            let want = buf.len().min((self.end - self.pos) as usize);
            let n = read_at(self.file, &mut buf[..want], self.pos)?;
            self.pos += n as u64;
            Ok(n)
        }
    }

    /// The start of the first line beginning in `x..x + 64 KiB` (just past a newline), if any: a
    /// longer line is left whole inside a bigger range rather than read twice to find its end.
    fn line_start(f: &File, x: u64, buf: &mut [u8]) -> std::io::Result<Option<u64>> {
        let n = read_at(f, buf, x - 1)?;
        Ok(memchr::memchr(b'\n', &buf[..n]).map(|i| x + i as u64))
    }

    /// Newlines in `a..b`; None when the range holds a NUL byte, a read fails or `stop` is set.
    fn newlines(f: &File, a: u64, b: u64, buf: &mut [u8], stop: &AtomicBool) -> Option<u64> {
        let mut r = Range { file: f, pos: a, end: b };
        let mut n = 0u64;
        loop {
            let got = r.read(buf).ok()?;
            if got == 0 {
                return Some(n);
            }
            if stop.load(Ordering::Relaxed) || memchr::memchr(0, &buf[..got]).is_some() {
                stop.store(true, Ordering::Relaxed);
                return None;
            }
            n += memchr::memchr_iter(b'\n', &buf[..got]).count() as u64;
        }
    }

    pub(super) enum Outcome {
        /// searched and reported
        Done,
        /// nothing reported: search the file as one stream
        Stream,
        /// nothing reported: the file has no line breaks to split at and no NUL byte, so here it is in
        /// one buffer of its exact size (a stream would grow its buffer by doubling, copying each time)
        Buffer(Vec<u8>),
    }

    /// What one read of a file's first `SMALL_FILE + 1` bytes found.
    pub(super) enum First {
        /// the whole file, this many bytes, in the buffer
        Whole(usize),
        /// more than `SMALL_FILE` bytes
        Larger,
        /// a small file for the stream reader: it starts with a byte-order mark (only that reader
        /// transcodes), or it cannot be read at an offset
        Other,
    }

    /// The rest of a file of `len` bytes whose first `SMALL_FILE + 1` are in `buf` (`first` said
    /// `Larger`): the whole file's length, unless it holds a NUL byte or starts with a byte-order
    /// mark (the stream reader's cases) or is no longer `len` bytes long.
    pub(super) fn rest(file: &File, len: usize, buf: &mut Vec<u8>) -> Option<usize> {
        let mut at = SMALL_FILE as usize + 1;
        if len < at {
            return None;
        }
        if buf.len() < len {
            buf.resize(MID_FILE as usize, 0);
        }
        while at < len {
            match read_at(file, &mut buf[at..len], at as u64) {
                Ok(0) | Err(_) => break,
                Ok(n) => at += n,
            }
        }
        #[cfg(not(unix))]
        let _ = std::io::Seek::seek(&mut &*file, std::io::SeekFrom::Start(0));
        let b = &buf[..len];
        let bom = b.starts_with(&[0xEF, 0xBB, 0xBF]) || b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]);
        (at == len && !bom && memchr::memchr(0, b).is_none()).then_some(len)
    }

    /// Reads into `buf`, kept by the thread across files (sized once, never zeroed again).
    pub(super) fn first(file: &File, buf: &mut Vec<u8>) -> First {
        let max = SMALL_FILE as usize;
        if buf.len() <= max {
            buf.resize(max + 1, 0);
        }
        let read = read_at(file, &mut buf[..max + 1], 0);
        // Windows moves the file's position on a read at an offset; the stream reader starts at it
        #[cfg(not(unix))]
        let _ = std::io::Seek::seek(&mut &*file, std::io::SeekFrom::Start(0));
        match read {
            Ok(n) if n > max => First::Larger,
            Ok(n) => {
                let b = &buf[..n];
                let bom = b.starts_with(&[0xEF, 0xBB, 0xBF]) || b.starts_with(&[0xFF, 0xFE]) || b.starts_with(&[0xFE, 0xFF]);
                if bom {
                    First::Other
                } else {
                    First::Whole(n)
                }
            }
            Err(_) => First::Other,
        }
    }

    /// The whole file when it holds no NUL byte (a slice search checks only its start for binary data).
    fn whole(file: &File, size: u64) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; usize::try_from(size).ok()?];
        let mut at = 0;
        while at < buf.len() {
            // a piece at a time, so the NUL check reads it while it is still in cache
            let end = buf.len().min(at + (1 << 20));
            match read_at(file, &mut buf[at..end], at as u64) {
                Ok(0) => break,
                Ok(n) if memchr::memchr(0, &buf[at..at + n]).is_some() => return None,
                Ok(n) => at += n,
                Err(_) => return None,
            }
        }
        buf.truncate(at);
        Some(buf)
    }

    struct Part {
        out: Vec<u8>,
        hits: Vec<Hit>,
        matched: bool,
        count: u64,
        /// the search reached the end of the range
        ok: bool,
    }

    /// Search `file` (`size` bytes) as ranges of whole lines, `chunk` bytes or so each, on several
    /// threads, and report the results in file order exactly as one sequential pass would. A first
    /// pass counts the newlines of every range (for the line numbers). Reports nothing when the file
    /// must be searched sequentially after all: it starts with a byte-order mark (transcoded as one
    /// stream), holds a NUL byte (binary detection), a read failed, or it has no line breaks to split at.
    #[allow(clippy::too_many_arguments)]
    pub(super) fn search(
        file: &File,
        size: u64,
        chunk: u64,
        rel: &Arc<str>,
        matcher: &AnyMatcher,
        o: &Options,
        c: &Counters,
        cancel: &AtomicBool,
        tx: &mpsc::Sender<Vec<Hit>>,
        plain: Option<&PlainSink>,
    ) -> Outcome {
        let mut head = [0u8; 3];
        match read_at(file, &mut head, 0) {
            Ok(n) if n >= 2 && (head[..2] == [0xFF, 0xFE] || head[..2] == [0xFE, 0xFF] || (n == 3 && head == [0xEF, 0xBB, 0xBF])) => return Outcome::Stream,
            Ok(_) => {}
            Err(_) => return Outcome::Stream,
        }
        let mut bounds = vec![0u64];
        let mut probe = vec![0u8; 64 << 10];
        let mut x = chunk;
        while x < size {
            match line_start(file, x, &mut probe) {
                Ok(Some(b)) if b < size => {
                    bounds.push(b);
                    x = b + chunk;
                }
                Ok(_) => x += chunk,
                Err(_) => return Outcome::Stream,
            }
        }
        bounds.push(size);
        if bounds.len() < 3 {
            return whole(file, size).map_or(Outcome::Stream, Outcome::Buffer);
        }
        let ranges: Vec<(u64, u64)> = bounds.windows(2).map(|w| (w[0], w[1])).collect();
        let threads = range_threads(o).min(ranges.len());

        let next = AtomicUsize::new(0);
        let stop = AtomicBool::new(false);
        let counted: Vec<Option<Vec<(usize, u64)>>> = std::thread::scope(|s| {
            let hs: Vec<_> = (0..threads)
                .map(|_| {
                    s.spawn(|| {
                        let mut buf = vec![0u8; 1 << 20];
                        let mut mine = Vec::new();
                        loop {
                            let i = next.fetch_add(1, Ordering::Relaxed);
                            let Some(&(a, b)) = ranges.get(i) else { return Some(mine) };
                            mine.push((i, newlines(file, a, b, &mut buf, &stop)?));
                        }
                    })
                })
                .collect();
            hs.into_iter().map(|h| h.join().ok().flatten()).collect()
        });
        // a NUL byte (binary handling stays sequential) or a read error: no line numbers to trust
        let Ok(per_thread) = counted.into_iter().map(|w| w.ok_or(())).collect::<Result<Vec<_>, ()>>() else {
            return Outcome::Stream;
        };
        let mut lines = vec![0u64; ranges.len()];
        for (i, n) in per_thread.concat() {
            lines[i] = n;
        }
        let mut bases = Vec::with_capacity(ranges.len());
        let mut acc = 0u64;
        for n in &lines {
            bases.push(acc);
            acc += n;
        }

        // one window of ranges at a time, so the output held back for ordering stays bounded
        let mut matched = false;
        let mut count = 0u64;
        'windows: for (rs, bs) in ranges.chunks(threads).zip(bases.chunks(threads)) {
            let parts: Vec<Part> = std::thread::scope(|s| {
                let hs: Vec<_> = rs
                    .iter()
                    .zip(bs)
                    .map(|(&(a, b), &base)| s.spawn(move || search_range(file, a, b, base, rel, matcher, o, c, cancel, plain.is_some())))
                    .collect();
                hs.into_iter().map(|h| h.join().unwrap_or(Part { out: Vec::new(), hits: Vec::new(), matched: false, count: 0, ok: false })).collect()
            });
            for part in parts {
                matched |= part.matched;
                count += part.count;
                if let Some(p) = plain {
                    p.write(&part.out, cancel);
                } else if !part.hits.is_empty() {
                    let _ = tx.send(part.hits);
                }
                if !part.ok {
                    // the file changed after the first pass, or a read failed: what was found so far stands
                    c.failed(format!("{rel}: changed while it was searched, or a read failed"));
                    break 'windows;
                }
            }
            if cancel.load(Ordering::Relaxed) {
                break;
            }
        }
        if matched {
            c.files_matched.fetch_add(1, Ordering::Relaxed);
            if o.count_only {
                let _ = tx.send(vec![Hit { path: Arc::clone(rel), line: count, column: 0, text: String::new(), spans: Vec::new(), context: false }]);
            }
        }
        Outcome::Done
    }

    #[allow(clippy::too_many_arguments)]
    fn search_range(
        file: &File,
        a: u64,
        b: u64,
        base: u64,
        rel: &Arc<str>,
        matcher: &AnyMatcher,
        o: &Options,
        c: &Counters,
        cancel: &AtomicBool,
        plain: bool,
    ) -> Part {
        // its own matcher (its own regex cache pool) and its own searcher, without transcoding: the
        // middle of a file is not the start of a stream
        let matcher = matcher.clone();
        let mut searcher =
            SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).bom_sniffing(false).line_number(!o.count_only).invert_match(o.invert).build();
        let (ptx, prx) = mpsc::channel::<Vec<Hit>>();
        // with `hold` the plain text is formatted into `out` and never written to this sink
        let sink_plain = PlainSink::new(Box::new(std::io::sink()));
        let mut sink = Collector {
            path: Arc::clone(rel),
            tx: &ptx,
            batch: Vec::new(),
            plain: if plain { Some(&sink_plain) } else { None },
            out: Vec::new(),
            want_spans: o.want_spans,
            stats_matches: &c.matches,
            max_results: None,
            max_count: None,
            matched: false,
            cancel,
            matcher: &matcher,
            invert: o.invert,
            count_only: o.count_only,
            files_only: false,
            file_count: 0,
            line_base: base,
            hold: true,
            binary: false,
            multiline: false, // never split in multiline mode
        };
        let mut range = Range { file, pos: a, end: b };
        let res = match &matcher {
            AnyMatcher::Regex(m) => searcher.search_reader(m, &mut range, &mut sink),
            AnyMatcher::Fuzzy(m) => searcher.search_reader(m.as_ref(), &mut range, &mut sink),
            AnyMatcher::Pcre2(m) => searcher.search_reader(m, &mut range, &mut sink),
            AnyMatcher::FuzzyRegex(m) => searcher.search_reader(m.as_ref(), &mut range, &mut sink),
        };
        sink.flush();
        let ok = res.is_ok() && !sink.binary && (range.pos >= b || cancel.load(Ordering::Relaxed));
        let (out, matched, count) = (std::mem::take(&mut sink.out), sink.matched, sink.file_count);
        drop(sink);
        drop(ptx);
        Part { out, hits: prx.into_iter().flatten().collect(), matched, count, ok }
    }
}

/// Hand the hits to the caller in arrival order, applying the per-file `max_count`.
fn drain(rx: mpsc::Receiver<Vec<Hit>>, o: &Options, mut on_hit: impl FnMut(Hit)) {
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
}

fn stats(c: &Counters, t0: Instant, cancel: &AtomicBool) -> Stats {
    Stats {
        files_searched: c.files_searched.load(Ordering::Relaxed),
        files_matched: c.files_matched.load(Ordering::Relaxed),
        matches: c.matches.load(Ordering::Relaxed),
        bytes_searched: c.bytes.load(Ordering::Relaxed),
        errors: c.errors.load(Ordering::Relaxed),
        elapsed_ms: t0.elapsed().as_secs_f64() * 1000.0,
        cancelled: cancel.load(Ordering::Relaxed),
        failures: c.failures.lock().map(|v| v.clone()).unwrap_or_default(),
    }
}

/// Search `roots`, sending every hit to `on_hit` as it is found (from the calling thread). Returns
/// statistics; `cancel` may be set from another thread (or by `max_results`) to stop early.
pub fn search(roots: &[PathBuf], o: &Options, cancel: Arc<AtomicBool>, on_hit: impl FnMut(Hit)) -> Result<Stats> {
    run_walk(roots, o, cancel, None, on_hit)
}

/// `search` with plain output written straight to `out` by the search threads (no `Hit`s).
/// Only for plain match output: not with counts, file lists, `max_count` or JSON.
pub fn search_plain(roots: &[PathBuf], o: &Options, cancel: Arc<AtomicBool>, out: Arc<PlainSink>) -> Result<Stats> {
    run_walk(roots, o, cancel, Some(out), |_| {})
}

fn run_walk(roots: &[PathBuf], o: &Options, cancel: Arc<AtomicBool>, plain: Option<Arc<PlainSink>>, on_hit: impl FnMut(Hit)) -> Result<Stats> {
    let t0 = Instant::now();
    if roots.is_empty() {
        bail!("no paths to search");
    }
    let matcher = Arc::new(build_matcher(o)?);
    let counters = Arc::new(Counters::default());
    let mut on_hit = on_hit;
    let searched = match small_first(roots, o, &matcher, &counters, &cancel, plain.as_deref(), &mut on_hit) {
        Small::Done => return Ok(stats(&counters, t0, &cancel)),
        Small::Big(seen) => Arc::new(seen),
    };
    let threads = walk_threads(o, &counters);
    // set once the reads look like disk reads: the walk then grows to `cold_threads`
    let cold = Arc::new(AtomicBool::new(false));
    let mut wb = walk_builder(roots, o)?;
    wb.threads(threads);
    if o.threads == 0 {
        wb.grow_threads(cold_threads().saturating_sub(threads), Arc::clone(&cold));
    }
    let walker = wb.build_parallel();
    let (tx, rx) = mpsc::channel::<Vec<Hit>>();
    let roots_owned: Arc<[PathBuf]> = roots.into();
    let opts = Arc::new(o.clone());
    let (c2, cancel2) = (Arc::clone(&counters), Arc::clone(&cancel));
    let worker = std::thread::spawn(move || {
        walker.run(|| {
            let tx = tx.clone();
            // a clone per thread: each gets its own regex cache pool instead of contending for one
            let matcher = AnyMatcher::clone(&matcher);
            let cancel = Arc::clone(&cancel2);
            let c = Arc::clone(&c2);
            let opts = Arc::clone(&opts);
            let roots = Arc::clone(&roots_owned);
            let plain = plain.clone();
            let searched = Arc::clone(&searched);
            let cold = Arc::clone(&cold);
            // made on the first file: the threads kept for a cold walk mostly never run
            let mut fw = None;
            Box::new(move |entry| {
                if cancel.load(Ordering::Relaxed) {
                    return WalkState::Quit;
                }
                let entry = match entry {
                    Ok(e) => e,
                    Err(e) => {
                        c.failed(e.to_string());
                        return WalkState::Continue;
                    }
                };
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) || (!searched.is_empty() && searched.contains(entry.path())) {
                    return WalkState::Continue;
                }
                let size = if opts.want_bytes { entry.metadata().ok().map(|m| m.len()) } else { None };
                let rel: Arc<str> = Arc::from(relative(entry.path(), &roots));
                let fw = fw.get_or_insert_with(|| file_worker(&opts));
                search_one(fw, &matcher, entry.path(), rel, size, &opts, &c, &cancel, &tx, plain.as_deref());
                if !cold.load(Ordering::Relaxed) && reads_from_disk(&c) {
                    cold.store(true, Ordering::Relaxed);
                }
                if cancel.load(Ordering::Relaxed) {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });
    });
    drain(rx, o, &mut on_hit);
    worker.join().map_err(|_| anyhow!("search worker panicked"))?;
    Ok(stats(&counters, t0, &cancel))
}

/// Search the given files (paths relative to `root`, as an index lists them) with exactly the
/// per-file logic of `search`; glob and type filters of `o` apply to these paths. Used by the
/// indexed path, which has already chosen the files that can match.
pub fn search_files(root: &Path, files: Vec<String>, o: &Options, cancel: Arc<AtomicBool>, on_hit: impl FnMut(Hit)) -> Result<Stats> {
    run_files(root, files, o, cancel, None, on_hit)
}

/// `search_files` with plain output (see `search_plain`).
pub fn search_files_plain(root: &Path, files: Vec<String>, o: &Options, cancel: Arc<AtomicBool>, out: Arc<PlainSink>) -> Result<Stats> {
    run_files(root, files, o, cancel, Some(out), |_| {})
}

fn run_files(root: &Path, files: Vec<String>, o: &Options, cancel: Arc<AtomicBool>, plain: Option<Arc<PlainSink>>, on_hit: impl FnMut(Hit)) -> Result<Stats> {
    let t0 = Instant::now();
    let matcher = build_matcher(o)?;
    let ov = overrides(root, o)?;
    let files: Vec<(PathBuf, Arc<str>)> = files.into_iter().filter(|rel| !filtered_out(&ov, rel)).map(|rel| (root.join(&rel), Arc::from(rel))).collect();
    let threads = default_threads(o).min(files.len().max(1));
    Ok(search_list(files, threads, &matcher, o, &cancel, plain.as_deref(), on_hit, t0))
}

/// Search a known list of files (path, path as reported) on `threads` threads.
#[allow(clippy::too_many_arguments)]
fn search_list(
    files: Vec<(PathBuf, Arc<str>)>,
    threads: usize,
    matcher: &AnyMatcher,
    o: &Options,
    cancel: &AtomicBool,
    plain: Option<&PlainSink>,
    on_hit: impl FnMut(Hit),
    t0: Instant,
) -> Stats {
    let (tx, rx) = mpsc::channel::<Vec<Hit>>();
    let counters = Counters::default();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads {
            let tx = tx.clone();
            let (counters, next, files) = (&counters, &next, &files);
            // a clone per thread: each gets its own regex cache pool instead of contending for one
            let matcher = matcher.clone();
            s.spawn(move || {
                let matcher = &matcher;
                let mut fw = file_worker(o);
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    if i >= files.len() || cancel.load(Ordering::Relaxed) {
                        break;
                    }
                    let (path, rel) = &files[i];
                    let size = if o.want_bytes { std::fs::metadata(path).ok().map(|m| m.len()) } else { None };
                    search_one(&mut fw, matcher, path, Arc::clone(rel), size, o, counters, cancel, &tx, plain);
                }
            });
        }
        drop(tx);
        drain(rx, o, on_hit);
    });
    stats(&counters, t0, cancel)
}

/// True when the walk with these overrides would not reach `rel` (an ancestor directory or the
/// file itself is excluded).
fn filtered_out(ov: &ignore::overrides::Override, rel: &str) -> bool {
    if ov.is_empty() {
        return false;
    }
    let mut at = 0;
    while let Some(i) = rel[at..].find('/') {
        if ov.matched(&rel[..at + i], true).is_ignore() {
            return true;
        }
        at += i + 1;
    }
    ov.matched(rel, false).is_ignore()
}

pub fn relative(path: &Path, roots: &[PathBuf]) -> String {
    for r in roots {
        if let Ok(rel) = path.strip_prefix(r) {
            let s = rel.to_string_lossy().replace('\\', "/");
            if s.is_empty() {
                // the root is the file itself
                return r.to_string_lossy().replace('\\', "/");
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

    #[test]
    fn write_lossy_equals_from_utf8_lossy() {
        let parts: &[&[u8]] = &[b"a", b"\xC3\xA9", b"\xE6\x97\xA5", b"\xF0\x9F\x98\x80", b"\xFF", b"\xC3", b"\xE6\x97", b"\xF0\x9F", b"\x80", b"\n"];
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: usize| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state % n as u64) as usize
        };
        for _ in 0..20_000 {
            let mut b = Vec::new();
            for _ in 0..next(12) {
                b.extend_from_slice(parts[next(parts.len())]);
            }
            let piece = 1 + next(9);
            let mut out = Vec::new();
            write_lossy(&mut out, &b, piece).unwrap();
            assert_eq!(out, String::from_utf8_lossy(&b).as_bytes(), "{b:?} piece {piece}");
        }
    }

    #[test]
    fn suffix_candidates_find_the_same_lines() {
        let words = [
            "dev_resume(",
            "x_suspend(",
            " _resume(",
            "KÖNIG",
            "königin",
            "pm_RESUME(",
            "rc",
            "\t",
            "ing ",
            "sing\n",
            "12px",
            "é_resume(",
            "abc",
            "_",
            "(",
            "\n",
            "\r\n",
            "\u{212A}önig",
        ];
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut hay = String::new();
        for _ in 0..20000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            hay.push_str(words[(state % words.len() as u64) as usize]);
        }
        let lines = |m: &AnyMatcher, invert: bool| {
            let mut v = Vec::new();
            let mut s = SearcherBuilder::new().line_number(true).invert_match(invert).build();
            let AnyMatcher::Regex(m) = m else { unreachable!() };
            s.search_slice(
                m,
                hay.as_bytes(),
                grep_searcher::sinks::Lossy(|n, _| {
                    v.push(n);
                    Ok(true)
                }),
            )
            .unwrap();
            v
        };
        let mut engaged = 0;
        for (pat, case) in [
            (r"\w+_(suspend|resume)\(", Case::Sensitive),
            (r"\w+_resume\(", Case::Insensitive),
            (r"\w+KÖNIG", Case::Insensitive),
            (r"[a-z]+ing\b", Case::Sensitive),
            (r"\d+px", Case::Sensitive),
            (r".*abc", Case::Sensitive),
            (r"\S*_resume\($", Case::Smart),
            (r"PM_RESUME", Case::Insensitive),
        ] {
            let o = Options { pattern: pat.to_string(), case, ..Options::default() };
            let fast = build_matcher(&o).unwrap();
            let AnyMatcher::Regex(ref f) = fast else { unreachable!() };
            engaged += f.suffixes.is_some() as usize;
            let slow = AnyMatcher::Regex(LineRegex::plain(f.inner.clone()));
            for invert in [false, true] {
                let (a, b) = (lines(&fast, invert), lines(&slow, invert));
                assert_eq!(a, b, "{pat} {case:?} invert={invert}");
                assert!(invert || !a.is_empty(), "{pat}: no matches, the test proves nothing");
            }
        }
        // the rest have a short suffix, a literal prefix, or too many case variants (s folds to ſ too)
        assert_eq!(engaged, 5);
        let o = Options { pattern: "PM_RESUME".into(), ..Options::default() };
        let AnyMatcher::Regex(m) = build_matcher(&o).unwrap() else { unreachable!() };
        assert!(m.suffixes.is_none(), "a literal prefix is the engine's job");
    }

    #[test]
    fn line_start_candidates_find_the_same_lines() {
        let words = ["a", "aa", "aaa", "b", "!", "\n", "\r\n", "#include <x>", "x", "A", "é", "É", " ", "\t", "ab", "\n\n", "\nab"];
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut hay = String::new();
        for _ in 0..20000 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            hay.push_str(words[(state % words.len() as u64) as usize]);
        }
        let lines = |m: &AnyMatcher, invert: bool| {
            let mut v = Vec::new();
            let mut s = SearcherBuilder::new().line_number(true).invert_match(invert).build();
            let AnyMatcher::Regex(m) = m else { unreachable!() };
            s.search_slice(
                m,
                hay.as_bytes(),
                grep_searcher::sinks::Lossy(|n, _| {
                    v.push(n);
                    Ok(true)
                }),
            )
            .unwrap();
            v
        };
        let mut engaged = 0;
        for (pat, case) in [
            (r"^(a|aa)+$", Case::Sensitive),
            (r"^a", Case::Sensitive),
            (r"^#include", Case::Sensitive),
            (r"^(?:a|b)+!", Case::Insensitive),
            (r"^é", Case::Insensitive),
            (r"^ab?\s*$", Case::Sensitive),
            (r"^a$|^b$", Case::Sensitive),
            (r"^$", Case::Sensitive),
            (r"^\w", Case::Sensitive),
            (r"^a|b", Case::Sensitive),
            (r"^a\z", Case::Sensitive),
            (r"^a\s*", Case::Sensitive),
            (r"^(?:a|ab)(?:\n|b)", Case::Sensitive),
            (r"^a[^x]", Case::Sensitive),
        ] {
            let o = Options { pattern: pat.to_string(), case, ..Options::default() };
            let fast = build_matcher(&o).unwrap();
            let AnyMatcher::Regex(ref f) = fast else { unreachable!() };
            engaged += f.line_start.is_some() as usize;
            let slow = AnyMatcher::Regex(LineRegex::plain(f.inner.clone()));
            for invert in [false, true] {
                let (a, b) = (lines(&fast, invert), lines(&slow, invert));
                assert_eq!(a, b, "{pat} {case:?} invert={invert}");
                assert!(invert || !a.is_empty(), "{pat}: no matches, the test proves nothing");
            }
            for line in hay.split('\n').take(3000) {
                assert_eq!(spans_in(&fast, line.as_bytes()), spans_in(&slow, line.as_bytes()), "{pat} on {line:?}");
            }
            assert_eq!(spans_in(&fast, hay.as_bytes()), spans_in(&slow, hay.as_bytes()), "{pat} on the whole text");
            // every short text over a small alphabet, from every offset
            let (AnyMatcher::Regex(fm), AnyMatcher::Regex(sm)) = (&fast, &slow) else { unreachable!() };
            let alphabet: [&[u8]; 5] = [b"a", b"b", b"\n", b"!", "é".as_bytes()];
            let mut texts: Vec<Vec<u8>> = vec![Vec::new()];
            for _ in 0..4 {
                let more: Vec<Vec<u8>> = texts.iter().flat_map(|t| alphabet.iter().map(move |c| [t.as_slice(), c].concat())).collect();
                texts.extend(more);
            }
            for t in &texts {
                for at in 0..=t.len() {
                    assert_eq!(fm.find_at(t, at).unwrap(), sm.find_at(t, at).unwrap(), "{pat} find_at {t:?} {at}");
                    assert_eq!(fm.shortest_match_at(t, at).unwrap().is_some(), sm.shortest_match_at(t, at).unwrap().is_some(), "{pat} {t:?} {at}");
                }
            }
        }
        // `^$`, `^\w` (no finite prefix literals), `^a|b` (not anchored), `^a\z` and a line break in
        // the prefix keep the plain path
        assert_eq!(engaged, 9);
    }

    fn fuzzy_find(pat: &str, k: u8, hay: &str) -> Option<(usize, usize)> {
        let m = FuzzyMatcher::new(pat, k, false).unwrap();
        m.find_at(hay.as_bytes(), 0).unwrap().map(|m| (m.start(), m.end()))
    }

    /// First end (and distance) of a match within `k` edits: the textbook dynamic programme.
    fn naive_first_end(pat: &[u8], hay: &[u8], k: usize) -> Option<(usize, usize)> {
        let m = pat.len();
        let mut col: Vec<usize> = (0..=m).collect();
        for (i, &c) in hay.iter().enumerate() {
            let mut next = vec![0usize; m + 1];
            for j in 1..=m {
                next[j] = (next[j - 1] + 1).min(col[j] + 1).min(col[j - 1] + usize::from(pat[j - 1] != c));
            }
            col = next;
            if col[m] <= k {
                return Some((i + 1, col[m]));
            }
        }
        None
    }

    #[test]
    fn fuzzy_blocks_agree_with_the_dynamic_programme() {
        let mut state = 0x2545F4914F6CDD1Du64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        for round in 0..400 {
            let m = 1 + next(300) as usize;
            let pat: Vec<u8> = (0..m).map(|_| b"abc"[next(3) as usize]).collect();
            // text: the pattern with a few edits, inside noise, so matches at small distances exist
            let mut hay: Vec<u8> = (0..next(200)).map(|_| b"abcd"[next(4) as usize]).collect();
            let mut copy = pat.clone();
            for _ in 0..next(6) {
                let at = next(copy.len() as u64) as usize;
                match next(3) {
                    0 => copy[at] = b'd',
                    1 => drop(copy.remove(at)),
                    _ => copy.insert(at, b'a'),
                }
                if copy.is_empty() {
                    copy.push(b'b');
                }
            }
            hay.extend_from_slice(&copy);
            hay.extend((0..next(50)).map(|_| b"abcd"[next(4) as usize]));
            let k = (next(8) as usize).min(m - 1);
            let f = FuzzyMatcher::new(std::str::from_utf8(&pat).unwrap(), k as u8, false).unwrap();
            assert_eq!(f.first_end(&hay, 0), naive_first_end(&pat, &hay, k), "round {round}: m={m} k={k}");
            if m <= 128 {
                // the block path on short patterns too, against the single-word one
                let b = Blocks::new(&pat, false);
                let (mut pv, mut mv) = b.start();
                let mut score = m as isize;
                let mut got = None;
                for (i, &c) in hay.iter().enumerate() {
                    score += b.step(&mut pv, &mut mv, c);
                    if score <= k as isize {
                        got = Some((i + 1, score as usize));
                        break;
                    }
                }
                assert_eq!(got, naive_first_end(&pat, &hay, k), "blocks, round {round}: m={m} k={k}");
            }
        }
        assert!(FuzzyMatcher::new(&"a".repeat(FUZZY_MAX + 1), 1, false).is_err());
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
