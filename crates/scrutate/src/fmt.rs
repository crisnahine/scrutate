//! Plain-text rendering of hits (same layout as the Python reference, so tools and people see the
//! same thing from either engine).

use std::borrow::Cow;

use crate::search::Hit;

/// Text as one line a terminal shows as it is: control characters written as escapes (`\n`,
/// `\x1b`), so a file name cannot start a line of its own or move the cursor.
pub fn shown(s: &str) -> Cow<'_, str> {
    if !s.chars().any(char::is_control) {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() && (c as u32) < 0x100 => out.push_str(&format!("\\x{:02x}", c as u32)),
            c if c.is_control() => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

/// Characters of an argument an error message repeats.
const ECHO_CHARS: usize = 200;

/// An argument as an error message repeats it: `shown`, and cut to its first 200 characters.
pub fn echo(s: &str) -> String {
    let n = s.chars().count();
    if n <= ECHO_CHARS {
        return shown(s).into_owned();
    }
    let head: String = s.chars().take(ECHO_CHARS).collect();
    format!("{}... (cut: {n} characters in all)", shown(&head))
}

/// `path:start-end` of a hit, as text.
pub fn loc(h: &Hit) -> String {
    format!("{}:{}-{}", shown(&h.path), h.start, h.end)
}

pub fn fmt_hits(hits: &[Hit], words: &[String], snippet: bool, why: bool) -> String {
    if hits.is_empty() {
        return "(no results)".to_string();
    }
    hit_blocks(hits, words, snippet, why).join("\n")
}

/// One block of text per hit: its location line and, with `snippet`, the lines that matter.
pub fn hit_blocks(hits: &[Hit], words: &[String], snippet: bool, why: bool) -> Vec<String> {
    let mut out = Vec::with_capacity(hits.len());
    for h in hits {
        let name = if h.qualname.is_empty() { &h.name } else { &h.qualname };
        let mut head = format!("{}  [{}] {}", loc(h), h.kind, name);
        if why && !h.why.is_empty() {
            head.push_str(&format!("  ({})", h.why.iter().map(|(k, _)| k.as_str()).collect::<Vec<_>>().join(", ")));
        }
        if snippet {
            let body = h.snippet(words, 8);
            head.push('\n');
            head.push_str(&body.split('\n').map(|l| format!("    {l}")).collect::<Vec<_>>().join("\n"));
        }
        out.push(head);
    }
    out
}

pub fn hits_json(hits: &[Hit]) -> serde_json::Value {
    serde_json::Value::Array(
        hits.iter()
            .map(|h| {
                let why: serde_json::Map<String, serde_json::Value> = h.why.iter().cloned().collect();
                serde_json::json!({
                    "id": h.loc(), "path": h.path, "start": h.start, "end": h.end,
                    "name": if h.qualname.is_empty() { &h.name } else { &h.qualname },
                    "kind": h.kind, "score": h.score, "why": why,
                })
            })
            .collect(),
    )
}
