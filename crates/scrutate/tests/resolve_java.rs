//! End-to-end tests of the Java resolver through `refs`, `callers` and `callees`, on a small
//! Maven-shaped repository: two packages, an interface with two implementations, a static import,
//! an overload and a chain of calls typed by declared return types.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

const SHAPES: &str = "src/main/java/com/acme/shapes";
const APP: &str = "src/main/java/com/acme/app";

const MAIN: &str = "package com.acme.app;

import com.acme.shapes.Circle;
import com.acme.shapes.Shape;
import com.acme.shapes.Square;
import java.util.List;

public class Main {
    double circles() {
        return new Circle(1).area();
    }

    double squares() {
        Square q = new Square();
        return q.area();
    }

    double any(Shape s) {
        return s.area();
    }

    boolean chain() {
        return Circle.unit().summary().ok();
    }

    int sizes(List<String> names) {
        return names.size() + Util.scale(names.size(), 2);
    }
}
";

const MAIN_TEST: &str = "package com.acme.app;

import static com.acme.app.Util.scale;

import java.util.function.IntUnaryOperator;

public class MainTest {
    int one() {
        return scale(1);
    }

    int two() {
        return scale(1, 2);
    }

    int same() {
        return Util.scale(3);
    }

    IntUnaryOperator ref() {
        return Util::scale;
    }
}
";

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new() -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_resolve_java_{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let files = [
            ("pom.xml".to_string(), "<project><artifactId>acme</artifactId></project>\n"),
            (format!("{SHAPES}/Shape.java"), "package com.acme.shapes;\n\npublic interface Shape {\n    double area();\n}\n"),
            (
                format!("{SHAPES}/Circle.java"),
                "package com.acme.shapes;\n\npublic class Circle implements Shape {\n    private final double r;\n\n    public Circle(double r) {\n        this.r = r;\n    }\n\n    public double area() {\n        return 3 * r * r;\n    }\n\n    public static Circle unit() {\n        return new Circle(1);\n    }\n\n    public Report summary() {\n        return new Report(area());\n    }\n}\n",
            ),
            (format!("{SHAPES}/Square.java"), "package com.acme.shapes;\n\npublic class Square implements Shape {\n    public double area() {\n        return 4;\n    }\n}\n"),
            (
                format!("{SHAPES}/Report.java"),
                "package com.acme.shapes;\n\npublic class Report {\n    private final double value;\n\n    public Report(double value) {\n        this.value = value;\n    }\n\n    public boolean ok() {\n        return value > 0;\n    }\n}\n",
            ),
            (
                format!("{APP}/Util.java"),
                "package com.acme.app;\n\npublic class Util {\n    public static int scale(int x) {\n        return x * 2;\n    }\n\n    public static int scale(int x, int y) {\n        return x * y;\n    }\n}\n",
            ),
            (format!("{APP}/Main.java"), MAIN),
            ("src/test/java/com/acme/app/MainTest.java".to_string(), MAIN_TEST),
        ];
        for (rel, body) in &files {
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
        serde_json::from_str(&out).unwrap_or_else(|e| panic!("{e}: {out}"))
    }
    /// The relative `path:start` of each item of a `callers` or `callees` answer.
    fn names(&self, args: &[&str]) -> Vec<String> {
        let v = self.json(args);
        let items = v.as_array().or_else(|| v["resolved"].as_array()).unwrap_or_else(|| panic!("{v}"));
        items.iter().map(|h| h["name"].as_str().unwrap_or("").to_string()).collect()
    }
    /// `file:line` of the occurrences `refs NAME --def DEF` ties to that definition.
    fn refs_of(&self, name: &str, def: &str) -> Vec<String> {
        let v = self.json(&["refs", name, "--def", def, "--json"]);
        let defs = v["definitions"].as_array().unwrap();
        assert_eq!(defs.len(), 1, "{v}");
        let mut out: Vec<String> = defs[0]["references"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| format!("{}:{}", o["path"].as_str().unwrap().rsplit('/').next().unwrap(), o["line"]))
            .collect();
        out.sort();
        out
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn java_calls_resolve_through_packages_types_imports_and_overloads() {
    let r = Repo::new();
    let circle = format!("{SHAPES}/Circle.java");
    let (square, shape, util) = (format!("{SHAPES}/Square.java"), format!("{SHAPES}/Shape.java"), format!("{APP}/Util.java"));

    // an interface with two implementations: each call lands on the type its receiver is declared with
    assert_eq!(r.refs_of("area", &circle), ["Circle.java:10", "Circle.java:19", "Main.java:10"]);
    assert_eq!(r.refs_of("area", &square), ["Main.java:15", "Square.java:4"]);
    assert_eq!(r.refs_of("area", &shape), ["Main.java:19", "Shape.java:4"]);
    assert_eq!(r.names(&["callers", &format!("{circle}:10"), "--json"]), ["Main.circles", "Circle.summary"]);
    assert_eq!(r.names(&["callers", &format!("{square}:4"), "--json"]), ["Main.squares"]);

    // a chain typed by declared return types; `names.size()` is a call of `java.util`
    assert_eq!(r.names(&["callees", "Main.chain", "--json"]), ["Report.ok", "Circle.summary", "Circle.unit"]);
    let v = r.json(&["callees", "Main.sizes", "--json"]);
    assert_eq!(v["external"], serde_json::json!(["size"]), "{v}");

    // overloads: the number of arguments picks one; a method reference stands for both
    assert_eq!(r.refs_of("scale", &format!("{util}:4")), ["MainTest.java:17", "MainTest.java:21", "MainTest.java:3", "MainTest.java:9", "Util.java:4"]);
    assert_eq!(r.refs_of("scale", &format!("{util}:8")), ["Main.java:27", "MainTest.java:13", "MainTest.java:21", "MainTest.java:3", "Util.java:8"]);
    let v = r.json(&["refs", "scale", "--json"]);
    assert_eq!(v["unresolved"], serde_json::json!([]), "{v}");
    let (_, text) = r.run(&["callers", "scale"]);
    assert!(!text.contains("could not"), "every call of scale is decided: {text}");
}
