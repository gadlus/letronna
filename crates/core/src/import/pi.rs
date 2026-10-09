//! Sessions in pi's format, which OpenClaw writes too, as it runs on pi's
//! agent: `~/.pi/agent/sessions/<folder>/<time>_<id>.jsonl` for pi and
//! `~/.openclaw/agents/<agent>/sessions/<id>.jsonl` for OpenClaw.
//!
//! A session there is a tree. Every entry names its parent, a branch begun
//! from an earlier message hangs off it, and the session goes on from the
//! newest entry, so what is imported is that entry's line back to the root,
//! as pi shows it. Messages pi turns into the user's turn for its model --
//! a `!` command run, a note from an extension, the summary of a branch
//! left behind -- come in as notices, which the model still sees.

use super::{
    Imported, Source, Tool, first_user_title, head_text, modified_secs, nothing_to_import,
    short_title, subdirs,
};
use crate::chat::{MAX_TOOL_CHARS, Message, ToolCall};
use crate::tools::keep_ends;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// pi's folder: `PI_CODING_AGENT_DIR`, as pi takes it, or `~/.pi/agent`.
pub(super) fn pi_root() -> Option<PathBuf> {
    std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::tools::home().map(|home| home.join(".pi/agent")))
}

/// The lines read to find the first thing a session was sent: OpenClaw puts
/// a few changes of model and thinking before it.
const RUN_LINES: usize = 20;

pub(super) fn openclaw_root() -> Option<PathBuf> {
    crate::tools::home().map(|home| home.join(".openclaw"))
}

/// Every pi session under `root`, unread: one folder per project.
pub(super) fn pi_files(root: &Path) -> Vec<PathBuf> {
    subdirs(&root.join("sessions"))
        .iter()
        .flat_map(|dir| sessions_in(dir))
        .collect()
}

/// Every OpenClaw session under `root` a person took part in: one folder
/// per agent. Runs OpenClaw made on its own, on a schedule or as a helper,
/// are left out, or an hourly job would fill the list. The files it
/// renames on a reset or delete keep their place but not their ending, and
/// are left out with it.
pub(super) fn openclaw_files(root: &Path) -> Vec<PathBuf> {
    subdirs(&root.join("agents"))
        .iter()
        .flat_map(|agent| {
            let dir = agent.join("sessions");
            let runs = runs_in(&dir);
            sessions_in(&dir)
                .into_iter()
                .filter(move |path| !is_run(path, &runs))
        })
        .collect()
}

/// The sessions OpenClaw's index says it ran on its own, by id: its keys
/// read `agent:<agent>:<kind>:...`.
fn runs_in(dir: &Path) -> HashSet<String> {
    let Ok(text) = std::fs::read_to_string(dir.join("sessions.json")) else {
        return HashSet::new();
    };
    let Ok(Value::Object(index)) = serde_json::from_str::<Value>(&text) else {
        return HashSet::new();
    };
    index
        .iter()
        .filter(|(key, _)| matches!(key.split(':').nth(2), Some("cron" | "subagent")))
        .filter_map(|(_, session)| session["sessionId"].as_str().map(str::to_owned))
        .collect()
}

/// Whether the session at `path` is a run OpenClaw made on its own: one its
/// index says so of, or, for one older than the index remembers, one that
/// begins with the prompt a schedule sends.
fn is_run(path: &Path, runs: &HashSet<String>) -> bool {
    if path
        .file_stem()
        .and_then(|s| s.to_str())
        .is_some_and(|id| runs.contains(id))
    {
        return true;
    }
    super::read_lines(path)
        .into_iter()
        .flatten()
        .take(RUN_LINES)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .find(|r| r["type"] == "message" && r["message"]["role"] == "user")
        .is_some_and(|r| {
            content_text(&r["message"]["content"])
                .0
                .starts_with("[cron:")
        })
}

fn sessions_in(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .collect()
}

/// A session's id, title and project folder, from its first bytes: the
/// header holds the id and folder, and the title is the first thing said
/// unless the session was given a name, which only a full read would find.
pub(super) fn head(path: &Path) -> (String, String, Option<PathBuf>) {
    let text = head_text(path);
    let mut records = text
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok());
    let header = records
        .by_ref()
        .find(|r| r["type"] == "session")
        .unwrap_or(Value::Null);
    let title = records
        .filter(|r| r["type"] == "message" && r["message"]["role"] == "user")
        .map(|r| content_text(&r["message"]["content"]).0)
        .find(|text| !text.trim().is_empty())
        .map(|text| short_title(&text))
        .unwrap_or_default();
    (session_id(&header, path), title, folder(&header))
}

pub(super) fn import(path: &Path, tool: Tool) -> Result<Imported, String> {
    let mut header = Value::Null;
    let mut entries = Vec::new();
    let mut skipped = 0usize;
    for line in super::read_lines(path)? {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            skipped += 1;
            continue;
        };
        if record["type"] == "session" {
            header = record;
        } else {
            entries.push(record);
        }
    }
    let mut name = None;
    let mut messages = Vec::new();
    for entry in branch(entries) {
        match entry["type"].as_str().unwrap_or_default() {
            "message" => message(&entry["message"], &mut messages, &mut skipped),
            "custom_message" => {
                let (text, pictures) = content_text(&entry["content"]);
                skipped += pictures;
                notice(text, &mut messages);
            }
            "session_info" => {
                name = entry["name"].as_str().map(str::to_owned).or(name);
            }
            // Model and thinking changes, labels, usage, compaction marks
            // and extensions' own state: pi's bookkeeping, not anything said.
            _ => {}
        }
    }
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    let title = match name.as_deref().map(str::trim) {
        Some(name) if !name.is_empty() => short_title(name),
        _ => first_user_title(&messages),
    };
    Ok(Imported {
        source: Source {
            tool,
            session_id: session_id(&header, path),
            path: path.to_owned(),
        },
        title,
        workspace: folder(&header),
        updated: modified_secs(path),
        messages,
        skipped,
    })
}

fn session_id(header: &Value, path: &Path) -> String {
    header["id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .or_else(|| path.file_stem().and_then(|s| s.to_str()))
        .unwrap_or_default()
        .to_owned()
}

fn folder(header: &Value) -> Option<PathBuf> {
    header["cwd"]
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}

/// The entries on the session's current branch, in order: the newest one
/// and its parents back to the root. A file from before entries had ids is
/// one line of them already.
fn branch(entries: Vec<Value>) -> Vec<Value> {
    if entries.iter().any(|e| e["id"].as_str().is_none()) {
        return entries;
    }
    let at: HashMap<&str, usize> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| Some((e["id"].as_str()?, i)))
        .collect();
    let mut kept = vec![false; entries.len()];
    let mut next = entries.len().checked_sub(1);
    while let Some(i) = next {
        // A parent named twice would otherwise loop.
        if kept[i] {
            break;
        }
        kept[i] = true;
        next = entries[i]["parentId"]
            .as_str()
            .and_then(|parent| at.get(parent).copied());
    }
    entries
        .into_iter()
        .zip(kept)
        .filter_map(|(entry, kept)| kept.then_some(entry))
        .collect()
}

/// One of pi's messages as this app's.
fn message(message: &Value, out: &mut Vec<Message>, skipped: &mut usize) {
    match message["role"].as_str().unwrap_or_default() {
        "user" => {
            let (text, pictures) = content_text(&message["content"]);
            *skipped += pictures;
            if !text.trim().is_empty() {
                out.push(Message {
                    role: "user".into(),
                    content: text,
                    ..Default::default()
                });
            }
        }
        "assistant" => {
            let mut said = Vec::new();
            let mut thought = Vec::new();
            let mut tool_calls = Vec::new();
            for block in message["content"].as_array().into_iter().flatten() {
                match block["type"].as_str().unwrap_or_default() {
                    "text" => said.extend(block["text"].as_str().map(str::to_owned)),
                    "thinking" => thought.extend(block["thinking"].as_str().map(str::to_owned)),
                    "toolCall" => {
                        let (Some(id), Some(name)) = (
                            block["id"].as_str().filter(|id| !id.is_empty()),
                            block["name"].as_str().filter(|n| !n.is_empty()),
                        ) else {
                            *skipped += 1;
                            continue;
                        };
                        tool_calls.push(ToolCall {
                            id: id.to_owned(),
                            name: name.to_owned(),
                            arguments: match &block["arguments"] {
                                Value::Null => "{}".to_owned(),
                                Value::String(text) => text.clone(),
                                arguments => arguments.to_string(),
                            },
                        });
                    }
                    _ => *skipped += 1,
                }
            }
            let content = said.join("\n");
            // A request that failed leaves an empty reply with its error.
            if content.trim().is_empty() && tool_calls.is_empty() {
                return;
            }
            out.push(Message {
                role: "assistant".into(),
                content,
                reasoning: thought.join("\n"),
                model: message["model"].as_str().unwrap_or_default().to_owned(),
                tool_calls,
                ..Default::default()
            });
        }
        "toolResult" => {
            let Some(id) = message["toolCallId"].as_str().filter(|id| !id.is_empty()) else {
                *skipped += 1;
                return;
            };
            let (text, pictures) = content_text(&message["content"]);
            *skipped += pictures;
            out.push(Message {
                role: "tool".into(),
                content: keep_ends(text, MAX_TOOL_CHARS, ""),
                tool_call_id: id.to_owned(),
                name: message["toolName"].as_str().unwrap_or_default().to_owned(),
                is_error: message["isError"].as_bool().unwrap_or(false),
                ..Default::default()
            });
        }
        "bashExecution" if message["excludeFromContext"].as_bool() != Some(true) => {
            let command = message["command"].as_str().unwrap_or_default();
            let output = message["output"].as_str().unwrap_or_default();
            notice(format!("Ran `{command}`:\n{output}"), out);
        }
        "custom" => {
            let (text, pictures) = content_text(&message["content"]);
            *skipped += pictures;
            notice(text, out);
        }
        "branchSummary" => {
            let summary = message["summary"].as_str().unwrap_or_default();
            notice(format!("Summary of a branch left behind:\n{summary}"), out);
        }
        // A `!!` command run kept from the model, the system prompt, and
        // a compaction's summary of messages this import keeps in full.
        _ => {}
    }
}

fn notice(text: String, out: &mut Vec<Message>) {
    if !text.trim().is_empty() {
        out.push(Message {
            role: "user".into(),
            content: text,
            notice: true,
            ..Default::default()
        });
    }
}

/// The text of a content that is a string or a list of blocks, and how
/// many pictures it held, which have no place here.
fn content_text(content: &Value) -> (String, usize) {
    if let Some(text) = content.as_str() {
        return (text.to_owned(), 0);
    }
    let blocks = content.as_array().map_or(&[][..], Vec::as_slice);
    let text = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, blocks.iter().filter(|b| b["type"] == "image").count())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SESSION: &str = concat!(
        r#"{"type":"session","version":3,"id":"p-1","timestamp":"2026-10-01T00:00:00Z","cwd":"/home/dev/webshop"}"#,
        "\n",
        r#"{"type":"model_change","id":"a","parentId":null,"timestamp":"t","provider":"anthropic","modelId":"claude-opus-4-6"}"#,
        "\n",
        r#"{"type":"message","id":"b","parentId":"a","timestamp":"t","message":{"role":"user","content":[{"type":"text","text":"Add a health check"}],"timestamp":1}}"#,
        "\n",
        r#"{"type":"message","id":"c","parentId":"b","timestamp":"t","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Routes first.","thinkingSignature":"sig"},{"type":"text","text":"Let me look."},{"type":"toolCall","id":"call_1","name":"read","arguments":{"path":"app.py"}}],"model":"claude-opus-4-6","stopReason":"toolUse"}}"#,
        "\n",
        r#"{"type":"message","id":"d","parentId":"c","timestamp":"t","message":{"role":"toolResult","toolCallId":"call_1","toolName":"read","content":[{"type":"text","text":"app = Flask()"}],"isError":false}}"#,
        "\n",
        r#"{"type":"message","id":"e","parentId":"d","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"A first try."}],"model":"claude-opus-4-6"}}"#,
        "\n",
        r#"{"type":"message","id":"f","parentId":"d","timestamp":"t","message":{"role":"bashExecution","command":"pytest","output":"1 passed","exitCode":0,"cancelled":false,"truncated":false,"timestamp":2}}"#,
        "\n",
        r#"{"type":"message","id":"g","parentId":"f","timestamp":"t","message":{"role":"assistant","content":[],"stopReason":"error","errorMessage":"overloaded"}}"#,
        "\n",
        r#"{"type":"message","id":"h","parentId":"g","timestamp":"t","message":{"role":"assistant","content":[{"type":"text","text":"Added the endpoint."}],"model":"claude-opus-4-6"}}"#,
        "\n",
        r#"{"type":"session_info","id":"i","parentId":"h","timestamp":"t","name":"Health check"}"#,
        "\n",
    );

    #[test]
    fn the_branch_the_session_goes_on_from_is_the_one_imported() {
        let dir = std::env::temp_dir().join("letronna-import-pi");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("2026-10-01T00-00-00_p-1.jsonl");
        std::fs::write(&path, SESSION).unwrap();

        let imported = import(&path, Tool::Pi).unwrap();
        assert_eq!(imported.source.tool, Tool::Pi);
        assert_eq!(imported.source.session_id, "p-1");
        assert_eq!(imported.workspace, Some(PathBuf::from("/home/dev/webshop")));
        assert_eq!(imported.title, "Health check");
        assert_eq!(imported.skipped, 0);
        let said: Vec<&str> = imported
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        // "A first try." is on a branch left behind, and the failed request
        // left nothing said.
        assert_eq!(
            said,
            [
                "Add a health check",
                "Let me look.",
                "app = Flask()",
                "Ran `pytest`:\n1 passed",
                "Added the endpoint."
            ]
        );
        let reply = &imported.messages[1];
        assert_eq!(reply.reasoning, "Routes first.");
        assert_eq!(reply.model, "claude-opus-4-6");
        assert_eq!(reply.tool_calls[0].arguments, r#"{"path":"app.py"}"#);
        assert_eq!(imported.messages[2].tool_call_id, "call_1");
        assert!(imported.messages[3].notice);

        let (id, title, folder) = head(&path);
        assert_eq!(id, "p-1");
        assert_eq!(title, "Add a health check");
        assert_eq!(folder, Some(PathBuf::from("/home/dev/webshop")));
    }

    #[test]
    fn a_session_from_before_ids_is_read_in_order() {
        let entries = vec![
            serde_json::json!({ "type": "message", "message": { "role": "user", "content": "one" } }),
            serde_json::json!({ "type": "message", "message": { "role": "user", "content": "two" } }),
        ];
        assert_eq!(branch(entries).len(), 2);
    }

    #[test]
    fn a_parent_named_in_a_loop_ends_the_branch() {
        let entries = vec![
            serde_json::json!({ "id": "a", "parentId": "b" }),
            serde_json::json!({ "id": "b", "parentId": "a" }),
        ];
        assert_eq!(branch(entries).len(), 2);
    }

    #[test]
    fn openclaw_leaves_out_its_own_runs_and_the_files_it_set_aside() {
        let root = std::env::temp_dir().join("letronna-import-openclaw");
        let _ = std::fs::remove_dir_all(&root);
        let sessions = root.join("agents/main/sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        std::fs::write(sessions.join("s-1.jsonl"), SESSION).unwrap();
        std::fs::write(
            sessions.join("s-2.jsonl.deleted.2026-10-07T21-56-45.217Z"),
            SESSION,
        )
        .unwrap();
        // An hourly job's run, indexed as one, and an older one the index
        // has forgotten, told by the prompt the schedule sent.
        std::fs::write(sessions.join("s-3.jsonl"), SESSION).unwrap();
        let older = SESSION.replace(
            "Add a health check",
            "[cron:8fe1 Hourly summary] Sum up the feed",
        );
        std::fs::write(sessions.join("s-4.jsonl"), older).unwrap();
        std::fs::write(
            sessions.join("sessions.json"),
            r#"{"agent:main:main":{"sessionId":"s-1"},"agent:main:cron:8fe1":{"sessionId":"s-3"}}"#,
        )
        .unwrap();
        assert_eq!(openclaw_files(&root), [sessions.join("s-1.jsonl")]);
    }
}
