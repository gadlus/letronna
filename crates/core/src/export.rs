//! A session written out for somewhere else: Markdown to read or pass on,
//! JSON that `/import` reads back, or a new session of Claude Code's or
//! Codex's own, so a conversation begun here can go on there.
//!
//! A session handed to another tool is always a new file under a new id.
//! Going on with it there must never overwrite what that tool already
//! holds -- an earlier export of the same session included, which may have
//! grown there since. What the other tool has no place for stays behind:
//! the model's thinking, which Claude's API takes back only with the
//! signature its own model gave it, and pictures, which Letronna keeps as
//! files beside the session.

use crate::chat::Message;
use crate::import::Tool;
use crate::store::Conversation;
use chrono::{DateTime, Local, SecondsFormat, Utc};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// What a JSON export calls itself, so `/import` can tell it from another
/// tool's file.
pub const FORMAT: &str = "letronna-session";
/// The JSON export's version, raised only when an older Letronna would
/// misread a newer file.
pub const VERSION: u64 = 1;
/// The tools a session can be handed to.
pub const TOOLS: [Tool; 2] = [Tool::ClaudeCode, Tool::Codex];

/// Claude Code's longest project folder name: past it the name is cut and a
/// hash of the whole path added, as `claude_code_project` does too.
const CLAUDE_CODE_NAME_MAX: usize = 200;
/// The result handed over for a call nothing answered: both tools' models
/// want a result for every call, and the call may well have run.
const NO_RESULT: &str = "No result was recorded for this call; it may have run.";

/// A session handed to another tool: where it went, and what to type to go
/// on with it there.
#[derive(Clone, Debug, PartialEq)]
pub struct Continued {
    pub tool: Tool,
    pub session_id: String,
    pub path: PathBuf,
    /// Where to run `command`: the session's project folder, or the home
    /// folder for a session without one.
    pub folder: PathBuf,
    pub command: String,
}

/// Write `conversation` as a new session of `tool`'s, to go on with there.
pub fn continue_in(conversation: &Conversation, tool: Tool) -> Result<Continued, String> {
    let folder = conversation
        .workspace
        .clone()
        .filter(|p| p.is_dir())
        .or_else(crate::tools::home)
        .ok_or("There is no home folder to start the session in.")?;
    let folder = real_path(&folder);
    let now = Utc::now();
    match tool {
        Tool::ClaudeCode => {
            let root = crate::import::claude_code_root()
                .ok_or("There is no home folder to find Claude Code's sessions in.")?;
            to_claude_code(conversation, &root, &folder, now)
        }
        Tool::Codex => {
            let root = crate::import::codex_root()
                .ok_or("There is no home folder to find Codex's sessions in.")?;
            to_codex(conversation, &root, &folder, now)
        }
        _ => Err(format!(
            "Letronna cannot write sessions for {} yet.",
            tool.label()
        )),
    }
}

/// `conversation` as Markdown: who said what, each tool call with its
/// result, and the model's thinking folded away.
pub fn markdown(conversation: &Conversation) -> String {
    let title = match conversation.title.trim() {
        "" => "Untitled",
        title => title,
    };
    let mut out = format!("# {title}\n\n");
    if let Some(folder) = &conversation.workspace {
        out.push_str(&format!("In `{}`\n\n---\n\n", folder.display()));
    }
    let messages = &conversation.messages;
    for (at, message) in messages.iter().enumerate() {
        let part = match message.role.as_str() {
            "user" => user_markdown(message),
            "assistant" => assistant_markdown(message, &messages[at + 1..]),
            // Shown with the call it answers, unless no call before it made it.
            "tool" if !answers_a_call(messages, at) => {
                format!(
                    "**Result**\n\n{}",
                    call_markdown(&message.name, "", Some(message))
                )
            }
            "failure" => format!("**Error**\n\n{}\n\n", message.content.trim()),
            _ => continue,
        };
        out.push_str(&part);
        out.push_str("---\n\n");
    }
    out
}

/// `conversation` as JSON, its messages in the shape chat APIs take them,
/// which `/import` reads back as a session of its own.
pub fn json(conversation: &Conversation) -> Result<String, String> {
    let messages: Vec<Value> = conversation
        .messages
        .iter()
        .filter_map(json_message)
        .collect();
    let doc = json!({
        "format": FORMAT,
        "version": VERSION,
        // Fresh for each export, so importing the same file twice opens the
        // session the first import made, while a later export of the same
        // session, holding more, imports as a session of its own.
        "id": uuid()?,
        "title": conversation.title,
        "workspace": conversation.workspace.as_ref().map(|p| p.to_string_lossy()),
        "updated": conversation.updated,
        "messages": messages,
    });
    serde_json::to_string_pretty(&doc)
        .map_err(|e| format!("Could not write the session as JSON: {e}"))
}

fn json_message(message: &Message) -> Option<Value> {
    let mut out = json!({ "role": message.role, "content": message.content });
    match message.role.as_str() {
        "user" => {
            if message.notice {
                out["notice"] = json!(true);
            }
        }
        "assistant" => {
            if !message.model.is_empty() {
                out["model"] = json!(message.model);
            }
            if !message.reasoning.is_empty() {
                out["reasoning"] = json!(message.reasoning);
            }
            if !message.tool_calls.is_empty() {
                out["tool_calls"] = message
                    .tool_calls
                    .iter()
                    .map(|call| {
                        json!({
                            "id": call.id,
                            "type": "function",
                            "function": { "name": call.name, "arguments": call.arguments },
                        })
                    })
                    .collect();
            }
        }
        "tool" => {
            out["tool_call_id"] = json!(message.tool_call_id);
            out["name"] = json!(message.name);
            if message.is_error {
                out["is_error"] = json!(true);
            }
        }
        // A failure is the window's note of what went wrong, not anything
        // either side said.
        _ => return None,
    }
    Some(out)
}

fn user_markdown(message: &Message) -> String {
    let who = if message.notice { "Notice" } else { "You" };
    let mut out = format!("**{who}**\n\n{}\n\n", message.content.trim_end());
    for picture in &message.images {
        let name = picture.file_name().unwrap_or(picture.as_os_str());
        out.push_str(&format!("*Attached: {}*\n\n", name.to_string_lossy()));
    }
    out
}

/// An assistant message, with each of its calls and the result that came
/// back for it, found among the tool messages right after it.
fn assistant_markdown(message: &Message, after: &[Message]) -> String {
    let who = match message.model.as_str() {
        "" => "Assistant",
        model => model,
    };
    let mut out = format!("**{who}**\n\n");
    if !message.reasoning.trim().is_empty() {
        out.push_str(&format!(
            "<details>\n<summary>Thinking</summary>\n\n{}\n\n</details>\n\n",
            message.reasoning.trim()
        ));
    }
    if !message.content.trim().is_empty() {
        out.push_str(message.content.trim_end());
        out.push_str("\n\n");
    }
    let results: Vec<&Message> = after.iter().take_while(|m| m.role == "tool").collect();
    for call in &message.tool_calls {
        let result = results.iter().find(|r| r.tool_call_id == call.id).copied();
        out.push_str(&call_markdown(&call.name, &call.arguments, result));
    }
    out
}

/// One call folded into a summary of what it touched, its arguments and its
/// result inside.
fn call_markdown(name: &str, arguments: &str, result: Option<&Message>) -> String {
    let mut summary = name.to_owned();
    let subject = subject(arguments);
    if !subject.is_empty() {
        summary.push_str(" · ");
        summary.push_str(&subject);
    }
    if result.is_some_and(|r| r.is_error) {
        summary.push_str(" (failed)");
    }
    let mut out = format!(
        "<details>\n<summary>{}</summary>\n\n",
        escape_html(&summary)
    );
    if !arguments.trim().is_empty() {
        let pretty = serde_json::from_str::<Value>(arguments)
            .ok()
            .and_then(|v| serde_json::to_string_pretty(&v).ok())
            .unwrap_or_else(|| arguments.to_owned());
        out.push_str(&fenced(&pretty, "json"));
    }
    match result {
        Some(result) => out.push_str(&fenced(&result.content, "")),
        None => out.push_str("*No result was recorded.*\n\n"),
    }
    out.push_str("</details>\n\n");
    out
}

/// Whether the tool message at `at` answers a call made by the assistant
/// message before the run of results it is in.
fn answers_a_call(messages: &[Message], at: usize) -> bool {
    let id = &messages[at].tool_call_id;
    messages[..at]
        .iter()
        .rev()
        .find(|m| m.role != "tool")
        .is_some_and(|m| m.role == "assistant" && m.tool_calls.iter().any(|c| &c.id == id))
}

/// What a call's arguments say it works on, as its row in the window names
/// it: the path, command or address, on one line.
fn subject(arguments: &str) -> String {
    let Ok(fields) = serde_json::from_str::<Value>(arguments) else {
        return String::new();
    };
    ["path", "file_path", "command", "url", "pattern", "query"]
        .iter()
        .find_map(|key| fields[*key].as_str())
        .map(|text| crate::tools::one_line(text).chars().take(80).collect())
        .unwrap_or_default()
}

/// `text` in a code block whose fence is longer than any run of backticks
/// in it, so nothing inside can close it early.
fn fenced(text: &str, language: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest = longest.max(run);
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!(
        "{fence}{language}\n{}\n{fence}\n\n",
        text.trim_end_matches('\n')
    )
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

// -- Handing over -----------------------------------------------------------

/// A message as another tool is handed it.
#[derive(Debug, PartialEq)]
enum Turn {
    User(String),
    Assistant {
        text: String,
        model: String,
        calls: Vec<Call>,
    },
    /// A result for each call in the turn before, in the same order.
    Results(Vec<Answer>),
}

#[derive(Debug, PartialEq)]
struct Call {
    id: String,
    name: String,
    /// The JSON-encoded arguments as the model wrote them.
    arguments: String,
}

#[derive(Debug, PartialEq)]
struct Answer {
    id: String,
    text: String,
    is_error: bool,
}

/// `messages` as turns another tool can take: every call under an id its
/// API accepts, each answered right after the turn that made it, and what
/// neither side said left out.
fn turns(messages: &[Message]) -> Vec<Turn> {
    let mut out = Vec::new();
    let mut ids = Ids::default();
    let mut at = 0;
    while at < messages.len() {
        let message = &messages[at];
        at += 1;
        match message.role.as_str() {
            "user" if !message.content.trim().is_empty() => {
                out.push(Turn::User(message.content.clone()));
            }
            "assistant" => {
                let results: Vec<&Message> = messages[at..]
                    .iter()
                    .take_while(|m| m.role == "tool")
                    .collect();
                at += results.len();
                if message.content.trim().is_empty() && message.tool_calls.is_empty() {
                    continue;
                }
                let calls: Vec<Call> = message
                    .tool_calls
                    .iter()
                    .map(|call| Call {
                        id: ids.safe(&call.id),
                        name: safe_name(&call.name),
                        arguments: call.arguments.clone(),
                    })
                    .collect();
                let answers: Vec<Answer> = message
                    .tool_calls
                    .iter()
                    .zip(&calls)
                    .map(|(asked, call)| {
                        match results.iter().find(|r| r.tool_call_id == asked.id) {
                            Some(result) => Answer {
                                id: call.id.clone(),
                                text: result.content.clone(),
                                is_error: result.is_error,
                            },
                            None => Answer {
                                id: call.id.clone(),
                                text: NO_RESULT.to_owned(),
                                is_error: false,
                            },
                        }
                    })
                    .collect();
                out.push(Turn::Assistant {
                    text: message.content.clone(),
                    model: message.model.clone(),
                    calls,
                });
                if !answers.is_empty() {
                    out.push(Turn::Results(answers));
                }
            }
            // A result no call in the turn before it made, a failure the
            // window showed, and a picture sent with no words: nothing the
            // other tool's model could be handed as said.
            _ => {}
        }
    }
    out
}

/// Call ids made safe for every tool's API and kept apart: Claude's takes
/// only letters, digits, `_` and `-`, while some gateways mint ids such as
/// `functions.read_file:0`, and some reuse `call_0` on every turn.
#[derive(Default)]
struct Ids {
    taken: HashSet<String>,
}

impl Ids {
    fn safe(&mut self, id: &str) -> String {
        let base: String = id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .take(64)
            .collect();
        let base = if base.is_empty() {
            "call".to_owned()
        } else {
            base
        };
        let mut id = base.clone();
        let mut n = 1;
        while !self.taken.insert(id.clone()) {
            n += 1;
            id = format!("{base}_{n}");
        }
        id
    }
}

/// A tool's name as Claude's API takes one: letters, digits, `_` and `-`,
/// at most 128 of them.
fn safe_name(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(128)
        .collect();
    if safe.is_empty() {
        "tool".to_owned()
    } else {
        safe
    }
}

/// A call's arguments as the object Claude's API wants for its input; ones
/// that do not parse to an object are kept whole under `arguments`.
fn input_object(arguments: &str) -> Value {
    match serde_json::from_str::<Value>(arguments) {
        Ok(value @ Value::Object(_)) => value,
        _ if arguments.trim().is_empty() => json!({}),
        _ => json!({ "arguments": arguments }),
    }
}

fn to_claude_code(
    conversation: &Conversation,
    root: &Path,
    folder: &Path,
    now: DateTime<Utc>,
) -> Result<Continued, String> {
    let session_id = uuid()?;
    let text = claude_code_text(conversation, &session_id, folder, now)?;
    let path = root
        .join("projects")
        .join(claude_code_project(folder))
        .join(format!("{session_id}.jsonl"));
    write_whole(&path, &text)?;
    Ok(Continued {
        tool: Tool::ClaudeCode,
        command: format!("claude --resume {session_id}"),
        session_id,
        path,
        folder: folder.to_owned(),
    })
}

/// A Claude Code transcript of `conversation`: a record per turn, each
/// pointing at the one before, then the session's title.
fn claude_code_text(
    conversation: &Conversation,
    session_id: &str,
    folder: &Path,
    now: DateTime<Utc>,
) -> Result<String, String> {
    let turns = turns(&conversation.messages);
    if turns.is_empty() {
        return Err("This session has nothing in it to hand over yet.".to_owned());
    }
    let cwd = folder.to_string_lossy();
    let mut lines = Vec::with_capacity(turns.len() + 1);
    let mut parent: Option<String> = None;
    for (n, turn) in turns.iter().enumerate() {
        let (kind, message) = match turn {
            Turn::User(text) => ("user", json!({ "role": "user", "content": text })),
            Turn::Assistant { text, model, calls } => {
                let mut content = Vec::new();
                if !text.trim().is_empty() {
                    content.push(json!({ "type": "text", "text": text }));
                }
                for call in calls {
                    content.push(json!({
                        "type": "tool_use",
                        "id": call.id,
                        "name": call.name,
                        "input": input_object(&call.arguments),
                    }));
                }
                let message = json!({
                    "id": format!("msg_{}", uuid()?.replace('-', "")),
                    "type": "message",
                    "role": "assistant",
                    "model": if model.is_empty() { "letronna" } else { model.as_str() },
                    "content": content,
                    "stop_reason": if calls.is_empty() { "end_turn" } else { "tool_use" },
                    "stop_sequence": null,
                    "usage": { "input_tokens": 0, "output_tokens": 0 },
                });
                ("assistant", message)
            }
            Turn::Results(answers) => {
                let content: Vec<Value> = answers
                    .iter()
                    .map(|answer| {
                        json!({
                            "type": "tool_result",
                            "tool_use_id": answer.id,
                            "content": answer.text,
                            "is_error": answer.is_error,
                        })
                    })
                    .collect();
                ("user", json!({ "role": "user", "content": content }))
            }
        };
        let uuid = uuid()?;
        lines.push(
            json!({
                "parentUuid": parent,
                "isSidechain": false,
                "userType": "external",
                "cwd": cwd,
                "sessionId": session_id,
                "type": kind,
                "message": message,
                "uuid": uuid,
                "timestamp": stamp(turn_time(now, n, turns.len())),
            })
            .to_string(),
        );
        parent = Some(uuid);
    }
    if !conversation.title.trim().is_empty() {
        lines.push(
            json!({
                "type": "custom-title",
                "customTitle": conversation.title,
                "sessionId": session_id,
            })
            .to_string(),
        );
    }
    Ok(lines.join("\n") + "\n")
}

/// The folder under `~/.claude/projects` that Claude Code keeps `folder`'s
/// sessions in: every UTF-16 unit that is not an ASCII letter or digit made
/// a `-`, as its JavaScript does, with a hash of the path after a cut one.
fn claude_code_project(folder: &Path) -> String {
    let path = folder.to_string_lossy();
    let name: String = path
        .encode_utf16()
        .map(|unit| match char::from_u32(u32::from(unit)) {
            Some(c) if c.is_ascii_alphanumeric() => c,
            _ => '-',
        })
        .collect();
    if name.len() <= CLAUDE_CODE_NAME_MAX {
        return name;
    }
    // Claude Code's string hash: Java's, over UTF-16 units, made positive.
    let hash = path.encode_utf16().fold(0i32, |hash, unit| {
        hash.wrapping_shl(5)
            .wrapping_sub(hash)
            .wrapping_add(i32::from(unit))
    });
    format!(
        "{}-{}",
        &name[..CLAUDE_CODE_NAME_MAX],
        base36(i64::from(hash).unsigned_abs())
    )
}

fn base36(mut n: u64) -> String {
    const DIGITS: &[u8] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = Vec::new();
    loop {
        out.push(DIGITS[(n % 36) as usize]);
        n /= 36;
        if n == 0 {
            break;
        }
    }
    out.reverse();
    String::from_utf8(out).unwrap_or_default()
}

fn to_codex(
    conversation: &Conversation,
    root: &Path,
    folder: &Path,
    now: DateTime<Utc>,
) -> Result<Continued, String> {
    let session_id = uuid()?;
    let text = codex_text(conversation, &session_id, folder, now)?;
    // Codex files a session under the day and time it began, in local time.
    let local = now.with_timezone(&Local);
    let path = root
        .join("sessions")
        .join(local.format("%Y").to_string())
        .join(local.format("%m").to_string())
        .join(local.format("%d").to_string())
        .join(format!(
            "rollout-{}-{session_id}.jsonl",
            local.format("%Y-%m-%dT%H-%M-%S")
        ));
    write_whole(&path, &text)?;
    Ok(Continued {
        tool: Tool::Codex,
        command: format!("codex resume {session_id}"),
        session_id,
        path,
        folder: folder.to_owned(),
    })
}

/// A Codex rollout of `conversation`: the session's head, then each turn as
/// the items its model is handed, with the events its screen shows them by.
fn codex_text(
    conversation: &Conversation,
    session_id: &str,
    folder: &Path,
    now: DateTime<Utc>,
) -> Result<String, String> {
    let turns = turns(&conversation.messages);
    if turns.is_empty() {
        return Err("This session has nothing in it to hand over yet.".to_owned());
    }
    let started = stamp(turn_time(now, 0, turns.len() + 1));
    let line = |at: &str, kind: &str, payload: Value| {
        json!({ "timestamp": at, "type": kind, "payload": payload }).to_string()
    };
    let mut lines = vec![line(
        &started,
        "session_meta",
        json!({
            "id": session_id,
            "timestamp": started,
            "cwd": folder.to_string_lossy(),
            "originator": "letronna",
            "cli_version": env!("CARGO_PKG_VERSION"),
            "source": "cli",
            "model_provider": "openai",
        }),
    )];
    for (n, turn) in turns.iter().enumerate() {
        let at = stamp(turn_time(now, n + 1, turns.len() + 1));
        match turn {
            Turn::User(text) => {
                lines.push(line(
                    &at,
                    "response_item",
                    json!({
                        "type": "message",
                        "role": "user",
                        "content": [{ "type": "input_text", "text": text }],
                    }),
                ));
                lines.push(line(
                    &at,
                    "event_msg",
                    json!({ "type": "user_message", "message": text, "images": [] }),
                ));
            }
            Turn::Assistant { text, calls, .. } => {
                if !text.trim().is_empty() {
                    lines.push(line(
                        &at,
                        "response_item",
                        json!({
                            "type": "message",
                            "role": "assistant",
                            "content": [{ "type": "output_text", "text": text }],
                        }),
                    ));
                    lines.push(line(
                        &at,
                        "event_msg",
                        json!({ "type": "agent_message", "message": text }),
                    ));
                }
                for call in calls {
                    lines.push(line(
                        &at,
                        "response_item",
                        json!({
                            "type": "function_call",
                            "name": call.name,
                            "arguments": call.arguments,
                            "call_id": call.id,
                        }),
                    ));
                }
            }
            Turn::Results(answers) => {
                for answer in answers {
                    lines.push(line(
                        &at,
                        "response_item",
                        json!({
                            "type": "function_call_output",
                            "call_id": answer.id,
                            "output": answer.text,
                        }),
                    ));
                }
            }
        }
    }
    Ok(lines.join("\n") + "\n")
}

/// When the turn at `n` of `count` is said to have happened: a second apart,
/// the last one now, so the session sorts as the newest in either tool.
fn turn_time(now: DateTime<Utc>, n: usize, count: usize) -> DateTime<Utc> {
    let before = count.saturating_sub(n + 1);
    now - chrono::Duration::seconds(i64::try_from(before).unwrap_or(i64::MAX / 2))
}

fn stamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// Write `text` to `path` whole or not at all: the other tool may list the
/// folder at any moment, and must never find half a session there.
fn write_whole(path: &Path, text: &str) -> Result<(), String> {
    let failed = |e: std::io::Error| format!("Could not write {}: {e}", path.display());
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no folder.", path.display()))?;
    std::fs::create_dir_all(dir).map_err(failed)?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    // Named so neither tool's listing takes it for a session while it is
    // being written.
    let part = dir.join(format!(".{name}.part"));
    let written = crate::store::write_private(&part, text.as_bytes())
        .and_then(|_| std::fs::rename(&part, path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&part);
        return Err(failed(e));
    }
    Ok(())
}

/// `path` as the other tool will see its working folder: with links
/// resolved, as a process's own folder always is, so `/tmp` is the
/// `/private/tmp` that macOS gives a process started there.
fn real_path(path: &Path) -> PathBuf {
    let Ok(real) = std::fs::canonicalize(path) else {
        return path.to_owned();
    };
    // Windows gives the long form, `\\?\C:\...`, which no tool calls a folder by.
    let text = real.to_string_lossy();
    match text.strip_prefix(r"\\?\") {
        Some(share) if share.starts_with(r"UNC\") => PathBuf::from(format!(r"\\{}", &share[4..])),
        Some(short) => PathBuf::from(short),
        None => real.clone(),
    }
}

/// A random version-4 UUID, the form both tools name their sessions by.
fn uuid() -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| {
            "This computer would not give a random number to name the session by.".to_owned()
        })?;
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ToolCall;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-export-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn user(text: &str) -> Message {
        Message {
            role: "user".into(),
            content: text.into(),
            ..Default::default()
        }
    }

    fn assistant(text: &str, calls: &[(&str, &str, &str)]) -> Message {
        Message {
            role: "assistant".into(),
            content: text.into(),
            model: "test-model".into(),
            tool_calls: calls
                .iter()
                .map(|(id, name, arguments)| ToolCall {
                    id: (*id).into(),
                    name: (*name).into(),
                    arguments: (*arguments).into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    fn result(id: &str, name: &str, text: &str) -> Message {
        Message {
            role: "tool".into(),
            content: text.into(),
            tool_call_id: id.into(),
            name: name.into(),
            ..Default::default()
        }
    }

    /// A session with a bit of everything: thinking, a call answered, a
    /// call left dangling, a notice and a failure the window showed.
    fn session() -> Conversation {
        let mut thinking = assistant(
            "Let me look.",
            &[("tu_1", "read_file", r#"{"path":"src/auth.rs"}"#)],
        );
        thinking.reasoning = "The bug is probably in auth.".into();
        let notice = Message {
            notice: true,
            ..user("The job finished.")
        };
        let failure = Message {
            role: "failure".into(),
            content: "The gateway timed out.".into(),
            ..Default::default()
        };
        Conversation {
            id: 3,
            title: "Fix the login bug".into(),
            workspace: Some(PathBuf::from("/home/dev/project")),
            updated: 1_700_000_000,
            messages: vec![
                user("Fix the login bug"),
                thinking,
                result("tu_1", "read_file", "fn login() {}"),
                assistant(
                    "",
                    &[(
                        "functions.run:0",
                        "run_command",
                        r#"{"command":"cargo test"}"#,
                    )],
                ),
                notice,
                failure,
                assistant("Fixed it.", &[]),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn markdown_shows_each_call_with_its_result_and_folds_the_thinking() {
        let text = markdown(&session());
        assert!(text.starts_with("# Fix the login bug\n\nIn `/home/dev/project`"));
        assert!(text.contains("**You**\n\nFix the login bug"));
        assert!(text.contains("**test-model**"));
        assert!(text.contains("<summary>Thinking</summary>\n\nThe bug is probably in auth."));
        assert!(text.contains("<summary>read_file · src/auth.rs</summary>"));
        assert!(text.contains("```\nfn login() {}\n```"));
        // The result shows inside its call, not again on its own.
        assert_eq!(text.matches("fn login() {}").count(), 1);
        assert!(text.contains("<summary>run_command · cargo test</summary>"));
        assert!(text.contains("*No result was recorded.*"));
        assert!(text.contains("**Notice**\n\nThe job finished."));
        assert!(text.contains("**Error**\n\nThe gateway timed out."));
    }

    #[test]
    fn a_fence_outgrows_the_backticks_inside_it() {
        assert_eq!(fenced("plain", ""), "```\nplain\n```\n\n");
        let inner = "before\n```rust\nfn main() {}\n```\nafter";
        let block = fenced(inner, "");
        assert!(block.starts_with("````\n"));
        assert!(block.ends_with("\n````\n\n"));
    }

    #[test]
    fn a_summary_cannot_open_markup_of_its_own() {
        let call = call_markdown("run_command", r#"{"command":"echo </details> <b>"}"#, None);
        assert!(call.contains("<summary>run_command · echo &lt;/details&gt; &lt;b&gt;</summary>"));
    }

    #[test]
    fn json_keeps_the_messages_in_chat_shape_and_leaves_failures_out() {
        let text = json(&session()).unwrap();
        let doc: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(doc["format"], FORMAT);
        assert_eq!(doc["version"], VERSION);
        assert_eq!(doc["title"], "Fix the login bug");
        assert_eq!(doc["workspace"], "/home/dev/project");
        assert_eq!(doc["id"].as_str().unwrap().len(), 36);
        let messages = doc["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 6, "the failure is left out");
        assert_eq!(messages[1]["reasoning"], "The bug is probably in auth.");
        assert_eq!(messages[1]["tool_calls"][0]["type"], "function");
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"src/auth.rs"}"#
        );
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "tu_1");
        assert_eq!(messages[4]["notice"], true);
        // Two exports of the same session are two files, told apart.
        let again: Value = serde_json::from_str(&json(&session()).unwrap()).unwrap();
        assert_ne!(again["id"], doc["id"]);
    }

    #[test]
    fn every_call_is_answered_under_an_id_claude_accepts() {
        let turns = turns(&session().messages);
        assert_eq!(turns.len(), 7);
        assert_eq!(turns[0], Turn::User("Fix the login bug".into()));
        let Turn::Assistant { calls, .. } = &turns[3] else {
            panic!("the second call's turn is missing");
        };
        assert_eq!(calls[0].id, "functions_run_0");
        let Turn::Results(answers) = &turns[4] else {
            panic!("the dangling call was not answered");
        };
        assert_eq!(answers[0].id, "functions_run_0");
        assert_eq!(answers[0].text, NO_RESULT);
        // The notice reaches the model as the user's turn, as it did here;
        // the failure does not.
        assert_eq!(turns[5], Turn::User("The job finished.".into()));
        assert!(matches!(&turns[6], Turn::Assistant { text, .. } if text == "Fixed it."));
    }

    #[test]
    fn ids_a_gateway_reuses_stay_apart() {
        let messages = vec![
            user("go"),
            assistant("", &[("call_0", "read_file", "{}")]),
            result("call_0", "read_file", "a"),
            assistant("", &[("call_0", "read_file", "{}")]),
            result("call_0", "read_file", "b"),
        ];
        let turns = turns(&messages);
        let ids: Vec<&str> = turns
            .iter()
            .filter_map(|turn| match turn {
                Turn::Results(answers) => Some(answers[0].id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(ids, ["call_0", "call_0_2"]);
        let texts: Vec<&str> = turns
            .iter()
            .filter_map(|turn| match turn {
                Turn::Results(answers) => Some(answers[0].text.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(texts, ["a", "b"], "each result stays with its own call");
    }

    #[test]
    fn claude_code_names_a_project_folder_the_way_claude_code_does() {
        assert_eq!(
            claude_code_project(Path::new("/Users/dev/letronna")),
            "-Users-dev-letronna"
        );
        assert_eq!(
            claude_code_project(Path::new("/tmp/a.b_c-d/é")),
            "-tmp-a-b-c-d--"
        );
        // A character past the BMP is two UTF-16 units, so two dashes.
        assert_eq!(claude_code_project(Path::new("/x/🙂")), "-x---");
        // Past 200 the name is cut and Claude Code's hash of the path added:
        // Java's String.hashCode, made positive, in base 36.
        let long = format!("/{}", "a".repeat(250));
        let name = claude_code_project(Path::new(&long));
        let hash = long
            .encode_utf16()
            .fold(0i32, |h, u| h.wrapping_mul(31).wrapping_add(i32::from(u)));
        assert_eq!(
            name,
            format!(
                "-{}-{}",
                "a".repeat(199),
                base36(i64::from(hash).unsigned_abs())
            )
        );
    }

    #[test]
    fn base36_counts_as_javascript_does() {
        assert_eq!(base36(0), "0");
        assert_eq!(base36(35), "z");
        assert_eq!(base36(36), "10");
        assert_eq!(base36(2_147_483_648), "zik0zk");
    }

    #[test]
    fn a_claude_code_export_imports_back_as_the_same_conversation() {
        let root = scratch("claude-round");
        let folder = scratch("claude-round-folder");
        let continued = to_claude_code(&session(), &root, &folder, Utc::now()).unwrap();
        assert_eq!(continued.tool, Tool::ClaudeCode);
        assert_eq!(
            continued.path,
            root.join("projects")
                .join(claude_code_project(&folder))
                .join(format!("{}.jsonl", continued.session_id))
        );
        assert_eq!(
            continued.command,
            format!("claude --resume {}", continued.session_id)
        );
        assert!(
            !continued
                .path
                .with_file_name(format!(".{}.jsonl.part", continued.session_id))
                .exists()
        );

        let text = std::fs::read_to_string(&continued.path).unwrap();
        let records: Vec<Value> = text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        // Each record points at the one before it, the first at none.
        assert!(records[0]["parentUuid"].is_null());
        assert_eq!(records[1]["parentUuid"], records[0]["uuid"]);
        assert!(
            records
                .iter()
                .take(7)
                .all(|r| r["sessionId"] == continued.session_id.as_str())
        );
        assert_eq!(records.last().unwrap()["type"], "custom-title");
        assert_eq!(records.last().unwrap()["customTitle"], "Fix the login bug");
        // Thinking stays behind: it has no signature Claude would take back.
        assert!(!text.contains("probably in auth"));

        let candidate = crate::import::candidate_at(&continued.path).unwrap();
        assert_eq!(candidate.tool, Tool::ClaudeCode);
        assert_eq!(candidate.session_id, continued.session_id);
        let back = crate::import::import(&candidate).unwrap();
        assert_eq!(back.workspace.as_deref(), Some(folder.as_path()));
        let roles: Vec<&str> = back.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(
            roles,
            [
                "user",
                "assistant",
                "tool",
                "assistant",
                "tool",
                "user",
                "assistant"
            ]
        );
        assert_eq!(back.messages[1].tool_calls[0].name, "read_file");
        assert_eq!(
            back.messages[1].tool_calls[0].arguments,
            r#"{"path":"src/auth.rs"}"#
        );
        assert_eq!(back.messages[2].content, "fn login() {}");
        assert_eq!(back.messages[4].content, NO_RESULT);
        assert_eq!(back.messages[6].content, "Fixed it.");
    }

    #[test]
    fn a_codex_export_imports_back_as_the_same_conversation() {
        let root = scratch("codex-round");
        let folder = scratch("codex-round-folder");
        let now = Utc::now();
        let continued = to_codex(&session(), &root, &folder, now).unwrap();
        let local = now.with_timezone(&Local);
        let day = ["%Y", "%m", "%d"]
            .iter()
            .fold(root.join("sessions"), |dir, part| {
                dir.join(local.format(part).to_string())
            });
        assert_eq!(continued.path.parent().unwrap(), day);
        let name = continued
            .path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(
            name.starts_with("rollout-")
                && name.ends_with(&format!("-{}.jsonl", continued.session_id))
        );
        assert_eq!(
            continued.command,
            format!("codex resume {}", continued.session_id)
        );

        let candidate = crate::import::candidate_at(&continued.path).unwrap();
        assert_eq!(candidate.tool, Tool::Codex);
        assert_eq!(candidate.session_id, continued.session_id);
        let back = crate::import::import(&candidate).unwrap();
        assert_eq!(back.title, "Fix the login bug");
        assert_eq!(back.workspace.as_deref(), Some(folder.as_path()));
        let roles: Vec<&str> = back.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(
            roles,
            [
                "user",
                "assistant",
                "tool",
                "assistant",
                "tool",
                "user",
                "assistant"
            ]
        );
        assert_eq!(back.messages[3].tool_calls[0].id, "functions_run_0");
        assert_eq!(
            back.messages[3].tool_calls[0].arguments,
            r#"{"command":"cargo test"}"#
        );
        assert_eq!(back.messages[4].tool_call_id, "functions_run_0");
    }

    #[test]
    fn a_json_export_imports_back_with_its_calls_and_notices() {
        let dir = scratch("json-round");
        let path = dir.join("session.json");
        std::fs::write(&path, json(&session()).unwrap()).unwrap();

        let candidate = crate::import::candidate_at(&path).unwrap();
        assert_eq!(candidate.tool, Tool::Letronna);
        assert_eq!(candidate.title, "Fix the login bug");
        assert_eq!(
            candidate.workspace,
            Some(PathBuf::from("/home/dev/project"))
        );
        let back = crate::import::import(&candidate).unwrap();
        assert_eq!(back.skipped, 0);
        let original: Vec<Message> = session()
            .messages
            .into_iter()
            .filter(|m| m.role != "failure")
            .collect();
        assert_eq!(back.messages.len(), original.len());
        for (got, want) in back.messages.iter().zip(&original) {
            assert_eq!(got.role, want.role);
            assert_eq!(got.content, want.content);
            assert_eq!(got.reasoning, want.reasoning);
            assert_eq!(got.model, want.model);
            assert_eq!(got.tool_calls, want.tool_calls);
            assert_eq!(got.tool_call_id, want.tool_call_id);
            assert_eq!(got.name, want.name);
            assert_eq!(got.notice, want.notice);
        }
    }

    #[test]
    fn a_session_with_nothing_said_is_not_handed_over() {
        let empty = Conversation {
            title: "Empty".into(),
            ..Default::default()
        };
        let root = scratch("nothing");
        let folder = scratch("nothing-folder");
        let error = to_claude_code(&empty, &root, &folder, Utc::now()).unwrap_err();
        assert_eq!(error, "This session has nothing in it to hand over yet.");
        assert!(!root.join("projects").exists(), "nothing was written");
        assert!(to_codex(&empty, &root, &folder, Utc::now()).is_err());
    }

    #[test]
    fn exports_never_land_on_the_same_file_twice() {
        let root = scratch("twice");
        let folder = scratch("twice-folder");
        let first = to_claude_code(&session(), &root, &folder, Utc::now()).unwrap();
        let second = to_claude_code(&session(), &root, &folder, Utc::now()).unwrap();
        assert_ne!(first.path, second.path);
        assert!(first.path.exists() && second.path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn an_exported_session_is_readable_by_its_owner_alone() {
        use std::os::unix::fs::PermissionsExt;
        let root = scratch("private");
        let folder = scratch("private-folder");
        let continued = to_codex(&session(), &root, &folder, Utc::now()).unwrap();
        let mode = std::fs::metadata(&continued.path)
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn the_turns_run_up_to_now_a_second_apart() {
        let now = Utc::now();
        assert_eq!(turn_time(now, 2, 3), now);
        assert_eq!(turn_time(now, 0, 3), now - chrono::Duration::seconds(2));
        assert!(stamp(now).ends_with('Z'));
    }

    #[test]
    fn arguments_that_are_not_an_object_are_kept_whole() {
        assert_eq!(input_object(r#"{"a":1}"#), json!({ "a": 1 }));
        assert_eq!(input_object(""), json!({}));
        assert_eq!(
            input_object(r#"{"cut off"#),
            json!({ "arguments": r#"{"cut off"# })
        );
        assert_eq!(input_object("[1]"), json!({ "arguments": "[1]" }));
    }

    #[test]
    fn a_uuid_is_version_four() {
        let id = uuid().unwrap();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert!(matches!(&id[19..20], "8" | "9" | "a" | "b"));
    }

    /// Hands a session to the real Claude Code and asks it what was said.
    /// Uses the signed-in Claude Code on this machine, one short reply.
    #[test]
    #[ignore = "needs Claude Code installed and signed in; run with --ignored"]
    fn a_live_claude_code_resumes_an_exported_session() {
        let folder = scratch("live-claude");
        let conversation = Conversation {
            title: "Live export check".into(),
            workspace: Some(folder.clone()),
            messages: vec![
                user("Remember this word for later: periwinkle."),
                assistant(
                    "",
                    &[("functions.read:0", "read_file", r#"{"path":"notes.txt"}"#)],
                ),
                result("functions.read:0", "read_file", "The notes say: tangerine."),
                assistant(
                    "I will remember periwinkle, and the notes say tangerine.",
                    &[],
                ),
            ],
            ..Default::default()
        };
        let continued = continue_in(&conversation, Tool::ClaudeCode).unwrap();
        let output = std::process::Command::new("claude")
            .args(["-p", "--resume", &continued.session_id])
            .arg("Which word did I ask you to remember, and what did the notes say? Answer with just the two words.")
            .current_dir(&continued.folder)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let _ = std::fs::remove_dir_all(continued.path.parent().unwrap());
        let said = String::from_utf8_lossy(&output.stdout).to_lowercase();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            said.contains("periwinkle") && said.contains("tangerine"),
            "{said}"
        );
    }

    /// Hands a session to the real Codex and asks it what was said. Uses the
    /// signed-in Codex on this machine, one short reply.
    #[test]
    #[ignore = "needs Codex installed and signed in; run with --ignored"]
    fn a_live_codex_resumes_an_exported_session() {
        let folder = scratch("live-codex");
        let conversation = Conversation {
            title: "Live export check".into(),
            workspace: Some(folder.clone()),
            messages: vec![
                user("Remember this word for later: periwinkle."),
                assistant("", &[("toolu_01", "read_file", r#"{"path":"notes.txt"}"#)]),
                result("toolu_01", "read_file", "The notes say: tangerine."),
                assistant(
                    "I will remember periwinkle, and the notes say tangerine.",
                    &[],
                ),
            ],
            ..Default::default()
        };
        let continued = continue_in(&conversation, Tool::Codex).unwrap();
        let output = std::process::Command::new("codex")
            .args(["exec", "--skip-git-repo-check", "-s", "read-only", "resume"])
            .arg(&continued.session_id)
            .arg("Which word did I ask you to remember, and what did the notes say? Answer with just the two words.")
            .current_dir(&continued.folder)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        let _ = std::fs::remove_file(&continued.path);
        let said = String::from_utf8_lossy(&output.stdout).to_lowercase();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            said.contains("periwinkle") && said.contains("tangerine"),
            "{said}"
        );
    }
}
