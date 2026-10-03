//! The input files a run's request names, and which of them it never read.
//!
//! A task handed over with its files under `in/`, as benchmark tasks and
//! batch jobs lay them out beside the `out/` they write to, is judged on
//! having used each of them. A run that read two of three and wrote its
//! answer anyway had nobody to notice: in the window the user sees what was
//! read, but a run nobody watches ends on its own. So such a run is told
//! once, as it would end, which of the files its request named it never
//! touched.
//!
//! Only an exact path under `in/` counts, and only to a text file there as
//! the run starts: a folder, a wildcard or a picture is the run's to look
//! into as it sees fit. A file counts as read once any call names it, by its
//! name or by a wildcard that fits it, since `cat in/*`, a script written and
//! then run, or a helper sent to look read it without `read_file`, and being
//! told to read what it had read would cost the run a round for nothing.

use crate::chat::Message;
use std::path::Path;

/// What a run is told, before the files, when it would end with some of
/// its inputs never read.
const UNREAD: &str = "The task names input files you have not read. Read them, or say why they do not matter, then end with your whole answer again, since only your last message is passed on.";

/// Characters that stand around a path in prose, markdown or code, never in it.
const AROUND: [char; 14] = [
    '`', '\'', '"', '<', '>', '(', ')', '[', ']', '{', '}', ',', ';', '|',
];

/// The text files under `workspace`'s `in/` that `request` names by their
/// exact path, each once, relative to `workspace` with `/` between folders.
pub(crate) fn named(request: &str, workspace: &Path) -> Vec<String> {
    let Ok(root) = std::fs::canonicalize(workspace) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for word in request.split(|c: char| c.is_whitespace() || AROUND.contains(&c)) {
        // `@` put the file into the message already.
        if word.starts_with('@') {
            continue;
        }
        // Markdown's bold and a sentence's end are no part of the path.
        let word = word
            .trim_start_matches('*')
            .trim_end_matches(['*', '.', ':', '!', '?']);
        // Which files a wildcard means is the run's to find out.
        if word.contains(['*', '?']) {
            continue;
        }
        if let Some(path) = input(word, workspace, &root)
            && !found.contains(&path)
        {
            found.push(path);
        }
    }
    found
}

/// Where `word` lies inside `root`, the canonical `workspace`, when it names
/// a text file under `in/`.
fn input(word: &str, workspace: &Path, root: &Path) -> Option<String> {
    // Most words are no path into `in/`, and need no look at the disk.
    if !word.contains("in/") && !word.contains("in\\") {
        return None;
    }
    // Before canonicalising, which on Windows signs in to a share it names.
    if crate::paths::unsafe_raw(word, Some(workspace)).is_some() {
        return None;
    }
    // An absolute `word`, as a request that spells out the workspace writes
    // it, stands on its own.
    let file = std::fs::canonicalize(root.join(word)).ok()?;
    let inside = file.strip_prefix(root).ok()?;
    if !inside.starts_with("in") || !file.is_file() {
        return None;
    }
    // A picture or a PDF is not read whole as text, so it is not asked after.
    crate::tools::open_text(&file).ok()?;
    let parts: Vec<_> = inside.iter().map(|part| part.to_string_lossy()).collect();
    Some(parts.join("/"))
}

/// Those of `inputs` that no call among `messages` names, by the file's
/// name or by a wildcard that fits it.
pub(crate) fn unread<'a>(inputs: &'a [String], messages: &[Message]) -> Vec<&'a String> {
    let calls: Vec<&str> = messages
        .iter()
        .flat_map(|message| &message.tool_calls)
        .map(|call| call.arguments.as_str())
        .collect();
    inputs
        .iter()
        .filter(|input| {
            let name = input.rsplit('/').next().unwrap_or(input);
            !calls.iter().any(|arguments| names(arguments, name))
        })
        .collect()
}

/// Whether a call's `arguments` name the file `name`, or hold a path whose
/// wildcard fits it, as `cat in/*` and `glob('in/*.csv')` do.
fn names(arguments: &str, name: &str) -> bool {
    if arguments.contains(name) {
        return true;
    }
    arguments
        .split(|c: char| c.is_whitespace() || AROUND.contains(&c) || matches!(c, '\\' | '='))
        // A lone `*`, as in a list item or a product, is no path.
        .filter(|word| word.contains(['*', '?']) && word.contains(['/', '.']))
        .any(|word| {
            let last = word.rsplit('/').next().unwrap_or(word);
            crate::command::fits(last, name)
        })
}

/// What a run that never read `unread` is told, the files last since a
/// fold cuts a long notice off at its end.
pub(crate) fn notice(unread: &[&String]) -> String {
    let files: Vec<String> = unread.iter().map(|path| format!("- `{path}`")).collect();
    format!("{UNREAD}\n{}", files.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ToolCall;
    use crate::store::tests::Scratch;
    use serde_json::json;

    /// A workspace holding `in/brief.md`, `in/data.json`, `in/cases/one.json`,
    /// `in/chart.png` and `notes.txt`, removed when dropped.
    fn workspace(name: &str) -> Scratch {
        let dir = Scratch::new(&format!("inputs-{name}"));
        std::fs::create_dir_all(dir.join("in/cases")).unwrap();
        std::fs::write(dir.join("in/brief.md"), "# Brief\n").unwrap();
        std::fs::write(dir.join("in/data.json"), "{}").unwrap();
        std::fs::write(dir.join("in/cases/one.json"), "{}").unwrap();
        std::fs::write(dir.join("in/chart.png"), b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR").unwrap();
        std::fs::write(dir.join("notes.txt"), "notes").unwrap();
        dir
    }

    /// An assistant message making one call of `name` with `arguments`.
    fn calling(name: &str, arguments: serde_json::Value) -> Message {
        Message {
            role: "assistant".into(),
            tool_calls: vec![ToolCall {
                id: "call-0".into(),
                name: name.into(),
                arguments: arguments.to_string(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn exact_text_files_under_in_are_inputs_however_the_request_writes_them() {
        let dir = workspace("named");
        let request = format!(
            "Read these:\n- {}\n- **`in/data.json`**, then ./in/brief.md again.\n",
            dir.join("in/brief.md").display()
        );
        assert_eq!(named(&request, &dir), ["in/brief.md", "in/data.json"]);
    }

    #[test]
    fn folders_wildcards_pictures_missing_files_and_files_outside_in_are_not_inputs() {
        let dir = workspace("not-named");
        let request = "Read in/cases/, in/cases/*.json, in/chart.png, in/gone.md, \
                       notes.txt, in/../notes.txt and @in/brief.md.";
        assert!(named(request, &dir).is_empty());
    }

    #[test]
    fn an_input_counts_as_read_once_any_call_names_it_or_a_wildcard_fits_it() {
        let inputs = ["in/brief.md".to_owned(), "in/data.json".to_owned()];
        let read = |message: Message| unread(&inputs, &[message]);
        let file = calling("read_file", json!({"path": "in/brief.md"}));
        assert_eq!(read(file), [&inputs[1]]);
        let shell = calling("run_command", json!({"command": "cat in/*.json"}));
        assert_eq!(read(shell), [&inputs[0]]);
        let script = calling(
            "write_file",
            json!({"path": "sum.py", "content": "for f in glob.glob('in/*'):\n    print(f)"}),
        );
        assert!(read(script).is_empty());
    }

    #[test]
    fn a_lone_star_or_a_listing_of_the_folder_reads_nothing() {
        let inputs = ["in/brief.md".to_owned()];
        let messages = [
            calling("list_dir", json!({"path": "in"})),
            calling(
                "run_command",
                json!({"command": "ls in/ && echo $((2 * 3))"}),
            ),
            calling(
                "write_file",
                json!({"path": "out/a.md", "content": "* one\n* two"}),
            ),
        ];
        assert_eq!(unread(&inputs, &messages), [&inputs[0]]);
    }

    #[test]
    fn the_notice_ends_with_the_files_it_names() {
        let unread = ["in/data.json".to_owned()];
        let text = notice(&unread.iter().collect::<Vec<_>>());
        assert!(text.starts_with(UNREAD), "{text}");
        assert!(text.ends_with("\n- `in/data.json`"), "{text}");
    }
}
