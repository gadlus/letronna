//! Kiro CLI's sessions: `~/.kiro/sessions/cli/<id>.jsonl`, a record for
//! each prompt, reply and set of tool results, beside `<id>.json`, which
//! holds the session's title and folder.
//!
//! A `Clear` record is the user's `/clear`, after which Kiro CLI's model
//! starts again with nothing, so only what came after it is imported --
//! unless nothing did, as in a session cleared and then left, and then the
//! part before it is.

use super::{
    Imported, Source, Tool, first_user_title, head_text, modified_secs, nothing_to_import,
    short_title,
};
use crate::chat::{MAX_TOOL_CHARS, Message, ToolCall};
use crate::tools::keep_ends;
use serde_json::Value;
use std::path::{Path, PathBuf};

pub(super) fn root() -> Option<PathBuf> {
    crate::tools::home().map(|home| home.join(".kiro"))
}

/// Every session under `root` with something in it, unread but for its
/// details: one Kiro CLI started as a helper for another is left out.
pub(super) fn files(root: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(root.join("sessions/cli"))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .filter(|path| std::fs::metadata(path).is_ok_and(|m| m.len() > 0))
        .filter(|path| details(path)["session_created_reason"] != "subagent")
        .collect()
}

/// A session's id, title and project folder, from its details, or for a
/// session Kiro CLI has not titled, the first prompt in its first bytes.
pub(super) fn head(path: &Path) -> (String, String, Option<PathBuf>) {
    let details = details(path);
    let title = own_title(&details).unwrap_or_else(|| {
        head_text(path)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|r| r["kind"] == "Prompt")
            .map(|r| text(&r["data"]["content"]))
            .find(|text| !text.trim().is_empty())
            .map(|text| short_title(&text))
            .unwrap_or_default()
    });
    (session_id(&details, path), title, folder(&details))
}

pub(super) fn import(path: &Path) -> Result<Imported, String> {
    let mut messages = Vec::new();
    // What came before the last `/clear`, for a session left right after it.
    let mut before = Vec::new();
    let mut skipped = 0usize;
    for line in super::read_lines(path)? {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            skipped += 1;
            continue;
        };
        let content = &record["data"]["content"];
        match record["kind"].as_str().unwrap_or_default() {
            "Prompt" => {
                let said = text(content);
                if !said.trim().is_empty() {
                    messages.push(Message {
                        role: "user".into(),
                        content: said,
                        ..Default::default()
                    });
                }
            }
            "AssistantMessage" => reply(content, &mut messages, &mut skipped),
            "ToolResults" => results(content, &mut messages, &mut skipped),
            "Clear" => {
                if !messages.is_empty() {
                    before = std::mem::take(&mut messages);
                }
                skipped = 0;
            }
            _ => {}
        }
    }
    if messages.is_empty() {
        messages = before;
    }
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    let details = details(path);
    Ok(Imported {
        source: Source {
            tool: Tool::Kiro,
            session_id: session_id(&details, path),
            path: path.to_owned(),
        },
        title: own_title(&details).unwrap_or_else(|| first_user_title(&messages)),
        workspace: folder(&details),
        updated: modified_secs(path),
        messages,
        skipped,
    })
}

/// The session's details, in the `.json` beside its records, or null.
fn details(path: &Path) -> Value {
    std::fs::read_to_string(path.with_extension("json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

fn session_id(details: &Value, path: &Path) -> String {
    details["session_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .or_else(|| path.file_stem().and_then(|s| s.to_str()))
        .unwrap_or_default()
        .to_owned()
}

fn own_title(details: &Value) -> Option<String> {
    let title = details["title"].as_str()?.trim();
    (!title.is_empty()).then(|| short_title(title))
}

fn folder(details: &Value) -> Option<PathBuf> {
    details["cwd"]
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}

/// The text blocks of a record's content, joined.
fn text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["kind"] == "text")
        .filter_map(|block| block["data"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn reply(content: &Value, out: &mut Vec<Message>, skipped: &mut usize) {
    let mut thought = Vec::new();
    let mut tool_calls = Vec::new();
    for block in content.as_array().into_iter().flatten() {
        let data = &block["data"];
        match block["kind"].as_str().unwrap_or_default() {
            "text" => {}
            "thinking" => thought.extend(data["text"].as_str().map(str::to_owned)),
            "toolUse" => {
                let (Some(id), Some(name)) = (
                    data["toolUseId"].as_str().filter(|id| !id.is_empty()),
                    data["name"].as_str().filter(|n| !n.is_empty()),
                ) else {
                    *skipped += 1;
                    continue;
                };
                tool_calls.push(ToolCall {
                    id: id.to_owned(),
                    name: name.to_owned(),
                    arguments: match &data["input"] {
                        Value::Null => "{}".to_owned(),
                        input => input.to_string(),
                    },
                });
            }
            _ => *skipped += 1,
        }
    }
    let said = text(content);
    if said.trim().is_empty() && tool_calls.is_empty() {
        return;
    }
    out.push(Message {
        role: "assistant".into(),
        content: said,
        reasoning: thought.join("\n"),
        tool_calls,
        ..Default::default()
    });
}

/// A set of tool results, each answering a call by its id; a result in
/// several parts, text or JSON, has them joined.
fn results(content: &Value, out: &mut Vec<Message>, skipped: &mut usize) {
    for block in content.as_array().into_iter().flatten() {
        let data = &block["data"];
        let Some(id) = data["toolUseId"].as_str().filter(|id| !id.is_empty()) else {
            *skipped += 1;
            continue;
        };
        let parts: Vec<String> = data["content"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| match part["kind"].as_str()? {
                "text" => part["data"].as_str().map(str::to_owned),
                "json" => Some(part["data"].to_string()),
                _ => None,
            })
            .collect();
        let name = out
            .iter()
            .rev()
            .flat_map(|m| &m.tool_calls)
            .find(|call| call.id == id)
            .map(|call| call.name.clone())
            .unwrap_or_default();
        out.push(Message {
            role: "tool".into(),
            content: keep_ends(parts.join("\n"), MAX_TOOL_CHARS, ""),
            tool_call_id: id.to_owned(),
            name,
            is_error: data["status"] == "error",
            ..Default::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORDS: &str = concat!(
        r#"{"version":"v1","kind":"Prompt","data":{"message_id":"1","content":[{"kind":"text","data":"Something to throw away"}],"meta":{"timestamp":1}}}"#,
        "\n",
        r#"{"version":"v1","kind":"Clear","data":{}}"#,
        "\n",
        r#"{"version":"v1","kind":"Prompt","data":{"message_id":"2","content":[{"kind":"text","data":"Add a health check"}],"meta":{"timestamp":2}}}"#,
        "\n",
        r#"{"version":"v1","kind":"AssistantMessage","data":{"message_id":"3","content":[{"kind":"thinking","data":{"text":"Routes first.","signature":"sig","redactedContent":[]}},{"kind":"text","data":"Let me look."},{"kind":"toolUse","data":{"toolUseId":"tooluse_1","name":"fs_read","input":{"path":"app.py"}}},{"kind":"toolUse","data":{"toolUseId":"tooluse_2","name":"execute_bash","input":{"command":"pytest"}}}]}}"#,
        "\n",
        r#"{"version":"v1","kind":"ToolResults","data":{"message_id":"4","content":[{"kind":"toolResult","data":{"toolUseId":"tooluse_1","content":[{"kind":"text","data":"app = Flask()"}],"status":"success"}},{"kind":"toolResult","data":{"toolUseId":"tooluse_2","content":[{"kind":"json","data":{"exit_status":"1","stdout":"","stderr":"no tests"}}],"status":"error"}}],"results":{}}}"#,
        "\n",
        r#"{"version":"v1","kind":"AssistantMessage","data":{"message_id":"5","content":[{"kind":"text","data":"Added the endpoint."}]}}"#,
        "\n",
    );

    #[test]
    fn a_session_starts_again_where_it_was_cleared() {
        let dir = std::env::temp_dir().join("letronna-import-kiro/sessions/cli");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("k-1.jsonl");
        std::fs::write(&path, RECORDS).unwrap();
        std::fs::write(
            dir.join("k-1.json"),
            r#"{"session_id":"k-1","cwd":"/home/dev/webshop","title":"Health check endpoint"}"#,
        )
        .unwrap();

        let imported = import(&path).unwrap();
        assert_eq!(imported.source.tool, Tool::Kiro);
        assert_eq!(imported.source.session_id, "k-1");
        assert_eq!(imported.workspace, Some(PathBuf::from("/home/dev/webshop")));
        assert_eq!(imported.title, "Health check endpoint");
        let roles: Vec<&str> = imported.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "tool", "assistant"]);
        assert_eq!(imported.messages[0].content, "Add a health check");
        let reply = &imported.messages[1];
        assert_eq!(reply.content, "Let me look.");
        assert_eq!(reply.reasoning, "Routes first.");
        assert_eq!(reply.tool_calls[1].arguments, r#"{"command":"pytest"}"#);
        assert_eq!(imported.messages[2].name, "fs_read");
        assert_eq!(imported.messages[2].content, "app = Flask()");
        assert!(imported.messages[3].is_error);
        assert!(imported.messages[3].content.contains("no tests"));

        assert_eq!(head(&path).1, "Health check endpoint");

        // Cleared and left: what came before the clear is kept.
        let left = dir.join("k-2.jsonl");
        let cleared = r#"{"version":"v1","kind":"Clear","data":{}}"#;
        std::fs::write(&left, format!("{RECORDS}{cleared}\n")).unwrap();
        assert_eq!(import(&left).unwrap().messages.len(), 5);
    }

    #[test]
    fn a_helpers_session_is_not_listed() {
        let root = std::env::temp_dir().join("letronna-import-kiro-list");
        let _ = std::fs::remove_dir_all(&root);
        let dir = root.join("sessions/cli");
        std::fs::create_dir_all(&dir).unwrap();
        for id in ["mine", "helper"] {
            std::fs::write(dir.join(format!("{id}.jsonl")), RECORDS).unwrap();
            std::fs::write(dir.join(format!("{id}.lock")), "").unwrap();
        }
        std::fs::write(
            dir.join("helper.json"),
            r#"{"session_created_reason":"subagent"}"#,
        )
        .unwrap();
        std::fs::write(dir.join("empty.jsonl"), "").unwrap();
        assert_eq!(files(&root), [dir.join("mine.jsonl")]);
    }
}
