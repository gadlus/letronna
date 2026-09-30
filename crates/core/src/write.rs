//! Putting a file tool's text on disk: whole or not at all, and in the format
//! the file already had.
//!
//! A plain `fs::write` truncates the file first, so a crash, a full disk or a
//! stopped process part way through leaves half a Cargo.toml behind. Here the
//! bytes go to a temporary file beside the real one, which is synced and then
//! renamed over it, so the file is always either the old one or the new one.
//! The new file takes the old one's permissions (and on Unix its owner), and a
//! link is written through rather than replaced. A file that cannot be swapped
//! -- a folder the user may not add files to, an owner that cannot be carried
//! over, or on Windows a file another program holds open -- is written in
//! place as before. Every write ends by checking the size on disk. The folder
//! itself is not synced: a power cut may undo the rename, which still leaves
//! the old file whole. The file tools write through [`atomic`] and delete
//! through [`remove`], nothing else, so there is one place to see every
//! change they make: each hands the old bytes to the turn's `history::Log`
//! first, for a rewind to put back.

use std::fs::{File, Metadata, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// Counts temporary files, so two writes in this process never share a name.
static TEMPS: AtomicU64 = AtomicU64::new(0);

/// Replace `path`'s contents with `bytes`, whole or not at all wherever the
/// file can be swapped, noting the change in `log`.
pub fn atomic(path: &Path, bytes: &[u8], log: &crate::history::Log) -> Result<(), String> {
    let shown = path.display();
    let target = through_link(path)?;
    let old = match std::fs::metadata(&target) {
        Ok(meta) => Some(meta),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(format!("Could not write `{shown}`: {e}")),
    };
    if old.as_ref().is_some_and(|meta| !meta.is_file()) {
        return Err(format!("`{shown}` is not a regular file."));
    }
    // Where the disk ignores case, a rename over `A.TXT` renames `a.txt` too;
    // the file keeps its own name by being written under it.
    let target = match old {
        Some(_) => std::fs::canonicalize(&target).unwrap_or(target),
        None => target,
    };
    if old
        .as_ref()
        .is_some_and(|meta| meta.permissions().readonly())
    {
        return Err(format!("`{shown}` is read-only."));
    }
    // Only once the write is sure to be tried, so a refused one keeps nothing.
    let before = log.keep(&target, old.is_some());
    let swapped = swap(&target, bytes, old.as_ref())
        .map_err(|e| format!("Could not write `{shown}`: {e}"))?;
    if !swapped {
        in_place(&target, bytes).map_err(|e| format!("Could not write `{shown}`: {e}"))?;
    }
    let on_disk = std::fs::metadata(&target).map_or(0, |meta| meta.len());
    if on_disk != bytes.len() as u64 {
        return Err(format!(
            "Write verification failed: `{shown}` is {on_disk} bytes on disk, expected {}.",
            bytes.len()
        ));
    }
    log.note(path, before, bytes);
    Ok(())
}

/// Delete the file at `path`, keeping what it held in `log` first, so a
/// rewind can bring it back.
pub fn remove(path: &Path, log: &crate::history::Log) -> std::io::Result<()> {
    // Its real path, taken while it is there, names it as `atomic` would.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned());
    let before = log.keep(path, true);
    std::fs::remove_file(path)?;
    log.gone(&real, before);
    Ok(())
}

/// The file a write to `path` lands in: the end of the chain when `path` is a
/// link, because a rename over the link would replace the link itself.
fn through_link(path: &Path) -> Result<PathBuf, String> {
    let link = std::fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink());
    if !link {
        return Ok(path.to_path_buf());
    }
    std::fs::canonicalize(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            format!(
                "`{}` is a link to a file that does not exist.",
                path.display()
            )
        }
        _ => format!("Could not follow the link `{}`: {e}", path.display()),
    })
}

/// Write `bytes` beside `target` and rename them over it; `Ok(false)` when the
/// file has to be written in place instead.
fn swap(target: &Path, bytes: &[u8], old: Option<&Metadata>) -> std::io::Result<bool> {
    let Some(temp) = temp_beside(target) else {
        return Ok(false);
    };
    // Opened as fs::write opens a file, so a new one gets the usual mode less
    // the umask rather than owner-only. A folder the user may not add files to
    // can still hold a file they may write, and a name too long for the
    // suffix is still a name, so failing here is not the end.
    let Ok(file) = OpenOptions::new().write(true).create_new(true).open(&temp) else {
        return Ok(false);
    };
    let swapped = fill(file, bytes, old).and_then(|carried| {
        if !carried {
            return Ok(false);
        }
        match std::fs::rename(&temp, target) {
            Ok(()) => Ok(true),
            Err(e) if held_open(cfg!(windows), e.raw_os_error()) => Ok(false),
            Err(e) => Err(e),
        }
    });
    if !matches!(swapped, Ok(true)) {
        let _ = std::fs::remove_file(&temp);
    }
    swapped
}

/// A hidden name in the target's own folder, so the rename stays on one file
/// system and a file watcher's glob for the real name does not match it.
fn temp_beside(target: &Path) -> Option<PathBuf> {
    let name = target.file_name()?.to_string_lossy();
    let n = TEMPS.fetch_add(1, Ordering::Relaxed);
    Some(target.with_file_name(format!(".{name}.{}.{n}.tmp", std::process::id())))
}

/// Put `bytes` in the temporary file with the old file's owner and
/// permissions, and sync it; `Ok(false)` when the owner cannot be carried.
fn fill(mut file: File, bytes: &[u8], old: Option<&Metadata>) -> std::io::Result<bool> {
    file.write_all(bytes)?;
    if let Some(old) = old {
        if !same_owner(&file, old)? {
            return Ok(false);
        }
        // After the owner, because a change of owner can clear set-id bits.
        file.set_permissions(old.permissions())?;
    }
    // Before the rename, or a crash could leave the name on unwritten blocks.
    file.sync_all()?;
    Ok(true)
}

/// Give the temporary file the old file's owner and group where they differ;
/// `false` when that is not allowed, as for a file someone else owns.
#[cfg(unix)]
fn same_owner(file: &File, old: &Metadata) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let new = file.metadata()?;
    if (new.uid(), new.gid()) == (old.uid(), old.gid()) {
        return Ok(true);
    }
    Ok(std::os::unix::fs::fchown(file, Some(old.uid()), Some(old.gid())).is_ok())
}

/// Windows gives a new file its folder's owner and access rules; copying the
/// old file's own is not cheap, so it is not tried.
#[cfg(not(unix))]
fn same_owner(_: &File, _: &Metadata) -> std::io::Result<bool> {
    Ok(true)
}

/// The old way, for a file that cannot be swapped: truncate it and write it.
fn in_place(target: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(target)?;
    file.write_all(bytes)?;
    file.sync_all()
}

/// Whether a rename that failed with OS error `code` met a file another
/// program holds open, which Windows will not replace but will let be
/// written: access denied (5), a sharing violation (32) or a lock (33).
fn held_open(windows: bool, code: Option<i32>) -> bool {
    windows && matches!(code, Some(5 | 32 | 33))
}

/// `content` in the format of the `file` it replaces: that file's UTF-8 byte
/// order mark, and its line ending where `content` uses the other throughout.
pub fn keep_format(file: &[u8], content: &str) -> String {
    let crlf = content.matches("\r\n").count();
    let text = match line_ending(file) {
        Some("\r\n") if crlf == 0 => content.replace('\n', "\r\n"),
        Some("\n") if crlf > 0 && crlf == content.matches('\n').count() => {
            content.replace("\r\n", "\n")
        }
        _ => content.to_owned(),
    };
    if file.starts_with("\u{FEFF}".as_bytes()) && !text.starts_with('\u{FEFF}') {
        return format!("\u{FEFF}{text}");
    }
    text
}

/// The line break a file uses, judged by its first as edit_file judges it;
/// `None` when it has none.
fn line_ending(file: &[u8]) -> Option<&'static str> {
    let at = file.iter().position(|&b| b == b'\n')?;
    Some(if file[..at].ends_with(b"\r") {
        "\r\n"
    } else {
        "\n"
    })
}

/// Refuse JSON or TOML that would no longer parse, since a broken
/// package.json or Cargo.toml stops the whole project. A file that did not
/// parse before -- a template, or comments its own tools allow -- is not held
/// to it, and neither are the JSON files editors read with comments. A
/// notebook is JSON too, which read_file shows unquoted as cells, so an edit
/// copied from that view is refused rather than left unable to open.
pub fn check_syntax(path: &Path, before: Option<&[u8]>, after: &str) -> Result<(), String> {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    let kind = match extension.as_deref() {
        Some("json") if !with_comments(path) => "JSON",
        Some("ipynb") => "JSON",
        Some("toml") => "TOML",
        _ => return Ok(()),
    };
    let parsed = before
        .is_none_or(|old| std::str::from_utf8(old).is_ok_and(|text| parse(kind, text).is_ok()));
    match parse(kind, after) {
        Err(e) if parsed => Err(format!(
            "This would leave `{}` as invalid {kind}, so nothing was written: {e}",
            path.file_name().unwrap_or_default().to_string_lossy()
        )),
        _ => Ok(()),
    }
}

/// Parse `text` as JSON or TOML, less a byte order mark, which neither parser
/// takes and which the file may well have.
fn parse(kind: &str, text: &str) -> Result<(), String> {
    let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
    match kind {
        "JSON" => serde_json::from_str::<serde_json::Value>(text)
            .map(drop)
            .map_err(|e| e.to_string()),
        _ => text
            .parse::<toml::Table>()
            .map(drop)
            .map_err(|e| e.to_string()),
    }
}

/// JSON files whose own tools allow comments and trailing commas: those of
/// TypeScript, VS Code and dev containers.
fn with_comments(path: &Path) -> bool {
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    name.starts_with("tsconfig")
        || name.starts_with("jsconfig")
        || name.contains("devcontainer")
        || path.parent().and_then(Path::file_name) == Some(std::ffi::OsStr::new(".vscode"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::Log;

    /// An empty folder of its own for one test.
    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-write-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[cfg(unix)]
    fn mode(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    #[cfg(unix)]
    #[test]
    fn a_new_file_gets_the_usual_mode_and_a_replaced_one_keeps_its_own() {
        use std::os::unix::fs::PermissionsExt;
        let dir = folder("mode");
        std::fs::write(dir.join("plain.txt"), "").unwrap();
        atomic(&dir.join("new.txt"), b"new", &Log::default()).unwrap();
        assert_eq!(std::fs::read_to_string(dir.join("new.txt")).unwrap(), "new");
        assert_eq!(mode(&dir.join("new.txt")), mode(&dir.join("plain.txt")));

        let script = dir.join("run.sh");
        std::fs::write(&script, "echo old\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o750)).unwrap();
        atomic(&script, b"echo new\n", &Log::default()).unwrap();
        assert_eq!(std::fs::read_to_string(&script).unwrap(), "echo new\n");
        assert_eq!(mode(&script), 0o750);
        assert_eq!(names(&dir), ["new.txt", "plain.txt", "run.sh"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_link_is_written_through_and_a_link_to_nothing_is_refused() {
        let dir = folder("link");
        std::fs::write(dir.join("real.txt"), "old").unwrap();
        std::os::unix::fs::symlink("real.txt", dir.join("link.txt")).unwrap();
        atomic(&dir.join("link.txt"), b"new", &Log::default()).unwrap();
        let link = std::fs::symlink_metadata(dir.join("link.txt")).unwrap();
        assert!(link.file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(dir.join("real.txt")).unwrap(),
            "new"
        );

        std::os::unix::fs::symlink("gone.txt", dir.join("dangling.txt")).unwrap();
        let err = atomic(&dir.join("dangling.txt"), b"x", &Log::default()).unwrap_err();
        assert!(err.contains("does not exist"), "{err}");
        assert!(!dir.join("gone.txt").exists());
        assert_eq!(names(&dir), ["dangling.txt", "link.txt", "real.txt"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_file_in_a_folder_that_takes_no_new_files_is_written_in_place() {
        use std::os::unix::fs::PermissionsExt;
        let dir = folder("locked");
        let file = dir.join("a.txt");
        std::fs::write(&file, "old").unwrap();
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let written = atomic(&file, b"new contents", &Log::default());
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        written.unwrap();
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "new contents");
        assert_eq!(names(&dir), ["a.txt"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_read_only_file_or_a_folder_is_refused_and_left_alone() {
        let dir = folder("refused");
        let file = dir.join("a.txt");
        std::fs::write(&file, "old").unwrap();
        let mut permissions = std::fs::metadata(&file).unwrap().permissions();
        permissions.set_readonly(true);
        std::fs::set_permissions(&file, permissions.clone()).unwrap();
        let err = atomic(&file, b"new", &Log::default()).unwrap_err();
        assert!(err.contains("read-only"), "{err}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "old");
        #[allow(clippy::permissions_set_readonly_false)]
        permissions.set_readonly(false);
        std::fs::set_permissions(&file, permissions).unwrap();

        std::fs::create_dir(dir.join("sub")).unwrap();
        assert!(atomic(&dir.join("sub"), b"x", &Log::default()).is_err());
        assert_eq!(names(&dir), ["a.txt", "sub"]);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_temporary_file_is_hidden_in_the_same_folder_under_a_new_name() {
        let target = Path::new("src").join("main.rs");
        let (a, b) = (temp_beside(&target).unwrap(), temp_beside(&target).unwrap());
        assert_eq!(a.parent(), target.parent());
        let name = a.file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with(".main.rs.") && name.ends_with(".tmp"),
            "{name}"
        );
        assert_ne!(a, b);
    }

    #[test]
    fn windows_writes_in_place_only_over_a_file_another_program_holds_open() {
        assert!(held_open(true, Some(32)));
        assert!(held_open(true, Some(5)));
        assert!(held_open(true, Some(33)));
        assert!(!held_open(true, Some(2)));
        assert!(!held_open(true, None));
        assert!(!held_open(false, Some(5)));
    }

    #[test]
    fn a_rewrite_takes_the_line_ending_and_mark_of_the_file_it_replaces() {
        let crlf = "\u{FEFF}a\r\nb\r\n".as_bytes();
        assert_eq!(keep_format(crlf, "x\ny\n"), "\u{FEFF}x\r\ny\r\n");
        assert_eq!(keep_format(crlf, "\u{FEFF}x\n"), "\u{FEFF}x\r\n");
        assert_eq!(keep_format(b"a\nb\n", "x\r\ny\r\n"), "x\ny\n");
        assert_eq!(keep_format(b"a\nb\n", "\u{FEFF}x\n"), "\u{FEFF}x\n");
    }

    #[test]
    fn mixed_line_endings_and_a_file_without_any_are_written_as_sent() {
        assert_eq!(keep_format(b"a\nb\n", "x\r\ny\n"), "x\r\ny\n");
        assert_eq!(keep_format(b"a\r\n", "x\r\ny\n"), "x\r\ny\n");
        assert_eq!(keep_format(b"one line", "x\r\ny\n"), "x\r\ny\n");
        assert_eq!(keep_format(b"", "x\ny"), "x\ny");
    }

    #[test]
    fn broken_json_or_toml_is_refused_unless_it_was_broken_before_or_allows_comments() {
        let package = Path::new("app/package.json");
        let err = check_syntax(package, None, "{\"a\": 1,}").unwrap_err();
        assert!(
            err.contains("package.json") && err.contains("JSON"),
            "{err}"
        );
        assert!(check_syntax(package, Some(b"{}"), "{\"a\": 1,}").is_err());
        assert!(check_syntax(package, Some(b"{{ template }}"), "{{ other }}").is_ok());
        assert!(check_syntax(package, None, "\u{FEFF}{\"a\": 1}").is_ok());
        let commented = "{\n  // strict mode\n  \"strict\": true,\n}";
        assert!(check_syntax(Path::new("tsconfig.base.json"), None, commented).is_ok());
        assert!(check_syntax(Path::new(".vscode/settings.json"), None, commented).is_ok());
        assert!(check_syntax(Path::new(".vscode/../package.json"), None, commented).is_err());
        let notebook = Path::new("Analysis.IPYNB");
        assert!(check_syntax(notebook, Some(b"{}"), "{\"source\": \"print(\"7\")\"}").is_err());

        let cargo = Path::new("Cargo.toml");
        assert!(check_syntax(cargo, Some(b"[package]"), "[package]\nname = \"a\"").is_ok());
        let err = check_syntax(cargo, Some(b"[package]"), "[package\nname = a").unwrap_err();
        assert!(err.contains("TOML"), "{err}");
        assert!(check_syntax(Path::new("notes.md"), None, "{").is_ok());
    }
}
