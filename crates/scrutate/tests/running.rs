//! Big searches running at the same time share the threads: the count across processes, what a
//! stopped, killed or blocked search does to it, and what is never counted. Searches of this user
//! elsewhere on the machine change the count, so the checks compare counts rather than expect
//! fixed ones.
#![cfg(target_os = "macos")]

use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const BIN: &str = env!("CARGO_BIN_EXE_scrutate");

/// One test at a time: each one counts the searches the others start.
static ONE: Mutex<()> = Mutex::new(());

/// 4,000 files of 16 KB in 40 directories: a walk long enough to stop in the middle. Each file has
/// two lines with `needle`: 1,000 files (when a walk is counted) print about 48 KB, less than the
/// output buffer and a pipe hold, and 4,000 files 192 KB, more.
fn tree() -> &'static Path {
    static T: OnceLock<PathBuf> = OnceLock::new();
    T.get_or_init(|| {
        // kept between runs, only read: 64 MB is too much to write for every run
        let done = std::env::temp_dir().join("scrutate_running_tree_1");
        if done.is_dir() {
            return done;
        }
        let root = std::env::temp_dir().join(format!("scrutate_running_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let body = "needle\n".repeat(2) + &"a line of text, long enough to make the file take some time\n".repeat(270);
        for d in 0..40 {
            let dir = root.join(format!("d{d}"));
            std::fs::create_dir_all(&dir).unwrap();
            for f in 0..100 {
                std::fs::write(dir.join(format!("f{f}.txt")), &body).unwrap();
            }
        }
        // whole or not there: a run stopped while writing leaves only its own directory
        if std::fs::rename(&root, &done).is_err() {
            let _ = std::fs::remove_dir_all(&root);
        }
        done
    })
}

struct Search {
    child: Child,
    /// (searches running, threads) per debug line
    lines: Receiver<(usize, usize)>,
}

impl Search {
    fn start(args: &[&str], out: Stdio) -> Search {
        let mut child = Command::new(BIN)
            .arg("grep")
            .args(args)
            .arg(".")
            .current_dir(tree())
            .env("SCRUTATE_GREP_DEBUG", "1")
            .env_remove("SCRUTATE_THREADS")
            // a group of its own, as a shell gives a job: in a group with no parent outside it
            // the system drops SIGTSTP
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(out)
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let err = child.stderr.take().unwrap();
        let (tx, lines) = channel();
        std::thread::spawn(move || {
            for line in BufReader::new(err).lines().map_while(Result::ok) {
                if let Some(rest) = line.strip_prefix("[scrutate] searches running: ") {
                    let (k, t) = rest.split_once(", threads: ").unwrap();
                    let _ = tx.send((k.parse().unwrap_or(0), t.parse().unwrap()));
                }
            }
        });
        Search { child, lines }
    }

    fn next(&self) -> (usize, usize) {
        self.lines.recv_timeout(Duration::from_secs(60)).expect("a debug line")
    }

    fn signal(&self, sig: &str) {
        assert!(self.try_signal(sig));
    }

    /// `false` when the search has ended.
    fn try_signal(&self, sig: &str) -> bool {
        // the shell's own `kill`: some systems have no kill program
        let cmd = format!("kill {sig} {} 2>/dev/null", self.child.id());
        Command::new("sh").args(["-c", &cmd]).status().unwrap().success()
    }

    fn finish(mut self) -> Vec<(usize, usize)> {
        self.child.wait().unwrap();
        self.lines.iter().collect()
    }
}

/// The count a big search sees when it starts, and the threads it takes then.
fn probe() -> (usize, usize) {
    let s = Search::start(&["-F", "nothing-matches-this"], Stdio::null());
    let first = s.next();
    s.finish();
    first
}

fn lock() -> std::sync::MutexGuard<'static, ()> {
    tree();
    ONE.lock().unwrap_or_else(|p| p.into_inner())
}

#[test]
fn a_stopped_search_is_counted_and_a_killed_one_is_not() {
    let _one = lock();
    let a = Search::start(&["-F", "nothing-matches-this"], Stdio::null());
    a.next();
    a.signal("-STOP");
    let mut ok = false;
    // another search of this user may start or end between the two probes: try again
    for _ in 0..5 {
        let (with, _) = probe();
        let b = Search::start(&["-F", "nothing-matches-this"], Stdio::null());
        b.next();
        b.signal("-STOP");
        let (more, _) = probe();
        b.signal("-KILL");
        b.finish();
        let (after, _) = probe();
        if more == with + 1 && after == with {
            ok = true;
            break;
        }
    }
    a.signal("-KILL");
    a.finish();
    assert!(ok, "a stopped search added 1 to the count and its kill took it back");
}

/// Waits until the process is stopped, as `ps` shows it.
fn stopped(pid: u32) -> bool {
    for _ in 0..200 {
        let out = Command::new("ps").args(["-o", "state=", "-p", &pid.to_string()]).output().unwrap();
        if String::from_utf8_lossy(&out.stdout).contains('T') {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn a_search_stopped_from_its_terminal_leaves_the_count_and_joins_again() {
    let _one = lock();
    let mut seen = Vec::new();
    // another search of this user may start or end between the probes: try again
    for _ in 0..5 {
        let (alone, _) = probe();
        let a = Search::start(&["-F", "nothing-matches-this"], Stdio::null());
        a.next();
        // what Ctrl-Z sends
        a.signal("-TSTP");
        assert!(stopped(a.child.id()), "SIGTSTP stops the search");
        let (while_stopped, _) = probe();
        a.signal("-CONT");
        // SIGSTOP cannot be caught: the search stays counted, so this shows that it joined again
        a.signal("-STOP");
        assert!(stopped(a.child.id()));
        let (again, _) = probe();
        a.signal("-KILL");
        a.finish();
        let (after, _) = probe();
        seen.push((alone, while_stopped, again, after));
        if while_stopped == alone && again == alone + 1 && after == alone {
            return;
        }
    }
    panic!("(alone, stopped with Ctrl-Z, continued, killed): {seen:?}");
}

#[test]
fn a_search_stopped_and_continued_prints_what_it_would_have() {
    let _one = lock();
    let run = |stop: bool| {
        let file = std::env::temp_dir().join(format!("scrutate_running_out_{}_{stop}", std::process::id()));
        let a = Search::start(&["-F", "needle"], Stdio::from(std::fs::File::create(&file).unwrap()));
        let mut stops = 0;
        if stop {
            a.next();
            while stops < 3 && a.try_signal("-TSTP") {
                assert!(stopped(a.child.id()));
                a.signal("-CONT");
                stops += 1;
            }
            assert!(stops > 0, "the search ended before it could be stopped");
        }
        let mut a = a;
        assert!(a.child.wait().unwrap().success());
        let mut lines: Vec<String> = std::fs::read_to_string(&file).unwrap().lines().map(str::to_owned).collect();
        let _ = std::fs::remove_file(&file);
        lines.sort();
        lines
    };
    let whole = run(false);
    assert_eq!(whole.len(), 8000);
    assert!(run(true) == whole, "the output of a search stopped three times differs");
}

#[test]
fn searches_started_together_share_and_take_back_the_threads() {
    let _one = lock();
    let (alone, full) = probe();
    let a = Search::start(&["-F", "nothing-matches-this"], Stdio::null());
    let first = a.next();
    // stopped right after it counted itself: the next one starts before it looks again
    a.signal("-STOP");
    let b = Search::start(&["-F", "nothing-matches-this"], Stdio::null());
    let (k, t) = b.next();
    assert!(k > first.0, "{k} searches seen after {first:?}");
    b.signal("-STOP");
    a.signal("-CONT");
    let seen = a.finish();
    assert!(seen.iter().any(|&(n, threads)| n > first.0 && threads < full), "the first search saw the second and took fewer threads: {seen:?}");
    b.signal("-CONT");
    let seen = b.finish();
    // with the first one ended, the second takes its threads back
    if k == alone + 1 && full > 1 {
        assert!(seen.iter().any(|&(n, threads)| n < k && threads > t), "{seen:?} after ({k}, {t})");
    }
}

#[test]
fn a_search_blocked_on_its_output_gives_way() {
    let _one = lock();
    let (alone, full) = probe();
    // nobody reads this pipe: the search fills it and waits
    let a = Search::start(&["-F", "needle"], Stdio::piped());
    a.next();
    let mut seen = Vec::new();
    for _ in 0..100 {
        let p = probe();
        seen.push(p);
        if p == (alone, full) {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut a = a;
    assert!(a.child.try_wait().unwrap().is_none(), "the blocked search went on");
    assert_eq!(seen.last(), Some(&(alone, full)), "probes while the first search waited on its reader: {seen:?}");
    a.child.kill().unwrap();
    a.finish();
}

#[test]
fn a_thread_count_given_or_a_small_search_is_not_counted() {
    let _one = lock();
    for args in [&["-j4", "-F", "nothing-matches-this", "."][..], &["-F", "needle", "d0/f0.txt"], &["-F", "needle", "d0"]] {
        let out = Command::new(BIN)
            .arg("grep")
            .args(args)
            .current_dir(tree())
            .env("SCRUTATE_GREP_DEBUG", "1")
            .env_remove("SCRUTATE_THREADS")
            .stdout(Stdio::null())
            .output()
            .unwrap();
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(!err.contains("searches running"), "{args:?}: {err}");
    }
}
