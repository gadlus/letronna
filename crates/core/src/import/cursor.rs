//! Cursor's agent transcripts, which it writes for its own agent to look
//! back on: `~/.cursor/projects/<project>/agent-transcripts/<id>/<id>.jsonl`,
//! a line for each message.
//!
//! They keep what was said and every tool called, but no call's id and no
//! result, so each call comes in under an id made here with a result saying
//! Cursor kept none. The project folder's name is its path with everything
//! but letters and digits made a dash, which cannot be undone by reading
//! it; the folder is found by walking down the disk instead, through the
//! folders whose names, made the same way, begin what is left.

use super::{
    Imported, Source, Tool, first_user_title, modified_secs, nothing_to_import, short_title,
    subdirs,
};
use crate::chat::{Message, ToolCall};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// The result given to every call: Cursor's transcript never keeps one.
const NO_RESULT: &str = "Cursor's transcript does not keep what this call returned.";
/// The deepest a project folder is looked for: a guard against a disk with
/// folders that link back up into themselves.
const DEEPEST: usize = 40;

pub(super) fn root() -> Option<PathBuf> {
    crate::tools::home().map(|home| home.join(".cursor"))
}

/// Every transcript under `root`, unread; a helper's transcript, kept in a
/// `subagents` folder beside its parent's, is left out.
pub(super) fn files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for project in subdirs(&root.join("projects")) {
        for dir in subdirs(&project.join("agent-transcripts")) {
            let Some(id) = dir.file_name() else {
                continue;
            };
            let path = dir.join(format!("{}.jsonl", id.to_string_lossy()));
            if path.is_file() {
                out.push(path);
            }
        }
    }
    out
}

/// The lines read to title a transcript: its first message is the user's,
/// though Cursor's context can come before it.
const TITLE_LINES: usize = 50;

/// A transcript's id, title and project folder. The title is read line by
/// line, whole: a first message carrying attached files and selections can
/// run past any fixed head.
pub(super) fn head(path: &Path) -> (String, String, Option<PathBuf>) {
    let title = super::read_lines(path)
        .into_iter()
        .flatten()
        .take(TITLE_LINES)
        .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
        .filter(|record| record["role"] == "user")
        .find_map(|record| typed(&text(&record["message"]["content"])))
        .map(|text| short_title(&text))
        .unwrap_or_default();
    (session_id(path), title, project(path))
}

pub(super) fn import(path: &Path) -> Result<Imported, String> {
    let lines = super::read_lines(path)?;
    let mut messages: Vec<Message> = Vec::new();
    let mut skipped = 0usize;
    let mut calls = 0usize;
    for line in lines {
        let Ok(record) = serde_json::from_str::<Value>(&line) else {
            skipped += 1;
            continue;
        };
        let content = &record["message"]["content"];
        match record["role"].as_str().unwrap_or_default() {
            "user" => {
                if let Some(said) = typed(&text(content)) {
                    messages.push(Message {
                        role: "user".into(),
                        content: said,
                        ..Default::default()
                    });
                }
            }
            "assistant" => {
                let said = text(content);
                let tool_calls: Vec<ToolCall> = content
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|block| block["type"] == "tool_use")
                    .filter_map(|block| {
                        let name = block["name"].as_str().filter(|n| !n.is_empty())?;
                        calls += 1;
                        Some(ToolCall {
                            id: format!("cursor-{calls}"),
                            name: name.to_owned(),
                            arguments: match &block["input"] {
                                Value::Null => "{}".to_owned(),
                                input => input.to_string(),
                            },
                        })
                    })
                    .collect();
                if said.trim().is_empty() && tool_calls.is_empty() {
                    continue;
                }
                // Words with no call, after words with none, are one reply
                // written in two lines.
                if tool_calls.is_empty()
                    && let Some(last) = messages.last_mut()
                    && last.role == "assistant"
                    && last.tool_calls.is_empty()
                {
                    last.content.push('\n');
                    last.content.push_str(&said);
                    continue;
                }
                let results: Vec<Message> = tool_calls
                    .iter()
                    .map(|call| Message {
                        role: "tool".into(),
                        content: NO_RESULT.to_owned(),
                        tool_call_id: call.id.clone(),
                        name: call.name.clone(),
                        ..Default::default()
                    })
                    .collect();
                messages.push(Message {
                    role: "assistant".into(),
                    content: said,
                    tool_calls,
                    ..Default::default()
                });
                messages.extend(results);
            }
            // The mark Cursor leaves where a turn ended.
            _ => {}
        }
    }
    if messages.is_empty() {
        return Err(nothing_to_import(path, skipped));
    }
    Ok(Imported {
        source: Source {
            tool: Tool::Cursor,
            session_id: session_id(path),
            path: path.to_owned(),
        },
        title: first_user_title(&messages),
        workspace: project(path),
        updated: modified_secs(path),
        messages,
        skipped,
    })
}

fn session_id(path: &Path) -> String {
    path.file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or_default()
        .to_owned()
}

/// The text blocks of a message's content, joined.
fn text(content: &Value) -> String {
    content
        .as_array()
        .into_iter()
        .flatten()
        .filter(|block| block["type"] == "text")
        .filter_map(|block| block["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// What the user typed in a Cursor message, which it wraps in
/// `<user_query>` among the files, selections and time it adds; `None` for
/// a message Cursor sent on its own, which is only such context.
fn typed(text: &str) -> Option<String> {
    if let Some(start) = text.find("<user_query>") {
        let query = &text[start + "<user_query>".len()..];
        let query = query
            .find("</user_query>")
            .map_or(query, |end| &query[..end]);
        let query = query.trim();
        return (!query.is_empty()).then(|| query.to_owned());
    }
    let text = text.trim();
    (!text.is_empty() && !text.starts_with('<')).then(|| text.to_owned())
}

/// The project a transcript belongs to: its folder under `projects`, read
/// back into the path it was named from.
fn project(path: &Path) -> Option<PathBuf> {
    let name = path.parent()?.parent()?.parent()?.file_name()?.to_str()?;
    folder_named(name)
}

/// The folder whose path Cursor turned into `name`.
fn folder_named(name: &str) -> Option<PathBuf> {
    if cfg!(windows) {
        // A drive comes first, as `c-Users-...` for `C:\Users\...`.
        let (drive, rest) = name.split_once('-')?;
        if drive.len() != 1 {
            return None;
        }
        return walk(&PathBuf::from(format!("{drive}:\\")), rest, 0);
    }
    walk(Path::new("/"), name, 0)
}

/// The folder under `dir` whose path from it comes out as `rest`.
fn walk(dir: &Path, rest: &str, depth: usize) -> Option<PathBuf> {
    if depth > DEEPEST {
        return None;
    }
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let made = dashed(&entry.file_name().to_string_lossy());
        if made.is_empty() || !rest.starts_with(&made) || !path.is_dir() {
            continue;
        }
        if rest.len() == made.len() {
            return Some(path);
        }
        if let Some(after) = rest[made.len()..].strip_prefix('-')
            && let Some(found) = walk(&path, after, depth + 1)
        {
            return Some(found);
        }
    }
    None
}

/// A folder's name as Cursor writes it into a project's: every character
/// but an ASCII letter or digit made a dash.
fn dashed(name: &str) -> String {
    name.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRANSCRIPT: &str = concat!(
        r#"{"role":"user","message":{"content":[{"type":"text","text":"<git_status>clean</git_status>\n<agent_skills>none</agent_skills>"}]}}"#,
        "\n",
        r#"{"role":"user","message":{"content":[{"type":"text","text":"<timestamp>Monday</timestamp>\n<user_query>\nFix the login bug\n</user_query>"}]}}"#,
        "\n",
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Let me look."},{"type":"tool_use","name":"Read","input":{"path":"src/auth.rs"}}]}}"#,
        "\n",
        r#"{"role":"assistant","message":{"content":[{"type":"tool_use","name":"Shell","input":{"command":"cargo test"}}]}}"#,
        "\n",
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":"Fixed it."}]}}"#,
        "\n",
        r#"{"role":"assistant","message":{"content":[{"type":"text","text":"The tests pass."}]}}"#,
        "\n",
        r#"{"type":"turn_ended","status":"success"}"#,
        "\n",
    );

    // The name is built from a canonical path, which on Windows is `\?\C:\…`
    // and never what Cursor names a project there.
    #[cfg(unix)]
    #[test]
    fn a_transcript_gives_each_call_an_id_and_says_its_result_was_not_kept() {
        let base = std::env::temp_dir().join("letronna-import-cursor");
        let _ = std::fs::remove_dir_all(&base);
        // A project whose name has a dash of its own, beside one it could
        // be taken for.
        let real = base.join("work/web-shop");
        std::fs::create_dir_all(&real).unwrap();
        std::fs::create_dir_all(base.join("work/web")).unwrap();
        let real = std::fs::canonicalize(real).unwrap();
        let name = dashed(&real.to_string_lossy())
            .trim_start_matches('-')
            .to_owned();
        let dir = base
            .join(".cursor/projects")
            .join(&name)
            .join("agent-transcripts/t-1");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t-1.jsonl");
        std::fs::write(&path, TRANSCRIPT).unwrap();

        let imported = import(&path).unwrap();
        assert_eq!(imported.source.tool, Tool::Cursor);
        assert_eq!(imported.source.session_id, "t-1");
        assert_eq!(imported.title, "Fix the login bug");
        assert_eq!(imported.workspace.as_deref(), Some(real.as_path()));
        let roles: Vec<&str> = imported.messages.iter().map(|m| m.role.as_str()).collect();
        assert_eq!(
            roles,
            [
                "user",
                "assistant",
                "tool",
                "assistant",
                "tool",
                "assistant"
            ]
        );
        assert_eq!(imported.messages[0].content, "Fix the login bug");
        assert_eq!(imported.messages[1].tool_calls[0].id, "cursor-1");
        assert_eq!(
            imported.messages[1].tool_calls[0].arguments,
            r#"{"path":"src/auth.rs"}"#
        );
        assert_eq!(imported.messages[2].tool_call_id, "cursor-1");
        assert_eq!(imported.messages[2].content, NO_RESULT);
        assert_eq!(imported.messages[3].tool_calls[0].id, "cursor-2");
        assert_eq!(imported.messages[5].content, "Fixed it.\nThe tests pass.");
        assert_eq!(imported.skipped, 0);

        assert_eq!(
            head(&path),
            ("t-1".to_owned(), "Fix the login bug".to_owned(), Some(real))
        );
        assert_eq!(files(&base.join(".cursor")), [path]);
    }

    #[test]
    fn only_the_query_is_taken_from_what_cursor_wraps_around_it() {
        assert_eq!(
            typed("<attached_files>a.rs</attached_files>\n<user_query>why?</user_query>")
                .as_deref(),
            Some("why?")
        );
        assert_eq!(typed("<agent_transcripts>…</agent_transcripts>"), None);
        assert_eq!(typed("plain words").as_deref(), Some("plain words"));
    }

    #[test]
    fn a_folder_that_is_not_there_is_not_made_up() {
        assert_eq!(folder_named("no-such-folder-anywhere-at-all-xyz"), None);
    }
}
