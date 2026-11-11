//! How many big searches this user runs at once, across processes, so that each takes its share
//! of threads. The count is a System V semaphore: a search adds 1 with `SEM_UNDO`, so the kernel
//! takes it back when the process dies, however it dies. Anything that fails (no System V IPC, a
//! set with that key made by another user, a limit) leaves the count unknown and the search runs
//! as it would alone. Only on macOS: on Linux the cores are the limit, and splitting them never
//! beat each search taking them all (py-big on 5 CPUs, 4 at once: 53 ms on 5 threads each, 60 ms
//! on 1, 53 ms on 2).

use std::io::Write;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

/// Threads for each of `running` big searches when one alone takes `alone`. On macOS the kernel
/// serves about four file-reading threads at full speed across all processes, so the threads are
/// split (py-big, 4 at once: 235 ms each on 5 threads, 190 ms on 1).
pub fn threads(alone: usize, running: usize) -> usize {
    (alone / running.max(1)).max(1)
}

/// This process's counted searches, and its writes to stdout in progress (while one runs, none of
/// its searches is counted: a reader that stopped reading must not slow the others).
struct State {
    held: usize,
    writing: usize,
}

static STATE: Mutex<State> = Mutex::new(State { held: 0, writing: 0 });
/// `State::held`, read without the lock on every write
static HELD: AtomicUsize = AtomicUsize::new(0);

fn state() -> std::sync::MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(|p| p.into_inner())
}

/// One big search, counted while this lives.
pub struct Counted(sys::Id);

impl Counted {
    pub fn start() -> Option<Counted> {
        let id = sys::id()?;
        let mut s = state();
        if s.writing == 0 && !sys::add(id, 1) {
            return None;
        }
        s.held += 1;
        HELD.fetch_add(1, Ordering::Relaxed);
        Some(Counted(id))
    }

    /// The big searches running now, this one included.
    pub fn running(&self) -> Option<usize> {
        sys::value(self.0).map(|n| n.max(1))
    }
}

impl Drop for Counted {
    fn drop(&mut self) {
        let mut s = state();
        s.held -= 1;
        HELD.fetch_sub(1, Ordering::Relaxed);
        if s.writing == 0 {
            sys::add(self.0, -1);
        }
    }
}

/// Stdout that takes this process's searches out of the count while a write to it may block.
pub struct GivesWay(pub std::io::Stdout);

impl GivesWay {
    fn around<T>(&mut self, f: impl FnOnce(&mut std::io::Stdout) -> T) -> T {
        if HELD.load(Ordering::Relaxed) == 0 || !sys::stdout_may_block() {
            return f(&mut self.0);
        }
        let id = sys::id();
        let step = |s: &mut State, into: bool| {
            if let (Some(id), true) = (id, s.held > 0) {
                sys::add(id, if into { s.held as i32 } else { -(s.held as i32) });
            }
        };
        {
            let mut s = state();
            s.writing += 1;
            if s.writing == 1 {
                step(&mut s, false);
            }
        }
        let r = f(&mut self.0);
        let mut s = state();
        s.writing -= 1;
        if s.writing == 0 {
            step(&mut s, true);
        }
        r
    }
}

impl Write for GivesWay {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.around(|o| o.write(buf))
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.around(|o| o.flush())
    }
}

#[cfg(target_os = "macos")]
mod sys {
    use std::ffi::c_int;

    pub type Id = c_int;

    // as `struct sembuf` in <sys/sem.h>
    #[repr(C)]
    struct Sembuf {
        num: u16,
        op: i16,
        flg: i16,
    }

    extern "C" {
        fn semget(key: c_int, nsems: c_int, flags: c_int) -> c_int;
        fn semop(id: c_int, ops: *mut Sembuf, n: usize) -> c_int;
        fn semctl(id: c_int, num: c_int, cmd: c_int, ...) -> c_int;
        fn getuid() -> u32;
    }

    const IPC_CREAT: c_int = 0o1000;
    const IPC_STAT: c_int = 2;
    const IPC_NOWAIT: i16 = 0o4000;
    const SEM_UNDO: i16 = 0o10000;
    const GETVAL: c_int = 5;
    /// Byte offset of `sem_perm.cuid` in `struct semid_ds`
    const CUID_AT: usize = 8;

    /// The set, made on first use: one semaphore, read and written by this user only.
    pub fn id() -> Option<Id> {
        static ID: std::sync::OnceLock<Option<Id>> = std::sync::OnceLock::new();
        *ID.get_or_init(|| {
            // SAFETY: no arguments
            let uid = unsafe { getuid() };
            // 0 is IPC_PRIVATE, a new set on every call
            let key = (0x5343_5254 ^ uid) as c_int;
            if key == 0 {
                return None;
            }
            // SAFETY: plain integers
            let id = unsafe { semget(key, 1, IPC_CREAT | 0o600) };
            if id < 0 {
                return None;
            }
            // a set another user made with this key and opened to others is not ours to count in
            let mut ds = [0u64; 64];
            // SAFETY: larger than `struct semid_ds` (72 bytes); IPC_STAT takes a pointer to it as
            // the fourth argument
            if unsafe { semctl(id, 0, IPC_STAT, ds.as_mut_ptr()) } != 0 {
                return None;
            }
            let bytes: &[u8] = unsafe { std::slice::from_raw_parts(ds.as_ptr().cast(), 16) };
            let cuid = u32::from_ne_bytes(bytes[CUID_AT..CUID_AT + 4].try_into().ok()?);
            (cuid == uid).then_some(id)
        })
    }

    /// Adds `n` to the count, undone by the kernel when the process ends; never waits.
    pub fn add(id: Id, n: i32) -> bool {
        let mut op = Sembuf { num: 0, op: n as i16, flg: SEM_UNDO | IPC_NOWAIT };
        // SAFETY: one operation, in a struct laid out as `struct sembuf`
        unsafe { semop(id, &mut op, 1) == 0 }
    }

    pub fn value(id: Id) -> Option<usize> {
        // SAFETY: GETVAL takes no fourth argument
        let v = unsafe { semctl(id, 0, GETVAL) };
        (v >= 0).then_some(v as usize)
    }

    /// Stdout is a pipe, a socket or a terminal, where a write waits for the reader; a file never
    /// makes it wait.
    pub fn stdout_may_block() -> bool {
        static MAY: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        *MAY.get_or_init(|| {
            use std::io::IsTerminal;
            use std::os::fd::FromRawFd;
            use std::os::unix::fs::FileTypeExt;
            // SAFETY: borrowed for one `fstat`, never closed
            let out = std::mem::ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(1) });
            out.metadata().is_ok_and(|m| m.file_type().is_fifo() || m.file_type().is_socket()) || std::io::stdout().is_terminal()
        })
    }
}

#[cfg(not(target_os = "macos"))]
mod sys {
    pub type Id = i32;
    pub fn id() -> Option<Id> {
        None
    }
    pub fn add(_: Id, _: i32) -> bool {
        false
    }
    pub fn value(_: Id) -> Option<usize> {
        None
    }
    pub fn stdout_may_block() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shares() {
        assert_eq!(threads(5, 1), 5);
        assert_eq!(threads(5, 2), 2);
        assert_eq!(threads(5, 3), 1);
        assert_eq!(threads(5, 4), 1);
        assert_eq!(threads(5, 9), 1);
        assert_eq!(threads(12, 4), 3);
        assert_eq!(threads(5, 0), 5, "an unknown count is one search");
    }
}
