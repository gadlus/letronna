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
//! question can be typed while the current one is still being answered. A
//! line entered while tools are at work goes to the model with their
//! results; one entered during the last answer waits its turn instead.

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::terminal;
use letronna_core::store::Todo;
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
        blurb: "show or change the model; add image, speech or video for those",
    },
    Command {
        name: "/gateways",
        blurb: "see, add or remove gateways",
    },
    Command {
        name: "/team",
        blurb: "bill hosted models to a team or to yourself",
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
        blurb: "let the model search the web; /search services for your own",
    },
    Command {
        name: "/jobs",
        blurb: "commands still running in the background",
    },
    Command {
        name: "/skills",
        blurb: "skills: use one, switch them on or off, add from GitHub",
    },
    Command {
        name: "/resume",
        blurb: "pick up a session from before",
    },
    Command {
        name: "/undo",
        blurb: "take back the last exchange",
    },
    Command {
        name: "/retry",
        blurb: "ask your last message again",
    },
    Command {
        name: "/branch",
        blurb: "carry on in a copy of this session",
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

/// Steps of the plan listed over the box at once. A plan can run to a dozen,
/// and every row it takes is one the conversation loses; the heading still
/// counts them all.
const PLAN_ROWS: usize = 5;

/// Rows of conversation the plan never takes: half of a 24-row terminal, the
/// shortest in common use, which there still leaves the whole plan its room.
const PLAN_LEAVES: usize = 12;

fn matches(text: &str) -> Vec<&'static Command> {
    if !text.starts_with('/') || text.contains(char::is_whitespace) {
        return Vec::new();
    }
    COMMANDS
        .iter()
        .filter(|c| c.name.starts_with(text))
        .collect()
}

/// Columns the longest command name takes, so every blurb starts in the
/// same place however much of the list is showing.
pub fn name_width() -> usize {
    COMMANDS
        .iter()
        .map(|c| c.name.chars().count())
        .max()
        .unwrap_or(0)
}

/// Where the menu's shown rows start. It only moves as far as it has to for
/// the highlighted row to be one of them: the list held more commands than
/// it drew, and the arrows walked all of them, so a highlight could sit on a
/// row nobody could see and enter would take it.
fn menu_top(top: usize, picked: usize, len: usize, limit: usize) -> usize {
    if len <= limit {
        return 0;
    }
    let top = top.min(len - limit);
    if picked < top {
        picked
    } else if picked >= top + limit {
        picked + 1 - limit
    } else {
        top
    }
}

/// The command a mistyped one was most likely meant to be, when one is
/// within two keystrokes of it.
pub fn closest(typed: &str) -> Option<&'static str> {
    let typed = typed.to_lowercase();
    COMMANDS
        .iter()
        .map(|c| (distance(&typed, c.name), c.name))
        .filter(|(edits, _)| *edits <= 2)
        .min_by_key(|(edits, _)| *edits)
        .map(|(_, name)| name)
}

/// Letters to add, drop or change to turn `a` into `b`.
fn distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, x) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, y) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = if x == *y {
                diagonal
            } else {
                1 + diagonal.min(above).min(row[j])
            };
            diagonal = above;
        }
    }
    row[b.len()]
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

/// A setting with a natural order, picked by sliding along a track rather
/// than from a list: effort goes from less to more, and a list hides that
/// the options are points on one line.
struct Slider {
    question: String,
    stops: Vec<String>,
    at: usize,
}

pub struct Bar {
    line: Line,
    picked: usize,
    /// The first of the menu's rows on show, when there are more than fit.
    menu_top: usize,
    footer: String,
    /// Lines entered but not yet answered, in the order they were typed.
    queued: VecDeque<String>,
    live: bool,
    done: bool,
    choice: Option<Choice>,
    slider: Option<Slider>,
    /// A line that must not be shown: typed as dots, and kept out of the
    /// conversation when it is sent. For keys.
    secret: bool,
    /// The question `ask` has open, shown in the box above what is typed.
    asking: Option<String>,
    /// What the slider settled on: a stop, or nothing when it was left.
    slid: Option<Option<usize>>,
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
    /// The open session's checklist, shown over the box while any of it is
    /// left to do.
    plan: Vec<Todo>,
}

impl Bar {
    pub fn new(footer: String) -> Self {
        Self {
            line: Line::default(),
            picked: 0,
            menu_top: 0,
            footer,
            queued: VecDeque::new(),
            live: false,
            done: false,
            choice: None,
            slider: None,
            secret: false,
            asking: None,
            slid: None,
            answer: None,
            working: false,
            interrupt: false,
            note: None,
            handled: 0,
            enter_at: None,
            enters: 0,
            waiting: None,
            ticked: std::time::Instant::now(),
            plan: Vec::new(),
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

    /// Show this checklist over the box, in place of the one before.
    pub fn set_plan(&mut self, todos: &[Todo]) {
        self.plan = todos.to_vec();
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
            for line in &detail {
                render::line(&render::dim(line));
            }
            return crate::input::menu(options, 0);
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

    /// Ask for a line of text, with `hint` as the question at the top of the box.
    /// A secret one is drawn as dots and never echoed into the conversation,
    /// so a key does not end up in the scrollback for anyone to read.
    pub fn ask(&mut self, hint: &str, secret: bool) -> Option<String> {
        if !self.live {
            let prompt = format!("{}{} ", render::PAD, render::blue("›"));
            let _ = hint;
            return if secret {
                crate::input::secret(&prompt)
            } else {
                crate::input::read(&prompt)
            };
        }
        self.secret = secret;
        self.asking = Some(hint.to_owned());
        self.note = Some("enter to save  ·  esc to skip".to_owned());
        self.settle();
        let answer = self.wait();
        self.secret = false;
        self.asking = None;
        self.note = None;
        self.settle();
        answer
    }

    /// Pick one of `stops` by sliding along a track, starting at `start`.
    /// `None` when it was left where it was.
    pub fn slide(&mut self, question: &str, stops: &[&str], start: usize) -> Option<usize> {
        if !self.live {
            render::line(&render::bold(question));
            return crate::input::menu(stops, start);
        }

        self.slider = Some(Slider {
            question: question.to_owned(),
            stops: stops.iter().map(|s| (*s).to_owned()).collect(),
            at: start.min(stops.len().saturating_sub(1)),
        });
        self.slid = None;
        self.settle();
        loop {
            if self.done {
                self.slider = None;
                self.settle();
                return None;
            }
            if let Some(settled) = self.slid.take() {
                self.slider = None;
                self.settle();
                return settled;
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
        if self.handled > 0 && noted && self.asking.is_none() {
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

    /// Send words as though they had just been typed and entered, ahead of
    /// anything already waiting, for a command that asks something again.
    pub fn send_first(&mut self, text: &str) {
        self.echo(text);
        self.queued.push_front(text.to_owned());
        self.settle();
    }

    /// Take the lines waiting for the model, up to the first command: that
    /// command, and everything typed after it, stays for the loop to run in
    /// the order it was typed.
    pub fn take_typed(&mut self) -> Vec<String> {
        let plain = self
            .queued
            .iter()
            .take_while(|line| !line.starts_with('/'))
            .count();
        self.queued.drain(..plain).collect()
    }

    /// Do `work` without the screen going quiet: the waiting line in the
    /// box while it is up, a wheel printed in place when it is not.
    pub fn busy<T: Send + 'static>(
        &mut self,
        label: &str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        if !self.live {
            return crate::input::busy(label, work);
        }
        let handle = std::thread::spawn(work);
        self.wait_line_about(label);
        while !handle.is_finished() {
            self.pump();
        }
        self.wait_line(false);
        handle.join().expect("the work panicked")
    }

    /// Like `busy`, for a wait that can be given up on: escape flips `stop`,
    /// which `work` watches, the same key that stops a reply.
    pub fn busy_cancellable<T: Send + 'static>(
        &mut self,
        label: &str,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        if !self.live {
            return crate::input::busy(label, work);
        }
        let handle = std::thread::spawn(work);
        self.wait_line_about(label);
        self.working(true);
        while !handle.is_finished() {
            self.pump();
            if self.interrupted() {
                stop.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
        self.working(false);
        self.wait_line(false);
        handle.join().expect("the work panicked")
    }

    /// Show or take away the waiting line, naming the step of the plan being
    /// done when there is one.
    pub fn wait_line(&mut self, on: bool) {
        let step = self.plan.iter().find(|t| t.status == "doing");
        self.waiting = on.then(|| match step {
            Some(step) => crate::spinner::Waiting::during(&step.text),
            None => crate::spinner::Waiting::new(),
        });
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
        // The footer says whether escape will stop anything, so it is
        // redrawn now rather than at the next key; that also brings out the
        // last of what the turn printed, "Stopped." included.
        self.settle();
    }

    /// How long a wait between pumps may block on its own channel: a pump
    /// waits a tick for keys while the box is up and returns at once when
    /// it is not, where a wait that never blocked would spin.
    pub fn patience(&self) -> Duration {
        if self.live { Duration::ZERO } else { TICK }
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
        if self.slider.is_some() {
            self.nudge(key);
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
            // Escape on an empty answer skips the question.
            KeyCode::Esc if self.asking.is_some() && self.line.is_empty() => {
                self.queued.push_back(String::new())
            }
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
        let shown = matches(self.line.text()).len();
        self.menu_top = menu_top(self.menu_top, self.picked, shown, MENU_LIMIT);
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
        if !self.secret {
            self.echo(&line);
        }
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

    /// Move along the track. It stops at each end rather than wrapping:
    /// sliding off the top and landing on the bottom is not what a slider is.
    fn nudge(&mut self, key: KeyEvent) {
        let Some(slider) = self.slider.as_mut() else {
            return;
        };
        let last = slider.stops.len().saturating_sub(1);
        match key.code {
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Down => {
                slider.at = slider.at.saturating_sub(1)
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Up => {
                slider.at = (slider.at + 1).min(last)
            }
            KeyCode::Home => slider.at = 0,
            KeyCode::End => slider.at = last,
            KeyCode::Enter => self.slid = Some(Some(slider.at)),
            KeyCode::Esc => self.slid = Some(None),
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => self.done = true,
            KeyCode::Char(digit) if digit.is_ascii_digit() => {
                if let Some(index) = digit.to_digit(10).and_then(|d| (d as usize).checked_sub(1))
                    && index <= last
                {
                    slider.at = index;
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
        let text: String = if self.secret {
            self.line.text().chars().map(|_| '•').collect()
        } else {
            self.line.text().to_owned()
        };
        for part in text.split('\n') {
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
        let (_, height) = terminal::size().unwrap_or((80, 24));
        crate::screen::chrome(self.lines(height as usize));
    }

    /// The waiting line, the plan, the box, the menu when one is open, and
    /// the footer, for a window `height` rows tall.
    fn lines(&self, height: usize) -> Vec<String> {
        let room = self.room();
        let edge = "─".repeat(room + 2);
        let mut lines = Vec::new();
        // The waiting line sits above the box, where the answer it is waiting
        // for will appear.
        if let Some(waiting) = &self.waiting {
            lines.push(waiting.line(render::width()));
        }
        lines.push(format!(
            "{}{}",
            render::MARGIN,
            render::dim(&format!("╭{edge}╮"))
        ));
        match (&self.slider, &self.choice) {
            (Some(slider), _) => self.sliding(slider, room, &mut lines),
            (None, Some(choice)) => self.asking(choice, room, &mut lines),
            (None, None) => self.typing(room, &mut lines),
        }
        lines.push(format!(
            "{}{}",
            render::MARGIN,
            render::dim(&format!("╰{edge}╯"))
        ));

        if self.choice.is_none() {
            let options = matches(self.line.text());
            let top = menu_top(self.menu_top, self.picked, options.len(), MENU_LIMIT);
            let end = (top + MENU_LIMIT).min(options.len());
            let width = name_width();
            let more = |count: usize, arrow: &str| {
                format!(
                    "{}  {}",
                    render::GUTTER,
                    render::dim(&format!("{arrow} {count} more"))
                )
            };
            if top > 0 {
                lines.push(more(top, "↑"));
            }
            for (i, option) in options.iter().enumerate().take(end).skip(top) {
                let marker = if i == self.picked { "›" } else { " " };
                let padded = format!("{:<width$}", option.name);
                let name = if i == self.picked {
                    render::blue(&padded)
                } else {
                    padded
                };
                lines.push(format!(
                    "{}{marker} {name}  {}",
                    render::GUTTER,
                    render::dim(option.blurb)
                ));
            }
            if end < options.len() {
                lines.push(more(options.len() - end, "↓"));
            }
        }

        let back = crate::screen::scrolled();
        let note = if let Some(said) = &self.note {
            said.clone()
        } else if self.slider.is_some() {
            "← → to slide  ·  enter to keep it  ·  esc to leave it as it was".to_owned()
        } else if self.choice.is_some() {
            "↑↓ to choose  ·  enter to take it  ·  esc to decline".to_owned()
        } else if back > 0 {
            format!("scrolled back {back} lines  ·  end to follow again")
        } else if self.working {
            // Said while it applies rather than only after a long silence:
            // a reply going the wrong way is worth stopping early.
            format!("{}  ·  esc to stop", self.footer)
        } else {
            self.footer.clone()
        };
        lines.push(format!("{}{}", render::PAD, render::dim(&note)));

        // The plan gets only the rows the rest leaves over: what is being
        // typed or asked comes first, and a screen of chrome would push its
        // own bottom rows off the window.
        let mut plan = checklist(&self.plan, room);
        let spare = height.saturating_sub(lines.len() + PLAN_LEAVES);
        if plan.len() > spare {
            // The heading alone still says how far along the plan is.
            plan.truncate(spare.min(1));
        }
        let under = usize::from(self.waiting.is_some());
        lines.splice(under..under, plan);
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
        if let Some(question) = &self.asking {
            for part in render::fold(question, room) {
                let width = render::visible(&part);
                lines.push(self.boxed(&render::bold(&part), width, room));
            }
        }
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

    /// The box while it holds a slider: the question, the names of the stops
    /// spread along the track, and the track with a knob on it. The stretch
    /// up to the knob is drawn filled, so which way is "more" is plain
    /// without reading a label.
    fn sliding(&self, slider: &Slider, room: usize, lines: &mut Vec<String>) {
        let question = render::bold(&slider.question);
        lines.push(self.boxed(&question, render::visible(&question), room));
        lines.push(self.boxed("", 0, room));

        let (labels, track, width) = track(&slider.stops, slider.at, room);
        lines.push(self.boxed(&labels, width, room));
        lines.push(self.boxed(&track, width, room));
        lines.push(self.boxed("", 0, room));
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

/// The plan over the box while any of it is left to do: a heading with the
/// count, then the steps from the first one not done, each cut to `room`
/// columns so that no row runs past the box's right edge.
fn checklist(todos: &[Todo], room: usize) -> Vec<String> {
    if todos.iter().all(|t| t.status == "done") {
        return Vec::new();
    }
    let done = todos.iter().filter(|t| t.status == "done").count();
    let heading = format!("Plan · {done} of {} done", todos.len());
    // A row without a mark has the mark's two columns as well.
    let mut lines = vec![format!(
        "{}{}",
        render::PAD,
        render::dim(&render::fit(&heading, room + 2))
    )];
    // Finished steps are counted in the heading, so the list starts where the
    // work is, filled up from above when that would leave rows spare.
    let first = todos.iter().position(|t| t.status != "done").unwrap_or(0);
    let start = first.min(todos.len().saturating_sub(PLAN_ROWS));
    for todo in todos.iter().skip(start).take(PLAN_ROWS) {
        let text = render::fit(&todo.text, room);
        let row = match todo.status.as_str() {
            "done" => format!("{} {}", render::green("✓"), render::dim(&text)),
            "doing" => format!("{} {text}", render::blue("◐")),
            _ => format!("{} {text}", render::dim("○")),
        };
        lines.push(format!("{}{row}", render::PAD));
    }
    let rest = todos.len().saturating_sub(start + PLAN_ROWS);
    if rest > 0 {
        let more = render::fit(&format!("… {rest} more"), room + 2);
        lines.push(format!("{}{}", render::PAD, render::dim(&more)));
    }
    lines
}

/// Where each stop sits on a track `width` columns wide: spread evenly, the
/// first at the left end and the last at the right.
fn stops_at(count: usize, width: usize) -> Vec<usize> {
    match count {
        0 => Vec::new(),
        1 => vec![width / 2],
        n => (0..n).map(|i| i * (width - 1) / (n - 1)).collect(),
    }
}

/// The two rows of a slider -- the names over the track, and the track --
/// and how many columns they take. Names are centred over their stop but
/// kept inside the ends, so the first and last do not hang off the track.
fn track(stops: &[String], at: usize, room: usize) -> (String, String, usize) {
    let pad = 2;
    let width = room.saturating_sub(pad * 2).max(stops.len() * 2 + 1);
    let places = stops_at(stops.len(), width);

    let mut labels = String::new();
    let mut used = 0usize;
    for (i, (stop, place)) in stops.iter().zip(&places).enumerate() {
        let len = stop.chars().count();
        let start = place
            .saturating_sub(len / 2)
            .min(width.saturating_sub(len))
            .max(used);
        labels.push_str(&" ".repeat(start - used));
        labels.push_str(&if i == at {
            render::bold(&render::blue(stop))
        } else {
            render::dim(stop)
        });
        used = start + len;
    }

    let knob = places.get(at).copied().unwrap_or(0);
    let track = format!(
        "{}{}{}",
        render::blue(&"━".repeat(knob)),
        render::blue("●"),
        render::dim(&"─".repeat(width.saturating_sub(knob + 1)))
    );

    let indent = " ".repeat(pad);
    (
        format!("{indent}{labels}"),
        format!("{indent}{track}"),
        pad + width.max(used),
    )
}

#[cfg(test)]
mod tests {
    use super::{
        Bar, Choice, MENU_LIMIT, PLAN_ROWS, Slider, checklist, closest, matches, menu_top,
        stops_at, track,
    };
    use crate::render::{MARGIN, plain, visible};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use letronna_core::store::Todo;

    fn step(text: &str, status: &str) -> Todo {
        Todo {
            text: text.into(),
            status: status.into(),
        }
    }

    /// The plan as it reads, whether or not this run has colour.
    fn shown(todos: &[Todo], room: usize) -> Vec<String> {
        checklist(todos, room)
            .iter()
            .map(String::as_str)
            .map(plain)
            .collect()
    }

    fn press(bar: &mut Bar, code: KeyCode) {
        bar.nudge(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn ctrl_c_during_a_turn_stops_it_and_leaves() {
        let mut bar = Bar::new(String::new());
        bar.working(true);
        bar.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL));
        assert!(bar.interrupted(), "the turn was not stopped");
        assert!(bar.finished(), "the terminal stayed open");
        // Escape stops the turn too, but stays.
        let mut bar = Bar::new(String::new());
        bar.working(true);
        bar.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(bar.interrupted() && !bar.finished());
    }

    #[test]
    fn the_slider_stops_at_its_ends_and_enter_keeps_where_it_is() {
        let mut bar = Bar::new(String::new());
        bar.slider = Some(Slider {
            question: "How hard?".into(),
            stops: vec!["auto".into(), "low".into(), "medium".into(), "high".into()],
            at: 0,
        });
        // Left at the first stop stays there rather than wrapping to the last.
        press(&mut bar, KeyCode::Left);
        assert_eq!(bar.slider.as_ref().unwrap().at, 0);
        // Right walks along, and stops at the far end.
        for _ in 0..6 {
            press(&mut bar, KeyCode::Right);
        }
        assert_eq!(bar.slider.as_ref().unwrap().at, 3);
        press(&mut bar, KeyCode::Left);
        press(&mut bar, KeyCode::Enter);
        assert_eq!(bar.slid, Some(Some(2)));
    }

    #[test]
    fn escape_leaves_it_as_it_was() {
        let mut bar = Bar::new(String::new());
        bar.slider = Some(Slider {
            question: "How hard?".into(),
            stops: vec!["auto".into(), "low".into()],
            at: 1,
        });
        press(&mut bar, KeyCode::Left);
        press(&mut bar, KeyCode::Esc);
        // Moved, then left: nothing is taken.
        assert_eq!(bar.slid, Some(None));
    }

    #[test]
    fn a_digit_jumps_straight_to_its_stop() {
        let mut bar = Bar::new(String::new());
        bar.slider = Some(Slider {
            question: "How hard?".into(),
            stops: vec!["auto".into(), "low".into(), "medium".into(), "high".into()],
            at: 0,
        });
        press(&mut bar, KeyCode::Char('4'));
        assert_eq!(bar.slider.as_ref().unwrap().at, 3);
        // A digit past the last stop is ignored rather than clamped.
        press(&mut bar, KeyCode::Char('9'));
        assert_eq!(bar.slider.as_ref().unwrap().at, 3);
    }

    #[test]
    fn the_stops_reach_both_ends_of_the_track() {
        assert_eq!(stops_at(4, 31), vec![0, 10, 20, 30]);
        assert_eq!(stops_at(2, 10), vec![0, 9]);
        assert_eq!(stops_at(1, 10), vec![5]);
        assert!(stops_at(0, 10).is_empty());
    }

    #[test]
    fn the_knob_sits_on_its_stop() {
        let stops: Vec<String> = ["auto", "low", "medium", "high"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        // No colour in a test, so this is the bare track.
        let (labels, track, width) = track(&stops, 2, 40);
        let knob = track.chars().position(|c| c == '●').unwrap();
        let places = stops_at(4, 36);
        assert_eq!(knob, 2 + places[2]);
        assert!(labels.contains("auto") && labels.contains("high"));
        // Every row fits inside the box.
        assert!(width <= 40);
        assert!(track.chars().count() <= 40);
    }

    #[test]
    fn the_menu_only_opens_for_a_slash() {
        assert!(matches("what is this").is_empty());
        assert!(matches("").is_empty());
        assert!(!matches("/").is_empty());
    }

    #[test]
    fn the_menu_keeps_the_highlight_in_view() {
        let len = 16;
        let shows = |top: usize, picked: usize| top <= picked && picked < top + MENU_LIMIT;
        // Up from the first row wraps to the last, which has to come into view.
        let top = menu_top(0, len - 1, len, MENU_LIMIT);
        assert!(shows(top, len - 1));
        assert_eq!(top + MENU_LIMIT, len);
        // Walking back up does not shift the rows while the highlight is on show.
        assert_eq!(menu_top(top, len - 3, len, MENU_LIMIT), top);
        // Every step down the whole list keeps it on show.
        let mut top = 0;
        for picked in 0..len {
            top = menu_top(top, picked, len, MENU_LIMIT);
            assert!(shows(top, picked), "row {picked} hidden under top {top}");
        }
        // A list that fits never scrolls.
        assert_eq!(menu_top(5, 3, 4, MENU_LIMIT), 0);
    }

    #[test]
    fn a_typo_points_at_the_command_it_meant() {
        assert_eq!(closest("/mdoel"), Some("/model"));
        assert_eq!(closest("/hlep"), Some("/help"));
        assert_eq!(closest("/Models"), Some("/model"));
        assert_eq!(closest("/xyzzy"), None);
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

    #[test]
    fn the_plan_counts_what_is_done_and_marks_each_state() {
        let plan = [
            step("Read the code", "done"),
            step("Write the parser", "doing"),
            step("Test it", "pending"),
        ];
        assert_eq!(
            shown(&plan, 40),
            [
                "    Plan · 1 of 3 done",
                "    ✓ Read the code",
                "    ◐ Write the parser",
                "    ○ Test it",
            ]
        );
    }

    #[test]
    fn a_finished_or_empty_plan_takes_no_room() {
        assert!(checklist(&[], 40).is_empty());
        assert!(checklist(&[step("Read the code", "done")], 40).is_empty());
    }

    #[test]
    fn the_plan_keeps_inside_the_box_at_narrow_widths() {
        let mut plan = vec![step("Check the samples", "pending"); 8];
        plan[0] = step("Read every module the importer touches", "done");
        plan[1] = step("Write the parser for each format", "doing");
        plan[2] = step("Test it\nagainst the samples", "pending");
        // The narrowest room is the one the box never goes below.
        for room in [8, 12, 20] {
            let edge = visible(&format!("{MARGIN}╭{}╮", "─".repeat(room + 2)));
            let lines = shown(&plan, room);
            for line in &lines {
                assert!(visible(line) <= edge, "{line:?} in {room}");
            }
            assert!(lines[1].starts_with("    ◐ Write"), "{:?}", lines[1]);
        }
        let narrow = shown(&plan, 8);
        assert_eq!(narrow[1], "    ◐ Write t…");
        assert_eq!(narrow[2], "    ○ Test it…");
        assert_eq!(narrow.last().unwrap(), "    … 2 more");
    }

    #[test]
    fn a_long_plan_lists_from_where_the_work_is_and_counts_the_rest() {
        let plan = |done: usize| -> Vec<Todo> {
            (1..=12)
                .map(|i| match i {
                    i if i <= done => step(&format!("step {i}"), "done"),
                    i if i == done + 1 => step(&format!("step {i}"), "doing"),
                    i => step(&format!("step {i}"), "pending"),
                })
                .collect()
        };
        let early = shown(&plan(4), 40);
        assert_eq!(early.len(), 1 + PLAN_ROWS + 1);
        assert_eq!(early[0], "    Plan · 4 of 12 done");
        assert_eq!(early[1], "    ◐ step 5");
        assert_eq!(early.last().unwrap(), "    … 3 more");
        // Near the end the rows are filled from above instead of left empty.
        let late = shown(&plan(10), 40);
        assert_eq!(late.len(), 1 + PLAN_ROWS);
        assert_eq!(late[1], "    ✓ step 8");
        assert_eq!(late.last().unwrap(), "    ○ step 12");
    }

    #[test]
    fn the_waiting_line_names_the_step_being_done() {
        let mut bar = Bar::new(String::new());
        bar.set_plan(&[step("Read the code", "done")]);
        bar.wait_line(true);
        assert!(!bar.waiting.as_ref().unwrap().line(80).contains(" · "));
        bar.set_plan(&[
            step("Read the code", "done"),
            step("Write the parser", "doing"),
        ]);
        bar.wait_line(true);
        let line = bar.waiting.as_ref().unwrap().line(80);
        assert!(line.contains(" · Write the parser"), "{line}");
    }

    #[test]
    fn a_question_with_the_plan_open_still_fits_a_24_row_window() {
        let rows = |bar: &Bar, height: usize| -> Vec<String> {
            bar.lines(height)
                .iter()
                .map(String::as_str)
                .map(plain)
                .collect()
        };
        let mut plan = vec![step("Check the samples", "pending"); 12];
        plan[0] = step("Write the parser", "doing");
        let mut bar = Bar::new(String::new());
        bar.set_plan(&plan);
        bar.wait_line(true);
        // Under the waiting line, with only the box below it, the whole plan fits.
        let typing = rows(&bar, 24);
        assert!(typing[1].starts_with("    Plan ·"), "{typing:#?}");
        assert!(typing.iter().any(|l| l == "    … 7 more"), "{typing:#?}");
        // A shorter window keeps the heading, which still says how far along it is.
        let short = rows(&bar, 18);
        assert!(short[1].starts_with("    Plan ·"), "{short:#?}");
        assert!(short[2].contains('╭'), "{short:#?}");
        // A run_command approval of a long script needs every row itself.
        bar.choice = Some(Choice {
            question: "Run it?".into(),
            detail: vec!["echo 1".into(); 12],
            options: ["Yes", "Always", "No", "Tell it what to do instead"]
                .map(String::from)
                .into(),
            picked: 0,
        });
        let asking = rows(&bar, 24);
        assert!(asking.len() <= 24, "{asking:#?}");
        assert!(!asking.iter().any(|l| l.contains("Plan ·")), "{asking:#?}");
        assert!(asking.iter().any(|l| l.contains("4 Tell it what to do")));
        assert!(asking.last().unwrap().contains("enter to take it"));
    }

    #[test]
    fn what_was_typed_goes_in_up_to_the_first_command() {
        let mut bar = Bar::new(String::new());
        bar.queued = ["check the tests too", "and the docs", "/where", "then this"]
            .map(String::from)
            .into();
        assert_eq!(bar.take_typed(), ["check the tests too", "and the docs"]);
        // The command waits for the loop, and so does what came after it.
        assert_eq!(bar.queued, ["/where", "then this"]);
        assert!(bar.take_typed().is_empty());
    }
}
