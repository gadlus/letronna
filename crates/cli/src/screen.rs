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
use std::sync::atomic::{AtomicBool, Ordering};
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
    /// Rows the conversation had when last painted: what came since is added
    /// to `scroll` while it is back, so what is being read stays put.
    painted_rows: usize,
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
    /// What each row of the window held when last painted, and the window's
    /// size then: only rows that changed are written again.
    shown: Vec<String>,
    size: (u16, u16),
}

/// One reply's thinking, folded to a line unless opened.
struct Fold {
    text: String,
    open: bool,
    /// Still coming in.
    live: bool,
    began: Instant,
    took: Duration,
    /// A tool's output rather than the model's thinking.
    output: bool,
}

/// Rows of a tool's output shown before the rest is folded under them.
const OUTPUT_ROWS: usize = 12;

/// Lines of thinking shown under the heading while it comes in.
const PREVIEW: usize = 2;

impl Fold {
    fn rows(&self, width: usize) -> Vec<String> {
        if self.output {
            return self.output_rows(width);
        }
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

impl Fold {
    /// A tool's output as rows `width` wide: all of them when open or few
    /// enough, else the first `OUTPUT_ROWS` -- rows, not lines, so one long
    /// line cannot fill the window -- and how many more there are.
    fn output_rows(&self, width: usize) -> Vec<String> {
        let indent = "      ";
        let inner = width.saturating_sub(indent.len()).max(10);
        let body: Vec<String> = (self.text.trim_end().lines())
            .flat_map(|line| render::fold(line.trim_end(), inner))
            .map(|row| format!("{indent}{}", render::dim(&row)))
            .collect();
        if body.len() <= OUTPUT_ROWS {
            return body;
        }
        if self.open {
            let mut rows = body;
            rows.push(format!(
                "{indent}{}",
                render::dim("▴ click or ctrl+o to fold")
            ));
            return rows;
        }
        let hidden = body.len() - OUTPUT_ROWS;
        let mut rows: Vec<String> = body.into_iter().take(OUTPUT_ROWS).collect();
        rows.push(format!(
            "{indent}{}",
            render::dim(&format!(
                "… {hidden} more rows · click or ctrl+o to show them"
            ))
        ));
        rows
    }

    /// Whether opening it shows anything more: thinking always, output only
    /// when some of it is folded away.
    fn hides(&self, width: usize) -> bool {
        !self.output || self.output_rows(width).len() > OUTPUT_ROWS
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
            painted_rows: 0,
            chrome: Vec::new(),
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
            shown: Vec::new(),
            size: (0, 0),
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
    s.shown.clear();
    print!("\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[?2004h");
    // Asking for the mouse has to go through crossterm on Windows. The
    // escape sequence alone tells the terminal to report mouse events, but a
    // console only delivers them once ENABLE_MOUSE_INPUT is set on its input
    // handle, which is an API call and not something that can be printed.
    // Without it no mouse event ever arrives and selecting does nothing.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableMouseCapture);
    // Presses, drags and the wheel, not every move of the pointer over the
    // window, each of which would otherwise be read and drawn for nothing.
    #[cfg(unix)]
    print!("\x1b[?1003l");
    // Whether the window has focus, so a notification goes out only when it
    // has not. A terminal that cannot say ignores the request.
    let _ = crossterm::execute!(std::io::stdout(), crossterm::event::EnableFocusChange);
    // Shift and enter reach a terminal program as a plain enter unless the
    // terminal is asked to say which keys were held, and only some can.
    // Those that say they can are asked; the rest are left as they are, so
    // nothing else changes there.
    #[cfg(unix)]
    if crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false) {
        let flags = crossterm::event::KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES;
        let pushed = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PushKeyboardEnhancementFlags(flags)
        );
        SHIFT_TOLD.store(pushed.is_ok(), Ordering::Relaxed);
    }
    let _ = std::io::stdout().flush();
}

/// Whether the terminal was asked to tell shift-enter from enter.
static SHIFT_TOLD: AtomicBool = AtomicBool::new(false);

/// Whether shift-enter reaches the box as itself: a console says which keys
/// are held of its own accord, and a terminal only once asked.
pub fn shift_enter() -> bool {
    cfg!(windows) || SHIFT_TOLD.load(Ordering::Relaxed)
}

/// Stop telling shift-enter from enter, if the terminal was asked to. It
/// takes no lock, so a panic can call it whatever state the screen is in,
/// and it goes before leaving the conversation's own screen, which kitty
/// keeps a stack of these flags for apart from the shell's.
pub fn forget_keyboard() {
    if SHIFT_TOLD.swap(false, Ordering::Relaxed) {
        let _ = crossterm::execute!(
            std::io::stdout(),
            crossterm::event::PopKeyboardEnhancementFlags
        );
    }
}

/// Give the window back exactly as it was found -- even after a panic left
/// the screen's lock poisoned, which leaves what it guards whole enough to
/// say whether the window is held.
pub fn close() {
    let mut s = screen()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !s.open {
        return;
    }
    s.open = false;
    forget_keyboard();
    let _ = crossterm::execute!(
        std::io::stdout(),
        crossterm::event::DisableFocusChange,
        crossterm::event::DisableMouseCapture
    );
    print!("\x1b[?2004l\x1b[?25h\x1b[?1049l");
    let _ = std::io::stdout().flush();
}

/// Whether the conversation ends in a blank line with nothing on the line
/// after it yet, so another blank line would make a gap of two.
pub fn after_blank() -> bool {
    screen().lock().is_ok_and(|s| s.after_blank())
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
                output: false,
            },
        );
        s.lines.push(String::new());
        at
    };
    now();
    at
}

/// A tool's output on lines of its own, its first rows shown and the rest
/// folded under them, which a click or ctrl+o shows.
pub fn output(text: &str) {
    {
        let Ok(mut s) = screen().lock() else { return };
        if s.lines.last().is_some_and(|open| !open.is_empty()) {
            s.lines.push(String::new());
        }
        let at = s.lines.len().saturating_sub(1);
        s.folds.insert(
            at,
            Fold {
                text: text.to_owned(),
                open: false,
                live: false,
                began: Instant::now(),
                took: Duration::ZERO,
                output: true,
            },
        );
        s.lines.push(String::new());
    }
    soon();
}

/// More of the thinking `fold` holds.
pub fn think(fold: usize, text: &str) {
    {
        let Ok(mut s) = screen().lock() else { return };
        let Some(f) = s.folds.get_mut(&fold) else {
            return;
        };
        // From the model, so only colour gets through, as with any reply.
        f.text.push_str(&render::safe(text));
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
        let width = (s.width as usize).max(20);
        let Some(&at) = s
            .folds
            .iter()
            .filter(|(_, f)| !f.live && (f.open || f.hides(width)))
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
        s.shown.clear();
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
pub fn copy(text: &str) -> bool {
    print!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
    let _ = std::io::stdout().flush();
    system_copy(text)
}

/// Hand `text` to the system's own clipboard command, the first of them
/// here that takes it: pbcopy on macOS, clip on Windows, and wl-copy, xclip
/// or xsel on Linux, which need a display to reach. The terminal's own way
/// above is not taken up by every terminal -- Terminal.app ignores it,
/// iTerm2 and tmux only with a setting -- and cannot say whether it was, so
/// this is what says a copy took.
fn system_copy(text: &str) -> bool {
    use std::process::{Command, Stdio};
    if over_ssh(|name| std::env::var_os(name)) {
        return false;
    }
    let commands: &[(&str, &[&str])] = if cfg!(target_os = "macos") {
        &[("pbcopy", &[])]
    } else if cfg!(windows) {
        &[("clip.exe", &[])]
    } else {
        &[
            ("wl-copy", &[]),
            ("xclip", &["-selection", "clipboard"]),
            ("xsel", &["--clipboard", "--input"]),
        ]
    };
    // clip reads UTF-16 when the text says so, and the console's code page
    // otherwise, which would mangle anything outside it.
    let bytes: Vec<u8> = if cfg!(windows) {
        [0xFF, 0xFE]
            .into_iter()
            .chain(text.encode_utf16().flat_map(u16::to_le_bytes))
            .collect()
    } else {
        text.as_bytes().to_vec()
    };
    for (program, args) in commands {
        let spawned = Command::new(program)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut child) = spawned else {
            continue;
        };
        let wrote = (child.stdin.take()).is_some_and(|mut stdin| stdin.write_all(&bytes).is_ok());
        if child.wait().is_ok_and(|status| status.success()) && wrote {
            return true;
        }
    }
    false
}

/// Whether this runs in a session reached over ssh, as `var` reads the
/// environment: there the system's clipboard is the far machine's, which
/// pbcopy or xclip with X forwarding fill without the copy reaching the
/// one at the keyboard, so only the terminal's own way can say it did.
fn over_ssh(var: impl Fn(&str) -> Option<std::ffi::OsString>) -> bool {
    ["SSH_TTY", "SSH_CONNECTION"]
        .iter()
        .any(|name| var(name).is_some_and(|value| !value.is_empty()))
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

/// Have the next paint write every row, not only those that changed, for a
/// terminal that may no longer show the last frame: Apple's Terminal can
/// drop what a tab showed while another tab was in front, and a window
/// resized and put back between two paints comes back the size it was.
pub fn repaint() {
    if let Ok(mut s) = screen().lock() {
        s.shown.clear();
    }
}

impl Screen {
    /// Rows of the conversation as they are on screen, the open line
    /// included. Built fresh for the tail only; the rest is cached.
    #[cfg(test)]
    fn view(&mut self, width: u16) -> Vec<String> {
        let open = self.lay_out(width);
        let mut rows = self.rows.clone();
        rows.extend(open);
        rows
    }

    /// Bring the wrapped rows up to date for `width`, handing back those of
    /// the line still open, which change too often to keep: the rest are
    /// read where they are, rather than the whole conversation copied for
    /// each frame.
    fn lay_out(&mut self, width: u16) -> Vec<String> {
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
        match self.lines.last() {
            Some(open) => render::fold(open, width as usize),
            None => Vec::new(),
        }
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

    /// Scrolled back, keep the view on what is being read while the reply
    /// goes on growing under it, or a fold under its top closes, now the
    /// conversation is `rows` long; following, it moves with the foot.
    fn hold(&mut self, rows: usize) {
        if self.scroll > 0 {
            self.scroll += rows.saturating_sub(self.painted_rows);
            self.scroll = self
                .scroll
                .saturating_sub(self.painted_rows.saturating_sub(rows));
        }
        self.painted_rows = rows;
    }

    /// Whether the open line and the one above it are both empty: a fold
    /// or a block stands on an empty line too, but is not a blank one.
    fn after_blank(&self) -> bool {
        let at = self.lines.len();
        at >= 2
            && self.lines[at - 1].is_empty()
            && self.lines[at - 2].is_empty()
            && !self.blocks.contains_key(&(at - 2))
            && !self.folds.contains_key(&(at - 2))
    }

    /// How many rows of the window the conversation gets.
    fn depth(&self, rows: u16) -> usize {
        (rows as usize).saturating_sub(self.chrome.len()).max(1)
    }

    /// Where a place on the screen falls in the conversation.
    fn buffer_at(&mut self, column: u16, row: u16) -> Option<(usize, usize)> {
        let (width, rows) = crossterm::terminal::size().unwrap_or((80, 24));
        let total = self.rows.len() + self.lay_out(width).len();
        let depth = self.depth(rows);
        let first = total.saturating_sub(depth + self.scroll);
        let index = first + row as usize;
        ((row as usize) < depth && index < total).then_some((index, column as usize))
    }

    /// The marked text, if any is marked.
    fn selected(&self) -> Option<String> {
        let (from, to) = (self.from?, self.to?);
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        if start == end {
            return None;
        }
        // What was written rather than how it is drawn: a line wrapped over
        // several rows is one line again, without the indent the rows after
        // its first hang from, and the margin or mark in front of a line --
        // "    ", "  ● ", "  ⎿ " -- is left behind, so a command copied from
        // an answer pastes as the one line it is.
        let mut out = String::new();
        let mut last_full = false;
        for row in start.0..=end.0 {
            let text = render::plain(self.rows.get(row).map(String::as_str).unwrap_or(""));
            let chars: Vec<char> = text.chars().collect::<Vec<char>>();
            // The selection's ends are columns, which a wide character takes
            // two of.
            let from = if row == start.0 {
                render::char_at(&text, start.1)
            } else {
                0
            };
            let to = if row == end.0 {
                render::char_at(&text, end.1)
            } else {
                chars.len()
            };
            let cut: String = chars
                .iter()
                .skip(from)
                .take(to.saturating_sub(from))
                .collect();
            let starts_line = self.starts.binary_search(&row).is_ok() || row == 0;
            let cut = match (starts_line, from) {
                (true, 0) => unmarked(&cut).to_owned(),
                (true, _) => cut,
                (false, _) => cut.trim_start().to_owned(),
            };
            if row > start.0 {
                // A row broken mid-word ran to the edge; any other broke at
                // a space, which the break took.
                match (starts_line, last_full) {
                    (true, _) => out.push('\n'),
                    (false, false) => out.push(' '),
                    (false, true) => {}
                }
            }
            last_full = chars.len() >= self.width as usize
                && chars.last().is_some_and(|c| !c.is_whitespace());
            out.push_str(cut.trim_end());
        }
        Some(out).filter(|t| !t.trim().is_empty())
    }
}

/// A line as written, without the margin or the mark the conversation
/// puts in front of it: four spaces, or two and a mark and a space.
fn unmarked(row: &str) -> &str {
    if let Some(rest) = row.strip_prefix(render::PAD) {
        return rest;
    }
    let mut chars = row.char_indices();
    match (chars.next(), chars.next(), chars.next(), chars.next()) {
        (Some((_, ' ')), Some((_, ' ')), Some((_, mark)), Some((at, ' '))) if mark != ' ' => {
            &row[at + 1..]
        }
        _ => row,
    }
}

fn paint(s: &mut Screen) {
    if !s.open {
        return;
    }
    let (columns, rows) = crossterm::terminal::size().unwrap_or((80, 24));
    // A window of another size has every row in another place.
    if (columns, rows) != s.size {
        s.size = (columns, rows);
        s.shown.clear();
    }
    let open = s.lay_out(columns);
    let total = s.rows.len() + open.len();
    let depth = s.depth(rows);
    s.hold(total);
    // Scrolling stops at the top rather than running off into nothing.
    s.scroll = s.scroll.min(total.saturating_sub(depth));
    let first = total.saturating_sub(depth + s.scroll);

    let marked = s
        .from
        .zip(s.to)
        .map(|(a, b)| if a <= b { (a, b) } else { (b, a) });

    let mut next: Vec<String> = Vec::with_capacity(depth + s.chrome.len());
    for row in 0..depth {
        let index = first + row;
        let text = match index.checked_sub(s.rows.len()) {
            Some(at) => open.get(at).map(String::as_str).unwrap_or(""),
            None => s.rows[index].as_str(),
        };
        next.push(match marked {
            Some((start, end)) if index >= start.0 && index <= end.0 => {
                let from = if index == start.0 { start.1 } else { 0 };
                let to = if index == end.0 { Some(end.1) } else { None };
                render::mark(text, from, to)
            }
            _ => text.to_owned(),
        });
    }
    next.extend(s.chrome.iter().cloned());

    // Only the rows that changed are written, inside a synchronized update
    // that terminals which know it draw at once and others pass over: the
    // waiting line turning or one word streaming in no longer sends the
    // whole window again, which over ssh and on a console was a flicker.
    let mut frame = String::from("\x1b[?2026h\x1b[?25l");
    for (at, text) in next.iter().enumerate() {
        if s.shown.get(at) != Some(text) {
            frame.push_str(&format!("\x1b[{};1H\x1b[2K{text}\x1b[0m", at + 1));
        }
    }
    frame.push_str("\x1b[?2026l");
    s.shown = next;
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
            painted_rows: 0,
            chrome: Vec::new(),
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
            shown: Vec::new(),
            size: (0, 0),
        };
        s.blocks
            .insert(1, Box::new(|width| vec!["x".repeat(width.min(6))]));
        assert_eq!(s.view(80)[..2], ["ab", "xxxxxx"]);
        assert_eq!(s.view(4)[..2], ["ab", "xxxx"]);
    }

    #[test]
    fn copied_text_joins_wrapped_rows_without_marks() {
        let mut s = Screen {
            lines: vec![
                "  ● the reply text that wraps across rows here".into(),
                "    cargo build --release".into(),
                "    fn main() {".into(),
                "        run();".into(),
                String::new(),
            ],
            rows: Vec::new(),
            wrapped: 0,
            width: 0,
            scroll: 0,
            painted_rows: 0,
            chrome: Vec::new(),
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
            shown: Vec::new(),
            size: (0, 0),
        };
        let rows = s.view(20);
        let last = rows.len() - 2;
        s.from = Some((0, 0));
        s.to = Some((last, rows[last].chars().count()));
        assert_eq!(
            s.selected().unwrap(),
            "the reply text that wraps across rows here\ncargo build --release\nfn main() {\n    run();"
        );
    }

    #[test]
    fn a_copy_over_ssh_is_left_to_the_terminal() {
        let given = |names: &'static [&'static str]| {
            move |name: &str| {
                names
                    .contains(&name)
                    .then(|| "10.0.0.2 51234 10.0.0.9 22".into())
            }
        };
        assert!(super::over_ssh(given(&["SSH_CONNECTION"])));
        assert!(super::over_ssh(given(&["SSH_TTY"])));
        assert!(!super::over_ssh(given(&["TERM"])));
        assert!(!super::over_ssh(|_| Some(std::ffi::OsString::new())));
    }

    #[test]
    fn output_while_scrolled_back_keeps_the_view() {
        let mut s = Screen {
            lines: vec![String::new()],
            rows: Vec::new(),
            wrapped: 0,
            width: 0,
            scroll: 0,
            painted_rows: 100,
            chrome: Vec::new(),
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
            shown: Vec::new(),
            size: (0, 0),
        };
        // Following, new rows move the view with them.
        s.hold(104);
        assert_eq!(s.scroll, 0);
        // Scrolled back, the view moves back by what came, staying put.
        s.scroll = 20;
        s.hold(110);
        assert_eq!(s.scroll, 26);
        s.hold(110);
        assert_eq!(s.scroll, 26);
        // Rows going, as a fold under it closes, take the view back as many.
        s.hold(100);
        assert_eq!(s.scroll, 16);
    }

    #[test]
    fn long_output_shows_its_first_rows_and_opens_in_place() {
        let mut fold = Fold {
            text: (1..=30)
                .map(|n| format!("line {n}"))
                .collect::<Vec<_>>()
                .join("\n"),
            open: false,
            live: false,
            began: Instant::now(),
            took: Duration::ZERO,
            output: true,
        };
        let rows: Vec<String> = fold.rows(80).iter().map(|r| render::plain(r)).collect();
        assert_eq!(rows.len(), super::OUTPUT_ROWS + 1);
        assert!(rows.last().unwrap().contains("18 more rows"), "{rows:?}");
        assert!(fold.hides(80));
        fold.open = true;
        assert_eq!(fold.rows(80).len(), 31);
        // One long line counts by the rows it takes, not as one line.
        fold.open = false;
        fold.text = "x".repeat(4000);
        assert_eq!(fold.rows(80).len(), super::OUTPUT_ROWS + 1);
        // Short output has nothing folded away.
        fold.text = "ok".into();
        assert!(!fold.hides(80));
    }

    #[test]
    fn a_blank_line_is_known_from_a_fold_on_an_empty_line() {
        let mut s = Screen {
            lines: vec!["› the question".into(), String::new(), String::new()],
            rows: Vec::new(),
            wrapped: 0,
            width: 0,
            scroll: 0,
            painted_rows: 0,
            chrome: Vec::new(),
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
            shown: Vec::new(),
            size: (0, 0),
        };
        assert!(s.after_blank());
        s.blocks
            .insert(1, Box::new(|width| vec!["x".repeat(width.min(6))]));
        assert!(!s.after_blank());
        s.lines = vec!["text".into(), String::new()];
        assert!(!s.after_blank());
        s.lines = vec![String::new()];
        assert!(!s.after_blank());
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
            painted_rows: 0,
            chrome: Vec::new(),
            from: None,
            to: None,
            open: false,
            painted: Instant::now(),
            blocks: HashMap::new(),
            folds: HashMap::new(),
            starts: Vec::new(),
            shown: Vec::new(),
            size: (0, 0),
        };
        s.folds.insert(
            1,
            Fold {
                text: text.into(),
                open: false,
                live,
                began: Instant::now(),
                took: Duration::from_secs(12),
                output: false,
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
