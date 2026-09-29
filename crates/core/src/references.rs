//! `@path` references in a message the user sends.
//!
//! `@src/main.rs`, `@src/main.rs#L10-20` (or `#L10`) and `@docs/` put the
//! file, those lines or the folder's listing into the message itself, so the
//! model starts with them instead of spending a round on `read_file`. Paths go
//! through the same checks and paging as `read_file`: a file of keys, one in
//! Letronna's own folder or one outside the workspace and home folder is not
//! read, and a path that is not there stays as typed. `@diff` attaches the
//! project's uncommitted changes and `@session:12` the end of that session.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use crate::tools::{self, ToolContext};

/// What stands between the words typed and what the references attached, so
/// a front end can show the message as it was typed.
const ATTACHED: &str = "\n\n[Attached with @: what the message names, as it is now.]\n";

/// Room for everything attached to one message. A page of a file is already
/// capped by `read_file`'s paging; this stops a message naming many of them.
const ATTACHED_CHARS: usize = 4 * crate::chat::MAX_TOOL_CHARS;

/// Less room than this left, and no more references are read: a header
/// with a few lines under it would help nobody.
const ROOM_LEFT: usize = 200;

/// The entries of a folder listed at most, which a `node_modules` would pass.
const LISTED: usize = 500;

/// Names after an `@` that mean someone rather than a path.
const MENTIONS: &[&str] = &["crew"];

/// The reference that attaches the project's uncommitted changes. It is kept
/// by name, as `@crew` is, so a file called diff is not attached this way.
pub const DIFF: &str = "diff";

/// What goes before a session's id in a reference to it.
pub const SESSION: &str = "session:";

/// How long git gets to say what changed, as long as a search gets.
const DIFF_TIME: Duration = crate::grep::SEARCH_TIME;

/// `text` with what its `@` references name attached after it, or `text` as
/// it is when none of them names anything there.
pub fn expand(text: &str, workspace: Option<&Path>) -> String {
    let ctx = ToolContext {
        workspace: workspace.map(Path::to_path_buf),
        ..ToolContext::default()
    };
    let mut out = String::new();
    let mut seen: Vec<&str> = Vec::new();
    for written in references(text) {
        if seen.contains(&written) {
            continue;
        }
        seen.push(written);
        let room = ATTACHED_CHARS.saturating_sub(out.chars().count());
        if room < ROOM_LEFT {
            break;
        }
        let Some(section) = attach(written, &ctx) else {
            continue;
        };
        // Cut at a line, so a page never ends half way through one.
        let section = if section.chars().count() <= room {
            section
        } else {
            let head: String = section.chars().take(room).collect();
            let kept = head.rfind('\n').map_or("", |at| &head[..at]);
            format!(
                "{kept}\n[The rest is left out: the message holds all it can. read_file shows it.]"
            )
        };
        out.push_str("\n\n");
        out.push_str(&section);
    }
    if out.is_empty() {
        return text.to_owned();
    }
    format!("{text}{ATTACHED}{}", out.trim_start_matches('\n'))
}

/// What the user typed of a message `expand` made, without what it attached.
pub fn typed(content: &str) -> &str {
    content.split(ATTACHED).next().unwrap_or(content)
}

/// Each reference in `text` as written, without its `@`: an `@` at the start
/// or after a space, up to the next space, `\ ` standing for a space. One in
/// an address such as `me@example.com` has no space before it.
fn references(text: &str) -> Vec<&str> {
    let mut found = Vec::new();
    let mut before = ' ';
    for (at, c) in text.char_indices() {
        if c == '@' && before.is_whitespace() {
            let rest = &text[at + 1..];
            let len = word_len(rest);
            let written = rest[..len].trim_end_matches([',', ';', ':', '!', '?', ')', '"', '\'']);
            let named = written
                .starts_with(|c: char| c.is_alphanumeric() || matches!(c, '.' | '_' | '-' | '/'));
            if named && !MENTIONS.contains(&written.to_lowercase().as_str()) {
                found.push(written);
            }
        }
        before = c;
    }
    found
}

/// How many bytes of `text` one word takes, a space after a backslash kept.
fn word_len(text: &str) -> usize {
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        if c.is_whitespace() && !escaped {
            return at;
        }
        escaped = c == '\\';
    }
    text.len()
}

/// The section for one reference, headed by it: the file's lines, the
/// folder's entries, or why it is not attached. None for a path that is not
/// there, which is left as typed text.
fn attach(written: &str, ctx: &ToolContext) -> Option<String> {
    let bare = written.trim_end_matches('.');
    let named = if bare == DIFF {
        Some(diff(ctx))
    } else {
        session_id(bare).map(tools::read_end)
    };
    if let Some(body) = named {
        let header = format!("--- @{bare} ---");
        return Some(match body {
            Ok(body) => format!("{header}\n{body}"),
            Err(why) => format!("{header}\nNot attached: {why}"),
        });
    }
    let (raw, lines) = split_lines(written);
    let raw = raw.replace("\\ ", " ");
    // A sentence can end on a reference, but a file can end on a dot too.
    let raw = if !exists(&raw, ctx) && exists(raw.trim_end_matches('.'), ctx) {
        raw.trim_end_matches('.').to_owned()
    } else {
        raw
    };
    if !exists(&raw, ctx) {
        return None;
    }
    let header = format!("--- @{} ---", written.trim_end_matches('.'));
    let body = match tools::resolve(&raw, ctx) {
        Ok(path) if path.is_dir() => listing(&path),
        Ok(path) => read(&path, lines),
        Err(why) => Err(why),
    };
    Some(match body {
        Ok(body) => format!("{header}\n{body}"),
        Err(why) => format!("{header}\nNot attached: {why}"),
    })
}

/// The session a reference such as `session:12` names.
fn session_id(written: &str) -> Option<u64> {
    written.strip_prefix(SESSION)?.parse().ok()
}

/// The project's changes to tracked files since its last commit, as git
/// shows them, leaving out what `read_file` would refuse to read.
fn diff(ctx: &ToolContext) -> Result<String, String> {
    let Some(folder) = ctx.workspace.as_deref() else {
        return Err("the session has no project folder to compare.".into());
    };
    // A program named in the repository's own config would run on a diff,
    // and nothing asked first; `git diff` as a command asks.
    if crate::command::config_runs_programs(folder) {
        return Err(
            "the repository's config names programs git would run, so ask for git diff instead."
                .into(),
        );
    }
    let names = git(folder, &["diff", "--name-only", "-z", "--relative", "HEAD"])?;
    // A list cut short ends part way through a name, and a pathspec of part
    // of one can be the folder holding a file of keys, so only the names git
    // ended are taken.
    let ended = names.rfind('\0').map_or("", |at| &names[..at]);
    let data = crate::chat::data_dir();
    let (shown, kept): (Vec<&str>, Vec<&str>) = ended
        .split('\0')
        .filter(|name| !name.is_empty())
        // Joined, not resolved: a deleted file is a change too.
        .partition(|name| {
            let path = folder.join(name);
            !tools::secret(&path) && !tools::private(&path, &data)
        });
    if shown.is_empty() && kept.is_empty() {
        return Ok("No changes since the last commit.".into());
    }
    let mut args = vec![
        "diff",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--relative",
        "HEAD",
        "--",
    ];
    args.extend(&shown);
    let mut out = if shown.is_empty() {
        String::new()
    } else {
        git(folder, &args)?
    };
    if !kept.is_empty() {
        out.push_str(&format!(
            "\n[Left out, as files holding keys: {}.]",
            kept.join(", ")
        ));
    }
    Ok(out.trim_start_matches('\n').to_owned())
}

/// What git prints for `args` in `folder`, with the read-only guards on, at
/// most `ATTACHED_CHARS` of it and for at most `DIFF_TIME`.
fn git(folder: &Path, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new("git");
    command
        .args(["--no-pager", "--literal-pathspecs"])
        .args(args)
        .current_dir(folder)
        .envs(crate::command::GUARDS.iter().copied())
        .env_remove("GIT_EXTERNAL_DIFF")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("git could not be run: {e}"))?;
    let Some(stdout) = child.stdout.take() else {
        return Err("git gave no output to read.".into());
    };
    // Read on a thread, so a git that hangs is given up on rather than
    // holding the message.
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let read = stdout
            .take(ATTACHED_CHARS as u64)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = sender.send(read);
    });
    let read = receiver.recv_timeout(DIFF_TIME);
    let whole = matches!(&read, Ok(Ok(bytes)) if bytes.len() < ATTACHED_CHARS);
    // Cut or late, the rest is not wanted and git would wait on it.
    if !whole {
        let _ = child.kill();
    }
    let status = child.wait();
    let bytes = match read {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(e)) => return Err(format!("git's output could not be read: {e}")),
        Err(_) => return Err("git took too long to say what changed.".into()),
    };
    if whole && !status.is_ok_and(|s| s.success()) {
        return Err(
            "git could not compare the folder; is it in a repository with a commit?".into(),
        );
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Whether `raw` names something on disk, as `resolve` would join it. A link
/// counts even when it leads nowhere, so a refusal says why.
fn exists(raw: &str, ctx: &ToolContext) -> bool {
    let base = ctx
        .workspace
        .clone()
        .or_else(tools::home)
        .unwrap_or_default();
    !raw.is_empty() && std::fs::symlink_metadata(base.join(raw)).is_ok()
}

/// A reference split into its path and the lines asked for, first and last,
/// from a `#L10-20` or `#L10` on its end; a path without one asks for all.
fn split_lines(written: &str) -> (&str, Option<(usize, usize)>) {
    let Some((path, range)) = written.rsplit_once("#L") else {
        return (written, None);
    };
    let range = range.trim_end_matches('.');
    let (first, last) = range.split_once('-').unwrap_or((range, range));
    match (first.parse::<usize>(), last.parse::<usize>()) {
        (Ok(first), Ok(last)) if first > 0 => (path, Some((first, last.max(first)))),
        _ => (written, None),
    }
}

/// The lines asked for, numbered and paged as `read_file` shows them.
fn read(path: &Path, lines: Option<(usize, usize)>) -> Result<String, String> {
    let (offset, limit) = lines.map_or((1, usize::MAX), |(first, last)| (first, last - first + 1));
    let (mut text, cut) = tools::open_text(path)?;
    tools::page(&mut text, offset, limit, cut).map(|(shown, _)| shown)
}

/// A folder's entries as `list_dir` gives them, folders ending in a slash.
fn listing(path: &Path) -> Result<String, String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .map_err(|e| e.to_string())?
        .filter_map(Result::ok)
        .map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir() { name + "/" } else { name }
        })
        .collect();
    if names.is_empty() {
        return Ok("The folder is empty.".into());
    }
    names.sort();
    let more = names.len().saturating_sub(LISTED);
    names.truncate(LISTED);
    if more > 0 {
        names.push(format!("[{more} more entries are left out.]"));
    }
    Ok(names.join("\n"))
}

/// The reference that names `path`: relative to `workspace` when inside it,
/// a folder ending in a slash, and spaces written as `\ `.
pub fn reference(path: &Path, workspace: Option<&Path>) -> String {
    let inside = workspace.and_then(|w| path.strip_prefix(w).ok());
    let mut named = inside.unwrap_or(path).to_string_lossy().into_owned();
    if path.is_dir() && !named.ends_with(['/', '\\']) {
        named.push('/');
    }
    format!("@{}", named.replace(' ', "\\ "))
}

/// What `partial`, typed after an `@`, completes to, and the names it could
/// go on as when there is more than one, for the input to offer.
pub fn complete(partial: &str, workspace: Option<&Path>) -> (String, Vec<String>) {
    let ctx = ToolContext {
        workspace: workspace.map(Path::to_path_buf),
        ..ToolContext::default()
    };
    let raw = partial.replace("\\ ", " ");
    let (folder, start) = match raw.rfind(['/', '\\']) {
        Some(at) => (&raw[..=at], &raw[at + 1..]),
        None => ("", raw.as_str()),
    };
    // Through `resolve`, so no names come from a folder that may not be read.
    let Ok(dir) = tools::resolve(if folder.is_empty() { "." } else { folder }, &ctx) else {
        return (partial.to_owned(), Vec::new());
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return (partial.to_owned(), Vec::new());
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            // Hidden entries only once a dot is typed, as a shell offers them.
            let offered =
                name.starts_with(start) && (start.starts_with('.') || !name.starts_with('.'));
            offered.then(|| if e.path().is_dir() { name + "/" } else { name })
        })
        .collect();
    names.sort();
    let Some(first) = names.first() else {
        return (partial.to_owned(), Vec::new());
    };
    let common = names.iter().fold(first.as_str(), |common, name| {
        let len = common
            .char_indices()
            .zip(name.chars())
            .find(|((_, a), b)| a != b)
            .map_or(common.len().min(name.len()), |((at, _), _)| at);
        &common[..len]
    });
    let completed = format!("{folder}{common}").replace(' ', "\\ ");
    let choices = if names.len() > 1 { names } else { Vec::new() };
    (completed, choices)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn project(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-refs-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        let lines: String = (1..=30).map(|n| format!("line {n}\n")).collect();
        std::fs::write(dir.join("src/main.rs"), lines).unwrap();
        std::fs::write(dir.join("src/lib.rs"), "pub fn lib() {}\n").unwrap();
        std::fs::write(dir.join(".env"), "TOKEN=sk-secret-value\n").unwrap();
        std::fs::write(dir.join("notes with space.md"), "spaced\n").unwrap();
        dir
    }

    #[test]
    fn a_file_is_attached_whole_under_its_header() {
        let dir = project("whole");
        let sent = expand("look at @src/lib.rs please", Some(&dir));
        assert_eq!(typed(&sent), "look at @src/lib.rs please");
        assert!(
            sent.contains("--- @src/lib.rs ---\n1|pub fn lib() {}"),
            "{sent}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_range_and_a_single_line_attach_only_those_lines() {
        let dir = project("range");
        let sent = expand("@src/main.rs#L10-12 and @src/main.rs#L20", Some(&dir));
        assert!(
            sent.contains("--- @src/main.rs#L10-12 ---\n10|line 10\n11|line 11\n12|line 12\n"),
            "{sent}"
        );
        assert!(!sent.contains("13|line 13"), "{sent}");
        assert!(
            sent.contains("--- @src/main.rs#L20 ---\n20|line 20"),
            "{sent}"
        );
        assert!(!sent.contains("21|line 21"), "{sent}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_range_past_the_end_says_so_and_one_running_over_stops_at_the_end() {
        let dir = project("past");
        let sent = expand("@src/main.rs#L40-50", Some(&dir));
        assert!(
            sent.contains("Not attached: The file has 30 lines, so offset 40 is past its end."),
            "{sent}"
        );
        let sent = expand("@src/main.rs#L29-99", Some(&dir));
        assert!(sent.ends_with("29|line 29\n30|line 30"), "{sent}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_folder_is_attached_as_its_listing() {
        let dir = project("folder");
        let sent = expand("what is in @src/", Some(&dir));
        assert!(
            sent.contains("--- @src/ ---\ndeep/\nlib.rs\nmain.rs"),
            "{sent}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_missing_path_stays_as_typed_text() {
        let dir = project("missing");
        let text = "fix @src/gone.rs and @nothing/ here";
        assert_eq!(expand(text, Some(&dir)), text);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_secret_file_is_refused_as_read_file_refuses_it() {
        let dir = project("secret");
        let sent = expand("use @.env", Some(&dir));
        assert!(
            sent.contains("--- @.env ---\nNot attached: `.env` holds keys"),
            "{sent}"
        );
        assert!(!sent.contains("sk-secret-value"), "{sent}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_email_address_and_the_crew_mention_are_not_paths() {
        let dir = project("mention");
        std::fs::write(dir.join("crew"), "not for attaching\n").unwrap();
        std::fs::write(dir.join("example.com"), "not for attaching\n").unwrap();
        let text = "mail me@example.com and ask @crew about it";
        assert_eq!(expand(text, Some(&dir)), text);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_reference_ending_a_sentence_or_holding_a_space_is_found() {
        let dir = project("punct");
        let sent = expand(
            "read @src/lib.rs. Then @notes\\ with\\ space.md, too",
            Some(&dir),
        );
        assert!(
            sent.contains("--- @src/lib.rs ---\n1|pub fn lib() {}"),
            "{sent}"
        );
        assert!(
            sent.contains("--- @notes\\ with\\ space.md ---\n1|spaced"),
            "{sent}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn many_references_together_stay_under_the_cap() {
        let dir = project("cap");
        let long: String = (1..=400)
            .map(|n| format!("{n} {}\n", "x".repeat(60)))
            .collect();
        for n in 0..8 {
            std::fs::write(dir.join(format!("big{n}.txt")), &long).unwrap();
        }
        let text: String = (0..8).map(|n| format!("@big{n}.txt ")).collect();
        let sent = expand(&text, Some(&dir));
        let attached = sent.len() - typed(&sent).len();
        assert!(attached < ATTACHED_CHARS + 500, "{attached}");
        assert!(sent.contains("[The rest is left out"), "{sent}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_picked_file_is_named_so_that_expanding_finds_it() {
        let dir = project("picked");
        let named = reference(&dir.join("notes with space.md"), Some(&dir));
        assert_eq!(named, "@notes\\ with\\ space.md");
        assert_eq!(reference(&dir.join("src"), Some(&dir)), "@src/");
        assert!(expand(&named, Some(&dir)).contains("1|spaced"));
        let outside = reference(&dir.join("src/lib.rs"), None);
        assert_eq!(outside, format!("@{}", dir.join("src/lib.rs").display()));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn diff_attaches_the_changes_but_not_a_file_of_keys() {
        let dir = project("diff");
        let git = |args: &[&str]| {
            let done = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(done.status.success(), "{done:?}");
        };
        git(&["init", "-q"]);
        git(&["add", "-f", "."]);
        git(&["commit", "-q", "-m", "start"]);
        std::fs::write(dir.join("src/lib.rs"), "pub fn changed() {}\n").unwrap();
        std::fs::write(dir.join(".env"), "TOKEN=sk-new-secret\n").unwrap();
        let sent = expand("review @diff.", Some(&dir));
        assert!(sent.contains("--- @diff ---\ndiff --git"), "{sent}");
        assert!(sent.contains("+pub fn changed() {}"), "{sent}");
        assert!(!sent.contains("sk-new-secret"), "{sent}");
        assert!(
            sent.contains("[Left out, as files holding keys: .env.]"),
            "{sent}"
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_cut_list_of_changes_does_not_take_in_the_folder_of_a_part_name() {
        let dir = project("diff-cut");
        let git = |args: &[&str]| {
            let done = Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(done.status.success(), "{done:?}");
        };
        // Files of keys, left out of the diff, whose names with their NULs
        // fill all but two bytes of what is read, so the list is cut just
        // after the `zz` of `zz/.env`.
        std::fs::create_dir_all(dir.join(".aws")).unwrap();
        std::fs::create_dir_all(dir.join("zz")).unwrap();
        let mut filled = 0;
        let mut n = 0;
        while filled < ATTACHED_CHARS - 2 {
            let len = (ATTACHED_CHARS - 2 - filled).min(80) - 1;
            let name = format!(".aws/{n:04}{}", "x".repeat(len - 9));
            std::fs::write(dir.join(&name), "old\n").unwrap();
            filled += len + 1;
            n += 1;
        }
        std::fs::write(dir.join("zz/.env"), "TOKEN=old\n").unwrap();
        git(&["init", "-q"]);
        git(&["add", "-f", "."]);
        git(&["commit", "-q", "-m", "start"]);
        for entry in std::fs::read_dir(dir.join(".aws")).unwrap() {
            std::fs::write(entry.unwrap().path(), "new\n").unwrap();
        }
        std::fs::write(dir.join("zz/.env"), "TOKEN=sk-cut-secret\n").unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..ToolContext::default()
        };
        let shown = diff(&ctx).unwrap();
        assert!(!shown.contains("sk-cut-secret"), "{shown}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn diff_without_a_project_says_why_and_a_bad_session_stays_as_typed() {
        let sent = expand("@diff", None);
        assert!(
            sent.contains("--- @diff ---\nNot attached: the session has no project folder"),
            "{sent}"
        );
        assert_eq!(session_id("session:12"), Some(12));
        assert_eq!(session_id("session:twelve"), None);
        let dir = project("session");
        assert_eq!(expand("see @session:x", Some(&dir)), "see @session:x");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_path_completes_to_what_the_names_share() {
        let dir = project("complete");
        assert_eq!(complete("sr", Some(&dir)), ("src/".to_owned(), Vec::new()));
        let (completed, choices) = complete("src/", Some(&dir));
        assert_eq!(completed, "src/");
        assert_eq!(choices, ["deep/", "lib.rs", "main.rs"]);
        assert_eq!(complete("src/m", Some(&dir)).0, "src/main.rs");
        assert_eq!(complete("notes", Some(&dir)).0, "notes\\ with\\ space.md");
        assert_eq!(complete("zz", Some(&dir)), ("zz".to_owned(), Vec::new()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
