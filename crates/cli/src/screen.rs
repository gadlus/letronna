//! The window Letronna draws in.
//!
//! The conversation is kept here rather than handed to the terminal, which is
//! what lets the input box stay welded to the bottom: scrolling back through
//! what was said moves our own view and leaves the box where it is.
//!
//! The cost of holding it ourselves is that the terminal's own scrollback and
//! mouse selection are not available while this is up. In exchange the layout
//! is ours: nothing can strand a box in the middle of the conversation, a
//! resize is a repaint, and the bottom of the window is always the bottom of
//! the window. What was said is on disk in the session file either way.
//!
//! Everything here goes through one lock. The spinner writes from its own
//! thread, and a half-drawn frame is worse than a slightly late one.

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

/// How often a frame is painted while text is streaming in. Faster than this
/// and we spend the answer repainting it.
const FRAME: Duration = Duration::from_millis(33);

struct Screen {
    /// The conversation, one entry per logical line. The last one is still
    /// open: streamed text is appended to it until a newline arrives.
    lines: Vec<String>,
    /// Wrapped rows for every line but the last, which changes too often to
    /// be worth caching.
    rows: Vec<String>,
    /// How many of `lines` are in `rows`.
    wrapped: usize,
    /// Width the cache was built for; a change means building it again.
    width: u16,
    /// Rows scrolled back from the foot of the conversation. Zero follows.
    scroll: usize,
    /// The input box and anything under it, owned by the caller and handed
    /// over whenever it changes.
    chrome: Vec<String>,
    /// The waiting line, while there is nothing to show yet.
    status: Option<String>,
    open: bool,
    /// Whether the wheel is ours. When it is not, the terminal handles the
    /// mouse again and text can be selected with it.
    mouse: bool,
    painted: Instant,
}

fn screen() -> &'static Mutex<Screen> {
    static SCREEN: OnceLock<Mutex<Screen>> = OnceLock::new();
    SCREEN.get_or_init(|| {
        Mutex::new(Screen {
            lines: vec![String::new()],
            rows: Vec::new(),
            wrapped: 0,
            width: 0,
            scroll: 0,
            chrome: Vec::new(),
            status: None,
            open: false,
            mouse: false,
            painted: Instant::now(),
        })
    })
}

/// Whether the conversation is being held here rather than printed.
pub fn held() -> bool {
    screen().lock().map(|s| s.open).unwrap_or(false)
}

/// Take the window: its own screen, and the mouse, so the wheel scrolls the
/// conversation instead of the terminal's idea of one.
pub fn open() {
    let Ok(mut s) = screen().lock() else { return };
    if s.open {
        return;
    }
    s.open = true;
    s.mouse = false;
    // Alternate screen and the cursor away. The mouse is deliberately left
    // with the terminal: taking it would scroll the conversation with the
    // wheel, at the price of never being able to select a line of it, and
    // copying out of a conversation is worth more than a wheel. `/mouse`
    // trades back for anyone who would rather have the scrolling.
    // The last of these asks for pasted text in one piece.
    print!("\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[?2004h");
    let _ = std::io::stdout().flush();
}

/// Give the window back exactly as it was found.
pub fn close() {
    let Ok(mut s) = screen().lock() else { return };
    if !s.open {
        return;
    }
    s.open = false;
    print!("\x1b[?2004l\x1b[?1006l\x1b[?1000l\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();
}

/// Take the mouse, or give it back. Given back, the terminal's own selection
/// works again and the wheel stops scrolling the conversation -- PageUp and
/// PageDown still do.
pub fn capture(on: bool) {
    let Ok(mut s) = screen().lock() else { return };
    if !s.open {
        return;
    }
    s.mouse = on;
    print!(
        "{}",
        if on {
            "\x1b[?1000h\x1b[?1006h"
        } else {
            "\x1b[?1006l\x1b[?1000l"
        }
    );
    let _ = std::io::stdout().flush();
}

/// Whether the wheel is currently ours.
pub fn capturing() -> bool {
    screen().lock().map(|s| s.mouse).unwrap_or(false)
}

/// Add text, which may run over several lines or stop mid-line.
pub fn push(text: &str) {
    {
        let Ok(mut s) = screen().lock() else { return };
        for ch in text.chars() {
            match ch {
                '\n' => s.lines.push(String::new()),
                // A stray carriage return would otherwise sit in the text and
                // push the rest of the line off the left.
                '\r' => {}
                _ => {
                    if let Some(last) = s.lines.last_mut() {
                        last.push(ch);
                    }
                }
            }
        }
        // Anything arriving means the tail is where the interest is.
        s.scroll = 0;
    }
    soon();
}

/// The waiting line, or `None` to take it away.
pub fn status(text: Option<String>) {
    if let Ok(mut s) = screen().lock() {
        s.status = text;
    }
    soon();
}

/// Hand over the input box and whatever sits under it.
pub fn chrome(lines: Vec<String>) {
    if let Ok(mut s) = screen().lock() {
        s.chrome = lines;
    }
    now();
}

/// Empty the conversation, for /clear.
pub fn wipe() {
    if let Ok(mut guard) = screen().lock() {
        guard.lines = vec![String::new()];
        guard.rows.clear();
        guard.wrapped = 0;
        guard.scroll = 0;
    }
    now();
}

/// Move the view. Positive is back through the conversation.
pub fn scroll(rows: isize) {
    if let Ok(mut s) = screen().lock() {
        s.scroll = s.scroll.saturating_add_signed(rows);
    }
    now();
}

/// Back to following the conversation.
pub fn follow() {
    if let Ok(mut s) = screen().lock() {
        s.scroll = 0;
    }
    now();
}

/// How far back the view is, for saying so in the footer.
pub fn scrolled() -> usize {
    screen().lock().map(|s| s.scroll).unwrap_or(0)
}

/// Paint, but not more often than a frame: for text arriving a word at a time.
fn soon() {
    let Ok(mut s) = screen().lock() else { return };
    if s.painted.elapsed() < FRAME {
        return;
    }
    paint(&mut s);
}

/// Paint now: for anything the user did, where a delay would be felt.
fn now() {
    let Ok(mut s) = screen().lock() else { return };
    paint(&mut s);
}

/// Catch up on anything a throttled paint skipped.
pub fn settle() {
    now();
}

fn paint(s: &mut Screen) {
    if !s.open {
        return;
    }
    let (columns, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    if columns != s.width {
        s.width = columns;
        s.rows.clear();
        s.wrapped = 0;
    }
    // Everything but the line still being written to is worth caching; that
    // one is re-wrapped every frame because it changes every frame.
    while s.wrapped + 1 < s.lines.len() {
        let wrapped = wrap(&s.lines[s.wrapped], s.width as usize);
        s.rows.extend(wrapped);
        s.wrapped += 1;
    }
    let tail = s
        .lines
        .last()
        .map(|line| wrap(line, s.width as usize))
        .unwrap_or_default();

    let furniture = s.chrome.len() + usize::from(s.status.is_some());
    let view = (rows as usize).saturating_sub(furniture).max(1);
    let total = s.rows.len() + tail.len();
    // Scrolling stops at the top rather than running off into nothing.
    s.scroll = s.scroll.min(total.saturating_sub(view));
    let start = total.saturating_sub(view + s.scroll);

    let mut frame = String::from("\x1b[?25l");
    for row in 0..view {
        let index = start + row;
        let text = if index < s.rows.len() {
            s.rows.get(index).map(String::as_str).unwrap_or("")
        } else {
            tail.get(index - s.rows.len()).map(String::as_str).unwrap_or("")
        };
        frame.push_str(&format!("\x1b[{};1H\x1b[2K{text}\x1b[0m", row + 1));
    }
    let mut row = view + 1;
    if let Some(status) = &s.status {
        frame.push_str(&format!("\x1b[{row};1H\x1b[2K{status}\x1b[0m"));
        row += 1;
    }
    for line in &s.chrome {
        frame.push_str(&format!("\x1b[{row};1H\x1b[2K{line}\x1b[0m"));
        row += 1;
    }
    print!("{frame}");
    let _ = std::io::stdout().flush();
    s.painted = Instant::now();
}

/// Break one line into rows that fit, keeping words whole where they will
/// go whole. Escape sequences take no width and travel with the text they
/// colour, so a run of colour still open at a break carries on to the next
/// row rather than stopping there.
fn wrap(line: &str, width: usize) -> Vec<String> {
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
    let visible: Vec<char> = plain(line);
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
fn plain(text: &str) -> Vec<char> {
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
pub fn fold_into(text: &str, width: usize) -> Vec<String> {
    wrap(text, width)
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

#[cfg(test)]
mod tests {
    use super::{visible, wrap};

    #[test]
    fn colour_takes_no_room() {
        assert_eq!(visible("hello"), 5);
        assert_eq!(visible("\x1b[1mhello\x1b[0m"), 5);
    }

    #[test]
    fn lines_break_between_words() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        // Nothing to break on: the row is filled rather than left short.
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        assert_eq!(wrap("short", 40), vec!["short"]);
    }

    #[test]
    fn the_rest_of_a_line_hangs_under_its_first_word() {
        // The bullet opens the line; what wraps sits under the words, not
        // under the bullet.
        assert_eq!(
            wrap("● one two three", 9),
            vec!["● one two", "  three"]
        );
        // Indented text keeps its indent when it wraps.
        assert_eq!(wrap("    aaa bbb", 8), vec!["    aaa", "    bbb"]);
        // A list marker hangs the same way.
        assert_eq!(wrap("- one two", 7), vec!["- one", "  two"]);
        // A bare number is not a marker.
        assert_eq!(wrap("17 times 23", 8), vec!["17 times", "23"]);
    }

    #[test]
    fn a_break_carries_the_colour_over() {
        let rows = wrap("\x1b[1mone two three", 7);
        assert_eq!(rows.len(), 2);
        // The escape stays with the first row and costs it no width.
        assert!(rows[0].starts_with('\x1b'));
        assert_eq!(visible(&rows[0]), 7);
    }
}
