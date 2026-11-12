//! End-to-end tests of scope-aware `refs`, `deps` and `rdeps` on PHP: namespaces and `use`,
//! typed locals, parameters and properties, `$this`, `self::`, `static::` and `parent::`.

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
        let root = std::env::temp_dir().join(format!("scrutate_resolve_php_{}_{}", name, std::process::id()));
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
        v["result"].clone()
    }
    /// (path, line) of every occurrence -> path of the definition it resolved to, or the reason
    /// it stayed unresolved.
    fn resolved(&self, name: &str) -> BTreeMap<(String, u64), String> {
        let v = self.json(&["refs", name, "--json"]);
        let mut m = BTreeMap::new();
        for d in v["definitions"].as_array().unwrap() {
            for r in d["references"].as_array().unwrap() {
                m.insert((r["path"].as_str().unwrap().to_string(), r["line"].as_u64().unwrap()), d["path"].as_str().unwrap().to_string());
            }
        }
        for u in v["unresolved"].as_array().unwrap() {
            m.insert((u["path"].as_str().unwrap().to_string(), u["line"].as_u64().unwrap()), u["reason"].as_str().unwrap_or("-").to_string());
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

const SHAPE: &str = "<?php\nnamespace App\\Shapes;\n\nabstract class Shape\n{\n    public function describe() { return static::label() . self::label(); }\n    public static function label() { return \"shape\"; }\n    public function area() { return 0; }\n}\n";
const CIRCLE: &str = "<?php\nnamespace App\\Shapes;\n\nclass Circle extends Shape\n{\n    public function area() { return 3; }\n    public function twice() { return $this->area() * 2; }\n}\n";
const SQUARE: &str = "<?php\nnamespace App\\Geo;\n\nuse App\\Shapes\\Shape as Base;\n\nclass Square extends Base\n{\n    public function area()\n    {\n        return parent::area() + 4;\n    }\n}\n";
const APP: &str = "<?php
namespace App;

use App\\Shapes\\{Circle, Shape};
use App\\Geo\\Square;
use function App\\Geo\\area as geoArea;
use Vendor\\Lib\\Thing;

class App
{
    private Square $square;

    public function __construct(private Circle $circle) {}

    public function run(Shape $s, $x)
    {
        $c = new Circle();
        $a = $c->area();
        $b = $this->square->area();
        $d = $this->circle->area();
        $e = $s->area();
        $f = geoArea();
        $g = area();
        $h = (new Square())->area();
        $i = $x->area();
        $j = (new Thing())->area();
        $k = $this->make()->area();
        return $c->describe();
    }

    public function make(): Square { return new Square(); }
}
";

fn shapes(tag: &str) -> Repo {
    Repo::new(
        tag,
        &[
            ("src/Shapes/Shape.php", SHAPE),
            ("src/Shapes/Circle.php", CIRCLE),
            ("src/Geo/Square.php", SQUARE),
            ("src/Geo/helpers.php", "<?php\nnamespace App\\Geo;\n\nfunction area() { return 1; }\n"),
            ("src/global.php", "<?php\nfunction area() { return 2; }\n"),
            ("src/App.php", APP),
        ],
    )
}

#[test]
fn php_method_calls_follow_the_type_of_their_receiver() {
    let r = shapes("calls");
    let (shape, circle, square, helpers, global, app) =
        ("src/Shapes/Shape.php", "src/Shapes/Circle.php", "src/Geo/Square.php", "src/Geo/helpers.php", "src/global.php", "src/App.php");
    assert_eq!(
        r.resolved("area"),
        expect(&[
            (shape, 8, shape),
            (circle, 6, circle),
            // `$this` is the class around
            (circle, 7, circle),
            (square, 8, square),
            // `parent::` is the class it extends, named through a `use` alias
            (square, 10, shape),
            (helpers, 4, helpers),
            (global, 2, global),
            // `use function`
            (app, 6, helpers),
            // a local built with `new`
            (app, 18, circle),
            // a typed property, and a property the constructor declares
            (app, 19, square),
            (app, 20, circle),
            // a typed parameter
            (app, 21, shape),
            // an unqualified call falls back to the global function
            (app, 23, global),
            (app, 24, square),
            (app, 25, "receiver"),
            // a class from outside the repository
            (app, 26, "external"),
            // the return type a method declares
            (app, 27, square),
        ])
    );
    // `self::` and `static::` are the class around; an inherited method is the base's
    assert_eq!(r.resolved("label"), expect(&[(shape, 6, shape), (shape, 7, shape)]));
    assert_eq!(r.resolved("describe"), expect(&[(shape, 6, shape), (app, 28, shape)]));
    let (_, out) = r.run(&["refs", "area"]);
    assert!(!out.contains("not resolved in this language"), "{out}");
}

#[test]
fn php_imports_name_the_files_of_their_classes() {
    let r = shapes("imports");
    let deps: Vec<(u64, Option<String>)> = r
        .json(&["deps", "src/App.php", "--json"])
        .as_array()
        .unwrap()
        .iter()
        .map(|d| (d["line"].as_u64().unwrap(), d["resolved"].as_str().map(String::from)))
        .collect();
    let file = |p: &str| Some(p.to_string());
    assert_eq!(
        deps,
        [(4, file("src/Shapes/Circle.php")), (4, file("src/Shapes/Shape.php")), (5, file("src/Geo/Square.php")), (6, file("src/Geo/helpers.php")), (7, None)]
    );
    let (_, out) = r.run(&["rdeps", "src/Geo/Square.php"]);
    assert!(out.contains("src/App.php:5"), "{out}");
    assert!(!out.contains("could not be resolved"), "{out}");
}

#[test]
fn the_accept_fixture_resolves_a_method_through_a_typed_local() {
    let r = Repo::new(
        "accept",
        &[
            ("app/Models/User.php", "<?php\nnamespace App\\Models;\n\nclass User\n{\n    public function name() { return \"u\"; }\n}\n"),
            ("app/Http/Controller.php", "<?php\nnamespace App\\Http;\n\nuse App\\Models\\User;\n\nclass Controller\n{\n    public function show() { $u = new User(); return $u->name(); }\n}\n"),
        ],
    );
    assert_eq!(r.resolved("name"), expect(&[("app/Http/Controller.php", 8, "app/Models/User.php"), ("app/Models/User.php", 6, "app/Models/User.php")]));
}
