//! The pseudo-terminal a tty job runs on.
//!
//! A job's input is otherwise closed, so a program that asks a question
//! reads the end of its input and gives up, and one that checks for a
//! terminal, such as a REPL, a debugger or a setup wizard, will not start
//! at all. On a pseudo-terminal it runs as at a keyboard: send_job types
//! into the side Letronna holds, and what the program prints comes back
//! the same way, read as any job's output is.
//!
//! Stopping such a job ends every process in the session its shell leads,
//! not only the shell's group, since a shell at a terminal puts each
//! command typed into it in a group of its own.
//!
//! Windows has no such job yet: its pseudo-console needs code of its own.

use std::fs::File;
use std::sync::mpsc::Sender;

/// Why a tty job cannot start on this system.
#[cfg(windows)]
pub(super) const UNAVAILABLE: &str =
    "tty is not available on Windows yet; use flags or piped input.";

/// The terminal's size: wide enough that a long line or a table is not
/// wrapped, and the 50 rows a full-screen prompt lays itself out in.
#[cfg(unix)]
static SIZE: libc::winsize = libc::winsize {
    ws_row: 50,
    ws_col: 200,
    ws_xpixel: 0,
    ws_ypixel: 0,
};

/// Open a pseudo-terminal, set `shell` to start on it as the leader of a
/// session of its own whose controlling terminal it is, so that Ctrl-C
/// typed into it reaches the program and a program that opens /dev/tty
/// finds it, and return Letronna's side of it.
#[cfg(unix)]
pub(super) fn attach(shell: &mut std::process::Command) -> Result<File, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::process::CommandExt;
    let failed = |e: std::io::Error| format!("Cannot open a terminal for it: {e}");
    let last = || failed(std::io::Error::last_os_error());
    // O_NOCTTY on both sides: where Letronna leads a session with no
    // terminal, as an app the system starts does, opening one without it
    // can make the terminal Letronna's own, and openpty does not say that
    // it passes it. The main side is opened as posix_openpt opens it, but
    // with close-on-exec from the start, as std opens every file: left open
    // in a command another thread starts, it would keep the terminal alive
    // after the job.
    let master = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open("/dev/ptmx")
        .map_err(failed)?;
    // SAFETY: both take the open descriptor of a terminal's main side.
    if unsafe { libc::grantpt(master.as_raw_fd()) } == -1
        || unsafe { libc::unlockpt(master.as_raw_fd()) } == -1
    {
        return Err(last());
    }
    // Opened with close-on-exec, as std opens every file.
    let slave = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NOCTTY)
        .open(other_side(&master)?)
        .map_err(failed)?;
    let size = std::ptr::from_ref(&SIZE);
    // SAFETY: the descriptor is open, and the size is read during the call.
    if unsafe { libc::ioctl(slave.as_raw_fd(), libc::TIOCSWINSZ as _, size) } == -1 {
        return Err(last());
    }
    shell
        .stdin(slave.try_clone().map_err(failed)?)
        .stdout(slave.try_clone().map_err(failed)?)
        .stderr(slave);
    // SAFETY: setsid and ioctl are safe to call between fork and exec. The
    // shell leads its own session, so its process group is still its id,
    // as stopping the job needs.
    unsafe {
        shell.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    Ok(master)
}

/// Where the other side of the terminal `master` opens.
#[cfg(unix)]
fn other_side(master: &File) -> Result<std::path::PathBuf, String> {
    use std::os::fd::AsRawFd;
    use std::os::unix::ffi::OsStrExt;
    // ptsname answers in one buffer for the whole process, so its callers
    // take turns until the name is copied out.
    static TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _turn = TURN.lock().unwrap_or_else(|e| e.into_inner());
    // SAFETY: the descriptor is a terminal's main side, open for the call.
    let name = unsafe { libc::ptsname(master.as_raw_fd()) };
    if name.is_null() {
        let e = std::io::Error::last_os_error();
        return Err(format!("Cannot open a terminal for it: {e}"));
    }
    // SAFETY: ptsname gave a string ending in a nul, unchanged until the
    // next call, which waits for the turn.
    let name = unsafe { std::ffi::CStr::from_ptr(name) };
    Ok(std::ffi::OsStr::from_bytes(name.to_bytes()).into())
}

/// Windows has no tty job yet.
#[cfg(windows)]
pub(super) fn attach(_: &mut std::process::Command) -> Result<File, String> {
    Err(UNAVAILABLE.into())
}

/// Times the processes of a session are looked for: one killed can have
/// started another just before, and that one is found the next time.
#[cfg(unix)]
const ROUNDS: usize = 3;

/// End every process but `leader` in the session it leads, as the shell of
/// a tty job does. A shell at a terminal gives each command typed into it a
/// group of its own, so one left running with `&` outlives a kill of the
/// shell's group, and killed, the shell can pass nothing on.
#[cfg(unix)]
pub(super) fn end_session(leader: libc::pid_t) {
    for _ in 0..ROUNDS {
        let mut found = false;
        for pid in processes() {
            // SAFETY: getsid only asks, and kill only sends a signal. A
            // session's id is not given to a new process while the session
            // has members, so it names no other.
            if pid > 0 && pid != leader && unsafe { libc::getsid(pid) } == leader {
                unsafe { libc::kill(pid, libc::SIGKILL) };
                found = true;
            }
        }
        if !found {
            return;
        }
    }
}

/// The id of every process this user can see.
#[cfg(target_os = "macos")]
fn processes() -> Vec<libc::pid_t> {
    // SAFETY: with no buffer it only counts them.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    // Room for processes started between the two calls.
    let mut pids: Vec<libc::pid_t> = vec![0; count + 64];
    let room = i32::try_from(std::mem::size_of_val(pids.as_slice())).unwrap_or(i32::MAX);
    // SAFETY: the buffer holds `room` bytes.
    let found = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), room) };
    pids.truncate(usize::try_from(found).unwrap_or(0).min(pids.len()));
    pids
}

/// The id of every process, as /proc lists them.
#[cfg(any(target_os = "linux", target_os = "android"))]
fn processes() -> Vec<libc::pid_t> {
    let Ok(entries) = std::fs::read_dir("/proc") else {
        return Vec::new();
    };
    entries
        .flatten()
        .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
        .collect()
}

/// Elsewhere none are found, and the shell's group alone is killed.
#[cfg(all(
    unix,
    not(any(target_os = "macos", target_os = "linux", target_os = "android"))
))]
fn processes() -> Vec<libc::pid_t> {
    Vec::new()
}

/// Whether the program on the terminal `master` is reading a line without
/// showing it, as a password prompt does: a REPL or a menu that reads key
/// by key turns echo off too, but not line by line.
#[cfg(unix)]
pub(super) fn hides_typing(master: &File) -> bool {
    use std::os::fd::AsRawFd;
    // SAFETY: all zeroes is a valid value of this plain C struct.
    let mut modes: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is open, and `modes` is the struct to fill.
    if unsafe { libc::tcgetattr(master.as_raw_fd(), &mut modes) } != 0 {
        return false;
    }
    modes.c_lflag & libc::ECHO == 0 && modes.c_lflag & libc::ICANON != 0
}

/// Windows has no tty job, so nothing reads a password from one.
#[cfg(windows)]
pub(super) fn hides_typing(_: &File) -> bool {
    false
}

/// The questions a program asks its terminal that one with no screen
/// still answers, as a program that gets no answer waits without a sound:
/// where the cursor is, said as the top left, and whether all is well.
const QUERIES: [(&[u8], &[u8]); 2] = [(b"\x1b[6n", b"\x1b[1;1R"), (b"\x1b[5n", b"\x1b[0n")];

/// The longest query but one byte: what a read can end with that the next
/// one finishes.
const CARRIED: usize = 3;

/// Answers the queries in a tty job's output as a terminal would.
pub(super) struct Answers {
    /// The end of the last read, which could be a query's start.
    carry: Vec<u8>,
    /// Where the answers go to be written on a thread of their own: on the
    /// one reading the output, an answer the program never reads would stop
    /// that reading, and the program with it once its output fills up.
    to: Sender<&'static [u8]>,
}

impl Answers {
    /// Answer the queries on the terminal `master`.
    pub(super) fn on(master: File) -> Self {
        let (to, answers) = std::sync::mpsc::channel::<&'static [u8]>();
        std::thread::spawn(move || {
            use std::io::Write;
            // Ends once the output is all read and the sender dropped.
            for answer in answers {
                let _ = (&master).write_all(answer);
            }
        });
        Answers {
            carry: Vec::new(),
            to,
        }
    }

    /// Answer each query `bytes` finishes.
    pub(super) fn heard(&mut self, bytes: &[u8]) {
        let mut seen = std::mem::take(&mut self.carry);
        seen.extend_from_slice(bytes);
        for (query, answer) in QUERIES {
            for _ in seen.windows(query.len()).filter(|at| at == &query) {
                let _ = self.to.send(answer);
            }
        }
        // Shorter than any query, so none is answered twice.
        self.carry = seen[seen.len().saturating_sub(CARRIED)..].to_vec();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_cut_across_two_reads_is_answered_once() {
        let (to, answered) = std::sync::mpsc::channel();
        let mut answers = Answers {
            carry: Vec::new(),
            to,
        };
        answers.heard(b"ready\x1b[");
        answers.heard(b"6n and \x1b[5n");
        answers.heard(b"n");
        drop(answers);
        assert_eq!(
            answered.iter().collect::<Vec<_>>().concat(),
            b"\x1b[1;1R\x1b[0n"
        );
    }
}
