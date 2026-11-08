//! Every language sample must yield exactly the symbols recorded in tests/samples/expected.json.
//! Run with UPDATE_EXPECTED=1 to re-record after an intentional rule change (review the diff!).

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use scrutate::parse::parse_source;

#[test]
fn samples_yield_the_recorded_symbols() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples");
    let expected_path = dir.join("expected.json");
    let mut got: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name == "expected.json" {
            continue;
        }
        let src = fs::read_to_string(&p).unwrap();
        let chunks = parse_source(&name, &src);
        got.insert(name, chunks.iter().map(|c| (c.kind.to_string(), c.qualname.clone())).collect());
    }
    if std::env::var("UPDATE_EXPECTED").is_ok() {
        fs::write(&expected_path, serde_json::to_string_pretty(&got).unwrap()).unwrap();
        return;
    }
    let expected: BTreeMap<String, Vec<(String, String)>> =
        serde_json::from_str(&fs::read_to_string(&expected_path).expect("expected.json (run with UPDATE_EXPECTED=1 once)")).unwrap();
    let mut failures = Vec::new();
    for (name, chunks) in &got {
        match expected.get(name) {
            Some(e) if e == chunks => {}
            Some(e) => failures.push(format!("{name}: expected {e:?}\n   got {chunks:?}")),
            None => failures.push(format!("{name}: no expectation recorded")),
        }
    }
    for name in expected.keys() {
        if !got.contains_key(name) {
            failures.push(format!("{name}: sample missing"));
        }
    }
    assert!(failures.is_empty(), "symbol regressions:\n{}", failures.join("\n"));
    assert!(got.len() >= 60, "fewer samples than expected: {}", got.len());
}

#[test]
fn every_text_file_is_searchable_even_without_symbols() {
    let chunks = parse_source("notes.whatever", "line one\nline two\n");
    assert!(!chunks.is_empty());
    let windows = parse_source("data.unknown", &"x\n".repeat(200));
    assert!(windows.iter().all(|c| c.kind == "window" || c.kind == "module"));
}
