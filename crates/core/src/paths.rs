//! Paths that Windows opens as something other than a plain file.
//!
//! A path the model gives is judged here as text, before anything touches
//! it, because on Windows touching it is the harm: `\\host\share` or
//! `//host/share` makes Windows sign in to that computer with the user's
//! credentials, and checking where a path leads means opening it first. A
//! device path, a hidden stream, a device name such as CON, or a name ending
//! in a dot or a space opens something other than the file it names. On
//! other systems each of these is an ordinary name, so nothing is refused
//! there; the rules sit apart from that switch so every system tests them.
//! A project that itself sits on a share is the one exception: its own share
//! may be named in full, as the prompt names the project folder, because
//! working in it already signs in there.

use std::path::Path;

/// Why Windows would open `raw` as something other than a plain file, or
/// `None` when it would not, it is on `workspace`'s own share, or this is not
/// Windows.
pub fn unsafe_raw(raw: &str, workspace: Option<&Path>) -> Option<&'static str> {
    if !cfg!(windows) {
        return None;
    }
    on_windows(raw, workspace.and_then(Path::to_str))
}

/// What `unsafe_raw` refuses on Windows, where `/` separates as `\` does.
fn on_windows(raw: &str, workspace: Option<&str>) -> Option<&'static str> {
    let path = raw.replace('/', "\\");
    if [r"\\.\", r"\\?\", r"\??\"]
        .iter()
        .any(|prefix| path.starts_with(prefix))
    {
        return Some("is a Windows device path, not a file in a folder");
    }
    // A WebDAV `host@SSL` share starts the same way, and signs in the same.
    let own = share_of(&path).is_some_and(|share| workspace.and_then(share_of) == Some(share));
    if path.starts_with(r"\\") && !own {
        return Some(
            "is on another computer, and opening it would send that computer the user's Windows sign-in",
        );
    }
    let mut chars = path.chars();
    let past_drive = match (chars.next(), chars.next()) {
        (Some(letter), Some(':')) if letter.is_ascii_alphabetic() => chars.as_str(),
        _ => path.as_str(),
    };
    if past_drive.contains(':') {
        return Some("names a hidden stream inside a file, after its `:`");
    }
    let names = path
        .split('\\')
        .filter(|name| !matches!(*name, "" | "." | ".."));
    for name in names {
        if device(name) {
            return Some(
                "is a name Windows keeps for a device such as the console or a port, and opening one can hang",
            );
        }
        if name.ends_with(['.', ' ']) {
            return Some(
                "has a name ending in a dot or a space, which Windows drops, so a different file would open",
            );
        }
    }
    None
}

/// Whether Windows opens `name` as a device, whatever extension follows it.
fn device(name: &str) -> bool {
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let port = ["COM", "LPT"]
        .iter()
        .find_map(|port| stem.strip_prefix(port));
    matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || port.is_some_and(|number| {
        matches!(number, "¹" | "²" | "³")
            || (number.len() == 1 && number.bytes().all(|b| b.is_ascii_digit()))
    })
}

/// The `host\share` that `path` starts with, lower-cased as Windows matches
/// it, whether written `\\host\share` or `\\?\UNC\host\share`.
fn share_of(path: &str) -> Option<String> {
    let path = path.replace('/', "\\").to_lowercase();
    let rest = path
        .strip_prefix(r"\\?\unc\")
        .or_else(|| path.strip_prefix(r"\\"))?;
    let mut names = rest.split('\\');
    let (host, share) = (names.next()?, names.next()?);
    (!host.is_empty() && !share.is_empty()).then(|| format!(r"{host}\{share}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_share_on_another_computer_is_refused_whichever_way_its_slashes_lean() {
        for path in [
            r"\\host\share\a.txt",
            "//host/share/a.txt",
            r"\/host/share",
            r"\\evil.example@SSL\DavWWWRoot\a.png",
            r"\\host",
            r"\\nas\pp\a.txt",
            r"\\nas.evil.example\p\app\a.txt",
        ] {
            for workspace in [None, Some(r"C:\app"), Some(r"\\nas\p\app")] {
                let why = on_windows(path, workspace).unwrap_or_default();
                assert!(why.contains("another computer"), "{path}: {why}");
            }
        }
    }

    #[test]
    fn a_path_into_a_project_on_a_share_is_allowed() {
        for workspace in [r"\\nas\p\app", r"\\?\UNC\NAS\P\app", "//nas/p/app"] {
            for path in [r"\\nas\p\app\src\main.rs", "//NAS/P/app/media/x.png"] {
                assert_eq!(
                    on_windows(path, Some(workspace)),
                    None,
                    "{workspace}: {path}"
                );
            }
            // The share is let through, not whatever is named on it.
            let why = on_windows(r"\\nas\p\app\CON", Some(workspace)).unwrap_or_default();
            assert!(why.contains("keeps for a device"), "{workspace}: {why}");
        }
    }

    #[test]
    fn device_paths_are_refused() {
        for path in [
            r"\\.\PhysicalDrive0",
            r"\\?\C:\Users\a.txt",
            "//?/C:/Users/a.txt",
            r"\\?\UNC\host\share\a.txt",
            r"\??\C:\a.txt",
        ] {
            let why = on_windows(path, None).unwrap_or_default();
            assert!(why.contains("device path"), "{path}: {why}");
        }
    }

    #[test]
    fn a_hidden_stream_is_refused_but_a_drive_is_not() {
        for path in [
            "notes.txt:hidden",
            r"C:\x\a.txt::$DATA",
            r"src:x\a.rs",
            "C:/x/a.txt:hidden",
        ] {
            let why = on_windows(path, None).unwrap_or_default();
            assert!(why.contains("hidden stream"), "{path}: {why}");
        }
        for path in [r"C:\Users\a.txt", "c:/x/y.md", "C:a.txt", "C:"] {
            assert_eq!(on_windows(path, None), None, "{path}");
        }
    }

    #[test]
    fn device_names_are_refused_in_any_case_and_with_any_extension() {
        for path in [
            "CON",
            "nul.txt",
            r"src\aux.rs",
            "Com1.log",
            "lpt9",
            "COM¹",
            "conout$",
            "con .txt",
            "prn.tar.gz",
            "nul/a.txt",
        ] {
            let why = on_windows(path, None).unwrap_or_default();
            assert!(why.contains("keeps for a device"), "{path}: {why}");
        }
        for path in [
            "console.log",
            "com10.txt",
            "lpt.txt",
            "COM",
            "nullable.rs",
            "auxiliary/a.md",
            "icon.png",
        ] {
            assert_eq!(on_windows(path, None), None, "{path}");
        }
    }

    #[test]
    fn a_name_ending_in_a_dot_or_a_space_is_refused_but_dot_folders_are_not() {
        for path in [
            "letronna.exe.",
            "notes.md ",
            "dir./a.txt",
            r"a\...\b",
            "a /b",
        ] {
            let why = on_windows(path, None).unwrap_or_default();
            assert!(why.contains("dot or a space"), "{path}: {why}");
        }
        for path in [
            ".",
            "..",
            "./src/../a.txt",
            ".env.example",
            "a/.hidden/b",
            "src/",
            r"C:\x\",
            "",
        ] {
            assert_eq!(on_windows(path, None), None, "{path}");
        }
    }

    #[test]
    fn only_windows_refuses_anything() {
        let project = Path::new(r"\\nas\p\app");
        assert_eq!(
            unsafe_raw(r"\\host\share\a.txt", Some(project)).is_some(),
            cfg!(windows)
        );
        assert_eq!(unsafe_raw(r"\\nas\p\app\a.txt", Some(project)), None);
        assert_eq!(unsafe_raw("src/main.rs", None), None);
    }
}
