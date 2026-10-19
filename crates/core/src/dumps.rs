//! Crash dumps a run leaves in its project, and what it is told of them.
//!
//! A test that crashes writes a core dump where it ran, and a run nobody
//! watches ends with it still there: on a benchmark, a patch taken of every
//! changed and untracked file then carries a binary `core` that git will not
//! apply, and the task fails without its code ever being tested. So a
//! session's own run nobody watches looks once, as it would end, for crash
//! dumps made since it began, and is told to delete them.
//!
//! Only a file's exact name counts, as kernels, Valgrind and the JVM write
//! them: `core.py` or a `core/` folder is a project's own. A dump an ignore
//! file names, or one in a hidden folder or a package, is left out, since no
//! patch of the project takes it. It only advises: nothing here deletes.

use std::path::Path;
use std::time::SystemTime;

/// Entries of the project walked at most, so a run in a big folder, or in
/// the home folder, still ends at once.
const WALKED: usize = 20_000;

/// Dumps a notice names at most: a crashing test leaves one or two, and the
/// first few are enough to find the rest by.
const NAMED: usize = 5;

/// Folders of packages and builds, which no patch takes and which can hold
/// more entries than the rest of the project.
const SKIPPED: [&str; 2] = ["node_modules", "target"];

/// The crash dumps under `workspace` modified at `since` or later, by their
/// paths from there with `/` between folders, in order.
pub(crate) fn left(workspace: &Path, since: SystemTime) -> Vec<String> {
    let mut walk = ignore::WalkBuilder::new(workspace);
    walk.filter_entry(|entry| !SKIPPED.iter().any(|skipped| entry.file_name() == *skipped));
    let mut found: Vec<String> = (walk.build().take(WALKED))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|kind| kind.is_file()))
        .filter(|entry| entry.file_name().to_str().is_some_and(crash_dump))
        // Looked at last, so only a dump's name costs a look at the disk.
        .filter(|entry| {
            let modified = entry.metadata().ok().and_then(|meta| meta.modified().ok());
            modified.is_some_and(|at| at >= since)
        })
        .filter_map(|entry| {
            let inside = entry.path().strip_prefix(workspace).ok()?;
            let parts: Vec<_> = inside.iter().map(|part| part.to_string_lossy()).collect();
            Some(parts.join("/"))
        })
        .collect();
    found.sort();
    found
}

/// What a run that left the crash dumps `found` is told.
pub(crate) fn notice(found: &[String]) -> String {
    let mut named: Vec<String> = found.iter().take(NAMED).map(|f| format!("`{f}`")).collect();
    if found.len() > NAMED {
        named.push(format!("and {} more", found.len() - NAMED));
    }
    format!(
        "[{} check] Before ending: a program that crashed while you worked left crash dumps in \
         the project: {}. They are not part of the task, and a patch taken of the project would \
         carry them, so delete them unless the task asks for them. Only your last message is \
         passed on as your answer, so end with your whole answer again, not only what changed.",
        crate::BRAND,
        named.join(", ")
    )
}

/// Whether a file named `name` is a crash dump: `core`, `core.<pid>` or
/// `<program>.core`, Valgrind's `vgcore.<pid>`, or the JVM's
/// `hs_err_pid<pid>.log` or `java_pid<pid>.hprof`.
fn crash_dump(name: &str) -> bool {
    let pid = |digits: &str| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit());
    let between =
        |head: &str, tail: &str| (name.strip_prefix(head)).and_then(|rest| rest.strip_suffix(tail));
    name == "core"
        || between("core.", "").is_some_and(pid)
        || between("vgcore.", "").is_some_and(pid)
        || between("hs_err_pid", ".log").is_some_and(pid)
        || between("java_pid", ".hprof").is_some_and(pid)
        // A BSD names it for the program, which has a name of some length.
        || between("", ".core").is_some_and(|program| !program.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::Scratch;
    use std::time::Duration;

    #[test]
    fn the_names_dumps_are_written_with_are_crash_dumps() {
        for name in [
            "core",
            "core.1234",
            "python3.core",
            "vgcore.5678",
            "hs_err_pid42.log",
            "java_pid42.hprof",
        ] {
            assert!(crash_dump(name), "{name}");
        }
    }

    #[test]
    fn a_project_file_that_only_looks_like_one_is_not() {
        for name in [
            "core.py",
            "core.",
            "core.12a",
            "core1234",
            "Core",
            "score",
            ".core",
            "vgcore",
            "vgcore.x1",
            "hs_err_pid.log",
            "hs_err_pid42.txt",
            "java_pid42.hprof.gz",
            "java_pidx.hprof",
        ] {
            assert!(!crash_dump(name), "{name}");
        }
    }

    #[test]
    fn a_new_dump_in_a_folder_is_named_by_its_path_and_one_in_a_package_is_not() {
        let dir = Scratch::new("dumps-left");
        let since = SystemTime::now() - Duration::from_secs(60);
        for folder in ["tests", "node_modules/pkg"] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
            std::fs::write(dir.join(folder).join("core.77"), "dump").unwrap();
        }
        std::fs::create_dir_all(dir.join("core")).unwrap();
        std::fs::write(dir.join("core.py"), "print()").unwrap();
        assert_eq!(left(&dir, since), ["tests/core.77"]);
    }
}
