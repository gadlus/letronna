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
fn spell(seconds: u64) -> String {
    match (seconds / 60, seconds % 60) {
        (0, s) => format!("{s}s"),
        (m, 0) => format!("{m}m"),
        (m, s) => format!("{m}m {s}s"),
    }
}

/// What to say and how loudly, given how long it has been.
fn mood(word: &'static str, seconds: u64) -> (String, fn(&str) -> String) {
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

/// One wait, from the moment the question went.
pub struct Waiting {
    began: Instant,
    word: &'static str,
}

impl Default for Waiting {
    fn default() -> Self {
        Self::new()
    }
}

impl Waiting {
    pub fn new() -> Self {
        Self {
            began: Instant::now(),
            // Whichever word the clock lands on; they all mean the same
            // thing, and the same one every time reads as a frozen screen.
            word: WORDS[Instant::now().elapsed().subsec_nanos() as usize % WORDS.len()],
        }
    }

    /// The line as it should look now.
    pub fn line(&self) -> String {
        let elapsed = self.began.elapsed();
        let seconds = elapsed.as_secs();
        let frame = FRAMES[(elapsed.as_millis() / FRAME_MS) as usize % FRAMES.len()];
        let (say, paint) = mood(self.word, seconds);
        let since = if seconds > 0 {
            format!(" {}", spell(seconds))
        } else {
            String::new()
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
        assert!(!Waiting::new().line().trim().is_empty());
    }
}
