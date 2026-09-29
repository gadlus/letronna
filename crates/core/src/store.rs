//! Sessions on disk.
//!
//! One append-only file per session, `sessions/<id>.jsonl`, a JSON record per
//! line. Everything shared between sessions lives in `settings.json`.
//!
//! The shape matters because two Letronnas will run at once -- the window and
//! the terminal. A single file holding every session, rewritten whole on each
//! save, means whichever process writes last erases the other's work. Here a
//! session is only ever appended to, and only by whoever is running it, so
//! two of them never tread on each other. A crash mid-write costs at most the
//! last line instead of the whole history.

use crate::chat::{self as backend, Message};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
pub struct Conversation {
    /// Stable identity, so a streaming reply lands in the session it was sent
    /// from even if the user switches or deletes sessions meanwhile.
    #[serde(default)]
    pub id: u64,
    pub title: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub updated: u64,
    #[serde(default)]
    pub pinned: bool,
    /// Messages the file holds, counted while listing the sessions and
    /// before any of them are read. Only meaningful while `messages` is
    /// empty; ask `length()` rather than reading this.
    #[serde(skip)]
    pub counted: usize,
    /// Project folder attached to this session, if any.
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    /// The model's own checklist for the work in progress.
    #[serde(default)]
    pub todos: Vec<Todo>,
    /// Messages already on disk. Everything past this is appended on save.
    /// Only `flush` and `rewind` should touch it; it is visible because
    /// callers build a Conversation with `..Default::default()`.
    #[serde(skip)]
    pub flushed: usize,
    /// The descriptive fields as last written, so save appends a `meta` line
    /// only when one of them actually changed.
    #[serde(skip)]
    pub written: Option<Meta>,
    /// A rewind not yet on disk: how many messages the session was cut back
    /// to. Only `rewind` and `flush` should touch it; it is visible for the
    /// same reason as `flushed`.
    #[serde(skip)]
    pub rewound: Option<usize>,
    /// Deleted by the other Letronna while this one had it open, so it is
    /// never written again, even once a new session takes its id. Only
    /// `flush` should touch it; it is visible for the same reason as `flushed`.
    #[serde(skip)]
    pub gone: bool,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub title: String,
    pub updated: u64,
    pub pinned: bool,
    pub workspace: Option<PathBuf>,
    pub todos: Vec<Todo>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub text: String,
    /// "pending", "doing" or "done".
    pub status: String,
}

/// Replace `todos` with the checklist a `todo` call sends and answer the
/// call with how far along it is; a call that cannot be read changes nothing.
pub fn apply_todo(todos: &mut Vec<Todo>, arguments: &str) -> Result<String, String> {
    *todos = parse_todos(arguments)?;
    let done = todos.iter().filter(|t| t.status == "done").count();
    Ok(format!(
        "Checklist updated: {done} of {} done.",
        todos.len()
    ))
}

/// The checklist the last readable `todo` call in `messages` set, or none.
pub fn todos_in(messages: &[Message]) -> Vec<Todo> {
    // One that cannot be read changed nothing, as `apply_todo` has it, so the
    // one before it still stands.
    messages
        .iter()
        .rev()
        .flat_map(|m| m.tool_calls.iter().rev())
        .filter(|call| call.name == crate::tools::TODO)
        .find_map(|call| parse_todos(&call.arguments).ok())
        .unwrap_or_default()
}

/// The checklist a `todo` call sets, from its arguments.
fn parse_todos(arguments: &str) -> Result<Vec<Todo>, String> {
    let args = crate::tools::parse_args_for(crate::tools::TODO, arguments)?;
    let items = args["items"]
        .as_array()
        .ok_or_else(|| "`items` is required.".to_owned())?;
    Ok(items
        .iter()
        .map(|item| Todo {
            text: item["text"].as_str().unwrap_or_default().to_owned(),
            status: item["status"].as_str().unwrap_or("pending").to_owned(),
        })
        .filter(|todo| !todo.text.is_empty())
        .collect())
}

/// A line in a session file.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum Record {
    /// Written once, when the session is created.
    Head {
        id: u64,
        created: u64,
    },
    /// The descriptive fields; the last one in the file wins.
    Meta(Meta),
    Message(Message),
    /// From here on the session is its first `to` messages. Taking messages
    /// back is appended like everything else rather than cutting the file.
    Rewind {
        to: usize,
    },
}

/// Everything that is not a session.
#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub gateway: String,
    /// The URL pasted for the Custom gateway. Kept apart from `endpoint`,
    /// which follows whichever gateway the current model is on.
    #[serde(default)]
    pub custom_endpoint: String,
    #[serde(default)]
    pub presence: String,
    /// Run write and command tools without asking.
    #[serde(default)]
    pub skip_approvals: bool,
    /// How hard the model should think: empty, or a level such as low or max;
    /// a model without that level is sent the nearest one it has.
    #[serde(default)]
    pub effort: String,
    /// Show the model's thinking as it arrives, rather than only its answer.
    #[serde(default)]
    pub thinking: bool,
    /// Let the gateway search the web.
    #[serde(default)]
    pub search: bool,
    /// Which session was open last, by id rather than position.
    #[serde(default)]
    pub active_id: u64,
    /// Only look for updates when asked.
    #[serde(default)]
    pub manual_updates: bool,
    /// Every session shares one browser, its sign-ins remembered, rather
    /// than each having a private one, forgotten when Letronna quits.
    #[serde(default)]
    pub browser_shared: bool,
    /// The agent browses unseen: the browser panel does not open by itself
    /// when it uses the browser, though it can still be opened to look.
    #[serde(default)]
    pub browse_unseen: bool,
    /// The window's colours: empty for Night, "paper", or "system" to follow
    /// the computer's light or dark mode.
    #[serde(default)]
    pub appearance: String,
    /// A faint film grain on the window's chrome and empty pages, never over
    /// the transcript, code or diffs.
    #[serde(default = "yes")]
    pub grain: bool,
}

// Written out rather than derived so the film grain starts on for a first
// run too, as it does for a settings file from before it existed.
impl Default for Settings {
    fn default() -> Self {
        Settings {
            endpoint: String::new(),
            model: String::new(),
            gateway: String::new(),
            custom_endpoint: String::new(),
            presence: String::new(),
            skip_approvals: false,
            effort: String::new(),
            thinking: false,
            search: false,
            active_id: 0,
            manual_updates: false,
            browser_shared: false,
            browse_unseen: false,
            appearance: String::new(),
            grain: true,
        }
    }
}

fn yes() -> bool {
    true
}

/// Settings that are chosen as one and so saved as one: a model id belongs
/// to its gateway and that gateway's endpoint.
const CHOSEN_TOGETHER: [&str; 3] = ["gateway", "model", "endpoint"];

#[derive(Default)]
pub struct Store {
    pub conversations: Vec<Conversation>,
    pub active: usize,
    pub settings: Settings,
    /// The settings as JSON when last read or written, so a save puts back
    /// only the fields changed here since; only loading and `save` touch it.
    pub settings_written: serde_json::Value,
}

impl Conversation {
    /// How long the conversation is, whether or not it has been read off the
    /// disk yet. Once it is in memory the messages are the truth; before
    /// that, the count taken while listing is.
    pub fn length(&self) -> usize {
        if self.messages.is_empty() {
            self.counted
        } else {
            self.messages.len()
        }
    }

    /// Take the conversation back to its first `len` messages, on disk too
    /// once it is saved.
    pub fn rewind(&mut self, len: usize) {
        if len >= self.messages.len() {
            return;
        }
        self.messages.truncate(len);
        // Rewound to nothing, the messages are empty and the listing's count
        // is read again: left as it was, `length` would report the old
        // history and `fill` would read it back in.
        self.counted = self.counted.min(len);
        // What is already written stays in the file; a rewind record goes
        // after it, and anything said from here on after that. Rewinding
        // again before a save only moves the record back.
        if self.flushed > len {
            self.flushed = len;
            self.rewound = Some(len);
        }
    }
}

impl Store {
    /// An id no existing session uses. Ids also name files, so a collision
    /// would merge two histories; the folder is checked as well as memory.
    fn next_id(&self) -> u64 {
        let highest = self.conversations.iter().map(|c| c.id).max().unwrap_or(0);
        let on_disk = session_ids().into_iter().max().unwrap_or(0);
        highest.max(on_disk) + 1
    }

    /// Claim an id for a new session by creating its empty file, so two
    /// Letronnas starting sessions at the same moment never get the same one.
    pub fn reserve_id(&self) -> u64 {
        // The highest id plus one is only where the search starts: another
        // process can read the same folder in the same instant. Creating the
        // file with create_new is atomic, so exactly one of them gets it.
        let _ = std::fs::create_dir_all(sessions_dir());
        let mut id = self.next_id();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(session_path(id))
            {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => id += 1,
                // Claimed, or the folder cannot be written at all. Then the
                // session carries on in memory and its save reports the
                // failure, as it did before ids were claimed.
                _ => return id,
            }
        }
    }

    /// Give every session an id. Older saves have none.
    pub fn assign_ids(&mut self) {
        let mut next = self.next_id();
        for c in self.conversations.iter_mut() {
            if c.id == 0 {
                c.id = next;
                next += 1;
            }
        }
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.conversations.iter().position(|c| c.id == id)
    }

    /// Append whatever is new in each session, then the settings. Returns the
    /// first failure, so the caller can say the history is not safe.
    pub fn save(&mut self) -> std::io::Result<()> {
        std::fs::create_dir_all(sessions_dir())?;
        for conversation in self.conversations.iter_mut() {
            conversation.flush()?;
        }
        self.settings.active_id = self
            .conversations
            .get(self.active)
            .map(|c| c.id)
            .unwrap_or(0);
        let now = serde_json::to_value(&self.settings)?;
        // Most saves are for a message, with no setting moved, and those are
        // known without reading the file.
        if now == self.settings_written {
            return Ok(());
        }
        // Only what moved here goes back, so a setting the other process
        // changed meanwhile is not put back the way this one last saw it.
        let mut then = self.settings_written.clone();
        // But a model goes back with the gateway it was chosen on, or the
        // file could pair it with the one the other process chose. Only a
        // new model or gateway does that: a send sets the endpoint again and
        // the Custom URL is edited on its own, and neither is a pick to put
        // back over the other process's.
        let picked = ["gateway", "model"]
            .iter()
            .any(|&f| then.get(f) != now.get(f));
        if picked && let Some(fields) = then.as_object_mut() {
            for field in CHOSEN_TOGETHER {
                fields.remove(field);
            }
            // A Custom model was listed from the Custom URL, so it goes too.
            if self.settings.gateway == "Custom" {
                fields.remove("custom_endpoint");
            }
        }
        crate::state::update::<Settings>(&settings_path(), |disk| {
            crate::state::merge(disk, &then, &now)
        })
        .map_err(std::io::Error::other)?;
        self.settings_written = now;
        Ok(())
    }

    /// Forget a session and remove its file, its helpers' conversations,
    /// the copies kept of the files it wrote, and its lock file if it can
    /// take the lock.
    pub fn remove(&mut self, index: usize) {
        if index >= self.conversations.len() {
            return;
        }
        let gone = self.conversations.remove(index);
        let path = session_path(gone.id);
        let lock = Lock::take(&path);
        let _ = std::fs::remove_file(&path);
        // A lock file someone else holds stays: removed, the next writer
        // would lock a new file beside them and both would write.
        if lock.is_ok() {
            let _ = std::fs::remove_file(path.with_extension("lock"));
        }
        let _ = std::fs::remove_dir_all(helpers_dir(gone.id));
        crate::history::forget(gone.id);
    }
}

impl Conversation {
    fn meta(&self) -> Meta {
        Meta {
            title: self.title.clone(),
            updated: self.updated,
            pinned: self.pinned,
            workspace: self.workspace.clone(),
            todos: self.todos.clone(),
        }
    }

    /// Append the head (first time), any rewind and new messages, and the
    /// descriptive fields if they have moved since the last write.
    fn flush(&mut self) -> std::io::Result<()> {
        if self.gone {
            return Ok(());
        }
        self.flush_to(&session_path(self.id))
    }

    /// Flush into the file at `path` rather than the session's own.
    fn flush_to(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        let meta = self.meta();
        let meta_changed = self.written.as_ref() != Some(&meta);
        // Every session is flushed on every save, and most have nothing new.
        // Those are known without the file system: taking the lock and
        // checking the file first made each save touch every session on disk.
        if self.written.is_some()
            && self.flushed == self.messages.len()
            && self.rewound.is_none()
            && !meta_changed
        {
            return Ok(());
        }
        let _lock = Lock::take(path)?;
        // A reserved session's file is there but empty, and still needs its head.
        let fresh = !std::fs::metadata(path).is_ok_and(|m| m.len() > 0);
        // Written before and gone now: the other Letronna deleted it. Writing
        // anyway brought it back as a fragment -- a head and whatever changed
        // since the last save, with none of the history before it. Remembered
        // rather than checked each time, because the freed id is handed out
        // again and this would then write into someone else's session.
        if fresh && self.written.is_some() {
            self.gone = true;
            return Ok(());
        }
        if !fresh && self.flushed == self.messages.len() && self.rewound.is_none() && !meta_changed
        {
            return Ok(());
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        let mut out = String::new();
        // A crash part way through a line leaves the file without its last
        // newline. Appending straight after it would glue the next record to
        // the torn one and lose both; ending the torn line first loses one.
        if torn(&mut file)? {
            out.push('\n');
        }
        if fresh {
            push(
                &mut out,
                &Record::Head {
                    id: self.id,
                    created: self.updated,
                },
            );
        }
        if meta_changed {
            push(&mut out, &Record::Meta(meta.clone()));
        }
        if let Some(to) = self.rewound {
            push(&mut out, &Record::Rewind { to });
        }
        for message in &self.messages[self.flushed.min(self.messages.len())..] {
            push(&mut out, &Record::Message(message.clone()));
        }
        file.write_all(out.as_bytes())?;
        self.flushed = self.messages.len();
        self.written = Some(meta);
        self.rewound = None;
        Ok(())
    }
}

fn push(out: &mut String, record: &Record) {
    if let Ok(line) = serde_json::to_string(record) {
        out.push_str(&line);
        out.push('\n');
    }
}

/// Whether the file stops part way through a line, as a write cut short leaves it.
fn torn(file: &mut std::fs::File) -> std::io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

/// Rebuild a session by replaying its file. A line that will not parse is
/// skipped rather than failing the session: losing one message is better than
/// losing the conversation it was part of.
fn replay(path: &std::path::Path) -> Option<Conversation> {
    // Read leniently: a write cut part way through a character leaves bytes
    // that are not UTF-8, which would otherwise fail the whole file rather
    // than the one line they are on.
    let bytes = std::fs::read(path).ok()?;
    let text = String::from_utf8_lossy(&bytes);
    let mut conversation = Conversation::default();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Record>(line) {
            Ok(Record::Head { id, created }) => {
                conversation.id = id;
                conversation.updated = created;
            }
            Ok(Record::Meta(meta)) => {
                conversation.title = meta.title;
                conversation.updated = meta.updated;
                conversation.pinned = meta.pinned;
                conversation.workspace = meta.workspace;
                conversation.todos = meta.todos;
            }
            Ok(Record::Message(message)) => conversation.messages.push(message),
            Ok(Record::Rewind { to }) => conversation.messages.truncate(to),
            Err(_) => continue,
        }
    }
    // A crash in the first save can tear the head line itself. The file is
    // named after the session, so its id survives that.
    if conversation.id == 0 {
        conversation.id = path.file_stem()?.to_str()?.parse().ok()?;
    }
    if conversation.id == 0 {
        return None;
    }
    conversation.flushed = conversation.messages.len();
    conversation.written = Some(conversation.meta());
    Some(conversation)
}

/// A helper's conversation, written in the same lines as a session but never
/// listed or searched as one.
pub struct Transcript {
    pub path: PathBuf,
    pub conversation: Conversation,
}

impl Transcript {
    /// Start a conversation for a new helper of `session`, and say which
    /// number it got there.
    pub fn create(
        session: u64,
        title: &str,
        workspace: Option<PathBuf>,
    ) -> Result<(u32, Self), String> {
        let dir = helpers_dir(session);
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("Could not make a place for the helper's conversation: {e}"))?;
        let conversation = Conversation {
            id: session,
            title: title.into(),
            updated: backend::now(),
            workspace,
            ..Default::default()
        };
        for n in 1..=u32::MAX {
            let path = dir.join(format!("{n}.jsonl"));
            // Making the file is what claims the number: of two processes
            // asking at once exactly one gets it, where counting and adding
            // one, as `next_id` does, would hand both the same.
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Could not start the helper's conversation: {e}")),
            };
            // The head goes in here because flush writes one only into a file
            // that is not there yet, and this one now is.
            let mut head = String::new();
            push(
                &mut head,
                &Record::Head {
                    id: session,
                    created: conversation.updated,
                },
            );
            let mut transcript = Self { path, conversation };
            file.write_all(head.as_bytes())
                .and_then(|_| transcript.save())
                .map_err(|e| format!("Could not start the helper's conversation: {e}"))?;
            return Ok((n, transcript));
        }
        Err("This session has no numbers left for another helper.".into())
    }

    /// Read back helper `n` of `session`, if it is there.
    pub fn open(session: u64, n: u32) -> Option<Self> {
        let path = helpers_dir(session).join(format!("{n}.jsonl"));
        let conversation = replay(&path)?;
        Some(Self { path, conversation })
    }

    /// Append whatever is new, as saving a session does.
    pub fn save(&mut self) -> std::io::Result<()> {
        self.conversation.flush_to(&self.path)
    }
}

/// What a session looks like from the outside: enough to recognise it by,
/// and nothing that was said in it.
pub struct Summary {
    pub id: u64,
    pub title: String,
    pub updated: u64,
    pub messages: usize,
    pub workspace: Option<PathBuf>,
    pub pinned: bool,
    pub todos: Vec<Todo>,
}

/// The sessions on disk, newest first, without loading what is in them.
///
/// Only the head and meta records of each file are read, so this stays cheap
/// with hundreds of sessions and -- more to the point -- a caller asking
/// "which sessions are there" does not pull old conversations into a new one.
pub fn summaries() -> Vec<Summary> {
    let mut out: Vec<Summary> = session_ids()
        .into_iter()
        .filter_map(|id| {
            let bytes = std::fs::read(session_path(id)).ok()?;
            // Reserved and not yet saved: it belongs to whoever reserved it.
            if bytes.is_empty() {
                return None;
            }
            let text = String::from_utf8_lossy(&bytes);
            let mut summary = Summary {
                id,
                title: String::new(),
                updated: 0,
                messages: 0,
                workspace: None,
                pinned: false,
                todos: Vec::new(),
            };
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                // Messages are most of the file and none of them are wanted
                // here, so they are counted by their tag rather than parsed.
                // The tag is written first, which is what makes this safe;
                // anything that does not match still goes through serde.
                if line.starts_with("{\"type\":\"message\"") {
                    summary.messages += 1;
                    continue;
                }
                match serde_json::from_str::<Record>(line) {
                    Ok(Record::Head { created, .. }) => summary.updated = created,
                    Ok(Record::Meta(meta)) => {
                        summary.title = meta.title;
                        summary.updated = meta.updated;
                        summary.workspace = meta.workspace;
                        summary.pinned = meta.pinned;
                        summary.todos = meta.todos;
                    }
                    Ok(Record::Message(_)) => summary.messages += 1,
                    Ok(Record::Rewind { to }) => summary.messages = summary.messages.min(to),
                    Err(_) => {}
                }
            }
            Some(summary)
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.updated));
    out
}

/// A place in an earlier session where a search found what it looked for.
pub struct Hit {
    pub id: u64,
    pub title: String,
    pub updated: u64,
    pub role: String,
    /// The matching words with a little of what surrounds them, on one line.
    pub snippet: String,
}

/// Matches taken from any one session, so a long one cannot fill the list.
const HITS_PER_SESSION: usize = 3;
/// Characters of context kept either side of a match.
const SNIPPET_SIDE: usize = 100;

/// What was said in past sessions that contains `query`, ignoring case,
/// newest sessions first.
pub fn search(query: &str, limit: usize) -> Vec<Hit> {
    search_in(&sessions_dir(), query, limit)
}

fn search_in(dir: &std::path::Path, query: &str, limit: usize) -> Vec<Hit> {
    let query = query.trim().to_lowercase();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    if query.is_empty() {
        return Vec::new();
    }
    let mut sessions: Vec<(u64, Vec<Hit>)> = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "jsonl") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let text = String::from_utf8_lossy(&bytes);
        // Most sessions never mention it, which the whole file says at once.
        // Quotes and backslashes are escaped on disk, so a query holding
        // either cannot be checked that way and reads the messages instead.
        if !query.contains(['"', '\\']) && !text.to_lowercase().contains(&query) {
            continue;
        }
        // The file's name gives the id when the head line is torn, as in
        // `replay`, so a hit still says which session to open.
        let named = path.file_stem().and_then(|s| s.to_str()?.parse().ok());
        let (mut id, mut title, mut updated) = (named.unwrap_or(0), String::new(), 0);
        // Each hit keeps its message's place, and the cap waits for the end
        // of the file: a rewind further down can take the first hits back
        // and leave later ones standing.
        let (mut hits, mut said) = (Vec::new(), 0);
        for line in text.lines() {
            match serde_json::from_str::<Record>(line) {
                Ok(Record::Head { id: head, created }) => (id, updated) = (head, created),
                // The last meta line wins, and it can come after messages.
                Ok(Record::Meta(meta)) => (title, updated) = (meta.title, meta.updated),
                Ok(Record::Message(message)) => {
                    if let Some(snippet) = around(&message.content, &query) {
                        // A notice has the user's role, but the snippet rarely
                        // reaches its header, and a job's output read back as
                        // the user's words becomes a request they never made.
                        let role = if message.notice {
                            "notice".into()
                        } else {
                            message.role
                        };
                        hits.push((said, role, snippet));
                    }
                    said += 1;
                }
                Ok(Record::Rewind { to }) => {
                    hits.retain(|(at, ..)| *at < to);
                    said = said.min(to);
                }
                _ => {}
            }
        }
        if hits.is_empty() {
            continue;
        }
        let hits = hits
            .into_iter()
            .take(HITS_PER_SESSION)
            .map(|(_, role, snippet)| Hit {
                id,
                title: title.clone(),
                updated,
                role,
                snippet,
            })
            .collect();
        sessions.push((updated, hits));
    }
    sessions.sort_by_key(|(updated, _)| std::cmp::Reverse(*updated));
    sessions
        .into_iter()
        .flat_map(|(_, hits)| hits)
        .take(limit)
        .collect()
}

/// The first place `query` (already lowercase) occurs in `text`, with
/// `SNIPPET_SIDE` characters either side, on one line.
fn around(text: &str, query: &str) -> Option<String> {
    let lower = text.to_lowercase();
    // Counted in characters, since lowercasing can change a character's
    // length in bytes and the cut is made in the original.
    let at = lower[..lower.find(query)?].chars().count();
    let from = at.saturating_sub(SNIPPET_SIDE);
    let to = at + query.chars().count() + SNIPPET_SIDE;
    let cut: String = text.chars().skip(from).take(to - from).collect();
    let mut snippet = crate::tools::one_line(&cut);
    if from > 0 {
        snippet.insert(0, '…');
    }
    if text.chars().count() > to {
        snippet.push('…');
    }
    Some(snippet)
}

pub fn sessions_dir() -> PathBuf {
    backend::data_dir().join("sessions")
}

fn session_path(id: u64) -> PathBuf {
    sessions_dir().join(format!("{id}.jsonl"))
}

/// Where the helpers of a session keep their conversations.
fn helpers_dir(session: u64) -> PathBuf {
    // Outside `sessions/`, where every file is taken for a session, and in
    // the data folder, which no tool may read.
    backend::data_dir().join("agents").join(session.to_string())
}

pub fn settings_path() -> PathBuf {
    backend::data_dir().join("settings.json")
}

/// The old single-file store, kept only so it can be converted once.
fn legacy_path() -> PathBuf {
    backend::data_dir().join("chats.json")
}

/// A lock held while a session file is being written.
///
/// The window and the terminal share these files and both append to them, so
/// two of them can be part way through a line at the same time -- which is
/// one corrupt record and, because a record is a whole message, one message
/// lost. The lock is the operating system's, on a file beside the session
/// that stays there: the system lets go of it when its holder closes the file
/// or dies, so no lock is ever stale and no one removes a lock file from
/// under its holder.
pub struct Lock(std::fs::File);

impl Lock {
    /// Wait up to a second for the lock, and fail rather than write without it.
    pub(crate) fn take(path: &std::path::Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        for _ in 0..40 {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
        // A holder keeps it for one append, so a second of waiting means
        // something is stuck. Writing anyway could tear a line; failing
        // leaves the messages unwritten in memory, and the next save sends
        // them.
        Err(std::io::Error::other(
            "another Letronna is still writing this session",
        ))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Closing the file lets go too, but Windows may take its time over
        // a lock left to the close.
        let _ = self.0.unlock();
    }
}

/// Whether session `id` is surely not saved: a file that merely cannot be
/// read now does not count, as its kept copies would go with the answer.
fn gone(id: u64) -> bool {
    std::fs::metadata(session_path(id)).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn session_ids() -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        // A lock left by a crash shares its session's stem.
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            // Each session's lock file sits beside it under the same number.
            e.file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".jsonl"))
                .and_then(|s| s.parse().ok())
        })
        .collect()
}

/// Load every session, newest last, plus the settings.
/// The store with the sessions listed but not read.
///
/// Every session's messages are in memory after `load`, which is most of what
/// it costs and almost none of what is used: one conversation is open and the
/// rest are a list of titles. This reads the head and meta of each file and
/// leaves the messages for `fill`, which is what the terminal wants -- it
/// starts often and opens one session. The window keeps `load`, because its
/// sidebar searches across all of them.
pub fn load_listing() -> Store {
    migrate();
    crate::history::sweep(gone);
    let mut conversations: Vec<Conversation> = summaries()
        .into_iter()
        .map(|s| {
            let mut conversation = Conversation {
                id: s.id,
                title: s.title,
                updated: s.updated,
                workspace: s.workspace,
                pinned: s.pinned,
                todos: s.todos,
                counted: s.messages,
                ..Default::default()
            };
            // Record what the file already says. Left empty, the first save
            // wrote a fresh meta line into every session -- one saying
            // unpinned, no todos, whatever the window had set there.
            conversation.written = Some(conversation.meta());
            conversation
        })
        .collect();
    conversations.sort_by_key(|c| c.updated);
    let (settings, settings_written) = settings();
    Store {
        conversations,
        active: 0,
        settings,
        settings_written,
    }
}

/// Read a session's messages, if they are not already here.
pub fn fill(conversation: &mut Conversation) {
    if conversation.counted == 0 || !conversation.messages.is_empty() {
        return;
    }
    if let Some(full) = replay(&session_path(conversation.id)) {
        conversation.messages = full.messages;
        // What has been written is what was just read, so a flush appends
        // only what is said from here on rather than the whole history again.
        conversation.flushed = conversation.messages.len();
        conversation.written = Some(conversation.meta());
    }
}

/// The settings, and as JSON what the file said before a retired gateway is
/// moved, so the move is saved like any other change.
fn settings() -> (Settings, serde_json::Value) {
    let mut s: Settings = crate::state::read(&settings_path()).unwrap_or_default();
    let read = serde_json::to_value(&s).unwrap_or_default();
    retire_gateway(&mut s);
    (s, read)
}

/// A gateway that is no longer offered moves to the Letronna account. Its
/// model is dropped too, since that id belonged to the old gateway; an empty
/// model is picked again from the new list.
fn retire_gateway(s: &mut Settings) {
    use crate::gateway::{GATEWAYS, LETRONNA};
    // Saves from before Custom had its own field kept its URL in `endpoint`.
    if s.gateway == "Custom" && s.custom_endpoint.is_empty() {
        s.custom_endpoint = s.endpoint.clone();
    }
    if s.gateway.is_empty() || GATEWAYS.iter().any(|g| g.name == s.gateway) {
        return;
    }
    s.gateway = LETRONNA.into();
    s.endpoint = GATEWAYS[0].base.into();
    s.model.clear();
}

#[cfg(test)]
mod retire_tests {
    use super::*;

    #[test]
    fn old_gateways_move_to_letronna_and_known_ones_stay() {
        let mut s = Settings {
            gateway: "Retired".into(),
            model: "retired/some-model".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(s.gateway, "Letronna");
        assert_eq!(s.endpoint, "https://api.letronna.ai/v1");
        assert!(s.model.is_empty());

        let mut s = Settings {
            gateway: "OpenRouter".into(),
            model: "x/y".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (s.gateway.as_str(), s.model.as_str()),
            ("OpenRouter", "x/y")
        );
    }
}

pub fn load() -> Store {
    migrate();
    crate::history::sweep(gone);
    let mut conversations: Vec<Conversation> = session_ids()
        .into_iter()
        .filter_map(|id| replay(&session_path(id)))
        .collect();
    conversations.sort_by_key(|c| c.updated);
    let (settings, settings_written) = settings();
    let active = conversations
        .iter()
        .position(|c| c.id == settings.active_id)
        .unwrap_or(conversations.len().saturating_sub(1));
    Store {
        conversations,
        active,
        settings,
        settings_written,
    }
}

/// Convert a `chats.json` written by an earlier build, once. The original is
/// kept beside the new files rather than deleted, so a conversion that goes
/// wrong is recoverable.
fn migrate() {
    let legacy = legacy_path();
    if !legacy.exists() || sessions_dir().exists() {
        return;
    }
    #[derive(Deserialize)]
    struct Old {
        #[serde(default)]
        conversations: Vec<Conversation>,
        #[serde(default)]
        endpoint: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        gateway: String,
        #[serde(default)]
        presence: String,
        #[serde(default)]
        skip_approvals: bool,
    }
    let Ok(bytes) = std::fs::read(&legacy) else {
        return;
    };
    let Ok(old) = serde_json::from_slice::<Old>(&bytes) else {
        return;
    };
    if std::fs::create_dir_all(sessions_dir()).is_err() {
        return;
    }
    let mut store = Store {
        conversations: old.conversations,
        active: 0,
        settings: Settings {
            endpoint: old.endpoint,
            model: old.model,
            gateway: old.gateway,
            presence: old.presence,
            skip_approvals: old.skip_approvals,
            active_id: 0,
            // Settings that did not exist when the old file was written.
            ..Default::default()
        },
        ..Default::default()
    };
    store.assign_ids();
    if store.save().is_ok() {
        let _ = std::fs::rename(&legacy, legacy.with_extension("json.migrated"));
    }
}

pub fn keys_path() -> PathBuf {
    backend::data_dir().join("keys.json")
}

/// Write the keys file. Private to this user: the keys are the one thing
/// here worth stealing.
pub fn save_keys(keys: &std::collections::HashMap<String, String>) -> std::io::Result<()> {
    std::fs::create_dir_all(backend::data_dir())?;
    let keys = serde_json::to_value(keys)?;
    crate::state::update::<std::collections::HashMap<String, String>>(&keys_path(), |disk| {
        *disk = keys
    })
    .map_err(std::io::Error::other)
}

/// Put one key in the keys file, or with `None` take it out, leaving every
/// other slot as the file has it, and give back the keys it then holds.
pub fn save_key(
    slot: &str,
    key: Option<&str>,
) -> std::io::Result<std::collections::HashMap<String, String>> {
    std::fs::create_dir_all(backend::data_dir())?;
    let mut saved = serde_json::Value::Null;
    crate::state::update::<std::collections::HashMap<String, String>>(&keys_path(), |disk| {
        match (disk.as_object_mut(), key) {
            (Some(keys), Some(key)) => {
                keys.insert(slot.to_owned(), key.into());
            }
            (Some(keys), None) => {
                keys.remove(slot);
            }
            (None, Some(key)) => *disk = serde_json::json!({ slot: key }),
            (None, None) => {}
        }
        saved = disk.clone();
    })
    .map_err(std::io::Error::other)?;
    // Null, with no file and nothing to put in one, is no keys.
    Ok(serde_json::from_value(saved).unwrap_or_default())
}

/// Write a file only its owner may read: the keys, the sign-in, bot tokens.
/// On Linux and macOS a plain write leaves it readable by every account on
/// the machine (0644), so it is made 0600, a file from before included;
/// on Windows the user's own AppData is private already.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // `mode` only applies to a new file.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    std::fs::write(path, bytes)
}

pub fn load_keys() -> std::collections::HashMap<String, String> {
    crate::state::read(&keys_path()).unwrap_or_default()
}

pub fn bucket(updated: u64, now: u64) -> &'static str {
    let age = now.saturating_sub(updated);
    match age / 86_400 {
        0 => "TODAY",
        1 => "YESTERDAY",
        2..=6 => "EARLIER THIS WEEK",
        7..=13 => "LAST WEEK",
        _ => "OLDER",
    }
}

pub fn age_label(updated: u64, now: u64) -> String {
    let age = now.saturating_sub(updated);
    if updated == 0 {
        String::new()
    } else if age < 3600 {
        format!("{}m", (age / 60).max(1))
    } else if age < 86_400 {
        format!("{}h", age / 3600)
    } else {
        format!("{}d", age / 86_400)
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    #[cfg(unix)]
    use super::write_private;
    use super::{
        Conversation, Lock, Store, Todo, Transcript, apply_todo, fill, load, load_listing, replay,
        search, search_in, summaries, todos_in,
    };
    use crate::chat::{Message, ToolCall};

    /// Held by every test that points `AGENT_DATA_DIR` somewhere: it is one
    /// variable for the whole process, and tests run side by side.
    static DATA_DIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn scratch(name: &str) -> std::path::PathBuf {
        // One folder per test, reused and emptied each run. Naming it after
        // the process as well left a new one behind on every run.
        let dir = std::env::temp_dir().join(format!("letronna-store-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        let file = dir.join("keys.json");
        // Written before, readable by everyone, as a plain write left it.
        std::fs::write(&file, "{}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, b"{\"OpenAI\":\"sk-1\"}").unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\"OpenAI\":\"sk-1\"}"
        );
        // And a new one starts that way.
        let fresh = dir.join("account.json");
        write_private(&fresh, b"{}").unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    fn said(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: text.into(),
            ..Default::default()
        }
    }

    /// Session 1, holding `n` messages that each say their own place.
    fn numbered(n: usize) -> Conversation {
        Conversation {
            id: 1,
            messages: (0..n).map(|i| said("user", &i.to_string())).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn appends_only_what_is_new_and_replays_to_the_same_thing() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("append");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            title: "first".into(),
            updated: 100,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let after_first = std::fs::read_to_string(&path).unwrap();
        // head + meta + one message
        assert_eq!(after_first.lines().count(), 3);

        // Saving again with nothing new must not write anything.
        store.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after_first);

        // A new message appends one line; the rest of the file is untouched.
        store.conversations[0]
            .messages
            .push(said("assistant", "hi"));
        store.save().unwrap();
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert!(after_second.starts_with(&after_first));
        assert_eq!(after_second.lines().count(), 4);

        // A title change appends a meta line, not a rewrite.
        store.conversations[0].title = "renamed".into();
        store.conversations[0].todos = vec![Todo {
            text: "step".into(),
            status: "doing".into(),
        }];
        store.save().unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with(&after_second)
        );

        let back = replay(&path).unwrap();
        assert_eq!(back.id, 1);
        assert_eq!(back.title, "renamed");
        assert_eq!(back.todos.len(), 1);
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].content, "hi");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn listing_then_saving_leaves_every_session_as_it_was() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("listing");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            title: "pinned in the window".into(),
            updated: 100,
            pinned: true,
            todos: vec![Todo {
                text: "step".into(),
                status: "doing".into(),
            }],
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::read_to_string(&path).unwrap();

        // What the terminal does every time it starts: list, then save.
        let mut listed = load_listing();
        assert!(listed.conversations[0].pinned);
        assert_eq!(listed.conversations[0].todos.len(), 1);
        listed.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // Opening the session and saving with nothing said adds nothing too.
        fill(&mut listed.conversations[0]);
        listed.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_retired_gateway_moves_to_letronna_in_the_window_too() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("retired");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::write(
            dir.join("settings.json"),
            r#"{"gateway":"Retired","endpoint":"https://retired.example/v1","model":"retired/some-model"}"#,
        )
        .unwrap();

        // The window reads everything with `load`, the terminal with
        // `load_listing`; both must see the same gateway.
        let window = load().settings;
        assert_eq!(window.gateway, "Letronna");
        assert!(window.model.is_empty());
        assert_eq!(load_listing().settings.gateway, window.gateway);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_torn_line_costs_one_message_not_the_session() {
        let dir = scratch("torn");
        let path = dir.join("torn.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"head\",\"id\":7,\"created\":5}\n\
             {\"type\":\"meta\",\"title\":\"kept\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"one\"}\n\
             {\"type\":\"message\",\"role\":\"user\",\"cont\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"three\"}\n",
        )
        .unwrap();
        let back = replay(&path).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.title, "kept");
        // The half-written line is dropped; the ones either side survive.
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].content, "three");
    }

    #[test]
    fn a_torn_last_line_is_sealed_before_the_next_append() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("torn-tail");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        // A write cut short: half a record and no newline after it.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\":\"message\",\"role\":\"us");
        std::fs::write(&path, text).unwrap();

        store.conversations[0].messages.push(said("user", "again"));
        store.save().unwrap();
        let back = replay(&path).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "again"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_bad_byte_costs_one_line() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("bad-byte");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("7.jsonl");
        // The third line was cut in the middle of the two bytes of an é.
        let mut bytes = b"{\"type\":\"head\",\"id\":7,\"created\":5}\n\
            {\"type\":\"meta\",\"title\":\"kept\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}\n\
            {\"type\":\"message\",\"role\":\"user\",\"content\":\"one\"}\n\
            {\"type\":\"message\",\"role\":\"user\",\"content\":\"caf"
            .to_vec();
        bytes.push(0xC3);
        bytes.extend_from_slice(
            b"\n{\"type\":\"message\",\"role\":\"user\",\"content\":\"three\"}\n",
        );
        std::fs::write(&path, bytes).unwrap();

        let back = replay(&path).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["one", "three"]);
        let listed = summaries();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "kept");
        assert_eq!(search_in(&sessions, "three", 10).len(), 1);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_with_a_broken_head_keeps_its_id() {
        let dir = scratch("broken-head");
        let path = dir.join("7.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"head\",\"id\":7,\"cr\n\
             {\"type\":\"meta\",\"title\":\"kept\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();
        let back = replay(&path).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.title, "kept");
        assert_eq!(back.messages.len(), 1);
        let hits = search_in(&dir, "hello", 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 7);
    }

    #[test]
    fn search_finds_what_was_said_newest_session_first() {
        let dir = scratch("search");
        let session = |id: u64, title: &str, updated: u64, said: &[(&str, &str)]| {
            let mut text = format!(
                "{{\"type\":\"head\",\"id\":{id},\"created\":{updated}}}\n\
                 {{\"type\":\"meta\",\"title\":\"{title}\",\"updated\":{updated},\"pinned\":false,\"workspace\":null,\"todos\":[]}}\n"
            );
            for (role, content) in said {
                text.push_str(&format!(
                    "{{\"type\":\"message\",\"role\":\"{role}\",\"content\":\"{content}\"}}\n"
                ));
            }
            std::fs::write(dir.join(format!("{id}.jsonl")), text).unwrap();
        };
        session(
            1,
            "Database",
            100,
            &[
                ("user", "Should we use Postgres or SQLite?"),
                ("assistant", "Postgres, for the concurrency."),
            ],
        );
        session(2, "Moving", 300, &[("user", "Moving to postgres now")]);
        session(3, "Unrelated", 200, &[("user", "nothing to see")]);

        let hits = search_in(&dir, "POSTGRES", 10);
        let found: Vec<(u64, &str)> = hits.iter().map(|h| (h.id, h.role.as_str())).collect();
        assert_eq!(found, [(2, "user"), (1, "user"), (1, "assistant")]);
        assert_eq!(hits[1].title, "Database");
        assert_eq!(hits[1].snippet, "Should we use Postgres or SQLite?");
        assert_eq!(search_in(&dir, "POSTGRES", 1).len(), 1);
        assert!(search_in(&dir, "mysql", 10).is_empty());
    }

    #[test]
    fn a_search_hit_on_a_notice_is_not_the_users() {
        let dir = scratch("search-notice");
        std::fs::write(
            dir.join("1.jsonl"),
            "{\"type\":\"message\",\"role\":\"user\",\"content\":\"run the tests\"}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"the tests failed\",\"notice\":true}\n",
        )
        .unwrap();
        let roles: Vec<String> = search_in(&dir, "tests", 10)
            .into_iter()
            .map(|hit| hit.role)
            .collect();
        assert_eq!(roles, ["user", "notice"]);
    }

    #[test]
    fn a_long_match_is_shown_with_its_surroundings_only() {
        let text = format!("{} the needle {}", "a".repeat(300), "b".repeat(300));
        let snippet = super::around(&text, "needle").unwrap();
        assert!(
            snippet.starts_with('…') && snippet.ends_with('…'),
            "{snippet}"
        );
        assert!(snippet.contains("the needle"));
        assert!(snippet.chars().count() < 220, "{}", snippet.chars().count());
    }

    #[test]
    fn a_rewound_session_reads_back_shorter() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::read_to_string(&path).unwrap();

        // The rewind is one more line; nothing already written changes.
        store.conversations[0].rewind(2);
        store.save().unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(after, format!("{before}{{\"type\":\"rewind\",\"to\":2}}\n"));

        // It is written once: saving again with nothing new adds nothing.
        store.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);

        let back = replay(&path).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].content, "1");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn rewinding_twice_before_a_save_keeps_the_shorter() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-twice");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        store.conversations[0].rewind(4);
        store.conversations[0].rewind(2);
        store.save().unwrap();

        let back = replay(&dir.join("sessions/1.jsonl")).unwrap();
        assert_eq!(back.messages.len(), 2);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn what_is_said_after_a_rewind_follows_it() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-then-say");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        store.conversations[0].rewind(2);
        store.conversations[0].messages.push(said("user", "again"));
        store.save().unwrap();

        let back = replay(&dir.join("sessions/1.jsonl")).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "again"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_listing_counts_what_is_left() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-listing");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        store.conversations[0].rewind(2);
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 2);

        // Counting carries on from the rewind, not from the end of the file.
        store.conversations[0].messages.push(said("user", "again"));
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 3);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn search_skips_what_was_rewound() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-search");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            messages: vec![
                said("user", "one fig"),
                said("user", "two fig"),
                said("user", "three fig"),
                said("user", "four fig"),
            ],
            ..Default::default()
        });
        store.save().unwrap();
        store.conversations[0].rewind(1);
        store.conversations[0]
            .messages
            .push(said("user", "five fig"));
        store.save().unwrap();

        let sessions = dir.join("sessions");
        assert!(search_in(&sessions, "three", 10).is_empty());
        // More hits than a session may show were taken back, which must not
        // hide the one said after them.
        let figs: Vec<String> = search_in(&sessions, "fig", 10)
            .into_iter()
            .map(|hit| hit.snippet)
            .collect();
        assert_eq!(figs, ["one fig", "five fig"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_listed_session_rewound_to_the_start_stays_empty() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-start");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();

        // The terminal reads a listed session when it is opened, so opening
        // it again must not bring back what was just taken out of it.
        let mut listed = load_listing();
        fill(&mut listed.conversations[0]);
        listed.conversations[0].rewind(0);
        fill(&mut listed.conversations[0]);
        assert_eq!(listed.conversations[0].length(), 0);
        listed.save().unwrap();
        let back = replay(&dir.join("sessions/1.jsonl")).unwrap();
        assert!(back.messages.is_empty());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_leftover_lock_file_does_not_list_a_session_twice() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("leftover-lock");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();
        // What a process that died part way through a save leaves behind.
        std::fs::write(dir.join("sessions/1.lock"), "").unwrap();

        assert_eq!(summaries().len(), 1);
        assert_eq!(super::load().conversations.len(), 1);
    }

    #[test]
    fn a_setting_changed_in_one_process_survives_the_other_saving() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-merge");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("settings.json");
        // Laid out by hand, not the way a save lays it out.
        let by_hand = "{\"endpoint\":\"\",\"model\":\"a/one\"}";
        std::fs::write(&path, by_hand).unwrap();

        // The window and the terminal, both started from that file.
        let mut window = load_listing();
        let mut terminal = load_listing();
        // Saving with no setting changed leaves the file as it was.
        window.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), by_hand);

        terminal.settings.model = "b/two".into();
        terminal.save().unwrap();
        window.settings.effort = "high".into();
        window.save().unwrap();
        let back = load_listing().settings;
        assert_eq!(
            (back.model.as_str(), back.effort.as_str()),
            ("b/two", "high")
        );
    }

    #[test]
    fn a_settings_file_without_appearance_loads_as_night_with_grain() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("appearance-old");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let file = dir.join("settings.json");
        // As a build from before the light theme wrote it.
        std::fs::write(
            &file,
            r#"{"endpoint":"https://api.letronna.ai/v1","model":"a/model","skip_approvals":true}"#,
        )
        .unwrap();

        let mut store = load();
        assert_eq!(store.settings.appearance, "");
        assert!(store.settings.grain);
        assert!(store.settings.skip_approvals);
        // Saved and read again, the grain is still on and nothing else moved.
        store.save().unwrap();
        let again = load().settings;
        assert_eq!(again.appearance, "");
        assert!(again.grain);
        assert!(again.skip_approvals);
        // A first run, with no settings file at all, starts the same way.
        std::fs::remove_file(&file).unwrap();
        let first = load().settings;
        assert_eq!((first.appearance.as_str(), first.grain), ("", true));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_todo_call_sets_the_whole_checklist_and_says_how_far_along_it_is() {
        let mut todos = vec![Todo {
            text: "an old plan".into(),
            status: "doing".into(),
        }];
        let said = apply_todo(
            &mut todos,
            r#"{"items":[{"text":"read","status":"done"},{"text":"write"},{"text":"","status":"doing"}]}"#,
        );
        assert_eq!(said.unwrap(), "Checklist updated: 1 of 2 done.");
        // A missing status is still to do, and an item with no words is dropped.
        let items: Vec<_> = todos
            .iter()
            .map(|t| (t.text.as_str(), t.status.as_str()))
            .collect();
        assert_eq!(items, [("read", "done"), ("write", "pending")]);
    }

    #[test]
    fn a_todo_call_that_cannot_be_read_leaves_the_checklist_as_it_was() {
        let plan = vec![Todo {
            text: "step".into(),
            status: "doing".into(),
        }];
        let mut todos = plan.clone();
        assert_eq!(
            apply_todo(&mut todos, r#"{"text":"step"}"#).unwrap_err(),
            "`items` is required."
        );
        assert!(apply_todo(&mut todos, r#"{"items":[{"text":"#).is_err());
        assert!(todos == plan);
    }

    #[test]
    fn the_checklist_is_the_last_one_left_standing() {
        let todo = |items: &str| Message {
            role: "assistant".into(),
            tool_calls: vec![ToolCall {
                id: "call".into(),
                name: crate::tools::TODO.into(),
                arguments: format!(r#"{{"items":[{items}]}}"#),
            }],
            ..Default::default()
        };
        let first = todo(r#"{"text":"plan","status":"done"}"#);
        let second = todo(r#"{"text":"build","status":"doing"}"#);
        assert_eq!(todos_in(&[first.clone(), second.clone()])[0].text, "build");
        assert_eq!(todos_in(&[first])[0].text, "plan");
        assert!(todos_in(&[]).is_empty());
        // A call that could not be read changed nothing, so the one before stands.
        let mut garbled = second.clone();
        garbled.tool_calls[0].arguments = r#"{"items": [{"text": "lost"#.into();
        assert_eq!(todos_in(&[second, garbled])[0].text, "build");
    }

    #[test]
    fn a_settings_file_that_will_not_parse_is_kept_beside_the_new_one() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-corrupt");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let torn = "{\"endpoint\":\"\",\"model\":\"a/one\",\"gatew";
        std::fs::write(dir.join("settings.json"), torn).unwrap();

        let mut store = load_listing();
        assert!(store.settings.model.is_empty());
        store.settings.model = "b/two".into();
        store.save().unwrap();

        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.to_string_lossy().contains("settings.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), torn);
        assert_eq!(load_listing().settings.model, "b/two");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_model_is_saved_with_the_gateway_it_was_chosen_on() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-pair");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::write(
            dir.join("settings.json"),
            "{\"endpoint\":\"https://api.openai.com/v1\",\"model\":\"gpt-a\",\"gateway\":\"OpenAI\"}",
        )
        .unwrap();

        let mut window = load_listing();
        let mut terminal = load_listing();
        terminal.settings.gateway = "OpenRouter".into();
        terminal.settings.model = "x/y".into();
        terminal.save().unwrap();
        // The window picks another model on the gateway it was already on.
        window.settings.model = "gpt-b".into();
        window.save().unwrap();
        let back = load_listing().settings;
        assert_eq!(
            (back.gateway.as_str(), back.model.as_str()),
            ("OpenAI", "gpt-b")
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_new_endpoint_or_custom_url_alone_leaves_the_other_process_model_be() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-endpoint");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        // The terminal's /model leaves the endpoint as it was, here empty.
        std::fs::write(
            dir.join("settings.json"),
            "{\"endpoint\":\"\",\"model\":\"gpt-a\",\"gateway\":\"OpenAI\"}",
        )
        .unwrap();

        let mut window = load_listing();
        let mut terminal = load_listing();
        terminal.settings.gateway = "OpenRouter".into();
        terminal.settings.model = "x/y".into();
        terminal.save().unwrap();
        // The window edits the Custom URL, then sends, which sets the
        // endpoint of the gateway it is still on.
        window.settings.custom_endpoint = "http://localhost:8000/v1".into();
        window.save().unwrap();
        window.settings.endpoint = "https://api.openai.com/v1".into();
        window.save().unwrap();
        let back = load_listing().settings;
        assert_eq!(
            (
                back.gateway.as_str(),
                back.model.as_str(),
                back.custom_endpoint.as_str()
            ),
            ("OpenRouter", "x/y", "http://localhost:8000/v1")
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_custom_model_is_saved_with_the_custom_url_it_was_listed_from() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-custom");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::write(
            dir.join("settings.json"),
            "{\"endpoint\":\"\",\"model\":\"gpt-a\",\"gateway\":\"OpenAI\",\"custom_endpoint\":\"http://one/v1\"}",
        )
        .unwrap();

        let mut window = load_listing();
        let mut terminal = load_listing();
        terminal.settings.custom_endpoint = "http://two/v1".into();
        terminal.save().unwrap();
        // The window picks a model from the list it loaded from the old URL.
        window.settings.gateway = "Custom".into();
        window.settings.model = "local/m".into();
        window.save().unwrap();
        let back = load_listing().settings;
        assert_eq!(
            (back.model.as_str(), back.custom_endpoint.as_str()),
            ("local/m", "http://one/v1")
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_key_saved_for_one_slot_leaves_every_other_slot_as_the_file_has_it() {
        use std::collections::HashMap;
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("keys-slot");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("keys.json");
        // Saved by the terminal after the window last read the file.
        std::fs::write(&path, "{\"OpenAI\":\"sk-1\",\"Custom\":\"c-2\"}").unwrap();

        let saved = super::save_key("OpenRouter", Some("or-3")).unwrap();
        let all = HashMap::from([
            ("OpenAI".to_owned(), "sk-1".to_owned()),
            ("Custom".to_owned(), "c-2".to_owned()),
            ("OpenRouter".to_owned(), "or-3".to_owned()),
        ]);
        assert_eq!(saved, all);
        assert_eq!(super::load_keys(), all);

        // Taking a key out takes out that slot and nothing else.
        let saved = super::save_key("Custom", None).unwrap();
        assert_eq!(saved.len(), 2);
        assert!(!saved.contains_key("Custom"));
        assert_eq!(super::load_keys(), saved);

        // With no file, taking a key out writes none.
        std::fs::remove_file(&path).unwrap();
        assert!(super::save_key("OpenAI", None).unwrap().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn a_session_deleted_elsewhere_is_not_written_back() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("deleted-elsewhere");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut terminal = Store::default();
        terminal.conversations.push(numbered(2));
        terminal.save().unwrap();
        let path = dir.join("sessions/1.jsonl");

        // The window lists the same session and deletes it.
        let mut window = load_listing();
        window.remove(0);
        assert!(!path.exists());

        // The terminal still has it open, and more is said and renamed there.
        terminal.conversations[0]
            .messages
            .push(said("user", "still here?"));
        terminal.conversations[0].title = "renamed".into();
        terminal.save().unwrap();
        assert!(!path.exists());

        // The window starts a new session, which is handed the freed id.
        let id = window.next_id();
        assert_eq!(id, 1);
        window.conversations.push(Conversation {
            id,
            title: "new".into(),
            messages: vec![said("user", "fresh")],
            ..Default::default()
        });
        window.save().unwrap();
        let new = std::fs::read_to_string(&path).unwrap();

        // The terminal says more again; none of it lands in the new session.
        terminal.conversations[0]
            .messages
            .push(said("user", "anyone?"));
        terminal.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), new);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn appearance_round_trips() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("appearance");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        for (appearance, grain) in [("paper", false), ("system", true), ("", false)] {
            let mut store = Store::default();
            store.settings.appearance = appearance.into();
            store.settings.grain = grain;
            store.save().unwrap();
            let back = load().settings;
            assert_eq!((back.appearance.as_str(), back.grain), (appearance, grain));
        }
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn two_writers_of_one_session_never_interleave_their_lines() {
        use std::io::Write;
        let dir = scratch("lock-interleave");
        let path = dir.join("1.jsonl");
        let writer = |tag: &'static str| {
            let path = path.clone();
            std::thread::spawn(move || {
                for _ in 0..20 {
                    let _lock = Lock::take(&path).unwrap();
                    // One line in two writes with a pause between, as a slow
                    // append would be.
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .unwrap();
                    file.write_all(tag.as_bytes()).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    file.write_all(b" end\n").unwrap();
                }
            })
        };
        let (first, second) = (writer("first"), writer("second"));
        first.join().unwrap();
        second.join().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 40);
        assert!(text.lines().all(|l| l == "first end" || l == "second end"));
    }

    #[test]
    fn a_save_that_cannot_take_the_lock_writes_nothing_and_the_next_catches_up() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("lock-timeout");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::read_to_string(&path).unwrap();

        store.conversations[0]
            .messages
            .push(said("assistant", "hi"));
        let held = Lock::take(&path).unwrap();
        assert!(store.save().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        drop(held);
        store.save().unwrap();
        let back = replay(&path).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].content, "hi");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_lock_whose_holder_is_gone_is_free_at_once() {
        let dir = scratch("lock-gone");
        let path = dir.join("1.jsonl");
        // What a process that dies holding the lock leaves behind: the file,
        // locked until the system closes it.
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .unwrap();
        holder.lock().unwrap();
        drop(holder);
        let started = std::time::Instant::now();
        assert!(Lock::take(&path).is_ok());
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn a_lock_file_is_never_removed_from_under_its_holder() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("lock-owner");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        for id in [1, 2] {
            store.conversations.push(Conversation {
                id,
                messages: vec![said("user", "hello")],
                ..Default::default()
            });
        }
        store.save().unwrap();
        // The lock files beside them are not sessions.
        assert_eq!(summaries().len(), 2);
        let (one, two) = (dir.join("sessions/1.jsonl"), dir.join("sessions/2.jsonl"));

        // Waiting in vain and letting go both leave the file where it is.
        let held = Lock::take(&one).unwrap();
        assert!(Lock::take(&one).is_err());
        assert!(one.with_extension("lock").exists());
        drop(held);
        assert!(one.with_extension("lock").exists());

        // Removing a session removes its lock file only if nobody holds it.
        let held = Lock::take(&two).unwrap();
        store.remove(1);
        assert!(!two.exists());
        assert!(two.with_extension("lock").exists());
        drop(held);
        store.remove(0);
        assert!(!one.exists());
        assert!(!one.with_extension("lock").exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn ids_reserved_at_the_same_time_differ() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("reserve");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        // Each thread is a Letronna starting up: its own empty store, the
        // same folder, all let go at once.
        let gate = std::sync::Arc::new(std::sync::Barrier::new(16));
        let racers: Vec<_> = (0..16)
            .map(|_| {
                let gate = gate.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    Store::default().reserve_id()
                })
            })
            .collect();
        let mut ids: Vec<u64> = racers.into_iter().map(|r| r.join().unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 16);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_reserved_session_is_listed_once_saved_and_not_before() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("reserve-save");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        let id = store.reserve_id();
        assert!(summaries().is_empty());

        // The file is already there, and the first save still writes its head.
        store.conversations.push(Conversation {
            id,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 1);
        let back = replay(&dir.join(format!("sessions/{id}.jsonl"))).unwrap();
        assert_eq!(back.id, id);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_transcript_replays_like_a_session() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let (n, mut helper) = Transcript::create(7, "explore", Some("work".into())).unwrap();
        assert_eq!(
            helper.path,
            dir.join("agents").join("7").join(format!("{n}.jsonl"))
        );
        helper.conversation.messages = numbered(4).messages;
        helper.conversation.todos = vec![Todo {
            text: "look".into(),
            status: "doing".into(),
        }];
        helper.save().unwrap();
        helper.conversation.rewind(2);
        helper
            .conversation
            .messages
            .push(said("assistant", "again"));
        helper.save().unwrap();

        let mut back = Transcript::open(7, n).unwrap();
        assert_eq!(back.path, helper.path);
        assert_eq!(back.conversation.title, "explore");
        assert_eq!(back.conversation.workspace, Some("work".into()));
        assert_eq!(back.conversation.todos.len(), 1);
        let contents: Vec<&str> = back
            .conversation
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(contents, ["0", "1", "again"]);

        // Opened again, it appends what is new rather than all of it.
        let before = std::fs::read_to_string(&back.path).unwrap();
        back.save().unwrap();
        assert_eq!(std::fs::read_to_string(&back.path).unwrap(), before);
        back.conversation.messages.push(said("user", "more"));
        back.save().unwrap();
        let after = std::fs::read_to_string(&back.path).unwrap();
        assert!(after.starts_with(&before));
        assert_eq!(after.lines().count(), before.lines().count() + 1);
        assert!(Transcript::open(7, n + 1).is_none());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn listing_and_search_ignore_transcripts() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript-listing");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            title: "the session".into(),
            messages: vec![said("user", "a fig in the session")],
            ..Default::default()
        });
        store.save().unwrap();
        let (_, mut helper) = Transcript::create(1, "the helper", None).unwrap();
        helper
            .conversation
            .messages
            .push(said("user", "a fig in the helper"));
        helper.save().unwrap();

        let listed: Vec<u64> = summaries().iter().map(|s| s.id).collect();
        assert_eq!(listed, [1]);
        let loaded: Vec<String> = load().conversations.into_iter().map(|c| c.title).collect();
        assert_eq!(loaded, ["the session"]);
        assert_eq!(load_listing().conversations.len(), 1);
        let found: Vec<String> = search("fig", 10).into_iter().map(|h| h.snippet).collect();
        assert_eq!(found, ["a fig in the session"]);

        let ctx = crate::tools::ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let listed = crate::tools::run("list_sessions", "{}", &ctx).unwrap();
        assert!(
            listed.contains("the session") && !listed.contains("the helper"),
            "{listed}"
        );
        let found = crate::tools::run("search_sessions", r#"{"query":"fig"}"#, &ctx).unwrap();
        assert!(
            found.contains("in the session") && !found.contains("in the helper"),
            "{found}"
        );
        // Nor can the model open the file by its path: it is in the data
        // folder, even with that folder as the workspace.
        let args = serde_json::json!({ "path": helper.path }).to_string();
        let read = crate::tools::run("read_file", &args, &ctx);
        assert!(
            read.as_ref()
                .is_err_and(|e| e.contains("Letronna's own folder")),
            "{read:?}"
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn removing_a_session_removes_its_helpers() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript-remove");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.conversations.push(Conversation {
            id: 2,
            ..numbered(1)
        });
        store.save().unwrap();
        for session in [1, 1, 2] {
            Transcript::create(session, "helper", None).unwrap();
        }

        store.remove(0);
        assert!(!dir.join("sessions").join("1.jsonl").exists());
        assert!(!dir.join("agents").join("1").exists());
        assert!(Transcript::open(1, 1).is_none());
        // Another session's helpers are its own.
        assert!(Transcript::open(2, 1).is_some());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn two_transcripts_made_at_once_get_different_numbers() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript-race");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        // More than two, all let go together, to give a race every chance.
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));
        let makers: Vec<_> = (0..8)
            .map(|_| {
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    Transcript::create(3, "helper", None).unwrap().0
                })
            })
            .collect();
        let mut numbers: Vec<u32> = makers.into_iter().map(|m| m.join().unwrap()).collect();
        numbers.sort();
        assert_eq!(numbers, (1..=8).collect::<Vec<u32>>());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn removing_a_session_removes_the_copies_of_its_files() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("remove-history");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();
        let kept = crate::history::folder(1);
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("copy"), "old").unwrap();

        store.remove(0);
        assert!(!dir.join("sessions/1.jsonl").exists());
        assert!(!kept.exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }
}
