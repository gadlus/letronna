//! What a long call is doing now, for the window and the terminal to show
//! under it while it runs: one line per conversation, set by the tool as it
//! hears of progress and cleared when it ends. A tool with nothing to say
//! leaves it unset.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

fn lines() -> &'static Mutex<HashMap<u64, String>> {
    static LINES: OnceLock<Mutex<HashMap<u64, String>>> = OnceLock::new();
    LINES.get_or_init(Default::default)
}

/// Say what the call running in `conversation` is doing now.
pub fn say(conversation: u64, line: impl Into<String>) {
    if let Ok(mut lines) = lines().lock() {
        lines.insert(conversation, line.into());
    }
}

/// What the call running in `conversation` last said it is doing.
pub fn of(conversation: u64) -> Option<String> {
    lines().lock().ok()?.get(&conversation).cloned()
}

/// Forget it, as the call ends.
pub fn clear(conversation: u64) {
    if let Ok(mut lines) = lines().lock() {
        lines.remove(&conversation);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_is_kept_per_conversation_until_cleared() {
        say(91, "queued");
        say(91, "round 2: run_command");
        say(92, "running");
        assert_eq!(of(91).as_deref(), Some("round 2: run_command"));
        clear(91);
        assert_eq!((of(91), of(92).as_deref()), (None, Some("running")));
        clear(92);
    }
}
