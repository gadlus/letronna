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

use crate::cancel::Cancel;
use crate::tools::ToolContext;
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

/// Folders of installed packages and build caches, by name, that a tester
/// which must not see the work can have linked into the project as it was:
/// what tests import from or build with, and nothing the work writes, as a
/// `dist/` or `build/` it built, a `*.egg-info` it packaged or a
/// `__pycache__` of its new modules would be.
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

/// Characters of the tests' output the run is shown at most, from its end,
/// where a runner sums up what failed.
const SHOWN: usize = 3_000;

/// Characters a `Run:` line's command may hold at most: one line of shell,
/// not a script.
const LINE: usize = 400;

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
/// when it could not run or failed. It gets no input and may not prompt.
#[cfg(any(unix, test))]
fn git(dir: &Path, args: &[&str]) -> Option<Vec<String>> {
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
    let printed = String::from_utf8_lossy(&output.stdout);
    let lines = printed.split(['\0', '\n']).filter(|line| !line.is_empty());
    (output.status.success()).then(|| lines.map(str::to_owned).collect())
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
    let files = git(
        workspace,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
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

/// The project in `workspace` as its last commit holds it, as `before`
/// writes it out, with the package stores and build caches git ignores
/// there linked in (`stores`): a folder for a tester started with the run
/// to work in, which the work never reaches, where its tests can still be
/// parsed and their imports of the project's own code and packages
/// resolved. None where `workspace` is not the top of a git project, has no
/// commit yet, ignores more than `COPIED` entries, or where the folder
/// could not be made.
#[cfg(unix)]
pub(crate) fn base(workspace: &Path) -> Option<PathBuf> {
    top(workspace)?;
    let ignored = ignored(workspace)?;
    if ignored.len() > COPIED {
        return None;
    }
    let to = before(workspace)?;
    if link(workspace, stores(&ignored, &to), &to).is_none() {
        discard(&to);
        return None;
    }
    Some(to)
}

/// No tester starts with the run here, as `copy` makes no copy either.
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
/// project as it was, where there is one.
pub(crate) fn brief(request: &str, sentences: &[String], before: Option<&Path>) -> String {
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
    briefing(request, sentences, &opening, unnamed, outcome, rows)
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
    text.push_str(&format!(
        "\n\nEnd your report with the one command that runs all your tests from the project's \
         root, with paths relative to it, on a line of its own written `Run: <command>`: one \
         that runs them once and exits rather than watching, installs nothing, and needs no \
         file beyond your test files, so any fixture or helper they need goes in a file named \
         with `{MARKER}` too. Then a table of one row per test, written {rows}."
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
const INSTALLS: [&str; 23] = [
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
];

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
    if INSTALLS.iter().any(|verb| lower.contains(verb)) {
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
/// of it takes them; where no copy can be made they are not run.
pub(crate) fn ran(kept: &Kept, command: &str, ctx: &ToolContext, limit: Duration) -> Ran {
    let Some(workspace) = ctx.workspace.as_deref() else {
        return Ran::Unrun("the run has no project folder".into());
    };
    let Some(copy) = copy(workspace) else {
        return Ran::Unrun("no copy of the project could be made to run them in".into());
    };
    let placed = kept.files.iter().all(|file| {
        let at = copy.join(file);
        (at.parent()).is_some_and(|parent| std::fs::create_dir_all(parent).is_ok())
            && std::fs::copy(kept.dir.join(file), &at).is_ok()
    });
    if !placed {
        discard(&copy);
        return Ran::Unrun(format!(
            "the test files kept in `{}` could not be put back in a copy of the project",
            kept.dir.display()
        ));
    }
    let outcome = run_command(command, &copy, ctx, limit);
    discard(&copy);
    match failed(kept, command, &outcome) {
        Some(notice) => Ran::Failed(notice),
        None => Ran::Passed(command.to_owned()),
    }
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
    let args = serde_json::json!({"command": command, "timeout": limit.as_secs().max(1)});
    crate::tools::run("run_command", &args.to_string(), &ctx)
}

/// The notice sending the run back with `outcome`, what `command` made of
/// the tester's tests `kept` run against its work; None when they passed.
fn failed(kept: &Kept, command: &str, outcome: &Result<String, String>) -> Option<String> {
    let (ended, output) = match outcome {
        // A command that passed has no exit code line; a note of where the
        // whole output went, or what the user's hooks said, can follow one.
        Ok(output) => {
            let code = (output.lines().rev()).find_map(|line| line.strip_prefix("[exit code "))?;
            let code = code.trim_end_matches(']');
            (format!("exited with code {code}"), output.as_str())
        }
        // Without the tool's advice to call it again with a longer timeout,
        // which is for a model that made the call.
        Err(why) => {
            let why = why.split(" If it only needs longer").next().unwrap_or(why);
            ("did not finish".to_owned(), why)
        }
    };
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
/// still be writing there. The paths it deleted from the project, in order.
pub(crate) fn tidy(run: u64) -> Vec<String> {
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
    let mut walk = ignore::WalkBuilder::new(workspace);
    walk.hidden(false);
    walk.filter_entry(|entry| !SKIPPED.iter().any(|skipped| entry.file_name() == *skipped));
    let mut found: Vec<String> = (walk.build().take(WALKED))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| (entry.file_name().to_str()).is_some_and(|name| name.contains(MARKER)))
        .filter_map(|entry| {
            let inside = entry.path().strip_prefix(workspace).ok()?;
            let parts: Vec<_> = inside.iter().map(|part| part.to_string_lossy()).collect();
            Some(parts.join("/"))
        })
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
    tell(
        "A tester that hasn't seen your work wrote tests from the request alone while you \
         worked, in the project as it was before your change, where none could pass yet; its \
         report follows, ending with a table of its tests, whose rows say NOT RUN or FAIL for \
         want of your change, not what your work does. Make your work do what each test \
         expects where the request's words say so; where a test expects something the words \
         do not say, quote the words that show it in your answer.",
        report,
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
        assert!(brief.ends_with("what it expected and what it got`."));
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
        let brief = brief("Save at /api/items/save.", &[], Some(Path::new("/tmp/was")));
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
        let brief = brief("Save at /api/items/save.", &[], None);
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
        let brief = brief("Save at /api/items/save.", &[], None);
        assert!(brief.contains("written `Run: <command>`"), "{brief}");
        assert!(
            brief.contains("from the project's root, with paths relative to it"),
            "{brief}"
        );
        assert!(
            brief.contains("runs them once and exits rather than watching, installs nothing"),
            "{brief}"
        );
        assert!(brief.ends_with("what it expected and what it got`."));
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
        let Ran::Failed(text) = ran(&kept, &command, &ctx, TIMEOUT) else {
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
            ran(&kept, &command, &ctx, TIMEOUT),
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
            ran(&kept, &command, &ctx, TIMEOUT),
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
        let Ran::Failed(text) = ran(&kept, &command, &ctx, Duration::from_secs(1)) else {
            panic!("the command finished");
        };
        assert!(
            text.contains("`sh letronna_spec_slow.sh` did not finish"),
            "{text}"
        );
        assert!(text.contains("Timed out after 1s"), "{text}");
        // Without the tool's advice to a model to call it again.
        assert!(!text.contains("If it only needs longer"), "{text}");
        tidy(7);
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
            brief.ends_with("not yet implemented`: none can pass before the change, and one that does tests nothing of it."),
            "{brief}"
        );
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
        let report = "Run: npx vitest run\nR1 | adds | FAIL | npx vitest run | not yet implemented";
        let text = told_ahead(report, Some(&kept));
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
        assert!(text.ends_with(report));
        let bare = told_ahead(report, None);
        assert!(!bare.contains("kept out of the project"), "{bare}");
    }
}
