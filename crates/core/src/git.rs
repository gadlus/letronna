//! What changed in the project folder since its last commit, as git sees it.
//!
//! `/diff` and the window's Changes tab show this without asking, so git runs
//! here with the read-only guards (`command::GUARDS`) and with no external
//! diff and no textconv: a repository's own config must not pick a program
//! for a look at its changes to run. Clean filters still run, as they do for
//! any `git diff`; the guards cannot turn those off either.
//!
//! Untracked files are shown too, each diffed against nothing, since a file
//! a tool created is as much a change as one it edited. What git prints is
//! cut at a size, with a line saying so, rather than read whole.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The most of the summary kept: a line per file, so thousands of them.
const STAT_CAP: usize = 64 * 1024;

/// The most of the patch kept. Enough to read a large change through; a
/// generated file or a vendored tree is not worth holding in full.
const PATCH_CAP: usize = 256 * 1024;

/// Untracked files named and diffed, past which the rest are counted.
const UNTRACKED_MAX: usize = 200;

/// Width of the summary's lines, so both front ends get the same.
const STAT_WIDTH: &str = "--stat=100";

/// The changes in a project folder: git's summary of them, then the patch.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct WorkingDiff {
    pub stat: String,
    pub patch: String,
}

impl WorkingDiff {
    /// Whether nothing has changed.
    pub fn is_empty(&self) -> bool {
        self.stat.trim().is_empty() && self.patch.trim().is_empty()
    }
}

/// What changed in `folder` since the last commit, tracked and untracked,
/// limited to `only` when given (paths relative to `folder`). Outside a git
/// repository there is nothing to compare with, and the error says so.
pub fn working_diff(folder: &Path, only: Option<&[PathBuf]>) -> Result<WorkingDiff, String> {
    let inside = git(folder, &["rev-parse", "--is-inside-work-tree"], 64)
        .map(|(out, _)| out.trim() == "true")
        .unwrap_or(false);
    if !inside {
        return Err(format!(
            "{} is not in a git repository, so there is nothing to compare it with.",
            folder.display()
        ));
    }
    if only.is_some_and(<[PathBuf]>::is_empty) {
        return Ok(WorkingDiff::default());
    }
    let base = base(folder)?;
    let paths: Vec<String> = match only {
        Some(paths) => paths.iter().map(|p| p.display().to_string()).collect(),
        None => vec![".".to_owned()],
    };
    let diff = |extra: &str, cap: usize| {
        let mut args = vec![
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--relative",
            extra,
            base.as_str(),
            "--",
        ];
        args.extend(paths.iter().map(String::as_str));
        git(folder, &args, cap)
    };
    let (mut stat, stat_cut) = diff(STAT_WIDTH, STAT_CAP)?;
    let (mut patch, mut patch_cut) = diff("--patch", PATCH_CAP)?;

    let mut listing = vec!["ls-files", "-z", "--others", "--exclude-standard", "--"];
    listing.extend(paths.iter().map(String::as_str));
    let (untracked, _) = git(folder, &listing, STAT_CAP)?;
    let untracked: Vec<&str> = untracked.split('\0').filter(|p| !p.is_empty()).collect();
    for (at, path) in untracked.iter().enumerate() {
        if at == UNTRACKED_MAX {
            stat.push_str(&format!(
                " and {} more untracked files\n",
                untracked.len() - UNTRACKED_MAX
            ));
            break;
        }
        stat.push_str(&format!(" {path} (untracked)\n"));
        if patch_cut {
            continue;
        }
        // One that went between the listing and now is only left out.
        let Ok((text, cut)) = new_file(folder, path, PATCH_CAP.saturating_sub(patch.len())) else {
            continue;
        };
        patch.push_str(&text);
        patch_cut = cut;
    }
    if stat_cut {
        stat.push_str(&cut_note("summary", STAT_CAP));
    }
    if patch_cut {
        patch.push_str(&cut_note("patch", PATCH_CAP));
    }
    Ok(WorkingDiff {
        stat: printable(&stat),
        patch: printable(&patch),
    })
}

/// What the changes are measured from: the last commit, or nothing at all
/// in a repository that has none yet.
fn base(folder: &Path) -> Result<String, String> {
    if git(
        folder,
        &["rev-parse", "--verify", "--quiet", "HEAD^{commit}"],
        128,
    )
    .is_ok()
    {
        return Ok("HEAD".to_owned());
    }
    // The empty tree, asked of git rather than written in, since its name
    // depends on the repository's hash.
    let (tree, _) = git(folder, &["hash-object", "-t", "tree", "--stdin"], 128)?;
    Ok(tree.trim().to_owned())
}

/// An untracked file as a patch that adds it, at most `cap` bytes of it.
fn new_file(folder: &Path, path: &str, cap: usize) -> Result<(String, bool), String> {
    // A difference is exit status 1 here, which `git` counts as a failure.
    let args = [
        "diff",
        "--no-index",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
        "--patch",
        "--",
        "/dev/null",
        path,
    ];
    run(folder, &args, cap, &[0, 1])
}

/// The line put where a part was cut.
fn cut_note(part: &str, cap: usize) -> String {
    format!(
        "… the {part} is cut here at {} KB; run git diff to see all of it.\n",
        cap / 1024
    )
}

/// Run git in `folder` with the guards on, keeping at most `cap` bytes of
/// what it prints; whether it was cut rides along.
fn git(folder: &Path, args: &[&str], cap: usize) -> Result<(String, bool), String> {
    run(folder, args, cap, &[0])
}

fn run(folder: &Path, args: &[&str], cap: usize, fine: &[i32]) -> Result<(String, bool), String> {
    let mut cmd = Command::new("git");
    cmd.args([
        "--no-pager",
        "--literal-pathspecs",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.quotePath=false",
    ])
    .args(args)
    .current_dir(folder)
    .envs(crate::command::GUARDS.iter().copied())
    .env_remove("GIT_EXTERNAL_DIFF")
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("git could not be run: {e}"))?;
    let Some(stdout) = child.stdout.take() else {
        return Err("git gave no output to read.".into());
    };
    let mut bytes = Vec::new();
    let read = stdout.take(cap as u64 + 1).read_to_end(&mut bytes);
    let cut = bytes.len() > cap;
    if cut {
        // The rest is not wanted, and git would wait on a full pipe for it.
        let _ = child.kill();
    }
    let mut errors = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_string(&mut errors);
    }
    let status = child
        .wait()
        .map_err(|e| format!("git did not finish: {e}"))?;
    read.map_err(|e| format!("git's output could not be read: {e}"))?;
    if !cut && !status.code().is_some_and(|code| fine.contains(&code)) {
        let why = errors.lines().next().unwrap_or("it failed").trim();
        return Err(format!("git {} failed: {why}", args[0]));
    }
    bytes.truncate(cap);
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if cut {
        // End on a whole line, or a whole name of a `-z` listing, rather
        // than halfway through one.
        let end = text.rfind(['\n', '\0']).map_or(0, |at| at + 1);
        text.truncate(end);
    }
    Ok((text, cut))
}

/// `text` without what a terminal would act on: a file's own escapes must
/// not reach the screen as escapes. Tabs and line ends stay.
fn printable(text: &str) -> String {
    text.chars()
        .filter(|&c| c != '\r')
        .map(|c| match c {
            '\n' | '\t' => c,
            c if c.is_control() => '?',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own for one test, under the system's temp folder.
    fn scratch(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("letronna-git-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    /// Run git in `folder` as a test sets a repository up, ignoring the
    /// user's own config.
    fn setup(folder: &Path, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(args)
            .current_dir(folder)
            .env(
                "GIT_CONFIG_GLOBAL",
                if cfg!(windows) { "nul" } else { "/dev/null" },
            )
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    /// A repository with one committed file.
    fn repo(name: &str) -> PathBuf {
        let folder = scratch(name);
        setup(&folder, &["init", "-q"]);
        std::fs::write(folder.join("a.txt"), "one\ntwo\n").unwrap();
        setup(&folder, &["add", "a.txt"]);
        setup(&folder, &["commit", "-q", "-m", "first"]);
        folder
    }

    #[test]
    fn an_edit_shows_in_the_summary_and_the_patch() {
        let folder = repo("edit");
        std::fs::write(folder.join("a.txt"), "one\n2\n").unwrap();
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.stat.contains("a.txt"), "{}", diff.stat);
        assert!(diff.patch.contains("-two\n+2\n"), "{}", diff.patch);
    }

    #[test]
    fn an_untracked_file_shows_as_added() {
        let folder = repo("untracked");
        std::fs::write(folder.join("new.txt"), "fresh\n").unwrap();
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.stat.contains("new.txt (untracked)"), "{}", diff.stat);
        assert!(diff.patch.contains("+fresh"), "{}", diff.patch);
        // Limited to another file, it is left out.
        let only = working_diff(&folder, Some(&[PathBuf::from("a.txt")])).unwrap();
        assert!(only.is_empty(), "{only:?}");
    }

    #[test]
    fn a_repository_s_own_diff_programs_do_not_run() {
        let folder = repo("drivers");
        std::fs::write(folder.join(".gitattributes"), "*.txt diff=evil\n").unwrap();
        setup(
            &folder,
            &["config", "diff.evil.command", "touch ran-command"],
        );
        setup(
            &folder,
            &["config", "diff.evil.textconv", "touch ran-textconv; cat"],
        );
        setup(&folder, &["config", "diff.external", "touch ran-external"]);
        setup(&folder, &["config", "core.pager", "touch ran-pager; cat"]);
        std::fs::write(folder.join("a.txt"), "one\nthree\n").unwrap();
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.patch.contains("+three"), "{}", diff.patch);
        for marker in ["ran-command", "ran-textconv", "ran-external", "ran-pager"] {
            assert!(!folder.join(marker).exists(), "{marker} ran");
        }
    }

    #[test]
    fn a_long_patch_is_cut_with_a_note() {
        let folder = repo("long");
        let big: String = (0..40_000).map(|n| format!("line {n}\n")).collect();
        std::fs::write(folder.join("a.txt"), big).unwrap();
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.patch.len() < PATCH_CAP + 200);
        assert!(
            diff.patch.contains("the patch is cut here"),
            "{}",
            &diff.patch[..200]
        );
    }

    #[test]
    fn a_repository_with_no_commit_compares_with_nothing() {
        let folder = scratch("unborn");
        setup(&folder, &["init", "-q"]);
        std::fs::write(folder.join("a.txt"), "one\n").unwrap();
        setup(&folder, &["add", "a.txt"]);
        let diff = working_diff(&folder, None).unwrap();
        assert!(diff.patch.contains("+one"), "{}", diff.patch);
    }

    #[test]
    fn a_folder_outside_git_says_so() {
        let folder = scratch("none");
        // A temp folder inside someone's repository is in one, and there
        // is nothing to check.
        let error = Command::new("git")
            .args(["rev-parse", "--is-inside-work-tree"])
            .current_dir(&folder)
            .output()
            .unwrap();
        if error.status.success() {
            return;
        }
        let why = working_diff(&folder, None).unwrap_err();
        assert!(why.contains("not in a git repository"), "{why}");
    }

    #[test]
    fn escapes_in_a_file_do_not_reach_the_screen() {
        assert_eq!(printable("a\x1b[31mb\r\n\tc"), "a?[31mb\n\tc");
    }
}
