//! Sessions Hermes exported. Hermes keeps its own in SQLite, which this app
//! does not read; `hermes sessions export <file>` is the way out it offers,
//! a line of JSON for each session with its messages in the shape chat
//! APIs take them. One file can hold every session, so each line is a
//! candidate of its own. A trace export, `--format trace`, is Claude Code's
//! format and imports as one.

use super::{
    Candidate, Imported, Source, Tool, chat_messages, first_user_title, modified_secs,
    nothing_to_import, short_title,
};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Whether `record`, a line of a file, is a session as Hermes exports one.
pub(super) fn is_session(record: &Value) -> bool {
    record["id"].is_string()
        && record["messages"].is_array()
        && (record.get("source").is_some() || record.get("started_at").is_some())
}

/// The sessions in the export at `path`, each a candidate.
pub(super) fn candidates(path: &Path) -> Vec<Candidate> {
    sessions(path)
        .iter()
        .map(|session| {
            let messages = session["messages"]
                .as_array()
                .map_or(&[][..], Vec::as_slice);
            let said = messages
                .iter()
                .filter(|m| m["role"] == "user" && m["_compressed_summary"] != true)
                .map(|m| super::chat_text(&m["content"]))
                .find(|text| !text.trim().is_empty())
                .unwrap_or_default();
            Candidate {
                tool: Tool::Hermes,
                session_id: session["id"].as_str().unwrap_or_default().to_owned(),
                path: path.to_owned(),
                title: title(session).unwrap_or_else(|| short_title(&said)),
                workspace: folder(session),
                updated: updated(session).unwrap_or_else(|| modified_secs(path)),
            }
        })
        .collect()
}

/// The session `session_id` from the export at `path`.
pub(super) fn import(path: &Path, session_id: &str) -> Result<Imported, String> {
    let sessions = sessions(path);
    let session = sessions
        .iter()
        .find(|s| s["id"] == session_id)
        .ok_or_else(|| format!("{} holds no session {session_id}.", path.display()))?;
    let mut items = session["messages"].as_array().cloned().unwrap_or_default();
    for item in &mut items {
        // Hermes names a result's tool in `tool_name`, and marks the
        // summary that stands for turns it compacted, which the model sees
        // as a note rather than as anything the user said.
        if item.get("name").is_none()
            && let Some(name) = item["tool_name"].as_str().map(str::to_owned)
        {
            item["name"] = Value::String(name);
        }
        if item["_compressed_summary"].as_bool() == Some(true) {
            item["notice"] = Value::Bool(true);
        }
    }
    let mut skipped = 0usize;
    let messages = chat_messages(&items, &mut skipped);
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    Ok(Imported {
        source: Source {
            tool: Tool::Hermes,
            session_id: session_id.to_owned(),
            path: path.to_owned(),
        },
        title: title(session).unwrap_or_else(|| first_user_title(&messages)),
        workspace: folder(session),
        updated: updated(session).unwrap_or_else(|| modified_secs(path)),
        messages,
        skipped,
    })
}

/// Every session line of the file, read whole: each line holds all of one
/// session's messages.
fn sessions(path: &Path) -> Vec<Value> {
    super::read_lines(path)
        .into_iter()
        .flatten()
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(is_session)
        .collect()
}

fn title(session: &Value) -> Option<String> {
    let title = session["title"].as_str()?.trim();
    (!title.is_empty()).then(|| short_title(title))
}

fn folder(session: &Value) -> Option<PathBuf> {
    session["cwd"]
        .as_str()
        .filter(|cwd| !cwd.is_empty())
        .map(PathBuf::from)
}

/// When the session last moved, in seconds: Hermes keeps times as floats.
fn updated(session: &Value) -> Option<u64> {
    ["ended_at", "started_at"]
        .iter()
        .find_map(|key| session[*key].as_f64())
        .filter(|at| *at > 0.0)
        .map(|at| at as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXPORT: &str = concat!(
        r#"{"id":"20261001_aaa","source":"cli","model":"hermes-4","title":"Health check","cwd":"/home/dev/webshop","started_at":1759276800.5,"ended_at":1759277000.25,"messages":[{"id":1,"session_id":"20261001_aaa","role":"user","content":"Add a health check","timestamp":1759276801.0},{"id":2,"role":"assistant","content":"Let me look.","reasoning":"Routes first.","tool_calls":[{"id":"call_1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"app.py\"}"}}],"timestamp":1759276802.0},{"id":3,"role":"tool","content":"app = Flask()","tool_call_id":"call_1","tool_name":"read_file","timestamp":1759276803.0},{"id":4,"role":"assistant","content":"Added the endpoint.","timestamp":1759276804.0}],"timings":{}}"#,
        "\n",
        r#"{"id":"20261002_bbb","source":"telegram","started_at":1759363200.0,"messages":[{"id":9,"role":"user","content":"[Summary of earlier turns] We set up the webshop.","_compressed_summary":true},{"id":10,"role":"user","content":"What next?"},{"id":11,"role":"assistant","content":"Tests."}],"timings":{}}"#,
        "\n",
    );

    fn export() -> PathBuf {
        let dir = std::env::temp_dir().join("letronna-import-hermes");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("hermes.jsonl");
        std::fs::write(&path, EXPORT).unwrap();
        path
    }

    #[test]
    fn each_session_in_an_export_is_offered_on_its_own() {
        let path = export();
        let found = candidates(&path);
        let ids: Vec<&str> = found.iter().map(|c| c.session_id.as_str()).collect();
        assert_eq!(ids, ["20261001_aaa", "20261002_bbb"]);
        assert_eq!(found[0].title, "Health check");
        assert_eq!(found[0].workspace, Some(PathBuf::from("/home/dev/webshop")));
        assert_eq!(found[0].updated, 1_759_277_000);
        // Untitled, it takes the first thing said, which a summary is not.
        assert_eq!(found[1].title, "What next?");
    }

    #[test]
    fn a_session_imports_with_its_calls_and_its_summary_as_a_notice() {
        let path = export();
        let imported = import(&path, "20261001_aaa").unwrap();
        assert_eq!(imported.source.tool, Tool::Hermes);
        assert_eq!(imported.title, "Health check");
        let roles: Vec<&str> = imported.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "assistant"]);
        assert_eq!(imported.messages[1].reasoning, "Routes first.");
        assert_eq!(imported.messages[1].tool_calls[0].name, "read_file");
        assert_eq!(imported.messages[2].name, "read_file");

        let later = import(&path, "20261002_bbb").unwrap();
        assert!(later.messages[0].notice);
        assert_eq!(
            later.title, "What next?",
            "the summary is not what was said"
        );

        let Err(missing) = import(&path, "nope") else {
            panic!("a session the file does not hold imported");
        };
        assert!(missing.ends_with("holds no session nope."));
    }
}
