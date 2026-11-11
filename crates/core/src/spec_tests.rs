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
//! back to run them, and what it put back is deleted as it ends: at the paths
//! they are kept from, and in a git project any other copy by the same name
//! that git neither tracks nor ignores, that was not there before and that
//! is there as the run ends.
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
//! where the failures the run's output names are all in tests the tester
//! did not write (`own_failing`), such as the project's own under a command
//! that runs its whole suite, which say nothing of the work; nor where the
//! attempt's tests would import the first attempt's code through an
//! editable install; and an attempt that installed anything from its
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
use std::collections::{BTreeMap, HashSet};
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

/// Files under `site-packages` and the folder of its files read at most
/// for a path into the project, which an editable install leaves there.
const EDITABLE_READ: u64 = 64 * 1024;

/// How deep under a linked store a `site-packages` folder is looked for.
const STORE_DEPTH: usize = 5;

/// Characters of the tests' output the run is shown at most: the lines that
/// name a failure (`excerpt`), then its end, where a runner sums up.
const SHOWN: usize = 3_000;

/// Of `SHOWN`, what the lines naming failures take at most, so the end of
/// the output always has a third.
const FAILURES_SHOWN: usize = SHOWN * 2 / 3;

/// Characters each failure's lines take at most, its own and those after
/// it: an assertion's expected and actual values, a diff of a few lines, or
/// the frame that says where.
const FAILURE_SHOWN: usize = 600;

/// Characters a line kept for a failure holds at most: a log line next to
/// one can quote a whole SQL statement.
const FAILURE_LINE: usize = 200;

/// What a line of a test runner's output starts with, past its indent, when
/// it is the runner's word that a test failed or why (`Named::Verdict`):
/// Go's `--- FAIL` and `panic:`; unittest's `FAIL:` and `ERROR: test`;
/// jest's and vitest's `FAIL ` before a file, where Go's summary has a tab;
/// pytest's `FAILED` and `E` lines; testify's `Error Trace:`; jest's `●`
/// and the marks a list of tests puts by one that failed; jest's `Expected`
/// and `Received`; cargo's `assertion`, `error: test failed` and `error:
/// could not compile`; rustc's `error[`; rspec's `Failure/Error:`;
/// PHPUnit's `Failed asserting that`; and a C or C++ `Assertion failed`.
/// An assertion's error is read off the first word (`named`). A bare
/// `error:` is not here: a Node app logging through winston prints one on
/// every negative test, thirty of which took a mocha run's whole room from
/// its one `AssertionError`.
const VERDICT_STARTS: [&str; 22] = [
    "--- FAIL",
    "panic:",
    "FAIL:",
    "ERROR: test",
    "FAIL ",
    "FAILED",
    "E   ",
    "Error Trace:",
    "●",
    "✕",
    "×",
    "✗",
    "✘",
    "Expected",
    "Received",
    "assertion",
    "error: test failed",
    "error: could not compile",
    "error[",
    "Failure/Error:",
    "Failed asserting that",
    "Assertion failed",
];

/// What a runner's verdict line holds anywhere: cargo's `panicked at`,
/// Maven's `<<< FAILURE!` and `<<< ERROR!`, and Go's `[build failed]`.
const VERDICT_HOLDS: [&str; 4] = [
    "panicked at",
    "<<< FAILURE!",
    "<<< ERROR!",
    " [build failed]",
];

/// Bytes of a test command's output the driver reads at most: all that a
/// run of it keeps (`tools::COMMAND_KEPT`), where a model's call keeps
/// `tools::MAX_OUTPUT`, so the failures in a long log's middle reach
/// `excerpt`. Past 4 MiB a pipe's middle is gone before anyone reads it.
const OUTPUT_READ: usize = crate::tools::COMMAND_KEPT;

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

/// Variables never lent, as no `LD_*` or `*FLAGS` is, but for `GOFLAGS`
/// set to the flags of `GOFLAGS`: they say where programs and libraries
/// are found, or how a build goes, which the run may have set for a build
/// against what its tests need. Nor is the import path, which the copy is
/// put first on for the run (`with_import_path`), since a lent one,
/// exported after that, would undo it.
const UNLENT: [&str; 7] = [
    "PATH",
    "HOME",
    "CGO_ENABLED",
    "NODE_ENV",
    "PYTHONHOME",
    "PYTHONPATH",
    "PYTHONSAFEPATH",
];

/// The `GOFLAGS` flags lent all the same: each says where Go takes modules
/// from, or that no version stamp is read, which a test run in a copy of
/// the project needs as the run's own did; any other Go flag, as any other
/// `*FLAGS` variable, says how a build goes.
const GOFLAGS: [&str; 4] = [
    "-mod=mod",
    "-mod=vendor",
    "-mod=readonly",
    "-buildvcs=false",
];

/// What a command's output holds when its program's set-up failed before
/// anything of its own ran: Go's own lines, and what Go, cargo, pip and
/// npm print when a module or package they lack cannot be fetched, as none
/// can be where the run has no network, pnpm's and npm's codes for a
/// registry that did not answer among them. A pipe into `tail` hides the
/// exit code such a command ends with, so its output is read instead.
const SET_UP_FAILED: [&str; 17] = [
    "[setup failed]",
    "module lookup disabled by GOPROXY=off",
    "inconsistent vendoring",
    "proxy.golang.org",
    "server misbehaving",
    "no such host",
    "network is unreachable",
    "resolve host",
    "failed to download",
    "Temporary failure in name resolution",
    "NewConnectionError",
    "No matching distribution found",
    "ENOTFOUND",
    "EAI_AGAIN",
    "ERR_PNPM_META_FETCH_FAIL",
    "ERR_PNPM_FETCH_",
    "ERR_SOCKET_TIMEOUT",
];

/// How a line ends when a Unix shell, `env` or `timeout` found no program
/// to run under the name before it: bash's words, dash's, and what each
/// says of a path that leads nowhere. A program can print the words too, so
/// the exit code such a shell ends with is asked for as well.
const NOT_FOUND: [&str; 3] = [
    ": command not found",
    ": not found",
    ": No such file or directory",
];
const NOT_FOUND_CODE: &str = "127";

/// zsh's words for the same, which come before the name.
const NOT_FOUND_ZSH: &str = "command not found: ";

/// What cmd and PowerShell print for the same, after the name, and end
/// with no code of their own for it.
const NOT_RECOGNIZED: &str = "is not recognized as";

/// What Python prints before the name of a module `-m` was given that is
/// not installed, as `python -m pytest` does without pytest.
const NO_MODULE: &str = "No module named ";

/// Programs that run another the command need not name, such as the script
/// a package gives for its tests, and the test runners such a script runs:
/// one of these missing under one of those is the command's own runner.
const WRAPPERS: [&str; 6] = ["npm", "pnpm", "yarn", "npx", "bun", "bunx"];
const RUNNERS: [&str; 14] = [
    "jest",
    "vitest",
    "mocha",
    "ava",
    "tap",
    "tsx",
    "ts-node",
    "playwright",
    "jasmine",
    "karma",
    "node",
    "deno",
    "tsc",
    "pytest",
];

/// What a line of a runner's output starts with, past its indent, once it
/// has got as far as its tests, beyond what `passing` and a verdict's line
/// (`named`) show: node's mark by a test that failed and TAP's, the counts
/// jest, vitest and cargo print, and Go's word that a test is starting.
const TESTS_RAN: [&str; 6] = [
    "✖",
    "not ok ",
    "Tests:",
    "Test Files",
    "test result:",
    "=== RUN",
];

/// Words a runner's line opens with before a count of its tests: cargo's
/// `running 3 tests`, unittest's `Ran 3 tests` and pytest's `collected 3
/// items`. Only with the number, since a script's own `running tests...`
/// says nothing ran.
const TESTS_COUNTED: [&str; 3] = ["running", "Ran", "collected"];

/// What a line holds when the host it could not reach is this computer,
/// which is the work's own server and no registry; `::1` is another
/// (`loopback`).
const LOOPBACK: [&str; 3] = ["127.0.0.1", "localhost", "0.0.0.0"];

/// Whether `output`, a command's, says its program's set-up failed before
/// anything of its own ran, by `SET_UP_FAILED`.
pub(crate) fn set_up_failed(output: &str) -> bool {
    SET_UP_FAILED.iter().any(|sign| output.contains(sign))
}

/// A tester's files kept for a run: its id, the project it puts them back
/// in, where they are kept, and the command the tester gave to run them
/// from the project's root, if it gave one.
struct Moved {
    run: u64,
    workspace: PathBuf,
    kept: Kept,
    command: Option<String>,
    /// The variables the tester's own command that ran them set, as
    /// `VAR=value` words, for the driver to export ahead of `command`
    /// (`lend`); none where it set none.
    set_up: Vec<String>,
    /// Written by a tester started with the run, in the project as it was,
    /// so they have never met the work.
    unmet: bool,
    /// The files whose names hold `MARKER` that git neither tracked nor
    /// ignored in the project as they were kept (`marked`), which no sweep
    /// deletes; None where git could not say, and then nothing is swept.
    there: Option<Vec<String>>,
}

/// The tester's files kept for each run so far, so what the run puts back
/// is deleted as it ends, and the folder they are kept in with them.
fn moved() -> &'static Mutex<Vec<Moved>> {
    static MOVED: OnceLock<Mutex<Vec<Moved>>> = OnceLock::new();
    MOVED.get_or_init(Default::default)
}

/// What a run that ended leaves to sweep from its project (`sweep`): the
/// names of its tester's files, the files so named that were there as they
/// were kept, and the copies found as it ended (`copies`), by their paths;
/// None where git could not say then, and the sweep lists them when it is
/// made rather than losing them.
struct Sweep {
    workspace: PathBuf,
    names: Vec<String>,
    there: Vec<String>,
    listed: Option<Vec<String>>,
}

/// The sweeps left for the last run at work in a project to make as it
/// ends, since another run there may be running copies of its own.
fn sweeps() -> &'static Mutex<Vec<Sweep>> {
    static SWEEPS: OnceLock<Mutex<Vec<Sweep>>> = OnceLock::new();
    SWEEPS.get_or_init(Default::default)
}

/// A tester at work in a project itself, as one is where no copy of the
/// project could be made: whose run, where, and the files there whose
/// names hold `MARKER` as it began (`left`), which are the project's own or
/// the user's, not the tester's, so `keep` leaves them. Its files there are
/// its own until it answers, so no run's end sweeps them meanwhile.
struct Writer {
    run: u64,
    workspace: PathBuf,
    before: Vec<String>,
}

/// The testers writing in a project itself, one entry for each.
fn writing() -> &'static Mutex<Vec<Writer>> {
    static WRITING: OnceLock<Mutex<Vec<Writer>>> = OnceLock::new();
    WRITING.get_or_init(Default::default)
}

/// A tester at work in a project itself, from `writing_in` until dropped.
pub(crate) struct Writing(u64);

/// Marks `workspace` as one the tester of run `run` writes in, until what
/// this gives is dropped, with the files there whose names hold `MARKER`
/// listed first, so `keep` takes only those the tester adds.
pub(crate) fn writing_in(run: u64, workspace: &Path) -> Writing {
    let before = left(workspace);
    if let Ok(mut writing) = writing().lock() {
        writing.push(Writer {
            run,
            workspace: workspace.to_path_buf(),
            before,
        });
    }
    Writing(run)
}

impl Drop for Writing {
    fn drop(&mut self) {
        if let Ok(mut writing) = writing().lock()
            && let Some(at) = writing.iter().position(|writer| writer.run == self.0)
        {
            writing.remove(at);
        }
    }
}

/// The files whose names hold `MARKER` that were in `workspace` as the
/// tester of run `run` began writing there; None when no tester of the run
/// writes there.
fn written_before(run: u64, workspace: &Path) -> Option<Vec<String>> {
    let writing = writing().lock().ok()?;
    (writing.iter())
        .find(|writer| writer.run == run && writer.workspace == workspace)
        .map(|writer| writer.before.clone())
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

/// A new folder of the temp folder for `what`, named for this process. Not
/// with `MARKER` or any spelling of it: a runner printing absolute paths
/// puts the folder a command runs in among a failure's lines, which are
/// read for the tester's files (`names_own`).
fn folder(what: &str) -> PathBuf {
    let stamp = (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH))
        .map_or(0, |since| since.as_nanos());
    std::env::temp_dir().join(format!(
        "{}-harness-{what}-{}-{stamp}",
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
            // A folder, with `/` after it, is how git lists a repository
            // inside the project that it does not track; joined as listed,
            // the path could not be linked.
            let file = file.trim_end_matches('/');
            let (from, at) = (workspace.join(file), to.join(file));
            // A file the run deleted, or a path listed twice.
            let Ok(kind) = std::fs::symlink_metadata(&from) else {
                continue;
            };
            if std::fs::symlink_metadata(&at).is_ok() {
                continue;
            }
            // A file left out could have the tests fail for the wrong
            // reason, so none is: the copy fails, and the log says on what.
            if let Err(error) = place(&from, &at, &kind.file_type()) {
                unplaced("copy", file, &error);
                return None;
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

/// Puts what is at `from` in the project, of `kind`, at `at` in a copy of
/// it, making the folders above: a link as it is, a folder as a link to it,
/// which a repository inside the project is, and a file by a copy.
#[cfg(unix)]
fn place(from: &Path, at: &Path, kind: &std::fs::FileType) -> std::io::Result<()> {
    if let Some(parent) = at.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if kind.is_symlink() {
        return std::os::unix::fs::symlink(std::fs::read_link(from)?, at);
    }
    if kind.is_dir() {
        return std::os::unix::fs::symlink(from, at);
    }
    std::fs::copy(from, at).map(drop)
}

/// Notes in the log that `step`, `copy` or `link`, could not place `path`,
/// a path from the project, for `error`: a run that lost the driver's run
/// of the tester's tests to a copy that could not be made otherwise left
/// nothing to say why.
fn unplaced(step: &'static str, path: &str, error: &std::io::Error) {
    crate::diag::event(
        "spec_copy_failed",
        &[
            ("step", step.into()),
            ("path", path.into()),
            (crate::diag::ERROR, error.to_string().into()),
        ],
    );
    #[cfg(test)]
    if let Ok(mut noted) = noted().lock() {
        noted.push((step, path.to_owned()));
    }
}

/// What `unplaced` has noted so far, each step and path, for the tests.
#[cfg(test)]
fn noted() -> &'static Mutex<Vec<(&'static str, String)>> {
    static NOTED: OnceLock<Mutex<Vec<(&'static str, String)>>> = OnceLock::new();
    NOTED.get_or_init(Default::default)
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
/// One whose path `into` holds already, as a path listed twice or a folder
/// the copy has, is left as it is. None when one could not be linked, which
/// the log says (`unplaced`).
#[cfg(unix)]
fn link<'a>(
    workspace: &Path,
    entries: impl IntoIterator<Item = &'a str>,
    into: &Path,
) -> Option<()> {
    for entry in entries {
        let entry = entry.trim_end_matches('/');
        let at = into.join(entry);
        if std::fs::symlink_metadata(&at).is_ok() {
            continue;
        }
        let linked = (|| {
            if let Some(parent) = at.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::os::unix::fs::symlink(workspace.join(entry), &at)
        })();
        if let Err(error) = linked {
            unplaced("link", entry, &error);
            return None;
        }
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
    // end, so what the cut keeps is what the driver needs. The command runs
    // the tester's tests alone: the driver reads any failure in its run as
    // the work's, and a test of the project's own can fail where the driver
    // runs it for reasons of its own, such as a variable the run's commands
    // set.
    text.push_str(&format!(
        "\n\nEnd your report with a table of one row per test, written {rows}. Then, on a \
         line of its own as the report's very last line, the one command that, from the \
         project's root, runs your tests and no others, with paths relative to it, written \
         `Run: <command>`, with any variable your own test commands needed, such as a module \
         cache path, as `VAR=value` words before the command: one that names your test files \
         or filters on a name all your tests share, \
         such as `go test -run LetronnaSpec ./pkg/`, never the project's whole suite, such as \
         `go test ./...`, since a test of the project's own that fails here says nothing of the \
         change; one that runs them once and exits rather than watching, installs nothing, and \
         needs no file beyond your test files, so any fixture or helper they need goes in a \
         file named with `{MARKER}` too. A long report is cut in its middle before it is read, \
         so what comes last is kept."
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
    let dialect = crate::command::Dialect::here();
    let assessed = crate::command::assess(line, dialect, None);
    (assessed.commands.iter())
        .filter_map(runs)
        .find(|program| !matches!(program.as_str(), "export" | "cd"))
}

/// The variables `line`, a command of the run's, sets for `program`, as
/// `VAR=value` words: those before the program on the last command of the
/// line that runs it, and those `export`ed or set on their own anywhere
/// before that command, past the `cd`s and the other programs the line
/// runs first, such as an `ls` or an `rm`, since the shell keeps them past
/// `;` and `&&`; not one set before another program, as `FOO=1 make` does,
/// which is gone once that program ends. The tester's command runs with
/// them, so a tester that lacked the run's set-up, such as a module cache,
/// gets it. None when `line` never runs `program`, as a build's set-up may
/// be against the tests, and empty when it runs it with none; only a value
/// the shell reads as written, none whose name marks it as a secret or as
/// `UNLENT`, and `LENT` at most.
pub(crate) fn environment(line: &str, program: &str) -> Option<Vec<String>> {
    let dialect = crate::command::Dialect::here();
    let assessed = crate::command::assess(line, dialect, None);
    // A word made by running something, or holding a variable this does
    // not know, is not what the shell read.
    let plain_words = |command: &crate::command::Command, words: &[String]| {
        let words = words.iter().filter(|word| plain(word)).cloned();
        command.spelled_out.then(|| words.collect::<Vec<_>>())
    };
    let mut exported = Vec::new();
    let mut found = None;
    for command in &assessed.commands {
        if sets(command) {
            exported.extend(plain_words(command, &command.argv).unwrap_or_default());
        } else if runs(command).as_deref() == Some(program) {
            let own = plain_words(command, &command.argv[..command.at]).unwrap_or_default();
            found = Some([exported.clone(), own].concat());
        }
    }
    found.map(|mut found| {
        found.truncate(LENT);
        found
    })
}

/// The program `command`, one of a line's, runs, as the tables name it;
/// None for a lone assignment, which runs none.
fn runs(command: &crate::command::Command) -> Option<String> {
    (command.argv.get(command.at)).map(|word| crate::command::name_of(word))
}

/// Whether `command` only sets variables, as an `export` or a lone
/// assignment does, which the shell keeps for the commands after it.
fn sets(command: &crate::command::Command) -> bool {
    matches!(runs(command).as_deref(), None | Some("export"))
}

/// Programs whose first word is a subcommand, as `go test` and `cargo
/// build` are: a line of one that set nothing says the program needs no
/// set-up only for the same subcommand, since `go version` and `go env`
/// run without the module cache `go test` cannot.
const SUBCOMMANDS: [&str; 18] = [
    "go", "cargo", "npm", "pnpm", "yarn", "bun", "deno", "dotnet", "mvn", "gradle", "gradlew",
    "mix", "stack", "swift", "uv", "poetry", "pip", "make",
];

/// Words that only ask a program about itself, so a line of them ran no
/// test and says nothing of the set-up a test needs.
const PROBES: [&str; 4] = ["--version", "-V", "--help", "-h"];

/// Whether `line`, a command of the run's that runs `program` with no
/// set-up, ran what `command`, the tester's, runs, so that it says the
/// program needs none: the same subcommand, for a program that takes one
/// (`SUBCOMMANDS`), as `go test` is to `go test` and not to `go version`;
/// anything but a probe of the program (`PROBES`), for one that does not,
/// as `pytest -q tests/` is and `pytest --version` is not. False when
/// either does not run `program`.
pub(crate) fn same_run(line: &str, command: &str, program: &str) -> bool {
    let (Some(ran), Some(wanted)) = (arguments(line, program), arguments(command, program)) else {
        return false;
    };
    if SUBCOMMANDS.contains(&program) {
        return subcommand(&ran).is_some() && subcommand(&ran) == subcommand(&wanted);
    }
    !ran.iter().any(|word| PROBES.contains(&word.as_str()))
}

/// The words after `program` on the last command of `line` that runs it;
/// None when none does.
fn arguments(line: &str, program: &str) -> Option<Vec<String>> {
    let dialect = crate::command::Dialect::here();
    let assessed = crate::command::assess(line, dialect, None);
    (assessed.commands.iter().rev())
        .find(|command| runs(command).as_deref() == Some(program))
        .map(|command| command.argv[command.at + 1..].to_vec())
}

/// The subcommand among `arguments`, a program's: the first that is not a
/// flag.
fn subcommand(arguments: &[String]) -> Option<&str> {
    (arguments.iter())
        .find(|word| !word.starts_with('-'))
        .map(String::as_str)
}

/// Whether `word` sets a variable to a value with nothing in it for the
/// shell to expand or to end the word at, and no credentials in an address,
/// under a name that marks it neither as a secret nor as one never lent.
fn plain(word: &str) -> bool {
    let Some((name, value)) = word.split_once('=') else {
        return false;
    };
    let upper = name.to_ascii_uppercase();
    // `https://user:token@host` carries a secret under any name.
    let credentials = (value.split_once("://")).is_some_and(|(_, address)| address.contains('@'));
    crate::command::assignment(word)
        && !SECRETS.iter().any(|secret| upper.contains(secret))
        && !UNLENT.contains(&upper.as_str())
        && !upper.starts_with("LD_")
        && (!upper.ends_with("FLAGS") || go_flags(&upper, value))
        && !credentials
        && (value.chars()).all(|c| c.is_ascii_alphanumeric() || "_-./:,+@%=".contains(c))
}

/// Whether `name`, in capitals, is `GOFLAGS` set to flags of `GOFLAGS` and
/// no other.
fn go_flags(name: &str, value: &str) -> bool {
    name == "GOFLAGS"
        && !value.is_empty()
        && (value.split_whitespace()).all(|flag| GOFLAGS.contains(&flag))
}

/// The tester's files kept for run `run` and the command that runs them,
/// for the driver to run them against the work; None when none were kept,
/// or the tester gave no command.
pub(crate) fn to_run(run: u64) -> Option<(Kept, String)> {
    let moved = moved().lock().ok()?;
    let kept = first_kept(&moved, run)?;
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
    /// The command did not get as far as a test, which says nothing of the
    /// work: the command, and why, read off its output (`unrunnable`).
    Broken(String),
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
    // Nor does a command that never reached a test: sent back with it as a
    // failure, a run looked for a fault in its work that was not there. A
    // fetch that fails on the work alone is the work's doing, a dependency
    // it added or a lockfile it broke, and stays a failure: but only once
    // it has failed there twice, since the first may have met a registry
    // that was out of reach for a moment, and what the second makes of the
    // tests is then what came of them.
    let mut outcome = outcome;
    let mut broken = missing(command, &outcome);
    if broken.is_none()
        && let Some(why) = unfetched(&outcome)
    {
        match reaches_tests_before(kept, command, workspace, ctx, limit) {
            true => outcome = self::outcome(kept, command, ctx, limit).unwrap_or(outcome),
            false => broken = Some(why),
        }
    }
    if let Some(why) = broken {
        return Ran::Broken(format!(
            "`{command}` did not get as far as a test, since {why}"
        ));
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

/// Why `outcome`, what the test command `command` made of some work, shows
/// that the command did not get as far as a test, so that it says nothing
/// of the work: its program is not installed (`missing`), or a package or a
/// host it needed could not be fetched (`unfetched`). None when it passed,
/// did not finish, or failed any other way.
pub(crate) fn unrunnable(command: &str, outcome: &Result<String, String>) -> Option<String> {
    missing(command, outcome).or_else(|| unfetched(outcome))
}

/// The lines of `outcome`, a test command's that ran to a failing end, and
/// how it ended, where none of them is a runner's word on a test: a verdict
/// (`named`), a pass (`passing`), one of `TESTS_RAN` or a count after one
/// of `TESTS_COUNTED`. None otherwise: a test that failed after logging
/// what a broken command prints, or a suite that ran before a later step of
/// the command went missing, is a failure like any other.
fn before_any_test(outcome: &Result<String, String>) -> Option<(String, Vec<&str>)> {
    let output = outcome.as_ref().ok()?;
    let (ended, _) = failure(outcome)?;
    let lines = lines_of(output);
    let reached = |line: &&str| {
        let trimmed = line.trim_start();
        let mut words = trimmed.split_whitespace();
        let counted = (words.next()).is_some_and(|word| TESTS_COUNTED.contains(&word))
            && (words.next()).is_some_and(|word| word.parse::<u64>().is_ok());
        counted || passing(line) || TESTS_RAN.iter().any(|start| trimmed.starts_with(start))
    };
    if named_lines(&lines).contains(&Some(Named::Verdict)) || lines.iter().any(reached) {
        return None;
    }
    Some((ended, lines))
}

/// Why `outcome`, what `command` made of some work, shows that a program
/// the command runs is not installed here, with the line that says so;
/// None where it shows no such thing, or any test ran (`before_any_test`).
/// Only for a program the command itself names (`runs_program`): a tester's
/// script that calls the tool the work was to add fails the same way, and
/// that is the work's failure.
fn missing(command: &str, outcome: &Result<String, String>) -> Option<String> {
    let (ended, lines) = before_any_test(outcome)?;
    let no_such = ended.ends_with(&format!(" {NOT_FOUND_CODE}"));
    lines.iter().find_map(|line| {
        let name = missing_program(line, no_such).filter(|name| runs_program(command, name))?;
        Some(format!(
            "its program `{name}` is not installed here (`{}`)",
            clipped(line.trim())
        ))
    })
}

/// Why `outcome`, a test command's, shows that a package or a host it
/// needed could not be fetched (`set_up_failed`), with the line that says
/// so; None where it shows no such thing, or any test ran
/// (`before_any_test`). A line about this computer (`LOOPBACK`) is passed
/// over: that is the work's own server not answering.
fn unfetched(outcome: &Result<String, String>) -> Option<String> {
    let (_, lines) = before_any_test(outcome)?;
    let line = (lines.iter()).find(|line| set_up_failed(line) && !loopback(line))?;
    Some(format!(
        "a package or a host it needs could not be reached from here (`{}`)",
        clipped(line.trim())
    ))
}

/// Whether `line` names this computer as a host: by one of `LOOPBACK`, or
/// by `::1` standing as a whole address, as in `[::1]:3000` and `::1:3000`,
/// not as the end of another, as in `2001:db8::1`.
fn loopback(line: &str) -> bool {
    let part = |c: char| c.is_ascii_hexdigit() || c == ':';
    let whole = |(at, found): (usize, &str)| {
        !line[..at].ends_with(part)
            && !(line[at + found.len()..].chars().next()).is_some_and(|c| c.is_ascii_hexdigit())
    };
    LOOPBACK.iter().any(|host| line.contains(host)) || line.match_indices("::1").any(whole)
}

/// The name of the program `line`, of a command's output, says was not
/// found: before one of `NOT_FOUND` or after zsh's words where the command
/// ended as a shell does then (`no_such`), before cmd's and PowerShell's
/// words, between the quotes of pnpm's and yarn's `Command "x" not found`,
/// and after Python's `NO_MODULE` where one name ends the line. Without its
/// folder and the quotes around it.
fn missing_program(line: &str, no_such: bool) -> Option<String> {
    let line = line.trim();
    let first = |text: &str| text.split_whitespace().next().map(program_name);
    let last = |text: &str| text.split_whitespace().next_back().map(program_name);
    if let Some((_, name)) = line.split_once(NOT_FOUND_ZSH) {
        return no_such.then(|| first(name)).flatten();
    }
    if let Some(head) = NOT_FOUND.iter().find_map(|end| line.strip_suffix(end)) {
        return no_such.then(|| last(head)).flatten();
    }
    if let Some((head, _)) = line.split_once(NOT_RECOGNIZED) {
        return last(head);
    }
    if let Some((_, rest)) = line.split_once("Command \"")
        && let Some((name, after)) = rest.split_once('"')
        && after.trim_start().starts_with("not found")
    {
        return Some(program_name(name));
    }
    let (_, name) = line.split_once(NO_MODULE)?;
    (name.split_whitespace().count() == 1).then(|| program_name(name))
}

/// `word` as a program's name: without the quotes or the punctuation a
/// message puts around it, and without its folder.
fn program_name(word: &str) -> String {
    let marks = |c: char| c.is_whitespace() || "'\"`‘’.,:".contains(c);
    let word = word.trim_matches(marks);
    word.rsplit(['/', '\\']).next().unwrap_or(word).to_owned()
}

/// Whether `command` runs the program `name`: as one of its words, bare or
/// by a path from the root, or, where it runs one of `WRAPPERS`, as one of
/// `RUNNERS`. A word that is a path inside the project is not counted, such
/// as a script or a tool the work builds: it is missing only if the work
/// did not make it.
fn runs_program(command: &str, name: &str) -> bool {
    let breaks = |c: char| c.is_whitespace() || matches!(c, ';' | '|' | '&' | '(' | ')' | '=');
    let quotes = |c: char| matches!(c, '\'' | '"');
    let words: Vec<String> = (command.split(breaks))
        .map(|word| word.trim_matches(quotes))
        .filter(|word| !word.contains('/') || word.starts_with('/'))
        .map(program_name)
        .collect();
    let wrapped = || words.iter().any(|word| WRAPPERS.contains(&word.as_str()));
    words.iter().any(|word| word == name) || (RUNNERS.contains(&name) && wrapped())
}

/// Whether `command` gets as far as a test on the project in `workspace`
/// as it was, with the tester's tests `kept` put there, within `limit`:
/// asked where it could not fetch what it needed on the work, to tell a
/// registry out of reach from a dependency the work added. False where the
/// project as it was cannot be written out, where it is the work, the run
/// having committed all of it, where the time to the deadline `ctx` carries
/// does not hold two runs and half a review's for a fix after them, and
/// where the command could not be run there either.
fn reaches_tests_before(
    kept: &Kept,
    command: &str,
    workspace: &Path,
    ctx: &ToolContext,
    limit: Duration,
) -> bool {
    let fix = crate::requirements::REVIEW_TIME / 2;
    if time_left(ctx).is_some_and(|left| left < fix + limit * 2) {
        return false;
    }
    // Nothing changed since the last commit: the two would fail alike.
    if baseline(workspace).is_some() {
        return false;
    }
    let Some(base) = base(workspace) else {
        return false;
    };
    let before = outcome_in(kept, command, &base, ctx, limit);
    discard(&base);
    before.is_some_and(|before| before.is_ok() && unrunnable(command, &before).is_none())
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
/// passed, or why they could not be run and are its own to run. Where the
/// command itself could not run here, also at a later done (`Ran::Broken`),
/// so it names the tests itself.
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
        Ran::Broken(why) => format!(
            "The tests a tester wrote from the request alone could not be run against your \
             work as it stands, which says nothing of the work: {why}. Run them yourself the \
             way you found works, fix your work for each that fails on what the request's \
             words say, and delete the copies before you end."
        ),
    }
}

/// Whether `outcome`, what the tester's tests made of some work, is a pass
/// that chose something: the command ended well and its output does not
/// say that no test ran (`ran_nothing`), which a bare project or a wrong
/// path can bring.
pub(crate) fn passed(outcome: &Result<String, String>) -> bool {
    failure(outcome).is_none() && !ran_nothing(outcome.as_deref().unwrap_or_default())
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
/// cut a call made for no conversation, so `limit` has to fit in it. Up to
/// `OUTPUT_READ` bytes of its output are kept, not a model's cut of it, so
/// the failures in a long log's middle are there for `excerpt` to find.
fn run_command(
    command: &str,
    copy: &Path,
    ctx: &ToolContext,
    limit: Duration,
) -> Result<String, String> {
    let mut ctx = ctx.clone();
    ctx.workspace = Some(copy.to_path_buf());
    ctx.conversation = 0;
    ctx.caller.keeps = Some(OUTPUT_READ);
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
/// editable finder to sys.meta_path, so the copy's code wins; a PYTHONPATH
/// of the run's own is not lent (`UNLENT`), since exported after this one
/// it would take its place. As it is for any other command.
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
        // whole output went, of how long it ran and the time left, or of
        // what the user's hooks said, can follow one.
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

/// What every notice of a failing test tells the run before it changes what
/// its work does for the test. On a benchmark smoke run a tester expected a
/// cause sanitized that the request's `classFilter` kept out, and the run,
/// sent back with that failure near its deadline, made its work sanitize
/// every cause: a review's row had passed the behaviour the words set, and
/// the hidden test failed on the change. The words for the exact case come
/// first, and a narrower rule in them wins over a test that ignores it.
const WORDS_FIRST: &str = "Before changing what your work does for a test, find the request's \
                           words for that exact case: where they set a narrower rule the test \
                           ignores, such as a filter, a condition or an exception, keep what \
                           your work does (where a review's row passed it, that is a sign you \
                           read the words right) and answer the test by quoting the words.";

/// `WORDS_FIRST`, then what to do where the words are silent on the case,
/// `silent`, which each notice says for itself.
fn words_first(silent: &str) -> String {
    format!("{WORDS_FIRST} Where the words are silent on the case, {silent}.")
}

/// What the report of a tester started ahead of the work tells the run in
/// place of `WORDS_FIRST`: its tests are read before the work exists, so
/// the rule is about what to build, not what to keep.
const WORDS_FIRST_AHEAD: &str = "Before making your work do what a test expects, find the \
                                 request's words for that exact case: where they set a narrower \
                                 rule the test ignores, such as a filter, a condition or an \
                                 exception, follow the words and answer the test by quoting \
                                 them; where they are silent on the case, say so in your answer.";

/// The notice sending the run back with `outcome`, what `command` made of
/// the tester's tests `kept` run against its work; None when they passed.
/// Where the failures it attributes appear to be all in tests the tester
/// did not write (`own_failing`), it says so: they may be the project's own
/// as it was, or this environment's, and the run is to check before it
/// changes anything for them.
fn failed(kept: &Kept, command: &str, outcome: &Result<String, String>) -> Option<String> {
    let (ended, output) = failure(outcome)?;
    let (shown, left_out) = quoted(output);
    let others = match own_failing(kept, command, output) {
        Some(false) => {
            "The failures shown appear to be in tests the tester did not write, and may be \
             the project's own as it was or this environment's: check whether they fail on \
             the project as it was too, and say so where they do. "
        }
        _ => "",
    };
    let words = words_first("say so in your answer");
    Some(format!(
        "Your work as it stands was run against the tests a tester wrote from the request \
         alone, in a copy of the project with them put back at the paths they are kept from, \
         so nothing in the project was touched: `{command}` {ended}. Its \
         output{left_out}:\n\n{shown}\n\n{others}Fix your work for each test that fails on what the \
         request's words say, and run the tests again the same way. {words} One that \
         calls the code wrongly, such as with a name or import the request does not spell, or that \
         fails for want of a fixture, helper or set-up the tester left out, is the tester's to \
         fix where it is kept, in `{}`, since that is where it is run from; never something to \
         add to the project. If the command itself cannot run here, run the tests with the \
         set-up you found works. Then end with your whole answer again.",
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
    let (shown, left_out) = quoted(output);
    let words = words_first("put back what broke the test and say so");
    format!(
        "Your first complete version, the work as it stood when you first said you were done, \
         passed `{command}`; what you ended with fails it. Both were run against the tests a \
         tester wrote from the request alone, each in a copy of the project with them put back \
         at the paths they are kept from, so nothing in the project was touched: your work as \
         it stands {ended}. Its output{left_out}:\n\n{shown}\n\nThe files that \
         differ between that first version and your work as it stands: {changed}. Put back \
         what broke the test, unless the request's words side with your change. {words} Then \
         end with your whole answer again."
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
    // A repository inside the project, which git lists as a folder with
    // `/` after it: its files are its own, and the first version holds a
    // link to it where the project holds the folder.
    let current = current.into_iter().filter(|path| !path.ends_with('/'));
    let work = |path: &String| {
        let name = path.rsplit('/').next().unwrap_or(path);
        !name.contains(MARKER)
            && !(path.split('/')).any(|part| SKIPPED.contains(&part) || artefact(part))
    };
    let mut paths: Vec<String> = (current.chain(files_under(first))).filter(work).collect();
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
/// spaces between them squeezed to one, each where a word starts, so `0
/// tests run` is not read in `10 tests run`.
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
/// one stands means nothing ran, read as a number: `10 passed` is a run, and
/// so are a crate's tests beside its doc-tests' `0 passed`.
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
    let starts_word =
        |at: usize| !(squeezed[..at].chars().next_back()).is_some_and(char::is_alphanumeric);
    let said = |words: &str| squeezed.match_indices(words).any(|(at, _)| starts_word(at));
    if NOTHING_RAN.iter().any(|words| said(words)) {
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
    let (shown, left_out) = quoted(output);
    format!(
        "`{command}` passes on the project as it was and fails with your change. Its \
         output{left_out}:\n\n{shown}\n\nRun it once more first. If it still fails, \
         then where the request's words change what that test expects, update the test's \
         expectation and quote the words in your answer; otherwise fix the code so it passes \
         again. Do not delete, skip or weaken the test. Then end with your whole answer again."
    )
}

/// How a line of a test runner's output names a failure (`named`).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Named {
    /// The runner's own word that a test failed, or why.
    Verdict,
    /// An exception's name or a trace, which a test that passes can log
    /// too, as a negative test or a page's script does.
    Raised,
}

/// Lines jest heads what a test logged with: what follows, indented deeper
/// or blank, is the test's own output and names no failure.
const CONSOLE: [&str; 6] = [
    "console.log",
    "console.error",
    "console.warn",
    "console.info",
    "console.debug",
    "console.trace",
];

/// What a notice quotes of `output`, a test command's, as `excerpt` shows
/// it, and the words saying that `…` stands for lines left out, where any
/// were.
fn quoted(output: &str) -> (String, &'static str) {
    let shown = excerpt(output);
    let note = match shown == output.trim_end() {
        true => "",
        false => ", with `…` for lines left out",
    };
    (shown, note)
}

/// What the run is shown of `output`, a test command's: all of it when it
/// fits in `SHOWN` characters; otherwise, in their order, the lines naming
/// a failure (`named`), each with the lines after it up to `FAILURE_SHOWN`
/// characters or `another_part` of the report, the runner's verdicts first
/// and what was raised only in what they leave of `FAILURES_SHOWN`, and a
/// failure's line shown once however often it repeats; then as many whole
/// lines of its end as the rest holds, with `…` for each run of lines left
/// out. A runner's summary at the end names the failures but not why, and
/// a log printed between the tests can push the why out of any tail. A bare
/// carriage return, as a progress bar prints, ends a line too.
fn excerpt(output: &str) -> String {
    let output = output.trim_end();
    if output.chars().count() <= SHOWN {
        return output.to_owned();
    }
    let lines = lines_of(output);
    let names = named_lines(&lines);
    let gap = "…\n".chars().count();
    let mut picked: BTreeMap<usize, String> = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut used = 0;
    for tier in [Named::Verdict, Named::Raised] {
        for at in (0..lines.len()).filter(|at| names[*at] == Some(tier)) {
            if picked.contains_key(&at) || seen.contains(lines[at].trim()) {
                continue;
            }
            let mut block: Vec<(usize, String)> = Vec::new();
            let mut size = 0;
            for (next, line) in lines.iter().enumerate().skip(at) {
                let shown = clipped(line);
                let cost = shown.chars().count() + 1;
                let over = size + cost > FAILURE_SHOWN || another_part(line);
                if next > at && (over || picked.contains_key(&next)) {
                    break;
                }
                size += cost;
                block.push((next, shown));
            }
            if used + size + gap > FAILURES_SHOWN {
                break;
            }
            used += size + gap;
            for (next, _) in &block {
                if names[*next].is_some() {
                    seen.insert(lines[*next].trim());
                }
            }
            picked.extend(block);
        }
    }
    // The end, in whole lines, one `…` before it; a line picked already
    // costs nothing again. A last line too long for what is left shows its
    // end, after a `…` of its own.
    let mut room = SHOWN.saturating_sub(used + gap);
    for at in (0..lines.len()).rev() {
        if picked.contains_key(&at) {
            continue;
        }
        let size = lines[at].chars().count() + 1;
        if size > room {
            if at + 1 == lines.len() && room > 1 {
                let end: String = lines[at].chars().skip(size - room + 1).collect();
                picked.insert(at, format!("…{end}"));
            }
            break;
        }
        room -= size;
        picked.insert(at, lines[at].to_owned());
    }
    let mut shown = String::new();
    let mut next = 0;
    for (at, line) in picked {
        if at != next {
            shown.push_str("…\n");
        }
        shown.push_str(&line);
        shown.push('\n');
        next = at + 1;
    }
    shown.pop();
    shown
}

/// The lines of `output`, a test command's: a bare carriage return, as a
/// progress bar prints, ends a line too.
fn lines_of(output: &str) -> Vec<&str> {
    (output.split('\n'))
        .flat_map(|line| line.strip_suffix('\r').unwrap_or(line).split('\r'))
        .collect()
}

/// What a line of a test runner's output starts with, past its indent,
/// when it is the runner's verdict on a test or file it names
/// (`attributed`): Go's `--- FAIL`; unittest's `FAIL:` and `ERROR: test`;
/// jest's and vitest's `FAIL ` before a file; pytest's `FAILED` before a
/// test's path; jest's `●` and the marks a list of tests puts by one that
/// failed; and rspec's `Failure/Error:`.
const ATTRIBUTED_STARTS: [&str; 11] = [
    "--- FAIL",
    "FAIL:",
    "ERROR: test",
    "FAIL ",
    "FAILED",
    "●",
    "✕",
    "×",
    "✗",
    "✘",
    "Failure/Error:",
];

/// Characters after a line naming a failure within which a file or test
/// the lines name is read as the failure's own: a Go panic's goroutine dump
/// runs to a couple of thousand before the frame in the test that reached
/// it, where `excerpt` shows far less of each failure.
const OWN_WINDOW: usize = 4_000;

/// Bytes read of each of the tester's Go files for its tests' names; a
/// test file runs to far less.
const TEST_FILE_READ: u64 = 256 * 1024;

/// Whether the failures `output`, what `command` made of the tester's
/// tests `kept`, names are the tester's own: Some(true) when `command` runs
/// its tests alone, naming its files or a filter on their names
/// (`names_own`), or when the lines of a failure, up to `OWN_WINDOW`
/// characters and ending where another part of the report or a passing
/// test's lines begin, name one of its files (`names_own`) or one of its
/// Go tests (`test_names`); Some(false) when the runner attributes a
/// failure to a test or file (`attributed`) and none of those is the
/// tester's; None when it attributes none, as when only a build error, a
/// panic, an assertion's values or a trace says that something failed,
/// when a runner this does not know prints, or when the command did not
/// finish. Under a command that runs the project's whole suite, a test of
/// the project's own can fail for reasons of its own, such as a variable
/// the run's commands set, and says nothing of the work.
pub(crate) fn own_failing(kept: &Kept, command: &str, output: &str) -> Option<bool> {
    if names_own(command) {
        return Some(true);
    }
    let tests = test_names(kept);
    let lines = lines_of(output);
    let names = named_lines(&lines);
    let owns: Vec<bool> = (lines.iter())
        .map(|line| names_own(line) || tests.iter().any(|test| names_test(line, test)))
        .collect();
    let mut attributed_any = false;
    for at in (0..lines.len()).filter(|at| names[*at].is_some()) {
        attributed_any |= attributed(lines[at]);
        let mut size = 0;
        for (next, line) in lines.iter().enumerate().skip(at) {
            if next > at && (size > OWN_WINDOW || another_part(line) || passing(line)) {
                break;
            }
            size += line.chars().count() + 1;
            if owns[next] {
                return Some(true);
            }
        }
    }
    attributed_any.then_some(false)
}

/// Whether every failure `output`, a run of the tester's command against
/// the work, attributes to a test or file (`attributed`) is attributed in
/// `bare` too, its run on the project as it was, each line read with its
/// timings left out (`untimed`), since Go prints a test's time: then the
/// failures are the project's own as it was, and the work caused none.
/// False where `output` attributes none.
pub(crate) fn already_failing(output: &str, bare: &str) -> bool {
    let attributed_in = |text: &str| -> HashSet<String> {
        let lines = lines_of(text);
        let names = named_lines(&lines);
        (0..lines.len())
            .filter(|at| names[*at].is_some() && attributed(lines[*at]))
            .map(|at| untimed(lines[at]))
            .collect()
    };
    let first = attributed_in(output);
    !first.is_empty() && first.is_subset(&attributed_in(bare))
}

/// `line` with its timings left out and its spaces made one: a test's time
/// differs from one run to the next, as Go's `(0.01s)`, jest's `(12 ms)`,
/// pytest's `in 0.42s` and a package line's trailing `0.4s` do, where a
/// subtest's or a case's number, `TestTable/case_1` or `test_x[1]`, tells
/// one test from another and stays.
fn untimed(line: &str) -> String {
    let words: Vec<&str> = line.split_whitespace().collect();
    let mut kept: Vec<&str> = Vec::with_capacity(words.len());
    let mut at = 0;
    while at < words.len() {
        let number = bare(words[at]);
        // A number and its unit as two words, `(12 ms)`.
        if !number.is_empty()
            && number.chars().all(|c| c.is_ascii_digit() || c == '.')
            && words
                .get(at + 1)
                .is_some_and(|unit| duration_unit(bare(unit)))
        {
            at += 2;
            continue;
        }
        if duration(number) {
            // With the `in` of a summary's `in 0.42s`.
            if kept.last() == Some(&"in") {
                kept.pop();
            }
            at += 1;
            continue;
        }
        kept.push(words[at]);
        at += 1;
    }
    kept.join(" ")
}

/// `word` without the brackets and comma a runner puts around a timing.
fn bare(word: &str) -> &str {
    word.trim_matches(|c| matches!(c, '(' | ')' | '[' | ']' | ','))
}

/// Whether `word` is a duration, digits with a unit after them: `0.01s`,
/// `12ms`.
fn duration(word: &str) -> bool {
    let unit = word.trim_start_matches(|c: char| c.is_ascii_digit() || c == '.');
    unit.len() < word.len() && duration_unit(unit)
}

/// Whether `unit` is a unit of time a runner prints a duration in.
fn duration_unit(unit: &str) -> bool {
    matches!(unit, "s" | "ms" | "µs" | "us" | "ns" | "m" | "h")
}

/// The names of the tests in the tester's Go files `kept`, `TestX` for each
/// `func TestX(`: Go's `--- FAIL:` names the test and not its file, and the
/// file named under it is a helper's where the test asserts through one
/// without `t.Helper()`. Other runners name the file.
fn test_names(kept: &Kept) -> Vec<String> {
    use std::io::Read;
    let mut names = Vec::new();
    for file in (kept.files.iter()).filter(|file| file.ends_with("_test.go")) {
        let Ok(opened) = std::fs::File::open(kept.dir.join(file)) else {
            continue;
        };
        let mut text = String::new();
        if opened
            .take(TEST_FILE_READ)
            .read_to_string(&mut text)
            .is_err()
        {
            continue;
        }
        for line in text.lines() {
            let Some(rest) = line.strip_prefix("func Test") else {
                continue;
            };
            let name: String = (rest.chars())
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if !name.is_empty() {
                names.push(format!("Test{name}"));
            }
        }
    }
    names
}

/// Whether `line` names the test `test` as a whole word, with no letter,
/// digit or underscore before or after it: `TestTotal` is found in `---
/// FAIL: TestTotal (0.00s)`, `TestTotal/sub` and `pkg.TestTotal(`, and not
/// in `TestTotalRounding`, which may be the project's own.
fn names_test(line: &str, test: &str) -> bool {
    let word = |c: char| c.is_ascii_alphanumeric() || c == '_';
    line.match_indices(test).any(|(at, found)| {
        let before = (line[..at].chars().next_back()).is_none_or(|c| !word(c));
        let after = (line[at + found.len()..].chars().next()).is_none_or(|c| !word(c));
        before && after
    })
}

/// Whether `line`, of a test runner's output, is the runner's verdict on a
/// test or file it names: it starts, past its indent, with one of
/// `ATTRIBUTED_STARTS` other than jest's `● Console`, holds Maven's `<<<
/// FAILURE!` or `<<< ERROR!`, ends in ` FAILED` as cargo's and Gradle's
/// lines for a test do, or is a counted or numbered failure (`titled`). A
/// build error, a panic, an assertion's values or a trace names a failure
/// too (`named`), but not whose.
fn attributed(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.starts_with("● Console") {
        return false;
    }
    ATTRIBUTED_STARTS
        .iter()
        .any(|start| trimmed.starts_with(start))
        || ["<<< FAILURE!", "<<< ERROR!"]
            .iter()
            .any(|held| line.contains(held))
        || trimmed.trim_end().ends_with(" FAILED")
        || titled(trimmed)
}

/// Whether `trimmed`, a line past its indent, opens with mocha's count of
/// what is failing, `2 failing`, or numbers a failure as PHPUnit and mocha
/// list them, `1) Cart`.
fn titled(trimmed: &str) -> bool {
    let mut words = trimmed.split_whitespace();
    let first = words.next().unwrap_or_default();
    let counted = first.parse::<u64>().is_ok() && words.next() == Some("failing");
    let numbered = (first.strip_suffix(')'))
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()));
    counted || numbered
}

/// Whether `line` is a runner's word on a test that passed or is starting,
/// past which a failure's lines have ended: Go's `--- PASS`, `=== RUN` and
/// `ok`, its `PASS` alone, jest's ticks, and pytest's `PASSED` and `XPASS`,
/// which its `-rA` summary lists after what failed.
fn passing(line: &str) -> bool {
    let trimmed = line.trim_start();
    let ended = trimmed.trim_end();
    [
        "--- PASS",
        "=== RUN",
        "=== PAUSE",
        "=== CONT",
        "PASSED",
        "XPASS",
        "ok ",
        "ok\t",
        "✓",
        "√",
        "✔",
    ]
    .iter()
    .any(|start| trimmed.starts_with(start))
        || ended == "PASS"
        || ended.ends_with(" PASSED")
}

/// Whether `text` names one of the tester's files or tests: holds `MARKER`
/// in any case, with or without its underscore, as `letronna_spec` in a
/// path does and `LetronnaSpec` in a test's name. The run's own folders
/// (`folder`) are named so as never to.
fn names_own(text: &str) -> bool {
    let flat: String = (text.chars().filter(|c| !matches!(c, '_' | '-'))).collect();
    flat.to_lowercase().contains(&MARKER.replace('_', ""))
}

/// Whether `line` starts another part of a runner's report, where a
/// failure's lines end: another file's, as jest's and vitest's `PASS ` and
/// `FAIL ` start one, a test's logs, as jest's `● Console` and its
/// `console.log` and the like do, or another binary's, as cargo's `test
/// result:` ends one and its `Running ` opens the next.
fn another_part(line: &str) -> bool {
    let trimmed = line.trim_start();
    ["PASS ", "FAIL ", "● Console", "Running ", "test result:"]
        .iter()
        .any(|start| trimmed.starts_with(start))
        || CONSOLE.contains(&trimmed.trim_end())
}

/// `line` cut to `FAILURE_LINE` characters, `…` marking the cut.
fn clipped(line: &str) -> String {
    match line.char_indices().nth(FAILURE_LINE) {
        Some((cut, _)) => format!("{}…", &line[..cut]),
        None => line.to_owned(),
    }
}

/// How each of `lines` names a failure, as `named` reads it, but none in a
/// block jest heads with one of `CONSOLE`: the lines after the heading that
/// are blank or indented deeper than it.
fn named_lines(lines: &[&str]) -> Vec<Option<Named>> {
    let mut console = None;
    let mut names = Vec::with_capacity(lines.len());
    for line in lines {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if console.is_some_and(|depth| trimmed.is_empty() || indent > depth) {
            names.push(None);
            continue;
        }
        console = CONSOLE.contains(&trimmed.trim_end()).then_some(indent);
        names.push(console.is_none().then(|| named(line)).flatten());
    }
    names
}

/// How `line`, of a test runner's output, names a failure. As the runner's
/// verdict when it starts, past its indent, with one of `VERDICT_STARTS`
/// other than jest's `● Console` heading a test's logs, holds one of
/// `VERDICT_HOLDS`, ends in ` FAILED` as cargo's and Gradle's lines for a
/// test do, gives a Go file's line and column as a build error does, holds
/// a TypeScript error's code, or opens with mocha's count of what is
/// failing or a numbered failure as PHPUnit and mocha list them (`titled`),
/// or an assertion's error. As raised when it opens with `Traceback`, with a bare
/// `error:`, as an app's logger prints on every line and rustc on a build
/// error without a code, or with another word ending in `Error` or
/// `Exception`. Never for a mere mention of an error, which a log prints on
/// every line, as `SQL logic error` or `record not found`.
fn named(line: &str) -> Option<Named> {
    let trimmed = line.trim_start();
    if trimmed.starts_with("● Console") {
        return None;
    }
    let first = trimmed.split_whitespace().next().unwrap_or_default();
    let name = first.trim_end_matches(':');
    let asserted = [
        "AssertionError",
        "AssertionFailedError",
        "ComparisonFailure",
    ]
    .iter()
    .any(|error| name.ends_with(error));
    let verdict = VERDICT_STARTS
        .iter()
        .any(|start| trimmed.starts_with(start))
        || VERDICT_HOLDS.iter().any(|held| line.contains(held))
        || trimmed.trim_end().ends_with(" FAILED")
        || go_position(trimmed)
        || typescript_code(line)
        || titled(trimmed)
        || asserted;
    if verdict {
        return Some(Named::Verdict);
    }
    let raised = name.ends_with("Error") || name.ends_with("Exception") || first == "error:";
    (raised || trimmed.starts_with("Traceback")).then_some(Named::Raised)
}

/// Whether `line` gives a Go file's line and column before a message, as
/// `go build` and `go vet` print an error: `x_test.go:12:9: undefined: F`.
/// A log's `x.go:406 record not found` gives no column.
fn go_position(line: &str) -> bool {
    let digits = |text: &str| (text.find(|c: char| !c.is_ascii_digit())).filter(|end| *end > 0);
    line.match_indices(".go:").any(|(at, found)| {
        let rest = &line[at + found.len()..];
        let Some(row) = digits(rest) else {
            return false;
        };
        let Some(rest) = rest[row..].strip_prefix(':') else {
            return false;
        };
        digits(rest).is_some_and(|column| rest[column..].starts_with(": "))
    })
}

/// Whether `line` holds a TypeScript compiler's error code, `error TS2304`.
fn typescript_code(line: &str) -> bool {
    (line.match_indices("error TS"))
        .any(|(at, found)| line[at + found.len()..].starts_with(|c: char| c.is_ascii_digit()))
}

/// `output`, a command's, without the note a long one's result ends with in
/// a run with a deadline, of how long it ran and how much of the run's time
/// is left: that time is the run's, and a helper it is quoted to has its own.
fn without_time_left(output: &str) -> String {
    let note = |line: &&str| line.starts_with("[It ran for ") && line.ends_with("time are left.]");
    match output.lines().any(|line| note(&line)) {
        true => (output.lines().filter(|line| !note(line)))
            .collect::<Vec<_>>()
            .join("\n")
            .trim_end()
            .to_owned(),
        false => output.to_owned(),
    }
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
    let (shown, left_out) = quoted(&without_time_left(output));
    let mut text = format!(
        "Make the change the request below asks for, from scratch, in the project in your \
         working folder, which holds the project as it was before anyone began on it. Another \
         agent's attempt at the same request did not pass the tests a tester wrote from the \
         request's words alone: `{command}` {ended}, and this is its \
         output{left_out}:\n\n{shown}\n\n\
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
         The request, word for word:\n"
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
/// all the same, so none stays where a patch would take it. In the project
/// itself, a file that was there as the tester began writing
/// (`writing_in`) is the project's own or the user's whatever its name, and
/// stays, with no word from git needed, as does one git tracks or ignores;
/// where no tester was marked as writing there and git cannot say, each
/// is taken.
pub(crate) fn keep(
    run: u64,
    from: &Path,
    workspace: &Path,
    command: Option<String>,
) -> Option<Kept> {
    let dir = folder("tests");
    // In the project itself, the files that were there as the tester began
    // are the project's own or the user's, whatever git makes of them; and
    // one git tracks or ignores is the project's whatever its name, which
    // tells them apart where no tester was marked as writing there.
    let in_project = from == workspace;
    let before = in_project.then(|| written_before(run, workspace)).flatten();
    let theirs = in_project
        .then(|| crate::git::untracked(from, &format!("*{MARKER}*")))
        .flatten();
    let files: Vec<String> = (left(from).into_iter())
        .filter(|file| before.as_ref().is_none_or(|before| !before.contains(file)))
        .filter(|file| theirs.as_ref().is_none_or(|theirs| theirs.contains(file)))
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
    let there = marked(workspace);
    if let Ok(mut moved) = moved().lock() {
        moved.push(Moved {
            run,
            workspace: workspace.to_path_buf(),
            kept: kept.clone(),
            command,
            set_up: Vec::new(),
            unmet: false,
            there,
        });
    }
    Some(kept)
}

/// Records `set_up`, the variables the tester's own command that ran its
/// tests set, with `kept`, its files as `keep` gave them, for the driver to
/// export ahead of their command (`lent`): the module cache the tester's
/// copy needed, the driver's copy needs too.
pub(crate) fn lend(kept: &Kept, set_up: Vec<String>) {
    if let Ok(mut moved) = moved().lock()
        && let Some(entry) = moved.iter_mut().find(|entry| entry.kept.dir == kept.dir)
    {
        entry.set_up = set_up;
    }
}

/// The variables recorded with the tester's files `to_run` gives for run
/// `run`, if any were.
pub(crate) fn lent(run: u64) -> Vec<String> {
    let Ok(moved) = moved().lock() else {
        return Vec::new();
    };
    (first_kept(&moved, run))
        .map(|kept| kept.set_up.clone())
        .unwrap_or_default()
}

/// The tester's files kept for run `run` that the driver runs, as `to_run`
/// and `lent` both read them: the first kept for it.
fn first_kept(moved: &[Moved], run: u64) -> Option<&Moved> {
    moved.iter().find(|kept| kept.run == run)
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
        && let Some(entry) = moved.iter_mut().find(|entry| entry.kept.dir == kept.dir)
    {
        entry.unmet = true;
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
/// `keep` kept them from, and other copies of them left there (`sweep`), and
/// the folder they were kept in, and forgets them: they are the tester's, by
/// their names, and no part of the task, and a run stopped at its deadline
/// never deletes them itself. A tester started
/// with the run and not taken is abandoned: stopped, its report dropped, and
/// its folder deleted, here and once more as it ends, since a stopped one
/// can still be writing there. The run's first complete version goes too,
/// and the folders kept aside for it (`aside`); not the folder a second
/// attempt installed from, since the project's packages may lead into it.
/// The paths it deleted from the project, in order.
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
    let mut ended = Vec::new();
    let mut due = Vec::new();
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
        ended.push(kept.workspace.clone());
        if let Some(there) = &kept.there {
            let names: Vec<String> = (kept.kept.files.iter())
                .map(|file| name(file).to_owned())
                .collect();
            due.push((kept.workspace.clone(), names, there.clone()));
        }
        false
    });
    let busy: Vec<PathBuf> = moved.iter().map(|kept| kept.workspace.clone()).collect();
    drop(moved);
    ended.sort();
    ended.dedup();
    // Git is asked once for each project the run ended in, for what is
    // listed now and what is swept now alike.
    let found: BTreeMap<PathBuf, Option<Vec<String>>> = (ended.iter())
        .map(|workspace| (workspace.clone(), marked(workspace)))
        .collect();
    // The copies to sweep are listed now, as the run ends, whether the
    // sweep is made now or left for the last run in the folder: a file made
    // there later, such as a copy the user keeps of a kept test, is not the
    // run's whatever its name. Where git could not say, the sweep lists
    // them when it is made.
    let due = due.into_iter().map(|(workspace, names, there)| Sweep {
        listed: (found.get(&workspace).and_then(|found| found.as_deref()))
            .map(|found| copies(found, &names, &there)),
        workspace,
        names,
        there,
    });
    if let Ok(mut sweeps) = sweeps().lock() {
        sweeps.extend(due);
    }
    for workspace in &ended {
        // Another run there, or a tester writing there, may be running
        // copies of its own, so the last to end sweeps for all of them.
        let written_in = (writing().lock())
            .is_ok_and(|writing| writing.iter().any(|writer| writer.workspace == *workspace));
        if !busy.contains(workspace) && !written_in {
            let listing = found.get(workspace).and_then(|found| found.as_deref());
            deleted.extend(sweep(workspace, listing));
        }
    }
    deleted.sort();
    deleted.dedup();
    deleted
}

/// The copies of a tester's files, `names`, that a run left in its project
/// among `found`, the files there that git neither tracks nor ignores
/// (`marked`), by their paths: each outside the folders `SKIPPED` names
/// whose name is one of `names` and which was not `there` when the run
/// kept them. A `cp -r` into the wrong folder leaves such copies, and a
/// patch of the project would take them. Git lists only what it finds
/// untracked, so a file it reads otherwise, in a repository inside the
/// project or by a name the disk spells another way, is left out.
fn copies(found: &[String], names: &[String], there: &[String]) -> Vec<String> {
    (found.iter())
        .filter(|path| !path.split('/').any(|part| SKIPPED.contains(&part)))
        .filter(|path| names.iter().any(|kept| kept == name(path)) && !there.contains(path))
        .cloned()
        .collect()
}

/// Deletes from `workspace` the copies of their tester's files that the runs
/// which ended there left, as their sweeps list them (`copies`), and gives
/// their paths: each that is still among `found`, the files there that git
/// neither tracks nor ignores (`marked`) now, so one the user has added
/// since stays; a sweep that could not list them as its run ended lists
/// them now. Nothing where git could not say, `found` None: the sweeps are
/// left for a later run's end. The folders the deletions leave empty go
/// too.
fn sweep(workspace: &Path, found: Option<&[String]>) -> Vec<String> {
    let Some(found) = found else {
        return Vec::new();
    };
    let due: Vec<Sweep> = match sweeps().lock() {
        Ok(mut sweeps) => {
            let (due, rest) = (std::mem::take(&mut *sweeps).into_iter())
                .partition(|sweep| sweep.workspace == workspace);
            *sweeps = rest;
            due
        }
        Err(_) => return Vec::new(),
    };
    if due.is_empty() {
        return Vec::new();
    }
    let mut listed: Vec<String> = (due.into_iter())
        .flat_map(|sweep| {
            let Sweep {
                names,
                there,
                listed,
                ..
            } = sweep;
            listed.unwrap_or_else(|| copies(found, &names, &there))
        })
        .collect();
    listed.sort();
    listed.dedup();
    let found: HashSet<&String> = found.iter().collect();
    let deleted: Vec<String> = (listed.into_iter())
        .filter(|path| found.contains(path))
        .filter(|path| std::fs::remove_file(workspace.join(path)).is_ok())
        .collect();
    for path in &deleted {
        let mut folder = Path::new(path).parent();
        while let Some(inside) = folder.filter(|inside| !inside.as_os_str().is_empty()) {
            if std::fs::remove_dir(workspace.join(inside)).is_err() {
                break;
            }
            folder = inside.parent();
        }
    }
    deleted
}

/// The files in `workspace`, the top of a git project, that git neither
/// tracks nor ignores and whose names hold `MARKER`, by their paths from
/// there; None anywhere else, or where git could not say.
fn marked(workspace: &Path) -> Option<Vec<String>> {
    if !crate::git::is_top(workspace) {
        return None;
    }
    let listed = crate::git::untracked(workspace, &format!("*{MARKER}*"))?;
    Some(
        listed
            .into_iter()
            .filter(|path| name(path).contains(MARKER))
            .collect(),
    )
}

/// The name of the file at `path`, a path with `/` between folders.
fn name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
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
/// `SKIPPED` names or in a repository inside `dir`, whose files are its
/// own, and none a link leads to.
fn files_under(dir: &Path) -> Vec<String> {
    let mut walk = ignore::WalkBuilder::new(dir);
    walk.hidden(false);
    walk.filter_entry(|entry| {
        let repository = entry.depth() > 0
            && entry.file_type().is_some_and(|kind| kind.is_dir())
            && entry.path().join(".git").exists();
        !repository && !SKIPPED.iter().any(|skipped| entry.file_name() == *skipped)
    });
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
    let words = words_first("say so in your answer");
    format!(
        "A tester that hasn't seen your work wrote tests from the request alone but could not \
         run them where it worked; its report follows. {} Run them with the project's own \
         runner and the set-up you found works, fix a test only where it calls the code wrongly, \
         such as a guessed name or import the request does not spell, and fix your work for \
         each test that fails on what the request's words say. {words} Then delete the \
         copies, since they are not part of the task.{}\n\n{}",
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
    let words = words_first("say so in your answer");
    // One its budget stopped reported what it had: not every test it meant
    // to write, nor each one run.
    let ran = match crate::agents::ran_out(report) {
        true => {
            "wrote tests from the request alone and ran out of budget before it was done; its \
             report follows, with a table of the tests it got to, which may leave out some it \
             never ran or wrote."
        }
        false => {
            "wrote tests from the request alone and ran them; its report follows, ending with \
             a table of its tests."
        }
    };
    tell(
        &format!(
            "A tester that hasn't seen your work {ran} Fix your work for each test that fails \
             on what the request's words say. {words}"
        ),
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
        &format!(
            "A tester that hasn't seen your work wrote tests from the request alone while you \
             worked, in the project as it was before your change, where none could pass yet; \
             its report follows, ending with a table of its tests, whose rows say NOT RUN or \
             FAIL for want of your change, not what your work does. Make your work do what each \
             test expects where the request's words say so. {WORDS_FIRST_AHEAD}"
        ),
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

    #[cfg(unix)]
    #[test]
    fn a_repository_inside_the_project_is_copied_as_a_link_to_it() {
        let root = Scratch::new("spec-tests-copy-nested");
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::write(root.join("README"), "a project").unwrap();
        committed(&root, "first");
        // Git lists a repository it does not track as a folder, `nested/`.
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        assert!(git(&nested, &["init", "-q"]).is_some());
        std::fs::write(nested.join("inner.txt"), "inner").unwrap();
        assert!(listed(&root).unwrap().contains(&"nested/".to_owned()));

        let copy = copy(&root).expect("the project was not copied");
        assert_eq!(std::fs::read_link(copy.join("nested")).unwrap(), nested);
        assert!(copy.join("nested/inner.txt").exists());
        assert!(copy.join("README").exists());
        // Nor is the repository among the files that differ between the
        // first version, which links to it, and the project.
        assert_eq!(differing(&copy, &root), Vec::<String>::new());
        std::fs::write(root.join("README"), "a project, changed").unwrap();
        assert_eq!(differing(&copy, &root), ["README"]);
        discard(&copy);
    }

    #[cfg(unix)]
    #[test]
    fn an_ignored_entry_whose_path_the_copy_holds_already_is_skipped() {
        let root = Scratch::new("spec-tests-copy-held");
        for folder in ["build", "dist"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            std::fs::write(root.join(folder).join("out.txt"), "built").unwrap();
        }
        let into = Scratch::new("spec-tests-copy-held-into");
        std::fs::create_dir_all(into.join("build")).unwrap();
        std::fs::write(into.join("build/own.txt"), "the copy's").unwrap();
        // Listed twice, and a path the copy holds as a folder of its own:
        // neither fails the copy, and the folder stays as it is.
        assert_eq!(link(&root, ["build/", "dist/", "dist/"], &into), Some(()));
        assert!(into.join("build/own.txt").exists());
        assert!(!into.join("build").is_symlink());
        assert_eq!(
            std::fs::read_link(into.join("dist")).unwrap(),
            root.join("dist")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_file_that_cannot_be_copied_fails_the_copy_and_is_named() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        let root = Scratch::new("spec-tests-copy-unreadable");
        // Root reads anything, so the file would copy.
        if std::fs::metadata(&root).unwrap().uid() == 0 {
            return;
        }
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::write(root.join("README"), "a project").unwrap();
        committed(&root, "first");
        std::fs::write(root.join("secret.txt"), "unreadable").unwrap();
        std::fs::set_permissions(
            root.join("secret.txt"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();

        assert_eq!(copy(&root), None);
        let noted = noted().lock().unwrap();
        assert!(
            noted
                .iter()
                .any(|(step, path)| *step == "copy" && path == "secret.txt"),
            "{noted:?}"
        );
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

    /// Writes `file` under `at`, with the folders above it, holding its path.
    fn written(at: &Path, file: &str) {
        let path = at.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, file).unwrap();
    }

    #[test]
    fn copies_of_a_kept_file_left_elsewhere_go_as_the_run_ends_and_nothing_else() {
        let root = Scratch::new("spec-tests-strays");
        let from = Scratch::new("spec-tests-strays-from");
        let kept_name = "drift_letronna_spec_test.go";
        assert!(git(&root, &["init", "-q"]).is_some());
        std::fs::write(root.join(".gitignore"), "build/\n").unwrap();
        // The project's own test, committed under the same name.
        written(&root, &format!("legacy/{kept_name}"));
        written(&root, "src/main.go");
        committed(&root, "first");
        // A copy the user made of an earlier run's test and kept.
        written(&root, &format!("tests/{kept_name}"));
        written(&from, &format!("backend/{kept_name}"));
        let kept = keep(140, &from, &root, None).unwrap();
        // The run copies the folder into itself by mistake, then puts the
        // test back where it is kept from.
        written(&root, &format!("backend/backend/{kept_name}"));
        written(&root, &format!("backend/{kept_name}"));
        // None of these is a copy to sweep: one git ignores, a package's,
        // another name, and a repository's inside the project.
        written(&root, &format!("build/{kept_name}"));
        written(&root, &format!("node_modules/pkg/{kept_name}"));
        written(&root, "notes/plan_letronna_spec.md");
        let inner = root.join("vendor/lib");
        std::fs::create_dir_all(&inner).unwrap();
        assert!(git(&inner, &["init", "-q"]).is_some());
        written(&inner, kept_name);
        committed(&inner, "inner");

        assert_eq!(
            tidy(140),
            [
                format!("backend/backend/{kept_name}"),
                format!("backend/{kept_name}"),
            ]
        );
        // The folders they emptied go with them.
        assert!(!root.join("backend").exists());
        for stays in [
            format!("legacy/{kept_name}"),
            "src/main.go".to_owned(),
            format!("tests/{kept_name}"),
            format!("build/{kept_name}"),
            format!("node_modules/pkg/{kept_name}"),
            "notes/plan_letronna_spec.md".to_owned(),
            format!("vendor/lib/{kept_name}"),
        ] {
            assert!(root.join(&stays).exists(), "{stays}");
        }
        assert!(!kept.dir.exists());
    }

    #[test]
    fn a_copy_made_after_a_run_ended_is_not_swept_for_it() {
        let root = Scratch::new("spec-tests-strays-later");
        let from = Scratch::new("spec-tests-strays-later-from");
        assert!(git(&root, &["init", "-q"]).is_some());
        written(&from, "tests/x_letronna_spec_test.go");
        keep(152, &from, &root, None).unwrap();
        written(&from, "tests/y_letronna_spec_test.go");
        keep(153, &from, &root, None).unwrap();
        // The first run leaves a copy in the wrong folder and ends while the
        // other is still at work there, so its sweep waits for the other's end.
        written(&root, "tests/tests/x_letronna_spec_test.go");
        assert!(tidy(152).is_empty());
        // The user then keeps a copy of the first run's test under its own
        // name: the other run's end sweeps what the first left there, and
        // nothing made there since.
        written(&root, "tests/x_letronna_spec_test.go");
        assert_eq!(tidy(153), ["tests/tests/x_letronna_spec_test.go"]);
        assert!(root.join("tests/x_letronna_spec_test.go").exists());
        assert!(!root.join("tests/tests").exists());
    }

    #[test]
    fn a_sweep_git_could_not_list_waits_for_a_later_runs_end() {
        let root = Scratch::new("spec-tests-strays-git-off");
        let from = Scratch::new("spec-tests-strays-git-off-from");
        assert!(git(&root, &["init", "-q"]).is_some());
        written(&from, "t/x_letronna_spec_test.go");
        keep(157, &from, &root, None).unwrap();
        written(&root, "t/t/x_letronna_spec_test.go");
        // Git cannot read the repository as the run ends: nothing is swept,
        // and nothing is lost.
        std::fs::rename(root.join(".git"), root.join("git-off")).unwrap();
        assert!(tidy(157).is_empty());
        assert!(root.join("t/t/x_letronna_spec_test.go").exists());
        std::fs::rename(root.join("git-off"), root.join(".git")).unwrap();
        // The next run to end there sweeps what the first left.
        written(&from, "u/y_letronna_spec_test.go");
        keep(158, &from, &root, None).unwrap();
        assert_eq!(tidy(158), ["t/t/x_letronna_spec_test.go"]);
    }

    #[test]
    fn a_tracked_file_renamed_by_case_alone_is_kept() {
        let root = Scratch::new("spec-tests-strays-case");
        // Only where the disk ignores case are the two spellings one file.
        std::fs::write(root.join("Probe"), "").unwrap();
        if !root.join("probe").exists() {
            return;
        }
        std::fs::remove_file(root.join("Probe")).unwrap();
        let from = Scratch::new("spec-tests-strays-case-from");
        assert!(git(&root, &["init", "-q"]).is_some());
        written(&root, "Backend/drift_letronna_spec_test.go");
        committed(&root, "first");
        written(&from, "other/drift_letronna_spec_test.go");
        keep(148, &from, &root, None).unwrap();
        std::fs::rename(root.join("Backend"), root.join("moving")).unwrap();
        std::fs::rename(root.join("moving"), root.join("backend")).unwrap();

        assert!(tidy(148).is_empty());
        assert!(root.join("backend/drift_letronna_spec_test.go").exists());
    }

    #[test]
    fn no_copy_is_swept_while_a_tester_writes_in_the_project() {
        let root = Scratch::new("spec-tests-strays-writing");
        let from = Scratch::new("spec-tests-strays-writing-from");
        assert!(git(&root, &["init", "-q"]).is_some());
        written(&from, "t/x_letronna_spec_test.go");
        keep(149, &from, &root, None).unwrap();
        written(&root, "t/t/x_letronna_spec_test.go");
        let writing = writing_in(150, &root);
        assert!(tidy(149).is_empty());
        assert!(root.join("t/t/x_letronna_spec_test.go").exists());
        drop(writing);
        // The next run to end there sweeps what the first left.
        written(&from, "u/y_letronna_spec_test.go");
        keep(150, &from, &root, None).unwrap();
        assert_eq!(tidy(150), ["t/t/x_letronna_spec_test.go"]);
    }

    #[test]
    fn a_tester_in_the_project_itself_leaves_the_files_that_were_there() {
        let root = Scratch::new("spec-tests-keep-tracked");
        assert!(git(&root, &["init", "-q"]).is_some());
        written(&root, "tests/old_letronna_spec_test.go");
        committed(&root, "first");
        // The user's own copy of an earlier run's test, which git does not
        // track either.
        written(&root, "tests/mine_letronna_spec_test.go");
        let writing = writing_in(151, &root);
        written(&root, "tests/new_letronna_spec_test.go");

        let kept = keep(151, &root, &root, None).unwrap();
        assert_eq!(kept.files, ["tests/new_letronna_spec_test.go"]);
        assert!(!root.join("tests/new_letronna_spec_test.go").exists());
        drop(writing);
        assert_eq!(tidy(151), Vec::<String>::new());
        assert!(root.join("tests/old_letronna_spec_test.go").exists());
        assert!(root.join("tests/mine_letronna_spec_test.go").exists());
        // With no tester marked as writing there, as when none ran, git
        // tells the project's own from the rest.
        written(&root, "tests/late_letronna_spec_test.go");
        let kept = keep(156, &root, &root, None).unwrap();
        assert_eq!(
            kept.files,
            [
                "tests/late_letronna_spec_test.go",
                "tests/mine_letronna_spec_test.go"
            ]
        );
        tidy(156);
        assert!(root.join("tests/old_letronna_spec_test.go").exists());

        // The same where git cannot read the repository, as it cannot one
        // another user owns: nothing that was there is taken.
        let broken = Scratch::new("spec-tests-keep-broken-git");
        std::fs::write(broken.join(".git"), "not a repository").unwrap();
        assert!(git(&broken, &["status"]).is_none());
        written(&broken, "tests/old_letronna_spec_test.go");
        let writing = writing_in(154, &broken);
        written(&broken, "tests/new_letronna_spec_test.go");
        let kept = keep(154, &broken, &broken, None).unwrap();
        assert_eq!(kept.files, ["tests/new_letronna_spec_test.go"]);
        drop(writing);
        tidy(154);
        assert!(broken.join("tests/old_letronna_spec_test.go").exists());
        // With no tester marked as writing there and git unable to say,
        // every file so named is the tester's.
        written(&broken, "tests/more_letronna_spec_test.go");
        let kept = keep(155, &broken, &broken, None).unwrap();
        assert_eq!(
            kept.files,
            [
                "tests/more_letronna_spec_test.go",
                "tests/old_letronna_spec_test.go"
            ]
        );
        tidy(155);
    }

    #[test]
    fn the_last_run_to_end_in_a_folder_sweeps_the_copies_left_there() {
        let root = Scratch::new("spec-tests-strays-shared");
        let from = Scratch::new("spec-tests-strays-shared-from");
        assert!(git(&root, &["init", "-q"]).is_some());
        written(&from, "a/one_letronna_spec_test.go");
        keep(144, &from, &root, None).unwrap();
        written(&from, "b/two_letronna_spec_test.go");
        keep(145, &from, &root, None).unwrap();
        written(&root, "a/a/one_letronna_spec_test.go");
        written(&root, "b/b/two_letronna_spec_test.go");

        // The other run may be running its copies still.
        assert!(tidy(144).is_empty());
        assert!(root.join("b/b/two_letronna_spec_test.go").exists());
        assert_eq!(
            tidy(145),
            [
                "a/a/one_letronna_spec_test.go",
                "b/b/two_letronna_spec_test.go"
            ]
        );
    }

    #[test]
    fn outside_git_only_the_paths_the_tests_were_kept_from_are_deleted() {
        let root = Scratch::new("spec-tests-strays-no-git");
        let from = Scratch::new("spec-tests-strays-no-git-from");
        written(&from, "tests/letronna_spec_test.go");
        keep(146, &from, &root, None).unwrap();
        written(&root, "tests/letronna_spec_test.go");
        written(&root, "tests/tests/letronna_spec_test.go");

        assert_eq!(tidy(146), ["tests/letronna_spec_test.go"]);
        assert!(root.join("tests/tests/letronna_spec_test.go").exists());
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
        // One its budget stopped is not said to have run them all.
        let spent = "tester stopped when the helper budget ran out; its partial report is \
                     below.\nR1 | x | PASS";
        let partial = told(spent, None, false);
        assert!(
            partial.contains("ran out of budget before it was done"),
            "{partial}"
        );
        assert!(!partial.contains("and ran them"), "{partial}");
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
            brief.contains(
                "from the project's root, runs your tests and no others, with paths relative to it"
            ),
            "{brief}"
        );
        assert!(
            brief.contains("runs them once and exits rather than watching, installs nothing"),
            "{brief}"
        );
        // With the set-up its own commands needed, since the driver runs the
        // command in a copy that has the same.
        assert!(
            brief.contains(
                "with any variable your own test commands needed, such as a module cache path, \
                 as `VAR=value` words before the command"
            ),
            "{brief}"
        );
        // Its own tests alone: a test of the project's own can fail where
        // the driver runs the command for reasons of its own.
        assert!(
            brief.contains("the one command that, from the project's root, runs your tests"),
            "{brief}"
        );
        assert!(
            brief.contains(
                "names your test files or filters on a name all your tests share, such as \
                 `go test -run LetronnaSpec ./pkg/`, never the project's whole suite, such as \
                 `go test ./...`"
            ),
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
        // before a long table, the cut loses it, and `agents::answer` writes
        // it again after the quote.
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
        let copy = Path::new("/tmp/letronna-harness-copy-1");
        let project = Path::new("/work/project");
        assert_eq!(
            relative(
                "cd /tmp/letronna-harness-copy-1 && pytest /tmp/letronna-harness-copy-1/tests/t_letronna_spec.py",
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

    /// What `run_command` makes of a command that printed `output` and
    /// ended with `code`.
    fn exited(output: &str, code: u8) -> Result<String, String> {
        Ok(format!("{output}\n\n[exit code {code}]"))
    }

    #[test]
    fn a_runner_that_is_not_installed_is_no_failing_test() {
        let deno = "deno test --allow-read packages/core/src/letronna_spec_x.test.ts";
        let missing = exited(
            "\n[stderr]\n/usr/bin/bash: line 1: deno: command not found",
            127,
        );
        assert_eq!(
            unrunnable(deno, &missing).as_deref(),
            Some(
                "its program `deno` is not installed here (`/usr/bin/bash: line 1: deno: \
                 command not found`)"
            )
        );
        // Each shell's words, env's, timeout's, a wrapper's and Python's,
        // for the program the command names; under a wrapper, for a runner
        // its script names.
        let found_missing = [
            (
                "export CI=1; deno test",
                "sh: 1: deno: not found",
                127,
                "deno",
            ),
            ("deno test", "zsh:1: command not found: deno", 127, "deno"),
            (
                "env CI=1 deno test",
                "env: ‘deno’: No such file or directory",
                127,
                "deno",
            ),
            (
                "/opt/deno/bin/deno test",
                "bash: line 1: /opt/deno/bin/deno: No such file or directory",
                127,
                "deno",
            ),
            (
                "timeout 60 deno test",
                "timeout: failed to run command ‘deno’: No such file or directory",
                127,
                "deno",
            ),
            (
                "deno test",
                "'deno' is not recognized as an internal or external command,\noperable program or batch file.",
                1,
                "deno",
            ),
            (
                "deno test",
                "deno : The term 'deno' is not recognized as the name of a cmdlet",
                1,
                "deno",
            ),
            (
                "pnpm -F core exec vitest run",
                " ERR_PNPM_RECURSIVE_EXEC_FIRST_FAIL  Command \"vitest\" not found",
                1,
                "vitest",
            ),
            ("yarn jest", "error Command \"jest\" not found.", 1, "jest"),
            (
                "python3 -m pytest -q tests",
                "/usr/bin/python3: No module named pytest",
                1,
                "pytest",
            ),
            ("npm test", "sh: 1: jest: not found", 127, "jest"),
        ];
        for (command, output, code, name) in found_missing {
            let why = unrunnable(command, &exited(output, code));
            assert!(
                why.as_deref()
                    .is_some_and(|why| why.starts_with(&format!("its program `{name}` is"))),
                "{command}: {output}: {why:?}"
            );
        }
        // Not for a program the command does not name: a tester's script
        // that calls the tool the work was to add fails the same way, and
        // so does a command that runs the work's own tool by its path.
        let not_missing = [
            (
                "sh tests/letronna_spec_cli.sh",
                "tests/letronna_spec_cli.sh: 3: mytool: not found",
                127,
            ),
            (
                "sh tests/letronna_spec_cli.sh",
                "tests/letronna_spec_cli.sh: line 3: ./bin/tool: No such file or directory",
                127,
            ),
            ("./bin/tool --selftest", "sh: 1: ./bin/tool: not found", 127),
            ("npm test", "sh: 1: mytool: not found", 127),
            (
                "python3 -m pytest",
                "ModuleNotFoundError: No module named 'mytool'",
                1,
            ),
            // Those words with another exit code are something a program said.
            ("deno test", "deno: command not found", 1),
        ];
        for (command, output, code) in not_missing {
            assert_eq!(
                unrunnable(command, &exited(output, code)),
                None,
                "{command}: {output}"
            );
        }
        // A test that logged them and failed is a failing test, whatever
        // the runner's marks; so is a suite that ran before a later step
        // of the command went missing.
        for output in [
            "--- FAIL: TestShell (0.00s)\n    shell_test.go:9: got \"sh: deno: command not found\"\nFAIL",
            "✖ runs the hook (3ms)\n  sh: 1: deno: not found\nℹ fail 1",
            "not ok 1 - runs the hook\n# sh: deno: command not found",
            "test result: ok. 3 passed; 0 failed\nsh: line 1: deno: command not found",
            "running 3 tests\nsh: line 1: deno: command not found",
            "collected 2 items\nsh: line 1: deno: command not found",
        ] {
            assert_eq!(
                unrunnable("deno test", &exited(output, 127)),
                None,
                "{output}"
            );
        }
        // A script's own word that it is running them is no test run.
        let echoed = exited("running tests...\nsh: line 2: deno: command not found", 127);
        assert!(unrunnable("deno test", &echoed).is_some());
        // Nor is one that passed or was cut at its limit.
        assert_eq!(
            unrunnable("deno test", &Ok("deno: command not found".into())),
            None
        );
        assert_eq!(
            unrunnable("deno test", &Err("Timed out after 600s.".into())),
            None
        );
    }

    #[test]
    fn a_fetch_that_failed_before_any_test_ran_is_no_failing_test() {
        let command = "pnpm -F core exec vitest run predicate.letronna_spec";
        let pnpm = exited(
            " ERR_PNPM_META_FETCH_FAIL  GET https://registry.npmjs.org/pnpm: request to \
             https://registry.npmjs.org/pnpm failed, reason: getaddrinfo EAI_AGAIN \
             registry.npmjs.org\n\nThis error happened while installing a direct dependency \
             of /tmp/bench-home/.local/share/pnpm/.tools/pnpm/10.28.1_tmp_5923\n\n[stderr]\n \
             ERROR  Command failed with exit code 1: pnpm add pnpm@10.28.1 \
             --loglevel=error\nFor help, run: pnpm help exec",
            1,
        );
        let why = unrunnable(command, &pnpm).unwrap();
        assert!(
            why.starts_with(
                "a package or a host it needs could not be reached from here \
                 (`ERR_PNPM_META_FETCH_FAIL  GET https://registry.npmjs.org/pnpm"
            ),
            "{why}"
        );
        for output in [
            "curl: (6) Could not resolve host: deno.land",
            "npm error code ENOTFOUND\nnpm error network request to \
             https://registry.npmjs.org/vitest failed",
            "FAIL\texample.com/app [setup failed]",
        ] {
            assert!(
                unrunnable(command, &exited(output, 1)).is_some(),
                "{output}"
            );
        }
        // A test that could not reach a host failed, as its runner says;
        // and the work's own server not answering is no registry.
        for output in [
            " FAIL  tests/fetch.test.ts > loads the page\nError: getaddrinfo EAI_AGAIN \
             example.com\n Test Files  1 failed (1)",
            "FAILED tests/test_fetch.py::test_loads - NewConnectionError\n1 failed in 0.20s",
            " ✓ parses (2ms)\n log: getaddrinfo ENOTFOUND example.com\nexit after hook",
            "Error: connect ECONNREFUSED 127.0.0.1:3000",
            "getaddrinfo ENOTFOUND for [::1]:3000",
            "curl: (6) Could not resolve host: ::1",
            "requests.exceptions.ConnectionError: NewConnectionError: localhost:8080 refused",
            "dial tcp: lookup localhost: no such host",
        ] {
            assert_eq!(unrunnable(command, &exited(output, 1)), None, "{output}");
        }
        // An address that only ends as this computer's does is another's.
        let far = exited(
            "npm error network request to 2001:db8::1 failed, ENOTFOUND",
            1,
        );
        assert!(unrunnable(command, &far).is_some());
        // make ends with its own code, so the shell's is never seen under it.
        let made = exited(
            "/bin/sh: jest: command not found\nmake: *** [test] Error 127",
            2,
        );
        assert_eq!(unrunnable("make test", &made), None);
        // A refused connection is no sign of a set-up that failed, so a
        // passing command that mentions one still lends its set-up.
        assert!(!set_up_failed("ok: ECONNREFUSED is retried"));
    }

    #[cfg(unix)]
    #[test]
    fn a_fetch_that_fails_on_the_work_alone_is_the_works_failure() {
        let root = Scratch::new("spec-tests-run-fetch");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join("README"), "a project").unwrap();
        repo(&["add", "."]);
        repo(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "c",
        ]);
        // The registry is out of reach wherever `needs-net` is, as a
        // dependency the work added would leave it.
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            "test -f needs-net && { echo 'npm error code ENOTFOUND'; exit 1; }\necho '--- FAIL: \
             TestMade (0.00s)'; exit 1\n",
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(28, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        std::fs::write(root.join("needs-net"), "").unwrap();
        // On the project as it was the command reaches its tests: a failure.
        let ran_on_work = ran(&kept, &command, &ctx, TIMEOUT, || None);
        let Ran::Failed(told) = &ran_on_work else {
            panic!("{ran_on_work:?}");
        };
        assert!(told.contains("npm error code ENOTFOUND"), "{told}");
        // A fetch that failed once and then reached the tests was a blip:
        // what the second run made of them is what came of them.
        let mark = root.join("blipped");
        std::fs::write(
            kept.dir.join("letronna_spec_test.sh"),
            format!(
                "test -f needs-net && ! test -f {mark} && {{ touch {mark}; echo 'npm error code \
                 ENOTFOUND'; exit 1; }}\ntest -f needs-net || {{ echo '--- FAIL: TestMade'; exit \
                 1; }}\n",
                mark = mark.display()
            ),
        )
        .unwrap();
        assert_eq!(
            ran(&kept, &command, &ctx, TIMEOUT, || None),
            Ran::Passed(command.clone())
        );
        std::fs::remove_file(&mark).unwrap();
        std::fs::write(
            kept.dir.join("letronna_spec_test.sh"),
            "test -f needs-net && { echo 'npm error code ENOTFOUND'; exit 1; }\nexit 1\n",
        )
        .unwrap();
        // Where the project as it was is the work, or cannot reach it
        // either, nothing is known of the work.
        repo(&["add", "needs-net"]);
        repo(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.com",
            "commit",
            "-qm",
            "d",
        ]);
        let ran_before = ran(&kept, &command, &ctx, TIMEOUT, || None);
        assert!(
            matches!(&ran_before, Ran::Broken(why) if why.contains("ENOTFOUND")),
            "{ran_before:?}"
        );
        tidy(28);
    }

    #[cfg(unix)]
    #[test]
    fn a_command_whose_runner_is_missing_is_not_sent_back_as_a_failure() {
        let root = Scratch::new("spec-tests-run-missing");
        let repo = |args: &[&str]| assert!(git(&root, args).is_some(), "git {args:?}");
        repo(&["init", "-q"]);
        std::fs::write(root.join("README"), "a project").unwrap();
        repo(&["add", "."]);
        std::fs::write(root.join("letronna_spec_test.sh"), "exit 0\n").unwrap();
        let command = "letronna-no-such-runner test ./letronna_spec_test.sh".to_owned();
        let kept = keep(27, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        // The first version is not asked for: nothing was judged.
        let ran = ran(
            &kept,
            &command,
            &ctx,
            TIMEOUT,
            || -> Option<FirstVersion> { panic!("the first version was asked for") },
        );
        let Ran::Broken(why) = &ran else {
            panic!("{ran:?}");
        };
        assert!(
            why.starts_with(&format!(
                "`{command}` did not get as far as a test, since its program \
                 `letronna-no-such-runner` is not installed here (`"
            )),
            "{why}"
        );
        let told = first_run(&ran);
        assert!(
            told.starts_with(
                "The tests a tester wrote from the request alone could not be run against \
                 your work as it stands, which says nothing of the work: `letronna-no-such-runner"
            ),
            "{told}"
        );
        assert!(
            told.contains("Run them yourself the way you found works"),
            "{told}"
        );
        assert!(!told.contains("exited with code"), "{told}");
        tidy(27);
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
        let copy = Path::new("/tmp/letronna-harness-copy-7");
        let prefix = "export PYTHONPATH=\"/tmp/letronna-harness-copy-7/src:/tmp/letronna-harness-copy-7\
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
        // Words of a count are read where a word starts.
        assert!(ran_nothing("0 tests run"));
        assert!(!ran_nothing("10 tests run"));
        assert!(!ran_nothing("Ran 10 tests in 0.1s"));
        assert!(!ran_nothing("Tests:       10 total"));
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
                 change. Its output:\n\ntest parses ... FAILED\n\n[exit code 101]\n\n\
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
    fn output_naming_no_failure_is_shown_from_a_lines_start_to_its_end() {
        let lines: Vec<String> = (0..400)
            .map(|at| format!("line {at:04} {}", "x".repeat(20)))
            .collect();
        let long = lines.join("\n");
        let shown = excerpt(&long);
        assert!(shown.starts_with("…\nline "), "{shown}");
        assert!(shown.ends_with(lines.last().unwrap()), "{shown}");
        assert_eq!(shown.matches('…').count(), 1, "{shown}");
        assert!(shown.chars().count() <= SHOWN, "{}", shown.len());
    }

    #[test]
    fn short_output_is_shown_as_it_is() {
        assert_eq!(excerpt("a\nb"), "a\nb");
        let failed = "--- FAIL: TestA (0.00s)\n    a_test.go:3: boom\nFAIL\n[exit code 1]";
        assert_eq!(excerpt(&format!("{failed}\n\n")), failed);
    }

    /// One test's log lines as an ORM prints them, which mention errors
    /// without any test failing.
    fn orm_log(at: usize) -> String {
        format!(
            "2026/11/09 11:46:38 /tmp/build/drift_detection_service.go:406 record not found\n\
             [0.915ms] [rows:0] SELECT * FROM `drifts` WHERE `drifts`.`id` = {at} ORDER BY \
             `drifts`.`id` LIMIT 1\n"
        )
    }

    #[test]
    fn a_go_tests_failure_is_shown_past_the_log_lines_around_it() {
        let mut output = String::new();
        for at in 0..40 {
            output.push_str(&orm_log(at));
        }
        output.push_str(
            "2026/11/09 11:46:38 /tmp/build/baseline.go:88 SQL logic error: no such table (1)\n",
        );
        output.push_str(&orm_log(40));
        output.push_str(
            "--- FAIL: TestLetronnaSpecDriftScoreComputation (0.00s)\n\
             \x20   drift_letronna_spec_test.go:45: \n\
             \x20       \tError Trace:\t/app/backend/drift_letronna_spec_test.go:45\n\
             \x20       \tError:      \tNot equal: \n\
             \x20       \t            \texpected: 0.5\n\
             \x20       \t            \tactual  : 0\n\
             \x20       \tTest:       \tTestLetronnaSpecDriftScoreComputation\n",
        );
        let insert = format!(
            "[1.204ms] [rows:1] INSERT INTO `drifts` VALUES {}",
            "(1, 0.5), ".repeat(FAILURE_LINE / 5)
        );
        output.push_str(&format!("{insert}\n"));
        for at in 41..80 {
            output.push_str(&orm_log(at));
        }
        output.push_str("FAIL\nFAIL\tgithub.com/acme/backend/services\t2.309s\n[exit code 1]\n");

        let shown = excerpt(&output);
        assert!(
            shown.starts_with(
                "…\n--- FAIL: TestLetronnaSpecDriftScoreComputation (0.00s)\n    \
                 drift_letronna_spec_test.go:45: \n"
            ),
            "{shown}"
        );
        assert!(shown.contains("\texpected: 0.5\n"), "{shown}");
        assert!(shown.contains("\tactual  : 0\n"), "{shown}");
        // A long line in a failure's wake is cut, and the log after it is
        // left out up to the end.
        let cut: String = insert.chars().take(FAILURE_LINE).collect();
        assert!(shown.contains(&format!("\n{cut}…\n…\n")), "{shown}");
        assert!(shown.ends_with("FAIL\tgithub.com/acme/backend/services\t2.309s\n[exit code 1]"));
        assert!(!shown.contains("SQL logic error"), "{shown}");
        assert!(shown.chars().count() <= SHOWN, "{}", shown.chars().count());
    }

    #[test]
    fn pytests_assertion_is_shown_past_the_logs_it_captured() {
        let mut output = String::from(
            "============================= test session starts ==============================\n\
             collected 30 items\n\n\
             tests/test_letronna_spec_cart.py ..............F...............          [100%]\n\n\
             =================================== FAILURES ===================================\n\
             ___________________________ test_discount_is_capped ____________________________\n\n\
             \x20   def test_discount_is_capped():\n\
             \x20       cart = Cart()\n\
             >       assert cart.total(discount=0.9) == 5\n\
             E       AssertionError: assert 1.0 == 5\n\
             E        +  where 1.0 = total(discount=0.9)\n\n\
             tests/test_letronna_spec_cart.py:12: AssertionError\n\
             ------------------------------ Captured log call -------------------------------\n",
        );
        for at in 0..80 {
            output.push_str(&format!(
                "DEBUG    cart:cart.py:20 total for item {at}, error budget 0\n"
            ));
        }
        output.push_str(
            "=========================== short test summary info ============================\n\
             FAILED tests/test_letronna_spec_cart.py::test_discount_is_capped - AssertionError\n\
             ========================= 1 failed, 29 passed in 0.31s =========================\n\
             [exit code 1]",
        );

        let shown = excerpt(&output);
        assert!(
            shown.contains(
                "…\nE       AssertionError: assert 1.0 == 5\nE        +  where 1.0 = \
                 total(discount=0.9)\n\ntests/test_letronna_spec_cart.py:12: AssertionError\n"
            ),
            "{shown}"
        );
        assert!(
            shown.contains("\nFAILED tests/test_letronna_spec_cart.py::test_discount_is_capped")
        );
        assert!(
            shown
                .ends_with("1 failed, 29 passed in 0.31s =========================\n[exit code 1]")
        );
        // The session's opening and the logs past the failure's own few
        // lines are left out.
        assert!(!shown.contains("collected 30 items"), "{shown}");
        assert!(!shown.contains("item 20,"), "{shown}");
        assert!(shown.chars().count() <= SHOWN, "{}", shown.chars().count());
    }

    #[test]
    fn jests_expected_and_received_are_shown_past_another_files_console() {
        let mut output = String::from(
            " FAIL  src/cart.letronna_spec.test.js\n\
             \x20 Cart\n\
             \x20   ✓ adds an item (3 ms)\n\
             \x20   ✕ caps the discount (5 ms)\n\n\
             \x20 ● Cart › caps the discount\n\n\
             \x20   expect(received).toBe(expected) // Object.is equality\n\n\
             \x20   Expected: 5\n\
             \x20   Received: 1\n\n\
             \x20     10 |   cart.add(\"book\", 10);\n\
             \x20   > 11 |   expect(cart.total(0.9)).toBe(5);\n\n\
             \x20PASS  src/order.test.js\n\
             \x20 ● Console\n\n",
        );
        for at in 0..60 {
            output.push_str(&format!(
                "    console.log\n      order {at} saved with error margin 0\n\n"
            ));
        }
        output.push_str(
            "Test Suites: 1 failed, 1 passed, 2 total\n\
             Tests:       1 failed, 61 passed, 62 total\n\
             [exit code 1]",
        );

        let shown = excerpt(&output);
        assert!(
            shown.starts_with(" FAIL  src/cart.letronna_spec.test.js\n"),
            "{shown}"
        );
        assert!(shown.contains("  ● Cart › caps the discount\n"), "{shown}");
        assert!(
            shown.contains("    Expected: 5\n    Received: 1\n"),
            "{shown}"
        );
        assert!(shown.ends_with("Tests:       1 failed, 61 passed, 62 total\n[exit code 1]"));
        // Another file's logs are no failure.
        assert!(!shown.contains("● Console"), "{shown}");
        assert!(!shown.contains("order 0 saved"), "{shown}");
        assert!(shown.chars().count() <= SHOWN, "{}", shown.chars().count());
    }

    #[test]
    fn many_failures_leave_the_end_a_third() {
        let output: String = (0..300)
            .map(|at| format!("--- FAIL: TestCase{at:03} (0.00s)\n    case_test.go:9: wrong\n"))
            .chain(["FAIL\tgithub.com/acme/cases\t0.4s\n[exit code 1]".to_owned()])
            .collect();
        let shown = excerpt(&output);
        assert!(
            shown.starts_with("--- FAIL: TestCase000 (0.00s)\n"),
            "{shown}"
        );
        assert!(shown.ends_with("FAIL\tgithub.com/acme/cases\t0.4s\n[exit code 1]"));
        assert!(shown.chars().count() <= SHOWN, "{}", shown.chars().count());
        let (first, _) = shown.split_once("\n…\n").unwrap();
        assert!(first.chars().count() <= FAILURES_SHOWN, "{first}");
    }

    #[test]
    fn a_line_names_a_failure_by_a_runners_own_words_not_by_mentioning_an_error() {
        for line in [
            "--- FAIL: TestA (0.01s)",
            "    --- FAIL: TestA/sub (0.00s)",
            "panic: runtime error: index out of range [recovered]",
            "        \tError Trace:\t/app/a_test.go:12",
            "E       assert 3 == 4",
            "FAILED tests/test_a.py::test_b - assert 3 == 4",
            "  ● sum › adds",
            "    ✕ adds (3 ms)",
            "  × adds 1ms",
            "    Expected: 4",
            "  2 failing",
            "     AssertionError [ERR_ASSERTION]: 3 == -1",
            "org.opentest4j.AssertionFailedError: expected: <5> but was: <1>",
            "thread 'tests::b' panicked at src/lib.rs:10:9:",
            "test tests::b ... FAILED",
            "assertion `left == right` failed",
            "error[E0425]: cannot find value `x` in this scope",
            "error: test failed, to rerun pass `--lib`",
            "error: could not compile `cart` (lib test) due to 1 previous error",
            "FAIL: test_b (tests.test_a.T)",
            "ERROR: test_c (tests.test_a.T)",
            " FAIL  src/a.test.ts > adds",
            "     Failure/Error: expect(x).to eq(1)",
            "./drift_letronna_spec_test.go:12:9: undefined: NewDriftService",
            "FAIL\tgithub.com/acme/backend [build failed]",
            "CartTest > totalIsSum() FAILED",
            "Failed asserting that 1 matches expected 5.",
            "1) Tests\\CartTest::testTotal",
            "[ERROR] totalIsSum(com.acme.CartTest)  Time elapsed: 0.01 s  <<< FAILURE!",
            "[ERROR] loads(com.acme.CartTest)  Time elapsed: 0.01 s  <<< ERROR!",
            "src/cart.ts(3,5): error TS2304: Cannot find name 'total'.",
        ] {
            assert_eq!(named(line), Some(Named::Verdict), "{line}");
        }
        // What a passing test can log too comes after the verdicts.
        for line in [
            "Traceback (most recent call last):",
            "ValueError: bad input",
            "        \tError:      \tNot equal: ",
            "java.lang.IllegalArgumentException: quantity must be positive",
            "Error: Not implemented: HTMLCanvasElement.prototype.getContext",
            "error: GET /carts/3 failed with 404 {\"service\":\"carts\"}",
            "error: aborting due to 2 previous errors",
        ] {
            assert_eq!(named(line), Some(Named::Raised), "{line}");
        }
        for line in [
            "2026/11/09 11:46:38 /tmp/a.go:406 record not found",
            "2026/11/09 11:46:38 /tmp/a.go:88 SQL logic error: no such table (1)",
            "[0.915ms] [rows:0] SELECT * FROM `drifts`",
            "    drift_test.go:45: a log line",
            "FAIL\tgithub.com/acme/backend\t2.309s",
            "  ● Console",
            "DEBUG    cart:cart.py:20 error budget 0",
            "test result: ok. 12 passed; 0 failed",
            "ok  \tgithub.com/acme/backend\t0.2s",
            "Errors: 0",
            "[ERROR] Tests run: 3, Failures: 1, Errors: 0, Skipped: 0",
        ] {
            assert_eq!(named(line), None, "{line}");
        }
        // Nor does anything a test logged under jest's heading.
        let logged = [
            "  console.error",
            "    Error: Not implemented",
            "",
            "  ● Chart › draws",
        ];
        assert_eq!(
            named_lines(&logged),
            [None, None, None, Some(Named::Verdict)]
        );
    }

    /// `lines` of `filler` before and after `middle`, ending in `summary`.
    fn amid(filler: &str, lines: usize, middle: &str, summary: &str) -> String {
        let noise = format!("{filler}\n").repeat(lines);
        format!("{noise}{middle}{noise}{summary}")
    }

    #[test]
    fn a_jest_failure_is_shown_before_the_errors_a_page_logged() {
        let console = "    console.error\n      Error: Not implemented: HTMLCanvasElement.prototype.\
                       getContext (without installing the canvas npm package)\n          at \
                       module.exports (node_modules/jsdom/lib/jsdom/browser/not-implemented.js:9:17)\n\n";
        let raw = "Error: Not implemented: navigation (except hash changes)\n    at \
                   module.exports (node_modules/jsdom/lib/jsdom/browser/not-implemented.js:9:17)\n";
        let output = format!(
            " FAIL  src/chart.letronna_spec.test.js\n  ● Console\n\n{}{}  ● Chart › draws the \
             total\n\n    expect(received).toBe(expected) // Object.is equality\n\n    \
             Expected: 42\n    Received: 0\n\n{}Tests:       1 failed, 3 passed, 4 total\n\
             [exit code 1]",
            console.repeat(20),
            raw.repeat(20),
            "  console.log\n    rendered the chart\n\n".repeat(60),
        );
        let shown = excerpt(&output);
        assert!(shown.contains("  ● Chart › draws the total\n"), "{shown}");
        assert!(
            shown.contains("    Expected: 42\n    Received: 0\n"),
            "{shown}"
        );
        // What a page logged is no failure, and what it raised shows once.
        assert!(!shown.contains("HTMLCanvasElement"), "{shown}");
        let raised = shown.matches("navigation (except hash changes)").count();
        assert!((1..=5).contains(&raised), "{shown}");
        assert!(shown.ends_with("Tests:       1 failed, 3 passed, 4 total\n[exit code 1]"));
    }

    #[test]
    fn a_mocha_failure_is_shown_before_the_errors_an_apps_logger_printed() {
        // A Node app logging through winston prints `error: ...` on every
        // negative test, before and after the failure; only the assertion
        // is the runner's own word.
        let logged = "error: GET /carts/3 failed with 404 {\"service\":\"carts\",\"timestamp\":\
                      \"2026-11-09T11:46:38.123Z\",\"level\":\"error\"}\n";
        let failure = "  1 failing\n\n  1) Cart\n       totals the items:\n\n      \
                       AssertionError [ERR_ASSERTION]: Expected values to be strictly equal:\n\n      \
                       3 !== 4\n\n      + expected - actual\n\n      -3\n      +4\n\n      at \
                       Context.<anonymous> (test/cart_letronna_spec.test.js:42:12)\n";
        let output = format!(
            "\n  Cart\n    ✓ adds an item (12ms)\n{}\n{failure}{}[exit code 1]",
            logged.repeat(30),
            logged.repeat(30),
        );
        let shown = excerpt(&output);
        assert!(shown.contains(failure), "{shown}");
        assert!(shown.chars().count() <= SHOWN, "{}", shown.chars().count());
        // The logger's lines take only what the verdicts leave.
        assert!(shown.matches("error: GET /carts/3").count() < 30, "{shown}");
        assert!(shown.ends_with("[exit code 1]"), "{shown}");
    }

    #[test]
    fn a_maven_failure_is_shown_before_the_exceptions_its_tests_logged() {
        let logged = "java.lang.IllegalArgumentException: quantity must be positive\n\
                      \tat com.acme.Cart.add(Cart.java:20)\n\
                      \tat com.acme.CartTest.rejectsNegative(CartTest.java:30)\n";
        let output = format!(
            "{}[ERROR] Tests run: 3, Failures: 1, Errors: 0, Skipped: 0, Time elapsed: 0.05 s \
             <<< FAILURE! - in com.acme.CartTest\n[ERROR] totalIsSum(com.acme.CartTest)  Time \
             elapsed: 0.01 s  <<< FAILURE!\norg.opentest4j.AssertionFailedError: expected: <5> \
             but was: <1>\n\tat com.acme.CartTest.totalIsSum(CartTest.java:12)\n{}[INFO] BUILD \
             FAILURE\n[exit code 1]",
            logged.repeat(40),
            "[INFO] Downloading from central: https://repo.example/artifact.pom\n".repeat(60),
        );
        let shown = excerpt(&output);
        assert!(
            shown.contains("totalIsSum(com.acme.CartTest)  Time elapsed: 0.01 s  <<< FAILURE!\n"),
            "{shown}"
        );
        assert!(
            shown.contains("AssertionFailedError: expected: <5> but was: <1>"),
            "{shown}"
        );
        assert!(
            shown.ends_with("[INFO] BUILD FAILURE\n[exit code 1]"),
            "{shown}"
        );
        assert!(shown.chars().count() <= SHOWN, "{}", shown.chars().count());
    }

    #[test]
    fn a_diffs_values_are_shown_past_its_blank_lines() {
        let failure = "  ● Cart › totals\n\n    expect(received).toEqual(expected) // deep \
                       equality\n\n    - Expected  - 1\n    + Received  + 1\n\n      Object {\n\
                       \x20   -   \"total\": 5,\n    +   \"total\": 1,\n      }\n\n";
        let output = amid(
            "    console.log at render",
            200,
            failure,
            "Tests:       1 failed, 1 total\n[exit code 1]",
        );
        let shown = excerpt(&output);
        assert!(
            shown.contains("    -   \"total\": 5,\n    +   \"total\": 1,\n"),
            "{shown}"
        );
    }

    #[test]
    fn a_go_build_error_is_shown_past_a_long_download_log() {
        let shown = excerpt(&amid(
            "go: downloading github.com/acme/orm v1.2.3",
            100,
            "# github.com/acme/backend/services\n\
             ./drift_letronna_spec_test.go:12:9: undefined: NewDriftService\n",
            "FAIL\tgithub.com/acme/backend/services [build failed]\n[exit code 1]",
        ));
        assert!(
            shown.contains("\n./drift_letronna_spec_test.go:12:9: undefined: NewDriftService\n"),
            "{shown}"
        );
    }

    #[test]
    fn a_long_last_line_and_a_progress_bar_are_cut_where_they_show() {
        let last = format!("{}the end", "x".repeat(2 * SHOWN));
        let shown = excerpt(&last);
        assert!(shown.starts_with("…x"), "{shown}");
        assert!(shown.ends_with("the end"), "{shown}");
        assert!(shown.chars().count() <= SHOWN, "{}", shown.chars().count());
        // Each redraw a carriage return makes is a line of its own.
        let bars: String = (0..400).map(|at| format!("\rdownloading {at}%")).collect();
        let output = format!("{bars}\n--- FAIL: TestA (0.00s)\n{bars}\nFAIL\n[exit code 1]");
        let shown = excerpt(&output);
        assert!(
            shown.starts_with("…\n--- FAIL: TestA (0.00s)\n\ndownloading 0%\n"),
            "{shown}"
        );
        assert!(
            shown.ends_with("downloading 399%\nFAIL\n[exit code 1]"),
            "{shown}"
        );
    }

    #[test]
    fn the_notice_says_lines_were_left_out_only_when_they_were() {
        let short = Ok("--- FAIL: TestA (0.00s)\nFAIL\n[exit code 1]".to_owned());
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["a_letronna_spec_test.go".into()],
        };
        let whole = failed(&kept, "go test ./...", &short).unwrap();
        assert!(
            whole.contains("exited with code 1. Its output:\n\n--- FAIL"),
            "{whole}"
        );
        let long = Ok(amid(
            "log",
            2000,
            "--- FAIL: TestA (0.00s)\n",
            "FAIL\n[exit code 1]",
        ));
        let cut = failed(&kept, "go test ./...", &long).unwrap();
        assert!(
            cut.contains("Its output, with `…` for lines left out:\n\n…\n--- FAIL"),
            "{cut}"
        );
    }

    #[test]
    fn whose_tests_fail_is_read_off_the_failures_lines_or_the_command() {
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["color_letronna_spec_test.go".to_owned()],
        };
        let whose = |command: &str, output: &str| own_failing(&kept, command, output);
        // The project's whole suite, with a test of the project's own
        // failing: none of the tester's.
        let others = "--- FAIL: TestEnvNoColor (0.00s)\n    termenv_test.go:45: expected \
                      true\nFAIL\nFAIL\tgithub.com/acme/termenv\t0.4s\n[exit code 1]";
        assert_eq!(whose("go test ./...", others), Some(false));
        // The tester's: by the test's name in any case, by its file among the
        // failure's lines, or by a command that runs its tests alone.
        let named = "--- FAIL: TestLetronnaSpecRestoresColor (0.00s)\n    x_test.go:9: wrong\nFAIL";
        assert_eq!(whose("go test ./...", named), Some(true));
        let filed = "--- FAIL: TestRestoresColor (0.00s)\n    color_letronna_spec_test.go:9: \
                     wrong\nFAIL";
        assert_eq!(whose("go test ./...", filed), Some(true));
        assert_eq!(
            whose("go test -run TestLetronnaSpec ./pkg/a/ ./pkg/b/", others),
            Some(true)
        );
        assert_eq!(
            whose("pytest -q tests/test_letronna_spec.py", others),
            Some(true)
        );
        // Other runners' summaries name the file.
        let pytest = "FAILED tests/test_other.py::test_y - assert 1 == 2\n[exit code 1]";
        assert_eq!(whose("pytest -q", pytest), Some(false));
        let own = "FAILED tests/test_letronna_spec.py::test_y - assert 1 == 2\n[exit code 1]";
        assert_eq!(whose("pytest -q", own), Some(true));
        let jest = " FAIL  src/other.test.ts\n  ● adds\n\n    Expected: 4\n    Received: 3\n";
        assert_eq!(whose("npx jest", jest), Some(false));
        let hyphened = " FAIL  src/letronna-spec.test.ts\n  ● adds\n\n    Expected: 4\n";
        assert_eq!(whose("npx jest", hyphened), Some(true));
        // The folder the command runs in, as a runner printing absolute
        // paths shows it, names nothing of the tester's.
        let traced = "--- FAIL: TestEnvNoColor (0.00s)\n    Error Trace:\t\
                      /tmp/letronna-harness-copy-4242-7/termenv_test.go:45\n    Error:      \t\
                      Should be true\n    Test:       \tTestEnvNoColor\nFAIL";
        assert_eq!(whose("go test ./...", traced), Some(false));
        // A mention of the tester's package well past a failure's lines is
        // no failure of its own.
        let apart = format!(
            "{others}\n{}ok  \tgithub.com/acme/letronna_spec\t0.1s",
            "    a log line\n".repeat(60)
        );
        assert_eq!(whose("go test ./...", &apart), Some(false));
        // Output naming no failure, as a runner this does not know prints or
        // a command that did not finish leaves it, says nothing either way.
        assert_eq!(
            whose("sh ./*_test.sh", "made.txt is missing\n[exit code 1]"),
            None
        );
        assert_eq!(whose("go test ./...", "Timed out after 600s."), None);
        // A build error, a panic or a logger's lines say that something
        // failed, not whose: nothing either way.
        let compile = "error[E0425]: cannot find value `total` in this scope\n --> src/lib.rs:3:5\n\n\
                       error: could not compile `cart` (lib test) due to 1 previous error\n\
                       [exit code 101]";
        assert_eq!(whose("cargo test", compile), None);
        let built = "# github.com/acme/pkg\n./cart.go:12:9: undefined: Total\nFAIL\tgithub.com/acme/pkg \
                     [build failed]\n[exit code 1]";
        assert_eq!(whose("go test ./pkg/", built), None);
        let logged = format!(
            "{}[exit code 1]",
            "error: GET /carts/3 failed with 404\n".repeat(5)
        );
        assert_eq!(whose("npx mocha", &logged), None);
        // A panic in the work reached from the tester's test: the frame in
        // its file is far below the failure's line, past what the notice
        // shows of a failure, and still within the window.
        let frames =
            "goroutine 7 [running]:\ntesting.tRunner.func1.2()\n\t/usr/local/go/src/testing/\
                      testing.go:1632 +0x3fc\n"
                .repeat(24);
        assert!(frames.chars().count() > FAILURE_SHOWN && frames.chars().count() < OWN_WINDOW);
        let panicked = format!(
            "--- FAIL: TestTotal (0.00s)\npanic: runtime error: invalid memory address \
             [recovered]\n{frames}github.com/acme/pkg.TestTotal(0xc000106340)\n\t\
             /tmp/letronna-harness-copy-1-2/total_letronna_spec_test.go:9 +0x1d\nFAIL\t\
             github.com/acme/pkg\t0.01s"
        );
        assert_eq!(whose("go test ./pkg/", &panicked), Some(true));
        // But not past where a passing test's lines begin.
        let passed = "--- FAIL: TestTotal (0.00s)\n    cart_test.go:9: wrong\n=== RUN   \
                      TestLetronnaSpecEmpty\n--- PASS: TestLetronnaSpecEmpty (0.00s)\nFAIL";
        assert_eq!(whose("go test ./pkg/", passed), Some(false));
        // pytest's -rA summary lists what passed after what failed, and
        // cargo goes on to the next binary: neither names a failure of the
        // tester's own.
        let summary = "FAILED tests/test_other.py::test_y - assert 1 == 2\nPASSED \
                       tests/test_letronna_spec.py::test_adds\nXPASS \
                       tests/test_letronna_spec.py::test_more\n1 failed, 2 passed in 0.3s";
        assert_eq!(whose("pytest -rA", summary), Some(false));
        let binaries = "test tests::adds ... FAILED\n\nfailures:\n    tests::adds\n\ntest result: \
                        FAILED. 0 passed; 1 failed; 0 ignored\n\n     Running \
                        tests/letronna_spec_cart.rs (target/debug/deps/letronna_spec_cart-1a2b)\n\n\
                        running 0 tests\n[exit code 101]";
        assert_eq!(whose("cargo test --no-fail-fast", binaries), Some(false));
    }

    #[test]
    fn a_go_test_named_without_the_marker_is_known_by_its_kept_file() {
        let dir = Scratch::new("spec-tests-go-names");
        std::fs::create_dir_all(dir.join("pkg")).unwrap();
        std::fs::write(
            dir.join("pkg/total_letronna_spec_test.go"),
            "package pkg\n\nimport \"testing\"\n\nfunc TestTotal(t *testing.T) {\n\tassertEqual(t, \
             Total(), 4)\n}\n\nfunc TestEmpty(t *testing.T) {}\n",
        )
        .unwrap();
        let kept = Kept {
            dir: dir.to_path_buf(),
            files: vec!["pkg/total_letronna_spec_test.go".to_owned()],
        };
        assert_eq!(test_names(&kept), ["TestTotal", "TestEmpty"]);
        // Asserting through the project's helper without t.Helper(), the
        // failure's line names the helper's file, and the test's name is
        // all that says whose it is.
        let helped = "--- FAIL: TestTotal (0.00s)\n    assert.go:45: expected 4, got 3\nFAIL\nFAIL\t\
                      github.com/acme/pkg\t0.01s";
        assert_eq!(own_failing(&kept, "go test ./pkg/", helped), Some(true));
        let others = "--- FAIL: TestEnvNoColor (0.00s)\n    assert.go:45: expected true\nFAIL";
        assert_eq!(own_failing(&kept, "go test ./pkg/", others), Some(false));
        // A name that opens another test's is not that test's: the
        // project's `TestEnvNoColor` is not the tester's `TestEnv`, nor its
        // `TestTotalRounding` the tester's `TestTotal`.
        std::fs::write(
            dir.join("pkg/env_letronna_spec_test.go"),
            "package pkg\n\nfunc TestEnv(t *testing.T) {}\n",
        )
        .unwrap();
        let named = Kept {
            dir: dir.to_path_buf(),
            files: vec!["pkg/env_letronna_spec_test.go".to_owned()],
        };
        assert_eq!(test_names(&named), ["TestEnv"]);
        assert_eq!(own_failing(&named, "go test ./pkg/", others), Some(false));
        let rounding = "--- FAIL: TestTotalRounding (0.00s)\n    assert.go:45: wrong\nFAIL";
        assert_eq!(own_failing(&kept, "go test ./pkg/", rounding), Some(false));
        for line in [
            "--- FAIL: TestTotal (0.00s)",
            "    --- FAIL: TestTotal/sub (0.00s)",
            "github.com/acme/pkg.TestTotal(0xc000106340)",
        ] {
            assert!(names_test(line, "TestTotal"), "{line}");
        }
        assert!(!names_test(
            "--- FAIL: TestTotalRounding (0.00s)",
            "TestTotal"
        ));
        assert!(!names_test("--- FAIL: TestEnvNoColor (0.00s)", "TestEnv"));
        assert!(!names_test("MyTestTotal(t)", "TestTotal"));
    }

    #[test]
    fn failures_the_project_had_as_it_was_are_told_from_ones_the_work_caused() {
        let bare = "--- FAIL: TestEnvNoColor (0.01s)\n    termenv_test.go:45: expected true\nFAIL\t\
                    github.com/acme/termenv\t0.4s";
        let same = "--- FAIL: TestEnvNoColor (0.00s)\n    termenv_test.go:45: expected true\nFAIL\t\
                    github.com/acme/termenv\t0.3s";
        assert!(already_failing(same, bare));
        let more = format!(
            "{same}\n--- FAIL: TestColorProfile (0.00s)\n    termenv_test.go:80: wrong\nFAIL"
        );
        assert!(!already_failing(&more, bare));
        // A sibling case the work newly breaks is a failure of its own,
        // though only a number tells it from one the project had.
        let table = |case: &str, took: &str| {
            format!(
                "--- FAIL: TestTable ({took})\n    --- FAIL: TestTable/case_{case} ({took})\nFAIL"
            )
        };
        assert!(already_failing(&table("1", "0.02s"), &table("1", "0.00s")));
        assert!(!already_failing(&table("2", "0.00s"), &table("1", "0.00s")));
        let parametrised = |case: &str| {
            format!(
                "FAILED tests/test_x.py::test_x[{case}] - assert 1 == 2\n1 failed in 0.4{case}s"
            )
        };
        assert!(already_failing(&parametrised("1"), &parametrised("1")));
        assert!(!already_failing(&parametrised("2"), &parametrised("1")));
        // Only the timings go.
        assert_eq!(
            untimed("--- FAIL: TestTotal (0.01s)"),
            "--- FAIL: TestTotal"
        );
        assert_eq!(untimed("    ✕ adds (12 ms)"), "✕ adds");
        assert_eq!(
            untimed("FAIL\tgithub.com/acme/termenv\t0.4s"),
            "FAIL github.com/acme/termenv"
        );
        assert_eq!(untimed("1 failed, 3 passed in 0.42s"), "1 failed, 3 passed");
        assert_eq!(
            untimed("FAILED tests/test_x.py::test_x[1] - assert 1 == 2"),
            "FAILED tests/test_x.py::test_x[1] - assert 1 == 2"
        );
        // Nothing attributed says nothing.
        assert!(!already_failing("panic: boom\n[exit code 2]", bare));
        let plain = "made.txt is missing\n[exit code 1]";
        assert!(!already_failing(plain, plain));
    }

    #[test]
    fn before_changing_behaviour_for_a_test_the_run_reads_the_words_for_that_case() {
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["tests/letronna_spec.test.ts".to_owned()],
        };
        // A narrower rule in the words wins; a review's row is a sign of
        // having read them right, not a licence of its own.
        assert!(
            WORDS_FIRST.contains(
                "where they set a narrower rule the test ignores, such as a filter, a condition \
                 or an exception, keep what your work does (where a review's row passed it, \
                 that is a sign you read the words right) and answer the test by quoting the \
                 words."
            ),
            "{WORDS_FIRST}"
        );
        let report = "R1 | adds | FAIL | npx vitest run x | got []\nRun: npx vitest run x";
        let failing = Ok("FAIL tests/letronna_spec.test.ts\n[exit code 1]".to_owned());
        let silent = "Where the words are silent on the case, say so in your answer.";
        let notices = [
            failed(&kept, "npx vitest run x", &failing).unwrap(),
            told(report, Some(&kept), true),
            unrun(report, &kept, true),
        ];
        for text in notices {
            assert!(text.contains(WORDS_FIRST), "{text}");
            assert_eq!(text.matches(WORDS_FIRST).count(), 1, "{text}");
            assert!(text.contains(silent), "{text}");
        }
        // Beside "put back what broke the test", silence says what to put back.
        let regressed = regressed("npx vitest run x", "exited with code 1", "FAIL", &[]);
        assert!(regressed.contains(WORDS_FIRST), "{regressed}");
        assert!(
            regressed.contains(
                "Where the words are silent on the case, put back what broke the test and say so."
            ),
            "{regressed}"
        );
        // Ahead of the work, the rule is about what to build.
        let ahead = told_ahead(report, Some(&kept));
        assert!(ahead.contains(WORDS_FIRST_AHEAD), "{ahead}");
        assert!(!ahead.contains(WORDS_FIRST), "{ahead}");
        assert!(
            WORDS_FIRST_AHEAD.starts_with("Before making your work do what a test expects"),
            "{WORDS_FIRST_AHEAD}"
        );
    }

    #[test]
    fn failures_in_tests_the_tester_did_not_write_are_said_to_be_such() {
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["color_letronna_spec_test.go".to_owned()],
        };
        let said = "The failures shown appear to be in tests the tester did not write, and may \
                    be the project's own as it was or this environment's: check whether they \
                    fail on the project as it was too, and say so where they do. Fix your work";
        let others = Ok(
            "--- FAIL: TestEnvNoColor (0.00s)\n    termenv_test.go:45: expected \
                        true\nFAIL\n[exit code 1]"
                .to_owned(),
        );
        let text = failed(&kept, "go test ./...", &others).unwrap();
        assert!(text.contains(said), "{text}");
        // Not of the tester's own failing, nor of output naming no failure.
        let own =
            Ok("--- FAIL: TestLetronnaSpecRestoresColor (0.00s)\nFAIL\n[exit code 1]".to_owned());
        let text = failed(&kept, "go test ./...", &own).unwrap();
        assert!(!text.contains("the tester did not write"), "{text}");
        let plain = Ok("made.txt is missing\n[exit code 1]".to_owned());
        let text = failed(&kept, "sh ./*_test.sh", &plain).unwrap();
        assert!(!text.contains("the tester did not write"), "{text}");
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
        assert!(
            was.to_string_lossy().contains("-harness-before-"),
            "{was:?}"
        );
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
        // A count is read as a number, not as words ending in `0 passed`.
        assert!(passed(&Ok("10 passed\n".into())));
        assert!(passed(&Ok("====== 20 passed in 3.1s ======\n".into())));
        assert!(passed(&Ok(
            "running 12 tests\ntest result: ok. 12 passed; 0 failed; 0 ignored\n\n   \
             Doc-tests cart\n\nrunning 0 tests\n\ntest result: ok. 0 passed; 0 failed\n"
                .into()
        )));
        assert!(!passed(&Ok("0 passed\n".into())));
        assert!(!passed(&Ok("collected 0 items\n".into())));
        assert!(!passed(&Ok("no tests ran in 0.01s\n".into())));
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
    fn a_failure_in_the_middle_of_a_long_log_reaches_the_notice() {
        let root = Scratch::new("spec-tests-long-log");
        let noise = "i=0; while [ $i -lt 3000 ]; do echo \"record $i not found\"; i=$((i+1)); done";
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            format!("{noise}\necho '--- FAIL: TestMiddle (0.00s)'\n{noise}\nexit 1\n"),
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(141, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        // Far past what a model's call keeps, and the line is none of those
        // the tool keeps from what it cuts.
        let ran = outcome_in(&kept, &command, &root, &ctx, TIMEOUT).unwrap();
        assert!(ran.as_deref().unwrap().len() > 10 * crate::tools::MAX_OUTPUT);
        let notice = failed(&kept, &command, &ran).unwrap();
        assert!(
            notice.contains("lines left out:\n\n…\n--- FAIL: TestMiddle (0.00s)\n"),
            "{notice}"
        );
        assert!(
            notice.contains("record 2999 not found\n\n[exit code 1]"),
            "{notice}"
        );
        tidy(141);
    }

    #[cfg(unix)]
    #[test]
    fn a_failure_past_megabytes_of_log_reaches_the_notice() {
        let root = Scratch::new("spec-tests-huge-log");
        // Past the 4 MiB a pipe keeps, so the run itself leaves a gap, and
        // the failure in the 3 MiB it keeps of the end.
        let noise = |lines: usize| format!("yes 'record not found in drifts' | head -n {lines}");
        std::fs::write(
            root.join("letronna_spec_test.sh"),
            format!(
                "{}\necho '--- FAIL: TestDeep (0.00s)'\n{}\nexit 1\n",
                noise(100_000),
                noise(70_000)
            ),
        )
        .unwrap();
        let command = "sh letronna_spec_test.sh".to_owned();
        let kept = keep(147, &root, &root, Some(command.clone())).unwrap();
        let ctx = ToolContext::of(&crate::chat::Connection {
            workspace: Some(root.to_path_buf()),
            ..Default::default()
        });
        let ran = outcome_in(&kept, &command, &root, &ctx, TIMEOUT).unwrap();
        let output = ran.as_deref().unwrap();
        assert!(output.len() > 2 * 1024 * 1024, "{}", output.len());
        assert!(output.contains(" left out]\n"));
        let notice = failed(&kept, &command, &ran).unwrap();
        assert!(
            notice.contains("\n--- FAIL: TestDeep (0.00s)\n"),
            "{notice}"
        );
        tidy(147);
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
            "FAIL pairs compose\nexpected [a, b] got []\n[exit code 1]\n\n\
             [It ran for 75 s. About 21 minutes of this run's time are left.]",
        );
        assert!(brief.starts_with("Make the change the request below asks for, from scratch"));
        // The run's time is not the helper's: the note of it is left out.
        assert!(
            brief.contains("got []\n[exit code 1]\n\nThat attempt"),
            "{brief}"
        );
        assert!(!brief.contains("It ran for"), "{brief}");
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
            dir.to_string_lossy().contains("-harness-copy-"),
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
            text.ends_with(&format!(
                "Put back what broke the test, unless the request's words side with your \
                 change. {} Then end with your whole answer again.",
                words_first("put back what broke the test and say so")
            )),
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
        assert!(text.contains("Its output:\n\nboom\n\n"), "{text}");
        let none = regressed("sh t.sh", "did not finish", "", &[]);
        assert!(
            none.contains("as it stands: none that a listing of the project shows. Put back"),
            "{none}"
        );
    }

    /// `environment`, with the words it found joined by a space.
    fn lent_words(line: &str, program: &str) -> Option<String> {
        environment(line, program).map(|found| found.join(" "))
    }

    #[cfg(unix)]
    #[test]
    fn the_set_up_of_the_runs_passing_command_is_read_off_its_line() {
        assert_eq!(
            lent_words(
                "GOMODCACHE=/root/go/pkg/mod GOCACHE=/tmp/gocache go test ./...",
                "go"
            )
            .as_deref(),
            Some("GOMODCACHE=/root/go/pkg/mod GOCACHE=/tmp/gocache")
        );
        // Exported or set on their own before the program, after a `cd`,
        // through `env`, and quoted.
        assert_eq!(
            lent_words(
                "export COVERAGE_FILE=.coverage; CI=1; cd /app && env \
                 DJANGO_SETTINGS_MODULE=\"app.settings\" pytest -q 2>&1 | tail -20",
                "pytest"
            )
            .as_deref(),
            Some("COVERAGE_FILE=.coverage CI=1 DJANGO_SETTINGS_MODULE=app.settings")
        );
        // Nothing after the program, and nothing from a line that sets
        // none, though each runs it.
        assert_eq!(
            lent_words("go test ./... && FOO=1 make check", "go").as_deref(),
            Some("")
        );
        assert_eq!(lent_words("pytest -q", "pytest").as_deref(), Some(""));
        // A secret, or a value with a word's end in it, is not lent.
        assert_eq!(
            lent_words(
                "API_KEY=x GITHUB_TOKEN=y CLIENT_SECRET=z DB_PASSWORD=w AUTH_HEADER=v C='a b' \
                 D=1 go test",
                "go"
            )
            .as_deref(),
            Some("D=1")
        );
        // Nor what says where programs and libraries are found, or how a
        // build goes.
        assert_eq!(
            lent_words(
                "PATH=/usr/local/go/bin HOME=/root LD_LIBRARY_PATH=/opt/lib GOFLAGS=-race \
                 CGO_ENABLED=0 NODE_ENV=test PYTHONHOME=/py GOMODCACHE=/m go test",
                "go"
            )
            .as_deref(),
            Some("GOMODCACHE=/m")
        );
        // Nor the import path, which the copy is put first on for the run.
        assert_eq!(
            lent_words("PYTHONPATH=/opt/x PYTHONSAFEPATH=1 D=1 pytest", "pytest").as_deref(),
            Some("D=1")
        );
        // Nothing at all off a line that never runs the tester's program,
        // however that is reached on it.
        assert_eq!(lent_words("GOMODCACHE=/m go build ./...", "pytest"), None);
        assert_eq!(
            lent_words("cd /app && GOMODCACHE=/m go build ./...", "go").as_deref(),
            Some("GOMODCACHE=/m")
        );
        // Nor anything off a command with a word made by running something,
        // or holding a variable this does not know.
        assert_eq!(
            lent_words("A=$(pwd) D=1 go test", "go").as_deref(),
            Some("")
        );
        assert_eq!(
            lent_words("B=$SOME_PLACE/x D=1 go test", "go").as_deref(),
            Some("")
        );
        // `LENT` at most.
        let many: Vec<String> = (0..LENT + 2).map(|at| format!("V{at}={at}")).collect();
        assert_eq!(
            lent_words(&format!("{} go test", many.join(" ")), "go"),
            Some(many[..LENT].join(" "))
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

    #[cfg(unix)]
    #[test]
    fn set_up_exported_past_other_programs_on_the_line_is_read_too() {
        // As a run writes it: a listing before the exports, and a pipe after
        // the program.
        assert_eq!(
            lent_words(
                "cd /app && ls zz_* 2>/dev/null; export GOMODCACHE=/root/go/pkg/mod \
                 GOFLAGS=-mod=mod && go test -tags analyze -run LetronnaSpec -v . 2>&1 | tail -40",
                "go"
            )
            .as_deref(),
            Some("GOMODCACHE=/root/go/pkg/mod GOFLAGS=-mod=mod")
        );
        // An export or a lone assignment stays set past the program before
        // it; a variable set before another program is gone with it.
        assert_eq!(
            lent_words(
                "rm -rf out; FOO=1 make; export BAR=2; BAZ=3; go test .",
                "go"
            )
            .as_deref(),
            Some("BAR=2 BAZ=3")
        );
        // The last command of the program on the line, with what was
        // exported before it; not what is exported after it.
        assert_eq!(
            lent_words(
                "go build ./... && export X=1 && GOMODCACHE=/m go test .",
                "go"
            )
            .as_deref(),
            Some("X=1 GOMODCACHE=/m")
        );
        assert_eq!(
            lent_words("go test .; export Y=2", "go").as_deref(),
            Some("")
        );
        // Nothing from a line that never runs the program.
        assert_eq!(lent_words("export GOMODCACHE=/m; ls", "go"), None);
    }

    #[cfg(unix)]
    #[test]
    fn go_flags_saying_where_modules_come_from_are_lent_and_build_flags_are_not() {
        assert_eq!(
            lent_words(
                "CFLAGS=-O2 LDFLAGS=-s RUSTFLAGS=-Dwarnings GOFLAGS=-mod=mod go test",
                "go"
            )
            .as_deref(),
            Some("GOFLAGS=-mod=mod")
        );
        for flag in ["-mod=vendor", "-mod=readonly", "-buildvcs=false"] {
            assert_eq!(
                lent_words(&format!("GOFLAGS={flag} go test"), "go"),
                Some(format!("GOFLAGS={flag}"))
            );
        }
        for value in ["-race", "-mod=mod,-race", "-ldflags=-s", ""] {
            assert_eq!(
                lent_words(&format!("GOFLAGS={value} go test"), "go").as_deref(),
                Some(""),
                "{value}"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_bare_line_of_the_program_counts_only_where_it_ran_what_the_tester_runs() {
        let go = "go test -tags analyze -run LetronnaSpec .";
        assert!(same_run(
            "cd /app && go test ./... 2>&1 | tail -20",
            go,
            "go"
        ));
        assert!(!same_run("go version", go, "go"));
        assert!(!same_run("go env GOMODCACHE", go, "go"));
        assert!(!same_run("go vet ./...", go, "go"));
        assert!(!same_run("cargo --version", "cargo test -p pest", "cargo"));
        assert!(!same_run("cargo build", "cargo test -p pest", "cargo"));
        assert!(same_run("cargo test", "cargo test -p pest", "cargo"));
        assert!(same_run("npm test", "npm test", "npm"));
        assert!(!same_run("npm run build", "npm test", "npm"));
        // A program without subcommands ran its tests unless it was asked
        // about itself.
        let pytest = "pytest -q tests/test_letronna_spec.py";
        assert!(same_run("pytest", pytest, "pytest"));
        assert!(same_run(
            "cd /app && pytest -q 2>&1 | tail -5",
            pytest,
            "pytest"
        ));
        assert!(!same_run("pytest --version", pytest, "pytest"));
        assert!(!same_run(
            "python --version",
            "python -m pytest tests/",
            "python"
        ));
        assert!(same_run(
            "python -m pytest -q",
            "python -m pytest tests/",
            "python"
        ));
        // Nothing where either does not run the program.
        assert!(!same_run("ls", go, "go"));
        assert!(!same_run("go test .", "pytest -q", "go"));
    }

    #[cfg(unix)]
    #[test]
    fn an_address_with_credentials_in_it_is_not_lent() {
        assert_eq!(
            lent_words(
                "GOPROXY=https://user:tok@goproxy.corp,direct GONOSUMDB=corp.example go test",
                "go"
            )
            .as_deref(),
            Some("GONOSUMDB=corp.example")
        );
        assert_eq!(
            lent_words(
                "PIP_INDEX_URL=https://u:p@pypi.corp/simple pytest",
                "pytest"
            )
            .as_deref(),
            Some("")
        );
        // One without them is, with the `@` a value may hold elsewhere.
        assert_eq!(
            lent_words(
                "GOPROXY=https://goproxy.corp,direct GOFLAGS=-mod=mod go test",
                "go"
            )
            .as_deref(),
            Some("GOPROXY=https://goproxy.corp,direct GOFLAGS=-mod=mod")
        );
        assert_eq!(
            lent_words("MAIL=user@example.com go test", "go").as_deref(),
            Some("MAIL=user@example.com")
        );
    }

    #[test]
    fn a_set_up_failure_is_told_from_a_commands_output() {
        assert!(set_up_failed(
            "go: downloading github.com/x/y v1.2.3\ngo: github.com/x/y@v1.2.3: Get \
             \"https://proxy.golang.org/github.com/x/y/@v/v1.2.3.mod\": dial tcp: lookup \
             proxy.golang.org on 127.0.0.11:53: server misbehaving\nFAIL\tapp [setup failed]"
        ));
        assert!(set_up_failed(
            "go: github.com/x/y@v1.2.3: module lookup disabled by GOPROXY=off"
        ));
        assert!(set_up_failed(
            "    Updating crates.io index\nwarning: spurious network error (3 tries remaining): \
             Could not resolve host: index.crates.io\nerror: failed to download from \
             `https://index.crates.io/config.json`"
        ));
        assert!(set_up_failed(
            "WARNING: Retrying (Retry(total=0)) after connection broken by \
             'NewConnectionError(...: Failed to establish a new connection: [Errno -3] \
             Temporary failure in name resolution)'\nERROR: No matching distribution found \
             for pytest"
        ));
        assert!(set_up_failed(
            "npm ERR! code ENOTFOUND\nnpm ERR! network request to \
             https://registry.npmjs.org/vitest failed, reason: getaddrinfo ENOTFOUND \
             registry.npmjs.org"
        ));
        assert!(!set_up_failed(
            "=== RUN   TestLetronnaSpec\n--- PASS: TestLetronnaSpec (0.00s)\nPASS\nok  \tapp\t0.012s"
        ));
        assert!(!set_up_failed("3 passed in 0.10s"));
    }
}
