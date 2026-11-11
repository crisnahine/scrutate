//! `scrutate grep` and the command line as a user meets them: environment switches, odd
//! arguments, per-file limits, context lines, globs over several roots and the paths printed.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

struct Dir(PathBuf);

impl Dir {
    fn new(name: &str) -> Dir {
        let root = std::env::temp_dir().join(format!("scrutate_grepcli_{}_{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        Dir(fs::canonicalize(&root).unwrap())
    }
    fn write(&self, rel: &str, bytes: &[u8]) {
        let p = self.0.join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, bytes).unwrap();
    }
    fn run(&self, args: &[&str]) -> (i32, String, String) {
        run(&self.0, args.iter().map(|a| a.as_ref()).collect::<Vec<&std::ffi::OsStr>>().as_slice(), &[])
    }
    fn run_env(&self, args: &[&str], env: &[(&str, &str)]) -> (i32, String, String) {
        run(&self.0, args.iter().map(|a| a.as_ref()).collect::<Vec<&std::ffi::OsStr>>().as_slice(), env)
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn run(cwd: &Path, args: &[&std::ffi::OsStr], env: &[(&str, &str)]) -> (i32, String, String) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(cwd).stdin(Stdio::null()).env_remove("NO_COLOR");
    for v in ["SCRUTATE_MCP_STRUCTURED", "SCRUTATE_GREP_HIDDEN", "SCRUTATE_GREP_NO_IGNORE", "SCRUTATE_GREP_SMART_CASE", "SCRUTATE_GREP_FOLLOW"] {
        c.env_remove(v);
    }
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output().expect("run scrutate");
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

fn sorted(out: &str) -> Vec<String> {
    let mut v: Vec<String> = out.lines().map(String::from).collect();
    v.sort();
    v
}

#[test]
fn on_off_environment_variables_take_one_and_empty() {
    let d = Dir::new("env");
    d.write(".hidden/a.txt", b"needle\n");
    d.write("ignored/b.txt", b"needle\n");
    d.write(".ignore", b"ignored/\n");
    d.write("c.txt", b"Needle\n");
    for var in ["NO_COLOR", "SCRUTATE_GREP_HIDDEN", "SCRUTATE_GREP_NO_IGNORE", "SCRUTATE_GREP_SMART_CASE", "SCRUTATE_GREP_FOLLOW", "SCRUTATE_MCP_STRUCTURED"] {
        for v in ["1", "", "0", "true", "off"] {
            let (code, _, err) = d.run_env(&["grep", "needle"], &[(var, v)]);
            assert!(code == 0 || code == 1, "{var}={v:?}: exit {code}, {err}");
            let (code, _, err) = d.run_env(&["languages"], &[(var, v)]);
            assert_eq!(code, 0, "{var}={v:?}: {err}");
        }
    }
    let (code, _, err) = d.run_env(&["serve"], &[("SCRUTATE_MCP_STRUCTURED", "1"), ("NO_COLOR", "1")]);
    assert_eq!(code, 0, "serve: {err}");
    let has = |env: &[(&str, &str)], what: &str| d.run_env(&["grep", "-l", "needle"], env).1.contains(what);
    assert!(has(&[("SCRUTATE_GREP_HIDDEN", "1")], ".hidden/a.txt"));
    assert!(has(&[("SCRUTATE_GREP_HIDDEN", "yes")], ".hidden/a.txt"));
    assert!(!has(&[("SCRUTATE_GREP_HIDDEN", "0")], ".hidden/a.txt"));
    assert!(!has(&[("SCRUTATE_GREP_HIDDEN", "")], ".hidden/a.txt"));
    assert!(has(&[("SCRUTATE_GREP_NO_IGNORE", "on")], "ignored/b.txt"));
    assert!(!has(&[("SCRUTATE_GREP_NO_IGNORE", "false")], "ignored/b.txt"));
    assert!(has(&[("SCRUTATE_GREP_SMART_CASE", "1")], "c.txt"));
    assert!(!has(&[("SCRUTATE_GREP_SMART_CASE", "no")], "c.txt"));
}

#[cfg(unix)]
#[test]
fn arguments_that_are_not_utf8_are_errors_not_aborts() {
    use std::os::unix::ffi::OsStrExt;
    let d = Dir::new("utf8");
    d.write("a.txt", b"foo\n");
    let bad = std::ffi::OsStr::from_bytes(b"d1/\xff.txt");
    for args in [vec!["grep".as_ref(), "foo".as_ref(), bad], vec!["grep".as_ref(), bad], vec!["grep".as_ref(), "--json".as_ref(), "foo".as_ref(), bad]] {
        let (code, _, err) = run(&d.0, &args, &[]);
        assert!(code == 1 || code == 2, "exit {code}: {err}");
        assert!(!err.contains("panicked"), "{err}");
    }
}

fn three_foo_files(d: &Dir, n: usize) {
    for i in 0..n {
        d.write(&format!("d1/f{i}.txt"), b"foo 1\nx\nfoo 3\ny\nfoo 5\nz\n");
    }
}

#[test]
fn max_count_is_a_per_file_limit_of_the_search() {
    let d = Dir::new("maxcount");
    three_foo_files(&d, 8);
    let (_, out, _) = d.run(&["grep", "-m", "1", "--max-results", "5", "foo", "d1"]);
    assert_eq!(out.lines().count(), 5, "{out}");
    let (_, out, _) = d.run(&["grep", "-m", "1", "-A", "1", "foo", "d1/f0.txt"]);
    assert_eq!(out, "1:foo 1\n2-x\n");
    // as ripgrep: a match inside the trailing context still shows as a match
    let (_, out, _) = d.run(&["grep", "-m", "1", "-A", "3", "foo", "d1/f0.txt"]);
    assert_eq!(out, "1:foo 1\n2-x\n3:foo 3\n4-y\n");
    let (_, out, _) = d.run(&["grep", "-c", "-m", "2", "foo", "d1/f0.txt"]);
    assert_eq!(out, "2\n");
    let (_, out, _) = d.run(&["grep", "-v", "-m", "2", "foo", "d1/f0.txt"]);
    assert_eq!(out, "2:x\n4:y\n");
    let (_, out, _) = d.run(&["grep", "-U", "-m", "1", r"foo 1\nx", "d1/f0.txt"]);
    assert_eq!(out, "1:foo 1\n2:x\n");
    let (_, out, _) = d.run(&["grep", "--json", "-m", "1", "foo", "d1"]);
    let summary = out.lines().find(|l| l.contains("\"summary\"")).unwrap();
    assert!(summary.contains("\"matches\":8"), "{summary}");
    assert_eq!(out.lines().filter(|l| l.contains("\"type\":\"match\"")).count(), 8);
    // a file large enough to be searched in several ranges: still one limit for the whole file
    let mut big = Vec::new();
    for i in 0..20000 {
        big.extend_from_slice(format!("line {i} foo\n").as_bytes());
    }
    d.write("big.txt", &big);
    let split = [("SCRUTATE_SPLIT_FILE_BYTES", "4096"), ("SCRUTATE_THREADS", "4")];
    let (_, out, _) = d.run_env(&["grep", "-m", "3", "foo", "big.txt"], &split);
    assert_eq!(out.lines().count(), 3, "{out}");
    let (_, out, _) = d.run_env(&["grep", "-c", "-m", "3", "foo", "big.txt"], &split);
    assert_eq!(out, "3\n");
    let (_, out, _) = d.run_env(&["grep", "-c", "foo", "big.txt"], &split);
    assert_eq!(out, "20000\n");
}

#[test]
fn max_results_holds_for_counts_and_file_lists() {
    let d = Dir::new("maxresults");
    three_foo_files(&d, 4);
    let (_, out, _) = d.run(&["grep", "-c", "--max-results", "2", "foo", "d1"]);
    let total: u64 = out.lines().map(|l| l.rsplit(':').next().unwrap().parse::<u64>().unwrap()).sum();
    assert_eq!(total, 2, "{out}");
    let (_, out, _) = d.run(&["grep", "-l", "--max-results", "2", "foo", "d1"]);
    assert_eq!(out.lines().count(), 2, "{out}");
    let (code, _, _) = d.run(&["grep", "-l", "--max-results", "2", "foo", "d1"]);
    assert_eq!(code, 0);
}

#[test]
fn context_lines_of_crlf_files_lose_the_carriage_return_too() {
    let d = Dir::new("crlf");
    d.write("crlf.txt", b"ctx1\r\nfoo\r\nctx2\r\n");
    let (_, out, _) = d.run(&["grep", "-C1", "foo", "crlf.txt"]);
    assert_eq!(out, "1-ctx1\n2:foo\n3-ctx2\n");
    let (_, out, _) = d.run(&["grep", "--json", "-C1", "foo", "crlf.txt"]);
    assert!(!out.contains("\\r"), "{out}");
}

#[test]
fn globs_with_a_slash_apply_under_every_root() {
    let d = Dir::new("globs");
    d.write("dirA/src/x.rs", b"needle\n");
    d.write("dirB/src/x.rs", b"needle\n");
    d.write("dirB/other/x.rs", b"needle\n");
    let (_, out, _) = d.run(&["grep", "-l", "-g", "src/*.rs", "needle", "dirA", "dirB"]);
    assert_eq!(sorted(&out), ["dirA/src/x.rs", "dirB/src/x.rs"]);
    let (_, out, _) = d.run(&["grep", "-l", "-g", "!src/*.rs", "needle", "dirA", "dirB"]);
    assert_eq!(sorted(&out), ["dirB/other/x.rs"]);
    let (_, out, _) = d.run(&["grep", "-l", "--max-results", "1", "-g", "src/*.rs", "needle", "dirA", "dirB"]);
    assert_eq!(out.lines().count(), 1, "{out}");
}

#[test]
fn paths_are_printed_as_given() {
    let d = Dir::new("paths");
    d.write("dir/a.txt", b"needle\n");
    d.write("top.txt", b"needle\n");
    let abs = d.0.join("dir").to_string_lossy().into_owned();
    for (args, want) in [
        (vec![], vec!["dir/a.txt", "top.txt"]),
        (vec!["."], vec!["./dir/a.txt", "./top.txt"]),
        (vec!["dir"], vec!["dir/a.txt"]),
        (vec!["dir/"], vec!["dir/a.txt"]),
        (vec!["./top.txt"], vec!["./top.txt"]),
        (vec!["dir", "./top.txt"], vec!["./top.txt", "dir/a.txt"]),
    ] {
        let mut a = vec!["grep", "-l", "needle"];
        a.extend(&args);
        assert_eq!(sorted(&d.run(&a).1), want, "{args:?}");
    }
    let (_, out, _) = d.run(&["grep", "-l", "needle", &abs]);
    assert_eq!(out, format!("{abs}/a.txt\n"));
    let (_, out, _) = d.run(&["grep", "needle", "dir"]);
    assert_eq!(out, "dir/a.txt:1:needle\n");
    let (_, out, _) = d.run(&["grep", "--json", "needle", "dir"]);
    assert!(out.contains("\"path\":\"dir/a.txt\""), "{out}");
    let (_, out, _) = d.run(&["grep", "-c", "needle", "dir"]);
    assert_eq!(out, "dir/a.txt:1\n");
}

#[test]
fn a_size_too_large_for_64_bits_is_refused() {
    let d = Dir::new("size");
    d.write("a.txt", b"foo\n");
    let (code, _, err) = d.run(&["grep", "--max-filesize", "17179869184G", "foo"]);
    assert_eq!(code, 2, "{err}");
    assert!(err.contains("bad size"), "{err}");
}

#[test]
fn smart_case_folds_only_a_pattern_with_lowercase_literals() {
    let d = Dir::new("smart");
    d.write("a.txt", b"ABC\nabc\n");
    for (pat, want) in [
        (r"\p{Ll}+", vec!["2:abc"]),
        (r"\w+", vec!["1:ABC", "2:abc"]),
        (r"[a-c]+", vec!["1:ABC", "2:abc"]),
        (r"\p{Ll}b", vec!["1:ABC", "2:abc"]),
        (r"[[:lower:]]+", vec!["2:abc"]),
        (r"Ab", vec![]),
    ] {
        assert_eq!(sorted(&d.run(&["grep", "-S", pat, "a.txt"]).1), want, "{pat}");
    }
}

#[test]
fn a_search_path_that_does_not_exist_is_an_error() {
    let d = Dir::new("missing");
    d.write("a.txt", b"foo\n");
    let (code, _, err) = d.run(&["grep", "foo", "no/such/dir"]);
    assert_eq!(code, 2, "{err}");
    let (code, out, _) = d.run(&["grep", "foo", "a.txt", "no/such/dir"]);
    assert_eq!((code, out.as_str()), (2, "a.txt:1:foo\n"));
    let (code, _, _) = d.run(&["grep", "--json", "foo", "no/such/dir"]);
    assert_eq!(code, 2);
    let (code, _, _) = d.run(&["grep", "bar", "a.txt"]);
    assert_eq!(code, 1);
}

#[cfg(unix)]
#[test]
fn an_unreadable_file_in_the_walk_does_not_change_the_exit_code() {
    use std::os::unix::fs::PermissionsExt;
    let d = Dir::new("unreadable");
    d.write("dir/a.txt", b"foo\n");
    d.write("dir/b.txt", b"foo\n");
    fs::set_permissions(d.0.join("dir/b.txt"), fs::Permissions::from_mode(0o000)).unwrap();
    if fs::read(d.0.join("dir/b.txt")).is_ok() {
        return; // running as root
    }
    let (code, _, _) = d.run(&["grep", "foo", "dir"]);
    fs::set_permissions(d.0.join("dir/b.txt"), fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(code, 0);
}

#[test]
fn find_takes_a_question_as_words() {
    let d = Dir::new("find");
    d.write("auth.py", b"def renew_token(session):\n    \"\"\"Renew the session token before it expires.\"\"\"\n    return session\n");
    let (code, plain, err) = d.run(&["find", "where is the token renewed"]);
    assert_eq!(code, 0, "{err}");
    let (code, asked, err) = d.run(&["find", "where is the token renewed?"]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(asked, plain);
    // a regex-looking query that greps nothing falls back to the ranked search once there is an index
    let (code, out, err) = d.run(&["find", "renew token (session"]);
    assert!(code == 0 && out.contains("auth.py"), "exit {code}: {out} {err}");
}

#[test]
fn tsquery_shows_control_characters_in_paths_as_escapes() {
    let d = Dir::new("tsq");
    if fs::write(d.0.join("a\nb\x1b.py"), b"def f():\n    pass\n").is_err() {
        return; // the filesystem refuses such names
    }
    let (code, out, err) = d.run(&["tsquery", "python", "(function_definition name: (identifier) @n)", "."]);
    assert_eq!(code, 0, "{err}");
    assert_eq!(out.lines().count(), 1, "{out}");
    assert!(out.contains(r"a\nb\x1b.py:1:"), "{out}");
}

#[test]
fn symbols_are_found_for_matches_under_a_path_argument() {
    let d = Dir::new("symbols_sub");
    d.write("sub/c.py", b"def holder():\n    needle = 1\n    return needle\n");
    assert_eq!(d.run(&["index"]).0, 0);
    for path in ["sub", "sub/", "./sub", "."] {
        let (code, out, _) = d.run(&["grep", "--symbols", "needle", path]);
        assert_eq!(code, 0);
        assert!(out.contains("sub/c.py:1-3  [function] holder"), "{path}: {out}");
    }
    let abs = d.0.join("sub");
    let (_, out, _) = d.run(&["grep", "--symbols", "needle", abs.to_str().unwrap()]);
    assert!(out.contains("sub/c.py:1-3  [function] holder"), "{out}");
}

#[test]
fn a_glob_on_the_command_line_is_taken_whole() {
    let d = Dir::new("globcomma");
    for f in ["a.txt", "b.log", "c.rs"] {
        d.write(f, b"foo\n");
    }
    let (code, out, err) = d.run(&["grep", "-l", "-g", "*.{txt,log}", "foo"]);
    assert_eq!((code, sorted(&out)), (0, vec!["a.txt".to_string(), "b.log".to_string()]), "{err}");
    // as ripgrep: one glob that holds a comma, so no file matches
    let (code, out, _) = d.run(&["grep", "-l", "-g", "*.txt,*.log", "foo"]);
    assert_eq!((code, out.as_str()), (1, ""));
    let (_, out, _) = d.run(&["grep", "-l", "-g", "*.txt", "-g", "*.rs", "foo"]);
    assert_eq!(sorted(&out), ["a.txt", "c.rs"]);
    let (_, out, _) = d.run_env(&["grep", "-l", "foo"], &[("SCRUTATE_GREP_GLOBS", "*.txt,*.log")]);
    assert_eq!(sorted(&out), ["a.txt", "b.log"]);
    let (_, out, _) = d.run_env(&["grep", "-l", "foo"], &[("SCRUTATE_GREP_GLOBS", "*.{txt,log},!b.*")]);
    assert_eq!(sorted(&out), ["a.txt"]);
    let (_, out, _) = d.run_env(&["grep", "-l", "-g", "*.rs", "foo"], &[("SCRUTATE_GREP_GLOBS", "*.txt")]);
    assert_eq!(sorted(&out), ["c.rs"]);
    d.write(".scrutate.toml", b"[grep]\nglobs = [\"*.{txt,log}\"]\n");
    let (code, out, err) = d.run(&["grep", "-l", "foo"]);
    assert_eq!((code, sorted(&out)), (0, vec!["a.txt".to_string(), "b.log".to_string()]), "{err}");
}

#[test]
fn context_groups_are_separated_as_ripgrep_does() {
    let d = Dir::new("ctxsep");
    d.write("a.txt", b"foo\na\nb\nc\nd\nfoo\ne\n");
    d.write("b.txt", b"x\nfoo\ny\nfoo\nz\n");
    d.write("t.txt", b"foo\nb\nc\nfoo\ne\nf\ng\nfoo\n");
    let a = "a.txt:1:foo\na.txt-2-a\n--\na.txt:6:foo\na.txt-7-e\n";
    let b = "b.txt:2:foo\nb.txt-3-y\nb.txt:4:foo\nb.txt-5-z\n";
    for sort in [false, true] {
        let with = |args: &[&str]| {
            let mut v = vec!["grep", "-H", "-j", "1"];
            if sort {
                v.push("--sort");
            }
            v.extend(args);
            d.run(&v).1
        };
        assert_eq!(with(&["-A1", "foo", "a.txt"]), a, "sort {sort}");
        assert_eq!(with(&["-A1", "foo", "a.txt", "b.txt"]), format!("{a}--\n{b}"), "sort {sort}");
        assert_eq!(with(&["-A1", "foo", "b.txt"]), b, "sort {sort}");
        assert_eq!(with(&["-C1", "foo", "t.txt"]), "t.txt:1:foo\nt.txt-2-b\nt.txt-3-c\nt.txt:4:foo\nt.txt-5-e\n--\nt.txt-7-g\nt.txt:8:foo\n", "sort {sort}");
        assert_eq!(with(&["-A0", "foo", "a.txt"]), "a.txt:1:foo\na.txt:6:foo\n", "sort {sort}");
        assert_eq!(with(&["-U", "-A1", r"c\nd", "a.txt", "b.txt"]), "a.txt:4:c\na.txt:5:d\na.txt-6-foo\n", "sort {sort}");
    }
    // many threads: one separator between any two groups, wherever each file's lines land
    let (_, out, _) = d.run(&["grep", "-A1", "foo", "a.txt", "b.txt", "t.txt"]);
    assert_eq!(out.lines().filter(|l| *l == "--").count(), 5, "{out}");
    assert!(!out.starts_with("--") && !out.ends_with("--\n"), "{out}");
    for flags in [["--json", "-A1"], ["-c", "-A1"], ["-l", "-A1"]] {
        let (_, out, _) = d.run(&["grep", flags[0], flags[1], "foo", "a.txt", "b.txt"]);
        assert!(!out.lines().any(|l| l == "--"), "{flags:?}: {out}");
    }
    // a file large enough to be searched in ranges gives the same lines
    let big: String = (0..3000).map(|i| if i % 100 == 0 { "foo here\n".to_string() } else { format!("line {i}\n") }).collect();
    d.write("big.txt", big.as_bytes());
    let (_, whole, _) = d.run(&["grep", "-C1", "foo", "big.txt"]);
    let (_, split, _) = d.run_env(&["grep", "-C1", "foo", "big.txt"], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096"), ("SCRUTATE_THREADS", "4")]);
    assert_eq!(whole, split);
    assert_eq!(whole.lines().filter(|l| *l == "--").count(), 29, "{whole}");
}

#[test]
fn file_names_are_printed_as_ripgrep_prints_them() {
    let d = Dir::new("withname");
    d.write("a.txt", b"foo\nbar\nfoo2\n");
    d.write("d/b.txt", b"foo x\n");
    let big: String = (0..3000).map(|i| if i % 100 == 0 { "foo here\n".to_string() } else { format!("line {i}\n") }).collect();
    d.write("big.txt", big.as_bytes());
    let long = format!("foo{}\n", "y".repeat(70_000));
    d.write("long.txt", long.as_bytes());
    let out = |args: &[&str]| {
        let mut v = vec!["grep"];
        v.extend(args);
        let (code, out, err) = d.run(&v);
        assert!(code <= 1, "{args:?}: {err}");
        out
    };
    // one file argument: no name; a directory, several paths or the default root: names
    for sort in [&[][..], &["--sort"][..], &["-j", "1"][..]] {
        let with = |args: &[&str]| out(&[sort, args].concat());
        assert_eq!(with(&["foo", "a.txt"]), "1:foo\n3:foo2\n", "{sort:?}");
        assert_eq!(with(&["foo", "./a.txt"]), "1:foo\n3:foo2\n", "{sort:?}");
        assert_eq!(with(&["-c", "foo", "a.txt"]), "2\n", "{sort:?}");
        assert_eq!(with(&["-l", "foo", "a.txt"]), "a.txt\n", "{sort:?}");
        assert_eq!(with(&["-C1", "bar", "a.txt"]), "1-foo\n2:bar\n3-foo2\n", "{sort:?}");
        assert_eq!(with(&["-U", r"bar\nfoo", "a.txt"]), "2:bar\n3:foo2\n", "{sort:?}");
        assert_eq!(with(&["foo", "d"]), "d/b.txt:1:foo x\n", "{sort:?}");
        assert_eq!(with(&["-c", "foo", "d"]), "d/b.txt:1\n", "{sort:?}");
        assert_eq!(sorted(&with(&["foo", "a.txt", "d"])), ["a.txt:1:foo", "a.txt:3:foo2", "d/b.txt:1:foo x"], "{sort:?}");
        assert_eq!(sorted(&with(&["-c", "foo", "a.txt", "d/b.txt"])), ["a.txt:2", "d/b.txt:1"], "{sort:?}");
        assert!(with(&["-c", "foo"]).contains("a.txt:2\n"), "{sort:?}");
        // -H and -I: the later one wins
        assert_eq!(with(&["-H", "foo", "a.txt"]), "a.txt:1:foo\na.txt:3:foo2\n", "{sort:?}");
        assert_eq!(with(&["--with-filename", "-c", "foo", "a.txt"]), "a.txt:2\n", "{sort:?}");
        assert_eq!(with(&["-I", "foo", "d"]), "1:foo x\n", "{sort:?}");
        assert_eq!(with(&["--no-filename", "-c", "foo", "d"]), "1\n", "{sort:?}");
        assert_eq!(with(&["-I", "-l", "foo", "d"]), "d/b.txt\n", "{sort:?}");
        assert_eq!(with(&["-H", "-I", "foo", "a.txt"]), "1:foo\n3:foo2\n", "{sort:?}");
        assert_eq!(with(&["-I", "-H", "foo", "a.txt"]), "a.txt:1:foo\na.txt:3:foo2\n", "{sort:?}");
        // a file searched in ranges and a line written from where it lies
        let lines = with(&["foo", "big.txt"]);
        assert_eq!(lines.lines().count(), 30, "{sort:?}");
        assert!(lines.starts_with("1:foo here\n101:foo here\n"), "{sort:?}: {lines}");
        assert_eq!(with(&["foo", "long.txt"]), format!("1:{long}"), "{sort:?}");
    }
    let (_, split, _) = d.run_env(&["grep", "foo", "big.txt"], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096"), ("SCRUTATE_THREADS", "4")]);
    assert_eq!(split, out(&["foo", "big.txt"]));
    let (_, split, _) = d.run_env(&["grep", "-c", "foo", "big.txt"], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096"), ("SCRUTATE_THREADS", "4")]);
    assert_eq!(split, "30\n");
    // JSON always carries the path
    for args in [&["--json", "foo", "a.txt"][..], &["--json", "-I", "foo", "a.txt"], &["--json", "-c", "foo", "a.txt"], &["--json", "-I", "-l", "foo", "d"]] {
        let o = out(args);
        assert!(
            o.lines().filter(|l| !l.contains("\"summary\"")).all(|l| l.contains("\"path\":\"a.txt\"") || l.contains("\"path\":\"d/b.txt\"")),
            "{args:?}: {o}"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_backslash_in_a_file_name_is_printed_as_it_is() {
    let d = Dir::new("backslash");
    d.write("real\\nname.py", b"REAL_MARK = 1\n");
    d.write("dir\\sub/file.py", b"BSD_MARK = 1\n");
    let (code, out, err) = d.run(&["grep", "-l", "REAL_MARK|BSD_MARK"]);
    assert_eq!((code, sorted(&out)), (0, vec!["dir\\sub/file.py".to_string(), "real\\nname.py".to_string()]), "{err}");
    let (_, out, _) = d.run(&["grep", "-c", "MARK"]);
    assert_eq!(sorted(&out), ["dir\\sub/file.py:1", "real\\nname.py:1"]);
    let (_, out, _) = d.run(&["grep", "--json", "REAL_MARK"]);
    assert!(out.contains(r#""path":"real\\nname.py""#), "{out}");
    let (_, out, _) = d.run(&["grep", "BSD_MARK", "dir\\sub"]);
    assert_eq!(out, "dir\\sub/file.py:1:BSD_MARK = 1\n");
    let (_, out, _) = d.run(&["grep", "-H", "REAL_MARK", "real\\nname.py"]);
    assert_eq!(out, "real\\nname.py:1:REAL_MARK = 1\n");
}

/// procfs and sysfs files report a size of 0 and return a page or less per read.
#[cfg(target_os = "linux")]
#[test]
fn files_under_proc_and_sys_are_read_to_the_end() {
    let d = Dir::new("procfs");
    let mut checked = 0;
    for (file, pat) in
        [("/proc/cpuinfo", "processor"), ("/proc/kallsyms", " "), ("/proc/self/mountinfo", " "), ("/sys/kernel/mm/transparent_hugepage/enabled", "a")]
    {
        let Ok(text) = fs::read_to_string(file) else { continue };
        let want = text.lines().filter(|l| l.contains(pat)).count();
        let (dir, name) = file.rsplit_once('/').unwrap();
        // reached from a root above it that is not `/` by its path (a procfs mounted deeper)
        let (depth, rel) = (file.matches('/').count().to_string(), &file[1..]);
        let mut cases: Vec<Vec<&str>> =
            vec![vec!["grep", "-c", pat, file], vec!["grep", "-c", "--stats", pat, file], vec!["grep", "-c", "--max-depth", "1", "-g", name, pat, dir]];
        if !file.contains("/self/") {
            cases.push(vec!["grep", "-c", "--max-depth", &depth, "-g", rel, pat, "/etc/.."]);
        }
        for args in &cases {
            let (_, out, err) = d.run(args);
            let got: usize = out.trim().rsplit(':').next().unwrap_or("").parse().unwrap_or(usize::MAX);
            assert_eq!(got, want, "{args:?}: {out} {err}");
        }
        checked += 1;
    }
    assert!(checked > 0);
}

#[test]
fn the_later_case_flag_wins() {
    let d = Dir::new("caseflags");
    d.write("c.txt", b"Foo\nfoo\nFOO\n");
    let count = |args: &[&str], env: &[(&str, &str)]| {
        let mut v = vec!["grep", "-c"];
        v.extend(args);
        v.extend(["Foo", "c.txt"]);
        d.run_env(&v, env).1
    };
    assert_eq!(count(&["-i", "-S"], &[]), "1\n");
    assert_eq!(count(&["-S", "-i"], &[]), "3\n");
    assert_eq!(count(&["--ignore-case", "--smart-case"], &[]), "1\n");
    assert_eq!(count(&["-i"], &[("SCRUTATE_GREP_SMART_CASE", "1")]), "3\n");
    assert_eq!(count(&["-S", "-i"], &[("SCRUTATE_GREP_SMART_CASE", "1")]), "3\n");
    assert_eq!(count(&[], &[("SCRUTATE_GREP_SMART_CASE", "1")]), "1\n");
}

#[test]
fn a_multi_line_count_counts_matches_that_share_a_line() {
    let d = Dir::new("mlcount");
    d.write("m.txt", b"x a\nb a\nb\nq\n");
    d.write("aa.txt", b"aa\n");
    // ripgrep's counts: the two matches of `a\nb` share line 2; `a` cannot span lines
    for (args, want) in
        [(&["-U", r"a\nb", "m.txt"][..], "2\n"), (&["-U", r"a\nb|q", "m.txt"], "3\n"), (&["-U", "a$", "m.txt"], "2\n"), (&["-U", "a", "aa.txt"], "1\n")]
    {
        let mut v = vec!["grep", "-c"];
        v.extend(args);
        assert_eq!(d.run(&v).1, want, "{args:?}");
    }
}

#[test]
fn a_bad_configuration_file_stops_only_the_commands_that_use_it() {
    let d = Dir::new("badcfg");
    d.write("a.txt", b"foo\n");
    d.write(".scrutate.toml", b"[grep]\nhiden = true\n");
    for args in [&["--version"][..], &["--help"], &["grep", "--help"], &["refs", "--help"], &["help", "grep"], &["completions", "bash"]] {
        let (code, out, err) = d.run(args);
        assert_eq!(code, 0, "{args:?}: {err}");
        assert!(!out.is_empty(), "{args:?}");
    }
    let (code, out, err) = d.run(&["config"]);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains("hiden"), "{out}");
    assert!(err.contains(".scrutate.toml") && err.contains("hiden"), "{err}");
    for args in [&["grep", "foo"][..], &["grep", "foo", "a.txt"], &["languages"]] {
        let (code, _, err) = d.run(args);
        assert_eq!(code, 2, "{args:?}");
        assert!(err.contains(".scrutate.toml") && err.contains("hiden"), "{args:?}: {err}");
    }
}

#[test]
fn the_thread_count_has_a_ceiling() {
    let d = Dir::new("threads");
    for i in 0..11 {
        d.write(&format!("sub/f{i}.txt"), b"foo\n");
    }
    let t = std::time::Instant::now();
    let (code, out, err) = d.run(&["grep", "-j", "100000", "-c", "foo", "."]);
    assert_eq!((code, out.lines().count()), (0, 11), "{err}");
    let (code, out, err) = d.run_env(&["grep", "-c", "foo", "."], &[("SCRUTATE_THREADS", "100000")]);
    assert_eq!((code, out.lines().count()), (0, 11), "{err}");
    d.write("big.txt", &b"foo\n".repeat(5000));
    let (code, out, err) = d.run_env(&["grep", "-j", "100000", "-c", "foo", "big.txt"], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096")]);
    assert_eq!((code, out.as_str()), (0, "5000\n"), "{err}");
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    assert!(d.run(&["grep", "--help"]).1.contains("256"));
}

/// A file of 300 KB: a match, then a NUL byte past the first read buffer, then two more matches.
fn late_nul_file() -> Vec<u8> {
    let mut v = b"foo first\n".to_vec();
    v.extend_from_slice(&[b'x'; 80]);
    v.push(b'\n');
    for _ in 0..3000 {
        v.extend_from_slice(&[b'y'; 99]);
        v.push(b'\n');
    }
    v.extend_from_slice(b"nul\0here foo\nfoo last\n");
    v
}

#[test]
fn named_files_are_searched_as_ripgrep_does() {
    let d = Dir::new("named");
    d.write(".ignore", b"ign.txt\n");
    d.write("ign.txt", b"foo\n");
    d.write("a.txt", b"foo\n");
    d.write("nul.txt", b"foo\nbar\0baz\nfoo2\n");
    d.write("big.bin", &late_nul_file());
    let note = |off: u64| format!("binary file matches (found \"\\0\" byte around offset {off})");
    // ripgrep 15's output and exit status for each
    for (args, code, want) in [
        (&["foo", "ign.txt"][..], 0, "1:foo\n".to_string()),
        (&["-c", "foo", "ign.txt", "nul.txt"], 0, "ign.txt:1\nnul.txt:2\n".into()),
        (&["foo", "nul.txt"], 0, format!("{}\n", note(7))),
        (&["-H", "foo", "nul.txt"], 0, format!("nul.txt: {}\n", note(7))),
        (&["--sort", "foo", "nul.txt", "a.txt"], 0, format!("a.txt:1:foo\nnul.txt: {}\n", note(7))),
        (&["-c", "foo", "nul.txt"], 0, "2\n".into()),
        (&["-l", "foo", "nul.txt"], 0, "nul.txt\n".into()),
        (&["-v", "foo", "nul.txt"], 0, format!("{}\n", note(7))),
        (&["-v", "-c", "foo", "nul.txt"], 0, "2\n".into()),
        (&["-v", "-l", "foo", "nul.txt"], 0, "nul.txt\n".into()),
        (&["zzz", "nul.txt"], 1, String::new()),
        (&["foo", "big.bin"], 0, format!("1:foo first\n{}\n", note(300094))),
        (&["-c", "foo", "big.bin"], 0, "3\n".into()),
    ] {
        let mut v = vec!["grep"];
        v.extend(args);
        let (c, out, err) = d.run(&v);
        assert_eq!((c, out), (code, want), "{args:?}: {err}");
        // the same through the sorted (non-streaming) output
        if !args.contains(&"-c") && !args.contains(&"-l") && !args.contains(&"--sort") {
            v.insert(1, "--sort");
            assert_eq!(d.run(&v).0, code, "{v:?}");
        }
    }
    // a file named twice is searched twice, as ripgrep does, but counted and listed once per path
    assert_eq!(d.run(&["grep", "foo", "a.txt", "a.txt"]).1, "a.txt:1:foo\na.txt:1:foo\n");
    assert_eq!(d.run(&["grep", "-c", "foo", "a.txt", "a.txt"]).1, "a.txt:2\n");
    assert_eq!(d.run(&["grep", "-l", "foo", "a.txt", "a.txt"]).1, "a.txt\n");
}

#[test]
fn a_file_with_a_nul_byte_found_by_the_walk_is_left_out_of_counts_and_lists() {
    let d = Dir::new("walknul");
    d.write("d/keep.log", b"\n\n\nx\na.b\nfoobar e\0binnd\n");
    d.write("d/ok.txt", b"zzz\n");
    d.write("d/big.bin", &late_nul_file());
    let run = |args: &[&str]| sorted(&d.run(&[&["grep"][..], args, &["d"]].concat()).1);
    // ripgrep 15: -v -l and -v -c agree, and a count stops at the NUL byte
    assert_eq!(run(&["-v", "-l", "baz"]), ["d/ok.txt"]);
    assert_eq!(run(&["-v", "-c", "baz"]), ["d/ok.txt:1"]);
    assert!(run(&["-c", "foo"]).is_empty());
    assert_eq!(run(&["-l", "foo"]), ["d/big.bin"]);
}

/// `scrutate ARGS` in `cwd` with `input` as its standard input: a pipe, or with `None` a regular
/// file (`file`).
fn run_stdin(cwd: &Path, args: &[&str], input: Option<&[u8]>, file: Option<&Path>) -> (i32, String, String) {
    let mut c = Command::new(BIN);
    c.args(args).current_dir(cwd).env_remove("NO_COLOR").stdout(Stdio::piped()).stderr(Stdio::piped());
    match file {
        Some(f) => c.stdin(fs::File::open(f).unwrap()),
        None => c.stdin(Stdio::piped()),
    };
    let mut child = c.spawn().unwrap();
    if let Some(bytes) = input {
        use std::io::Write;
        child.stdin.take().unwrap().write_all(bytes).unwrap();
    }
    drop(child.stdin.take());
    let out = child.wait_with_output().unwrap();
    (out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn standard_input_is_searched_when_it_is_a_pipe_or_a_file() {
    let d = Dir::new("stdin");
    d.write("a.txt", b"foo in a file\n");
    let pipe = |args: &[&str], input: &[u8]| run_stdin(&d.0, &[&["grep"][..], args].concat(), Some(input), None);
    // ripgrep 15 with -n
    assert_eq!(pipe(&["foo"], b"x\nfoo\n"), (0, "2:foo\n".into(), String::new()));
    assert_eq!(pipe(&["-H", "foo"], b"foo\n").1, "<stdin>:1:foo\n");
    assert_eq!(pipe(&["-c", "foo"], b"foo\nfoo\n").1, "2\n");
    assert_eq!(pipe(&["-c", "-H", "foo"], b"foo\n").1, "<stdin>:1\n");
    assert_eq!(pipe(&["-l", "foo"], b"foo\n").1, "<stdin>\n");
    assert_eq!(pipe(&["--sort", "foo"], b"foo\n").1, "1:foo\n");
    assert!(pipe(&["--json", "foo"], b"foo\n").1.contains("\"path\":\"<stdin>\""));
    assert_eq!(pipe(&["foo"], b"bar\n").0, 1);
    assert_eq!(pipe(&["foo"], b"foo\0\nfoo\n").1, "binary file matches (found \"\\0\" byte around offset 3)\n");
    assert_eq!(pipe(&["-c", "foo"], b"foo\0\nfoo\n").1, "2\n");
    // `-` names standard input among other paths
    assert_eq!(sorted(&pipe(&["foo", "-", "a.txt"], b"foo\n").1), ["<stdin>:1:foo", "a.txt:1:foo in a file"]);
    assert_eq!(pipe(&["foo", "a.txt"], b"foo\n").1, "1:foo in a file\n");
    // a regular file as standard input
    d.write("in.txt", b"foo from a redirect\n");
    assert_eq!(run_stdin(&d.0, &["grep", "foo"], None, Some(&d.0.join("in.txt"))).1, "1:foo from a redirect\n");
    // no standard input to read (`/dev/null`, the tests' default): the current directory
    assert_eq!(sorted(&d.run(&["grep", "-l", "foo"]).1), ["a.txt", "in.txt"]);
    // --index answers from the index whatever standard input is
    let (code, _, err) = pipe(&["--index", "foo"], b"foo\n");
    assert!(code == 2 && err.contains("text index"), "{code} {err}");
}

#[test]
fn a_sorted_search_with_a_cap_keeps_the_first_matches_in_path_order() {
    let d = Dir::new("orderedcap");
    for i in 0..40 {
        let body: String = (0..30).map(|k| if k % 3 == 0 { format!("foo {i} {k}\n") } else { format!("x {k}\n") }).collect();
        d.write(&format!("d{}/s{}/f{i:02}.txt", i % 4, i % 3), body.as_bytes());
    }
    d.write("d9/big.txt", &(0..3000).map(|k| format!("foo big {k}\n")).collect::<String>().into_bytes());
    for env in [&[][..], &[("SCRUTATE_SPLIT_FILE_BYTES", "4096")], &[("SCRUTATE_SMALL_TREE", "0"), ("SCRUTATE_THREADS", "8")]] {
        let full = d.run_env(&["grep", "--sort", "foo"], env).1;
        for (n, extra) in [(50usize, &[][..]), (7, &["-v"][..]), (380, &[]), (5000, &[])] {
            let mut args = vec!["grep", "--sort", "--max-results"];
            let n_s = n.to_string();
            args.push(&n_s);
            args.extend(extra);
            args.push("foo");
            let want: String = if extra.is_empty() {
                full.lines().take(n).map(|l| format!("{l}\n")).collect()
            } else {
                d.run_env(&["grep", "--sort", "-v", "foo"], env).1.lines().take(n).map(|l| format!("{l}\n")).collect()
            };
            for _ in 0..5 {
                let (code, out, err) = d.run_env(&args, env);
                assert_eq!(code, 0, "{err}");
                assert_eq!(out, want, "{args:?} {env:?}");
            }
        }
        // counts and file lists are cut at the same line
        let firsts: Vec<&str> = full.lines().take(50).collect();
        let mut counts: std::collections::BTreeMap<&str, usize> = Default::default();
        for l in &firsts {
            *counts.entry(l.split(':').next().unwrap()).or_default() += 1;
        }
        let want: String = counts.iter().map(|(p, c)| format!("{p}:{c}\n")).collect();
        assert_eq!(d.run_env(&["grep", "--sort", "-c", "--max-results", "50", "foo"], env).1, want, "{env:?}");
        let files: String = counts.keys().take(3).map(|p| format!("{p}\n")).collect();
        assert_eq!(d.run_env(&["grep", "--sort", "-l", "--max-results", "3", "foo"], env).1, files, "{env:?}");
    }
}

#[test]
fn line_number_flags_are_accepted() {
    let d = Dir::new("lineno");
    d.write("a.txt", b"x\nfoo\n");
    d.write("b.txt", b"foo\n");
    assert_eq!(d.run(&["grep", "-n", "foo", "a.txt"]).1, "2:foo\n");
    assert_eq!(d.run(&["grep", "--line-number", "foo", "a.txt"]).1, "2:foo\n");
    assert_eq!(d.run(&["grep", "-N", "foo", "a.txt"]).1, "foo\n");
    assert_eq!(d.run(&["grep", "--no-line-number", "-H", "foo", "a.txt"]).1, "a.txt:foo\n");
    assert_eq!(d.run(&["grep", "-N", "-C1", "foo", "a.txt"]).1, "x\nfoo\n");
    assert_eq!(d.run(&["grep", "-N", "--sort", "foo", "a.txt", "b.txt"]).1, "a.txt:foo\nb.txt:foo\n");
    // the later of the two wins, as in ripgrep
    assert_eq!(d.run(&["grep", "-N", "-n", "foo", "a.txt"]).1, "2:foo\n");
    assert_eq!(d.run(&["grep", "-n", "-N", "foo", "a.txt"]).1, "foo\n");
}
