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
//! Until it goes in it waits above the box rather than in the conversation,
//! where up takes it back and ctrl-s sends it at once.

use crossterm::event::{
    self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::terminal;
use letronna_core::approvals::{self, Request};
use letronna_core::store::Todo;
use std::collections::VecDeque;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use crate::edit::Line;
use crate::render;

/// A line of the `/` menu: Letronna's own commands, or one the user, the
/// project or a skill added.
#[derive(Clone, Copy)]
pub struct Command<'a> {
    pub name: &'a str,
    pub blurb: &'a str,
}

pub const COMMANDS: &[Command<'static>] = &[
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
        name: "/mode",
        blurb: "show or change how much runs without asking",
    },
    Command {
        name: "/model",
        blurb: "show or change the model; add image, speech or video for those",
    },
    Command {
        name: "/fallback",
        blurb: "models to carry on with when yours is overloaded or out of credit",
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
        name: "/style",
        blurb: "list reply styles, or pick one: /style concise",
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
        name: "/notify",
        blurb: "say when a reply ends or needs you while you are away, or stop",
    },
    Command {
        name: "/voice",
        blurb: "read replies aloud as they stream, paid from your credit",
    },
    Command {
        name: "/verify",
        blurb: "ask the model to build or test after it changes files, or not",
    },
    Command {
        name: "/jobs",
        blurb: "commands still running in the background",
    },
    Command {
        name: "/loop",
        blurb: "run a prompt every so often, or at the model's pace; /loop stop ends it",
    },
    Command {
        name: "/heartbeat",
        blurb: "look in on this session every so many minutes",
    },
    Command {
        name: "/agents",
        blurb: "helpers working for this session; stop one or tell it something",
    },
    Command {
        name: "/workflows",
        blurb: "workflow runs: look in, pause, stop, resume, skip or retry",
    },
    Command {
        name: "/crew",
        blurb: "let Letronna run workflows of helpers for big tasks",
    },
    Command {
        name: "/skills",
        blurb: "skills: use one, switch them on or off, add from GitHub",
    },
    Command {
        name: "/mcp",
        blurb: "MCP servers: how each is doing; reconnect, enable, disable, login, logout, log, reload",
    },
    Command {
        name: "/plugins",
        blurb: "plugins: add from git, switch on or off, allow servers, update, remove, market",
    },
    Command {
        name: "/resume",
        blurb: "pick up a session; a word finds it by title or what was said",
    },
    Command {
        name: "/diff",
        blurb: "what changed in the project; /diff session for this session's files",
    },
    Command {
        name: "/import",
        blurb: "continue a session from Claude Code or Codex",
    },
    Command {
        name: "/undo",
        blurb: "take back the last exchange; --files puts files back too",
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
        name: "/init",
        blurb: "have the model write or fill in the project's AGENTS.md",
    },
    Command {
        name: "/btw",
        blurb: "ask something aside; the answer is not kept",
    },
    Command {
        name: "/goal",
        blurb: "keep working until a goal is met: /goal <text>, --check <command>, resume, clear",
    },
    Command {
        name: "/image",
        blurb: "attach a picture to your next message: /image <path>",
    },
    Command {
        name: "/preview",
        blurb: "serve an HTML or SVG file to your browser: /preview [--open] <path>",
    },
    Command {
        name: "/where",
        blurb: "show the project folder and session file",
    },
    Command {
        name: "/usage",
        blurb: "tokens and cost: this session, by day and by model",
    },
    Command {
        name: "/insights",
        blurb: "the last weeks by day and hour, top tools and models: /insights 8",
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

/// The most columns a name takes in the menu and `/help`: over twice the
/// longest of Letronna's own, and an added name longer than that is cut
/// rather than pushing every description off the row.
const NAME_WIDTH: usize = 24;

/// Rows the box will grow to before what is inside it starts to scroll.
/// Enough for a paragraph, not so many that the conversation is squeezed out.
const MAX_ROWS: usize = 8;

/// Queued lines shown above the box. A few is what anyone queues; past that
/// the oldest are counted rather than drawn, so they cannot squeeze the
/// conversation off the screen.
const QUEUED_LIMIT: usize = 4;

/// How often the helpers are looked in on: four times a second is live to
/// the eye, and what their lines say changes by the second at most.
const LOOK: Duration = Duration::from_millis(250);

/// Helpers given a line of their own while a turn runs. The rest share one
/// line, so a fan-out of helpers cannot squeeze the conversation out of a
/// short window.
const RELAY_LIMIT: usize = 4;

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

/// How long a question that came up on its own has to have been up, and the
/// typing under it to have stopped, before an enter or a digit answers it:
/// longer than the gap between keys in a word, shorter than reading the
/// question takes.
const STILL: Duration = Duration::from_millis(500);

/// The menu's lines for what is typed: the command named exactly so first,
/// then Letronna's own, then the `added` ones, as (name, blurb).
fn matches<'a>(text: &str, added: &'a [(String, String)]) -> Vec<Command<'a>> {
    if !text.starts_with('/') || text.contains(char::is_whitespace) {
        return Vec::new();
    }
    let added = added.iter().map(|(name, blurb)| Command { name, blurb });
    // MCP servers' prompts come after the built-in commands.
    let prompts = letronna_core::mcp::prompts()
        .into_iter()
        .map(|(name, blurb)| Command { name, blurb });
    let mut found: Vec<Command> = COMMANDS
        .iter()
        .copied()
        .chain(prompts)
        .chain(added)
        .filter(|c| c.name.starts_with(text))
        .collect();
    // Enter takes the first, so an added `/u` is not lost to `/undo`.
    found.sort_by_key(|c| c.name != text);
    found
}

/// The names an `@` path could go on as, as many as fit in `width` columns
/// of the footer, which is cut rather than wrapped.
fn offered(names: &[String], width: usize) -> String {
    let room = width.saturating_sub(render::PAD.len() + 2);
    let mut out = String::new();
    for name in names {
        let wanted = out.chars().count() + name.chars().count() + 2;
        if !out.is_empty() && wanted > room {
            out.push_str("  …");
            break;
        }
        if !out.is_empty() {
            out.push_str("  ");
        }
        out.push_str(name);
    }
    out
}

/// Columns the longest command name takes, the `added` ones' too, up to
/// `NAME_WIDTH`, so every blurb starts in the same place.
pub fn name_width(added: &[(String, String)]) -> usize {
    let added = added.iter().map(|(name, _)| name.as_str());
    COMMANDS
        .iter()
        .map(|c| c.name)
        .chain(added)
        .map(|name| name.chars().count())
        .max()
        .unwrap_or(0)
        .min(NAME_WIDTH)
}

/// Whether `line` runs as a command rather than going to the model: a line
/// starting with a path, "/usr/bin/foo is broken", is said as it is.
pub fn is_command(line: &str) -> bool {
    letronna_core::slash::split(line).is_some()
}

/// What a line that is not a command sends: a leading "//" stands for one
/// slash, so "//tmp is full" says "/tmp is full".
pub fn unescaped(line: &str) -> &str {
    if line.starts_with("//") {
        &line[1..]
    } else {
        line
    }
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
pub fn closest<'a>(typed: &str, added: &'a [(String, String)]) -> Option<&'a str> {
    let typed = typed.to_lowercase();
    let added = added.iter().map(|(name, _)| name.as_str());
    COMMANDS
        .iter()
        .map(|c| c.name)
        .chain(added)
        .map(|name| (distance(&typed, &name.to_lowercase()), name))
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

/// What ended a wait at the prompt.
#[derive(Debug, PartialEq, Eq)]
pub enum Waited {
    /// A line was entered.
    Line(String),
    /// The doorbell rang for notices that came while the session sat idle.
    Rang,
    /// A helper asked for a yes, which `asked` hands over.
    Asked,
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
    /// Set by the send-now key: what is queued should go in without waiting
    /// for the reply to finish.
    hurry: bool,
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
    /// The open session's goal on one line, shown over the box while it has one.
    goal: Option<String>,
    /// The session whose helpers are shown, or 0 before one is open.
    helpers_of: u64,
    /// A line for each of its helpers at work, as last looked at.
    relay: Vec<String>,
    /// When the helpers were last looked at.
    looked: std::time::Instant,
    /// The doorbell for notices, once the conversation listens for them.
    bell: Option<async_channel::Receiver<()>>,
    /// Helpers' requests for a yes, once the terminal answers them.
    asks: Option<Receiver<Request>>,
    /// A request taken but not yet answered: one that ended a wait at the
    /// prompt, or one put back to be asked later. `asked` hands it over first.
    held: Option<Request>,
    /// While a question that came up on its own is open, when it came up or
    /// when a letter was last typed under it, whichever is later.
    guard: Option<std::time::Instant>,
    /// Whether anyone is looking, for notifications.
    focus: crate::notify::Focus,
    /// The commands the user, the project and skills added, as the menu
    /// offers them: (`/name`, blurb).
    added: Vec<(String, String)>,
    /// The open session's project folder, where an `@` path is completed.
    folder: Option<std::path::PathBuf>,
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
            hurry: false,
            note: None,
            handled: 0,
            enter_at: None,
            enters: 0,
            waiting: None,
            ticked: std::time::Instant::now(),
            plan: Vec::new(),
            goal: None,
            helpers_of: 0,
            relay: Vec::new(),
            looked: std::time::Instant::now(),
            bell: None,
            asks: None,
            held: None,
            guard: None,
            focus: crate::notify::Focus::new(std::time::Instant::now()),
            added: Vec::new(),
            folder: None,
        }
    }

    /// Offer `added` in the `/` menu beside Letronna's own commands, as
    /// (`/name`, blurb).
    pub fn offer(&mut self, added: Vec<(String, String)>) {
        self.added = added;
    }

    /// The command, Letronna's own or an added one, that the mistyped
    /// `typed` was most likely meant to be.
    pub fn meant(&self, typed: &str) -> Option<&str> {
        closest(typed, &self.added)
    }

    /// What is in the box, typed and not yet sent.
    pub fn draft(&self) -> &str {
        self.line.text()
    }

    /// Complete `@` paths in `folder`, the open session's project folder.
    pub fn set_folder(&mut self, folder: Option<std::path::PathBuf>) {
        self.folder = folder;
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

    /// Show this goal line over the box, or none.
    pub fn set_goal(&mut self, line: Option<String>) {
        self.goal = line;
        self.settle();
    }

    /// Let the inbox's doorbell end a wait at the prompt, only with the box
    /// up: a line read from a pipe cannot be cut short, so there the inbox is
    /// not told that anything will wake an idle session.
    pub fn listen(&mut self) {
        if self.live {
            self.bell = Some(letronna_core::inbox::listen());
        }
    }

    /// Whether nobody is looking at the box, as of `now`; never while it is
    /// not up, when there is no terminal to notify through.
    pub fn away(&self, now: std::time::Instant) -> bool {
        self.live && self.focus.away(now)
    }

    /// Take helpers' requests for a yes from now on, to put to the user in the
    /// box. False, taking nothing, when the box is not up.
    pub fn serve(&mut self) -> bool {
        if !self.live {
            return false;
        }
        self.asks = Some(approvals::serve());
        true
    }

    /// The next helper's request waiting for an answer, taken without waiting.
    pub fn asked(&mut self) -> Option<Request> {
        self.held
            .take()
            .or_else(|| self.asks.as_ref()?.try_recv().ok())
    }

    /// Put back a request that is better asked later, for `asked` to hand
    /// over first.
    pub fn hold(&mut self, request: Request) {
        self.held = Some(request);
    }

    /// Wait at the prompt for a whole line, for the doorbell, or for a
    /// helper's request. `None` when the user is done.
    pub fn wait(&mut self) -> Option<Waited> {
        loop {
            if self.done {
                return None;
            }
            if let Some(waited) = self.waited() {
                return Some(waited);
            }
            if !self.live {
                return crate::input::read(&format!("{} ", render::blue("›"))).map(Waited::Line);
            }
            self.pump();
        }
    }

    /// A line already entered, a helper's request, or else a ring, taken
    /// without waiting for any: what the user typed goes first, and the
    /// notices it rang for go in ahead of it anyway.
    fn waited(&mut self) -> Option<Waited> {
        if let Some(line) = self.next_line() {
            return Some(Waited::Line(line));
        }
        // A receiver cannot be looked into without taking what it holds, so
        // the request is kept here until `asked` hands it over.
        if self.held.is_none() {
            self.held = self.asks.as_ref().and_then(|asks| asks.try_recv().ok());
        }
        if self.held.is_some() {
            return Some(Waited::Asked);
        }
        let bell = self.bell.as_ref()?;
        bell.try_recv().ok().map(|()| Waited::Rang)
    }

    /// Wait for a whole line that answers a question, leaving any ring for
    /// the prompt: a turn it started now would cut into the question. `None`
    /// when the user is done.
    pub fn reply(&mut self) -> Option<String> {
        if let Some(line) = self.next_line() {
            return Some(line);
        }
        if !self.live {
            return crate::input::read(&format!("{} ", render::blue("›")));
        }
        loop {
            if self.done {
                return None;
            }
            if let Some(line) = self.next_line() {
                return Some(line);
            }
            self.pump();
        }
    }

    /// The first line queued, put into the conversation now that it is taken,
    /// unless it is a key or a question skipped.
    fn next_line(&mut self) -> Option<String> {
        let line = self.queued.pop_front()?;
        if !self.secret && !line.is_empty() {
            self.echo(&line);
        }
        self.settle();
        Some(line)
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

    /// `choose`, for a question nobody at the keyboard asked for. It can come
    /// up under someone typing, so an enter or a digit counts only after
    /// `STILL`: the end of a line, or a digit in it, is not taken for a yes.
    pub fn interject(
        &mut self,
        question: &str,
        detail: Vec<String>,
        options: &[&str],
    ) -> Option<usize> {
        self.guard = Some(std::time::Instant::now());
        let picked = self.choose(question, detail, options);
        self.guard = None;
        picked
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
        let answer = self.reply();
        self.secret = false;
        self.asking = None;
        self.note = None;
        self.settle();
        answer
    }

    /// Ask for a line to go with an answer just given, with `hint` at the top
    /// of the box: enter sends it, escape sends nothing. Lines typed before it
    /// was asked were meant for the model, so they stay queued for it rather
    /// than answer this.
    pub fn reason(&mut self, hint: &str) -> Option<String> {
        if !self.live {
            render::line(&render::bold(hint));
            return crate::input::read(&format!("{}{} ", render::PAD, render::blue("›")));
        }
        let earlier = std::mem::take(&mut self.queued);
        self.asking = Some(hint.to_owned());
        self.note = Some("enter to send  ·  esc to skip".to_owned());
        self.settle();
        let answer = self.reply();
        self.asking = None;
        self.note = None;
        let later = std::mem::replace(&mut self.queued, earlier);
        self.queued.extend(later);
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
                    self.focus.touch(std::time::Instant::now());
                    self.key(key);
                }
                // Terminals that hand pasted text over in one piece. Windows
                // consoles do not, which the enter key handles instead.
                Ok(Event::Paste(text)) => {
                    self.handled += 1;
                    self.focus.touch(std::time::Instant::now());
                    if self.choice.is_none() {
                        self.line.paste(&text);
                        self.picked = 0;
                    }
                }
                // Dragging a window edge sends a burst of these, and
                // restoring one sends several at once. Note it and deal with
                // the last shape rather than every shape on the way there.
                Ok(Event::Resize(_, _)) => resized = true,
                Ok(Event::FocusGained) => self.focus.report(true),
                Ok(Event::FocusLost) => self.focus.report(false),
                Ok(Event::Mouse(mouse)) => {
                    // A pointer passing over the window is not someone
                    // looking at it; a click or a scroll is.
                    if mouse.kind != MouseEventKind::Moved {
                        self.focus.touch(std::time::Instant::now());
                    }
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
        if self.looked.elapsed() >= LOOK {
            self.looked = std::time::Instant::now();
            if self.look_at_helpers() {
                self.settle();
            }
        }
    }

    /// Follow the helpers of `session`, the one now open: a line for each at
    /// work while a turn runs, and how many there are in the footer.
    pub fn show_helpers_of(&mut self, session: u64) {
        if self.helpers_of == session {
            return;
        }
        self.helpers_of = session;
        // Looked at now, so the last session's helpers are not shown as this one's.
        self.look_at_helpers();
        self.settle();
    }

    /// Read how the open session's helpers are getting on, and say whether
    /// that changes the screen: their lines show while a turn runs, and
    /// their count always.
    fn look_at_helpers(&mut self) -> bool {
        let now = std::time::Instant::now();
        let relay: Vec<String> = (letronna_core::agents::seen().iter())
            .filter(|helper| helper.session == self.helpers_of && helper.ended.is_none())
            .map(|helper| relay_line(helper, now))
            .collect();
        let changed = relay.len() != self.relay.len() || (self.working && relay != self.relay);
        self.relay = relay;
        changed
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
        self.queued.push_front(text.to_owned());
        self.settle();
    }

    /// Take the lines waiting for the model, up to the first command: that
    /// command, and everything typed after it, stays for the loop to run in
    /// the order it was typed.
    pub fn take_typed(&mut self) -> Vec<String> {
        let plain = self.sendable();
        let typed: Vec<String> = self
            .queued
            .drain(..plain)
            .map(|line| unescaped(&line).to_owned())
            .collect();
        self.hurry = false;
        // They go in now, so this is where they join the conversation.
        if !typed.is_empty() {
            for line in &typed {
                self.echo(line);
            }
            self.settle();
        }
        typed
    }

    /// Whether anything typed is waiting to be sent or run.
    pub fn has_queued(&self) -> bool {
        !self.queued.is_empty()
    }

    /// How many queued lines the model's next step would take: those before
    /// the first command.
    fn sendable(&self) -> usize {
        self.queued
            .iter()
            .take_while(|line| !is_command(line))
            .count()
    }

    /// Whether the user asked for what is queued to go in now, cutting the
    /// reply short, with something queued that can.
    pub fn hurried(&self) -> bool {
        self.hurry && self.sendable() > 0
    }

    /// Put the lines queued for the model back in the box, ahead of anything
    /// typed since, for a turn that was stopped. Commands stay queued to run,
    /// since they were never going to the model.
    pub fn hand_back(&mut self) {
        let (commands, lines): (Vec<String>, Vec<String>) =
            self.queued.drain(..).partition(|line| is_command(line));
        self.queued = commands.into();
        if lines.is_empty() {
            return;
        }
        let mut text = lines.join("\n\n");
        let typed = self.line.text().trim();
        if !typed.is_empty() {
            text = format!("{text}\n\n{typed}");
        }
        self.line.set(&text);
        self.settle();
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

    /// Change what the waiting line names without starting its clock again.
    pub fn rename_wait(&mut self, what: &str) {
        self.waiting = self.waiting.take().map(|waiting| waiting.renamed(what));
        self.settle();
    }

    /// Say whether a turn is running, so escape knows what it means.
    pub fn working(&mut self, on: bool) {
        self.working = on;
        if !on {
            self.interrupt = false;
            self.hurry = false;
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
        let options = matches(self.line.text(), &self.added);
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
            // Not ctrl-enter, which most terminals send as a plain enter,
            // nor alt-enter, which is the newline above and fills the screen
            // on Windows. Ctrl-s reaches us as itself everywhere, since raw
            // mode turns off the flow control that once took it.
            KeyCode::Char('s') if control => self.send_now(),
            KeyCode::Tab => {
                if let Some(choice) = options.get(self.picked) {
                    self.line.set(choice.name);
                    self.picked = 0;
                } else if let Some(partial) = self.line.reference_before() {
                    let (completed, choices) =
                        letronna_core::references::complete(&partial, self.folder.as_deref());
                    self.line.replace_reference(&completed);
                    if !choices.is_empty() {
                        self.note = Some(offered(&choices, render::width()));
                    }
                }
            }
            KeyCode::Up if !options.is_empty() => {
                self.picked = self.picked.checked_sub(1).unwrap_or(options.len() - 1)
            }
            KeyCode::Down if !options.is_empty() => self.picked = (self.picked + 1) % options.len(),
            // What has not gone in yet can still be changed, so up brings the
            // newest queued line back before it reaches for what was sent.
            KeyCode::Up if self.line.is_empty() && !self.queued.is_empty() => {
                if let Some(line) = self.queued.pop_back() {
                    self.line.set(&line);
                }
            }
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
            // Escape on an empty answer skips the question, even one asked
            // in the middle of a turn, as the reason for a no is.
            KeyCode::Esc if self.asking.is_some() && self.line.is_empty() => {
                self.queued.push_back(String::new())
            }
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
        let shown = matches(self.line.text(), &self.added).len();
        self.menu_top = menu_top(self.menu_top, self.picked, shown, MENU_LIMIT);
    }

    /// Send what is in the box: the line ending the enter put in comes off,
    /// a highlighted command wins over what was typed, and what is left joins
    /// the queue, going into the conversation once it is taken from there.
    fn send(&mut self) {
        let text = self.line.text().trim_end_matches('\n').to_owned();
        self.line.set(&text);
        if let Some(choice) = matches(self.line.text(), &self.added).get(self.picked) {
            self.line.set(choice.name);
        }
        self.picked = 0;
        let line = self.line.take().trim().to_owned();
        if line.is_empty() {
            self.settle();
            return;
        }
        self.queued.push_back(line);
        self.settle();
    }

    /// The send-now key: what is in the box is entered as with enter, and
    /// while a turn runs, what is queued goes to the model without waiting
    /// for the reply to finish.
    fn send_now(&mut self) {
        if !self.line.is_empty() {
            self.send();
        }
        self.hurry = self.working && self.sendable() > 0;
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
        // Letters are typing, not answering, and each one puts an answer off
        // again; an answer that comes too soon is dropped rather than put off,
        // so pressing it again a moment later takes.
        if let Some(since) = self.guard.as_mut() {
            match key.code {
                KeyCode::Enter | KeyCode::Char('0'..='9') if since.elapsed() < STILL => return,
                KeyCode::Char(_) | KeyCode::Backspace => *since = std::time::Instant::now(),
                _ => {}
            }
        }
        let Some(choice) = self.choice.as_mut() else {
            return;
        };
        let last = choice.options.len().saturating_sub(1);
        match key.code {
            KeyCode::Up => choice.picked = choice.picked.checked_sub(1).unwrap_or(last),
            KeyCode::Down => choice.picked = (choice.picked + 1) % choice.options.len(),
            KeyCode::Enter => self.answer = Some(choice.picked),
            KeyCode::Esc => self.answer = Some(last),
            KeyCode::Char('c') if key.modifiers == KeyModifiers::CONTROL => {
                // A helper's question can come up while a call runs, and
                // leaving there stops the turn, as Ctrl-C does anywhere else
                // in one.
                self.interrupt = self.working;
                self.done = true;
            }
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
    pub fn echo(&self, line: &str) {
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
        // Above the waiting line, which stays next to the box it waits to fill.
        if self.working {
            self.relay_lines(&mut lines);
        }
        // The waiting line sits above the box, where the answer it is waiting
        // for will appear.
        if let Some(waiting) = &self.waiting {
            lines.push(waiting.line(render::width()));
        }
        // Lines entered while something runs wait here rather than in the
        // conversation, which only gets them once they go in. At the prompt
        // a line is taken as soon as it is entered, so it never shows.
        if (self.working || self.waiting.is_some()) && !self.queued.is_empty() {
            lines.push(format!(
                "{}{}",
                render::PAD,
                render::dim("queued · goes in after this step")
            ));
            let hidden = self.queued.len().saturating_sub(QUEUED_LIMIT);
            if hidden > 0 {
                lines.push(format!(
                    "{}{}",
                    render::PAD,
                    render::dim(&format!("… {hidden} more"))
                ));
            }
            for line in self.queued.iter().skip(hidden) {
                lines.push(format!(
                    "{}{} {}",
                    render::GUTTER,
                    render::dim("›"),
                    render::dim(&one_row(line, room))
                ));
            }
        }
        // Next to the box, since what is typed there ends the run it drives.
        if let Some(goal) = &self.goal {
            lines.push(format!(
                "{}{}",
                render::PAD,
                render::dim(&one_row(goal, room))
            ));
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
            let options = matches(self.line.text(), &self.added);
            let top = menu_top(self.menu_top, self.picked, options.len(), MENU_LIMIT);
            let end = (top + MENU_LIMIT).min(options.len());
            let width = name_width(&self.added);
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
                let padded = format!("{:<width$}", render::fit(option.name, width));
                let name = if i == self.picked {
                    render::blue(&padded)
                } else {
                    padded
                };
                // An added command's description is its writer's and can
                // run long; a row that wrapped would push the box up.
                let blurb_room = render::width().saturating_sub(render::GUTTER.len() + width + 4);
                lines.push(format!(
                    "{}{marker} {name}  {}",
                    render::GUTTER,
                    render::dim(&render::fit(option.blurb, blurb_room))
                ));
            }
            if end < options.len() {
                lines.push(more(options.len() - end, "↓"));
            }
        }

        let back = crate::screen::scrolled();
        let helpers = at_work(self.relay.len());
        // Cut rather than wrapped, now the count makes it longer: a wrapped
        // last row would run off the foot of the window and scroll everything
        // above it.
        let width = render::width().saturating_sub(render::PAD.len() + 1);
        let note = if let Some(said) = &self.note {
            said.clone()
        } else if self.slider.is_some() {
            "← → to slide  ·  enter to keep it  ·  esc to leave it as it was".to_owned()
        } else if self.choice.is_some() {
            "↑↓ to choose  ·  enter to take it  ·  esc to decline".to_owned()
        } else if back > 0 {
            format!("scrolled back {back} lines  ·  end to follow again")
        } else if self.working && self.sendable() > 0 {
            "ctrl-s to send now  ·  ↑ to take it back  ·  esc to stop".to_owned()
        } else if self.working {
            // Said while it applies rather than only after a long silence:
            // a reply going the wrong way is worth stopping early.
            fitted(&self.footer, &format!("{helpers}  ·  esc to stop"), width)
        } else {
            fitted(&self.footer, &helpers, width)
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

    /// The helpers' lines, each cut to the window, and no more of them than
    /// `RELAY_LIMIT` before the rest are counted instead.
    fn relay_lines(&self, lines: &mut Vec<String>) {
        let room = render::width().saturating_sub(render::INSIDE.len() + 3);
        for text in self.relay.iter().take(RELAY_LIMIT) {
            lines.push(format!(
                "{}{} {}",
                render::INSIDE,
                render::dim("◆"),
                render::dim(&render::clip(text, room))
            ));
        }
        if self.relay.len() > RELAY_LIMIT {
            let more = format!("… {} more", self.relay.len() - RELAY_LIMIT);
            lines.push(format!("{}  {}", render::INSIDE, render::dim(&more)));
        }
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

/// A queued line cut to one row `room` columns wide: its first line, with an
/// ellipsis where anything was left off.
fn one_row(text: &str, room: usize) -> String {
    let first = text.lines().next().unwrap_or_default();
    if first.chars().count() <= room && !text.trim_end().contains('\n') {
        return first.to_owned();
    }
    let mut row: String = first.chars().take(room.saturating_sub(1)).collect();
    row.push('…');
    row
}

/// What a helper's line says at `now`: which one it is, what it was given to
/// do, and how far it has got.
fn relay_line(helper: &letronna_core::agents::Seen, now: std::time::Instant) -> String {
    let mut parts = vec![helper.name.clone()];
    let task = letronna_core::tools::one_line(&helper.description);
    if !task.is_empty() {
        parts.push(task);
    }
    let tokens = (helper.status.input).load(std::sync::atomic::Ordering::Relaxed);
    parts.push(format!("{} tokens", letronna_core::budget::short(tokens)));
    let seconds = now.saturating_duration_since(helper.started).as_secs();
    parts.push(crate::spinner::spell(seconds));
    parts.join(" · ")
}

/// The footer's note of how many helpers are at work, if any are.
fn at_work(helpers: usize) -> String {
    match helpers {
        0 => String::new(),
        1 => "  ·  1 helper working".to_owned(),
        n => format!("  ·  {n} helpers working"),
    }
}

/// The footer with `then` after it, in `width` columns. The footer gives way
/// first, so the model's name loses its end before what the user can do now.
fn fitted(footer: &str, then: &str, width: usize) -> String {
    let room = width.saturating_sub(then.chars().count());
    render::clip(&format!("{}{then}", render::clip(footer, room)), width)
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
        Bar, Choice, MENU_LIMIT, NAME_WIDTH, PLAN_ROWS, QUEUED_LIMIT, RELAY_LIMIT, STILL, Slider,
        Waited, at_work, checklist, closest, fitted, is_command, matches, menu_top, name_width,
        offered, one_row, relay_line, stops_at, track, unescaped,
    };
    use crate::render::{MARGIN, plain, visible};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use letronna_core::store::Todo;
    use std::time::Instant;

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

    fn hit(bar: &mut Bar, code: KeyCode, modifiers: KeyModifiers) {
        bar.key(KeyEvent::new(code, modifiers));
    }

    /// The rows drawn above the box's top edge.
    fn above_the_box(bar: &Bar) -> Vec<String> {
        bar.lines(40)
            .into_iter()
            .take_while(|line| !line.contains('╭'))
            .collect()
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
    fn escape_in_a_question_asked_during_a_turn_skips_it_without_stopping_the_turn() {
        let mut bar = Bar::new(String::new());
        bar.working(true);
        bar.asking = Some("Why not?".into());
        bar.key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        assert!(!bar.interrupted(), "the turn was stopped");
        assert_eq!(bar.queued.pop_front().as_deref(), Some(""));
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
        assert!(matches("what is this", &[]).is_empty());
        assert!(matches("", &[]).is_empty());
        assert!(!matches("/", &[]).is_empty());
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
        assert_eq!(closest("/mdoel", &[]), Some("/model"));
        assert_eq!(closest("/hlep", &[]), Some("/help"));
        assert_eq!(closest("/Models", &[]), Some("/model"));
        assert_eq!(closest("/xyzzy", &[]), None);
        let added = [("/Review".to_owned(), String::new())];
        assert_eq!(closest("/reviw", &added), Some("/Review"));
    }

    #[test]
    fn typing_narrows_the_list() {
        let all = matches("/", &[]).len();
        let e = matches("/e", &[]);
        assert!(e.len() < all && !e.is_empty());
        assert!(e.iter().all(|c| c.name.starts_with("/e")));
        assert_eq!(matches("/exit", &[]).len(), 1);
        // A command that does not exist offers nothing rather than guessing.
        assert!(matches("/nonsense", &[]).is_empty());
        // Once it carries an argument it is no longer a name being typed.
        assert!(matches("/model gpt", &[]).is_empty());
    }

    #[test]
    fn added_commands_follow_letronna_s_own_in_the_menu() {
        let added = [("/review".to_owned(), "look over a change".to_owned())];
        let names: Vec<&str> = matches("/re", &added).iter().map(|c| c.name).collect();
        assert_eq!(names, ["/resume", "/retry", "/review"]);
        let long = [("/a-long-command-name".to_owned(), String::new())];
        assert_eq!(name_width(&long), "/a-long-command-name".len());
        // A name past the cap is cut, not every row pushed along by it.
        let longer = [(format!("/{}", "x".repeat(200)), String::new())];
        assert_eq!(name_width(&longer), NAME_WIDTH);
    }

    #[test]
    fn enter_on_an_added_name_that_starts_one_of_letronna_s_takes_the_added_one() {
        let mut bar = Bar::new(String::new());
        bar.offer(vec![("/u".to_owned(), "a kept prompt".to_owned())]);
        bar.line.set("/u");
        bar.send();
        assert_eq!(bar.queued.pop_front().as_deref(), Some("/u"));
        // Letronna's own exact names still win over longer ones.
        bar.line.set("/mode");
        bar.send();
        assert_eq!(bar.queued.pop_front().as_deref(), Some("/mode"));
        bar.line.set("/us");
        bar.send();
        assert_eq!(bar.queued.pop_front().as_deref(), Some("/usage"));
    }

    #[test]
    fn a_line_starting_with_two_slashes_says_one() {
        assert_eq!(unescaped("//tmp is full"), "/tmp is full");
        assert_eq!(unescaped("/tmp/x is full"), "/tmp/x is full");
        assert!(!is_command("//tmp is full"));
        let mut bar = Bar::new(String::new());
        bar.queued = ["//etc is odd".to_owned()].into();
        assert_eq!(bar.take_typed(), ["/etc is odd"]);
    }

    #[test]
    fn a_queued_line_starting_with_a_path_goes_to_the_model() {
        assert!(is_command("/review 42"));
        assert!(!is_command("/usr/bin/foo is broken"));
        let mut bar = Bar::new(String::new());
        bar.queued = ["/usr/bin/foo is broken".to_owned(), "/help".to_owned()].into();
        assert_eq!(bar.sendable(), 1);
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
    fn a_goal_shows_on_the_line_over_the_box_until_it_goes() {
        let mut bar = Bar::new(String::new());
        assert!(above_the_box(&bar).is_empty());
        bar.set_goal(Some(
            "Goal: ship it · 20/20 · paused: 20 continuations".into(),
        ));
        let above = above_the_box(&bar);
        assert_eq!(above.len(), 1, "{above:?}");
        assert!(above[0].contains("paused: 20 continuations"), "{above:?}");
        bar.set_goal(None);
        assert!(above_the_box(&bar).is_empty());
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

    #[test]
    fn a_ring_ends_the_wait_without_a_line() {
        let mut bar = Bar::new(String::new());
        let (ring, bell) = async_channel::bounded(1);
        bar.bell = Some(bell);
        assert_eq!(bar.waited(), None);
        ring.try_send(()).unwrap();
        assert_eq!(bar.waited(), Some(Waited::Rang));
        assert_eq!(bar.waited(), None, "one ring ended two waits");
        // A line typed before the ring is answered first, and the ring is
        // kept for the wait after it.
        ring.try_send(()).unwrap();
        bar.queued.push_back("and the docs".into());
        assert_eq!(bar.waited(), Some(Waited::Line("and the docs".into())));
        assert_eq!(bar.waited(), Some(Waited::Rang));
    }

    #[test]
    fn a_helpers_line_says_which_it_is_and_how_far_it_has_got() {
        let started = std::time::Instant::now();
        let mut helper = letronna_core::agents::Seen {
            id: 1 << 40,
            session: 1,
            name: "explore-2".into(),
            kind: "explore".into(),
            description: "Find where\nwindows start".into(),
            n: 1,
            ended: None,
            started,
            status: Default::default(),
        };
        let tokens = |helper: &letronna_core::agents::Seen, n| {
            (helper.status.input).store(n, std::sync::atomic::Ordering::Relaxed);
        };
        tokens(&helper, 38_400);
        let later = |seconds| started + std::time::Duration::from_secs(seconds);
        assert_eq!(
            relay_line(&helper, later(72)),
            "explore-2 · Find where windows start · 38k tokens · 1m 12s"
        );
        // Given no words for the user, it says only what it has got to.
        helper.description.clear();
        tokens(&helper, 900);
        assert_eq!(relay_line(&helper, later(5)), "explore-2 · 900 tokens · 5s");
    }

    #[test]
    fn helpers_at_work_are_counted_always_and_drawn_while_a_turn_runs() {
        let mut bar = Bar::new("model".into());
        bar.relay = (1..=6)
            .map(|n| format!("helper-{n} · 1k tokens · 5s"))
            .collect();
        // Only what is not cut to the window is looked at, so the test
        // passes in a terminal of any width.
        let idle = bar.lines(40);
        assert!(!idle.iter().any(|line| line.contains('◆')));

        bar.working = true;
        let busy = bar.lines(40);
        let drawn = busy.iter().filter(|line| line.contains('◆')).count();
        assert_eq!(drawn, RELAY_LIMIT);
        assert!(busy.iter().any(|line| line.ends_with("… 2 more")));
        assert_eq!(at_work(0), "");
        assert_eq!(at_work(1), "  ·  1 helper working");
        assert_eq!(at_work(6), "  ·  6 helpers working");
    }

    #[test]
    fn a_narrow_footer_loses_the_model_before_what_the_user_can_do() {
        let footer = "claude-sonnet-4-5  ·  letronna  ·  thinking high  ·  web";
        let then = format!("{}  ·  esc to stop", at_work(2));
        assert_eq!(fitted(footer, &then, 200), format!("{footer}{then}"));
        let cut = fitted(footer, &then, 60);
        assert_eq!(cut.chars().count(), 60);
        assert!(cut.starts_with("claude-sonnet") && cut.ends_with(&format!("…{then}")));
        // Narrower than the hint itself, the hint loses its end too.
        for width in [0, 1, 10, 30] {
            assert!(fitted(footer, &then, width).chars().count() <= width);
        }
    }

    #[test]
    fn lines_read_from_a_pipe_do_not_listen_for_the_bell() {
        // A bar never installed is what piped input and no colour get, and
        // nothing there would wake an idle session.
        let mut bar = Bar::new(String::new());
        bar.listen();
        assert!(bar.bell.is_none());
        assert!(!letronna_core::inbox::listened());
    }

    #[test]
    fn a_line_entered_while_it_works_waits_above_the_box_until_it_goes_in() {
        let mut bar = Bar::new(String::new());
        // At the prompt a line is taken the moment it is entered, so it is
        // never drawn as waiting.
        bar.line.set("hello");
        bar.send();
        assert!(above_the_box(&bar).is_empty());
        bar.queued.clear();

        bar.working(true);
        bar.line.set("check the docs too");
        bar.send();
        assert!(bar.line.is_empty());
        let above = above_the_box(&bar);
        assert!(above[0].contains("queued · goes in after this step"));
        assert!(above[1].contains("check the docs too"));
        assert_eq!(bar.take_typed(), ["check the docs too"]);
        assert!(
            above_the_box(&bar).is_empty(),
            "it went in and still showed"
        );
    }

    #[test]
    fn a_long_queue_shows_its_newest_lines_and_counts_the_rest() {
        let mut bar = Bar::new(String::new());
        bar.working(true);
        bar.queued = (0..QUEUED_LIMIT + 2).map(|i| format!("line {i}")).collect();
        let above = above_the_box(&bar);
        assert_eq!(above.len(), 2 + QUEUED_LIMIT);
        assert!(above[1].contains("… 2 more"));
        assert!(!above.iter().any(|row| row.contains("line 1")));
        assert!(
            above
                .last()
                .unwrap()
                .contains(&format!("line {}", QUEUED_LIMIT + 1))
        );
    }

    #[test]
    fn a_queued_line_is_cut_to_one_row() {
        assert_eq!(one_row("check the docs", 20), "check the docs");
        assert_eq!(one_row("check the docs", 10), "check the…");
        // A line under a first line is something left off, too.
        assert_eq!(
            one_row("check the docs\nand the tests", 40),
            "check the docs…"
        );
        assert_eq!(one_row("check the docs\n", 40), "check the docs");
    }

    #[test]
    fn up_on_an_empty_box_takes_the_newest_queued_line_back() {
        let mut bar = Bar::new(String::new());
        bar.working(true);
        bar.queued = ["check the docs", "and the tests"].map(String::from).into();
        hit(&mut bar, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(bar.line.text(), "and the tests");
        assert_eq!(bar.queued, ["check the docs"]);
        // With something in the box, up is the box's own again.
        hit(&mut bar, KeyCode::Up, KeyModifiers::NONE);
        assert_eq!(bar.line.text(), "and the tests");
        assert_eq!(bar.queued, ["check the docs"]);
    }

    #[test]
    fn a_stopped_turn_puts_what_was_queued_back_in_the_box() {
        let mut bar = Bar::new(String::new());
        bar.queued = ["check the docs", "/where", "and the tests"]
            .map(String::from)
            .into();
        bar.line.set("half typed ");
        bar.hand_back();
        // Ahead of what was typed since, as the window does.
        assert_eq!(
            bar.line.text(),
            "check the docs\n\nand the tests\n\nhalf typed"
        );
        // A command was never going to the model, so it still runs.
        assert_eq!(bar.queued, ["/where"]);
        bar.queued.clear();
        bar.hand_back();
        assert_eq!(
            bar.line.text(),
            "check the docs\n\nand the tests\n\nhalf typed"
        );
    }

    #[test]
    fn the_send_now_key_hurries_only_what_can_go_in() {
        let send_now = |bar: &mut Bar| hit(bar, KeyCode::Char('s'), KeyModifiers::CONTROL);
        let mut bar = Bar::new(String::new());
        // At the prompt it is enter, with nothing to cut short.
        bar.line.set("hello");
        send_now(&mut bar);
        assert_eq!(bar.queued, ["hello"]);
        assert!(!bar.hurried());
        bar.queued.clear();

        bar.working(true);
        // A command waits for the turn to end whatever is pressed.
        bar.queued.push_back("/where".into());
        send_now(&mut bar);
        assert!(!bar.hurried());
        bar.queued.clear();

        // What is in the box is entered first and goes with the rest.
        bar.queued.push_back("check the docs".into());
        bar.line.set("and the tests");
        send_now(&mut bar);
        assert_eq!(bar.queued, ["check the docs", "and the tests"]);
        assert!(bar.hurried());
        assert_eq!(bar.take_typed(), ["check the docs", "and the tests"]);
        assert!(
            !bar.hurried(),
            "the step that took them did not end the hurry"
        );

        // Taken back before the reply was cut, nothing is left to hurry.
        bar.queued.push_back("one more".into());
        send_now(&mut bar);
        hit(&mut bar, KeyCode::Up, KeyModifiers::NONE);
        assert!(!bar.hurried());
        // And a turn that ends forgets it.
        send_now(&mut bar);
        assert!(bar.hurried());
        bar.working(false);
        assert!(!bar.hurried());
    }

    #[test]
    fn the_footer_names_the_send_now_key_while_lines_wait() {
        let mut bar = Bar::new("fake".into());
        bar.working(true);
        assert!(!bar.lines(40).last().unwrap().contains("ctrl-s"));
        bar.queued.push_back("check the docs".into());
        assert!(bar.lines(40).last().unwrap().contains("ctrl-s to send now"));
    }

    #[test]
    fn an_enter_typed_as_a_helper_asks_is_not_taken_for_yes() {
        let mut bar = Bar::new(String::new());
        bar.choice = Some(Choice {
            question: "Helper general wants to run a command".into(),
            detail: Vec::new(),
            options: vec!["Yes".into(), "No".into()],
            picked: 0,
        });
        let key = |code| KeyEvent::new(code, KeyModifiers::NONE);
        // Just after the question came up, neither answers.
        bar.guard = Some(Instant::now());
        bar.pick(key(KeyCode::Enter));
        bar.pick(key(KeyCode::Char('1')));
        assert_eq!(bar.answer, None);
        // Nor just after a letter, however long it has been up.
        bar.guard = Instant::now().checked_sub(STILL);
        bar.pick(key(KeyCode::Char('k')));
        bar.pick(key(KeyCode::Enter));
        assert_eq!(bar.answer, None);
        // Once it has been up a moment with nothing typed, enter takes it.
        bar.guard = Instant::now().checked_sub(STILL);
        bar.pick(key(KeyCode::Enter));
        assert_eq!(bar.answer, Some(0));
    }

    #[test]
    fn the_paths_offered_after_an_at_are_cut_to_the_footer() {
        let names: Vec<String> = ["deep/", "lib.rs", "main.rs"].map(String::from).into();
        assert_eq!(offered(&names, 80), "deep/  lib.rs  main.rs");
        assert_eq!(offered(&names, 20), "deep/  lib.rs  …");
    }
}
