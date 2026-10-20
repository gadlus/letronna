//! Pictures that reach the box without a path typed: one on the clipboard,
//! or a file dragged in from a file manager, which a terminal pastes as the
//! file's path.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The kinds of picture `/image` takes, by extension.
const KINDS: [&str; 6] = ["png", "jpg", "jpeg", "gif", "webp", "bmp"];

/// The folder a picture taken off the clipboard waits in for `/image`,
/// which keeps its own copy with the session's media and removes this one.
pub fn pasted_folder() -> PathBuf {
    letronna_core::chat::data_dir().join("pasted")
}

/// Whether `path` is a picture taken off the clipboard, waiting to be
/// attached.
pub fn pasted(path: &Path) -> bool {
    path.parent() == Some(pasted_folder().as_path())
}

/// Write the picture on the clipboard to `to` as a PNG, saying whether there
/// was one: AppleScript asks for it on macOS, PowerShell on Windows, and
/// wl-paste or xclip, whichever is there, on Linux. Nothing new is needed
/// for it, and text on the clipboard is left to the terminal's own paste.
pub fn from_clipboard(to: &Path) -> bool {
    let took = if cfg!(target_os = "macos") {
        macos(to)
    } else if cfg!(windows) {
        windows(to)
    } else {
        linux(to)
    };
    let written = took && std::fs::metadata(to).is_ok_and(|file| file.len() > 0);
    if !written {
        let _ = std::fs::remove_file(to);
    }
    written
}

fn macos(to: &Path) -> bool {
    // A clipboard holding no picture fails the write inside the try, which
    // leaves an empty file, read above as none.
    let file = to
        .display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let lines = [
        format!("set f to (open for access POSIX file \"{file}\" with write permission)"),
        "try".to_owned(),
        "write (the clipboard as «class PNGf») to f".to_owned(),
        "end try".to_owned(),
        "close access f".to_owned(),
    ];
    let mut command = Command::new("osascript");
    for line in &lines {
        command.arg("-e").arg(line);
    }
    quietly(&mut command)
}

fn windows(to: &Path) -> bool {
    let file = to.display().to_string().replace('\'', "''");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms, System.Drawing; \
         $picture = [Windows.Forms.Clipboard]::GetImage(); \
         if ($picture) {{ $picture.Save('{file}', [System.Drawing.Imaging.ImageFormat]::Png) }}"
    );
    // The clipboard is only reachable from a single-threaded apartment.
    quietly(Command::new("powershell").args(["-NoProfile", "-STA", "-Command", &script]))
}

fn linux(to: &Path) -> bool {
    let readers: [(&str, &[&str]); 2] = [
        ("wl-paste", &["--type", "image/png"]),
        (
            "xclip",
            &["-selection", "clipboard", "-t", "image/png", "-o"],
        ),
    ];
    readers.iter().any(|(program, args)| {
        let Ok(file) = std::fs::File::create(to) else {
            return false;
        };
        let read = Command::new(program)
            .args(*args)
            .stdin(Stdio::null())
            .stdout(file)
            .stderr(Stdio::null())
            .status();
        read.is_ok_and(|status| status.success())
            && std::fs::metadata(to).is_ok_and(|file| file.len() > 0)
    })
}

/// Run `command` with nothing shown, saying whether it worked.
fn quietly(command: &mut Command) -> bool {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

/// The picture file a pasted line is the path of, as a terminal pastes a
/// file dragged into it: whole, in quotes, with its spaces escaped, or as a
/// file:// address.
pub fn dropped(text: &str) -> Option<PathBuf> {
    let line = text.trim();
    if line.is_empty() || line.contains('\n') {
        return None;
    }
    let quoted = ['"', '\''].iter().find_map(|&quote| {
        line.strip_prefix(quote)
            .and_then(|inner| inner.strip_suffix(quote))
    });
    let line = match quoted {
        Some(inner) => inner.to_owned(),
        // A backslash is the separator on Windows, not an escape.
        None if cfg!(windows) => line.to_owned(),
        None => line.replace("\\ ", " "),
    };
    let line = match line.strip_prefix("file://") {
        Some(address) => address_path(address, cfg!(windows))?,
        None => line,
    };
    let path = PathBuf::from(line);
    let kind = path.extension()?.to_str()?.to_lowercase();
    (KINDS.contains(&kind.as_str()) && path.is_absolute() && path.is_file()).then_some(path)
}

/// The path a file:// address names, from what follows `file://`, as
/// terminals send one: escaped with `%XX`, its host empty or `localhost`
/// (another host's leaves no absolute path), and on Windows with a slash
/// before the drive, as in `/C:/x.png`.
fn address_path(address: &str, windows: bool) -> Option<String> {
    let address = address.strip_prefix("localhost").unwrap_or(address);
    let bytes = address.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%' {
            let hex = address.get(at + 1..at + 3)?;
            decoded.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            decoded.push(bytes[at]);
            at += 1;
        }
    }
    let path = String::from_utf8(decoded).ok()?;
    let drive = path.as_bytes();
    if windows
        && drive.len() > 2
        && drive[0] == b'/'
        && drive[1].is_ascii_alphabetic()
        && drive[2] == b':'
    {
        return Some(path[1..].to_owned());
    }
    Some(path)
}

/// The line that attaches the picture at `path`, as typing `/image` with
/// it would: quoted, which `/image` takes off as one pair, so a quote or a
/// space in the name stays part of it.
pub fn image_line(path: &Path) -> String {
    format!("/image \"{}\"", path.display())
}

#[cfg(test)]
mod tests {
    use super::{address_path, dropped, pasted, pasted_folder};

    #[test]
    fn only_a_picture_waiting_in_the_pasted_folder_is_taken_for_one() {
        assert!(pasted(&pasted_folder().join("1791995592021.png")));
        assert!(!pasted(&pasted_folder()));
        assert!(!pasted(std::path::Path::new("/Users/someone/shot.png")));
    }

    #[test]
    fn a_dragged_in_picture_is_known_by_its_path() {
        let dir = std::env::temp_dir().join(format!("letronna-dropped-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let shot = dir.join("a shot.png");
        std::fs::write(&shot, b"not really a png").unwrap();
        let plain = shot.display().to_string();
        assert_eq!(dropped(&plain), Some(shot.clone()));
        assert_eq!(dropped(&format!("'{plain}'")), Some(shot.clone()));
        #[cfg(unix)]
        assert_eq!(dropped(&plain.replace(' ', "\\ ")), Some(shot.clone()));
        assert_eq!(dropped(&format!("file://{plain}")), Some(shot.clone()));
        // As terminals send an address: escaped, and from this machine.
        #[cfg(unix)]
        {
            let escaped = plain.replace(' ', "%20");
            assert_eq!(dropped(&format!("file://{escaped}")), Some(shot.clone()));
            assert_eq!(
                dropped(&format!("file://localhost{escaped}")),
                Some(shot.clone())
            );
            assert_eq!(dropped(&format!("file://elsewhere{escaped}")), None);
        }
        // Words, a missing file, or a file of another kind are pasted as text.
        assert_eq!(dropped("see the screenshot"), None);
        assert_eq!(dropped(&dir.join("gone.png").display().to_string()), None);
        let notes = dir.join("notes.txt");
        std::fs::write(&notes, b"text").unwrap();
        assert_eq!(dropped(&notes.display().to_string()), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_file_address_comes_back_as_its_path() {
        assert_eq!(
            address_path("/Users/me/a%20shot.png", false).as_deref(),
            Some("/Users/me/a shot.png")
        );
        assert_eq!(
            address_path("localhost/tmp/%E5%9B%BE.png", false).as_deref(),
            Some("/tmp/图.png")
        );
        // The slash before a drive goes on Windows only.
        assert_eq!(
            address_path("/C:/Users/me/x.png", true).as_deref(),
            Some("C:/Users/me/x.png")
        );
        assert_eq!(
            address_path("/C:/x.png", false).as_deref(),
            Some("/C:/x.png")
        );
        // A broken escape is no address.
        assert_eq!(address_path("/tmp/a%2", false), None);
        assert_eq!(address_path("/tmp/a%zz.png", false), None);
    }
}
