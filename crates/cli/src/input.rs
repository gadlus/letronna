//! Reading a line from the user.

use std::io::{BufRead, Write};

/// Print a prompt and read one line. `None` at end of input, so Ctrl-D and a
/// closed pipe both end the session rather than spinning.
pub fn read(prompt: &str) -> Option<String> {
    let mut out = std::io::stdout();
    let _ = write!(out, "{prompt}");
    let _ = out.flush();
    let mut line = String::new();
    match std::io::stdin().lock().read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => Some(line.trim_end_matches(['\n', '\r']).to_owned()),
    }
}
