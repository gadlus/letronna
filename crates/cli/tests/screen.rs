//! The terminal's own screen, drawn in a pseudo-terminal: what it writes
//! again when the terminal it draws in may have lost what it showed.
//!
//! A frame writes only the rows unlike the last one, so a terminal that drops
//! the screen -- as Apple's Terminal can for a tab while another is in
//! front -- would get back only the rows that change next. The data folder
//! is a scratch one set up for a Custom provider that is never asked, with
//! updates off and the browser swapped for `false`, so nothing here reaches
//! the network or a sign-in page. Pseudo-terminals are Unix's, so these
//! tests are too.

#![cfg(unix)]

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

/// Far longer than reaching the prompt takes, so only a hang fails a test.
const PATIENCE: Duration = Duration::from_secs(60);
/// No output for this long counts as the screen being drawn.
const QUIET: Duration = Duration::from_millis(500);

/// A scratch data folder for test `name` that goes straight to the prompt.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("letronna-screen-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("project")).unwrap();
    // Port 9 discards: nothing here asks the model, and nothing could answer.
    let settings = serde_json::json!({
        "endpoint": "http://127.0.0.1:9/v1",
        "custom_endpoint": "http://127.0.0.1:9/v1",
        "gateway": "Custom",
        "model": "stub-model",
        "manual_updates": true,
    });
    std::fs::write(dir.join("settings.json"), settings.to_string()).unwrap();
    std::fs::write(dir.join("keys.json"), r#"{"Custom":"stub"}"#).unwrap();
    dir
}

/// A terminal running in a pseudo-terminal, and everything it has drawn.
struct Running {
    child: Child,
    keys: std::fs::File,
    drawn: Arc<Mutex<Vec<u8>>>,
}

impl Running {
    /// Starts the terminal on `dir` and waits for its prompt to be drawn.
    fn start(dir: &Path) -> Running {
        let (mut master, mut slave) = (0, 0);
        let mut size = libc::winsize {
            ws_row: 30,
            ws_col: 100,
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
                // Linux takes the size as `*const`, macOS as `*mut`.
                &raw mut size,
            )
        };
        assert_eq!(opened, 0, "{}", std::io::Error::last_os_error());
        // SAFETY: openpty has just opened both, and nothing else holds them.
        let (master, slave) =
            unsafe { (OwnedFd::from_raw_fd(master), OwnedFd::from_raw_fd(slave)) };
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
            .stdin(Stdio::from(slave.try_clone().unwrap()))
            .stdout(Stdio::from(slave))
            .stderr(Stdio::piped());
        // SAFETY: between fork and exec it only makes system calls. A session
        // of its own makes the pseudo-terminal its controlling one, and puts it
        // and anything it starts in a group the test can end whole.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let mut child = command.spawn().expect("the terminal would not start");
        drop(command);
        let keys = std::fs::File::from(master);
        let mut screen = keys.try_clone().unwrap();
        let drawn = Arc::new(Mutex::new(Vec::new()));
        let into = drawn.clone();
        std::thread::spawn(move || {
            let mut block = [0; 8192];
            while let Ok(n) = screen.read(&mut block) {
                if n == 0 {
                    return;
                }
                into.lock().unwrap().extend_from_slice(&block[..n]);
            }
        });
        let (marks, heard) = mpsc::channel();
        let stderr = child.stderr.take().unwrap();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if marks.send(line).is_err() {
                    return;
                }
            }
        });
        let mut running = Running { child, keys, drawn };
        loop {
            let Ok(line) = heard.recv_timeout(PATIENCE) else {
                running.stop();
                panic!("the terminal did not reach its prompt in {PATIENCE:?}");
            };
            if line.starts_with("startup ready ") {
                break;
            }
        }
        running.settled();
        running
    }

    /// Waits for it to stop drawing, and gives back what it drew meanwhile.
    fn settled(&self) -> String {
        let from = self.drawn.lock().unwrap().len();
        let began = Instant::now();
        let mut seen = from;
        let mut still = Instant::now();
        while still.elapsed() < QUIET && began.elapsed() < PATIENCE {
            std::thread::sleep(Duration::from_millis(50));
            let now = self.drawn.lock().unwrap().len();
            if now != seen {
                seen = now;
                still = Instant::now();
            }
        }
        String::from_utf8_lossy(&self.drawn.lock().unwrap()[from..]).into_owned()
    }

    /// Sends what the terminal would on a key or an event.
    fn send(&mut self, bytes: &[u8]) {
        self.keys.write_all(bytes).unwrap();
        self.keys.flush().unwrap();
    }

    /// Ends the terminal and whatever it started.
    fn stop(&mut self) {
        let group = libc::pid_t::try_from(self.child.id()).unwrap();
        // SAFETY: a signal to the group this test made for the terminal.
        unsafe { libc::kill(-group, libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop();
    }
}

#[test]
fn coming_back_into_focus_draws_every_row_again() {
    let dir = scratch("focus");
    let mut running = Running::start(&dir);
    // Nothing has changed, so leaving focus draws nothing again.
    running.send(b"\x1b[O");
    assert!(!running.settled().contains("\x1b[1;1H"));
    // Back in focus, the top row is written again with the rest, so a screen
    // the terminal dropped meanwhile comes back whole.
    running.send(b"\x1b[I");
    let drawn = running.settled();
    assert!(drawn.contains("\x1b[1;1H"), "nothing was drawn again");
    assert!(drawn.contains("\x1b[30;1H"), "the foot was not drawn again");
}
