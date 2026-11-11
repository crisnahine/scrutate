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

/// `languages --json` must name every kind the index gives a language's sample.
#[test]
fn the_registry_names_every_kind_a_sample_yields() {
    let mut missing = Vec::new();
    for sub in ["tests/samples", "tests/samples_extra"] {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(sub);
        for entry in fs::read_dir(&dir).unwrap() {
            let p = entry.unwrap().path();
            let name = p.file_name().unwrap().to_string_lossy().into_owned();
            let Ok(src) = fs::read_to_string(&p) else { continue };
            let Some(spec) = scrutate::parse::spec_for(&name, &src) else { continue };
            let known = scrutate::langs::definition_kinds(spec);
            for c in parse_source(&name, &src) {
                if !matches!(c.kind, "module" | "window") && !known.contains(&c.kind) {
                    missing.push(format!("{}: {} ({name})", spec.id, c.kind));
                }
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(missing.is_empty(), "kinds the registry does not name:\n{}", missing.join("\n"));
}

#[test]
fn a_racket_define_syntax_rule_is_a_macro() {
    let chunks = parse_source("m.rkt", "#lang racket\n(define-syntax-rule (twice e) (begin e e))\n(define (f x) (twice x))\n");
    let found: Vec<(&str, &str)> = chunks.iter().map(|c| (c.kind, c.name.as_str())).collect();
    assert!(found.contains(&("macro", "twice")) && found.contains(&("function", "f")), "{found:?}");
}

#[test]
fn every_text_file_is_searchable_even_without_symbols() {
    let chunks = parse_source("notes.whatever", "line one\nline two\n");
    assert!(!chunks.is_empty());
    let windows = parse_source("data.unknown", &"x\n".repeat(200));
    assert!(windows.iter().all(|c| c.kind == "window" || c.kind == "module"));
}
