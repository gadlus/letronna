//! Git worktrees that helpers and sessions work in, so agents writing side by
//! side do not trip over each other or over the user's own edits.
//!
//! Everything goes through the git CLI: it is the git the user has, it knows
//! every repository layout, and a helper costs a handful of calls. A worktree
//! is `<top>/.letronna/worktrees/<slug>` on branch `letronna/<slug>`, made
//! from the caller's HEAD, so the user's uncommitted edits are not in it.
//! `<top>` is always the main worktree's, so one made from a helper's
//! worktree is not inside it and cannot go when that one goes. Only that
//! folder is excluded, through the exclude file git names, which is the right
//! one even from a worktree or a submodule; the rest of `.letronna` holds
//! agent types a project commits. git runs with no hooks and no fsmonitor, so
//! no program the repository names runs unasked.
//!
//! A worktree goes, with its branch, only when no job its helper started still
//! runs and two probes -- its commits past its base and a `status` that shows
//! untracked, ignored and submodule changes whatever the config says -- both
//! answer and both show nothing but ignored build output. Anything else keeps
//! it, since a wrong guess that way loses a helper's work. A record read back
//! from disk is used only when it names a place `create` could have made, so
//! a sidecar cannot point git anywhere else.
//!
//! A session's work goes back to the checkout it was made from as one patch,
//! never as a merge: its commits, edits and new files since its base, staged
//! in an index of its own so its real one is left alone. A file the user has
//! not edited there is merged three ways, as `git apply --3way` does, which
//! copes with commits the user made meanwhile; one they have edited is laid
//! onto their copy, keeping their edits. Every file is tried first on a copy
//! of the index or with `--check`, and nothing is written while any file
//! conflicts; nothing is committed there either way.

use crate::store::Conversation;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashSet;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// How long one git call may take: far past any local one, and short enough
/// that a git stuck on a lock or a hook does not hold a helper for good.
const LIMIT: Duration = Duration::from_secs(30);
/// How often a running git is looked at: a local call takes a few of these.
const TICK: Duration = Duration::from_millis(10);
/// The longest slug made from a name: room for a helper's name, well inside
/// Windows' path limit once the repository's own path comes before it.
const SLUG_CHARS: usize = 48;
/// Numbered slugs tried when a name's is taken, as many as a session's
/// helpers could plausibly leave behind.
const TRIES: u32 = 100;
/// Where worktrees go under a repository's top folder, as git is told it.
const FOLDER: &str = ".letronna/worktrees";
/// The exclude line keeping them out of the user's `git status`.
const EXCLUDE: &str = "/.letronna/worktrees/";
/// Folders build tools and package managers fill and fill again on their own,
/// so ignored files in them are no helper's work to keep.
const BUILT: [&str; 7] = [
    "target",
    "node_modules",
    "dist",
    "build",
    ".venv",
    "__pycache__",
    ".next",
];
/// How many files or commits a kept line or an error names: enough to know
/// them by, few enough for one line of a report.
const NAMED: usize = 5;
/// Config every git call here runs with, so no program the repository names
/// runs: no hooks (nothing is ever below the null device, and `<` is no name
/// on Windows) and no fsmonitor, and `status` takes no optional lock another
/// git may be waiting on. read-only-commands (#144) has the same idea in its
/// `GUARDS`; this is the part these calls need.
const GUARDS: [(&str, &str); 6] = [
    ("GIT_CONFIG_COUNT", "2"),
    ("GIT_CONFIG_KEY_0", "core.hooksPath"),
    ("GIT_CONFIG_VALUE_0", "/dev/null/<no hooks>"),
    ("GIT_CONFIG_KEY_1", "core.fsmonitor"),
    ("GIT_CONFIG_VALUE_1", "false"),
    ("GIT_OPTIONAL_LOCKS", "0"),
];

/// Taken by every call that changes a repository's worktrees, branches or
/// exclude file: git holds a lock for those, and a second one at once fails.
static CHANGING: Mutex<()> = Mutex::new(());

/// A worktree a helper or a session works in, as its sidecar or its session
/// file keeps it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: String,
    /// The commit it was made from, past which its own commits are counted.
    pub base: String,
    /// Where its session's folder is inside it, as git names it: empty at the
    /// top, else ending in a slash.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub prefix: String,
    /// The folder it was made from, which its work is applied back to; none
    /// in a record from before records kept it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkout: Option<PathBuf>,
}

/// What `status` found in a worktree.
struct Status {
    /// Whatever is uncommitted -- a change, a new file or a submodule's -- as
    /// git names it.
    changed: Vec<String>,
    /// Ignored files and folders outside build output, as git names them.
    ignored: Vec<String>,
}

/// What bringing a worktree's work back to its checkout finds, by the files'
/// paths in the repository.
#[derive(Debug, Default, PartialEq)]
pub struct Preflight {
    /// The top folder of the checkout it goes into.
    pub into: PathBuf,
    /// Files whose changes go in as the checkout is now.
    pub clean: Vec<String>,
    /// Files whose changes clash with the checkout's own copy.
    pub conflicts: Vec<String>,
}

/// How a file's changes go into the checkout.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Way {
    /// Merged three ways from the copy the change was made to, which takes
    /// commits made there since but needs the copy unedited.
    Merged,
    /// Laid onto the copy there, edits and all, where the lines around each
    /// change still read as they did.
    Laid,
}

/// Whether an `isolation` option asks for a worktree, refusing one that
/// names anything else.
pub fn asked(isolation: &Value) -> Result<bool, String> {
    match isolation {
        Value::Null => Ok(false),
        Value::String(kind) if kind == "worktree" => Ok(true),
        other => Err(format!(
            "isolation {other} is not one there is: the only one is \"worktree\"."
        )),
    }
}

/// Why no worktree can be made from `workspace` for want of anything to make
/// one from: no project folder, a folder in no git repository (or no git to
/// ask), or a repository with no commit yet. `None` when `create` can be
/// tried, whose own refusals are of another kind: a repository whose main
/// folder git does not name, too many worktrees left, a path too long.
pub fn nowhere(workspace: Option<&Path>) -> Option<String> {
    let Some(workspace) = workspace else {
        return Some("no project folder is attached".into());
    };
    if git(workspace, &["rev-parse", "--git-dir"]).is_err() {
        return Some(format!(
            "{} is not in a git repository",
            workspace.display()
        ));
    }
    git(workspace, &["rev-parse", "--verify", "HEAD"])
        .is_err()
        .then(|| "its repository has no commit yet".to_owned())
}

/// Makes a worktree named for `name` in the repository `workspace` is in,
/// from its HEAD and on a branch of its own; a name already taken gets the
/// first free number after it.
pub fn create(workspace: &Path, name: &str) -> Result<Worktree, String> {
    let (top, prefix) = main_top(workspace)?;
    let base = git(workspace, &["rev-parse", "--verify", "HEAD"])
        .map_err(|_| "A worktree starts from a commit, and this repository has none yet.")?;
    let _changing = changing();
    unlinked(&top)?;
    exclude(&top)?;
    let slug = slug(name);
    for n in 1..=TRIES {
        let name = match n {
            1 => slug.clone(),
            n => format!("{slug}-{n}"),
        };
        let (folder, branch) = (format!("{FOLDER}/{name}"), format!("letronna/{name}"));
        let path = native(&top.join(FOLDER).join(&name));
        if path.exists() || has_branch(&top, &branch) {
            continue;
        }
        // The branch first, which git makes only for the first to ask, so
        // another Letronna on this repository looking at the same moment
        // moves on to the next number instead of taking this folder.
        if let Err(error) = git(&top, &["branch", &branch, &base]) {
            if has_branch(&top, &branch) {
                continue;
            }
            return Err(error);
        }
        if let Err(error) = git(&top, &["worktree", "add", &folder, &branch]) {
            undo(&top, &folder, Some(&branch));
            return Err(long_paths(error));
        }
        return Ok(Worktree {
            path,
            branch,
            base,
            prefix,
            checkout: Some(native(workspace)),
        });
    }
    Err(format!(
        "{TRIES} worktrees named {slug} are left in {}; remove some first.",
        top.join(FOLDER).display()
    ))
}

/// Moves `session` into a worktree of its own named for `name`, made from its
/// folder's HEAD, so it works beside the user's checkout instead of in it.
pub fn isolate(session: &mut Conversation, name: &str) -> Result<(), String> {
    let Some(folder) = session.workspace.as_deref() else {
        return Err("A worktree needs a project folder, and none is attached.".into());
    };
    let made = create(folder, name)?;
    session.workspace = Some(made.workspace());
    session.worktree = Some(made);
    Ok(())
}

/// Points `session` back at its worktree, made again when its folder was
/// removed, unless it was moved to another folder since.
pub fn resume(session: &mut Conversation) -> Result<(), String> {
    let Some(tree) = session.worktree.clone() else {
        return Ok(());
    };
    let inside = (session.workspace.as_ref()).is_some_and(|folder| folder.starts_with(&tree.path));
    if !inside {
        return Ok(());
    }
    let checkout = tree.checkout.clone();
    let again = tree.reopen(checkout.as_deref())?;
    session.workspace = Some(again.workspace());
    session.worktree = Some(again);
    Ok(())
}

/// What a model working in `workspace` is told when that is in a worktree
/// Letronna made: read from the path alone, so it holds still all session.
pub fn note(workspace: &Path) -> Option<String> {
    let tree = (workspace.ancestors()).find(|folder| {
        folder
            .parent()
            .is_some_and(|parent| parent.ends_with(FOLDER))
    })?;
    let name = tree.file_name()?.to_str()?;
    (slug(name) == name).then(|| {
        format!(
            "You work in git worktree `{}` on branch `letronna/{name}`, made from HEAD; the \
             user's uncommitted edits and ignored files such as .env, node_modules and target \
             are not in it, and your changes stay here until the user applies them to their \
             checkout.",
            tree.display()
        )
    })
}

/// The top folder of the main worktree of the repository `workspace` is in,
/// and git's prefix for `workspace` inside its own worktree.
fn main_top(workspace: &Path) -> Result<(PathBuf, String), String> {
    let asked = [
        "rev-parse",
        "--path-format=absolute",
        "--git-dir",
        "--git-common-dir",
        "--show-toplevel",
        "--show-prefix",
    ];
    let outside = || {
        format!(
            "A worktree needs a git repository, and {} is not in one.",
            workspace.display()
        )
    };
    let answer = git(workspace, &asked).map_err(|_| outside())?;
    let mut lines = answer.lines();
    let (Some(own), Some(common), Some(top)) = (lines.next(), lines.next(), lines.next()) else {
        return Err(outside());
    };
    // An empty prefix is the last line, which the trim took.
    let prefix = lines.next().unwrap_or_default().to_owned();
    let common = Path::new(common);
    // What a normal repository's worktrees share is the main one's `.git`; a
    // submodule's or a separate git folder names its worktree only from it.
    let main = match common.parent() {
        Some(main) if common.ends_with(".git") => main.to_path_buf(),
        _ if Path::new(own) == common => PathBuf::from(top),
        _ => {
            return Err(format!(
                "{} is in a worktree of a repository whose main folder git does not name, so \
                 no worktree is made from it; open the main folder instead.",
                workspace.display()
            ));
        }
    };
    Ok((native(&main), prefix))
}

/// `path` with the system's own separators: git names folders with `/` on
/// Windows too, which would read mixed with the ones joined on.
fn native(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Refuses a `.letronna` or `.letronna/worktrees` in `top` that is a link or
/// leads out of it: a repository can commit one, and git would put the
/// checkout, and the helper's writes, wherever it points.
fn unlinked(top: &Path) -> Result<(), String> {
    let unreadable = |path: &Path, e| format!("Could not look at {}: {e}", path.display());
    let root = std::fs::canonicalize(top).map_err(|e| unreadable(top, e))?;
    let mut folder = top.to_path_buf();
    for part in [".letronna", "worktrees"] {
        folder.push(part);
        let linked = match std::fs::symlink_metadata(&folder) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(e) => return Err(unreadable(&folder, e)),
            Ok(meta) => meta.file_type().is_symlink(),
        };
        let inside = std::fs::canonicalize(&folder).is_ok_and(|real| real.starts_with(&root));
        if linked || !inside {
            return Err(format!(
                "{} is a link or leads out of the repository, so no worktree is made there.",
                folder.display()
            ));
        }
    }
    Ok(())
}

/// `error` with a way out added when it is Windows' path limit, which a
/// worktree's longer paths reach before the project's own.
fn long_paths(error: String) -> String {
    if error.contains("Filename too long") {
        format!("{error} (setting git's core.longpaths to true lifts the limit)")
    } else {
        error
    }
}

/// Takes away what a `worktree add` that failed part way made, which holds
/// nothing of a helper's yet and would push the next one to a new number,
/// with the `branch` made for it, if one was.
fn undo(top: &Path, folder: &str, branch: Option<&str>) {
    // Twice forced, since git locks a worktree while it is being made.
    let _ = git(top, &["worktree", "remove", "--force", "--force", folder]);
    let _ = git(top, &["worktree", "prune"]);
    if let Some(branch) = branch {
        let _ = git(top, &["branch", "-D", branch]);
    }
}

/// A name made safe for a folder and a branch: lower-case letters, digits and
/// single dashes, never empty, never long and never a Windows device's.
pub fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            slug.push(c.to_ascii_lowercase());
        } else if !slug.is_empty() && !slug.ends_with('-') {
            slug.push('-');
        }
    }
    // Only ASCII went in, so any byte count is a character boundary.
    slug.truncate(SLUG_CHARS);
    match slug.trim_end_matches('-') {
        "" => "helper".into(),
        slug if device(slug) => format!("{slug}-helper"),
        slug => slug.into(),
    }
}

/// Whether `slug` is a name Windows keeps for a device, which no file or
/// folder there can have.
fn device(slug: &str) -> bool {
    let numbered = |stem| {
        (slug.strip_prefix(stem))
            .is_some_and(|n| n.len() == 1 && n.bytes().all(|b| b.is_ascii_digit()))
    };
    ["con", "prn", "aux", "nul"].contains(&slug) || numbered("com") || numbered("lpt")
}

impl Worktree {
    /// Removes it and its branch when nothing was done in it and none of
    /// `jobs` still runs, giving None, or keeps both and gives the line saying
    /// where they are and why.
    pub fn finish(&self, jobs: &[u64]) -> Option<String> {
        let Some((top, name)) = self.place() else {
            return Some(format!(
                "Its worktree record names no worktree Letronna makes, so {} was left alone.",
                self.path.display()
            ));
        };
        // Started in it, a job may write there yet.
        if jobs.iter().any(|&id| crate::jobs::running(id) != Ok(false)) {
            return Some(format!(
                "Its worktree is kept at {} on branch {} (jobs still running).",
                self.path.display(),
                self.branch
            ));
        }
        let theirs = format!("refs/heads/{}", self.branch);
        let commits = git(
            &self.path,
            &["rev-list", "--count", "HEAD", &theirs, "--not", &self.base],
        )
        .ok()
        .and_then(|count| count.parse::<u64>().ok());
        let status = status(&self.path);
        let stashed = stashed(&self.path, &self.branch);
        let clean =
            matches!(&status, Some(found) if found.changed.is_empty() && found.ignored.is_empty());
        if commits != Some(0) || !clean || stashed != Some(0) {
            return Some(self.kept(commits, status.as_ref(), stashed));
        }
        let _changing = changing();
        if let Err(error) = git(&top, &["worktree", "remove", &format!("{FOLDER}/{name}")]) {
            return Some(format!(
                "Its worktree changed nothing but could not be removed ({error}); it is at {} \
                 on branch {}.",
                self.path.display(),
                self.branch
            ));
        }
        if let Err(error) = git(&top, &["branch", "-D", &self.branch]) {
            return Some(format!(
                "Its worktree changed nothing and was removed, but its branch {} is left: {error}",
                self.branch
            ));
        }
        None
    }

    /// Itself when its folder is still there or its branch can be checked out
    /// there again, else a new worktree in its place made from HEAD as it is
    /// now; only ever in the repository `workspace` is in.
    pub fn reopen(self, workspace: Option<&Path>) -> Result<Self, String> {
        let main = workspace.and_then(|workspace| main_top(workspace).ok());
        let place = (self.place()).filter(|(top, _)| main.is_some_and(|(main, _)| main == *top));
        let Some((top, name)) = place else {
            return Err(format!(
                "{} is not a worktree Letronna makes for this project, so nothing will work in \
                 it.",
                self.path.display()
            ));
        };
        if self.path.is_dir() {
            return Ok(self);
        }
        // As a review leaves it: the worktree removed, the branch kept to merge.
        if has_branch(&top, &self.branch) {
            self.check_out_again(&top, &format!("{FOLDER}/{name}"))?;
            return Ok(self);
        }
        let (prefix, checkout) = (self.prefix, self.checkout);
        create(&top, &name).map(|made| Self {
            prefix,
            checkout,
            ..made
        })
    }

    /// Checks its branch out at `folder` of `top` again, commits and all.
    fn check_out_again(&self, top: &Path, folder: &str) -> Result<(), String> {
        let _changing = changing();
        unlinked(top)?;
        exclude(top)?;
        // One whose folder was deleted by hand is on git's list until pruned.
        let _ = git(top, &["worktree", "prune"]);
        git(top, &["worktree", "add", folder, &self.branch]).map_err(|error| {
            undo(top, folder, None);
            format!(
                "Its branch {} could not be checked out again: {}",
                self.branch,
                long_paths(error)
            )
        })?;
        Ok(())
    }

    /// The folder its helper works in: the one its session's folder is in the
    /// checkout, when the checkout has that as a real folder, else the top.
    pub fn workspace(&self) -> PathBuf {
        let mut inside = self.path.clone();
        for part in Path::new(&self.prefix).components() {
            let Component::Normal(part) = part else {
                return self.path.clone();
            };
            inside.push(part);
            // A link the checkout holds would take the helper out of it.
            if !std::fs::symlink_metadata(&inside).is_ok_and(|meta| meta.is_dir()) {
                return self.path.clone();
            }
        }
        inside
    }

    /// The line saying its helper worked at the top of the worktree, when that
    /// is the folder it was `given` in place of its session's.
    pub fn at_top(&self, given: &Path) -> Option<String> {
        let wanted = self.prefix.trim_end_matches('/');
        (!wanted.is_empty() && given == self.path).then(|| {
            format!(
                "Its session's folder {wanted} is not a folder in the commit its worktree was \
                 made from, so it worked at the worktree's top."
            )
        })
    }

    /// What applying its work to the checkout it was made from would do,
    /// writing nothing.
    pub fn preflight(&self) -> Result<Preflight, String> {
        self.bring_back(false)
    }

    /// Applies its work to the checkout it was made from, staging what it
    /// merges there as `git apply --3way` does and committing nothing, unless
    /// a file conflicts, when nothing is written at all.
    pub fn apply(&self) -> Result<Preflight, String> {
        self.bring_back(true)
    }

    /// Removes it and its branch: at once when nothing was done in it or all
    /// of it is in its checkout already, else only when the user is `sure`,
    /// having seen the error naming what would go.
    pub fn discard(&self, sure: bool) -> Result<(), String> {
        let Some((top, name)) = self.place() else {
            return Err(format!(
                "Its worktree record names no worktree Letronna makes, so {} was left alone.",
                self.path.display()
            ));
        };
        let mut force = sure;
        if !sure {
            let range = format!("{}..refs/heads/{}", self.base, self.branch);
            let asked = ["log", "--no-show-signature", "--format=%s", range.as_str()];
            let commits: Option<Vec<String>> = (git(&top, &asked).ok()).map(|log| {
                log.lines()
                    .map(|subject| format!("\"{subject}\""))
                    .collect()
            });
            // A folder already gone holds nothing more.
            let status = match self.path.exists() {
                true => status(&self.path),
                false => Some(Status {
                    changed: Vec::new(),
                    ignored: Vec::new(),
                }),
            };
            // Ignored files are in no patch, so applying never took them;
            // stash entries outlive it either way.
            let risked = match (&commits, &status) {
                (Some(commits), Some(found)) => {
                    let done = commits.is_empty() && found.changed.is_empty();
                    !found.ignored.is_empty() || !(done || self.applied())
                }
                _ => true,
            };
            if risked {
                return Err(self.unsaved(commits.as_deref(), status.as_ref()));
            }
            // Edits its checkout has now are all git would keep it for.
            force = status.is_some_and(|found| !found.changed.is_empty());
        }
        self.remove(&top, &name, force)
    }

    /// What bringing its work back finds and, with `write`, the work brought
    /// back.
    fn bring_back(&self, write: bool) -> Result<Preflight, String> {
        let into = self.into()?;
        let (diff, index, trial) = (
            Scratch::new("patch"),
            Scratch::new("index"),
            Scratch::new("trial"),
        );
        let files = self.diff(&diff.0, &index.0)?;
        let patch = named(&diff.0)?;
        if write {
            // A copy only touched since reads as edited to `apply --3way`
            // until its index entry is refreshed; the exit says some were.
            let _ = git(&into, &["update-index", "-q", "--refresh"]);
        }
        let edited = unstaged(&into)?;
        let mut found = Preflight {
            into: into.clone(),
            ..Default::default()
        };
        let mut ways = Vec::new();
        for way in [Way::Merged, Way::Laid] {
            let (group, others): (Vec<&String>, Vec<&String>) =
                (files.iter()).partition(|file| edited.contains(*file) == (way == Way::Laid));
            if group.is_empty() {
                continue;
            }
            // Named by the edited files, which are few, whichever way; the
            // whole patch's names could pass what a command line holds.
            let filter: Vec<String> = match way {
                _ if others.is_empty() => Vec::new(),
                Way::Laid => group.iter().map(|file| only("--include", file)).collect(),
                Way::Merged => others.iter().map(|file| only("--exclude", file)).collect(),
            };
            if fits(&into, patch, way, &filter, &trial)? {
                found.clean.extend(group.into_iter().cloned());
            } else {
                for file in group {
                    let alone = [only("--include", file)];
                    match fits(&into, patch, way, &alone, &trial)? {
                        true => found.clean.push(file.clone()),
                        false => found.conflicts.push(file.clone()),
                    }
                }
            }
            ways.push((way, filter));
        }
        found.clean.sort();
        found.conflicts.sort();
        if !write {
            return Ok(found);
        }
        if !found.conflicts.is_empty() {
            return Err(format!(
                "Nothing was applied, since these files conflict with {}: {}. Ask the session to \
                 bring its branch up to date with the checkout's, or apply it with git.",
                into.display(),
                few(&found.conflicts)
            ));
        }
        for (way, filter) in ways {
            let mut args = match way {
                Way::Merged => vec!["apply", "--3way"],
                Way::Laid => vec!["apply"],
            };
            args.extend(filter.iter().map(String::as_str));
            args.push(patch);
            git(&into, &args)?;
        }
        Ok(found)
    }

    /// Writes everything done in it since its base -- commits, edits and new
    /// files, nothing ignored -- to `patch` as one diff, staged in the
    /// scratch index `index`, giving the files it touches.
    fn diff(&self, patch: &Path, index: &Path) -> Result<Vec<String>, String> {
        std::fs::copy(index_of(&self.path)?, index)
            .map_err(|e| format!("Could not copy the worktree's index: {e}"))?;
        let staged = |args: &[&str]| git_on(&self.path, args, Some(index));
        staged(&["add", "--all"])?;
        let output = format!("--output={}", named(patch)?);
        // The plumbing diff, which no diff config, colour or external tool
        // the user set can turn into something `apply` cannot read.
        let base = self.base.as_str();
        staged(&[
            "diff-index",
            "--cached",
            "--patch",
            "--binary",
            &output,
            base,
        ])?;
        let names = staged(&["diff-index", "--cached", "--name-only", "-z", base])?;
        Ok((names.split('\0').filter(|name| !name.is_empty()))
            .map(str::to_owned)
            .collect())
    }

    /// The top folder of the checkout it was made from, while that is still
    /// a checkout of the repository it was made in.
    fn into(&self) -> Result<PathBuf, String> {
        let gone = || {
            "The folder its worktree was made from is gone or in another repository now, so its \
             work has nowhere to go."
                .to_owned()
        };
        let (Some((top, _)), Some(checkout)) = (self.place(), self.checkout.as_deref()) else {
            return Err(gone());
        };
        if main_top(checkout).ok().is_none_or(|(main, _)| main != top) {
            return Err(gone());
        }
        let into = git(checkout, &["rev-parse", "--show-toplevel"]).map_err(|_| gone())?;
        Ok(native(Path::new(&into)))
    }

    /// Whether all its work is in its checkout already, as `apply` leaves it:
    /// taking it back out of there would fit.
    fn applied(&self) -> bool {
        let (diff, index) = (Scratch::new("patch"), Scratch::new("index"));
        let (Ok(into), Ok(files), Ok(patch)) =
            (self.into(), self.diff(&diff.0, &index.0), named(&diff.0))
        else {
            return false;
        };
        !files.is_empty() && git(&into, &["apply", "--check", "--reverse", patch]).is_ok()
    }

    /// The error naming what removing it would lose: its `commits`, as their
    /// subjects, and what `status` found uncommitted or ignored, None where
    /// git could not tell.
    fn unsaved(&self, commits: Option<&[String]>, status: Option<&Status>) -> String {
        let mut lost = Vec::new();
        match commits {
            None => lost.push("commits that could not be counted".to_owned()),
            Some([]) => {}
            Some(one @ [_]) => lost.push(format!("1 commit ({})", few(one))),
            Some(all) => lost.push(format!("{} commits ({})", all.len(), few(all))),
        }
        match status {
            None => lost.push("changes that could not be looked at".to_owned()),
            Some(found) => {
                if !found.changed.is_empty() {
                    lost.push(format!("uncommitted changes to {}", few(&found.changed)));
                }
                if !found.ignored.is_empty() {
                    lost.push(format!("ignored files {}", few(&found.ignored)));
                }
            }
        }
        format!(
            "Its worktree at {} on branch {} has {}. Discarding it deletes them.",
            self.path.display(),
            self.branch,
            lost.join("; ")
        )
    }

    /// Removes it and its branch, and with `force` the uncommitted work in
    /// it, which git otherwise keeps it for.
    fn remove(&self, top: &Path, name: &str, force: bool) -> Result<(), String> {
        let _changing = changing();
        if self.path.exists() {
            let folder = format!("{FOLDER}/{name}");
            let mut args = vec!["worktree", "remove", folder.as_str()];
            if force {
                args.insert(2, "--force");
            }
            git(top, &args)?;
        }
        // One whose folder was deleted by hand is on git's list until pruned.
        let _ = git(top, &["worktree", "prune"]);
        if has_branch(top, &self.branch) {
            git(top, &["branch", "-D", &self.branch])?;
        }
        Ok(())
    }

    /// The repository's top folder and the slug, when this names a place
    /// `create` could have made.
    fn place(&self) -> Option<(PathBuf, String)> {
        let name = self.path.file_name()?.to_str()?;
        let folder = self.path.parent()?;
        let top = folder.parent()?.parent()?;
        let fits = self.path.is_absolute()
            && folder.ends_with(FOLDER)
            && name.starts_with(|c: char| c.is_ascii_alphanumeric())
            && (name.chars()).all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
            && self.branch == format!("letronna/{name}")
            && !self.base.is_empty()
            && self.base.chars().all(|c| c.is_ascii_hexdigit())
            && (Path::new(&self.prefix).components()).all(|c| matches!(c, Component::Normal(_)));
        fits.then(|| (top.to_path_buf(), name.to_owned()))
    }

    /// The line saying it was kept, from what the probes found, None where
    /// one could not tell.
    fn kept(
        &self,
        commits: Option<u64>,
        status: Option<&Status>,
        stashed: Option<usize>,
    ) -> String {
        let commits = match commits {
            Some(0) => "no commits".to_owned(),
            Some(1) => "1 commit".to_owned(),
            Some(n) => format!("{n} commits"),
            None => "commits unknown".to_owned(),
        };
        let mut dirty = match status {
            Some(found) if !found.changed.is_empty() => "uncommitted changes".to_owned(),
            Some(_) => "nothing uncommitted".to_owned(),
            None => "uncommitted changes unknown".to_owned(),
        };
        match stashed {
            Some(0) => {}
            Some(1) => dirty.push_str(", 1 stash entry"),
            Some(n) => dirty.push_str(&format!(", {n} stash entries")),
            None => dirty.push_str(", stash entries unknown"),
        }
        let ignored = status.map_or(&[][..], |found| &found.ignored);
        if !ignored.is_empty() {
            dirty.push_str(", ignored files: ");
            dirty.push_str(&ignored[..ignored.len().min(NAMED)].join(", "));
        }
        if ignored.len() > NAMED {
            dirty.push_str(&format!(" and {} more", ignored.len() - NAMED));
        }
        format!(
            "Its worktree is kept at {} on branch {} ({commits}, {dirty}).",
            self.path.display(),
            self.branch
        )
    }
}

/// Adds the line keeping worktrees out of `git status` to the exclude file
/// git names for `top`, unless it is there already.
fn exclude(top: &Path) -> Result<(), String> {
    let named = git(top, &["rev-parse", "--git-path", "info/exclude"])?;
    // Relative to where git ran, unless git gave it whole.
    let file = top.join(named);
    let text = match std::fs::read(&file) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("Could not read {}: {e}", file.display())),
    };
    if text.lines().any(|line| line.trim() == EXCLUDE) {
        return Ok(());
    }
    let mut line = String::new();
    if !text.is_empty() && !text.ends_with('\n') {
        line.push('\n');
    }
    line.push_str(EXCLUDE);
    line.push('\n');
    file.parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&file)
        })
        .and_then(|mut out| out.write_all(line.as_bytes()))
        .map_err(|e| format!("Could not keep worktrees out of git status: {e}"))
}

/// Whether the repository at `top` has a branch called `branch`.
fn has_branch(top: &Path, branch: &str) -> bool {
    let full = format!("refs/heads/{branch}");
    git(top, &["rev-parse", "--verify", "--quiet", &full]).is_ok()
}

/// What is uncommitted or ignored in the worktree at `path`, whatever the
/// repository's config would hide, or None when git could not tell.
fn status(path: &Path) -> Option<Status> {
    let asked = [
        "status",
        "--porcelain",
        "-z",
        "--untracked-files=all",
        "--ignore-submodules=none",
        // An ignored folder as a whole, not each of the files in a `target`.
        "--ignored=matching",
    ];
    let answer = git(path, &asked).ok()?;
    let mut found = Status {
        changed: Vec::new(),
        ignored: Vec::new(),
    };
    let mut entries = answer.split('\0');
    while let Some(entry) = entries.next() {
        match (entry.get(..2), entry.get(3..)) {
            (Some("!!"), Some(name)) if !built(name) => found.ignored.push(name.to_owned()),
            (Some("!!"), _) | (None, _) => {}
            (Some(code), name) => {
                found.changed.push(name.unwrap_or(entry).to_owned());
                // A rename or copy is followed by the name it had.
                if code.starts_with(['R', 'C']) {
                    entries.next();
                }
            }
        }
    }
    Some(found)
}

/// How many stash entries were made on `branch`: `git stash` in a worktree
/// puts them in the stash all worktrees share, where they would outlive it.
fn stashed(path: &Path, branch: &str) -> Option<usize> {
    let list = git(path, &["stash", "list", "--format=%gs"]).ok()?;
    let (made, named) = (format!("WIP on {branch}:"), format!("On {branch}:"));
    let on_branch = |line: &&str| line.starts_with(&made) || line.starts_with(&named);
    Some(list.lines().filter(on_branch).count())
}

/// Whether the ignored `path` git named is in a build output folder; git ends
/// a folder's name with a slash, so only a file's own name is no folder.
fn built(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts.iter().any(|part| BUILT.contains(part))
}

/// Whether the changes to the files `filter` picks from `patch` go into the
/// checkout at `into` the `way` given, tried so nothing is written: merged
/// into a copy of its index at `trial`, or laid on with `--check`.
fn fits(
    into: &Path,
    patch: &str,
    way: Way,
    filter: &[String],
    trial: &Scratch,
) -> Result<bool, String> {
    let mut args = vec!["apply"];
    args.extend(filter.iter().map(String::as_str));
    if way == Way::Laid {
        args.extend(["--check", patch]);
        return Ok(git(into, &args).is_ok());
    }
    // Afresh each time, as a try leaves what it merged in the copy.
    std::fs::copy(index_of(into)?, &trial.0)
        .map_err(|e| format!("Could not copy the checkout's index: {e}"))?;
    args.extend(["--3way", "--cached", patch]);
    Ok(git_on(into, &args, Some(&trial.0)).is_ok())
}

/// The files in the checkout at `top` whose copy is not as its index has
/// it -- edited, deleted or new -- by their paths in the repository.
fn unstaged(top: &Path) -> Result<HashSet<String>, String> {
    let asked = [
        "status",
        "--porcelain",
        "-z",
        "--untracked-files=all",
        "--no-renames",
    ];
    let answer = git(top, &asked)?;
    // The second letter is the copy against the index.
    Ok((answer.split('\0'))
        .filter(|entry| entry.get(1..2).is_some_and(|copy| copy != " "))
        .filter_map(|entry| entry.get(3..))
        .map(str::to_owned)
        .collect())
}

/// The index file of the worktree at `dir`.
fn index_of(dir: &Path) -> Result<PathBuf, String> {
    // Relative to where git ran, unless git gave it whole.
    git(dir, &["rev-parse", "--git-path", "index"]).map(|named| dir.join(named))
}

/// `path` as git is given it, which takes only Unicode.
fn named(path: &Path) -> Result<&str, String> {
    (path.to_str()).ok_or_else(|| format!("git cannot be given the path {}.", path.display()))
}

/// The `flag` option of `git apply` picking `file` alone: its pattern with
/// the characters a pattern reads escaped.
fn only(flag: &str, file: &str) -> String {
    let mut option = format!("{flag}=");
    for c in file.chars() {
        if matches!(c, '*' | '?' | '[' | '\\') {
            option.push('\\');
        }
        option.push(c);
    }
    option
}

/// The first of `names`, enough to know them by, and how many more there are.
fn few(names: &[String]) -> String {
    let mut said = names[..names.len().min(NAMED)].join(", ");
    if names.len() > NAMED {
        said.push_str(&format!(" and {} more", names.len() - NAMED));
    }
    said
}

/// A file of its own in the temporary folder, gone with this.
struct Scratch(PathBuf);

impl Scratch {
    /// A new one, its name ending in `kind`.
    fn new(kind: &str) -> Self {
        static MADE: AtomicU64 = AtomicU64::new(0);
        let n = MADE.fetch_add(1, Ordering::Relaxed);
        let name = format!("letronna-worktree-{}-{n}.{kind}", std::process::id());
        Self(std::env::temp_dir().join(name))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
        // Left by a git that was ended while it had the file open.
        let mut lock = self.0.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(lock);
    }
}

/// The hold on changing worktrees, which a panic while holding it leaves usable.
fn changing() -> MutexGuard<'static, ()> {
    CHANGING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs git in `dir` and gives what it printed, trimmed, or what went wrong.
/// It can never wait on the user: it gets no input and may not prompt.
fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    git_on(dir, args, None)
}

/// `git`, on the index file `index` in place of the repository's own when
/// given one.
fn git_on(dir: &Path, args: &[&str], index: Option<&Path>) -> Result<String, String> {
    let what = args.first().copied().unwrap_or_default();
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .envs(GUARDS)
        // Set by a hook or a shell Letronna started from, these would choose
        // the repository in place of `-C`.
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_COMMON_DIR")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match index {
        Some(index) => command.env("GIT_INDEX_FILE", index),
        None => command.env_remove("GIT_INDEX_FILE"),
    };
    // Letronna owns no console, and Windows would open one for every call.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    // A group of its own, so the timeout also ends what git started.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("Could not run git: {e}"))?;
    // Read as it comes, so a full pipe cannot stall git before it exits.
    let out = child.stdout.take().map(drain);
    let err = child.stderr.take().map(drain);
    let deadline = Instant::now() + LIMIT;
    let late = || format!("git {what} did not finish in {} s.", LIMIT.as_secs());
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(TICK),
            _ => {
                end(&mut child);
                return Err(late());
            }
        }
    };
    // A program git started can hold its pipes past its exit; that is not
    // waited for past the deadline.
    let read = |pipe: Option<Receiver<String>>| match pipe {
        Some(pipe) => (pipe.recv_timeout(deadline.saturating_duration_since(Instant::now()))).ok(),
        None => Some(String::new()),
    };
    let (Some(out), Some(err)) = (read(out), read(err)) else {
        return Err(late());
    };
    if !status.success() {
        return Err(format!("git {what} failed: {}", err.trim()));
    }
    Ok(out.trim_end().to_owned())
}

/// Kills a git that ran too long and, on Unix, every program in its group,
/// which could hold its pipes or the repository's locks after it; on Windows
/// only git goes.
fn end(child: &mut Child) {
    #[cfg(unix)]
    if let Ok(group) = libc::pid_t::try_from(child.id()) {
        // SAFETY: kill reads no memory. git leads the group, as it was made
        // with process_group(0), and is not waited for yet, so its id is
        // still no one else's.
        unsafe { libc::kill(-group, libc::SIGKILL) };
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Reads all of `pipe` on a thread of its own, lossily, since a name that is
/// not UTF-8 must still show a worktree is not empty, and sends it on once
/// the pipe ends.
fn drain(mut pipe: impl Read + Send + 'static) -> Receiver<String> {
    let (send, receive) = mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = pipe.read_to_end(&mut bytes);
        let _ = send.send(String::from_utf8_lossy(&bytes).into_owned());
    });
    receive
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Makes `dir` a repository whose one commit holds what is in it, and a
    /// readme so there is something.
    /// `path` as the paths made here have it: real, and on Windows without
    /// the verbatim prefix, which git's paths lack.
    pub(crate) fn real(path: &Path) -> PathBuf {
        let real = std::fs::canonicalize(path).unwrap();
        let shown = real.to_string_lossy();
        PathBuf::from(shown.strip_prefix(r"\\?\").unwrap_or(&shown))
    }

    pub(crate) fn repo(dir: &Path) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join("readme.txt"), "a repository").unwrap();
        git(dir, &["init", "-q"]).unwrap();
        commit(dir, "First");
    }

    /// Commits everything in `dir`, as a made-up author.
    pub(crate) fn commit(dir: &Path, message: &str) {
        git(dir, &["add", "-A"]).unwrap();
        let (who, mail) = ("user.name=tester", "user.email=tester@example.invalid");
        let unsigned = "commit.gpgsign=false";
        let args = [
            "-c", who, "-c", mail, "-c", unsigned, "commit", "-q", "-m", message,
        ];
        git(dir, &args).unwrap();
    }

    /// An empty folder of the test's own, as git names it.
    fn folder(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-worktree-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Without Windows' verbatim `\\?\` prefix, which git's paths lack.
        let real = std::fs::canonicalize(dir).unwrap();
        let shown = real.to_string_lossy();
        PathBuf::from(shown.strip_prefix(r"\\?\").unwrap_or(&shown))
    }

    /// A repository of the test's own.
    fn fresh(name: &str) -> PathBuf {
        let dir = folder(name).join("repo");
        repo(&dir);
        dir
    }

    #[test]
    fn a_name_becomes_a_slug_safe_for_a_folder_and_a_branch() {
        assert_eq!(slug("Explore_2"), "explore-2");
        assert_eq!(slug("../../etc/passwd"), "etc-passwd");
        assert_eq!(slug("wf-3-12"), "wf-3-12");
        assert_eq!(slug("CON"), "con-helper");
        assert_eq!(slug("lpt9"), "lpt9-helper");
        assert_eq!(slug("com12"), "com12");
        assert_eq!(slug("console"), "console");
        assert_eq!(slug("  --  "), "helper");
        assert_eq!(slug("ünïcode only"), "n-code-only");
        assert_eq!(slug(&"a".repeat(80)).len(), SLUG_CHARS);
        assert_eq!(
            slug(&format!("{}-b", "a".repeat(SLUG_CHARS - 1))),
            "a".repeat(47)
        );
    }

    #[test]
    fn the_exclude_goes_where_git_says_and_only_once() {
        let repo = fresh("exclude");
        let other = repo.parent().unwrap().join("other");
        git(&repo, &["worktree", "add", "-q", "../other"]).unwrap();
        // From a linked worktree, git names the exclude file of the main one.
        exclude(&other).unwrap();
        exclude(&other).unwrap();
        create(&repo, "explore").unwrap();

        let text = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
        assert_eq!(
            text.lines().filter(|line| *line == EXCLUDE).count(),
            1,
            "{text}"
        );
        assert!(!other.join(".git").is_dir());
        assert_eq!(git(&repo, &["status", "--porcelain"]), Ok(String::new()));
    }

    #[test]
    fn an_untouched_worktree_goes_with_its_branch() {
        let repo = fresh("untouched");
        let made = create(&repo, "Explore").unwrap();

        assert_eq!(made.path, repo.join(".letronna/worktrees/explore"));
        assert_eq!(made.branch, "letronna/explore");
        assert_eq!(made.base, git(&repo, &["rev-parse", "HEAD"]).unwrap());
        assert!(made.path.join("readme.txt").is_file());
        assert_eq!(made.finish(&[]), None);
        assert!(!made.path.exists());
        assert!(!has_branch(&repo, &made.branch));
    }

    #[test]
    fn a_worktree_with_a_commit_is_kept() {
        let repo = fresh("committed");
        let made = create(&repo, "general").unwrap();
        std::fs::write(made.path.join("new.txt"), "work").unwrap();
        commit(&made.path, "Work");

        let kept = made.finish(&[]).unwrap();
        assert_eq!(
            kept,
            format!(
                "Its worktree is kept at {} on branch letronna/general (1 commit, nothing \
                 uncommitted).",
                made.path.display()
            )
        );
        assert!(made.path.join("new.txt").is_file());
        assert!(has_branch(&repo, "letronna/general"));
    }

    #[test]
    fn a_worktree_with_uncommitted_changes_is_kept() {
        let repo = fresh("dirty");
        let made = create(&repo, "general").unwrap();
        std::fs::write(made.path.join("readme.txt"), "changed").unwrap();

        let kept = made.finish(&[]).unwrap();
        assert!(
            kept.ends_with("(no commits, uncommitted changes)."),
            "{kept}"
        );
        assert!(made.path.is_dir());
    }

    #[test]
    fn a_failed_probe_keeps_it_and_says_unknown() {
        let repo = fresh("unknown");
        let mut made = create(&repo, "general").unwrap();
        // A base git does not have fails the count.
        made.base = "0".repeat(40);

        let kept = made.finish(&[]).unwrap();
        assert!(
            kept.ends_with("(commits unknown, nothing uncommitted)."),
            "{kept}"
        );
        assert!(made.path.is_dir());
        assert!(has_branch(&repo, "letronna/general"));
    }

    #[test]
    fn a_taken_name_gets_the_next_number() {
        let repo = fresh("taken");
        let first = create(&repo, "general").unwrap();
        let second = create(&repo, "general").unwrap();

        assert_eq!(second.path, repo.join(".letronna/worktrees/general-2"));
        assert_eq!(second.branch, "letronna/general-2");
        assert!(first.path.is_dir() && second.path.is_dir());
    }

    #[test]
    fn a_record_letronna_did_not_make_is_left_alone() {
        let repo = fresh("forged");
        let base = git(&repo, &["rev-parse", "HEAD"]).unwrap();
        let forged = Worktree {
            path: repo.clone(),
            branch: "letronna/repo".into(),
            base,
            prefix: String::new(),
            checkout: Some(repo.clone()),
        };
        let mut climbing = create(&repo, "general").unwrap();
        climbing.prefix = "../../..".into();

        let said = forged.finish(&[]).unwrap();
        assert!(said.ends_with("was left alone."), "{said}");
        assert!(forged.reopen(Some(&repo)).is_err());
        assert!(repo.join("readme.txt").is_file());
        let said = climbing.finish(&[]).unwrap();
        assert!(said.ends_with("was left alone."), "{said}");
        assert_eq!(climbing.workspace(), climbing.path);
        assert!(climbing.reopen(Some(&repo)).is_err());
    }

    #[test]
    fn a_record_is_reopened_only_in_its_own_sessions_repository() {
        let repo = fresh("own-repository");
        let other = fresh("other-repository");
        let made = create(&repo, "general").unwrap();
        assert_eq!(made.finish(&[]), None);

        assert!(made.clone().reopen(Some(&other)).is_err());
        assert!(made.clone().reopen(None).is_err());
        assert!(!other.join(FOLDER).exists());
        assert!(made.reopen(Some(&repo)).is_ok());
    }

    #[test]
    fn a_removed_worktree_is_made_again_when_its_helper_carries_on() {
        let repo = fresh("reopened");
        let made = create(&repo, "general").unwrap();
        let still = made.clone().reopen(Some(&repo)).unwrap();
        assert_eq!(still, made);
        assert_eq!(made.finish(&[]), None);

        let again = made.clone().reopen(Some(&repo)).unwrap();
        assert_eq!(again.path, made.path);
        assert!(again.path.join("readme.txt").is_file());
    }

    #[test]
    fn a_worktree_removed_for_review_comes_back_with_its_branchs_commits() {
        let repo = fresh("reviewed");
        let made = create(&repo, "general").unwrap();
        std::fs::write(made.path.join("done.txt"), "done").unwrap();
        commit(&made.path, "Done");
        git(
            &repo,
            &["worktree", "remove", ".letronna/worktrees/general"],
        )
        .unwrap();

        let again = made.clone().reopen(Some(&repo)).unwrap();
        assert_eq!(again, made);
        assert!(made.path.join("done.txt").is_file());
        let said = made.finish(&[]).unwrap();
        assert!(said.contains("(1 commit, nothing uncommitted)"), "{said}");
    }

    #[test]
    fn a_stash_made_in_the_worktree_keeps_it() {
        let repo = fresh("stashed");
        let made = create(&repo, "general").unwrap();
        std::fs::write(made.path.join("readme.txt"), "half done").unwrap();
        let (who, mail) = ("user.name=tester", "user.email=tester@example.invalid");
        git(&made.path, &["-c", who, "-c", mail, "stash", "-q"]).unwrap();

        let said = made.finish(&[]).unwrap();
        assert!(
            said.contains("(no commits, nothing uncommitted, 1 stash entry)"),
            "{said}"
        );
        assert!(made.path.is_dir());
    }

    #[test]
    fn a_folder_in_no_repository_is_refused() {
        let dir = folder("no-repo");

        let refused = create(&dir, "general").unwrap_err();
        assert!(
            refused.starts_with("A worktree needs a git repository"),
            "{refused}"
        );
        assert!(!dir.join(".letronna").exists());
    }

    #[test]
    fn new_files_keep_the_worktree_when_config_hides_untracked_files() {
        let repo = fresh("hidden-untracked");
        git(&repo, &["config", "status.showUntrackedFiles", "no"]).unwrap();
        let made = create(&repo, "general").unwrap();
        std::fs::write(made.path.join("new.rs"), "work").unwrap();

        let kept = made.finish(&[]).unwrap();
        assert!(
            kept.ends_with("(no commits, uncommitted changes)."),
            "{kept}"
        );
        assert!(made.path.join("new.rs").is_file());
    }

    #[test]
    fn a_worktree_made_from_a_helpers_goes_under_the_main_one_from_its_commit() {
        let repo = fresh("from-helper");
        let first = create(&repo, "first").unwrap();
        std::fs::write(first.path.join("new.txt"), "work").unwrap();
        commit(&first.path, "Work");
        let second = create(&first.path, "second").unwrap();

        assert_eq!(second.path, repo.join(".letronna/worktrees/second"));
        let head = git(&first.path, &["rev-parse", "HEAD"]).unwrap();
        assert_eq!(second.base, head);
        assert!(second.path.join("new.txt").is_file());
    }

    #[test]
    fn a_kept_worktree_outlives_the_one_it_was_made_from() {
        let repo = fresh("outlives");
        let first = create(&repo, "first").unwrap();
        let second = create(&first.path, "second").unwrap();
        std::fs::write(second.path.join("wip.txt"), "work").unwrap();

        assert!(second.finish(&[]).is_some());
        assert_eq!(first.finish(&[]), None);
        assert!(second.path.join("wip.txt").is_file());
    }

    #[test]
    fn ignored_files_keep_the_worktree_and_are_named() {
        let repo = fresh("ignored");
        std::fs::write(repo.join(".gitignore"), "*.log\n.env\ntarget/\n").unwrap();
        commit(&repo, "Ignore");
        let made = create(&repo, "general").unwrap();
        std::fs::write(made.path.join("findings.log"), "work").unwrap();
        std::fs::write(made.path.join(".env"), "KEY=1").unwrap();
        std::fs::create_dir_all(made.path.join("target/debug")).unwrap();
        std::fs::write(made.path.join("target/debug/app"), "built").unwrap();

        let kept = made.finish(&[]).unwrap();
        let named = "(no commits, nothing uncommitted, ignored files: .env, findings.log).";
        assert!(kept.ends_with(named), "{kept}");
        assert!(made.path.join("findings.log").is_file());
    }

    #[test]
    fn ignored_build_output_alone_lets_the_worktree_go() {
        let repo = fresh("built");
        std::fs::write(repo.join(".gitignore"), "target/\nnode_modules/\n*.pyc\n").unwrap();
        commit(&repo, "Ignore");
        let made = create(&repo, "general").unwrap();
        for file in [
            "target/debug/app",
            "web/node_modules/x/i.js",
            "tool/__pycache__/a.pyc",
        ] {
            let path = made.path.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "built").unwrap();
        }

        assert_eq!(made.finish(&[]), None);
        assert!(!made.path.exists());
    }

    #[test]
    fn a_job_still_running_keeps_the_worktree() {
        let repo = fresh("job");
        let made = create(&repo, "general").unwrap();
        let command = if cfg!(windows) {
            "Start-Sleep -Seconds 30"
        } else {
            "sleep 30"
        };
        let job = crate::jobs::start(command, &made.path).unwrap();

        let kept = made.finish(&[job]);
        let _ = crate::jobs::stop(job);
        let kept = kept.unwrap();
        assert!(kept.ends_with("(jobs still running)."), "{kept}");
        assert!(made.path.is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn no_hook_or_fsmonitor_the_repository_names_runs() {
        use std::os::unix::fs::PermissionsExt;
        let repo = fresh("guards");
        let marker = repo.parent().unwrap().join("ran");
        let program = repo.parent().unwrap().join("program");
        let body = format!("#!/bin/sh\necho ran >> '{}'\n", marker.display());
        std::fs::write(&program, body).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::create_dir_all(repo.join(".git/hooks")).unwrap();
        std::fs::copy(&program, repo.join(".git/hooks/post-checkout")).unwrap();
        let named = program.to_str().unwrap();
        git(&repo, &["config", "core.fsmonitor", named]).unwrap();

        let made = create(&repo, "general").unwrap();
        assert_eq!(made.finish(&[]), None);
        assert!(!marker.exists());
        // Without the guards the same repository runs it.
        let plain = Command::new("git")
            .arg("-C")
            .arg(&repo)
            .arg("status")
            .output();
        assert!(plain.unwrap().status.success());
        assert!(marker.exists());
    }

    #[test]
    fn a_worktree_that_fails_part_way_leaves_nothing_behind() {
        let repo = fresh("failed-add");
        std::fs::write(repo.join(".gitattributes"), "*.txt filter=broken\n").unwrap();
        commit(&repo, "Filter");
        // A checkout through a filter that always fails stops part way.
        git(&repo, &["config", "filter.broken.smudge", "false"]).unwrap();
        git(&repo, &["config", "filter.broken.required", "true"]).unwrap();

        assert!(create(&repo, "general").is_err());
        assert!(!repo.join(".letronna/worktrees/general").exists());
        assert!(!has_branch(&repo, "letronna/general"));
        let listed = git(&repo, &["worktree", "list", "--porcelain"]).unwrap();
        assert_eq!(listed.matches("worktree ").count(), 1, "{listed}");
    }

    #[test]
    fn a_half_made_worktree_is_taken_away_with_its_branch() {
        let repo = fresh("half-made");
        // Locked, as git leaves one it was killed while making.
        let (folder, branch) = (".letronna/worktrees/general", "letronna/general");
        let add = ["worktree", "add", "-q", "--lock", "-b", branch, folder];
        git(&repo, &add).unwrap();

        undo(&repo, folder, Some(branch));
        assert!(!repo.join(folder).exists());
        assert!(!has_branch(&repo, branch));
        assert_eq!(create(&repo, "general").unwrap().branch, branch);
    }

    #[test]
    fn a_session_in_a_subfolder_works_in_the_same_subfolder_of_the_worktree() {
        let repo = fresh("subfolder");
        std::fs::create_dir_all(repo.join("apps/web")).unwrap();
        std::fs::write(repo.join("apps/web/page.txt"), "a page").unwrap();
        commit(&repo, "Web");
        std::fs::create_dir_all(repo.join("apps/new")).unwrap();

        let made = create(&repo.join("apps/web"), "web").unwrap();
        assert_eq!(made.prefix, "apps/web/");
        assert_eq!(made.workspace(), made.path.join("apps/web"));
        // A folder only the user's copy has is not in the checkout.
        let untracked = create(&repo.join("apps/new"), "new").unwrap();
        assert_eq!(untracked.workspace(), untracked.path);
        let said = untracked.at_top(&untracked.workspace()).unwrap();
        assert!(
            said.starts_with("Its session's folder apps/new is not"),
            "{said}"
        );
        assert_eq!(made.at_top(&made.workspace()), None);
        // Made again, it lands in the same place.
        assert_eq!(made.finish(&[]), None);
        let again = made.clone().reopen(Some(&repo.join("apps/web"))).unwrap();
        assert_eq!(again.workspace(), made.path.join("apps/web"));
    }

    #[cfg(unix)]
    #[test]
    fn a_letronna_folder_that_is_a_link_is_refused() {
        let repo = fresh("linked");
        let outside = repo.parent().unwrap().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(".letronna")).unwrap();

        let refused = create(&repo, "general").unwrap_err();
        assert!(refused.contains("is a link or leads out"), "{refused}");
        std::fs::remove_file(repo.join(".letronna")).unwrap();
        std::fs::create_dir_all(repo.join(".letronna")).unwrap();
        std::os::unix::fs::symlink(&outside, repo.join(".letronna/worktrees")).unwrap();
        let refused = create(&repo, "general").unwrap_err();
        assert!(refused.contains("is a link or leads out"), "{refused}");
        assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn a_git_past_its_time_goes_with_what_it_started() {
        use std::io::BufRead;
        use std::os::unix::process::CommandExt;
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & echo $!; wait"]);
        let mut child = command
            .stdout(Stdio::piped())
            .process_group(0)
            .spawn()
            .unwrap();
        let mut started = String::new();
        let out = child.stdout.take().unwrap();
        std::io::BufReader::new(out)
            .read_line(&mut started)
            .unwrap();

        end(&mut child);
        let alive = || {
            let mut probe = Command::new("kill");
            probe.args(["-0", started.trim()]).stderr(Stdio::null());
            probe.status().unwrap().success()
        };
        // Gone once whoever takes in orphans has waited for it.
        let deadline = Instant::now() + Duration::from_secs(5);
        while alive() && Instant::now() < deadline {
            std::thread::sleep(TICK);
        }
        assert!(!alive(), "sleep {} outlived its group", started.trim());
    }

    /// Lines 1 to 30, one number a line, so a test can change one far from
    /// another.
    fn lines() -> String {
        (1..=30).map(|n| format!("{n}\n")).collect()
    }

    /// Writes `text` over line `n`, from 1, of `file`.
    fn reword(file: &Path, n: usize, text: &str) {
        let old = std::fs::read_to_string(file).unwrap();
        let new: String = (old.lines().enumerate())
            .map(|(at, line)| format!("{}\n", if at + 1 == n { text } else { line }))
            .collect();
        std::fs::write(file, new).unwrap();
    }

    /// A repository whose `list.txt` holds `lines`, and a session's
    /// worktree made from it.
    fn listed(name: &str) -> (PathBuf, Worktree) {
        let repo = fresh(name);
        std::fs::write(repo.join("list.txt"), lines()).unwrap();
        commit(&repo, "List");
        let made = create(&repo, "session").unwrap();
        (repo, made)
    }

    #[test]
    fn applying_onto_a_dirty_checkout_keeps_the_users_edits() {
        let (repo, made) = listed("apply-dirty");
        reword(&made.path.join("list.txt"), 2, "two");
        commit(&made.path, "Two");
        reword(&made.path.join("list.txt"), 28, "twenty-eight");
        std::fs::write(made.path.join("new.txt"), "new").unwrap();
        // The user carries on in their checkout meanwhile, in the same file.
        reword(&repo.join("list.txt"), 15, "fifteen");
        std::fs::write(repo.join("readme.txt"), "the user's").unwrap();
        let head = git(&repo, &["rev-parse", "HEAD"]).unwrap();

        let found = made.preflight().unwrap();
        assert_eq!(found.into, repo);
        assert_eq!(found.clean, ["list.txt", "new.txt"]);
        assert!(found.conflicts.is_empty(), "{found:?}");
        assert_eq!(made.apply(), Ok(found));
        let list = std::fs::read_to_string(repo.join("list.txt")).unwrap();
        let words: Vec<&str> = (list.lines())
            .filter(|line| line.parse::<u32>().is_err())
            .collect();
        assert_eq!(words, ["two", "fifteen", "twenty-eight"]);
        let readme = std::fs::read_to_string(repo.join("readme.txt")).unwrap();
        assert_eq!(readme, "the user's");
        assert!(repo.join("new.txt").is_file());
        assert_eq!(git(&repo, &["rev-parse", "HEAD"]), Ok(head));
    }

    #[test]
    fn a_conflicting_hunk_is_reported_and_nothing_written() {
        let (repo, made) = listed("apply-conflict");
        reword(&made.path.join("list.txt"), 2, "two");
        std::fs::write(made.path.join("new.txt"), "new").unwrap();
        // The user changed the same line and committed it.
        reword(&repo.join("list.txt"), 2, "TWO");
        commit(&repo, "Two");

        let found = made.preflight().unwrap();
        assert_eq!(found.clean, ["new.txt"]);
        assert_eq!(found.conflicts, ["list.txt"]);
        let refused = made.apply().unwrap_err();
        assert!(
            refused.starts_with("Nothing was applied, since these files conflict with"),
            "{refused}"
        );
        assert!(refused.contains(": list.txt."), "{refused}");
        assert!(!repo.join("new.txt").exists());
        assert_eq!(git(&repo, &["status", "--porcelain"]), Ok(String::new()));
    }

    #[test]
    fn a_change_near_one_the_user_committed_since_is_merged_in() {
        let (repo, made) = listed("apply-merged");
        reword(&made.path.join("list.txt"), 10, "ten");
        // Among the lines around the session's change, which laying it on
        // would need as they were.
        reword(&repo.join("list.txt"), 12, "twelve");
        commit(&repo, "Twelve");

        assert_eq!(made.apply().unwrap().clean, ["list.txt"]);
        // A checkout with core.autocrlf, as on Windows, writes CRLF.
        let list = std::fs::read_to_string(repo.join("list.txt"))
            .unwrap()
            .replace("\r\n", "\n");
        assert!(list.contains("9\nten\n11\ntwelve\n13\n"), "{list}");
    }

    #[test]
    fn discarding_a_worktree_with_commits_needs_confirmation() {
        let repo = fresh("discard");
        let made = create(&repo, "session").unwrap();
        std::fs::write(made.path.join("done.txt"), "done").unwrap();
        commit(&made.path, "Done");
        std::fs::write(made.path.join("half.txt"), "half").unwrap();

        let asked = made.discard(false).unwrap_err();
        let named = format!(
            "Its worktree at {} on branch letronna/session has 1 commit (\"Done\"); uncommitted \
             changes to half.txt. Discarding it deletes them.",
            made.path.display()
        );
        assert_eq!(asked, named);
        assert!(made.path.join("done.txt").is_file());
        assert!(has_branch(&repo, "letronna/session"));
        assert_eq!(made.discard(true), Ok(()));
        assert!(!made.path.exists());
        assert!(!has_branch(&repo, "letronna/session"));
    }

    #[test]
    fn a_worktree_whose_work_was_applied_goes_without_asking() {
        let repo = fresh("discard-applied");
        let made = create(&repo, "session").unwrap();
        std::fs::write(made.path.join("done.txt"), "done").unwrap();
        commit(&made.path, "Done");
        made.apply().unwrap();

        assert_eq!(made.discard(false), Ok(()));
        assert!(!made.path.exists());
        assert!(repo.join("done.txt").is_file());
    }

    #[test]
    fn a_worktree_session_comes_back_after_its_folder_was_removed() {
        let repo = fresh("session-back");
        let mut session = Conversation {
            workspace: Some(repo.clone()),
            ..Default::default()
        };
        isolate(&mut session, "session-7").unwrap();
        let tree = session.worktree.clone().unwrap();
        assert_eq!(tree.path, repo.join(".letronna/worktrees/session-7"));
        assert_eq!(session.workspace.as_ref(), Some(&tree.path));
        assert_eq!(tree.checkout.as_ref(), Some(&repo));
        std::fs::remove_dir_all(&tree.path).unwrap();

        resume(&mut session).unwrap();
        assert_eq!(session.worktree.as_ref(), Some(&tree));
        assert_eq!(session.workspace.as_ref(), Some(&tree.path));
        assert!(tree.path.join("readme.txt").is_file());
        // Moved to another folder since, it has left its worktree.
        std::fs::remove_dir_all(&tree.path).unwrap();
        session.workspace = Some(repo.clone());
        resume(&mut session).unwrap();
        assert_eq!(session.workspace, Some(repo));
        assert!(!tree.path.exists());
    }

    #[test]
    fn a_model_in_a_worktree_is_told_so_from_its_path_alone() {
        let top = Path::new("/work/.letronna/worktrees/session-3");
        let said = note(&top.join("apps").join("web")).unwrap();
        let start = format!(
            "You work in git worktree `{}` on branch `letronna/session-3`, made from HEAD;",
            top.display()
        );
        assert!(said.starts_with(&start), "{said}");
        assert_eq!(note(Path::new("/work/apps/web")), None);
        assert_eq!(note(Path::new("/work/.letronna/worktrees/Not Made")), None);
    }
}
