//! Plain-text rendering of hits (same layout as the Python reference, so tools and people see the
//! same thing from either engine).

use crate::search::Hit;

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
        let mut head = format!("{}  [{}] {}", h.loc(), h.kind, name);
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
