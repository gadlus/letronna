//! Notifications from the terminal, by escape sequence.
//!
//! A program in a terminal cannot reach the desktop's notifications, but
//! several terminals turn an escape sequence into one: iTerm2 reads OSC 9,
//! kitty OSC 99, and Ghostty, WezTerm and foot OSC 777. The rest get the
//! bell, which every terminal does something with -- a sound, a mark on the
//! tab, a badge on the dock icon -- and which tmux and screen pass on while
//! keeping other sequences to themselves. Which terminal it is, is read
//! from the variables it sets, since asking it would mean waiting on an
//! answer that most never send.
//!
//! Only while nobody is looking: the terminal says when it gains and loses
//! focus once asked to (the window asks when it is taken), and until one
//! has said so, no key for `IDLE` counts as looking away.

use letronna_core::notify::{Note, Notifier};
use std::io::Write;
use std::time::{Duration, Instant};

/// No key for this long counts as looking away in a terminal that has not
/// reported its focus: long enough to read an answer as it streams, and
/// what Claude Code waits before its own idle notification.
const IDLE: Duration = Duration::from_secs(60);

/// How a terminal is asked to notify.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Channel {
    /// iTerm2's: one line of text.
    Osc9,
    /// kitty's: a title, then a body.
    Osc99,
    /// Ghostty's, WezTerm's and foot's: a title and a body.
    Osc777,
    Bell,
}

/// The channel the terminal whose variables `var` reads understands.
pub fn channel(var: impl Fn(&str) -> Option<String>) -> Channel {
    if var("TMUX").is_some() || var("STY").is_some() {
        return Channel::Bell;
    }
    let term = var("TERM").unwrap_or_default();
    if term == "xterm-kitty" || var("KITTY_WINDOW_ID").is_some() {
        return Channel::Osc99;
    }
    match var("TERM_PROGRAM").as_deref() {
        Some("iTerm.app") => Channel::Osc9,
        Some("WezTerm" | "ghostty") => Channel::Osc777,
        _ if term.starts_with("foot") => Channel::Osc777,
        _ => Channel::Bell,
    }
}

/// What to write for `note` on `channel`. A note carries no control
/// characters, so nothing in it can end the sequence early.
pub fn sequence(channel: Channel, note: &Note) -> String {
    match channel {
        Channel::Osc9 => format!("\x1b]9;{}: {}\x07", note.title, note.body),
        // d=0 holds the title back until the body with the same id.
        Channel::Osc99 => format!(
            "\x1b]99;i=letronna:d=0:p=title;{}\x1b\\\x1b]99;i=letronna:p=body;{}\x1b\\",
            note.title, note.body
        ),
        // Its fields are split at semicolons, so the title may hold none.
        Channel::Osc777 => format!(
            "\x1b]777;notify;{};{}\x07",
            note.title.replace(';', ","),
            note.body
        ),
        Channel::Bell => "\x07".to_owned(),
    }
}

/// Notifies through the terminal this runs in.
pub struct Terminal(Channel);

impl Terminal {
    /// The terminal this runs in, known by its variables.
    pub fn here() -> Self {
        Self(channel(|name| std::env::var(name).ok()))
    }
}

impl Notifier for Terminal {
    fn show(&mut self, note: &Note) {
        // One write, so a frame painted meanwhile cannot land inside it.
        print!("{}", sequence(self.0, note));
        let _ = std::io::stdout().flush();
    }
}

/// Whether anyone is looking at the terminal.
pub struct Focus {
    /// What the terminal last said, once it has said anything.
    reported: Option<bool>,
    /// When a key, a paste or the mouse last came.
    touched: Instant,
}

impl Focus {
    pub fn new(now: Instant) -> Self {
        Self {
            reported: None,
            touched: now,
        }
    }

    /// The terminal gained focus, or lost it.
    pub fn report(&mut self, focused: bool) {
        self.reported = Some(focused);
    }

    /// Someone used the keyboard or the mouse here.
    pub fn touch(&mut self, now: Instant) {
        self.touched = now;
    }

    /// Whether nobody is looking: as the terminal last said, or failing
    /// that, when nothing has been touched for `IDLE`.
    pub fn away(&self, now: Instant) -> bool {
        match self.reported {
            Some(focused) => !focused,
            None => now.saturating_duration_since(self.touched) >= IDLE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn in_terminal(vars: &[(&str, &str)]) -> Channel {
        channel(|name| {
            vars.iter()
                .find(|(set, _)| *set == name)
                .map(|(_, value)| (*value).to_owned())
        })
    }

    fn note(title: &str, body: &str) -> Note {
        Note {
            session: 1,
            title: title.into(),
            body: body.into(),
        }
    }

    #[test]
    fn each_terminal_gets_the_sequence_it_understands() {
        assert_eq!(in_terminal(&[("TERM_PROGRAM", "iTerm.app")]), Channel::Osc9);
        assert_eq!(in_terminal(&[("TERM", "xterm-kitty")]), Channel::Osc99);
        assert_eq!(in_terminal(&[("KITTY_WINDOW_ID", "1")]), Channel::Osc99);
        assert_eq!(in_terminal(&[("TERM_PROGRAM", "ghostty")]), Channel::Osc777);
        assert_eq!(in_terminal(&[("TERM_PROGRAM", "WezTerm")]), Channel::Osc777);
        assert_eq!(in_terminal(&[("TERM", "foot-extra")]), Channel::Osc777);
        assert_eq!(
            in_terminal(&[("TERM_PROGRAM", "Apple_Terminal")]),
            Channel::Bell
        );
        assert_eq!(in_terminal(&[("WT_SESSION", "x")]), Channel::Bell);
        assert_eq!(in_terminal(&[]), Channel::Bell);
        // Inside tmux or screen only the bell gets out.
        let tmux = [("TMUX", "/tmp/tmux"), ("TERM_PROGRAM", "iTerm.app")];
        assert_eq!(in_terminal(&tmux), Channel::Bell);
        assert_eq!(
            in_terminal(&[("STY", "1.pts"), ("TERM", "xterm-kitty")]),
            Channel::Bell
        );
    }

    #[test]
    fn the_sequences_carry_the_title_and_body_whole() {
        let shown = note("Fix; the build", "Finished: it passes; all of it.");
        assert_eq!(
            sequence(Channel::Osc9, &shown),
            "\x1b]9;Fix; the build: Finished: it passes; all of it.\x07"
        );
        assert_eq!(
            sequence(Channel::Osc99, &shown),
            "\x1b]99;i=letronna:d=0:p=title;Fix; the build\x1b\\\
             \x1b]99;i=letronna:p=body;Finished: it passes; all of it.\x1b\\"
        );
        assert_eq!(
            sequence(Channel::Osc777, &shown),
            "\x1b]777;notify;Fix, the build;Finished: it passes; all of it.\x07"
        );
        assert_eq!(sequence(Channel::Bell, &shown), "\x07");
    }

    #[test]
    fn a_terminal_that_reports_focus_is_believed_and_one_that_does_not_goes_by_idleness() {
        let start = Instant::now();
        let mut focus = Focus::new(start);
        assert!(!focus.away(start + IDLE / 2));
        assert!(focus.away(start + IDLE));
        focus.touch(start + IDLE);
        assert!(!focus.away(start + IDLE + IDLE / 2));
        focus.report(false);
        assert!(focus.away(start + IDLE));
        focus.report(true);
        assert!(!focus.away(start + IDLE * 10));
    }
}
