//! What the training scripts read from the binary: `batch --features` (the ranker's rows).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

fn tree(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("scrutate_training_{}_{}", name, std::process::id()));
    let _ = fs::remove_dir_all(&root);
    for (rel, text) in files {
        let p = root.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, text).unwrap();
    }
    root
}

fn run(root: &PathBuf, args: &[&str]) -> Vec<serde_json::Value> {
    let out = Command::new(BIN).arg("--root").arg(root).args(args).stdin(Stdio::null()).env("SCRUTATE_SEMANTIC", "none").output().unwrap();
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8(out.stdout).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

const SHOP: &str = "class Order:\n    def total(self):\n        \"\"\"Sum of the lines.\"\"\"\n        return 1\n\n\ndef total_price():\n    return 2\n";

#[test]
fn batch_features_name_every_feature_once_per_query() {
    let root = tree("features", &[("shop.py", SHOP)]);
    let q = root.join("q.jsonl");
    fs::write(&q, "{\"query\": \"total\", \"k\": 3}\n").unwrap();
    let rec = &run(&root, &["batch", "--features", q.to_str().unwrap()])[0];
    let names: Vec<&str> = rec["feature_names"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(names, scrutate::search::FEATURES);
    let cands = rec["candidates"].as_array().unwrap();
    let total = cands.iter().find(|c| c["qualname"] == "Order.total").expect("Order.total is a candidate");
    assert_eq!((total["name"].as_str(), total["kind"].as_str()), (Some("total"), Some("method")));
    let x = total["x"].as_array().unwrap();
    assert_eq!(x.len(), names.len());
    let at = |n: &str| x[names.iter().position(|f| *f == n).unwrap()].as_f64().unwrap();
    assert_eq!((at("exact_name"), at("k_method"), at("has_doc")), (1.0, 1.0, 1.0));
    for key in ["exact_ids", "lex_ids", "sem_ids"] {
        assert!(rec[key].is_array(), "{key}: {rec}");
    }
    let _ = fs::remove_dir_all(&root);
}
