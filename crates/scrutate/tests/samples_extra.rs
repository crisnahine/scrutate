//! The optional grammars (`--features extra-langs`): each sample yields exactly the symbols in
//! tests/samples_extra/expected.json. UPDATE_EXPECTED=1 re-records (review the diff).
#![cfg(feature = "extra-langs")]

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use scrutate::parse::parse_source;

#[test]
fn extra_language_samples_yield_the_recorded_symbols() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/samples_extra");
    let expected_path = dir.join("expected.json");
    let mut got: BTreeMap<String, Vec<(String, String)>> = BTreeMap::new();
    for entry in fs::read_dir(&dir).unwrap() {
        let p = entry.unwrap().path();
        let name = p.file_name().unwrap().to_string_lossy().into_owned();
        if name == "expected.json" {
            continue;
        }
        let src = fs::read_to_string(&p).unwrap();
        got.insert(name, parse_source(&src_name(&p), &src).iter().map(|c| (c.kind.to_string(), c.qualname.clone())).collect());
    }
    if std::env::var("UPDATE_EXPECTED").is_ok() {
        fs::write(&expected_path, serde_json::to_string_pretty(&got).unwrap()).unwrap();
        return;
    }
    let expected: BTreeMap<String, Vec<(String, String)>> = serde_json::from_str(&fs::read_to_string(&expected_path).unwrap()).unwrap();
    assert_eq!(got, expected);
    assert_eq!(got.len(), 13);
}

fn src_name(p: &Path) -> String {
    p.file_name().unwrap().to_string_lossy().into_owned()
}
