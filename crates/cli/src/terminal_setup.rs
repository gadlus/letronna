//! `/terminal-setup`: what this terminal makes of the keys the box uses,
//! and what to change where it falls short.
//!
//! \+enter and ctrl+j make a new line in every terminal. Shift+enter needs
//! the terminal to say shift was held, which most do once asked, and the
//! box asks; on a Mac the alt keys need Option to act as Meta. Each
//! terminal keeps those settings its own way, so this says which terminal
//! it is, what works already and the setting to change. Only Apple's
//! Terminal, whose settings are a file of the user's own, is changed for
//! them, and only after a yes: the file is copied first, and the way back
//! is said.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use crate::render;
use crate::session::Runner;

/// The terminals told apart, by the variables they set.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Terminal {
    Apple,
    ITerm,
    Ghostty,
    WezTerm,
    Kitty,
    /// VS Code's, and Cursor's and Windsurf's, which say they are it.
    VsCode,
    Windows,
    Other,
}

impl Terminal {
    fn name(self) -> &'static str {
        match self {
            Terminal::Apple => "Apple's Terminal",
            Terminal::ITerm => "iTerm2",
            Terminal::Ghostty => "Ghostty",
            Terminal::WezTerm => "WezTerm",
            Terminal::Kitty => "kitty",
            Terminal::VsCode => "VS Code's terminal",
            Terminal::Windows => "Windows Terminal",
            Terminal::Other => "this terminal",
        }
    }
}

/// The terminal whose variables `var` reads.
fn which(var: impl Fn(&str) -> Option<String>) -> Terminal {
    if var("KITTY_WINDOW_ID").is_some() || var("TERM").as_deref() == Some("xterm-kitty") {
        return Terminal::Kitty;
    }
    if var("WT_SESSION").is_some() {
        return Terminal::Windows;
    }
    match var("TERM_PROGRAM").as_deref() {
        Some("Apple_Terminal") => Terminal::Apple,
        Some("iTerm.app") => Terminal::ITerm,
        Some("ghostty") => Terminal::Ghostty,
        Some("WezTerm") => Terminal::WezTerm,
        Some("vscode") => Terminal::VsCode,
        _ => Terminal::Other,
    }
}

/// What to change in `terminal` for shift+enter, when it does not reach the
/// box as itself; `macos` is the Mac's major version, where there is one.
fn shift_enter_fix(terminal: Terminal, macos: Option<u32>) -> Option<String> {
    Some(match terminal {
        // Shift+return makes a new line in Terminal from macOS 27; before
        // that, option+return does once Option acts as Meta.
        Terminal::Apple if macos.is_some_and(|major| major >= 27) => return None,
        Terminal::Apple => "Terminal has no shift+enter of its own before macOS 27: \
                            option+enter makes a new line once Option acts as Meta"
            .to_owned(),
        Terminal::VsCode => "add to keybindings.json (Preferences: Open Keyboard Shortcuts \
                             (JSON)): { \"key\": \"shift+enter\", \"command\": \
                             \"workbench.action.terminal.sendSequence\", \"args\": { \"text\": \
                             \"\\u001b\\r\" }, \"when\": \"terminalFocus\" }"
            .to_owned(),
        // These say which keys were held once asked, and the box asks.
        Terminal::ITerm | Terminal::Ghostty | Terminal::WezTerm | Terminal::Kitty => {
            format!(
                "{} tells it apart once asked, but did not answer this time; Letronna \
                 asks again when it starts",
                terminal.name()
            )
        }
        Terminal::Windows => return None,
        Terminal::Other => "it may not tell shift+enter from enter".to_owned(),
    })
}

/// Where `terminal` is told to send Option as Meta, for the alt keys.
fn option_fix(terminal: Terminal) -> Option<&'static str> {
    Some(match terminal {
        Terminal::Apple => {
            "Terminal › Settings › Profiles › your profile › Keyboard › Use Option as Meta key"
        }
        Terminal::ITerm => "Settings › Profiles › Keys › General › Left Option key: Esc+",
        Terminal::Ghostty => "macos-option-as-alt = true in Ghostty's config",
        Terminal::Kitty => "macos_option_as_alt yes in kitty.conf",
        Terminal::VsCode => "\"terminal.integrated.macOptionIsMeta\": true in settings.json",
        Terminal::WezTerm | Terminal::Windows | Terminal::Other => return None,
    })
}

/// PlistBuddy, which reads and changes one key of a plist in place.
const PLIST_BUDDY: &str = "/usr/libexec/PlistBuddy";

/// Apple's Terminal's settings in the home folder `home`.
fn apple_plist(home: &Path) -> PathBuf {
    home.join("Library/Preferences/com.apple.Terminal.plist")
}

/// What PlistBuddy prints for `key` of `plist`, or `None` where it has none.
fn plist_value(plist: &Path, key: &str) -> Option<String> {
    let out = Command::new(PLIST_BUDDY)
        .args(["-c", &format!("Print {key}")])
        .arg(plist)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// The profiles new windows open with, the default and the one at start,
/// that do not yet send Option as Meta. One whose name has a quote in it
/// cannot be named to PlistBuddy, so is left to be changed by hand.
fn apple_profiles_without_meta(plist: &Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for key in [":'Default Window Settings'", ":'Startup Window Settings'"] {
        if let Some(name) = plist_value(plist, key)
            && !name.is_empty()
            && !name.contains('\'')
            && !names.contains(&name)
        {
            names.push(name);
        }
    }
    names.retain(|name| {
        let key = format!(":'Window Settings':'{name}':useOptionAsMetaKey");
        plist_value(plist, &key).as_deref() != Some("true")
    });
    names
}

/// Make `profiles` of `plist` send Option as Meta, a copy of the file kept
/// at `backup` first and put back if any change fails. `reload` restarts
/// the settings service, so Terminal does not write over the change with
/// what it holds; Terminal itself takes it on its next start.
fn switch_meta_on(
    plist: &Path,
    profiles: &[String],
    backup: &Path,
    reload: bool,
) -> Result<(), String> {
    if let Some(folder) = backup.parent() {
        std::fs::create_dir_all(folder).map_err(|e| e.to_string())?;
    }
    std::fs::copy(plist, backup).map_err(|e| format!("Could not copy its settings first: {e}"))?;
    for profile in profiles {
        let key = format!(":'Window Settings':'{profile}':useOptionAsMetaKey");
        let done = [format!("Set {key} true"), format!("Add {key} bool true")]
            .iter()
            .any(|change| {
                Command::new(PLIST_BUDDY)
                    .args(["-c", change])
                    .arg(plist)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .is_ok_and(|status| status.success())
            });
        if !done {
            let _ = std::fs::copy(backup, plist);
            return Err(format!(
                "Could not change the profile {profile}; its settings are as they were."
            ));
        }
    }
    if reload {
        let _ = Command::new("killall")
            .arg("cfprefsd")
            .stderr(Stdio::null())
            .status();
    }
    Ok(())
}

/// The Mac's major version, such as 27, where this is a Mac.
fn macos_major() -> Option<u32> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = Command::new("sw_vers")
        .arg("-productVersion")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// The home folder of the account this runs as, which HOME may not be: a
/// change made under another HOME is no file the settings service holds.
#[cfg(unix)]
fn account_home() -> Option<PathBuf> {
    // SAFETY: getpwuid gives a record the C library owns, or null; its
    // home is copied out before anything else can call it.
    unsafe {
        let record = libc::getpwuid(libc::getuid());
        if record.is_null() || (*record).pw_dir.is_null() {
            return None;
        }
        let dir = std::ffi::CStr::from_ptr((*record).pw_dir);
        Some(PathBuf::from(dir.to_string_lossy().into_owned()))
    }
}

#[cfg(not(unix))]
fn account_home() -> Option<PathBuf> {
    None
}

/// `/terminal-setup`: say what this terminal does with the keys, and what to
/// change; in Apple's Terminal, offer to make Option act as Meta.
pub fn terminal_setup(runner: &mut Runner) {
    let var = |name: &str| std::env::var(name).ok();
    let terminal = which(var);
    let macos = macos_major();
    let shift = crate::screen::shift_enter();
    render::blank();
    render::said(
        &render::blue("●"),
        &render::bold(&format!("Terminal setup: {}", terminal.name())),
    );
    let new_line = match (shift, shift_enter_fix(terminal, macos)) {
        (true, _) | (false, None) => "shift+enter, \\+enter or ctrl+j".to_owned(),
        (false, Some(_)) => "\\+enter or ctrl+j".to_owned(),
    };
    render::line(&format!("  new line   {new_line}"));
    if let (false, Some(fix)) = (shift, shift_enter_fix(terminal, macos)) {
        render::line(&render::dim(&format!(
            "             for shift+enter: {fix}"
        )));
    }
    if var("TMUX").is_some() {
        render::line(&render::dim(
            "             inside tmux, keys are reported once tmux is told: \
             set -g extended-keys on",
        ));
    }
    // Apple's Terminal's own settings say whether it is done already.
    let plist = std::env::var_os("HOME").map(|home| apple_plist(Path::new(&home)));
    let apple = plist.filter(|plist| terminal == Terminal::Apple && plist.exists());
    let without = apple.as_deref().map(apple_profiles_without_meta);
    if without.as_ref().is_some_and(Vec::is_empty) {
        render::line(
            "  option     acts as Meta, so alt+b/f, alt+enter and the other alt keys work",
        );
    } else if cfg!(target_os = "macos")
        && let Some(fix) = option_fix(terminal)
    {
        render::line(
            "  option     alt+b/f, alt+enter and the other alt keys need Option to act as Meta",
        );
        render::line(&render::dim(&format!("             {fix}")));
    }
    if let (Some(plist), Some(profiles)) = (apple, without)
        && !profiles.is_empty()
    {
        offer_meta(runner, &plist, &profiles);
    }
}

/// Offer to make Option act as Meta in `profiles` of Apple's Terminal's
/// settings at `plist`, and do it on a yes.
fn offer_meta(runner: &mut Runner, plist: &Path, profiles: &[String]) {
    let names = profiles.join(" and ");
    let detail = vec![
        format!("Ticks Use Option as Meta key in the profile {names}."),
        "Option then no longer types characters such as é or © in Terminal.".to_owned(),
        "Its settings are copied first; Terminal takes it on its next start.".to_owned(),
    ];
    let question = "Make Option act as Meta in Terminal?";
    let options = ["Make it so", "Leave it as it is"];
    if runner.bar.choose(question, detail, &options) != Some(0) {
        return;
    }
    let at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_secs());
    let backup = letronna_core::chat::data_dir()
        .join("terminal-setup")
        .join(format!("com.apple.Terminal-{at}.plist"));
    let reload = account_home().is_some_and(|account| plist == apple_plist(&account));
    match switch_meta_on(plist, profiles, &backup, reload) {
        Ok(()) => {
            render::line(&render::green(&format!(
                "Option acts as Meta in {names} once Terminal starts again."
            )));
            render::line(&render::dim(&format!(
                "To undo it, untick the box in Terminal › Settings › Profiles › Keyboard, or \
                 run: defaults import com.apple.Terminal {}",
                backup.display()
            )));
        }
        Err(why) => render::line(&render::red(&why)),
    }
}

#[cfg(test)]
mod tests {
    use super::{Terminal, shift_enter_fix, which};
    // Only the macOS tests change Terminal's settings.
    #[cfg(target_os = "macos")]
    use super::switch_meta_on;

    fn in_terminal(vars: &[(&str, &str)]) -> Terminal {
        which(|name| {
            vars.iter()
                .find(|(set, _)| *set == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn each_terminal_is_told_apart_by_what_it_sets() {
        assert_eq!(
            in_terminal(&[("TERM_PROGRAM", "Apple_Terminal")]),
            Terminal::Apple
        );
        assert_eq!(
            in_terminal(&[("TERM_PROGRAM", "iTerm.app")]),
            Terminal::ITerm
        );
        assert_eq!(
            in_terminal(&[("TERM_PROGRAM", "ghostty")]),
            Terminal::Ghostty
        );
        assert_eq!(in_terminal(&[("TERM_PROGRAM", "vscode")]), Terminal::VsCode);
        assert_eq!(in_terminal(&[("KITTY_WINDOW_ID", "1")]), Terminal::Kitty);
        assert_eq!(in_terminal(&[("WT_SESSION", "x")]), Terminal::Windows);
        assert_eq!(in_terminal(&[]), Terminal::Other);
    }

    #[test]
    fn apples_terminal_needs_no_shift_enter_fix_from_macos_27() {
        assert_eq!(shift_enter_fix(Terminal::Apple, Some(27)), None);
        assert!(
            shift_enter_fix(Terminal::Apple, Some(15))
                .is_some_and(|fix| fix.contains("option+enter"))
        );
        assert!(
            shift_enter_fix(Terminal::VsCode, None).is_some_and(|fix| fix.contains("sendSequence"))
        );
        assert_eq!(shift_enter_fix(Terminal::Windows, None), None);
    }

    /// A Terminal settings file with `default` as its default profile and a
    /// second profile that sends Option as Meta already.
    #[cfg(target_os = "macos")]
    fn settings(dir: &std::path::Path, default: &str) -> std::path::PathBuf {
        let plist = dir.join("com.apple.Terminal.plist");
        let _ = std::fs::remove_file(&plist);
        for change in [
            format!("Add :'Default Window Settings' string {default}"),
            format!("Add :'Startup Window Settings' string {default}"),
            "Add :'Window Settings' dict".to_owned(),
            format!("Add :'Window Settings':'{default}' dict"),
            format!("Add :'Window Settings':'{default}':name string {default}"),
            "Add :'Window Settings':Pro dict".to_owned(),
            "Add :'Window Settings':Pro:useOptionAsMetaKey bool true".to_owned(),
        ] {
            let done = std::process::Command::new(super::PLIST_BUDDY)
                .args(["-c", &change])
                .arg(&plist)
                .output()
                .unwrap();
            assert!(done.status.success(), "{change}");
        }
        plist
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn option_as_meta_is_switched_on_in_a_copy_of_the_settings_kept_first() {
        let dir =
            std::env::temp_dir().join(format!("letronna-terminal-setup-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let plist = settings(&dir, "Clear Dark");
        assert_eq!(
            super::apple_profiles_without_meta(&plist),
            vec!["Clear Dark".to_owned()]
        );
        let before = std::fs::read(&plist).unwrap();
        let backup = dir.join("kept").join("before.plist");
        switch_meta_on(&plist, &["Clear Dark".to_owned()], &backup, false).unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), before);
        assert!(super::apple_profiles_without_meta(&plist).is_empty());
        let pro = super::plist_value(&plist, ":'Window Settings':Pro:useOptionAsMetaKey");
        assert_eq!(pro.as_deref(), Some("true"));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
