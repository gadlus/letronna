//! The input box, held at the foot of the window.
//!
//! The conversation is printed into the terminal's own scrollback, not into a
//! screen of our own. That is the design decision here: the wheel, the
//! scrollbar, dragging to select and copying all belong to the terminal and
//! keep working, because we never ask for them. What we take is the last few
//! rows, by shrinking the scrolling region to everything above them, so the
//! box stays put while answers scroll past it. Scroll back far enough and the
//! box goes with everything else, which is the price and a fair one.
//!
//! Keys are read on the same loop that pumps the model's reply, so the next
//! question can be typed while the current one is still being answered; a
//! line entered mid-answer waits its turn rather than cutting in.

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use std::collections::VecDeque;
use std::io::Write;
use std::time::Duration;

use crate::edit::Line;
use crate::render;

pub struct Command {
    pub name: &'static str,
    pub blurb: &'static str,
}

pub const COMMANDS: &[Command] = &[
    Command {
        name: "/help",
        blurb: "what Letronna can do here",
    },
    Command {
        name: "/new",
        blurb: "start a fresh session",
    },
    Command {
        name: "/plan",
        blurb: "read and search only, never write or run",
    },
    Command {
        name: "/model",
        blurb: "show or change the model",
    },
    Command {
        name: "/resume",
        blurb: "pick up a session from before",
    },
    Command {
        name: "/where",
        blurb: "show the project folder and session file",
    },
    Command {
        name: "/clear",
        blurb: "clear the screen",
    },
    Command {
        name: "/exit",
        blurb: "leave",
    },
];

/// Commands the menu will show at once, so a long list cannot eat the window.
const MENU_LIMIT: usize = 8;

/// Rows the box will grow to before what is inside it starts to scroll.
/// Enough for a paragraph, not so many that the conversation is squeezed out.
const MAX_ROWS: usize = 8;

/// How long a pump waits for a keystroke before handing the loop back. Short
/// enough that typing feels immediate, long enough not to spin.
const TICK: Duration = Duration::from_millis(20);

fn matches(text: &str) -> Vec<&'static Command> {
    if !text.starts_with('/') || text.contains(char::is_whitespace) {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(text))
        .collect()
}

/// Whether more input is already waiting. The wait is long enough to cover
/// the gap between two characters of a paste and far shorter than the gap
/// between two keystrokes of a person typing.
fn pending() -> bool {
    event::poll(Duration::from_millis(15)).unwrap_or(false)
}

/// A question with a fixed set of answers, asked in the box instead of the
/// line: approvals are the same three or four answers every time, and typing
/// a letter for them is a small thing to get wrong in a hurry.
struct Choice {
    question: String,
    detail: Vec<String>,
    options: Vec<String>,
    picked: usize,
}

pub struct Bar {
    line: Line,
    picked: usize,
    footer: String,
    /// Lines entered but not yet answered, in the order they were typed.
    queued: VecDeque<String>,
    /// Rows the box is holding at the foot of the window.
    height: u16,
    /// Where the box was last painted, so a resize can wipe it from there
    /// rather than leaving a copy stranded mid-conversation.
    painted: Option<(u16, u16)>,
    live: bool,
    done: bool,
    choice: Option<Choice>,
    answer: Option<usize>,
    /// True while a turn is in flight, which is what makes escape mean stop.
    working: bool,
    interrupt: bool,
    /// The waiting line, while nothing has come back yet. Drawn here rather
    /// than by a thread of its own, so there is only ever one writer to the
    /// terminal and a keystroke cannot land in the middle of a redraw.
    waiting: Option<crate::spinner::Waiting>,
    /// When the waiting line was last moved on.
    ticked: std::time::Instant,
}

impl Bar {
    pub fn new(footer: String) -> Self {
        Self {
            line: Line::default(),
            picked: 0,
            footer,
            queued: VecDeque::new(),
            height: 0,
            painted: None,
            live: false,
            done: false,
            choice: None,
            answer: None,
            working: false,
            interrupt: false,
            waiting: None,
            ticked: std::time::Instant::now(),
        }
    }

    /// Take the last rows of the window.
    pub fn install(&mut self) {
        if !render::interactive() || terminal::enable_raw_mode().is_err() {
            return;
        }
        render::set_raw(true);
        allow_selection();
        self.live = true;
        // The real cursor stays where output lands; the one in the box is
        // drawn, so a redraw cannot pull the conversation out of line.
        print!("\x1b[?25l");
        // Terminals that hand pasted text over in one piece are asked to.
        print!("\x1b[?2004h");
        self.height = self.wanted();
        self.make_room(self.height);
        self.frame();
        self.draw();
    }

    /// Give the window back exactly as it was found.
    pub fn uninstall(&mut self) {
        if !self.live {
            return;
        }
        let (_, rows) = terminal::size().unwrap_or((80, 24));
        let top = rows.saturating_sub(self.height) + 1;
        print!("\x1b[r");
        for row in top..=rows {
            print!("\x1b[{row};1H\x1b[2K");
        }
        print!("\x1b[{top};1H\x1b[?2004l\x1b[?25h");
        render::flush();
        let _ = terminal::disable_raw_mode();
        render::set_raw(false);
        self.live = false;
        self.painted = None;
    }

    /// Paint the box again, for whoever has just wiped the screen from
    /// under it.
    pub fn redraw(&mut self) {
        if !self.live {
            return;
        }
        self.painted = None;
        self.make_room(self.height);
        self.frame();
        self.draw();
    }

    pub fn set_footer(&mut self, footer: String) {
        self.footer = footer;
        self.settle();
    }

    /// Wait for a whole line. `None` when the user is done.
    pub fn wait(&mut self) -> Option<String> {
        if let Some(line) = self.queued.pop_front() {
            return Some(line);
        }
        if !self.live {
            return crate::input::read(&format!("{} ", render::blue("›")));
        }
        loop {
            if self.done {
                return None;
            }
            if let Some(line) = self.queued.pop_front() {
                return Some(line);
            }
            self.pump();
        }
    }

    /// Ask a question with a fixed set of answers. Returns the one picked, or
    /// `None` if the user left instead.
    pub fn choose(
        &mut self,
        question: &str,
        detail: Vec<String>,
        options: &[&str],
    ) -> Option<usize> {
        if !self.live {
            render::line(&render::bold(question));
            for (i, option) in options.iter().enumerate() {
                render::line(&render::dim(&format!("  {}. {option}", i + 1)));
            }
            let typed = crate::input::read(&format!("{} ", render::blue("›")))?;
            return typed.trim().parse::<usize>().ok()?.checked_sub(1);
        }

        self.choice = Some(Choice {
            question: question.to_owned(),
            detail,
            options: options.iter().map(|o| (*o).to_owned()).collect(),
            picked: 0,
        });
        self.answer = None;
        self.settle();
        loop {
            if self.done {
                self.choice = None;
                self.settle();
                return None;
            }
            if let Some(answer) = self.answer.take() {
                self.choice = None;
                self.settle();
                return Some(answer);
            }
            self.pump();
        }
    }

    /// Deal with whatever keys have arrived and hand the loop straight back.
    /// Called from inside a running turn, which is what lets you type while
    /// the model is still talking.
    pub fn pump(&mut self) {
        if !self.live {
            return;
        }
        // Where the line stood before this drain. A drain that swallows a
        // burst of keys is how a paste looks on a console that has no paste
        // event to send.
        let mark = self.line.at();
        let mut drained = 0usize;
        while event::poll(TICK).unwrap_or(false) {
            drained += 1;
            match event::read() {
                Ok(Event::Key(key)) => self.key(key),
                // Terminals that hand pasted text over in one piece. Windows
                // consoles do not, which the enter key handles instead.
                Ok(Event::Paste(text)) => {
                    if self.choice.is_none() {
                        self.line.paste(&text);
                        self.picked = 0;
                        self.settle();
                    }
                }
                Ok(Event::Resize(_, _)) => {
                    self.height = self.wanted();
                    self.frame();
                    self.draw();
                }
                _ => continue,
            }
        }
        // Nobody types eight characters inside one poll. If that many arrived
        // and they put several lines in the box, it was a paste, and it is
        // stood behind a placeholder now that it is all here.
        if drained > 8 && self.choice.is_none() {
            self.line.fold_burst(mark);
            self.settle();
        }
        // The waiting line turns on this loop, which runs every twenty
        // milliseconds whether or not anything was typed.
        if self.waiting.is_some() && self.ticked.elapsed() >= Duration::from_millis(90) {
            self.ticked = std::time::Instant::now();
            self.draw();
        }
    }

    /// Show or take away the waiting line.
    pub fn wait_line(&mut self, on: bool) {
        self.waiting = on.then(crate::spinner::Waiting::new);
        self.settle();
    }

    /// Say whether a turn is running, so escape knows what it means.
    pub fn working(&mut self, on: bool) {
        self.working = on;
        if !on {
            self.interrupt = false;
        }
    }

    /// Whether the user has asked to stop the turn, clearing the request.
    pub fn interrupted(&mut self) -> bool {
        std::mem::take(&mut self.interrupt)
    }

    /// True once the user has asked to leave.
    pub fn finished(&self) -> bool {
        self.done
    }

    fn key(&mut self, key: KeyEvent) {
        // Windows reports press and release; act on one of them.
        if key.kind != KeyEventKind::Press {
            return;
        }
        if self.choice.is_some() {
            self.pick(key);
            return;
        }
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let options = matches(self.line.text());
        match key.code {
            KeyCode::Char('c') if control => {
                // While an answer is running this stops it and leaves; the
                // turn does not have to finish first, which is the whole
                // reason anyone reaches for it.
                if self.working {
                    self.interrupt = true;
                    self.done = true;
                } else if self.line.is_empty() {
                    self.done = true;
                } else {
                    self.line.clear();
                    self.picked = 0;
                }
            }
            KeyCode::Char('d') if control && self.line.is_empty() => self.done = true,
            // A newline on purpose, for writing a paragraph.
            KeyCode::Enter if shift || alt => self.line.insert('\n'),
            // A paste arriving as keystrokes: nobody types the next character
            // within a few milliseconds of pressing enter.
            KeyCode::Enter if pending() => self.line.insert('\n'),
            KeyCode::Enter => {
                if let Some(choice) = options.get(self.picked) {
                    self.line.set(choice.name);
                }
                self.picked = 0;
                let line = self.line.take();
                let line = line.trim().to_owned();
                if !line.is_empty() {
                    self.echo(&line);
                    self.queued.push_back(line);
                }
            }
            KeyCode::Tab => {
                if let Some(choice) = options.get(self.picked) {
                    self.line.set(choice.name);
                    self.picked = 0;
                }
            }
            KeyCode::Up if !options.is_empty() => {
                self.picked = self.picked.checked_sub(1).unwrap_or(options.len() - 1)
            }
            KeyCode::Down if !options.is_empty() => {
                self.picked = (self.picked + 1) % options.len()
            }
            KeyCode::Up => self.line.up(),
            KeyCode::Down => self.line.down(),
            KeyCode::Left if control => self.line.word_left(),
            KeyCode::Right if control => self.line.word_right(),
            KeyCode::Left => self.line.left(),
            KeyCode::Right => self.line.right(),
            KeyCode::Home => self.line.home(),
            KeyCode::End => self.line.end(),
            // Escape stops an answer when one is running, and is otherwise
            // the quickest way to drop what you were typing.
            KeyCode::Esc if self.working && self.line.is_empty() => self.interrupt = true,
            KeyCode::Esc => {
                self.line.clear();
                self.picked = 0;
            }
            // Ctrl-Backspace reaches a good many terminals as Ctrl-H.
            KeyCode::Backspace if control => self.line.backspace_word(),
            KeyCode::Char('h') if control => self.line.backspace_word(),
            KeyCode::Char('w') if control => self.line.backspace_word(),
            KeyCode::Delete if control => self.line.delete_word(),
            KeyCode::Backspace => {
                self.line.backspace();
                self.picked = 0;
            }
            KeyCode::Delete => self.line.delete(),
            KeyCode::Char(ch) if !control => {
                self.line.insert(ch);
                self.picked = 0;
            }
            _ => return,
        }
        self.settle();
    }

    /// Steer a question. Enter takes what is highlighted, a digit takes that
    /// one outright, and escape is the same as picking the last answer, which
    /// is always the one that does nothing.
    fn pick(&mut self, key: KeyEvent) {
        let Some(choice) = self.choice.as_mut() else {
            return;
        };
        let last = choice.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => choice.picked = choice.picked.checked_sub(1).unwrap_or(last),
            KeyCode::Down => choice.picked = (choice.picked + 1) % choice.options.len(),
            KeyCode::Enter => self.answer = Some(choice.picked),
            KeyCode::Esc => self.answer = Some(last),
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => self.done = true,
            KeyCode::Char(digit) if digit.is_ascii_digit() => {
                if let Some(index) = digit.to_digit(10).and_then(|d| (d as usize).checked_sub(1)) {
                    if index < choice.options.len() {
                        self.answer = Some(index);
                    }
                }
            }
            _ => return,
        }
        self.settle();
    }

    /// Put the line into the conversation above the box, so your side of it
    /// is in the scrollback where the answers are.
    fn echo(&self, line: &str) {
        render::blank();
        let mut parts = line.lines();
        render::said(
            &render::blue("›"),
            &render::bold(parts.next().unwrap_or_default()),
        );
        // The rest of a pasted block sits under the first line rather than
        // behind another arrow: it is one thing you said, not several.
        for part in parts {
            render::line(&render::bold(part));
        }
        render::blank();
    }

    /// Rows the box wants, given what is in it.
    fn wanted(&self) -> u16 {
        let room = self.room();
        let body = match &self.choice {
            Some(choice) => {
                let mut rows = render::fold(&choice.question, room).len();
                rows += choice
                    .detail
                    .iter()
                    .map(|d| render::fold(d, room).len())
                    .sum::<usize>();
                // The blank line before the answers, and the answers.
                rows += 1 + choice.options.len();
                rows
            }
            None => self.rows().len(),
        };
        let menu = match self.choice {
            Some(_) => 0,
            None => matches(self.line.text()).len().min(MENU_LIMIT),
        };
        // Two borders, the body, whatever the menu takes, the footer, and
        // the waiting line when there is one.
        (2 + body + menu + 1 + usize::from(self.waiting.is_some())) as u16
    }

    /// Room inside the box for what is typed.
    fn room(&self) -> usize {
        render::width()
            .saturating_sub(render::MARGIN.len() * 2 + 4)
            .max(8)
    }

    /// The typed text, wrapped. Never more than `MAX_ROWS`: past that it
    /// follows the cursor.
    fn rows(&self) -> Vec<String> {
        let room = self.room().saturating_sub(2).max(4);
        let mut rows: Vec<String> = Vec::new();
        for part in self.line.text().split('\n') {
            rows.extend(render::fold(part, room));
        }
        if rows.is_empty() {
            rows.push(String::new());
        }
        if rows.len() > MAX_ROWS {
            let (row, _) = self.line.line_of_cursor();
            let last = rows.len() - MAX_ROWS;
            let first = row.saturating_sub(MAX_ROWS - 1).min(last);
            rows = rows[first..first + MAX_ROWS].to_vec();
        }
        rows
    }

    /// Redraw, taking more or fewer rows first if what is in the box has
    /// changed shape.
    fn settle(&mut self) {
        if !self.live {
            return;
        }
        let wanted = self.wanted();
        if wanted != self.height {
            if wanted > self.height {
                self.make_room(wanted - self.height);
            }
            self.height = wanted;
            self.frame();
        }
        self.draw();
    }

    /// Scroll the conversation up by `rows`, so nothing already printed ends
    /// up underneath the box.
    fn make_room(&self, rows: u16) {
        print!("\x1b[r");
        for _ in 0..rows {
            print!("{}", render::nl());
        }
        render::flush();
    }

    /// Shrink the scrolling region to everything above the box and leave the
    /// cursor at the bottom of it, which is where output will land.
    fn frame(&mut self) {
        let (_, rows) = terminal::size().unwrap_or((80, 24));
        let bottom = rows.saturating_sub(self.height).max(1);
        print!("\x1b[r");
        self.erase(rows);
        print!("\x1b[1;{bottom}r");
        for row in (bottom + 1)..=rows {
            print!("\x1b[{row};1H\x1b[2K");
        }
        print!("\x1b[{bottom};1H");
        render::flush();
    }

    /// Clear the rows the box was last painted on, wherever they are now.
    fn erase(&mut self, rows: u16) {
        if let Some((top, held)) = self.painted.take() {
            for row in top..=(top + held).min(rows) {
                print!("\x1b[{row};1H\x1b[2K");
            }
        }
    }

    fn draw(&mut self) {
        if !self.live {
            return;
        }
        let (_, rows) = terminal::size().unwrap_or((80, 24));
        let room = self.room();
        let edge = "─".repeat(room + 2);

        let mut lines = vec![format!(
            "{}{}",
            render::MARGIN,
            render::dim(&format!("╭{edge}╮"))
        )];
        match &self.choice {
            Some(choice) => self.asking(choice, room, &mut lines),
            None => self.typing(room, &mut lines),
        }
        lines.push(format!(
            "{}{}",
            render::MARGIN,
            render::dim(&format!("╰{edge}╯"))
        ));

        if self.choice.is_none() {
            for (i, option) in matches(self.line.text())
                .iter()
                .take(MENU_LIMIT)
                .enumerate()
            {
                let marker = if i == self.picked { "›" } else { " " };
                let name = if i == self.picked {
                    render::blue(option.name)
                } else {
                    option.name.to_owned()
                };
                lines.push(format!(
                    "{}{marker} {name}  {}",
                    render::GUTTER,
                    render::dim(option.blurb)
                ));
            }
        }
        let note = match &self.choice {
            Some(_) => "↑↓ to choose  ·  enter to take it  ·  esc to decline".to_owned(),
            None => self.footer.clone(),
        };
        lines.push(format!("{}{}", render::PAD, render::dim(&note)));
        if let Some(waiting) = &self.waiting {
            lines.push(waiting.line());
        }

        let top = rows.saturating_sub(self.height) + 1;
        let mut out = std::io::stdout().lock();
        // Save and restore, so whatever was streaming into the conversation
        // carries on from where it was. The region is repeated because some
        // terminals forget it over a window restore, and a box that rights
        // itself on the next keystroke beats one that stays wrong.
        let _ = write!(out, "\x1b7\x1b[1;{}r", top.saturating_sub(1).max(1));
        for (i, text) in lines.iter().enumerate() {
            let _ = write!(out, "\x1b[{};1H\x1b[2K{text}", top + i as u16);
        }
        let _ = write!(out, "\x1b8");
        let _ = out.flush();
        self.painted = Some((top, self.height.saturating_sub(1)));
    }

    /// One row of the box, padded so its right edge lines up.
    fn boxed(&self, text: &str, width: usize, room: usize) -> String {
        format!(
            "{}{} {text}{} {}",
            render::MARGIN,
            render::dim("│"),
            " ".repeat(room.saturating_sub(width)),
            render::dim("│")
        )
    }

    /// The box while it holds a line being typed.
    fn typing(&self, room: usize, lines: &mut Vec<String>) {
        let rows = self.rows();
        let (cursor_row, cursor_column) = self.line.line_of_cursor();
        let on = cursor_row.min(rows.len() - 1);
        for (i, text) in rows.iter().enumerate() {
            let lead = if i == 0 {
                format!("{} ", render::blue("›"))
            } else {
                "  ".to_owned()
            };
            // The caret is drawn rather than parked: the real cursor has to
            // stay where output lands.
            let (body, width) = if i == on {
                let at = cursor_column.min(text.chars().count());
                let before: String = text.chars().take(at).collect();
                let under: String = text.chars().skip(at).take(1).collect();
                let after: String = text.chars().skip(at + 1).collect();
                let blank = under.is_empty();
                let under = if blank { " ".to_owned() } else { under };
                (
                    format!("{before}{}{after}", render::caret_over(&under)),
                    text.chars().count() + usize::from(blank),
                )
            } else {
                (text.clone(), text.chars().count())
            };
            lines.push(self.boxed(&format!("{lead}{body}"), 2 + width, room));
        }
    }

    /// The box while it holds a question. The answers are listed inside it
    /// and steered with the arrow keys, so agreeing to something is one key
    /// and never the wrong letter.
    fn asking(&self, choice: &Choice, room: usize, lines: &mut Vec<String>) {
        for part in render::fold(&choice.question, room) {
            let width = render::visible(&part);
            lines.push(self.boxed(&render::bold(&part), width, room));
        }
        for detail in &choice.detail {
            for part in render::fold(detail, room) {
                let width = render::visible(&part);
                lines.push(self.boxed(&render::dim(&part), width, room));
            }
        }
        lines.push(self.boxed("", 0, room));
        for (i, option) in choice.options.iter().enumerate() {
            let chosen = i == choice.picked;
            let text = format!("{} {} {option}", if chosen { "›" } else { " " }, i + 1);
            let width = render::visible(&text);
            lines.push(self.boxed(
                &if chosen {
                    render::blue(&text)
                } else {
                    render::dim(&text)
                },
                width,
                room,
            ));
        }
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.uninstall();
    }
}

/// Put back the console's quick-edit flag, which is what dragging the mouse
/// to select text uses on Windows.
///
/// Raw mode is a read-modify-write of the console's input mode, and Windows
/// clears quick edit on any write that does not ask for it back by name.
#[cfg(windows)]
fn allow_selection() {
    use windows_sys::Win32::System::Console::{
        GetConsoleMode, GetStdHandle, SetConsoleMode, ENABLE_EXTENDED_FLAGS,
        ENABLE_QUICK_EDIT_MODE, STD_INPUT_HANDLE,
    };
    unsafe {
        let handle = GetStdHandle(STD_INPUT_HANDLE);
        let mut mode = 0u32;
        if GetConsoleMode(handle, &mut mode) != 0 {
            SetConsoleMode(handle, mode | ENABLE_EXTENDED_FLAGS | ENABLE_QUICK_EDIT_MODE);
        }
    }
}

#[cfg(not(windows))]
fn allow_selection() {}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn the_menu_only_opens_for_a_slash() {
        assert!(matches("what is this").is_empty());
        assert!(matches("").is_empty());
        assert!(!matches("/").is_empty());
    }

    #[test]
    fn typing_narrows_the_list() {
        let all = matches("/").len();
        let e = matches("/e");
        assert!(e.len() < all && !e.is_empty());
        assert!(e.iter().all(|c| c.name.starts_with("/e")));
        assert_eq!(matches("/exit").len(), 1);
        // A command that does not exist offers nothing rather than guessing.
        assert!(matches("/nonsense").is_empty());
        // Once it carries an argument it is no longer a name being typed.
        assert!(matches("/model gpt").is_empty());
    }
}
