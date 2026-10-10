//! Putting Letronna where it belongs, once.
//!
//! Both halves of the program live in one folder under the user's own account
//! -- no administrator, no service, nothing outside the home directory. The
//! window goes in the folder itself and the terminal in `bin`, which is what
//! goes on PATH, because Windows filenames do not care about case and
//! `letronna.exe` beside `Letronna.exe` is one file overwriting the other.
//! On Windows the terminal is `letronnacli.exe` there, so Task Manager tells
//! the two apart, and small shims keep the command `letronna`.
//!
//! Signing in comes first. Not because anything here needs a token -- it does
//! not -- but because an install that asks afterwards is an install people
//! finish and then abandon at the question.

use letronna_core::auth;

use crate::commands::open_in_browser;
use crate::render;
use crate::session::Runner;

/// What to put on the machine.
#[derive(Clone, Copy, PartialEq)]
pub enum Parts {
    Both,
    Window,
    Terminal,
}

impl Parts {
    fn wants_window(self) -> bool {
        self != Parts::Terminal
    }
    fn wants_terminal(self) -> bool {
        self != Parts::Window
    }
}

/// Where everything goes.
pub fn home() -> std::path::PathBuf {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join("Programs")
            .join("Letronna")
    } else {
        std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(std::env::temp_dir)
            .join(".local")
            .join("share")
            .join("letronna")
    }
}

/// Ask which parts to put on the machine, and put them there.
pub fn offer(runner: &mut Runner, number: usize, of: usize) {
    render::blank();
    render::said(
        &render::blue("●"),
        &format!(
            "{}  {}",
            render::bold("Install"),
            render::dim(&format!("{number} of {of}"))
        ),
    );
    render::line(&render::dim(&format!("Into {}", home().display())));

    let Some(parts) = choose_parts(runner) else {
        render::line(&render::dim(
            "Nothing installed. `letronna setup` offers it again.",
        ));
        return;
    };

    match runner.bar.busy("putting it in place", move || place(parts)) {
        Ok(done) => {
            for line in done {
                render::line(&render::dim(&line));
            }
            if parts.wants_terminal() {
                render::line(&render::dim(
                    "Open a new terminal for `letronna` to be found.",
                ));
            }
        }
        Err(error) => render::line(&render::red(&error)),
    }
}

fn choose_parts(runner: &mut Runner) -> Option<Parts> {
    let picked = runner.bar.choose(
        "What should go on this computer?",
        Vec::new(),
        &[
            "Both — the window and the terminal",
            "The window only",
            "The terminal only",
            "Neither, stop here",
        ],
    )?;
    match picked {
        0 => Some(Parts::Both),
        1 => Some(Parts::Window),
        2 => Some(Parts::Terminal),
        _ => None,
    }
}

/// Copy the binaries in, and put the terminal on PATH.
fn place(parts: Parts) -> Result<Vec<String>, String> {
    let root = home();
    std::fs::create_dir_all(&root).map_err(|e| format!("Cannot make {}: {e}", root.display()))?;
    let mut done = Vec::new();
    let here = std::env::current_exe().map_err(|e| e.to_string())?;
    let beside = here
        .parent()
        .ok_or("Cannot tell where this is running from.")?;

    if parts.wants_terminal() {
        let bin = root.join("bin");
        std::fs::create_dir_all(&bin).map_err(|e| e.to_string())?;
        let target = bin.join(terminal_name());
        #[cfg(windows)]
        letronna_core::update::install_terminal(&bin, Some(here.as_path()))?;
        // Copying onto a running program fails on Windows, and the one place
        // that happens is installing over the copy that is doing the asking.
        #[cfg(not(windows))]
        if target != here {
            std::fs::copy(&here, &target)
                .map_err(|e| format!("Cannot write {}: {e}", target.display()))?;
        }
        done.push(format!("terminal  {}", target.display()));
        match on_path(&bin) {
            Ok(true) => done.push(format!("path      {}", bin.display())),
            Ok(false) => done.push("path      already there".into()),
            Err(error) => done.push(format!("path      not set: {error}")),
        }
    }

    if parts.wants_window() {
        let source = beside.join(window_build_name());
        let target = root.join(window_name());
        if source.exists() {
            std::fs::copy(&source, &target)
                .map_err(|e| format!("Cannot write {}: {e}", target.display()))?;
            done.push(format!("window    {}", target.display()));
        } else {
            done.push(format!(
                "window    not found beside this ({})",
                source.display()
            ));
        }
    }

    Ok(done)
}

/// Add a folder to the user's own PATH. True when it was actually added.
#[cfg(windows)]
fn on_path(dir: &std::path::Path) -> Result<bool, String> {
    let dir = dir.display().to_string();
    // Read, compare, write: the user's PATH only, never the machine's.
    let script = format!(
        "$p=[Environment]::GetEnvironmentVariable('Path','User'); \
         if ($p -split ';' -contains '{dir}') {{ 'already' }} \
         else {{ [Environment]::SetEnvironmentVariable('Path', ($p.TrimEnd(';') + ';{dir}'), 'User'); 'added' }}"
    );
    let out = std::process::Command::new("powershell")
        .args(["-NoProfile", "-Command", &script])
        .output()
        .map_err(|e| e.to_string())?;
    Ok(String::from_utf8_lossy(&out.stdout).trim() == "added")
}

/// On Linux the shell reads its own files, so this writes a line into the
/// profile rather than setting anything itself.
#[cfg(not(windows))]
fn on_path(dir: &std::path::Path) -> Result<bool, String> {
    use std::io::Write;
    let home = std::env::var_os("HOME").ok_or("No home directory.")?;
    let profile = std::path::PathBuf::from(home).join(".profile");
    let line = format!("export PATH=\"$PATH:{}\"", dir.display());
    if std::fs::read_to_string(&profile)
        .map(|text| text.contains(&line))
        .unwrap_or(false)
    {
        return Ok(false);
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&profile)
        .map_err(|e| e.to_string())?;
    writeln!(file, "\n# Letronna\n{line}").map_err(|e| e.to_string())?;
    Ok(true)
}

fn terminal_name() -> &'static str {
    if cfg!(windows) {
        letronna_core::update::TERMINAL_EXE
    } else {
        "letronna"
    }
}

/// What the window is called in a build folder.
fn window_build_name() -> &'static str {
    if cfg!(windows) {
        "letronna-gui.exe"
    } else {
        "letronna-gui"
    }
}

/// What it is called once installed.
fn window_name() -> &'static str {
    if cfg!(windows) {
        "Letronna.exe"
    } else {
        "letronna-gui"
    }
}

/// Take it all off again.
pub fn remove() -> Result<Vec<String>, String> {
    let root = home();
    if !root.exists() {
        return Ok(vec!["Nothing installed here.".into()]);
    }
    let mut done = Vec::new();
    // The terminal may be the thing running this, so what cannot go now is
    // left rather than failing the whole uninstall.
    match std::fs::remove_dir_all(&root) {
        Ok(()) => done.push(format!("removed  {}", root.display())),
        Err(error) => done.push(format!(
            "left     {} ({error}); it is in use, delete it once Letronna is closed",
            root.display()
        )),
    }
    done.push(match auth::sign_out() {
        Ok(None) => "signed out".into(),
        Ok(Some(note)) => format!("signed out here. {note}"),
        Err(why) => format!("left     the sign-in: {why}"),
    });
    let _ = open_in_browser;
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::Parts;

    #[test]
    fn each_choice_asks_for_what_it_says() {
        assert!(Parts::Both.wants_window() && Parts::Both.wants_terminal());
        assert!(Parts::Window.wants_window() && !Parts::Window.wants_terminal());
        assert!(!Parts::Terminal.wants_window() && Parts::Terminal.wants_terminal());
    }

    #[test]
    fn everything_lands_under_the_user() {
        let home = super::home();
        assert!(home.is_absolute());
        // Never a system folder: no administrator, no service.
        let text = home.display().to_string().to_lowercase();
        assert!(!text.contains("program files") && !text.contains("/usr/"));
    }
}
