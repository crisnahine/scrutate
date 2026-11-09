//! Soundness of the text index: whenever a regex matches a text, the trigram condition derived from
//! the pattern must accept that text's trigrams (otherwise `grep --index` would miss the match).
#![no_main]
use libfuzzer_sys::fuzz_target;
use scrutate::instant::{Case, Mode, Options};
use scrutate::textindex::{query_for, trigrams, Q};

fn holds(q: &Q, tris: &[u32]) -> bool {
    match q {
        Q::All => true,
        Q::Tri(t) => tris.binary_search(t).is_ok(),
        Q::And(v) => v.iter().all(|q| holds(q, tris)),
        Q::Or(v) => v.iter().any(|q| holds(q, tris)),
    }
}

fuzz_target!(|data: &[u8]| {
    let Some((&flags, rest)) = data.split_first() else { return };
    let Ok(s) = std::str::from_utf8(rest) else { return };
    let Some((pat, text)) = s.split_once('\u{0}') else { return };
    if pat.len() > 64 {
        return;
    }
    let case = if flags & 1 == 1 { Case::Insensitive } else { Case::Sensitive };
    let mode = match flags >> 1 & 3 {
        0 => Mode::Regex,
        1 => Mode::Literal,
        2 => Mode::Identifier,
        _ => Mode::Fuzzy { max_edits: 1 },
    };
    let o = Options { pattern: pat.to_string(), mode, case, ..Options::default() };
    let Ok(cond) = query_for(&o) else { return };
    let tris = trigrams(text.as_bytes());
    let re = match mode {
        Mode::Regex => regex::RegexBuilder::new(pat).case_insensitive(case == Case::Insensitive).size_limit(1 << 20).build(),
        Mode::Literal => regex::RegexBuilder::new(&regex::escape(pat)).case_insensitive(case == Case::Insensitive).build(),
        Mode::Identifier => regex::Regex::new(&scrutate::instant::identifier_regex(pat)),
        Mode::Fuzzy { .. } => return,
    };
    if let Ok(re) = re {
        for line in text.split('\n') {
            if re.is_match(line) {
                assert!(holds(&cond, &tris), "pattern {pat:?} matches {line:?} but {cond:?} rejects the text");
            }
        }
    }
});
