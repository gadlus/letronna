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
use std::path::PathBuf;
use std::io::Write;

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
    /// Project folder attached to this session, if any.
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    /// The model's own checklist for the work in progress.
    #[serde(default)]
    pub todos: Vec<Todo>,
    /// Messages already on disk. Everything past this is appended on save.
    /// Only `flush` should touch it; it is visible because callers build a
    /// Conversation with `..Default::default()`.
    #[serde(skip)]
    pub flushed: usize,
    /// The descriptive fields as last written, so save appends a `meta` line
    /// only when one of them actually changed.
    #[serde(skip)]
    pub written: Option<Meta>,
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
}

/// Everything that is not a session.
#[derive(Default, Serialize, Deserialize)]
pub struct Settings {
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub gateway: String,
    #[serde(default)]
    pub presence: String,
    /// Run write and command tools without asking.
    #[serde(default)]
    pub skip_approvals: bool,
    /// Which session was open last, by id rather than position.
    #[serde(default)]
    pub active_id: u64,
}

#[derive(Default)]
pub struct Store {
    pub conversations: Vec<Conversation>,
    pub active: usize,
    pub settings: Settings,
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

    /// Append the head (first time), any new messages, and the descriptive
    /// fields if they have moved since the last write.
    fn flush(&mut self) -> std::io::Result<()> {
        let path = session_path(self.id);
        let fresh = !path.exists();
        let meta = self.meta();
        let meta_changed = self.written.as_ref() != Some(&meta);
        if !fresh && self.flushed == self.messages.len() && !meta_changed {
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
        for message in &self.messages[self.flushed.min(self.messages.len())..] {
            push(&mut out, &Record::Message(message.clone()));
        }
        file.write_all(out.as_bytes())?;
        self.flushed = self.messages.len();
        self.written = Some(meta);
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
            };
            for line in text.lines().filter(|l| !l.trim().is_empty()) {
                match serde_json::from_str::<Record>(line) {
                    Ok(Record::Head { created, .. }) => summary.updated = created,
                    Ok(Record::Meta(meta)) => {
                        summary.title = meta.title;
                        summary.updated = meta.updated;
                        summary.workspace = meta.workspace;
                    }
                    Ok(Record::Message(_)) => summary.messages += 1,
                    Err(_) => {}
                }
            }
            Some(summary)
        })
        .collect();
    out.sort_by_key(|s| std::cmp::Reverse(s.updated));
    out
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
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{Conversation, Store, Todo, replay};
    use crate::chat::Message;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn said(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: text.into(),
            ..Default::default()
        }
    }

    #[test]
    fn appends_only_what_is_new_and_replays_to_the_same_thing() {
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
}
