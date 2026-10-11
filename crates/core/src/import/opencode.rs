//! opencode's sessions, read through opencode itself: since 1.x it keeps
//! them in a SQLite file, and its own `db` and `export` commands are the
//! way out of it this app needs no SQLite of its own for. A file written by
//! `opencode export` is read the same way.
//!
//! An assistant message there is a run of parts across the steps of one
//! reply: text, thinking, and tool calls that hold their own results. It is
//! split where words follow a call, so each result lands before the words
//! the model wrote after reading it.

use super::{Candidate, Imported, Source, Tool, first_user_title, nothing_to_import, short_title};
use crate::chat::{MAX_TOOL_CHARS, Message, ToolCall};
use crate::tools::keep_ends;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// How long opencode has to list or export sessions before it is stopped:
/// it answers in under half a second here, so this only catches one stuck.
const LIMIT: Duration = Duration::from_secs(15);

/// The newest sessions of every project, each with the first thing typed in
/// it to title a session opencode left as "New session - <date>". The
/// session's own columns alone are the fallback, should the message tables
/// change shape.
const LISTING: &str = "SELECT s.id, s.title, s.directory, s.time_updated, \
     (SELECT json_extract(p.data, '$.text') FROM part p JOIN message m ON m.id = p.message_id \
      WHERE p.session_id = s.id AND json_extract(m.data, '$.role') = 'user' \
      AND json_extract(p.data, '$.type') = 'text' \
      AND coalesce(json_extract(p.data, '$.synthetic'), 0) = 0 \
      ORDER BY p.time_created, p.id LIMIT 1) AS said \
     FROM session s WHERE s.parent_id IS NULL AND s.time_archived IS NULL \
     ORDER BY s.time_updated DESC LIMIT ";
const LISTING_PLAIN: &str = "SELECT id, title, directory, time_updated FROM session \
     WHERE parent_id IS NULL AND time_archived IS NULL ORDER BY time_updated DESC LIMIT ";

/// opencode's sessions, newest first, `limit` at most: none where opencode
/// is not installed or does not answer.
pub(super) fn list(limit: usize) -> Vec<Candidate> {
    let Some(program) = program() else {
        return Vec::new();
    };
    let limit = limit.min(100_000);
    let rows = [LISTING, LISTING_PLAIN].iter().find_map(|query| {
        let query = format!("{query}{limit}");
        let text = run(&program, &["db", &query, "--format", "json"]).ok()?;
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Array(rows)) => Some(rows),
            _ => None,
        }
    });
    let database = database();
    rows.into_iter()
        .flatten()
        .filter_map(|row| {
            let session_id = row["id"].as_str().filter(|id| !id.is_empty())?.to_owned();
            Some(Candidate {
                tool: Tool::Opencode,
                session_id,
                path: database.clone(),
                title: title(
                    row["title"].as_str().unwrap_or_default(),
                    row["said"].as_str().unwrap_or_default(),
                ),
                workspace: row["directory"].as_str().map(PathBuf::from),
                updated: row["time_updated"].as_u64().unwrap_or(0) / 1000,
            })
        })
        .collect()
}

/// The session id, title and project folder of a file `opencode export`
/// wrote.
pub(super) fn head(path: &Path) -> (String, String, Option<PathBuf>) {
    let Some(doc) = read_file(path).ok() else {
        return (String::new(), String::new(), None);
    };
    let info = &doc["info"];
    let said = doc["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["info"]["role"] == "user")
        .map(|m| user_text(m["parts"].as_array().map_or(&[][..], Vec::as_slice)))
        .find(|text| !text.trim().is_empty())
        .unwrap_or_default();
    (
        info["id"].as_str().unwrap_or_default().to_owned(),
        title(info["title"].as_str().unwrap_or_default(), &said),
        info["directory"].as_str().map(PathBuf::from),
    )
}

/// The session `session_id` from the file at `path` when it is an export,
/// or else asked of opencode, for one it listed.
pub(super) fn import(path: &Path, session_id: &str) -> Result<Imported, String> {
    let doc = if is_export(path) {
        read_file(path)?
    } else {
        let program = program().ok_or("opencode is not installed here any more.")?;
        let text = run(&program, &["export", session_id])?;
        parse(&text).ok_or("opencode did not give the session back as JSON.")?
    };
    let info = &doc["info"];
    let mut skipped = 0usize;
    let items = doc["messages"].as_array().map_or(&[][..], Vec::as_slice);
    let messages = messages(items, &mut skipped);
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    let said = first_user_title(&messages);
    Ok(Imported {
        source: Source {
            tool: Tool::Opencode,
            session_id: info["id"].as_str().unwrap_or(session_id).to_owned(),
            path: path.to_owned(),
        },
        title: title(info["title"].as_str().unwrap_or_default(), &said),
        workspace: info["directory"].as_str().map(PathBuf::from),
        updated: info["time"]["updated"].as_u64().unwrap_or(0) / 1000,
        messages,
        skipped,
    })
}

/// Whether `doc`, a whole JSON file, is one `opencode export` wrote.
pub(super) fn is_export_doc(doc: &Value) -> bool {
    doc["info"]["id"]
        .as_str()
        .is_some_and(|id| id.starts_with("ses_"))
        && doc["messages"].is_array()
}

fn is_export(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "json")
}

fn read_file(path: &Path) -> Result<Value, String> {
    let bytes =
        std::fs::read(path).map_err(|e| format!("Could not read {}: {e}", path.display()))?;
    parse(&String::from_utf8_lossy(&bytes))
        .filter(is_export_doc)
        .ok_or_else(|| format!("{} is not a session opencode exported.", path.display()))
}

/// JSON from opencode, from its first brace: a line it may print before
/// the document does not spoil it.
fn parse(text: &str) -> Option<Value> {
    let start = text.find('{')?;
    serde_json::from_str(&text[start..]).ok()
}

/// opencode's own title, unless it is the one opencode gives a session
/// before it has one, and then the first thing typed.
fn title(own: &str, said: &str) -> String {
    let placeholder = own.is_empty() || own.starts_with("New session - ");
    short_title(if placeholder { said } else { own })
}

/// The opencode program: on PATH as the login shell sets it, or where its
/// installer puts it.
fn program() -> Option<PathBuf> {
    crate::files::on_path("opencode").or_else(|| {
        let installed = crate::tools::home()?.join(".opencode/bin/opencode");
        installed.is_file().then_some(installed)
    })
}

/// Where opencode keeps its sessions, as the source a listed one names.
fn database() -> PathBuf {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| crate::tools::home().map(|home| home.join(".local/share")))
        .unwrap_or_default()
        .join("opencode/opencode.db")
}

/// What opencode prints for `args`, or why it gave nothing.
fn run(program: &Path, args: &[&str]) -> Result<String, String> {
    // Into a file, not a pipe: opencode runs on Bun, which writes to a pipe
    // in the background and can exit with a large export half sent, as a
    // live export of 1.1 MB was, while a file is written whole.
    static RUNS: AtomicUsize = AtomicUsize::new(0);
    let out = std::env::temp_dir().join(format!(
        "letronna-opencode-{}-{}.json",
        std::process::id(),
        RUNS.fetch_add(1, Ordering::Relaxed)
    ));
    let printed = run_into(program, args, &out);
    let text = printed.and_then(|()| {
        std::fs::read(&out).map_err(|e| format!("What opencode printed could not be read: {e}"))
    });
    let _ = std::fs::remove_file(&out);
    Ok(String::from_utf8_lossy(&text?).into_owned())
}

fn run_into(program: &Path, args: &[&str], out: &Path) -> Result<(), String> {
    let file = std::fs::File::create(out)
        .map_err(|e| format!("There is nowhere to keep what opencode prints: {e}"))?;
    let mut command = std::process::Command::new(program);
    command
        .args(args)
        .envs(
            crate::login_env::vars()
                .iter()
                .map(|(name, value)| (name, value)),
        )
        .stdin(std::process::Stdio::null())
        .stdout(file)
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("opencode could not be run: {e}"))?;
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > LIMIT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!(
                    "opencode took longer than {}s and was stopped.",
                    LIMIT.as_secs()
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            Err(e) => return Err(format!("opencode did not finish: {e}")),
        }
    };
    if status.success() {
        Ok(())
    } else {
        Err(format!("opencode ended with {status}."))
    }
}

/// The text the user typed in a message's parts, leaving out what opencode
/// added itself, such as a file's contents.
fn user_text(parts: &[Value]) -> String {
    parts
        .iter()
        .filter(|p| p["type"] == "text" && p["synthetic"].as_bool() != Some(true))
        .filter(|p| p["ignored"].as_bool() != Some(true))
        .filter_map(|p| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// One round of an assistant message as it builds up.
#[derive(Default)]
struct Round {
    said: Vec<String>,
    thought: Vec<String>,
    calls: Vec<ToolCall>,
    results: Vec<Message>,
}

impl Round {
    /// The round as messages: the reply, then each result that came back.
    fn flush(&mut self, model: &str, out: &mut Vec<Message>) {
        let round = std::mem::take(self);
        let content = round.said.join("\n");
        if content.trim().is_empty() && round.calls.is_empty() {
            return;
        }
        out.push(Message {
            role: "assistant".into(),
            content,
            reasoning: round.thought.join("\n"),
            model: model.to_owned(),
            tool_calls: round.calls,
            ..Default::default()
        });
        out.extend(round.results);
    }
}

fn messages(items: &[Value], skipped: &mut usize) -> Vec<Message> {
    let mut out = Vec::new();
    for item in items {
        let parts = item["parts"].as_array().map_or(&[][..], Vec::as_slice);
        match item["info"]["role"].as_str().unwrap_or_default() {
            "user" => {
                *skipped += parts.iter().filter(|p| p["type"] == "file").count();
                let text = user_text(parts);
                if !text.trim().is_empty() {
                    out.push(Message {
                        role: "user".into(),
                        content: text,
                        ..Default::default()
                    });
                }
            }
            "assistant" => {
                let model = item["info"]["modelID"].as_str().unwrap_or_default();
                let mut round = Round::default();
                for part in parts {
                    match part["type"].as_str().unwrap_or_default() {
                        kind @ ("text" | "reasoning") => {
                            let Some(text) = part["text"].as_str().filter(|t| !t.is_empty()) else {
                                continue;
                            };
                            if !round.calls.is_empty() {
                                round.flush(model, &mut out);
                            }
                            if kind == "text" {
                                round.said.push(text.to_owned());
                            } else {
                                round.thought.push(text.to_owned());
                            }
                        }
                        "tool" => tool_part(part, &mut round, skipped),
                        "file" => *skipped += 1,
                        // Step marks, snapshots and patches: opencode's own
                        // record of the reply, not anything said in it.
                        _ => {}
                    }
                }
                round.flush(model, &mut out);
            }
            _ => *skipped += 1,
        }
    }
    out
}

/// A tool part: the call, and the result it holds once it has finished.
fn tool_part(part: &Value, round: &mut Round, skipped: &mut usize) {
    let (Some(id), Some(name)) = (
        part["callID"].as_str().filter(|id| !id.is_empty()),
        part["tool"].as_str().filter(|n| !n.is_empty()),
    ) else {
        *skipped += 1;
        return;
    };
    let state = &part["state"];
    round.calls.push(ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: match &state["input"] {
            Value::Null => "{}".to_owned(),
            input => input.to_string(),
        },
    });
    let (text, is_error) = match state["status"].as_str().unwrap_or_default() {
        "completed" => (state["output"].as_str().unwrap_or_default(), false),
        "error" => (state["error"].as_str().unwrap_or_default(), true),
        // Still running when the session was left: no result to give.
        _ => return,
    };
    round.results.push(Message {
        role: "tool".into(),
        content: keep_ends(text.to_owned(), MAX_TOOL_CHARS, ""),
        tool_call_id: id.to_owned(),
        name: name.to_owned(),
        is_error,
        ..Default::default()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = r#"{
  "info": {
    "id": "ses_abc",
    "projectID": "global",
    "directory": "/home/dev/webshop",
    "title": "New session - 2026-09-10T14:02:00.000Z",
    "time": { "created": 1757512920000, "updated": 1757513000000 }
  },
  "messages": [
    {
      "info": { "id": "msg_1", "role": "user", "sessionID": "ses_abc" },
      "parts": [
        { "type": "text", "text": "Add a health check" },
        { "type": "text", "text": "Called the Read tool with app.py", "synthetic": true },
        { "type": "file", "url": "file:///home/dev/webshop/app.py" }
      ]
    },
    {
      "info": { "id": "msg_2", "role": "assistant", "modelID": "gpt-6", "sessionID": "ses_abc" },
      "parts": [
        { "type": "step-start" },
        { "type": "reasoning", "text": "Look at the routes first." },
        { "type": "text", "text": "Let me look." },
        { "type": "tool", "callID": "call_1", "tool": "read", "state": { "status": "completed", "input": { "filePath": "app.py" }, "output": "app = Flask()" } },
        { "type": "tool", "callID": "call_2", "tool": "bash", "state": { "status": "error", "input": { "command": "pytest" }, "error": "no tests ran" } },
        { "type": "step-finish", "reason": "tool-calls" },
        { "type": "step-start" },
        { "type": "text", "text": "Added the endpoint." },
        { "type": "step-finish", "reason": "stop" }
      ]
    }
  ]
}"#;

    #[test]
    fn an_export_splits_a_reply_where_its_words_follow_its_calls() {
        let dir = std::env::temp_dir().join("letronna-import-opencode");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("session.json");
        std::fs::write(&path, EXPORT).unwrap();

        let imported = import(&path, "ignored for a file").unwrap();
        assert_eq!(imported.source.tool, Tool::Opencode);
        assert_eq!(imported.source.session_id, "ses_abc");
        assert_eq!(imported.workspace, Some(PathBuf::from("/home/dev/webshop")));
        // opencode's placeholder gives way to what was typed.
        assert_eq!(imported.title, "Add a health check");
        assert_eq!(imported.updated, 1_757_513_000);
        // The attached file is the one part with no place here.
        assert_eq!(imported.skipped, 1);

        let roles: Vec<&str> = imported.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "tool", "assistant"]);
        assert_eq!(imported.messages[0].content, "Add a health check");
        let reply = &imported.messages[1];
        assert_eq!(reply.content, "Let me look.");
        assert_eq!(reply.reasoning, "Look at the routes first.");
        assert_eq!(reply.model, "gpt-6");
        assert_eq!(reply.tool_calls.len(), 2);
        assert_eq!(reply.tool_calls[0].arguments, r#"{"filePath":"app.py"}"#);
        assert_eq!(imported.messages[2].content, "app = Flask()");
        assert_eq!(imported.messages[3].content, "no tests ran");
        assert!(imported.messages[3].is_error);
        assert_eq!(imported.messages[4].content, "Added the endpoint.");

        let (id, title, folder) = head(&path);
        assert_eq!(id, "ses_abc");
        assert_eq!(title, "Add a health check");
        assert_eq!(folder, Some(PathBuf::from("/home/dev/webshop")));
    }

    #[test]
    fn a_title_of_its_own_is_kept() {
        assert_eq!(title("Health check", "add one"), "Health check");
        assert_eq!(
            title("New session - 2026-09-10T14:02", "add one"),
            "add one"
        );
        assert_eq!(title("", "add one"), "add one");
    }

    #[test]
    fn a_line_before_the_json_does_not_spoil_it() {
        let doc =
            parse("Exporting session: ses_abc\n{\"info\":{\"id\":\"ses_abc\"},\"messages\":[]}")
                .unwrap();
        assert!(is_export_doc(&doc));
        assert!(!is_export_doc(
            &serde_json::json!({ "info": { "id": "x" }, "messages": [] })
        ));
    }

    /// Lists and imports through the opencode installed here, reading the
    /// owner's sessions and changing nothing.
    #[test]
    #[ignore = "needs opencode installed with sessions; run with --ignored"]
    fn a_live_opencode_lists_and_exports_its_sessions() {
        let listed = list(5);
        assert!(!listed.is_empty(), "opencode listed nothing");
        assert!(listed.windows(2).all(|w| w[0].updated >= w[1].updated));
        let imported = import(&listed[0].path, &listed[0].session_id).unwrap();
        assert_eq!(imported.source.session_id, listed[0].session_id);
        assert!(!imported.messages.is_empty());
    }
}
