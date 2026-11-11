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
    assert_eq!(out, "d1/f0.txt:1:foo 1\nd1/f0.txt-2-x\n");
    // as ripgrep: a match inside the trailing context still shows as a match
    let (_, out, _) = d.run(&["grep", "-m", "1", "-A", "3", "foo", "d1/f0.txt"]);
    assert_eq!(out, "d1/f0.txt:1:foo 1\nd1/f0.txt-2-x\nd1/f0.txt:3:foo 3\nd1/f0.txt-4-y\n");
    let (_, out, _) = d.run(&["grep", "-c", "-m", "2", "foo", "d1/f0.txt"]);
    assert_eq!(out, "d1/f0.txt:2\n");
    let (_, out, _) = d.run(&["grep", "-v", "-m", "2", "foo", "d1/f0.txt"]);
    assert_eq!(out, "d1/f0.txt:2:x\nd1/f0.txt:4:y\n");
    let (_, out, _) = d.run(&["grep", "-U", "-m", "1", r"foo 1\nx", "d1/f0.txt"]);
    assert_eq!(out, "d1/f0.txt:1:foo 1\nd1/f0.txt:2:x\n");
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
    assert_eq!(out, "big.txt:3\n");
    let (_, out, _) = d.run_env(&["grep", "-c", "foo", "big.txt"], &split);
    assert_eq!(out, "big.txt:20000\n");
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
    assert_eq!(out, "crlf.txt-1-ctx1\ncrlf.txt:2:foo\ncrlf.txt-3-ctx2\n");
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
        (r"\p{Ll}+", vec!["a.txt:2:abc"]),
        (r"\w+", vec!["a.txt:1:ABC", "a.txt:2:abc"]),
        (r"[a-c]+", vec!["a.txt:1:ABC", "a.txt:2:abc"]),
        (r"\p{Ll}b", vec!["a.txt:1:ABC", "a.txt:2:abc"]),
        (r"[[:lower:]]+", vec!["a.txt:2:abc"]),
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
