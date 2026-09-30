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

/// The caret, drawn over whatever character it sits on. The real cursor
/// cannot be parked there: it has to stay where output lands, or a redraw
/// would pull the conversation out of line.
pub fn caret_over(text: &str) -> String {
    if colour() {
        format!("\x1b[7m{text}\x1b[0m",)
    } else {
        text.to_owned()
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
pub fn green(text: &str) -> String {
    wrap("38;5;114", text)
}
pub fn amber(text: &str) -> String {
    wrap("38;5;179", text)
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

/// The arrow's column inside the box: past the margin, the border and the
/// space after it. The waiting line belongs here rather than out in the
/// conversation, because nothing has been said yet -- it is the box thinking,
/// not Letronna talking.
pub const INSIDE: &str = "    ";

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

/// Room between the face and the details beside it. The face is drawn from a
/// photograph, so its edge is ragged and a couple of spaces read as none at
/// all; this is wide enough that the two are clearly separate things.
pub const FACE_GAP: usize = 6;

pub fn line(text: &str) {
    out(&format!("{PAD}{text}"), true);
}

/// A line with a mark in front of it. The mark stands in the two columns the
/// indent would otherwise occupy, so a marked line and an ordinary one put
/// their text in the same place -- which is the whole point of the indent.
pub fn said(mark: &str, text: &str) {
    out(&format!("{GUTTER}{mark} {text}"), true);
}

/// Everything printed goes through here, so raw mode's need for a carriage
/// return with every newline is dealt with once.
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
    crossterm::terminal::size()
        .map(|(w, _)| w as usize)
        .unwrap_or(80)
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

/// Break one line into rows that fit, keeping words whole where they will
/// go whole. Escape sequences take no width and travel with the text they
/// colour, so a run of colour still open at a break carries on to the next
/// row rather than stopping there.
pub fn fold(line: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![String::new()];
    }
    // What the line is indented by, so the rest of it hangs under its own
    // first word instead of falling back to the margin. Without this a poem
    // or a block of code goes ragged at the left as soon as a line is too
    // long for the window.
    let indent = hanging(line).min(width / 2);
    let carry = " ".repeat(indent);
    let mut rows = Vec::new();
    let mut row = String::new();
    let mut filled = 0usize;

    for (word, wide) in words(line) {
        // A space at the head of a row is the one left behind by the break;
        // dropping it keeps the text against the margin. The line's own
        // leading spaces are not that, and are kept.
        if word.trim().is_empty() && !rows.is_empty() && filled == indent {
            continue;
        }
        if filled + wide <= width {
            row.push_str(&word);
            filled += wide;
            continue;
        }
        if filled > indent {
            // The space the break happened at would otherwise hang off the
            // end of the row it was in.
            let mut done = std::mem::take(&mut row);
            while done.ends_with(' ') {
                done.pop();
            }
            rows.push(done);
            row.push_str(&carry);
            filled = indent;
            if word.trim().is_empty() {
                continue;
            }
        }
        // A word too long for any row is cut rather than left to overflow.
        if wide > width {
            for ch in word.chars() {
                if ch == '\x1b' {
                    row.push(ch);
                    continue;
                }
                // Escapes already in the row do not count towards its width.
                if filled == width {
                    rows.push(std::mem::take(&mut row));
                    row.push_str(&carry);
                    filled = indent;
                }
                row.push(ch);
                if !in_escape(&row) {
                    filled += 1;
                }
            }
            continue;
        }
        row.push_str(&word);
        filled += wide;
    }
    rows.push(row);
    rows
}

/// The column the rest of a line should hang from: whatever it is indented
/// by, and past a marker where one opens it. The bullet on an answer, the
/// arrow on a question and the dashes and numbers of a list all read as one
/// column when what follows them lines up underneath.
fn hanging(line: &str) -> usize {
    let visible: Vec<char> = printing(line);
    let mut at = 0;
    while visible.get(at) == Some(&' ') {
        at += 1;
    }
    let mark = match visible.get(at) {
        Some('●') | Some('›') | Some('⏺') | Some('•') | Some('-') | Some('*') => 1,
        Some(digit) if digit.is_ascii_digit() => {
            let mut end = at;
            while visible.get(end).is_some_and(|c| c.is_ascii_digit()) {
                end += 1;
            }
            // "1." and "2)" open a list; a bare number is just a number.
            match visible.get(end) {
                Some('.') | Some(')') => end + 1 - at,
                _ => 0,
            }
        }
        _ => 0,
    };
    if mark > 0 && visible.get(at + mark) == Some(&' ') {
        return at + mark + 1;
    }
    at
}

/// The characters of `text` that take up room, with the escapes dropped.
fn printing(text: &str) -> Vec<char> {
    let mut out = Vec::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Whether the tail of `text` is part of an escape sequence, so its
/// characters are not counted as width.
fn in_escape(text: &str) -> bool {
    match text.rfind('\x1b') {
        Some(at) => !text[at..].chars().skip(1).any(|c| c.is_ascii_alphabetic()),
        None => false,
    }
}

/// Split into words and the runs of spaces between them, each with the width
/// it will take. Escape sequences ride along with whatever follows them.
fn words(line: &str) -> Vec<(String, usize)> {
    let mut out: Vec<(String, usize)> = Vec::new();
    let mut current = String::new();
    let mut wide = 0usize;
    let mut space = false;
    let mut chars = line.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            current.push(ch);
            for next in chars.by_ref() {
                current.push(next);
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
            continue;
        }
        let is_space = ch == ' ';
        if wide > 0 && is_space != space {
            out.push((std::mem::take(&mut current), wide));
            wide = 0;
        }
        space = is_space;
        current.push(ch);
        wide += 1;
    }
    if !current.is_empty() {
        out.push((current, wide));
    }
    out
}

/// Break text into rows that fit, for anything that has to lay itself out
/// before handing the result over.
/// The text of a line with the escapes taken out, for putting on a
/// clipboard, where colour is not wanted and would not survive anyway.
pub fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(ch);
        }
    }
    out
}

/// Turn a run of a line inside out, for showing what is selected.
///
/// The row is rewritten without its own colour first. Reverse video is just
/// another attribute, so the first reset the line carries -- and a coloured
/// line is full of them -- would switch the highlight off again halfway
/// through. Losing the colour of a row while it is marked is a small price
/// for the mark being visible at all.
pub fn mark(text: &str, from: usize, to: Option<usize>) -> String {
    let plain = plain(text);
    let chars: Vec<char> = plain.chars().collect();
    let to = to.unwrap_or(chars.len()).min(chars.len());
    let from = from.min(to);
    let before: String = chars[..from].iter().collect();
    let span: String = chars[from..to].iter().collect();
    let after: String = chars[to..].iter().collect();
    if !colour() {
        return plain;
    }
    format!("{before}\x1b[7m{span}\x1b[27m{after}")
}

/// Width of text as printed, ignoring the escapes that colour it.
pub fn visible(text: &str) -> usize {
    let mut width = 0;
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch == '\x1b' {
            for next in chars.by_ref() {
                if next.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            width += 1;
        }
    }
    width
}

/// Plain text on a single row, its line breaks made spaces, cut to `width`
/// columns with an ellipsis where it was cut.
pub fn fit(text: &str, width: usize) -> String {
    let text = letronna_core::tools::one_line(text);
    if text.chars().count() <= width {
        return text;
    }
    let mut cut: String = text.chars().take(width.saturating_sub(1)).collect();
    if width > 0 {
        cut.push('…');
    }
    cut
}

/// Plain text cut to `width` columns, ending in an ellipsis where it was
/// cut, for a row that must not wrap.
pub fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_owned();
    }
    let Some(room) = width.checked_sub(1) else {
        return String::new();
    };
    let mut cut: String = text.chars().take(room).collect();
    cut.push('…');
    cut
}

/// A tool call: what ran and on what, then how it went on the line under it.
/// Two lines rather than one because the second is where its output goes, and
/// a result that hangs off the end of the first line is easy to miss.
pub fn tool(name: &str, argument: &str, outcome: &str, failed: bool) {
    let mark = if failed { red("●") } else { green("●") };
    let call = if argument.is_empty() {
        bold(name)
    } else {
        format!("{}{}", bold(name), dim(&format!("({argument})")))
    };
    out(&format!("{GUTTER}{mark} {call}"), true);
    let tail = if failed { red(outcome) } else { dim(outcome) };
    out(&format!("{PAD}{} {tail}", dim("⎿")), true);
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
            &format!(
                "{PAD}  {}",
                dim(&format!("… {} more lines", total - max_lines))
            ),
            true,
        );
    }
}

/// Lines shown under each notice, as many as under a tool's output: enough
/// for a failing test's summary, where a helper's report of a page would
/// bury the conversation.
const NOTICE_LINES: usize = 12;

/// Notices drawn dim with a mark of their own, one for each thing that
/// happened meanwhile, since the model reads them as the user's turn but
/// neither side said them.
pub fn notice(text: &str) {
    blank();
    let mut body = String::new();
    // The first line is the header telling the model what a notice is not,
    // which the mark says here. Each notice after it opens with a dash, and
    // the lines it brought are indented under it.
    for line in text.lines().skip(1) {
        let Some(first) = line.strip_prefix("- ") else {
            body.push_str(line.strip_prefix("    ").unwrap_or(line));
            body.push('\n');
            continue;
        };
        detail(&std::mem::take(&mut body), NOTICE_LINES);
        said(&dim("◆"), &dim(first));
    }
    detail(&body, NOTICE_LINES);
}

#[cfg(test)]
mod tests {
    use super::{fit, fold, visible};

    #[test]
    fn colour_takes_no_room() {
        assert_eq!(visible("hello"), 5);
        assert_eq!(visible("\x1b[1mhello\x1b[0m"), 5);
    }

    #[test]
    fn fitting_keeps_to_one_row_of_the_width() {
        assert_eq!(fit("one\ntwo", 20), "one two");
        assert_eq!(fit("abcdef", 4), "abc…");
        assert_eq!(fit("abcd", 4), "abcd");
        assert_eq!(fit("abcd", 0), "");
    }

    #[test]
    fn lines_break_between_words() {
        assert_eq!(fold("one two three", 7), vec!["one two", "three"]);
        // Nothing to break on: the row is filled rather than left short.
        assert_eq!(fold("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(fold("short", 40), vec!["short"]);
    }

    #[test]
    fn the_rest_of_a_line_hangs_under_its_first_word() {
        // The bullet opens the line; what wraps sits under the words, not
        // under the bullet.
        assert_eq!(fold("● one two three", 9), vec!["● one two", "  three"]);
        // Indented text keeps its indent when it wraps.
        assert_eq!(fold("    aaa bbb", 8), vec!["    aaa", "    bbb"]);
        // A list marker hangs the same way.
        assert_eq!(fold("- one two", 7), vec!["- one", "  two"]);
        // A bare number is not a marker.
        assert_eq!(fold("17 times 23", 8), vec!["17 times", "23"]);
    }

    #[test]
    fn a_break_carries_the_colour_over() {
        let rows = fold("\x1b[1mone two three", 7);
        assert_eq!(rows.len(), 2);
        // The escape stays with the first row and costs it no width.
        assert!(rows[0].starts_with('\x1b'));
        assert_eq!(visible(&rows[0]), 7);
    }
}
