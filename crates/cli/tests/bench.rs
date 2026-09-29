//! The terminal's start and memory, held to `bench/baseline.json`.
//!
//! The core's bench (`crates/core/src/bench.rs`) times what sessions cost;
//! this one times what a person waits for: from starting `letronna` to its
//! prompt being up, with 500 sessions on disk, and the memory it holds then.
//! It runs the binary Cargo built for it in a pseudo-terminal of its own,
//! since the terminal only draws in one, and waits for the "ready" mark that
//! `LETRONNA_PROFILE_STARTUP` has it print. The data folder is a scratch one
//! set up for a Custom provider that is never asked, with updates off and
//! the browser swapped for `false`, so a start reaches neither the network
//! nor a sign-in page. Pseudo-terminals are Unix's, so the bench is too.

#![cfg(unix)]

#[path = "../../core/src/bench/baseline.rs"]
mod baseline;

use baseline::{check, median};
use letronna_core::chat::Message;
use letronna_core::store::{self, Conversation, Settings, Store};
use std::io::{BufRead, BufReader, Read};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// Sessions on disk when the terminal starts: a few months of use.
const SESSIONS: u64 = 500;
/// How long a start may take before the bench gives up on it: a hundred
/// times what one takes, so only a start stuck on something fails here.
const PATIENCE: Duration = Duration::from_secs(30);
/// Bytes in the megabyte the memory is given in.
const MB: f64 = 1024.0 * 1024.0;

/// A scratch data folder holding `SESSIONS` short sessions and settings
/// that go straight to the prompt.
fn scratch() -> PathBuf {
    let dir = std::env::temp_dir().join("letronna-bench-terminal");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("project")).expect("the bench's folder");
    // The only test in this binary that reads the variable.
    unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
    // Port 9 discards: nothing here asks the model, and nothing could answer.
    let endpoint = "http://127.0.0.1:9/v1";
    let mut store = Store {
        settings: Settings {
            gateway: "Custom".into(),
            endpoint: endpoint.into(),
            custom_endpoint: endpoint.into(),
            model: "bench-model".into(),
            manual_updates: true,
            ..Default::default()
        },
        conversations: (1..=SESSIONS)
            .map(|id| Conversation {
                id,
                title: format!("Session {id}"),
                updated: 1_700_000_000 + id,
                messages: (0..20).map(said).collect(),
                ..Default::default()
            })
            .collect(),
        ..Default::default()
    };
    store.save().expect("the bench's sessions");
    store::save_key("Custom", Some("bench")).expect("the bench's key");
    dir
}

/// Message `i` of a short session: questions and answers in turn.
fn said(i: usize) -> Message {
    let (role, content) = if i.is_multiple_of(2) {
        ("user", format!("Why does step {i} of the build fail?"))
    } else {
        (
            "assistant",
            format!("Step {i} runs out of room; split it in two."),
        )
    };
    Message {
        role: role.into(),
        content,
        ..Default::default()
    }
}

/// Starts the terminal on `dir` in a pseudo-terminal of its own, its marks
/// coming back line by line from its stderr.
fn start(dir: &Path) -> (Child, mpsc::Receiver<String>) {
    let (mut master, mut slave) = (0, 0);
    let mut size = libc::winsize {
        ws_row: 40,
        ws_col: 120,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: both descriptors and the size are this frame's own, and the
    // name and settings may be left out.
    let opened = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            &mut size,
        )
    };
    assert_eq!(
        opened,
        0,
        "no pseudo-terminal: {}",
        std::io::Error::last_os_error()
    );
    // SAFETY: openpty has just opened both, and nothing else holds them.
    let (master, slave) = unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
    let mut command = Command::new(env!("CARGO_BIN_EXE_letronna"));
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", dir)
        .env("TERM", "xterm-256color")
        .env("AGENT_DATA_DIR", dir)
        .env("BROWSER", "/usr/bin/false")
        .env(letronna_core::diag::PROFILE_STARTUP, "1")
        .current_dir(dir.join("project"))
        .stdin(Stdio::from(
            slave.try_clone().expect("the terminal's input"),
        ))
        .stdout(Stdio::from(slave))
        .stderr(Stdio::piped());
    // SAFETY: between fork and exec it only makes system calls. A session of
    // its own makes the pseudo-terminal its controlling one, and puts it and
    // anything it starts in a group the bench can end whole.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().expect("the terminal would not start");
    // Letting go of the command's copies of the terminal's end, so the
    // screen below ends when the terminal does.
    drop(command);
    // Read and thrown away, so a full screen never holds the terminal up.
    let mut screen = std::fs::File::from(master);
    std::thread::spawn(move || {
        let mut block = [0; 8192];
        while screen.read(&mut block).is_ok_and(|n| n > 0) {}
    });
    let (marks, heard) = mpsc::channel();
    let stderr = child.stderr.take().expect("the terminal's stderr");
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if marks.send(line).is_err() {
                return;
            }
        }
    });
    (child, heard)
}

/// Ends the terminal and whatever it started.
fn stop(child: &mut Child) {
    let group = libc::pid_t::try_from(child.id()).expect("a process id");
    // SAFETY: a signal to the group this bench made for the terminal.
    unsafe { libc::kill(-group, libc::SIGKILL) };
    let _ = child.wait();
}

/// Starts the terminal once and waits at its prompt: how long that took, in
/// milliseconds, and how much memory it held there, in megabytes.
fn started(dir: &Path) -> (f64, f64) {
    let began = Instant::now();
    let (mut child, marks) = start(dir);
    loop {
        let Ok(line) = marks.recv_timeout(PATIENCE) else {
            stop(&mut child);
            panic!("the terminal did not reach its prompt in {PATIENCE:?}");
        };
        if line.starts_with("startup ready ") {
            break;
        }
    }
    let taken = began.elapsed().as_secs_f64() * 1000.0;
    let held = memory(child.id());
    stop(&mut child);
    (taken, held.expect("could not read the terminal's memory"))
}

/// How much of process `pid` is in RAM, in megabytes, as the system keeps it.
#[cfg(target_os = "macos")]
fn memory(pid: u32) -> Option<f64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = libc::c_int::try_from(size_of::<libc::proc_taskinfo>()).ok()?;
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: the buffer is as large as the flavor's struct and says so, and
    // it is read only once the call reports filling all of it.
    let filled = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if filled != size {
        return None;
    }
    // SAFETY: filled in full, as just checked.
    Some(unsafe { info.assume_init() }.pti_resident_size as f64 / MB)
}

/// How much of process `pid` is in RAM, in megabytes, as /proc keeps it.
#[cfg(not(target_os = "macos"))]
fn memory(pid: u32) -> Option<f64> {
    let statm = std::fs::read_to_string(format!("/proc/{pid}/statm")).ok()?;
    // Sizes in pages: the whole program, then what of it is resident.
    let pages: f64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: sysconf only reads a system setting.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    Some(pages * page as f64 / MB)
}

#[test]
#[ignore = "bench, some seconds: cargo test -p letronna-cli --release --locked --test bench -- --ignored --nocapture"]
fn the_terminal_keeps_to_its_baseline() {
    let dir = scratch();
    // Not counted: the first start writes the listing's index, and the
    // system may look over a program it has not run before.
    started(&dir);
    let mut held = Vec::new();
    let start = median(|| {
        let (taken, memory) = started(&dir);
        held.push(memory);
        taken
    });
    // The memory those same starts held, its median taken the same way.
    let mut held = held.into_iter();
    let memory = median(|| held.next().unwrap_or_default());
    let _ = std::fs::remove_dir_all(&dir);
    let measured = [("terminal_start_ms", start), ("terminal_rss_mb", memory)];
    if let Err(slower) = check(&measured) {
        panic!("{slower}");
    }
}
