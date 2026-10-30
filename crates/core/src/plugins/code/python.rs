//! Python plugins: one process each, run by the Python already on this
//! computer, which hears the same messages a JavaScript plugin does as lines
//! of JSON on its stdin and answers on its stdout.
//!
//! Nothing is installed for it: `runner.py` and the `letronna` module are
//! written to the data folder from what this binary carries, and the runner
//! keeps the real stdout to itself, so a `print` in a plugin goes to its log
//! rather than into the messages. The process starts with only the
//! environment an MCP server started with `env_scrub` gets, in a process
//! group or job of its own, so stopping it stops whatever it started, and it
//! exits by itself when Letronna goes, as its stdin closes.
//!
//! A Python plugin is a program with the user's own access to the computer;
//! nothing here can hold it to less, which is what the user is told before
//! allowing one.

use super::running::{Host, Loaded};
use crate::mcp::Process;
use serde_json::{Value, json};
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::SyncSender;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

/// The runner and the module plugins import, as this binary carries them.
const RUNNER: &str = include_str!("runner.py");
const MODULE: &str = include_str!("letronna.py");
/// Bumped when the messages change, so an older copy is never used.
const SDK: &str = "1";
/// The oldest Python a plugin runs on.
const OLDEST: (u32, u32) = (3, 9);
/// How long asking a Python its version may take, and how long a failed
/// search is trusted before Python is looked for again.
const PROBE_TIME: Duration = Duration::from_secs(5);
const NOT_FOUND_FOR: Duration = Duration::from_secs(60);
/// The lines of what a plugin wrote to stderr kept to say why it ended.
const LAST_WORDS: usize = 5;

/// A Python found on this computer: the program, and what goes before the
/// runner on its command line.
#[derive(Clone, Debug, PartialEq)]
struct Python {
    program: String,
    args: Vec<String>,
}

/// A running Python plugin, as `Loaded` holds it.
struct Running {
    process: Mutex<Option<Process>>,
    queue: SyncSender<Value>,
    generation: u64,
    stopped: Arc<AtomicBool>,
}

impl Host for Running {
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        let process = self
            .process
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(process) = process {
            process.end();
        }
        // Wakes the thread that writes to it, which then lets go of the
        // queue for whatever host comes next.
        let stop = json!({"type": "stop", "generation": self.generation});
        if self.queue.try_send(stop.clone()).is_err() {
            let queue = self.queue.clone();
            std::thread::spawn(move || queue.send(stop));
        }
    }
}

/// Finds Python and writes the runner, which a start needs and which may
/// take a while the first time.
pub(super) fn prepare(plugin: &Loaded) -> Result<(), String> {
    found()?;
    sdk_folder(plugin).map(drop)
}

/// Starts `plugin`'s process, as its host `generation`.
pub(super) fn start(plugin: Arc<Loaded>, generation: u64) -> Result<Box<dyn Host>, String> {
    let python = found()?;
    let sdk = sdk_folder(&plugin)?;
    let runner = sdk.join("runner.py");
    let mut args = python.args.clone();
    args.extend([
        "-u".to_owned(),
        runner.to_string_lossy().into_owned(),
        plugin.folder.to_string_lossy().into_owned(),
        plugin.code.main.clone(),
    ]);
    let config: crate::mcp::ServerConfig = serde_json::from_value(json!({
        "command": python.program,
        "args": args,
        "cwd": ".",
        "env_scrub": true,
        "env": {
            // Its folder is hashed for what the user allowed; a cache
            // written there would read as new code.
            "PYTHONDONTWRITEBYTECODE": "1",
            "PYTHONUTF8": "1",
            "PYTHONIOENCODING": "utf-8",
            "LETRONNA_PLUGIN": plugin.name,
            "LETRONNA_PLUGIN_DATA": plugin.own,
        },
    }))
    .map_err(|e| format!("its command could not be made: {e}"))?;
    let mut child = crate::mcp::spawn(&config, &plugin.folder)?;
    let tree = crate::jobs::Tree::of(&child)?;
    let (Some(stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        Process { child, tree }.end();
        return Err("its pipes could not be opened.".into());
    };
    let stopped = Arc::new(AtomicBool::new(false));
    let running = Running {
        process: Mutex::new(Some(Process { child, tree })),
        queue: plugin.sender(),
        generation,
        stopped: stopped.clone(),
    };
    let last_words = Arc::new(Mutex::new(Vec::<String>::new()));
    spawn(&plugin, "write", {
        let plugin = plugin.clone();
        move || write(&plugin, stdin, generation)
    })?;
    spawn(&plugin, "log", {
        let (plugin, last_words) = (plugin.clone(), last_words.clone());
        move || log(&plugin, stderr, &last_words)
    })?;
    spawn(&plugin, "read", {
        let plugin = plugin.clone();
        move || {
            read(&plugin, stdout);
            // Its stderr closes as its stdout does; what it said last is
            // read before saying why it ended.
            std::thread::sleep(Duration::from_millis(50));
            let said = last_words
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .join(" ");
            let why = match said.trim() {
                "" => "its process ended.".to_owned(),
                said => format!("its process ended: {said}"),
            };
            // So the writer lets go of the queue; from a thread of its own
            // when the queue is full, so this one still says why.
            let stop = json!({"type": "stop", "generation": generation});
            let queue = plugin.sender();
            if queue.try_send(stop.clone()).is_err() {
                std::thread::spawn(move || queue.send(stop));
            }
            if !stopped.load(Ordering::SeqCst) {
                plugin.ended(generation, Err(why));
            }
        }
    })?;
    Ok(Box::new(running))
}

fn spawn(plugin: &Loaded, what: &str, work: impl FnOnce() + Send + 'static) -> Result<(), String> {
    std::thread::Builder::new()
        .name(format!("plugin {} {what}", plugin.name))
        .spawn(work)
        .map(drop)
        .map_err(|e| format!("its {what} thread could not start: {e}"))
}

/// Hands the plugin its messages, hello first, until it is stopped or its
/// stdin closes.
fn write(plugin: &Loaded, mut stdin: std::process::ChildStdin, generation: u64) {
    let queue = plugin.queue.lock().unwrap_or_else(PoisonError::into_inner);
    let since = plugin.epoch();
    let mut line = |message: &Value| {
        let mut text = message.to_string();
        text.push('\n');
        stdin
            .write_all(text.as_bytes())
            .and_then(|()| stdin.flush())
    };
    if line(&plugin.hello()).is_err() {
        return;
    }
    while let Ok(message) = queue.recv() {
        if message["type"] == "stop" {
            if message["generation"] == generation {
                return;
            }
            continue;
        }
        if Loaded::stale(&message, since) {
            continue;
        }
        if line(&message).is_err() {
            return;
        }
    }
}

/// Hands what the plugin says to `Loaded`, a line at a time, until its
/// stdout closes.
fn read(plugin: &Loaded, stdout: std::process::ChildStdout) {
    let mut from = BufReader::new(stdout);
    while let Some(line) = crate::mcp::next_line(&mut from) {
        let Ok(line) = line else {
            plugin.problem("sent a message too long to read, which was passed over.");
            continue;
        };
        match serde_json::from_slice::<Value>(&line) {
            Ok(message) => plugin.heard(message),
            Err(_) => plugin.log(&String::from_utf8_lossy(&line)),
        }
    }
}

/// Keeps what the plugin writes to stderr in its log, and its last lines
/// for when it ends.
fn log(plugin: &Loaded, stderr: std::process::ChildStderr, last_words: &Mutex<Vec<String>>) {
    let mut from = BufReader::new(stderr);
    while let Some(Ok(line)) = crate::mcp::next_line(&mut from) {
        let line = String::from_utf8_lossy(&line).trim_end().to_owned();
        plugin.log(&line);
        let mut last = last_words.lock().unwrap_or_else(PoisonError::into_inner);
        last.push(line);
        let over = last.len().saturating_sub(LAST_WORDS);
        last.drain(..over);
    }
}

/// The folder the runner and the module are in, written from what this
/// binary carries when they are not there as it has them.
fn sdk_folder(plugin: &Loaded) -> Result<PathBuf, String> {
    // The plugin's own folder is `<data>/plugin-data/<name>`.
    let data = plugin
        .own
        .parent()
        .and_then(Path::parent)
        .ok_or("its data folder has no parent.")?;
    let folder = data.join("plugin-sdk").join(SDK);
    std::fs::create_dir_all(&folder)
        .map_err(|e| format!("Could not make {}: {e}", folder.display()))?;
    for (name, text) in [("runner.py", RUNNER), ("letronna.py", MODULE)] {
        let path = folder.join(name);
        if std::fs::read(&path).is_ok_and(|had| had == text.as_bytes()) {
            continue;
        }
        // Whole or not at all, as two plugins may start at once.
        let staged = folder.join(format!("{name}.{}.new", std::process::id()));
        std::fs::write(&staged, text)
            .and_then(|()| std::fs::rename(&staged, &path))
            .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
    }
    Ok(folder)
}

/// The Python plugins run on, found once; a search that found none is
/// tried again after `NOT_FOUND_FOR`, in case one was installed.
fn found() -> Result<Python, String> {
    static FOUND: Mutex<Option<(Instant, Result<Python, String>)>> = Mutex::new(None);
    let mut found = FOUND.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, result)) = found.as_ref()
        && (result.is_ok() || at.elapsed() < NOT_FOUND_FOR)
    {
        return result.clone();
    }
    let result = candidates(std::env::var("LETRONNA_PYTHON").ok().as_deref())
        .into_iter()
        .find(|python| probe(python).is_some_and(|version| version >= OLDEST))
        .ok_or_else(|| {
            format!(
                "needs Python {}.{} or later, and none was found. Install it from python.org, \
                 or set LETRONNA_PYTHON to the one to use.",
                OLDEST.0, OLDEST.1
            )
        });
    *found = Some((Instant::now(), result.clone()));
    result
}

/// Whether a Python that plugins can run on is on this computer, without
/// waiting to look when it was looked for already: `None` before the first
/// look.
pub(super) fn available() -> Option<bool> {
    // Asked of a list on a window's thread, so it never searches itself.
    static LOOKED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    static LOOKING: AtomicBool = AtomicBool::new(false);
    LOOKED.get().copied().or_else(|| {
        if !LOOKING.swap(true, Ordering::SeqCst) {
            std::thread::spawn(|| {
                let _ = LOOKED.set(found().is_ok());
            });
        }
        None
    })
}

/// The Pythons to try, best first: the one the user named, then where each
/// system keeps one.
fn candidates(named: Option<&str>) -> Vec<Python> {
    let one = |program: &str, args: &[&str]| Python {
        program: program.to_owned(),
        args: args.iter().map(|arg| (*arg).to_owned()).collect(),
    };
    let mut candidates = Vec::new();
    if let Some(named) = named.map(str::trim).filter(|named| !named.is_empty()) {
        candidates.push(one(named, &[]));
    }
    if cfg!(windows) {
        // The launcher python.org installs; `python` alone may be the
        // Store's stand-in, which only says how to install it.
        candidates.extend([one("py", &["-3"]), one("python", &[]), one("python3", &[])]);
    } else if cfg!(target_os = "macos") {
        candidates.extend([
            one("/opt/homebrew/bin/python3", &[]),
            one("/usr/local/bin/python3", &[]),
            one("python3", &[]),
        ]);
        // Without the Command Line Tools, /usr/bin/python3 opens a dialog
        // offering to install them instead of running.
        if quietly("xcode-select", &["-p"]).is_some_and(|out| out.status.success()) {
            candidates.push(one("/usr/bin/python3", &[]));
        }
    } else {
        candidates.extend([one("python3", &[]), one("python", &[])]);
    }
    candidates
}

/// The version `python` is, when it runs at all.
fn probe(python: &Python) -> Option<(u32, u32)> {
    let mut args = python.args.clone();
    args.extend([
        "-c".into(),
        "import sys; print('%d.%d' % sys.version_info[:2])".into(),
    ]);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let out = quietly(&python.program, &args)?;
    if !out.status.success() {
        return None;
    }
    let said = String::from_utf8_lossy(&out.stdout);
    let (major, minor) = said.trim().split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Runs `program` with `args`, with no window and nothing on its stdin, for
/// what it printed, given up on past `PROBE_TIME`.
fn quietly(program: &str, args: &[&str]) -> Option<std::process::Output> {
    let mut command = Command::new(program);
    command
        .args(args)
        .envs(
            crate::login_env::vars()
                .iter()
                .map(|(name, value)| (name, value)),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command.spawn().ok()?;
    let until = Instant::now() + PROBE_TIME;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return child.wait_with_output().ok(),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(20)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    }
}

/// Whether a Python plugins can run on is here, looked for now.
#[cfg(test)]
pub(super) fn here() -> bool {
    found().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_python_the_user_named_is_tried_first_then_the_systems_own() {
        let named = candidates(Some("/opt/python/bin/python3"));
        assert_eq!(named[0].program, "/opt/python/bin/python3");
        let usual = candidates(None);
        if cfg!(windows) {
            assert_eq!(
                (usual[0].program.as_str(), usual[0].args.as_slice()),
                ("py", &["-3".to_owned()][..])
            );
        } else if !cfg!(target_os = "macos") {
            assert_eq!(usual[0].program, "python3");
        }
        assert_eq!(candidates(Some("  ")), usual);
    }
}
