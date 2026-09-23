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
                    format!(" {seconds}s")
                } else {
                    String::new()
                };
                // A line of its own under the conversation rather than a line
                // in it: nothing was said yet, and when it is, the answer
                // should not have to scroll past the waiting.
                crate::screen::status(Some(format!(
                    "{}{} {}{}",
                    render::INSIDE,
                    render::blue(FRAMES[frame % FRAMES.len()]),
                    render::dim(word),
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
