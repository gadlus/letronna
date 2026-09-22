//! Drawing to the terminal.
//!
//! Everything prints into ordinary scrollback rather than taking over the
//! screen: what Letronna said is still there after you quit, selection and
//! mouse scroll keep working, and piping it somewhere behaves. That rules out
//! panes that update in place, which is the trade accepted here.

use std::io::{IsTerminal, Write};

/// ANSI, unless output is being piped or the terminal has opted out.
pub fn colour() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        std::io::stdout().is_terminal()
            && std::env::var_os("NO_COLOR").is_none()
            && std::env::var("TERM").is_ok_and(|t| t != "dumb")
    })
}

fn wrap(code: &str, text: &str) -> String {
    if colour() {
        format!("\x1b[{code}m{text}\x1b[0m")
    } else {
        text.to_owned()
    }
}

pub fn dim(text: &str) -> String {
    wrap("2", text)
}
pub fn bold(text: &str) -> String {
    wrap("1", text)
}
pub fn blue(text: &str) -> String {
    wrap("38;5;69", text)
}
pub fn green(text: &str) -> String {
    wrap("38;5;71", text)
}
pub fn red(text: &str) -> String {
    wrap("38;5;167", text)
}

/// Two spaces, so everything the agent says sits in from the prompt and reads
/// as one column.
pub const PAD: &str = "  ";

pub fn line(text: &str) {
    println!("{PAD}{text}");
}

pub fn blank() {
    println!();
}

pub fn flush() {
    let _ = std::io::stdout().flush();
}

/// Print streamed text as it arrives, indenting each new line so a wrapped
/// answer keeps the column.
///
/// `**bold**` and `` `code` `` become real emphasis on the way past. It is a
/// state machine rather than a parse because the text arrives in pieces and a
/// marker can be split across two of them; buffering whole lines to format
/// them would cost the word-by-word arrival that makes a terminal feel alive.
#[derive(Default)]
pub struct Stream {
    at_line_start: bool,
    bold: bool,
    code: bool,
    /// One `*` seen, waiting to find out whether it is half of a pair.
    star: bool,
    started: bool,
}

impl Stream {
    fn emit(&mut self, out: &mut impl Write, ch: char) {
        if !self.started {
            self.at_line_start = true;
            self.started = true;
        }
        if self.at_line_start && ch != '\n' {
            let _ = write!(out, "{PAD}");
            self.at_line_start = false;
        }
        let _ = write!(out, "{ch}");
        if ch == '\n' {
            self.at_line_start = true;
        }
    }

    fn toggle(&mut self, out: &mut impl Write, on: bool, code: &str) {
        if colour() {
            let _ = write!(out, "{}", if on { code } else { "\x1b[0m" });
        }
    }

    pub fn push(&mut self, text: &str) {
        let mut out = std::io::stdout().lock();
        for ch in text.chars() {
            if self.star {
                self.star = false;
                if ch == '*' {
                    self.bold = !self.bold;
                    let bold = self.bold;
                    self.toggle(&mut out, bold, "\x1b[1m");
                    continue;
                }
                self.emit(&mut out, '*');
            }
            match ch {
                '*' => self.star = true,
                '`' => {
                    self.code = !self.code;
                    let code = self.code;
                    self.toggle(&mut out, code, "\x1b[38;5;180m");
                }
                _ => self.emit(&mut out, ch),
            }
        }
        let _ = out.flush();
    }

    /// Finish the current line, if one is open, and drop any formatting still
    /// hanging so the prompt after it is not tinted.
    pub fn end(&mut self) {
        let mut out = std::io::stdout().lock();
        if self.star {
            self.star = false;
            self.emit(&mut out, '*');
        }
        if (self.bold || self.code) && colour() {
            let _ = write!(out, "\x1b[0m");
        }
        self.bold = false;
        self.code = false;
        if !self.at_line_start {
            let _ = writeln!(out);
            self.at_line_start = true;
        }
        let _ = out.flush();
    }
}

/// A tool call, as one dim line: what ran, on what, and how it went.
pub fn tool(name: &str, argument: &str, outcome: &str, failed: bool) {
    let head = format!("{PAD}{} {}", dim("⏺"), bold(name));
    let arg = if argument.is_empty() {
        String::new()
    } else {
        format!(" {}", dim(argument))
    };
    let tail = if failed {
        red(outcome)
    } else {
        dim(outcome)
    };
    println!("{head}{arg}  {tail}");
}

/// The indented body of a tool result, clipped so a big file does not bury
/// the conversation.
pub fn detail(text: &str, max_lines: usize) {
    let total = text.lines().count();
    for line in text.lines().take(max_lines) {
        println!("{PAD}  {}", dim(line));
    }
    if total > max_lines {
        println!("{PAD}  {}", dim(&format!("… {} more lines", total - max_lines)));
    }
}
