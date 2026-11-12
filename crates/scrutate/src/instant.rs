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
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// globs matched from one directory, whatever path is searched (a configuration file's)
    pub scoped_globs: Option<Arc<ScopedGlobs>>,
    /// language ids / type names from the registry (`py`, `rust`, ...); negative with a leading `!`
    pub types: Vec<String>,
    pub max_filesize: Option<u64>,
    pub max_depth: Option<usize>,
    pub max_count: Option<u64>,
    pub max_results: Option<u64>,
    /// the hits arrive in the order of `rg --sort path`: the roots as given, each one's files by
    /// their path compared part by part (`a/b` before `a-b`), then by line; with `max_results`
    /// they are exactly the first `max_results` matches in that order, the same on every run
    /// (files are listed and sorted first, and searched in that order until enough matches are
    /// in). Off, a capped search keeps whichever matches it finds first. Needs a hit sink:
    /// `search_plain` refuses it
    pub first_in_path_order: bool,
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
    /// archive member names are reported as they are (for JSON, which escapes them); off, with
    /// control characters shown as escapes (`fmt::shown`), so a name cannot forge an output line
    pub raw_member_names: bool,
    /// report each path as the walk reached it from the root it was given (`dir/a.rs`, `./a.rs`),
    /// as grep does; otherwise relative to the root. Also as ripgrep: a root that is a file is
    /// searched whatever its bytes (a NUL byte ends a line, and line output stops at the first
    /// match after one with a `NOTE_LINE` hit), and the root `-` is standard input
    pub paths_as_given: bool,
    /// plain output starts each line with the file's path (`-H`); off, `line:text` as for one file
    pub with_filename: bool,
    /// plain output has line numbers (`-N` turns them off; hits always carry them)
    pub line_numbers: bool,
    /// files under these paths are read until a read returns nothing (procfs and sysfs return a
    /// page or less per read); set by the search from its roots, on Linux (the walk adds those it
    /// finds below them)
    pub read_to_end_under: Vec<PathBuf>,
    /// regex size limits (resource exhaustion protection)
    pub regex_size_limit: usize,
    pub dfa_size_limit: usize,
    /// files at least this large are searched as several ranges of whole lines at once (an eighth of
    /// it each): one huge file would otherwise keep a single core busy while the others idle
    pub split_file_bytes: u64,
    /// files the one-thread walk takes before the parallel walk does the rest (0 skips it); `None`
    /// chooses from the roots (`small_tree_files`)
    pub small_tree: Option<usize>,
    /// the process's current directory as `std::env::current_dir` gave it, when the caller has it:
    /// the walk then does not look it up again (0.17 M instructions on macOS)
    pub cwd: Option<PathBuf>,
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
            scoped_globs: None,
            types: Vec::new(),
            max_filesize: None,
            max_depth: None,
            max_count: None,
            max_results: None,
            first_in_path_order: false,
            before: 0,
            after: 0,
            threads: 0,
            count_only: false,
            files_only: false,
            want_bytes: false,
            want_spans: false,
            search_zip: false,
            raw_member_names: false,
            paths_as_given: false,
            with_filename: true,
            line_numbers: true,
            read_to_end_under: Vec::new(),
            regex_size_limit: 20 * (1 << 20),
            dfa_size_limit: 10 * (1 << 20),
            split_file_bytes: 64 << 20,
            small_tree: None,
            cwd: None,
        }
    }
}

/// One result record. `line` is 1-based; `column` is the 1-based byte offset of the first match in
/// the line (0 when not applicable, e.g. inverted matches). A `line` of `NOTE_LINE` is a note on
/// the file, its text in `text`: a named file with a NUL byte matched (`Options::paths_as_given`),
/// or the search of a walked file that matched stopped at a NUL byte.
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

/// The `line` of a `Hit` that is a note on its file, not one of its lines (sorts after them).
pub const NOTE_LINE: u64 = u64::MAX;

/// The path a search reports for standard input.
pub const STDIN_LABEL: &str = "<stdin>";

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
/// Approximate matching of a literal (Myers 1999, bit-parallel). Lines are never spanned. An edit
/// is one character: an ASCII byte, a UTF-8 encoded character, or a byte of invalid UTF-8 (which
/// no pattern character equals). A pigeonhole prefilter (one of k+1 pieces of the pattern must
/// occur verbatim) finds candidate lines with a multi-substring scan, so only those lines pay for
/// the edit-distance computation.
#[derive(Clone)]
pub struct FuzzyMatcher {
    fwd: Peq,
    /// the pattern reversed, for the start of a match
    rev: Peq,
    m: usize,
    k: usize,
    prefilter: Option<aho_corasick::AhoCorasick>,
    /// patterns longer than 128 characters: the pattern and its reverse in 64-bit blocks
    long: Option<Box<(Blocks, Blocks)>>,
}

/// The longest fuzzy pattern accepted, in characters (the work per character grows with its length / 64).
pub const FUZZY_MAX: usize = 4096;

/// The non-ASCII character at `hay[i]` and its length, or `None` and 1 for a byte of invalid UTF-8.
#[inline]
pub(crate) fn wide_char(hay: &[u8], i: usize) -> (Option<char>, usize) {
    let n = match hay[i] {
        0xC2..=0xDF => 2,
        0xE0..=0xEF => 3,
        0xF0..=0xF4 => 4,
        _ => return (None, 1),
    };
    match hay.get(i..i + n).and_then(|b| std::str::from_utf8(b).ok()) {
        Some(s) => (s.chars().next(), n),
        None => (None, 1),
    }
}

#[inline]
fn unit_len(hay: &[u8], i: usize) -> usize {
    if hay[i] < 0x80 {
        1
    } else {
        wide_char(hay, i).1
    }
}

/// The characters equal to `c` under case folding: the simple case folding of the regex engine,
/// so `-i` means the same with and without `--fuzzy`.
fn fold_class(c: char) -> Vec<char> {
    use regex_syntax::hir::{ClassUnicode, ClassUnicodeRange};
    let mut cls = ClassUnicode::new([ClassUnicodeRange::new(c, c)]);
    if cls.try_case_fold_simple().is_err() {
        return vec![c];
    }
    cls.iter().flat_map(|r| (r.start() as u32..=r.end() as u32).filter_map(char::from_u32)).collect()
}

/// For each pattern position, the characters that match it.
fn fuzzy_classes(pattern: &str, fold: bool) -> Vec<Vec<char>> {
    pattern.chars().map(|c| if fold { fold_class(c) } else { vec![c] }).collect()
}

/// Strings of which every match of the fuzzy pattern holds one verbatim, up to ASCII case when
/// `fold`: `max_edits` edits change at most that many of `max_edits + 1` pieces of the pattern.
/// With `fold` a piece is given in each spelling that differs beyond ASCII case (`é`/`É`, `k` and
/// the Kelvin sign). None when the pieces would be empty or there would be more than 64 strings.
pub fn fuzzy_pieces(pattern: &str, max_edits: u8, fold: bool) -> Option<Vec<Vec<u8>>> {
    let classes = fuzzy_classes(pattern, fold);
    let n = max_edits as usize + 1;
    let len = classes.len() / n;
    if len == 0 {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for i in 0..n {
        let mut spellings = vec![String::new()];
        for cls in &classes[i * len..if i == n - 1 { classes.len() } else { (i + 1) * len }] {
            let mut alts: Vec<char> = cls.iter().copied().filter(|c| !c.is_ascii()).collect();
            alts.extend(cls.iter().copied().find(char::is_ascii));
            spellings = spellings.iter().flat_map(|s| alts.iter().map(move |a| format!("{s}{a}"))).collect();
            if out.len() + spellings.len() > 64 {
                return None;
            }
        }
        out.extend(spellings);
    }
    Some(out.into_iter().map(String::into_bytes).collect())
}

/// Myers' match masks of a pattern of up to 128 characters: bit j is set for the characters that
/// match position j.
#[derive(Clone)]
struct Peq {
    ascii: [u128; 128],
    /// sorted by character
    wide: Vec<(char, u128)>,
}

impl Peq {
    fn new(classes: &[Vec<char>]) -> Peq {
        let mut p = Peq { ascii: [0; 128], wide: Vec::new() };
        for (j, cls) in classes.iter().enumerate().take(128) {
            for &c in cls {
                if c.is_ascii() {
                    p.ascii[c as usize] |= 1 << j;
                } else if let Some(e) = p.wide.iter_mut().find(|e| e.0 == c) {
                    e.1 |= 1 << j;
                } else {
                    p.wide.push((c, 1 << j));
                }
            }
        }
        p.ascii[b'\n' as usize] = 0;
        p.wide.sort_by_key(|e| e.0);
        p
    }

    /// The mask of the character at `hay[i]` and its length.
    #[inline]
    fn at(&self, hay: &[u8], i: usize) -> (u128, usize) {
        let b = hay[i];
        if b < 0x80 {
            return (self.ascii[b as usize], 1);
        }
        let (c, n) = wide_char(hay, i);
        (c.and_then(|c| self.wide.binary_search_by_key(&c, |e| e.0).ok()).map_or(0, |x| self.wide[x].1), n)
    }
}

/// Myers' recurrence on 64-bit blocks, for patterns longer than one machine word: the addition
/// and the two shifts carry from block to block (Hyyrö 2003).
#[derive(Clone)]
struct Blocks {
    words: usize,
    m: usize,
    /// `words` masks per ASCII byte
    ascii: Vec<u64>,
    /// the non-ASCII characters of the pattern, sorted; `rows` holds `words` masks for each
    wide: Vec<char>,
    rows: Vec<u64>,
    zero: Vec<u64>,
}

impl Blocks {
    fn new(classes: &[Vec<char>]) -> Blocks {
        let words = classes.len().div_ceil(64);
        let mut ascii = vec![0u64; 128 * words];
        let mut wide: std::collections::BTreeMap<char, Vec<u64>> = Default::default();
        for (j, cls) in classes.iter().enumerate() {
            for &c in cls {
                if c.is_ascii() {
                    ascii[c as usize * words + j / 64] |= 1 << (j % 64);
                } else {
                    wide.entry(c).or_insert_with(|| vec![0; words])[j / 64] |= 1 << (j % 64);
                }
            }
        }
        for w in 0..words {
            ascii[b'\n' as usize * words + w] = 0;
        }
        let (wide, rows): (Vec<char>, Vec<Vec<u64>>) = wide.into_iter().unzip();
        Blocks { words, m: classes.len(), ascii, wide, rows: rows.concat(), zero: vec![0; words] }
    }

    /// The masks of the character at `hay[i]` and its length.
    #[inline]
    fn eq_at(&self, hay: &[u8], i: usize) -> (&[u64], usize) {
        let b = hay[i];
        if b < 0x80 {
            let r = b as usize * self.words;
            return (&self.ascii[r..r + self.words], 1);
        }
        let (c, n) = wide_char(hay, i);
        match c.and_then(|c| self.wide.binary_search(&c).ok()) {
            Some(r) => (&self.rows[r * self.words..(r + 1) * self.words], n),
            None => (&self.zero, n),
        }
    }

    fn start(&self) -> (Vec<u64>, Vec<u64>) {
        let mut pv = vec![u64::MAX; self.words];
        if !self.m.is_multiple_of(64) {
            pv[self.words - 1] = (1u64 << (self.m % 64)) - 1;
        }
        (pv, vec![0; self.words])
    }

    /// One text character (its masks `eq`): updates the column and returns the change of the last
    /// row's distance.
    fn step(&self, pv: &mut [u64], mv: &mut [u64], eq: &[u64]) -> isize {
        let last = self.words - 1;
        let top = 1u64 << ((self.m - 1) % 64);
        let (mut carry, mut ph_in, mut mh_in, mut delta) = (0u64, 0u64, 0u64, 0isize);
        for w in 0..self.words {
            let eq = eq[w];
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

/// One step of Myers' recurrence on a pattern of up to 128 characters (`top` and `mask` from its
/// length): updates `pv` and `mv` and returns the change of the distance.
#[inline]
fn myers_step(eq: u128, pv: &mut u128, mv: &mut u128, top: u128, mask: u128) -> isize {
    let xv = eq | *mv;
    let xh = (((eq & *pv).wrapping_add(*pv)) ^ *pv) | eq;
    let mut ph = *mv | !(xh | *pv);
    let mut mh = *pv & xh;
    let delta = if ph & top != 0 {
        1
    } else if mh & top != 0 {
        -1
    } else {
        0
    };
    ph = (ph << 1) & mask;
    mh = (mh << 1) & mask;
    *pv = (mh | !(xv | ph)) & mask;
    *mv = ph & xv;
    delta
}

impl FuzzyMatcher {
    pub fn new(pattern: &str, max_edits: u8, fold_case: bool) -> Result<FuzzyMatcher> {
        let classes = fuzzy_classes(pattern, fold_case);
        let m = classes.len();
        if m == 0 || m > FUZZY_MAX {
            bail!("fuzzy patterns must be 1..={FUZZY_MAX} characters");
        }
        if max_edits as usize >= m {
            bail!("the number of edits ({max_edits}) must be smaller than the pattern length ({m} characters)");
        }
        let reversed: Vec<Vec<char>> = classes.iter().rev().cloned().collect();
        let prefilter = fuzzy_pieces(pattern, max_edits, fold_case)
            .filter(|ps| ps.iter().all(|p| p.len() >= 2))
            .and_then(|ps| aho_corasick::AhoCorasick::builder().ascii_case_insensitive(fold_case).build(ps).ok());
        let long = (m > 128).then(|| Box::new((Blocks::new(&classes), Blocks::new(&reversed))));
        Ok(FuzzyMatcher { fwd: Peq::new(&classes), rev: Peq::new(&reversed), m, k: max_edits as usize, prefilter, long })
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
        let mut i = at;
        if let Some(b) = &self.long {
            let (fwd, _) = &**b;
            let (mut pv, mut mv) = fwd.start();
            let mut score = self.m as isize;
            while i < hay.len() {
                if hay[i] == b'\n' {
                    (pv, mv) = fwd.start();
                    score = self.m as isize;
                    i += 1;
                    continue;
                }
                let (eq, n) = fwd.eq_at(hay, i);
                score += fwd.step(&mut pv, &mut mv, eq);
                i += n;
                if score <= self.k as isize {
                    return Some((i, score as usize));
                }
            }
            return None;
        }
        let m = self.m;
        let top = 1u128 << (m - 1);
        let mask = if m == 128 { u128::MAX } else { (1u128 << m) - 1 };
        let mut pv = mask;
        let mut mv = 0u128;
        let mut score = m as isize;
        while i < hay.len() {
            let c = hay[i];
            let (eq, n) = if c < 0x80 {
                if c == b'\n' {
                    pv = mask;
                    mv = 0;
                    score = m as isize;
                    i += 1;
                    continue;
                }
                (self.fwd.ascii[c as usize], 1)
            } else {
                self.fwd.at(hay, i)
            };
            score += myers_step(eq, &mut pv, &mut mv, top, mask);
            i += n;
            if score <= self.k as isize {
                return Some((i, score as usize));
            }
        }
        None
    }

    /// Start of the match that ends at `end` with distance `d`: run the algorithm on the reversed
    /// pattern over the reversed window and take the farthest position still within `d` edits (the
    /// longest match, which highlights the whole misspelt word rather than a fragment of it).
    fn start_for(&self, hay: &[u8], end: usize, d: usize) -> usize {
        let m = self.m;
        // the window: back to the line start or m + k characters, whichever is nearer
        let units = m + self.k;
        let (mut lo, mut n) = (end, 0);
        while lo > 0 && n < units && hay[lo - 1] != b'\n' && end - lo < 4 * units {
            lo -= 1;
            if hay[lo] & 0xC0 != 0x80 {
                n += 1;
            }
        }
        if n < units && end - lo >= 4 * units {
            let stop = (lo + 3).min(end);
            while lo < stop && hay[lo] & 0xC0 == 0x80 {
                lo += 1;
            }
        }
        let mut starts = Vec::with_capacity(units);
        let mut i = lo;
        while i < end {
            starts.push(i);
            i += unit_len(hay, i);
        }
        let mut best = None;
        if let Some(b) = &self.long {
            let (_, rev) = &**b;
            let (mut pv, mut mv) = rev.start();
            let mut score = m as isize;
            for &u in starts.iter().rev() {
                score += rev.step(&mut pv, &mut mv, rev.eq_at(hay, u).0);
                if score <= d as isize {
                    best = Some(u);
                }
            }
        } else {
            let top = 1u128 << (m - 1);
            let mask = if m == 128 { u128::MAX } else { (1u128 << m) - 1 };
            let (mut pv, mut mv, mut score) = (mask, 0u128, m as isize);
            for &u in starts.iter().rev() {
                score += myers_step(self.rev.at(hay, u).0, &mut pv, &mut mv, top, mask);
                if score <= d as isize {
                    best = Some(u);
                }
            }
        }
        best.unwrap_or_else(|| starts.len().checked_sub(m).map_or(lo, |j| starts[j]))
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

/// `-F` with one case-sensitive literal of one line: a substring search finds what the regex
/// engine would, without building it (0.4 M instructions and 30 pages touched at start-up).
#[derive(Clone, Debug)]
pub struct Literal(memchr::memmem::Finder<'static>);

impl Literal {
    fn of(o: &Options, insensitive: bool) -> Option<Literal> {
        let p = o.pattern.as_bytes();
        let plain = o.mode == Mode::Literal && !insensitive && !o.word && !o.multiline;
        // the length bound keeps it to patterns the regex engine's size limit lets through
        (plain && !p.is_empty() && p.len() <= 1024 && !p.contains(&b'\n')).then(|| Literal(memchr::memmem::Finder::new(p).into_owned()))
    }
}

impl Matcher for Literal {
    type Captures = NoCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<MatchSpan>, NoError> {
        let n = self.0.needle().len();
        Ok(haystack.get(at..).and_then(|h| self.0.find(h)).map(|i| MatchSpan::new(at + i, at + i + n)))
    }

    fn new_captures(&self) -> Result<NoCaptures, NoError> {
        Ok(NoCaptures::new())
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        Some(LineTerminator::default())
    }

    fn find_candidate_line(&self, haystack: &[u8]) -> Result<Option<grep_matcher::LineMatchKind>, NoError> {
        Ok(self.0.find(haystack).map(grep_matcher::LineMatchKind::Confirmed))
    }
}

// ------------------------------------------------------------------------- matcher selection
#[derive(Clone)]
pub enum AnyMatcher {
    Literal(Literal),
    Regex(LineRegex),
    Fuzzy(Box<FuzzyMatcher>),
    Pcre2(grep_pcre2::RegexMatcher),
    FuzzyRegex(Box<crate::fuzzyre::FuzzyRegex>),
}

impl AnyMatcher {
    /// A match may hold a line break (the searcher's own test for searching across lines).
    fn spans_lines(&self) -> bool {
        fn can<M: Matcher>(m: &M) -> bool {
            m.line_terminator().is_none() && !m.non_matching_bytes().is_some_and(|b| b.contains(b'\n'))
        }
        match self {
            AnyMatcher::Literal(m) => can(m),
            AnyMatcher::Regex(m) => can(m),
            AnyMatcher::Fuzzy(m) => can(m.as_ref()),
            AnyMatcher::Pcre2(m) => can(m),
            AnyMatcher::FuzzyRegex(m) => can(m.as_ref()),
        }
    }

    fn search_slice<S: Sink>(&self, searcher: &mut Searcher, slice: &[u8], sink: S) -> Result<(), S::Error> {
        match self {
            AnyMatcher::Literal(m) => searcher.search_slice(m, slice, sink),
            AnyMatcher::Regex(m) => searcher.search_slice(m, slice, sink),
            AnyMatcher::Fuzzy(m) => searcher.search_slice(m.as_ref(), slice, sink),
            AnyMatcher::Pcre2(m) => searcher.search_slice(m, slice, sink),
            AnyMatcher::FuzzyRegex(m) => searcher.search_slice(m.as_ref(), slice, sink),
        }
    }

    fn search_file<S: Sink>(&self, searcher: &mut Searcher, file: &std::fs::File, sink: S) -> Result<(), S::Error> {
        match self {
            AnyMatcher::Literal(m) => searcher.search_file(m, file, sink),
            AnyMatcher::Regex(m) => searcher.search_file(m, file, sink),
            AnyMatcher::Fuzzy(m) => searcher.search_file(m.as_ref(), file, sink),
            AnyMatcher::Pcre2(m) => searcher.search_file(m, file, sink),
            AnyMatcher::FuzzyRegex(m) => searcher.search_file(m.as_ref(), file, sink),
        }
    }

    fn search_reader<R: std::io::Read, S: Sink>(&self, searcher: &mut Searcher, reader: R, sink: S) -> Result<(), S::Error> {
        match self {
            AnyMatcher::Literal(m) => searcher.search_reader(m, reader, sink),
            AnyMatcher::Regex(m) => searcher.search_reader(m, reader, sink),
            AnyMatcher::Fuzzy(m) => searcher.search_reader(m.as_ref(), reader, sink),
            AnyMatcher::Pcre2(m) => searcher.search_reader(m, reader, sink),
            AnyMatcher::FuzzyRegex(m) => searcher.search_reader(m.as_ref(), reader, sink),
        }
    }
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

/// Several patterns as one that matches where any of them does, and the mode to search it with:
/// literals and identifiers become regex alternatives. `fixed` is `-F`, which escapes `-P` patterns.
pub fn join_patterns(patterns: &[String], mode: Mode, fixed: bool) -> Result<(String, Mode)> {
    if let [one] = patterns {
        let one = if mode == Mode::Pcre2 && fixed { regex::escape(one) } else { one.clone() };
        return Ok((one, mode));
    }
    if matches!(mode, Mode::Fuzzy { .. }) {
        bail!("--fuzzy takes one pattern");
    }
    let part = |p: &str| match mode {
        Mode::Literal => regex::escape(p),
        Mode::Pcre2 if fixed => regex::escape(p),
        Mode::Identifier => identifier_regex(p),
        _ => p.to_string(),
    };
    let joined = patterns.iter().map(|p| format!("(?:{})", part(p))).collect::<Vec<_>>().join("|");
    Ok((joined, if matches!(mode, Mode::Pcre2 | Mode::FuzzyRegex { .. }) { mode } else { Mode::Regex }))
}

/// What the options ask every matcher of them to find; `build_matcher` and the text index both
/// start from this, so a file the index lets through is judged by the same pattern.
pub struct Translated<'a> {
    /// case folding on top of `pattern` (the smart-case rule applied)
    pub insensitive: bool,
    /// regex source for Regex, Literal and Identifier; the pattern as given for the other modes
    pub pattern: std::borrow::Cow<'a, str>,
}

impl Options {
    pub fn translate(&self) -> Translated<'_> {
        let insensitive = match self.case {
            Case::Sensitive => false,
            Case::Insensitive => true,
            Case::Smart => smart_folds(&self.pattern, matches!(self.mode, Mode::Regex | Mode::Pcre2 | Mode::FuzzyRegex { .. })),
        };
        match self.mode {
            Mode::Literal => Translated { insensitive, pattern: regex::escape(&self.pattern).into() },
            // the identifier regex carries its own (?i)
            Mode::Identifier => Translated { insensitive: false, pattern: identifier_regex(&self.pattern).into() },
            _ => Translated { insensitive, pattern: self.pattern.as_str().into() },
        }
    }

    /// Rejects options the text index cannot answer for: it holds only the files of the default walk.
    pub fn check_indexable(&self) -> Result<()> {
        if self.search_zip {
            bail!("--index does not cover archive members; search with -z and without --index");
        }
        if self.hidden || self.no_ignore || self.follow_links || self.max_depth.is_some() || self.max_filesize.is_some() {
            bail!("--index searches the files of the default walk; drop --hidden/--no-ignore/-L/--max-depth/--max-filesize or search without --index");
        }
        Ok(())
    }
}

/// The smart-case rule of ripgrep: fold when the pattern has a literal and none of its literals is
/// uppercase. In a regex, classes (`\w`, `\p{Ll}`, `[[:lower:]]`) are not literals; `[a-z]` is.
fn smart_folds(pattern: &str, regex: bool) -> bool {
    use regex_syntax::ast::{Ast, ClassSet, ClassSetItem};
    fn lit(c: char, seen: &mut (bool, bool)) {
        seen.0 = true;
        seen.1 |= c.is_uppercase();
    }
    fn item(i: &ClassSetItem, seen: &mut (bool, bool)) {
        match i {
            ClassSetItem::Literal(l) => lit(l.c, seen),
            ClassSetItem::Range(r) => {
                lit(r.start.c, seen);
                lit(r.end.c, seen);
            }
            ClassSetItem::Bracketed(b) => set(&b.kind, seen),
            ClassSetItem::Union(u) => u.items.iter().for_each(|x| item(x, seen)),
            _ => {}
        }
    }
    fn set(s: &ClassSet, seen: &mut (bool, bool)) {
        match s {
            ClassSet::Item(i) => item(i, seen),
            ClassSet::BinaryOp(op) => {
                set(&op.lhs, seen);
                set(&op.rhs, seen);
            }
        }
    }
    fn walk(a: &Ast, seen: &mut (bool, bool)) {
        match a {
            Ast::Literal(l) => lit(l.c, seen),
            Ast::ClassBracketed(c) => set(&c.kind, seen),
            Ast::Repetition(r) => walk(&r.ast, seen),
            Ast::Group(g) => walk(&g.ast, seen),
            Ast::Alternation(x) => x.asts.iter().for_each(|x| walk(x, seen)),
            Ast::Concat(x) => x.asts.iter().for_each(|x| walk(x, seen)),
            _ => {}
        }
    }
    if !regex {
        return !uppercase_literal(pattern, false);
    }
    match regex_syntax::ast::parse::Parser::new().parse(pattern) {
        Ok(ast) => {
            let mut seen = (false, false);
            walk(&ast, &mut seen);
            seen.0 && !seen.1
        }
        // PCRE2-only syntax
        Err(_) => !uppercase_literal(pattern, true),
    }
}

/// Whether the pattern has an uppercase letter that stands for itself (the smart-case rule, as in
/// ripgrep): in a regex, `\S`, `\W`, `\pL` and `\p{Lu}` are not such letters.
fn uppercase_literal(pattern: &str, regex: bool) -> bool {
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        if regex && c == '\\' {
            match chars.next() {
                Some('p' | 'P') if chars.peek() == Some(&'{') => {
                    chars.by_ref().find(|&d| d == '}');
                }
                Some('p' | 'P') => {
                    chars.next();
                }
                // a letter by its code point stands for itself: `\x41`, `\x{41}`, `\u0041`, `\U00000041`
                Some(kind @ ('x' | 'u' | 'U')) => {
                    let digits: String = if chars.peek() == Some(&'{') {
                        chars.by_ref().skip(1).take_while(|&d| d != '}').collect()
                    } else {
                        chars
                            .by_ref()
                            .take(if kind == 'x' {
                                2
                            } else if kind == 'u' {
                                4
                            } else {
                                8
                            })
                            .collect()
                    };
                    if u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32).is_some_and(char::is_uppercase) {
                        return true;
                    }
                }
                _ => {}
            }
        } else if c.is_uppercase() {
            return true;
        }
    }
    false
}

pub fn build_matcher(o: &Options) -> Result<AnyMatcher> {
    if matches!(o.mode, Mode::Fuzzy { .. } | Mode::FuzzyRegex { .. }) {
        // neither matcher looks beyond a line or at word boundaries: saying so beats finding nothing
        if o.word {
            bail!("whole-word matching (-w) is not available with --fuzzy or --fuzzy-regex");
        }
        if o.multiline {
            bail!("multiline matching (-U) is not available with --fuzzy or --fuzzy-regex");
        }
    }
    let Translated { insensitive, pattern: pat } = o.translate();
    if let Some(m) = Literal::of(o, insensitive) {
        return Ok(AnyMatcher::Literal(m));
    }
    match o.mode {
        Mode::Fuzzy { max_edits } => Ok(AnyMatcher::Fuzzy(Box::new(FuzzyMatcher::new(&pat, max_edits, insensitive)?))),
        Mode::FuzzyRegex { max_edits } => Ok(AnyMatcher::FuzzyRegex(Box::new(crate::fuzzyre::FuzzyRegex::new(&pat, max_edits, insensitive)?))),
        Mode::Pcre2 => {
            let m = grep_pcre2::RegexMatcherBuilder::new()
                .caseless(insensitive)
                .word(o.word)
                .multi_line(true)
                .dotall(false)
                .utf(true)
                .ucp(true)
                .jit_if_available(true)
                .build(&pat)
                .with_context(|| format!("invalid PCRE2 pattern {:?}", o.pattern))?;
            Ok(AnyMatcher::Pcre2(m))
        }
        _ => {
            let mut b = RegexMatcherBuilder::new();
            // ^ and $ are line anchors in every mode, as in ripgrep; without it the searcher has to
            // run the regex one line at a time
            b.case_insensitive(insensitive)
                .word(o.word)
                .multi_line(true)
                .dot_matches_new_line(false)
                .size_limit(o.regex_size_limit)
                .dfa_size_limit(o.dfa_size_limit)
                .line_terminator(if o.multiline { None } else { Some(b'\n') });
            let m = b.build(&pat).with_context(|| format!("invalid pattern {:?}", o.pattern))?;
            let suffix_ok = !o.word && o.mode != Mode::Literal;
            Ok(AnyMatcher::Regex(if suffix_ok { LineRegex::new(m, &pat, insensitive) } else { LineRegex::plain(m) }))
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
/// formatted into a buffer and handed over in one call, so no per-line allocation or channel message.
/// Used for the common piped case (no colour, no JSON, no sorting); the bytes are the same as the
/// CLI's own formatting of `Hit`s.
///
/// Output that follows other output within `HOLD_NS` waits in a buffer, so the lines of many files
/// leave in one write: a write per matching file wakes the reader of a pipe every time (88,000
/// writes for `return` in the Linux tree). Output after a quiet spell leaves at once.
pub struct PlainSink {
    out: Mutex<std::io::BufWriter<Box<dyn Write + Send>>>,
    /// when output last left, in nanoseconds since `start`
    left: AtomicU64,
    /// the buffer holds output
    holding: AtomicBool,
    start: Instant,
    /// the reader went away (`| head`)
    pub broken: AtomicBool,
    /// a file searched with context lines has written output (later files start with `--`)
    grouped: AtomicBool,
}

/// The longest output waits for more to leave with it, unless no thread starts a file meanwhile.
const HOLD_NS: u64 = 1_000_000;

impl PlainSink {
    pub fn new(out: Box<dyn Write + Send>) -> PlainSink {
        PlainSink {
            out: Mutex::new(std::io::BufWriter::with_capacity(1 << 16, out)),
            left: AtomicU64::new(0),
            holding: AtomicBool::new(false),
            start: Instant::now(),
            broken: AtomicBool::new(false),
            grouped: AtomicBool::new(false),
        }
    }

    fn waited(&self) -> Option<u64> {
        let now = self.start.elapsed().as_nanos() as u64;
        (now.saturating_sub(self.left.load(Ordering::Relaxed)) >= HOLD_NS).then_some(now)
    }

    /// Runs `f` on the writer as one unit (no other thread's output inside it), then lets the held
    /// output go if it has waited long enough.
    fn with(&self, cancel: &AtomicBool, f: impl FnOnce(&mut dyn Write) -> std::io::Result<()>) {
        if self.broken.load(Ordering::Relaxed) {
            return;
        }
        let res = match self.out.lock() {
            Ok(mut w) => f(&mut *w).and_then(|_| {
                if let Some(now) = self.waited().filter(|_| !w.buffer().is_empty()) {
                    self.left.store(now, Ordering::Relaxed);
                    w.flush()?;
                }
                self.holding.store(!w.buffer().is_empty(), Ordering::Relaxed);
                Ok(())
            }),
            Err(_) => Err(std::io::Error::other("output lock poisoned")),
        };
        if res.is_err() {
            self.broken.store(true, Ordering::Relaxed);
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// `first`: the first output of a file searched with context, which another file's output
    /// before it separates with `--`.
    fn write(&self, buf: &[u8], first: bool, cancel: &AtomicBool) {
        if !buf.is_empty() {
            self.with(cancel, |w| self.separate(w, first).and_then(|_| w.write_all(buf)));
        }
    }

    /// `before`, `head`, `text` with invalid UTF-8 replaced, and a line break, as one unit.
    fn write_line(&self, before: &[u8], head: &[u8], text: &[u8], first: bool, cancel: &AtomicBool) {
        self.with(cancel, |w| {
            self.separate(w, first)
                .and_then(|_| w.write_all(before))
                .and_then(|_| w.write_all(head))
                .and_then(|_| write_lossy(w, text, 1 << 18))
                .and_then(|_| w.write_all(b"\n"))
        });
    }

    fn separate(&self, w: &mut dyn Write, first: bool) -> std::io::Result<()> {
        if first && self.grouped.swap(true, Ordering::Relaxed) {
            w.write_all(b"--\n")?;
        }
        Ok(())
    }

    /// Lets held output go once it has waited. Threads call this between files, so output waits
    /// no longer than `HOLD_NS` or the longest file being searched.
    fn tick(&self, cancel: &AtomicBool) {
        if self.holding.load(Ordering::Relaxed) && self.waited().is_some() {
            self.with(cancel, |_| Ok(()));
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

/// `path:line:text\n` (`-` separators for context lines), the CLI's plain format; an empty `path`
/// leaves out the name and its separator, a `line` of 0 the numbers. A multi-line match prints
/// each of its lines with its own number, as grep-class tools do.
pub fn format_plain(out: &mut Vec<u8>, path: &str, line: u64, text: &[u8], context: bool) {
    let sep = if context { b'-' } else { b':' };
    for (k, part) in text.split(|&b| b == b'\n').enumerate() {
        if !path.is_empty() {
            out.extend_from_slice(path.as_bytes());
            out.push(sep);
        }
        if line != 0 {
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
        }
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
    /// a named file, searched as ripgrep does (`Options::paths_as_given`): NUL bytes end lines
    convert: bool,
    /// where the searcher first saw a NUL byte
    binary_at: Option<u64>,
    /// a match may span lines (otherwise a matched line holds no line break)
    multiline: bool,
    /// context lines are asked for and this file has written no plain output yet
    first: bool,
    /// context lines on several threads: plain output leaves only at the end of a group, so no
    /// other file's lines land inside one
    whole_groups: bool,
    /// plain lines start with `path`
    named: bool,
    /// plain lines have line numbers
    numbered: bool,
    /// `-c` counts each match of a multi-line search, not each merged group of lines
    count_each: bool,
}

fn spans_in(matcher: &AnyMatcher, line: &[u8]) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    each_match(matcher, line, |a, b| {
        out.push((a, b));
        out.len() < 64
    });
    out
}

/// Calls `f` with each match in `line` until it returns false.
fn each_match(matcher: &AnyMatcher, line: &[u8], mut f: impl FnMut(usize, usize) -> bool) {
    let mut at = 0;
    let find = |at: usize| -> Option<MatchSpan> {
        match matcher {
            AnyMatcher::Literal(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::Regex(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::Fuzzy(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::Pcre2(m) => m.find_at(line, at).ok().flatten(),
            AnyMatcher::FuzzyRegex(m) => m.find_at(line, at).ok().flatten(),
        }
    };
    while at <= line.len() {
        match find(at) {
            Some(m) => {
                at = if m.end() > m.start() { m.end() } else { m.end() + 1 };
                if !f(m.start(), m.end()) {
                    break;
                }
            }
            None => break,
        }
    }
}

impl<'a> Sink for Collector<'a> {
    type Error = std::io::Error;

    fn matched(&mut self, _s: &Searcher, mat: &SinkMatch<'_>) -> Result<bool, std::io::Error> {
        if self.cancel.load(Ordering::Relaxed) {
            return Ok(false);
        }
        if self.files_only || self.count_only {
            if self.files_only && self.matched {
                return Ok(true);
            }
            let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
            if self.over_limit(n) {
                return Ok(false);
            }
            self.matched = true;
            // as ripgrep: merged matches that span lines count one by one
            self.file_count += if self.count_each {
                let mut n = 0;
                each_match(self.matcher, mat.bytes(), |_, _| {
                    n += 1;
                    true
                });
                n.max(1)
            } else {
                1
            };
            if self.max_results.is_some_and(|max| n >= max) {
                self.cancel.store(true, Ordering::Relaxed);
                return Ok(false);
            }
            // one match is enough to list the file; with -v ripgrep reads on and drops a file
            // with a NUL byte
            return Ok(!self.files_only || (self.invert && !self.convert));
        }
        if self.convert && self.binary_at.is_some() {
            // as ripgrep: a match past a NUL byte ends the output, `finish` writes the note
            let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
            self.matched |= !self.over_limit(n);
            return Ok(false);
        }
        let bytes = mat.bytes();
        let line = bytes.strip_suffix(b"\n").map(|b| b.strip_suffix(b"\r").unwrap_or(b)).unwrap_or(bytes);
        if let Some(p) = self.plain {
            let n = self.stats_matches.fetch_add(1, Ordering::Relaxed) + 1;
            if self.over_limit(n) {
                return Ok(false);
            }
            self.matched = true;
            let number = if self.numbered { mat.line_number().unwrap_or(0) + self.line_base } else { 0 };
            if line.len() >= 1 << 16 && !self.hold && !self.whole_groups && !(self.multiline && memchr::memchr(b'\n', line).is_some()) {
                // a very long line is written from where it is instead of being copied into the buffer
                let mut head = Vec::with_capacity(self.path.len() + 24);
                format_plain(&mut head, if self.named { &self.path } else { "" }, number, b"", false);
                head.pop(); // the newline: the text goes between
                p.write_line(&self.out, &head, line, std::mem::take(&mut self.first), self.cancel);
                self.out.clear();
            } else {
                format_plain(&mut self.out, if self.named { &self.path } else { "" }, number, lossy(line).as_bytes(), false);
            }
            if self.out.len() >= 1 << 16 && !self.hold && !self.whole_groups {
                p.write(&self.out, std::mem::take(&mut self.first), self.cancel);
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
        let line = bytes.strip_suffix(b"\n").map(|b| b.strip_suffix(b"\r").unwrap_or(b)).unwrap_or(bytes);
        if self.plain.is_some() {
            let name = if self.named { &self.path } else { "" };
            let number = if self.numbered { ctx.line_number().unwrap_or(0) + self.line_base } else { 0 };
            format_plain(&mut self.out, name, number, lossy(line).as_bytes(), true);
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

    fn context_break(&mut self, _s: &Searcher) -> Result<bool, std::io::Error> {
        if let Some(p) = self.plain.filter(|_| self.whole_groups && !self.hold && self.out.len() >= 1 << 16) {
            // the next group is a first output: the sink writes the `--` before it
            p.write(&self.out, std::mem::take(&mut self.first), self.cancel);
            self.out.clear();
            self.first = true;
        } else if self.plain.is_some() {
            self.out.extend_from_slice(b"--\n");
        }
        Ok(true)
    }

    fn binary_data(&mut self, _s: &Searcher, offset: u64) -> Result<bool, std::io::Error> {
        self.binary_at = Some(offset);
        Ok(true)
    }

    fn finish(&mut self, _s: &Searcher, f: &grep_searcher::SinkFinish) -> Result<(), std::io::Error> {
        self.binary = f.binary_byte_offset().is_some();
        if let (true, false, Some(at)) = (self.matched, self.files_only || self.count_only, self.binary_at) {
            // as ripgrep: a named file's line output ends here; a walked file stopped being searched
            let note = if self.convert {
                format!("binary file matches (found \"\\0\" byte around offset {at})")
            } else {
                format!("WARNING: stopped searching binary file after match (found \"\\0\" byte around offset {at})")
            };
            if self.plain.is_some() {
                if self.named {
                    self.out.extend_from_slice(self.path.as_bytes());
                    self.out.extend_from_slice(b": ");
                }
                self.out.extend_from_slice(note.as_bytes());
                self.out.push(b'\n');
            } else {
                self.push(Hit { path: Arc::clone(&self.path), line: NOTE_LINE, column: 0, text: note, spans: Vec::new(), context: false });
            }
        }
        self.flush();
        Ok(())
    }
}

impl Collector<'_> {
    /// Match number `n` (counted over all threads) is past `max_results`: another thread got there
    /// first, so this one is not reported, nor counted.
    fn over_limit(&self, n: u64) -> bool {
        let over = self.max_results.is_some_and(|max| n > max);
        if over {
            self.stats_matches.fetch_sub(1, Ordering::Relaxed);
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
        if let (Some(p), false, false) = (self.plain, self.hold, self.out.is_empty()) {
            p.write(&self.out, std::mem::take(&mut self.first), self.cancel);
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
        return asked_threads(o);
    }
    *N.get_or_init(|| performance_cores().map_or_else(cores, |p| p.clamp(2, cores())))
}

/// Most threads a search starts for `Options::threads`: past a few hundred, starting them costs
/// more than any search gains (`-j 4000` took 1.8 s on 11 files, `-j 100000` failed to start them).
pub const MAX_THREADS: usize = 256;

/// `Options::threads` as a number of threads (0, the default, is not one).
fn asked_threads(o: &Options) -> usize {
    o.threads.min(MAX_THREADS)
}

/// An open and read of a small file taking this long came from the disk, not the page cache:
/// on macOS about 25 us warm and 160 us or more cold (M3 Pro); on Linux 99.9% of warm reads
/// under 15 us and 97% of cold reads over 30 us (a VM on the same machine).
const COLD_READ_NS: u64 = if cfg!(target_os = "linux") { 30_000 } else { 75_000 };

fn reads_from_disk(c: &Counters) -> bool {
    // three quarters slow: a few reads delayed by a busy machine do not make a warm tree look cold
    let reads = c.small_reads.load(Ordering::Relaxed);
    reads >= 16 && c.slow_reads.load(Ordering::Relaxed) * 4 >= reads * 3
}

/// Most threads for a tree read from the disk. Those reads wait on the device, so more of them
/// than cores are worth having. On Linux 64: the Linux tree cold on 5 CPUs took 1.22 s on 5
/// threads, 0.76 s on 10, 0.55 s on 20, 0.41 s on 40, 0.37 s on 80, 0.38 s on 160. Elsewhere twice
/// the cores, at most 24: on macOS the same tree took 2.7 s on 5 threads, 1.6 s on 11, 1.31 s on
/// 24, 1.24 s on 48, 1.36 s on 96, and 200,000 small files were slower on 48 than on 22.
fn cold_threads() -> usize {
    if cfg!(target_os = "linux") {
        64
    } else {
        2 * cores()
    }
}

/// A cold tree gets one more thread for every this many slow reads, not all `cold_threads` at
/// once: starting a thread costs about as much as a slow read, which a small tree never earns
/// back (231 files cold on Linux: 8.3 ms with a thread per slow read, 7.4 ms per 4, 6.0 ms per 8,
/// 5.8 ms per 16; the Linux tree takes 0.37 s with any of them).
const COLD_READS_PER_THREAD: u64 = 16;

/// Threads the reads so far call for, on top of `default_threads`.
fn cold_extra(o: &Options, c: &Counters) -> usize {
    if o.threads == 0 && reads_from_disk(c) {
        let most = cold_threads().saturating_sub(default_threads(o));
        ((c.slow_reads.load(Ordering::Relaxed) / COLD_READS_PER_THREAD) as usize).min(most)
    } else {
        0
    }
}

/// Threads for one large file searched as ranges: every core (at most 12), since that work is
/// matching rather than opening files.
fn range_threads(o: &Options) -> usize {
    if o.threads != 0 {
        asked_threads(o)
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

/// ripgrep's own ignore file: read before `.ignore` and `.gitignore`, switched off with them.
pub const RGIGNORE: &str = ".rgignore";

/// The glob and file-type filters of `o`, plus the index directory, which is never searched.
pub fn overrides(root: &Path, o: &Options) -> Result<ignore::overrides::Override> {
    let mut ob = OverrideBuilder::new(root);
    ob.add(&format!("!{}/", crate::files::INDEX_DIR))?;
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

/// Globs matched against each path from `dir`, the directory of the file that holds them. A path
/// outside `dir` is not touched; a path named on the command line is searched whatever they say.
#[derive(Debug)]
pub struct ScopedGlobs {
    /// canonical, as the roots are compared with it
    dir: PathBuf,
    ov: ignore::overrides::Override,
}

/// Where a searched root lies from `ScopedGlobs::dir`.
enum Scope {
    /// at or below it: the root's path from it
    Below(PathBuf),
    /// above it: its path from the root
    Above(PathBuf),
    Outside,
}

impl ScopedGlobs {
    pub fn new(dir: &Path, globs: &[String]) -> Result<ScopedGlobs> {
        let mut ob = OverrideBuilder::new(".");
        for g in globs {
            ob.add(g).with_context(|| format!("invalid glob {g:?}"))?;
        }
        Ok(ScopedGlobs { dir: std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf()), ov: ob.build()? })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn scope(&self, root: &Path) -> Scope {
        let Ok(root) = std::fs::canonicalize(root) else { return Scope::Outside };
        if let Ok(p) = root.strip_prefix(&self.dir) {
            Scope::Below(p.to_path_buf())
        } else if let Ok(p) = self.dir.strip_prefix(&root) {
            Scope::Above(p.to_path_buf())
        } else {
            Scope::Outside
        }
    }

    /// Whether the globs leave out `rest`, a path below a root in `scope`.
    fn excludes(&self, scope: &Scope, rest: &Path, is_dir: bool) -> bool {
        thread_local! {
            static JOINED: std::cell::RefCell<PathBuf> = const { std::cell::RefCell::new(PathBuf::new()) };
        }
        match scope {
            Scope::Below(p) if p.as_os_str().is_empty() => self.ov.matched(rest, is_dir).is_ignore(),
            Scope::Below(p) => JOINED.with_borrow_mut(|j| {
                j.as_mut_os_string().clear();
                j.push(p);
                j.push(rest);
                self.ov.matched(&*j, is_dir).is_ignore()
            }),
            Scope::Above(p) => rest.strip_prefix(p).is_ok_and(|r| !r.as_os_str().is_empty() && self.ov.matched(r, is_dir).is_ignore()),
            Scope::Outside => false,
        }
    }

    /// Whether the globs leave out `rel`, a file below a root in `scope` named with `/`, or a
    /// directory on the way down to it.
    fn excludes_file(&self, scope: &Scope, rel: &str) -> bool {
        let mut at = 0;
        while let Some(i) = rel[at..].find('/') {
            if self.excludes(scope, Path::new(&rel[..at + i]), true) {
                return true;
            }
            at += i + 1;
        }
        self.excludes(scope, Path::new(rel), false)
    }
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
    if !o.no_ignore {
        wb.add_custom_ignore_filename(RGIGNORE);
    }
    if let Some(cwd) = &o.cwd {
        wb.process_current_dir(cwd);
    }
    // anchored at a root that is a file, the filters would miss the files of the other roots
    let anchor = if roots[0].is_dir() { roots[0].as_path() } else { roots[0].parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")) };
    wb.overrides(overrides(anchor, o)?);
    if let Some(sg) = o.scoped_globs.clone() {
        let scopes: Vec<(PathBuf, Scope)> = roots.iter().map(|r| (r.clone(), sg.scope(r))).collect();
        wb.filter_entry(move |e| {
            let path = e.path();
            match scopes.iter().find_map(|(r, s)| path.strip_prefix(r).ok().map(|rest| (s, rest))) {
                Some((s, rest)) => !sg.excludes(s, rest, e.file_type().is_some_and(|t| t.is_dir())),
                None => true,
            }
        });
    }
    Ok(wb)
}

/// How many files the one-thread walk (`small_first`) takes before the parallel walk does the rest;
/// 0 skips it. Roots holding only files get 256: starting the parallel walk costs more there than
/// the search itself (three large files: 40.6 ms against 41.9). Any directory to enter gets 0: one
/// thread reading every directory then holds up the searches (the ripgrep repository, 60
/// directories: 5.6 ms in parallel against 6.0). `Options::small_tree` sets the number for any tree
/// (the tests use both walks).
fn small_tree_files(roots: &[PathBuf], o: &Options) -> usize {
    o.small_tree.unwrap_or_else(|| if has_dirs(roots, o) { 0 } else { 256 })
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

/// A regular file, or anything but a directory named on the command line (a named pipe,
/// `/dev/stdin`), as ripgrep does; a pipe met in a directory could wait for a writer forever.
fn searchable(e: &ignore::DirEntry) -> bool {
    e.file_type().is_some_and(|t| t.is_file() || (e.depth() == 0 && !t.is_dir()))
}

enum Small {
    Done,
    /// more files than the budget: the paths already searched, each with the times it was (overlapping
    /// roots reach a file more than once), for the parallel walk to skip that many times
    Big(std::collections::HashMap<PathBuf, AtomicUsize>),
}

/// Walks on the calling thread while a few threads search the files it finds, so a small tree pays
/// neither for a parallel walk nor for walking before searching. The first file waits for a second
/// before any thread starts: a tree of one file is searched on the calling thread, with no thread
/// (four threads cost about 1 M instructions, a third of a one-file search past start-up). Past
/// `small_tree_files` files the walk stops; the caller's parallel walk then skips what was searched
/// here. Walk errors count only when this walk finished (the parallel walk meets them again otherwise).
#[allow(clippy::too_many_arguments)]
fn small_first(
    roots: &[PathBuf],
    labels: &[PathBuf],
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
    type Job = (PathBuf, Arc<str>, Option<u64>, bool);
    let (work_tx, work_rx) = mpsc::channel::<Job>();
    let work_rx = Mutex::new(work_rx);
    let (tx, rx) = mpsc::channel::<Vec<Hit>>();
    let workers = default_threads(o).min(3);
    let (seen, errors, big) = std::thread::scope(|s| {
        let work_rx = &work_rx;
        let worker = |tx: mpsc::Sender<Vec<Hit>>| {
            #[cfg(test)]
            tests::SMALL_WORKERS.with(|n| n.set(n.get() + 1));
            // a clone per thread: each gets its own regex cache pool instead of contending for one
            let matcher = matcher.clone();
            s.spawn(move || {
                let mut fw = file_worker(o);
                loop {
                    let job = match work_rx.lock() {
                        Ok(r) => r.recv(),
                        Err(_) => break,
                    };
                    let Ok((path, rel, size, named)) = job else { break };
                    if !cancel.load(Ordering::Relaxed) {
                        search_one(&mut fw, &Run { matcher: &matcher, o, c, cancel, tx: &tx, plain }, &path, rel, size, named);
                    }
                }
            });
        };
        let (mut started, mut added) = (false, 0);
        let mut held: Option<Job> = None;
        let mut seen = std::collections::HashMap::<PathBuf, AtomicUsize>::new();
        let mut taken = 0;
        let mut errors = Vec::new();
        let mut big = false;
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
            if !searchable(&entry) {
                c.met_dir(&entry);
                continue;
            }
            if taken == limit {
                big = true;
                break;
            }
            taken += 1;
            let size = if o.want_bytes { entry.metadata().ok().map(|m| m.len()) } else { None };
            let rel = label(entry.path(), labels, o);
            let named = o.paths_as_given && entry.depth() == 0;
            let path = entry.into_path();
            *seen.entry(path.clone()).or_default().get_mut() += 1;
            let job = (path, rel, size, named);
            if !started {
                let Some(first) = held.take() else {
                    held = Some(job);
                    continue;
                };
                for _ in 0..workers {
                    worker(tx.clone());
                }
                started = true;
                if work_tx.send(first).is_err() {
                    break;
                }
            }
            if work_tx.send(job).is_err() {
                break;
            }
            while added < cold_extra(o, c) {
                worker(tx.clone());
                added += 1;
            }
            while let Ok(batch) = rx.try_recv() {
                batch.into_iter().for_each(&mut *on_hit);
            }
        }
        drop(work_tx);
        if let Some((path, rel, size, named)) = held.filter(|_| !cancel.load(Ordering::Relaxed)) {
            c.alone.store(true, Ordering::Relaxed);
            search_one(&mut file_worker(o), &Run { matcher, o, c, cancel, tx: &tx, plain }, &path, rel, size, named);
            c.alone.store(false, Ordering::Relaxed);
        }
        drop(tx);
        drain(rx, &mut *on_hit);
        (seen, errors, big)
    });
    if big && !cancel.load(Ordering::Relaxed) {
        return Small::Big(seen);
    }
    for e in errors {
        c.failed(e);
    }
    Small::Done
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
    /// directories below a root that are on a kernel pseudo filesystem (`met_dir`), and whether
    /// there are any: the flag is all a file pays for until there are
    pseudo_dirs: Mutex<Vec<PathBuf>>,
    has_pseudo_dirs: AtomicBool,
    /// one file is searched and no other at the same time: its output need not wait for the end
    /// of a context group
    alone: AtomicBool,
}

impl Counters {
    /// A directory the walk met. One named `proc` or `sys` gets a `statfs` (a procfs mounted inside
    /// a tree, such as a container's root filesystem); every other entry, only a name comparison.
    #[cfg_attr(not(target_os = "linux"), allow(unused_variables))]
    fn met_dir(&self, e: &ignore::DirEntry) {
        #[cfg(target_os = "linux")]
        if e.depth() > 0 && matches!(e.file_name().as_encoded_bytes(), b"proc" | b"sys") && e.file_type().is_some_and(|t| t.is_dir()) && on_pseudo_fs(e.path())
        {
            self.pseudo_dirs.lock().unwrap_or_else(|p| p.into_inner()).push(e.path().to_path_buf());
            self.has_pseudo_dirs.store(true, Ordering::Release);
        }
    }

    /// `path` is read until a read returns nothing (it is on a pseudo filesystem).
    fn read_to_end(&self, o: &Options, path: &Path) -> bool {
        (!o.read_to_end_under.is_empty() && o.read_to_end_under.iter().any(|p| path.starts_with(p)))
            || (self.has_pseudo_dirs.load(Ordering::Acquire) && self.pseudo_dirs.lock().unwrap_or_else(|p| p.into_inner()).iter().any(|p| path.starts_with(p)))
    }

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
    /// for files named on the command line, made on the first one
    named: Option<Searcher>,
    small: Vec<u8>,
}

fn file_worker(o: &Options) -> FileWorker {
    FileWorker { searcher: make_searcher(o, BinaryDetection::quit(0)), named: None, small: Vec::new() }
}

fn make_searcher(o: &Options, binary: BinaryDetection) -> Searcher {
    make_searcher_limited(o, binary, None)
}

/// `heap`: the most bytes the searcher may hold, a line and its context (all of the input with -U).
fn make_searcher_limited(o: &Options, binary: BinaryDetection, heap: Option<usize>) -> Searcher {
    SearcherBuilder::new()
        .binary_detection(binary)
        .heap_limit(heap)
        .line_number(!(o.count_only || o.files_only)) // line numbers cost a pass over the buffer
        .multi_line(o.multiline)
        .before_context(o.before)
        .after_context(o.after)
        .invert_match(o.invert)
        .max_matches(o.max_count)
        .build()
}

/// What every file of one search shares: the matcher, the options, the counters, the cancel flag and
/// where the results go.
#[derive(Clone, Copy)]
struct Run<'a> {
    matcher: &'a AnyMatcher,
    o: &'a Options,
    c: &'a Counters,
    cancel: &'a AtomicBool,
    tx: &'a mpsc::Sender<Vec<Hit>>,
    plain: Option<&'a PlainSink>,
}

impl<'a> Run<'a> {
    fn collector(&self, path: Arc<str>) -> Collector<'a> {
        let o = self.o;
        Collector {
            path,
            tx: self.tx,
            batch: Vec::new(),
            plain: self.plain,
            out: Vec::new(),
            want_spans: o.want_spans,
            stats_matches: &self.c.matches,
            max_results: o.max_results,
            matched: false,
            cancel: self.cancel,
            matcher: self.matcher,
            invert: o.invert,
            count_only: o.count_only,
            files_only: o.files_only,
            file_count: 0,
            line_base: 0,
            hold: false,
            binary: false,
            convert: false,
            binary_at: None,
            multiline: o.multiline,
            first: o.before > 0 || o.after > 0,
            whole_groups: (o.before > 0 || o.after > 0) && default_threads(o) > 1 && !self.c.alone.load(Ordering::Relaxed),
            named: o.with_filename,
            numbered: o.line_numbers,
            count_each: o.multiline && o.count_only && !o.invert && self.matcher.spans_lines(),
        }
    }
}

/// Search one file and send its hits to `tx`. `named`: a root that is a file, or `-` for standard
/// input (`Options::paths_as_given`).
fn search_one(w: &mut FileWorker, r: &Run, path: &Path, rel: Arc<str>, size: Option<u64>, named: bool) {
    let (o, c) = (r.o, r.c);
    if let Some(p) = r.plain {
        p.tick(r.cancel);
    }
    c.files_searched.fetch_add(1, Ordering::Relaxed);
    if let Some(n) = size {
        c.bytes.fetch_add(n, Ordering::Relaxed);
    }
    if o.search_zip {
        if let Some(kind) = crate::archive::Kind::of(&rel) {
            search_archive(r, path, &rel, kind);
            return;
        }
    }
    if named && path.as_os_str() == "-" {
        let searcher = w.named.get_or_insert_with(|| make_searcher(o, BinaryDetection::convert(0)));
        let mut sink = Collector { convert: true, ..r.collector(Arc::clone(&rel)) };
        if let Err(e) = r.matcher.search_reader(searcher, std::io::stdin().lock(), &mut sink) {
            c.failed(format!("{rel}: {e}"));
        }
        report(r, sink, rel, true);
        return;
    }
    let mut searcher = if named { w.named.get_or_insert_with(|| make_searcher(o, BinaryDetection::convert(0))) } else { &mut w.searcher };
    let mut fresh = None;
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
    if splittable(o) && !c.read_to_end(o, path) {
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
                let min = o.split_file_bytes;
                if len >= min {
                    match split::search(r, &file, len, (min / 8).max(1), &rel) {
                        split::Outcome::Done => return,
                        split::Outcome::Stream => {}
                        split::Outcome::Buffer(b) => owned = Some(b),
                    }
                } else if len <= MID_FILE && range_threads(o) > 1 {
                    // a searcher of its own, so the thread's keeps the buffer size it had: where a
                    // NUL byte after the first fill stops the output depends on that size. One
                    // thread meets the files in a fixed order and keeps its one searcher
                    searcher = fresh.insert(make_searcher(o, if named { BinaryDetection::convert(0) } else { BinaryDetection::quit(0) }));
                }
            }
        }
    }
    let whole: Option<&[u8]> = owned.as_deref().or(small.map(|n| &w.small[..n]));
    let mut sink = Collector { convert: named, ..r.collector(Arc::clone(&rel)) };
    let res = match whole {
        // only the stream reader turns NUL bytes into line breaks
        Some(b) if named && memchr::memchr(0, b).is_some() => r.matcher.search_reader(searcher, b, &mut sink),
        Some(b) => r.matcher.search_slice(searcher, b, &mut sink),
        None => {
            // a positioned read on Windows moves the file's cursor, and this search reads from it
            #[cfg(windows)]
            let _ = std::io::Seek::seek(&mut &file, std::io::SeekFrom::Start(0));
            r.matcher.search_file(searcher, &file, &mut sink)
        }
    };
    if let Err(e) = res {
        c.failed(format!("{rel}: {e}"));
    }
    report(r, sink, rel, named);
}

/// The end of one file's search: what it found leaves (a search aborted by an error or
/// cancellation still reports it). As ripgrep, a file the walk found that holds a NUL byte has
/// no count and is not listed.
fn report(r: &Run, mut sink: Collector, rel: Arc<str>, named: bool) {
    let o = r.o;
    sink.flush();
    if sink.matched && (named || !sink.binary || !(o.count_only || o.files_only)) {
        r.c.files_matched.fetch_add(1, Ordering::Relaxed);
        if o.count_only || o.files_only {
            // one record per file carries the count in `line`
            let _ = r.tx.send(vec![Hit { path: rel, line: sink.file_count, column: 0, text: String::new(), spans: Vec::new(), context: false }]);
        }
    }
}

/// `-z`: every member of an archive, or the decompressed file, as a file of its own, reported as
/// `archive.zip{member}` (the decompressed file keeps its own name).
fn search_archive(run: &Run, path: &Path, rel: &Arc<str>, kind: crate::archive::Kind) {
    let (c, cancel) = (run.c, run.cancel);
    // the searcher holds a whole line, and a small compressed file can expand to one line of
    // gigabytes; -U holds the whole member, so a limit would refuse large members that are fine
    let limit = (!run.o.multiline).then_some(crate::archive::MAX_LINE);
    let searcher = &mut make_searcher_limited(run.o, BinaryDetection::quit(0), limit);
    let mut each = |member: Option<&str>, r: &mut dyn std::io::Read| -> bool {
        let label: Arc<str> = match member {
            Some(m) if run.o.raw_member_names => Arc::from(format!("{rel}{{{m}}}")),
            Some(m) => Arc::from(format!("{rel}{{{}}}", crate::fmt::shown(m))),
            None => Arc::clone(rel),
        };
        let mut sink = run.collector(Arc::clone(&label));
        let res = run.matcher.search_reader(searcher, r, &mut sink);
        match res {
            // grep-searcher's own words for the limit name no line
            Err(e) if e.to_string().starts_with("configured allocation limit") => {
                c.failed(format!("{label}: a line longer than {} MiB; stopped", crate::archive::MAX_LINE >> 20))
            }
            Err(e) => c.failed(format!("{label}: {e}")),
            Ok(()) => {}
        }
        report(run, sink, label, false);
        !cancel.load(Ordering::Relaxed)
    };
    let mut failed = |what: String| c.failed(format!("{rel}: {what}"));
    if let Err(e) = crate::archive::walk(path, kind, &mut each, &mut failed) {
        c.failed(format!("{rel}: {e}"));
    }
}

/// Files up to this size are read with one call (`split::first`) instead of reading until a
/// zero-length read; within it the searcher's binary check covers the whole file.
const SMALL_FILE: u64 = 64 << 10;
/// Files up to this size are streamed by a searcher made for the file, larger ones by the
/// thread's. Reading them whole instead cost each thread a buffer the size of the largest such
/// file it met (py-big: 12.6 MB peak resident on 5 threads, 8.2 MB streamed) for 1% fewer
/// instructions at most (py-big and Linux).
const MID_FILE: u64 = 1 << 20;

/// Line-at-a-time searches whose results do not depend on neighbouring lines or on a running count.
/// `-l` stays sequential: it stops at the first match. On one thread a file is never split, but a
/// file of one long line is still read into a buffer of its size (`split::search`).
fn splittable(o: &Options) -> bool {
    !o.multiline && o.before == 0 && o.after == 0 && o.max_count.is_none() && o.max_results.is_none() && !o.files_only
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

    /// Reads into `buf`, kept by the thread across files (sized once, never zeroed again).
    pub(super) fn first(file: &File, buf: &mut Vec<u8>) -> First {
        let max = SMALL_FILE as usize;
        if buf.len() <= max {
            *buf = vec![0; max + 1];
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

    /// The whole file when it holds no NUL byte (a slice search checks only its start for binary
    /// data) and is still `size` bytes long. Each thread fills its own part of the buffer: the time
    /// goes to the kernel copying pages into fresh memory, and that work spreads as far as opening
    /// files does (100 MB on macOS: 12.6 ms on one thread, 4.0 ms on 5, 6.7 ms on 11).
    fn whole(file: &File, size: u64, threads: usize) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; usize::try_from(size).ok()?];
        let part = buf.len().div_ceil(threads.max(1)).max(1 << 20);
        let filled = std::thread::scope(|s| {
            let hs: Vec<_> = buf.chunks_mut(part).enumerate().map(|(i, b)| s.spawn(move || fill(file, b, (i * part) as u64))).collect();
            // the scope waits for the threads this stops joining at the first failure
            hs.into_iter().all(|h| h.join().unwrap_or(false))
        });
        filled.then_some(buf)
    }

    /// Fills `buf` from `at` in the file; false on a NUL byte, a failed read or a shorter file.
    fn fill(file: &File, buf: &mut [u8], at: u64) -> bool {
        let mut done = 0;
        while done < buf.len() {
            // a piece at a time, so the NUL check reads it while it is still in cache
            let end = buf.len().min(done + (1 << 20));
            match read_at(file, &mut buf[done..end], at + done as u64) {
                Ok(n) if n > 0 && memchr::memchr(0, &buf[done..done + n]).is_none() => done += n,
                _ => return false,
            }
        }
        true
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
    pub(super) fn search(r: &Run, file: &File, size: u64, chunk: u64, rel: &Arc<str>) -> Outcome {
        let Run { o, c, cancel, tx, plain, .. } = *r;
        let mut head = [0u8; 3];
        match read_at(file, &mut head, 0) {
            Ok(n) if n >= 2 && (head[..2] == [0xFF, 0xFE] || head[..2] == [0xFE, 0xFF] || (n == 3 && head == [0xEF, 0xBB, 0xBF])) => return Outcome::Stream,
            Ok(_) => {}
            Err(_) => return Outcome::Stream,
        }
        // one thread only asks whether the file has a line break to split at: the stream holds 3^k
        // times 64 KiB for a long line (141.6 MB peak resident for one 100 MB line, 103.9 whole)
        let one = range_threads(o) == 1;
        let mut bounds = vec![0u64];
        let mut probe = vec![0u8; 64 << 10];
        let mut x = chunk;
        while x < size && !(one && bounds.len() > 1) {
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
            return whole(file, size, default_threads(o)).map_or(Outcome::Stream, Outcome::Buffer);
        }
        if one {
            return Outcome::Stream;
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
                let hs: Vec<_> = rs.iter().zip(bs).map(|(&(a, b), &base)| s.spawn(move || search_range(r, file, a, b, base, rel))).collect();
                hs.into_iter().map(|h| h.join().unwrap_or(Part { out: Vec::new(), hits: Vec::new(), matched: false, count: 0, ok: false })).collect()
            });
            for part in parts {
                matched |= part.matched;
                count += part.count;
                if let Some(p) = plain {
                    p.write(&part.out, false, cancel);
                } else if !part.hits.is_empty() {
                    let _ = tx.send(part.hits);
                }
                if !part.ok {
                    // the file changed after the first pass, or a read failed: what was found so far stands
                    c.failed(format!("{rel}: changed while it was searched, or a read failed"));
                    break 'windows;
                }
            }
            if let Some(p) = plain {
                p.tick(cancel);
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

    fn search_range(r: &Run, file: &File, a: u64, b: u64, base: u64, rel: &Arc<str>) -> Part {
        let (o, cancel) = (r.o, r.cancel);
        // its own matcher (its own regex cache pool) and its own searcher, without transcoding: the
        // middle of a file is not the start of a stream
        let matcher = r.matcher.clone();
        let mut searcher =
            SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).bom_sniffing(false).line_number(!o.count_only).invert_match(o.invert).build();
        let (ptx, prx) = mpsc::channel::<Vec<Hit>>();
        // with `hold` the plain text is formatted into `out` and never written to this sink
        let sink_plain = PlainSink::new(Box::new(std::io::sink()));
        let range_run = Run { matcher: &matcher, tx: &ptx, plain: if r.plain.is_some() { Some(&sink_plain) } else { None }, ..*r };
        let mut sink = Collector {
            max_results: None,
            files_only: false,
            line_base: base,
            hold: true,
            multiline: false, // never split in multiline mode
            ..range_run.collector(Arc::clone(rel))
        };
        let mut range = Range { file, pos: a, end: b };
        let res = matcher.search_reader(&mut searcher, &mut range, &mut sink);
        sink.flush();
        let ok = res.is_ok() && !sink.binary && (range.pos >= b || cancel.load(Ordering::Relaxed));
        let (out, matched, count) = (std::mem::take(&mut sink.out), sink.matched, sink.file_count);
        drop(sink);
        drop(ptx);
        Part { out, hits: prx.into_iter().flatten().collect(), matched, count, ok }
    }
}

/// Hand the hits to the caller in arrival order.
fn drain(rx: mpsc::Receiver<Vec<Hit>>, mut on_hit: impl FnMut(Hit)) {
    for batch in rx {
        batch.into_iter().for_each(&mut on_hit);
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

fn run_walk(roots: &[PathBuf], o: &Options, cancel: Arc<AtomicBool>, plain: Option<Arc<PlainSink>>, mut on_hit: impl FnMut(Hit)) -> Result<Stats> {
    let t0 = Instant::now();
    if roots.is_empty() {
        bail!("no paths to search");
    }
    let matcher = Arc::new(build_matcher(o)?);
    let counters = Arc::new(Counters::default());
    let rest: Vec<PathBuf>;
    let stdin_too = o.paths_as_given && roots.iter().any(|r| r.as_os_str() == "-");
    let given = roots;
    let roots = if stdin_too {
        rest = roots.iter().filter(|r| r.as_os_str() != "-").cloned().collect();
        &rest[..]
    } else {
        roots
    };
    let pseudo = pseudo_roots(roots);
    let with_pseudo;
    let o = if pseudo.is_empty() {
        o
    } else {
        with_pseudo = Options { read_to_end_under: pseudo, ..o.clone() };
        &with_pseudo
    };
    if o.first_in_path_order {
        if plain.is_some() {
            bail!("first_in_path_order needs a hit sink, not plain output");
        }
        // as `rg --sort path`: the roots in the order given, each walked in path order
        let mut files = Vec::new();
        for root in given {
            if stdin_too && root.as_os_str() == "-" {
                files.push((PathBuf::from("-"), Arc::from(STDIN_LABEL), true));
                continue;
            }
            let at = files.len();
            files.extend(list_files(std::slice::from_ref(root), roots, o, &counters)?);
            files[at..].sort_by(|a, b| Path::new(&*a.1).cmp(Path::new(&*b.1)));
        }
        return Ok(first_in_path_order(files, o.max_results, &matcher, o, &counters, &cancel, on_hit, t0));
    }
    if stdin_too {
        search_stdin(o, &matcher, &counters, &cancel, plain.as_deref(), &mut on_hit);
    }
    if roots.is_empty() {
        return Ok(stats(&counters, t0, &cancel));
    }
    // the overrides are anchored at one root: a glob naming a directory needs a walk per root
    if roots.len() > 1 && o.globs.iter().any(|g| g.contains('/')) {
        for root in roots.chunks(1) {
            if !cancel.load(Ordering::Relaxed) {
                walk(root, roots, o, &matcher, &counters, &cancel, plain.clone(), &mut on_hit)?;
            }
        }
    } else {
        walk(roots, roots, o, &matcher, &counters, &cancel, plain, &mut on_hit)?;
    }
    Ok(stats(&counters, t0, &cancel))
}

/// Search standard input, reported as `STDIN_LABEL`.
fn search_stdin(o: &Options, matcher: &AnyMatcher, c: &Counters, cancel: &AtomicBool, plain: Option<&PlainSink>, on_hit: &mut impl FnMut(Hit)) {
    let (tx, rx) = mpsc::channel::<Vec<Hit>>();
    c.alone.store(true, Ordering::Relaxed);
    std::thread::scope(|s| {
        s.spawn(move || {
            let mut fw = file_worker(o);
            search_one(&mut fw, &Run { matcher, o, c, cancel, tx: &tx, plain }, Path::new("-"), Arc::from(STDIN_LABEL), None, true);
        });
        drain(rx, &mut *on_hit);
    });
    c.alone.store(false, Ordering::Relaxed);
}

/// A walk joins the count of big searches running (`crate::running`) once it has searched this
/// many files, so a repository smaller than that never makes the system calls. A walk starts
/// with all its threads and the ones over its share pause.
const COUNTED_AFTER: u64 = 1000;

/// `SCRUTATE_GREP_DEBUG`: a line on stderr when a walk joins the count of big searches running
/// and when that count changes, with the threads it takes (`None`: no count on this system).
fn show_share(shown: &AtomicUsize, running: Option<usize>, threads: usize) {
    static DEBUG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*DEBUG.get_or_init(|| std::env::var_os("SCRUTATE_GREP_DEBUG").is_some_and(|v| !v.is_empty())) {
        return;
    }
    let key = running.map_or(usize::MAX, |k| k << 16 | threads);
    if shown.swap(key, Ordering::Relaxed) != key {
        match running {
            Some(k) => eprintln!("[scrutate] searches running: {k}, threads: {threads}"),
            None => eprintln!("[scrutate] searches running: not counted, threads: {threads}"),
        }
    }
}

/// Search the files under `roots`, reported as found under `labels`.
#[allow(clippy::too_many_arguments)]
fn walk(
    roots: &[PathBuf],
    labels: &[PathBuf],
    o: &Options,
    matcher: &Arc<AnyMatcher>,
    counters: &Arc<Counters>,
    cancel: &Arc<AtomicBool>,
    plain: Option<Arc<PlainSink>>,
    on_hit: &mut impl FnMut(Hit),
) -> Result<()> {
    let searched = match small_first(roots, labels, o, matcher, counters, cancel, plain.as_deref(), on_hit) {
        Small::Done => return Ok(()),
        Small::Big(seen) => Arc::new(seen),
    };
    let threads = default_threads(o);
    // counted once the walk has proved big, and only without -j: a number asked for is kept
    let counted = (o.threads == 0).then(|| Arc::new(std::sync::OnceLock::<Option<crate::running::Counted>>::new()));
    let share = Arc::new(AtomicUsize::new(threads));
    let shown = Arc::new(AtomicUsize::new(0));
    // raised as reads turn out to come from the disk: the walk grows by that many threads
    let cold = Arc::new(AtomicUsize::new(cold_extra(o, counters)));
    // the threads the walk runs: the share plus the cold ones
    let want = Arc::new(AtomicUsize::new(threads + cold.load(Ordering::Relaxed)));
    let mut wb = walk_builder(roots, o)?;
    wb.threads(threads);
    if o.threads == 0 {
        wb.grow_threads(cold_threads().saturating_sub(threads), Arc::clone(&want));
    }
    let walker = wb.build_parallel();
    let (tx, rx) = mpsc::channel::<Vec<Hit>>();
    let labels_owned: Arc<[PathBuf]> = labels.into();
    let matcher = Arc::clone(matcher);
    let opts = Arc::new(o.clone());
    let (c2, cancel2) = (Arc::clone(counters), Arc::clone(cancel));
    let worker = std::thread::spawn(move || {
        walker.run(|| {
            let tx = tx.clone();
            // a clone per thread: each gets its own regex cache pool instead of contending for one
            let matcher = AnyMatcher::clone(&matcher);
            let cancel = Arc::clone(&cancel2);
            let c = Arc::clone(&c2);
            let opts = Arc::clone(&opts);
            let labels = Arc::clone(&labels_owned);
            let plain = plain.clone();
            let searched = Arc::clone(&searched);
            let (cold, want, share, shown) = (Arc::clone(&cold), Arc::clone(&want), Arc::clone(&share), Arc::clone(&shown));
            let counted = counted.clone();
            let mut files = 0u32;
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
                if !searchable(&entry) {
                    c.met_dir(&entry);
                    return WalkState::Continue;
                }
                if let Some(n) = if searched.is_empty() { None } else { searched.get(entry.path()) } {
                    let mut left = n.load(Ordering::Relaxed);
                    while left > 0 {
                        match n.compare_exchange_weak(left, left - 1, Ordering::Relaxed, Ordering::Relaxed) {
                            Ok(_) => return WalkState::Continue,
                            Err(now) => left = now,
                        }
                    }
                }
                let size = if opts.want_bytes { entry.metadata().ok().map(|m| m.len()) } else { None };
                let rel = label(entry.path(), &labels, &opts);
                let fw = fw.get_or_insert_with(|| file_worker(&opts));
                let named = opts.paths_as_given && entry.depth() == 0;
                search_one(fw, &Run { matcher: &matcher, o: &opts, c: &c, cancel: &cancel, tx: &tx, plain: plain.as_deref() }, entry.path(), rel, size, named);
                let extra = cold_extra(&opts, &c);
                let mut changed = extra > cold.load(Ordering::Relaxed);
                if changed {
                    cold.fetch_max(extra, Ordering::Relaxed);
                }
                // looked at again as searches start and end; a small tree never gets here
                files = files.wrapping_add(1);
                if let Some(counted) = counted.as_ref().filter(|_| files % 64 == 16 && c.files_searched.load(Ordering::Relaxed) >= COUNTED_AFTER) {
                    let n = counted.get_or_init(crate::running::Counted::start);
                    let k = n.as_ref().and_then(|n| n.running());
                    let now = k.map_or(threads, |k| crate::running::threads(threads, k));
                    changed |= share.swap(now, Ordering::Relaxed) != now;
                    show_share(&shown, k, now);
                }
                if changed {
                    want.store(share.load(Ordering::Relaxed) + cold.load(Ordering::Relaxed), Ordering::Relaxed);
                }
                if cancel.load(Ordering::Relaxed) {
                    WalkState::Quit
                } else {
                    WalkState::Continue
                }
            })
        });
    });
    drain(rx, &mut *on_hit);
    worker.join().map_err(|_| anyhow!("search worker panicked"))?;
    Ok(())
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
    let scoped = o.scoped_globs.as_ref().map(|sg| (sg, sg.scope(root)));
    let mut files: Vec<(PathBuf, Arc<str>, bool)> = files
        .into_iter()
        .filter(|rel| !filtered_out(&ov, rel) && !scoped.as_ref().is_some_and(|(sg, s)| sg.excludes_file(s, rel)))
        .map(|rel| (root.join(&rel), Arc::from(rel), false))
        .collect();
    let counters = Counters::default();
    if o.first_in_path_order {
        if plain.is_some() {
            bail!("first_in_path_order needs a hit sink, not plain output");
        }
        files.sort_by(|a, b| Path::new(&*a.1).cmp(Path::new(&*b.1)));
        return Ok(first_in_path_order(files, o.max_results, &matcher, o, &counters, &cancel, on_hit, t0));
    }
    let threads = default_threads(o).min(files.len().max(1));
    search_list(&files, threads, None, &matcher, o, &counters, &cancel, plain.as_deref(), on_hit);
    Ok(stats(&counters, t0, &cancel))
}

/// The files under `roots` the search would reach (path, path as reported, named on the command
/// line), for `first_in_path_order`.
fn list_files(roots: &[PathBuf], labels: &[PathBuf], o: &Options, c: &Counters) -> Result<Vec<(PathBuf, Arc<str>, bool)>> {
    let found = Mutex::new(Vec::new());
    walk_builder(roots, o)?.build_parallel().run(|| {
        Box::new(|entry| {
            match entry {
                Ok(e) if searchable(&e) => {
                    let item = (e.path().to_path_buf(), label(e.path(), labels, o), o.paths_as_given && e.depth() == 0);
                    found.lock().unwrap_or_else(|p| p.into_inner()).push(item);
                }
                Ok(e) => c.met_dir(&e),
                Err(e) => c.failed(e.to_string()),
            }
            WalkState::Continue
        })
    });
    Ok(found.into_inner().unwrap_or_else(|p| p.into_inner()))
}

/// `Options::first_in_path_order`: search `files` in their order until `n` matches are in, let the
/// files started meanwhile finish, then hand over the first `n` matches (all without `n`) in file
/// and line order.
#[allow(clippy::too_many_arguments)]
fn first_in_path_order(
    files: Vec<(PathBuf, Arc<str>, bool)>,
    n: Option<u64>,
    matcher: &AnyMatcher,
    o: &Options,
    c: &Counters,
    cancel: &AtomicBool,
    mut on_hit: impl FnMut(Hit),
    t0: Instant,
) -> Stats {
    // no file holds more than `n` of the first `n`; the cut below stops the rest
    let o = Options { max_results: None, max_count: n.map(|n| o.max_count.map_or(n, |m| m.min(n))).or(o.max_count), ..o.clone() };
    let mut hits = Vec::new();
    let threads = default_threads(&o).min(files.len().max(1));
    search_list(&files, threads, n, matcher, &o, c, cancel, None, |h| hits.push(h));
    // a file that several roots reach is listed once per root under one label: the k-th copy of a
    // hit belongs to its k-th listing
    let mut listed: std::collections::HashMap<&str, Vec<usize>> = Default::default();
    for (i, f) in files.iter().enumerate() {
        listed.entry(&*f.1).or_default().push(i);
    }
    let listing = |label: &str| {
        let mut s = label;
        loop {
            if let Some(v) = listed.get(s) {
                return Some(v);
            }
            // an archive member, `archive.zip{member}`
            s = &s[..s.rfind('{')?];
        }
    };
    let at: Vec<usize> = {
        let mut copies: std::collections::HashMap<(&str, u64, u64), usize> = Default::default();
        hits.iter()
            .map(|h| {
                let copy = copies.entry((&h.path, h.line, h.column)).or_insert(0);
                *copy += 1;
                listing(&h.path).map_or(usize::MAX, |v| v[(*copy - 1).min(v.len() - 1)])
            })
            .collect()
    };
    let mut keyed: Vec<(usize, Hit)> = at.into_iter().zip(hits).collect();
    keyed.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.path.cmp(&b.1.path)).then(a.1.line.cmp(&b.1.line)));
    let cap = n.unwrap_or(u64::MAX);
    let (mut left, mut files_matched, mut last) = (cap, 0u64, None::<Arc<str>>);
    for (_, mut h) in keyed {
        if left == 0 {
            break;
        }
        if !h.context && h.line != NOTE_LINE {
            let take = if o.count_only { h.line.min(left) } else { 1 };
            if o.count_only {
                h.line = take;
            }
            left -= take;
            if last.as_ref() != Some(&h.path) {
                files_matched += 1;
                last = Some(Arc::clone(&h.path));
            }
        }
        on_hit(h);
    }
    let total = c.matches.load(Ordering::Relaxed);
    match n {
        Some(n) => Stats { matches: n - left, files_matched, cancelled: cancel.load(Ordering::Relaxed) || total >= n, ..stats(c, t0, cancel) },
        None => stats(c, t0, cancel),
    }
}

/// Search a known list of files (path, path as reported, named) on `threads` threads, handed out
/// in list order; with `stop_at`, no file is started once that many matches are in.
#[allow(clippy::too_many_arguments)]
fn search_list(
    files: &[(PathBuf, Arc<str>, bool)],
    threads: usize,
    stop_at: Option<u64>,
    matcher: &AnyMatcher,
    o: &Options,
    counters: &Counters,
    cancel: &AtomicBool,
    plain: Option<&PlainSink>,
    on_hit: impl FnMut(Hit),
) {
    let (tx, rx) = mpsc::channel::<Vec<Hit>>();
    let next = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|s| {
        for _ in 0..threads {
            let tx = tx.clone();
            let next = &next;
            // a clone per thread: each gets its own regex cache pool instead of contending for one
            let matcher = matcher.clone();
            s.spawn(move || {
                let matcher = &matcher;
                let mut fw = file_worker(o);
                loop {
                    // checked before a file is taken, so the files searched are always the first ones
                    if cancel.load(Ordering::Relaxed) || stop_at.is_some_and(|n| counters.matches.load(Ordering::Relaxed) >= n) {
                        break;
                    }
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some((path, rel, named)) = files.get(i) else { break };
                    let size = if o.want_bytes { std::fs::metadata(path).ok().map(|m| m.len()) } else { None };
                    search_one(&mut fw, &Run { matcher, o, c: counters, cancel, tx: &tx, plain }, path, Arc::clone(rel), size, *named);
                }
            });
        }
        drop(tx);
        drain(rx, on_hit);
    });
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

/// The roots that lie on a kernel pseudo filesystem: one `statfs` per root, none per file. The
/// walk finds those below a root (`Counters::met_dir`).
#[cfg(target_os = "linux")]
fn pseudo_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    roots.iter().filter(|r| on_pseudo_fs(r)).cloned().collect()
}

#[cfg(not(target_os = "linux"))]
fn pseudo_roots(_roots: &[PathBuf]) -> Vec<PathBuf> {
    Vec::new()
}

/// `p` is on procfs, sysfs or another kernel pseudo filesystem, whose files report a size of 0
/// and return a page or less per read.
#[cfg(target_os = "linux")]
fn on_pseudo_fs(p: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    extern "C" {
        fn statfs(path: *const std::ffi::c_char, buf: *mut u64) -> std::ffi::c_int;
    }
    let Ok(c) = std::ffi::CString::new(p.as_os_str().as_bytes()) else { return false };
    // larger than `struct statfs` on every Linux ABI; `f_type` is its first field
    let mut buf = [0u64; 32];
    if unsafe { statfs(c.as_ptr(), buf.as_mut_ptr()) } != 0 {
        return false;
    }
    #[cfg(target_arch = "s390x")]
    let f_type = (buf[0] >> 32) as u32;
    #[cfg(not(target_arch = "s390x"))]
    let f_type = unsafe { *(buf.as_ptr() as *const std::ffi::c_long) } as u32;
    // proc, sysfs, debugfs, tracefs, securityfs, configfs, cgroup, cgroup2
    matches!(f_type, 0x9fa0 | 0x6265_6572 | 0x6462_6720 | 0x7472_6163 | 0x7363_6673 | 0x6265_6570 | 0x0027_e0eb | 0x6367_7270)
}

/// How a file found under `roots` is reported (see `Options::paths_as_given`).
fn label(path: &Path, roots: &[PathBuf], o: &Options) -> Arc<str> {
    if !o.paths_as_given {
        return Arc::from(relative(path, roots));
    }
    let s = path.to_string_lossy();
    if cfg!(windows) {
        Arc::from(slashed(s))
    } else {
        Arc::from(&*s)
    }
}

/// A path as reported: `/` between its parts. Only Windows has `\` as a separator; elsewhere it is
/// a character of the name.
pub fn slashed(s: std::borrow::Cow<'_, str>) -> String {
    if cfg!(windows) {
        windows_form(&s)
    } else {
        s.into_owned()
    }
}

/// A Windows path as it is printed: `/` between its parts. A verbatim drive path (`\\?\C:\x`,
/// what `canonicalize` gives) takes no forward slashes, so it is printed in its plain form.
fn windows_form(s: &str) -> String {
    let s = s.replace('\\', "/");
    match s.strip_prefix("//?/") {
        Some(rest) if rest.as_bytes().get(1) == Some(&b':') => rest.to_string(),
        _ => s,
    }
}

pub fn relative(path: &Path, roots: &[PathBuf]) -> String {
    for r in roots {
        if let Ok(rel) = path.strip_prefix(r) {
            let s = slashed(rel.to_string_lossy());
            if s.is_empty() {
                // the root is the file itself
                return slashed(r.to_string_lossy());
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

    thread_local! {
        /// the threads `small_first` started on this thread
        pub(super) static SMALL_WORKERS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    }

    #[test]
    fn a_thread_keeps_no_buffer_the_size_of_a_larger_file() {
        let root = std::env::temp_dir().join(format!("scrutate_thread_buffer_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("big.txt");
        std::fs::write(&path, format!("needle first\n{}needle last\n", "x\n".repeat(150_000))).unwrap();
        let o = Options { pattern: "needle".into(), mode: Mode::Literal, threads: 2, ..Options::default() };
        let matcher = build_matcher(&o).unwrap();
        let (c, cancel) = (Counters::default(), AtomicBool::new(false));
        let (tx, rx) = mpsc::channel();
        let mut fw = file_worker(&o);
        for size in [None, Some(std::fs::metadata(&path).unwrap().len())] {
            search_one(&mut fw, &Run { matcher: &matcher, o: &o, c: &c, cancel: &cancel, tx: &tx, plain: None }, &path, Arc::from("big.txt"), size, false);
        }
        drop(tx);
        let lines: Vec<u64> = rx.into_iter().flatten().map(|h| h.line).collect();
        assert_eq!(lines, [1, 150_002, 1, 150_002]);
        assert!(fw.small.capacity() <= SMALL_FILE as usize + 1, "{}", fw.small.capacity());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_tree_of_one_file_is_searched_without_a_thread() {
        let root = std::env::temp_dir().join(format!("scrutate_small_threads_{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "hello\n").unwrap();
        let o = Options { pattern: "hello".into(), mode: Mode::Literal, threads: 2, ..Options::default() };
        let found = |o: &Options| {
            SMALL_WORKERS.with(|n| n.set(0));
            let mut lines = Vec::new();
            search(std::slice::from_ref(&root), o, Arc::new(AtomicBool::new(false)), |h| lines.push(h.text)).unwrap();
            lines.sort();
            (lines, SMALL_WORKERS.with(|n| n.get()))
        };
        assert_eq!(found(&o), (vec!["hello".to_string()], 0));
        std::fs::write(root.join("b.txt"), "hello again\n").unwrap();
        assert_eq!(found(&o), (vec!["hello".to_string(), "hello again".to_string()], 2));
        // a walk cut after the first file still searches it, and the parallel walk the rest
        let cut = Options { small_tree: Some(1), ..o.clone() };
        assert_eq!(found(&cut), (vec!["hello".to_string(), "hello again".to_string()], 0));
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_windows_path_is_printed_with_slashes_and_a_verbatim_drive_path_in_its_plain_form() {
        assert_eq!(windows_form(r"C:\Users\x\a.txt"), "C:/Users/x/a.txt");
        assert_eq!(windows_form(r"\\?\C:\Users\x\a.txt"), "C:/Users/x/a.txt");
        assert_eq!(windows_form(r"dir\a.txt"), "dir/a.txt");
        assert_eq!(windows_form(r"\\server\share\a.txt"), "//server/share/a.txt");
        assert_eq!(windows_form(r"\\?\UNC\server\share\a.txt"), "//?/UNC/server/share/a.txt");
    }

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
            (r"\S*_resume\($", Case::Sensitive),
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
                let b = Blocks::new(&fuzzy_classes(std::str::from_utf8(&pat).unwrap(), false));
                let (mut pv, mut mv) = b.start();
                let mut score = m as isize;
                let mut got = None;
                for i in 0..hay.len() {
                    score += b.step(&mut pv, &mut mv, b.eq_at(&hay, i).0);
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

    /// `naive_first_end` over characters: each unit is a character or a byte of invalid UTF-8.
    fn naive_first_end_chars(classes: &[Vec<char>], hay: &[u8], k: usize) -> Option<(usize, usize)> {
        let m = classes.len();
        let mut col: Vec<usize> = (0..=m).collect();
        let mut i = 0;
        while i < hay.len() {
            let (c, n) = if hay[i] < 0x80 { (Some(hay[i] as char), 1) } else { wide_char(hay, i) };
            i += n;
            if c == Some('\n') {
                col = (0..=m).collect();
                continue;
            }
            let mut next = vec![0usize; m + 1];
            for j in 1..=m {
                let same = c.is_some_and(|c| classes[j - 1].contains(&c));
                next[j] = (next[j - 1] + 1).min(col[j] + 1).min(col[j - 1] + usize::from(!same));
            }
            col = next;
            if col[m] <= k {
                return Some((i, col[m]));
            }
        }
        None
    }

    #[test]
    fn fuzzy_edits_are_characters_and_case_folds_as_in_the_regex_engine() {
        assert_eq!(fuzzy_find("naïve", 1, "a naive b"), Some((2, 7)));
        assert_eq!(fuzzy_find("naive", 1, "a naïve b"), Some((2, 8)));
        assert_eq!(fuzzy_find("日本語", 1, "日本誤のテキスト"), Some((0, 6)));
        assert_eq!(fuzzy_find("naive", 1, "na\u{0}ve"), Some((0, 5)));
        let m = FuzzyMatcher::new("naive", 1, false).unwrap();
        assert_eq!(m.find_at(b"x na\xffve", 0).unwrap().map(|s| (s.start(), s.end())), Some((2, 7)), "invalid UTF-8 is one unit per byte");
        assert_eq!(m.find_at(b"\xff\xfe\xc3", 0).unwrap(), None);
        for (pat, hay) in [("ÉTÉ", "été"), ("été", "ÉTÉ"), ("kelvin", "\u{212A}ELVIN"), ("ß", "ẞ"), ("Σ", "ς")] {
            let m = FuzzyMatcher::new(pat, 0, true).unwrap();
            assert!(m.find_at(hay.as_bytes(), 0).unwrap().is_some(), "{pat} in {hay}");
            assert!(regex::RegexBuilder::new(pat).case_insensitive(true).build().unwrap().is_match(hay), "the regex engine agrees on {pat}");
        }
        let mut state = 0x9E37_79B9_7F4A_7C15u64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let alpha = ["a", "b", "é", "É", "ü", "日", "k", "\u{212A}", "\u{ff}"];
        for round in 0..600 {
            let fold = next(2) == 0;
            let long = next(4) == 0;
            let m = if long { 129 + next(60) as usize } else { 1 + next(9) as usize };
            let pat: String = (0..m).map(|_| alpha[next(8) as usize]).collect();
            let mut hay: Vec<u8> = Vec::new();
            for _ in 0..next(if long { 400 } else { 40 }) {
                match next(12) {
                    0 => hay.push(0xff),
                    1 => hay.push(b'\n'),
                    _ => hay.extend_from_slice(alpha[next(alpha.len() as u64) as usize].as_bytes()),
                }
            }
            if next(2) == 0 {
                let at = (next(hay.len() as u64 + 1) as usize).min(hay.len());
                hay.splice(at..at, pat.bytes().take(pat.len() - 1));
            }
            let k = (next(4) as usize).min(m - 1);
            let f = FuzzyMatcher::new(&pat, k as u8, fold).unwrap();
            let want = naive_first_end_chars(&fuzzy_classes(&pat, fold), &hay, k);
            assert_eq!(f.first_end(&hay, 0), want, "round {round}: {pat:?} k={k} fold={fold} in {hay:?}");
            let found = f.find_at(&hay, 0).unwrap();
            assert_eq!(found.map(|s| s.end()), want.map(|w| w.0), "round {round}: the prefilter keeps every match");
            if let Some(s) = found {
                let mut at = 0;
                while at < s.start() {
                    at += unit_len(&hay, at);
                }
                assert_eq!(at, s.start(), "round {round}: a match starts on a character: {s:?} in {hay:?}");
            }
        }
    }

    #[test]
    fn fuzzy_case_fold() {
        let m = FuzzyMatcher::new("Token", 0, true).unwrap();
        assert!(m.find_at(b"a TOKEN b", 0).unwrap().is_some());
        let m = FuzzyMatcher::new("Token", 0, false).unwrap();
        assert!(m.find_at(b"a TOKEN b", 0).unwrap().is_none());
    }

    #[test]
    fn smart_case_counts_only_letters_that_stand_for_themselves() {
        for (pattern, regex, upper) in [
            ("foo", true, false),
            ("Foo", true, true),
            (r"\Sfoo", true, false),
            (r"\W+\Bfoo\d", true, false),
            (r"\pLfoo\p{Lu}x", true, false),
            (r"\SFoo", true, true),
            (r"foo\\Bar", true, true),
            (r"\Sfoo", false, true),
            (r"\x41bc", true, true),
            (r"\x61bc", true, false),
            (r"\x{41}bc", true, true),
            (r"\x{1F600}bc", true, false),
            (r"\u0041bc", true, true),
            (r"\u00e9bc", true, false),
            (r"\U00000041bc", true, true),
            (r"\xzz", true, false),
        ] {
            assert_eq!(uppercase_literal(pattern, regex), upper, "{pattern:?} regex={regex}");
        }
        let o = Options { pattern: "fooz".into(), mode: Mode::Fuzzy { max_edits: 1 }, word: true, ..Options::default() };
        assert!(build_matcher(&o).err().unwrap().to_string().contains("-w) is not available with --fuzzy"));
        let o = Options { pattern: "fooz".into(), mode: Mode::FuzzyRegex { max_edits: 1 }, multiline: true, ..Options::default() };
        assert!(build_matcher(&o).err().unwrap().to_string().contains("-U) is not available with --fuzzy"));
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

    #[test]
    fn joined_patterns_keep_each_mode_meaning() {
        let ps = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(join_patterns(&ps(&["a.b"]), Mode::Pcre2, true).unwrap(), (r"a\.b".to_string(), Mode::Pcre2));
        assert_eq!(join_patterns(&ps(&["a.b"]), Mode::Literal, false).unwrap(), ("a.b".to_string(), Mode::Literal));
        assert_eq!(join_patterns(&ps(&["a.b", "c"]), Mode::Literal, true).unwrap(), (r"(?:a\.b)|(?:c)".to_string(), Mode::Regex));
        assert_eq!(join_patterns(&ps(&["a.b", "c"]), Mode::Pcre2, true).unwrap().1, Mode::Pcre2);
        let (p, m) = join_patterns(&ps(&["fooBar", "x"]), Mode::Identifier, false).unwrap();
        assert_eq!((p.as_str(), m), (r"(?:(?i)\bfoo[_\-]?Bar\b)|(?:(?i)\bx\b)", Mode::Regex));
        let e = join_patterns(&ps(&["a", "b"]), Mode::Fuzzy { max_edits: 1 }, false).unwrap_err();
        assert_eq!(e.to_string(), "--fuzzy takes one pattern");
        let zip = Options { search_zip: true, hidden: true, ..Options::default() };
        assert!(zip.check_indexable().unwrap_err().to_string().starts_with("--index does not cover archive members"));
        assert!(Options { max_depth: Some(1), ..Options::default() }.check_indexable().unwrap_err().to_string().starts_with("--index searches the files"));
        assert!(Options::default().check_indexable().is_ok());
    }

    #[test]
    fn held_output_leaves_in_fewer_writes_and_never_waits_on_a_quiet_sink() {
        struct Record(Arc<Mutex<Vec<Vec<u8>>>>);
        impl Write for Record {
            fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().push(b.to_vec());
                Ok(b.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let writes = Arc::new(Mutex::new(Vec::new()));
        let sink = PlainSink::new(Box::new(Record(Arc::clone(&writes))));
        let cancel = AtomicBool::new(false);
        let written = || writes.lock().unwrap().concat();
        let quiet = || std::thread::sleep(std::time::Duration::from_millis(5));
        let mut all = Vec::new();
        for i in 0..1000 {
            let line = format!("file{i}:1:text\n");
            sink.write(line.as_bytes(), false, &cancel);
            all.extend_from_slice(line.as_bytes());
        }
        assert!(writes.lock().unwrap().len() < 500, "a burst leaves in few writes");
        quiet();
        sink.tick(&cancel);
        assert_eq!(written(), all, "held output leaves once it has waited");
        quiet();
        sink.write(b"late:1:text\n", false, &cancel);
        all.extend_from_slice(b"late:1:text\n");
        assert_eq!(written(), all, "output after a quiet spell leaves at once");
        sink.write(b"last:1:text\n", false, &cancel);
        sink.write_line(b"", b"long:1:", b"x\xFFy", false, &cancel);
        sink.flush().unwrap();
        all.extend_from_slice("last:1:text\nlong:1:x\u{FFFD}y\n".as_bytes());
        assert_eq!(written(), all);
        assert!(!sink.broken.load(Ordering::Relaxed) && !cancel.load(Ordering::Relaxed));
    }

    #[test]
    fn a_cold_tree_earns_threads_by_its_slow_reads() {
        let o = Options::default();
        let most = cold_threads().saturating_sub(default_threads(&o));
        let reads = |all: u64, slow: u64| {
            let c = Counters::default();
            c.small_reads.store(all, Ordering::Relaxed);
            c.slow_reads.store(slow, Ordering::Relaxed);
            cold_extra(&o, &c)
        };
        assert_eq!(reads(15, 15), 0, "too few reads to tell");
        assert_eq!(reads(1000, 700), 0, "mostly fast reads are a warm tree");
        assert_eq!(reads(16, 16), 1.min(most));
        assert_eq!(reads(100, 80), 5.min(most));
        assert_eq!(reads(1_000_000, 1_000_000), most);
        let c = Counters::default();
        c.small_reads.store(100, Ordering::Relaxed);
        c.slow_reads.store(100, Ordering::Relaxed);
        assert_eq!(cold_extra(&Options { threads: 4, ..Options::default() }, &c), 0, "-j is kept");
    }
}
