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
            (
                format!("{APP}/Fmt.java"),
                "package com.acme.app;\n\npublic class Fmt {\n    public static String show(int x) {\n        return \"i\";\n    }\n\n    public static String show(String s) {\n        return \"s\";\n    }\n\n    String both() {\n        return show(1);\n    }\n}\n",
            ),
            (format!("{APP}/Main.java"), MAIN),
            ("src/test/java/com/acme/app/MainTest.java".to_string(), MAIN_TEST),
        ];
        Repo::with("acme", &files)
    }

    fn with(tag: &str, files: &[(String, &str)]) -> Repo {
        let root = std::env::temp_dir().join(format!("scrutate_resolve_java_{tag}_{}", std::process::id()));
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
    /// `line reason` of each unresolved occurrence `refs NAME` finds in `file`.
    fn reasons(&self, name: &str, file: &str) -> Vec<String> {
        let v = self.json(&["refs", name, "--json"]);
        let rows = v["unresolved"].as_array().unwrap().iter().filter(|o| o["path"] == file);
        rows.map(|o| format!("{} {}", o["line"], o["reason"].as_str().unwrap())).collect()
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
    // a call through the interface may reach either: it comes last, undecided
    assert_eq!(r.names(&["callers", &format!("{circle}:10"), "--json"]), ["Main.circles", "Circle.summary", "Main.any"]);
    assert_eq!(r.names(&["callers", &format!("{square}:4"), "--json"]), ["Main.squares", "Main.any"]);

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
    // imports name files through the package declarations, wherever the source root is
    let deps = r.json(&["deps", &format!("{APP}/Main.java"), "--json"]);
    let resolved: Vec<&str> = deps.as_array().unwrap().iter().filter_map(|d| d["resolved"].as_str()).collect();
    for want in ["Circle.java", "Shape.java", "Square.java"] {
        assert!(resolved.iter().any(|p| p.ends_with(&format!("{SHAPES}/{want}"))), "{want}: {deps}");
    }
    let (_, importers) = r.run(&["rdeps", &format!("{SHAPES}/Circle.java")]);
    assert!(importers.contains(&format!("{APP}/Main.java")), "{importers}");
    let (_, importers) = r.run(&["rdeps", &format!("{APP}/Util.java")]);
    assert!(importers.contains("MainTest.java"), "a static import names the class file: {importers}");
    // two overloads of one arity: the call stands for both, and each lists it among its callers
    for line in [4, 8] {
        let (_, text) = r.run(&["callers", &format!("{APP}/Fmt.java:{line}")]);
        assert!(text.contains("Fmt.both"), "{line}: {text}");
    }
}

#[test]
fn a_call_through_a_base_type_is_an_undecided_caller_of_an_override() {
    let files: Vec<(String, &str)> = vec![
        ("j/Sized.java".to_string(), "public interface Sized {\n    int size();\n}\n"),
        ("j/Base.java".to_string(), "public class Base implements Sized {\n    public int size() { return 0; }\n}\n"),
        ("j/Mid.java".to_string(), "public class Mid extends Base {\n}\n"),
        ("j/Big.java".to_string(), "public class Big extends Mid {\n    @Override\n    public int size() { return 9; }\n}\n"),
        ("j/Small.java".to_string(), "public class Small extends Base {\n    public int size() { return 1; }\n}\n"),
        (
            "j/Use.java".to_string(),
            "public class Use {\n    int viaSized(Sized s) { return s.size(); }\n    int viaBase(Base b) { return b.size(); }\n    int viaMid(Mid m) { return m.size(); }\n    int viaSub(Big b) { return b.size(); }\n    int viaNew() { Base b = new Big(); return b.size(); }\n    int viaSmall(Small s) { return s.size(); }\n    int viaAgain() { Base b = new Big(); b = new Small(); return b.size(); }\n    int viaMade() { Base b = make(); return b.size(); }\n    Base make() { return new Big(); }\n}\n",
        ),
    ];
    let r = Repo::with("override", &files);
    let split = |spec: &str| {
        let (_, text) = r.run(&["callers", spec]);
        let names: Vec<String> = text.lines().filter_map(|l| l.split("] ").nth(1)).map(str::to_string).collect();
        let n = text.split_once(" of these call").and_then(|(a, _)| a.rsplit('(').next()?.parse().ok()).unwrap_or(0);
        let (mut d, mut u) = (names[..names.len() - n].to_vec(), names[names.len() - n..].to_vec());
        d.sort();
        u.sort();
        (d, u)
    };
    let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let unsure = strs(&["Use.viaAgain", "Use.viaBase", "Use.viaMade", "Use.viaMid", "Use.viaSized"]);
    assert_eq!(split("j/Big.java:3"), (strs(&["Use.viaNew", "Use.viaSub"]), unsure.clone()));
    assert_eq!(split("j/Small.java:2"), (strs(&["Use.viaSmall"]), unsure));
    assert_eq!(split("j/Base.java:2"), (strs(&["Use.viaAgain", "Use.viaBase", "Use.viaMade", "Use.viaMid"]), strs(&["Use.viaSized"])));
    assert_eq!(r.names(&["callees", "Use.viaBase", "--json"]), ["Base.size"]);
    // `new Big()` also calls the class
    assert_eq!(r.names(&["callees", "Use.viaNew", "--json"]), ["Big", "Big.size"]);
    assert_eq!(r.refs_of("size", "j/Big.java:3"), ["Big.java:3", "Use.java:5", "Use.java:6"]);
    assert_eq!(r.refs_of("size", "j/Base.java:2"), ["Base.java:2", "Use.java:3", "Use.java:4", "Use.java:8", "Use.java:9"]);
}

#[test]
fn a_super_call_is_a_decided_call_of_the_base_method_and_no_call_of_the_override() {
    let files: Vec<(String, &str)> = vec![
        ("j/Base.java".to_string(), "public class Base {\n    public int size() { return 0; }\n}\n"),
        (
            "j/Big.java".to_string(),
            "public class Big extends Base {\n    @Override\n    public int size() { return super.size() + 9; }\n    int other() { return this.size(); }\n}\n",
        ),
        ("j/Bigger.java".to_string(), "public class Bigger extends Big {\n    public int size() { return super.size() + 1; }\n}\n"),
        ("j/Use.java".to_string(), "public class Use {\n    int viaBase(Base b) { return b.size(); }\n}\n"),
        (
            "j/Far.java".to_string(),
            "import java.util.ArrayList;\n\npublic class Far extends ArrayList<String> {\n    public int size() { return super.size() + 3; }\n}\n",
        ),
    ];
    let r = Repo::with("super", &files);
    let split = |spec: &str| {
        let (_, text) = r.run(&["callers", spec]);
        let names: Vec<String> = text.lines().filter_map(|l| l.split("] ").nth(1)).map(str::to_string).collect();
        let n = if text.contains("could not tell whether any") {
            names.len()
        } else {
            text.split_once(" of these call").and_then(|(a, _)| a.rsplit('(').next()?.parse().ok()).unwrap_or(0)
        };
        let (mut d, mut u) = (names[..names.len() - n].to_vec(), names[names.len() - n..].to_vec());
        d.sort();
        u.sort();
        (d, u)
    };
    let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    // the override is no caller of itself; the base lists it as decided
    assert_eq!(split("j/Big.java:3"), (strs(&["Big.other", "Bigger.size"]), strs(&["Use.viaBase"])));
    assert_eq!(split("j/Base.java:2"), (strs(&["Big.size", "Use.viaBase"]), strs(&[])));
    assert_eq!(split("j/Bigger.java:2"), (strs(&[]), strs(&["Big.other", "Use.viaBase"])));
    // a base from outside the repository: the super call is no call of the repository
    assert_eq!(split("j/Far.java:4"), (strs(&[]), strs(&[])));
    assert_eq!(r.names(&["callees", "Big.size", "--json"]), ["Base.size"]);
    assert_eq!(r.names(&["callees", "Bigger.size", "--json"]), ["Big.size"]);
    assert_eq!(r.refs_of("size", "j/Base.java:2"), ["Base.java:2", "Big.java:3", "Use.java:2"]);
    assert_eq!(r.refs_of("size", "j/Big.java:3"), ["Big.java:3", "Big.java:4", "Bigger.java:2"]);
}

#[test]
fn a_class_used_as_a_call_receiver_is_a_use_of_the_class_and_a_variable_receiver_is_a_local() {
    let main = "package p;

import java.util.List;
import q.Lib;

public class Main {
    private List<String> items;

    int foo() {
        return 0;
    }

    int run(List<String> names) {
        List<String> list = names;
        list.add(\"a\");
        names.add(\"b\");
        items.add(\"c\");
        return Util.foo() + Lib.lib() + Lib.Inner.deep() + Box.open() + Util.make().bar() + Util.<String>id(\"x\").length();
    }

    int viaLocal() {
        Util foo = null;
        return foo.foo();
    }

    static class Box {
        static int open() {
            return 5;
        }
    }
}
";
    let files: Vec<(String, &str)> = vec![
        (
            "p/Util.java".to_string(),
            "package p;\n\npublic class Util {\n    public static int foo() {\n        return 1;\n    }\n\n    public static Util make() {\n        return new Util();\n    }\n\n    public int bar() {\n        return 2;\n    }\n\n    public static <T> T id(T x) {\n        return x;\n    }\n}\n",
        ),
        (
            "q/Lib.java".to_string(),
            "package q;\n\npublic class Lib {\n    public static int lib() {\n        return 3;\n    }\n\n    public static class Inner {\n        public static int deep() {\n            return 4;\n        }\n    }\n}\n",
        ),
        ("p/Main.java".to_string(), main),
    ];
    let r = Repo::with("receiver", &files);
    // a class of the same package, an imported one, a nested one, in front of a chain or a generic call
    assert_eq!(
        r.refs_of("Util", "p/Util.java:3"),
        ["Main.java:18", "Main.java:18", "Main.java:18", "Main.java:22", "Util.java:3", "Util.java:8", "Util.java:9"]
    );
    assert_eq!(r.refs_of("Lib", "q/Lib.java:3"), ["Lib.java:3", "Main.java:18", "Main.java:18", "Main.java:4"]);
    assert_eq!(r.refs_of("Box", "p/Main.java:26"), ["Main.java:18", "Main.java:26"]);
    assert_eq!(r.reasons("Util", "p/Main.java"), Vec::<String>::new());
    // a local, a parameter or a field in front of a call is that variable
    assert_eq!(r.reasons("list", "p/Main.java"), ["14 local variable", "15 local variable"]);
    assert_eq!(r.reasons("names", "p/Main.java"), ["13 local variable", "14 local variable", "16 local variable"]);
    assert_eq!(r.reasons("items", "p/Main.java"), ["7 field or key", "17 field or key"]);
    // a variable named as a method of the class around is no call of that method
    assert_eq!(r.names(&["callers", "p/Util.java:4", "--json"]), ["Main.run", "Main.viaLocal"]);
    let (_, text) = r.run(&["callers", "p/Util.java:4"]);
    assert!(!text.contains("could not"), "{text}");
    assert_eq!(r.run(&["callers", "p/Main.java:9", "--json"]), (1, "[]\n".to_string()));
}
