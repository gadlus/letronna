//! What Letronna keeps from one session to the next.
//!
//! Three Markdown files in the data folder, all of them the user's to open
//! and edit. `SOUL.md` says who Letronna is and takes the place of the
//! built-in first line of the prompt. `USER.md` holds what it knows about the
//! user and `MEMORY.md` its own notes; the model keeps those two with the
//! `memory` tool. When a project folder is attached, its `AGENTS.md` (or
//! `CLAUDE.md`) joins them.
//!
//! USER.md and MEMORY.md go into every request, so each has a small budget --
//! Hermes Agent's, which has run this way for a long time -- and a full one
//! makes the model rewrite rather than pile up. Their entries are separated
//! the way Hermes separates them, so its files can be copied across as-is.
//!
//! Everything is read on each request rather than once per session, which
//! keeps this stateless like the context window. The price is one missed
//! prompt cache after the model writes to memory, which is not often.

use crate::chat::Connection;
use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::path::Path;

/// The line between entries, as Hermes Agent writes it.
const SEPARATOR: &str = "\n§\n";
/// USER.md's budget in characters: about 500 tokens.
const USER_LIMIT: usize = 1_375;
/// MEMORY.md's budget in characters: about 800 tokens.
const MEMORY_LIMIT: usize = 2_200;
/// A cap on SOUL.md and project instructions. People write these rather than
/// the model, so it only stops a huge one crowding out the conversation.
const FILE_LIMIT: usize = 20_000;

/// How the model is told to use its memory, when it has the tool to.
const GUIDANCE: &str = " You have a memory that outlasts this session: what you know about the user (USER.md) and your own notes (MEMORY.md), shown below when there is anything in them, and kept with the memory tool. Save what will still matter in a later session -- the user's name and preferences, their setup, corrections they gave you -- and not the progress of the task at hand. Write facts, not instructions to yourself.";

/// Who Letronna is, from SOUL.md, when the user has written one.
pub fn soul() -> Option<String> {
    read_capped(&crate::chat::data_dir().join("SOUL.md"))
}

/// What follows the built-in lines of the prompt: memory, the project's own
/// instructions, the helper types files define, and how to use them. Nothing
/// where others read the replies.
pub fn context(connection: &Connection) -> String {
    context_in(&crate::chat::data_dir(), connection)
}

fn context_in(dir: &Path, connection: &Connection) -> String {
    if connection.remote.as_ref().is_some_and(|r| r.shared) {
        return String::new();
    }
    let mut out = String::new();
    if connection.tools {
        out.push_str(GUIDANCE);
    }
    for target in [Target::User, Target::Memory] {
        let entries = entries(&dir.join(target.file()));
        if !entries.is_empty() {
            out.push_str(&format!(
                "\n\n{} ({}, {}/{} characters):\n{}",
                target.title(),
                target.file(),
                size(&entries),
                target.limit(),
                entries.join(SEPARATOR)
            ));
        }
    }
    let project = connection.workspace.as_deref().and_then(|workspace| {
        ["AGENTS.md", "CLAUDE.md"]
            .into_iter()
            .find_map(|name| read_capped(&workspace.join(name)).map(|text| (name, text)))
    });
    if let Some((name, text)) = project {
        out.push_str(&format!(
            "\n\nThe project's instructions, from {name}:\n{text}"
        ));
    }
    if connection.tools
        && let Some(types) = crate::agent_defs::advert_in(dir, connection.workspace.as_deref())
    {
        out.push_str(&format!("\n\n{types}"));
    }
    out
}

/// A file people write, trimmed and kept under `FILE_LIMIT`. None when it is
/// missing or empty.
fn read_capped(path: &Path) -> Option<String> {
    let text = std::fs::read_to_string(path).ok()?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    if text.chars().count() <= FILE_LIMIT {
        return Some(text.to_owned());
    }
    let cut: String = text.chars().take(FILE_LIMIT).collect();
    Some(format!("{cut}\n… [cut at {FILE_LIMIT} characters]"))
}

#[derive(Clone, Copy)]
enum Target {
    User,
    Memory,
}

impl Target {
    fn parse(name: &str) -> Result<Self, String> {
        match name {
            "user" => Ok(Self::User),
            "memory" => Ok(Self::Memory),
            _ => Err("target is \"user\" or \"memory\".".into()),
        }
    }

    fn file(self) -> &'static str {
        match self {
            Self::User => "USER.md",
            Self::Memory => "MEMORY.md",
        }
    }

    fn limit(self) -> usize {
        match self {
            Self::User => USER_LIMIT,
            Self::Memory => MEMORY_LIMIT,
        }
    }

    fn title(self) -> &'static str {
        match self {
            Self::User => "What you know about the user",
            Self::Memory => "Your notes",
        }
    }
}

/// The entries in a store, oldest first. A file edited on Windows has CRLF
/// line ends, which would hide the separator.
fn entries(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .unwrap_or_default()
        .replace("\r\n", "\n")
        .split(SEPARATOR)
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_owned)
        .collect()
}

/// How much of the budget the entries use, separators included.
fn size(entries: &[String]) -> usize {
    entries.join(SEPARATOR).chars().count()
}

/// What a store holds, as the tool reports it.
fn listing(target: Target, entries: &[String]) -> String {
    if entries.is_empty() {
        return format!("{} is empty.", target.file());
    }
    format!(
        "{} ({}/{} characters):\n{}",
        target.file(),
        size(entries),
        target.limit(),
        entries.join(SEPARATOR)
    )
}

/// The one entry that contains `old_text`.
fn find(entries: &[String], old_text: &str) -> Result<usize, String> {
    let old_text = old_text.trim();
    if old_text.is_empty() {
        return Err("Quote part of the entry in old_text.".into());
    }
    let mut hits = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.contains(old_text))
        .map(|(at, _)| at);
    match (hits.next(), hits.next()) {
        (Some(at), None) => Ok(at),
        (None, _) => Err("No entry contains that text.".into()),
        (Some(_), Some(_)) => {
            Err("More than one entry contains that text; quote more of the one you mean.".into())
        }
    }
}

/// Change one store and report what it holds now. Over budget, nothing is
/// written and the model is shown the store so it can make room.
fn apply(
    dir: &Path,
    target: Target,
    action: &str,
    content: &str,
    old_text: &str,
) -> Result<String, String> {
    let path = dir.join(target.file());
    let before = entries(&path);
    let mut after = before.clone();
    let content = content.trim();
    match action {
        "add" if content.is_empty() => return Err("Say what to keep in content.".into()),
        "add" if before.iter().any(|entry| entry == content) => {
            return Ok(format!("Already kept.\n\n{}", listing(target, &before)));
        }
        "add" => after.push(content.to_owned()),
        "replace" if content.is_empty() => {
            return Err("Say what the entry becomes in content, or remove it instead.".into());
        }
        "replace" => after[find(&before, old_text)?] = content.to_owned(),
        "remove" => {
            after.remove(find(&before, old_text)?);
        }
        other => return Err(format!("Unknown action `{other}`: add, replace or remove.")),
    }
    let used = size(&after);
    if used > target.limit() {
        return Err(format!(
            "That would make {} {used} of {} characters. Replace or remove an entry first.\n\n{}",
            target.file(),
            target.limit(),
            listing(target, &before)
        ));
    }
    let text = if after.is_empty() {
        String::new()
    } else {
        format!("{}\n", after.join(SEPARATOR))
    };
    // Written beside and moved over, so a crash cannot leave half a file.
    let temp = path.with_extension("tmp");
    std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&temp, text))
        .and_then(|_| std::fs::rename(&temp, &path))
        .map_err(|e| format!("Could not save {}: {e}", target.file()))?;
    Ok(listing(target, &after))
}

/// The model's hand on USER.md and MEMORY.md.
pub struct Remember;

impl Tool for Remember {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn description(&self) -> &'static str {
        "Keep something for later sessions, or correct what is kept. target \"user\" is what you know about the user (USER.md); target \"memory\" is your own notes (MEMORY.md). Both are shown to you at the start of every session and have small budgets. action \"add\" keeps content as a new entry; \"replace\" swaps the entry containing old_text for content; \"remove\" deletes the entry containing old_text. When one is full, replace or remove before adding."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "action": {"type": "string", "enum": ["add", "replace", "remove"]},
            "target": {"type": "string", "enum": ["user", "memory"]},
            "content": {"type": "string", "description": "The entry to keep, or what the entry becomes."},
            "old_text": {"type": "string", "description": "Part of the entry to replace or remove, unique to it."}
        }, "required": ["action", "target"]})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let text = |key: &str| args[key].as_str().unwrap_or_default();
        let target = Target::parse(text("target"))?;
        apply(
            &crate::chat::data_dir(),
            target,
            text("action"),
            text("content"),
            text("old_text"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::Remote;

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-memory-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn adds_replaces_and_removes_entries() {
        let dir = scratch("entries");
        apply(&dir, Target::User, "add", "Name is Hugo.", "").unwrap();
        apply(&dir, Target::User, "add", "Prefers short answers.", "").unwrap();
        let kept = std::fs::read_to_string(dir.join("USER.md")).unwrap();
        assert_eq!(kept, "Name is Hugo.\n§\nPrefers short answers.\n");

        apply(
            &dir,
            Target::User,
            "replace",
            "Prefers terse answers.",
            "short",
        )
        .unwrap();
        let out = apply(&dir, Target::User, "remove", "", "Hugo").unwrap();
        assert!(out.starts_with("USER.md ("), "{out}");
        assert_eq!(entries(&dir.join("USER.md")), ["Prefers terse answers."]);
        // Saying the same thing twice keeps one copy.
        apply(&dir, Target::User, "add", "Prefers terse answers.", "").unwrap();
        assert_eq!(entries(&dir.join("USER.md")).len(), 1);
    }

    #[test]
    fn a_full_store_refuses_and_shows_what_is_in_it() {
        let dir = scratch("full");
        let long = "x".repeat(USER_LIMIT - 10);
        apply(&dir, Target::User, "add", &long, "").unwrap();
        let refused = apply(&dir, Target::User, "add", "one more fact here", "").unwrap_err();
        assert!(refused.contains("Replace or remove"), "{refused}");
        assert!(
            refused.contains(&long),
            "the model is shown what to make room in"
        );
        assert_eq!(entries(&dir.join("USER.md")), [long]);
    }

    #[test]
    fn replace_and_remove_need_text_that_picks_one_entry() {
        let dir = scratch("pick");
        apply(&dir, Target::Memory, "add", "Uses PowerShell.", "").unwrap();
        apply(&dir, Target::Memory, "add", "Uses Rust 2024.", "").unwrap();
        assert!(apply(&dir, Target::Memory, "remove", "", "Uses").is_err());
        assert!(apply(&dir, Target::Memory, "remove", "", "Python").is_err());
        assert!(apply(&dir, Target::Memory, "remove", "", "").is_err());
        assert!(apply(&dir, Target::Memory, "remove", "", "Rust").is_ok());
    }

    #[test]
    fn memory_and_project_instructions_reach_the_prompt_but_not_a_shared_chat() {
        let dir = scratch("context");
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("AGENTS.md"),
            "Run cargo test before committing.",
        )
        .unwrap();
        // A file edited on Windows still splits into entries.
        std::fs::write(
            dir.join("MEMORY.md"),
            "Works late.\r\n§\r\nLikes tables.\r\n",
        )
        .unwrap();
        let agents = project.join(".letronna").join("agents");
        std::fs::create_dir_all(&agents).unwrap();
        std::fs::write(
            agents.join("docs.md"),
            "---\ndescription: Writes docs.\n---\n",
        )
        .unwrap();
        let connection = Connection {
            tools: true,
            workspace: Some(project),
            ..Default::default()
        };
        let text = context_in(&dir, &connection);
        assert!(text.contains("memory tool"));
        assert!(
            text.contains(
                "Your notes (MEMORY.md, 27/2200 characters):\nWorks late.\n§\nLikes tables."
            ),
            "{text}"
        );
        assert!(!text.contains("USER.md,"), "an empty store is left out");
        assert!(text.contains("from AGENTS.md:\nRun cargo test before committing."));
        assert!(
            text.ends_with("agent tool's type: docs (Writes docs)."),
            "{text}"
        );

        let shared = Connection {
            remote: Some(Remote {
                shared: true,
                ..Default::default()
            }),
            ..connection
        };
        assert_eq!(context_in(&dir, &shared), "");
    }

    #[test]
    fn a_huge_file_is_cut_short() {
        let dir = scratch("huge");
        std::fs::write(dir.join("SOUL.md"), "y".repeat(FILE_LIMIT + 50)).unwrap();
        let soul = read_capped(&dir.join("SOUL.md")).unwrap();
        assert!(soul.ends_with(&format!("[cut at {FILE_LIMIT} characters]")));
        assert!(read_capped(&dir.join("missing.md")).is_none());
    }
}
