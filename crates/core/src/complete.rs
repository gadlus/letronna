//! What the composer offers after a `/` or an `@`.
//!
//! `/` at the start of a message offers the front end's own commands and the
//! skills switched on; `@` offers the project's files and folders, earlier
//! sessions and `@diff`. The project's names come from the search tool's own
//! walk, so `.gitignore` is honoured and the walk stops at the same time and
//! file budget, and they are kept a while per project so typing does not walk
//! it again. A path is offered only when sending would attach it: it goes
//! through `read_file`'s checks, as `references::expand` does.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::references::{DIFF, SESSION};
use crate::skills::Skill;
use crate::tools::{self, ToolContext};

/// The most choices offered at once; more is a list nobody reads down.
const OFFERED: usize = 40;

/// Sessions offered among the paths, so they never push the files away.
const SESSIONS: usize = 5;

/// How long a project's names are reused before it is walked again: long
/// enough for one reference to be typed, short enough that a file made a
/// moment ago turns up.
const FRESH: std::time::Duration = std::time::Duration::from_secs(30);

/// A project's names, walked once and kept, with when they were walked.
type Walked = (Instant, Arc<Vec<String>>);

/// One project's names, behind a lock held while it is walked.
type Slot = Arc<Mutex<Option<Walked>>>;

/// Each project's names, behind a lock of its own that is held while it is
/// walked, so the keys typed during a walk wait for it rather than each
/// starting another.
static WALKED: Mutex<Option<HashMap<PathBuf, Slot>>> = Mutex::new(None);

/// What a choice is, for the icon beside it and what taking it does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Command,
    Skill,
    Folder,
    File,
    Session,
    Diff,
}

/// One thing the composer can offer.
#[derive(Clone, Debug, PartialEq)]
pub struct Choice {
    pub kind: Kind,
    /// What the row shows.
    pub label: String,
    /// What the row shows beside it, dimmed.
    pub detail: String,
    /// What takes the place of what was typed, the `/` or `@` included.
    pub text: String,
}

/// A `/` or `@` being typed: which, where it is and what follows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Typing<'a> {
    pub trigger: char,
    /// The byte the `/` or `@` is at.
    pub start: usize,
    /// What is typed after it, up to the cursor.
    pub query: &'a str,
}

/// What is being completed with the cursor at byte `cursor` of `text`: a
/// `/` that starts the message, or an `@` at the start or after a space, with
/// no space typed since (`\ ` stands for one in a path).
pub fn typing(text: &str, cursor: usize) -> Option<Typing<'_>> {
    let before = text.get(..cursor)?;
    let start = word_start(before);
    let word = &before[start..];
    let trigger = word.chars().next()?;
    let query = &word[trigger.len_utf8()..];
    let offered = match trigger {
        // `//` sends a line starting with a slash, so it is not a command.
        '/' => before[..start].trim().is_empty() && !query.starts_with('/'),
        '@' => true,
        _ => false,
    };
    offered.then_some(Typing {
        trigger,
        start,
        query,
    })
}

/// Where the last word of `text` starts: after the last space that is not
/// written `\ `.
fn word_start(text: &str) -> usize {
    let mut start = 0;
    let mut escaped = false;
    for (at, c) in text.char_indices() {
        if c.is_whitespace() && !escaped {
            start = at + c.len_utf8();
        }
        escaped = c == '\\';
    }
    start
}

/// The commands and skills a `/` followed by `query` could mean: `built_ins`
/// as (name, what it does) without their slash, then each skill switched on,
/// those starting with `query` before those only holding it.
pub fn commands(query: &str, built_ins: &[(&str, &str)], skills: &[Skill]) -> Vec<Choice> {
    let wanted = query.to_lowercase();
    let mut found: Vec<(bool, Choice)> = Vec::new();
    let offers = built_ins
        .iter()
        .map(|&(name, about)| {
            (
                name.to_owned(),
                Choice {
                    kind: Kind::Command,
                    label: format!("/{name}"),
                    detail: about.to_owned(),
                    text: format!("/{name} "),
                },
            )
        })
        .chain(skills.iter().filter(|s| s.enabled).map(|skill| {
            (
                skill.name.clone(),
                Choice {
                    kind: Kind::Skill,
                    label: format!("/{}", skill.name),
                    detail: one_line(&skill.description),
                    // A skill goes into the box to be read and sent, as the
                    // terminal's /skills <name> puts it there.
                    text: skill.body.clone(),
                },
            )
        }));
    for (name, choice) in offers {
        let name = name.to_lowercase();
        if name.starts_with(&wanted) {
            found.push((true, choice));
        } else if name.contains(&wanted) {
            found.push((false, choice));
        }
    }
    // Stable, so each group keeps the order it was given in.
    found.sort_by_key(|(first, _)| !first);
    found
        .into_iter()
        .map(|(_, choice)| choice)
        .take(OFFERED)
        .collect()
}

/// What an `@` followed by `query` could mean: the project's paths, then
/// earlier sessions as (id, title) and `@diff` when their names hold it. A
/// `/` in `query` looks only in that folder. With no project folder there are
/// no paths, rather than a walk of the home folder.
pub fn references(
    query: &str,
    workspace: Option<&Path>,
    sessions: &[(u64, String)],
) -> Vec<Choice> {
    let raw = query.replace("\\ ", " ");
    let mut found = match workspace {
        Some(root) => {
            let names = walked(root);
            let ctx = ToolContext {
                workspace: Some(root.to_path_buf()),
                ..ToolContext::default()
            };
            matching(&names, &raw)
                .into_iter()
                // Checked last and only for what is shown, since each check
                // opens the path.
                .filter(|path| tools::resolve(path, &ctx).is_ok())
                .take(OFFERED)
                .map(path_choice)
                .collect()
        }
        None => Vec::new(),
    };
    if raw.contains('/') {
        return found;
    }
    let wanted = raw.to_lowercase();
    let sessions = sessions
        .iter()
        .filter(|(_, title)| title.to_lowercase().contains(&wanted))
        .take(SESSIONS)
        .map(|(id, title)| Choice {
            kind: Kind::Session,
            label: match one_line(title) {
                title if title.is_empty() => "Untitled session".to_owned(),
                title => title,
            },
            detail: format!("@{SESSION}{id}"),
            text: format!("@{SESSION}{id} "),
        });
    let mut others: Vec<Choice> = sessions.collect();
    if workspace.is_some() && DIFF.starts_with(&wanted) {
        others.insert(
            0,
            Choice {
                kind: Kind::Diff,
                label: format!("@{DIFF}"),
                detail: "what changed since the last commit".to_owned(),
                text: format!("@{DIFF} "),
            },
        );
    }
    // Typed out, a session or the diff is what was meant, not a file whose
    // name happens to hold the same letters.
    let named = !wanted.is_empty();
    if named {
        found.splice(0..0, others);
    } else {
        found.extend(others);
    }
    found.truncate(OFFERED);
    found
}

/// The choice for a path the walk found, relative to the project.
fn path_choice(path: &str) -> Choice {
    let folder = path.ends_with('/');
    let name = path.trim_end_matches('/');
    let (parent, leaf) = name.rsplit_once('/').unwrap_or(("", name));
    Choice {
        kind: if folder { Kind::Folder } else { Kind::File },
        label: if folder {
            format!("{leaf}/")
        } else {
            leaf.to_owned()
        },
        detail: parent.to_owned(),
        text: format!("@{} ", path.replace(' ', "\\ ")),
    }
}

/// The paths among `names` that `query` could mean. With a folder typed, the
/// entries right in it whose names hold the rest; with none, those at the top
/// for nothing typed, else any whose name holds it. Names starting with what
/// is typed come first, then shallower ones; hidden names wait for a dot.
fn matching<'a>(names: &'a [String], query: &str) -> Vec<&'a str> {
    let (folder, wanted) = match query.rfind('/') {
        Some(at) => (&query[..=at], &query[at + 1..]),
        None => ("", query),
    };
    let (folder, wanted) = (folder.to_lowercase(), wanted.to_lowercase());
    let mut found: Vec<(bool, usize, &str)> = names
        .iter()
        .filter_map(|path| {
            let lower = path.to_lowercase();
            let name = lower.trim_end_matches('/');
            let (parent, leaf) = match name.rfind('/') {
                Some(at) => (&lower[..=at], &name[at + 1..]),
                None => ("", name),
            };
            let depth = name.matches('/').count();
            let near = if query.contains('/') || wanted.is_empty() {
                parent == folder
            } else {
                true
            };
            // Hidden names only once a dot is typed, as a shell offers them.
            let shown = wanted.starts_with('.') || !leaf.starts_with('.');
            (near && shown && leaf.contains(&wanted))
                .then(|| (leaf.starts_with(&wanted), depth, path.as_str()))
        })
        .collect();
    // Stable, so the walk's name order holds within each rank.
    found.sort_by_key(|&(first, depth, _)| (!first, depth));
    found.into_iter().map(|(_, _, path)| path).collect()
}

/// The names under `root`, walked again when the last walk is older than
/// `FRESH`. The walk stops at the search tool's time and file budget.
pub fn walked(root: &Path) -> Arc<Vec<String>> {
    let walk = || {
        let deadline = Instant::now() + crate::grep::SEARCH_TIME;
        Arc::new(
            crate::grep::entries(root, deadline, crate::grep::SEARCH_FILES).unwrap_or_default(),
        )
    };
    // The map's lock is let go at once, so another project's list is not
    // held up by this one's walk.
    let slot = match WALKED.lock() {
        Ok(mut kept) => kept
            .get_or_insert_with(HashMap::new)
            .entry(root.to_path_buf())
            .or_default()
            .clone(),
        Err(_) => return walk(),
    };
    let Ok(mut kept) = slot.lock() else {
        return walk();
    };
    if let Some((when, names)) = kept.as_ref()
        && when.elapsed() < FRESH
    {
        return names.clone();
    }
    let names = walk();
    *kept = Some((Instant::now(), names.clone()));
    names
}

/// `text` on one line, for a row.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(label: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-complete-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src/deep")).unwrap();
        std::fs::create_dir_all(dir.join("target/debug")).unwrap();
        std::fs::write(dir.join("src/main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.join("src/deep/mainly.rs"), "\n").unwrap();
        std::fs::write(dir.join("src/lib.rs"), "\n").unwrap();
        std::fs::write(dir.join("target/debug/main"), "\n").unwrap();
        std::fs::write(dir.join("notes with space.md"), "\n").unwrap();
        std::fs::write(dir.join(".env"), "TOKEN=x\n").unwrap();
        std::fs::write(dir.join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn skill(name: &str, enabled: bool) -> Skill {
        Skill {
            name: name.into(),
            description: "Reviews\na change".into(),
            body: format!("Use {name} on this."),
            folder: PathBuf::new(),
            source: crate::skills::Source::User,
            enabled,
            learned: false,
        }
    }

    #[test]
    fn a_slash_only_counts_at_the_start_of_the_message() {
        let found = typing("/com", 4).unwrap();
        assert_eq!((found.trigger, found.start, found.query), ('/', 0, "com"));
        assert_eq!(typing("  /new", 6).map(|t| t.start), Some(2));
        assert_eq!(typing("run /new", 8), None);
        assert_eq!(typing("//tmp", 5), None);
        assert_eq!(typing("/new now", 8), None);
    }

    #[test]
    fn a_word_starting_with_a_wide_letter_is_not_completed() {
        assert_eq!(typing("é", 2), None);
        assert_eq!(typing("voilà 日本", 13), None);
    }

    #[test]
    fn an_at_counts_at_the_start_of_a_word_up_to_the_cursor() {
        let found = typing("look at @src/ma please", 15).unwrap();
        assert_eq!(
            (found.trigger, found.start, found.query),
            ('@', 8, "src/ma")
        );
        assert_eq!(
            typing("@notes\\ wi", 10).map(|t| t.query),
            Some("notes\\ wi")
        );
        assert_eq!(typing("mail me@example", 15), None);
        assert_eq!(typing("@src done", 9), None);
    }

    #[test]
    fn commands_offer_the_built_ins_then_the_skills_switched_on() {
        let built_ins = [("new", "start a fresh session"), ("btw", "ask aside")];
        let skills = [
            skill("review", true),
            skill("never", false),
            skill("renew", true),
        ];
        let labels = |query| -> Vec<String> {
            commands(query, &built_ins, &skills)
                .into_iter()
                .map(|c| c.label)
                .collect()
        };
        assert_eq!(labels(""), ["/new", "/btw", "/review", "/renew"]);
        assert_eq!(labels("ne"), ["/new", "/renew"]);
        let review = commands("rev", &built_ins, &skills).remove(0);
        assert_eq!(review.kind, Kind::Skill);
        assert_eq!(review.detail, "Reviews a change");
        assert_eq!(review.text, "Use review on this.");
        assert_eq!(commands("new", &built_ins, &skills)[0].text, "/new ");
    }

    #[test]
    fn the_walk_lists_files_and_folders_but_not_ignored_or_secret_ones() {
        let dir = project("walk");
        let names =
            crate::grep::entries(&dir, Instant::now() + crate::grep::SEARCH_TIME, 100).unwrap();
        for wanted in ["src/", "src/main.rs", "src/deep/", "src/deep/mainly.rs"] {
            assert!(names.iter().any(|n| n == wanted), "{wanted} in {names:?}");
        }
        assert!(!names.iter().any(|n| n.starts_with("target")), "{names:?}");
        assert!(!names.iter().any(|n| n == ".env"), "{names:?}");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn keys_typed_during_a_walk_wait_for_it_rather_than_walking_again() {
        let dir = project("once");
        for n in 0..2_000 {
            std::fs::write(dir.join(format!("src/file{n}.rs")), "\n").unwrap();
        }
        let start = Arc::new(std::sync::Barrier::new(8));
        let lists: Vec<_> = (0..8)
            .map(|_| {
                let (dir, start) = (dir.clone(), start.clone());
                std::thread::spawn(move || {
                    start.wait();
                    walked(&dir)
                })
            })
            .collect();
        let lists: Vec<_> = lists.into_iter().map(|t| t.join().unwrap()).collect();
        assert!(lists.iter().all(|l| Arc::ptr_eq(l, &lists[0])));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn nothing_typed_offers_the_top_of_the_project_then_the_diff_and_sessions() {
        let dir = project("top");
        let sessions = [(7, "Fix the build".to_owned())];
        let found = references("", Some(&dir), &sessions);
        let labels: Vec<&str> = found.iter().map(|c| c.label.as_str()).collect();
        assert!(
            labels.starts_with(&["notes with space.md", "src/"]),
            "{labels:?}"
        );
        assert!(labels.ends_with(&["@diff", "Fix the build"]), "{labels:?}");
        assert_eq!(found[0].text, "@notes\\ with\\ space.md ");
        assert_eq!(found.last().unwrap().text, "@session:7 ");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_name_matches_anywhere_and_a_folder_lists_what_is_in_it() {
        let dir = project("match");
        let texts = |query| -> Vec<String> {
            references(query, Some(&dir), &[])
                .into_iter()
                .filter(|c| matches!(c.kind, Kind::File | Kind::Folder))
                .map(|c| c.text)
                .collect()
        };
        assert_eq!(texts("main"), ["@src/main.rs ", "@src/deep/mainly.rs "]);
        assert_eq!(
            texts("src/"),
            ["@src/deep/ ", "@src/lib.rs ", "@src/main.rs "]
        );
        assert_eq!(texts("src/l"), ["@src/lib.rs "]);
        assert_eq!(texts("notes\\ w"), ["@notes\\ with\\ space.md "]);
        assert_eq!(texts(".git"), ["@.gitignore "]);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn without_a_project_only_sessions_are_offered() {
        let sessions = [
            (3, "Notes on\nthe release".to_owned()),
            (4, "Other".to_owned()),
        ];
        let found = references("rel", None, &sessions);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "Notes on the release");
        assert_eq!(found[0].kind, Kind::Session);
    }
}
