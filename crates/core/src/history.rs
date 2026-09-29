//! What the file tools wrote, kept so a rewind can put the files back.
//!
//! Just before write_file or edit_file replaces a file, `write::atomic` hands
//! its old bytes to the turn's [`Log`], which keeps them once per content,
//! named by their SHA-256, in a folder of the session's own:
//! `file-history/<session id>/` in the data folder, removed with the session.
//! The hashes before and after each write ride on the call's result message
//! ([`FileChange`] in `Message::files`) rather than in a record of their own,
//! so this history is cut back, copied and replayed with the session itself.
//!
//! Going back to a turn looks at the messages it drops: each file they wrote
//! goes back to the oldest `before` among them, or away if a tool created it.
//! A file whose hash is no longer the last `after` was changed since -- by
//! the user or a command -- and is left alone rather than overwritten. Only
//! the file tools are covered: what a command changes is never seen here.
//!
//! Files are noted by their real path, so one file under two spellings is one
//! file. A rewind touches only what the tools themselves could write -- no
//! link, nothing whose folder is really outside the project -- and keeps the
//! version it replaces in the same folder first, so it can be undone by hand.

use crate::chat::Message;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// The largest file whose old bytes are kept. Source files are well under a
/// megabyte; ten covers a big generated one, a lock file or a bundle, without
/// one data file taking most of the session's allowance.
pub const FILE_CAP: u64 = 10 * 1024 * 1024;

/// What one session's kept copies may add up to. A hundred megabytes is
/// dozens of turns rewriting large files; past it the session carries on,
/// and a rewind names the files it has no copy of.
pub const SESSION_CAP: u64 = 100 * 1024 * 1024;

/// One file a tool call wrote, by its content hash before and after.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: PathBuf,
    /// What was there, or `None` when the call created the file.
    #[serde(default)]
    pub before: Option<String>,
    pub after: String,
}

/// Where a turn keeps old bytes, and what its calls have written since their
/// results were last taken. The default keeps and notes nothing.
#[derive(Clone, Default)]
pub struct Log {
    dir: Option<PathBuf>,
    changes: Arc<Mutex<Vec<FileChange>>>,
}

impl Log {
    /// The log for a turn of session `session`; session 0 is none, and keeps
    /// nothing.
    pub fn new(session: u64) -> Self {
        Self {
            dir: (session != 0).then(|| folder(session)),
            ..Self::default()
        }
    }

    /// The files written since the last take, for the result of the call
    /// that wrote them.
    pub fn take(&self) -> Vec<FileChange> {
        self.changes
            .lock()
            .map(|mut changes| std::mem::take(&mut *changes))
            .unwrap_or_default()
    }

    /// Keep what `path` holds before a write replaces it, and say what that
    /// was: `Some(None)` for no file, `None` when nothing is to be noted --
    /// this log keeps nothing, or the old file cannot be read.
    pub(crate) fn keep(&self, path: &Path, exists: bool) -> Option<Option<String>> {
        let dir = self.dir.as_ref()?;
        if !exists {
            return Some(None);
        }
        let bytes = std::fs::read(path).ok()?;
        let hash = hash(&bytes);
        // A copy that cannot be made costs only this file's rewind, which
        // then says there is no copy; the write itself goes ahead.
        let _ = store(dir, &hash, &bytes);
        Some(Some(hash))
    }

    /// Note that `path` now holds `bytes`, where `keep` found `before`, by
    /// its real path: on macOS and Windows that is also the case on disk.
    pub(crate) fn note(&self, path: &Path, before: Option<Option<String>>, bytes: &[u8]) {
        let Some(before) = before else {
            return;
        };
        let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
        if let Ok(mut changes) = self.changes.lock() {
            changes.push(FileChange {
                path,
                before,
                after: hash(bytes),
            });
        }
    }
}

/// Where session `session` keeps its copies.
pub fn folder(session: u64) -> PathBuf {
    crate::chat::data_dir()
        .join("file-history")
        .join(session.to_string())
}

/// Remove session `session`'s copies, as the session itself goes.
pub fn forget(session: u64) {
    let _ = std::fs::remove_dir_all(folder(session));
}

/// A copy is written in well under a second, so a part-written one an hour
/// old was left by a crash, not by a write still going in another window.
const PART_AGE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// Clear what a crash or a deleted session left behind: the folders of
/// sessions that are `gone`, and copies never finished. A session is saved
/// before its first turn writes anything, so a live one is never taken.
pub fn sweep(gone: impl Fn(u64) -> bool) {
    sweep_in(&crate::chat::data_dir().join("file-history"), gone);
}

fn sweep_in(root: &Path, gone: impl Fn(u64) -> bool) {
    let Ok(folders) = std::fs::read_dir(root) else {
        return;
    };
    for folder in folders.flatten() {
        let id = folder
            .file_name()
            .to_str()
            .and_then(|name| name.parse().ok());
        match id {
            Some(id) if gone(id) => {
                let _ = std::fs::remove_dir_all(folder.path());
            }
            Some(_) => remove_parts(&folder.path()),
            None => {}
        }
    }
}

/// Remove the copies in `dir` that were started long ago and never finished.
fn remove_parts(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let part = entry.file_name().to_string_lossy().ends_with(".part");
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|at| at.elapsed().ok())
            .is_some_and(|age| age > PART_AGE);
        if part && stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// Keep `bytes` in `dir` under `hash`, unless a copy is there already or it
/// would go over a cap.
fn store(dir: &Path, hash: &str, bytes: &[u8]) -> std::io::Result<()> {
    let blob = dir.join(hash);
    let size = bytes.len() as u64;
    if blob.exists() || size > FILE_CAP {
        return Ok(());
    }
    make_folder(dir)?;
    if used(dir) + size > SESSION_CAP {
        return Ok(());
    }
    // Written aside and renamed, so a copy is never seen half written: its
    // name is the promise of what is in it. Owner-only, as the file it is a
    // copy of may hold a secret.
    let temp = dir.join(format!("{hash}.{}.part", std::process::id()));
    crate::store::write_private(&temp, bytes)?;
    std::fs::rename(&temp, &blob)
}

/// Make `dir`, and whatever above it is missing, owner-only where the
/// platform has modes; the user's own AppData is private on Windows already.
fn make_folder(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

/// What the copies in `dir` add up to.
fn used(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum()
}

/// The SHA-256 of `bytes`, as lowercase hex.
fn hash(bytes: &[u8]) -> String {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// What a rewind would do to one file.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Fate {
    /// It goes back to what it was.
    Back,
    /// A tool created it, so it goes.
    Remove,
    /// It changed after Letronna last wrote it, and is left alone.
    Changed,
    /// Its old bytes were not kept: it was over a cap, or the copy is gone.
    NoCopy,
    /// It is a link, or its folder is really outside the project folder:
    /// the file tools would not write there either.
    Outside,
}

impl Fate {
    /// Whether a rewind writes or removes a file of this fate; the others it
    /// leaves alone.
    pub fn touches(self) -> bool {
        matches!(self, Fate::Back | Fate::Remove)
    }

    /// What happens to a file of this fate, said after its name.
    pub fn says(self) -> &'static str {
        match self {
            Fate::Back => "goes back to how it was",
            Fate::Remove => "is removed, as a tool made it",
            Fate::Changed => "changed since, so it is left alone",
            Fate::NoCopy => "has no kept copy, so it is left alone",
            Fate::Outside => "is a link or outside the project folder, so it is left alone",
        }
    }
}

/// Said beside the files when the turns a rewind drops ran commands.
pub const COMMANDS_NOTE: &str = "Commands it ran are not undone: what they changed stays.";

/// Whether any of `dropped` ran a command, whose changes are never kept.
pub fn ran_commands(dropped: &[Message]) -> bool {
    dropped
        .iter()
        .any(|m| m.role == "tool" && m.name == "run_command")
}

/// The files the tools wrote in `messages` that are inside `workspace`, each
/// once, relative to it and in the order first written: what a session
/// changed, for git to be asked about. One outside is not the project's.
pub fn written(messages: &[Message], workspace: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = Vec::new();
    // Found once, not per change as `label` would: the window lists these
    // on every frame, and a long session has written hundreds of times.
    let real = real(Some(workspace));
    let roots = [Some(workspace), real.as_deref()];
    let changes = messages
        .iter()
        .filter(|m| m.role == "tool")
        .flat_map(|m| &m.files);
    for change in changes {
        let file = roots
            .iter()
            .flatten()
            .find_map(|root| change.path.strip_prefix(root).ok())
            .unwrap_or(&change.path)
            .to_path_buf();
        if file.is_relative() && !files.contains(&file) {
            files.push(file);
        }
    }
    files
}

/// One file a rewind would touch.
#[derive(Clone, Debug)]
pub struct Restore {
    pub path: PathBuf,
    pub fate: Fate,
    /// The hash it goes back to, or `None` to remove it.
    to: Option<String>,
    /// The hash Letronna last left it with.
    last: String,
}

/// What going back past `dropped`, the messages a rewind of session
/// `session` in `workspace` takes out, would do to the files written in
/// them. A file that is already as it was is left out.
pub fn plan(session: u64, dropped: &[Message], workspace: Option<&Path>) -> Vec<Restore> {
    plan_in(&folder(session), dropped, workspace)
}

fn plan_in(dir: &Path, dropped: &[Message], workspace: Option<&Path>) -> Vec<Restore> {
    let root = real(workspace);
    let mut files: Vec<Restore> = Vec::new();
    let changes = dropped
        .iter()
        .filter(|m| m.role == "tool")
        .flat_map(|m| &m.files);
    for change in changes {
        match files.iter_mut().find(|file| file.path == change.path) {
            Some(file) => {
                // A write that did not start from what the one before left
                // means someone changed the file between the two, and going
                // back past both would take their change with it.
                if change.before.as_deref() != Some(file.last.as_str()) {
                    file.fate = Fate::Changed;
                }
                file.last.clone_from(&change.after);
            }
            None => files.push(Restore {
                path: change.path.clone(),
                fate: Fate::Back,
                to: change.before.clone(),
                last: change.after.clone(),
            }),
        }
    }
    files.retain_mut(|file| {
        let now = current(&file.path);
        if now == file.to {
            return false;
        }
        file.fate = match &file.to {
            _ if !reachable(&file.path, root.as_deref()) => Fate::Outside,
            _ if file.fate == Fate::Changed => Fate::Changed,
            _ if now.as_deref() != Some(file.last.as_str()) => Fate::Changed,
            None => Fate::Remove,
            Some(hash) if dir.join(hash).is_file() => Fate::Back,
            Some(_) => Fate::NoCopy,
        };
        true
    });
    files
}

/// Files a rewind's question names one by one; past this it says how many
/// more, so a turn that rewrote a whole tree still fits on the screen.
const LISTED: usize = 10;

/// The lines a rewind's question shows: each file and what will become of
/// it, relative to `workspace`.
pub fn listing(plan: &[Restore], workspace: Option<&Path>) -> Vec<String> {
    let mut lines: Vec<String> = plan
        .iter()
        .take(LISTED)
        .map(|file| format!("{} {}", label(&file.path, workspace), file.fate.says()))
        .collect();
    if plan.len() > LISTED {
        lines.push(format!("and {} more", plan.len() - LISTED));
    }
    lines
}

/// How putting the files back went.
#[derive(Debug, Default)]
pub struct Restored {
    /// Files written back or removed.
    pub back: usize,
    /// A line for each file left alone, saying why.
    pub left: Vec<String>,
    /// Where the versions a restore replaced or removed were kept.
    pub kept: PathBuf,
}

impl Restored {
    /// What to tell the user: how many files went back and where the agent's
    /// versions of them are, then each one left alone.
    pub fn lines(&self) -> Vec<String> {
        let kept = self.kept.display();
        let back = match self.back {
            0 => None,
            1 => Some(format!(
                "Put back 1 file; the agent's version of it is kept in {kept}."
            )),
            n => Some(format!(
                "Put back {n} files; the agent's versions are kept in {kept}."
            )),
        };
        back.into_iter().chain(self.left.iter().cloned()).collect()
    }
}

/// Put back the files of `plan` that can be, checking each again first: the
/// question may have been up a while.
pub fn restore(session: u64, plan: &[Restore], workspace: Option<&Path>) -> Restored {
    restore_in(&folder(session), plan, workspace)
}

fn restore_in(dir: &Path, plan: &[Restore], workspace: Option<&Path>) -> Restored {
    let root = real(workspace);
    let mut done = Restored {
        kept: dir.to_owned(),
        ..Restored::default()
    };
    for file in plan {
        let name = label(&file.path, workspace);
        if !file.fate.touches() {
            done.left.push(format!("{name} {}.", file.fate.says()));
            continue;
        }
        match put_back(dir, file, root.as_deref()) {
            Ok(()) => done.back += 1,
            Err(why) => done.left.push(format!("{name}: {why}.")),
        }
    }
    done
}

/// Put back one file, `root` being the real project folder. What is there
/// now is checked and kept just before; a save landing between that and the
/// write is the one change this can still overwrite.
fn put_back(dir: &Path, file: &Restore, root: Option<&Path>) -> Result<(), String> {
    if !reachable(&file.path, root) {
        return Err("it is a link or outside the project folder, so it was left alone".into());
    }
    let old = file.to.as_deref().map(|to| kept(dir, to)).transpose()?;
    let now = std::fs::read(&file.path)
        .ok()
        .map(|bytes| (hash(&bytes), bytes));
    let Some((last, now)) = now.filter(|(last, _)| *last == file.last) else {
        return Err("it changed in the meantime, so it was left alone".into());
    };
    // Letronna's version goes into the same folder before it is replaced,
    // so the rewind can itself be undone by hand.
    let _ = store(dir, &last, &now);
    if !dir.join(&last).is_file() {
        return Err("its current version could not be kept, so it was left alone".into());
    }
    let Some(old) = old else {
        return std::fs::remove_file(&file.path).map_err(|e| format!("could not remove it: {e}"));
    };
    // Written the way the tools write, but noted nowhere: a rewind is not a
    // turn, and there is nothing after it to go back past.
    crate::write::atomic(&file.path, &old, &Log::default())
}

/// The bytes kept in `dir` as `name`, once they are sure to be what the name
/// says.
fn kept(dir: &Path, name: &str) -> Result<Vec<u8>, String> {
    let bytes = std::fs::read(dir.join(name)).map_err(|_| "no copy of it was kept".to_owned())?;
    if hash(&bytes) != name {
        return Err("its kept copy is damaged, so it was left alone".into());
    }
    Ok(bytes)
}

/// The hash of what is at `path` now, or `None` when nothing readable is.
fn current(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| hash(&bytes))
}

/// The real path of the project folder, or `None` when there is none.
fn real(workspace: Option<&Path>) -> Option<PathBuf> {
    workspace.and_then(|root| std::fs::canonicalize(root).ok())
}

/// Whether a rewind may write or remove `path`: where the file tools could,
/// so not a link and in a folder that is really inside `root`, the real
/// project folder. A session line naming anywhere else is not followed.
fn reachable(path: &Path, root: Option<&Path>) -> bool {
    let Some(root) = root else {
        return false;
    };
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    path.parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .is_some_and(|parent| parent.starts_with(root))
}

/// `path` as a rewind lists it: inside the project folder, relative to it.
/// Paths are noted by their real path, so the folder's real path is tried
/// too: on macOS a temp folder is under /private, on Windows it gains `\\?\`.
pub fn label(path: &Path, workspace: Option<&Path>) -> String {
    let real = real(workspace);
    [workspace, real.as_deref()]
        .into_iter()
        .flatten()
        .find_map(|root| path.strip_prefix(root).ok())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Give session `to` the copies session `from` kept, for a fork or branch
/// whose messages it took. Linked where the disk allows, so they cost
/// nothing more; copied where it does not.
pub fn share(from: u64, to: u64) {
    share_between(&folder(from), &folder(to));
}

fn share_between(from: &Path, to: &Path) {
    let Ok(entries) = std::fs::read_dir(from) else {
        return;
    };
    if make_folder(to).is_err() {
        return;
    }
    for entry in entries.flatten() {
        let target = to.join(entry.file_name());
        if std::fs::hard_link(entry.path(), &target).is_err() {
            let _ = std::fs::copy(entry.path(), &target);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::{ToolContext, run};

    /// A project folder and a history folder of their own for one test.
    fn scratch(name: &str) -> (PathBuf, PathBuf) {
        let root =
            std::env::temp_dir().join(format!("letronna-history-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let (project, kept) = (root.join("project"), root.join("kept"));
        std::fs::create_dir_all(&project).unwrap();
        (project, kept)
    }

    /// Run a file tool as a turn does, and give back its result message.
    fn call(ctx: &ToolContext, name: &str, args: serde_json::Value) -> Message {
        run(name, &args.to_string(), ctx).unwrap();
        Message {
            role: "tool".into(),
            name: name.into(),
            files: ctx.files.take(),
            ..Default::default()
        }
    }

    fn context(project: &Path, kept: &Path) -> ToolContext {
        ToolContext {
            workspace: Some(project.to_owned()),
            files: Log {
                dir: Some(kept.to_owned()),
                ..Log::default()
            },
            ..Default::default()
        }
    }

    fn fates(plan: &[Restore], project: &Path) -> Vec<(String, Fate)> {
        plan.iter()
            .map(|file| (label(&file.path, Some(project)), file.fate))
            .collect()
    }

    #[test]
    fn a_rewind_puts_back_what_the_tools_changed_and_removes_what_they_made() {
        let (project, kept) = scratch("back");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        let ctx = context(&project, &kept);
        let turns = [
            call(
                &ctx,
                "edit_file",
                serde_json::json!({"path": "a.txt", "old": "one", "new": "two"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "b.txt", "content": "new"}),
            ),
            call(
                &ctx,
                "edit_file",
                serde_json::json!({"path": "a.txt", "old": "two", "new": "three"}),
            ),
        ];
        assert_eq!(turns[0].files[0].before, Some(hash(b"one\n")));
        assert_eq!(turns[1].files[0].before, None);

        let plan = plan_in(&kept, &turns, Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [
                ("a.txt".to_owned(), Fate::Back),
                ("b.txt".to_owned(), Fate::Remove)
            ]
        );
        let done = restore_in(&kept, &plan, Some(&project));
        assert_eq!(done.back, 2);
        assert!(done.left.is_empty(), "{done:?}");
        // What the rewind replaced is kept as well, and the user told where.
        assert!(kept.join(hash(b"three\n")).is_file());
        assert!(kept.join(hash(b"new")).is_file());
        let said = done.lines();
        assert!(
            said[0].ends_with(&format!("kept in {}.", kept.display())),
            "{said:?}"
        );
        // Back to before the oldest dropped write, not the latest.
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "one\n"
        );
        assert!(!project.join("b.txt").exists());
        // The restore is written like any other write, but noted nowhere.
        assert!(ctx.files.take().is_empty());

        // Going back only past the last turn stops at what the second left.
        std::fs::write(project.join("a.txt"), "three\n").unwrap();
        let plan = plan_in(&kept, &turns[2..], Some(&project));
        restore_in(&kept, &plan, Some(&project));
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "two\n"
        );
    }

    #[test]
    fn a_file_changed_since_is_named_and_left_alone() {
        let (project, kept) = scratch("changed");
        std::fs::write(project.join("a.txt"), "one").unwrap();
        std::fs::write(project.join("b.txt"), "one").unwrap();
        let ctx = context(&project, &kept);
        let turns = [
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "a.txt", "content": "two"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "b.txt", "content": "two"}),
            ),
        ];
        // The user, or a command, edits one after the tool did.
        std::fs::write(project.join("a.txt"), "mine").unwrap();
        let plan = plan_in(&kept, &turns, Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [
                ("a.txt".to_owned(), Fate::Changed),
                ("b.txt".to_owned(), Fate::Back)
            ]
        );
        // And the other changes while the question is up.
        std::fs::write(project.join("b.txt"), "also mine").unwrap();
        let done = restore_in(&kept, &plan, Some(&project));
        assert_eq!(done.back, 0);
        assert_eq!(
            done.left,
            [
                "a.txt changed since, so it is left alone.",
                "b.txt: it changed in the meantime, so it was left alone."
            ]
        );
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "mine"
        );
        assert_eq!(
            std::fs::read_to_string(project.join("b.txt")).unwrap(),
            "also mine"
        );
    }

    #[test]
    fn a_file_already_as_it_was_is_left_out() {
        let (project, kept) = scratch("same");
        std::fs::write(project.join("a.txt"), "one").unwrap();
        let ctx = context(&project, &kept);
        let turns = [
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "a.txt", "content": "two"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "a.txt", "content": "one"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "c.txt", "content": "x"}),
            ),
        ];
        std::fs::remove_file(project.join("c.txt")).unwrap();
        assert!(plan_in(&kept, &turns, Some(&project)).is_empty());
    }

    #[test]
    fn copies_stop_at_the_caps_and_the_rewind_says_so() {
        let (project, kept) = scratch("caps");
        let big = vec![b'x'; FILE_CAP as usize + 1];
        std::fs::write(project.join("big.txt"), &big).unwrap();
        let ctx = context(&project, &kept);
        let turn = call(
            &ctx,
            "write_file",
            serde_json::json!({"path": "big.txt", "content": "small"}),
        );
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [("big.txt".to_owned(), Fate::NoCopy)]
        );
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 0);
        assert_eq!(
            std::fs::read_to_string(project.join("big.txt")).unwrap(),
            "small"
        );

        // A session whose copies fill its allowance keeps no more. The
        // filler is sparse: it counts its full length but takes no disk.
        std::fs::create_dir_all(&kept).unwrap();
        let filler = std::fs::File::create(kept.join("filler")).unwrap();
        filler.set_len(SESSION_CAP).unwrap();
        store(&kept, &hash(b"more"), b"more").unwrap();
        assert!(!kept.join(hash(b"more")).exists());
    }

    #[test]
    fn nothing_is_kept_or_noted_without_a_session() {
        let (project, _) = scratch("none");
        let ctx = ToolContext {
            workspace: Some(project.clone()),
            ..Default::default()
        };
        run("write_file", r#"{"path": "a.txt", "content": "x"}"#, &ctx).unwrap();
        assert!(ctx.files.take().is_empty());
    }

    #[test]
    fn a_result_that_wrote_nothing_saves_no_files_field() {
        let line = serde_json::to_string(&Message::default()).unwrap();
        assert!(!line.contains("files"), "{line}");
    }

    #[test]
    fn files_are_listed_relative_to_the_project_folder() {
        let project = Path::new("/work/site");
        assert_eq!(
            label(&project.join("src").join("a.rs"), Some(project)),
            Path::new("src").join("a.rs").display().to_string()
        );
        assert_eq!(
            label(Path::new("/elsewhere/b.rs"), Some(project)),
            "/elsewhere/b.rs"
        );
    }

    #[test]
    fn a_long_list_names_ten_files_and_counts_the_rest() {
        let plan: Vec<Restore> = (0..12)
            .map(|i| Restore {
                path: PathBuf::from(format!("f{i}.txt")),
                fate: Fate::Back,
                to: None,
                last: String::new(),
            })
            .collect();
        let lines = listing(&plan, None);
        assert_eq!(lines.len(), 11);
        assert_eq!(lines[0], "f0.txt goes back to how it was");
        assert_eq!(lines[10], "and 2 more");
    }

    #[test]
    fn a_fork_shares_the_copies_it_was_given() {
        let (_, kept) = scratch("share");
        let (from, to) = (kept.join("1"), kept.join("2"));
        std::fs::create_dir_all(&from).unwrap();
        std::fs::write(from.join(hash(b"old")), "old").unwrap();
        share_between(&from, &to);
        assert_eq!(
            std::fs::read_to_string(to.join(hash(b"old"))).unwrap(),
            "old"
        );
        // A session with nothing kept gives nothing, and makes no folder.
        share_between(&kept.join("3"), &kept.join("4"));
        assert!(!kept.join("4").exists());
    }

    #[test]
    fn a_change_made_between_two_writes_is_not_undone_with_them() {
        let (project, kept) = scratch("between");
        std::fs::write(project.join("a.txt"), "one").unwrap();
        let ctx = context(&project, &kept);
        let first = call(
            &ctx,
            "write_file",
            serde_json::json!({"path": "a.txt", "content": "two"}),
        );
        std::fs::write(project.join("a.txt"), "mine").unwrap();
        let second = call(
            &ctx,
            "write_file",
            serde_json::json!({"path": "a.txt", "content": "three"}),
        );
        let plan = plan_in(&kept, &[first, second.clone()], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [("a.txt".to_owned(), Fate::Changed)]
        );
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 0);
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "three"
        );
        // Going back past the second write alone gives the change back.
        let plan = plan_in(&kept, &[second], Some(&project));
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 1);
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "mine"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_became_a_link_or_moved_out_is_left_alone() {
        let (project, kept) = scratch("links");
        let outside = project.parent().unwrap().join("outside");
        std::fs::create_dir_all(project.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        for name in ["d.txt", "sub/e.txt", "f.txt"] {
            std::fs::write(project.join(name), "orig").unwrap();
        }
        let ctx = context(&project, &kept);
        let turns: Vec<Message> = ["d.txt", "sub/e.txt", "f.txt"]
            .into_iter()
            .map(|name| {
                let args = serde_json::json!({"path": name, "content": "new"});
                call(&ctx, "write_file", args)
            })
            .collect();
        let asked = plan_in(&kept, &turns, Some(&project));
        assert!(asked.iter().all(|file| file.fate == Fate::Back));

        // While the question is up, a file and another's folder move out of
        // the project and are linked back, and a third becomes a link to a
        // copy of itself still inside.
        let link = |to: &Path, at: &str| std::os::unix::fs::symlink(to, project.join(at)).unwrap();
        std::fs::rename(project.join("d.txt"), outside.join("d.txt")).unwrap();
        link(&outside.join("d.txt"), "d.txt");
        std::fs::rename(project.join("sub"), outside.join("sub")).unwrap();
        link(&outside.join("sub"), "sub");
        std::fs::rename(project.join("f.txt"), project.join("g.txt")).unwrap();
        link(Path::new("g.txt"), "f.txt");

        let done = restore_in(&kept, &asked, Some(&project));
        assert_eq!(done.back, 0);
        assert!(
            done.left[0]
                .ends_with("is a link or outside the project folder, so it was left alone.")
        );
        let plan = plan_in(&kept, &turns, Some(&project));
        assert_eq!(plan.len(), 3);
        assert!(
            plan.iter().all(|file| file.fate == Fate::Outside),
            "{plan:?}"
        );
        restore_in(&kept, &plan, Some(&project));
        for moved in [
            outside.join("d.txt"),
            outside.join("sub/e.txt"),
            project.join("g.txt"),
        ] {
            assert_eq!(std::fs::read_to_string(moved).unwrap(), "new");
        }
    }

    #[test]
    fn a_session_line_naming_a_file_elsewhere_touches_nothing() {
        let (project, kept) = scratch("forged");
        let victim = project.parent().unwrap().join("victim.txt");
        std::fs::write(&victim, "precious\n").unwrap();
        let forged = Message {
            role: "tool".into(),
            files: vec![FileChange {
                path: victim.clone(),
                before: None,
                after: hash(b"precious\n"),
            }],
            ..Default::default()
        };
        let plan = plan_in(&kept, &[forged], Some(&project));
        assert_eq!(plan[0].fate, Fate::Outside);
        // Nor when the plan itself says remove, or there is no project.
        let made_up = [Restore {
            path: victim.clone(),
            fate: Fate::Remove,
            to: None,
            last: hash(b"precious\n"),
        }];
        assert_eq!(restore_in(&kept, &made_up, Some(&project)).back, 0);
        assert_eq!(restore_in(&kept, &made_up, None).back, 0);
        assert!(victim.exists());
    }

    #[test]
    fn one_file_under_two_spellings_is_one_file() {
        let (project, kept) = scratch("alias");
        std::fs::write(project.join("a.txt"), "x\n").unwrap();
        let ctx = context(&project, &kept);
        let write = |path: &str, content: &str| {
            let args = serde_json::json!({"path": path, "content": content});
            call(&ctx, "write_file", args)
        };
        let mut turns = vec![write("a.txt", "A\n")];
        // Through a link to it inside the project, which the tools allow.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("a.txt", project.join("link.txt")).unwrap();
            turns.push(write("link.txt", "B\n"));
        }
        // Where the disk ignores case, as on macOS and Windows, so does this.
        if project.join("A.TXT").exists() {
            turns.push(write("A.TXT", "C\n"));
        }
        let plan = plan_in(&kept, &turns, Some(&project));
        assert_eq!(fates(&plan, &project), [("a.txt".to_owned(), Fate::Back)]);
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 1);
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "x\n"
        );
    }

    #[cfg(unix)]
    #[test]
    fn copies_are_for_their_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (project, kept) = scratch("mode");
        std::fs::write(project.join(".env"), "KEY=one\n").unwrap();
        let ctx = context(&project, &kept);
        let args = serde_json::json!({"path": ".env", "content": "KEY=two\n"});
        call(&ctx, "write_file", args);
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&kept), 0o700);
        assert_eq!(mode(&kept.join(hash(b"KEY=one\n"))), 0o600);
    }

    #[test]
    fn a_sweep_clears_gone_sessions_and_unfinished_copies() {
        let (_, root) = scratch("sweep");
        for id in ["1", "2", "notes"] {
            std::fs::create_dir_all(root.join(id)).unwrap();
        }
        std::fs::write(root.join("1").join(hash(b"old")), "old").unwrap();
        let (fresh, stale) = (root.join("1/a.7.part"), root.join("1/b.7.part"));
        std::fs::write(&fresh, "half").unwrap();
        std::fs::write(&stale, "half").unwrap();
        let long_ago = std::time::SystemTime::now() - PART_AGE * 2;
        let file = std::fs::File::options().write(true).open(&stale).unwrap();
        file.set_modified(long_ago).unwrap();

        sweep_in(&root, |id| id != 1);
        assert!(root.join("1").join(hash(b"old")).exists());
        // One being written in another window right now stays.
        assert!(fresh.exists());
        assert!(!stale.exists());
        assert!(!root.join("2").exists());
        assert!(root.join("notes").exists());
    }

    #[test]
    fn a_session_s_files_are_listed_once_and_only_inside_the_project() {
        let (project, kept) = scratch("written");
        let ctx = context(&project, &kept);
        let one = call(
            &ctx,
            "write_file",
            serde_json::json!({"path": "a.txt", "content": "1"}),
        );
        let two = call(
            &ctx,
            "write_file",
            serde_json::json!({"path": "b.txt", "content": "2"}),
        );
        let again = call(
            &ctx,
            "write_file",
            serde_json::json!({"path": "a.txt", "content": "3"}),
        );
        let outside = Message {
            role: "tool".into(),
            files: vec![FileChange {
                path: std::env::temp_dir().join("elsewhere.txt"),
                ..FileChange::default()
            }],
            ..Default::default()
        };
        let files = written(&[one, two, again, outside], &project);
        assert_eq!(files, vec![PathBuf::from("a.txt"), PathBuf::from("b.txt")]);
    }

    #[cfg(unix)]
    #[test]
    fn a_session_s_files_are_found_through_a_linked_project_folder() {
        let (project, _) = scratch("written-link");
        let link = project.with_file_name("link");
        std::os::unix::fs::symlink(&project, &link).unwrap();
        let real = std::fs::canonicalize(&project).unwrap();
        let wrote = Message {
            role: "tool".into(),
            files: vec![FileChange {
                path: real.join("src").join("a.txt"),
                ..FileChange::default()
            }],
            ..Default::default()
        };
        let files = written(&[wrote], &link);
        assert_eq!(files, vec![Path::new("src").join("a.txt")]);
    }
}
