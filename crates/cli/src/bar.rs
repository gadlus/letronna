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

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::terminal;
use std::collections::VecDeque;
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
        name: "/login",
        blurb: "sign in to Letronna",
    },
    Command {
        name: "/logout",
        blurb: "sign out on this machine",
    },
    Command {
        name: "/whoami",
        blurb: "who is signed in",
    },
    Command {
        name: "/effort",
        blurb: "how hard the model should think",
    },
    Command {
        name: "/thinking",
        blurb: "show the model's thinking, or hide it",
    },
    Command {
        name: "/search",
        blurb: "let the model search the web",
    },
    Command {
        name: "/jobs",
        blurb: "commands still running in the background",
    },
    Command {
        name: "/skills",
        blurb: "saved instructions you can hand the model",
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

/// Rows a turn of the wheel moves, and rows a page key moves.
const WHEEL: isize = 3;
const PAGE: isize = 10;

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
    live: bool,
    done: bool,
    choice: Option<Choice>,
    answer: Option<usize>,
    /// True while a turn is in flight, which is what makes escape mean stop.
    working: bool,
    interrupt: bool,
    /// Keys and pastes handled in the drain now running, an enter's place
    /// among them, and how many enters there were. Together these say whether
    /// an enter ended what you were typing or sat in the middle of something
    /// arriving, without guessing from the clock.
    handled: usize,
    enter_at: Option<usize>,
    enters: usize,
    /// Something to say in the footer instead of the model, until the next
    /// thing you do. Long enough to be read, gone before it is in the way.
    note: Option<String>,
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
            live: false,
            done: false,
            choice: None,
            answer: None,
            working: false,
            interrupt: false,
            note: None,
            handled: 0,
            enter_at: None,
            enters: 0,
            waiting: None,
            ticked: std::time::Instant::now(),
        }
    }

    /// Take the window.
    pub fn install(&mut self) {
        if !render::interactive() || terminal::enable_raw_mode().is_err() {
            return;
        }
        render::set_raw(true);
        self.live = true;
        crate::screen::open();
        self.settle();
    }

    /// Give the window back exactly as it was found.
    pub fn uninstall(&mut self) {
        if !self.live {
            return;
        }
        crate::screen::close();
        let _ = terminal::disable_raw_mode();
        render::set_raw(false);
        self.live = false;
    }

    /// Paint again, for whoever has just changed what is under the box.
    pub fn redraw(&mut self) {
        self.settle();
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
        let mut resized = false;
        let noted = self.note.is_some();
        self.handled = 0;
        self.enter_at = None;
        self.enters = 0;
        while event::poll(TICK).unwrap_or(false) {
            match event::read() {
                // A key release is not something anybody did; counting it
                // would make every key look like the middle of a burst,
                // since a release always follows its press.
                Ok(Event::Key(key)) if key.kind != KeyEventKind::Press => continue,
                Ok(Event::Key(key)) => {
                    self.handled += 1;
                    self.key(key);
                }
                // Terminals that hand pasted text over in one piece. Windows
                // consoles do not, which the enter key handles instead.
                Ok(Event::Paste(text)) => {
                    self.handled += 1;
                    if self.choice.is_none() {
                        self.line.paste(&text);
                        self.picked = 0;
                    }
                }
                // Dragging a window edge sends a burst of these, and
                // restoring one sends several at once. Note it and deal with
                // the last shape rather than every shape on the way there.
                Ok(Event::Resize(_, _)) => resized = true,
                Ok(Event::Mouse(mouse)) => {
                    self.mouse(mouse);
                    // The footer says how far back the view is and whether
                    // something was just copied, so it has to be rebuilt when
                    // the mouse is what changed those.
                    self.settle();
                }
                _ => continue,
            }
        }
        if resized {
            self.settle();
        }
        // Nobody types eight characters inside one poll. If that many arrived
        // and they put several lines in the box, it was a paste, and it is
        // stood behind a placeholder now that it is all here.
        // An enter that was the last thing to arrive, and the only one, ended
        // a line someone was typing. An enter with more behind it, or one of
        // several, is a line ending inside text that was pasted.
        if self.enters == 1 && self.enter_at == Some(self.handled) {
            self.send();
        } else if self.handled > 8 && self.choice.is_none() {
            self.line.fold_burst(mark);
        }
        // One repaint for the whole drain. Painting after every keystroke
        // meant a paste of a few thousand characters asked for a few thousand
        // repaints, and while we were busy with those the console's input
        // queue filled and threw the rest of the paste away -- which is how
        // words went missing out of the middle of pasted text.
        // Anything the user does takes the footer back from a note.
        if self.handled > 0 && noted {
            self.note = None;
        }
        if self.handled > 0 || resized {
            self.settle();
        }
        // The waiting line turns on this loop, which runs every twenty
        // milliseconds whether or not anything was typed.
        if self.waiting.is_some() && self.ticked.elapsed() >= Duration::from_millis(90) {
            self.ticked = std::time::Instant::now();
            self.settle();
        }
    }

    /// Put words in the box as though they had been typed, for a command
    /// that hands the model something to work from.
    pub fn say(&mut self, text: &str) {
        self.line.clear();
        self.line.insert_str(text);
        self.settle();
    }

    /// Show or take away the waiting line.
    pub fn wait_line(&mut self, on: bool) {
        self.waiting = on.then(crate::spinner::Waiting::new);
        self.settle();
    }

    /// Show a waiting line that says what is being waited for.
    pub fn wait_line_about(&mut self, what: &str) {
        self.waiting = Some(crate::spinner::Waiting::about(what));
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
            // With something marked, this is the copy key, which is what
            // anybody pressing it over a selection meant by it. Leaving is
            // what it does when there is nothing to copy.
            KeyCode::Char('c') if control && crate::screen::selecting() => {
                if let Some(text) = crate::screen::selected() {
                    let lines = text.lines().count();
                    crate::screen::copy(&text);
                    crate::screen::unselect();
                    self.note = Some(format!(
                        "copied {lines} line{}",
                        if lines == 1 { "" } else { "s" }
                    ));
                }
            }
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
            // Every enter puts a line ending in. Whether it also ends the
            // message is decided once the drain is done, by where it fell
            // among everything else that arrived.
            KeyCode::Enter => {
                self.line.insert('\n');
                self.enters += 1;
                self.enter_at = Some(self.handled);
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
            KeyCode::Down if !options.is_empty() => self.picked = (self.picked + 1) % options.len(),
            KeyCode::Up => self.line.up(),
            KeyCode::Down => self.line.down(),
            KeyCode::Left if control => self.line.word_left(),
            KeyCode::Right if control => self.line.word_right(),
            KeyCode::Left => self.line.left(),
            KeyCode::Right => self.line.right(),
            KeyCode::PageUp => crate::screen::scroll(PAGE),
            KeyCode::PageDown => crate::screen::scroll(-PAGE),
            KeyCode::Home => self.line.home(),
            // End follows the conversation again when you have scrolled back,
            // and otherwise goes to the end of the line.
            KeyCode::End if crate::screen::scrolled() > 0 => crate::screen::follow(),
            KeyCode::End => self.line.end(),
            // Escape stops an answer when one is running, and is otherwise
            // the quickest way to drop what you were typing.
            KeyCode::Esc if crate::screen::selecting() => crate::screen::unselect(),
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
            _ => {}
        }
    }

    /// Send what is in the box: the line ending the enter put in comes off,
    /// a highlighted command wins over what was typed, and what is left goes
    /// into the conversation and the queue.
    fn send(&mut self) {
        let text = self.line.text().trim_end_matches('\n').to_owned();
        self.line.set(&text);
        if let Some(choice) = matches(self.line.text()).get(self.picked) {
            self.line.set(choice.name);
        }
        self.picked = 0;
        let line = self.line.take().trim().to_owned();
        if line.is_empty() {
            self.settle();
            return;
        }
        self.echo(&line);
        self.queued.push_back(line);
        self.settle();
    }

    /// The mouse: the wheel moves the view, and dragging marks text and
    /// copies it when let go. The terminal cannot do either of these while
    /// the conversation is held here, so they are done here.
    fn mouse(&mut self, mouse: MouseEvent) {
        match mouse.kind {
            MouseEventKind::ScrollUp => crate::screen::scroll(WHEEL),
            MouseEventKind::ScrollDown => crate::screen::scroll(-WHEEL),
            MouseEventKind::Down(MouseButton::Left) => {
                crate::screen::select_from(mouse.column, mouse.row)
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                crate::screen::select_to(mouse.column, mouse.row)
            }
            MouseEventKind::Up(MouseButton::Left) => {
                // Where the button came up is the end of the selection. Not
                // every terminal reports the movement in between -- a console
                // may send the press and the release and nothing else -- so
                // taking the end from the release is what makes a drag work
                // at all there.
                crate::screen::select_to(mouse.column, mouse.row);
                if let Some(text) = crate::screen::select_done() {
                    let lines = text.lines().count();
                    crate::screen::copy(&text);
                    self.note = Some(format!(
                        "copied {lines} line{}  ·  ctrl-c copies again",
                        if lines == 1 { "" } else { "s" }
                    ));
                    self.settle();
                }
            }
            _ => {}
        }
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
                if let Some(index) = digit.to_digit(10).and_then(|d| (d as usize).checked_sub(1))
                    && index < choice.options.len()
                {
                    self.answer = Some(index);
                }
            }
            _ => {}
        }
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

    /// Build the box and hand it to the screen, which paints it under the
    /// conversation. Nothing here touches the terminal, so there is no frame
    /// held in it that a resize could strand.
    fn settle(&mut self) {
        if !self.live {
            return;
        }
        crate::screen::chrome(self.lines());
    }

    /// The waiting line, the box, the menu when one is open, and the footer.
    fn lines(&self) -> Vec<String> {
        let room = self.room();
        let edge = "─".repeat(room + 2);
        let mut lines = Vec::new();
        // The waiting line sits above the box, where the answer it is waiting
        // for will appear.
        if let Some(waiting) = &self.waiting {
            lines.push(waiting.line());
        }
        lines.push(format!(
            "{}{}",
            render::MARGIN,
            render::dim(&format!("╭{edge}╮"))
        ));
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

        let back = crate::screen::scrolled();
        let note = if let Some(said) = &self.note {
            said.clone()
        } else if self.choice.is_some() {
            "↑↓ to choose  ·  enter to take it  ·  esc to decline".to_owned()
        } else if back > 0 {
            format!("scrolled back {back} lines  ·  end to follow again")
        } else {
            self.footer.clone()
        };
        lines.push(format!("{}{}", render::PAD, render::dim(&note)));
        lines
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
