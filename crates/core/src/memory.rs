//! What Letronna keeps from one session to the next.
//!
//! Three Markdown files in the data folder, all of them the user's to open
//! and edit. `SOUL.md` says who Letronna is and takes the place of the
//! built-in first line of the prompt. `USER.md` holds what it knows about the
//! user and `MEMORY.md` its own notes; the model keeps those two with the
//! `memory` tool. When a project folder is attached, the `AGENTS.md` (or
//! `CLAUDE.md`) in it and in each folder above it up to the repository's root
//! join them, so a subfolder of a repository keeps the repository's rules.
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
/// A cap on the project's instruction files together: about 7,500 tokens, so
/// a project folder deep in a repository costs at most half as much again as
/// one at its root. A file past it is only named, in one line.
const INSTRUCTIONS_LIMIT: usize = 30_000;

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
    let project = connection
        .workspace
        .as_deref()
        .map(instructions)
        .unwrap_or_default();
    // A cloned repository's files are anyone's; one that looks planted is
    // marked for the model rather than dropped.
    let project: Vec<(String, String)> = project
        .into_iter()
        .map(|(label, text)| (label, crate::untrusted::vetted("this file", &text)))
        .collect();
    match project.as_slice() {
        [] => {}
        [(label, text)] => out.push_str(&format!(
            "\n\nThe project's instructions, from {label}:\n{text}"
        )),
        _ => {
            out.push_str("\n\nThe project's instructions, from the repository's root down to the project folder; where two disagree, the later one wins.");
            for (label, text) in &project {
                out.push_str(&format!("\n\nFrom {label}:\n{text}"));
            }
        }
    }
    if connection.tools
        && let Some(types) = crate::agent_defs::advert_in(dir, connection.workspace.as_deref())
    {
        out.push_str(&format!("\n\n{types}"));
    }
    out
}

/// The project's instructions, each labelled with its path from `workspace`:
/// the AGENTS.md, else CLAUDE.md, of each folder from the repository's root
/// down to `workspace`, the root's first. The nearest are read first, so when
/// they are too long together it is the root's that are cut.
fn instructions(workspace: &Path) -> Vec<(String, String)> {
    // A `.git` file rather than a folder marks the top of a worktree or a
    // submodule, which is as far as its own rules go.
    let dirs = folders(workspace, |dir| dir.join(".git").symlink_metadata().is_ok());
    let mut left = INSTRUCTIONS_LIMIT;
    let mut found = Vec::new();
    for (up, dir) in dirs.into_iter().enumerate() {
        let file = ["AGENTS.md", "CLAUDE.md"].into_iter().find_map(|name| {
            let label = format!("{}{name}", "../".repeat(up));
            read_ends(&dir.join(name), &label, left.min(FILE_LIMIT)).map(|text| (label, text))
        });
        if let Some((label, text)) = file {
            left = left.saturating_sub(text.chars().count());
            found.push((label, text));
        }
    }
    found.reverse();
    found
}

/// `workspace` and the folders above it, nearest first, up to the first that
/// `is_root` says is a repository's top. Only `workspace` when none is: the
/// folders above a project with no repository are no part of it.
fn folders(workspace: &Path, is_root: impl Fn(&Path) -> bool) -> Vec<&Path> {
    let mut found = Vec::new();
    // A relative path runs out in "", which would mean the process's own
    // folder.
    for dir in workspace
        .ancestors()
        .take_while(|dir| !dir.as_os_str().is_empty())
    {
        found.push(dir);
        if is_root(dir) {
            return found;
        }
    }
    vec![workspace]
}

/// A file people write, trimmed and kept under `max` characters: past that,
/// its first 70% and last 20% of them, with a line between pointing
/// `read_file` at `label`. None when it is missing or empty, or not a plain
/// file or UTF-8, as for `read_capped`. Only as much of each end is read as
/// could be kept, however long the file.
fn read_ends(path: &Path, label: &str, max: usize) -> Option<String> {
    use std::io::{Read, Seek, SeekFrom};
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    // Enough for `max` characters of any width and one more, from each end.
    let most = (max as u64 + 1) * 4;
    let mut file = std::fs::File::open(path).ok()?;
    let mut head = Vec::new();
    if meta.len() <= most * 2 {
        // Taken still: the file may have grown since its length was read.
        file.take(most * 2).read_to_end(&mut head).ok()?;
        let text = decode(head)?;
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        if text.chars().count() <= max {
            return Some(text.to_owned());
        }
        return Some(cut_middle(text, text, label, max));
    }
    (&file).take(most).read_to_end(&mut head).ok()?;
    let mut tail = Vec::new();
    file.seek(SeekFrom::End(-(most as i64))).ok()?;
    file.take(most).read_to_end(&mut tail).ok()?;
    // The seek can land inside a character, whose remaining bytes are no
    // text.
    let split = tail
        .iter()
        .take(3)
        .take_while(|b| *b & 0xC0 == 0x80)
        .count();
    tail.drain(..split);
    let (head, tail) = (decode(head)?, decode(tail)?);
    Some(cut_middle(head.trim_start(), tail.trim_end(), label, max))
}

/// The first 70% of `max` characters of `head` and the last 20% of `tail`,
/// with a line between saying where the whole file is.
fn cut_middle(head: &str, tail: &str, label: &str, max: usize) -> String {
    let first: String = head.chars().take(max * 7 / 10).collect();
    let skip = tail.chars().count().saturating_sub(max / 5);
    let last: String = tail.chars().skip(skip).collect();
    let line = format!("… [cut here to fit; read_file `{label}` has the whole file]");
    [first.as_str(), &line, &last]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A file people write, trimmed and kept under `FILE_LIMIT`. None when it is
/// missing or empty, or not a plain file: an AGENTS.md in a cloned repository
/// that links to /dev/zero would otherwise be read until memory ran out, and
/// on every request.
fn read_capped(path: &Path) -> Option<String> {
    use std::io::Read;
    if !std::fs::metadata(path).ok()?.is_file() {
        return None;
    }
    // Enough for `FILE_LIMIT` characters of any width and one more, so a cut
    // is still seen as one.
    let most = (FILE_LIMIT as u64 + 1) * 4;
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(most)
        .read_to_end(&mut bytes)
        .ok()?;
    let text = decode(bytes)?;
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

/// `bytes` as text, less the last character when a read stopped short inside
/// it. None when anything else in them is not UTF-8.
fn decode(bytes: Vec<u8>) -> Option<String> {
    match String::from_utf8(bytes) {
        Ok(text) => Some(text),
        Err(e) if e.utf8_error().error_len().is_none() => {
            let valid = e.utf8_error().valid_up_to();
            let mut bytes = e.into_bytes();
            bytes.truncate(valid);
            String::from_utf8(bytes).ok()
        }
        Err(_) => None,
    }
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

/// The entries in a store, oldest first, or none when it cannot be read.
fn entries(path: &Path) -> Vec<String> {
    split(&read_store(path).unwrap_or_default())
}

/// A store's text with any BOM taken off, and empty when there is no file.
fn read_store(path: &Path) -> std::io::Result<String> {
    match std::fs::read_to_string(path) {
        // A BOM is not whitespace to trim, so it would stick to the first entry.
        Ok(text) => Ok(text.strip_prefix('\u{feff}').unwrap_or(&text).to_owned()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

/// A store's text as its entries, oldest first. A file edited on Windows has
/// CRLF line ends, which would hide the separator.
fn split(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
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

/// Whether a person wrote the store: it does not read back the way Letronna
/// writes it, or it holds more than the budget Letronna keeps to.
fn hand_written(text: &str, entries: &[String], limit: usize) -> bool {
    let text = text.replace("\r\n", "\n");
    size(entries) > limit || text.strip_suffix('\n').unwrap_or(&text) != entries.join(SEPARATOR)
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
    // The window and the terminal both save here, so the store is read again
    // under the lock and neither writes over what the other just kept.
    let _lock = crate::store::Lock::take(&path).map_err(|e| {
        format!(
            "{} is being written by another Letronna, so nothing was changed: {e}",
            target.file()
        )
    })?;
    // Taken as empty, a file that is there but cannot be read would be
    // replaced by the one new entry.
    let raw = read_store(&path).map_err(|e| {
        format!(
            "{} could not be read, so nothing was changed: {e}",
            target.file()
        )
    })?;
    let before = split(&raw);
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
    // Notes a person wrote without separators are one entry of several lines,
    // and read back unchanged, so the layout cannot tell them from Letronna's.
    // A replace or remove drops that entry whole, so the file is copied first.
    let drops_lines = before
        .iter()
        .any(|entry| entry.contains('\n') && !after.contains(entry));
    let backed_up = if drops_lines || hand_written(&raw, &before, target.limit()) {
        let backup = format!("{}.bak.{}", target.file(), crate::chat::now());
        std::fs::copy(&path, dir.join(&backup)).map_err(|e| {
            format!(
                "Could not back up {}, so nothing was changed: {e}",
                target.file()
            )
        })?;
        format!(
            "{} may have held notes written by hand, so that version was copied to {backup} first.\n\n",
            target.file()
        )
    } else {
        String::new()
    };
    // Written beside and moved over, so a crash cannot leave half a file. The
    // name is per process because a lock that waited too long is taken anyway.
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    std::fs::create_dir_all(dir)
        .and_then(|_| std::fs::write(&temp, text))
        .and_then(|_| std::fs::rename(&temp, &path))
        .map_err(|e| {
            let _ = std::fs::remove_file(&temp);
            format!("Could not save {}: {e}", target.file())
        })?;
    Ok(format!("{backed_up}{}", listing(target, &after)))
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
        // Read only as far as the cut, even in characters four bytes wide.
        std::fs::write(dir.join("AGENTS.md"), "\u{1f600}".repeat(FILE_LIMIT * 3)).unwrap();
        let wide = read_capped(&dir.join("AGENTS.md")).unwrap();
        assert!(wide.ends_with(&format!("[cut at {FILE_LIMIT} characters]")));
        assert!(wide.starts_with("\u{1f600}"));
    }

    #[test]
    fn instructions_that_are_not_a_plain_file_are_passed_over() {
        let dir = scratch("not-a-file");
        std::fs::create_dir_all(dir.join("AGENTS.md")).unwrap();
        assert!(read_capped(&dir.join("AGENTS.md")).is_none());
        // What a cloned repository can hold on macOS and Linux, and which
        // never ends when read.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("/dev/zero", dir.join("CLAUDE.md")).unwrap();
            assert!(read_capped(&dir.join("CLAUDE.md")).is_none());
        }
    }

    fn backups(dir: &Path) -> Vec<String> {
        std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
            .filter(|name| name.contains(".bak."))
            .collect()
    }

    #[test]
    fn a_store_that_cannot_be_read_is_not_overwritten() {
        let dir = scratch("unreadable");
        // What Windows PowerShell's `>` writes: UTF-16 with a BOM.
        let utf16: Vec<u8> = [0xFF, 0xFE]
            .into_iter()
            .chain("Name is Hugo.".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        std::fs::write(dir.join("USER.md"), &utf16).unwrap();
        let refused = apply(&dir, Target::User, "add", "Prefers short answers.", "").unwrap_err();
        assert!(
            refused.contains("USER.md could not be read, so nothing was changed"),
            "{refused}"
        );
        assert_eq!(std::fs::read(dir.join("USER.md")).unwrap(), utf16);
    }

    #[test]
    fn a_bom_is_left_off_the_first_entry() {
        let dir = scratch("bom");
        let path = dir.join("MEMORY.md");
        std::fs::write(&path, "\u{feff}Uses PowerShell.\r\n§\r\nUses Rust.\r\n").unwrap();
        assert_eq!(entries(&path), ["Uses PowerShell.", "Uses Rust."]);
        apply(
            &dir,
            Target::Memory,
            "replace",
            "Uses PowerShell 7.",
            "PowerShell",
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "Uses PowerShell 7.\n§\nUses Rust.\n"
        );
        assert!(
            backups(&dir).is_empty(),
            "a file saved by a Windows editor is not taken for one written by hand"
        );
    }

    #[test]
    fn two_writers_keep_both_entries() {
        let dir = scratch("writers");
        let path = dir.join("USER.md");
        // Holding the lock stands in for another process part way through a save.
        let lock = crate::store::Lock::take(&path);
        let writer = {
            let dir = dir.clone();
            std::thread::spawn(move || {
                apply(&dir, Target::User, "add", "Prefers short answers.", "")
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        std::fs::write(&path, "Name is Hugo.\n").unwrap();
        drop(lock);
        writer.join().unwrap().unwrap();
        assert_eq!(entries(&path), ["Name is Hugo.", "Prefers short answers."]);
    }

    #[test]
    fn a_save_leaves_another_process_temp_file_alone() {
        let dir = scratch("temp");
        // An older build running beside this one still saves through USER.tmp.
        std::fs::write(dir.join("USER.tmp"), "half of another save").unwrap();
        apply(&dir, Target::User, "add", "Name is Hugo.", "").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("USER.tmp")).unwrap(),
            "half of another save"
        );
        assert_eq!(entries(&dir.join("USER.md")), ["Name is Hugo."]);
    }

    #[test]
    fn a_hand_written_file_is_backed_up_before_it_is_replaced() {
        let dir = scratch("backup");
        // Notes as an editor saves them read back unchanged, as one entry.
        let notes = "Name: Hugo\nLives in Malaysia\nPrefers short answers\n";
        std::fs::write(dir.join("USER.md"), notes).unwrap();
        let out = apply(
            &dir,
            Target::User,
            "replace",
            "Lives in Singapore.",
            "Malaysia",
        )
        .unwrap();
        let saved = backups(&dir);
        assert_eq!(saved.len(), 1);
        assert!(out.contains(&saved[0]), "{out}");
        assert_eq!(std::fs::read_to_string(dir.join(&saved[0])).unwrap(), notes);
        assert_eq!(entries(&dir.join("USER.md")), ["Lives in Singapore."]);

        // Letronna never saves more than the budget, even laid out its way.
        let long = format!("{}\n", "x".repeat(MEMORY_LIMIT + 1));
        std::fs::write(dir.join("MEMORY.md"), &long).unwrap();
        apply(&dir, Target::Memory, "remove", "", "x").unwrap();
        assert_eq!(backups(&dir).len(), 2);

        // A file Letronna wrote itself is not copied again.
        apply(&dir, Target::User, "add", "Name is Hugo.", "").unwrap();
        assert_eq!(backups(&dir).len(), 2);

        // A file laid out some other way is copied even before an add.
        let other = scratch("layout");
        std::fs::write(other.join("USER.md"), "Likes tea.\n\n\n").unwrap();
        apply(&other, Target::User, "add", "Works late.", "").unwrap();
        assert_eq!(backups(&other).len(), 1);
    }

    #[test]
    fn instructions_are_read_from_the_repository_root_down_to_the_project() {
        let dir = scratch("tree");
        let repo = dir.join("repo");
        let project = repo.join("crates").join("core");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(repo.join(".git")).unwrap();
        std::fs::write(dir.join("AGENTS.md"), "Above the repository.").unwrap();
        std::fs::write(repo.join("AGENTS.md"), "Root rules.").unwrap();
        std::fs::write(repo.join("crates").join("CLAUDE.md"), "Crate rules.").unwrap();
        std::fs::write(project.join("AGENTS.md"), "Core rules.").unwrap();
        std::fs::write(project.join("CLAUDE.md"), "Not read beside AGENTS.md.").unwrap();
        let connection = Connection {
            workspace: Some(project.clone()),
            ..Default::default()
        };
        let text = context_in(&dir, &connection);
        assert!(
            text.ends_with(
                "later one wins.\n\nFrom ../../AGENTS.md:\nRoot rules.\n\nFrom ../CLAUDE.md:\nCrate rules.\n\nFrom AGENTS.md:\nCore rules."
            ),
            "{text}"
        );
        assert!(
            !text.contains("Above") && !text.contains("Not read"),
            "{text}"
        );

        // A worktree or a submodule has a `.git` file, and is a root of its own.
        std::fs::write(repo.join("crates").join(".git"), "gitdir: elsewhere").unwrap();
        let text = context_in(&dir, &connection);
        assert!(
            text.contains("From ../CLAUDE.md:") && !text.contains("Root rules"),
            "{text}"
        );
        // One file alone reads as it always did.
        std::fs::write(project.join(".git"), "gitdir: elsewhere").unwrap();
        let text = context_in(&dir, &connection);
        assert!(text.ends_with("The project's instructions, from AGENTS.md:\nCore rules."));
    }

    #[test]
    fn the_walk_up_stops_at_the_repository_root_and_never_passes_it() {
        let project = Path::new("/home/user_a/repo/crates/core");
        let at = |root: &'static str| move |dir: &Path| dir == Path::new(root);
        assert_eq!(
            folders(project, at("/home/user_a/repo")),
            [
                project,
                project.parent().unwrap(),
                Path::new("/home/user_a/repo")
            ]
        );
        assert_eq!(
            folders(project, at("/home/user_a/repo/crates/core")),
            [project]
        );
        // No repository: the folders above are not the project's.
        assert_eq!(folders(project, at("/elsewhere")), [project]);
        // A relative path never reaches the process's own folder.
        assert_eq!(
            folders(Path::new("repo/core"), at("")),
            [Path::new("repo/core")]
        );
    }

    #[test]
    fn a_long_instruction_file_keeps_its_head_and_its_tail() {
        let dir = scratch("ends");
        let path = dir.join("AGENTS.md");
        // Read whole, and read from each end past twice what could be kept.
        for middle in [2_000, 100_000] {
            std::fs::write(&path, format!("Start.{}End.", "x".repeat(middle))).unwrap();
            let text = read_ends(&path, "../AGENTS.md", 1_000).unwrap();
            assert!(
                text.starts_with("Start.") && text.ends_with("End."),
                "{text}"
            );
            assert!(text.contains("read_file `../AGENTS.md` has the whole file"));
            assert!(text.chars().count() < 1_000, "{}", text.chars().count());
        }
        // A seek into the middle of a character drops what is left of it.
        std::fs::write(&path, format!("a{}z", "\u{1f600}".repeat(10_000))).unwrap();
        let text = read_ends(&path, "AGENTS.md", 1_000).unwrap();
        assert!(
            text.ends_with(&format!("{}z", "\u{1f600}".repeat(199))),
            "{text}"
        );
        assert!(!text.contains('\u{fffd}'));
        assert_eq!(
            read_ends(&path, "AGENTS.md", 1_000_000)
                .unwrap()
                .chars()
                .count(),
            10_002
        );
        std::fs::write(&path, " \n ").unwrap();
        assert!(read_ends(&path, "AGENTS.md", 1_000).is_none());
    }

    #[test]
    fn instructions_too_long_together_cut_the_root_first_and_stay_capped() {
        let dir = scratch("capped");
        let project = dir.join("a").join("b");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        for (folder, letter) in [(&dir, "r"), (&dir.join("a"), "a"), (&project, "b")] {
            std::fs::write(folder.join("AGENTS.md"), letter.repeat(FILE_LIMIT + 10)).unwrap();
        }
        let found = instructions(&project);
        let labels: Vec<_> = found.iter().map(|(label, _)| label.as_str()).collect();
        assert_eq!(labels, ["../../AGENTS.md", "../AGENTS.md", "AGENTS.md"]);
        let sizes: Vec<_> = found.iter().map(|(_, text)| text.chars().count()).collect();
        assert!(sizes[0] < sizes[1] && sizes[1] < sizes[2], "{sizes:?}");
        assert!(
            sizes.iter().sum::<usize>() <= INSTRUCTIONS_LIMIT,
            "{sizes:?}"
        );
        assert!(found[2].1.starts_with('b') && found[2].1.ends_with('b'));

        // Past the cap, a file is only named, in one line.
        std::fs::write(project.join("AGENTS.md"), "b".repeat(FILE_LIMIT)).unwrap();
        let rest = "a".repeat(INSTRUCTIONS_LIMIT - FILE_LIMIT);
        std::fs::write(dir.join("a").join("AGENTS.md"), rest).unwrap();
        let found = instructions(&project);
        assert_eq!(
            found[0].1,
            "… [cut here to fit; read_file `../../AGENTS.md` has the whole file]"
        );
    }
}
