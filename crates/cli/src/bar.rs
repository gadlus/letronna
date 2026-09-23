//! The input box, pinned to the bottom of the window.
//!
//! The terminal's scrolling region is shrunk to everything above the box, so
//! answers scroll past while the box holds its place at the foot of the
//! window. Keys are read on the same loop that pumps the model's reply, which
//! means you can type the next question while the current one is still being
//! answered; a line entered mid-answer waits its turn rather than cutting in.
//!
//! Everything above the box is ordinary scrollback, so the conversation is
//! still there after you quit and selection keeps working.
//!
//! When the terminal cannot do any of this, because output is piped or
//! redirected, the box quietly becomes a plain prompt and a read of one line.

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEventKind,
};
use crossterm::terminal;
use std::collections::VecDeque;
use std::time::Duration;

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
        blurb: "show the model and gateway in use",
    },
    Command {
        name: "/where",
        blurb: "show the project folder and session file",
    },
    Command {
        name: "/mouse",
        blurb: "hand the mouse back so you can select text",
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

/// How long a pump waits for a keystroke before handing the loop back. Short
/// enough that typing feels immediate, long enough not to spin.
const TICK: Duration = Duration::from_millis(20);

/// Rows a turn of the wheel moves, and rows a page key moves.
const WHEEL: isize = 3;
const PAGE: isize = 10;

fn matches(text: &str) -> Vec<&'static Command> {
    if !text.starts_with('/') {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(text))
        .collect()
}

/// The tail of `text` that fits. A long line keeps its end -- the part being
/// typed -- in view, rather than its beginning.
fn fit(text: &str, room: usize) -> String {
    let count = text.chars().count();
    if count <= room {
        return text.to_owned();
    }
    text.chars().skip(count - room).collect()
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
    text: String,
    picked: usize,
    footer: String,
    /// Lines entered but not yet answered, in the order they were typed.
    queued: VecDeque<String>,
    /// False when the terminal cannot give us the bottom of the window, in
    /// which case this is a plain prompt.
    live: bool,
    done: bool,
    /// Set while a question is on the screen, and taken away by the answer.
    choice: Option<Choice>,
    answer: Option<usize>,
    /// True while a turn is in flight, which is what makes Escape mean stop
    /// rather than clear the line.
    working: bool,
    interrupt: bool,
}

impl Bar {
    pub fn new(footer: String) -> Self {
        Self {
            text: String::new(),
            picked: 0,
            footer,
            queued: VecDeque::new(),
            live: false,
            done: false,
            choice: None,
            answer: None,
            working: false,
            interrupt: false,
        }
    }

    /// Take over the window.
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
    pub fn choose(&mut self, question: &str, detail: Vec<String>, options: &[&str]) -> Option<usize> {
        if !self.live {
            // Nothing to steer with, so fall back to typing the number.
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
        while event::poll(TICK).unwrap_or(false) {
            match event::read() {
                Ok(Event::Key(key)) => self.key(key),
                Ok(Event::Mouse(mouse)) => match mouse.kind {
                    MouseEventKind::ScrollUp => crate::screen::scroll(WHEEL),
                    MouseEventKind::ScrollDown => crate::screen::scroll(-WHEEL),
                    _ => continue,
                },
                // A resize is just a repaint now; there is no frame held in
                // the terminal that could be left behind by one.
                Ok(Event::Resize(_, _)) => self.settle(),
                _ => continue,
            }
            self.settle();
        }
    }

    /// Say whether a turn is running, so Escape knows what it means.
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
        let options = matches(&self.text);
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                // While an answer is running, Ctrl-C stops it and leaves --
                // the turn does not have to finish first, which is the whole
                // reason anyone reaches for it. Otherwise it clears what you
                // were typing, and on an empty line it leaves. Nothing is
                // thrown away without warning.
                if self.working {
                    self.interrupt = true;
                    self.done = true;
                } else if self.text.is_empty() {
                    self.done = true;
                } else {
                    self.text.clear();
                    self.picked = 0;
                }
            }
            (KeyCode::Char('d'), KeyModifiers::CONTROL) if self.text.is_empty() => {
                self.done = true;
            }
            (KeyCode::Enter, _) => {
                // A highlighted command wins; otherwise take the text as typed.
                let mut line = std::mem::take(&mut self.text);
                if let Some(choice) = options.get(self.picked) {
                    line = choice.name.to_owned();
                }
                self.picked = 0;
                let line = line.trim().to_owned();
                if !line.is_empty() {
                    self.echo(&line);
                    self.queued.push_back(line);
                }
            }
            (KeyCode::Tab, _) => {
                if let Some(choice) = options.get(self.picked) {
                    self.text = choice.name.to_owned();
                    self.picked = 0;
                }
            }
            (KeyCode::Up, _) if !options.is_empty() => {
                self.picked = self.picked.checked_sub(1).unwrap_or(options.len() - 1);
            }
            (KeyCode::Down, _) if !options.is_empty() => {
                self.picked = (self.picked + 1) % options.len();
            }
            // While an answer is coming, Escape stops waiting for it. There
            // is nothing else it could usefully mean then, and a turn that
            // has gone quiet needs a way out that is not killing the window.
            (KeyCode::Esc, _) if self.working && self.text.is_empty() => {
                self.interrupt = true;
            }
            (KeyCode::PageUp, _) => crate::screen::scroll(PAGE),
            (KeyCode::PageDown, _) => crate::screen::scroll(-PAGE),
            // Scrolling back is for reading; End puts you back with the
            // conversation, which is where typing belongs.
            (KeyCode::End, _) => crate::screen::follow(),
            (KeyCode::Esc, _) if crate::screen::scrolled() > 0 => crate::screen::follow(),
            (KeyCode::Esc, _) => {
                self.text.clear();
                self.picked = 0;
            }
            (KeyCode::Backspace, _) => {
                self.text.pop();
                self.picked = 0;
            }
            (KeyCode::Char(ch), _) => {
                self.text.push(ch);
                self.picked = 0;
            }
            _ => return,
        }
        self.settle();
    }

    /// Steer a question. Enter takes what is highlighted, a digit takes that
    /// one outright, and Escape is the same as picking the last answer, which
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
        render::said(&render::blue("›"), &render::bold(line));
        // Room for the answer to start in, and for the spinner to sit in
        // while there is nothing to show yet.
        render::blank();
    }

    /// Rebuild the box and hand it to the screen, which paints it under the
    /// conversation. Nothing here touches the terminal directly, so there is
    /// no frame that a resize could strand.
    fn settle(&mut self) {
        if !self.live {
            return;
        }
        crate::screen::chrome(self.chrome());
    }

    /// The box, while it is holding a question rather than a line. The
    /// answers are listed inside it and steered with the arrow keys, so
    /// agreeing to something is one key and never the wrong letter.
    fn asking(&self, choice: &Choice, edge: &str, room: usize) -> Vec<String> {
        let mut lines = vec![format!(
            "{}{}",
            render::MARGIN,
            render::dim(&format!("╭{edge}╮"))
        )];
        let row = |text: String, width: usize| {
            format!(
                "{}{} {text}{} {}",
                render::MARGIN,
                render::dim("│"),
                " ".repeat(room.saturating_sub(width)),
                render::dim("│")
            )
        };
        // A question is broken over as many lines as it needs. Cutting it
        // short with an ellipsis hides the half that says what you would be
        // agreeing to.
        for part in crate::screen::fold_into(&choice.question, room) {
            let width = crate::screen::visible(&part);
            lines.push(row(render::bold(&part), width));
        }

        for line in &choice.detail {
            for part in crate::screen::fold_into(line, room) {
                let width = crate::screen::visible(&part);
                lines.push(row(render::dim(&part), width));
            }
        }

        lines.push(row(String::new(), 0));
        for (i, option) in choice.options.iter().enumerate() {
            let chosen = i == choice.picked;
            let head = format!("{} {} ", if chosen { "›" } else { " " }, i + 1);
            // An answer that does not fit is wrapped under itself rather than
            // cut, so a long one can still be read before it is taken.
            for (line, part) in crate::screen::fold_into(option, room.saturating_sub(head.len()))
                .into_iter()
                .enumerate()
            {
                let text = if line == 0 {
                    format!("{head}{part}")
                } else {
                    format!("{}{part}", " ".repeat(head.len()))
                };
                let width = crate::screen::visible(&text);
                lines.push(row(
                    if chosen {
                        render::blue(&text)
                    } else {
                        render::dim(&text)
                    },
                    width,
                ));
            }
        }
        lines.push(format!(
            "{}{}",
            render::MARGIN,
            render::dim(&format!("╰{edge}╯"))
        ));
        lines.push(format!(
            "{}{}",
            render::PAD,
            render::dim("↑↓ to choose  ·  enter to take it  ·  esc to decline")
        ));
        lines
    }

    /// The box, the menu under it when one is open, and the footer.
    fn chrome(&self) -> Vec<String> {
        let (columns, _) = terminal::size().unwrap_or((80, 24));
        // Full width, less the indent the conversation sits in and the box's
        // own borders and padding.
        let room = (columns as usize)
            .saturating_sub(render::MARGIN.len() * 2 + 4)
            .max(8);
        // Two columns of that room belong to the arrow.
        let typing = room.saturating_sub(2);
        let shown = fit(&self.text, typing);

        let edge = "─".repeat(room + 2);
        if let Some(choice) = &self.choice {
            return self.asking(choice, &edge, room);
        }
        let mut lines = vec![
            format!("{}{}", render::MARGIN, render::dim(&format!("╭{edge}╮"))),
            format!(
                "{}{} {} {}{}{} {}",
                render::MARGIN,
                render::dim("│"),
                render::blue("›"),
                shown,
                // The caret is drawn rather than parked: the real cursor is
                // hidden, because there is nowhere useful for it to sit.
                render::caret(),
                " ".repeat(typing.saturating_sub(shown.chars().count() + 1)),
                render::dim("│")
            ),
            format!("{}{}", render::MARGIN, render::dim(&format!("╰{edge}╯"))),
        ];

        let options = matches(&self.text);
        for (i, option) in options.iter().take(MENU_LIMIT).enumerate() {
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

        // While you are reading back through the conversation, the footer
        // says so and how to get back rather than repeating the model.
        let back = crate::screen::scrolled();
        let note = if back > 0 {
            format!("scrolled back {back} lines  ·  End to follow again")
        } else {
            self.footer.clone()
        };
        lines.push(format!("{}{}", render::PAD, render::dim(&note)));
        lines
    }
}

impl Drop for Bar {
    fn drop(&mut self) {
        self.uninstall();
    }
}

#[cfg(test)]
mod tests {
    use super::{fit, matches, Bar};

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
    }

    #[test]
    fn a_long_line_keeps_the_end_in_view() {
        assert_eq!(fit("hello", 10), "hello");
        assert_eq!(fit("hello", 5), "hello");
        // What was typed last is what you are looking at.
        assert_eq!(fit("abcdefghij", 4), "ghij");
        // Counted in characters, so the box does not split one in half.
        assert_eq!(fit("héllo wörld", 5), "wörld");
    }

    #[test]
    fn lines_typed_during_an_answer_wait_their_turn() {
        let mut bar = Bar::new(String::new());
        // Not live, so nothing is drawn; the queue is what matters here.
        bar.queued.push_back("first".into());
        bar.queued.push_back("second".into());
        assert_eq!(bar.wait().as_deref(), Some("first"));
        assert_eq!(bar.wait().as_deref(), Some("second"));
    }
}
