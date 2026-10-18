//! The "something is happening" line, shown while waiting on the model.
//!
//! A request can sit silent for a long time before the first token, and an
//! unexplained pause reads as a hang. This is only what the line should say;
//! the box draws it, on the same thread as everything else it draws. It used
//! to spin on a thread of its own, which meant two threads writing escape
//! sequences to one terminal, and a keystroke landing between the two halves
//! of a redraw left the window in pieces.

use std::time::Instant;

use crate::render;

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

/// How long one frame is held.
const FRAME_MS: u128 = 90;

/// Words for the wait. Deliberately about thinking rather than about doing:
/// the agent may be about to read a file or about to answer, and this line
/// cannot tell which, so it does not claim to.
const WORDS: [&str; 6] = [
    "Thinking",
    "Working",
    "Pondering",
    "Considering",
    "Weighing it up",
    "Mulling",
];

/// When the wait stops being ordinary. A request that has gone quiet for
/// half a minute is worth saying so about, and one past two minutes is worth
/// saying loudly, because it may not be coming at all.
const LONG: u64 = 30;
const TOO_LONG: u64 = 120;

/// The wait, in the units a person would use. Ninety seconds is a minute and
/// a half to everyone except a computer.
pub fn spell(seconds: u64) -> String {
    match (seconds / 60, seconds % 60) {
        (0, s) => format!("{s}s"),
        (m, 0) => format!("{m}m"),
        (m, s) => format!("{m}m {s}s"),
    }
}

/// What to say and how loudly, given how long it has been.
fn mood(word: &str, seconds: u64) -> (String, fn(&str) -> String) {
    if seconds >= TOO_LONG {
        (
            format!("{word} — still nothing back, esc to stop"),
            render::red as fn(&str) -> String,
        )
    } else if seconds >= LONG {
        (
            format!("{word} — longer than usual"),
            render::amber as fn(&str) -> String,
        )
    } else {
        (word.to_owned(), render::blue as fn(&str) -> String)
    }
}

/// The same escalation, for a wait that has a name. Escape stops a tool as
/// well, so the late message reads the same.
fn mood_about(what: &str, seconds: u64) -> (String, fn(&str) -> String) {
    if seconds >= TOO_LONG {
        (
            format!("{what} — still going, esc to stop"),
            render::red as fn(&str) -> String,
        )
    } else if seconds >= LONG {
        (
            format!("{what} — longer than usual"),
            render::amber as fn(&str) -> String,
        )
    } else {
        (what.to_owned(), render::blue as fn(&str) -> String)
    }
}

/// One wait, from the moment the question went.
pub struct Waiting {
    began: Instant,
    /// Where the clock it shows counts from: the turn's start for a wait on
    /// the model, so a turn of several steps shows how long it has taken in
    /// all; the wait's own start otherwise. Whether it is taking long is
    /// always the wait's own.
    shown: Instant,
    word: &'static str,
    /// What is being waited for, when it is something nameable. Waiting on
    /// the model is "Thinking"; waiting on a tool says which tool, because
    /// "Thinking" over a two-minute command is a lie.
    about: Option<String>,
    /// The step of its plan the model is on, said after the word: it is the
    /// one thing this line can tell about what the wait is for.
    step: Option<String>,
}

impl Default for Waiting {
    fn default() -> Self {
        Self::new()
    }
}

impl Waiting {
    pub fn new() -> Self {
        let now = Instant::now();
        // Whichever word the clock lands on; they all mean the same thing,
        // and the same one every time reads as a frozen screen. The time of
        // day, since an instant just taken has had no time to pass.
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.subsec_nanos());
        Self {
            began: now,
            shown: now,
            word: WORDS[nanos as usize % WORDS.len()],
            about: None,
            step: None,
        }
    }

    /// The same wait, its clock counting from `from`.
    pub fn counted_from(self, from: Instant) -> Self {
        Self {
            shown: from,
            ..self
        }
    }

    /// A wait with a name on it.
    pub fn about(what: &str) -> Self {
        Self {
            about: Some(what.to_owned()),
            ..Self::new()
        }
    }

    /// A wait on the model while it works on a step of its plan.
    pub fn during(step: &str) -> Self {
        Self {
            step: Some(step.to_owned()),
            ..Self::new()
        }
    }

    /// The same wait under another name, its clock still running.
    pub fn renamed(self, what: &str) -> Self {
        Self {
            about: Some(what.to_owned()),
            ..self
        }
    }

    /// The line as it should look now, in a window `width` columns wide.
    pub fn line(&self, width: usize) -> String {
        let elapsed = self.began.elapsed();
        let seconds = elapsed.as_secs();
        let frame = FRAMES[(elapsed.as_millis() / FRAME_MS) as usize % FRAMES.len()];
        let since = match self.shown.elapsed().as_secs() {
            0 => String::new(),
            shown => format!(" {}", spell(shown)),
        };
        let (say, paint) = match (&self.about, &self.step) {
            // A long command gives way before the clock and the warning, and
            // stops short of the edge, as a step does below.
            (Some(what), _) => {
                let (bare, _) = mood_about("", seconds);
                let taken = format!("{}{frame} {bare}{since}{}", render::INSIDE, render::MARGIN);
                let what = render::fit(what, width.saturating_sub(taken.chars().count()));
                mood_about(&what, seconds)
            }
            (None, Some(step)) => {
                // The step gives way before the word, a warning or the clock,
                // and stops short of the edge: past it the terminal wraps the
                // line and pushes the box out of the window.
                let (bare, _) = mood(&format!("{} · ", self.word), seconds);
                let taken = format!("{}{frame} {bare}{since}{}", render::INSIDE, render::MARGIN);
                let step = render::fit(step, width.saturating_sub(taken.chars().count()));
                mood(&format!("{} · {step}", self.word), seconds)
            }
            (None, None) => mood(self.word, seconds),
        };
        format!(
            "{}{} {}{}",
            render::INSIDE,
            paint(frame),
            paint(&say),
            render::dim(&since)
        )
    }
}

#[cfg(test)]
mod tests {
    use super::{LONG, TOO_LONG, Waiting, spell};
    use crate::render::{MARGIN, visible};

    #[test]
    fn the_wait_is_spelled_the_way_people_say_it() {
        assert_eq!(spell(9), "9s");
        assert_eq!(spell(59), "59s");
        assert_eq!(spell(60), "1m");
        assert_eq!(spell(90), "1m 30s");
        assert_eq!(spell(605), "10m 5s");
    }

    #[test]
    fn it_speaks_up_before_it_shouts() {
        const { assert!(LONG < TOO_LONG) };
    }

    #[test]
    fn the_line_says_something_from_the_first_moment() {
        assert!(!Waiting::new().line(80).trim().is_empty());
    }

    #[test]
    fn a_long_command_gives_way_before_the_clock() {
        let command = "cargo test --workspace --all-features -- --nocapture --test-threads=1";
        let began = std::time::Instant::now() - std::time::Duration::from_secs(5);
        let line = Waiting::about(command).counted_from(began).line(40);
        assert!(visible(&line) <= 40, "{line}");
        assert!(crate::render::plain(&line).ends_with("5s"), "{line}");
    }

    #[test]
    fn the_word_is_not_always_the_same() {
        let words: std::collections::HashSet<&str> = (0..200)
            .map(|_| {
                std::thread::sleep(std::time::Duration::from_micros(50));
                Waiting::new().word
            })
            .collect();
        assert!(words.len() > 1);
    }

    #[test]
    fn the_wait_names_the_step_and_still_fits_a_narrow_window() {
        let step = "Write the parser for every format the importer has to read";
        let wide = Waiting::during(step).line(200);
        assert!(wide.contains(&format!(" · {step}")), "{wide}");
        for width in [30, 40, 60] {
            let line = Waiting::during(step).line(width);
            assert!(visible(&line) <= width - MARGIN.len(), "{line}");
            assert!(line.contains(" · Wri"), "{line}");
        }
    }

    #[test]
    fn a_renamed_wait_keeps_its_clock() {
        let wait = Waiting::about("read_file(a) and 1 more");
        let began = wait.began;
        let wait = wait.renamed("read_file(b)");
        assert_eq!(wait.began, began);
        assert_eq!(wait.about.as_deref(), Some("read_file(b)"));
    }
}
