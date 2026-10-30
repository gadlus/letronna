//! What the window is called.
//!
//! A terminal tab is often the only part of Letronna visible once you have
//! switched to something else, so the title says whether it still wants you.
//! Idle it is just the name; working, a mark turns in front of it, which is
//! enough to catch the eye from across a taskbar without being a notification.
//!
//! A terminal that draws progress in its tab (OSC 9;4) shows a running bar
//! there too, while a turn runs. Only those known to draw it are asked:
//! iTerm2 before 3.6.6 takes any OSC 9 for a notification, and would pop
//! one up saying "4;3".

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::render;

const NAME: &str = "Letronna";

/// The open session's name, which leads the title: a tab is read from its
/// start, and a row of tabs all reading "Letronna" says nothing.
static SESSION: Mutex<String> = Mutex::new(String::new());

/// Whether a question waits for the user: a card or a line asked for while
/// it works, or one nobody at the keyboard asked for.
static WAITING: AtomicBool = AtomicBool::new(false);

/// Whether a turn is running, so the busy title is left to the wheel.
static BUSY: AtomicBool = AtomicBool::new(false);

/// What the title says now, with `mark` turning in front while it works.
fn said(mark: Option<&str>) -> String {
    let session = SESSION.lock().map(|name| name.clone()).unwrap_or_default();
    titled(&session, WAITING.load(Ordering::Relaxed), mark)
}

/// The title for `session`: its name first, then Letronna's; a mark in
/// front while it works, and a word that it waits for you when it does.
fn titled(session: &str, waiting: bool, mark: Option<&str>) -> String {
    let named = match session.is_empty() {
        true => NAME.to_owned(),
        false => format!("{session} · {NAME}"),
    };
    if waiting {
        return format!("●{TEXT_PRESENTATION} waiting for you · {named}");
    }
    match mark {
        Some(mark) => format!("{mark}{TEXT_PRESENTATION} {named}"),
        None => named,
    }
}

/// Keep the title the terminal had, so leaving can put it back where the
/// terminal keeps a stack of them.
pub fn save() {
    if render::colour() {
        print!("\x1b[22;0t");
    }
}

/// Put back the title the terminal had before: cleared first, for a
/// terminal that keeps no stack, then taken off the stack by one that does.
pub fn restore() {
    progress(false);
    render::title("");
    if render::colour() {
        print!("\x1b[23;0t");
    }
}

/// Lead the title with the open session's name, given on one line.
pub fn name(session: &str) {
    if let Ok(mut name) = SESSION.lock() {
        *name = session.to_owned();
    }
    if !BUSY.load(Ordering::Relaxed) {
        idle();
    }
}

/// Say whether a question waits for the user.
pub fn waiting(on: bool) {
    if WAITING.swap(on, Ordering::Relaxed) != on && !BUSY.load(Ordering::Relaxed) {
        idle();
    }
}

/// The turning mark: the same braille wheel the waiting line uses, so the
/// tab and the window agree. Stars and sparkles look like the right idea and
/// are the wrong characters -- terminals hand those to an emoji font, which
/// paints its own colour behind them and ignores the theme entirely. Braille
/// is plain text and takes the colour of the tab it sits in.
const MARKS: [&str; 8] = [
    "\u{2744}", "\u{2745}", "\u{2746}", "\u{2745}", "\u{2744}", "\u{2743}", "\u{2742}", "\u{2743}",
];

/// Spelled with the variation selector that asks for text rather than
/// emoji. Without it a terminal hands the character to an emoji font, which
/// paints its own colours -- that green block behind the mark -- and pays no
/// attention to the theme. With it the glyph is ordinary text and takes the
/// tab's own colours, which is as close to ours as a title can be: a tab
/// title is plain text and the terminal, not the program, decides how it is
/// drawn.
const TEXT_PRESENTATION: char = '\u{FE0E}';

/// Slower than the spinner on the screen. A title is read in passing, and
/// redrawing it eleven times a second would be work nobody sees.
const TICK: Duration = Duration::from_millis(400);

/// Whether the terminal whose variables `var` reads draws OSC 9;4 progress
/// in its tab: Windows Terminal, ConEmu, Ghostty from 1.2 and iTerm2 from
/// 3.6.6. Inside tmux or screen none is, since they keep it to themselves.
fn draws_progress(var: impl Fn(&str) -> Option<String>) -> bool {
    if var("TMUX").is_some() || var("STY").is_some() {
        return false;
    }
    if var("WT_SESSION").is_some() || var("ConEmuANSI").as_deref() == Some("ON") {
        return true;
    }
    let version = var("TERM_PROGRAM_VERSION").unwrap_or_default();
    match var("TERM_PROGRAM").as_deref() {
        Some("ghostty") => at_least(&version, &[1, 2, 0]),
        Some("iTerm.app") => at_least(&version, &[3, 6, 6]),
        _ => false,
    }
}

/// Whether `version`, such as "3.6.6" or "1.2.0-main+abc", is `least` or
/// later; one that cannot be read is not.
fn at_least(version: &str, least: &[u32]) -> bool {
    let parts: Option<Vec<u32>> = version
        .split('.')
        .take(least.len())
        .map(|part| {
            let digits: String = part.chars().take_while(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
        .collect();
    parts.is_some_and(|parts| parts.len() == least.len() && parts.as_slice() >= least)
}

/// Show a running bar in the tab, or take it away, where the terminal draws
/// one. Sent again with each turn of the mark, since Ghostty drops a bar
/// that is not kept up.
fn progress(running: bool) {
    static DRAWN: OnceLock<bool> = OnceLock::new();
    let drawn = *DRAWN.get_or_init(|| draws_progress(|name| std::env::var(name).ok()));
    if drawn && render::ansi() {
        print!("\x1b]9;4;{}\x07", if running { 3 } else { 0 });
        render::flush();
    }
}

/// Say the window is doing nothing.
pub fn idle() {
    render::title(&said(None));
}

/// Say the window is busy, for as long as this is held.
pub struct Busy {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Busy {
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        if !render::ansi() {
            return Self { stop, handle: None };
        }
        let flag = stop.clone();
        BUSY.store(true, Ordering::Relaxed);
        let handle = std::thread::spawn(move || {
            let mut at = 0usize;
            while !flag.load(Ordering::Relaxed) {
                render::title(&said(Some(MARKS[at % MARKS.len()])));
                progress(true);
                at += 1;
                std::thread::sleep(TICK);
            }
            BUSY.store(false, Ordering::Relaxed);
            progress(false);
            idle();
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }
}

impl Drop for Busy {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{NAME, TEXT_PRESENTATION, draws_progress, titled};

    fn in_terminal(vars: &[(&str, &str)]) -> bool {
        draws_progress(|name| {
            vars.iter()
                .find(|(set, _)| *set == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    #[test]
    fn the_tab_shows_progress_only_in_a_terminal_that_draws_it() {
        let term =
            |program, version| [("TERM_PROGRAM", program), ("TERM_PROGRAM_VERSION", version)];
        assert!(in_terminal(&[("WT_SESSION", "x")]));
        assert!(in_terminal(&[("ConEmuANSI", "ON")]));
        assert!(in_terminal(&term("ghostty", "1.2.0")));
        assert!(in_terminal(&term("ghostty", "1.3.0-main+0a1b2c")));
        assert!(!in_terminal(&term("ghostty", "1.1.3")));
        assert!(in_terminal(&term("iTerm.app", "3.6.6")));
        assert!(in_terminal(&term("iTerm.app", "3.10.0")));
        // An older iTerm2 would show it as a notification.
        assert!(!in_terminal(&term("iTerm.app", "3.6.5")));
        assert!(!in_terminal(&[("TERM_PROGRAM", "iTerm.app")]));
        assert!(!in_terminal(&term("Apple_Terminal", "455")));
        assert!(!in_terminal(&[]));
        assert!(!in_terminal(&[("TMUX", "/tmp/tmux"), ("WT_SESSION", "x")]));
    }

    #[test]
    fn the_title_leads_with_the_session_and_says_when_it_waits() {
        assert_eq!(titled("", false, None), NAME);
        assert_eq!(
            titled("Fix the build", false, None),
            "Fix the build · Letronna"
        );
        assert_eq!(
            titled("Fix the build", false, Some("❄")),
            format!("❄{TEXT_PRESENTATION} Fix the build · Letronna")
        );
        assert!(titled("Fix the build", true, Some("❄")).contains("waiting for you"));
    }
}
