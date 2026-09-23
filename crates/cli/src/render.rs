//! Drawing to the terminal.
//!
//! Everything prints into ordinary scrollback rather than taking over the
//! screen: what Letronna said is still there after you quit, selection and
//! mouse scroll keep working, and piping it somewhere behaves. That rules out
//! panes that update in place, which is the trade accepted here.

use std::io::{IsTerminal, Write};

/// Raw mode is on for as long as the input box holds the bottom of the
/// window, and in raw mode a newline moves down without returning to the left
/// margin. Everything that prints a line asks here for the right one.
static RAW: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn set_raw(on: bool) {
    RAW.store(on, std::sync::atomic::Ordering::Relaxed);
}

pub fn nl() -> &'static str {
    if RAW.load(std::sync::atomic::Ordering::Relaxed) {
        "\r\n"
    } else {
        "\n"
    }
}

/// The caret drawn inside the input box. The real cursor cannot sit there: it
/// has to stay in the scrolling region so that output lands in the
/// conversation rather than on top of the box.
pub fn caret() -> String {
    if colour() {
        "\x1b[7m \x1b[0m".to_owned()
    } else {
        "_".to_owned()
    }
}

/// Whether stdin is a terminal, so the line editor can take over keystrokes.
/// Piped input has to go through the plain reader instead.
pub fn interactive() -> bool {
    std::io::stdin().is_terminal() && colour()
}

/// Name the window after the session, the way a terminal program should, and
/// put the old title back on the way out.
pub fn title(text: &str) {
    if colour() {
        print!("]0;{text}");
        flush();
    }
}

/// Start from a clean screen, so the shell's own banner is not the first
/// thing in the transcript.
pub fn clear() {
    if crate::screen::held() {
        crate::screen::wipe();
        return;
    }
    if colour() {
        print!("[2J[3J[H");
        flush();
    }
}

/// ANSI, unless output is being piped or the terminal has opted out.
pub fn colour() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| {
        if !std::io::stdout().is_terminal() || std::env::var_os("NO_COLOR").is_some() {
            return false;
        }
        // TERM is a Unix convention and cmd.exe does not set it, so asking for
        // it on Windows turns everything off in the one shell most people start
        // from. Ask the console itself instead: this also switches on virtual
        // terminal processing, without which the escapes below would be printed
        // literally.
        #[cfg(windows)]
        {
            crossterm::ansi_support::supports_ansi()
        }
        #[cfg(not(windows))]
        {
            std::env::var("TERM").is_ok_and(|t| t != "dumb")
        }
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
pub fn red(text: &str) -> String {
    wrap("38;5;167", text)
}

/// The left margin of the input box.
pub const MARGIN: &str = "  ";

/// Where a mark -- the arrow on your line, the bullet on an answer -- sits.
/// It lines up with the left edge of the box rather than the arrow inside it:
/// the conversation and the thing you type into start at the same column, so
/// there is one edge down the page and not two.
pub const GUTTER: &str = "  ";

/// Where the words start, mark or no mark. `GUTTER` plus a mark and a space
/// comes to the same place, which is what keeps the two sides level.
pub const PAD: &str = "    ";

/// The face on the tin, as characters. Twenty-eight columns wide, which
/// leaves room for the startup details beside it on an eighty-column
/// terminal.
pub const FACE: &[&str] = &[
    "⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀",
    "⠀⠀⠀⠀⠀⠀⠀⢀⠄⣂⣴⣶⣿⡿⢚⣾⣴⣄⡀⡀",
    "⠀⠀⠀⠀⠀⠀⢠⣰⣾⣾⣿⣿⢟⢥⣿⣿⣟⢌⣮⣾⣄",
    "⠀⠀⠀⠀⠐⢨⣾⣿⣿⣿⣿⢥⣿⣾⣿⣿⡞⣿⣿⣿⣿⣗⠄",
    "⠀⠀⠠⢐⡼⣿⣼⣿⣿⡫⢿⣻⠿⣿⣯⣿⣿⣿⢿⣿⣿⣿⣧⣂",
    "⠀⠠⠊⠁⣺⢿⣿⣟⢿⡓⡬⠼⣽⠗⠹⣿⣿⠁⠆⢽⣿⣟⣿⣷⢅⠠",
    "⠀⢀⢠⢼⢾⣿⣿⣷⡞⡎⠢⠀⡀⠂⢈⢿⣟⣈⣴⣿⣿⣿⣽⣾⣷⡑⡄",
    "⠀⠋⣰⢃⣼⣿⣿⣿⣧⠂⠀⠀⠀⠀⠀⣺⢟⠨⣿⣿⣿⡿⣿⣿⡯⡧⢘",
    "⠀⠘⠜⣱⢿⢟⣟⣿⣿⣷⣄⠀⠀⢀⣠⠃⠁⣰⣿⣿⣿⡿⡿⣟⠇⠃",
    "⠀⠀⠐⡍⢸⡹⡽⣿⢽⢿⡿⣿⣾⣿⡇⠀⠀⢈⣉⣽⡪⠐⢁⢀",
    "⠀⠀⠀⠀⠀⠁⠑⠩⠹⠈⢛⠱⡿⡻⣻⣴⣿⣿⣿⣿⡷⠋⠈⠠⣄⣀",
    "⠀⠀⠀⠀⠀⠀⢀⠄⢔⠪⠨⡨⣢⣿⣿⣿⣿⣿⡿⠉⠀⠀⡀⠨⠟⠟⠋⠁",
    "⠀⠀⠀⠀⣰⡌⠀⠀⡐⠥⢱⣵⣿⣿⣿⣿⣿⠏⣀⣤⠆⠀⠀⠀⠀⠀⠀⠊",
    "⠀⠀⠀⠀⠈⢻⣄⠀⠀⣡⣿⣿⣿⣿⣿⡿⣧⡾⠟⢋⡡⠀⣠⣤⣴⣤⣤⣠",
];

/// Width the face is padded to, so the column beside it lines up.
pub const FACE_WIDTH: usize = 28;


pub fn line(text: &str) {
    out(&format!("{PAD}{text}"), true);
}

/// A line with a mark in front of it. The mark stands in the two columns the
/// indent would otherwise occupy, so a marked line and an ordinary one put
/// their text in the same place -- which is the whole point of the indent.
pub fn said(mark: &str, text: &str) {
    out(&format!("{GUTTER}{mark} {text}"), true);
}

/// Everything printed goes through here: into the screen when it is holding
/// the conversation, straight to the terminal when it is not.
pub fn out(text: &str, newline: bool) {
    if crate::screen::held() {
        crate::screen::push(text);
        if newline {
            crate::screen::push("\n");
        }
        return;
    }
    if newline {
        print!("{text}{}", nl());
    } else {
        print!("{text}");
    }
    flush();
}

/// Terminal width, or a sane guess when it cannot be asked.
pub fn width() -> usize {
    crossterm::terminal::size().map(|(w, _)| w as usize).unwrap_or(80)
}

pub fn blank() {
    out("", true);
}

pub fn flush() {
    let _ = std::io::stdout().flush();
}

/// The mark on the model's side of the conversation, the width of the indent
/// so answers keep the column.
pub fn bullet() -> String {
    format!("{GUTTER}{} ", blue("●"))
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
    /// Printed in place of the indent on the first line, so an answer opens
    /// with a bullet and its wrapped lines still sit in the same column.
    lead: Option<String>,
    at_line_start: bool,
    bold: bool,
    code: bool,
    /// One `*` seen, waiting to find out whether it is half of a pair.
    star: bool,
    started: bool,
}

impl Stream {
    /// A stream that opens with `lead` instead of the usual indent. It has to
    /// be the width of the indent, or the first line will not line up with
    /// the rest.
    pub fn opening_with(lead: String) -> Self {
        Self {
            lead: Some(lead),
            ..Default::default()
        }
    }

    /// Text is built up and handed over in one piece rather than written a
    /// character at a time: a frame is painted for each piece, and a frame
    /// per character would be an answer spent repainting itself.
    fn emit(&mut self, out: &mut String, ch: char) {
        if !self.started {
            self.at_line_start = true;
            self.started = true;
        }
        if self.at_line_start && ch != '\n' {
            match self.lead.take() {
                Some(lead) => out.push_str(&lead),
                None => out.push_str(PAD),
            }
            self.at_line_start = false;
        }
        out.push(ch);
        if ch == '\n' {
            self.at_line_start = true;
        }
    }

    fn toggle(&mut self, out: &mut String, on: bool, code: &str) {
        if colour() {
            out.push_str(if on { code } else { "\x1b[0m" });
        }
    }

    pub fn push(&mut self, text: &str) {
        let mut buffer = String::with_capacity(text.len() + 8);
        for ch in text.chars() {
            if self.star {
                self.star = false;
                if ch == '*' {
                    self.bold = !self.bold;
                    let bold = self.bold;
                    self.toggle(&mut buffer, bold, "\x1b[1m");
                    continue;
                }
                self.emit(&mut buffer, '*');
            }
            match ch {
                '*' => self.star = true,
                '`' => {
                    self.code = !self.code;
                    let code = self.code;
                    self.toggle(&mut buffer, code, "\x1b[38;5;180m");
                }
                _ => self.emit(&mut buffer, ch),
            }
        }
        out(&buffer, false);
    }

    /// Finish the current line, if one is open, and drop any formatting still
    /// hanging so what comes after it is not tinted.
    pub fn end(&mut self) {
        let mut buffer = String::new();
        if self.star {
            self.star = false;
            self.emit(&mut buffer, '*');
        }
        if (self.bold || self.code) && colour() {
            buffer.push_str("\x1b[0m");
        }
        self.bold = false;
        self.code = false;
        let close = !self.at_line_start;
        self.at_line_start = true;
        out(&buffer, close);
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
    out(&format!("{head}{arg}  {tail}"), true);
}

/// The indented body of a tool result, clipped so a big file does not bury
/// the conversation.
pub fn detail(text: &str, max_lines: usize) {
    let total = text.lines().count();
    for line in text.lines().take(max_lines) {
        out(&format!("{PAD}  {}", dim(line)), true);
    }
    if total > max_lines {
        out(
            &format!("{PAD}  {}", dim(&format!("… {} more lines", total - max_lines))),
            true,
        );
    }
}
