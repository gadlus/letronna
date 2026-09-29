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
use std::io::Write;
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
#[derive(Default, Serialize, Deserialize)]
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
    /// How hard the model should think: empty, or low, medium or high.
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
}

#[derive(Default)]
pub struct Store {
    pub conversations: Vec<Conversation>,
    pub active: usize,
    pub settings: Settings,
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
    pub fn next_id(&self) -> u64 {
        let highest = self.conversations.iter().map(|c| c.id).max().unwrap_or(0);
        let on_disk = session_ids().into_iter().max().unwrap_or(0);
        highest.max(on_disk) + 1
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
        let bytes = serde_json::to_vec_pretty(&self.settings)?;
        let path = settings_path();
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, bytes)?;
        std::fs::rename(temp, path)
    }

    /// Forget a session and remove its file.
    pub fn remove(&mut self, index: usize) {
        if index >= self.conversations.len() {
            return;
        }
        let gone = self.conversations.remove(index);
        let _ = std::fs::remove_file(session_path(gone.id));
        let _ = std::fs::remove_dir_all(crate::tools::results_dir(gone.id));
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
        let path = session_path(self.id);
        let _lock = Lock::take(&path);
        let fresh = !path.exists();
        if !fresh && self.flushed == self.messages.len() && self.rewound.is_none() && !meta_changed
        {
            return Ok(());
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let mut out = String::new();
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

/// Rebuild a session by replaying its file. A line that will not parse is
/// skipped rather than failing the session: losing one message is better than
/// losing the conversation it was part of.
fn replay(path: &std::path::Path) -> Option<Conversation> {
    let text = std::fs::read_to_string(path).ok()?;
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
    if conversation.id == 0 {
        return None;
    }
    conversation.flushed = conversation.messages.len();
    conversation.written = Some(conversation.meta());
    Some(conversation)
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
            let text = std::fs::read_to_string(session_path(id)).ok()?;
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
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        // Most sessions never mention it, which the whole file says at once.
        // Quotes and backslashes are escaped on disk, so a query holding
        // either cannot be checked that way and reads the messages instead.
        if !query.contains(['"', '\\']) && !text.to_lowercase().contains(&query) {
            continue;
        }
        let (mut id, mut title, mut updated) = (0, String::new(), 0);
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
                        hits.push((said, message.role, snippet));
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
/// lost. The lock is a file beside the session: made with create_new, which
/// is atomic everywhere, and removed when the guard goes.
pub struct Lock(PathBuf);

impl Lock {
    /// Wait for the lock, but not forever. A process that died holding one
    /// would otherwise keep every other process out of that session for good,
    /// so a lock nobody has touched for a while is taken anyway.
    fn take(path: &std::path::Path) -> Self {
        let lock = path.with_extension("lock");
        for _ in 0..40 {
            if std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&lock)
                .is_ok()
            {
                return Self(lock);
            }
            let stale = std::fs::metadata(&lock)
                .and_then(|m| m.modified())
                .map(|t| t.elapsed().unwrap_or_default() > std::time::Duration::from_secs(10))
                .unwrap_or(true);
            if stale {
                let _ = std::fs::remove_file(&lock);
                continue;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        // A second of waiting is long enough that something is wrong. Writing
        // without the lock risks a torn line; not writing loses the message
        // for certain, so it writes.
        Self(lock)
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn session_ids() -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            e.path()
                .file_stem()
                .and_then(|s| s.to_str())
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
    Store {
        conversations,
        active: 0,
        settings: settings(),
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

fn settings() -> Settings {
    let mut s: Settings = std::fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    retire_gateway(&mut s);
    s
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
    let mut conversations: Vec<Conversation> = session_ids()
        .into_iter()
        .filter_map(|id| replay(&session_path(id)))
        .collect();
    conversations.sort_by_key(|c| c.updated);
    let settings: Settings = std::fs::read(settings_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let active = conversations
        .iter()
        .position(|c| c.id == settings.active_id)
        .unwrap_or(conversations.len().saturating_sub(1));
    Store {
        conversations,
        active,
        settings,
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
    write_private(&keys_path(), &serde_json::to_vec_pretty(keys)?)
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
    std::fs::read(keys_path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
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
pub(crate) mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    #[cfg(unix)]
    use super::write_private;
    use super::{Conversation, Store, Todo, fill, load_listing, replay, search_in, summaries};
    use crate::chat::Message;

    /// Held by every test that points `AGENT_DATA_DIR` somewhere: it is one
    /// variable for the whole process, and tests run side by side.
    pub(crate) static DATA_DIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

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
    fn removing_a_session_removes_the_outputs_saved_for_it() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("remove-results");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();
        let (mine, other) = (crate::tools::results_dir(1), crate::tools::results_dir(2));
        for results in [&mine, &other] {
            std::fs::create_dir_all(results).unwrap();
            std::fs::write(results.join("1-0.txt"), "saved").unwrap();
        }
        store.remove(0);
        assert!(!mine.exists() && !dir.join("sessions/1.jsonl").exists());
        assert!(other.join("1-0.txt").is_file());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }
}
