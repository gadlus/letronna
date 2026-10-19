//! `/memory`: what Letronna keeps from one session to the next, and a way
//! into the files it is kept in.
//!
//! The listing numbers each entry so it can be found in the file. Changing
//! one is done in the file, in the user's own editor, rather than in a line
//! editor here: the stores are plain text meant to be edited that way. The
//! editor gets a copy, put in place of the file only if nothing else wrote
//! the file meanwhile, as `crontab -e` does. What the file holds afterwards
//! is only reported on, never refused, since it is the user's own; the
//! memory tool still keeps the model to the budgets.
//!
//! `/memory log` lists what changed each store, the model's replaces and
//! removes among it, and `/memory restore` puts a store back as it was
//! before one of those changes. A restore is a change of its own, so it can
//! be undone the same way.

use letronna_core::chat;
use letronna_core::memory::{self, Change, Store};
use std::path::{Path, PathBuf};

use crate::render;
use crate::session::Runner;

/// What `/memory edit` takes, as the listing shows the stores.
const TARGETS: [&str; 4] = ["user", "memory", "project", "soul"];

/// How many changes of each store `/memory log` shows when it lists them
/// all: what changed lately fits on a screen, and one store's log has the
/// rest.
const LATEST: usize = 5;

/// `/memory`, or `/memory edit` and the store to open, `/memory log` and the
/// store whose changes to list, or `/memory restore`, the store and the
/// change to put it back before.
pub fn memory(runner: &mut Runner, rest: &str, workspace: Option<&Path>) {
    render::blank();
    let (word, given) = rest.split_once(' ').unwrap_or((rest, ""));
    let given = given.trim();
    let target = match given {
        "" => "memory",
        target => target,
    };
    match (word, change_named(given)) {
        ("", _) => list(workspace),
        ("edit", _) if TARGETS.contains(&target) => edit(runner, target, workspace),
        ("log", _) if given.is_empty() => {
            for target in TARGETS {
                // Without a project folder there is no project memory, so
                // nothing it changed to show.
                if target != "project" || workspace.is_some() {
                    log(target, workspace, LATEST);
                }
            }
        }
        ("log", _) if TARGETS.contains(&target) => log(target, workspace, usize::MAX),
        ("restore", Some((target, number))) => restore(target, number, workspace),
        _ => render::line(&render::red(
            "Usage: /memory, /memory edit or log user|memory|project|soul, or /memory restore user|memory|project|soul and a change's number.",
        )),
    }
}

/// The store and the number of the change `/memory restore` names, as
/// `user 12`.
fn change_named(given: &str) -> Option<(&'static str, u64)> {
    let (target, number) = given.split_once(' ')?;
    let target = TARGETS.into_iter().find(|known| *known == target)?;
    Some((target, number.trim().parse().ok()?))
}

/// A store's changes, newest first and at most `most` of them, each with
/// what it took out and put in.
fn log(target: &str, workspace: Option<&Path>, most: usize) {
    let changes = match memory::history(workspace, target) {
        Ok(changes) => changes,
        Err(error) => return render::line(&render::red(&error)),
    };
    let name = memory::store(workspace, target).map_or(target, |store| store.name);
    render::line(&render::bold(name));
    if changes.is_empty() {
        render::line(&render::dim("  no changes kept yet"));
    }
    for change in changes.iter().take(most) {
        render::line(&format!(
            "{}  {}",
            render::bold(&format!("{:>4}", change.number)),
            render::dim(&format!("{}, by {}", chat::ago(change.time), change.by()))
        ));
        changed(change);
    }
    if changes.len() > most {
        render::line(&render::dim(&format!(
            "  {} earlier: /memory log {target}",
            changes.len() - most
        )));
    }
    render::blank();
}

/// Each entry a change took out and put in, on a line of its own, cut to
/// the screen and with nothing in it that acts on the terminal.
fn changed(change: &Change) {
    let (removed, added) = (change.removed(), change.added());
    if removed.is_empty() && added.is_empty() {
        render::line(&render::dim("        the same entries, laid out anew"));
    }
    let room = render::width().saturating_sub(8);
    let marked = |mark: String, entry: &str| {
        let entry = render::fit(&memory::readable(entry), room);
        render::line(&format!("      {mark} {entry}"));
    };
    for entry in &removed {
        marked(render::red("-"), entry);
    }
    for entry in &added {
        marked(render::green("+"), entry);
    }
}

/// Put a store back as it was before one of its changes, and say what that
/// took out and put back, and how to undo it.
fn restore(target: &str, number: u64, workspace: Option<&Path>) {
    let name = memory::store(workspace, target).map_or(target, |store| store.name);
    match memory::restore(workspace, target, number) {
        Err(error) => render::line(&render::red(&error)),
        Ok(None) => render::line(&render::dim(&format!(
            "{name} already holds what it did before change {number}."
        ))),
        Ok(Some(change)) => {
            render::line(&format!("{name} is as it was before change {number}."));
            changed(&change);
            render::line(&render::dim(&format!(
                "This is change {0}; /memory restore {target} {0} undoes it.",
                change.number
            )));
        }
    }
}

/// Every store with its budget and numbered entries, then SOUL.md.
fn list(workspace: Option<&Path>) {
    for store in memory::stores(workspace) {
        heading(&store);
        match &store.entries {
            Err(error) => render::line(&render::red(error)),
            Ok(entries) if entries.is_empty() => render::line(&render::dim("  empty")),
            Ok(entries) => {
                for (at, entry) in entries.iter().enumerate() {
                    numbered(at + 1, entry);
                }
            }
        }
        render::blank();
    }
    if workspace.is_none() {
        render::line(&render::dim(
            "Project memory: none, as no project folder is attached.",
        ));
    }
    if let Some(soul) = memory::store(workspace, "soul") {
        match &soul.entries {
            Ok(entries) if entries.is_empty() => render::line(&format!(
                "{}  {}",
                render::bold(soul.name),
                render::dim("not written; Letronna's own first line is used")
            )),
            _ => heading(&soul),
        }
    }
    render::line(&render::dim(
        "Change one in your editor with /memory edit user|memory|project|soul, and see what changed with /memory log.",
    ));
}

/// A store's name and how much of its budget it uses, in red when over.
fn heading(store: &Store) {
    let budget = format!("{} characters", store.budget());
    let budget = if store.used > store.limit {
        render::red(&format!("{budget}, over"))
    } else {
        render::dim(&budget)
    };
    render::line(&format!("{}  {budget}", render::bold(store.name)));
}

/// One entry under its number, the lines of a long one under the first, and
/// what it holds that memory is not meant to keep. Escape sequences a model
/// or a file put there are shown as nothing, so none acts on the terminal.
fn numbered(number: usize, entry: &str) {
    let mark = format!("{number:>3}. ");
    for (at, line) in memory::readable(entry).lines().enumerate() {
        let lead = if at == 0 {
            mark.clone()
        } else {
            " ".repeat(mark.len())
        };
        render::line(&format!("{}{line}", render::dim(&lead)));
    }
    if let Some(found) = memory::flagged(entry) {
        render::line(&render::red(&format!("     holds {found}")));
    }
}

/// Open a store's file in the user's editor, then say what it holds now.
fn edit(runner: &mut Runner, target: &str, workspace: Option<&Path>) {
    // The editor gets a copy, which goes back in place of the file only if
    // nothing else wrote the file while it was open.
    let draft = match memory::draft(workspace, target) {
        Ok(draft) => draft,
        Err(error) => return render::line(&render::red(&error)),
    };
    let (program, args) = editor();
    let program = found(&program);
    // Started directly, never through a shell: the path is ours, and so is
    // every word of the command.
    let ran = runner.bar.lend(|| {
        std::process::Command::new(&program)
            .args(&args)
            .arg(&draft.path)
            .status()
    });
    let program = program.display();
    match ran {
        Err(e) => {
            draft.discard();
            return render::line(&render::red(&format!(
                "Could not start {program}: {e}. Set VISUAL or EDITOR to the editor you use."
            )));
        }
        Ok(status) if !status.success() => render::line(&render::amber(&format!(
            "{program} ended with {status}; what it saved is kept."
        ))),
        Ok(_) => {}
    }
    match draft.put_back() {
        Ok(None) => {}
        Ok(Some(note)) => render::line(&render::amber(&note)),
        Err(error) => render::line(&render::red(&error)),
    }
    report(target, workspace);
}

/// What a store holds after an edit, with a warning for going over its
/// budget and for each entry memory is not meant to keep.
fn report(target: &str, workspace: Option<&Path>) {
    let Some(store) = memory::store(workspace, target) else {
        return;
    };
    heading(&store);
    let entries = match &store.entries {
        Ok(entries) => entries,
        Err(error) => return render::line(&render::red(error)),
    };
    let soul = target == "soul";
    if store.used > store.limit {
        render::line(&render::amber(if soul {
            "Over the cap: the prompt cuts SOUL.md short there."
        } else {
            "Over its budget: every request carries all of it, and the model can add nothing more until it is smaller."
        }));
    }
    for (at, entry) in entries.iter().enumerate() {
        let Some(found) = memory::flagged(entry) else {
            continue;
        };
        render::line(&render::amber(&if soul {
            format!("SOUL.md holds {found}, and goes into every request as it is.")
        } else {
            format!(
                "Entry {} holds {found}; the model is told to act on none of it.",
                at + 1
            )
        }));
    }
}

/// The editor to open a file in: $VISUAL, then $EDITOR, as git and crontab
/// read them, then one every system of the kind has. Split into the program
/// and its own words, as `code --wait` needs.
pub(crate) fn editor() -> (String, Vec<String>) {
    let chosen = ["VISUAL", "EDITOR"]
        .into_iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|command| command.trim().to_owned())
        .find(|command| !command.is_empty());
    match chosen {
        // A path with spaces nobody quoted names the program whole.
        Some(command) if Path::new(&command).is_file() => (command, Vec::new()),
        Some(command) => words(&command),
        None if cfg!(windows) => ("notepad".into(), Vec::new()),
        None => ("vi".into(), Vec::new()),
    }
}

/// A command's program and arguments, read the way a shell reads words:
/// quotes keep spaces inside one, and backslashes are left alone so a
/// Windows path survives.
fn words(command: &str) -> (String, Vec<String>) {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    for c in command.chars() {
        match quote {
            Some(open) if c == open => quote = None,
            Some(_) => word.get_or_insert_default().push(c),
            None if c == '"' || c == '\'' => {
                quote = Some(c);
                word.get_or_insert_default();
            }
            None if c.is_whitespace() => words.extend(word.take()),
            None => word.get_or_insert_default().push(c),
        }
    }
    words.extend(word);
    let mut words = words.into_iter();
    (words.next().unwrap_or_default(), words.collect())
}

/// The program to start. On Windows a bare name is looked up on PATH with
/// each extension in PATHEXT, as Command tries only .exe and the `code`
/// there is code.cmd; given its full path, Command runs a .cmd safely.
/// Elsewhere, or when nothing matches, the name as it was given.
pub(crate) fn found(program: &str) -> PathBuf {
    if !cfg!(windows) {
        return PathBuf::from(program);
    }
    let var = |name| std::env::var(name).unwrap_or_default();
    resolve(program, &var("PATH"), &var("PATHEXT"), |path| {
        path.is_file()
    })
    .unwrap_or_else(|| PathBuf::from(program))
}

/// Where a bare program name -- no folder, no drive, no extension -- is
/// found in the `;`-separated Windows `path` with one of the `pathext`
/// extensions, trying each folder in turn as cmd does.
fn resolve(
    program: &str,
    path: &str,
    pathext: &str,
    exists: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    let bare = !program.contains(['/', '\\', ':']) && Path::new(program).extension().is_none();
    if !bare || program.is_empty() {
        return None;
    }
    // What Windows itself falls back to when PATHEXT is unset.
    let pathext = match pathext.trim() {
        "" => ".COM;.EXE;.BAT;.CMD",
        set => set,
    };
    path.split(';')
        .map(|dir| dir.trim().trim_matches('"').trim_end_matches('\\'))
        .filter(|dir| !dir.is_empty())
        .flat_map(|dir| {
            pathext
                .split(';')
                .filter(|ext| !ext.is_empty())
                // Windows reads names in any case; lower case is how they
                // are usually shown.
                .map(str::to_ascii_lowercase)
                .map(move |ext| PathBuf::from(format!("{dir}\\{program}{ext}")))
        })
        .find(|candidate| exists(candidate))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_editor_command_splits_into_words_as_a_shell_reads_them() {
        let split = |command: &str| {
            let (program, args) = words(command);
            (program, args.join("|"))
        };
        assert_eq!(split("vi"), ("vi".into(), String::new()));
        assert_eq!(split("  code   --wait "), ("code".into(), "--wait".into()));
        assert_eq!(
            split(r#""C:\Program Files\Notepad++\notepad++.exe" -multiInst -nosession"#),
            (
                r"C:\Program Files\Notepad++\notepad++.exe".into(),
                "-multiInst|-nosession".into()
            )
        );
        assert_eq!(
            split("'my editor' -f ''"),
            ("my editor".into(), "-f|".into())
        );
    }

    #[test]
    fn a_restore_names_a_known_store_and_a_change_number() {
        assert_eq!(change_named("user 12"), Some(("user", 12)));
        assert_eq!(change_named("soul  3 "), Some(("soul", 3)));
        assert_eq!(change_named("user"), None);
        assert_eq!(change_named("notes 3"), None);
        assert_eq!(change_named("user twelve"), None);
        assert_eq!(change_named(""), None);
    }

    #[test]
    fn a_bare_windows_editor_is_found_on_path_with_its_extension() {
        let on_disk = [
            r"C:\Program Files\Microsoft VS Code\bin\code.cmd",
            r"C:\Tools\code.exe",
            r"C:\Windows\System32\notepad.exe",
        ];
        let exists = |path: &Path| on_disk.iter().any(|file| Path::new(file) == path);
        let path = r#"C:\Windows\System32;"C:\Program Files\Microsoft VS Code\bin\";;C:\Tools"#;
        let found = |program: &str, pathext: &str| {
            resolve(program, path, pathext, exists).map(|path| path.display().to_string())
        };
        // The first folder on PATH that has it wins, as in cmd.
        let code = r"C:\Program Files\Microsoft VS Code\bin\code.cmd";
        assert_eq!(found("code", ".COM;.EXE;.BAT;.CMD").as_deref(), Some(code));
        assert_eq!(found("code", "").as_deref(), Some(code), "PATHEXT unset");
        assert_eq!(
            found("notepad", ".EXE").as_deref(),
            Some(r"C:\Windows\System32\notepad.exe")
        );
        assert_eq!(found("code", ".EXE").as_deref(), Some(r"C:\Tools\code.exe"));
        assert_eq!(found("vim", ".EXE;.CMD"), None);
        // A path or an extension is used as it was given.
        assert_eq!(found(code, ""), None);
        assert_eq!(found(r"C:\Tools\code", ""), None);
        assert_eq!(found("code.cmd", ""), None);
        assert_eq!(found("bin/code", ""), None);
        assert_eq!(found("", ""), None);
        let (program, _) = words(r#""C:\Program Files\Microsoft VS Code\bin\code.cmd" --wait"#);
        assert_eq!(found(&program, ""), None);
    }
}
