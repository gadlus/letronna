//! The window Letronna draws in.
//!
//! The conversation is kept here rather than handed to the terminal. That is
//! what lets the box stay welded to the foot of the window however far back
//! you scroll, and what makes a resize a repaint from what we still hold
//! rather than whatever the terminal made of the old lines.
//!
//! Holding it means the terminal's own selection is not available, so this
//! does that too: dragging marks text, letting go copies it. Plain dragging,
//! no shift, because a terminal only reserves shift for itself when the
//! program has taken the mouse and left selection unimplemented.
//!
//! One lock around the lot. Only the main loop draws, but it draws from
//! several places, and a half-written escape sequence is a broken window.

use std::io::Write;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::render;

/// How often a frame is painted while text is streaming in. Faster than this
/// and the answer is spent repainting itself.
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
    /// The input box and anything under it, owned by the caller.
    chrome: Vec<String>,
    /// Anchor and end of a selection being dragged, in wrapped-row and
    /// column terms, so it stays put when the view scrolls under it.
    from: Option<(usize, usize)>,
    to: Option<(usize, usize)>,
    open: bool,
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
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
        })
    })
}

/// Whether the conversation is being held here rather than printed.
pub fn held() -> bool {
    screen().lock().map(|s| s.open).unwrap_or(false)
}

/// Take the window: its own screen, the cursor away, the mouse for scrolling
/// and selecting, and pasted text in one piece where it is offered.
pub fn open() {
    let Ok(mut s) = screen().lock() else { return };
    if s.open {
        return;
    }
    s.open = true;
    // 1000 is buttons, 1002 adds movement while one is held, 1006 makes the
    // numbers big enough for a wide window.
    print!("\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?2004h");
    let _ = std::io::stdout().flush();
}

/// Give the window back exactly as it was found.
pub fn close() {
    let Ok(mut s) = screen().lock() else { return };
    if !s.open {
        return;
    }
    s.open = false;
    print!("\x1b[?2004l\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();
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

/// Hand over the input box and whatever sits under it.
pub fn chrome(lines: Vec<String>) {
    if let Ok(mut s) = screen().lock() {
        s.chrome = lines;
    }
    now();
}

/// Empty the conversation, for /clear.
pub fn wipe() {
    if let Ok(mut s) = screen().lock() {
        s.lines = vec![String::new()];
        s.rows.clear();
        s.wrapped = 0;
        s.scroll = 0;
        s.from = None;
        s.to = None;
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

/// Start a selection at a place on the screen.
pub fn select_from(column: u16, row: u16) {
    if let Ok(mut s) = screen().lock() {
        let at = s.buffer_at(column, row);
        s.from = at;
        s.to = at;
    }
    now();
}

/// Drag it to somewhere else.
pub fn select_to(column: u16, row: u16) {
    if let Ok(mut s) = screen().lock() {
        if s.from.is_some() {
            if let Some(at) = s.buffer_at(column, row) {
                s.to = Some(at);
            }
        }
    }
    now();
}

/// Finish, handing back what was marked. Nothing when it was a bare click,
/// which is how a selection is cleared.
pub fn select_done() -> Option<String> {
    let text = {
        let Ok(mut s) = screen().lock() else {
            return None;
        };
        let text = s.selected();
        if text.is_none() {
            s.from = None;
            s.to = None;
        }
        text
    };
    now();
    text
}

/// Whether anything is marked.
pub fn selecting() -> bool {
    screen()
        .lock()
        .map(|s| s.selected().is_some())
        .unwrap_or(false)
}

/// Drop the selection.
pub fn unselect() {
    if let Ok(mut s) = screen().lock() {
        s.from = None;
        s.to = None;
    }
    now();
}

/// Put text on the clipboard through the terminal, which works over ssh and
/// through a multiplexer because the terminal at the far end does the work.
pub fn copy(text: &str) {
    print!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = std::io::stdout().flush();
}

fn base64(bytes: &[u8]) -> String {
    const SET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from(b[0]) << 16 | u32::from(b[1]) << 8 | u32::from(b[2]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(SET[(n >> (18 - i * 6) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
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

impl Screen {
    /// Rows of the conversation as they are on screen, the open line
    /// included. Built fresh for the tail only; the rest is cached.
    fn view(&mut self, width: u16) -> Vec<String> {
        if width != self.width {
            self.width = width;
            self.rows.clear();
            self.wrapped = 0;
        }
        while self.wrapped + 1 < self.lines.len() {
            let wrapped = render::fold(&self.lines[self.wrapped], width as usize);
            self.rows.extend(wrapped);
            self.wrapped += 1;
        }
        let mut rows = self.rows.clone();
        if let Some(open) = self.lines.last() {
            rows.extend(render::fold(open, width as usize));
        }
        rows
    }

    /// How many rows of the window the conversation gets.
    fn depth(&self, rows: u16) -> usize {
        (rows as usize).saturating_sub(self.chrome.len()).max(1)
    }

    /// Where a place on the screen falls in the conversation.
    fn buffer_at(&mut self, column: u16, row: u16) -> Option<(usize, usize)> {
        let (width, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let view = self.view(width);
        let depth = self.depth(rows);
        let first = view.len().saturating_sub(depth + self.scroll);
        let index = first + row as usize;
        ((row as usize) < depth && index < view.len()).then_some((index, column as usize))
    }

    /// The marked text, if any is marked.
    fn selected(&self) -> Option<String> {
        let (from, to) = (self.from?, self.to?);
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        if start == end {
            return None;
        }
        let mut out = Vec::new();
        for row in start.0..=end.0 {
            let text = render::plain(self.rows.get(row).map(String::as_str).unwrap_or(""));
            let chars: Vec<char> = text.chars().collect::<Vec<char>>();
            let from = if row == start.0 { start.1 } else { 0 };
            let to = if row == end.0 { end.1 } else { chars.len() };
            let cut: String = chars
                .iter()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect();
            out.push(cut.trim_end().to_owned());
        }
        Some(out.join("\n")).filter(|t| !t.trim().is_empty())
    }
}

fn paint(s: &mut Screen) {
    if !s.open {
        return;
    }
    let (columns, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    let view = s.view(columns);
    let depth = s.depth(rows);
    // Scrolling stops at the top rather than running off into nothing.
    s.scroll = s.scroll.min(view.len().saturating_sub(depth));
    let first = view.len().saturating_sub(depth + s.scroll);

    let marked = s.from.zip(s.to).map(|(a, b)| if a <= b { (a, b) } else { (b, a) });

    let mut frame = String::from("\x1b[?25l");
    for row in 0..depth {
        let index = first + row;
        let text = view.get(index).map(String::as_str).unwrap_or("");
        let text = match marked {
            Some((start, end)) if index >= start.0 && index <= end.0 => {
                let from = if index == start.0 { start.1 } else { 0 };
                let to = if index == end.0 { Some(end.1) } else { None };
                render::mark(text, from, to)
            }
            _ => text.to_owned(),
        };
        frame.push_str(&format!("\x1b[{};1H\x1b[2K{text}\x1b[0m", row + 1));
    }
    let mut at = depth + 1;
    for line in &s.chrome {
        frame.push_str(&format!("\x1b[{at};1H\x1b[2K{line}\x1b[0m"));
        at += 1;
    }
    print!("{frame}");
    let _ = std::io::stdout().flush();
    s.painted = Instant::now();
}

#[cfg(test)]
mod tests {
    use super::base64;

    #[test]
    fn base64_is_what_the_terminal_expects() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"hello world"), "aGVsbG8gd29ybGQ=");
    }
}
