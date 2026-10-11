//! Kimi CLI's sessions: `~/.kimi/sessions/<folder>/<id>/context.jsonl`,
//! the messages as its model is sent them, a line each, with Kimi CLI's own
//! records of usage and checkpoints between them.
//!
//! The project's folder is named by the MD5 of its path, which `kimi.json`
//! lists beside every folder Kimi CLI has worked in, so the path is found
//! by hashing each listed one until a name matches.

use super::{
    Imported, Source, Tool, chat_messages, first_user_title, head_text, modified_secs,
    nothing_to_import, short_title, subdirs,
};
use md5::{Digest as _, Md5};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Kimi CLI's folder: `KIMI_SHARE_DIR`, as Kimi CLI takes it, or `~/.kimi`.
pub(super) fn root() -> Option<PathBuf> {
    std::env::var_os("KIMI_SHARE_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::tools::home().map(|home| home.join(".kimi")))
}

/// Every session under `root` with something in it, unread; one archived in
/// Kimi CLI is left out of the list, as Kimi CLI leaves it out of its own.
pub(super) fn files(root: &Path) -> Vec<PathBuf> {
    subdirs(&root.join("sessions"))
        .iter()
        .flat_map(|project| subdirs(project))
        .filter(|session| state(session)["archived"].as_bool() != Some(true))
        .map(|session| session.join("context.jsonl"))
        .filter(|path| std::fs::metadata(path).is_ok_and(|m| m.len() > 0))
        .collect()
}

/// A session's id, title and project folder, from its first bytes.
pub(super) fn head(path: &Path) -> (String, String, Option<PathBuf>) {
    let title = own_title(path).unwrap_or_else(|| {
        head_text(path)
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter(|r| r["role"] == "user" && !checkpoint(r))
            .map(|r| super::chat_text(&r["content"]))
            .find(|text| !text.trim().is_empty())
            .map(|text| short_title(&text))
            .unwrap_or_default()
    });
    (session_id(path), title, workspace(path))
}

pub(super) fn import(path: &Path) -> Result<Imported, String> {
    let mut items = Vec::new();
    let mut skipped = 0usize;
    for line in super::read_lines(path)? {
        match serde_json::from_str::<Value>(&line) {
            Ok(record) if !checkpoint(&record) => items.push(record),
            Ok(_) => {}
            Err(_) => skipped += 1,
        }
    }
    // Kimi CLI's records of the system prompt, usage and checkpoints start
    // their roles with `_`, and are left out without being counted.
    items.retain(|item| !item["role"].as_str().is_some_and(|r| r.starts_with('_')));
    let messages = chat_messages(&items, &mut skipped);
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    Ok(Imported {
        source: Source {
            tool: Tool::Kimi,
            session_id: session_id(path),
            path: path.to_owned(),
        },
        title: own_title(path).unwrap_or_else(|| first_user_title(&messages)),
        workspace: workspace(path),
        updated: modified_secs(path),
        messages,
        skipped,
    })
}

/// The session's id: the name of its folder.
fn session_id(path: &Path) -> String {
    path.parent()
        .and_then(|dir| dir.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The session's state beside its messages, or null.
fn state(session: &Path) -> Value {
    std::fs::read_to_string(session.join("state.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

/// The title the user gave the session in Kimi CLI, if any.
fn own_title(path: &Path) -> Option<String> {
    let state = state(path.parent()?);
    let title = state["custom_title"].as_str()?.trim();
    (!title.is_empty()).then(|| short_title(title))
}

/// Whether `record` is the marker Kimi CLI sends its model as the user's
/// turn at each checkpoint, which nobody typed.
fn checkpoint(record: &Value) -> bool {
    record["role"] == "user"
        && super::chat_text(&record["content"])
            .trim_start()
            .starts_with("<system>CHECKPOINT")
}

/// The project folder of the session at `path`, found among those
/// `kimi.json` lists by the MD5 of its path, the name Kimi CLI gave its
/// folder of sessions; one on another machine's file system has the
/// system's name in front. `kimi.json` is the one beside the `sessions`
/// the file is in, wherever Kimi CLI was told to keep them.
fn workspace(path: &Path) -> Option<PathBuf> {
    let project = path.parent()?.parent()?;
    let folder = project.file_name()?.to_str()?.to_owned();
    let root = project.parent()?.parent()?;
    let listed = std::fs::read_to_string(root.join("kimi.json")).ok()?;
    let listed: Value = serde_json::from_str(&listed).ok()?;
    listed["work_dirs"].as_array()?.iter().find_map(|dir| {
        let path = dir["path"].as_str()?;
        let hash: String = Md5::digest(path.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let name = match dir["kaos"].as_str().unwrap_or("local") {
            "local" => hash,
            kaos => format!("{kaos}_{hash}"),
        };
        (name == folder).then(|| PathBuf::from(path))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTEXT: &str = concat!(
        r#"{"role":"_system_prompt","content":"You are Kimi."}"#,
        "\n",
        r#"{"role":"_checkpoint","id":0}"#,
        "\n",
        r#"{"role":"user","content":[{"type":"text","text":"<system>CHECKPOINT 0</system>"}]}"#,
        "\n",
        r#"{"role":"user","content":"Add a health check"}"#,
        "\n",
        r#"{"role":"_usage","token_count":1200}"#,
        "\n",
        r#"{"role":"assistant","content":[{"type":"think","think":"Routes first.","encrypted":null},{"type":"text","text":"Let me look."}],"tool_calls":[{"type":"function","id":"read:0","function":{"name":"ReadFile","arguments":"{\"path\":\"app.py\"}"}}]}"#,
        "\n",
        r#"{"role":"tool","content":[{"type":"text","text":"app = Flask()"}],"tool_call_id":"read:0"}"#,
        "\n",
        r#"{"role":"assistant","content":"Added the endpoint."}"#,
        "\n",
    );

    #[test]
    fn a_session_leaves_out_kimis_own_records_and_finds_its_folder() {
        let root = std::env::temp_dir().join("letronna-import-kimi");
        let _ = std::fs::remove_dir_all(&root);
        let project = "/home/dev/webshop";
        let hash: String = Md5::digest(project.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let session = root.join("sessions").join(&hash).join("k-1");
        std::fs::create_dir_all(&session).unwrap();
        let path = session.join("context.jsonl");
        std::fs::write(&path, CONTEXT).unwrap();
        std::fs::write(
            root.join("kimi.json"),
            format!(r#"{{"work_dirs":[{{"path":"/elsewhere","kaos":"local"}},{{"path":"{project}","kaos":"local","last_session_id":null}}]}}"#),
        )
        .unwrap();

        let imported = import(&path).unwrap();
        assert_eq!(imported.source.tool, Tool::Kimi);
        assert_eq!(imported.source.session_id, "k-1");
        assert_eq!(imported.workspace, Some(PathBuf::from(project)));
        assert_eq!(imported.title, "Add a health check");
        assert_eq!(imported.skipped, 0);
        let roles: Vec<&str> = imported.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(roles, ["user", "assistant", "tool", "assistant"]);
        assert_eq!(imported.messages[1].reasoning, "Routes first.");
        assert_eq!(imported.messages[1].content, "Let me look.");
        assert_eq!(imported.messages[1].tool_calls[0].name, "ReadFile");
        assert_eq!(imported.messages[2].name, "ReadFile");
        assert_eq!(imported.messages[2].content, "app = Flask()");

        // A title given in Kimi CLI wins, and an archived session is not
        // listed, nor one with nothing in it.
        std::fs::write(
            session.join("state.json"),
            r#"{"custom_title":"Health check"}"#,
        )
        .unwrap();
        assert_eq!(head(&path).1, "Health check");
        let empty = root.join("sessions").join(&hash).join("k-2");
        std::fs::create_dir_all(&empty).unwrap();
        std::fs::write(empty.join("context.jsonl"), "").unwrap();
        assert_eq!(files(&root), std::slice::from_ref(&path));
        std::fs::write(session.join("state.json"), r#"{"archived":true}"#).unwrap();
        assert!(files(&root).is_empty());
    }
}
