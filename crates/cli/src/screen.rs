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

use std::collections::HashMap;
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
    /// Lines laid out afresh for each width instead of wrapped, by their
    /// index in `lines`: the opening screen, whose face a wrap breaks apart.
    blocks: HashMap<usize, LayOut>,
    /// The model's thinking, by its line in `lines`: one row while folded,
    /// all of it once opened.
    folds: HashMap<usize, Fold>,
    /// The first of `rows` each wrapped line starts at, so a fold that
    /// changes is laid out again from there on.
    starts: Vec<usize>,
}

/// One reply's thinking, folded to a line unless opened.
struct Fold {
    text: String,
    open: bool,
    /// Still coming in.
    live: bool,
    began: Instant,
    took: Duration,
}

/// Lines of thinking shown under the heading while it comes in.
const PREVIEW: usize = 2;

impl Fold {
    fn rows(&self, width: usize) -> Vec<String> {
        let indent = "    ";
        let inner = width.saturating_sub(indent.len()).max(10);
        let body: Vec<String> = self
            .text
            .trim()
            .lines()
            .flat_map(|line| render::fold(line.trim_end(), inner))
            .map(|row| format!("{indent}{}", render::dim(&row)))
            .collect();
        let heading = if self.live {
            render::dim("  ✻ Thinking…")
        } else {
            let mark = if self.open { "▾" } else { "▸" };
            let hint = if self.open {
                ""
            } else {
                "  · click or ctrl+o to show"
            };
            render::dim(&format!(
                "  {mark} Thought for {}s{hint}",
                self.took.as_secs().max(1)
            ))
        };
        let mut rows = vec![heading];
        if self.open {
            rows.extend(body);
        } else if self.live {
            // The newest lines with words in them: a paragraph break is no
            // news of what it is thinking.
            let said: Vec<String> = body
                .into_iter()
                .filter(|row| !render::plain(row).trim().is_empty())
                .collect();
            let from = said.len().saturating_sub(PREVIEW);
            rows.extend(said.into_iter().skip(from));
        }
        rows
    }
}

/// Rows for a window so many columns wide, each fitting it.
pub type LayOut = Box<dyn Fn(usize) -> Vec<String> + Send>;

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
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
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
    print!("\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[?2004h");
    // Asking for the mouse has to go through crossterm on Windows. The
    // escape sequence alone tells the terminal to report mouse events, but a
    // console only delivers them once ENABLE_MOUSE_INPUT is set on its input
    // handle, which is an API call and not something that can be printed.
    // Without it no mouse event ever arrives and selecting does nothing.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    // Whether the window has focus, so a notification goes out only when it
    // has not. A terminal that cannot say ignores the request.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableFocusChange);
    let _ = std::io::stdout().flush();
}

/// Give the window back exactly as it was found.
pub fn close() {
    let Ok(mut s) = screen().lock() else { return };
    if !s.open {
        return;
    }
    s.open = false;
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableMouseCapture
    );
    print!("\x1b[?2004l\x1b[?25h\x1b[?1049l");
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

/// Add rows that are laid out again for each width the window takes, on
/// lines of their own.
pub fn block(lay_out: LayOut) {
    {
        let Ok(mut s) = screen().lock() else { return };
        if s.lines.last().is_some_and(|open| !open.is_empty()) {
            s.lines.push(String::new());
        }
        let at = s.lines.len().saturating_sub(1);
        s.blocks.insert(at, lay_out);
        s.lines.push(String::new());
        s.scroll = 0;
    }
    // At once: it comes in one piece, often within a frame of the box, and
    // a throttled paint would leave it unseen until something else paints.
    now();
}

/// Begin a reply's thinking, folded, on a line of its own; its id is for
/// `think` and `thought`.
pub fn fold_open() -> usize {
    let at = {
        let Ok(mut s) = screen().lock() else { return 0 };
        if s.lines.last().is_some_and(|open| !open.is_empty()) {
            s.lines.push(String::new());
        }
        let at = s.lines.len().saturating_sub(1);
        s.folds.insert(
            at,
            Fold {
                text: String::new(),
                open: false,
                live: true,
                began: Instant::now(),
                took: Duration::ZERO,
            },
        );
        s.lines.push(String::new());
        s.scroll = 0;
        at
    };
    now();
    at
}

/// More of the thinking `fold` holds.
pub fn think(fold: usize, text: &str) {
    {
        let Ok(mut s) = screen().lock() else { return };
        let Some(f) = s.folds.get_mut(&fold) else {
            return;
        };
        f.text.push_str(text);
        s.again_from(fold);
    }
    soon();
}

/// The thinking `fold` holds is all in: it folds to one line.
pub fn thought(fold: usize) {
    {
        let Ok(mut s) = screen().lock() else { return };
        let Some(f) = s.folds.get_mut(&fold) else {
            return;
        };
        if !f.live {
            return;
        }
        f.live = false;
        f.took = f.began.elapsed();
        s.again_from(fold);
    }
    now();
}

/// Open or fold the latest thinking that has finished, for ctrl+o.
pub fn toggle_last() {
    {
        let Ok(mut s) = screen().lock() else { return };
        let Some(&at) = s
            .folds
            .iter()
            .filter(|(_, f)| !f.live)
            .map(|(at, _)| at)
            .max()
        else {
            return;
        };
        s.toggle(at);
    }
    now();
}

/// Open or fold the thinking at a place on the screen, saying whether a
/// click there was on one.
pub fn toggle_at(column: u16, row: u16) -> bool {
    let toggled = {
        let Ok(mut s) = screen().lock() else {
            return false;
        };
        let Some((row, _)) = s.buffer_at(column, row) else {
            return false;
        };
        let line = s
            .starts
            .partition_point(|&start| start <= row)
            .saturating_sub(1);
        let on_fold = s.folds.get(&line).is_some_and(|f| !f.live);
        if on_fold {
            s.toggle(line);
        }
        on_fold
    };
    if toggled {
        now();
    }
    toggled
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
        s.blocks.clear();
        s.folds.clear();
        s.starts.clear();
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
        // Nothing to drag to without an anchor; the press sets that.
        if let Some(at) = s.buffer_at(column, row).filter(|_| s.from.is_some()) {
            s.to = Some(at);
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

/// What is marked, if anything.
pub fn selected() -> Option<String> {
    screen().lock().ok().and_then(|s| s.selected())
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
            self.starts.clear();
            self.wrapped = 0;
        }
        while self.wrapped + 1 < self.lines.len() {
            self.starts.push(self.rows.len());
            let wrapped = match (
                self.blocks.get(&self.wrapped),
                self.folds.get(&self.wrapped),
            ) {
                (Some(lay_out), _) => lay_out(width as usize),
                (None, Some(fold)) => fold.rows(width as usize),
                (None, None) => render::fold(&self.lines[self.wrapped], width as usize),
            };
            self.rows.extend(wrapped);
            self.wrapped += 1;
        }
        let mut rows = self.rows.clone();
        if let Some(open) = self.lines.last() {
            rows.extend(render::fold(open, width as usize));
        }
        rows
    }

    /// Lay the conversation out again from line `at` on, as it changed.
    fn again_from(&mut self, at: usize) {
        if at < self.wrapped {
            let row = self.starts.get(at).copied().unwrap_or(0);
            self.rows.truncate(row);
            self.starts.truncate(at);
            self.wrapped = at;
        }
        // A selection made over rows that move would mark the wrong text.
        self.from = None;
        self.to = None;
    }

    fn toggle(&mut self, at: usize) {
        if let Some(fold) = self.folds.get_mut(&at) {
            fold.open = !fold.open;
            self.again_from(at);
        }
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

    let marked = s
        .from
        .zip(s.to)
        .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });

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
    for (at, line) in (depth + 1..).zip(s.chrome.iter()) {
        frame.push_str(&format!("\x1b[{at};1H\x1b[2K{line}\x1b[0m"));
    }
    print!("{frame}");
    let _ = std::io::stdout().flush();
    s.painted = Instant::now();
}

#[cfg(test)]
mod tests {
    use super::{Duration, Fold, HashMap, Instant, Screen, base64};
    use crate::render;

    #[test]
    fn a_block_is_laid_out_again_for_each_width_rather_than_wrapped() {
        let mut s = Screen {
            lines: vec!["ab".into(), String::new(), String::new()],
            rows: Vec::new(),
            wrapped: 0,
            width: 0,
            scroll: 0,
            chrome: Vec::new(),
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
        };
        s.blocks
            .insert(1, Box::new(|width| vec!["x".repeat(width.min(6))]));
        assert_eq!(s.view(80)[..2], ["ab", "xxxxxx"]);
        assert_eq!(s.view(4)[..2], ["ab", "xxxx"]);
    }

    /// The rows above the open line at the foot, plain.
    fn shown(s: &mut Screen, width: u16) -> Vec<String> {
        let mut rows: Vec<String> = s.view(width).iter().map(|r| render::plain(r)).collect();
        rows.pop();
        rows
    }

    fn held_with_thinking(text: &str, live: bool) -> Screen {
        let mut s = Screen {
            lines: vec!["asked".into(), String::new(), String::new()],
            rows: Vec::new(),
            wrapped: 0,
            width: 0,
            scroll: 0,
            chrome: Vec::new(),
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
        };
        s.folds.insert(
            1,
            Fold {
                text: text.into(),
                open: false,
                live,
                began: Instant::now(),
                took: Duration::from_secs(12),
            },
        );
        s
    }

    #[test]
    fn thinking_folds_to_one_line_and_opens_with_its_line_breaks() {
        let text = "First, read the file.\nThen check the token.\n\nThen answer.";
        let mut s = held_with_thinking(text, false);
        let folded = shown(&mut s, 80);
        assert_eq!(
            folded[..2],
            ["asked", "  ▸ Thought for 12s  · click or ctrl+o to show"]
        );
        assert_eq!(folded.len(), 2);

        s.toggle(1);
        let opened = shown(&mut s, 80);
        assert_eq!(
            opened[1..],
            [
                "  ▾ Thought for 12s",
                "    First, read the file.",
                "    Then check the token.",
                "    ",
                "    Then answer."
            ]
        );
        s.toggle(1);
        assert_eq!(shown(&mut s, 80).len(), 2);
    }

    #[test]
    fn thinking_still_coming_in_shows_its_last_lines() {
        let mut s = held_with_thinking("one\ntwo\nthree", true);
        assert_eq!(
            shown(&mut s, 80)[1..],
            ["  ✻ Thinking…", "    two", "    three"]
        );
    }

    #[test]
    fn a_long_line_of_thinking_wraps_under_its_indent() {
        let mut s = held_with_thinking(&"word ".repeat(30), false);
        s.toggle(1);
        let rows = shown(&mut s, 40);
        assert!(rows.len() > 4, "{rows:?}");
        assert!(
            rows[2..]
                .iter()
                .all(|r| r.starts_with("    ") && r.chars().count() <= 40),
            "{rows:?}"
        );
    }

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
