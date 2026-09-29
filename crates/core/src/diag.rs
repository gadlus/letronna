//! A local log and crash files, for finding out afterwards what went wrong.
//!
//! Nothing here may slow the app down or take it down: `event` only
//! `try_send`s into a bounded queue and one writer thread does the disk
//! work, so a slow or full disk costs events, never a stalled turn. Events
//! say what happened -- kinds, ids, sizes, durations and error classes --
//! and never what was said: no messages, prompts or tool output, so a log
//! can be handed over when asking for help. Known secrets are masked on the
//! way out all the same, since an id or a panic message can quote one. Each
//! frontend writes its own `logs/<frontend>.log`, rotated by size, so the
//! window and the terminal never interleave their lines; a panic leaves
//! `crashes/<frontend>-<time>.txt` with its message and a backtrace. That
//! message is kept as it is, masked, since it is the clue, even though it
//! can quote the text the panic choked on; so every file here is readable
//! by its owner only. Startup marks are the exception to the log: asked for
//! with `LETRONNA_PROFILE_STARTUP`, they go to stderr, where whoever asked
//! is looking, and cost one look at the environment otherwise.

use serde_json::Value;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, SyncSender, TrySendError};
use std::time::{Duration, Instant};

/// Events waiting for the writer. A burst larger than this means something
/// loops; dropping its tail beats making the caller wait on the disk.
const QUEUE: usize = 256;
/// A log is rotated past this size: some ten thousand events, weeks of
/// ordinary use.
const LOG_BYTES: u64 = 1024 * 1024;
/// Log files kept per frontend, the current one included, so the logs never
/// take more than 3 MB each.
const LOG_FILES: usize = 3;
/// Crash files kept, newest first: a crash on every start would otherwise
/// fill the disk a file at a time.
const CRASH_FILES: usize = 20;
/// Characters kept of a field's text: an id or a class fits, a message does
/// not.
const FIELD_CHARS: usize = 64;
/// How long a panic or a quit waits for the writer to catch up: enough for
/// a full queue, short enough that a stuck disk does not hold the exit.
const FLUSH_WAIT: Duration = Duration::from_secs(1);
/// How long `reachable` waits for an answer: a gateway that takes longer
/// than this to say anything is as good as down.
const REACH_WAIT: Duration = Duration::from_secs(5);
/// What a masked secret is replaced with.
const HIDDEN: &str = "***";
/// Set to have `mark` time the phases of a start, for finding what makes
/// one slow. A terminal's stderr is the screen it draws on, so send the
/// marks elsewhere: `LETRONNA_PROFILE_STARTUP=1 letronna 2>startup.txt`.
pub const PROFILE_STARTUP: &str = "LETRONNA_PROFILE_STARTUP";

/// The log of this process, once `init` has run.
struct Log {
    frontend: &'static str,
    sender: SyncSender<Entry>,
    /// The writer's thread, which must never wait on itself.
    writer: std::thread::ThreadId,
    started: Instant,
}

static LOG: OnceLock<Log> = OnceLock::new();
/// Events lost to a full queue since the writer last said so.
static DROPPED: AtomicU64 = AtomicU64::new(0);

enum Entry {
    Event {
        time: String,
        kind: &'static str,
        fields: Vec<(&'static str, Value)>,
    },
    /// Answered once everything queued before it is written.
    Flush(SyncSender<()>),
}

/// Start the log for this frontend ("cli" or "gui"): one writer thread and
/// a panic hook that leaves a crash file. Later calls do nothing.
pub fn init(frontend: &'static str) {
    if LOG.get().is_some() {
        return;
    }
    let logs = logs(&crate::chat::data_dir());
    let (sender, entries) = std::sync::mpsc::sync_channel(QUEUE);
    let spawned = std::thread::Builder::new()
        .name("diag".into())
        .spawn(move || write(entries, &logs, frontend));
    let Ok(writer) = spawned else {
        return;
    };
    let log = Log {
        frontend,
        sender,
        writer: writer.thread().id(),
        started: Instant::now(),
    };
    if LOG.set(log).is_err() {
        return;
    }
    catch_panics();
    event(
        "start",
        &[
            ("version", crate::update::VERSION.into()),
            ("os", std::env::consts::OS.into()),
            ("arch", std::env::consts::ARCH.into()),
        ],
    );
}

/// Queue an event without waiting; when the queue is full it is dropped and
/// counted. Fields are ids, sizes, durations and classes, never content.
pub fn event(kind: &'static str, fields: &[(&'static str, Value)]) {
    let Some(log) = LOG.get() else {
        return;
    };
    let entry = Entry::Event {
        time: now(),
        kind,
        fields: fields.to_vec(),
    };
    offer(&log.sender, entry, &DROPPED);
}

/// Say the frontend is stopping, and give the writer a moment to finish.
pub fn stop() {
    let Some(log) = LOG.get() else {
        return;
    };
    event("stop", &[("secs", log.started.elapsed().as_secs().into())]);
    flush();
}

/// "ok", or the class of the error, for an event's outcome field.
pub fn outcome<T>(result: &Result<T, String>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(error) => class(error),
    }
}

/// What kind of failure an error describes, in a word safe to log: the
/// message itself can quote paths, output or a key.
pub fn class(error: &str) -> &'static str {
    let error = error.to_lowercase();
    let says = |words: &[&str]| words.iter().any(|word| error.contains(word));
    if says(&["timed out", "timeout"]) {
        "timeout"
    } else if says(&[
        "http 401",
        "http 403",
        "unauthorized",
        "forbidden",
        "api key",
    ]) {
        "auth"
    } else if says(&["http 429", "rate limit", "too many requests"]) {
        "rate_limited"
    } else if says(&["http 5", "trouble right now"]) {
        "server"
    } else if says(&["cannot reach", "connection", "dns"]) {
        "unreachable"
    } else if says(&["not found", "no such file", "unknown tool"]) {
        "not_found"
    } else if says(&["permission denied", "not allowed", "not available here"]) {
        "denied"
    } else {
        "other"
    }
}

/// Milliseconds since `started`, as an event field.
pub fn millis(started: Instant) -> Value {
    u64::try_from(started.elapsed().as_millis())
        .unwrap_or(u64::MAX)
        .into()
}

/// Whether `url` answers at all, with its HTTP status. Any status counts:
/// this asks whether the server is there, not whether it likes us, so no
/// key is sent.
pub fn reachable(url: &str) -> Result<u16, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(REACH_WAIT)
        .build()
        .map_err(|error| error.to_string())?;
    match client.get(url).send() {
        Ok(response) => Ok(response.status().as_u16()),
        Err(error) if error.is_timeout() => Err("no answer within 5 seconds".into()),
        Err(_) => Err("cannot connect".into()),
    }
}

/// The logs folder in `dir`, the data folder.
pub fn logs(dir: &Path) -> PathBuf {
    dir.join("logs")
}

/// The newest crash file in `dir`, the data folder.
pub fn last_crash(dir: &Path) -> Option<PathBuf> {
    crashes(&dir.join("crashes")).into_iter().next()
}

/// Prints to stderr how long after the first mark `phase` was reached, when
/// `LETRONNA_PROFILE_STARTUP` is set to anything but 0.
pub fn mark(phase: &str) {
    // Read once: the first mark, first thing in main, decides for the rest,
    // and a start that is not being profiled pays for one look.
    static FIRST: OnceLock<Option<Instant>> = OnceLock::new();
    let asked = || std::env::var_os(PROFILE_STARTUP).is_some_and(|on| !on.is_empty() && on != "0");
    let Some(first) = FIRST.get_or_init(|| asked().then(Instant::now)) else {
        return;
    };
    let ms = first.elapsed().as_secs_f64() * 1000.0;
    // Not eprintln, which panics when stderr is gone.
    let _ = writeln!(std::io::stderr(), "startup {phase} {ms:.1} ms");
}

/// Queue `entry` unless the queue is full, counting it in `dropped` then.
fn offer(sender: &SyncSender<Entry>, entry: Entry, dropped: &AtomicU64) -> bool {
    let sent = sender.try_send(entry).is_ok();
    if !sent {
        dropped.fetch_add(1, Ordering::Relaxed);
    }
    sent
}

/// Wait, at most `FLUSH_WAIT`, for the writer to write what is queued.
fn flush() {
    let Some(log) = LOG.get() else {
        return;
    };
    if std::thread::current().id() == log.writer {
        return;
    }
    let deadline = Instant::now() + FLUSH_WAIT;
    let (done, written) = std::sync::mpsc::sync_channel(1);
    let mut entry = Entry::Flush(done);
    loop {
        match log.sender.try_send(entry) {
            Ok(()) => break,
            Err(TrySendError::Full(back)) if Instant::now() < deadline => {
                entry = back;
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(_) => return,
        }
    }
    let _ = written.recv_timeout(deadline.saturating_duration_since(Instant::now()));
}

fn now() -> String {
    chrono::Local::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, false)
}

/// The writer thread: every event becomes a line in `logs`. Write errors are
/// swallowed -- a full disk loses the log, not the app.
fn write(entries: Receiver<Entry>, logs: &Path, frontend: &'static str) {
    // Read once: a key saved later is still caught by its shape.
    let secrets = crate::redact::secrets();
    let pid = std::process::id();
    while let Ok(entry) = entries.recv() {
        let (time, kind, fields) = match entry {
            Entry::Flush(done) => {
                let _ = done.try_send(());
                continue;
            }
            Entry::Event { time, kind, fields } => (time, kind, fields),
        };
        let dropped = DROPPED.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            let lost = [("count", dropped.into())];
            let _ = append(
                logs,
                frontend,
                &line(&time, "dropped", frontend, pid, &lost, &secrets),
                LOG_BYTES,
            );
        }
        let _ = append(
            logs,
            frontend,
            &line(&time, kind, frontend, pid, &fields, &secrets),
            LOG_BYTES,
        );
    }
}

/// One event as a JSON line: text fields masked and cut short, and any
/// field that is not a number, a flag or a text written as null.
fn line(
    time: &str,
    kind: &str,
    frontend: &str,
    pid: u32,
    fields: &[(&str, Value)],
    secrets: &[String],
) -> String {
    let mut object = serde_json::Map::new();
    object.insert("time".into(), time.into());
    object.insert("kind".into(), kind.into());
    object.insert("frontend".into(), frontend.into());
    object.insert("pid".into(), pid.into());
    for (name, value) in fields {
        let value = match value {
            Value::String(text) => clip(&mask(text, secrets)).into(),
            Value::Number(_) | Value::Bool(_) => value.clone(),
            _ => Value::Null,
        };
        object.insert((*name).into(), value);
    }
    format!("{}\n", Value::Object(object))
}

fn clip(text: &str) -> String {
    match text.char_indices().nth(FIELD_CHARS) {
        Some((cut, _)) => format!("{}…", &text[..cut]),
        None => text.to_owned(),
    }
}

/// `text` with every known secret, bearer token and key-shaped word masked.
pub fn mask(text: &str, secrets: &[String]) -> String {
    crate::redact::hide(text, secrets, crate::redact::Shapes::Any, HIDDEN)
}

/// Append `line` to the frontend's log in `logs`, rotating the log first
/// once it has reached `limit` bytes.
fn append(logs: &Path, frontend: &str, line: &str, limit: u64) -> std::io::Result<()> {
    std::fs::create_dir_all(logs)?;
    let path = log_file(logs, frontend, 0);
    if std::fs::metadata(&path).is_ok_and(|meta| meta.len() >= limit) {
        // cli.log becomes cli.1.log, cli.1.log cli.2.log, and the oldest goes.
        for n in (1..LOG_FILES).rev() {
            let _ = std::fs::rename(log_file(logs, frontend, n - 1), log_file(logs, frontend, n));
        }
    }
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&path)?.write_all(line.as_bytes())
}

/// The frontend's log, `n` rotations old.
fn log_file(logs: &Path, frontend: &str, n: usize) -> PathBuf {
    match n {
        0 => logs.join(format!("{frontend}.log")),
        n => logs.join(format!("{frontend}.{n}.log")),
    }
}

/// Put a panic hook in front of the one there: it writes a crash file and
/// the log's last lines, then lets the old hook report as it always did.
fn catch_panics() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".into());
        let location = info.location().map(|at| at.to_string()).unwrap_or_default();
        let frontend = LOG.get().map_or("letronna", |log| log.frontend);
        let backtrace = std::backtrace::Backtrace::force_capture().to_string();
        // Read now rather than at start, so a key saved since is masked too.
        let secrets = crate::redact::secrets();
        let dir = crate::chat::data_dir();
        let _ = crash(&dir, frontend, &message, &location, &backtrace, &secrets);
        event("panic", &[("at", location.into())]);
        flush();
        previous(info);
    }));
}

/// Write a crash file for `frontend` in `dir`, the data folder, and keep
/// only the newest `CRASH_FILES` of them.
fn crash(
    dir: &Path,
    frontend: &str,
    message: &str,
    location: &str,
    backtrace: &str,
    secrets: &[String],
) -> std::io::Result<PathBuf> {
    let folder = dir.join("crashes");
    std::fs::create_dir_all(&folder)?;
    let time = chrono::Local::now();
    let path = folder.join(format!(
        "{frontend}-{}.txt",
        time.format("%Y%m%d-%H%M%S-%3f")
    ));
    let report = format!(
        "{} {} ({frontend}, {} {}) panicked at {}\n{location}\n\n{message}\n\n{backtrace}\n",
        crate::BRAND,
        crate::update::VERSION,
        std::env::consts::OS,
        std::env::consts::ARCH,
        time.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
    );
    crate::store::write_private(&path, mask(&report, secrets).as_bytes())?;
    for old in crashes(&folder).into_iter().skip(CRASH_FILES) {
        let _ = std::fs::remove_file(old);
    }
    Ok(path)
}

/// The crash files in `folder`, newest first.
fn crashes(folder: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut files: Vec<(std::time::SystemTime, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "txt"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    files.sort_by(|a, b| b.cmp(a));
    files.into_iter().map(|(_, path)| path).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-diag-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn owner_only(path: &Path) -> bool {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777 == 0o600
    }

    #[test]
    fn saved_secrets_bearer_tokens_and_key_shapes_are_masked() {
        let saved = "a-saved-key-of-no-known-shape".to_owned();
        let text = "saved a-saved-key-of-no-known-shape, Authorization: Bearer abc.def-123, \
                    key sk-proj-0123456789abcdef, token 0123456789abcdefghij0123456789abcdefghij, \
                    and sk-1 stays";
        let masked = mask(text, &[saved]);
        for secret in ["a-saved-key", "abc.def", "sk-proj-", "0123456789abcdefghij"] {
            assert!(!masked.contains(secret), "{secret} left in {masked}");
        }
        assert!(masked.contains("Authorization: Bearer *** key ***, token ***,"));
        assert!(masked.contains("and sk-1 stays"));
    }

    #[test]
    fn an_event_line_carries_no_content() {
        let error = "Cannot open /home/someone/plans/merger.txt: No such file or directory";
        let prompt = "please summarise the attached board minutes for me ".repeat(3);
        let fields = [
            ("outcome", outcome::<()>(&Err(error.into())).into()),
            ("name", prompt.clone().into()),
            ("args", serde_json::json!({ "path": "merger.txt" })),
            ("ms", 12.into()),
        ];
        let text = line("2026-09-29T10:00:00", "tool", "cli", 7, &fields, &[]);
        assert!(text.ends_with('\n') && text.matches('\n').count() == 1);
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["kind"], "tool");
        assert_eq!(value["frontend"], "cli");
        assert_eq!(value["pid"], 7);
        assert_eq!(value["outcome"], "not_found");
        assert_eq!(value["ms"], 12);
        assert!(value["args"].is_null());
        assert!(value["name"].as_str().unwrap().chars().count() <= FIELD_CHARS + 1);
        assert!(!text.contains("merger") && !text.contains(prompt.trim_end()));
    }

    #[test]
    fn a_full_log_rotates_and_the_oldest_goes() {
        let logs = scratch("rotate").join("logs");
        for n in 0..4 {
            append(&logs, "cli", &format!("line {n}\n"), 1).unwrap();
        }
        let read = |n| std::fs::read_to_string(log_file(&logs, "cli", n)).unwrap();
        assert_eq!(read(0), "line 3\n");
        assert_eq!(read(1), "line 2\n");
        assert_eq!(read(2), "line 1\n");
        assert!(!log_file(&logs, "cli", LOG_FILES).exists());
        #[cfg(unix)]
        assert!(owner_only(&log_file(&logs, "cli", 0)));
    }

    #[test]
    fn a_full_queue_drops_events_without_blocking() {
        let (sender, _entries) = std::sync::mpsc::sync_channel(2);
        let dropped = AtomicU64::new(0);
        let started = Instant::now();
        let sent: Vec<bool> = (0..5)
            .map(|_| {
                let entry = Entry::Event {
                    time: String::new(),
                    kind: "tool",
                    fields: Vec::new(),
                };
                offer(&sender, entry, &dropped)
            })
            .collect();
        assert_eq!(sent, [true, true, false, false, false]);
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[test]
    fn a_panic_leaves_a_masked_crash_file() {
        let dir = scratch("crash");
        let saved = "a-saved-key-of-no-known-shape".to_owned();
        let message = "index out of bounds, near a-saved-key-of-no-known-shape";
        let path = crash(&dir, "gui", message, "src/app.rs:10:5", "0: main", &[saved]).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("gui-") && name.ends_with(".txt"), "{name}");
        let report = std::fs::read_to_string(&path).unwrap();
        assert!(report.contains("index out of bounds, near ***"), "{report}");
        assert!(report.contains("src/app.rs:10:5") && report.contains("0: main"));
        assert!(!report.contains("a-saved-key"));
        assert_eq!(last_crash(&dir), Some(path.clone()));
        #[cfg(unix)]
        assert!(owner_only(&path));
    }

    #[test]
    fn errors_are_logged_as_a_class_not_their_words() {
        assert_eq!(
            class("The gateway is having trouble right now (HTTP 502)."),
            "server"
        );
        assert_eq!(
            class("Cannot reach the model. Check that your server is running"),
            "unreachable"
        );
        assert_eq!(class("Incorrect API key provided: sk-abc"), "auth");
        assert_eq!(class("operation timed out"), "timeout");
        assert_eq!(class("something else entirely"), "other");
        assert_eq!(outcome::<()>(&Ok(())), "ok");
    }
}
