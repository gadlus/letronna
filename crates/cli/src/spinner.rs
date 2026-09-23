//! The "something is happening" line, shown while waiting on the model.
//!
//! A request can sit silent for several seconds before the first token, and
//! an unexplained pause reads as a hang. This redraws one line in place until
//! the first word of the reply arrives, then wipes it so the answer starts on
//! a clean line and the scrollback keeps no trace of it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use crate::render;

const FRAMES: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

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

pub struct Spinner {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Spinner {
    /// Start spinning. Does nothing when output is not a terminal, so piped
    /// output stays clean.
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        if !render::colour() {
            return Self { stop, handle: None };
        }
        let flag = stop.clone();
        let word = WORDS[(Instant::now().elapsed().subsec_nanos() as usize
            + std::process::id() as usize)
            % WORDS.len()];
        let handle = std::thread::spawn(move || {
            let began = Instant::now();
            let mut frame = 0usize;
            while !flag.load(Ordering::Relaxed) {
                let seconds = began.elapsed().as_secs();
                let elapsed = if seconds > 0 {
                    format!(" {}", spell(seconds))
                } else {
                    String::new()
                };
                let (say, paint) = mood(word, seconds);
                // A line of its own under the conversation rather than a line
                // in it: nothing was said yet, and when it is, the answer
                // should not have to scroll past the waiting.
                crate::screen::status(Some(format!(
                    "{}{} {}{}",
                    render::INSIDE,
                    paint(FRAMES[frame % FRAMES.len()]),
                    paint(&say),
                    render::dim(&elapsed)
                )));
                frame += 1;
                std::thread::sleep(std::time::Duration::from_millis(90));
            }
            crate::screen::status(None);
        });
        Self {
            stop,
            handle: Some(handle),
        }
    }

    /// Stop and clear. Safe to call more than once.
    pub fn stop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Spinner {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::{spell, LONG, TOO_LONG};

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
        assert!(LONG < TOO_LONG);
    }
}
