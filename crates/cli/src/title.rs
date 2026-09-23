//! What the window is called.
//!
//! A terminal tab is often the only part of Letronna visible once you have
//! switched to something else, so the title says whether it still wants you.
//! Idle it is just the name; working, a mark turns in front of it, which is
//! enough to catch the eye from across a taskbar without being a notification.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use crate::render;

const NAME: &str = "Letronna";

/// The turning mark. Four positions is enough to read as motion and few
/// enough that a glance at a tab is not a flicker.
const MARKS: [&str; 4] = ["✳", "✻", "✽", "✻"];

/// Slower than the spinner on the screen. A title is read in passing, and
/// redrawing it eleven times a second would be work nobody sees.
const TICK: Duration = Duration::from_millis(400);

/// Say the window is doing nothing.
pub fn idle() {
    render::title(NAME);
}

/// Say the window is busy, for as long as this is held.
pub struct Busy {
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Busy {
    pub fn start() -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        if !render::colour() {
            return Self { stop, handle: None };
        }
        let flag = stop.clone();
        let handle = std::thread::spawn(move || {
            let mut at = 0usize;
            while !flag.load(Ordering::Relaxed) {
                render::title(&format!("{} {NAME}", MARKS[at % MARKS.len()]));
                at += 1;
                std::thread::sleep(TICK);
            }
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
