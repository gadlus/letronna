//! What a command changed in the project's files, noted so a rewind puts it
//! back as it puts back what the file tools wrote (see `history`).
//!
//! The project is looked at just before and just after a foreground
//! run_command the risk check does not find read-only. In a git work tree
//! git says where to look: before, the commit checked out and the files
//! that differ from it, whose bytes are kept; after, each file the command
//! created, changed or removed is noted on the call's result as a
//! [`FileChange`], its old bytes being the kept copy, or the commit's own
//! for a file that matched it. That is a few git calls and a handful of
//! copies however big the project is. The commit's bytes are not what was
//! on disk for a file a `filter` attribute names -- Git LFS, git-crypt, a
//! notebook stripper -- so such a file's change is said not kept. A
//! command that moved HEAD -- a checkout of another branch, a pull, a
//! commit -- has nothing noted: its files put back without the branch
//! would be a half state, so its row says to go back with git.
//!
//! Outside git, or in a folder its repository ignores, the folder is
//! walked as `search` walks it, every file of a megabyte or less read and
//! kept the first time and after that only the ones whose size or time
//! changed. A folder too big for that is not looked at, and the command's
//! row says so.
//!
//! Neither looks at git's own folder, inside submodules, what the project
//! ignores or anything outside the project, so a command's work on
//! branches, stashes and the like is git's to take back. A new file that
//! looks older than the command -- moved, or copied or unpacked with its
//! times kept -- may have been there all along, ignored, so it is left and
//! named; unless it holds what a file the command removed held, when it is
//! taken for that file moved and goes as that one comes back. A file the
//! user saves, or a job writes, while the command runs cannot be told from
//! the command's own change, and is noted as one; a rewind then lists it as
//! changed by a command. Copies come out of an allowance of their own,
//! `history::COMMAND_CAP`, so commands never use up the room the file
//! tools' undo relies on.
//!
//! A rewind writes only plain files, so a link, a folder or a submodule
//! the command made, changed or removed, and a file whose mode alone it
//! changed, is named rather than put back. A file it does put back gets
//! its mode back too: its own, or for one that matched the commit, the
//! mode git would check it out with. A walk outside git copies the folder,
//! so a run nobody watches -- a schedule's, a chat app's -- does not walk.

use crate::cancel::Cancel;
use crate::git::{Checkout, Entry, LINK, PLAIN, PROGRAM};
use crate::history::{FILE_CAP, FileChange, Log};
use crate::tools::ToolContext;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::Metadata;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime};

/// Files looked at before a command, past which nothing is noted: a
/// project's own sources and its uncommitted work run to hundreds, and
/// reading thousands before every command would hold each one up.
const WATCH_FILES: usize = 5_000;

/// What the files kept before a command may add up to, for the same reason.
const WATCH_BYTES: u64 = 50 * 1024 * 1024;

/// What the files read after a command may add up to, past which the rest
/// are not kept: a changed file is read as it is now, and one that matched
/// the commit out of the commit as well, so twice what is kept before.
const AFTER_BYTES: u64 = 2 * WATCH_BYTES;

/// The largest file kept outside git, where every file is copied once:
/// sources are well under a megabyte, and a data file over it would use up
/// the allowance before the sources were in.
const WALK_FILE: u64 = 1024 * 1024;

/// More files changed by one command than this and none are noted: a
/// rewind's question could not show them, and a change that big -- a
/// vendored tree, a generated folder -- is one to take back with git.
const CHANGED_MAX: usize = 500;

/// How much older than the command a file not seen before may look and
/// still count as one it made: a file's time can lag the clock read here by
/// a tick of the kernel's coarser one, a few milliseconds. On a disk that
/// keeps times coarser still, FAT's two seconds, a new file is missed
/// rather than an old one taken for new and removed by a rewind.
const SLACK: Duration = Duration::from_millis(100);

/// Files a line about them names before it counts the rest, enough to
/// place a change without the line running on.
const NAMED: usize = 3;

/// Said on the row of a command that moved HEAD, which has nothing noted.
const MOVED_HEAD: &str = "moved git HEAD; go back with git";
/// Said when git could not say what changed.
const GIT_FAILED: &str = "not undoable: git could not list the changes";
/// Said when the command went on in the background at its limit.
const LATER: &str = "later changes not undoable";
/// Said when the call was stopped before the project was looked at.
const STOPPED: &str = "not undoable: stopped before its files were looked at";
/// Said outside git in a run nobody watches, which does not walk.
const UNWATCHED: &str = "not undoable: outside git, a run nobody watches keeps no copies";

/// Said when there were more than [`WATCH_FILES`] files of a kind to look
/// at, `what` naming them.
fn too_many(what: &str) -> String {
    format!("not undoable: over {WATCH_FILES} {what}")
}

/// Said when the files to keep before a command were over [`WATCH_BYTES`],
/// `what` naming them.
fn too_big(what: &str) -> String {
    format!("not undoable: {what} over {} MB", WATCH_BYTES / 1024 / 1024)
}

/// A file as it was when last looked at.
#[derive(Clone)]
struct Seen {
    len: u64,
    modified: Option<SystemTime>,
    /// Its permission bits, where the system keeps them.
    mode: Option<u32>,
    /// `None` for a file too big to keep, or one that could not be read.
    hash: Option<String>,
}

/// What was at a path before the command.
enum Was {
    /// Nothing: git listed it as removed.
    Absent,
    File(Seen),
    /// A link, to where it pointed: a rewind makes no links.
    Link(Option<PathBuf>),
    /// A folder git lists whole: a repository of its own, or a submodule.
    Folder,
    /// Something else no file tool writes -- a socket, a pipe -- left out.
    Odd,
}

/// The files each session has kept, by the folder its copies are in, as
/// they were when kept: one whose size and time have not changed since is
/// neither read nor copied again.
static KEPT: Mutex<BTreeMap<PathBuf, BTreeMap<PathBuf, Seen>>> = Mutex::new(BTreeMap::new());

/// A project as a command found it, to tell after what the command changed.
pub(crate) struct Watch {
    log: Log,
    started: SystemTime,
    how: How,
}

/// What was looked at before the command, to compare with after it.
enum How {
    Git {
        checkout: Checkout,
        cwd: PathBuf,
        /// The files that differed from the commit, by path from its top.
        before: HashMap<String, Was>,
    },
    Walk {
        cwd: PathBuf,
        before: HashMap<PathBuf, Was>,
    },
    /// Nothing is looked at, for the reason given.
    Not(String),
}

/// Look at the project in `cwd` before `command` runs in `dir`, inside it,
/// when the call's log keeps copies; a command that only reads is noted as
/// changing nothing.
pub(crate) fn watch(command: &str, dir: &Path, cwd: &Path, ctx: &ToolContext) -> Option<Watch> {
    let log = &ctx.files;
    log.dir()?;
    let risk = crate::command::assess(command, crate::command::Dialect::here(), Some(dir)).risk;
    if risk == crate::command::Risk::ReadOnly {
        log.command(Vec::new(), String::new());
        return None;
    }
    let unwatched = ctx
        .caller
        .connection
        .as_ref()
        .is_some_and(|connection| connection.unattended);
    let outside_git = |cwd: &Path| {
        if unwatched {
            How::Not(UNWATCHED.into())
        } else {
            walk_before(cwd, log, &ctx.cancel)
        }
    };
    let how = match Checkout::of(cwd) {
        Ok(checkout) => match checkout.ignores(cwd) {
            Ok(false) => git_before(checkout, cwd, log, &ctx.cancel),
            // Git lists none of its files, as it would for one outside.
            Ok(true) => outside_git(cwd),
            Err(_) => How::Not(GIT_FAILED.into()),
        },
        // Git failing in a repository, over who owns it say, is not a
        // folder outside one: a walk would miss a command moving HEAD.
        Err(_) if cwd.ancestors().any(|up| up.join(".git").exists()) => How::Not(GIT_FAILED.into()),
        Err(_) => outside_git(cwd),
    };
    Some(Watch {
        log: log.clone(),
        // Taken once the look is over, just before the command starts.
        started: SystemTime::now(),
        how,
    })
}

impl Watch {
    /// Note on the log what the command changed since it was watched; it
    /// went on in the background when `moved`.
    pub(crate) fn after(self, moved: bool) {
        let found = match self.how {
            How::Not(why) => Err(why),
            How::Git {
                checkout,
                cwd,
                before,
            } => git_after(&checkout, &cwd, &before, self.started, &self.log),
            How::Walk { cwd, before } => walk_after(&cwd, &before, self.started),
        };
        let (changes, mut unkept) = match found {
            Ok(noted) => noted.done(&self.log),
            Err(why) => (Vec::new(), vec![why]),
        };
        if moved {
            unkept.push(LATER.to_owned());
        }
        self.log.command(changes, unkept.join("; "));
    }
}

/// The files git lists as uncommitted under `cwd`, kept, unless the call
/// is stopped first.
fn git_before(checkout: Checkout, cwd: &Path, log: &Log, cancel: &Cancel) -> How {
    let listed = match checkout.listed(cwd, WATCH_FILES) {
        Ok(Some(listed)) => listed,
        Ok(None) => return How::Not(too_many("uncommitted files")),
        Err(_) => return How::Not(GIT_FAILED.into()),
    };
    let found: Vec<(String, Option<Metadata>)> = listed
        .into_iter()
        .map(|file| {
            let meta = now_at(&checkout.top, &checkout.top.join(&file.path));
            (file.path, meta)
        })
        .collect();
    let metas = found.iter().filter_map(|(_, meta)| meta.as_ref());
    if kept_bytes(metas, FILE_CAP) > WATCH_BYTES {
        return How::Not(too_big("uncommitted files"));
    }
    let mut before = HashMap::new();
    for (path, meta) in found {
        if cancel.is_set() {
            return How::Not(STOPPED.into());
        }
        let was = match meta {
            None => Was::Absent,
            Some(meta) => look(log, &checkout.top.join(&path), &meta, FILE_CAP),
        };
        before.insert(path, was);
    }
    How::Git {
        checkout,
        cwd: cwd.to_owned(),
        before,
    }
}

/// Every file under `cwd` the project does not ignore, kept, unless the
/// call is stopped first.
fn walk_before(cwd: &Path, log: &Log, cancel: &Cancel) -> How {
    let found = match walk(cwd, cancel) {
        Ok(found) => found,
        Err(why) => return How::Not(why),
    };
    if kept_bytes(found.iter().map(|(_, meta)| meta), WALK_FILE) > WATCH_BYTES {
        return How::Not(too_big("files outside git"));
    }
    let mut before = HashMap::new();
    for (path, meta) in found {
        if cancel.is_set() {
            return How::Not(STOPPED.into());
        }
        let was = look(log, &path, &meta, WALK_FILE);
        before.insert(path, was);
    }
    How::Walk {
        cwd: cwd.to_owned(),
        before,
    }
}

/// What the files of `metas` that are kept, those of `cap` bytes or less,
/// add up to.
fn kept_bytes<'a>(metas: impl Iterator<Item = &'a Metadata>, cap: u64) -> u64 {
    metas
        .filter(|meta| meta.is_file() && meta.len() <= cap)
        .map(Metadata::len)
        .sum()
}

/// Everything under `cwd` but folders, as `search` finds it but with git's
/// own folder left out, or why not: more than [`WATCH_FILES`], or the call
/// was stopped.
fn walk(cwd: &Path, cancel: &Cancel) -> Result<Vec<(PathBuf, Metadata)>, String> {
    let mut walk = ignore::WalkBuilder::new(cwd);
    // Hidden files are the project's too, but `hidden(false)` alone would
    // take git's own folder in, which a command's own git call rewrites.
    walk.hidden(false)
        .require_git(false)
        .parents(false)
        .filter_entry(|entry| entry.file_name() != ".git");
    let mut found = Vec::new();
    for entry in walk.build().flatten() {
        if cancel.is_set() {
            return Err(STOPPED.into());
        }
        if entry.file_type().is_none_or(|kind| kind.is_dir()) {
            continue;
        }
        // Of a link itself, as the walk follows none.
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        found.push((entry.into_path(), meta));
        if found.len() > WATCH_FILES {
            return Err(too_many("files outside git"));
        }
    }
    Ok(found)
}

/// Let go of what the session whose copies are in `dir` was found to keep,
/// as the session goes.
pub(crate) fn forget(dir: &Path) {
    lock_kept().remove(dir);
}

fn lock_kept() -> std::sync::MutexGuard<'static, BTreeMap<PathBuf, BTreeMap<PathBuf, Seen>>> {
    // Nothing under this lock panics, so a poisoned one is still whole.
    KEPT.lock().unwrap_or_else(PoisonError::into_inner)
}

/// What is at `path`, a file's bytes kept when it is `cap` or smaller,
/// unless it was kept as it is already.
fn look(log: &Log, path: &Path, meta: &Metadata, cap: u64) -> Was {
    if meta.is_symlink() {
        return Was::Link(std::fs::read_link(path).ok());
    }
    if meta.is_dir() {
        return Was::Folder;
    }
    if !meta.is_file() {
        return Was::Odd;
    }
    let dir = log.dir().map(Path::to_path_buf).unwrap_or_default();
    let kept = lock_kept()
        .get(&dir)
        .and_then(|kept| kept.get(path))
        .filter(|seen| same(seen, meta))
        .cloned();
    // The copy itself may be gone since, with an earlier session that had
    // the same folder.
    let kept = kept.filter(|seen| {
        seen.hash
            .as_ref()
            .is_some_and(|hash| dir.join(hash).is_file())
    });
    if let Some(seen) = kept {
        return Was::File(seen);
    }
    // Read with the lock let go, as another session may be looking too.
    let bytes = (meta.len() <= cap).then(|| std::fs::read(path).ok());
    let seen = Seen {
        len: meta.len(),
        modified: meta.modified().ok(),
        mode: mode_of(meta),
        hash: bytes.flatten().map(|bytes| log.keep_bytes(&bytes)),
    };
    if seen.hash.is_some() && seen.modified.is_some() {
        let mut kept = lock_kept();
        kept.entry(dir)
            .or_default()
            .insert(path.to_owned(), seen.clone());
    }
    Was::File(seen)
}

/// Whether the file `meta` describes looks as it did when `seen`.
fn same(seen: &Seen, meta: &Metadata) -> bool {
    seen.len == meta.len()
        && seen.modified.is_some()
        && seen.modified == meta.modified().ok()
        && seen.mode == mode_of(meta)
}

/// What is at `path`, below `top`, unless a folder on the way down to it is
/// a link, when what the path reaches is not the project's, and it counts
/// as gone: nothing outside the project is read, and a rewind leaves it.
fn now_at(top: &Path, path: &Path) -> Option<Metadata> {
    if behind_link(top, path) {
        return None;
    }
    std::fs::symlink_metadata(path).ok()
}

/// Whether a folder on the way down from `top` to `path` is a link.
fn behind_link(top: &Path, path: &Path) -> bool {
    path.ancestors()
        .skip(1)
        .take_while(|up| *up != top)
        .any(|up| std::fs::symlink_metadata(up).is_ok_and(|meta| meta.is_symlink()))
}

/// Whether what is at `path` now, as `now` says, is as `was` found it.
fn unchanged(was: &Was, path: &Path, now: Option<&Metadata>) -> bool {
    match (was, now) {
        (Was::Absent, None) => true,
        (Was::File(seen), Some(meta)) => meta.is_file() && same(seen, meta),
        (Was::Link(to), Some(meta)) => meta.is_symlink() && std::fs::read_link(path).ok() == *to,
        (Was::Folder, Some(meta)) => meta.is_dir(),
        // A socket or a pipe that comes and goes is no change of the
        // project's, but a file written in its place is.
        (Was::Odd, now) => now.is_none_or(|meta| !meta.is_file()),
        _ => false,
    }
}

/// The permission bits of the file `meta` describes, where the system
/// keeps them.
fn mode_of(meta: &Metadata) -> Option<u32> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        Some(meta.permissions().mode() & 0o777)
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        None
    }
}

/// Whether the file `meta` describes has the mode `was`, as
/// [`FileChange::mode`] says it: all its bits, or for a mode git gave, only
/// whether it runs, which is all git keeps.
fn same_mode(was: u32, meta: &Metadata) -> bool {
    let Some(now) = mode_of(meta) else {
        return true;
    };
    match was & !0o777 {
        0 => now == was,
        _ => now & 0o100 == was & 0o100,
    }
}

/// Whether a file not seen before, `meta`, was made by the command rather
/// than there before it, unlooked at because ignored: its birth time where
/// the disk keeps one, else its last change, is no older than the command.
/// A file a command unpacks with its old times kept is taken for one that
/// was there, so a rewind leaves it.
fn made_since(meta: &Metadata, started: SystemTime) -> bool {
    let made = meta.created().or_else(|_| meta.modified());
    made.is_ok_and(|made| made + SLACK >= started)
}

/// What the files a command changed are, or why none are noted.
type Found = Result<Noted, String>;

/// What the look after a command found.
#[derive(Default)]
struct Noted {
    /// The folder the paths are below.
    top: PathBuf,
    changes: Vec<FileChange>,
    /// The changed files whose change could not be kept, by name.
    unkept: Vec<String>,
    /// The new files that look older than the command, by name and real
    /// path, with what each holds when it could be read.
    older: Vec<(String, PathBuf, Option<String>)>,
    /// Links the command made, changed or removed, and files it made
    /// links of, by name: a rewind makes no links and follows none.
    links: Vec<String>,
    /// Files it made folders of, and folders git lists whole -- another
    /// repository, a submodule -- it changed or removed, by name.
    folders: Vec<String>,
    /// Files whose mode alone it changed, by name.
    modes: Vec<String>,
    /// What has been read, against [`AFTER_BYTES`].
    read: u64,
}

impl Noted {
    /// Whether `len` bytes more may be read, counting them when they may.
    fn may_read(&mut self, len: u64) -> bool {
        if self.read + len > AFTER_BYTES {
            return false;
        }
        self.read += len;
        true
    }

    /// The hash of what the file at `path`, as `meta` describes it, holds,
    /// or `None` when it is too big, past what may be read or unreadable.
    fn hash_of(&mut self, path: &Path, meta: &Metadata) -> Option<String> {
        if meta.len() > FILE_CAP || !self.may_read(meta.len()) {
            return None;
        }
        std::fs::read(path)
            .ok()
            .map(|bytes| crate::history::hash(&bytes))
    }

    /// Name `name` as left as the command left it when what is there now,
    /// as `now` says, is a link or a folder, which a rewind does not
    /// replace; whether it did.
    fn not_plain(&mut self, name: &str, now: Option<&Metadata>) -> bool {
        let Some(meta) = now.filter(|meta| !meta.is_file()) else {
            return false;
        };
        if meta.is_symlink() {
            self.links.push(name.to_owned());
        } else if meta.is_dir() {
            self.folders.push(name.to_owned());
        }
        true
    }

    /// Note that the file `path`, named `name`, which held `from`
    /// (`Some(None)` for nothing, `None` when that is not known) with the
    /// mode `mode`, as [`FileChange::mode`] says it, is now as `now` says,
    /// or name it as not kept when that cannot be told.
    fn note(
        &mut self,
        path: &Path,
        name: String,
        from: Option<Option<String>>,
        mode: Option<u32>,
        now: Option<&Metadata>,
    ) {
        let Some(before) = from else {
            self.unkept.push(name);
            return;
        };
        let after = match now {
            None => Some(String::new()),
            Some(meta) => self.hash_of(path, meta),
        };
        let Some(after) = after else {
            self.unkept.push(name);
            return;
        };
        let moded = now.is_some_and(|meta| mode.is_some_and(|mode| !same_mode(mode, meta)));
        // Written over with what it held: no change to undo, but for a mode,
        // which a rewind does not change on its own.
        if before.as_deref().unwrap_or_default() == after {
            if moded {
                self.modes.push(name);
            }
            return;
        }
        self.changes.push(FileChange {
            path: real(&self.top, path),
            before,
            after,
            // Set again only where the rewind would get it wrong: a file it
            // makes anew, or one whose mode the command changed.
            mode: mode.filter(|_| now.is_none() || moded),
            made: crate::history::stamp(),
        });
    }

    /// Set aside `path`, named `name`, a new file that looks older than the
    /// command, as `meta` says, to be left unless the command moved it.
    fn older(&mut self, path: &Path, name: String, meta: &Metadata) {
        let hash = self.hash_of(path, meta);
        self.older.push((name, real(&self.top, path), hash));
    }

    /// The changes, with the files the command moved, and the lines saying
    /// what of them stays; `log` holds the copies.
    fn done(mut self, log: &Log) -> (Vec<FileChange>, Vec<String>) {
        // A move keeps a file's times, so it looks older than the command,
        // but it holds what a file the command removed held. Only one whose
        // copy was kept, as a rewind would otherwise take the moved file
        // away without bringing the other back.
        let kept = |hash: &String| log.dir().is_some_and(|dir| dir.join(hash).is_file());
        let gone: HashSet<String> = self
            .changes
            .iter()
            .filter(|change| change.after.is_empty())
            .filter_map(|change| change.before.clone())
            .filter(kept)
            .collect();
        let mut left = Vec::new();
        for (name, path, hash) in self.older {
            match hash {
                Some(after) if gone.contains(&after) => self.changes.push(FileChange {
                    path,
                    before: None,
                    after,
                    mode: None,
                    made: crate::history::stamp(),
                }),
                _ => left.push(name),
            }
        }
        let mut lines = Vec::new();
        if !self.unkept.is_empty() {
            lines.push(format!("{} could not be kept", named(&self.unkept)));
        }
        match left.len() {
            0 => {}
            1 => lines.push(format!(
                "left {}: it looks older than the command",
                named(&left)
            )),
            _ => lines.push(format!(
                "left {}: they look older than the command",
                named(&left)
            )),
        }
        let stay = [
            ("links", &self.links),
            ("folders and submodules", &self.folders),
            ("modes", &self.modes),
        ];
        for (what, names) in stay {
            if !names.is_empty() {
                lines.push(format!(
                    "{what} stay as the command left them: {}",
                    named(names)
                ));
            }
        }
        (self.changes, lines)
    }
}

/// `names` as a line names them: the first few, then how many more.
fn named(names: &[String]) -> String {
    let shown = names[..names.len().min(NAMED)].join(", ");
    match names.len().saturating_sub(NAMED) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    }
}

/// Said when a command changed more than [`CHANGED_MAX`] files.
fn too_many_changed() -> String {
    format!("not undoable: it changed over {CHANGED_MAX} files")
}

fn git_after(
    checkout: &Checkout,
    cwd: &Path,
    before: &HashMap<String, Was>,
    started: SystemTime,
    log: &Log,
) -> Found {
    if crate::git::head(cwd) != checkout.head {
        return Err(MOVED_HEAD.into());
    }
    let listed = match checkout.listed(cwd, WATCH_FILES) {
        Ok(Some(listed)) => listed,
        Ok(None) => return Err(too_many("uncommitted files after it")),
        Err(_) => return Err(GIT_FAILED.into()),
    };
    // A path git only lists as untracked was in neither the commit nor the
    // index; one listed as removed from the index and untracked was.
    let mut untracked: HashMap<&str, bool> = HashMap::new();
    for file in &listed {
        *untracked.entry(&file.path).or_insert(true) &= file.untracked;
    }
    let paths: BTreeSet<&str> = before
        .keys()
        .map(String::as_str)
        .chain(untracked.keys().copied())
        .collect();
    let mut changed = Vec::new();
    for path in paths {
        let full = checkout.top.join(path);
        let now = now_at(&checkout.top, &full);
        let was = before.get(path);
        if !was.is_some_and(|was| unchanged(was, &full, now.as_ref())) {
            changed.push((full, path, was, now));
        }
    }
    if changed.len() > CHANGED_MAX {
        return Err(too_many_changed());
    }
    // Tracked files not listed before matched the commit, or were not in
    // it at all when the command added them to the index.
    let clean: Vec<&str> = changed
        .iter()
        .filter(|(_, path, was, _)| was.is_none() && untracked.get(path) != Some(&true))
        .map(|(_, path, _, _)| *path)
        .collect();
    let tree = checkout
        .in_commit(&clean)
        .map_err(|_| GIT_FAILED.to_owned())?;
    let filtered = checkout.filtered(&clean).ok();
    let mut noted = Noted {
        top: checkout.top.clone(),
        ..Noted::default()
    };
    // The commit's bytes for the plain files among them that are gone or
    // still plain files and may be read, in one go; a filter stands between
    // the commit and the disk for some.
    let readable: Vec<(&str, &Entry)> = changed
        .iter()
        .filter(|(_, _, was, now)| was.is_none() && now.as_ref().is_none_or(Metadata::is_file))
        .filter_map(|(_, path, _, _)| Some((*path, tree.get(*path)?)))
        .filter(|(path, entry)| {
            matches!(entry.mode, PLAIN | PROGRAM)
                && filtered.as_ref().is_some_and(|set| !set.contains(*path))
                && entry
                    .size
                    .is_some_and(|size| size <= FILE_CAP && noted.may_read(size))
        })
        .collect();
    // Line endings written out may double a file, and each comes after a
    // line naming it.
    let cap = readable
        .iter()
        .map(|(_, entry)| 2 * entry.size.unwrap_or(0) + 128)
        .sum::<u64>();
    let committed = if readable.is_empty() {
        HashMap::new()
    } else {
        checkout
            .committed(&readable, cap as usize)
            .unwrap_or_default()
    };
    for (full, path, was, now) in changed {
        // Named from the project folder, as a rewind names files.
        let name = path
            .strip_prefix(&checkout.prefix)
            .unwrap_or(path)
            .to_owned();
        let in_commit = tree.get(path);
        let new = untracked.get(path) == Some(&true) || (was.is_none() && in_commit.is_none());
        match was {
            Some(Was::Link(_)) => {
                noted.links.push(name);
                continue;
            }
            Some(Was::Folder) => {
                noted.folders.push(name);
                continue;
            }
            _ if noted.not_plain(&name, now.as_ref()) => continue,
            _ => {}
        }
        let (from, mode) = match was {
            Some(Was::File(seen)) => (seen.hash.clone().map(Some), seen.mode),
            Some(_) => (Some(None), None),
            // In neither the commit nor the listing before: made by the
            // command, or there all along but ignored until the command
            // changed what is, which leaves it as it was.
            None if new => match &now {
                Some(meta) if made_since(meta, started) => (Some(None), None),
                Some(meta) => {
                    noted.older(&full, name, meta);
                    continue;
                }
                None => continue,
            },
            // It matched the commit, so the commit holds what it was, but
            // for a file a filter stands between the two for.
            None => match in_commit {
                Some(entry) if matches!(entry.mode, PLAIN | PROGRAM) => {
                    let bytes = committed.get(path);
                    let from = bytes.map(|bytes| Some(log.keep_bytes(bytes)));
                    (from, Some(entry.mode))
                }
                // A link, or a submodule, which git lists when it is gone
                // or on another commit.
                Some(entry) => {
                    match entry.mode {
                        LINK => noted.links.push(name),
                        _ => noted.folders.push(name),
                    }
                    continue;
                }
                None => (None, None),
            },
        };
        noted.note(&full, name, from, mode, now.as_ref());
    }
    Ok(noted)
}

fn walk_after(cwd: &Path, before: &HashMap<PathBuf, Was>, started: SystemTime) -> Found {
    // Not stopped by the call's stop, which also ends the command: what it
    // changed before it was stopped is what a rewind is most wanted for.
    let found = walk(cwd, &Cancel::default())
        .map_err(|_| format!("not undoable: the folder grew past {WATCH_FILES} files"))?;
    let now: HashMap<PathBuf, Metadata> = found.into_iter().collect();
    let mut changed: Vec<(&PathBuf, Option<&Was>, Option<Metadata>)> = Vec::new();
    for (path, was) in before {
        // One the walk no longer finds may be a folder now.
        let meta = match now.get(path) {
            Some(meta) => Some(meta.clone()),
            None => now_at(cwd, path),
        };
        if !unchanged(was, path, meta.as_ref()) {
            changed.push((path, Some(was), meta));
        }
    }
    for (path, meta) in &now {
        if !before.contains_key(path) {
            changed.push((path, None, Some(meta.clone())));
        }
    }
    if changed.len() > CHANGED_MAX {
        return Err(too_many_changed());
    }
    // In the order git would list them, so a rewind lists them so too.
    changed.sort_by(|a, b| a.0.cmp(b.0));
    let mut noted = Noted {
        top: cwd.to_owned(),
        ..Noted::default()
    };
    for (path, was, meta) in changed {
        let name = path.strip_prefix(cwd).unwrap_or(path).display().to_string();
        match was {
            Some(Was::Link(_)) => {
                noted.links.push(name);
                continue;
            }
            Some(Was::Folder) => {
                noted.folders.push(name);
                continue;
            }
            _ if noted.not_plain(&name, meta.as_ref()) => continue,
            _ => {}
        }
        let (from, mode) = match was {
            Some(Was::File(seen)) => (seen.hash.clone().map(Some), seen.mode),
            Some(_) => (Some(None), None),
            // As in git: one older than the command may have been there,
            // ignored. A socket or a pipe a command makes is not looked at.
            None => match &meta {
                Some(meta) if !meta.is_file() => continue,
                Some(meta) if made_since(meta, started) => (Some(None), None),
                Some(meta) => {
                    noted.older(path, name, meta);
                    continue;
                }
                None => continue,
            },
        };
        noted.note(path, name, from, mode, meta.as_ref());
    }
    Ok(noted)
}

/// `path`, below `top`, as the file tools note it: its real path, or once
/// it is gone, the real path of the nearest folder above it still there,
/// with the rest of the way down. Below a folder that is now a link, it is
/// the real path of `top` with the rest by name: what the link leads to is
/// not where the file was, and a rewind then finds it outside.
fn real(top: &Path, path: &Path) -> PathBuf {
    if behind_link(top, path) {
        let below = path.strip_prefix(top).unwrap_or(path);
        return std::fs::canonicalize(top).map_or_else(|_| path.to_owned(), |top| top.join(below));
    }
    let mut rest = Vec::new();
    for up in path.ancestors() {
        if let Ok(real) = std::fs::canonicalize(up) {
            return rest.iter().rev().fold(real, |real, name| real.join(name));
        }
        let Some(name) = up.file_name() else {
            break;
        };
        rest.push(name);
    }
    path.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_look_stopped_before_it_is_done_says_so() {
        let dir =
            std::env::temp_dir().join(format!("letronna-snapshot-{}-stop", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        let stopped = Cancel::default();
        stopped.set();
        let log = Log::default();
        let said = |how: How| match how {
            How::Not(why) => why,
            _ => "looked".into(),
        };
        assert_eq!(said(walk_before(&dir, &log, &stopped)), STOPPED);
        // In git, while it keeps what git lists.
        let init = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&dir)
            .status();
        assert!(init.unwrap().success());
        let checkout = Checkout::of(&dir).unwrap();
        assert_eq!(said(git_before(checkout, &dir, &log, &stopped)), STOPPED);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_session_that_goes_lets_go_of_what_it_kept() {
        let dir = PathBuf::from(format!("/letronna-gone-{}", std::process::id()));
        let seen = Seen {
            len: 1,
            modified: Some(SystemTime::now()),
            mode: None,
            hash: Some("h".into()),
        };
        lock_kept()
            .entry(dir.clone())
            .or_default()
            .insert(dir.join("a.txt"), seen);
        forget(&dir);
        assert!(!lock_kept().contains_key(&dir));
    }
}
