//! What was sent at the prompt, kept for each project folder across
//! sessions, so up and ctrl-r reach a prompt typed yesterday in another
//! session as well as one typed a moment ago in this one.
//!
//! One file of JSON lines in the data folder, readable by its owner alone:
//! a line is appended as each prompt goes, and once there are many more than
//! are kept, the file is written again with only those.

use std::io::Write;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// The newest prompts kept for each folder.
pub const KEPT: usize = 1_000;

/// Under this many lines the file is never written again, however much of
/// it is past what is kept.
const TIDY_AT: usize = 20_000;

/// What this process last kept, so the same prompt sent again and again in
/// a row is kept once.
static LAST: Mutex<Option<(String, String)>> = Mutex::new(None);

/// Whether this process has looked at how long the file is: it does at the
/// first prompt it keeps, not at each one, as the whole file is read for it.
static LOOKED: AtomicBool = AtomicBool::new(false);

fn file() -> std::path::PathBuf {
    letronna_core::chat::data_dir().join("prompt-history.jsonl")
}

/// How a folder is written in the file; no folder at all is the empty name.
fn key(folder: Option<&Path>) -> String {
    folder.map(|f| f.display().to_string()).unwrap_or_default()
}

/// The prompts sent in `folder`, oldest first, the newest `KEPT` of them,
/// and a run of the same one only once.
pub fn load(folder: Option<&Path>) -> Vec<String> {
    load_from(&file(), &key(folder))
}

fn load_from(path: &Path, folder: &str) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut kept: Vec<String> = Vec::new();
    for entry in text.lines().filter_map(entry) {
        if entry.0 == folder && kept.last() != Some(&entry.1) {
            kept.push(entry.1);
        }
    }
    let over = kept.len().saturating_sub(KEPT);
    kept.drain(..over);
    kept
}

/// A line of the file as its folder and its text.
fn entry(line: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let folder = value["folder"].as_str()?.to_owned();
    let text = value["text"].as_str()?.to_owned();
    Some((folder, text))
}

/// Keep `said`, sent at the prompt in `folder`.
pub fn add(folder: Option<&Path>, said: &str) {
    add_to(&file(), &key(folder), said);
}

fn add_to(path: &Path, folder: &str, said: &str) {
    if said.trim().is_empty() {
        return;
    }
    let this = (folder.to_owned(), said.to_owned());
    if let Ok(mut last) = LAST.lock() {
        if last.as_ref() == Some(&this) {
            return;
        }
        *last = Some(this);
    }
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let line = format!(
        "{}\n",
        serde_json::json!({"folder": folder, "text": said, "at": at})
    );
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    // Kept or not, the prompt still goes: history is a convenience.
    if let Ok(mut out) = options.open(path) {
        let _ = out.write_all(line.as_bytes());
    }
    if !LOOKED.swap(true, Ordering::Relaxed) {
        tidy(path);
    }
}

/// Write the file again with the newest `KEPT` lines of each folder, in
/// the order they were sent, once it holds more than twice those: with many
/// folders, a file past `TIDY_AT` can be mostly what is kept, and writing
/// it again would gain nothing.
fn tidy(path: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let lines: Vec<&str> = text.lines().collect();
    if lines.len() <= TIDY_AT {
        return;
    }
    let mut seen: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut keep = vec![false; lines.len()];
    for (at, line) in lines.iter().enumerate().rev() {
        let Some((folder, _)) = entry(line) else {
            continue;
        };
        let count = seen.entry(folder).or_default();
        if *count < KEPT {
            *count += 1;
            keep[at] = true;
        }
    }
    if lines.len() <= 2 * keep.iter().filter(|kept| **kept).count() {
        return;
    }
    let mut kept: String = (lines.iter().zip(&keep))
        .filter(|(_, kept)| **kept)
        .map(|(line, _)| format!("{line}\n"))
        .collect();
    // A prompt another terminal added since the read goes on the end rather
    // than being lost; a file another terminal tidied meanwhile is left be.
    let Ok(now) = std::fs::read_to_string(path) else {
        return;
    };
    let Some(added) = now.strip_prefix(text.as_str()) else {
        return;
    };
    kept.push_str(added);
    let _ = letronna_core::store::write_private(path, kept.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::{KEPT, TIDY_AT, add_to, load_from, tidy};

    #[test]
    fn history_is_kept_per_folder_and_skips_repeats() {
        let dir = std::env::temp_dir().join(format!("letronna-history-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prompt-history.jsonl");
        let _ = std::fs::remove_file(&path);
        add_to(&path, "/work/app", "fix the build");
        add_to(&path, "/work/app", "fix the build");
        add_to(&path, "/work/site", "update the page");
        add_to(&path, "/work/app", "line one\nline two");
        assert_eq!(
            load_from(&path, "/work/app"),
            ["fix the build", "line one\nline two"]
        );
        assert_eq!(load_from(&path, "/work/site"), ["update the page"]);
        assert!(load_from(&path, "/elsewhere").is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o077, 0, "readable by others: {mode:o}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The file at `path` with `lines` lines, sent in `folders` folders in
    /// turn.
    fn written(path: &std::path::Path, lines: usize, folders: usize) {
        let text: String = (0..lines)
            .map(|n| {
                let folder = format!("/f{}", n % folders);
                format!(
                    "{}\n",
                    serde_json::json!({"folder": folder, "text": format!("p{n}"), "at": 0})
                )
            })
            .collect();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn a_long_file_is_written_again_with_what_is_kept() {
        let dir = std::env::temp_dir().join(format!("letronna-tidy-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prompt-history.jsonl");
        written(&path, TIDY_AT + 1, 1);
        // Reading it leaves the file as it is.
        let kept = load_from(&path, "/f0");
        assert_eq!(kept.len(), KEPT);
        assert_eq!(kept.last().unwrap(), &format!("p{TIDY_AT}"));
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            TIDY_AT + 1
        );
        tidy(&path);
        let written = std::fs::read_to_string(&path).unwrap();
        assert_eq!(written.lines().count(), KEPT);
        assert_eq!(load_from(&path, "/f0"), kept);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_long_file_mostly_kept_is_not_written_again() {
        let dir = std::env::temp_dir().join(format!("letronna-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("prompt-history.jsonl");
        // Past TIDY_AT, but every line is among its folder's newest KEPT.
        let folders = TIDY_AT / KEPT + 5;
        written(&path, folders * KEPT, folders);
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        tidy(&path);
        assert_eq!(
            std::fs::read_to_string(&path).unwrap().lines().count(),
            folders * KEPT
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            before
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
