//! The terminal finishing, putting back and keeping updates, run as the real
//! binary with every folder it keeps things in pointed at a scratch one.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const LETRONNA: &str = env!("CARGO_BIN_EXE_letronna");

/// Held by each test: a binary copied by one test while another starts a
/// process can be held open for writing by that process, and Linux then
/// refuses to run it.
static ONE_AT_A_TIME: Mutex<()> = Mutex::new(());

/// An empty folder for one test, with the data folder inside it.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("letronna-cli-update-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("data")).unwrap();
    std::fs::create_dir_all(dir.join("bin")).unwrap();
    dir
}

/// `program` with `args` in `dir`, with nowhere to reach outside it.
fn command(program: &Path, dir: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(dir)
        .env("AGENT_DATA_DIR", dir.join("data"))
        .env("LOCALAPPDATA", dir)
        .env("HOME", dir)
        .env("USERPROFILE", dir)
        // Exported for the live tests, these would send a -p run to the
        // server they name rather than the stub.
        .env_remove("AGENT_BASE_URL")
        .env_remove("AGENT_API_KEY")
        .env_remove("AGENT_MODEL")
        // A debug build leaves updating alone unless told otherwise.
        .env("LETRONNA_UPDATES", "1")
        .stdin(Stdio::null());
    command
}

/// Runs `program` with `args` in `dir`, with nowhere to reach outside it.
fn run(program: &Path, dir: &Path, args: &[&str]) -> Output {
    command(program, dir, args).output().unwrap()
}

/// Runs it as [`run`] does, but stops it and gives nothing back when it is
/// still going after `limit`.
fn run_within(program: &Path, dir: &Path, args: &[&str], limit: Duration) -> Option<Output> {
    let mut child = command(program, dir, args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + limit;
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Some(child.wait_with_output().unwrap())
}

fn state_path(dir: &Path) -> PathBuf {
    dir.join("data").join("update.json")
}

fn write_state(dir: &Path, state: Value) {
    std::fs::write(state_path(dir), state.to_string()).unwrap();
}

fn read_state(dir: &Path) -> Value {
    serde_json::from_slice(&std::fs::read(state_path(dir)).unwrap()).unwrap()
}

/// A copy of the terminal where an install would put it.
fn installed(dir: &Path) -> PathBuf {
    let dest = dir.join("bin").join(if cfg!(windows) {
        "letronnacli.exe"
    } else {
        "letronna"
    });
    std::fs::copy(LETRONNA, &dest).unwrap();
    dest
}

/// `dest` with `tag` before its extension, as the updater names what it keeps.
fn beside(dest: &Path, tag: &str) -> PathBuf {
    let stem = dest.file_stem().unwrap().to_string_lossy();
    match dest.extension() {
        Some(ext) => dest.with_file_name(format!("{stem}.{tag}.{}", ext.to_string_lossy())),
        None => dest.with_file_name(format!("{stem}.{tag}")),
    }
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap();
    ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A model server on this computer answering every request with `status` and
/// `body`, and the base URL to set up for it.
fn model(status: &str, body: &str) -> String {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", server.local_addr().unwrap());
    let answer = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    std::thread::spawn(move || {
        for socket in server.incoming() {
            let Ok(mut socket) = socket else {
                continue;
            };
            // Read whole before answering, so closing does not reset it.
            let mut bytes = Vec::new();
            let mut block = [0; 65536];
            while let Ok(n) = socket.read(&mut block)
                && n > 0
            {
                bytes.extend_from_slice(&block[..n]);
                let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .and_then(|l| l.trim().parse().ok())
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
            let _ = socket.write_all(answer.as_bytes());
        }
    });
    base
}

/// Sets `dir` up to talk to the model server at `base`.
fn set_up(dir: &Path, base: &str) {
    let settings = json!({
        "endpoint": base,
        "model": "test-model",
        "gateway": "Custom",
        "custom_endpoint": base,
    });
    std::fs::write(dir.join("data").join("settings.json"), settings.to_string()).unwrap();
}

/// An update to this version, installed and started `starts` times.
fn pending(dest: &Path, starts: u32) -> Value {
    json!({
        "pending": {"from": "0.0.1", "to": VERSION, "starts": starts, "files": [dest]},
    })
}

#[test]
fn help_and_version_leave_the_update_state_as_it_was() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("help-version");
    let dest = dir.join("bin").join("letronna");
    // A start of the updated version would be counted, and this one would
    // be its last before it was put back.
    write_state(&dir, pending(&dest, 2));
    let before = std::fs::read(state_path(&dir)).unwrap();
    for flag in ["--help", "-h", "--version", "-V"] {
        let out = run(Path::new(LETRONNA), &dir, &[flag]);
        assert!(out.status.success(), "{flag}");
        assert_eq!(std::fs::read(state_path(&dir)).unwrap(), before, "{flag}");
    }
    assert!(!dir.join("data").join("update.lock").exists());
}

// Only where a stand-in window can be put first: elsewhere a real one opens,
// and the run waits on it.
#[cfg(unix)]
#[test]
fn opening_the_window_leaves_its_start_to_the_window() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("gui");
    let dest = installed(&dir);
    // A window that starts and leaves, where the launcher looks first, so
    // no real one opens.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let window = dir.join("bin").join("letronna-gui");
        std::fs::write(&window, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&window, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // Counted here as well as by the window, this start would be the third.
    write_state(&dir, pending(&dest, 2));
    let before = std::fs::read(state_path(&dir)).unwrap();
    let out = run(&dest, &dir, &["gui"]);
    assert!(out.status.success());
    assert_eq!(std::fs::read(state_path(&dir)).unwrap(), before);
}

#[test]
fn a_downloaded_update_is_put_in_place_and_started_instead() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("finish");
    let dest = installed(&dir);
    let new = beside(&dest, "new");
    std::fs::copy(LETRONNA, &new).unwrap();
    write_state(
        &dir,
        json!({
            "staged": {"version": VERSION, "files": [{"dest": dest, "sha256": sha256(&new)}]},
        }),
    );
    // Nothing is set up, so the question is refused once it gets that far.
    let out = run(&dest, &dir, &["-p", "hello"]);
    assert_eq!(out.status.code(), Some(1));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("Nothing is set up yet"), "{said}");
    assert!(!new.exists());
    assert!(beside(&dest, "old").is_file());
    let state = read_state(&dir);
    assert!(state["staged"].is_null());
    assert_eq!(state["pending"]["to"], VERSION);
    // Counted by the one started in its place, not by the run that swapped.
    assert_eq!(state["pending"]["starts"], 1);
}

#[test]
fn a_version_that_never_settled_is_put_back_and_started_instead() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("roll-back");
    let dest = installed(&dir);
    let old = beside(&dest, "old");
    // A version that says what it was asked, so that starting it can be told
    // from this one carrying on. Elsewhere, told apart from the version it
    // stands behind only by what follows the program.
    #[cfg(unix)]
    let kept = b"#!/bin/sh\necho \"the old version, asked $*\"\n".to_vec();
    #[cfg(not(unix))]
    let kept = [std::fs::read(LETRONNA).unwrap(), b"old".to_vec()].concat();
    std::fs::write(&old, &kept).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&old, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    write_state(&dir, pending(&dest, 2));
    let out = run(&dest, &dir, &["-p", "hello"]);
    let said = String::from_utf8_lossy(&out.stdout);
    if cfg!(unix) {
        assert!(out.status.success(), "{said}");
        assert_eq!(said.trim(), "the old version, asked -p hello");
    } else {
        assert_eq!(out.status.code(), Some(1));
        assert!(said.contains("Nothing is set up yet"), "{said}");
    }
    assert_eq!(std::fs::read(&dest).unwrap(), kept);
    assert!(beside(&dest, "failed").is_file());
    assert!(!old.exists());
    let state = read_state(&dir);
    assert!(state["pending"].is_null());
    assert_eq!(state["skip"], VERSION);
}

#[test]
fn a_version_given_up_on_that_was_not_put_back_carries_on() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("nothing-put-back");
    // Kept nowhere, so nothing is put back, and the state cannot be saved,
    // so a copy started again would find it just as this one did.
    let dest = dir.join("bin").join("letronna");
    write_state(&dir, pending(&dest, 2));
    std::fs::create_dir(dir.join("data").join("update.tmp")).unwrap();
    let Some(out) = run_within(
        Path::new(LETRONNA),
        &dir,
        &["-p", "hello"],
        Duration::from_secs(30),
    ) else {
        panic!("it kept starting itself again");
    };
    assert_eq!(out.status.code(), Some(1));
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(said.contains("Nothing is set up yet"), "{said}");
}

#[test]
fn a_question_that_is_answered_keeps_the_update() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("answered");
    let dest = dir.join("bin").join("letronna");
    std::fs::write(beside(&dest, "failed"), "the version put back before").unwrap();
    set_up(
        &dir,
        &model(
            "200 OK",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n",
        ),
    );
    write_state(&dir, pending(&dest, 0));
    let out = run(Path::new(LETRONNA), &dir, &["-p", "hello"]);
    let said = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "{said}");
    assert!(said.contains("Hello"), "{said}");
    assert!(read_state(&dir)["pending"].is_null());
    assert!(!beside(&dest, "failed").exists());
}

#[test]
fn a_question_that_fails_does_not_keep_the_update() {
    let _one = ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner());
    let dir = scratch("failed");
    let dest = dir.join("bin").join("letronna");
    set_up(
        &dir,
        &model(
            "400 Bad Request",
            "{\"error\":{\"message\":\"There is no model called test-model.\"}}",
        ),
    );
    write_state(&dir, pending(&dest, 0));
    let out = run(Path::new(LETRONNA), &dir, &["-p", "hello"]);
    assert_eq!(out.status.code(), Some(1));
    let pending = &read_state(&dir)["pending"];
    assert_eq!(pending["to"], VERSION);
    assert_eq!(pending["starts"], 1);
}
