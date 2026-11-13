//! The repo map: the files of the repository ranked by PageRank over the edges the index's names
//! give between files (`Names::file_edges`: calls and imports), each with its top definitions.
//! The same index gives the same bytes: the iteration count is fixed, and a tie goes to the file
//! with more edges in and out, then by path.

use std::cmp::Reverse;
use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::Result;

use crate::index::Symbol;
use crate::names::Names;
use crate::search::Searcher;

pub const DAMPING: f64 = 0.85;
pub const ITERATIONS: usize = 50;
/// Definitions one file of the map lists.
pub const DEFINITIONS: usize = 5;

/// One file of the map: its PageRank score and its top definitions, each with its share of the
/// calls of its name.
#[derive(Clone, Debug)]
pub struct MapFile {
    pub path: String,
    pub score: f64,
    pub definitions: Vec<(Symbol, i64)>,
}

/// The files under `under` (a directory or file from the root; every file when None) that define
/// code, highest score first. A file's definitions come most called first (the calls of the
/// name split among the files that define it), then outer before inner, longer before shorter,
/// and by line; a name defined several times in the file (overloads) is listed once.
pub fn map(s: &Searcher, under: Option<&str>) -> Result<Vec<MapFile>> {
    let mut files = s.ix.files()?;
    files.sort();
    let edges = Names::new(s)?.file_edges(&files)?;
    let scores = pagerank(files.len(), &edges);
    // edges in and out: of two files nobody points at, the one that points somewhere goes first
    let mut linked = vec![0.0; files.len()];
    for &(a, b, w) in &edges {
        linked[a] += w;
        linked[b] += w;
    }
    let under = under.map(|p| p.trim_start_matches("./").trim_end_matches('/')).filter(|p| !p.is_empty() && *p != ".");
    let kinds: Vec<&str> = crate::search::CODE_KINDS.iter().copied().chain(["macro"]).collect();
    let mut definers: HashMap<String, f64> = HashMap::new();
    for (name, _) in s.ix.definers(&kinds)? {
        *definers.entry(name).or_default() += 1.0;
    }
    let mut listed = Vec::new();
    for ((path, score), linked) in files.into_iter().zip(scores).zip(linked) {
        if under.is_some_and(|u| path != u && !path.starts_with(&format!("{u}/"))) {
            continue;
        }
        let defs: Vec<Symbol> = s.ix.symbols_in(&path)?.into_iter().filter(|d| kinds.contains(&d.kind.as_str())).collect();
        if !defs.is_empty() {
            listed.push((path, score, linked, defs));
        }
    }
    let names: BTreeSet<&String> = listed.iter().flat_map(|l| l.3.iter().map(|d| &d.name)).collect();
    let calls = s.ix.call_counts(&names.into_iter().collect::<Vec<_>>())?;
    let mut out = Vec::new();
    for (path, score, linked, defs) in listed {
        let share = |d: &Symbol| calls.get(&d.name).map_or(0.0, |&c| c as f64 / definers.get(&d.name).copied().unwrap_or(1.0));
        let mut definitions: Vec<(f64, Symbol)> = defs.into_iter().map(|d| (share(&d), d)).collect();
        let place = |d: &Symbol| (d.qualname.matches('.').count(), Reverse(d.end - d.start), d.start);
        definitions.sort_by(|(a, x), (b, y)| b.total_cmp(a).then_with(|| place(x).cmp(&place(y))));
        let mut seen = HashSet::new();
        definitions.retain(|(_, d)| seen.insert(d.qualname.clone()));
        let definitions = definitions.into_iter().take(DEFINITIONS).map(|(c, d)| (d, c.round() as i64)).collect();
        out.push((MapFile { path, score, definitions }, linked));
    }
    // scores closer than rounding are a tie
    let near = |x: f64| Reverse((x * 1e9).round() as i64);
    out.sort_by_cached_key(|(f, linked)| (near(f.score), near(*linked), f.path.clone()));
    Ok(out.into_iter().map(|(f, _)| f).collect())
}

/// PageRank of `n` nodes over weighted `edges` (from, to, weight): a node without edges out
/// shares its score among all.
fn pagerank(n: usize, edges: &[(usize, usize, f64)]) -> Vec<f64> {
    if n == 0 {
        return Vec::new();
    }
    let mut out_weight = vec![0.0; n];
    for &(a, _, w) in edges {
        out_weight[a] += w;
    }
    let mut score = vec![1.0 / n as f64; n];
    for _ in 0..ITERATIONS {
        let dangling: f64 = (0..n).filter(|&i| out_weight[i] == 0.0).map(|i| score[i]).sum();
        let mut next = vec![(1.0 - DAMPING) / n as f64 + DAMPING * dangling / n as f64; n];
        for &(a, b, w) in edges {
            next[b] += DAMPING * score[a] * w / out_weight[a];
        }
        score = next;
    }
    score
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_node_others_point_at_ranks_above_one_nobody_does() {
        // 1..=3 point at 0; 4 stands alone
        let s = pagerank(5, &[(1, 0, 1.0), (2, 0, 1.0), (3, 0, 2.0)]);
        assert!(s[0] > s[1] && s[1] == s[4], "{s:?}");
        assert!((s.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{s:?}");
    }
}
