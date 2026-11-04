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

use std::path::{Path, PathBuf};
#[cfg(any(unix, test))]
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

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

/// A tester's files kept for a run: its id, the project it puts them back
/// in, and where they are kept.
struct Moved {
    run: u64,
    workspace: PathBuf,
    kept: Kept,
}

/// The tester's files kept for each run so far, so what the run puts back
/// is deleted as it ends, and the folder they are kept in with them.
fn moved() -> &'static Mutex<Vec<Moved>> {
    static MOVED: OnceLock<Mutex<Vec<Moved>>> = OnceLock::new();
    MOVED.get_or_init(Default::default)
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
    let top = git(workspace, &["rev-parse", "--show-toplevel"])?;
    let canonical = |path: &Path| std::fs::canonicalize(path).ok();
    if canonical(Path::new(top.first()?)) != canonical(workspace) {
        return None;
    }
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
    let ignored = git(
        workspace,
        &[
            "ls-files",
            "-z",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    )?;
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
        for entry in &ignored {
            let entry = entry.trim_end_matches('/');
            let at = to.join(entry);
            std::fs::create_dir_all(at.parent()?).ok()?;
            std::os::unix::fs::symlink(workspace.join(entry), &at).ok()?;
        }
        Some(())
    })();
    if made.is_none() {
        discard(&to);
        return None;
    }
    Some(to)
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
    let written = (|| {
        let mut archive = Command::new("git")
            .arg("-C")
            .arg(workspace)
            .args(["archive", "--format=tar", "HEAD"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let unpacked = Command::new("tar")
            .arg("-xf")
            .arg("-")
            .arg("-C")
            .arg(&to)
            .stdin(archive.stdout.take()?)
            .stderr(Stdio::null())
            .status()
            .ok()?;
        (archive.wait().ok()?.success() && unpacked.success()).then_some(())
    })();
    if written.is_none() {
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
    let mut text = format!(
        "Test a change you have not seen, made to the project in your working folder, \
         against the request below, by writing tests from the request's words alone. Do not \
         look at what was changed or how: run no `git diff`, `git status`, `git log` or \
         `git show`, and read the project's code only to learn what to call and how, such as \
         names, arguments and set-up, never to decide what a result should be. {read_there}\
         Every name the request spells out, such as a path, route, field, flag, key or \
         command, is used in your tests as the request spells it, never as the code does: a \
         test that fails because the work spells it differently is a finding. Where the request \
         names nothing for something new, search your working folder for that name alone, not \
         for how it works. What each test expects comes from the request's words; quote them in \
         the test's name or a comment.\n\n\
         For each requirement, test the behaviour it states through the entry point a caller \
         of the project uses, end to end: each case it names, each form its input can be \
         written in, the side a limiting word such as only, unless, otherwise or and not keeps \
         out, with both sides in one input, steps it sets in no order in each order, such as the \
         later one first, each category it maps to an output with an input from outside the \
         project's own code that uses the request's words for it, and each other feature it is \
         said to work with, used together with it in one call. Set \
         each test up the way the project's own tests set up the nearest feature it already \
         has, and make each change before the first call or query, as a caller would, rather \
         than calling once beforehand to warm anything up. Where the project's data, docs or \
         tests already spell an output, expect that spelling.\n\n\
         Write the tests in the project's own test language and style, in new files whose \
         names hold `{MARKER}`, where its test runner finds them, such as beside its other \
         tests, and run them with that runner. Change no other file. A test that fails because \
         you called the code wrongly, such as a guessed name, import or argument the request \
         does not spell, is yours to fix from the code; one that fails on what the request \
         says should happen is a finding: keep it as it is, and do not make it pass.\n\n\
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
    text.push_str(
        "\n\nEnd your report with a table of one row per test, written `R1 | what it checks | \
         PASS` or `R1 | what it checks | FAIL | command that runs it | what it expected and \
         what it got`.",
    );
    text
}

/// The tester's files under `from`, the copy it worked in or the project
/// itself, moved into a new folder of the temp folder at the same paths,
/// and kept for run `run` of `workspace`, so `tidy` deletes what it puts
/// back; None when it left none. One that could not be kept is deleted all
/// the same, so none stays where a patch would take it.
pub(crate) fn keep(run: u64, from: &Path, workspace: &Path) -> Option<Kept> {
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
        });
    }
    Some(kept)
}

/// Deletes the tester's files run `run` put back in its project, at the paths
/// `keep` kept them from, and the folder they were kept in, and forgets them:
/// they are the tester's, by their names, and no part of the task, and a run
/// stopped at its deadline never deletes them itself. The paths it deleted
/// from the project, in order.
pub(crate) fn tidy(run: u64) -> Vec<String> {
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
pub(crate) fn unrun(report: &str, kept: &Kept) -> String {
    format!(
        "A tester that hasn't seen your work wrote tests from the request alone but could not \
         run them where it worked; its report follows. {} Run them with the project's own \
         runner and the set-up you found works, fix a test only where it calls the code wrongly, \
         such as a guessed name or import the request does not spell, and fix your work for \
         each test that fails on what the request's words say. Then delete the copies, since \
         they are not part of the task.\n\n{}",
        where_kept(kept),
        report.trim()
    )
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

/// What the run is told of the tester's `report`, with its table, and of
/// where its tests are `kept`.
pub(crate) fn told(report: &str, kept: Option<&Kept>) -> String {
    let mut text = String::from(
        "A tester that hasn't seen your work wrote tests from the request alone and ran them; \
         its report follows, ending with a table of its tests. Fix your work for each test \
         that fails, unless the test expects something the request's words do not say: then \
         quote the words that show it in your answer.",
    );
    if let Some(kept) = kept {
        text.push_str(&format!(
            " {} Delete the copies before you end, since they are not part of the task.",
            where_kept(kept)
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

        let kept = keep(1, &root, &root).unwrap();
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
        assert_eq!(keep(1, &root, &root), None);
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
        std::fs::write(root.join("src/lib.ts"), "export const a = 1;").unwrap();
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
        let kept = keep(2, &copy, &root).unwrap();
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
        let kept = keep(3, &root, &root).unwrap();
        // Another run in the same folder has a tester of its own.
        std::fs::write(root.join("tests/letronna_spec_b_test.go"), "test").unwrap();
        let other = keep(4, &root, &root).unwrap();
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
        );
        assert!(text.contains("kept out of the project, in `/tmp/kept`"));
        assert!(text.contains("`tests/letronna_spec.test.ts`"));
        assert!(text.contains("Delete the copies before you end"));
        assert!(text.ends_with("R1 | adds a pair | FAIL | npx vitest run x | got []"));
        assert!(!told("R1 | x | PASS", None).contains("kept out of the project"));
    }

    #[test]
    fn tests_a_tester_could_not_run_are_left_for_the_run_to_run() {
        let kept = Kept {
            dir: PathBuf::from("/tmp/kept"),
            files: vec!["drift/letronna_spec_test.go".to_owned()],
        };
        let report = "No Go toolchain in my copy.\nR1 | resolves | NOT RUN";
        let text = unrun(report, &kept);
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
}
