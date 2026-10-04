//! What the file tools wrote, kept so a rewind can put the files back.
//!
//! Just before write_file or edit_file replaces a file, `write::atomic` hands
//! its old bytes to the turn's [`Log`], which keeps them once per content,
//! named by their SHA-256, in a folder of the session's own:
//! `file-history/<session id>/` in the data folder, removed with the session.
//! The hashes before and after each write ride on the call's result message
//! ([`FileChange`] in `Message::files`) rather than in a record of their own,
//! so this history is cut back, copied and replayed with the session itself.
//! A file apply_patch deletes goes through `write::remove`, which keeps its
//! bytes the same way and notes an empty hash after, so it comes back too.
//!
//! What a foreground command changes in the project is noted the same way,
//! on its own result, by `snapshot`, which also says on the result what it
//! could not keep; its copies are charged to an allowance of their own.
//!
//! Going back to a turn looks at the messages it drops: each file they wrote
//! goes back to the oldest `before` among them, or away if a tool created it.
//! A file whose hash is no longer the last `after` was changed since -- by
//! the user or a command -- and is left alone rather than overwritten.
//!
//! Files are noted by their real path, so one file under two spellings is one
//! file. A rewind touches only what the tools themselves could write -- no
//! link, nothing whose folder is really outside the project -- and keeps the
//! version it replaces in the same folder first, so it can be undone by hand.

use crate::chat::Message;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};

/// The largest file whose old bytes are kept. Source files are well under a
/// megabyte; ten covers a big generated one, a lock file or a bundle, without
/// one data file taking most of the session's allowance.
pub const FILE_CAP: u64 = 10 * 1024 * 1024;

/// What one session's kept copies may add up to. A hundred megabytes is
/// dozens of turns rewriting large files; past it the session carries on,
/// and a rewind names the files it has no copy of.
pub const SESSION_CAP: u64 = 100 * 1024 * 1024;

/// What the copies kept for commands may add up to in one session, beside
/// [`SESSION_CAP`], so a command that rewrites a whole tree never leaves
/// an edit with no copy. A hundred megabytes holds the first look at a
/// folder outside git, at most fifty, with as much again for what commands
/// change after it.
pub const COMMAND_CAP: u64 = 100 * 1024 * 1024;

/// One file a tool call wrote, by its content hash before and after.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FileChange {
    pub path: PathBuf,
    /// What was there, or `None` when the call created the file.
    #[serde(default)]
    pub before: Option<String>,
    /// What it holds after, or empty when the call removed it: no SHA-256
    /// is empty, and a build that predates removals takes the empty hash
    /// for a file changed since and leaves it alone.
    pub after: String,
    /// The mode it had, where a command changed that or removed the file,
    /// for a rewind to set again: its permission bits, or for a file that
    /// matched the commit, git's mode for it, 0o100644 or 0o100755, which
    /// a rewind applies as a checkout does, under the umask. Left out, the
    /// file keeps the mode it has, as for what the file tools write.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<u32>,
}

/// Where a turn keeps old bytes, and what its calls have written since their
/// results were last taken. The default keeps and notes nothing.
#[derive(Clone, Default)]
pub struct Log {
    dir: Option<PathBuf>,
    changes: Arc<Mutex<Vec<FileChange>>>,
    /// What of the last command's changes was not kept, until taken.
    unkept: Arc<Mutex<Option<String>>>,
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

    /// What of the last command's changes a rewind cannot put back, for the
    /// result of the call that ran it, as `Message::unkept` says.
    pub fn unkept(&self) -> Option<String> {
        self.unkept
            .lock()
            .map(|mut unkept| unkept.take())
            .unwrap_or_default()
    }

    /// Where this log keeps copies, or `None` when it keeps nothing.
    pub(crate) fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }

    /// Keep `bytes` a command may change, from the commands' allowance, and
    /// give their hash, which is noted whether or not there was room.
    pub(crate) fn keep_bytes(&self, bytes: &[u8]) -> String {
        let hash = hash(bytes);
        if let Some(dir) = &self.dir {
            // As for a file tool: a copy that cannot be made costs only
            // this file's rewind, which then says there is no copy.
            let _ = store(dir, &hash, bytes, Pot::Commands);
        }
        hash
    }

    /// Note what a command changed, and what of its changes a rewind cannot
    /// put back: "" for nothing.
    pub(crate) fn command(&self, changes: Vec<FileChange>, unkept: String) {
        if let Ok(mut noted) = self.changes.lock() {
            noted.extend(changes);
        }
        if let Ok(mut noted) = self.unkept.lock() {
            *noted = Some(unkept);
        }
        if let Some(dir) = &self.dir {
            settle(dir);
        }
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
        let _ = store(dir, &hash, &bytes, Pot::Tools);
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
                mode: None,
            });
        }
    }

    /// Note that the file at `real`, its real path taken while it was still
    /// there, is gone, where `keep` found `before`.
    pub(crate) fn gone(&self, real: &Path, before: Option<Option<String>>) {
        let Some(before) = before else {
            return;
        };
        if let Ok(mut changes) = self.changes.lock() {
            changes.push(FileChange {
                path: real.to_owned(),
                before,
                after: String::new(),
                mode: None,
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
    let dir = folder(session);
    let _ = std::fs::remove_dir_all(&dir);
    lock_used().remove(&dir);
    crate::snapshot::forget(&dir);
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

/// Which allowance a copy is charged to.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Pot {
    /// What the file tools replace, and what a rewind replaces in turn.
    Tools,
    /// What a command may change, and what a rewind of that replaces.
    Commands,
}

/// What one session folder's copies add up to, by allowance.
#[derive(Clone, Copy, Default)]
struct Used {
    tools: u64,
    commands: u64,
}

/// Each session folder's [`Used`], counted from the folder the first time
/// a copy goes in and kept up from then on: listing a folder of thousands
/// of copies before each one would make a command's first look quadratic.
static USED: Mutex<BTreeMap<PathBuf, Used>> = Mutex::new(BTreeMap::new());

/// The file in a session folder that says how much of it was kept for
/// commands, which a listing of the folder cannot tell after a restart.
const LEDGER: &str = "commands.used";

fn lock_used() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, Used>> {
    // Nothing under this lock panics, so a poisoned one is still whole.
    USED.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What the copies in `dir` add up to, by allowance.
fn counted(dir: &Path) -> Used {
    let all: u64 = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| entry.metadata().ok())
        .map(|meta| meta.len())
        .sum();
    let commands = std::fs::read_to_string(dir.join(LEDGER))
        .ok()
        .and_then(|text| text.trim().parse::<u64>().ok())
        .unwrap_or(0)
        .min(all);
    Used {
        tools: all - commands,
        commands,
    }
}

/// Count `size` more bytes against `dir`'s `pot`, or say there is no room.
fn charge(dir: &Path, pot: Pot, size: u64) -> bool {
    let mut used = lock_used();
    let used = used.entry(dir.to_owned()).or_insert_with(|| counted(dir));
    let (now, cap) = match pot {
        Pot::Tools => (&mut used.tools, SESSION_CAP),
        Pot::Commands => (&mut used.commands, COMMAND_CAP),
    };
    if *now + size > cap {
        return false;
    }
    *now += size;
    true
}

/// Give back what a copy that could not be written was charged.
fn refund(dir: &Path, pot: Pot, size: u64) {
    if let Some(used) = lock_used().get_mut(dir) {
        let now = match pot {
            Pot::Tools => &mut used.tools,
            Pot::Commands => &mut used.commands,
        };
        *now = now.saturating_sub(size);
    }
}

/// Write down what `dir` has kept for commands, for the next start to count.
fn settle(dir: &Path) {
    let Some(commands) = lock_used().get(dir).map(|used| used.commands) else {
        return;
    };
    if commands == 0 && !dir.join(LEDGER).exists() {
        return;
    }
    let temp = dir.join(format!("{LEDGER}.{}.part", std::process::id()));
    if crate::store::write_private(&temp, commands.to_string().as_bytes()).is_ok() {
        let _ = std::fs::rename(&temp, dir.join(LEDGER));
    }
}

/// Keep `bytes` in `dir` under `hash`, charged to `pot`, unless a copy is
/// there already or it would go over a cap.
fn store(dir: &Path, hash: &str, bytes: &[u8], pot: Pot) -> std::io::Result<()> {
    let blob = dir.join(hash);
    let size = bytes.len() as u64;
    if blob.exists() || size > FILE_CAP {
        return Ok(());
    }
    make_folder(dir)?;
    if !charge(dir, pot, size) {
        return Ok(());
    }
    // Written aside and renamed, so a copy is never seen half written: its
    // name is the promise of what is in it. Owner-only, as the file it is a
    // copy of may hold a secret.
    let temp = dir.join(format!("{hash}.{}.part", std::process::id()));
    let written =
        crate::store::write_private(&temp, bytes).and_then(|()| std::fs::rename(&temp, &blob));
    if written.is_err() {
        refund(dir, pot, size);
    }
    written
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

/// The SHA-256 of `bytes`, as lowercase hex.
pub(crate) fn hash(bytes: &[u8]) -> String {
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

/// Said beside the files when the turns a rewind drops ran a command whose
/// changes were not all kept.
pub const COMMANDS_NOTE: &str = "Some commands it ran are not undone: what they changed stays.";

/// Said after the files when some of them were changed by a command, whose
/// changes are only looked for in the project's own files.
pub const COMMAND_FILES_NOTE: &str = "What commands did to git's own records, to submodules, \
     to files the project ignores or to anything outside it stays.";

/// Whether any of `dropped` ran a command whose changes were not all kept:
/// one left in the background, one that moved git HEAD or changed too
/// much, or one run before commands were watched.
pub fn ran_commands(dropped: &[Message]) -> bool {
    dropped
        .iter()
        .any(|m| m.role == "tool" && m.name == "run_command" && m.unkept.as_deref() != Some(""))
}

/// Characters of a command a line about it shows, enough to tell it from
/// the others a turn ran.
const COMMAND_CHARS: usize = 60;

/// A line for each command of `dropped` whose changes a rewind cannot all
/// put back, naming it and saying why.
pub fn unkept(dropped: &[Message]) -> Vec<String> {
    dropped
        .iter()
        .filter(|m| m.role == "tool" && m.name == "run_command")
        .filter_map(|m| {
            let args: serde_json::Value = serde_json::from_str(&m.args).unwrap_or_default();
            let why = match m.unkept.as_deref() {
                Some("") => return None,
                Some(why) => why,
                None if args["background"].as_bool() == Some(true) => {
                    "left running in the background, so not undone"
                }
                None => return None,
            };
            let line = args["command"].as_str()?.lines().next().unwrap_or_default();
            let mut shown: String = line.chars().take(COMMAND_CHARS).collect();
            if shown.len() < line.len() {
                shown.push('…');
            }
            Some(format!("`{shown}`: {why}."))
        })
        .collect()
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

/// A file the tools wrote in a session, and how the session found it.
struct Origin {
    /// As noted: its real path.
    path: PathBuf,
    /// Relative to the project folder.
    name: PathBuf,
    /// Its hash before the session's first write, or `None` when a tool
    /// created it.
    from: Option<String>,
}

/// The files the tools wrote in `messages` inside `workspace`, each once,
/// in the order first written.
fn origins(messages: &[Message], workspace: &Path) -> Vec<Origin> {
    let mut files: Vec<Origin> = Vec::new();
    let changes = messages
        .iter()
        .filter(|m| m.role == "tool")
        .flat_map(|m| &m.files);
    for change in changes {
        let name = PathBuf::from(label(&change.path, Some(workspace)));
        if name.is_relative() && !files.iter().any(|file| file.path == change.path) {
            files.push(Origin {
                path: change.path.clone(),
                name,
                from: change.before.clone(),
            });
        }
    }
    files
}

/// What is at `path` now: `None` when nothing is, an error for a link, a
/// device, a pipe or a file over [`FILE_CAP`]. A link could name a secret
/// outside the project, and a device or a pipe never ends.
fn read_now(path: &Path) -> Result<Option<Vec<u8>>, String> {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return Ok(None);
    };
    if !meta.is_file() {
        return Err("it is not a plain file".into());
    }
    if meta.len() > FILE_CAP {
        return Err("it is too big".into());
    }
    Ok(std::fs::read(path).ok())
}

/// A file as it is now against how the session found it: `None` when it
/// is back as it was, the patch when both are text, and an empty patch
/// marked binary when either is not.
fn compare(dir: &Path, file: &Origin) -> Result<Option<(crate::unified::Patch, bool)>, String> {
    let old = match &file.from {
        Some(hash) => kept(dir, hash).map_err(|why| {
            format!(
                "{}: {why}, so there is nothing to compare with.",
                file.name.display()
            )
        })?,
        None => Vec::new(),
    };
    let now = read_now(&file.path)
        .map_err(|why| format!("{}: {why}, so it is not compared.", file.name.display()))?;
    if now.as_deref() == Some(old.as_slice()) || (now.is_none() && file.from.is_none()) {
        return Ok(None);
    }
    let now = now.unwrap_or_default();
    let (Ok(old), Ok(new)) = (std::str::from_utf8(&old), std::str::from_utf8(&now)) else {
        return Ok(Some((crate::unified::Patch::default(), true)));
    };
    let name = file.name.display().to_string().replace('\\', "/");
    let before = match file.from {
        Some(_) => format!("a/{name}"),
        None => "/dev/null".to_owned(),
    };
    let after = match file.path.exists() {
        true => format!("b/{name}"),
        false => "/dev/null".to_owned(),
    };
    let mut patch = crate::unified::patch(old, new, &before, &after);
    patch.text = crate::git::printable(&patch.text);
    Ok(Some((patch, false)))
}

/// The files session `session`'s tools changed in `messages` inside
/// `workspace`, each counted from how the session found it to how it is
/// now. One back as it was is left out; one with no kept copy is listed
/// without counts. This needs no git, so it works in any folder.
pub fn session_files(
    session: u64,
    messages: &[Message],
    workspace: &Path,
) -> Vec<crate::git::ChangedFile> {
    session_files_in(&folder(session), messages, workspace)
}

fn session_files_in(
    dir: &Path,
    messages: &[Message],
    workspace: &Path,
) -> Vec<crate::git::ChangedFile> {
    origins(messages, workspace)
        .into_iter()
        .filter_map(|file| {
            let (patch, binary) = match compare(dir, &file) {
                Ok(None) => return None,
                Ok(Some(found)) => found,
                Err(_) => (crate::unified::Patch::default(), false),
            };
            Some(crate::git::ChangedFile {
                path: file.name,
                added: patch.added,
                removed: patch.removed,
                created: file.from.is_none(),
                binary,
            })
        })
        .collect()
}

/// `name`, one of [`session_files`], as a patch from how session `session`
/// found it to how it is now.
pub fn session_patch(
    session: u64,
    messages: &[Message],
    workspace: &Path,
    name: &Path,
) -> Result<String, String> {
    session_patch_in(&folder(session), messages, workspace, name)
}

fn session_patch_in(
    dir: &Path,
    messages: &[Message],
    workspace: &Path,
    name: &Path,
) -> Result<String, String> {
    let Some(file) = origins(messages, workspace)
        .into_iter()
        .find(|file| file.name == name)
    else {
        return Err(format!("This session did not change {}.", name.display()));
    };
    Ok(match compare(dir, &file)? {
        None => "It is back as the session found it.".to_owned(),
        Some((_, true)) => "It is not text, so there are no lines to show.".to_owned(),
        Some((patch, false)) => patch.text,
    })
}

/// What reverting `name` would do: the session's own writes to it undone,
/// back to how the session found it, as a rewind past all of them would.
/// Empty when it is already as it was.
pub fn revert_plan(
    session: u64,
    messages: &[Message],
    workspace: &Path,
    name: &Path,
) -> Vec<Restore> {
    plan_in(
        &folder(session),
        &writes_to(messages, workspace, name),
        Some(workspace),
    )
}

/// The tool results of `messages` cut down to their writes to `name`, all
/// a plan looks at: what wrote it, and what it wrote.
fn writes_to(messages: &[Message], workspace: &Path, name: &Path) -> Vec<Message> {
    messages
        .iter()
        .filter(|m| m.role == "tool")
        .flat_map(|m| m.files.iter().map(move |change| (m, change)))
        .filter(|(_, change)| Path::new(&label(&change.path, Some(workspace))) == name)
        .map(|(m, change)| Message {
            role: "tool".into(),
            name: m.name.clone(),
            files: vec![change.clone()],
            ..Default::default()
        })
        .collect()
}

/// One file a rewind would touch.
#[derive(Clone, Debug)]
pub struct Restore {
    pub path: PathBuf,
    pub fate: Fate,
    /// A command changed it, so it may be news to the user that it goes back.
    pub by_command: bool,
    /// The hash it goes back to, or `None` to remove it.
    to: Option<String>,
    /// The mode it goes back to, as [`FileChange::mode`] says it.
    mode: Option<u32>,
    /// The hash Letronna last left it with, empty when it removed it.
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
    let changes = dropped.iter().filter(|m| m.role == "tool").flat_map(|m| {
        let command = m.name == "run_command";
        m.files.iter().map(move |change| (change, command))
    });
    for (change, command) in changes {
        match files.iter_mut().find(|file| file.path == change.path) {
            Some(file) => {
                // A write that did not start from what the one before left
                // means someone changed the file between the two, and going
                // back past both would take their change with it.
                if change.before.as_deref() != present(&file.last) {
                    file.fate = Fate::Changed;
                }
                file.last.clone_from(&change.after);
                file.by_command |= command;
            }
            None => files.push(Restore {
                path: change.path.clone(),
                fate: Fate::Back,
                by_command: command,
                to: change.before.clone(),
                mode: change.mode,
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
            _ if now.as_deref() != present(&file.last) => Fate::Changed,
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
/// it, relative to `workspace`, and what of the commands' changes stays.
pub fn listing(plan: &[Restore], workspace: Option<&Path>) -> Vec<String> {
    let mut lines: Vec<String> = plan
        .iter()
        .take(LISTED)
        .map(|file| {
            let by = if file.by_command {
                " (changed by a command)"
            } else {
                ""
            };
            format!("{} {}{by}", label(&file.path, workspace), file.fate.says())
        })
        .collect();
    if plan.len() > LISTED {
        lines.push(format!("and {} more", plan.len() - LISTED));
    }
    if plan.iter().any(|file| file.by_command) {
        lines.push(COMMAND_FILES_NOTE.to_owned());
    }
    lines
}

/// One file a turn changed, as the card after the turn lists it.
#[derive(Clone, Debug)]
pub struct Changed {
    /// What undoing the turn would do to it.
    pub restore: Restore,
    /// What the turn did to its lines, when that can still be told.
    pub lines: Option<Lines>,
}

/// The lines a turn added to and removed from one file, and the diff.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Lines {
    pub added: usize,
    pub removed: usize,
    /// The diff, with long unchanged stretches folded to a count.
    pub rows: Vec<Row>,
}

/// One line of a file's diff.
#[derive(Clone, Debug, PartialEq)]
pub enum Row {
    Same(String),
    Removed(String),
    Added(String),
    /// This many unchanged lines, not shown.
    Folded(usize),
}

/// Unchanged lines kept either side of a change, so it can be placed.
const CONTEXT: usize = 2;

/// Past this many changed lines on either side, lines are not matched one
/// by one: the matching is quadratic, and a rewrite that big reads as a
/// replacement anyway.
const MATCHED: usize = 1000;

/// The files the messages of one `turn` of session `session` wrote, in
/// `workspace`, that are not already as they were before it.
pub fn changed(session: u64, turn: &[Message], workspace: Option<&Path>) -> Vec<Changed> {
    changed_in(&folder(session), turn, workspace)
}

fn changed_in(dir: &Path, turn: &[Message], workspace: Option<&Path>) -> Vec<Changed> {
    plan_in(dir, turn, workspace)
        .into_iter()
        .map(|restore| Changed {
            lines: lines_of(dir, &restore),
            restore,
        })
        .collect()
}

/// What the turn did to `file`'s lines: only while the file still holds
/// what the turn left, as a later change would be counted as the turn's,
/// and only while what it replaced was kept and both are text. A file the
/// turn removed counts as empty.
fn lines_of(dir: &Path, file: &Restore) -> Option<Lines> {
    // Read as the Changes tab reads it: a link could name a secret outside
    // the project, and a device or a pipe never ends.
    let now = read_now(&file.path).ok()?;
    if now.as_deref().map(hash).as_deref() != present(&file.last) {
        return None;
    }
    let old = match &file.to {
        Some(to) => kept(dir, to).ok()?,
        None => Vec::new(),
    };
    Some(diff(
        &String::from_utf8(old).ok()?,
        &String::from_utf8(now.unwrap_or_default()).ok()?,
    ))
}

/// The diff from `old` to `new`, line by line.
pub fn diff(old: &str, new: &str) -> Lines {
    // Lines are compared with their endings, so a turn that only added the
    // last newline or turned CRLF into LF is not counted as no change.
    let (old, new): (Vec<&str>, Vec<&str>) = (
        old.split_inclusive('\n').collect(),
        new.split_inclusive('\n').collect(),
    );
    // What both ends share is set aside first, so an edit in a long file is
    // matched over the few lines around it rather than the whole file.
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let tail = old[head..]
        .iter()
        .rev()
        .zip(new[head..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let (gone, came) = (&old[head..old.len() - tail], &new[head..new.len() - tail]);
    let mut rows: Vec<Row> = old[..head].iter().map(|l| Row::Same(text(l))).collect();
    if gone.len().max(came.len()) > MATCHED {
        rows.extend(gone.iter().map(|l| Row::Removed(text(l))));
        rows.extend(came.iter().map(|l| Row::Added(text(l))));
    } else {
        rows.extend(matched(gone, came));
    }
    rows.extend(old[old.len() - tail..].iter().map(|l| Row::Same(text(l))));
    let count = |f: fn(&Row) -> bool| rows.iter().filter(|row| f(row)).count();
    Lines {
        added: count(|row| matches!(row, Row::Added(_))),
        removed: count(|row| matches!(row, Row::Removed(_))),
        rows: fold(rows),
    }
}

/// `old` against `new` by their longest common run of lines.
fn matched(old: &[&str], new: &[&str]) -> Vec<Row> {
    // common[i][j] is how long a run the tails from i and j share.
    let mut common = vec![vec![0usize; new.len() + 1]; old.len() + 1];
    for i in (0..old.len()).rev() {
        for j in (0..new.len()).rev() {
            common[i][j] = if old[i] == new[j] {
                common[i + 1][j + 1] + 1
            } else {
                common[i + 1][j].max(common[i][j + 1])
            };
        }
    }
    let (mut rows, mut i, mut j) = (Vec::new(), 0, 0);
    while i < old.len() && j < new.len() {
        if old[i] == new[j] {
            rows.push(Row::Same(text(old[i])));
            (i, j) = (i + 1, j + 1);
        } else if common[i + 1][j] >= common[i][j + 1] {
            rows.push(Row::Removed(text(old[i])));
            i += 1;
        } else {
            rows.push(Row::Added(text(new[j])));
            j += 1;
        }
    }
    rows.extend(old[i..].iter().map(|l| Row::Removed(text(l))));
    rows.extend(new[j..].iter().map(|l| Row::Added(text(l))));
    rows
}

/// A line as the diff shows it, without its ending.
fn text(line: &str) -> String {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line).into()
}

/// Fold the unchanged lines further than `CONTEXT` from any change.
fn fold(rows: Vec<Row>) -> Vec<Row> {
    let near: Vec<bool> = (0..rows.len())
        .map(|i| {
            let to = (i + CONTEXT).min(rows.len() - 1);
            rows[i.saturating_sub(CONTEXT)..=to]
                .iter()
                .any(|row| !matches!(row, Row::Same(_)))
        })
        .collect();
    let mut out = Vec::new();
    for (row, near) in rows.into_iter().zip(near) {
        match out.last_mut() {
            _ if near => out.push(row),
            Some(Row::Folded(n)) => *n += 1,
            _ => out.push(Row::Folded(1)),
        }
    }
    out
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
    settle(dir);
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
    let now = std::fs::read(&file.path).ok();
    if now.as_deref().map(hash).as_deref() != present(&file.last) {
        return Err("it changed in the meantime, so it was left alone".into());
    }
    // Letronna's version goes into the same folder before it is replaced,
    // so the rewind can itself be undone by hand; a removed file has none.
    if let Some(now) = &now {
        let pot = if file.by_command {
            Pot::Commands
        } else {
            Pot::Tools
        };
        let _ = store(dir, &file.last, now, pot);
        if !dir.join(&file.last).is_file() {
            return Err("its current version could not be kept, so it was left alone".into());
        }
    }
    let Some(old) = old else {
        return std::fs::remove_file(&file.path).map_err(|e| format!("could not remove it: {e}"));
    };
    // A folder a command removed with it is made again, inside the project
    // as `reachable` found.
    if let Some(folder) = file.path.parent() {
        std::fs::create_dir_all(folder)
            .map_err(|e| format!("could not make its folder again: {e}"))?;
    }
    // Written the way the tools write, but noted nowhere: a rewind is not a
    // turn, and there is nothing after it to go back past.
    crate::write::atomic(&file.path, &old, &Log::default())?;
    let Some(mode) = file.mode else {
        return Ok(());
    };
    set_mode(&file.path, mode).map_err(|e| format!("its bytes went back but not its mode: {e}"))
}

/// Give the file at `path` the mode `mode`, as [`FileChange::mode`] says
/// it: a git mode lets it run wherever it may be read, as a checkout does
/// under the umask, or not at all. Never a set-id or sticky bit, which no
/// session line is trusted to give.
#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
    // Changed through the file it opens, not by name, so a link put in its
    // place since cannot carry the change outside the project.
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)?;
    let now = file.metadata()?.permissions().mode() & 0o777;
    let bits = match mode & !0o777 {
        0 => mode,
        _ if mode & 0o100 != 0 => now | (now & 0o444) >> 2,
        _ => now & !0o111,
    };
    file.set_permissions(std::fs::Permissions::from_mode(bits & 0o777))
}

#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> std::io::Result<()> {
    Ok(())
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

/// A noted hash as what is on disk: `None` for the empty one a removed file
/// is noted with.
fn present(hash: &str) -> Option<&str> {
    (!hash.is_empty()).then_some(hash)
}

/// The hash of what is at `path` now, or `None` when nothing readable is.
fn current(path: &Path) -> Option<String> {
    std::fs::read(path).ok().map(|bytes| hash(&bytes))
}

/// The real path of the folder the tools wrote in, the project folder or else
/// the fallback folder.
fn real(workspace: Option<&Path>) -> Option<PathBuf> {
    // None when it is not there.
    let root = workspace
        .map(Path::to_path_buf)
        .or_else(crate::chat::fallback_folder)?;
    std::fs::canonicalize(root).ok()
}

/// Whether a rewind may write or remove `path`: where the file tools could,
/// so not a link and in a folder that is really inside `root`, the real
/// project folder. A folder that is gone, as a command removed it, counts
/// as the nearest one above it still there, which `put_back` makes it in.
/// A session line naming anywhere else is not followed, nor is a folder on
/// the way that became a link since: paths are noted by their real path,
/// and the file would land wherever the link leads.
fn reachable(path: &Path, root: Option<&Path>) -> bool {
    let Some(root) = root else {
        return false;
    };
    if std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    // Folders that are gone are made by name, and a `..` among them would
    // climb out of the one found.
    if path.components().any(|part| part == Component::ParentDir) {
        return false;
    }
    path.ancestors()
        .skip(1)
        .find(|up| std::fs::symlink_metadata(up).is_ok())
        .filter(|up| up.is_dir())
        .and_then(|up| Some((up, std::fs::canonicalize(up).ok()?)))
        .is_some_and(|(up, real)| real == up && real.starts_with(root))
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
    // What the folder holds is counted afresh, with what it was given.
    lock_used().remove(to);
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

    /// Run a tool as a turn does, and give back its result message.
    fn call(ctx: &ToolContext, name: &str, args: serde_json::Value) -> Message {
        run(name, &args.to_string(), ctx).unwrap();
        Message {
            role: "tool".into(),
            name: name.into(),
            args: args.to_string(),
            files: ctx.files.take(),
            unkept: ctx.files.unkept(),
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
    fn a_rewind_brings_back_what_a_patch_deleted_or_moved_away() {
        let (project, kept) = scratch("patch");
        std::fs::write(project.join("gone.txt"), "bye\n").unwrap();
        std::fs::write(project.join("old.txt"), "keep\n").unwrap();
        let ctx = context(&project, &kept);
        let patch = "*** Begin Patch\n*** Delete File: gone.txt\n*** Update File: old.txt\n\
                     *** Move to: new.txt\n@@\n-keep\n+kept\n*** End Patch";
        let turns = [call(
            &ctx,
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        )];
        assert_eq!(turns[0].files[0].before, Some(hash(b"bye\n")));
        assert_eq!(turns[0].files[0].after, "");

        // A file made again where one was deleted is someone else's.
        std::fs::write(project.join("gone.txt"), "mine\n").unwrap();
        let plan = plan_in(&kept, &turns, Some(&project));
        assert_eq!(
            fates(&plan, &project)[0],
            ("gone.txt".into(), Fate::Changed)
        );
        std::fs::remove_file(project.join("gone.txt")).unwrap();

        let plan = plan_in(&kept, &turns, Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [
                ("gone.txt".to_owned(), Fate::Back),
                ("new.txt".to_owned(), Fate::Remove),
                ("old.txt".to_owned(), Fate::Back)
            ]
        );
        let done = restore_in(&kept, &plan, Some(&project));
        assert!(done.left.is_empty(), "{done:?}");
        assert_eq!(
            std::fs::read_to_string(project.join("gone.txt")).unwrap(),
            "bye\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.join("old.txt")).unwrap(),
            "keep\n"
        );
        assert!(!project.join("new.txt").exists());
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
        store(&kept, &hash(b"more"), b"more", Pot::Tools).unwrap();
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
                by_command: false,
                to: None,
                mode: None,
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
                mode: None,
            }],
            ..Default::default()
        };
        let plan = plan_in(&kept, &[forged], Some(&project));
        assert_eq!(plan[0].fate, Fate::Outside);
        // Nor when the plan itself says remove, or there is no project.
        let made_up = [Restore {
            path: victim.clone(),
            fate: Fate::Remove,
            by_command: false,
            to: None,
            mode: None,
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

    #[test]
    fn a_session_s_files_are_counted_from_how_it_found_them_without_git() {
        let (project, kept) = scratch("session-files");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        std::fs::write(project.join("c.txt"), "same\n").unwrap();
        let ctx = context(&project, &kept);
        let turns = [
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "a.txt", "content": "one\ntwo\n"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "b.txt", "content": "new\n"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "c.txt", "content": "changed\n"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "c.txt", "content": "same\n"}),
            ),
        ];
        let files = session_files_in(&kept, &turns, &project);
        let counts: Vec<(String, usize, usize, bool)> = files
            .iter()
            .map(|f| (f.path.display().to_string(), f.added, f.removed, f.created))
            .collect();
        // c.txt is back as it was, so it is not a change.
        assert_eq!(
            counts,
            vec![("a.txt".into(), 1, 0, false), ("b.txt".into(), 1, 0, true)]
        );
        let patch = session_patch_in(&kept, &turns, &project, Path::new("a.txt")).unwrap();
        assert!(patch.contains("--- a/a.txt\n+++ b/a.txt\n"), "{patch}");
        assert!(patch.contains(" one\n+two\n"), "{patch}");
        let made = session_patch_in(&kept, &turns, &project, Path::new("b.txt")).unwrap();
        assert!(made.starts_with("--- /dev/null\n"), "{made}");
    }

    #[cfg(unix)]
    #[test]
    fn a_session_file_turned_into_a_link_is_not_read() {
        let (project, kept) = scratch("session-link");
        let secret = project.parent().unwrap().join("session-link-secret.txt");
        std::fs::write(&secret, "hunter2\n").unwrap();
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        let ctx = context(&project, &kept);
        let turns = [call(
            &ctx,
            "write_file",
            serde_json::json!({"path": "a.txt", "content": "two\n"}),
        )];
        std::fs::remove_file(project.join("a.txt")).unwrap();
        std::os::unix::fs::symlink(&secret, project.join("a.txt")).unwrap();
        let files = session_files_in(&kept, &turns, &project);
        assert_eq!((files[0].added, files[0].removed), (0, 0), "{files:?}");
        let shown = session_patch_in(&kept, &turns, &project, Path::new("a.txt"));
        assert!(shown.is_err_and(|why| !why.contains("hunter2")));
    }

    #[test]
    fn reverting_one_file_undoes_only_the_session_s_writes_to_it() {
        let (project, kept) = scratch("revert-one");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        std::fs::write(project.join("b.txt"), "bee\n").unwrap();
        let ctx = context(&project, &kept);
        let turns = [
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "a.txt", "content": "1\n"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "b.txt", "content": "2\n"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "a.txt", "content": "3\n"}),
            ),
        ];
        let plan = plan_in(
            &kept,
            &writes_to(&turns, &project, Path::new("a.txt")),
            Some(&project),
        );
        assert_eq!(fates(&plan, &project), vec![("a.txt".into(), Fate::Back)]);
        let done = restore_in(&kept, &plan, Some(&project));
        assert_eq!(done.back, 1, "{:?}", done.left);
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "one\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.join("b.txt")).unwrap(),
            "2\n"
        );
        // Changed by hand since, it is left alone.
        std::fs::write(project.join("b.txt"), "mine\n").unwrap();
        let plan = plan_in(
            &kept,
            &writes_to(&turns, &project, Path::new("b.txt")),
            Some(&project),
        );
        assert_eq!(
            fates(&plan, &project),
            vec![("b.txt".into(), Fate::Changed)]
        );
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

    #[test]
    fn a_turn_lists_its_files_with_the_lines_it_added_and_removed() {
        let (project, kept) = scratch("changed-lines");
        std::fs::write(project.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        std::fs::write(project.join("c.txt"), "same\n").unwrap();
        let ctx = context(&project, &kept);
        let turn = [
            call(
                &ctx,
                "edit_file",
                serde_json::json!({"path": "a.txt", "old": "two", "new": "2\nand a half"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "b.txt", "content": "new\nfile\n"}),
            ),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "c.txt", "content": "other\n"}),
            ),
        ];
        // Written back by hand since: nothing of the turn is left in it.
        std::fs::write(project.join("c.txt"), "same\n").unwrap();

        let files = changed_in(&kept, &turn, Some(&project));
        let counts: Vec<_> = files
            .iter()
            .map(|file| {
                let lines = file.lines.as_ref().unwrap();
                (
                    label(&file.restore.path, Some(&project)),
                    lines.added,
                    lines.removed,
                )
            })
            .collect();
        assert_eq!(
            counts,
            [("a.txt".to_owned(), 2, 1), ("b.txt".to_owned(), 2, 0)]
        );

        // Changed again after the turn: the lines are no longer the turn's.
        std::fs::write(project.join("a.txt"), "someone else's\n").unwrap();
        let files = changed_in(&kept, &turn, Some(&project));
        assert!(files[0].lines.is_none());
        assert_eq!(files[0].restore.fate, Fate::Changed);
    }

    #[test]
    fn a_turn_that_deleted_a_file_lists_its_lines_as_removed() {
        let (project, kept) = scratch("changed-deleted");
        std::fs::write(project.join("gone.txt"), "one\ntwo\n").unwrap();
        let ctx = context(&project, &kept);
        let patch = "*** Begin Patch\n*** Delete File: gone.txt\n*** End Patch";
        let turn = [call(
            &ctx,
            "apply_patch",
            serde_json::json!({ "patch": patch }),
        )];
        let files = changed_in(&kept, &turn, Some(&project));
        assert_eq!(files.len(), 1);
        let lines = files[0].lines.as_ref().unwrap();
        assert_eq!((lines.added, lines.removed), (0, 2));
        assert_eq!(files[0].restore.fate, Fate::Back);
    }

    #[test]
    fn a_diff_counts_a_change_to_line_endings_alone() {
        let lines = diff("one\ntwo", "one\ntwo\n");
        assert_eq!((lines.added, lines.removed), (1, 1));
        assert_eq!(
            lines.rows,
            [
                Row::Same("one".into()),
                Row::Removed("two".into()),
                Row::Added("two".into()),
            ]
        );
        let lines = diff("a\r\nb\r\n", "a\nb\n");
        assert_eq!((lines.added, lines.removed), (2, 2));
        assert_eq!(diff("same\n", "same\n").added, 0);
    }

    #[test]
    fn a_diff_folds_what_did_not_change_far_from_the_edit() {
        let old: String = (1..=20).map(|n| format!("{n}\n")).collect();
        let new = old.replace("10\n", "ten\n");
        let lines = diff(&old, &new);
        assert_eq!((lines.added, lines.removed), (1, 1));
        let same = |n: usize| Row::Same(n.to_string());
        assert_eq!(
            lines.rows,
            [
                Row::Folded(7),
                same(8),
                same(9),
                Row::Removed("10".into()),
                Row::Added("ten".into()),
                same(11),
                same(12),
                Row::Folded(8),
            ]
        );
    }

    /// Run git in `folder` as a test sets a repository up, without the
    /// user's own config.
    // Its tests run sh commands, so they are unix-only.
    #[cfg(unix)]
    fn git(folder: &Path, args: &[&str]) {
        let ok = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(["-c", "commit.gpgsign=false"])
            .args(args)
            .current_dir(folder)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A project that is a repository with `files` committed, and a
    /// history folder of its own.
    // Its tests run sh commands, so they are unix-only.
    #[cfg(unix)]
    fn repository(name: &str, files: &[(&str, &str)]) -> (PathBuf, PathBuf) {
        let (project, kept) = scratch(name);
        git(&project, &["init", "-q"]);
        for (file, text) in files {
            std::fs::write(project.join(file), text).unwrap();
        }
        git(&project, &["add", "."]);
        git(&project, &["commit", "-q", "-m", "first"]);
        (project, kept)
    }

    /// Run `command` as a turn runs it in the foreground, and give back its
    /// result message, whether it worked or not.
    // Its tests run sh commands, so they are unix-only.
    #[cfg(unix)]
    fn ran(ctx: &ToolContext, command: &str) -> Message {
        let args = serde_json::json!({ "command": command }).to_string();
        let _ = run("run_command", &args, ctx);
        Message {
            role: "tool".into(),
            name: "run_command".into(),
            args,
            files: ctx.files.take(),
            unkept: ctx.files.unkept(),
            ..Default::default()
        }
    }

    #[cfg(unix)]
    fn text(project: &Path, name: &str) -> String {
        std::fs::read_to_string(project.join(name)).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn sed_in_place_rm_checkout_and_reset_hard_restore() {
        let files = [("a.txt", "one\n"), ("b.txt", "bee\n"), ("c.txt", "sea\n")];
        let (project, kept) = repository("command-git", &files);
        // Work not yet committed, which checkout and reset throw away.
        std::fs::write(project.join("c.txt"), "mine\n").unwrap();
        let ctx = context(&project, &kept);
        let cases = [
            (
                "sed -i.bak 's/one/two/' a.txt && rm a.txt.bak",
                "a.txt",
                "one\n",
            ),
            ("rm b.txt", "b.txt", "bee\n"),
            ("git checkout -- c.txt", "c.txt", "mine\n"),
            ("git reset -q --hard", "c.txt", "mine\n"),
        ];
        for (command, name, was) in cases {
            let turn = ran(&ctx, command);
            assert_eq!(turn.unkept.as_deref(), Some(""), "{command}");
            assert!(!ran_commands(std::slice::from_ref(&turn)), "{command}");
            let plan = plan_in(&kept, std::slice::from_ref(&turn), Some(&project));
            assert_eq!(fates(&plan, &project), [(name.to_owned(), Fate::Back)]);
            assert_eq!(
                listing(&plan, Some(&project)),
                [
                    format!("{name} goes back to how it was (changed by a command)"),
                    COMMAND_FILES_NOTE.to_owned()
                ]
            );
            let done = restore_in(&kept, &plan, Some(&project));
            assert_eq!(done.back, 1, "{command}: {:?}", done.left);
            assert_eq!(text(&project, name), was, "{command}");
        }
        // What was kept for commands is written down for the next start.
        assert!(counted(&kept).commands > 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_formatters_changes_restore() {
        // Outside git, where the whole folder is looked at.
        let (project, kept) = scratch("command-format");
        for folder in ["src", "out"] {
            std::fs::create_dir_all(project.join(folder)).unwrap();
        }
        std::fs::write(project.join(".gitignore"), "out/\n").unwrap();
        for name in ["src/a.rs", "src/b.rs", "out/gen.rs"] {
            std::fs::write(project.join(name), "fn  main( ) {}\n").unwrap();
        }
        std::fs::write(project.join("src/same.rs"), "fn main() {}\n").unwrap();
        let ctx = context(&project, &kept);
        // Written beside each file and moved over it, as formatters do.
        let turn = ran(
            &ctx,
            "for f in src/*.rs out/*.rs; do sed 's/  */ /g; s/( )/()/' \"$f\" > \"$f.new\" \
             && mv \"$f.new\" \"$f\"; done",
        );
        assert_eq!(text(&project, "src/a.rs"), "fn main() {}\n");
        let plan = plan_in(&kept, std::slice::from_ref(&turn), Some(&project));
        // Nor what the project ignores, nor a file written back the same.
        assert_eq!(
            fates(&plan, &project),
            [
                ("src/a.rs".to_owned(), Fate::Back),
                ("src/b.rs".to_owned(), Fate::Back)
            ]
        );
        // Reverting one of them from the Changes tab says so too.
        let one = writes_to(std::slice::from_ref(&turn), &project, Path::new("src/a.rs"));
        assert!(plan_in(&kept, &one, Some(&project))[0].by_command);
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 2);
        assert_eq!(text(&project, "src/a.rs"), "fn  main( ) {}\n");
        assert_eq!(text(&project, "src/b.rs"), "fn  main( ) {}\n");
        assert_eq!(text(&project, "out/gen.rs"), "fn main() {}\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_new_untracked_file_is_removed() {
        let (project, kept) = repository("command-new", &[("a.txt", "one\n")]);
        let ctx = context(&project, &kept);
        let turn = ran(
            &ctx,
            "mkdir gen && echo hi > gen/out.txt && echo hi > new.txt",
        );
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [
                ("gen/out.txt".to_owned(), Fate::Remove),
                ("new.txt".to_owned(), Fate::Remove)
            ]
        );
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 2);
        assert!(!project.join("gen/out.txt").exists());
        assert!(!project.join("new.txt").exists());
        assert_eq!(text(&project, "a.txt"), "one\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_the_command_stops_ignoring_is_left_alone() {
        let files = [(".gitignore", "secret.txt\n"), ("a.txt", "one\n")];
        let (project, kept) = repository("command-unignore", &files);
        std::fs::write(project.join("secret.txt"), "key\n").unwrap();
        // Older than the command by more than a clock's tick.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "printf '' > .gitignore && echo hi > new.txt");
        // It may have been the command's, so the user is told it stays.
        assert_eq!(
            turn.unkept.as_deref(),
            Some("left secret.txt: it looks older than the command")
        );
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [
                (".gitignore".to_owned(), Fate::Back),
                ("new.txt".to_owned(), Fate::Remove)
            ]
        );
        restore_in(&kept, &plan, Some(&project));
        assert_eq!(text(&project, "secret.txt"), "key\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_changes_too_many_files_records_none_and_says_so() {
        let (project, kept) = repository("command-many", &[("a.txt", "one\n")]);
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "for i in $(seq 1 501); do echo $i > f$i.txt; done");
        assert!(project.join("f501.txt").exists());
        assert!(turn.files.is_empty());
        assert_eq!(
            turn.unkept.as_deref(),
            Some("not undoable: it changed over 500 files")
        );
        assert!(ran_commands(&[turn]));
    }

    #[cfg(unix)]
    #[test]
    fn a_command_moved_to_the_background_keeps_what_it_changed_so_far() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (project, kept) = scratch("command-moved");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        // A conversation of its own, which a command can become a job for.
        let ctx = ToolContext {
            conversation: 7_704,
            ..context(&project, &kept)
        };
        let command = "echo two > a.txt; sleep 2; echo three > a.txt";
        let args = serde_json::json!({"command": command, "timeout": 1});
        let turn = call(&ctx, "run_command", args);
        assert_eq!(turn.unkept.as_deref(), Some("later changes not undoable"));
        let plan = plan_in(&kept, std::slice::from_ref(&turn), Some(&project));
        assert_eq!(fates(&plan, &project), [("a.txt".to_owned(), Fate::Back)]);
        // What the job does after is not the command's to undo.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while text(&project, "a.txt") != "three\n" {
            assert!(std::time::Instant::now() < deadline, "the job never wrote");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [("a.txt".to_owned(), Fate::Changed)]
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while crate::inbox::take(7_704, true).is_empty() {
            assert!(std::time::Instant::now() < deadline, "no exit notice");
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_branch_checkout_records_nothing() {
        let (project, kept) = repository("command-branch", &[("a.txt", "one\n")]);
        git(&project, &["checkout", "-q", "-b", "other"]);
        std::fs::write(project.join("a.txt"), "other\n").unwrap();
        git(&project, &["commit", "-q", "-am", "other"]);
        git(&project, &["checkout", "-q", "-"]);
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "git checkout -q other");
        assert_eq!(text(&project, "a.txt"), "other\n");
        // Putting a.txt back without the branch would be a half state.
        assert!(turn.files.is_empty(), "{:?}", turn.files);
        let turns = [turn];
        assert!(ran_commands(&turns));
        assert_eq!(
            unkept(&turns),
            ["`git checkout -q other`: moved git HEAD; go back with git."]
        );
    }

    #[cfg(unix)]
    #[test]
    fn background_commands_are_skipped() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (project, kept) = scratch("command-background");
        let ctx = context(&project, &kept);
        let args = serde_json::json!({"command": "echo hi > bg.txt", "background": true});
        call(&ctx, "run_command", args);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while !project.join("bg.txt").exists() {
            assert!(std::time::Instant::now() < deadline, "the job never wrote");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let args = serde_json::json!({"command": "echo hi > bg.txt", "background": true});
        let job = Message {
            role: "tool".into(),
            name: "run_command".into(),
            args: args.to_string(),
            files: ctx.files.take(),
            unkept: ctx.files.unkept(),
            ..Default::default()
        };
        assert!(job.files.is_empty() && job.unkept.is_none());
        let turns = [job];
        assert!(ran_commands(&turns));
        assert_eq!(
            unkept(&turns),
            ["`echo hi > bg.txt`: left running in the background, so not undone."]
        );
        // One that only reads has nothing to keep and nothing missed.
        let read = ran(&ctx, "cat bg.txt");
        assert!(read.files.is_empty());
        assert_eq!(read.unkept.as_deref(), Some(""));
        assert!(!ran_commands(&[read]));
    }

    #[cfg(unix)]
    #[test]
    fn a_file_tool_edit_after_a_large_snapshot_keeps_its_copy() {
        let (project, kept) = scratch("command-pots");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        // Commands have used up their allowance, as a first look at a big
        // folder might. The filler is sparse: it counts its full length
        // but takes no disk.
        std::fs::create_dir_all(&kept).unwrap();
        let filler = std::fs::File::create(kept.join("filler")).unwrap();
        filler.set_len(COMMAND_CAP).unwrap();
        std::fs::write(kept.join(LEDGER), COMMAND_CAP.to_string()).unwrap();
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "echo two > a.txt");
        // The change is noted, but its old bytes did not fit.
        let plan = plan_in(&kept, std::slice::from_ref(&turn), Some(&project));
        assert_eq!(fates(&plan, &project), [("a.txt".to_owned(), Fate::NoCopy)]);
        // The file tools have their own allowance, untouched.
        let edit = call(
            &ctx,
            "write_file",
            serde_json::json!({"path": "a.txt", "content": "three\n"}),
        );
        assert!(kept.join(hash(b"two\n")).is_file());
        let plan = plan_in(&kept, &[edit], Some(&project));
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 1);
        assert_eq!(text(&project, "a.txt"), "two\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_file_a_filter_writes_is_said_not_kept_rather_than_put_back_wrong() {
        let (project, kept) = scratch("command-filter");
        git(&project, &["init", "-q"]);
        // The repository's own filter, which a look at it turns off.
        for part in ["clean", "smudge"] {
            let key = format!("filter.rot.{part}");
            git(&project, &["config", key.as_str(), "tr a-z n-za-m"]);
        }
        std::fs::write(project.join(".gitattributes"), "*.sec filter=rot\n").unwrap();
        let secrets = ["s1.sec", "s2.sec", "s3.sec", "s4.sec"];
        // Older than the index git writes, so git trusts its times rather
        // than reading them again with the filter off.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
        for name in secrets.iter().chain(&["a.txt"]) {
            std::fs::write(project.join(name), "hello\n").unwrap();
            let file = std::fs::File::options()
                .write(true)
                .open(project.join(name));
            file.unwrap().set_modified(old).unwrap();
        }
        git(&project, &["add", "."]);
        git(&project, &["commit", "-q", "-m", "first"]);
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "for f in *.sec a.txt; do echo bye > $f; done");
        assert_eq!(
            turn.unkept.as_deref(),
            Some("s1.sec, s2.sec, s3.sec and 1 more could not be kept")
        );
        assert!(ran_commands(std::slice::from_ref(&turn)));
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(fates(&plan, &project), [("a.txt".to_owned(), Fate::Back)]);
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 1);
        assert_eq!(text(&project, "a.txt"), "hello\n");
        // Never the filtered bytes of the commit.
        assert_eq!(text(&project, "s1.sec"), "bye\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_removed_folder_comes_back_whole() {
        let files = [("src/a.txt", "one\n"), ("src/b.txt", "bee\n")];
        let (project, kept) = scratch("command-rmdir");
        git(&project, &["init", "-q"]);
        std::fs::create_dir_all(project.join("src")).unwrap();
        for (file, text) in files {
            std::fs::write(project.join(file), text).unwrap();
        }
        git(&project, &["add", "."]);
        git(&project, &["commit", "-q", "-m", "first"]);
        // Uncommitted work, tracked and not.
        std::fs::write(project.join("src/b.txt"), "mine\n").unwrap();
        std::fs::create_dir_all(project.join("wip")).unwrap();
        std::fs::write(project.join("wip/new.txt"), "draft\n").unwrap();
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "rm -rf src wip");
        let plan = plan_in(&kept, &[turn], Some(&project));
        let back =
            ["src/a.txt", "src/b.txt", "wip/new.txt"].map(|name| (name.to_owned(), Fate::Back));
        assert_eq!(fates(&plan, &project), back);
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 3);
        assert_eq!(text(&project, "src/a.txt"), "one\n");
        assert_eq!(text(&project, "src/b.txt"), "mine\n");
        assert_eq!(text(&project, "wip/new.txt"), "draft\n");

        // Outside git, a file written again where the folder was is still
        // the one the command removed, under the same real path.
        let (project, kept) = scratch("command-rmdir-walk");
        std::fs::create_dir_all(project.join("src")).unwrap();
        std::fs::write(project.join("src/a.txt"), "one\n").unwrap();
        let ctx = context(&project, &kept);
        let turns = [
            ran(&ctx, "rm -rf src"),
            call(
                &ctx,
                "write_file",
                serde_json::json!({"path": "src/a.txt", "content": "two\n"}),
            ),
        ];
        let plan = plan_in(&kept, &turns, Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [("src/a.txt".to_owned(), Fate::Back)]
        );
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 1);
        assert_eq!(text(&project, "src/a.txt"), "one\n");
    }

    #[cfg(unix)]
    #[test]
    fn a_moved_file_goes_back_and_a_new_one_that_looks_old_is_named() {
        for in_git in [true, false] {
            let files = [("a.txt", "one\n"), ("b.txt", "bee\n")];
            let (project, kept) = if in_git {
                repository("command-mv", &files)
            } else {
                let (project, kept) = scratch("command-mv-walk");
                for (file, text) in files {
                    std::fs::write(project.join(file), text).unwrap();
                }
                (project, kept)
            };
            // Older than the command by more than a clock's tick, as a
            // file it moves, links or copies with its times kept stays.
            std::thread::sleep(std::time::Duration::from_millis(300));
            let ctx = context(&project, &kept);
            let turn = ran(&ctx, "mv a.txt moved.txt && ln b.txt linked.txt");
            assert_eq!(
                turn.unkept.as_deref(),
                Some("left linked.txt: it looks older than the command"),
                "in git: {in_git}"
            );
            let plan = plan_in(&kept, &[turn], Some(&project));
            assert_eq!(
                fates(&plan, &project),
                [
                    ("a.txt".to_owned(), Fate::Back),
                    ("moved.txt".to_owned(), Fate::Remove)
                ],
                "in git: {in_git}"
            );
            assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 2);
            assert_eq!(text(&project, "a.txt"), "one\n");
            assert!(!project.join("moved.txt").exists());
            assert_eq!(text(&project, "linked.txt"), "bee\n");
        }
    }

    #[cfg(unix)]
    #[test]
    fn files_a_command_adds_to_the_index_are_noted_as_made() {
        let (project, kept) = repository("command-add", &[("a.txt", "one\n")]);
        std::thread::sleep(std::time::Duration::from_millis(300));
        let ctx = context(&project, &kept);
        let turn = ran(
            &ctx,
            "git mv a.txt b.txt && echo n > n.txt && git add n.txt",
        );
        assert_eq!(turn.unkept.as_deref(), Some(""));
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [
                ("a.txt".to_owned(), Fate::Back),
                ("n.txt".to_owned(), Fate::Remove),
                ("b.txt".to_owned(), Fate::Remove)
            ]
        );
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 3);
        assert_eq!(text(&project, "a.txt"), "one\n");
        assert!(!project.join("b.txt").exists() && !project.join("n.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_project_its_repository_ignores_is_walked() {
        let (project, kept) = scratch("command-ignored");
        let home = project.parent().unwrap();
        // A home folder kept in git, ignoring all but what it adds.
        git(home, &["init", "-q"]);
        std::fs::write(home.join(".gitignore"), "*\n").unwrap();
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "echo two > a.txt && echo x > b.txt");
        assert_eq!(turn.unkept.as_deref(), Some(""));
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [
                ("a.txt".to_owned(), Fate::Back),
                ("b.txt".to_owned(), Fate::Remove)
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_failing_in_a_repository_is_said_rather_than_walked_past() {
        let (project, kept) = scratch("command-broken-git");
        // A repository git cannot read, as one it refuses for who owns it.
        std::fs::create_dir_all(project.join(".git")).unwrap();
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "echo two > a.txt");
        assert!(turn.files.is_empty());
        assert_eq!(
            turn.unkept.as_deref(),
            Some("not undoable: git could not list the changes")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_session_given_a_used_folder_keeps_its_own_copies() {
        let (project, kept) = scratch("command-reused");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        let ctx = context(&project, &kept);
        ran(&ctx, "echo x > other.txt");
        // The session goes, and a new one is given its folder.
        std::fs::remove_dir_all(&kept).unwrap();
        lock_used().remove(&kept);
        let turn = ran(&ctx, "echo two > a.txt");
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(fates(&plan, &project), [("a.txt".to_owned(), Fate::Back)]);
    }

    #[cfg(unix)]
    #[test]
    fn the_look_after_a_command_stops_at_what_it_may_read() {
        let (project, kept) = repository("command-read-cap", &[("a.txt", "one\n")]);
        let ctx = context(&project, &kept);
        // Sparse, so they take no disk: ten of them are all that is read.
        let turn = ran(
            &ctx,
            "for i in 1 2 3 4 5 6 7 8 9 10 11; do \
             dd if=/dev/zero of=big$i.bin bs=1 count=0 seek=10485760 2>/dev/null; done",
        );
        assert_eq!(turn.files.len(), 10);
        assert_eq!(turn.unkept.as_deref(), Some("big9.bin could not be kept"));
    }

    /// The permission bits of `name` in `project`.
    #[cfg(unix)]
    fn mode(project: &Path, name: &str) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        let meta = std::fs::metadata(project.join(name)).unwrap();
        meta.permissions().mode() & 0o777
    }

    #[cfg(unix)]
    fn set_mode(project: &Path, name: &str, bits: u32) {
        use std::os::unix::fs::PermissionsExt;
        let bits = std::fs::Permissions::from_mode(bits);
        std::fs::set_permissions(project.join(name), bits).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_a_command_removes_is_named_and_never_written_back_as_a_file() {
        let link = |to: &str, project: &Path, at: &str| {
            std::os::unix::fs::symlink(to, project.join(at)).unwrap();
        };
        let (project, kept) = scratch("command-links");
        git(&project, &["init", "-q"]);
        std::fs::create_dir_all(project.join("d")).unwrap();
        std::fs::write(project.join("AGENTS.md"), "rules\n").unwrap();
        std::fs::write(project.join("d/a.txt"), "one\n").unwrap();
        link("AGENTS.md", &project, "CLAUDE.md");
        link("../AGENTS.md", &project, "d/link.md");
        git(&project, &["add", "."]);
        git(&project, &["commit", "-q", "-m", "first"]);
        // And one not committed.
        link("AGENTS.md", &project, "mine.md");
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "rm CLAUDE.md mine.md && rm -rf d");
        assert_eq!(
            turn.unkept.as_deref(),
            Some("links stay as the command left them: CLAUDE.md, d/link.md, mine.md")
        );
        assert!(ran_commands(std::slice::from_ref(&turn)));
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(fates(&plan, &project), [("d/a.txt".to_owned(), Fate::Back)]);
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 1);
        assert_eq!(text(&project, "d/a.txt"), "one\n");
        // Never a file holding where the link pointed.
        for name in ["CLAUDE.md", "d/link.md", "mine.md"] {
            assert!(
                std::fs::symlink_metadata(project.join(name)).is_err(),
                "{name}"
            );
        }

        // A link written over with a file, as GNU sed -i does, and one made.
        git(&project, &["checkout", "-q", "--", "."]);
        let turn = ran(
            &ctx,
            "rm CLAUDE.md && echo flat > CLAUDE.md && ln -s AGENTS.md new.md",
        );
        assert_eq!(
            turn.unkept.as_deref(),
            Some("links stay as the command left them: CLAUDE.md, new.md")
        );
        assert!(turn.files.is_empty(), "{:?}", turn.files);

        // Outside git, the same.
        let (project, kept) = scratch("command-links-walk");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        link("a.txt", &project, "link.txt");
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "rm link.txt && ln -s a.txt other.txt");
        assert_eq!(
            turn.unkept.as_deref(),
            Some("links stay as the command left them: link.txt, other.txt")
        );
        assert!(turn.files.is_empty(), "{:?}", turn.files);
    }

    #[cfg(unix)]
    #[test]
    fn a_file_put_back_has_its_mode_back() {
        let (project, kept) = scratch("command-modes");
        git(&project, &["init", "-q"]);
        std::fs::create_dir_all(project.join("d")).unwrap();
        for name in ["run.sh", "d/tool.sh", "go.sh"] {
            std::fs::write(project.join(name), "#!/bin/sh\n").unwrap();
            set_mode(&project, name, 0o755);
        }
        git(&project, &["add", "."]);
        git(&project, &["commit", "-q", "-m", "first"]);
        // Not committed: a key kept private, and a script of the user's.
        std::fs::write(project.join(".env"), "KEY=1\n").unwrap();
        set_mode(&project, ".env", 0o600);
        std::fs::write(project.join("mine.sh"), "#!/bin/sh\n").unwrap();
        set_mode(&project, "mine.sh", 0o750);
        let ctx = context(&project, &kept);
        let turn = ran(
            &ctx,
            "rm -rf d run.sh .env mine.sh && chmod -x go.sh && echo more >> go.sh",
        );
        assert_eq!(turn.unkept.as_deref(), Some(""));
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 5);
        assert_eq!(mode(&project, ".env"), 0o600);
        assert_eq!(mode(&project, "mine.sh"), 0o750);
        // What git would check out: runnable wherever it may be read.
        for name in ["run.sh", "d/tool.sh", "go.sh"] {
            let bits = mode(&project, name);
            assert!(bits & 0o100 != 0, "{name}: {bits:o}");
            assert_eq!(bits & 0o111, (bits & 0o444) >> 2, "{name}: {bits:o}");
        }
        assert_eq!(text(&project, "go.sh"), "#!/bin/sh\n");

        // A file moved goes back with its mode.
        std::thread::sleep(std::time::Duration::from_millis(300));
        let turn = ran(&ctx, "mkdir bin && mv mine.sh bin/mine.sh");
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [
                ("mine.sh".to_owned(), Fate::Back),
                ("bin/mine.sh".to_owned(), Fate::Remove)
            ]
        );
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 2);
        assert_eq!(mode(&project, "mine.sh"), 0o750);
    }

    #[cfg(unix)]
    #[test]
    fn a_mode_alone_changed_is_named() {
        let (project, kept) = repository("command-chmod", &[("a.txt", "one\n")]);
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "chmod +x a.txt");
        assert!(turn.files.is_empty());
        assert_eq!(
            turn.unkept.as_deref(),
            Some("modes stay as the command left them: a.txt")
        );

        let (project, kept) = scratch("command-chmod-walk");
        std::fs::write(project.join("b.txt"), "bee\n").unwrap();
        set_mode(&project, "b.txt", 0o644);
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "chmod 600 b.txt");
        assert!(turn.files.is_empty());
        assert_eq!(
            turn.unkept.as_deref(),
            Some("modes stay as the command left them: b.txt")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_made_a_folder_or_a_link_is_named() {
        for in_git in [true, false] {
            let files = [("a.txt", "one\n"), ("x.txt", "ex\n"), ("y.txt", "why\n")];
            let (project, kept) = if in_git {
                repository("command-retyped", &files)
            } else {
                let (project, kept) = scratch("command-retyped-walk");
                for (file, text) in files {
                    std::fs::write(project.join(file), text).unwrap();
                }
                (project, kept)
            };
            let ctx = context(&project, &kept);
            let turn = ran(
                &ctx,
                "rm x.txt && mkdir x.txt && echo f > x.txt/f && rm y.txt && ln -s a.txt y.txt",
            );
            assert_eq!(
                turn.unkept.as_deref(),
                Some(
                    "links stay as the command left them: y.txt; \
                     folders and submodules stay as the command left them: x.txt"
                ),
                "in git: {in_git}"
            );
            let plan = plan_in(&kept, &[turn], Some(&project));
            assert_eq!(
                fates(&plan, &project),
                [("x.txt/f".to_owned(), Fate::Remove)],
                "in git: {in_git}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn binary_files_and_line_endings_come_back_byte_for_byte() {
        let (project, kept) = scratch("command-bytes");
        git(&project, &["init", "-q"]);
        // Checked out with CR LF, though the commit holds LF.
        std::fs::write(project.join(".gitattributes"), "*.txt text eol=crlf\n").unwrap();
        let binary: Vec<u8> = (0..4096u32).map(|i| (i * 7 % 256) as u8).collect();
        std::fs::write(project.join("data.bin"), &binary).unwrap();
        std::fs::write(project.join("lines.txt"), "one\r\ntwo\r\n").unwrap();
        git(&project, &["add", "."]);
        git(&project, &["commit", "-q", "-m", "first"]);
        let fresh: Vec<u8> = binary.iter().rev().copied().collect();
        std::fs::write(project.join("new.bin"), &fresh).unwrap();
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "rm data.bin lines.txt new.bin");
        assert_eq!(turn.unkept.as_deref(), Some(""));
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 3);
        assert_eq!(std::fs::read(project.join("data.bin")).unwrap(), binary);
        assert_eq!(std::fs::read(project.join("new.bin")).unwrap(), fresh);
        assert_eq!(text(&project, "lines.txt"), "one\r\ntwo\r\n");

        // Asked for by the repository's config rather than an attribute.
        let (project, kept) = scratch("command-bytes-config");
        git(&project, &["init", "-q"]);
        git(&project, &["config", "core.autocrlf", "true"]);
        std::fs::write(project.join("notes.md"), "one\r\ntwo\r\n").unwrap();
        git(&project, &["add", "."]);
        git(&project, &["commit", "-q", "-m", "first"]);
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "rm notes.md");
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 1);
        assert_eq!(text(&project, "notes.md"), "one\r\ntwo\r\n");
    }

    #[cfg(unix)]
    #[test]
    fn nothing_outside_the_project_is_noted_or_touched() {
        let (repo, kept) = scratch("command-outside");
        git(&repo, &["init", "-q"]);
        let project = repo.join("app");
        std::fs::create_dir_all(project.join("d")).unwrap();
        std::fs::write(repo.join("other.txt"), "repo's\n").unwrap();
        std::fs::write(project.join("d/x.txt"), "mine\n").unwrap();
        git(&repo, &["add", "."]);
        git(&repo, &["commit", "-q", "-m", "first"]);
        let elsewhere = repo.parent().unwrap().join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::write(elsewhere.join("x.txt"), "theirs\n").unwrap();
        std::fs::write(repo.parent().unwrap().join("gone.txt"), "precious\n").unwrap();
        let ctx = context(&project, &kept);
        // Its repository's other files, a folder above it, and a folder of
        // its own made a link to one outside.
        let turn = ran(
            &ctx,
            "echo x > ../other.txt && rm ../../gone.txt && rm -rf d && ln -s ../../elsewhere d",
        );
        assert_eq!(
            turn.unkept.as_deref(),
            Some("links stay as the command left them: d")
        );
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            fates(&plan, &project),
            [("d/x.txt".to_owned(), Fate::Outside)]
        );
        let done = restore_in(&kept, &plan, Some(&project));
        assert_eq!(done.back, 0);
        assert_eq!(text(&elsewhere, "x.txt"), "theirs\n");
        assert_eq!(text(&repo, "other.txt"), "x\n");
        assert!(!repo.parent().unwrap().join("gone.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_submodule_a_command_removes_is_named() {
        let (project, kept) = repository("command-submodule", &[("a.txt", "one\n")]);
        let inner = project.parent().unwrap().join("inner");
        std::fs::create_dir_all(&inner).unwrap();
        git(&inner, &["init", "-q"]);
        std::fs::write(inner.join("s.txt"), "sub\n").unwrap();
        git(&inner, &["add", "."]);
        git(&inner, &["commit", "-q", "-m", "inner"]);
        let url = inner.to_str().unwrap();
        let add = [
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            url,
            "sub",
        ];
        git(&project, &add);
        git(&project, &["commit", "-q", "-m", "sub"]);
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, "rm -rf sub && echo two > a.txt");
        assert_eq!(
            turn.unkept.as_deref(),
            Some("folders and submodules stay as the command left them: sub")
        );
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(fates(&plan, &project), [("a.txt".to_owned(), Fate::Back)]);
    }

    #[cfg(unix)]
    #[test]
    fn many_long_paths_are_asked_about_in_parts() {
        let (project, kept) = scratch("command-long-paths");
        git(&project, &["init", "-q"]);
        // Each near 250 bytes, three hundred of them: over one call's worth.
        let folder = "f".repeat(120);
        std::fs::create_dir_all(project.join(&folder)).unwrap();
        let names: Vec<String> = (0..300)
            .map(|i| format!("{folder}/{i:03}{}", "n".repeat(120)))
            .collect();
        for name in &names {
            std::fs::write(project.join(name), name.as_bytes()).unwrap();
        }
        git(&project, &["add", "."]);
        git(&project, &["commit", "-q", "-m", "first"]);
        let ctx = context(&project, &kept);
        let turn = ran(&ctx, &format!("rm -rf {folder}"));
        assert_eq!(turn.unkept.as_deref(), Some(""));
        assert_eq!(turn.files.len(), 300);
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(restore_in(&kept, &plan, Some(&project)).back, 300);
        for name in &names {
            assert_eq!(&text(&project, name), name);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_saved_while_a_command_runs_is_listed_as_the_commands() {
        let (project, kept) = repository("command-midway", &[("a.txt", "one\n")]);
        let ctx = context(&project, &kept);
        let path = project.join("a.txt");
        let saver = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(1));
            std::fs::write(path, "saved\n").unwrap();
        });
        let turn = ran(&ctx, "sleep 3 && echo x > own.txt");
        saver.join().unwrap();
        // It cannot be told from the command's own change.
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(
            listing(&plan, Some(&project))[0],
            "a.txt goes back to how it was (changed by a command)"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_run_nobody_watches_walks_no_folder_outside_git() {
        let nobody = crate::chat::Connection {
            unattended: true,
            ..Default::default()
        };
        let unwatched = |project: &Path, kept: &Path| ToolContext {
            files: Log {
                dir: Some(kept.to_owned()),
                ..Log::default()
            },
            workspace: Some(project.to_owned()),
            ..ToolContext::of(&nobody)
        };
        let (project, kept) = scratch("command-unwatched");
        std::fs::write(project.join("a.txt"), "one\n").unwrap();
        let turn = ran(&unwatched(&project, &kept), "echo two > a.txt");
        assert!(turn.files.is_empty());
        assert_eq!(
            turn.unkept.as_deref(),
            Some("not undoable: outside git, a run nobody watches keeps no copies")
        );
        assert!(
            std::fs::read_dir(&kept)
                .into_iter()
                .flatten()
                .next()
                .is_none()
        );
        // In git it still keeps what git names, which copies next to nothing.
        let (project, kept) = repository("command-unwatched-git", &[("a.txt", "one\n")]);
        let turn = ran(&unwatched(&project, &kept), "echo two > a.txt");
        assert_eq!(turn.unkept.as_deref(), Some(""));
        let plan = plan_in(&kept, &[turn], Some(&project));
        assert_eq!(fates(&plan, &project), [("a.txt".to_owned(), Fate::Back)]);
    }
}
