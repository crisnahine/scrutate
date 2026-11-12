//! End-to-end tests of scope-aware `refs`: per language, a small repository with two same-named
//! definitions in different modules, and every occurrence checked against the one it must land on.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(name: &str, files: &[(&str, &str)]) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_resolve_e2e_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        for (rel, body) in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
        }
        let r = Repo { root };
        assert_eq!(r.run(&["index", "--no-text"]).0, 0);
        r
    }
    fn run(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(BIN).args(args).current_dir(&self.root).stdin(Stdio::null()).output().unwrap();
        (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned())
    }
    fn json(&self, args: &[&str]) -> serde_json::Value {
        let (code, out) = self.run(args);
        assert_eq!(code, 0, "{out}");
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"));
        // an answer leaves in its envelope; `check` prints a report of its own
        if v["schema"] == 2 {
            v["result"].clone()
        } else {
            v
        }
    }
    /// (path, line) of every occurrence -> path of the definition it resolved to ("-" = unresolved).
    fn resolved(&self, name: &str) -> BTreeMap<(String, u64), String> {
        let v = self.json(&["refs", name, "--json"]);
        let mut m = BTreeMap::new();
        for d in v["definitions"].as_array().unwrap() {
            for r in d["references"].as_array().unwrap() {
                m.insert((r["path"].as_str().unwrap().to_string(), r["line"].as_u64().unwrap()), d["path"].as_str().unwrap().to_string());
            }
        }
        for u in v["unresolved"].as_array().unwrap() {
            m.insert((u["path"].as_str().unwrap().to_string(), u["line"].as_u64().unwrap()), "-".to_string());
        }
        m
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn expect(want: &[(&str, u64, &str)]) -> BTreeMap<(String, u64), String> {
    want.iter().map(|(p, l, d)| ((p.to_string(), *l), d.to_string())).collect()
}

#[test]
fn python_two_areas() {
    let r = Repo::new(
        "py",
        &[
            ("shapes/circle.py", "def area(r):\n    return 3 * r * r\n"),
            ("shapes/square.py", "def area(s):\n    return s * s\n"),
            ("shapes/__init__.py", "from .circle import area\n"),
            (
                "app.py",
                "from shapes import square\nfrom shapes import area\nimport shapes.circle as c\n\n\ndef main(area_of=None):\n    a = area(1)\n    b = square.area(2)\n    d = c.area(3)\n    return a + b + d\n\n\n# area in a comment\n",
            ),
        ],
    );
    let (c, s) = ("shapes/circle.py", "shapes/square.py");
    assert_eq!(
        r.resolved("area"),
        expect(&[
            (c, 1, c),
            (s, 1, s),
            ("shapes/__init__.py", 1, c),
            ("app.py", 2, c),
            ("app.py", 7, c),
            ("app.py", 8, s),
            ("app.py", 9, c),
            ("app.py", 13, "-")
        ])
    );

    let v = r.json(&["refs", "area", "--def", "shapes/square.py", "--json"]);
    let defs = v["definitions"].as_array().unwrap();
    assert_eq!(defs.len(), 1, "{v}");
    let lines: Vec<u64> = defs[0]["references"].as_array().unwrap().iter().map(|o| o["line"].as_u64().unwrap()).collect();
    assert_eq!(lines, [8, 1], "app.py:8 and the definition itself: {v}");
    let (_, text) = r.run(&["refs", "area", "--def", "shapes/square.py:1"]);
    assert!(text.starts_with("shapes/square.py:1-2  [function] area  (2 references)"), "{text}");
    assert_eq!(r.run(&["refs", "area", "--def", "app.py"]).0, 2, "no definition there is an error");

    let v = r.json(&["refs", "area", "--textual", "--json"]);
    assert!(v["callers"].is_array() && v["references"].is_array(), "the old shape stays reachable: {v}");
}

#[test]
fn javascript_and_typescript_two_areas() {
    let r = Repo::new(
        "js",
        &[
            ("lib/a.js", "export function area() { return 1; }\n"),
            ("lib/b.js", "export function area() { return 2; }\n"),
            ("main.js", "import {area} from './lib/a.js';\nimport * as b from './lib/b';\narea();\nb.area();\n"),
            ("geo/point.ts", "export class Shape {\n  area() { return 1; }\n  twice() { return this.area() * 2; }\n}\n"),
            ("geo/box.ts", "export function area() { return 2; }\n"),
            ("geo/index.ts", "export { area } from './box';\n"),
            ("use.ts", "import { Shape } from './geo/point';\nimport { area } from './geo';\nconst s = new Shape();\narea();\ns.area();\n"),
        ],
    );
    let (a, b, p, x) = ("lib/a.js", "lib/b.js", "geo/point.ts", "geo/box.ts");
    assert_eq!(
        r.resolved("area"),
        expect(&[
            (a, 1, a),
            (b, 1, b),
            ("main.js", 1, a),
            ("main.js", 3, a),
            ("main.js", 4, b),
            (p, 2, p),
            (p, 3, p),
            (x, 1, x),
            ("geo/index.ts", 1, x),
            ("use.ts", 2, x),
            ("use.ts", 4, x),
            ("use.ts", 5, p), // `s` is built with `new Shape()`
        ])
    );
}

#[test]
fn rust_two_areas() {
    let r = Repo::new(
        "rs",
        &[
            ("Cargo.toml", "[package]\nname = \"shapes\"\nversion = \"0.1.0\"\n"),
            ("src/lib.rs", "pub mod app;\npub mod circle;\npub mod square;\npub use circle::area;\n"),
            ("src/circle.rs", "pub fn area() -> u32 {\n    1\n}\n"),
            ("src/square.rs", "pub fn area() -> u32 {\n    2\n}\n\npub fn twice() -> u32 {\n    area() * 2\n}\n"),
            (
                "src/app.rs",
                "use crate::area;\nuse crate::square;\n\npub fn run() -> u32 {\n    area()\n        + square::area()\n        + crate::circle::area()\n}\n",
            ),
        ],
    );
    let (c, s) = ("src/circle.rs", "src/square.rs");
    assert_eq!(
        r.resolved("area"),
        expect(&[
            (c, 1, c),
            (s, 1, s),
            (s, 6, s),
            ("src/lib.rs", 4, c),
            ("src/app.rs", 1, c),
            ("src/app.rs", 5, c),
            ("src/app.rs", 6, s),
            ("src/app.rs", 7, c)
        ])
    );
}

#[test]
fn go_two_areas() {
    let r = Repo::new(
        "go",
        &[
            ("go.mod", "module example.com/shapes\n\ngo 1.22\n"),
            ("circle/area.go", "package circle\n\nfunc Area() int { return 1 }\n"),
            ("square/area.go", "package square\n\nfunc Area() int { return 2 }\n"),
            ("square/twice.go", "package square\n\nfunc Twice() int { return Area() * 2 }\n"),
            ("main.go", "package main\n\nimport (\n\t\"example.com/shapes/circle\"\n\tsq \"example.com/shapes/square\"\n)\n\nfunc main() {\n\tprintln(circle.Area())\n\tprintln(sq.Area())\n}\n"),
        ],
    );
    let (c, s) = ("circle/area.go", "square/area.go");
    assert_eq!(r.resolved("Area"), expect(&[(c, 3, c), (s, 3, s), ("square/twice.go", 3, s), ("main.go", 9, c), ("main.go", 10, s)]));
}

#[test]
fn javascript_var_belongs_to_the_function_and_let_to_the_block() {
    let use_js = "import { helper } from './lib';\nfunction viaVar(flag) {\n  if (flag) {\n    var helper = other();\n  }\n  return helper();\n}\nfunction viaLet(flag) {\n  if (flag) {\n    let helper = other();\n  }\n  return helper();\n}\nfunction viaConst(flag) {\n  {\n    const helper = 3;\n  }\n  for (var i = 0; i < 1; i++) {\n    var other = helper();\n  }\n  return helper();\n}\n";
    let r = Repo::new("js_var", &[("lib.js", "export function helper() { return 1; }\n"), ("use.js", use_js)]);
    let h = "lib.js";
    assert_eq!(
        r.resolved("helper"),
        expect(&[
            (h, 1, h),
            ("use.js", 1, h),
            ("use.js", 4, "-"),
            ("use.js", 6, "-"),
            ("use.js", 10, "-"),
            ("use.js", 12, h),
            ("use.js", 16, "-"),
            ("use.js", 19, h),
            ("use.js", 21, h)
        ])
    );
}
