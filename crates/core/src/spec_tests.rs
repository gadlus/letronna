//! Tests of a run's work written by a helper that has not seen it, from the
//! request's words alone, and kept out of the project once they have run.
//!
//! A run checks its work with tests it writes itself, and the review helpers
//! started for it read its change, so each tends to share the run's reading
//! of the request: a case it misread is one they test the way it was built. A
//! tester given only the request, word for word, and told to read the code
//! only to learn how to call it, writes each test from what the words say
//! should happen, set up the way the project's own tests are.
//!
//! It works in a copy of the project where one can be made, so its tests,
//! half written or failing on purpose, never meet the reviews running the
//! project's suite beside it. Its files carry `MARKER` in their names, so
//! they can be told from the project's own, and are moved to a folder of
//! their own as soon as it answers: a run stopped at its deadline would
//! otherwise leave them in a patch taken of it, where one that fails can fail
//! a package's whole suite. The run is told where they are kept and puts them
//! back to run them, and what it put back is deleted as it ends.
//!
//! Once the run has acted on the tester's report and says it is done, the
//! driver itself runs those tests against the work as it stands, in a fresh
//! copy of the project with the files put back, and sends the run back with
//! what the runner printed: a report can be read and set aside, an output
//! cannot, and a tester that could not run its tests where it worked still
//! has them run.
//!
//! Where the project as its last commit holds it can be written out, the
//! tester starts with the run instead of at its first done, in that folder
//! (`base`), with the project's package stores and build caches linked in
//! (`STORES`) and nothing the work could have written, so its tests can be
//! parsed and their imports resolved: the work never reaches it, so it
//! cannot read the work however it is told, and it has its tests ready when
//! the run first says it is done rather than taking the end of the run's
//! time to write them. Its tests have met nothing but the project as it
//! was, so the driver runs them against the work there and then, and the
//! run hears what they make of it with the requirements notice. A tester
//! started with the run is kept in `pending` until it is taken: at the run's
//! first done, waited for a while, or at a later done once it has answered;
//! one still at work when the run ends is stopped and its folder deleted
//! (`tidy`).
//!
//! A run sent back with them `RUNS` times whose work still fails them has
//! had its say. A fresh attempt at a task that failed passes about a third
//! of the time, so where the project was committed as the run found it and
//! the time is left, a helper that has not seen the work makes the request
//! again from scratch, in such a folder (`base`), and the tests choose:
//! when its work passes them, ran some, and the first still does not pass,
//! the project is made to hold it, the first kept aside as a patch, and the
//! run is told to answer for the work as it stands. Tests that pass on the
//! project as it was choose nothing, so no attempt is made on them; nor
//! where the attempt's tests would import the first attempt's code through
//! an editable install; and an attempt that installed anything from its
//! folder is not taken, and its folder stays for what may lead into it.
//!
//! A copy of the work as it stood when the run first said it was done is
//! kept too, as its first complete version (`snapshot`): a review's row, or
//! the run's own doubt, can have it change a version that was right. When
//! the work the run ends with fails the tester's tests, they run in that
//! copy as well, and a first version that passes them names the files
//! changed since, for the run to put back what broke them. The tester's
//! command runs with the variables the run's own passing command of the
//! same program set, such as a module cache, so a tester that lacked the
//! run's set-up gets it, and a Python command runs with the copy first on
//! the import path, so an editable install of the project does not have it
//! test the project's code in place of the copy's (`with_import_path`). All
//! of it happens in copies in the temp folder, discarded as the run ends;
//! nothing is written into the project. The tester ends its report with
//! the one command that runs its tests, after its table, since a long
//! report is cut in the middle before the driver reads it.
//!
//! The project as it was serves one more check (`verdict`): a test command
//! of the run's own that failed, still fails in a fresh copy of the work, and
//! passes there found a regression, and the run is told so (`broken`); one
//! that ran no test (`ran_nothing`), or whose stores lead back into the
//! project (`imports_elsewhere`), proves nothing.

use crate::cancel::Cancel;
use crate::tools::ToolContext;
#[cfg(unix)]
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
#[cfg(any(unix, test))]
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What the name of each test file the tester writes holds.
pub(crate) const MARKER: &str = "letronna_spec";

/// Entries of the project walked at most for the tester's files, so a run in
/// a big folder still goes on at once.
const WALKED: usize = 100_000;

/// Folders of history, packages and builds, where no test of the tester's
/// belongs and which can hold more entries than the rest of the project.
const SKIPPED: [&str; 4] = [".git", "node_modules", "target", ".venv"];

/// Files and folders a copy of the project for the tester holds at most;
/// past that it works in the project itself. Only Unix makes a copy.
#[cfg(unix)]
const COPIED: usize = 50_000;

/// Kept test files the run's notice names at most; the rest are in the same
/// folder.
const NAMED: usize = 10;

/// How many times at most the driver runs the tester's tests against a run's
/// work, each time the run says it is done after their report: the fix is
/// checked once more, and a run that cannot make them pass by then has
/// quoted why they should not, or will not.
pub(crate) const RUNS: usize = 2;

/// How long the tester's tests may take when the driver runs them at most:
/// a test suite's worth; the driver gives less when less is left to the
/// run's deadline, which does not cut a command run for no conversation.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// How many suites' worth of time, `TIMEOUT` each, a run has to have left
/// before a command that rebuilds the whole workspace in a copy is compared
/// with the project as it was.
pub(crate) const REBUILD_SPARE: u32 = 3;

/// The most time a tester started with the run is given: enough to read
/// the project and write its tests, and well within the time a run that
/// says it is done early would wait for them.
const AHEAD: Duration = Duration::from_secs(20 * 60);

/// Of the time a run was `given`, how long a tester started with it has: a
/// third, up to `AHEAD`, since a run given long takes most of its time to
/// first say it is done, and one given little needs the rest for the fix.
pub(crate) fn time_ahead(given: Duration) -> Duration {
    (given / 3).min(AHEAD)
}

/// How long past its deadline a tester started with the run is still waited
/// for: the round in which it is asked to answer with what it has.
const GRACE: Duration = Duration::from_secs(2 * 60);

/// The least time the helper making a second attempt is given to its
/// deadline: a fresh attempt shorter than this rarely passes, and one
/// longer rarely fits in what is left after two verifications. A run needs
/// this, `AFTER_ATTEMPT` and the tests' own time left to start one.
pub(crate) const ATTEMPT_TIME: Duration = Duration::from_secs(15 * 60);

/// What a run keeps of its time past a second attempt's deadline: half for
/// the attempt's last word and the tests run against its work, half for the
/// run's own answer after them. The tests' time, as the first attempt's run
/// of them took, is kept on top of it.
pub(crate) const AFTER_ATTEMPT: Duration = Duration::from_secs(5 * 60);

/// The file a kept first attempt's changes to the last commit's files are
/// written to, beside the files it added.
const FIRST_PATCH: &str = "first.patch";

/// Folders of installed packages and build caches, by name, that a tester
/// which must not see the work, or a second attempt, can have linked into
/// the project as it was: what tests import from or build with, and nothing
/// the work writes, as a `dist/` or `build/` it built, a `*.egg-info` it
/// packaged or a `__pycache__` of its new modules would be.
const STORES: [&str; 9] = [
    "node_modules",
    ".venv",
    "venv",
    ".tox",
    "vendor",
    "target",
    ".gradle",
    ".m2",
    ".cache",
];

/// What a runner prints when it ran no test, in lower case: a pass that
/// says one of these chose nothing.
const VACUOUS: [&str; 9] = [
    "no tests ran",
    "0 passed",
    "collected 0 items",
    "no test files found",
    "no tests found",
    "no test files",
    "ran 0 tests",
    "0 tests completed",
    "tests: 0 total",
];

/// Files under `site-packages` and the folder of its files read at most
/// for a path into the project, which an editable install leaves there.
const EDITABLE_READ: u64 = 64 * 1024;

/// How deep under a linked store a `site-packages` folder is looked for.
const STORE_DEPTH: usize = 5;

/// Characters of the tests' output the run is shown at most, from its end,
/// where a runner sums up what failed.
const SHOWN: usize = 3_000;

/// Characters a `Run:` line's command may hold at most: one line of shell,
/// not a script.
const LINE: usize = 400;

/// Files that differ between a run's first complete version and its work as
/// it stands that the notice names at most; the rest are counted.
const DIFFERING: usize = 20;

/// Folders a test run or a build leaves behind, which hold no work of the
/// run's, so no file in them is one that differs.
const CACHES: [&str; 10] = [
    "__pycache__",
    "htmlcov",
    ".hypothesis",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".tox",
    ".nox",
    ".cache",
    ".eggs",
];

/// Variables of the run's own set-up lent to the tester's command at most.
const LENT: usize = 8;

/// Words in a variable's name that mark its value as a secret, which is not
/// lent: the command is written into a notice.
const SECRETS: [&str; 6] = ["KEY", "TOKEN", "SECRET", "PASSW", "CREDENTIAL", "AUTH"];

/// Variables never lent, as no `LD_*` or `*FLAGS` is: they say where
/// programs and libraries are found, or how a build goes, which the run may
/// have set for a build against what its tests need.
const UNLENT: [&str; 5] = ["PATH", "HOME", "CGO_ENABLED", "NODE_ENV", "PYTHONHOME"];

/// A tester's files kept for a run: its id, the project it puts them back
/// in, where they are kept, and the command the tester gave to run them
/// from the project's root, if it gave one.
struct Moved {
    run: u64,
    workspace: PathBuf,
    kept: Kept,
    command: Option<String>,
    /// Written by a tester started with the run, in the project as it was,
    /// so they have never met the work.
    unmet: bool,
}

/// The tester's files kept for each run so far, so what the run puts back
/// is deleted as it ends, and the folder they are kept in with them.
fn moved() -> &'static Mutex<Vec<Moved>> {
    static MOVED: OnceLock<Mutex<Vec<Moved>>> = OnceLock::new();
    MOVED.get_or_init(Default::default)
}

/// A tester started with a run, at work on a thread of its own: the folder
/// it works in, the stop that ends it, when it is to answer by, if it has a
/// deadline, and its report once it has answered.
struct Pending {
    run: u64,
    folder: PathBuf,
    cancel: Cancel,
    until: Option<Instant>,
    report: Option<Result<String, String>>,
}

/// The testers started with each run so far and not yet taken, and what
/// wakes a wait for one's report.
fn pending() -> &'static (Mutex<Vec<Pending>>, Condvar) {
    static PENDING: OnceLock<(Mutex<Vec<Pending>>, Condvar)> = OnceLock::new();
    PENDING.get_or_init(Default::default)
}

/// The tester's files, moved out of where it worked: the folder they are
/// kept in, and each one's path from the project, with `/` between folders.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Kept {
    pub dir: PathBuf,
    pub files: Vec<String>,
}

/// A run's first complete version: a copy of its work as it stood when it
/// first said it was done, and whether the tester's tests have run in it.
struct First {
    run: u64,
    dir: PathBuf,
    compared: bool,
}

/// A run's first complete version as `first` hands it out: where it is, and
/// whose it is, so a run of the tests in it can be marked.
#[derive(Debug, PartialEq)]
pub(crate) struct FirstVersion {
    run: u64,
    pub dir: PathBuf,
}

impl FirstVersion {
    /// Marks the tests as run in it, so they are not run there again.
    fn judged(&self) {
        if let Ok(mut firsts) = firsts().lock()
            && let Some(first) = firsts.iter_mut().find(|first| first.run == self.run)
        {
            first.compared = true;
        }
    }
}

/// Each run's first complete version, kept until `tidy` discards it.
fn firsts() -> &'static Mutex<Vec<First>> {
    static FIRSTS: OnceLock<Mutex<Vec<First>>> = OnceLock::new();
    FIRSTS.get_or_init(Default::default)
}

/// The folders kept aside for each run, such as the first attempt a second
/// took the place of, until `tidy` discards them.
fn asides() -> &'static Mutex<Vec<(u64, PathBuf)>> {
    static ASIDES: OnceLock<Mutex<Vec<(u64, PathBuf)>>> = OnceLock::new();
    ASIDES.get_or_init(Default::default)
}

/// Keeps `dir`, a folder of the temp folder made for run `run`, until `tidy`
/// deletes it with the run's other folders.
pub(crate) fn aside(run: u64, dir: &Path) {
    if let Ok(mut asides) = asides().lock() {
        asides.push((run, dir.to_path_buf()));
    }
}

/// Keeps a copy of run `run`'s work in `workspace` as it stands, the first
/// complete version it is at when the reviews start, for the tester's tests
/// to run in beside what the run ends with: a review's row, or the run's
/// own doubt, can have it change a version that was right. Nothing where no
/// copy can be made, or one is kept already.
pub(crate) fn snapshot(run: u64, workspace: &Path) {
    let kept = (firsts().lock()).is_ok_and(|firsts| firsts.iter().any(|first| first.run == run));
    if kept {
        return;
    }
    // Copied with no lock held: a big project takes a while.
    let Some(dir) = copy(workspace) else {
        return;
    };
    match firsts().lock() {
        Ok(mut firsts) => firsts.push(First {
            run,
            dir,
            compared: false,
        }),
        Err(_) => discard(&dir),
    }
}

/// Run `run`'s first complete version, for the tester's tests to run in
/// beside the work: None when none was kept, or they have run in it already,
/// so they run there once at most.
pub(crate) fn first(run: u64) -> Option<FirstVersion> {
    let firsts = firsts().lock().ok()?;
    let first = (firsts.iter()).find(|first| first.run == run && !first.compared)?;
    Some(FirstVersion {
        run,
        dir: first.dir.clone(),
    })
}

/// Where run `run`'s first complete version is kept, handed out or not.
#[cfg(test)]
pub(crate) fn snapshot_of(run: u64) -> Option<PathBuf> {
    let firsts = firsts().lock().ok()?;
    (firsts.iter().find(|first| first.run == run)).map(|first| first.dir.clone())
}

/// A new folder of the temp folder for `what`, named for this process.
fn folder(what: &str) -> PathBuf {
    let stamp = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
        .map_or(0, |since| since.as_nanos());
    std::env::temp_dir().join(format!(
        "{}-spec-{what}-{}-{stamp}",
        crate::BRAND.to_lowercase(),
        std::process::id()
    ))
}

/// What git printed in `dir` for `args`, split at each NUL or newline; None
/// when it could not run or failed.
#[cfg(any(unix, test))]
fn git(dir: &Path, args: &[&str]) -> Option<Vec<String>> {
    let printed = git_bytes(dir, args)?;
    let printed = String::from_utf8_lossy(&printed);
    let lines = printed.split(['\0', '\n']).filter(|line| !line.is_empty());
    Some(lines.map(str::to_owned).collect())
}

/// What git printed in `dir` for `args`, as it printed it; None when it
/// could not run or failed. It gets no input and may not prompt.
#[cfg(any(unix, test))]
fn git_bytes(dir: &Path, args: &[&str]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        // Set by a hook or a shell this started from, these would choose the
        // repository in place of `-C`.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    (output.status.success()).then_some(output.stdout)
}

/// A copy of `workspace` for the tester to work in, so its tests never meet
/// the reviews running the project's suite beside it: the files git tracks
/// and the new ones it does not ignore, and links to what it ignores, such
/// as installed packages and builds, which tests use as they are. None, and
/// the tester works in the project, where `workspace` is not the top of a
/// git project, past `COPIED` entries, or when one could not be copied.
#[cfg(unix)]
pub(crate) fn copy(workspace: &Path) -> Option<PathBuf> {
    top(workspace)?;
    let files = listed(workspace)?;
    let ignored = ignored(workspace)?;
    if files.len() + ignored.len() > COPIED {
        return None;
    }
    let to = folder("copy");
    let made = (|| {
        for file in &files {
            let (from, at) = (workspace.join(file), to.join(file));
            // A file the run deleted, or a path listed twice.
            let Ok(kind) = std::fs::symlink_metadata(&from) else {
                continue;
            };
            if std::fs::symlink_metadata(&at).is_ok() {
                continue;
            }
            std::fs::create_dir_all(at.parent()?).ok()?;
            match () {
                _ if kind.is_symlink() => {
                    std::os::unix::fs::symlink(std::fs::read_link(&from).ok()?, &at).ok()?
                }
                _ if kind.is_dir() => std::os::unix::fs::symlink(&from, &at).ok()?,
                _ => drop(std::fs::copy(&from, &at).ok()?),
            }
        }
        link(workspace, ignored.iter().map(String::as_str), &to)
    })();
    if made.is_none() {
        discard(&to);
        return None;
    }
    Some(to)
}

/// The files of the project in `workspace` as `copy` takes them, the ones
/// git tracks and the new ones it does not ignore, by their paths from
/// there; None where git could not list them.
#[cfg(unix)]
fn listed(workspace: &Path) -> Option<Vec<String>> {
    let args = [
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ];
    git(workspace, &args)
}

/// Whether `workspace` is the top of a git project, which a folder made
/// from what git lists there has to be: git lists the folder it runs in,
/// and below the top that is part of the project.
#[cfg(unix)]
fn top(workspace: &Path) -> Option<()> {
    let top = git(workspace, &["rev-parse", "--show-toplevel"])?;
    let canonical = |path: &Path| std::fs::canonicalize(path).ok();
    (canonical(Path::new(top.first()?)) == canonical(workspace)).then_some(())
}

/// The entries git ignores in `workspace`, such as installed packages and
/// builds, each by its path from there, a folder's with `/` after it.
#[cfg(unix)]
fn ignored(workspace: &Path) -> Option<Vec<String>> {
    git(
        workspace,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )
}

/// Links each of `entries`, entries git ignores in `workspace` as `ignored`
/// lists them, into `into`, a folder holding the project or the project as
/// it was, at the same path, making the folders above it: tests there find
/// the packages and builds they use as they are, without a copy of them.
#[cfg(unix)]
fn link<'a>(
    workspace: &Path,
    entries: impl IntoIterator<Item = &'a str>,
    into: &Path,
) -> Option<()> {
    for entry in entries {
        let entry = entry.trim_end_matches('/');
        let at = into.join(entry);
        std::fs::create_dir_all(at.parent()?).ok()?;
        std::os::unix::fs::symlink(workspace.join(entry), &at).ok()?;
    }
    Some(())
}

/// The entries of `ignored` that a tester which must not see the work can
/// have linked into `into`, the project as it was: the package stores and
/// build caches `STORES` names, under a folder that is there already. Not
/// a `dist/` or `build/`, which hold the work built, a `*.egg-info` or a
/// `__pycache__`, which hold it packaged or compiled, nor anything under a
/// folder the work added, which would name its new modules.
#[cfg(unix)]
fn stores<'a>(ignored: &'a [String], into: &Path) -> Vec<&'a str> {
    (ignored.iter())
        .map(|entry| entry.trim_end_matches('/'))
        .filter(|entry| {
            let path = Path::new(entry);
            let name = path.file_name().and_then(|name| name.to_str());
            name.is_some_and(|name| STORES.contains(&name))
                && path
                    .parent()
                    .is_some_and(|parent| into.join(parent).is_dir())
        })
        .collect()
}

/// Links that stand for packages and builds can't be made here without the
/// rights to, so the tester works in the project.
#[cfg(not(unix))]
pub(crate) fn copy(_workspace: &Path) -> Option<PathBuf> {
    None
}

/// The project in `workspace` as its last commit holds it, in a new folder of
/// the temp folder, for the tester to read how to call what already existed
/// without seeing the work; None where it can't be written out, as in a
/// project with no commit yet.
#[cfg(unix)]
pub(crate) fn before(workspace: &Path) -> Option<PathBuf> {
    let to = folder("before");
    std::fs::create_dir_all(&to).ok()?;
    // Checked out of a throwaway index rather than archived: an archive
    // leaves out what .gitattributes marks export-ignore, such as a tests
    // folder, and rewrites export-subst files, so a folder made from one is
    // not the project as it was.
    let index = to.with_extension("index");
    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(args)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_INDEX_FILE", &index)
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
    };
    let prefix = format!("--prefix={}/", to.display());
    let written = git(&["read-tree", "HEAD"]) && git(&["checkout-index", "-a", &prefix]);
    let _ = std::fs::remove_file(&index);
    if !written {
        discard(&to);
        return None;
    }
    Some(to)
}

/// The tester reads what it has, as `copy` makes none here either.
#[cfg(not(unix))]
pub(crate) fn before(_workspace: &Path) -> Option<PathBuf> {
    None
}

/// The commit the project in `workspace` is at while nothing in it is
/// changed, added or deleted since, as a run finds a project committed
/// before it: what a second attempt at the run's task starts from. None
/// where `workspace` is not the top of a git project, has no commit yet, or
/// holds changes that are not the run's, which no attempt could tell from
/// its own.
#[cfg(unix)]
pub(crate) fn baseline(workspace: &Path) -> Option<String> {
    top(workspace)?;
    let changed = git(workspace, &["status", "--porcelain"])?;
    changed.is_empty().then(|| head(workspace)).flatten()
}

/// No second attempt is made here, as `base` makes no folder for one.
#[cfg(not(unix))]
pub(crate) fn baseline(_workspace: &Path) -> Option<String> {
    None
}

/// The commit the project in `workspace` is at; None without one.
#[cfg(unix)]
pub(crate) fn head(workspace: &Path) -> Option<String> {
    git(workspace, &["rev-parse", "--verify", "HEAD"])?
        .into_iter()
        .next()
}

/// As `baseline`.
#[cfg(not(unix))]
pub(crate) fn head(_workspace: &Path) -> Option<String> {
    None
}

/// The project in `workspace` as its last commit holds it, as `before`
/// writes it out, with the package stores and build caches git ignores
/// there linked in (`stores`): a folder for a tester started with the run
/// to work in, which the work never reaches, where its tests can still be
/// parsed and their imports of the project's own code and packages
/// resolved, and for a second attempt at the run's task to be made in, with
/// nothing of the first in it. None where `workspace` is not the top of a
/// git project, has no commit yet, holds more than `COPIED` entries with
/// what it ignores, or where the folder could not be made.
#[cfg(unix)]
pub(crate) fn base(workspace: &Path) -> Option<PathBuf> {
    top(workspace)?;
    let ignored = ignored(workspace)?;
    // Capped as `copy` is: tests written in a folder too big to copy could
    // never be run by the driver.
    if listed(workspace)?.len() + ignored.len() > COPIED {
        return None;
    }
    let to = before(workspace)?;
    if link(workspace, stores(&ignored, &to), &to).is_none() {
        discard(&to);
        return None;
    }
    Some(to)
}

/// No tester starts with the run here, nor is a second attempt made, as
/// `copy` makes no copy either.
#[cfg(not(unix))]
pub(crate) fn base(_workspace: &Path) -> Option<PathBuf> {
    None
}

/// Deletes `copy`, a copy `copy` made: its links go, never what they lead to.
pub(crate) fn discard(copy: &Path) {
    if copy.starts_with(std::env::temp_dir()) {
        let _ = std::fs::remove_dir_all(copy);
    }
}

/// The task of the tester started for a run, for `sentences`, the
/// requirements `request` states. It is given the request word for word and
/// nothing of the work, and told to keep it that way: anything the request
/// spells out is used as it spells it, and code is read in `before`, the
/// project as it was, where there is one, and each case the run weighed and
/// set aside, `doubts`, to test.
pub(crate) fn brief(
    request: &str,
    sentences: &[String],
    before: Option<&Path>,
    doubts: &[String],
) -> String {
    let read_there = match before {
        Some(before) => format!(
            "The project as it was before the change is in `{}`: read code there, to learn how \
             to call what already existed and how its tests are set up, and not in your \
             working folder, which holds the change and is only where your tests run. ",
            before.display()
        ),
        None => String::new(),
    };
    let opening = format!(
        "Test a change you have not seen, made to the project in your working folder, \
         against the request below, by writing tests from the request's words alone. Do not \
         look at what was changed or how: run no `git diff`, `git status`, `git log` or \
         `git show`, and read the project's code only to learn what to call and how, such as \
         names, arguments and set-up, never to decide what a result should be. {read_there}"
    );
    let unnamed = "search your working folder for that name alone, not for how it works";
    let outcome = "A test that fails because you called the code wrongly, such as a guessed \
                   name, import or argument the request does not spell, is yours to fix from \
                   the code; one that fails on what the request says should happen is a \
                   finding: keep it as it is, and do not make it pass.";
    let rows = "`R1 | what it checks | PASS` or `R1 | what it checks | FAIL | command that \
                runs it | what it expected and what it got`";
    let mut text = briefing(request, sentences, &opening, unnamed, outcome, rows);
    text.push_str(&crate::doubts::section(doubts));
    text
}

/// The task of a tester started with a run, before the work exists, for
/// `sentences`, the requirements `request` states: as `brief` gives it, but
/// for a folder holding the project as it was, which the work never
/// reaches. Its tests cannot pass there, and it is told not to make them:
/// each is to fail for want of the change alone, so its imports resolve and
/// its set-up loads, and its table says so of each.
pub(crate) fn brief_ahead(request: &str, sentences: &[String]) -> String {
    let opening = "Test a change you have not seen, which is being made to the project now, \
                   against the request below, by writing tests from the request's words alone. \
                   Your working folder holds the project as it was before the change, and the \
                   change never reaches it, so there is nothing of it to look at: read the \
                   project's code only to learn what to call and how, such as names, arguments \
                   and set-up, never to decide what a result should be. ";
    let unnamed = "take the name from the request's own words, as whoever makes the change \
                   will, since the code has none for it yet";
    let outcome = "They cannot pass yet, since the change they test is not there: do not make \
                   them pass, and make no part of the change yourself. Make sure instead that \
                   each fails for want of the change alone: its imports of code that already \
                   exists resolve, its fixtures and set-up load, and the runner collects it. \
                   A test that fails for any other reason, such as a guessed name, import or \
                   argument of existing code that the request does not spell, is yours to fix \
                   from the code. Run them from the project's root with your folder first on \
                   the import path, as `python -m pytest` run from the root or `PYTHONPATH=.` \
                   does for Python and `npx` run from the root does for Node, and check once \
                   that the module under test resolves under your folder, such as by printing \
                   its `__file__`: where it resolves elsewhere, such as to an installed copy of \
                   the project, report each test as NOT RUN with that reason rather than its \
                   result.";
    let rows = "`R1 | what it checks | NOT RUN` where you could not run it, or `R1 | what it \
                checks | FAIL | command that runs it | not yet implemented`: none can pass \
                before the change, and one that does tests nothing of it";
    briefing(request, sentences, opening, unnamed, outcome, rows)
}

/// The tester's task that `brief` and `brief_ahead` share, for `sentences`,
/// the requirements `request` states: `opening` says what to test and where
/// to read, `unnamed` what to do for something new the request names
/// nothing for, `outcome` what to do with a test that fails, and `rows` how
/// a row of the table is written.
fn briefing(
    request: &str,
    sentences: &[String],
    opening: &str,
    unnamed: &str,
    outcome: &str,
    rows: &str,
) -> String {
    let mut text = format!(
        "{opening}\
         Every name the request spells out, such as a path, route, field, flag, key or \
         command, is used in your tests as the request spells it, never as the code does: a \
         test that fails because the work spells it differently is a finding. Where the request \
         names nothing for something new, {unnamed}. What each test expects comes from the \
         request's words; quote them in the test's name or a comment.\n\n\
         For each requirement, test the behaviour it states through the entry point a caller \
         of the project uses, end to end: each case it names, each form its input can be \
         written in, the side a limiting word such as only, unless, otherwise or and not keeps \
         out, with both sides in one input, steps it sets in no order in each order, such as the \
         later one first, each category it maps to an output with an input from outside the \
         project's own code that uses the request's words for it, and each other feature it is \
         said to work with, used together with it in one call. Then find the project's own \
         tests of the feature nearest to each new one, such as one it is said to work like or \
         with, or one of the same kind in the same module, and write each of their cases again \
         for the new one, in the same order of calls, with the same fixtures and through the \
         same entry point, such as a query made for the first time only after the change it \
         should see. Set each test up the way those tests do, and make each change before the \
         first call or query, as a caller would, rather than calling once beforehand to warm \
         anything up. Where the project's data, docs or tests already spell an output, expect \
         that spelling.\n\n\
         Write the tests in the project's own test language and style, in new files whose \
         names hold `{MARKER}`, where its test runner finds them, such as beside its other \
         tests, and run them with that runner. Change no other file. {outcome}\n\n\
         The request, word for word:\n"
    );
    text.push_str(request.trim());
    text.push_str("\n\nThe requirements it states, numbered:");
    for (at, sentence) in sentences
        .iter()
        .take(crate::requirements::LISTED)
        .enumerate()
    {
        text.push_str(&format!("\n- R{}: {sentence}", at + 1));
    }
    // The table and then the command, last: a long report is cut in the
    // middle before the driver reads it, and the command is read from the
    // end, so what the cut keeps is what the driver needs.
    text.push_str(&format!(
        "\n\nEnd your report with a table of one row per test, written {rows}. Then, on a \
         line of its own as the report's very last line, the one command that runs all your \
         tests from the project's root, with paths relative to it, written `Run: <command>`: \
         one that runs them once and exits rather than watching, installs nothing, and needs \
         no file beyond your test files, so any fixture or helper they need goes in a file \
         named with `{MARKER}` too. A long report is cut in its middle before it is read, so \
         what comes last is kept."
    ));
    text
}

/// Words that open a `Run:` line saying there is no command, as a tester
/// that could not run its tests tends to write it.
const PROSE: [&str; 8] = [
    "n/a",
    "none",
    "no ",
    "not ",
    "unable",
    "could not",
    "couldn't",
    "cannot",
];

/// Installers a `Run:` command may not call: run in a copy whose packages
/// and builds are links into the project, they would change the project's
/// own.
const INSTALLS: [&str; 26] = [
    "pip install",
    "pip3 install",
    "uv pip install",
    "uv sync",
    "uv run",
    "pipenv",
    "conda ",
    "poetry install",
    "poetry add",
    "setup.py develop",
    "setup.py install",
    "npm install",
    "npm i ",
    "npm ci",
    "pnpm install",
    "pnpm add",
    "yarn install",
    "yarn add",
    "cargo install",
    "go install",
    "go get",
    "make install",
    "apt-get",
    "npm link",
    "yarn link",
    "pnpm link",
];

/// Whether `command` would install or link anything, by `INSTALLS`.
pub(crate) fn installs(command: &str) -> bool {
    let lower = command.to_lowercase();
    INSTALLS.iter().any(|verb| lower.contains(verb))
}

/// The command the tester's `report` gives to run all its tests, off its
/// `Run:` line, the last where it wrote more than one, and the part in
/// backticks where it put one there; None when it gave none, said in words
/// that there is none, gave more than a line of shell, or would install
/// anything.
pub(crate) fn command(report: &str) -> Option<String> {
    let given = report.lines().rev().find_map(|line| {
        let line = line.trim().trim_start_matches(['-', '*', '>', ' ']);
        let rest = line.strip_prefix("Run:")?.trim().trim_matches('*').trim();
        // One span of backticks holds the command, whatever follows it; a
        // line with more than one is prose about commands, not one.
        let rest = match rest.matches('`').count() {
            0 => rest,
            2 => rest
                .trim_matches('`')
                .split('`')
                .next()
                .unwrap_or_default()
                .trim(),
            _ => "",
        };
        (!rest.is_empty()).then(|| rest.to_owned())
    })?;
    let lower = given.to_lowercase();
    if PROSE.iter().any(|word| lower.starts_with(word)) {
        return None;
    }
    if installs(&given) {
        return None;
    }
    (given.len() <= LINE).then_some(given)
}

/// `command` with each of `folders`, where the tester worked, written as
/// the folder it runs in: the tester may have spelt its copy's path out,
/// and the copy is gone by the time the driver runs the command elsewhere.
pub(crate) fn relative(command: &str, folders: &[&Path]) -> String {
    let mut text = command.to_owned();
    for folder in folders {
        let spelt = [
            Some(folder.to_path_buf()),
            std::fs::canonicalize(folder).ok(),
        ];
        for path in spelt.into_iter().flatten() {
            let path = path.to_string_lossy();
            if path.len() > 1 {
                text = folder_as_dot(&text, &path);
            }
        }
    }
    text
}

/// `text` with `path` written as `.` wherever it stands as a whole path or
/// the start of one: opening a word and followed by `/` or the word's end,
/// so `/app` is rewritten in `/app/tests` and left in `src/app/tests` and
/// `/apps`.
fn folder_as_dot(text: &str, path: &str) -> String {
    let opens = |before: &str| {
        (before.chars().last())
            .is_none_or(|c| c.is_whitespace() || matches!(c, '=' | '"' | '\'' | '(' | ':' | ','))
    };
    let ends = |after: &str| {
        (after.chars().next()).is_none_or(|c| {
            c == '/' || c.is_whitespace() || matches!(c, '"' | '\'' | ')' | ':' | ';' | ',')
        })
    };
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(path) {
        let (before, from) = rest.split_at(at);
        let after = &from[path.len()..];
        out.push_str(before);
        out.push_str(if opens(&out) && ends(after) {
            "."
        } else {
            path
        });
        rest = after;
    }
    out.push_str(rest);
    out
}

/// The program `line`, a command of the run's, runs, as the tables name it:
/// the first past the `cd`s, exports and assignments that only set it up;
/// None when `line` only sets up.
pub(crate) fn program(line: &str) -> Option<String> {
    set_up(line).0
}

/// The variables `line`, a command of the run's, sets for `program`, as
/// `VAR=value` words: those before the program, those `export`ed or set on
/// their own before it, and those on a command after a `cd`, which only
/// moves it. The tester's command runs with them, so a tester that lacked
/// the run's set-up, such as a module cache, gets it. None when `line` runs
/// another program, whose set-up may be a build's against the tests; only a
/// value the shell reads as written, none whose name marks it as a secret or
/// as `UNLENT`, and `LENT` at most.
pub(crate) fn environment(line: &str, program: &str) -> Vec<String> {
    let (runs, mut found) = set_up(line);
    if runs.as_deref() != Some(program) {
        return Vec::new();
    }
    found.truncate(LENT);
    found
}

/// What `line` runs and sets up: the first program past the `cd`s, exports
/// and assignments before it, if it reaches one, and the variables set on
/// the way, as `plain` allows them.
fn set_up(line: &str) -> (Option<String>, Vec<String>) {
    let dialect = crate::command::Dialect::here();
    let assessed = crate::command::assess(line, dialect, None);
    let mut found = Vec::new();
    for command in &assessed.commands {
        let program = (command.argv.get(command.at)).map(|word| crate::command::name_of(word));
        let sets = matches!(program.as_deref(), None | Some("export"));
        let words = match sets {
            true => &command.argv[..],
            false => &command.argv[..command.at],
        };
        // A word made by running something, or holding a variable this does
        // not know, is not what the shell read.
        if command.spelled_out {
            found.extend(words.iter().filter(|word| plain(word)).cloned());
        }
        if !sets && program.as_deref() != Some("cd") {
            return (program, found);
        }
    }
    (None, found)
}

/// Whether `word` sets a variable to a value with nothing in it for the
/// shell to expand or to end the word at, under a name that marks it
/// neither as a secret nor as one never lent.
fn plain(word: &str) -> bool {
    let Some((name, value)) = word.split_once('=') else {
        return false;
    };
    let upper = name.to_ascii_uppercase();
    crate::command::assignment(word)
        && !SECRETS.iter().any(|secret| upper.contains(secret))
        && !UNLENT.contains(&upper.as_str())
        && !upper.starts_with("LD_")
        && !upper.ends_with("FLAGS")
        && (value.chars()).all(|c| c.is_ascii_alphanumeric() || "_-./:,+@%=".contains(c))
}

/// The tester's files kept for run `run` and the command that runs them,
/// for the driver to run them against the work; None when none were kept,
/// or the tester gave no command.
pub(crate) fn to_run(run: u64) -> Option<(Kept, String)> {
    let moved = moved().lock().ok()?;
    let kept = moved.iter().find(|kept| kept.run == run)?;
    Some((kept.kept.clone(), kept.command.clone()?))
}

/// What came of the tester's tests run against the work.
#[derive(Debug, PartialEq)]
pub(crate) enum Ran {
    /// They passed, run with the command.
    Passed(String),
    /// Some failed, or the command did not finish: the notice sending the
    /// run back with the output.
    Failed(String),
    /// They could not be run without touching the project: why.
    Unrun(String),
}

/// What came of the work in `ctx`'s project, as it stands, run against the
/// tester's tests `kept` with `command`, given `limit` to run. They run in
/// a fresh copy of the project with the kept files put back at their paths,
/// as the tester's own did, so nothing is added to the project and no patch
/// of it takes them; where no copy can be made they are not run. When the
/// work fails them, `first` is asked for the run's first complete version,
/// and the tests run there too (`regression`): one that passes them makes
/// the notice one of what the run changed since, for it to put back what
/// broke them.
pub(crate) fn ran(
    kept: &Kept,
    command: &str,
    ctx: &ToolContext,
    limit: Duration,
    first: impl FnOnce() -> Option<FirstVersion>,
) -> Ran {
    let Some(workspace) = ctx.workspace.as_deref() else {
        return Ran::Unrun("the run has no project folder".into());
    };
    let Some(copy) = copy(workspace) else {
        return Ran::Unrun("no copy of the project could be made to run them in".into());
    };
    let outcome = outcome_in(kept, command, &copy, ctx, limit);
    discard(&copy);
    let Some(outcome) = outcome else {
        return Ran::Unrun(format!(
            "the test files kept in `{}` could not be put back in a copy of the project",
            kept.dir.display()
        ));
    };
    // A build of the whole workspace that did not fit in the time says
    // nothing of the work: it is not a failure to send the run back with.
    if unfinished_build(command, &outcome) {
        return Ran::Unrun("the build did not fit in the time".into());
    }
    if let Some(notice) = regression(kept, command, workspace, ctx, limit, &outcome, first) {
        return Ran::Failed(notice);
    }
    match failed(kept, command, &outcome) {
        Some(notice) => Ran::Failed(notice),
        None => Ran::Passed(command.to_owned()),
    }
}

/// The notice telling the run that its first complete version passes the
/// tester's tests `kept` with `command`, which its work in `workspace` as
/// it stands fails, as `outcome` says. `first` is asked for that version
/// only when the time to the deadline `ctx` carries holds its run within
/// `limit` and half a review's for the fix after it, measured now that the
/// work's own run has taken what it took, and its run is cut to leave that;
/// the tests run in it only where files differ between the two, since the
/// same files pass or fail alike. None otherwise, when the work passes, when
/// none is kept, or when it fails them too.
fn regression(
    kept: &Kept,
    command: &str,
    workspace: &Path,
    ctx: &ToolContext,
    limit: Duration,
    outcome: &Result<String, String>,
    first: impl FnOnce() -> Option<FirstVersion>,
) -> Option<String> {
    let (how, output) = failure(outcome)?;
    let fix = crate::requirements::REVIEW_TIME / 2;
    let left = time_left(ctx);
    let timely = left.is_none_or(|left| left >= fix + limit);
    let first = timely.then(first).flatten()?;
    // A store linked into the first version that leads back into the
    // project would have the tests there run the work as it stands, so the
    // two could only fail alike: the run in it is kept rather than spent. A
    // Python command runs with the folder first on the import path
    // (`with_import_path`), so only another language's link blinds it.
    if !pythonic(command) && imports_elsewhere(workspace, &first.dir).is_some() {
        return None;
    }
    let differ = differing(&first.dir, workspace);
    if differ.is_empty() {
        return None;
    }
    let again = left.map_or(limit, |left| limit.min(left.saturating_sub(fix)));
    let outcome = outcome_in(kept, command, &first.dir, ctx, again)?;
    // Judged by once the tests have run in it, whatever they said; not when
    // they could not be started.
    if happened(&outcome) {
        first.judged();
    }
    (failure(&outcome).is_none()).then(|| regressed(command, &how, output, &differ))
}

/// Whether the tester's tests ran, for `outcome`, what `run_command` made
/// of them: they did unless the tool could not start them; ones cut at
/// their limit ran.
fn happened(outcome: &Result<String, String>) -> bool {
    outcome.is_ok() || unfinished(outcome)
}

/// Whether `outcome`, what `run_command` made of a command, is one cut at
/// its limit before it finished.
fn unfinished(outcome: &Result<String, String>) -> bool {
    matches!(outcome, Err(why) if why.contains("Timed out"))
}

/// Whether `outcome`, what `run_command` made of `command` in a copy of the
/// project, is a build of the whole workspace (`rebuilds_all`) cut at its
/// limit: minutes on a small machine, it may not fit in the time given, and
/// then it says nothing of the work.
pub(crate) fn unfinished_build(command: &str, outcome: &Result<String, String>) -> bool {
    rebuilds_all(command) && unfinished(outcome)
}

/// How long the run making calls with `ctx` has to its deadline; None with
/// no deadline.
fn time_left(ctx: &ToolContext) -> Option<Duration> {
    let deadline = ctx.caller.connection.as_deref()?.deadline?;
    Some(deadline.saturating_duration_since(Instant::now()))
}

/// What the run is told, with the requirements notice, of the first run of
/// the tests a tester wrote ahead of its work against the work as it
/// stands, which went as `ran` says: an output it can act on, that they
/// passed, or why they could not be run and are its own to run.
pub(crate) fn first_run(ran: &Ran) -> String {
    match ran {
        Ran::Passed(command) => format!(
            "Your work as it stands was run against those tests, in a copy of the project with \
             them put back at the paths they are kept from: `{command}` passed. They are run \
             again once you say you are done, and what fails then comes back to you."
        ),
        Ran::Failed(notice) => notice.clone(),
        Ran::Unrun(why) => format!(
            "Those tests could not be run against your work as it stands: {why}. Run them \
             yourself, with the project's own runner and the set-up you found works, fix your \
             work for each that fails on what the request's words say, and delete the copies \
             before you end."
        ),
    }
}

/// Whether `outcome`, what the tester's tests made of some work, is a pass
/// that chose something: the command ended well and its output does not
/// say that no test ran, which a bare project or a wrong path can bring.
pub(crate) fn passed(outcome: &Result<String, String>) -> bool {
    if failure(outcome).is_some() {
        return false;
    }
    let lower = outcome.as_deref().unwrap_or_default().to_lowercase();
    !VACUOUS.iter().any(|said| lower.contains(said))
}

/// What `command` made of the tester's tests `kept` run against the work in
/// `ctx`'s project as it stands, within `limit`: in a fresh copy of the
/// project with the kept files put back at their paths, as the tester's own
/// did, so nothing is added to the project and no patch of it takes them.
/// None when no copy could be made, or a file could not be placed.
pub(crate) fn outcome(
    kept: &Kept,
    command: &str,
    ctx: &ToolContext,
    limit: Duration,
) -> Option<Result<String, String>> {
    let workspace = ctx.workspace.as_deref()?;
    let copy = copy(workspace)?;
    let outcome = outcome_in(kept, command, &copy, ctx, limit);
    discard(&copy);
    outcome
}

/// What `command` made of the tester's tests `kept` run against the work in
/// `folder`, which holds it already, as a second attempt's does: the kept
/// files are put at their paths there for the run and taken away after it.
/// None when one could not be placed.
pub(crate) fn outcome_in(
    kept: &Kept,
    command: &str,
    folder: &Path,
    ctx: &ToolContext,
    limit: Duration,
) -> Option<Result<String, String>> {
    let placed = kept.files.iter().all(|file| {
        let at = folder.join(file);
        (at.parent()).is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
            && std::fs::copy(kept.dir.join(file), &at).is_ok()
    });
    let outcome = placed.then(|| run_command(command, folder, ctx, limit));
    for file in &kept.files {
        let _ = std::fs::remove_file(folder.join(file));
    }
    outcome
}

/// What `run_command` made of `command` run in `copy` within `limit`, as
/// `ctx` allows it and as any call of the tool goes, hooks and the
/// scrubbing of its output included: with the copy as its project and for
/// no conversation, so one still running at the limit is stopped there
/// rather than left running in the background. The run's deadline does not
/// cut a call made for no conversation, so `limit` has to fit in it.
fn run_command(
    command: &str,
    copy: &Path,
    ctx: &ToolContext,
    limit: Duration,
) -> Result<String, String> {
    let mut ctx = ctx.clone();
    ctx.workspace = Some(copy.to_path_buf());
    ctx.conversation = 0;
    if let Some(parent) = ctx.caller.connection.as_deref() {
        ctx.caller.connection = Some(Arc::new(crate::chat::Connection {
            workspace: Some(copy.to_path_buf()),
            ..parent.clone()
        }));
    }
    let command = with_import_path(command, copy);
    let args = serde_json::json!({"command": command, "timeout": limit.as_secs().max(1)});
    crate::tools::run("run_command", &args.to_string(), &ctx)
}

/// Whether `command` runs Python: `python`, `python3` and the like, `pytest`
/// or `py.test`, past any set-up, or `-m pytest` anywhere on its line.
fn pythonic(command: &str) -> bool {
    let python = |name: &str| name.starts_with("python") || name == "pytest" || name == "py.test";
    program(command).is_some_and(|name| python(&name)) || command.contains("-m pytest")
}

/// `command`, a Python command run in `folder`, a copy of the project, with
/// `folder/src` and `folder` first on the import path, exported ahead of it:
/// where the project is installed as editable, the install names the
/// project's own folder, and a test run in a copy would otherwise import the
/// project's code rather than the copy's. The standard finder reads sys.path
/// first, PYTHONPATH ahead of site-packages, and setuptools appends its
/// editable finder to sys.meta_path, so the copy's code wins. As it is for
/// any other command.
fn with_import_path(command: &str, folder: &Path) -> String {
    if !pythonic(command) {
        return command.to_owned();
    }
    let folder = folder.display();
    format!("export PYTHONPATH=\"{folder}/src:{folder}${{PYTHONPATH:+:$PYTHONPATH}}\"; {command}")
}

/// How `command` ended when `outcome`, what it made of the tester's tests,
/// is a failure, and the output it printed; None when they passed.
pub(crate) fn failure(outcome: &Result<String, String>) -> Option<(String, &str)> {
    match outcome {
        // A command that passed has no exit code line; a note of where the
        // whole output went, or what the user's hooks said, can follow one.
        Ok(output) => {
            let code = (output.lines().rev()).find_map(|line| line.strip_prefix("[exit code "))?;
            let code = code.trim_end_matches(']');
            Some((format!("exited with code {code}"), output.as_str()))
        }
        // Without the tool's advice to call it again with a longer timeout,
        // which is for a model that made the call.
        Err(why) => {
            let why = why.split(" If it only needs longer").next().unwrap_or(why);
            Some(("did not finish".to_owned(), why))
        }
    }
}

/// The notice sending the run back with `outcome`, what `command` made of
/// the tester's tests `kept` run against its work; None when they passed.
fn failed(kept: &Kept, command: &str, outcome: &Result<String, String>) -> Option<String> {
    let (ended, output) = failure(outcome)?;
    Some(format!(
        "Your work as it stands was run against the tests a tester wrote from the request \
         alone, in a copy of the project with them put back at the paths they are kept from, \
         so nothing in the project was touched: `{command}` {ended}. Its output ends:\n\n{}\n\n\
         Fix your work for each test that fails on what the request's words say, and run the \
         tests again the same way. A test that expects what the words do not say is answered \
         by quoting them in your answer. One that calls the code wrongly, such as with a name \
         or import the request does not spell, or that fails for want of a fixture, helper or \
         set-up the tester left out, is the tester's to fix where it is kept, in `{}`, since \
         that is where it is run from; never something to add to the project. If the command \
         itself cannot run here, run the tests with the set-up you found works. Then end with \
         your whole answer again.",
        tail(output),
        kept.dir.display()
    ))
}

/// The notice sending the run back when its first complete version passed
/// the tester's tests with `command` and its work as it stands `ended` as
/// it did with `output`: with the files that `differ` between the two, for
/// it to put back what broke them.
fn regressed(command: &str, ended: &str, output: &str, differ: &[String]) -> String {
    let mut named: Vec<String> = (differ.iter().take(DIFFERING))
        .map(|file| format!("`{file}`"))
        .collect();
    if differ.len() > DIFFERING {
        named.push(format!("and {} more", differ.len() - DIFFERING));
    }
    let changed = match named.is_empty() {
        true => "none that a listing of the project shows".to_owned(),
        false => named.join(", "),
    };
    format!(
        "Your first complete version, the work as it stood when you first said you were done, \
         passed `{command}`; what you ended with fails it. Both were run against the tests a \
         tester wrote from the request alone, each in a copy of the project with them put back \
         at the paths they are kept from, so nothing in the project was touched: your work as \
         it stands {ended}. Its output ends:\n\n{}\n\nThe files that differ between that first \
         version and your work as it stands: {changed}. Put back what broke the test, unless \
         the test expects what the request's words do not say: then keep your change, and quote \
         the words in your answer or say the request is silent on it. Then end with your whole \
         answer again.",
        tail(output)
    )
}

/// The paths of the files that differ between `first`, a run's first
/// complete version, and its work as it stands in `workspace`: changed,
/// added or deleted since, in order, the files in hidden folders last. The
/// files git ignores are left out, as `copy` left them out of `first`, the
/// tester's own, by their names, and what a test run or a build leaves
/// behind, by `artefact`.
#[cfg(unix)]
fn differing(first: &Path, workspace: &Path) -> Vec<String> {
    let Some(current) = listed(workspace) else {
        return Vec::new();
    };
    let work = |path: &String| {
        let name = path.rsplit('/').next().unwrap_or(path);
        !name.contains(MARKER)
            && !(path.split('/')).any(|part| SKIPPED.contains(&part) || artefact(part))
    };
    let mut paths: Vec<String> = (current.into_iter().chain(files_under(first)))
        .filter(work)
        .collect();
    // The work's own files first: a run's hidden folders, such as a tool's,
    // would otherwise take the names.
    paths.sort_by(|a, b| (hidden(a).cmp(&hidden(b))).then_with(|| a.cmp(b)));
    paths.dedup();
    paths.retain(|path| {
        let (ours, theirs) = (first.join(path), workspace.join(path));
        let there = |path: &Path| std::fs::symlink_metadata(path).is_ok();
        (there(&ours) || there(&theirs)) && differs(&ours, &theirs)
    });
    paths
}

/// No copy is made here, so there is no first version to differ from.
#[cfg(not(unix))]
fn differing(_first: &Path, _workspace: &Path) -> Vec<String> {
    Vec::new()
}

/// Whether `part`, a component of a path, names what a test run or a build
/// leaves behind and no work of the run's: a cache, compiled Python,
/// coverage, a package's egg-info.
fn artefact(part: &str) -> bool {
    CACHES.contains(&part)
        || part.starts_with("coverage")
        || part.starts_with(".coverage")
        || part.ends_with(".pyc")
        || part.ends_with(".egg-info")
}

/// Whether a component of `path` is hidden.
fn hidden(path: &str) -> bool {
    path.split('/').any(|part| part.starts_with('.'))
}

/// Whether `command` runs cargo, which keys its artifacts by each package's
/// path: in a copy of the project it builds every crate of the workspace
/// again from scratch, minutes on a small machine, holding the target
/// folder's lock meanwhile, and twice over, for the work's copy and the
/// project as it was. So it is compared only with `REBUILD_SPARE` suites'
/// worth of time to spare; the other builders reuse their caches by content.
pub(crate) fn rebuilds_all(command: &str) -> bool {
    let apart =
        |c: char| c.is_whitespace() || matches!(c, '&' | ';' | '|' | '(' | ')' | '"' | '\'');
    (command.split(apart)).any(|word| {
        Path::new(word)
            .file_name()
            .is_some_and(|name| name == "cargo")
    })
}

/// Words a runner's output holds when it ran no test at all, with the
/// spaces between them squeezed to one.
const NOTHING_RAN: [&str; 8] = [
    "no tests to run",
    "no test files",
    "tests: 0 total",
    "collected 0 items",
    "no tests found",
    "0 tests run",
    "no tests ran",
    "ran 0 tests",
];

/// Whether `output`, a test runner's that exited 0, shows that nothing ran,
/// as a filter matching no test leaves it: Go marks each package's line
/// where nothing ran, and says nothing ran only where every line is marked;
/// jest counts `0 total` or finds no tests; cargo and mocha count `0 passed`
/// or `0 passing`, cargo once per test binary, so only a count of 0 wherever
/// one stands means nothing ran. Finer than `VACUOUS`, which a crate's
/// doc-tests with `0 passed` beside its tests would trip.
pub(crate) fn ran_nothing(output: &str) -> bool {
    let lower = output.to_lowercase();
    // A package with tests beside one without is a run all the same.
    let marked =
        |line: &str| line.contains("[no tests to run]") || line.contains("[no test files]");
    if lower.lines().any(marked) {
        return (lower.lines())
            .filter(|line| line.starts_with("ok") || line.starts_with('?'))
            .all(marked);
    }
    let words: Vec<&str> = lower.split_whitespace().collect();
    let squeezed = words.join(" ");
    if NOTHING_RAN.iter().any(|said| squeezed.contains(said)) {
        return true;
    }
    let counts: Vec<u64> = (words.windows(2))
        .filter(|pair| pair[1].starts_with("passed") || pair[1].starts_with("passing"))
        .filter_map(|pair| {
            pair[0]
                .trim_matches(|c: char| !c.is_ascii_digit())
                .parse()
                .ok()
        })
        .collect();
    !counts.is_empty() && counts.iter().all(|count| *count == 0)
}

/// Whether `command`, run in `folder` within `limit` as `run_command` runs
/// it, passed: Some(true) when it ran to its end with tests run, Some(false)
/// when it failed or did not finish, as `failure` reads it, and None when it
/// passed with nothing run, as `ran_nothing` reads it, which proves nothing.
pub(crate) fn verdict(
    command: &str,
    folder: &Path,
    ctx: &ToolContext,
    limit: Duration,
) -> Option<bool> {
    let outcome = run_command(command, folder, ctx, limit);
    if failure(&outcome).is_some() {
        return Some(false);
    }
    let output = outcome.as_deref().unwrap_or_default();
    (!ran_nothing(output)).then_some(true)
}

/// The notice sending a run back when `command`, a test of the project's own
/// that failed for it with `output`, passes on the project as it was: a
/// regression, unless the request changes what the test expects. It is told
/// to run it once more, since a test can fail by chance, and that the test
/// itself is not to go.
pub(crate) fn broken(command: &str, output: &str) -> String {
    format!(
        "`{command}` passes on the project as it was and fails with your change. Its output \
         ends:\n\n{}\n\nRun it once more first. If it still fails, then where the request's \
         words change what that test expects, update the test's expectation and quote the \
         words in your answer; otherwise fix the code so it passes again. Do not delete, skip \
         or weaken the test. Then end with your whole answer again.",
        tail(output)
    )
}

/// The end of `output`: `SHOWN` characters at most, from a line's start.
fn tail(output: &str) -> String {
    let output = output.trim_end();
    if output.chars().count() <= SHOWN {
        return output.to_owned();
    }
    let from = (output.char_indices().rev().nth(SHOWN - 1)).map_or(0, |(at, _)| at);
    let from = output[from..].find('\n').map_or(from, |at| from + at + 1);
    format!("…\n{}", &output[from..])
}

/// The task of the helper making a second attempt at `request` in a folder
/// holding the project as it was: the request word for word, that a first
/// attempt it is not shown did not pass the tests a tester wrote, which
/// `command` runs, as `ended` with `output`, and that its own work is run
/// against them the same way when it ends. Nothing of the first attempt's
/// code, so it shares none of that attempt's reading of the request, and
/// nothing of where the tests are or what they are called, so it builds to
/// the request rather than to them.
pub(crate) fn attempt(request: &str, command: &str, ended: &str, output: &str) -> String {
    let mut text = format!(
        "Make the change the request below asks for, from scratch, in the project in your \
         working folder, which holds the project as it was before anyone began on it. Another \
         agent's attempt at the same request did not pass the tests a tester wrote from the \
         request's words alone: `{command}` {ended}, and its output ends:\n\n{}\n\n\
         That attempt is not in your folder and not yours to look for or build on. Read the \
         project as it is, take from the request's words what should happen in each case they \
         name, and build it so that a caller of the project gets that, end to end, through the \
         entry point a caller uses. Where the output above shows what the tests expect and the \
         request's words agree, make that happen; where the words say otherwise, follow the \
         words. The tester's tests are kept out of your folder: when you end, they are run with \
         `{command}` against your work as you leave it, and your work is taken only if they \
         pass. Install, link or re-install nothing, with no `pip install`, `npm install`, `npm \
         link`, `go install`, `cargo install` or the like, and touch no file outside your folder: \
         the packages and builds you find linked in it are the project's own, to use as they \
         are. So before you end, run the project's own tests and a check of your own on each \
         case the request names, and leave the folder as it should be handed over: nothing half \
         done, no scratch files, and no file of yours whose name holds `{MARKER}`. The folder is \
         not a git repository, so there is no history to read in it. End with what you changed \
         and why.\n\n\
         The request, word for word:\n",
        tail(output)
    );
    text.push_str(request.trim());
    text
}

/// What `adopt` did: the paths it wrote in the project and those it removed
/// from it, each in order, those it could not, and the folder the first
/// attempt is kept in.
#[derive(Debug, Default, PartialEq)]
pub(crate) struct Adopted {
    pub written: Vec<String>,
    pub removed: Vec<String>,
    pub failed: Vec<String>,
    pub kept: PathBuf,
}

/// Makes the project in `workspace` hold the work in `from`, a second
/// attempt's folder made by `base`, in place of the first attempt's: each
/// file that differs is written over or added, each file of the project the
/// attempt has not is removed, the tester's files `excluded` and what git
/// ignores are left as they are, and the first attempt is kept beforehand in
/// a new folder of the temp folder, as `FIRST_PATCH`, its changes to the last
/// commit's files, with the files it added beside it at their paths. Nothing
/// under `.git` is touched, no link is followed and nothing is written
/// outside the project. Err, with the project as it was, when the two hold
/// the same files, the project's files could not be listed or the first
/// attempt could not be kept.
#[cfg(unix)]
pub(crate) fn adopt(workspace: &Path, from: &Path, excluded: &[String]) -> Result<Adopted, String> {
    let listed = "The project's files could not be listed.";
    let tracked = git(workspace, &["ls-files", "-z", "--cached"]).ok_or(listed)?;
    let untracked = git(
        workspace,
        &["ls-files", "-z", "--others", "--exclude-standard"],
    )
    .ok_or(listed)?;
    let theirs = attempted(from, &tracked, excluded);
    let ours: BTreeSet<String> = (tracked.iter().chain(&untracked))
        .filter(|path| !excluded.contains(path))
        .cloned()
        .collect();
    let written: Vec<String> = (theirs.iter())
        .filter(|path| differs(&workspace.join(path), &from.join(path)))
        .cloned()
        .collect();
    // Only a file or a link is removed: a folder standing where the project
    // tracks a path, as a submodule's does, is left.
    let removed: Vec<String> = (ours.iter())
        .filter(|path| !theirs.contains(*path))
        .filter(|path| {
            std::fs::symlink_metadata(workspace.join(path)).is_ok_and(|kind| !kind.is_dir())
        })
        .cloned()
        .collect();
    if written.is_empty() && removed.is_empty() {
        return Err("The second attempt left the project as it was.".into());
    }
    // Checked before anything is written, so a path that would reach
    // outside the project leaves it as it was.
    for path in written.iter().chain(&removed) {
        if inside(workspace, path).is_none() {
            return Err(format!("`{path}` would reach outside the project."));
        }
    }
    let kept = keep_first(workspace, &untracked)?;
    let mut failed = Vec::new();
    // Removed before anything is written, so a file the attempt made a
    // folder of has made way for the folder.
    for path in &removed {
        let Some(at) = inside(workspace, path) else {
            continue;
        };
        if std::fs::remove_file(&at).is_err() {
            failed.push(path.clone());
        }
    }
    for path in &written {
        let Some(at) = inside(workspace, path) else {
            continue;
        };
        if !put(&from.join(path), &at) {
            failed.push(path.clone());
        }
    }
    Ok(Adopted {
        written,
        removed,
        failed,
        kept,
    })
}

/// No second attempt is made here, so none is adopted.
#[cfg(not(unix))]
pub(crate) fn adopt(
    _workspace: &Path,
    _from: &Path,
    _excluded: &[String],
) -> Result<Adopted, String> {
    Err("No second attempt is made here.".into())
}

/// The files of the second attempt in `from`, by their paths from there, in
/// order: the files its walk finds, what the project's own rules ignore and
/// the tester's `excluded` left out, and those `tracked` by the project that
/// it still holds, however they are ignored, links among them. The walk
/// takes no link, since those `base` made stand for what git ignores, and
/// follows none, and `.git` is never entered.
#[cfg(unix)]
fn attempted(from: &Path, tracked: &[String], excluded: &[String]) -> BTreeSet<String> {
    let held = |path: &Path| std::fs::symlink_metadata(path).is_ok_and(|kind| !kind.is_dir());
    let mut walk = ignore::WalkBuilder::new(from);
    walk.hidden(false)
        .parents(false)
        .git_global(false)
        .git_exclude(false)
        .require_git(false);
    walk.filter_entry(|entry| !SKIPPED.iter().any(|skipped| entry.file_name() == *skipped));
    let mut found: BTreeSet<String> = (walk.build().take(WALKED))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| Some(slashed(entry.path().strip_prefix(from).ok()?)))
        .collect();
    found.extend(
        (tracked.iter())
            .filter(|path| held(&from.join(path)))
            .cloned(),
    );
    for path in excluded {
        found.remove(path);
    }
    found
}

/// Whether what is at `theirs`, a file or a link, differs from what is at
/// `ours`: nothing there, a file for a link or a link for a file, a link to
/// somewhere else, other bytes or another mode.
#[cfg(unix)]
fn differs(ours: &Path, theirs: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let (Ok(mine), Ok(its)) = (
        std::fs::symlink_metadata(ours),
        std::fs::symlink_metadata(theirs),
    ) else {
        return true;
    };
    if mine.is_symlink() || its.is_symlink() {
        let same = mine.is_symlink() && its.is_symlink();
        return !same || std::fs::read_link(ours).ok() != std::fs::read_link(theirs).ok();
    }
    if !mine.is_file() || mine.len() != its.len() {
        return true;
    }
    if (mine.permissions().mode() ^ its.permissions().mode()) & 0o111 != 0 {
        return true;
    }
    match (std::fs::read(ours), std::fs::read(theirs)) {
        (Ok(mine), Ok(its)) => mine != its,
        _ => true,
    }
}

/// Puts what is at `from`, a file or a link, at `at`, in place of what is
/// there: a link is made again to where it leads, never written through,
/// since that would carry the file outside the project.
#[cfg(unix)]
fn put(from: &Path, at: &Path) -> bool {
    let Ok(kind) = std::fs::symlink_metadata(from) else {
        return false;
    };
    let there = std::fs::symlink_metadata(at).ok();
    let linked = there.as_ref().is_some_and(|there| there.is_symlink());
    if (linked || (kind.is_symlink() && there.is_some())) && std::fs::remove_file(at).is_err() {
        return false;
    }
    if !(at.parent()).is_some_and(|parent| std::fs::create_dir_all(parent).is_ok()) {
        return false;
    }
    match kind.is_symlink() {
        true => (std::fs::read_link(from))
            .and_then(|to| std::os::unix::fs::symlink(to, at))
            .is_ok(),
        false => std::fs::copy(from, at).is_ok(),
    }
}

/// `path` under `workspace`, when it names a place inside the project and
/// nothing on the way to it there is a link: a write or a removal through
/// one would reach outside the project. Never anything in a `.git` folder,
/// at any depth.
#[cfg(unix)]
fn inside(workspace: &Path, path: &str) -> Option<PathBuf> {
    use std::path::Component;
    let relative = Path::new(path);
    let mut parts = relative.components();
    if !parts.all(|part| matches!(part, Component::Normal(name) if name != ".git")) {
        return None;
    }
    let mut at = workspace.to_path_buf();
    for part in relative.parent()?.components() {
        at.push(part);
        if std::fs::symlink_metadata(&at).is_ok_and(|kind| kind.is_symlink()) {
            return None;
        }
    }
    Some(workspace.join(relative))
}

/// Keeps the first attempt, the project in `workspace` as it stands, in a
/// new folder of the temp folder: `FIRST_PATCH`, its changes to the last
/// commit's files, and its `untracked` files beside it at their paths. The
/// folder, or why it could not be kept.
#[cfg(unix)]
fn keep_first(workspace: &Path, untracked: &[String]) -> Result<PathBuf, String> {
    let dir = folder("first");
    let patch = git_bytes(workspace, &["diff", "--binary", "HEAD"])
        .ok_or("The first attempt's changes could not be read.")?;
    let kept = (|| {
        std::fs::create_dir_all(&dir).ok()?;
        std::fs::write(dir.join(FIRST_PATCH), patch).ok()?;
        for path in untracked {
            let from = workspace.join(path);
            if !std::fs::symlink_metadata(&from).is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let at = dir.join(path);
            std::fs::create_dir_all(at.parent()?).ok()?;
            std::fs::copy(&from, &at).ok()?;
        }
        Some(())
    })();
    if kept.is_none() {
        discard(&dir);
        return Err("The first attempt could not be kept.".into());
    }
    Ok(dir)
}

/// What the run is told once the project holds a second attempt's work in
/// place of its own, `adopted`: that its work, run once more with `command`,
/// `ended` so, what was written and removed, where its own is kept, and to
/// answer again for the work as it stands.
pub(crate) fn adopted(command: &str, ended: &str, adopted: &Adopted) -> String {
    let named = |paths: &[String]| {
        let mut named: Vec<String> = (paths.iter().take(NAMED))
            .map(|path| format!("`{path}`"))
            .collect();
        if paths.len() > NAMED {
            named.push(format!("and {} more", paths.len() - NAMED));
        }
        named.join(", ")
    };
    let mut text = format!(
        "Your work as it stands was run once more against the tests a tester wrote from the \
         request alone, in a copy of the project, and `{command}` {ended}. Meanwhile a helper \
         that had not seen your work made a second attempt at the request from scratch, in a \
         copy of the project as it was before you began, and its work passes those tests. The \
         project now holds the second attempt in place of yours",
    );
    if !adopted.written.is_empty() {
        text.push_str(&format!(
            ": {} written, {}",
            adopted.written.len(),
            named(&adopted.written)
        ));
    }
    if !adopted.removed.is_empty() {
        let join = if adopted.written.is_empty() { ":" } else { ";" };
        text.push_str(&format!(
            "{join} {} of yours removed, {}",
            adopted.removed.len(),
            named(&adopted.removed)
        ));
    }
    text.push_str(&format!(
        ". Yours is kept until this run ends in `{}`: `{FIRST_PATCH}`, its changes to the last \
         commit's files, and the files it added at their paths. Do not put it back or merge it \
         in, since it did not pass.",
        adopted.kept.display()
    ));
    if !adopted.failed.is_empty() {
        text.push_str(&format!(
            " {} could not be written or removed here: {}.",
            adopted.failed.len(),
            named(&adopted.failed)
        ));
    }
    text.push_str(
        " Read the project as it now stands, with `git status` and `git diff`, change nothing, \
         and end with your whole answer again, describing the work as the project holds it now.",
    );
    text
}

/// What the run is told when the second attempt made in `base` ran
/// `commands` that install or link something: nothing of it is taken, and
/// the folder stays, since a package store linked from it may now lead into
/// it.
pub(crate) fn installed(base: &Path, commands: &[String]) -> String {
    let named: Vec<String> = (commands.iter().take(3))
        .map(|command| format!("`{command}`"))
        .collect();
    format!(
        "A helper that had not seen your work made a second attempt at the request in `{}`, a \
         copy of the project as it was before you began, and ran {} there, which may have \
         installed or linked something from that folder into the project's own packages. So \
         nothing of its work was taken, and the folder is left where it is, so that any such \
         link keeps working. Your work stands as it is. Since the project's own install may \
         now point at that folder, run your own install of the project again in the project \
         folder, as you first found it set up, such as `pip install -e .` or `npm install`; \
         then end with your whole answer again.",
        base.display(),
        named.join(", ")
    )
}

/// Why a second attempt made in `base` would test the first attempt's code
/// rather than its own, when it would: the project installed as editable,
/// by a `.pth`, `.egg-link` or `__editable__` file of a `site-packages`
/// folder under a linked Python store that names `workspace`, or an entry
/// of a linked `node_modules` that leads into `workspace` outside the
/// store. None when nothing of the kind is found under the stores `base`
/// links; a store elsewhere, such as the system's own `site-packages`, is
/// not looked at.
#[cfg(unix)]
pub(crate) fn imports_elsewhere(workspace: &Path, base: &Path) -> Option<String> {
    let real = std::fs::canonicalize(workspace).unwrap_or_else(|_| workspace.to_path_buf());
    let spelt = [
        workspace.to_string_lossy().into_owned(),
        real.to_string_lossy().into_owned(),
    ];
    let mut stores = Vec::new();
    linked_stores(base, 0, &mut stores);
    for store in stores {
        let name = store.file_name()?.to_string_lossy().into_owned();
        if name == "node_modules" {
            if let Some(entry) = leads_into(&store, &real) {
                return Some(format!(
                    "`{}` leads into the project, so the attempt's tests would import the \
                     first attempt's code",
                    entry.display()
                ));
            }
            continue;
        }
        let mut found = Vec::new();
        site_packages(&store, 0, &mut found);
        for packages in found {
            if let Some(file) = editable_in(&packages, &spelt) {
                return Some(format!(
                    "`{}` installs the project from where the first attempt is, so the \
                     attempt's tests would import that code",
                    file.display()
                ));
            }
        }
    }
    None
}

/// No store is linked here, so nothing leads elsewhere.
#[cfg(not(unix))]
pub(crate) fn imports_elsewhere(_workspace: &Path, _base: &Path) -> Option<String> {
    None
}

/// The links under `folder`, `depth` down, that stand for Python stores and
/// `node_modules`, as `base` makes them, added to `stores`; nothing under
/// `.git` or past `STORE_DEPTH`.
#[cfg(unix)]
fn linked_stores(folder: &Path, depth: usize, stores: &mut Vec<PathBuf>) {
    const PYTHON: [&str; 4] = [".venv", "venv", ".tox", "node_modules"];
    if depth > STORE_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(kind) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        let name = entry.file_name();
        if kind.is_symlink() {
            if PYTHON.iter().any(|store| name == *store) {
                stores.push(path);
            }
        } else if kind.is_dir() && name != ".git" {
            linked_stores(&path, depth + 1, stores);
        }
    }
}

/// The `site-packages` folders under `store`, `depth` down, added to
/// `found`; none past `STORE_DEPTH`.
#[cfg(unix)]
fn site_packages(store: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    if depth > STORE_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(store) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if entry.file_name() == "site-packages" {
            found.push(path);
        } else {
            site_packages(&path, depth + 1, found);
        }
    }
}

/// The first `.pth`, `.egg-link` or `__editable__` file of `packages`, a
/// `site-packages` folder, whose text names the project by one of `spelt`,
/// as an editable install of it does; None when none does.
#[cfg(unix)]
fn editable_in(packages: &Path, spelt: &[String]) -> Option<PathBuf> {
    use std::io::Read;
    let entries = std::fs::read_dir(packages).ok()?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        let editable = name.ends_with(".pth")
            || name.ends_with(".egg-link")
            || name.starts_with("__editable__");
        if !editable || !entry.path().is_file() {
            continue;
        }
        let Ok(file) = std::fs::File::open(entry.path()) else {
            continue;
        };
        let mut text = String::new();
        if file.take(EDITABLE_READ).read_to_string(&mut text).is_err() {
            continue;
        }
        if spelt.iter().any(|path| text.contains(path.as_str())) {
            return Some(entry.path());
        }
    }
    None
}

/// The first entry of `modules`, a linked `node_modules`, or of a scope in
/// it, that is a link leading into `real`, the project, outside the store
/// itself, as a linked or workspace package does; None when none does.
#[cfg(unix)]
fn leads_into(modules: &Path, real: &Path) -> Option<PathBuf> {
    let store = std::fs::canonicalize(modules).ok()?;
    let entries = std::fs::read_dir(modules).ok()?;
    let mut candidates = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('@')
            && let Ok(scoped) = std::fs::read_dir(&path)
        {
            candidates.extend(scoped.flatten().map(|entry| entry.path()));
        } else {
            candidates.push(path);
        }
    }
    candidates.into_iter().find(|path| {
        let linked = std::fs::symlink_metadata(path).is_ok_and(|kind| kind.is_symlink());
        linked
            && std::fs::canonicalize(path)
                .is_ok_and(|target| target.starts_with(real) && !target.starts_with(&store))
    })
}

/// `inside`, a path under the project, with `/` between folders.
fn slashed(inside: &Path) -> String {
    let parts: Vec<_> = inside.iter().map(|part| part.to_string_lossy()).collect();
    parts.join("/")
}

/// The tester's files under `from`, the copy it worked in or the project
/// itself, moved into a new folder of the temp folder at the same paths,
/// and kept for run `run` of `workspace` with `command`, the one the tester
/// gave to run them, so `tidy` deletes what it puts back and the driver can
/// run them; None when it left none. One that could not be kept is deleted
/// all the same, so none stays where a patch would take it.
pub(crate) fn keep(
    run: u64,
    from: &Path,
    workspace: &Path,
    command: Option<String>,
) -> Option<Kept> {
    let dir = folder("tests");
    let files: Vec<String> = (left(from).into_iter())
        .filter(|file| {
            let to = dir.join(file);
            let copied = to
                .parent()
                .is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
                && std::fs::copy(from.join(file), &to).is_ok();
            let _ = std::fs::remove_file(from.join(file));
            copied
        })
        .collect();
    if files.is_empty() {
        let _ = std::fs::remove_dir_all(&dir);
        return None;
    }
    let kept = Kept { dir, files };
    if let Ok(mut moved) = moved().lock() {
        moved.push(Moved {
            run,
            workspace: workspace.to_path_buf(),
            kept: kept.clone(),
            command,
            unmet: false,
        });
    }
    Some(kept)
}

/// `keep`, for the files of a tester started with run `run`: written in the
/// project as it was, they have never met the work, which `unmet` says, so
/// the driver runs them against it as soon as the run says it is done.
pub(crate) fn keep_ahead(
    run: u64,
    from: &Path,
    workspace: &Path,
    command: Option<String>,
) -> Option<Kept> {
    let kept = keep(run, from, workspace, command)?;
    if let Ok(mut moved) = moved().lock()
        && let Some(kept) = moved.iter_mut().rfind(|kept| kept.run == run)
    {
        kept.unmet = true;
    }
    Some(kept)
}

/// Whether the tester's tests kept for run `run` with a command to run
/// them were written ahead of the work, so they have never met it.
pub(crate) fn unmet(run: u64) -> bool {
    let Ok(moved) = moved().lock() else {
        return false;
    };
    (moved.iter()).any(|kept| kept.run == run && kept.unmet && kept.command.is_some())
}

/// Starts a tester for run `run` on a thread of its own, at work in
/// `folder`, which `base` made, where `test` gives its report, `cancel`
/// stops it and `until` is its deadline, if it has one, and keeps it
/// pending for `take`. False when one is pending for the run already or no
/// thread could be started, with `folder` deleted.
pub(crate) fn start(
    run: u64,
    folder: PathBuf,
    cancel: Cancel,
    until: Option<Instant>,
    test: impl FnOnce() -> Result<String, String> + Send + 'static,
) -> bool {
    let (lock, woken) = pending();
    {
        let Ok(mut pending) = lock.lock() else {
            discard(&folder);
            return false;
        };
        if pending.iter().any(|tester| tester.run == run) {
            discard(&folder);
            return false;
        }
        pending.push(Pending {
            run,
            folder: folder.clone(),
            cancel,
            until,
            report: None,
        });
    }
    let at = folder.clone();
    let started = std::thread::Builder::new()
        .name(format!("tester-{run}"))
        .spawn(move || {
            // A panic is a report too, or the run would wait the whole
            // window for one that never comes.
            let report = std::panic::catch_unwind(std::panic::AssertUnwindSafe(test))
                .unwrap_or_else(|_| Err("The tester's thread panicked.".into()));
            // Under the lock, so a run ending meanwhile finds either the
            // report or no tester to abandon, never a report on its way.
            let Ok(mut pending) = lock.lock() else {
                return;
            };
            match pending.iter_mut().find(|tester| tester.run == run) {
                Some(tester) => {
                    tester.report = Some(report);
                    woken.notify_all();
                }
                // Abandoned as its run ended: nothing reads the report, and
                // the folder is its own to delete.
                None => discard(&at),
            }
        });
    if started.is_err() {
        if let Ok(mut pending) = lock.lock() {
            pending.retain(|tester| tester.run != run);
        }
        discard(&folder);
        return false;
    }
    true
}

/// Whether a tester started with run `run` is pending: at work still, or
/// answered and not yet taken.
pub(crate) fn ahead(run: u64) -> bool {
    (pending().0.lock()).is_ok_and(|pending| pending.iter().any(|tester| tester.run == run))
}

/// The tester started with run `run`, once it has answered, waited for up
/// to `wait`: the folder it worked in, for `keep` to take its tests from and
/// `discard` after, and its report, or what ended it. None when none was
/// started with the run, or when it is still at work past `wait`: then it is
/// left to finish, and `tidy` stops it with the run if it has not by then.
pub(crate) fn take(run: u64, wait: Duration) -> Option<(PathBuf, Result<String, String>)> {
    let (lock, woken) = pending();
    let mut pending = lock.lock().ok()?;
    let until = Instant::now() + wait;
    loop {
        let at = pending.iter().position(|tester| tester.run == run)?;
        if pending[at].report.is_some() {
            let taken = pending.remove(at);
            return Some((taken.folder, taken.report?));
        }
        let left = until.checked_duration_since(Instant::now())?;
        if left.is_zero() {
            return None;
        }
        pending = woken.wait_timeout(pending, left).ok()?.0;
    }
}

/// `take`, waiting for the tester started with run `run` until its deadline
/// and `GRACE` after it, the round in which it answers with what it has,
/// but `most` at longest; `most` where it has no deadline.
pub(crate) fn take_due(run: u64, most: Duration) -> Option<(PathBuf, Result<String, String>)> {
    let wait = {
        let pending = pending().0.lock().ok()?;
        let tester = pending.iter().find(|tester| tester.run == run)?;
        match tester.until {
            Some(until) => (until + GRACE)
                .saturating_duration_since(Instant::now())
                .min(most),
            None => most,
        }
    };
    take(run, wait)
}

/// Deletes the tester's files run `run` put back in its project, at the paths
/// `keep` kept them from, and the folder they were kept in, and forgets them:
/// they are the tester's, by their names, and no part of the task, and a run
/// stopped at its deadline never deletes them itself. A tester started with
/// the run and not taken is abandoned: stopped, its report dropped, and its
/// folder deleted, here and once more as it ends, since a stopped one can
/// still be writing there. The run's first complete version goes too, and
/// the folders kept aside for it (`aside`); not the folder a second attempt
/// installed from, since the project's packages may lead into it. The paths
/// it deleted from the project, in order.
pub(crate) fn tidy(run: u64) -> Vec<String> {
    if let Ok(mut asides) = asides().lock() {
        asides.retain(|(of, dir)| {
            if *of == run {
                discard(dir);
            }
            *of != run
        });
    }
    if let Ok(mut firsts) = firsts().lock() {
        firsts.retain(|first| {
            if first.run == run {
                discard(&first.dir);
            }
            first.run != run
        });
    }
    if let Ok(mut pending) = pending().0.lock() {
        pending.retain(|tester| {
            if tester.run != run {
                return true;
            }
            tester.cancel.set();
            discard(&tester.folder);
            false
        });
    }
    let Ok(mut moved) = moved().lock() else {
        return Vec::new();
    };
    let mut deleted = Vec::new();
    moved.retain(|kept| {
        if kept.run != run {
            return true;
        }
        for file in &kept.kept.files {
            if std::fs::remove_file(kept.workspace.join(file)).is_ok() {
                deleted.push(file.clone());
            }
        }
        let _ = std::fs::remove_dir_all(&kept.kept.dir);
        false
    });
    deleted.sort();
    deleted
}

/// The files under `workspace` whose names hold `MARKER`, by their paths from
/// there, in order.
fn left(workspace: &Path) -> Vec<String> {
    (files_under(workspace).into_iter())
        .filter(|path| (path.rsplit('/').next()).is_some_and(|name| name.contains(MARKER)))
        .collect()
}

/// The files under `dir`, by their paths from there with `/` between
/// folders, in order: `WALKED` entries at most, none in the folders
/// `SKIPPED` names, and none a link leads to.
fn files_under(dir: &Path) -> Vec<String> {
    let mut walk = ignore::WalkBuilder::new(dir);
    walk.hidden(false);
    walk.filter_entry(|entry| !SKIPPED.iter().any(|skipped| entry.file_name() == *skipped));
    let mut found: Vec<String> = (walk.build().take(WALKED))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter_map(|entry| Some(slashed(entry.path().strip_prefix(dir).ok()?)))
        .collect();
    found.sort();
    found
}

/// What the run is told of the tester's `report` when it has no table of
/// tests that ran, where it left tests `kept`: a tester that could not run
/// them in its copy, such as one without the toolchain or set-up the run
/// found, still wrote each from the request, so the run runs them itself.
pub(crate) fn unrun(report: &str, kept: &Kept, again: bool) -> String {
    format!(
        "A tester that hasn't seen your work wrote tests from the request alone but could not \
         run them where it worked; its report follows. {} Run them with the project's own \
         runner and the set-up you found works, fix a test only where it calls the code wrongly, \
         such as a guessed name or import the request does not spell, and fix your work for \
         each test that fails on what the request's words say. Then delete the copies, since \
         they are not part of the task.{}\n\n{}",
        where_kept(kept),
        run_again(again),
        report.trim()
    )
}

/// What the run is told of the driver running the tester's tests against
/// its work, when it will (`again`): when the report gave the command and a
/// copy of the project can be made; nothing otherwise.
fn run_again(again: bool) -> &'static str {
    match again {
        true => {
            " Once you say you are done, they are run against your work as it stands, and \
             what fails comes back to you."
        }
        false => "",
    }
}

/// Where the tester's tests are `kept`, and how to run them, as the run is
/// told it.
fn where_kept(kept: &Kept) -> String {
    let mut named: Vec<String> = (kept.files.iter().take(NAMED))
        .map(|file| format!("`{file}`"))
        .collect();
    if kept.files.len() > NAMED {
        named.push(format!("and {} more", kept.files.len() - NAMED));
    }
    format!(
        "Its test files are kept out of the project, in `{}`, at the paths they ran from: {}. \
         Copy them back into the project at those paths to run them.",
        kept.dir.display(),
        named.join(", ")
    )
}

/// What the run is told of the tester's `report`, with its table, of where
/// its tests are `kept`, and that the driver runs them against its work,
/// when it will (`again`).
pub(crate) fn told(report: &str, kept: Option<&Kept>, again: bool) -> String {
    tell(
        "A tester that hasn't seen your work wrote tests from the request alone and ran them; \
         its report follows, ending with a table of its tests. Fix your work for each test \
         that fails, unless the test expects something the request's words do not say: then \
         quote the words that show it in your answer.",
        report,
        kept,
        again,
    )
}

/// What the run is told of the report of a tester started with it, which
/// wrote its tests in the project as it was: as `told` tells it, but that
/// its table says of each test only that it could not pass yet, so the
/// tests are read for what they expect rather than fixed for. What the
/// driver made of them against the work follows it (`first_run`), so it
/// promises no run to come.
pub(crate) fn told_ahead(report: &str, kept: Option<&Kept>) -> String {
    // Between the lines that mark it as written before the work, so its rows
    // hold the run to nothing (`requirements::rows`).
    let marked = format!(
        "{}\n{}\n{}",
        crate::requirements::AHEAD_OPENS,
        report.trim(),
        crate::requirements::AHEAD_CLOSES
    );
    tell(
        "A tester that hasn't seen your work wrote tests from the request alone while you \
         worked, in the project as it was before your change, where none could pass yet; its \
         report follows, ending with a table of its tests, whose rows say NOT RUN or FAIL for \
         want of your change, not what your work does. Make your work do what each test \
         expects where the request's words say so; where a test expects something the words \
         do not say, quote the words that show it in your answer.",
        &marked,
        kept,
        false,
    )
}

/// `opening`, then where the tester's tests are `kept` and that the driver
/// runs them when it will (`again`), then its `report`.
fn tell(opening: &str, report: &str, kept: Option<&Kept>, again: bool) -> String {
    let mut text = opening.to_owned();
    if let Some(kept) = kept {
        text.push_str(&format!(
            " {} Delete the copies before you end, since they are not part of the task.{}",
            where_kept(kept),
            run_again(again)
        ));
    }
    text.push_str(&format!("\n\n{}", report.trim()));
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Scratch;

    #[test]
    fn the_brief_gives_the_request_word_for_word_and_nothing_of_the_work() {
        let sentences = vec![
            "Pairs compose with Or.".to_owned(),
            "Targets differ.".to_owned(),
        ];
        let brief = brief(
            "  Make pairs work.\nPairs compose with Or.  ",
            &sentences,
            None,
            &[],
        );
        assert!(
            brief.contains(
                "The request, word for word:\nMake pairs work.\nPairs compose with Or.\n"
            )
        );
        assert!(brief.contains("- R1: Pairs compose with Or.\n- R2: Targets differ."));
        assert!(brief.contains("run no `git diff`"));
        assert!(brief.contains(&format!("names hold `{MARKER}`")));
        assert!(brief.contains("otherwise or and not keeps"), "{brief}");
        assert!(brief.contains("in each order"), "{brief}");
        assert!(brief.contains("uses the request's words for it"), "{brief}");
        // The nearest feature's own tests, done again for the new one.
        assert!(
            brief.contains(
                "write each of their cases again for the new one, in the same order of calls"
            ),
            "{brief}"
        );
        assert!(brief.contains("for the first time only after the change it should see"));
        assert!(brief.ends_with("so what comes last is kept."), "{brief}");
    }

    #[test]
    fn the_testers_files_are_moved_out_of_the_project_at_the_paths_they_ran_from() {
        let root = Scratch::new("spec-tests-kept");
        std::fs::create_dir_all(root.join("tests/deep")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join("tests/letronna_spec.test.ts"), "test one").unwrap();
        std::fs::write(root.join("tests/deep/test_letronna_spec.py"), "test two").unwrap();
        std::fs::write(root.join("tests/own.test.ts"), "the project's").unwrap();
        std::fs::write(
            root.join("node_modules/pkg/letronna_spec.js"),
            "a package's",
        )
        .unwrap();

        let kept = keep(1, &root, &root, None).unwrap();
        assert_eq!(
            kept.files,
            [
                "tests/deep/test_letronna_spec.py",
                "tests/letronna_spec.test.ts"
            ]
        );
        assert_eq!(
            std::fs::read_to_string(kept.dir.join("tests/letronna_spec.test.ts")).unwrap(),
            "test one"
        );
        assert!(!root.join("tests/letronna_spec.test.ts").exists());
        assert!(!root.join("tests/deep/test_letronna_spec.py").exists());
        // The project's own files, and a package's, stay where they are.
        assert!(root.join("tests/own.test.ts").exists());
        assert!(root.join("node_modules/pkg/letronna_spec.js").exists());

        // Once they are gone there is nothing more to keep, and the folder
        // they were kept in goes as the run ends.
        assert_eq!(keep(1, &root, &root, None), None);
        tidy(1);
        assert!(!kept.dir.exists());
    }

    #[test]
    fn files_in_hidden_folders_are_found_too() {
        let root = Scratch::new("spec-tests-hidden");
        std::fs::create_dir_all(root.join(".config/tests")).unwrap();
        std::fs::write(root.join(".config/tests/letronna_spec.ts"), "test").unwrap();
        assert_eq!(left(&root), [".config/tests/letronna_spec.ts"]);
    }

    #[test]
    fn names_the_request_spells_are_used_as_it_spells_them() {
        let brief = brief(
            "Save at /api/items/save.",
            &[],
            Some(Path::new("/tmp/was")),
            &[],
        );
        assert!(
            brief.contains("as the request spells it, never as the code does"),
            "{brief}"
        );
        assert!(
            brief.contains("spells it differently is a finding"),
            "{brief}"
        );
        assert!(
            brief.contains("search your working folder for that name alone"),
            "{brief}"
        );
        assert!(
            brief.contains("before the change is in `/tmp/was`"),
            "{brief}"
        );
        assert!(brief.contains("only where your tests run"), "{brief}");
    }

    #[test]
    fn with_nothing_from_before_the_brief_names_no_folder() {
        let brief = brief("Save at /api/items/save.", &[], None, &[]);
        assert!(!brief.contains("before the change is in"), "{brief}");
        assert!(brief.contains("as the request spells it"), "{brief}");
    }

    #[cfg(unix)]
    #[test]
    fn the_project_as_it_was_holds_the_last_commit_and_nothing_of_the_work() {
        let root = Scratch::new("spec-tests-before");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        // No commit yet, so nothing to write out.
        assert_eq!(before(&root), None);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("src/lib.ts"), "export const a = 1;").unwrap();
        // What an archive of the project would leave out or rewrite.
        std::fs::write(root.join("tests/lib.test.ts"), "test('a', () => {});").unwrap();
        std::fs::write(root.join("version.txt"), "$Format:%H$").unwrap();
        std::fs::write(
            root.join(".gitattributes"),
            "tests/ export-ignore\nversion.txt export-subst\n",
        )
        .unwrap();
        repo(&["add", "."]);
        repo(&[
            "-c",
            "user.name=user_a",
            "-c",
            "user.email=user_a@example.com",
            "commit",
            "-q",
            "-m",
            "first",
        ]);
        // The run's change, and a new file of its.
        std::fs::write(root.join("src/lib.ts"), "export const a = 2;").unwrap();
        std::fs::write(root.join("src/new.ts"), "export const b = 3;").unwrap();

        let was = before(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(was.join("src/lib.ts")).unwrap(),
            "export const a = 1;"
        );
        assert!(!was.join("src/new.ts").exists());
        assert!(!was.join(".git").exists());
        // The tests folder is there and the placeholder is as committed.
        assert!(was.join("tests/lib.test.ts").exists());
        assert_eq!(
            std::fs::read_to_string(was.join("version.txt")).unwrap(),
            "$Format:%H$"
        );
        assert!(!was.with_extension("index").exists());
        discard(&was);
        assert!(!was.exists());
        // The project itself is as the run left it.
        assert_eq!(
            std::fs::read_to_string(root.join("src/lib.ts")).unwrap(),
            "export const a = 2;"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_tester_works_in_a_copy_that_leaves_the_project_as_it_was() {
        let root = Scratch::new("spec-tests-copy");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        std::fs::write(root.join("src/lib.ts"), "export const a = 1;").unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), "a package").unwrap();
        repo(&["add", "."]);
        // The run's change, and a new file of its that git does not track yet.
        std::fs::write(root.join("src/lib.ts"), "export const a = 2;").unwrap();
        std::fs::write(root.join("src/new.ts"), "export const b = 3;").unwrap();

        let copy = copy(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(copy.join("src/lib.ts")).unwrap(),
            "export const a = 2;"
        );
        assert!(copy.join("src/new.ts").exists());
        assert!(!copy.join(".git").exists());
        // What git ignores is linked, as it is.
        assert!(
            std::fs::symlink_metadata(copy.join("node_modules"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(copy.join("node_modules/pkg/index.js")).unwrap(),
            "a package"
        );

        // The tester's test, and a change it was told not to make, stay in
        // the copy.
        std::fs::write(copy.join("src/letronna_spec.test.ts"), "test").unwrap();
        std::fs::write(copy.join("src/lib.ts"), "changed").unwrap();
        let kept = keep(2, &copy, &root, None).unwrap();
        assert_eq!(kept.files, ["src/letronna_spec.test.ts"]);
        discard(&copy);
        assert!(!copy.exists());
        assert!(!root.join("src/letronna_spec.test.ts").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("src/lib.ts")).unwrap(),
            "export const a = 2;"
        );
        assert!(root.join("node_modules/pkg/index.js").exists());
        tidy(2);
    }

    #[test]
    fn a_folder_that_is_not_the_top_of_a_git_project_is_not_copied() {
        let root = Scratch::new("spec-tests-no-git");
        assert_eq!(copy(&root), None);
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::create_dir_all(root.join("inner")).unwrap();
        assert_eq!(copy(&root.join("inner")), None);
    }

    #[test]
    fn what_the_run_put_back_is_deleted_as_it_ends_and_nothing_else() {
        let root = Scratch::new("spec-tests-tidied");
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/letronna_spec_a_test.go"), "test").unwrap();
        let kept = keep(3, &root, &root, None).unwrap();
        // Another run in the same folder has a tester of its own.
        std::fs::write(root.join("tests/letronna_spec_b_test.go"), "test").unwrap();
        let other = keep(4, &root, &root, None).unwrap();
        // The run copies its test back, changes it, and makes one of its own,
        // and the other run copies its test back too.
        std::fs::write(root.join("tests/letronna_spec_a_test.go"), "changed").unwrap();
        std::fs::write(root.join("tests/own_test.go"), "the run's").unwrap();
        std::fs::write(root.join("tests/letronna_spec_b_test.go"), "put back").unwrap();

        assert_eq!(tidy(3), ["tests/letronna_spec_a_test.go"]);
        assert!(!root.join("tests/letronna_spec_a_test.go").exists());
        assert!(root.join("tests/own_test.go").exists());
        assert!(!kept.dir.exists());
        // The other run's is its own to end.
        assert!(root.join("tests/letronna_spec_b_test.go").exists());
        assert!(other.dir.exists());
        // And each run's paths are tidied once.
        std::fs::write(root.join("tests/letronna_spec_a_test.go"), "again").unwrap();
        assert!(tidy(3).is_empty());
        assert_eq!(tidy(4), ["tests/letronna_spec_b_test.go"]);
    }

    #[test]
    fn the_run_is_told_where_the_tests_are_kept_and_to_delete_the_copies() {
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["tests/letronna_spec.test.ts".to_owned()],
        };
        let text = told(
            "Report.\nR1 | adds a pair | FAIL | npx vitest run x | got []\n",
            Some(&kept),
            false,
        );
        assert!(text.contains("kept out of the project, in `/tmp/kept`"));
        assert!(text.contains("`tests/letronna_spec.test.ts`"));
        assert!(text.contains("Delete the copies before you end"));
        assert!(text.ends_with("R1 | adds a pair | FAIL | npx vitest run x | got []"));
        assert!(!told("R1 | x | PASS", None, false).contains("kept out of the project"));
    }

    #[test]
    fn tests_a_tester_could_not_run_are_left_for_the_run_to_run() {
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["drift/letronna_spec_test.go".to_owned()],
        };
        let report = "No Go toolchain in my copy.\nR1 | resolves | NOT RUN";
        let text = unrun(report, &kept, false);
        assert!(
            text.contains("could not run them where it worked"),
            "{text}"
        );
        assert!(text.contains("kept out of the project, in `/tmp/kept`"));
        assert!(text.contains("`drift/letronna_spec_test.go`"));
        assert!(text.contains("the set-up you found works"), "{text}");
        assert!(
            text.contains("fix your work for each test that fails"),
            "{text}"
        );
        assert!(text.ends_with(report));
    }

    #[test]
    fn the_tester_is_asked_for_the_one_command_that_runs_its_tests() {
        let brief = brief("Save at /api/items/save.", &[], None, &[]);
        assert!(brief.contains("written `Run: <command>`"), "{brief}");
        assert!(
            brief.contains("from the project's root, with paths relative to it"),
            "{brief}"
        );
        assert!(
            brief.contains("runs them once and exits rather than watching, installs nothing"),
            "{brief}"
        );
        // The table first, and the command as the report's last line, where
        // the cut a long report takes keeps it.
        let table = brief
            .find("End your report with a table of one row per test")
            .unwrap();
        let run = brief.find("written `Run: <command>`").unwrap();
        assert!(table < run, "{brief}");
        assert!(brief.ends_with("so what comes last is kept."), "{brief}");
    }

    #[test]
    fn the_command_survives_the_cut_a_long_report_takes() {
        // A report longer than the agent tool quotes is cut in the middle,
        // keeping both ends, and each line indented, as `agents::answer`
        // quotes it: the command, written last, is kept and read; written
        // before a long table, it would be lost.
        let rows: Vec<String> = (1..=40)
            .map(|n| {
                format!(
                    "R{n} | case {n} of the request, checked end to end | FAIL | pytest -q \
                     tests/test_letronna_spec.py::test_{n} | not yet implemented"
                )
            })
            .collect();
        let table = rows.join("\n");
        let prose = "Wrote the tests from the request. ".repeat(70);
        let given = "python -m pytest -q tests/test_letronna_spec.py";
        let quoted = |report: &str| -> String {
            let cut = crate::chat::trim(report, crate::agents::MAX_REPORT);
            assert!(cut.contains("[middle trimmed]"), "{cut}");
            let lines: Vec<String> = cut.lines().map(|line| format!("    {line}")).collect();
            lines.join("\n")
        };
        let last = format!("{prose}\n{table}\nRun: {given}");
        assert_eq!(command(&quoted(&last)).as_deref(), Some(given));
        let first = format!("{prose}\nRun: {given}\n{table}");
        assert_eq!(command(&quoted(&first)), None);
        // Blank lines or a stray row after it do not hide it.
        let trailing = format!("{last}\n\nR41 | a late row | PASS\n\n");
        assert_eq!(command(&trailing).as_deref(), Some(given));
    }

    #[test]
    fn the_command_is_read_off_the_reports_run_line() {
        let report =
            "Ran them.\nRun: `npx vitest run tests/letronna_spec.test.ts`\nR1 | adds | PASS";
        assert_eq!(
            command(report).as_deref(),
            Some("npx vitest run tests/letronna_spec.test.ts")
        );
        let bold = "**Run:** pytest -q tests/test_letronna_spec.py\nR1 | x | FAIL | pytest | got 2";
        assert_eq!(
            command(bold).as_deref(),
            Some("pytest -q tests/test_letronna_spec.py")
        );
        // The last where it gave more than one; none without the line, with
        // nothing on it, or with a script's worth.
        assert_eq!(
            command("Run: go test ./a\nRun: go test ./...").as_deref(),
            Some("go test ./...")
        );
        assert_eq!(command("R1 | x | PASS"), None);
        assert_eq!(command("Run:\nR1 | x | PASS"), None);
        assert_eq!(command(&format!("Run: {}", "x".repeat(LINE + 1))), None);
        // The part in backticks, whatever follows them; not words saying
        // there is no command; nothing that installs.
        assert_eq!(
            command("Run: `pytest -q tests/test_letronna_spec.py`.").as_deref(),
            Some("pytest -q tests/test_letronna_spec.py")
        );
        assert_eq!(command("Run: N/A — no Go toolchain in my copy"), None);
        assert_eq!(command("Run: none, the tests could not run"), None);
        assert_eq!(command("Run: pip install -e . && pytest -q"), None);
        assert_eq!(command("Run: npm install && npx vitest run"), None);
        assert_eq!(command("Run: uv run pytest -q"), None);
        // More than one span of backticks is prose about commands.
        assert_eq!(command("Run: `cd tests` then `pytest -q`"), None);
    }

    #[test]
    fn a_command_spelt_for_the_testers_folder_is_made_relative() {
        let copy = Path::new("/tmp/letronna-spec-copy-1");
        let project = Path::new("/work/project");
        assert_eq!(
            relative(
                "cd /tmp/letronna-spec-copy-1 && pytest /tmp/letronna-spec-copy-1/tests/t_letronna_spec.py",
                &[copy, project]
            ),
            "cd . && pytest ./tests/t_letronna_spec.py"
        );
        assert_eq!(
            relative("go test /work/project/drift/...", &[copy, project]),
            "go test ./drift/..."
        );
        assert_eq!(
            relative("npx vitest run", &[copy, project]),
            "npx vitest run"
        );
        // Only where the folder stands as a path of its own: a project at
        // `/app` leaves `src/app` and `/apps` alone.
        let app = [Path::new("/app")];
        assert_eq!(
            relative("go test ./internal/app/...", &app),
            "go test ./internal/app/..."
        );
        assert_eq!(
            relative("pytest src/app/tests/t_letronna_spec.py", &app),
            "pytest src/app/tests/t_letronna_spec.py"
        );
        assert_eq!(
            relative("cd /apps/foo && pytest", &app),
            "cd /apps/foo && pytest"
        );
        assert_eq!(
            relative("pytest /app/tests/t.py --rootdir=/app", &app),
            "pytest ./tests/t.py --rootdir=."
        );
        assert_eq!(relative("sh /app/app/run.sh", &app), "sh ./app/run.sh");
    }

    #[test]
    fn the_run_hears_that_the_tests_are_run_against_its_work_when_the_report_says_how() {
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["tests/letronna_spec.test.ts".to_owned()],
        };
        let report = "Run: npx vitest run tests/letronna_spec.test.ts\nR1 | adds | FAIL | npx vitest run x | got []";
        for said in [true, false] {
            let told = told(report, Some(&kept), said);
            let unrun = unrun(report, &kept, said);
            for text in [told, unrun] {
                assert_eq!(
                    text.contains("they are run against your work as it stands"),
                    said,
                    "{text}"
                );
                assert!(text.ends_with(report), "{text}");
            }
        }
    }

    #[test]
    fn with_no_command_there_is_nothing_for_the_driver_to_run() {
        let root = Scratch::new("spec-tests-no-command");
        std::fs::write(root.join("letronna_spec.txt"), "a test").unwrap();
        assert!(keep(8, &root, &root, None).is_some());
        assert_eq!(to_run(8), None);
        tidy(8);
        assert_eq!(to_run(8), None);
    }

    #[cfg(unix)]
    #[test]
    fn the_work_as_it_stands_is_run_against_the_kept_tests_in_a_copy_of_the_project() {
        let root = Scratch::new("spec-tests-run");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join("README"), "a project").unwrap();
        repo(&["add", "."]);
        // The tester's test: the work has to make `made.txt`.
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            "test -f made.txt || { echo 'made.txt is missing'; exit 1; }\n",
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(6, &root, &root, Some(command.clone())).unwrap();
        assert_eq!(to_run(6), Some((kept.clone(), command.clone())));
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });

        // Before the work is there the test fails, the run hears what the
        // runner printed and where the test is kept, and the project got
        // neither the test nor anything the test made.
        let Ran::Failed(text) = ran(&kept, &command, &ctx, TIMEOUT, || None) else {
            panic!("the test did not fail");
        };
        assert!(
            text.contains("`sh letronna_spec_test.sh` exited with code 1"),
            "{text}"
        );
        assert!(text.contains("made.txt is missing"), "{text}");
        assert!(
            text.contains(&format!("in `{}`", kept.dir.display())),
            "{text}"
        );
        assert!(text.contains("Then end with your whole answer again."));
        assert!(!root.join("letronna_spec_test.sh").exists());
        assert!(root.join("README").exists());

        // Once the work is there, they passed.
        std::fs::write(root.join("made.txt"), "made").unwrap();
        assert_eq!(
            ran(&kept, &command, &ctx, TIMEOUT, || None),
            Ran::Passed(command.clone())
        );
        assert!(!root.join("letronna_spec_test.sh").exists());
        tidy(6);
        assert_eq!(to_run(6), None);
        // Where no copy of the project can be made they are not run.
        let plain = Scratch::new("spec-tests-run-no-git");
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(plain.to_path_buf()),
            ..Default::default()
        });
        assert_eq!(
            ran(&kept, &command, &ctx, TIMEOUT, || None),
            Ran::Unrun("no copy of the project could be made to run them in".into())
        );
    }

    #[test]
    fn the_first_run_of_tests_written_ahead_says_how_it_went() {
        let passed = first_run(&Ran::Passed("pytest -q".into()));
        assert!(passed.contains("`pytest -q` passed"), "{passed}");
        assert!(
            passed.contains("run again once you say you are done"),
            "{passed}"
        );
        assert_eq!(first_run(&Ran::Failed("the notice".into())), "the notice");
        let unrun = first_run(&Ran::Unrun(
            "the gate would not run `make test` as it stands".into(),
        ));
        assert!(
            unrun.starts_with(
                "Those tests could not be run against your work as it stands: the gate would \
                 not run `make test` as it stands. Run them yourself"
            ),
            "{unrun}"
        );
        assert!(
            unrun.ends_with("delete the copies before you end."),
            "{unrun}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_command_that_does_not_finish_is_reported_as_such() {
        let root = Scratch::new("spec-tests-run-slow");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join("README"), "a project").unwrap();
        repo(&["add", "."]);
        std::fs::write(root.join("letronna_spec_slow.sh"), "sleep 30\n").unwrap();
        let command = "sh letronna_spec_slow.sh".to_owned();
        let kept = keep(7, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        let Ran::Failed(text) = ran(&kept, &command, &ctx, Duration::from_secs(1), || None) else {
            panic!("the command finished");
        };
        assert!(
            text.contains("`sh letronna_spec_slow.sh` did not finish"),
            "{text}"
        );
        assert!(text.contains("Timed out after 1s"), "{text}");
        // Without the tool's advice to a model to call it again.
        assert!(!text.contains("If it only needs longer"), "{text}");
        // A build of the whole workspace cut at its limit says nothing of
        // the work: not a failure to send the run back with.
        let build = "sleep 30; cargo test".to_owned();
        assert_eq!(
            ran(&kept, &build, &ctx, Duration::from_secs(1), || None),
            Ran::Unrun("the build did not fit in the time".into())
        );
        tidy(7);
    }

    #[test]
    fn a_python_command_runs_with_the_copy_first_on_the_import_path() {
        let copy = Path::new("/tmp/letronna-spec-copy-7");
        let prefix = "export PYTHONPATH=\"/tmp/letronna-spec-copy-7/src:/tmp/letronna-spec-copy-7\
                      ${PYTHONPATH:+:$PYTHONPATH}\"; ";
        for line in [
            "python -m pytest -q tests/test_letronna_spec.py",
            "pytest -q",
            "cd . && python3 -m pytest",
            "export A=1; python3.12 tests/letronna_spec_check.py",
            "PYTHONPATH=. py.test tests",
            "coverage run -m pytest tests",
        ] {
            assert_eq!(
                with_import_path(line, copy),
                format!("{prefix}{line}"),
                "{line}"
            );
        }
        // Another language's command, or one that only sets up, runs as it is.
        for line in [
            "npx vitest run",
            "cargo test",
            "sh letronna_spec_test.sh",
            "export A=1",
        ] {
            assert_eq!(with_import_path(line, copy), line, "{line}");
        }
    }

    #[test]
    fn a_folder_kept_aside_for_the_run_goes_as_it_ends() {
        let dir = folder("aside");
        std::fs::create_dir_all(&dir).unwrap();
        aside(31, &dir);
        let other = folder("aside-other");
        std::fs::create_dir_all(&other).unwrap();
        aside(32, &other);
        tidy(31);
        assert!(!dir.exists());
        assert!(other.exists());
        tidy(32);
        assert!(!other.exists());
    }

    #[test]
    fn a_runner_that_ran_no_test_proves_nothing() {
        // Cargo counts once per test binary; a filter matching nothing
        // leaves every count at 0, while doc-tests with none beside tests
        // that ran are a run.
        assert!(ran_nothing(
            "running 0 tests\n\ntest result: ok. 0 passed; 0 failed; 0 ignored; 42 filtered \
             out\n\n   Doc-tests meta\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed"
        ));
        assert!(!ran_nothing(
            "test result: ok. 3 passed; 0 failed\n\ntest result: ok. 0 passed; 0 failed"
        ));
        // Go marks each package's line; nothing ran only where every one is.
        assert!(ran_nothing(
            "testing: warning: no tests to run\nPASS\nok  \tpkg\t0.002s [no tests to run]"
        ));
        assert!(ran_nothing(
            "?   \tpkg/a\t[no test files]\n?   \tpkg/b\t[no test files]"
        ));
        assert!(!ran_nothing(
            "ok  \tpkg/a\t0.1s\n?   \tpkg/b\t[no test files]"
        ));
        // Jest, vitest, pytest, unittest and mocha each say it their way.
        assert!(ran_nothing("Tests:       0 total\nSnapshots:   0 total"));
        assert!(ran_nothing("No tests found, exiting with code 0"));
        assert!(ran_nothing("No test files found, exiting with code 0"));
        assert!(ran_nothing("collected 0 items\n\nno tests ran in 0.01s"));
        assert!(ran_nothing("Ran 0 tests in 0.000s\n\nOK"));
        assert!(ran_nothing("  0 passing (2ms)"));
        assert!(!ran_nothing("3 passed in 0.2s"));
        assert!(!ran_nothing("Ran 12 tests in 0.1s\n\nOK"));
        assert!(!ran_nothing("(no output)"));
    }

    #[test]
    fn only_cargo_builds_the_whole_workspace_again_in_a_copy() {
        assert!(rebuilds_all("cargo test -p meta"));
        assert!(rebuilds_all("cd crates/meta && cargo test"));
        assert!(rebuilds_all("/usr/local/bin/cargo nextest run"));
        assert!(!rebuilds_all("go test ./..."));
        assert!(!rebuilds_all("pytest -q tests"));
        assert!(!rebuilds_all("npm run cargo-like"));
    }

    #[cfg(unix)]
    #[test]
    fn a_commands_verdict_is_a_pass_a_failure_or_nothing_run() {
        let root = Scratch::new("spec-tests-verdict");
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        let verdict = |line: &str| verdict(line, &root, &ctx, TIMEOUT);
        assert_eq!(verdict("echo '3 passed'"), Some(true));
        assert_eq!(verdict("true"), Some(true));
        assert_eq!(verdict("false"), Some(false));
        assert_eq!(verdict("echo 'test result: ok. 0 passed; 0 failed'"), None);
    }

    #[test]
    fn the_run_hears_which_test_passed_before_its_change_and_what_to_do() {
        let text = broken(
            "cargo test -p meta",
            "test parses ... FAILED\n\n[exit code 101]",
        );
        assert!(
            text.starts_with(
                "`cargo test -p meta` passes on the project as it was and fails with your \
                 change. Its output ends:\n\ntest parses ... FAILED\n\n[exit code 101]\n\n\
                 Run it once more first. If it still fails, then where the request's words \
                 change what that test expects, update the test's expectation and quote the \
                 words in your answer; otherwise fix the code so it passes again. Do not \
                 delete, skip or weaken the test."
            ),
            "{text}"
        );
        assert!(text.ends_with("Then end with your whole answer again."));
    }

    #[test]
    fn the_output_shown_is_its_end_from_a_lines_start() {
        let short = "a\nb";
        assert_eq!(tail(short), "a\nb");
        let lines: Vec<String> = (0..400)
            .map(|at| format!("line {at:04} {}", "x".repeat(20)))
            .collect();
        let long = lines.join("\n");
        let shown = tail(&long);
        assert!(shown.starts_with("…\nline "), "{shown}");
        assert!(shown.ends_with(lines.last().unwrap()), "{shown}");
        assert!(shown.chars().count() <= SHOWN + 2, "{}", shown.len());
    }

    #[cfg(unix)]
    #[test]
    fn the_base_holds_the_last_commit_with_links_to_what_git_ignores_and_nothing_of_the_work() {
        let root = Scratch::new("spec-tests-base");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        let write = |path: &str, text: &str| {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), text).unwrap();
        };
        write(
            ".gitignore",
            "node_modules/\n.venv/\ndist/\nbuild/\n__pycache__/\n*.egg-info/\n",
        );
        write("src/lib.ts", "export const a = 1;");
        // Package stores, and what the project builds or packages.
        write("node_modules/pkg/index.js", "a package");
        write(".venv/lib/site.py", "a venv");
        write("dist/bundle.js", "built");
        write("build/lib/lib.js", "built too");
        write("src/__pycache__/lib.pyc", "compiled");
        write("pkg.egg-info/PKG-INFO", "packaged");
        repo(&["add", "."]);
        // No commit yet, so no project as it was.
        assert_eq!(base(&root), None);
        repo(&[
            "-c",
            "user.name=user_a",
            "-c",
            "user.email=user_a@example.com",
            "commit",
            "-q",
            "-m",
            "first",
        ]);
        // The run's change, a new file of its, a package it installed, and a
        // new module of its with its own compiled files and packages.
        write("src/lib.ts", "export const a = 2;");
        write("src/new.ts", "export const b = 3;");
        write("node_modules/other/index.js", "another");
        write("newmod/__pycache__/mod.pyc", "compiled");
        write("newmod/node_modules/dep/index.js", "a dependency");

        let was = base(&root).unwrap();
        assert!(was.to_string_lossy().contains("-spec-before-"), "{was:?}");
        assert_eq!(
            std::fs::read_to_string(was.join("src/lib.ts")).unwrap(),
            "export const a = 1;"
        );
        assert!(!was.join("src/new.ts").exists());
        assert!(!was.join(".git").exists());
        // The package stores are linked as they are, what was installed
        // since included.
        for store in ["node_modules", ".venv"] {
            assert!(
                std::fs::symlink_metadata(was.join(store))
                    .unwrap()
                    .is_symlink(),
                "{store}"
            );
        }
        assert_eq!(
            std::fs::read_to_string(was.join("node_modules/pkg/index.js")).unwrap(),
            "a package"
        );
        assert_eq!(
            std::fs::read_to_string(was.join("node_modules/other/index.js")).unwrap(),
            "another"
        );
        // Not what the work may have built, packaged or compiled, nor
        // anything under a folder the work added.
        for left_out in [
            "dist",
            "build",
            "src/__pycache__",
            "pkg.egg-info",
            "newmod",
            "newmod/node_modules",
        ] {
            assert!(
                std::fs::symlink_metadata(was.join(left_out)).is_err(),
                "{left_out}"
            );
        }

        // The tester's test is kept from there as one that has not met the
        // work, and the base goes.
        std::fs::write(was.join("src/letronna_spec.test.ts"), "test").unwrap();
        let kept = keep_ahead(9, &was, &root, Some("npx vitest run".into())).unwrap();
        assert_eq!(kept.files, ["src/letronna_spec.test.ts"]);
        assert!(unmet(9));
        discard(&was);
        assert!(!was.exists());
        // The project is as the run left it, packages included.
        assert_eq!(
            std::fs::read_to_string(root.join("src/lib.ts")).unwrap(),
            "export const a = 2;"
        );
        assert!(root.join("node_modules/pkg/index.js").exists());
        assert!(!root.join("src/letronna_spec.test.ts").exists());
        tidy(9);
        assert!(!unmet(9));
        // Not for a folder below the top of the project.
        assert_eq!(base(&root.join("src")), None);
    }

    #[test]
    fn tests_kept_at_the_runs_end_met_the_work_and_those_kept_ahead_of_it_did_not() {
        let root = Scratch::new("spec-tests-unmet");
        std::fs::write(root.join("letronna_spec_a.sh"), "a").unwrap();
        assert!(keep(14, &root, &root, Some("sh letronna_spec_a.sh".into())).is_some());
        assert!(!unmet(14));
        // Without a command there is nothing for the driver to run either way.
        std::fs::write(root.join("letronna_spec_b.sh"), "b").unwrap();
        assert!(keep_ahead(15, &root, &root, None).is_some());
        assert!(!unmet(15));
        std::fs::write(root.join("letronna_spec_c.sh"), "c").unwrap();
        assert!(keep_ahead(16, &root, &root, Some("sh letronna_spec_c.sh".into())).is_some());
        assert!(unmet(16));
        for run in [14, 15, 16] {
            tidy(run);
        }
        assert!(!unmet(16));
    }

    #[test]
    fn a_tester_started_with_the_run_is_taken_once_it_has_answered() {
        let at = folder("ahead-taken");
        std::fs::create_dir_all(&at).unwrap();
        assert!(!ahead(11));
        let report =
            "Run: sh letronna_spec.sh\nR1 | a | FAIL | sh letronna_spec.sh | not yet implemented";
        let test = || Ok(report.to_owned());
        assert!(start(11, at.clone(), Cancel::default(), None, test));
        assert!(ahead(11));
        // A second one is not started while one is pending, and its folder goes.
        let other = folder("ahead-second");
        std::fs::create_dir_all(&other).unwrap();
        let none = || Ok(String::new());
        assert!(!start(11, other.clone(), Cancel::default(), None, none));
        assert!(!other.exists());
        let (from, taken) = take(11, Duration::from_secs(30)).unwrap();
        assert_eq!(from, at);
        assert_eq!(taken.as_deref(), Ok(report));
        // Taken once: nothing is pending now, and the folder is the taker's.
        assert!(!ahead(11));
        assert_eq!(take(11, Duration::ZERO), None);
        assert!(at.exists());
        discard(&at);
        assert!(!at.exists());
    }

    #[test]
    fn a_tester_still_at_work_is_left_to_finish_and_stopped_as_the_run_ends() {
        let at = folder("ahead-slow");
        std::fs::create_dir_all(&at).unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        let cancel = Cancel::default();
        let test = move || {
            let _ = held.recv();
            Ok("late".to_owned())
        };
        assert!(start(12, at.clone(), cancel.clone(), None, test));
        // Not finished within the wait: left pending, not taken, not stopped.
        assert_eq!(take(12, Duration::from_millis(50)), None);
        assert!(ahead(12));
        assert!(!cancel.is_set());
        // The run ends: the tester is stopped and forgotten, and its folder
        // goes at once, and once more as it finishes, with what it still
        // wrote there.
        tidy(12);
        assert!(cancel.is_set());
        assert!(!ahead(12));
        assert!(!at.exists());
        std::fs::create_dir_all(&at).unwrap();
        release.send(()).unwrap();
        let until = Instant::now() + Duration::from_secs(30);
        while at.exists() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(!at.exists());
        assert_eq!(take(12, Duration::ZERO), None);
    }

    #[test]
    fn a_tester_whose_thread_panics_reports_that_rather_than_nothing() {
        let at = folder("ahead-panic");
        std::fs::create_dir_all(&at).unwrap();
        let test = || -> Result<String, String> { panic!("the tester's thread broke") };
        assert!(start(17, at.clone(), Cancel::default(), None, test));
        let (from, report) = take(17, Duration::from_secs(30)).unwrap();
        assert_eq!(from, at);
        assert_eq!(report, Err("The tester's thread panicked.".to_owned()));
        assert!(!ahead(17));
        discard(&at);
    }

    #[test]
    fn a_tester_past_its_deadline_is_waited_for_one_round_and_no_longer() {
        let at = folder("ahead-due");
        std::fs::create_dir_all(&at).unwrap();
        let (release, held) = std::sync::mpsc::channel::<()>();
        // Its deadline and the round after it are past already.
        let until = Instant::now().checked_sub(GRACE);
        let test = move || {
            let _ = held.recv();
            Ok("late".to_owned())
        };
        assert!(start(18, at.clone(), Cancel::default(), until, test));
        let began = Instant::now();
        assert_eq!(take_due(18, Duration::from_secs(30)), None);
        assert!(began.elapsed() < Duration::from_secs(5));
        // One with no deadline is waited for as long as allowed.
        let other = folder("ahead-due-none");
        std::fs::create_dir_all(&other).unwrap();
        let (keep_held, held_too) = std::sync::mpsc::channel::<()>();
        let test = move || {
            let _ = held_too.recv();
            Ok("later".to_owned())
        };
        assert!(start(19, other.clone(), Cancel::default(), None, test));
        let began = Instant::now();
        assert_eq!(take_due(19, Duration::from_millis(100)), None);
        assert!(began.elapsed() >= Duration::from_millis(100));
        release.send(()).unwrap();
        keep_held.send(()).unwrap();
        tidy(18);
        tidy(19);
        assert!(!ahead(18) && !ahead(19));
    }

    #[test]
    fn a_report_nobody_took_goes_with_the_run() {
        let at = folder("ahead-untaken");
        std::fs::create_dir_all(&at).unwrap();
        let test = || Ok("report".to_owned());
        assert!(start(13, at.clone(), Cancel::default(), None, test));
        let answered = || {
            let pending = pending().0.lock().unwrap();
            (pending.iter().find(|tester| tester.run == 13)).is_some_and(|t| t.report.is_some())
        };
        let until = Instant::now() + Duration::from_secs(30);
        while !answered() && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(answered());
        tidy(13);
        assert!(!ahead(13));
        assert!(!at.exists());
    }

    #[test]
    fn a_tester_started_with_the_run_has_a_third_of_its_time_up_to_twenty_minutes() {
        let minutes = |n: u64| Duration::from_secs(n * 60);
        assert_eq!(time_ahead(minutes(30)), minutes(10));
        assert_eq!(time_ahead(minutes(60)), minutes(20));
        assert_eq!(time_ahead(minutes(120)), minutes(20));
    }

    #[test]
    fn a_tester_started_with_the_run_is_told_its_tests_cannot_pass_yet() {
        let sentences = vec!["Pairs compose with Or.".to_owned()];
        let brief = brief_ahead("  Make pairs work.\nPairs compose with Or.  ", &sentences);
        assert!(
            brief.contains(
                "holds the project as it was before the change, and the change never reaches it"
            ),
            "{brief}"
        );
        assert!(brief.contains("They cannot pass yet"), "{brief}");
        assert!(
            brief.contains("do not make them pass, and make no part of the change yourself"),
            "{brief}"
        );
        assert!(
            brief.contains("each fails for want of the change alone"),
            "{brief}"
        );
        assert!(
            brief.contains("take the name from the request's own words"),
            "{brief}"
        );
        assert!(brief.contains("written `Run: <command>`"), "{brief}");
        assert!(brief.contains("`R1 | what it checks | NOT RUN`"), "{brief}");
        assert!(
            brief.contains(
                "not yet implemented`: none can pass before the change, and one that does \
                 tests nothing of it. Then, on a line of its own as the report's very last line"
            ),
            "{brief}"
        );
        assert!(brief.ends_with("so what comes last is kept."), "{brief}");
        // Its folder first on the import path, so an installed copy of the
        // project, which may be the work, is not what its tests import.
        assert!(
            brief.contains("with your folder first on the import path"),
            "{brief}"
        );
        assert!(brief.contains("`python -m pytest`"), "{brief}");
        assert!(brief.contains("`PYTHONPATH=.`"), "{brief}");
        assert!(
            brief.contains("the module under test resolves under your folder"),
            "{brief}"
        );
        assert!(
            brief.contains("report each test as NOT RUN with that reason"),
            "{brief}"
        );
        // What it shares with the brief of a tester started at the run's end.
        assert!(
            brief.contains(
                "The request, word for word:\nMake pairs work.\nPairs compose with Or.\n"
            )
        );
        assert!(brief.contains("- R1: Pairs compose with Or."), "{brief}");
        assert!(brief.contains(&format!("names hold `{MARKER}`")), "{brief}");
        assert!(brief.contains("in each order"), "{brief}");
        assert!(
            brief.contains("as the request spells it, never as the code does"),
            "{brief}"
        );
        // And not what only that one needs: there is no work to look at and
        // no other folder to read.
        assert!(!brief.contains("run no `git diff`"), "{brief}");
        assert!(!brief.contains("before the change is in"), "{brief}");
        assert!(!brief.contains("search your working folder"), "{brief}");
        assert!(
            !brief.contains("what it expected and what it got"),
            "{brief}"
        );
    }

    #[test]
    fn the_run_hears_that_tests_written_ahead_of_its_work_could_not_pass_yet() {
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["tests/letronna_spec.test.ts".to_owned()],
        };
        // A row with a reason of its own in place of the placeholder, as a
        // tester may write it: between the marks it holds the run to nothing.
        let report =
            "R1 | adds | FAIL | npx vitest run | adds is not exported yet\nRun: npx vitest run";
        let text = told_ahead(report, Some(&kept));
        assert!(!crate::requirements::tabled(&text), "{text}");
        assert!(
            text.contains("while you worked, in the project as it was before your change"),
            "{text}"
        );
        assert!(
            text.contains("rows say NOT RUN or FAIL for want of your change"),
            "{text}"
        );
        assert!(text.contains("kept out of the project, in `/tmp/kept`"));
        assert!(text.contains("`tests/letronna_spec.test.ts`"));
        // What the driver made of the tests follows it, so it promises no
        // run to come.
        assert!(!text.contains("Once you say you are done"), "{text}");
        assert!(
            text.ends_with(&format!(
                "{}\n{report}\n{}",
                crate::requirements::AHEAD_OPENS,
                crate::requirements::AHEAD_CLOSES
            )),
            "{text}"
        );
        let bare = told_ahead(report, None);
        assert!(!bare.contains("kept out of the project"), "{bare}");
    }

    /// Commits what is in `root`, as user_a.
    #[cfg(unix)]
    fn committed(root: &Path, message: &str) {
        for args in [
            vec!["add", "."],
            vec![
                "-c",
                "user.name=user_a",
                "-c",
                "user.email=user_a@example.com",
                "commit",
                "-q",
                "-m",
                message,
            ],
        ] {
            assert!(git(root, &args).is_some(), "git {args:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_second_attempt_starts_from_the_last_commit_with_what_git_ignores_linked() {
        let root = Scratch::new("spec-tests-base");
        // No project, then one with no commit: nothing to start from.
        assert_eq!(baseline(&root), None);
        assert!(git(&root, &["init", "-q"]).is_some());
        assert_eq!(baseline(&root), None);
        assert_eq!(head(&root), None);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::create_dir_all(root.join("dist")).unwrap();
        std::fs::create_dir_all(root.join("sub/.venv")).unwrap();
        std::fs::write(root.join(".gitignore"), "node_modules/\ndist/\n.venv/\n").unwrap();
        std::fs::write(root.join("src/lib.ts"), "export const a = 1;").unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), "a package").unwrap();
        std::fs::write(root.join("dist/lib.js"), "built").unwrap();
        std::fs::write(root.join("sub/.venv/pyvenv.cfg"), "a venv").unwrap();
        // With files that are not committed yet, nor.
        assert_eq!(baseline(&root), None);
        committed(&root, "first");
        let commit = baseline(&root).unwrap();
        assert_eq!(commit.len(), 40, "{commit}");
        assert_eq!(head(&root), Some(commit.clone()));
        // A folder inside the project is not the top of it.
        assert_eq!(baseline(&root.join("src")), None);
        // The run's change and a new file of its: no baseline any more, but
        // the commit is still the one.
        std::fs::write(root.join("src/lib.ts"), "export const a = 2;").unwrap();
        std::fs::write(root.join("src/new.ts"), "export const b = 3;").unwrap();
        assert_eq!(baseline(&root), None);
        assert_eq!(head(&root), Some(commit));

        let base = base(&root).unwrap();
        assert_eq!(
            std::fs::read_to_string(base.join("src/lib.ts")).unwrap(),
            "export const a = 1;"
        );
        assert!(!base.join("src/new.ts").exists());
        assert!(!base.join(".git").exists());
        // The package store is linked, as it is; a build's output is not,
        // nor a store whose folder is not there at HEAD.
        assert!(
            std::fs::symlink_metadata(base.join("node_modules"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(
            std::fs::read_to_string(base.join("node_modules/pkg/index.js")).unwrap(),
            "a package"
        );
        assert!(!base.join("dist").exists());
        assert!(!base.join("sub").exists());
        discard(&base);
        assert!(!base.exists());
        // The project is as the run left it.
        assert_eq!(
            std::fs::read_to_string(root.join("src/lib.ts")).unwrap(),
            "export const a = 2;"
        );
        assert!(root.join("src/new.ts").exists());
        assert!(root.join("node_modules/pkg/index.js").exists());
        assert!(root.join("dist/lib.js").exists());
    }

    #[cfg(unix)]
    #[test]
    fn adopting_the_second_attempt_makes_the_project_hold_its_files_and_keeps_the_first() {
        use std::os::unix::fs::PermissionsExt;
        let root = Scratch::new("spec-tests-adopt");
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("bin")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
        std::fs::write(root.join(".gitignore"), "node_modules/\n*.log\n").unwrap();
        std::fs::write(root.join("src/lib.ts"), "export const a = 1;").unwrap();
        std::fs::write(root.join("src/old.ts"), "export const old = true;").unwrap();
        std::fs::write(root.join("README"), "a project").unwrap();
        std::fs::write(root.join("bin/run.sh"), "echo run").unwrap();
        std::fs::set_permissions(root.join("bin/run.sh"), PermissionsExt::from_mode(0o644))
            .unwrap();
        std::os::unix::fs::symlink("src/lib.ts", root.join("lib-link")).unwrap();
        std::fs::write(root.join("node_modules/pkg/index.js"), "a package").unwrap();
        committed(&root, "first");
        let commit = head(&root).unwrap();
        // The first attempt: a change, a deletion, a new file, an ignored
        // file, and the tester's test it put back and never deleted.
        std::fs::write(root.join("src/lib.ts"), "export const a = 2; // first").unwrap();
        std::fs::remove_file(root.join("README")).unwrap();
        std::fs::write(root.join("src/first.ts"), "first attempt").unwrap();
        std::fs::write(root.join("debug.log"), "noise").unwrap();
        std::fs::write(root.join("src/letronna_spec.test.ts"), "test").unwrap();
        // The second attempt, from the project as it was: another change,
        // another deletion, a new file, a mode made executable, and an
        // ignored file of its own.
        let from = base(&root).unwrap();
        std::fs::write(from.join("src/lib.ts"), "export const a = 3; // second").unwrap();
        std::fs::remove_file(from.join("src/old.ts")).unwrap();
        std::fs::write(from.join("src/second.ts"), "second attempt").unwrap();
        std::fs::set_permissions(from.join("bin/run.sh"), PermissionsExt::from_mode(0o755))
            .unwrap();
        std::fs::write(from.join("notes.log"), "ignored").unwrap();

        let excluded = ["src/letronna_spec.test.ts".to_owned()];
        let adopted = adopt(&root, &from, &excluded).unwrap();
        assert_eq!(
            adopted.written,
            ["README", "bin/run.sh", "src/lib.ts", "src/second.ts"]
        );
        assert_eq!(adopted.removed, ["src/first.ts", "src/old.ts"]);
        assert!(adopted.failed.is_empty());
        // The project holds the second attempt.
        let read = |path: &str| std::fs::read_to_string(root.join(path)).unwrap();
        assert_eq!(read("src/lib.ts"), "export const a = 3; // second");
        assert_eq!(read("README"), "a project");
        assert_eq!(read("src/second.ts"), "second attempt");
        assert!(!root.join("src/first.ts").exists());
        assert!(!root.join("src/old.ts").exists());
        let mode = std::fs::metadata(root.join("bin/run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "{mode:o}");
        // What git ignores, the tester's file, the link and the history
        // are as they were.
        assert_eq!(read("node_modules/pkg/index.js"), "a package");
        assert_eq!(read("debug.log"), "noise");
        assert!(!root.join("notes.log").exists());
        assert_eq!(read("src/letronna_spec.test.ts"), "test");
        assert!(
            std::fs::symlink_metadata(root.join("lib-link"))
                .unwrap()
                .is_symlink()
        );
        assert_eq!(head(&root), Some(commit));
        // The first attempt is kept: its changes to the committed files as
        // a patch, and the file it added at its path.
        assert!(adopted.kept.starts_with(std::env::temp_dir()));
        let patch = std::fs::read_to_string(adopted.kept.join(FIRST_PATCH)).unwrap();
        assert!(patch.contains("-export const a = 1;"), "{patch}");
        assert!(patch.contains("+export const a = 2; // first"), "{patch}");
        assert!(patch.contains("README"), "{patch}");
        assert_eq!(
            std::fs::read_to_string(adopted.kept.join("src/first.ts")).unwrap(),
            "first attempt"
        );
        discard(&from);
        discard(&adopted.kept);

        // An attempt that holds what the project holds is nothing to adopt.
        let same = copy(&root).unwrap();
        assert!(adopt(&root, &same, &[]).is_err());
        assert_eq!(read("src/lib.ts"), "export const a = 3; // second");
        discard(&same);
    }

    #[cfg(unix)]
    #[test]
    fn nothing_is_adopted_through_a_link_or_under_git() {
        let root = Scratch::new("spec-tests-adopt-links");
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::write(root.join("README"), "a project").unwrap();
        committed(&root, "first");
        let from = base(&root).unwrap();
        // The attempt made `.git` of its own, and a folder of files, which
        // the project has as a link to somewhere else.
        std::fs::create_dir_all(from.join(".git")).unwrap();
        std::fs::write(from.join(".git/config"), "theirs").unwrap();
        std::fs::create_dir_all(from.join("out")).unwrap();
        std::fs::write(from.join("out/made.txt"), "made").unwrap();
        let elsewhere = Scratch::new("spec-tests-adopt-elsewhere");
        std::os::unix::fs::symlink(&*elsewhere, root.join("out")).unwrap();

        let adopted = adopt(&root, &from, &[]);
        assert!(
            adopted
                .as_ref()
                .is_err_and(|why| why.contains("`out/made.txt`")),
            "{adopted:?}"
        );
        assert!(!elsewhere.join("made.txt").exists());
        let config = std::fs::read_to_string(root.join(".git/config")).unwrap();
        assert!(!config.contains("theirs"), "{config}");
        assert_eq!(inside(&root, "../escape"), None);
        assert_eq!(inside(&root, "/etc/passwd"), None);
        assert_eq!(inside(&root, ".git/HEAD"), None);
        assert_eq!(inside(&root, "vendor/dep/.git/config"), None);
        assert_eq!(inside(&root, "src/a.rs"), Some(root.join("src/a.rs")));
        assert_eq!(
            inside(&root, ".github/ci.yml"),
            Some(root.join(".github/ci.yml"))
        );
        discard(&from);
    }

    #[cfg(unix)]
    #[test]
    fn adoption_leaves_export_ignored_tracked_files_alone() {
        let root = Scratch::new("spec-tests-adopt-export-ignore");
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::create_dir_all(root.join("tests")).unwrap();
        std::fs::write(root.join("tests/test_a.py"), "def test_a(): pass\n").unwrap();
        std::fs::write(root.join(".gitattributes"), "tests/ export-ignore\n").unwrap();
        std::fs::write(root.join("src.py"), "x = 1\n").unwrap();
        committed(&root, "first");
        std::fs::write(root.join("src.py"), "x = 2  # first\n").unwrap();
        let from = base(&root).unwrap();
        assert!(from.join("tests/test_a.py").exists());
        std::fs::write(from.join("src.py"), "x = 3  # second\n").unwrap();

        let adopted = adopt(&root, &from, &[]).unwrap();
        assert_eq!(adopted.written, ["src.py"]);
        assert!(adopted.removed.is_empty(), "{:?}", adopted.removed);
        assert_eq!(
            std::fs::read_to_string(root.join("tests/test_a.py")).unwrap(),
            "def test_a(): pass\n"
        );
        discard(&from);
        discard(&adopted.kept);
    }

    #[cfg(unix)]
    #[test]
    fn a_path_made_a_folder_gives_way_and_a_folder_where_a_file_was_is_left() {
        let root = Scratch::new("spec-tests-adopt-folders");
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::write(root.join("plain"), "a file").unwrap();
        std::fs::write(root.join("gone"), "another").unwrap();
        committed(&root, "first");
        // The first attempt made a folder where `gone` was, with a file in it.
        std::fs::remove_file(root.join("gone")).unwrap();
        std::fs::create_dir_all(root.join("gone")).unwrap();
        std::fs::write(root.join("gone/inner"), "the first attempt's").unwrap();
        // The second made a folder of `plain`, and deleted `gone`.
        let from = base(&root).unwrap();
        std::fs::remove_file(from.join("plain")).unwrap();
        std::fs::create_dir_all(from.join("plain")).unwrap();
        std::fs::write(from.join("plain/inner.txt"), "inside").unwrap();
        std::fs::remove_file(from.join("gone")).unwrap();

        let adopted = adopt(&root, &from, &[]).unwrap();
        assert_eq!(adopted.written, ["plain/inner.txt"]);
        assert_eq!(adopted.removed, ["gone/inner", "plain"]);
        assert!(adopted.failed.is_empty(), "{:?}", adopted.failed);
        assert_eq!(
            std::fs::read_to_string(root.join("plain/inner.txt")).unwrap(),
            "inside"
        );
        // The folder where `gone` was is left, its file gone with the rest
        // of the first attempt's.
        assert!(root.join("gone").is_dir());
        assert!(!root.join("gone/inner").exists());
        discard(&from);
        discard(&adopted.kept);
    }

    #[cfg(unix)]
    #[test]
    fn an_attempt_whose_tests_would_import_the_first_attempts_code_is_told() {
        let root = Scratch::new("spec-tests-imports");
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::write(root.join(".gitignore"), ".venv/\nnode_modules/\n").unwrap();
        std::fs::write(root.join("README"), "a project").unwrap();
        committed(&root, "first");
        // Nothing linked, nothing elsewhere.
        let from = base(&root).unwrap();
        assert_eq!(imports_elsewhere(&root, &from), None);
        discard(&from);
        // A `.pth` for somewhere else is fine; an editable install of the
        // project is not.
        let packages = root.join(".venv/lib/python3.12/site-packages");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(packages.join("other.pth"), "/somewhere/else\n").unwrap();
        let from = base(&root).unwrap();
        assert_eq!(imports_elsewhere(&root, &from), None);
        std::fs::write(
            packages.join("__editable__.proj-1.0.pth"),
            format!(
                "import __editable___proj_finder  # {}/src\n",
                root.display()
            ),
        )
        .unwrap();
        let why = imports_elsewhere(&root, &from).unwrap();
        assert!(why.contains("__editable__.proj-1.0.pth"), "{why}");
        discard(&from);
        std::fs::remove_dir_all(root.join(".venv")).unwrap();
        // A package linked within the store is fine; one leading into the
        // project is not.
        std::fs::create_dir_all(root.join("node_modules/plain")).unwrap();
        std::fs::write(root.join("node_modules/plain/index.js"), "a package").unwrap();
        std::fs::create_dir_all(root.join("node_modules/.pnpm/dep")).unwrap();
        std::os::unix::fs::symlink("./.pnpm/dep", root.join("node_modules/dep")).unwrap();
        let from = base(&root).unwrap();
        assert_eq!(imports_elsewhere(&root, &from), None);
        std::fs::create_dir_all(root.join("packages/mine")).unwrap();
        std::fs::create_dir_all(root.join("node_modules/@scope")).unwrap();
        std::os::unix::fs::symlink(
            root.join("packages/mine"),
            root.join("node_modules/@scope/mine"),
        )
        .unwrap();
        let why = imports_elsewhere(&root, &from).unwrap();
        assert!(why.contains("node_modules/@scope/mine"), "{why}");
        discard(&from);
    }

    #[test]
    fn a_pass_that_ran_no_test_chooses_nothing() {
        assert!(passed(&Ok("3 passed in 0.10s\n".into())));
        assert!(!passed(&Ok(
            "collected 0 items\n\nno tests ran in 0.01s\n".into()
        )));
        assert!(!passed(&Ok(
            "No test files found, exiting with code 1\n".into()
        )));
        assert!(!passed(&Ok("Tests: 0 total\n".into())));
        assert!(!passed(&Ok("1 failed, 2 passed\n[exit code 1]".into())));
        assert!(!passed(&Err("Timed out after 1s".into())));
    }

    #[test]
    fn commands_that_install_or_link_are_known() {
        assert!(installs("npm link ../pkg"));
        assert!(installs("pip install -e ."));
        assert!(installs("cd src && PIP INSTALL ."));
        assert!(installs("cargo install --path ."));
        assert!(!installs("pytest -q tests"));
        assert!(!installs("npm test"));
        assert!(!installs("cargo build"));
    }

    #[test]
    fn the_run_is_told_when_the_attempt_installed_something_and_its_folder_stays() {
        let commands = ["pip install -e .".to_owned(), "npm link".to_owned()];
        let text = installed(Path::new("/tmp/base"), &commands);
        assert!(text.contains("at the request in `/tmp/base`"), "{text}");
        assert!(
            text.contains("ran `pip install -e .`, `npm link` there"),
            "{text}"
        );
        assert!(text.contains("the folder is left where it is"), "{text}");
        assert!(
            text.contains("run your own install of the project again in the project folder"),
            "{text}"
        );
        assert!(
            text.ends_with("end with your whole answer again."),
            "{text}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_kept_tests_are_run_in_the_attempts_own_folder_and_taken_away_after() {
        let root = Scratch::new("spec-tests-outcome-in");
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::write(root.join("README"), "a project").unwrap();
        committed(&root, "first");
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            "test -f made.txt || { echo 'made.txt is missing'; exit 1; }\n",
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(90, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        let from = base(&root).unwrap();
        // Before the attempt's work is there, the tests fail, as `failure`
        // says, and the folder is left without them.
        let failing = outcome_in(&kept, &command, &from, &ctx, TIMEOUT).unwrap();
        let (ended, output) = failure(&failing).unwrap();
        assert_eq!(ended, "exited with code 1");
        assert!(output.contains("made.txt is missing"), "{output}");
        assert!(!from.join("letronna_spec_test.sh").exists());
        // With it, they pass.
        std::fs::write(from.join("made.txt"), "made").unwrap();
        let passing = outcome_in(&kept, &command, &from, &ctx, TIMEOUT).unwrap();
        assert_eq!(failure(&passing), None);
        assert!(!from.join("letronna_spec_test.sh").exists());
        // The project itself, which still lacks it, fails them in a copy.
        let own = outcome(&kept, &command, &ctx, TIMEOUT).unwrap();
        assert!(failure(&own).is_some());
        assert!(!root.join("letronna_spec_test.sh").exists());
        assert!(!root.join("made.txt").exists());
        // One that did not finish is a failure too.
        let slow = Err("Timed out after 1s".to_owned());
        assert_eq!(
            failure(&slow),
            Some(("did not finish".to_owned(), "Timed out after 1s"))
        );
        discard(&from);
        tidy(90);
    }

    #[test]
    fn the_second_attempts_brief_gives_the_request_and_the_failing_output_and_nothing_else() {
        let brief = attempt(
            "  Make pairs work.\nPairs compose with Or.  ",
            "npx vitest run tests",
            "exited with code 1",
            "FAIL pairs compose\nexpected [a, b] got []",
        );
        assert!(brief.starts_with("Make the change the request below asks for, from scratch"));
        assert!(
            brief
                .ends_with("The request, word for word:\nMake pairs work.\nPairs compose with Or."),
            "{brief}"
        );
        assert!(
            brief.contains("`npx vitest run tests` exited with code 1"),
            "{brief}"
        );
        assert!(brief.contains("expected [a, b] got []"), "{brief}");
        assert!(
            brief.contains("not yours to look for or build on"),
            "{brief}"
        );
        assert!(
            brief.contains("your work is taken only if they pass"),
            "{brief}"
        );
        assert!(
            brief.contains("Install, link or re-install nothing"),
            "{brief}"
        );
        assert!(
            brief.contains("touch no file outside your folder"),
            "{brief}"
        );
        assert!(
            brief.contains(&format!("whose name holds `{MARKER}`")),
            "{brief}"
        );
        // Nothing of where the tests are kept or what they are called,
        // which it must build to the request rather than to.
        assert!(!brief.contains("letronna_spec.test.ts"), "{brief}");
        assert!(!brief.contains("/tmp/kept"), "{brief}");
    }

    #[test]
    fn the_run_is_told_the_project_holds_the_second_attempt_and_where_its_own_is_kept() {
        let taken = Adopted {
            written: vec!["src/lib.ts".to_owned(), "src/second.ts".to_owned()],
            removed: vec!["src/first.ts".to_owned()],
            failed: Vec::new(),
            kept: PathBuf::from("/tmp/first"),
        };
        let text = adopted("sh letronna_spec_test.sh", "exited with code 1", &taken);
        assert!(
            text.contains("`sh letronna_spec_test.sh` exited with code 1"),
            "{text}"
        );
        assert!(
            text.contains("now holds the second attempt in place of yours: 2 written, `src/lib.ts`, `src/second.ts`; 1 of yours removed, `src/first.ts`."),
            "{text}"
        );
        assert!(
            text.contains(&format!(
                "kept until this run ends in `/tmp/first`: `{FIRST_PATCH}`"
            )),
            "{text}"
        );
        assert!(text.contains("Do not put it back or merge it in"), "{text}");
        assert!(!text.contains("could not be written"), "{text}");
        // Only the answer is asked for: nothing checks a change after this.
        assert!(
            text.ends_with(
                "change nothing, and end with your whole answer again, describing the work as \
                 the project holds it now."
            ),
            "{text}"
        );
        // With only removals, and with a file that could not be written,
        // which is reported and nothing more.
        let partial = Adopted {
            written: Vec::new(),
            removed: vec!["a".to_owned()],
            failed: vec!["b".to_owned()],
            kept: PathBuf::from("/tmp/first"),
        };
        let text = adopted("make test", "did not finish", &partial);
        assert!(
            text.contains("in place of yours: 1 of yours removed, `a`."),
            "{text}"
        );
        assert!(
            text.contains("1 could not be written or removed here: `b`. Read"),
            "{text}"
        );
        assert!(!text.contains("put them right"), "{text}");
    }

    #[cfg(unix)]
    #[test]
    fn the_first_complete_version_is_kept_once_and_goes_as_the_run_ends() {
        let root = Scratch::new("spec-tests-first");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join("README"), "a project").unwrap();
        repo(&["add", "."]);
        snapshot(21, &root);
        let dir = snapshot_of(21).unwrap();
        assert!(
            dir.to_string_lossy().contains("-spec-copy-"),
            "{}",
            dir.display()
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("README")).unwrap(),
            "a project"
        );
        // Kept once, as the work stood then, and handed out until the tests
        // have run in it.
        std::fs::write(root.join("README"), "changed").unwrap();
        snapshot(21, &root);
        assert_eq!(snapshot_of(21), Some(dir.clone()));
        assert_eq!(
            std::fs::read_to_string(dir.join("README")).unwrap(),
            "a project"
        );
        let handed = first(21).unwrap();
        assert_eq!(handed.dir, dir);
        assert_eq!(first(21).map(|first| first.dir), Some(dir.clone()));
        handed.judged();
        assert_eq!(first(21), None);
        assert!(dir.exists());
        tidy(21);
        assert!(!dir.exists());
        assert_eq!(snapshot_of(21), None);
        // Where no copy can be made, none is kept.
        let plain = Scratch::new("spec-tests-first-plain");
        snapshot(22, &plain);
        assert_eq!(snapshot_of(22), None);
        assert_eq!(first(22), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_first_version_that_passes_the_tests_the_work_fails_names_what_changed_since() {
        let root = Scratch::new("spec-tests-referee");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join("README"), "a project").unwrap();
        std::fs::write(root.join("made.txt"), "made").unwrap();
        repo(&["add", "."]);
        // The first complete version makes `made.txt`, which the tester's
        // test checks for.
        snapshot(23, &root);
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            "test -f made.txt || { echo 'made.txt is missing'; exit 1; }\n",
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(23, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });

        // While the work passes, the first version is not asked for.
        let asked = std::cell::Cell::new(false);
        let peek = || {
            asked.set(true);
            first(23)
        };
        assert_eq!(
            ran(&kept, &command, &ctx, TIMEOUT, peek),
            Ran::Passed(command.clone())
        );
        assert!(!asked.get());

        // A review has the run drop `made.txt`, reword the README and add a
        // file: the run hears which version passed and what differs.
        std::fs::remove_file(root.join("made.txt")).unwrap();
        std::fs::write(root.join("README"), "a project, reviewed").unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/new.txt"), "new").unwrap();
        let Ran::Failed(text) = ran(&kept, &command, &ctx, TIMEOUT, || first(23)) else {
            panic!("the work passed");
        };
        assert!(
            text.starts_with(
                "Your first complete version, the work as it stood when you first said you \
                 were done, passed `sh letronna_spec_test.sh`; what you ended with fails it."
            ),
            "{text}"
        );
        assert!(
            text.contains("your work as it stands exited with code 1"),
            "{text}"
        );
        assert!(text.contains("made.txt is missing"), "{text}");
        assert!(
            text.contains("as it stands: `README`, `made.txt`, `src/new.txt`. Put back"),
            "{text}"
        );
        assert!(
            text.ends_with(
                "unless the test expects what the request's words do not say: then keep your \
                 change, and quote the words in your answer or say the request is silent on it. \
                 Then end with your whole answer again."
            ),
            "{text}"
        );
        // Nothing in the project changed: not the work, and no test was
        // left in it, nor in the first version.
        assert!(!root.join("made.txt").exists());
        assert!(!root.join("letronna_spec_test.sh").exists());
        assert_eq!(
            std::fs::read_to_string(root.join("README")).unwrap(),
            "a project, reviewed"
        );
        let dir = snapshot_of(23).unwrap();
        assert!(!dir.join("letronna_spec_test.sh").exists());
        // The first version is judged by once: the plain notice after.
        let Ran::Failed(text) = ran(&kept, &command, &ctx, TIMEOUT, || first(23)) else {
            panic!("the work passed");
        };
        assert!(
            text.starts_with("Your work as it stands was run against"),
            "{text}"
        );
        tidy(23);
        assert_eq!(to_run(23), None);
        assert_eq!(snapshot_of(23), None);
    }

    #[cfg(unix)]
    #[test]
    fn a_first_version_that_fails_the_tests_too_leaves_the_notice_as_it_was() {
        let root = Scratch::new("spec-tests-referee-both");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join("README"), "a project").unwrap();
        repo(&["add", "."]);
        // Kept without `made.txt`, so the first version fails the test as
        // the work does.
        snapshot(24, &root);
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            "test -f made.txt || { echo 'made.txt is missing'; exit 1; }\n",
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(24, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        // With the work as the first version is, the same files fail alike:
        // the tests do not run there, and it is not judged by.
        let Ran::Failed(text) = ran(&kept, &command, &ctx, TIMEOUT, || first(24)) else {
            panic!("the work passed");
        };
        assert!(
            text.starts_with("Your work as it stands was run against"),
            "{text}"
        );
        assert!(first(24).is_some());
        // Changed since, the first version runs them, fails them too, and
        // the notice is the plain one; judged by all the same.
        std::fs::write(root.join("README"), "a project, reviewed").unwrap();
        let Ran::Failed(text) = ran(&kept, &command, &ctx, TIMEOUT, || first(24)) else {
            panic!("the work passed");
        };
        assert!(
            text.starts_with("Your work as it stands was run against"),
            "{text}"
        );
        assert!(!text.contains("first complete version"), "{text}");
        assert!(text.contains("made.txt is missing"), "{text}");
        assert_eq!(first(24), None);
        tidy(24);
    }

    #[cfg(unix)]
    #[test]
    fn the_first_version_is_not_run_where_a_store_in_it_leads_back_into_the_project() {
        let root = Scratch::new("spec-tests-referee-editable");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join(".gitignore"), ".venv/\n").unwrap();
        std::fs::write(root.join("README"), "a project").unwrap();
        std::fs::write(root.join("made.txt"), "made").unwrap();
        repo(&["add", "."]);
        // The project is installed as editable from its own folder, and the
        // store is linked into every copy: a command of another language
        // than Python run in the first version would test the work as it
        // stands, so the two could only fail alike.
        let packages = root.join(".venv/lib/python3.12/site-packages");
        std::fs::create_dir_all(&packages).unwrap();
        std::fs::write(
            packages.join("__editable__.proj-1.0.pth"),
            format!("{}/src\n", root.display()),
        )
        .unwrap();
        snapshot(26, &root);
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            "test -f made.txt || { echo 'made.txt is missing'; exit 1; }\n",
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(26, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        std::fs::remove_file(root.join("made.txt")).unwrap();
        // The plain notice, and the first version kept rather than spent.
        let Ran::Failed(text) = ran(&kept, &command, &ctx, TIMEOUT, || first(26)) else {
            panic!("the work passed");
        };
        assert!(
            text.starts_with("Your work as it stands was run against"),
            "{text}"
        );
        assert!(first(26).is_some());
        tidy(26);
    }

    #[cfg(unix)]
    #[test]
    fn the_first_version_is_not_run_when_the_works_own_run_left_too_little_time() {
        let root = Scratch::new("spec-tests-referee-late");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join("README"), "a project").unwrap();
        std::fs::write(root.join("made.txt"), "made").unwrap();
        repo(&["add", "."]);
        snapshot(25, &root);
        std::fs::remove_file(root.join("made.txt")).unwrap();
        // The test takes two seconds to fail.
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            "sleep 2\ntest -f made.txt || { echo 'made.txt is missing'; exit 1; }\n",
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(25, &root, &root, Some(command.clone())).unwrap();
        // Before the work's own run there is just the time for a run of the
        // first version within the limit and half a review's for the fix;
        // after it there is not.
        let fix = crate::requirements::REVIEW_TIME / 2;
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            deadline: Some(Instant::now() + fix + TIMEOUT + Duration::from_secs(1)),
            ..Default::default()
        });
        let asked = std::cell::Cell::new(false);
        let peek = || {
            asked.set(true);
            first(25)
        };
        let Ran::Failed(text) = ran(&kept, &command, &ctx, TIMEOUT, peek) else {
            panic!("the work passed");
        };
        assert!(
            text.starts_with("Your work as it stands was run against"),
            "{text}"
        );
        // Not asked for, so still kept for a round with the time.
        assert!(!asked.get());
        assert!(first(25).is_some());
        tidy(25);
    }

    #[cfg(unix)]
    #[test]
    fn what_a_test_run_leaves_behind_is_not_among_the_files_that_differ() {
        let root = Scratch::new("spec-tests-differing");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join(".github")).unwrap();
        std::fs::write(root.join("src/a.py"), "a").unwrap();
        std::fs::write(root.join(".github/w.yml"), "w").unwrap();
        repo(&["add", "."]);
        let first = copy(&root).unwrap();
        // The run changes a source file and a hidden one, its tests leave
        // caches, compiled Python, coverage and an egg-info behind, and the
        // tester's test is put back.
        std::fs::write(root.join("src/a.py"), "b").unwrap();
        std::fs::write(root.join(".github/w.yml"), "x").unwrap();
        for file in [
            "__pycache__/a.cpython-312.pyc",
            ".hypothesis/unicode_data/x",
            "pkg.egg-info/PKG-INFO",
            "htmlcov/index.html",
            ".pytest_cache/v/cache/nodeids",
            "coverage/lcov.info",
            "coverage.xml",
            ".coverage",
            ".coverage.host.1",
            "tests/test_letronna_spec.py",
        ] {
            let at = root.join(file);
            std::fs::create_dir_all(at.parent().unwrap()).unwrap();
            std::fs::write(at, "left").unwrap();
        }
        assert_eq!(differing(&first, &root), ["src/a.py", ".github/w.yml"]);
        discard(&first);
    }

    #[test]
    fn the_files_that_differ_are_named_up_to_a_cap() {
        let files: Vec<String> = (0..DIFFERING + 5)
            .map(|at| format!("src/f{at:02}.rs"))
            .collect();
        let text = regressed("sh t.sh", "exited with code 1", "boom", &files);
        assert!(text.contains("`src/f00.rs`, `src/f01.rs`"), "{text}");
        assert!(
            text.contains(&format!(
                "`src/f{:02}.rs`, and 5 more. Put back",
                DIFFERING - 1
            )),
            "{text}"
        );
        assert!(!text.contains(&format!("src/f{DIFFERING:02}.rs")), "{text}");
        assert!(text.contains("Its output ends:\n\nboom\n\n"), "{text}");
        let none = regressed("sh t.sh", "did not finish", "", &[]);
        assert!(
            none.contains("as it stands: none that a listing of the project shows. Put back"),
            "{none}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_set_up_of_the_runs_passing_command_is_read_off_its_line() {
        assert_eq!(
            environment(
                "GOMODCACHE=/root/go/pkg/mod GOCACHE=/tmp/gocache go test ./...",
                "go"
            ),
            ["GOMODCACHE=/root/go/pkg/mod", "GOCACHE=/tmp/gocache"]
        );
        // Exported or set on their own before the program, after a `cd`,
        // through `env`, and quoted.
        assert_eq!(
            environment(
                "export PYTHONPATH=src; CI=1; cd /app && env DJANGO_SETTINGS_MODULE=\"app.settings\" \
                 pytest -q 2>&1 | tail -20",
                "pytest"
            ),
            [
                "PYTHONPATH=src",
                "CI=1",
                "DJANGO_SETTINGS_MODULE=app.settings"
            ]
        );
        // Nothing after the first program, and nothing from a line that
        // sets none.
        assert_eq!(
            environment("go test ./... && FOO=1 make check", "go"),
            Vec::<String>::new()
        );
        assert_eq!(environment("pytest -q", "pytest"), Vec::<String>::new());
        // A secret, or a value with a word's end in it, is not lent.
        assert_eq!(
            environment(
                "API_KEY=x GITHUB_TOKEN=y CLIENT_SECRET=z DB_PASSWORD=w AUTH_HEADER=v C='a b' \
                 D=1 go test",
                "go"
            ),
            ["D=1"]
        );
        // Nor what says where programs and libraries are found, or how a
        // build goes.
        assert_eq!(
            environment(
                "PATH=/usr/local/go/bin HOME=/root LD_LIBRARY_PATH=/opt/lib GOFLAGS=-mod=mod \
                 CGO_ENABLED=0 NODE_ENV=test PYTHONHOME=/py GOMODCACHE=/m go test",
                "go"
            ),
            ["GOMODCACHE=/m"]
        );
        // Nor anything off a command running another program than the
        // tester's, however it is reached.
        assert_eq!(
            environment("GOMODCACHE=/m go build ./...", "pytest"),
            Vec::<String>::new()
        );
        assert_eq!(
            environment("cd /app && GOMODCACHE=/m go build ./...", "go"),
            ["GOMODCACHE=/m"]
        );
        // Nor anything off a command with a word made by running something,
        // or holding a variable this does not know.
        assert_eq!(
            environment("A=$(pwd) D=1 go test", "go"),
            Vec::<String>::new()
        );
        assert_eq!(
            environment("B=$SOME_PLACE/x D=1 go test", "go"),
            Vec::<String>::new()
        );
        // `LENT` at most.
        let many: Vec<String> = (0..LENT + 2).map(|at| format!("V{at}={at}")).collect();
        assert_eq!(
            environment(&format!("{} go test", many.join(" ")), "go"),
            many[..LENT]
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_program_a_command_runs_is_read_past_its_set_up() {
        assert_eq!(
            program("cd /app && export X=1; Y=2 go test ./...").as_deref(),
            Some("go")
        );
        assert_eq!(
            program("env CI=1 pytest -q | tail -5").as_deref(),
            Some("pytest")
        );
        assert_eq!(
            program("./run_tests.sh --fast").as_deref(),
            Some("run_tests.sh")
        );
        assert_eq!(program("export X=1"), None);
        assert_eq!(program(""), None);
    }
}
