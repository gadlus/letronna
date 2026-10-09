//! Reading Claude Code and Codex sessions from disk, and turning one into a
//! session of this app's own -- so picking up a conversation started
//! elsewhere is no different from continuing one Letronna started.
//!
//! Both tools keep their own transcripts under the user's home directory,
//! one JSON object per line: Claude Code under
//! `~/.claude/projects/<escaped project path>/<session id>.jsonl`, Codex
//! under `~/.codex/sessions/YYYY/MM/DD/rollout-*.jsonl`. A session Letronna
//! exported as JSON, from this machine or another, comes back in the same
//! way. Nothing here ever writes to those files; it only reads them, and
//! everything it makes lands in a normal `Conversation`.

use crate::chat::{MAX_TOOL_CHARS, Message, ToolCall};
use crate::store::Conversation;
use crate::tools::keep_ends;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};

/// Enough of a file to find its title without reading the whole thing: a
/// session's first prompt is always near the top, and a file that keeps
/// growing for hours must not cost more to list than one just begun.
const HEAD_CAP_BYTES: u64 = 64 * 1024;

/// Which assistant wrote the session being imported.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tool {
    ClaudeCode,
    Codex,
    /// A session Letronna exported as JSON.
    Letronna,
}

impl Tool {
    pub fn label(self) -> &'static str {
        match self {
            Tool::ClaudeCode => "Claude Code",
            Tool::Codex => "Codex",
            Tool::Letronna => "Letronna",
        }
    }
}

/// Where an imported session came from, kept on the `Conversation` so
/// importing the same file twice finds the session it made the first time
/// rather than making a second.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Source {
    pub tool: Tool,
    pub session_id: String,
    pub path: PathBuf,
}

/// A session offered for import, read cheaply: its head, and as much of the
/// file as it took to find the first thing said.
#[derive(Clone)]
pub struct Candidate {
    pub tool: Tool,
    pub session_id: String,
    pub path: PathBuf,
    pub title: String,
    pub workspace: Option<PathBuf>,
    pub updated: u64,
}

/// A source file replayed in full, ready to become a `Conversation`.
pub struct Imported {
    pub source: Source,
    pub title: String,
    pub workspace: Option<PathBuf>,
    pub updated: u64,
    pub messages: Vec<Message>,
    /// Lines, or blocks within a line, that had no way to become part of a
    /// message: skipped rather than failing the whole import.
    pub skipped: usize,
}

/// Sessions from Claude Code and Codex, newest first, `limit` at most.
pub fn list(limit: usize) -> Vec<Candidate> {
    let mut files = Vec::new();
    if let Some(root) = claude_code_root() {
        files.extend(
            claude_code_files(&root)
                .into_iter()
                .map(|p| (Tool::ClaudeCode, p)),
        );
    }
    if let Some(root) = codex_root() {
        files.extend(codex_files(&root).into_iter().map(|p| (Tool::Codex, p)));
    }
    newest_titled(files, limit)
}

/// The newest `limit` of `files`, each titled from its head. Sorted and cut
/// before any head is read: a machine can hold thousands of sessions, and
/// reading the head of every one to offer a few is what made listing slow.
fn newest_titled(files: Vec<(Tool, PathBuf)>, limit: usize) -> Vec<Candidate> {
    // Only plain files: a pipe named like a session would hold the listing
    // thread for good the moment its head was opened.
    let mut files: Vec<(u64, Tool, PathBuf)> = files
        .into_iter()
        .filter(|(_, path)| path.is_file())
        .map(|(tool, path)| (modified_secs(&path), tool, path))
        .collect();
    files.sort_by_key(|&(updated, ..)| std::cmp::Reverse(updated));
    files.truncate(limit);
    files
        .into_iter()
        .map(|(updated, tool, path)| {
            let (session_id, title, workspace) = match tool {
                Tool::ClaudeCode => {
                    let (title, workspace) = claude_code_head(&path);
                    let session_id = path.file_stem().and_then(|s| s.to_str());
                    (session_id.unwrap_or_default().to_owned(), title, workspace)
                }
                Tool::Codex => codex_head(&path),
                Tool::Letronna => match letronna_candidate(&path) {
                    Some(found) => (found.session_id, found.title, found.workspace),
                    None => (String::new(), String::new(), None),
                },
            };
            Candidate {
                tool,
                session_id,
                path,
                title,
                workspace,
                updated,
            }
        })
        .collect()
}

/// Read `candidate`'s file in full and turn it into messages this app knows
/// how to save.
pub fn import(candidate: &Candidate) -> Result<Imported, String> {
    match candidate.tool {
        Tool::ClaudeCode => import_claude_code(&candidate.path),
        Tool::Codex => import_codex(&candidate.path),
        Tool::Letronna => import_letronna(&candidate.path),
    }
}

/// A candidate for a file named directly, as `/import <path>` takes: the
/// tool is told from where the file lives, a raw path carrying no other
/// hint of which format it is in. `None` for a path that is not a file.
pub fn candidate_at(path: &Path) -> Option<Candidate> {
    if !path.is_file() {
        return None;
    }
    if path.extension().is_some_and(|e| e == "json") {
        return letronna_candidate(path);
    }
    let codex = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.starts_with("rollout-"))
        || path.components().any(|c| c.as_os_str() == ".codex");
    let (tool, session_id, title, workspace) = if codex {
        let (session_id, title, workspace) = codex_head(path);
        (Tool::Codex, session_id, title, workspace)
    } else {
        let session_id = path.file_stem().and_then(|s| s.to_str())?.to_owned();
        let (title, workspace) = claude_code_head(path);
        (Tool::ClaudeCode, session_id, title, workspace)
    };
    Some(Candidate {
        tool,
        session_id,
        updated: modified_secs(path),
        path: path.to_owned(),
        title,
        workspace,
    })
}

/// `imported` as a session of its own, at `id`.
pub fn into_conversation(imported: Imported, id: u64) -> Conversation {
    // The folder is whatever the file says, and a file can come from another
    // machine or be written by hand: one that is not a folder here, or is the
    // root of the disk, would widen what the permission modes treat as inside
    // the project. Without one the session takes the folder it is opened in.
    let workspace = imported
        .workspace
        .filter(|p| p.is_dir() && p.parent().is_some());
    Conversation {
        id,
        title: imported.title,
        messages: imported.messages,
        updated: imported.updated,
        workspace,
        imported_from: Some(imported.source),
        ..Default::default()
    }
}

/// Why a file yielded no messages at all, with how much of it was skipped,
/// so an empty file reads differently from a full one. Skipped is not
/// unreadable: the count takes in records left out on purpose, like a
/// summary line, as much as lines this build cannot parse.
fn nothing_to_import(path: &Path, skipped: usize) -> String {
    let what = match skipped {
        0 => return format!("{} has nothing Letronna can import.", path.display()),
        1 => "1 part of it was".to_owned(),
        n => format!("{n} parts of it were"),
    };
    format!(
        "{} has nothing Letronna can import: {what} skipped.",
        path.display()
    )
}

pub(crate) fn claude_code_root() -> Option<PathBuf> {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::tools::home().map(|h| h.join(".claude")))
}

pub(crate) fn codex_root() -> Option<PathBuf> {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::tools::home().map(|h| h.join(".codex")))
}

// -- Claude Code -------------------------------------------------------

/// Every Claude Code session file under `root`, unread.
fn claude_code_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for project in subdirs(&root.join("projects")) {
        let Ok(files) = std::fs::read_dir(&project) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            if path.extension().is_none_or(|e| e != "jsonl") {
                continue;
            }
            if path.file_stem().and_then(|s| s.to_str()).is_none() {
                continue;
            }
            out.push(path);
        }
    }
    out
}

/// The title and project folder of a Claude Code session, from its first
/// bytes: the project folder is on every record, and the title is the text
/// of the first thing the user said.
fn claude_code_head(path: &Path) -> (String, Option<PathBuf>) {
    let mut title = String::new();
    let mut workspace = None;
    for line in head_text(path).lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if workspace.is_none()
            && let Some(cwd) = record["cwd"].as_str().filter(|c| !c.is_empty())
        {
            workspace = Some(PathBuf::from(cwd));
        }
        if title.is_empty() && record["type"].as_str() == Some("user") {
            let text = claude_user_text(&record["message"]["content"]);
            if !text.trim().is_empty() {
                title = short_title(&text);
            }
        }
        if workspace.is_some() && !title.is_empty() {
            break;
        }
    }
    (title, workspace)
}

fn import_claude_code(path: &Path) -> Result<Imported, String> {
    let session_id = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{} has no session id in its name.", path.display()))?
        .to_owned();
    let lines = read_lines(path)?;

    let mut messages = Vec::new();
    let mut tool_names: HashMap<String, String> = HashMap::new();
    let mut workspace = None;
    let mut skipped = 0usize;
    for line in lines {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            skipped += 1;
            continue;
        };
        if workspace.is_none()
            && let Some(cwd) = record["cwd"].as_str().filter(|c| !c.is_empty())
        {
            workspace = Some(PathBuf::from(cwd));
        }
        match record["type"].as_str().unwrap_or_default() {
            "user" => claude_user_record(&record, &mut messages, &tool_names, &mut skipped),
            "assistant" => {
                claude_assistant_record(&record, &mut messages, &mut tool_names, &mut skipped)
            }
            // A tool call answered outside a turn, a compaction summary and
            // the rest of Claude Code's own bookkeeping: none of it is a
            // message either side of the conversation said.
            _ => skipped += 1,
        }
    }
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    let messages = join_rounds(messages);
    let title = first_user_title(&messages);
    Ok(Imported {
        source: Source {
            tool: Tool::ClaudeCode,
            session_id,
            path: path.to_owned(),
        },
        title,
        workspace,
        updated: modified_secs(path),
        messages,
        skipped,
    })
}

/// A `"user"` record: plain text, or a tool result answering a call from the
/// turn before, or both at once.
fn claude_user_record(
    record: &Value,
    messages: &mut Vec<Message>,
    tool_names: &HashMap<String, String>,
    skipped: &mut usize,
) {
    let content = &record["message"]["content"];
    if let Some(text) = content.as_str() {
        if text.trim().is_empty() {
            *skipped += 1;
        } else {
            messages.push(Message {
                role: "user".into(),
                content: text.to_owned(),
                ..Default::default()
            });
        }
        return;
    }
    let Some(blocks) = content.as_array() else {
        *skipped += 1;
        return;
    };
    let mut said = Vec::new();
    for block in blocks {
        match block["type"].as_str().unwrap_or_default() {
            "text" => {
                if let Some(text) = block["text"].as_str() {
                    said.push(text.to_owned());
                }
            }
            "tool_result" => {
                let tool_use_id = block["tool_use_id"].as_str().unwrap_or_default().to_owned();
                let name = tool_names.get(&tool_use_id).cloned().unwrap_or_default();
                let text = tool_result_text(&block["content"]);
                messages.push(Message {
                    role: "tool".into(),
                    content: keep_ends(text, MAX_TOOL_CHARS, ""),
                    tool_call_id: tool_use_id,
                    name,
                    is_error: block["is_error"].as_bool().unwrap_or(false),
                    ..Default::default()
                });
            }
            // A pasted image, most often: nothing this app's messages carry.
            _ => *skipped += 1,
        }
    }
    if !said.is_empty() {
        messages.push(Message {
            role: "user".into(),
            content: said.join("\n"),
            ..Default::default()
        });
    }
}

/// An `"assistant"` record: the text said, any thinking, and any tools
/// called, all in the one message they arrived in.
fn claude_assistant_record(
    record: &Value,
    messages: &mut Vec<Message>,
    tool_names: &mut HashMap<String, String>,
    skipped: &mut usize,
) {
    let message = &record["message"];
    let Some(blocks) = message["content"].as_array() else {
        *skipped += 1;
        return;
    };
    let mut said = Vec::new();
    let mut thought = Vec::new();
    let mut tool_calls = Vec::new();
    for block in blocks {
        match block["type"].as_str().unwrap_or_default() {
            "text" => {
                if let Some(text) = block["text"].as_str() {
                    said.push(text.to_owned());
                }
            }
            "thinking" => {
                if let Some(text) = block["thinking"].as_str() {
                    thought.push(text.to_owned());
                }
            }
            "tool_use" => {
                let id = block["id"].as_str().unwrap_or_default().to_owned();
                let name = block["name"].as_str().unwrap_or_default().to_owned();
                if id.is_empty() || name.is_empty() {
                    *skipped += 1;
                    continue;
                }
                tool_names.insert(id.clone(), name.clone());
                let arguments = match block["input"].as_object() {
                    Some(_) => block["input"].to_string(),
                    None => "{}".to_owned(),
                };
                tool_calls.push(ToolCall {
                    id,
                    name,
                    arguments,
                });
            }
            _ => *skipped += 1,
        }
    }
    if said.is_empty() && tool_calls.is_empty() {
        *skipped += 1;
        return;
    }
    messages.push(Message {
        role: "assistant".into(),
        content: said.join("\n"),
        model: message["model"].as_str().unwrap_or_default().to_owned(),
        reasoning: thought.join("\n"),
        tool_calls,
        ..Default::default()
    });
}

/// The text of a `"user"` record's `content`: a plain string, or the `text`
/// blocks of an array, joined.
fn claude_user_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| b["type"].as_str() == Some("text"))
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// A `tool_result` block's `content`: a plain string, or the `text` of each
/// item in an array, joined; an image in the mix contributes nothing.
fn tool_result_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

// -- Codex ---------------------------------------------------------------

/// Every Codex rollout under `root`, unread.
fn codex_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for year in subdirs(&root.join("sessions")) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                let Ok(files) = std::fs::read_dir(&day) else {
                    continue;
                };
                for file in files.flatten() {
                    let path = file.path();
                    let name = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or_default();
                    if !(name.starts_with("rollout-") && name.ends_with(".jsonl")) {
                        continue;
                    }
                    out.push(path);
                }
            }
        }
    }
    out
}

/// The session id, title and project folder of a Codex rollout, from its
/// first bytes: `session_meta` is always the first line, and the title is
/// the text of the first user message after it.
fn codex_head(path: &Path) -> (String, String, Option<PathBuf>) {
    let mut session_id = String::new();
    let mut title = String::new();
    let mut workspace = None;
    for line in head_text(path).lines() {
        let Ok(record) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        match record["type"].as_str().unwrap_or_default() {
            "session_meta" => {
                let payload = &record["payload"];
                if let Some(id) = payload["id"].as_str() {
                    session_id = id.to_owned();
                }
                if let Some(cwd) = payload["cwd"].as_str().filter(|c| !c.is_empty()) {
                    workspace = Some(PathBuf::from(cwd));
                }
            }
            "response_item" if title.is_empty() => {
                let payload = &record["payload"];
                if payload["type"].as_str() == Some("message")
                    && payload["role"].as_str() == Some("user")
                {
                    let text = codex_content_text(&payload["content"]);
                    if !text.trim().is_empty() {
                        title = short_title(&text);
                    }
                }
            }
            _ => {}
        }
        if !session_id.is_empty() && workspace.is_some() && !title.is_empty() {
            break;
        }
    }
    if session_id.is_empty() {
        session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
    }
    (session_id, title, workspace)
}

fn import_codex(path: &Path) -> Result<Imported, String> {
    let lines = read_lines(path)?;

    let mut session_id = String::new();
    let mut workspace = None;
    let mut messages = Vec::new();
    let mut call_names: HashMap<String, String> = HashMap::new();
    let mut skipped = 0usize;
    for line in lines {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            skipped += 1;
            continue;
        };
        match record["type"].as_str().unwrap_or_default() {
            "session_meta" => {
                let payload = &record["payload"];
                if let Some(id) = payload["id"].as_str() {
                    session_id = id.to_owned();
                }
                if let Some(cwd) = payload["cwd"].as_str().filter(|c| !c.is_empty()) {
                    workspace = Some(PathBuf::from(cwd));
                }
            }
            "response_item" => codex_response_item(
                &record["payload"],
                &mut messages,
                &mut call_names,
                &mut skipped,
            ),
            // turn_context repeats the cwd already read from session_meta,
            // and event_msg repeats what a response_item on the same turn
            // already carries: counting either as skipped would double-count
            // one thing left out on purpose, not one that could not be read.
            _ => {}
        }
    }
    if session_id.is_empty() {
        session_id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
    }
    if session_id.is_empty() {
        return Err(format!("{} has no session id.", path.display()));
    }
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    let messages = join_rounds(messages);
    let title = first_user_title(&messages);
    Ok(Imported {
        source: Source {
            tool: Tool::Codex,
            session_id,
            path: path.to_owned(),
        },
        title,
        workspace,
        updated: modified_secs(path),
        messages,
        skipped,
    })
}

/// One `response_item` payload: a message, a tool call of one of Codex's
/// several shapes, or a tool's output answering one.
fn codex_response_item(
    payload: &Value,
    messages: &mut Vec<Message>,
    call_names: &mut HashMap<String, String>,
    skipped: &mut usize,
) {
    match payload["type"].as_str().unwrap_or_default() {
        "message" => {
            let role = payload["role"].as_str().unwrap_or_default();
            let text = codex_content_text(&payload["content"]);
            if (role == "user" || role == "assistant") && !text.trim().is_empty() {
                messages.push(Message {
                    role: role.to_owned(),
                    content: text,
                    ..Default::default()
                });
            } else {
                *skipped += 1;
            }
        }
        "function_call" => codex_call(
            payload["call_id"].as_str(),
            payload["name"].as_str(),
            payload["arguments"].as_str().unwrap_or("{}"),
            messages,
            call_names,
            skipped,
        ),
        "custom_tool_call" => codex_call(
            payload["call_id"].as_str(),
            payload["name"].as_str(),
            payload["input"].as_str().unwrap_or("{}"),
            messages,
            call_names,
            skipped,
        ),
        "local_shell_call" => {
            let command = payload["action"]["command"]
                .as_array()
                .map(|parts| {
                    parts
                        .iter()
                        .filter_map(|p| p.as_str())
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            if command.is_empty() {
                *skipped += 1;
                return;
            }
            codex_call(
                payload["call_id"].as_str(),
                Some("shell"),
                &json!({ "command": command }).to_string(),
                messages,
                call_names,
                skipped,
            );
        }
        "function_call_output" | "custom_tool_call_output" => {
            let Some(id) = payload["call_id"].as_str().filter(|c| !c.is_empty()) else {
                *skipped += 1;
                return;
            };
            let text = codex_output_text(&payload["output"]);
            let name = call_names.get(id).cloned().unwrap_or_default();
            messages.push(Message {
                role: "tool".into(),
                content: keep_ends(text, MAX_TOOL_CHARS, ""),
                tool_call_id: id.to_owned(),
                name,
                ..Default::default()
            });
        }
        // Reasoning, a web or tool search, image generation and the like:
        // Codex's own bookkeeping, none of it a message either side said.
        _ => *skipped += 1,
    }
}

/// A tool call of any of Codex's shapes, which all reduce to a call id, a
/// name and a JSON-encoded argument string.
fn codex_call(
    id: Option<&str>,
    name: Option<&str>,
    arguments: &str,
    messages: &mut Vec<Message>,
    call_names: &mut HashMap<String, String>,
    skipped: &mut usize,
) {
    let (Some(id), Some(name)) = (id.filter(|c| !c.is_empty()), name.filter(|n| !n.is_empty()))
    else {
        *skipped += 1;
        return;
    };
    call_names.insert(id.to_owned(), name.to_owned());
    messages.push(Message {
        role: "assistant".into(),
        tool_calls: vec![ToolCall {
            id: id.to_owned(),
            name: name.to_owned(),
            arguments: arguments.to_owned(),
        }],
        ..Default::default()
    });
}

/// The text of a `response_item` message's `content` array: Codex tags user
/// text `input_text` and its own `output_text`, joined in order.
fn codex_content_text(content: &Value) -> String {
    content
        .as_array()
        .map(|blocks| {
            blocks
                .iter()
                .filter(|b| {
                    matches!(
                        b["type"].as_str(),
                        Some("input_text" | "output_text" | "text")
                    )
                })
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

/// A tool output: a plain string, or an array of content items in the same
/// shape as a message's, joined; anything else is kept as its raw JSON
/// rather than lost.
fn codex_output_text(output: &Value) -> String {
    if let Some(text) = output.as_str() {
        return text.to_owned();
    }
    if output.is_array() {
        let joined = codex_content_text(output);
        if !joined.is_empty() {
            return joined;
        }
    }
    if output.is_null() {
        String::new()
    } else {
        output.to_string()
    }
}

// -- Letronna ---------------------------------------------------------------

/// A Letronna JSON export, read whole: it is one document, not lines.
fn read_letronna(path: &Path) -> Result<Value, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let doc: Value = serde_json::from_slice(&bytes)
        .map_err(|e| format!("{} is not JSON Letronna can read: {e}", path.display()))?;
    if doc["format"].as_str() != Some(crate::export::FORMAT) {
        return Err(format!(
            "{} is not a session Letronna exported.",
            path.display()
        ));
    }
    if doc["version"]
        .as_u64()
        .is_some_and(|v| v > crate::export::VERSION)
    {
        return Err(format!(
            "{} was exported by a newer Letronna. Update to import it.",
            path.display()
        ));
    }
    Ok(doc)
}

/// The export at `path` as a candidate; `None` for JSON that is not one.
fn letronna_candidate(path: &Path) -> Option<Candidate> {
    let doc = read_letronna(path).ok()?;
    Some(Candidate {
        tool: Tool::Letronna,
        session_id: letronna_id(&doc, path),
        title: doc["title"].as_str().unwrap_or_default().to_owned(),
        workspace: doc["workspace"].as_str().map(PathBuf::from),
        updated: doc["updated"]
            .as_u64()
            .unwrap_or_else(|| modified_secs(path)),
        path: path.to_owned(),
    })
}

/// The id an export was written under, or for a file without one, its name.
fn letronna_id(doc: &Value, path: &Path) -> String {
    doc["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .or_else(|| path.file_stem().and_then(|s| s.to_str()))
        .unwrap_or_default()
        .to_owned()
}

fn import_letronna(path: &Path) -> Result<Imported, String> {
    let doc = read_letronna(path)?;
    let mut skipped = 0usize;
    let items = doc["messages"].as_array().map_or(&[][..], Vec::as_slice);
    let messages = chat_messages(items, &mut skipped);
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    let title = match doc["title"].as_str().map(str::trim) {
        Some(title) if !title.is_empty() => title.to_owned(),
        _ => first_user_title(&messages),
    };
    Ok(Imported {
        source: Source {
            tool: Tool::Letronna,
            session_id: letronna_id(&doc, path),
            path: path.to_owned(),
        },
        title,
        workspace: doc["workspace"].as_str().map(PathBuf::from),
        updated: doc["updated"]
            .as_u64()
            .unwrap_or_else(|| modified_secs(path)),
        messages,
        skipped,
    })
}

/// Messages in the shape chat APIs take them -- `role` and `content`, with
/// `tool_calls` and `tool_call_id` for calls and their results -- as
/// Letronna's JSON export writes them.
fn chat_messages(items: &[Value], skipped: &mut usize) -> Vec<Message> {
    let mut out = Vec::new();
    let mut names: HashMap<String, String> = HashMap::new();
    for item in items {
        let content = chat_text(&item["content"]);
        match item["role"].as_str().unwrap_or_default() {
            "user" if !content.trim().is_empty() => out.push(Message {
                role: "user".into(),
                content,
                notice: item["notice"].as_bool().unwrap_or(false),
                ..Default::default()
            }),
            "assistant" => {
                let tool_calls: Vec<ToolCall> = item["tool_calls"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(chat_call)
                    .collect();
                if content.trim().is_empty() && tool_calls.is_empty() {
                    *skipped += 1;
                    continue;
                }
                for call in &tool_calls {
                    names.insert(call.id.clone(), call.name.clone());
                }
                out.push(Message {
                    role: "assistant".into(),
                    content,
                    model: item["model"].as_str().unwrap_or_default().to_owned(),
                    reasoning: item["reasoning"].as_str().unwrap_or_default().to_owned(),
                    tool_calls,
                    ..Default::default()
                });
            }
            "tool" => {
                let Some(id) = item["tool_call_id"].as_str().filter(|id| !id.is_empty()) else {
                    *skipped += 1;
                    continue;
                };
                let name = match item["name"].as_str() {
                    Some(name) if !name.is_empty() => name.to_owned(),
                    _ => names.get(id).cloned().unwrap_or_default(),
                };
                out.push(Message {
                    role: "tool".into(),
                    content: keep_ends(content, MAX_TOOL_CHARS, ""),
                    tool_call_id: id.to_owned(),
                    name,
                    is_error: item["is_error"].as_bool().unwrap_or(false),
                    ..Default::default()
                });
            }
            // The other app's instructions to its model, not anything said
            // in the session.
            "system" | "developer" => {}
            _ => *skipped += 1,
        }
    }
    out
}

/// One of `tool_calls`: an id, and a function's name and arguments, which
/// are a JSON string in the chat shape but an object in some files.
fn chat_call(call: &Value) -> Option<ToolCall> {
    let id = call["id"].as_str().filter(|id| !id.is_empty())?;
    let function = &call["function"];
    let name = function["name"].as_str().filter(|n| !n.is_empty())?;
    let arguments = match &function["arguments"] {
        Value::String(text) => text.clone(),
        Value::Null => "{}".to_owned(),
        other => other.to_string(),
    };
    Some(ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments,
    })
}

/// A chat message's `content`: a string, or the text of each part of an
/// array, joined; a picture in the mix contributes nothing.
fn chat_text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    content
        .as_array()
        .map(|parts| {
            parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

// -- Shared ----------------------------------------------------------------

/// The non-empty lines of `path`, read one at a time so a transcript that has
/// grown to gigabytes is never held whole; bytes that are not UTF-8 are
/// replaced rather than ending the read, and a read that fails ends it.
fn read_lines(path: &Path) -> Result<impl Iterator<Item = String>, String> {
    let file =
        std::fs::File::open(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut buf = Vec::new();
    Ok(std::iter::from_fn(move || {
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {
                    let line = String::from_utf8_lossy(&buf);
                    if !line.trim().is_empty() {
                        return Some(line.trim_end().to_owned());
                    }
                }
            }
        }
    }))
}

/// Assistant messages that follow one another folded into one. Claude Code
/// writes a record per content block and Codex an item per call, so one
/// round with two parallel calls arrives as two assistant messages, and the
/// first call's result then no longer follows the message that made it.
fn join_rounds(messages: Vec<Message>) -> Vec<Message> {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    for message in messages {
        match out.last_mut() {
            Some(last) if last.role == "assistant" && message.role == "assistant" => {
                if !message.content.is_empty() {
                    if !last.content.is_empty() {
                        last.content.push('\n');
                    }
                    last.content.push_str(&message.content);
                }
                if !message.reasoning.is_empty() {
                    if !last.reasoning.is_empty() {
                        last.reasoning.push('\n');
                    }
                    last.reasoning.push_str(&message.reasoning);
                }
                if last.model.is_empty() {
                    last.model = message.model;
                }
                last.tool_calls.extend(message.tool_calls);
            }
            _ => out.push(message),
        }
    }
    out
}

fn subdirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect()
}

/// Up to `HEAD_CAP_BYTES` of `path`, lossily: enough to title a session
/// without reading one that has grown to gigabytes.
fn head_text(path: &Path) -> String {
    let Ok(file) = std::fs::File::open(path) else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = file.take(HEAD_CAP_BYTES).read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn modified_secs(path: &Path) -> u64 {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A title from the first user message in `messages`: the words on one
/// line, to the length every session's title is cut to.
fn first_user_title(messages: &[Message]) -> String {
    messages
        .iter()
        .find(|m| m.role == "user" && !m.content.trim().is_empty())
        .map(|m| short_title(&m.content))
        .unwrap_or_default()
}

/// `text` as a title: whitespace collapsed, cut to 40 characters as every
/// session's title is.
fn short_title(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(40)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-import-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(dir: &Path, name: &str, text: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    const CLAUDE_CODE_FIXTURE: &str = concat!(
        r#"{"type":"user","cwd":"/home/dev/project","message":{"role":"user","content":"Fix the login bug"}}"#,
        "\n",
        r#"{"type":"assistant","cwd":"/home/dev/project","message":{"role":"assistant","model":"claude-opus-4","content":[{"type":"text","text":"Let me look."},{"type":"tool_use","id":"tu_1","name":"read_file","input":{"path":"src/auth.rs"}}]}}"#,
        "\n",
        r#"{"type":"user","cwd":"/home/dev/project","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"tu_1","content":"fn login() {}","is_error":false}]}}"#,
        "\n",
        "{\"type\":\"assistant\", not json\n",
        r#"{"type":"summary","summary":"chat about the bug"}"#,
        "\n",
        r#"{"type":"assistant","cwd":"/home/dev/project","message":{"role":"assistant","model":"claude-opus-4","content":[{"type":"text","text":"Fixed it."}]}}"#,
        "\n",
    );

    #[test]
    fn a_claude_code_session_maps_to_messages_a_title_and_its_folder() {
        let dir = scratch("claude-ok");
        let path = write(&dir, "abc123.jsonl", CLAUDE_CODE_FIXTURE);

        let imported = import_claude_code(&path).unwrap();
        assert_eq!(imported.source.tool, Tool::ClaudeCode);
        assert_eq!(imported.source.session_id, "abc123");
        assert_eq!(imported.source.path, path);
        assert_eq!(imported.workspace, Some(PathBuf::from("/home/dev/project")));
        assert_eq!(imported.title, "Fix the login bug");
        // The malformed line and the summary record are neither a user nor
        // an assistant turn, so both are counted rather than mapped.
        assert_eq!(imported.skipped, 2);

        let messages = imported.messages;
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "Fix the login bug");
        assert_eq!(messages[1].role, "assistant");
        assert_eq!(messages[1].content, "Let me look.");
        assert_eq!(messages[1].tool_calls.len(), 1);
        assert_eq!(messages[1].tool_calls[0].name, "read_file");
        assert_eq!(messages[2].role, "tool");
        assert_eq!(messages[2].tool_call_id, "tu_1");
        assert_eq!(messages[2].name, "read_file");
        assert_eq!(messages[2].content, "fn login() {}");
        assert!(!messages[2].is_error);
        assert_eq!(messages[3].content, "Fixed it.");
    }

    const CODEX_FIXTURE: &str = concat!(
        r#"{"timestamp":"2024-01-01T00:00:00Z","type":"session_meta","payload":{"id":"codex-abc","cwd":"/home/dev/project2"}}"#,
        "\n",
        r#"{"timestamp":"2024-01-01T00:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Add a health check endpoint"}]}}"#,
        "\n",
        r#"{"timestamp":"2024-01-01T00:00:02Z","type":"response_item","payload":{"type":"function_call","name":"shell","arguments":"{\"command\":\"ls\"}","call_id":"call_1"}}"#,
        "\n",
        r#"{"timestamp":"2024-01-01T00:00:03Z","type":"response_item","payload":{"type":"function_call_output","call_id":"call_1","output":"app.py\nREADME.md"}}"#,
        "\n",
        "not json at all\n",
        r#"{"timestamp":"2024-01-01T00:00:04Z","type":"response_item","payload":{"type":"reasoning","summary":[]}}"#,
        "\n",
        r#"{"timestamp":"2024-01-01T00:00:05Z","type":"turn_context","payload":{"cwd":"/home/dev/project2"}}"#,
        "\n",
        r#"{"timestamp":"2024-01-01T00:00:06Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Added the endpoint."}]}}"#,
        "\n",
    );

    #[test]
    fn a_codex_session_maps_calls_and_outputs_and_skips_what_it_cannot_read() {
        let dir = scratch("codex-ok");
        let path = write(
            &dir,
            "rollout-2024-01-01T00-00-00-abc123.jsonl",
            CODEX_FIXTURE,
        );

        let imported = import_codex(&path).unwrap();
        assert_eq!(imported.source.tool, Tool::Codex);
        assert_eq!(imported.source.session_id, "codex-abc");
        assert_eq!(
            imported.workspace,
            Some(PathBuf::from("/home/dev/project2"))
        );
        assert_eq!(imported.title, "Add a health check endpoint");
        // The unparseable line and the reasoning item are counted; the
        // turn_context line is left out on purpose and is not.
        assert_eq!(imported.skipped, 2);

        let messages = imported.messages;
        assert_eq!(messages.len(), 4);
        assert_eq!(messages[0].content, "Add a health check endpoint");
        assert_eq!(messages[1].tool_calls[0].name, "shell");
        assert_eq!(messages[1].tool_calls[0].id, "call_1");
        assert_eq!(messages[2].role, "tool");
        assert_eq!(messages[2].tool_call_id, "call_1");
        assert_eq!(messages[2].name, "shell");
        assert_eq!(messages[2].content, "app.py\nREADME.md");
        assert_eq!(messages[3].content, "Added the endpoint.");
    }

    #[test]
    fn a_file_with_nothing_readable_says_how_much_was_skipped() {
        let dir = scratch("nothing-readable");
        let path = write(&dir, "def456.jsonl", "not json\n{\"type\":\"summary\"}\n");

        let Err(error) = import_claude_code(&path) else {
            panic!("a file with no turns in it imported");
        };
        assert!(error.ends_with("has nothing Letronna can import: 2 parts of it were skipped."));
        // A summary alone is read without trouble, and still not a message.
        let summary = write(&dir, "ghi789.jsonl", "{\"type\":\"summary\"}\n");
        let Err(error) = import_claude_code(&summary) else {
            panic!("a summary alone imported");
        };
        assert!(error.ends_with("has nothing Letronna can import: 1 part of it was skipped."));
        let empty = write(&dir, "empty.jsonl", "");
        let Err(error) = import_claude_code(&empty) else {
            panic!("an empty file imported");
        };
        assert!(error.ends_with("has nothing Letronna can import."));
    }

    #[test]
    fn a_huge_tool_result_is_capped_the_way_a_live_tool_result_is() {
        let dir = scratch("codex-huge");
        let output = "x".repeat(MAX_TOOL_CHARS * 2);
        let output_item = format!(
            r#"{{"type":"response_item","payload":{{"type":"function_call_output","call_id":"c1","output":{}}}}}"#,
            serde_json::to_string(&output).unwrap()
        );
        let fixture = format!(
            "{}\n{output_item}\n",
            r#"{"type":"session_meta","payload":{"id":"s1","cwd":"/w"}}"#,
        );
        let path = write(&dir, "rollout-huge.jsonl", &fixture);
        let imported = import_codex(&path).unwrap();
        assert_eq!(imported.messages.len(), 1);
        assert!(imported.messages[0].content.len() <= MAX_TOOL_CHARS + 64);
    }

    #[test]
    fn the_source_is_remembered_and_serializes_back_the_same() {
        let source = Source {
            tool: Tool::Codex,
            session_id: "codex-abc".into(),
            path: PathBuf::from("/home/dev/.codex/sessions/2024/01/01/rollout-x.jsonl"),
        };
        let json = serde_json::to_string(&source).unwrap();
        let back: Source = serde_json::from_str(&json).unwrap();
        assert_eq!(back, source);

        let dir = scratch("remember");
        let path = write(&dir, "abc123.jsonl", CLAUDE_CODE_FIXTURE);
        let imported = import_claude_code(&path).unwrap();
        let conversation = into_conversation(imported, 7);
        assert_eq!(conversation.id, 7);
        let source = conversation.imported_from.unwrap();
        assert_eq!(source.tool, Tool::ClaudeCode);
        assert_eq!(source.session_id, "abc123");
        assert_eq!(source.path, path);
    }

    #[test]
    fn reimporting_the_same_source_is_found_rather_than_duplicated() {
        let dir = scratch("dedup");
        let path = write(&dir, "abc123.jsonl", CLAUDE_CODE_FIXTURE);

        let first = into_conversation(import_claude_code(&path).unwrap(), 1);
        let second = into_conversation(import_claude_code(&path).unwrap(), 2);
        // Both imports of the same file remember the same source, which is
        // what a caller checks before making a second session for it.
        assert_eq!(first.imported_from, second.imported_from);

        let mut store = crate::store::Store::default();
        store.conversations.push(first);
        assert_eq!(
            store.find_by_import(Tool::ClaudeCode, "abc123"),
            Some(0),
            "the session already imported from this source is found"
        );
        assert_eq!(store.find_by_import(Tool::Codex, "abc123"), None);
        assert_eq!(store.find_by_import(Tool::ClaudeCode, "nope"), None);
    }

    #[test]
    fn listing_reads_only_the_head_and_titles_from_what_it_finds() {
        let dir = scratch("listing");
        let claude_root = dir.join("claude");
        let project = claude_root.join("projects").join("-home-dev-project");
        std::fs::create_dir_all(&project).unwrap();
        write(&project, "abc123.jsonl", CLAUDE_CODE_FIXTURE);

        let found = newest_titled(
            claude_code_files(&claude_root)
                .into_iter()
                .map(|p| (Tool::ClaudeCode, p))
                .collect(),
            usize::MAX,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "abc123");
        assert_eq!(found[0].title, "Fix the login bug");
        assert_eq!(found[0].workspace, Some(PathBuf::from("/home/dev/project")));

        let codex_root = dir.join("codex");
        let day = codex_root.join("sessions/2024/01/01");
        std::fs::create_dir_all(&day).unwrap();
        write(&day, "rollout-2024-01-01T00-00-00-abc.jsonl", CODEX_FIXTURE);

        let found = newest_titled(
            codex_files(&codex_root)
                .into_iter()
                .map(|p| (Tool::Codex, p))
                .collect(),
            usize::MAX,
        );
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].session_id, "codex-abc");
        assert_eq!(found[0].title, "Add a health check endpoint");
    }

    #[test]
    fn listing_offers_the_newest_first_and_no_more_than_asked() {
        let dir = scratch("newest");
        let age = |path: &Path, secs: u64| {
            let when = std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs);
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(when).unwrap();
        };
        let old = write(&dir, "old.jsonl", CLAUDE_CODE_FIXTURE);
        age(&old, 1_000);
        let new = write(&dir, "new.jsonl", CLAUDE_CODE_FIXTURE);
        age(&new, 3_000);
        let middle = write(&dir, "rollout-middle.jsonl", CODEX_FIXTURE);
        age(&middle, 2_000);

        let found = newest_titled(
            vec![
                (Tool::ClaudeCode, old),
                (Tool::ClaudeCode, new),
                (Tool::Codex, middle),
            ],
            2,
        );
        let ids: Vec<&str> = found.iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(ids, ["new", "codex-abc"]);
        assert_eq!(found[0].title, "Fix the login bug");
        assert_eq!(found[0].updated, 3_000);
        assert_eq!(found[1].title, "Add a health check endpoint");
    }

    /// A pipe never ends and opening one waits for a writer, so a head read
    /// from it would hold the listing thread for good.
    #[cfg(unix)]
    #[test]
    fn listing_passes_over_what_is_not_a_plain_file() {
        let dir = scratch("not-a-file");
        let project = dir.join("projects").join("-home-dev-project");
        std::fs::create_dir_all(&project).unwrap();
        write(&project, "abc123.jsonl", CLAUDE_CODE_FIXTURE);
        let pipe = project.join("pipe.jsonl");
        let made = std::process::Command::new("mkfifo").arg(&pipe).status();
        assert!(made.is_ok_and(|s| s.success()), "mkfifo failed");

        let found = newest_titled(
            claude_code_files(&dir)
                .into_iter()
                .map(|p| (Tool::ClaudeCode, p))
                .collect(),
            usize::MAX,
        );
        let ids: Vec<&str> = found.iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(ids, ["abc123"]);
    }

    #[test]
    fn parallel_calls_stay_in_front_of_their_results() {
        let dir = scratch("parallel");
        let text = concat!(
            r#"{"type":"user","message":{"content":"go"}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"a","name":"read_file","input":{}}]}}"#,
            "\n",
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"b","name":"read_file","input":{}}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"a","content":"x"}]}}"#,
            "\n",
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"b","content":"y"}]}}"#,
            "\n",
        );
        let path = write(&dir, "par.jsonl", text);
        let messages = import_claude_code(&path).unwrap().messages;
        let roles: Vec<&str> = messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "tool"]);
        assert_eq!(messages[1].tool_calls.len(), 2);
    }

    #[test]
    fn a_folder_that_is_not_here_is_not_kept() {
        let imported = |workspace| Imported {
            source: Source {
                tool: Tool::Codex,
                session_id: "s".into(),
                path: PathBuf::from("s.jsonl"),
            },
            title: String::new(),
            workspace,
            updated: 0,
            messages: Vec::new(),
            skipped: 0,
        };
        assert_eq!(
            into_conversation(imported(Some(PathBuf::from("/nowhere/at/all"))), 1).workspace,
            None
        );
        assert_eq!(
            into_conversation(imported(Some(PathBuf::from("/"))), 1).workspace,
            None
        );
        let here = std::env::temp_dir();
        assert_eq!(
            into_conversation(imported(Some(here.clone())), 1).workspace,
            Some(here)
        );
    }

    #[test]
    fn a_file_that_is_not_utf8_still_imports() {
        let dir = scratch("lossy");
        let mut bytes = br#"{"type":"user","message":{"content":"caf"#.to_vec();
        bytes.push(0xff);
        bytes.extend_from_slice(b"\"}}\n");
        let path = dir.join("bad.jsonl");
        std::fs::write(&path, bytes).unwrap();
        assert_eq!(import_claude_code(&path).unwrap().messages.len(), 1);
    }
}
