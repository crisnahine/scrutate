//! Approximate regular expressions (`grep --fuzzy-regex=N`): a line matches when some part of it is
//! within N insertions, deletions or substitutions of a string the pattern matches. The pattern
//! becomes a Glushkov automaton over bytes (one state per character position, at most 64), and the
//! search runs it bit-parallel with one state set per number of errors (Navarro and Raffinot,
//! "Flexible Pattern Matching in Strings", section 6.5). Byte-oriented: classes are ASCII, and
//! anchors and word boundaries are not supported.

use anyhow::{bail, Context, Result};
use grep_matcher::{LineTerminator, Match, Matcher, NoCaptures, NoError};
use regex_syntax::hir::{Class, Hir, HirKind};

pub const MAX_POSITIONS: usize = 64;

#[derive(Clone, Debug)]
pub struct FuzzyRegex {
    /// positions whose class holds the byte
    bytes: Box<[u64; 256]>,
    /// follow sets by 8-bit chunk of a state set: chunk c, value v -> union of follow(i), i in v
    follow: Box<[[u64; 256]; 8]>,
    first: u64,
    last: u64,
    k: usize,
    /// state sets before any text byte, per number of errors (deletions from the start)
    init: Vec<u64>,
    /// longest string the pattern matches, plus k (bounds where a match can start)
    span: Option<usize>,
}

struct Build {
    classes: Vec<[bool; 256]>,
    follow: Vec<u64>,
}

/// (nullable, first, last) of a sub-expression, positions added to `b`.
fn glushkov(h: &Hir, b: &mut Build) -> Result<(bool, u64, u64)> {
    let new_pos = |b: &mut Build, class: [bool; 256]| -> Result<u64> {
        if b.classes.len() == MAX_POSITIONS {
            bail!("fuzzy regex patterns are limited to {MAX_POSITIONS} character positions");
        }
        b.classes.push(class);
        b.follow.push(0);
        Ok(1u64 << (b.classes.len() - 1))
    };
    let concat = |b: &mut Build, (n1, f1, l1): (bool, u64, u64), (n2, f2, l2): (bool, u64, u64)| {
        for i in 0..b.follow.len() {
            if l1 & (1 << i) != 0 {
                b.follow[i] |= f2;
            }
        }
        (n1 && n2, f1 | if n1 { f2 } else { 0 }, l2 | if n2 { l1 } else { 0 })
    };
    Ok(match h.kind() {
        HirKind::Empty => (true, 0, 0),
        HirKind::Literal(lit) => {
            let mut acc = (true, 0, 0);
            for &byte in lit.0.iter() {
                let mut class = [false; 256];
                class[byte as usize] = true;
                let p = new_pos(b, class)?;
                acc = concat(b, acc, (false, p, p));
            }
            acc
        }
        HirKind::Class(c) => {
            let mut class = [false; 256];
            match c {
                Class::Bytes(bc) => {
                    for r in bc.iter() {
                        for x in r.start()..=r.end() {
                            class[x as usize] = true;
                        }
                    }
                }
                Class::Unicode(uc) => {
                    for r in uc.iter() {
                        if r.end() as u32 > 0x7F {
                            bail!("fuzzy regex classes must be ASCII");
                        }
                        for x in r.start() as u32..=r.end() as u32 {
                            class[x as usize] = true;
                        }
                    }
                }
            }
            class[b'\n' as usize] = false;
            let p = new_pos(b, class)?;
            (false, p, p)
        }
        HirKind::Look(_) => bail!("anchors and word boundaries are not supported with --fuzzy-regex"),
        HirKind::Capture(cap) => glushkov(&cap.sub, b)?,
        HirKind::Concat(items) => {
            let mut acc = (true, 0, 0);
            for it in items {
                let x = glushkov(it, b)?;
                acc = concat(b, acc, x);
            }
            acc
        }
        HirKind::Alternation(items) => {
            let mut acc = (false, 0, 0);
            for it in items {
                let (n, f, l) = glushkov(it, b)?;
                acc = (acc.0 || n, acc.1 | f, acc.2 | l);
            }
            acc
        }
        HirKind::Repetition(rep) => {
            let mut acc = (true, 0, 0);
            for _ in 0..rep.min {
                let x = glushkov(&rep.sub, b)?;
                acc = concat(b, acc, x);
            }
            match rep.max {
                None => {
                    // a starred copy: its last positions loop back to its first
                    let (n, f, l) = glushkov(&rep.sub, b)?;
                    for i in 0..b.follow.len() {
                        if l & (1 << i) != 0 {
                            b.follow[i] |= f;
                        }
                    }
                    let _ = n;
                    acc = concat(b, acc, (true, f, l));
                }
                Some(max) => {
                    for _ in rep.min..max {
                        let (_, f, l) = glushkov(&rep.sub, b)?;
                        acc = concat(b, acc, (true, f, l));
                    }
                }
            }
            acc
        }
    })
}

impl FuzzyRegex {
    pub fn new(pattern: &str, max_edits: u8, case_insensitive: bool) -> Result<FuzzyRegex> {
        let hir = regex_syntax::ParserBuilder::new()
            .unicode(false)
            .utf8(false)
            .case_insensitive(case_insensitive)
            .build()
            .parse(pattern)
            .with_context(|| format!("invalid pattern {pattern:?}"))?;
        let mut b = Build { classes: Vec::new(), follow: Vec::new() };
        let (nullable, first, last) = glushkov(&hir, &mut b)?;
        if nullable {
            bail!("a fuzzy regex must not match the empty string (every line would match)");
        }
        let mut bytes = Box::new([0u64; 256]);
        for (p, class) in b.classes.iter().enumerate() {
            for (x, &on) in class.iter().enumerate() {
                if on {
                    bytes[x] |= 1 << p;
                }
            }
        }
        let mut follow = Box::new([[0u64; 256]; 8]);
        for (c, table) in follow.iter_mut().enumerate() {
            for (v, slot) in table.iter_mut().enumerate() {
                for bit in 0..8 {
                    let p = c * 8 + bit;
                    if v & (1 << bit) != 0 && p < b.follow.len() {
                        *slot |= b.follow[p];
                    }
                }
            }
        }
        let k = max_edits as usize;
        let mut m = FuzzyRegex { bytes, follow, first, last, k, init: Vec::new(), span: hir.properties().maximum_len().map(|n| n + k) };
        // deleting j pattern characters before any text is read
        let mut d = 0u64;
        for _ in 0..=k {
            m.init.push(d);
            d = m.step(d);
        }
        Ok(m)
    }

    /// The positions reachable in one move from `d`, or from the start (always active: search).
    fn step(&self, d: u64) -> u64 {
        let mut out = self.first;
        for c in 0..8 {
            out |= self.follow[c][((d >> (8 * c)) & 0xff) as usize];
        }
        out
    }

    /// End (exclusive) and number of errors of the first match in `hay[at..]`.
    fn first_end(&self, hay: &[u8], at: usize) -> Option<(usize, usize)> {
        let k = self.k;
        let mut r = self.init.clone();
        let mut next = vec![0u64; k + 1];
        for (i, &c) in hay[at..].iter().enumerate() {
            if c == b'\n' {
                r.copy_from_slice(&self.init);
                continue;
            }
            let bc = self.bytes[c as usize];
            next[0] = self.step(r[0]) & bc;
            for j in 1..=k {
                // match, insertion (text byte skipped), substitution, deletion (pattern position skipped)
                next[j] = (self.step(r[j]) & bc) | r[j - 1] | self.step(r[j - 1]) | self.step(next[j - 1]);
            }
            std::mem::swap(&mut r, &mut next);
            if let Some(j) = r.iter().position(|&s| s & self.last != 0) {
                return Some((at + i + 1, j));
            }
        }
        None
    }
}

impl Matcher for FuzzyRegex {
    type Captures = NoCaptures;
    type Error = NoError;

    fn find_at(&self, haystack: &[u8], at: usize) -> Result<Option<Match>, NoError> {
        let Some((end, _)) = self.first_end(haystack, at) else { return Ok(None) };
        let line_start = haystack[at..end].iter().rposition(|&b| b == b'\n').map(|i| at + i + 1).unwrap_or(at);
        let start = match self.span {
            Some(n) => end.saturating_sub(n).max(line_start),
            None => line_start,
        };
        Ok(Some(Match::new(start, end)))
    }

    fn new_captures(&self) -> Result<NoCaptures, NoError> {
        Ok(NoCaptures::new())
    }

    fn line_terminator(&self) -> Option<LineTerminator> {
        Some(LineTerminator::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(m: &impl Matcher, text: &str) -> Vec<usize> {
        text.lines().enumerate().filter(|(_, l)| m.find_at(l.as_bytes(), 0).ok().flatten().is_some()).map(|(i, _)| i).collect()
    }

    #[test]
    fn errors_and_syntax() {
        let m = FuzzyRegex::new("colou?r", 1, false).unwrap();
        assert_eq!(lines(&m, "color\ncolour\ncolr\ncoluor\nkolor\nxyz"), vec![0, 1, 2, 3, 4]);
        let m = FuzzyRegex::new("(get|set)_value", 1, false).unwrap();
        assert_eq!(lines(&m, "get_value\nset_valu\nput_value\nnet_vlue"), vec![0, 1]);
        let m = FuzzyRegex::new("(get|set)_value", 2, false).unwrap();
        assert_eq!(lines(&m, "get_value\nset_valu\nput_value\nnet_vlue\nfetch_all"), vec![0, 1, 2, 3]);
        let m = FuzzyRegex::new("Get_[a-z]+", 0, true).unwrap();
        assert_eq!(lines(&m, "GET_X\nget_name\nget-name"), vec![0, 1]);
        assert!(FuzzyRegex::new("^abc", 1, false).is_err());
        assert!(FuzzyRegex::new("a*", 1, false).is_err());
        assert!(FuzzyRegex::new(&"a".repeat(65), 1, false).is_err());
        assert!(FuzzyRegex::new("é", 0, false).is_ok());
    }

    #[test]
    fn agrees_with_myers_on_literals_and_with_the_regex_engine_exactly() {
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut next = |n: u64| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state % n
        };
        let mut text = String::new();
        for _ in 0..300 {
            let len = next(30);
            text.extend((0..len).map(|_| b"abcd_"[next(5) as usize] as char));
            text.push('\n');
        }
        for _ in 0..200 {
            let m = 2 + next(10) as usize;
            let pat: String = (0..m).map(|_| b"abcd"[next(4) as usize] as char).collect();
            let k = (next(3) as usize).min(m - 1);
            let fr = FuzzyRegex::new(&pat, k as u8, false).unwrap();
            let my = crate::instant::FuzzyMatcher::new(&pat, k as u8, false).unwrap();
            assert_eq!(lines(&fr, &text), lines(&my, &text), "{pat} k={k}");
        }
        for pat in ["ab(c|d)+a", "a[bc]{2,3}d", "(ab|cd)_?(ab)*c", "a.c", "[a-c]+d_b"] {
            let fr = FuzzyRegex::new(pat, 0, false).unwrap();
            let re = regex::Regex::new(pat).unwrap();
            let want: Vec<usize> = text.lines().enumerate().filter(|(_, l)| re.is_match(l)).map(|(i, _)| i).collect();
            assert_eq!(lines(&fr, &text), want, "{pat}");
        }
    }
}
