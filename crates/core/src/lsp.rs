//! Errors from a language server, told to the model after it edits a file.
//!
//! Without this the model learns that an edit broke the build only when it
//! runs one, a round or several later, if it remembers to. Here a language
//! server the user has set up is told of every file write_file and
//! edit_file change, and the next request ends with the errors it found
//! that were not there before: a short note on the wire only, never saved
//! in the session, as the checklist is.
//!
//! Nothing runs unless the user asks for it in `lsp.json` in the data
//! folder, a map of names to servers in the shape `mcp.json` gives one
//! (`command`, `args`, `env`, `cwd`, `${VAR}`), plus the file `extensions`
//! each serves and the `projects` it is for. It lives beside `hooks.json`
//! rather than in the project, because a server is a command and whoever
//! wrote a repo may not be whoever runs it. And a server serves only the
//! projects it names, none by default: most run the project's own code as
//! they check it -- rust-analyzer its build scripts and proc macros, a
//! TypeScript server the project's own compiler -- so a folder cloned a
//! minute ago must not start one on the agent's first edit. With no file
//! the cost of a write is one failed open, and a request's is a look at an
//! empty map.
//!
//! A server starts the first time a file it serves is written, in the
//! project folder, as a process tree of its own the way an MCP server run
//! here does, spoken to over its pipes in JSON-RPC framed by
//! `Content-Length` headers. One serves a session and its helpers, and is
//! shut down when the session is deleted or the app quits. A file is opened
//! as it was before its first edit, so the errors it already had are known
//! and not blamed on the edit; then each write sends the whole new text.
//! Only errors count, not warnings, and each is told once: one that goes
//! away is forgotten, so it is told again if it comes back.
//!
//! Every wait has a limit, and the request waits only when a file was
//! changed since it last asked, so a slow server costs a few seconds once,
//! not every round. A server that cannot start, does not answer or stops
//! is said to be gone once, and not started again in that session: a
//! process that dies once will most likely die again.

use crate::mcp::{Process, ServerConfig};
use crate::tools::ToolContext;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak, mpsc};
use std::time::{Duration, Instant};

/// The user's language servers, in the data folder.
const FILE: &str = "lsp.json";
/// How long a server gets to answer `initialize`: some load the whole
/// project first.
const START_WAIT: Duration = Duration::from_secs(20);
/// How long a file opened as it was before an edit waits for its errors,
/// which are what the edit is measured against.
const BEFORE_WAIT: Duration = Duration::from_secs(3);
/// The most a request waits for a server to report on the files changed
/// since the last one.
const NOTE_WAIT: Duration = Duration::from_secs(5);
/// How long a server must have said nothing new before its report counts
/// as finished: most report a file more than once, fast checks first.
const QUIET: Duration = Duration::from_millis(250);
/// How long a server gets to shut down before it is ended.
const STOP_WAIT: Duration = Duration::from_secs(1);
/// Errors told at once; past this the rest are counted.
const MAX_LINES: usize = 20;
/// Characters kept of one error's message.
const MESSAGE_CHARS: usize = 200;
/// The largest message read from a server, so a garbled header cannot ask
/// for all the memory there is.
const MAX_FRAME: usize = 64 << 20;
/// The most of one line read before a message: a header takes a few dozen
/// bytes, and a line printed with no end must not take all the memory.
const MAX_LINE: u64 = 8 << 10;

/// `lsp.json`: the servers, by a name of the user's choosing.
#[derive(Deserialize, Default)]
struct Config {
    #[serde(default)]
    servers: BTreeMap<String, Entry>,
}

/// One server in `lsp.json`.
#[derive(Deserialize, Clone)]
struct Entry {
    /// The file extensions it serves, with or without the dot.
    #[serde(default)]
    extensions: Vec<String>,
    /// The project folders it is for; none means none, as a server runs
    /// what the project holds.
    #[serde(default)]
    projects: Vec<PathBuf>,
    /// The LSP language id its files are opened as, when the extension
    /// does not say.
    #[serde(default)]
    language: Option<String>,
    #[serde(flatten)]
    server: ServerConfig,
}

impl Entry {
    /// Whether it is set to serve `path` in the project at `root`.
    fn serves(&self, path: &Path, root: &Path) -> bool {
        let Some(extension) = path.extension().and_then(|e| e.to_str()) else {
            return false;
        };
        let listed = self
            .extensions
            .iter()
            .any(|e| e.trim_start_matches('.').eq_ignore_ascii_case(extension));
        let here = self.projects.iter().any(|p| same_folder(p, root));
        listed && here && !self.server.disabled && !self.server.command.trim().is_empty()
    }
}

/// Whether two folders are the same one, however either is spelt.
fn same_folder(a: &Path, b: &Path) -> bool {
    a == b || std::fs::canonicalize(a).is_ok_and(|a| std::fs::canonicalize(b).is_ok_and(|b| a == b))
}

/// The language id an editor would open a file with this extension as.
fn language(extension: &str) -> String {
    let extension = extension.to_ascii_lowercase();
    let id = match extension.as_str() {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "ts" | "mts" | "cts" => "typescript",
        "tsx" => "typescriptreact",
        "js" | "mjs" | "cjs" => "javascript",
        "jsx" => "javascriptreact",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "cs" => "csharp",
        "go" => "go",
        "java" => "java",
        "rb" => "ruby",
        "sh" | "bash" => "shellscript",
        other => other,
    };
    id.to_owned()
}

/// Where `lsp.json` is read from.
#[cfg(not(test))]
fn config_file() -> Option<PathBuf> {
    Some(crate::chat::data_dir().join(FILE))
}

#[cfg(test)]
thread_local! {
    /// The config a test gives, so no test reads the real data folder.
    static CONFIG: std::cell::RefCell<Option<PathBuf>> = const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn config_file() -> Option<PathBuf> {
    CONFIG.with(|config| config.borrow().clone())
}

/// The config in `file`, or None when there is none or it cannot be read.
fn load(file: &Path) -> Option<Config> {
    let text = std::fs::read_to_string(file).ok()?;
    match serde_json::from_str(&text) {
        Ok(config) => Some(config),
        Err(_) => {
            // Taken for no servers, as a write must not fail over it; the
            // log says why nothing started.
            crate::diag::event("lsp_config", &[("outcome", "unreadable".into())]);
            None
        }
    }
}

/// A write a language server is to hear of once it has happened.
pub(crate) struct Planned {
    owner: u64,
    conversation: u64,
    name: String,
    entry: Entry,
    root: PathBuf,
    path: PathBuf,
    before: Option<String>,
}

/// What `wrote` needs to tell a server of a write to `path`, which holds
/// `before` now, or None where no server is set up for it.
pub(crate) fn before_write(
    ctx: &ToolContext,
    path: &Path,
    before: Option<&str>,
) -> Option<Planned> {
    let file = config_file()?;
    let root = ctx.workspace.as_deref()?;
    // A helper's writes go to the server of the session it works for, so a
    // run of helpers starts one server, not one each.
    let owner = match ctx.caller.root {
        0 => ctx.conversation,
        root => root,
    };
    plan(&file, owner, ctx.conversation, root, path, before)
}

fn plan(
    file: &Path,
    owner: u64,
    conversation: u64,
    root: &Path,
    path: &Path,
    before: Option<&str>,
) -> Option<Planned> {
    // Conversation 0 is none in particular, and no request would ask for it.
    if conversation == 0 || !path.starts_with(root) {
        return None;
    }
    let config = load(file)?;
    let (name, entry) = config
        .servers
        .into_iter()
        .find(|(_, entry)| entry.serves(path, root))?;
    Some(Planned {
        owner,
        conversation,
        name,
        entry,
        root: root.to_path_buf(),
        path: path.to_path_buf(),
        before: before.map(str::to_owned),
    })
}

/// Tell the server `planned` names that its file now holds `after`,
/// starting the server if this is its first.
pub(crate) fn wrote(planned: Option<Planned>, after: &str) {
    let Some(planned) = planned else {
        return;
    };
    let server = {
        let mut servers = lock(servers());
        let key = (planned.owner, planned.name.clone());
        servers
            .entry(key)
            .or_insert_with(|| {
                let (config, root) = (planned.entry.server.clone(), planned.root.clone());
                Server::start(&planned.name, &planned.root, move || launch(&config, &root))
            })
            .clone()
    };
    let extension = planned.path.extension().and_then(|e| e.to_str());
    let language =
        (planned.entry.language.clone()).unwrap_or_else(|| language(extension.unwrap_or_default()));
    server.queue(Job {
        conversation: planned.conversation,
        uri: uri(&planned.path),
        path: planned.path,
        language,
        before: planned.before,
        after: after.to_owned(),
    });
}

/// What the language servers found since `conversation` last asked, as a
/// note to end its next request with, or None when there is nothing new.
pub(crate) fn note(conversation: u64) -> Option<String> {
    if conversation == 0 {
        return None;
    }
    let serving: Vec<Arc<Server>> = lock(servers())
        .values()
        .filter(|server| lock(&server.known).users.contains(&conversation))
        .cloned()
        .collect();
    if serving.is_empty() {
        return None;
    }
    let deadline = Instant::now() + NOTE_WAIT;
    for server in &serving {
        server.settle(conversation, deadline);
    }
    let mut errors = Vec::new();
    let mut gone = Vec::new();
    for server in &serving {
        let (found, lost) = server.report(conversation);
        errors.extend(found);
        gone.extend(lost);
    }
    if errors.is_empty() && gone.is_empty() {
        return None;
    }
    // Said to be from Letronna: it lands after a tool's output or the
    // user's words, and must not read as part of either.
    let mut note =
        String::from("(From Letronna, not part of the above: what the language server says.)");
    if !errors.is_empty() {
        // Scrubbed as a command's output is, and fenced: an error quotes the
        // project -- its strings, a macro's own words -- which a note in
        // Letronna's voice must not vouch for.
        let listed: Vec<&str> = errors.iter().take(MAX_LINES).map(String::as_str).collect();
        let listed = crate::redact::scrub(&listed.join("\n"), true);
        note.push('\n');
        note.push_str(&crate::untrusted::quoted(
            "New errors since your last edit:",
            &listed,
        ));
        if errors.len() > MAX_LINES {
            note.push_str(&format!("\n... and {} more.", errors.len() - MAX_LINES));
        }
    }
    for why in gone {
        note.push('\n');
        // It can carry what the server said as it failed.
        note.push_str(&crate::redact::scrub(&why, true));
    }
    Some(note)
}

/// Shut down every language server, as the app quits.
pub fn stop_all() {
    let all: Vec<Arc<Server>> = lock(servers()).drain().map(|(_, server)| server).collect();
    // All at once, so quitting waits a few seconds at most however many run.
    let ending: Vec<_> = all
        .into_iter()
        .map(|server| std::thread::spawn(move || server.shut_down()))
        .collect();
    for end in ending {
        let _ = end.join();
    }
}

/// Shut down the servers of session `owner`, which has ended, without
/// waiting for them.
pub(crate) fn stop(owner: u64) {
    let mut servers = lock(servers());
    let theirs: Vec<(u64, String)> = servers.keys().filter(|k| k.0 == owner).cloned().collect();
    for key in theirs {
        if let Some(server) = servers.remove(&key) {
            std::thread::spawn(move || server.shut_down());
        }
    }
}

/// Servers by the session they serve and their name.
type Servers = HashMap<(u64, String), Arc<Server>>;

/// The running servers.
fn servers() -> &'static Mutex<Servers> {
    static SERVERS: OnceLock<Mutex<Servers>> = OnceLock::new();
    SERVERS.get_or_init(Mutex::default)
}

/// The lock, whether or not a thread panicked holding it: what it guards
/// is only ever changed whole.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `path` as a `file://` URI.
fn uri(path: &Path) -> String {
    crate::mcp::file_uri(path.as_os_str().as_encoded_bytes(), cfg!(windows))
}

/// A URI as it is compared: servers spell the same file differently, with
/// `:` escaped or not, and on Windows the drive in either case.
fn same_uri(uri: &str) -> String {
    let bytes = uri.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    let text = String::from_utf8_lossy(&out).into_owned();
    if cfg!(windows) {
        text.to_lowercase()
    } else {
        text
    }
}

/// The pipes to a server, and its process when it runs here.
struct Pipes {
    process: Option<Process>,
    from: Box<dyn Read + Send>,
    to: Box<dyn Write + Send>,
}

/// Start the server `config` gives, in the project at `root`.
fn launch(config: &ServerConfig, root: &Path) -> Result<Pipes, String> {
    let config = prepared(config)?;
    let mut child = crate::mcp::spawn(&config, root)?;
    // At once, so the server has had as little time as possible to start
    // anything outside the tree.
    let tree = match crate::jobs::Tree::of(&child) {
        Ok(tree) => tree,
        Err(e) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e);
        }
    };
    let (stdin, stdout, stderr) = (child.stdin.take(), child.stdout.take(), child.stderr.take());
    let process = Process { child, tree };
    let (Some(stdin), Some(stdout), Some(mut stderr)) = (stdin, stdout, stderr) else {
        process.end();
        return Err(format!("`{}` has no pipes to talk over.", config.command));
    };
    // A server that fills its stderr pipe stops until someone reads it.
    std::thread::spawn(move || std::io::copy(&mut stderr, &mut std::io::sink()));
    Ok(Pipes {
        process: Some(process),
        from: Box::new(stdout),
        to: Box::new(stdin),
    })
}

/// `config` as a server is started from it: `${VAR}`s filled in, and in
/// the project folder unless it names another.
fn prepared(config: &ServerConfig) -> Result<ServerConfig, String> {
    let mut config = config.expanded(&|var| std::env::var(var).ok())?;
    // A language server finds the project from the folder it starts in.
    config.cwd.get_or_insert_with(|| ".".into());
    // On Windows a server npm installs is a `.cmd` file only cmd finds, and
    // cmd looks in its current folder, the project, before the PATH: a
    // repository could ship a `.cmd` of the same name. Nothing else reads it.
    config
        .env
        .insert("NoDefaultCurrentDirectoryInExePath".into(), "1".into());
    Ok(config)
}

/// A write for the server to hear of.
struct Job {
    conversation: u64,
    path: PathBuf,
    uri: String,
    language: String,
    before: Option<String>,
    after: String,
}

/// One error a server reported.
#[derive(Clone)]
struct Found {
    /// From 0, as the protocol counts.
    line: u64,
    message: String,
}

/// A server's latest report on one file.
struct Published {
    errors: Vec<Found>,
    version: Option<i64>,
    tick: u64,
}

/// A file the server has open.
struct Open {
    version: i64,
    /// The report count when this version was sent: a report counted after
    /// it can only be of this version or a later one.
    sent: u64,
}

/// One conversation's view of one file it changed.
struct Seen {
    path: PathBuf,
    /// The messages of the errors it has been told of, or that were there
    /// before its first edit.
    messages: Vec<String>,
    /// The version a request last waited for, so a server that never
    /// reports on a file costs one wait, not one a round.
    waited: i64,
}

/// What is known of a server, under one lock.
#[derive(Default)]
struct Known {
    /// Why it can no longer be used; it is not started again.
    dead: Option<String>,
    /// The conversations it has been told of edits for.
    users: HashSet<u64>,
    /// The conversations told it is gone.
    told: HashSet<u64>,
    /// Its output has ended.
    gone: bool,
    stopping: bool,
    answers: HashMap<u64, Value>,
    /// By URI as `same_uri` spells it, as are `open` and `seen`.
    published: HashMap<String, Published>,
    /// Reports counted so far.
    tick: u64,
    last_report: Option<Instant>,
    queued: u64,
    done: u64,
    open: HashMap<String, Open>,
    seen: HashMap<(u64, String), Seen>,
}

impl Known {
    /// Whether the server has reported on the version of `key` last sent.
    fn fresh(&self, key: &str) -> bool {
        let (Some(open), Some(published)) = (self.open.get(key), self.published.get(key)) else {
            return false;
        };
        match published.version {
            Some(version) => version >= open.version,
            None => published.tick > open.sent,
        }
    }

    /// Whether `conversation` has a change the server has not reported on
    /// and no request has waited for yet.
    fn owes(&self, conversation: u64) -> bool {
        self.done < self.queued
            || self.seen.iter().any(|((c, key), seen)| {
                *c == conversation
                    && self
                        .open
                        .get(key)
                        .is_some_and(|open| open.version > seen.waited && !self.fresh(key))
            })
    }
}

/// One language server, as seen from this side of its pipes.
struct Server {
    name: String,
    root: PathBuf,
    to: Mutex<Option<Box<dyn Write + Send>>>,
    process: Mutex<Option<Process>>,
    /// Writes waiting for the worker, which does all the talking.
    jobs: Mutex<Option<mpsc::Sender<Job>>>,
    known: Mutex<Known>,
    /// Woken whenever `known` changes.
    heard: Condvar,
    next: AtomicU64,
}

impl Server {
    /// A server started by `connect` on a worker of its own, so the write
    /// that starts it does not wait for it.
    fn start(
        name: &str,
        root: &Path,
        connect: impl FnOnce() -> Result<Pipes, String> + Send + 'static,
    ) -> Arc<Self> {
        let (jobs, queue) = mpsc::channel();
        let server = Arc::new(Self {
            name: name.to_owned(),
            root: root.to_path_buf(),
            to: Mutex::new(None),
            process: Mutex::new(None),
            jobs: Mutex::new(Some(jobs)),
            known: Mutex::default(),
            heard: Condvar::new(),
            next: AtomicU64::new(1),
        });
        let worker = server.clone();
        std::thread::spawn(move || worker.work(connect, queue));
        server
    }

    fn work(
        self: Arc<Self>,
        connect: impl FnOnce() -> Result<Pipes, String>,
        queue: mpsc::Receiver<Job>,
    ) {
        if let Err(why) = self.connect(connect) {
            self.die(why);
        }
        // Until the server is shut down, which lets go of the sender.
        for job in queue {
            if lock(&self.known).dead.is_none() {
                self.apply(job);
            }
            lock(&self.known).done += 1;
            self.heard.notify_all();
        }
    }

    fn connect(
        self: &Arc<Self>,
        connect: impl FnOnce() -> Result<Pipes, String>,
    ) -> Result<(), String> {
        let pipes = connect()?;
        *lock(&self.process) = pipes.process;
        *lock(&self.to) = Some(pipes.to);
        // Shut down while it was starting: the shutdown found no process.
        if lock(&self.known).stopping {
            return Err(format!("`{}` was shut down.", self.name));
        }
        let weak = Arc::downgrade(self);
        let from = pipes.from;
        std::thread::spawn(move || listen(&weak, from));
        let root = uri(&self.root);
        let folder = self.root.file_name().map(|n| n.to_string_lossy());
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": {"name": "Letronna"},
            "rootUri": root,
            "rootPath": self.root.to_string_lossy(),
            "workspaceFolders": [{"uri": root, "name": folder.unwrap_or_default()}],
            "capabilities": {
                "textDocument": {
                    "synchronization": {"didSave": true},
                    "publishDiagnostics": {"versionSupport": true},
                },
                "workspace": {"configuration": true, "workspaceFolders": true},
            },
        });
        self.request("initialize", params, START_WAIT)
            .map_err(|e| format!("`{}` did not start: {e}", self.name))?;
        self.notify("initialized", json!({}))
    }

    /// Hand `job` to the worker.
    fn queue(&self, job: Job) {
        let mut known = lock(&self.known);
        // Even for a server already gone, so the conversation hears so.
        known.users.insert(job.conversation);
        if known.dead.is_some() {
            return;
        }
        let sent = lock(&self.jobs)
            .as_ref()
            .is_some_and(|jobs| jobs.send(job).is_ok());
        if sent {
            known.queued += 1;
        }
    }

    /// Tell the server of one write.
    fn apply(&self, job: Job) {
        let key = same_uri(&job.uri);
        let open = lock(&self.known).open.get(&key).map(|open| open.version);
        let document = |version: i64, text: &str| {
            json!({"textDocument": {"uri": job.uri, "languageId": job.language,
                "version": version, "text": text}})
        };
        let mut known_before = open.is_some();
        if open.is_none()
            && let Some(before) = &job.before
        {
            // Opened as it was, so what was wrong before the edit is known
            // and not blamed on it.
            self.mark(&key, 0);
            if self
                .notify("textDocument/didOpen", document(0, before))
                .is_err()
            {
                return;
            }
            self.wait_for_report(&key, Instant::now() + BEFORE_WAIT);
            known_before = true;
        }
        let version = open.map_or(1, |version| version + 1);
        {
            let mut known = lock(&self.known);
            let messages = match known_before {
                true => (known.published.get(&key))
                    .map(|p| p.errors.iter().map(|e| e.message.clone()).collect())
                    .unwrap_or_default(),
                false => Vec::new(),
            };
            known
                .seen
                .entry((job.conversation, key.clone()))
                .or_insert(Seen {
                    path: job.path.clone(),
                    messages,
                    waited: -1,
                });
        }
        self.mark(&key, version);
        let sent = if known_before {
            let change = json!({"textDocument": {"uri": job.uri, "version": version},
                "contentChanges": [{"text": job.after}]});
            self.notify("textDocument/didChange", change)
        } else {
            self.notify("textDocument/didOpen", document(version, &job.after))
        };
        // Saved, since the file is on disk already: some servers only run
        // their full check on a save.
        if sent.is_ok() {
            let _ = self.notify(
                "textDocument/didSave",
                json!({"textDocument": {"uri": job.uri}}),
            );
        }
    }

    /// Note that `version` of `key` is about to be sent.
    fn mark(&self, key: &str, version: i64) {
        let mut known = lock(&self.known);
        let sent = known.tick;
        known.open.insert(key.to_owned(), Open { version, sent });
    }

    /// Wait until the server reports on the version of `key` last sent, it
    /// goes, or `deadline` passes.
    fn wait_for_report(&self, key: &str, deadline: Instant) {
        let mut known = lock(&self.known);
        while !known.fresh(key) && !known.gone {
            let Some(left) = left(deadline) else {
                return;
            };
            known = self.wait(known, left);
        }
    }

    /// Wait, until `deadline` at most, for the server to report on what
    /// `conversation` changed and then fall quiet.
    fn settle(&self, conversation: u64, deadline: Instant) {
        let mut known = lock(&self.known);
        let mut waited = false;
        while known.owes(conversation) && known.dead.is_none() {
            let Some(left) = left(deadline) else {
                break;
            };
            waited = true;
            known = self.wait(known, left);
        }
        while waited
            && let Some(since) = known.last_report.map(|at| at.elapsed())
            && since < QUIET
            && let Some(left) = left(deadline)
        {
            known = self.wait(known, (QUIET - since).min(left));
        }
        let Known { seen, open, .. } = &mut *known;
        for ((c, key), seen) in seen.iter_mut() {
            if *c == conversation
                && let Some(open) = open.get(key)
            {
                seen.waited = open.version;
            }
        }
    }

    /// The errors in what `conversation` changed that it has not been told
    /// of, as `path:line: message`, and a word if the server is gone.
    fn report(&self, conversation: u64) -> (Vec<String>, Option<String>) {
        let mut known = lock(&self.known);
        let known = &mut *known;
        let gone = known
            .dead
            .clone()
            .filter(|_| known.told.insert(conversation))
            .map(|why| {
                format!(
                    "The language server `{}` is not running, so it will check no more edits this session: {why}",
                    self.name
                )
            });
        let mut keys: Vec<&(u64, String)> = known
            .seen
            .keys()
            .filter(|(c, _)| *c == conversation)
            .collect();
        keys.sort_by_key(|key| &known.seen[*key].path);
        let keys: Vec<(u64, String)> = keys.into_iter().cloned().collect();
        let mut lines = Vec::new();
        for key in keys {
            let Some(published) = known.published.get(&key.1) else {
                continue;
            };
            let Some(seen) = known.seen.get_mut(&key) else {
                continue;
            };
            // Matched by message, not line: an edit above an old error
            // moves it without making it new.
            let mut told = seen.messages.clone();
            for error in &published.errors {
                match told.iter().position(|m| *m == error.message) {
                    Some(at) => {
                        told.swap_remove(at);
                    }
                    None => {
                        let path = seen.path.strip_prefix(&self.root).unwrap_or(&seen.path);
                        let line = error.line + 1;
                        lines.push(format!("{}:{line}: {}", path.display(), error.message));
                    }
                }
            }
            seen.messages = published.errors.iter().map(|e| e.message.clone()).collect();
        }
        (lines, gone)
    }

    /// Give up on the server, saying why, and end its process.
    fn die(&self, why: String) {
        {
            let mut known = lock(&self.known);
            if !known.stopping {
                known.dead.get_or_insert(why);
            }
        }
        self.end();
        self.heard.notify_all();
    }

    /// Ask the server to shut down and exit, then end it whatever it did.
    fn shut_down(self: &Arc<Self>) {
        let alive = {
            let mut known = lock(&self.known);
            known.stopping = true;
            known.dead.is_none() && !known.gone
        };
        // The worker ends once it has finished what it is doing.
        lock(&self.jobs).take();
        if alive {
            // Asked on a thread of its own: a server that stopped reading
            // holds a write, and the pipe with it, until its process ends.
            let server = self.clone();
            std::thread::spawn(move || {
                if server.request("shutdown", Value::Null, STOP_WAIT).is_ok() {
                    let _ = server.notify("exit", Value::Null);
                }
            });
            let deadline = Instant::now() + 2 * STOP_WAIT;
            let mut known = lock(&self.known);
            while !known.gone
                && let Some(left) = left(deadline)
            {
                known = self.wait(known, left);
            }
        }
        self.end();
    }

    /// End the process, then let go of the pipe: ending it is what frees a
    /// write stuck on a server that stopped reading.
    fn end(&self) {
        if let Some(process) = lock(&self.process).take() {
            process.end();
        }
        lock(&self.to).take();
    }

    fn send(&self, message: &Value) -> Result<(), String> {
        let body = message.to_string();
        let gone = || format!("`{}` stopped.", self.name);
        let mut to = lock(&self.to);
        let to = to.as_mut().ok_or_else(gone)?;
        write!(to, "Content-Length: {}\r\n\r\n{body}", body.len())
            .and_then(|()| to.flush())
            .map_err(|_| gone())
    }

    fn notify(&self, method: &str, params: Value) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method, "params": params}))
    }

    /// Send a request and wait, until `wait` at most, for its answer.
    fn request(&self, method: &str, params: Value, wait: Duration) -> Result<Value, String> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = Instant::now() + wait;
        let mut known = lock(&self.known);
        loop {
            if let Some(answer) = known.answers.remove(&id) {
                return match answer.get("error") {
                    Some(error) => Err(error["message"]
                        .as_str()
                        .unwrap_or("it reported an error")
                        .to_owned()),
                    None => Ok(answer["result"].clone()),
                };
            }
            if known.gone {
                return Err(format!("`{}` stopped.", self.name));
            }
            let Some(left) = left(deadline) else {
                return Err(format!(
                    "`{}` did not answer {method} within {} seconds.",
                    self.name,
                    wait.as_secs()
                ));
            };
            known = self.wait(known, left);
        }
    }

    fn wait<'a>(&self, known: MutexGuard<'a, Known>, left: Duration) -> MutexGuard<'a, Known> {
        match self.heard.wait_timeout(known, left) {
            Ok((known, _)) => known,
            Err(poisoned) => poisoned.into_inner().0,
        }
    }

    /// Take in one message from the server.
    fn hear(self: &Arc<Self>, message: Value) {
        let method = message.get("method").and_then(Value::as_str);
        match (message.get("id"), method) {
            // The server asking something of us. Most servers wait for the
            // answer before they go on, so every request gets one: no
            // settings, and no edits made to files.
            (Some(id), Some(method)) => {
                let result = match method {
                    "workspace/configuration" => {
                        let asked = message["params"]["items"].as_array().map_or(0, Vec::len);
                        Value::Array(vec![Value::Null; asked])
                    }
                    "workspace/applyEdit" => json!({"applied": false}),
                    _ => Value::Null,
                };
                let reply = json!({"jsonrpc": "2.0", "id": id, "result": result});
                // Not on this thread: the worker may hold the pipe, waiting
                // for a server that waits for this answer.
                let server = self.clone();
                std::thread::spawn(move || server.send(&reply));
            }
            (Some(id), None) => {
                if let Some(id) = id.as_u64() {
                    lock(&self.known).answers.insert(id, message);
                    self.heard.notify_all();
                }
            }
            (None, Some("textDocument/publishDiagnostics")) => {
                let params = &message["params"];
                let Some(uri) = params["uri"].as_str() else {
                    return;
                };
                let errors = params["diagnostics"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    // Unmarked counts as an error: the protocol leaves it to
                    // the client, and a server that marks none marks none
                    // of its warnings either.
                    .filter(|d| d["severity"].as_u64().is_none_or(|s| s == 1))
                    .map(|d| Found {
                        line: d["range"]["start"]["line"].as_u64().unwrap_or(0),
                        message: first_line(d["message"].as_str().unwrap_or_default()),
                    })
                    .collect();
                let mut known = lock(&self.known);
                known.tick += 1;
                let published = Published {
                    errors,
                    version: params["version"].as_i64(),
                    tick: known.tick,
                };
                known.published.insert(same_uri(uri), published);
                known.last_report = Some(Instant::now());
                drop(known);
                self.heard.notify_all();
            }
            // Progress, logs and other notices.
            _ => {}
        }
    }
}

/// Read what the server says until it goes or is let go of.
fn listen(weak: &Weak<Server>, from: Box<dyn Read + Send>) {
    let mut from = BufReader::new(from);
    while let Ok(Some(message)) = read_frame(&mut from) {
        let Some(server) = weak.upgrade() else {
            return;
        };
        server.hear(message);
    }
    let Some(server) = weak.upgrade() else {
        return;
    };
    lock(&server.known).gone = true;
    let stopping = lock(&server.known).stopping;
    if stopping {
        server.heard.notify_all();
    } else {
        server.die(format!("`{}` stopped.", server.name));
    }
}

/// The next message from `from`, or None at its end. What is not a header
/// is passed over, text or not: some servers print a banner on stdout
/// first.
fn read_frame(from: &mut impl BufRead) -> std::io::Result<Option<Value>> {
    loop {
        let mut length = None;
        loop {
            // A longer line comes in pieces, each passed over.
            let mut line = Vec::new();
            if from.by_ref().take(MAX_LINE).read_until(b'\n', &mut line)? == 0 {
                return Ok(None);
            }
            let line = String::from_utf8_lossy(&line);
            let line = line.trim();
            if line.is_empty() && length.is_some() {
                break;
            }
            if let Some((name, value)) = line.split_once(':')
                && name.trim().eq_ignore_ascii_case("content-length")
            {
                length = value.trim().parse::<usize>().ok();
            }
        }
        let Some(length) = length.filter(|&length| length <= MAX_FRAME) else {
            return Err(std::io::ErrorKind::InvalidData.into());
        };
        let mut body = vec![0; length];
        from.read_exact(&mut body)?;
        // A message that is not JSON is dropped, not taken for the end.
        if let Ok(message) = serde_json::from_slice(&body) {
            return Ok(Some(message));
        }
    }
}

/// The first line of a server's message, capped: the rest is notes and
/// help the model can ask the build for.
fn first_line(message: &str) -> String {
    let line = message.lines().next().unwrap_or_default().trim();
    match line.char_indices().nth(MESSAGE_CHARS) {
        Some((at, _)) => format!("{}...", &line[..at]),
        None => line.to_owned(),
    }
}

/// The time left until `deadline`, or None once it has passed.
fn left(deadline: Instant) -> Option<Duration> {
    Some(deadline.saturating_duration_since(Instant::now())).filter(|left| !left.is_zero())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An empty folder for one test, which removes it when done.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-lsp-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A session id no other test uses, since the servers are shared.
    fn session() -> u64 {
        static NEXT: AtomicU64 = AtomicU64::new(7_000_000);
        NEXT.fetch_add(1, Ordering::Relaxed)
    }

    /// `lsp.json` in `dir`, with a server for `.rs` files in the project
    /// at `dir` running `command`.
    fn config(dir: &Path, command: &str) -> PathBuf {
        let file = dir.join(FILE);
        let text = json!({"servers": {"fake": {"command": command, "extensions": [".rs"],
            "projects": [dir]}}});
        std::fs::write(&file, text.to_string()).unwrap();
        file
    }

    fn frame(message: &Value) -> Vec<u8> {
        let body = message.to_string();
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    /// How the fake server behaves.
    #[derive(Clone, Copy, PartialEq)]
    enum Fake {
        /// Reports `ERROR: <message>` lines as errors and `WARN: ` lines as
        /// warnings, at their line.
        Checks,
        /// Hangs up on the first thing it hears.
        HangsUp,
    }

    /// A fake language server on pipes, and the methods it has heard.
    fn fake(kind: Fake) -> (Pipes, mpsc::Receiver<String>) {
        let (from_client, to_server) = std::io::pipe().unwrap();
        let (from_server, mut to_client) = std::io::pipe().unwrap();
        let (heard, log) = mpsc::channel();
        std::thread::spawn(move || {
            let mut from_client = BufReader::new(from_client);
            while let Ok(Some(message)) = read_frame(&mut from_client) {
                let method = message["method"].as_str().unwrap_or_default().to_owned();
                let _ = heard.send(method.clone());
                if kind == Fake::HangsUp {
                    return;
                }
                let params = &message["params"];
                let text = match method.as_str() {
                    "textDocument/didOpen" => params["textDocument"]["text"].as_str(),
                    "textDocument/didChange" => params["contentChanges"][0]["text"].as_str(),
                    _ => None,
                };
                let reply = match (method.as_str(), text) {
                    ("initialize", _) => json!({"jsonrpc": "2.0", "id": message["id"],
                        "result": {"capabilities": {}}}),
                    ("shutdown", _) => {
                        json!({"jsonrpc": "2.0", "id": message["id"], "result": null})
                    }
                    ("exit", _) => return,
                    (_, Some(text)) => {
                        let found: Vec<Value> = text
                            .lines()
                            .enumerate()
                            .filter_map(|(at, line)| {
                                let (severity, said) = match line.split_once(": ")? {
                                    ("ERROR", said) => (1, said),
                                    ("WARN", said) => (2, said),
                                    _ => return None,
                                };
                                Some(json!({"severity": severity, "message": said,
                                    "range": {"start": {"line": at, "character": 0},
                                              "end": {"line": at, "character": 1}}}))
                            })
                            .collect();
                        json!({"jsonrpc": "2.0", "method": "textDocument/publishDiagnostics",
                            "params": {"uri": params["textDocument"]["uri"],
                                "version": params["textDocument"]["version"],
                                "diagnostics": found}})
                    }
                    _ => continue,
                };
                if to_client.write_all(&frame(&reply)).is_err() {
                    return;
                }
            }
        });
        let pipes = Pipes {
            process: None,
            from: Box::new(from_server),
            to: Box::new(to_server),
        };
        (pipes, log)
    }

    /// A project with a config naming the fake server, which is already
    /// running for `owner`; the log of what it heard.
    fn project(name: &str, owner: u64, kind: Fake) -> (PathBuf, PathBuf, mpsc::Receiver<String>) {
        let dir = scratch(name);
        let file = config(&dir, "fake-language-server");
        let (pipes, log) = fake(kind);
        let server = Server::start("fake", &dir, move || Ok(pipes));
        lock(servers()).insert((owner, "fake".into()), server);
        (dir, file, log)
    }

    /// Write `after` over `path`, which held `before`, as the file tools do.
    fn edit(file: &Path, owner: u64, root: &Path, path: &Path, before: Option<&str>, after: &str) {
        std::fs::write(path, after).unwrap();
        wrote(plan(file, owner, owner, root, path, before), after);
    }

    #[test]
    fn a_new_error_is_reported_once_and_what_was_there_before_is_not() {
        let owner = session();
        let (dir, file, _) = project("new-error", owner, Fake::Checks);
        let path = dir.join("main.rs");
        let before = "ERROR: old trouble\nfn main() {}\n";
        std::fs::write(&path, before).unwrap();
        let after =
            "ERROR: old trouble\nfn main() {}\nERROR: expected i32, found &str\nWARN: unused\n";
        edit(&file, owner, &dir, &path, Some(before), after);

        let said = note(owner).unwrap();
        assert!(
            said.contains("New errors since your last edit:\n<untrusted-content-")
                && said.contains("\nmain.rs:3: expected i32, found &str\n</untrusted-content-"),
            "{said}"
        );
        assert!(
            !said.contains("old trouble") && !said.contains("unused"),
            "{said}"
        );
        assert_eq!(note(owner), None);

        // Moved down a line by the next edit, it is still not new.
        let later = format!("\n{after}");
        edit(&file, owner, &dir, &path, Some(after), &later);
        assert_eq!(note(owner), None);
        stop(owner);
    }

    #[test]
    fn a_fixed_error_is_cleared_and_told_again_if_it_comes_back() {
        let owner = session();
        let (dir, file, _) = project("fixed", owner, Fake::Checks);
        let path = dir.join("lib.rs");
        let broken = "ERROR: cannot find value `x`\n";
        edit(&file, owner, &dir, &path, None, broken);
        assert!(
            note(owner)
                .unwrap()
                .contains("lib.rs:1: cannot find value `x`")
        );

        edit(&file, owner, &dir, &path, Some(broken), "let x = 1;\n");
        assert_eq!(note(owner), None);

        edit(&file, owner, &dir, &path, Some("let x = 1;\n"), broken);
        assert!(
            note(owner)
                .unwrap()
                .contains("lib.rs:1: cannot find value `x`")
        );
        stop(owner);
    }

    #[test]
    fn errors_reach_the_model_fenced_with_keys_hidden() {
        let owner = session();
        let (dir, file, _) = project("fenced", owner, Fake::Checks);
        let path = dir.join("main.rs");
        let key = "ghp_0123456789abcdefghijABCDEFGHIJ012345";
        let broken = format!("ERROR: expected u32, found \"{key}\"\n");
        edit(&file, owner, &dir, &path, None, &broken);
        let said = note(owner).unwrap();
        stop(owner);
        assert!(!said.contains(key), "{said}");
        let (head, fenced) = said.split_once("<untrusted-content-").unwrap();
        assert!(head.starts_with("(From Letronna"), "{said}");
        assert!(fenced.contains("main.rs:1: expected u32, found"), "{said}");
    }

    #[test]
    fn a_write_through_the_file_tools_reaches_the_server() {
        let owner = session();
        let (dir, file, log) = project("tools", owner, Fake::Checks);
        CONFIG.with(|config| *config.borrow_mut() = Some(file));
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            conversation: owner,
            ..ToolContext::default()
        };
        let args = json!({"path": "new.rs", "content": "ERROR: mismatched types\n"});
        crate::tools::run("write_file", &args.to_string(), &ctx).unwrap();
        CONFIG.with(|config| *config.borrow_mut() = None);

        assert!(note(owner).unwrap().contains("new.rs:1: mismatched types"));
        // The save may still be on its way after the report came back.
        let heard: Vec<String> = (0..4)
            .map(|_| log.recv_timeout(Duration::from_secs(5)).unwrap())
            .collect();
        assert_eq!(
            heard,
            [
                "initialize",
                "initialized",
                "textDocument/didOpen",
                "textDocument/didSave"
            ]
        );
        stop(owner);
    }

    #[test]
    fn nothing_configured_starts_nothing_and_says_nothing() {
        let owner = session();
        let dir = scratch("nothing");
        let path = dir.join("main.rs");
        let missing = dir.join("missing").join(FILE);
        assert!(plan(&missing, owner, owner, &dir, &path, None).is_none());
        // One for other kinds of file is no server for this one.
        let other = config(&dir, "fake-language-server");
        assert!(plan(&other, owner, owner, &dir, &dir.join("notes.md"), None).is_none());

        CONFIG.with(|config| *config.borrow_mut() = Some(missing));
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            conversation: owner,
            ..ToolContext::default()
        };
        assert!(before_write(&ctx, &path, None).is_none());
        CONFIG.with(|config| *config.borrow_mut() = None);
        wrote(None, "ERROR: nobody hears this\n");

        assert!(lock(servers()).keys().all(|(o, _)| *o != owner));
        assert_eq!(note(owner), None);
    }

    #[test]
    fn a_server_serves_only_the_projects_it_lists() {
        let dir = scratch("listed");
        let clone = scratch("listed-clone");
        let listed = config(&dir, "fake-language-server");
        assert!(plan(&listed, 1, 1, &dir, &dir.join("main.rs"), None).is_some());
        // A folder the user never named, such as one cloned a minute ago.
        assert!(plan(&listed, 1, 1, &clone, &clone.join("build.rs"), None).is_none());
        let unlisted = clone.join(FILE);
        let text = json!({"servers": {"fake": {"command": "fake-language-server",
            "extensions": ["rs"]}}});
        std::fs::write(&unlisted, text.to_string()).unwrap();
        assert!(plan(&unlisted, 1, 1, &clone, &clone.join("main.rs"), None).is_none());
    }

    #[test]
    fn a_server_that_stops_is_told_of_once_and_not_started_again() {
        let owner = session();
        let (dir, file, log) = project("stops", owner, Fake::HangsUp);
        let path = dir.join("main.rs");
        edit(&file, owner, &dir, &path, None, "fn main() {}\n");
        let said = note(owner).unwrap();
        assert!(
            said.contains("The language server `fake` is not running") && said.contains("stopped"),
            "{said}"
        );
        edit(
            &file,
            owner,
            &dir,
            &path,
            Some("fn main() {}\n"),
            "ERROR: x\n",
        );
        assert_eq!(note(owner), None);
        assert_eq!(log.try_iter().collect::<Vec<_>>(), ["initialize"]);
        stop(owner);
    }

    #[test]
    fn a_server_that_is_not_installed_is_told_of_once() {
        let owner = session();
        let dir = scratch("missing");
        let file = config(&dir, "letronna-no-such-language-server");
        let path = dir.join("main.rs");
        let started = Instant::now();
        edit(&file, owner, &dir, &path, None, "fn main() {}\n");
        let said = note(owner).unwrap();
        assert!(said.contains("letronna-no-such-language-server"), "{said}");
        assert!(started.elapsed() < NOTE_WAIT);
        assert_eq!(note(owner), None);
        stop(owner);
    }

    #[test]
    fn a_server_is_asked_to_shut_down_and_exit() {
        let owner = session();
        let (dir, file, log) = project("shut-down", owner, Fake::Checks);
        edit(&file, owner, &dir, &dir.join("a.rs"), None, "\n");
        let _ = note(owner);
        let server = lock(servers()).remove(&(owner, "fake".into())).unwrap();
        server.shut_down();
        let heard: Vec<String> = log.try_iter().collect();
        assert!(
            heard.ends_with(&["shutdown".into(), "exit".into()]),
            "{heard:?}"
        );
    }

    #[test]
    fn a_server_starts_in_the_project_with_cmd_kept_from_looking_there() {
        let config: ServerConfig =
            serde_json::from_value(json!({"command": "typescript-language-server"})).unwrap();
        let config = prepared(&config).unwrap();
        assert_eq!(config.cwd.as_deref(), Some("."));
        assert_eq!(config.env["NoDefaultCurrentDirectoryInExePath"], "1");
    }

    #[test]
    fn frames_are_read_by_length_past_whatever_else_is_printed() {
        let one = frame(&json!({"id": 1}));
        let two = frame(&json!({"method": "a\r\n\r\nb"}));
        let text = [b"starting up\n".as_slice(), &one, &two].concat();
        let mut from = BufReader::new(text.as_slice());
        assert_eq!(read_frame(&mut from).unwrap(), Some(json!({"id": 1})));
        assert_eq!(
            read_frame(&mut from).unwrap(),
            Some(json!({"method": "a\r\n\r\nb"}))
        );
        assert_eq!(read_frame(&mut from).unwrap(), None);
    }

    #[cfg(unix)]
    #[test]
    fn quitting_does_not_wait_on_a_server_that_stopped_reading() {
        let dir = scratch("stuck");
        // Answers `initialize` unread, then reads nothing more.
        let body = json!({"jsonrpc": "2.0", "id": 1, "result": {"capabilities": {}}});
        let answer = format!("Content-Length: {}\r\n\r\n{body}", body.to_string().len());
        let config: ServerConfig = serde_json::from_value(json!({"command": "sh",
            "args": ["-c", "printf '%s' \"$0\"; exec sleep 600", answer]}))
        .unwrap();
        let root = dir.clone();
        let server = Server::start("stuck", &dir, move || launch(&config, &root));
        let path = dir.join("a.rs");
        // Far more than a pipe holds, so the worker is left writing it.
        server.queue(Job {
            conversation: session(),
            uri: uri(&path),
            path,
            language: "rust".into(),
            before: None,
            after: "x".repeat(4 << 20),
        });
        std::thread::sleep(Duration::from_millis(500));

        let (done, ended) = mpsc::channel();
        let stopping = server.clone();
        std::thread::spawn(move || {
            stopping.shut_down();
            let _ = done.send(());
        });
        let ended = ended.recv_timeout(4 * STOP_WAIT).is_ok();
        // Ended here if not, so a failure leaves nothing running.
        if let Some(process) = lock(&server.process).take() {
            process.end();
        }
        assert!(ended, "still shutting down");
    }

    #[test]
    fn a_line_that_is_not_text_or_never_ends_is_passed_over() {
        let one = frame(&json!({"id": 1}));
        let endless = vec![b'x'; 3 * MAX_LINE as usize];
        let text = [b"\xff\xfe starting\n".as_slice(), &endless, b"\n", &one].concat();
        let mut from = BufReader::new(text.as_slice());
        assert_eq!(read_frame(&mut from).unwrap(), Some(json!({"id": 1})));
    }

    #[test]
    fn a_uri_is_compared_however_the_server_escapes_it() {
        assert_eq!(
            same_uri("file:///c%3A/src/a.rs"),
            same_uri("file:///c:/src/a.rs")
        );
        assert_ne!(same_uri("file:///src/a.rs"), same_uri("file:///src/b.rs"));
    }
}
