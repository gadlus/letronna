//! Gemini CLI's sessions, under `~/.gemini/tmp/<project>/chats/`: a
//! `session-*.jsonl` log replayed record by record, or a `session-*.json`
//! from before the log, the whole conversation in one object.
//!
//! The log is more than messages appended. A message comes again under
//! its id with more in it, a `$patch` fills in a call's result or drops
//! and reorders messages, `$set` changes the session's details, and
//! `$rewindTo` takes the conversation back. Each is replayed the way Gemini
//! CLI loads its own file, so the import holds what Gemini CLI would show.

use super::{
    Imported, Source, Tool, first_user_title, head_text, modified_secs, nothing_to_import,
    short_title, subdirs,
};
use crate::chat::{MAX_TOOL_CHARS, Message, ToolCall};
use crate::tools::keep_ends;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// The largest `.json` session read whole just to title it in the list.
const LEGACY_HEAD_CAP: u64 = 4 * 1024 * 1024;

/// Gemini CLI's folder: `GEMINI_CLI_HOME`'s `.gemini`, as Gemini CLI takes
/// it, or the home folder's.
pub(super) fn root() -> Option<PathBuf> {
    std::env::var_os("GEMINI_CLI_HOME")
        .map(PathBuf::from)
        .or_else(crate::tools::home)
        .map(|home| home.join(".gemini"))
}

/// Every Gemini CLI session under `root`, unread. A helper's session is in
/// a folder named for the session that started it, and is left out.
pub(super) fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for project in subdirs(&root.join("tmp")) {
        let Ok(files) = std::fs::read_dir(project.join("chats")) else {
            continue;
        };
        for file in files.flatten() {
            let path = file.path();
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            if name.starts_with("session-") && (name.ends_with(".jsonl") || name.ends_with(".json"))
            {
                out.push(path);
            }
        }
    }
    out
}

/// The session id, title and project folder of a session, from its first
/// bytes, or from all of an old `.json` one, which has no first lines.
pub(super) fn head(path: &Path) -> (String, String, Option<PathBuf>) {
    let text =
        if is_legacy(path) && std::fs::metadata(path).is_ok_and(|m| m.len() <= LEGACY_HEAD_CAP) {
            std::fs::read_to_string(path).unwrap_or_default()
        } else {
            head_text(path)
        };
    let session = replay(&text);
    let title = session
        .meta
        .get("summary")
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
        .map(short_title)
        .or_else(|| {
            session
                .messages()
                .filter(|m| m["type"] == "user")
                .map(|m| content_text(&m["content"]).0)
                .find(|text| !ignored(text))
                .map(|text| short_title(&text))
        })
        .unwrap_or_default();
    (session_id(&session.meta, path), title, project_root(path))
}

pub(super) fn import(path: &Path) -> Result<Imported, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    let session = replay(&String::from_utf8_lossy(&bytes));
    let records: Vec<&Value> = session.messages().collect();
    let mut skipped = 0usize;
    let messages = messages(&records, &mut skipped);
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    let title = match session.meta.get("summary").and_then(Value::as_str) {
        Some(summary) if !summary.trim().is_empty() => short_title(summary),
        _ => first_user_title(&messages),
    };
    Ok(Imported {
        source: Source {
            tool: Tool::GeminiCli,
            session_id: session_id(&session.meta, path),
            path: path.to_owned(),
        },
        title,
        workspace: project_root(path),
        updated: modified_secs(path),
        messages,
        skipped,
    })
}

fn is_legacy(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json")
}

fn session_id(meta: &Map<String, Value>, path: &Path) -> String {
    meta.get("sessionId")
        .and_then(Value::as_str)
        .or_else(|| path.file_stem().and_then(|s| s.to_str()))
        .unwrap_or_default()
        .to_owned()
}

/// The project a session belongs to, from the file Gemini CLI keeps in the
/// project's folder to say whose it is. Folders from before it, named by a
/// hash of the path, have none, and their sessions take the folder they
/// are opened in.
fn project_root(path: &Path) -> Option<PathBuf> {
    let marker = path.parent()?.parent()?.join(".project_root");
    let root = std::fs::read_to_string(marker).ok()?;
    let root = root.trim();
    (!root.is_empty()).then(|| PathBuf::from(root))
}

/// A session as its records leave it: the details, and the messages in the
/// order they stand, a removed one left as a gap.
#[derive(Default)]
struct Replayed {
    meta: Map<String, Value>,
    order: Vec<Option<(String, Value)>>,
    at: HashMap<String, usize>,
}

impl Replayed {
    fn messages(&self) -> impl Iterator<Item = &Value> {
        self.order.iter().flatten().map(|(_, message)| message)
    }

    /// Put `message` in under its id: in its place if the id is there
    /// already, as a JavaScript Map keeps it, else at the end.
    fn set(&mut self, message: Value) {
        let Some(id) = message["id"].as_str().map(str::to_owned) else {
            return;
        };
        match self.at.get(&id) {
            Some(&at) => self.order[at] = Some((id, message)),
            None => {
                self.at.insert(id.clone(), self.order.len());
                self.order.push(Some((id, message)));
            }
        }
    }

    fn remove(&mut self, id: &str) {
        if let Some(at) = self.at.remove(id) {
            self.order[at] = None;
        }
    }

    fn clear(&mut self) {
        self.order.clear();
        self.at.clear();
    }

    /// Take the conversation back to before the message `id`, or to nothing
    /// when there is no such message.
    fn rewind(&mut self, id: &str) {
        let Some(&from) = self.at.get(id) else {
            self.clear();
            return;
        };
        for (id, _) in self.order.drain(from..).flatten() {
            self.at.remove(&id);
        }
    }

    /// Put the messages named in `ids` last, in that order, after the rest.
    fn reorder(&mut self, ids: &[Value]) {
        let named: Vec<&str> = ids
            .iter()
            .filter_map(Value::as_str)
            .filter(|id| self.at.contains_key(*id))
            .collect();
        let chosen: HashSet<&str> = named.iter().copied().collect();
        let mut kept: Vec<(String, Value)> = self.order.drain(..).flatten().collect();
        let mut moved: HashMap<String, Value> = HashMap::new();
        kept.retain(|(id, message)| {
            if chosen.contains(id.as_str()) {
                moved.insert(id.clone(), message.clone());
                false
            } else {
                true
            }
        });
        for id in named {
            if let Some(message) = moved.remove(id) {
                kept.push((id.to_owned(), message));
            }
        }
        self.at = kept
            .iter()
            .enumerate()
            .map(|(at, (id, _))| (id.clone(), at))
            .collect();
        self.order = kept.into_iter().map(Some).collect();
    }

    fn patch(&mut self, patch: &Value) {
        let Some(id) = patch["id"].as_str() else {
            return;
        };
        let Some(Some((_, message))) = self.at.get(id).map(|&at| &mut self.order[at]) else {
            return;
        };
        if !patch["content"].is_null() {
            message["content"] = patch["content"].clone();
        }
        let (Some(results), Some(calls)) = (
            patch["toolCalls"].as_array(),
            message["toolCalls"].as_array_mut(),
        ) else {
            return;
        };
        for result in results {
            if let Some(call) = calls.iter_mut().find(|c| c["id"] == result["id"])
                && result.get("result").is_some()
            {
                call["result"] = result["result"].clone();
            }
        }
    }
}

/// `text` replayed as Gemini CLI loads it: a log of records, or failing
/// that, one object holding the whole conversation.
fn replay(text: &str) -> Replayed {
    let mut session = Replayed::default();
    for line in text.lines() {
        let Ok(Value::Object(record)) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if let Some(id) = record.get("$rewindTo").and_then(Value::as_str) {
            session.rewind(id);
        } else if let Some(patch) = record.get("$patch").filter(|p| p.is_object()) {
            session.patch(patch);
            for update in patch["updates"].as_array().into_iter().flatten() {
                session.patch(update);
            }
            for id in patch["removeIds"].as_array().into_iter().flatten() {
                if let Some(id) = id.as_str() {
                    session.remove(id);
                }
            }
            if let Some(ids) = patch["orderIds"].as_array() {
                session.reorder(ids);
            }
        } else if record.get("id").is_some_and(Value::is_string) {
            session.set(Value::Object(record));
        } else if let Some(Value::Object(set)) = record.get("$set") {
            take_details(&mut session, set);
        } else if record.get("sessionId").is_some_and(Value::is_string)
            && record.get("projectHash").is_some_and(Value::is_string)
        {
            take_details(&mut session, &record);
        }
    }
    if session.meta.contains_key("sessionId") {
        return session;
    }
    // A session from before the log: the whole file is one object.
    match serde_json::from_str::<Value>(text) {
        Ok(Value::Object(whole)) if whole.contains_key("sessionId") => {
            let mut session = Replayed::default();
            take_details(&mut session, &whole);
            session
        }
        _ => session,
    }
}

/// The session's details from `record`, and the messages it holds in
/// place of all before them, as a full copy of the conversation does.
fn take_details(session: &mut Replayed, record: &Map<String, Value>) {
    for (key, value) in record {
        if key == "messages" {
            session.clear();
            for message in value.as_array().into_iter().flatten() {
                session.set(message.clone());
            }
        } else {
            session.meta.insert(key.clone(), value.clone());
        }
    }
}

/// Gemini CLI's messages as this app's: each model turn with its text,
/// thinking and calls, and each call's result after it, from the user turn
/// that carried it or, in older files, from the call's own record.
fn messages(records: &[&Value], skipped: &mut usize) -> Vec<Message> {
    let mut out = Vec::new();
    let mut answered: HashSet<String> = HashSet::new();
    let mut names: HashMap<String, String> = HashMap::new();
    for (at, record) in records.iter().enumerate() {
        match record["type"].as_str().unwrap_or_default() {
            "user" => {
                for part in parts(&record["content"]) {
                    let response = &part["functionResponse"];
                    if response.is_object() {
                        let id = response["id"].as_str().unwrap_or_default().to_owned();
                        if !id.is_empty() && answered.insert(id.clone()) {
                            let name = response["name"]
                                .as_str()
                                .map(str::to_owned)
                                .or_else(|| names.get(&id).cloned())
                                .unwrap_or_default();
                            let (text, is_error) = response_text(&response["response"]);
                            out.push(result(id, name, text, is_error));
                        }
                    } else if part.get("inlineData").is_some() || part.get("fileData").is_some() {
                        *skipped += 1;
                    }
                }
                let (text, _) = content_text(&record["content"]);
                if !ignored(&text) {
                    out.push(Message {
                        role: "user".into(),
                        content: text,
                        ..Default::default()
                    });
                }
            }
            "gemini" => {
                let (content, mut reasoning) = content_text(&record["content"]);
                if reasoning.is_empty() {
                    reasoning = record["thoughts"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|t| {
                            let subject = t["subject"].as_str().unwrap_or_default();
                            let description = t["description"].as_str().unwrap_or_default();
                            match subject {
                                "" => description.to_owned(),
                                _ => format!("{subject}: {description}"),
                            }
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                }
                let mut tool_calls: Vec<ToolCall> = parts(&record["content"])
                    .iter()
                    .filter_map(|part| call(&part["functionCall"]))
                    .collect();
                if tool_calls.is_empty() {
                    tool_calls = record["toolCalls"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(call)
                        .collect();
                }
                if content.trim().is_empty() && tool_calls.is_empty() {
                    continue;
                }
                for call in &tool_calls {
                    names.insert(call.id.clone(), call.name.clone());
                }
                let ids: Vec<String> = tool_calls.iter().map(|c| c.id.clone()).collect();
                out.push(Message {
                    role: "assistant".into(),
                    content,
                    reasoning,
                    model: record["model"].as_str().unwrap_or_default().to_owned(),
                    tool_calls,
                    ..Default::default()
                });
                // An older file keeps each result on its call, a newer one in
                // the user turn that follows: the call's own is used only
                // where no turn before the next model turn carries one.
                let carried = carried_ids(&records[at + 1..]);
                for call in record["toolCalls"].as_array().into_iter().flatten() {
                    let Some(id) = call["id"].as_str() else {
                        continue;
                    };
                    if !ids.iter().any(|i| i == id) || carried.contains(id) {
                        continue;
                    }
                    let Some(text) = call_result(&call["result"]) else {
                        continue;
                    };
                    if answered.insert(id.to_owned()) {
                        let status = call["status"].as_str().unwrap_or_default();
                        let name = call["name"].as_str().unwrap_or_default().to_owned();
                        out.push(result(
                            id.to_owned(),
                            name,
                            text,
                            matches!(status, "error" | "cancelled"),
                        ));
                    }
                }
            }
            // Gemini CLI's own notes on the screen: info, warnings, errors.
            _ => {}
        }
    }
    out
}

/// The ids of the results carried by the user turns in `after`, up to the
/// next model turn.
fn carried_ids(after: &[&Value]) -> HashSet<String> {
    after
        .iter()
        .take_while(|r| r["type"] != "gemini")
        .filter(|r| r["type"] == "user")
        .flat_map(|r| parts(&r["content"]))
        .filter_map(|part| part["functionResponse"]["id"].as_str().map(str::to_owned))
        .collect()
}

fn result(id: String, name: String, text: String, is_error: bool) -> Message {
    Message {
        role: "tool".into(),
        content: keep_ends(text, MAX_TOOL_CHARS, ""),
        tool_call_id: id,
        name,
        is_error,
        ..Default::default()
    }
}

/// A `functionCall` part or a call's record: an id, a name and arguments.
fn call(value: &Value) -> Option<ToolCall> {
    let id = value["id"].as_str().filter(|id| !id.is_empty())?;
    let name = value["name"].as_str().filter(|n| !n.is_empty())?;
    let arguments = match &value["args"] {
        Value::Null => "{}".to_owned(),
        args => args.to_string(),
    };
    Some(ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments,
    })
}

/// A call record's `result`: text, or the `functionResponse` parts it holds.
fn call_result(result: &Value) -> Option<String> {
    match result {
        Value::Null => None,
        Value::String(text) => Some(text.clone()),
        other => {
            let texts: Vec<String> = parts(other)
                .iter()
                .map(|part| match part.get("functionResponse") {
                    Some(response) => response_text(&response["response"]).0,
                    None => part["text"].as_str().unwrap_or_default().to_owned(),
                })
                .filter(|t| !t.is_empty())
                .collect();
            Some(texts.join("\n"))
        }
    }
}

/// What a `functionResponse` said, and whether it was an error: Gemini CLI
/// writes `output`, or `error` when the call failed.
fn response_text(response: &Value) -> (String, bool) {
    if let Some(output) = response["output"].as_str() {
        return (output.to_owned(), false);
    }
    match &response["error"] {
        Value::Null => {}
        Value::String(error) => return (error.clone(), true),
        error => return (error.to_string(), true),
    }
    match response {
        Value::Null => (String::new(), false),
        Value::String(text) => (text.clone(), false),
        other => (other.to_string(), false),
    }
}

/// `content` as a list of parts: Gemini's PartListUnion is a string, a part,
/// or a list of either.
fn parts(content: &Value) -> Vec<Value> {
    match content {
        Value::Null => Vec::new(),
        Value::String(text) => vec![serde_json::json!({ "text": text })],
        Value::Array(items) => items
            .iter()
            .map(|item| match item {
                Value::String(text) => serde_json::json!({ "text": text }),
                part => part.clone(),
            })
            .collect(),
        part => vec![part.clone()],
    }
}

/// The text said in `content`, and apart from it the thinking, which Gemini
/// marks as `thought` parts.
fn content_text(content: &Value) -> (String, String) {
    let mut said = Vec::new();
    let mut thought = Vec::new();
    for part in parts(content) {
        let Some(text) = part["text"].as_str() else {
            continue;
        };
        if part["thought"].as_bool() == Some(true) {
            thought.push(text.to_owned());
        } else {
            said.push(text.to_owned());
        }
    }
    (said.join(""), thought.join("\n"))
}

/// Whether a user turn is one Gemini CLI itself leaves out of the model's
/// history: empty, a slash command, a `?` for help, or context it added.
fn ignored(text: &str) -> bool {
    let text = text.trim();
    text.is_empty()
        || text.starts_with('/')
        || text.starts_with('?')
        || text.starts_with("<session_context>")
        || text.starts_with("<hook_context>")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-import-gemini-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A project folder as Gemini CLI makes it, with the marker naming the
    /// project, and a session file in its `chats`.
    fn session_file(dir: &Path, name: &str, text: &str) -> PathBuf {
        let chats = dir.join("tmp/webshop/chats");
        std::fs::create_dir_all(&chats).unwrap();
        std::fs::write(dir.join("tmp/webshop/.project_root"), "/home/dev/webshop\n").unwrap();
        let path = chats.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    const LOG: &str = concat!(
        r#"{"sessionId":"g-1","projectHash":"abc","startTime":"2026-10-01T00:00:00Z","lastUpdated":"2026-10-01T00:00:00Z"}"#,
        "\n",
        r#"{"id":"m1","timestamp":"t","type":"user","content":[{"text":"Add a health check"}]}"#,
        "\n",
        r#"{"id":"m0","timestamp":"t","type":"user","content":"/model"}"#,
        "\n",
        r#"{"id":"m2","timestamp":"t","type":"gemini","content":[{"text":"Checking the routes.","thought":true},{"text":"Let me look."},{"functionCall":{"id":"c1","name":"read_file","args":{"path":"app.py"}}}],"model":"gemini-3-pro"}"#,
        "\n",
        r#"{"id":"m3","timestamp":"t","type":"user","content":[{"functionResponse":{"id":"c1","name":"read_file","response":{"output":"app = Flask()"}}}]}"#,
        "\n",
        r#"{"id":"m4","timestamp":"t","type":"info","content":"Saved."}"#,
        "\n",
        r#"{"id":"m5","timestamp":"t","type":"gemini","content":"First draft."}"#,
        "\n",
        r#"{"$patch":{"id":"m5","content":"Added the endpoint."}}"#,
        "\n",
        r#"{"id":"m6","timestamp":"t","type":"user","content":"Now remove it"}"#,
        "\n",
        r#"{"$rewindTo":"m6"}"#,
        "\n",
        r#"{"$set":{"summary":"Health check endpoint for the webshop"}}"#,
        "\n",
    );

    #[test]
    fn a_log_is_replayed_with_its_patches_and_rewinds() {
        let dir = scratch("log");
        let path = session_file(&dir, "session-2026-10-01T00-00-g1.jsonl", LOG);

        let imported = import(&path).unwrap();
        assert_eq!(imported.source.tool, Tool::GeminiCli);
        assert_eq!(imported.source.session_id, "g-1");
        assert_eq!(imported.workspace, Some(PathBuf::from("/home/dev/webshop")));
        assert_eq!(imported.title, "Health check endpoint for the webshop");
        assert_eq!(imported.skipped, 0);

        let roles: Vec<&str> = imported.messages.iter().map(|m| m.role.as_str()).collect();
        // The slash command and the info note are left out as Gemini CLI
        // leaves them out of its own history, and the rewind took back the
        // last request.
        assert_eq!(roles, ["user", "assistant", "tool", "assistant"]);
        let reply = &imported.messages[1];
        assert_eq!(reply.content, "Let me look.");
        assert_eq!(reply.reasoning, "Checking the routes.");
        assert_eq!(reply.model, "gemini-3-pro");
        assert_eq!(reply.tool_calls[0].name, "read_file");
        assert_eq!(reply.tool_calls[0].arguments, r#"{"path":"app.py"}"#);
        assert_eq!(imported.messages[2].tool_call_id, "c1");
        assert_eq!(imported.messages[2].content, "app = Flask()");
        assert_eq!(imported.messages[3].content, "Added the endpoint.");

        let (id, title, folder) = head(&path);
        assert_eq!(id, "g-1");
        // The whole file is in its head here, summary included.
        assert_eq!(title, "Health check endpoint for the webshop");
        assert_eq!(folder, Some(PathBuf::from("/home/dev/webshop")));
    }

    #[test]
    fn an_old_json_session_keeps_results_on_its_calls() {
        let dir = scratch("legacy");
        let text = r#"{
  "sessionId": "g-old",
  "projectHash": "abc",
  "startTime": "2026-01-01T00:00:00Z",
  "lastUpdated": "2026-01-01T00:00:00Z",
  "messages": [
    { "id": "a", "timestamp": "t", "type": "user", "content": "List the files" },
    {
      "id": "b",
      "timestamp": "t",
      "type": "gemini",
      "content": "",
      "thoughts": [{ "subject": "Plan", "description": "List them.", "timestamp": "t" }],
      "toolCalls": [
        {
          "id": "ls-1",
          "name": "list_directory",
          "args": { "path": "." },
          "result": [{ "functionResponse": { "id": "ls-1", "name": "list_directory", "response": { "output": "app.py" } } }],
          "status": "success",
          "timestamp": "t"
        },
        {
          "id": "rm-1",
          "name": "run_shell_command",
          "args": { "command": "rm x" },
          "result": [{ "functionResponse": { "id": "rm-1", "name": "run_shell_command", "response": { "error": "denied" } } }],
          "status": "error",
          "timestamp": "t"
        }
      ]
    },
    { "id": "c", "timestamp": "t", "type": "gemini", "content": "There is one file." }
  ]
}"#;
        let path = session_file(&dir, "session-2026-01-01T00-00-old.json", text);
        let imported = import(&path).unwrap();
        assert_eq!(imported.title, "List the files");
        let roles: Vec<&str> = imported.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "tool", "assistant"]);
        assert_eq!(imported.messages[1].reasoning, "Plan: List them.");
        assert_eq!(imported.messages[2].content, "app.py");
        assert!(!imported.messages[2].is_error);
        assert_eq!(imported.messages[3].content, "denied");
        assert!(imported.messages[3].is_error);
        assert_eq!(head(&path).1, "List the files");
    }

    #[test]
    fn a_result_is_taken_once_when_both_the_call_and_the_turn_after_carry_it() {
        let dir = scratch("both");
        let text = concat!(
            r#"{"sessionId":"g-2","projectHash":"abc"}"#,
            "\n",
            r#"{"id":"u","timestamp":"t","type":"user","content":"go"}"#,
            "\n",
            r#"{"id":"g","timestamp":"t","type":"gemini","content":"","toolCalls":[{"id":"c","name":"read_file","args":{},"result":[{"functionResponse":{"id":"c","name":"read_file","response":{"output":"from the call"}}}],"status":"success","timestamp":"t"}]}"#,
            "\n",
            r#"{"id":"r","timestamp":"t","type":"user","content":[{"functionResponse":{"id":"c","name":"read_file","response":{"output":"from the turn"}}}]}"#,
            "\n",
        );
        let path = session_file(&dir, "session-x.jsonl", text);
        let imported = import(&path).unwrap();
        let results: Vec<&str> = imported
            .messages
            .iter()
            .filter(|m| m.role == "tool")
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(results, ["from the turn"]);
    }

    #[test]
    fn removed_and_reordered_messages_stand_as_gemini_cli_shows_them() {
        let mut session = Replayed::default();
        for id in ["a", "b", "c", "d"] {
            session.set(serde_json::json!({ "id": id, "type": "user", "content": id }));
        }
        session.remove("b");
        session.reorder(&[serde_json::json!("a"), serde_json::json!("x")]);
        // Again under the same id: the message changes, its place does not.
        session.set(serde_json::json!({ "id": "c", "type": "user", "content": "c2" }));
        let contents: Vec<&str> = session
            .messages()
            .map(|m| m["content"].as_str().unwrap())
            .collect();
        assert_eq!(contents, ["c2", "d", "a"]);
        session.rewind("nowhere");
        assert_eq!(session.messages().count(), 0);
    }

    #[test]
    fn a_helpers_session_is_not_listed() {
        let dir = scratch("listing");
        session_file(&dir, "session-2026-10-01T00-00-g1.jsonl", LOG);
        let helper = dir.join("tmp/webshop/chats/g-1");
        std::fs::create_dir_all(&helper).unwrap();
        std::fs::write(helper.join("sub-1.jsonl"), LOG).unwrap();
        let found = files(&dir);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("session-2026-10-01T00-00-g1.jsonl"));
    }
}
