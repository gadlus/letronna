//! Claude Code, as a [`Plugin`]: hands work to the real, interactive
//! `claude` and brings its answer back, in a Claude Code session of its own
//! for each Letronna chat.
//!
//! Each chat's Claude Code runs as one of Claude Code's own background
//! sessions (`claude --bg`), which the user can open at any time with
//! `claude attach`. A message goes in as that session's next prompt --
//! `claude --bg --resume <id> -- <message>` -- just as if it were typed, so
//! Claude Code keeps its own login, tools and permission prompts, and
//! nothing reaches it through a side door it would rightly distrust. Not
//! `claude -p`, and not Claude's sign-in.
//!
//! The session follows the chat. A chat's first message, and the first after
//! it is cleared, starts a new session in the chat's folder and mode; a later
//! one, or one in a chat opened again, resumes the session that chat had,
//! found by the chat's first exchange, which is kept on disk. Claude Code
//! keeps a session's options, so a changed mode resumes it as Claude Code's
//! copy of it, history whole, which the chat then follows. After each answer
//! the session is stopped; its conversation is kept, and the next message or
//! `claude attach` wakes it.
//!
//! The answer comes from Claude Code's hooks: Letronna starts it with a
//! settings file whose hooks run this binary to report over a loopback
//! socket each turn's last message, a question waiting for the user, and the
//! session's start and end. A permission Claude Code would ask for in its
//! own screen is asked in the chat instead: the reply is the question, the
//! user's next message is the answer, and the same turn goes on, so nothing
//! needs a terminal on any system. Each side proves it knows the token in
//! `port.json`, which only the user can read, before either says a word.
//!
//! Claude Code is also a model in the picker. The plugin serves a small
//! OpenAI-style endpoint on loopback and points the Custom gateway at it,
//! unless the user has Custom set to something else. A turn there passes the
//! user's newest message to the chat's Claude Code and streams back its
//! answer. The endpoint wants the key the gateway was given and takes no call
//! a web page could make, since what it passes on is run as the user's.

use super::{AppEvent, Plugin};
use crate::prelude::*;
use letronna_core::gate::Mode;
use letronna_core::tools::{Tool, ToolContext};
use ring::rand::SecureRandom;
use ring::{digest, hmac};
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The flag Claude Code runs this binary with as a hook; the path of
/// `port.json` follows it.
const HOOK_FLAG: &str = "--claude-code-hook";
/// Where Letronna listens, and the token both sides prove they know.
const PORT_FILE: &str = "port.json";
/// The hooks Claude Code is started with.
const SETTINGS_FILE: &str = "settings.json";
/// Which Claude Code session each chat has, so a chat opened again after a
/// restart goes on in the same one.
const SESSIONS_FILE: &str = "sessions.json";
/// The most chats remembered; the oldest is forgotten first.
const SESSIONS_KEPT: usize = 200;
/// How long either side waits for the other's next line of the handshake.
const HANDSHAKE_LIMIT: Duration = Duration::from_secs(5);
/// How long Claude Code lets a hook take to report, in seconds.
const HOOK_SECS: u64 = 10;
/// How long a permission question waits for the user's answer in the chat,
/// in seconds; after it, Claude Code asks in its own session.
const QUESTION_SECS: u64 = 24 * 60 * 60;
/// How long a Claude Code that was told to start has to say it did.
const START_LIMIT: Duration = Duration::from_secs(60);
/// How long one call waits for an answer before handing back what it has.
const ANSWER_LIMIT: Duration = Duration::from_secs(20 * 60);
/// How often a waiting call looks again.
const STEP: Duration = Duration::from_millis(250);
/// The model's name in the picker: Claude Code with the model it picks;
/// the others it offers follow as `claude-code-<name>`.
const MODEL: &str = "claude-code";
/// The effort levels Claude Code takes, with the words the picker shows.
/// Ultracode is Claude Code's own: the hardest level, with its workflows.
const LEVELS: [(&str, &str); 6] = [
    ("low", "Low"),
    ("medium", "Medium"),
    ("high", "High"),
    ("xhigh", "Extra high"),
    ("max", "Max"),
    ("ultracode", "Ultracode"),
];
/// How much of a chat goes with its first message to a Claude Code session
/// that starts after the chat did: its newest part, well inside the
/// command line Windows allows.
const EARLIER_CHARS: usize = 20_000;
/// How long Claude Code has to say which models it offers.
const OFFERED_LIMIT: Duration = Duration::from_secs(15);
/// Where the model endpoint answers; also how its address is told from
/// another the user gave the Custom gateway.
const MODEL_PATH: &str = "/claude-code/v1";
/// Where the model endpoint's ports start: each data folder has one of the
/// thousand from here, so the Custom gateway's address stays the same
/// between starts, and two Letronnas on two data folders do not want the
/// same one. Another is taken when it is busy.
const MODEL_PORTS: u16 = 47821;
/// The key the Custom gateway sends, kept so the saved one stays right.
const KEY_FILE: &str = "key";
/// The gateway the model endpoint is listed under.
const CUSTOM: &str = "Custom";
/// How often a streamed turn says it is still there while Claude Code
/// works, well inside Letronna's five minutes of allowed quiet.
const KEEPALIVE: Duration = Duration::from_secs(20);
/// The largest request taken: a long session resent whole, pictures and all.
const BODY_LIMIT: usize = 64 * 1024 * 1024;
/// How a reply that asks whether Claude Code may work in a folder begins,
/// which is how the user's next message is known for its answer.
const TRUST_ASKED: &str = "Claude Code has not worked in";
/// The folder, in the plugin's own, that Claude Code works in for a chat
/// with no project folder.
const WORK_DIR: &str = "work";
/// What a streamed turn says when Claude Code stops to ask the user.
const ASKING: &str = "*Claude Code is asking you something. Open it from Settings › Plugins › Claude Code to answer.*\n\n";

pub(crate) struct ClaudeCode {
    hub: Arc<Hub>,
}

impl ClaudeCode {
    pub(crate) fn new() -> Self {
        Self {
            hub: Arc::new(Hub::default()),
        }
    }
}

impl Plugin for ClaudeCode {
    fn id(&self) -> &'static str {
        "claude-code"
    }
    fn name(&self) -> &'static str {
        "Claude Code"
    }
    fn description(&self) -> &'static str {
        "Hand work to Claude Code, one session per chat, and get its answer back."
    }

    /// Keeps up with the window: the chat on screen, whose mode a turn runs
    /// in, the Custom gateway, and the page as sessions come and go off the
    /// UI thread.
    fn init(&self, cx: &mut App) {
        let hub = self.hub.clone();
        cx.spawn(async move |cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(500))
                    .await;
                cx.update(|cx| {
                    if let Some(seen) = see(cx) {
                        hub.lock().seen = seen;
                    }
                    // The window may not be open yet at the first start.
                    if hub.rewire.swap(false, Ordering::Relaxed) && !wire(&hub, cx) {
                        hub.rewire.store(true, Ordering::Relaxed);
                    }
                    if hub.changed.swap(false, Ordering::Relaxed) {
                        cx.refresh_windows();
                    }
                });
            }
        })
        .detach();
    }

    // It opens a port and offers a tool that drives another agent, so the
    // user turns it on.
    fn default_enabled(&self) -> bool {
        false
    }

    fn handle(&self, _: &AppEvent) {}

    fn set_enabled(&self, enabled: bool) {
        if self.hub.enabled.swap(enabled, Ordering::Relaxed) == enabled {
            return;
        }
        if enabled {
            start(&self.hub);
        } else {
            self.hub.stop();
        }
    }
    fn enabled(&self) -> bool {
        self.hub.enabled.load(Ordering::Relaxed)
    }

    fn tools(&self) -> Vec<Arc<dyn Tool>> {
        vec![Arc::new(HandOff {
            hub: self.hub.clone(),
        })]
    }

    fn settings(&self, cx: &App) -> Option<AnyElement> {
        let muted = cx.theme().muted_foreground;
        let View {
            chats,
            notes,
            listed,
        } = self.hub.view();
        let summary = if chats.is_empty() {
            "No chat has a Claude Code yet. Pick claude-code as the model, or ask for Claude Code in a chat."
        } else {
            "The chats' Claude Code sessions, newest first:"
        };
        let here = self.hub.clone();
        Some(
            v_flex()
                .gap_2()
                .pt_1()
                .child(div().text_xs().text_color(muted).child(summary))
                .children(chats.into_iter().enumerate().map(|(at, (line, short, folder))| {
                    let hub = self.hub.clone();
                    h_flex()
                        .gap_3()
                        .items_center()
                        .justify_between()
                        .child(div().flex_1().min_w_0().text_sm().child(line))
                        .child(
                            Button::new(("claude-code-attach", at))
                                .framed(cx)
                                .xsmall()
                                .label("Open")
                                .on_click(move |_, _, cx| {
                                    // A failure is shown on the page.
                                    let _ = hub.open(&folder, &["attach", &short]);
                                    cx.refresh_windows();
                                }),
                        )
                }))
                .child(
                    h_flex().child(
                        Button::new("claude-code-open")
                            .framed(cx)
                            .small()
                            .label("Open Claude Code in this chat's folder")
                            .on_click(move |_, _, cx| {
                                let folder = chat_folder(cx).or_else(home).unwrap_or_default();
                                let _ = here.open(&here.work_in(folder), &[]);
                                cx.refresh_windows();
                            }),
                    ),
                )
                .children(
                    notes
                        .into_iter()
                        .map(|note| div().text_xs().text_color(cx.theme().danger).child(note)),
                )
                .when(listed, |this| {
                    this.child(div().text_xs().text_color(muted).child(format!(
                        "In the model list under {CUSTOM}, as {MODEL} and the other models Claude Code offers, in the window and the terminal while this window is open."
                    )))
                })
                .child(div().text_xs().text_color(muted).child(
                    "Each chat's Claude Code runs in the background in the chat's folder and mode, and follows it: a new or cleared chat starts a new session, an older chat resumes its own, and a changed mode, model or effort carries over. When it needs your permission the chat asks you, and your next message answers. Open shows a session in Terminal, which you never need to. A folder Claude Code has never been opened in must be trusted there once.",
                ))
                .into_any_element(),
        )
    }
}

/// What the plugin, its page and its tool share.
#[derive(Default)]
struct Hub {
    enabled: AtomicBool,
    /// Something the page shows moved; the next tick redraws.
    changed: AtomicBool,
    /// The model endpoint started or stopped; the next tick sets the Custom
    /// gateway to match.
    rewire: AtomicBool,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    /// Bumped each time listening starts or stops, so an older loop quits.
    run: u64,
    /// The listener's files and address, while it is on.
    link: Option<Link>,
    /// The model endpoint, while it is on.
    model: Option<Endpoint>,
    /// The chat on screen in the window.
    seen: Seen,
    /// The program Claude Code is, when not the one found on this computer.
    claude: Option<PathBuf>,
    /// The models Claude Code offered when last asked.
    catalog: Vec<Offered>,
    /// Claude Code's own settings file, when not the usual one.
    config: Option<PathBuf>,
    /// Why something last failed, for the page.
    note: Option<String>,
    /// Why Claude Code is not in the model list, when it is not.
    unlisted: Option<String>,
    /// What each Claude Code session's hooks have said, by its id.
    live: HashMap<String, Live>,
    /// The short ids of the sessions turns are waiting on.
    expecting: Vec<String>,
    /// Which session each chat has, oldest first.
    chats: Vec<(String, Session)>,
}

struct Link {
    dir: PathBuf,
    port: u16,
    token: String,
}

struct Endpoint {
    port: u16,
    key: String,
}

impl Endpoint {
    fn url(&self) -> String {
        format!("http://127.0.0.1:{}{MODEL_PATH}", self.port)
    }
}

/// One model Claude Code offers, as its `/model` lists it.
#[derive(Clone, Debug, PartialEq)]
struct Offered {
    /// Its name in Letronna's picker.
    id: String,
    /// What `--model` takes for it, empty for Claude Code's own pick.
    flag: String,
    /// The effort levels it takes.
    efforts: Vec<String>,
}

/// What the window shows: the chat on screen, and every chat on Claude
/// Code with its mode, so chats running at once each keep their own.
#[derive(Clone, Default)]
struct Seen {
    conversation: u64,
    folder: Option<PathBuf>,
    mode: Mode,
    /// The mode a session starts in, the terminal's too.
    default: Mode,
    /// Each chat on Claude Code: its first message, folder and mode.
    chats: Vec<(String, Option<PathBuf>, Mode)>,
}

/// What the hooks of one run of a Claude Code session have said.
#[derive(Default)]
struct Live {
    /// Where Claude Code keeps its conversation.
    transcript: PathBuf,
    /// Its turns' last messages, oldest first, not yet taken.
    answers: VecDeque<String>,
    /// It is waiting for the user to answer it.
    asking: bool,
    /// A permission it asks for, in words, and the hook waiting for the
    /// user's answer.
    question: Option<(String, TcpStream)>,
    ended: bool,
}

/// A chat's Claude Code session, and the options it runs with, which Claude
/// Code keeps with it.
#[derive(Clone, Debug, Default, PartialEq)]
struct Session {
    id: String,
    /// Its `--permission-mode`.
    mode: String,
    /// Its `--model`, or empty for Claude Code's own pick.
    model: String,
    /// Its `--effort`, or empty for Claude Code's own.
    effort: String,
    folder: PathBuf,
    /// Where Claude Code keeps its conversation; gone, Claude Code no
    /// longer has the session, and resuming it would start an empty one.
    transcript: PathBuf,
}

impl Session {
    fn runs_as(&self, other: &Session) -> bool {
        (&self.mode, &self.model, &self.effort) == (&other.mode, &other.model, &other.effort)
    }
}

/// What the page shows.
struct View {
    /// A line, the short id and the folder of each chat's session.
    chats: Vec<(String, String, PathBuf)>,
    notes: Vec<String>,
    /// Claude Code is in the model list.
    listed: bool,
}

/// Where a session's answer stands.
#[derive(Debug, PartialEq)]
enum Taken {
    Answer(String),
    /// A permission it asks the user for, in words.
    Question(String),
    Waiting {
        asking: bool,
    },
    Gone,
}

/// Listen in `data_dir()/claude-code`, and accept hooks until stopped.
fn start(hub: &Arc<Hub>) {
    let dir = backend::data_dir().join("claude-code");
    let listening = std::env::current_exe()
        .map_err(|e| format!("Cannot find Letronna's own program for Claude Code to run: {e}."))
        .and_then(|exe| listen(&dir, &exe))
        .and_then(|hooks| Ok((hooks, serving(&dir, model_port(&dir))?)));
    let mut state = hub.lock();
    state.run += 1;
    match listening {
        Ok(((listener, link), (endpoint, model))) => {
            let run = state.run;
            state.chats = load_chats(&dir);
            state.link = Some(link);
            state.model = Some(model);
            state.note = None;
            let hooks = hub.clone();
            std::thread::spawn(move || accept(hooks, listener, run, Hub::meet));
            let turns = hub.clone();
            std::thread::spawn(move || accept(turns, endpoint, run, Hub::serve));
        }
        Err(why) => state.note = Some(why),
    }
    hub.rewire.store(true, Ordering::Relaxed);
    hub.changed.store(true, Ordering::Relaxed);
}

/// Open a port and write the settings Claude Code is started with, whose
/// hooks run `exe`.
fn listen(dir: &Path, exe: &Path) -> Result<(TcpListener, Link), String> {
    let token = random_hex(32)?;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .map_err(|e| format!("Cannot open a port for Claude Code: {e}."))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Cannot open a port for Claude Code: {e}."))?
        .port();
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot write {}: {e}.", dir.display()))?;
    let port_file = dir.join(PORT_FILE);
    write_private(
        &port_file,
        &json!({"port": port, "token": token}).to_string(),
    )?;
    let settings = dir.join(SETTINGS_FILE);
    std::fs::write(&settings, hooks(&hook_command(exe, &port_file)).to_string())
        .map_err(|e| format!("Cannot write {}: {e}.", settings.display()))?;
    let link = Link {
        dir: dir.to_owned(),
        port,
        token,
    };
    Ok((listener, link))
}

/// The model endpoint's port for the data folder `dir`.
fn model_port(dir: &Path) -> u16 {
    let hash = digest::digest(&digest::SHA256, dir.display().to_string().as_bytes());
    let spread = u16::from_be_bytes([hash.as_ref()[0], hash.as_ref()[1]]) % 1000;
    MODEL_PORTS + spread
}

/// Open the model endpoint, on `port` when it is free, with the key the
/// Custom gateway is to send.
fn serving(dir: &Path, port: u16) -> Result<(TcpListener, Endpoint), String> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, port))
        .or_else(|_| TcpListener::bind((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|e| format!("Cannot open a port for Claude Code: {e}."))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("Cannot open a port for Claude Code: {e}."))?
        .port();
    let path = dir.join(KEY_FILE);
    let kept = std::fs::read_to_string(&path)
        .ok()
        .map(|key| key.trim().to_owned())
        .filter(|key| key.len() == 64);
    let key = match kept {
        Some(key) => key,
        None => {
            let key = random_hex(32)?;
            write_private(&path, &key)?;
            key
        }
    };
    Ok((listener, Endpoint { port, key }))
}

fn accept(hub: Arc<Hub>, listener: TcpListener, run: u64, serve: fn(&Hub, TcpStream, u64)) {
    for stream in listener.incoming() {
        if hub.lock().run != run {
            return;
        }
        let Ok(stream) = stream else { continue };
        let hub = hub.clone();
        std::thread::spawn(move || serve(&hub, stream, run));
    }
}

impl Hub {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing that can panic runs under this lock, so a poisoned one is
        // still whole.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stop listening. The chats' sessions are already stopped between
    /// turns, and keep their conversations.
    fn stop(&self) {
        let mut state = self.lock();
        state.run += 1;
        let link = state.link.take();
        let model = state.model.take();
        state.live.clear();
        drop(state);
        if let Some(link) = link {
            let _ = std::fs::remove_file(link.dir.join(PORT_FILE));
            // Wakes the accept loop, which sees its run is over.
            let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, link.port));
        }
        if let Some(model) = model {
            let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, model.port));
        }
        self.rewire.store(true, Ordering::Relaxed);
        self.changed.store(true, Ordering::Relaxed);
    }

    /// The handshake with a hook that connected, and then its report.
    fn meet(&self, stream: TcpStream, run: u64) {
        let token = {
            let state = self.lock();
            match &state.link {
                Some(link) if state.run == run => link.token.clone(),
                _ => return,
            }
        };
        let _ = stream.set_read_timeout(Some(HANDSHAKE_LIMIT));
        let Ok(mut lines) = stream.try_clone().map(BufReader::new) else {
            return;
        };
        let Some(theirs) =
            read(&mut lines).and_then(|hello| hello["nonce"].as_str().map(str::to_owned))
        else {
            return;
        };
        let Ok(ours) = random_hex(16) else { return };
        let proof = sign(&token, "letronna", &theirs);
        if say(&stream, &json!({"proof": proof, "nonce": ours})).is_err() {
            return;
        }
        let Some(report) = read(&mut lines) else {
            return;
        };
        if verify(
            &token,
            "hook",
            &ours,
            report["proof"].as_str().unwrap_or_default(),
        ) {
            self.heard(&report["hook"], stream);
        }
    }

    /// Take in what a hook reported. Only a session seen to start is taken
    /// in, so what a run that was stopped says late is not taken for the
    /// next run's.
    fn heard(&self, hook: &Value, stream: TcpStream) {
        let (Some(id), Some(event)) = (
            hook["session_id"].as_str(),
            hook["hook_event_name"].as_str(),
        ) else {
            return;
        };
        let mut state = self.lock();
        if event == "SessionStart" {
            let live = Live {
                transcript: PathBuf::from(hook["transcript_path"].as_str().unwrap_or_default()),
                ..Live::default()
            };
            state.live.insert(id.to_owned(), live);
            self.changed.store(true, Ordering::Relaxed);
            return;
        }
        // Claude Code may skip the start hook of a session's first run in a
        // folder it was just trusted in, so a session a turn waits on is
        // taken in from whatever it says first, but for an end, which is
        // what a run stopped before says late.
        let waited = state
            .expecting
            .iter()
            .any(|short| id.starts_with(short.as_str()));
        if !state.live.contains_key(id) && (!waited || event == "SessionEnd") {
            return;
        }
        let live = state.live.entry(id.to_owned()).or_default();
        if live.transcript.as_os_str().is_empty() {
            live.transcript = PathBuf::from(hook["transcript_path"].as_str().unwrap_or_default());
        }
        match event {
            "Stop" | "StopFailure" => {
                live.answers.push_back(answer(hook));
                live.asking = false;
                live.question = None;
            }
            // The hook waits on this connection for the user's answer.
            "PermissionRequest" => {
                live.question = Some((question(hook), stream));
                live.asking = true;
            }
            "Notification" => {
                live.asking = matches!(
                    hook["notification_type"].as_str(),
                    Some("permission_prompt" | "elicitation_dialog")
                );
            }
            "SessionEnd" => {
                live.ended = true;
                live.question = None;
            }
            _ => return,
        }
        self.changed.store(true, Ordering::Relaxed);
    }

    /// The answers session `id` has given, or where it stands.
    fn take(&self, id: &str) -> Taken {
        let mut state = self.lock();
        let Some(live) = state.live.get_mut(id) else {
            return Taken::Waiting { asking: false };
        };
        if !live.answers.is_empty() {
            let said: Vec<String> = live.answers.drain(..).collect();
            self.changed.store(true, Ordering::Relaxed);
            return Taken::Answer(said.join("\n\n"));
        }
        if let Some((what, _)) = &live.question {
            return Taken::Question(what.clone());
        }
        if live.ended {
            Taken::Gone
        } else {
            Taken::Waiting {
                asking: live.asking,
            }
        }
    }

    /// Answer the permission session `id` waits on with what the user said;
    /// false when it waits on none.
    fn decide(&self, id: &str, said: &str) -> bool {
        let question = self
            .lock()
            .live
            .get_mut(id)
            .and_then(|live| live.question.take());
        let Some((_, hook)) = question else {
            return false;
        };
        self.changed.store(true, Ordering::Relaxed);
        say(&hook, &decision(said)).is_ok()
    }

    /// Let the permission session `id` waits on go unanswered, so Claude
    /// Code asks in its own session: nobody in a chat will answer it.
    fn decline(&self, id: &str) {
        if let Some(live) = self.lock().live.get_mut(id) {
            live.question = None;
        }
    }

    /// The full id of the session whose short id is `short`, once it has
    /// started.
    fn started(&self, short: &str) -> Option<String> {
        self.lock()
            .live
            .keys()
            .find(|id| id.starts_with(short))
            .cloned()
    }

    /// The session chat `key` has, if it has one.
    fn session(&self, key: &str) -> Option<Session> {
        self.lock()
            .chats
            .iter()
            .find(|(chat, _)| chat == key)
            .map(|(_, session)| session.clone())
    }

    /// Note that chat `key` has `session` now, and keep the list on disk.
    fn bind(&self, key: String, session: Session) {
        let mut state = self.lock();
        state.chats.retain(|(chat, _)| *chat != key);
        state.chats.push((key, session));
        let over = state.chats.len().saturating_sub(SESSIONS_KEPT);
        state.chats.drain(..over);
        if let Some(link) = &state.link {
            save_chats(&link.dir, &state.chats);
        }
        self.changed.store(true, Ordering::Relaxed);
    }

    /// Run the message as the next prompt of the Claude Code session its
    /// chat has, in its mode with its model and effort, and wait for the
    /// answer. A chat with no session, or one Claude Code no longer has,
    /// starts one, told the chat so far. A message to a session waiting on
    /// the user's permission answers it, and the turn that asked goes on.
    /// `pause` waits a step, told whether Claude Code is asking the user
    /// something, and says whether to keep waiting.
    fn ask(
        &self,
        asked: Ask<'_>,
        mut pause: impl FnMut(bool) -> bool,
    ) -> Result<(String, Session), String> {
        let (dir, claude) = {
            let state = self.lock();
            let Some(link) = &state.link else {
                return Err("Turn the Claude Code plugin on first.".to_owned());
            };
            (
                link.dir.clone(),
                state.claude.clone().or_else(claude_program),
            )
        };
        let Some(claude) = claude else {
            return Err("Claude Code is not installed. Get it at https://claude.com/claude-code, then run claude once to sign in.".to_owned());
        };
        // A session in another folder is not this chat's any more: the chat
        // was moved to a project, and starts afresh there.
        let had = asked
            .key
            .and_then(|key| self.session(key))
            .filter(|session| same_folder(&session.folder, asked.folder))
            .filter(|session| session.transcript.is_file());
        let kept = |pick: fn(&Session) -> &String| had.as_ref().map(pick).cloned();
        let mut wanted = Session {
            mode: claude_mode(asked.mode).to_owned(),
            model: asked
                .model
                .map(str::to_owned)
                .or_else(|| kept(|had| &had.model))
                .unwrap_or_default(),
            effort: asked
                .effort
                .map(str::to_owned)
                .or_else(|| kept(|had| &had.effort))
                .unwrap_or_default(),
            folder: had
                .as_ref()
                .map_or_else(|| asked.folder.to_owned(), |had| had.folder.clone()),
            ..Session::default()
        };
        let waiting = had
            .as_ref()
            .filter(|had| self.decide(&had.id, asked.message));
        let run = match waiting {
            // The turn that asked goes on, as it was.
            Some(had) => {
                wanted = had.clone();
                short(&had.id).to_owned()
            }
            None => self.launch(&claude, &dir, had.as_ref(), &wanted, &asked)?,
        };
        let _expecting = Expecting::new(self, &run);
        let began = Instant::now();
        let mut id = None;
        let mut questioned = false;
        // Still at work at the limit: left running, for `collect` to read.
        let mut working = false;
        let answer = loop {
            id = id.or_else(|| self.started(&run));
            let mut asking = false;
            match &id {
                Some(id) => match self.take(id) {
                    Taken::Answer(answer) => break answer,
                    Taken::Question(what) if asked.questions => {
                        questioned = true;
                        break format!(
                            "Claude Code wants to {what}.\n\nReply **allow** to let it, **deny** to stop it, or say what it should do instead."
                        );
                    }
                    Taken::Question(_) => {
                        self.decline(id);
                        asking = true;
                    }
                    Taken::Gone => {
                        return Err("Claude Code ended before it answered.".to_owned());
                    }
                    Taken::Waiting { asking: now } => asking = now,
                },
                None if began.elapsed() >= START_LIMIT => {
                    // It waits on its own screen for someone to answer, and
                    // stays so for the user to.
                    let shown = screen(&claude, &run)
                        .map(|shown| format!(": \"{shown}\""))
                        .unwrap_or_default();
                    return Err(format!(
                        "Claude Code is waiting on its own screen before it starts{shown}. Open it from Settings › Plugins › Claude Code to answer, or run claude attach {run}."
                    ));
                }
                None => {}
            }
            if began.elapsed() >= ANSWER_LIMIT {
                working = true;
                break "Claude Code is still working on it. Ask again later to collect its answer."
                    .to_owned();
            }
            if !pause(asking) {
                stop(&claude, &run);
                return Err("Stopped. What Claude Code did so far stays in its session.".to_owned());
            }
        };
        // Stopped between turns, so no process waits idle for each chat; its
        // conversation stays, for the next message or `claude attach`. One
        // that asked the user waits for their answer, and one still at work
        // finishes it.
        if !questioned && !working {
            stop(&claude, &run);
        }
        let id = id.unwrap_or(run);
        let transcript = self
            .lock()
            .live
            .get(&id)
            .map(|live| live.transcript.clone())
            .unwrap_or_default();
        let session = Session {
            id,
            transcript,
            ..wanted
        };
        // A changed option made a copy, history and all; the old one goes
        // from Claude Code's list of sessions. Not when a resume failed for
        // another reason and a new session took over: the old may be fine.
        if let Some(had) = had.filter(|had| had.id != session.id && !had.runs_as(&session)) {
            let _ = Command::new(&claude)
                .args(["rm", short(&had.id)])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        }
        Ok((answer, session))
    }

    /// Start the message in a background session: the chat's own resumed,
    /// as a copy with other options, or a new one; its short id.
    fn launch(
        &self,
        claude: &Path,
        dir: &Path,
        had: Option<&Session>,
        wanted: &Session,
        asked: &Ask<'_>,
    ) -> Result<String, String> {
        let settings = dir.join(SETTINGS_FILE);
        if let Some(session) = had {
            // A woken session would take its next prompt as a copy's first.
            stop(claude, short(&session.id));
            if session.runs_as(wanted) {
                // Its next run starts afresh in what the hooks say.
                self.lock().live.remove(&session.id);
            }
            let args = launch_args(Some(session), wanted, &settings, asked.message);
            if let Ok(short) = background(claude, &args, &session.folder) {
                return Ok(short);
            }
        }
        // No session, or one Claude Code no longer has.
        let told = format!("{}{}", asked.earlier, asked.message);
        background(
            claude,
            &launch_args(None, wanted, &settings, &told),
            asked.folder,
        )
    }

    /// The folder Claude Code works in for a chat's `folder`.
    fn work_in(&self, folder: PathBuf) -> PathBuf {
        let dir = self.lock().link.as_ref().map(|link| link.dir.clone());
        match dir {
            Some(dir) => work_in(folder, home().as_deref(), &dir),
            None => folder,
        }
    }

    /// Note in Claude Code's own settings that the user trusts it in
    /// `folder`.
    fn trust(&self, folder: &Path) -> Result<(), String> {
        let config = self.lock().config.clone().or_else(claude_config);
        let Some(config) = config else {
            return Err("Cannot find Claude Code's settings to note the trust in.".to_owned());
        };
        let servers = undecided_servers(folder, claude_settings().as_deref());
        trust_in(&config, folder)?;
        leave_off(folder, &servers)
    }

    /// Open a terminal in `folder` running `claude` with `args`; a failure is
    /// noted for the page and returned.
    fn open(&self, folder: &Path, args: &[&str]) -> Result<(), String> {
        let dir = self.lock().link.as_ref().map(|link| link.dir.clone());
        let opened = match dir {
            None => Err("Turn the Claude Code plugin on first.".to_owned()),
            Some(dir) => launch(&dir, folder, args),
        };
        self.lock().note = opened.as_ref().err().cloned();
        self.changed.store(true, Ordering::Relaxed);
        opened
    }

    /// Answer one request to the model endpoint.
    fn serve(&self, stream: TcpStream, run: u64) {
        let endpoint = {
            let state = self.lock();
            match &state.model {
                Some(model) if state.run == run => (model.port, model.key.clone()),
                _ => return,
            }
        };
        let _ = stream.set_read_timeout(Some(HANDSHAKE_LIMIT));
        let Some(request) = request(&stream) else {
            return;
        };
        let _ = stream.set_read_timeout(None);
        if let Err((status, why)) = allowed(&request, endpoint.0, &endpoint.1) {
            let _ = respond(&stream, status, &failure(why));
            return;
        }
        let route = request.path.strip_prefix(MODEL_PATH);
        let _ = match (request.method.as_str(), route) {
            ("GET", Some("/models")) => respond(&stream, "200 OK", &models(&self.catalog(true))),
            ("POST", Some("/chat/completions")) => {
                self.turn(&stream, &request.body);
                Ok(())
            }
            _ => respond(
                &stream,
                "404 Not Found",
                &failure("Claude Code's endpoint has only /models and /chat/completions."),
            ),
        };
    }

    /// A chat turn: the user's newest message goes to the chat's Claude
    /// Code, and its answer streams back as the reply.
    fn turn(&self, stream: &TcpStream, body: &[u8]) {
        let request = serde_json::from_slice::<Value>(body).unwrap_or_default();
        // Compaction, the Auto-mode judge and commit messages ask without
        // streaming; they are Letronna's own, not the user's to pass on.
        if request["stream"] != true {
            let _ = respond(
                stream,
                "400 Bad Request",
                &failure("Claude Code answers chat turns only."),
            );
            return;
        }
        let Some(message) = newest(&request) else {
            let _ = respond(
                stream,
                "400 Bad Request",
                &failure("Claude Code answers the user's messages only."),
            );
            return;
        };
        if (&*stream)
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nCache-Control: no-cache\r\nConnection: close\r\n\r\n")
            .is_err()
        {
            return;
        }
        let seen = self.lock().seen.clone();
        // The window's own chat is for a request whose prompt names no folder.
        let asked_in = project_folder(&request)
            .or_else(|| seen.folder.clone())
            .or_else(home)
            .unwrap_or_default();
        let mode = mode_for(&request, &asked_in, &seen);
        let folder = self.work_in(asked_in);
        let key = chat_key(&request);
        let (mut request, mut message) = (request, message);
        // The answer to whether Claude Code may work in the folder: yes
        // notes the trust and sends the message that asked for it.
        if asked_trust(&request) {
            if decision(&message)["behavior"] != "allow" {
                let no = format!(
                    "Claude Code stays out of `{}`. Reply **trust** to let it work there, or give this chat a project folder.",
                    folder.display()
                );
                let _ = (&*stream).write_all(said(&no).as_bytes());
                return;
            }
            if let Err(why) = self.trust(&folder) {
                let _ = (&*stream).write_all(format!("data: {}\n\n", failure(&why)).as_bytes());
                return;
            }
            request = before_trust(&request);
            let Some(again) = newest(&request) else {
                let _ = (&*stream).write_all(said("Claude Code is trusted there now.").as_bytes());
                return;
            };
            message = again;
        }
        let asked = request["model"].as_str().unwrap_or(MODEL);
        let catalog = self.catalog(false);
        // A model Claude Code no longer offers leaves the session's as it is.
        let model = catalog.iter().find(|model| model.id == asked);
        let mut quiet = Instant::now();
        let mut told = false;
        let before = earlier(&request);
        let asked = Ask {
            key: key.as_deref(),
            folder: &folder,
            mode,
            model: model.map(|model| model.flag.as_str()),
            effort: Some(claude_effort(&request)),
            earlier: &before,
            message: &message,
            questions: true,
        };
        let asked = self.ask(asked, |asking| {
            std::thread::sleep(STEP);
            if asking && !told {
                told = true;
                quiet = Instant::now();
                return (&*stream).write_all(event(ASKING).as_bytes()).is_ok();
            }
            if quiet.elapsed() < KEEPALIVE {
                return true;
            }
            quiet = Instant::now();
            // Stopping the reply in Letronna hangs up, and a write then fails.
            (&*stream)
                .write_all(b": Claude Code is working\n\n")
                .is_ok()
        });
        let events = match asked {
            Ok((answer, session)) => {
                // A new chat is known by its first exchange from now on.
                let key = key.unwrap_or_else(|| key_of(&message, &answer));
                self.bind(key, session);
                said(&answer)
            }
            // Claude Code works only in a folder the user trusts it in, and
            // asks once; the chat asks instead.
            Err(why) if why.contains("Workspace not trusted") => {
                let servers = undecided_servers(&folder, claude_settings().as_deref());
                let off = if servers.is_empty() {
                    String::new()
                } else {
                    format!(
                        " The MCP servers project files here name ({}) stay off, as Claude Code leaves them until asked; its /mcp turns them on.",
                        servers.join(", ")
                    )
                };
                said(&format!(
                    "{TRUST_ASKED} `{}` before. Letting it in is like opening Claude Code there and trusting the folder: it can read, change and run things in it, as its mode allows.{off}\n\nReply **trust** to let it, and your message goes to it.",
                    folder.display()
                ))
            }
            Err(why) => format!("data: {}\n\n", failure(&why)),
        };
        let _ = (&*stream).write_all(events.as_bytes());
    }

    /// The models Claude Code offers: asked afresh when `fresh`, or when
    /// it never has been; the last answer, or only its own pick, when it
    /// cannot be asked.
    fn catalog(&self, fresh: bool) -> Vec<Offered> {
        let (known, claude) = {
            let state = self.lock();
            (
                state.catalog.clone(),
                state.claude.clone().or_else(claude_program),
            )
        };
        if !fresh && !known.is_empty() {
            return known;
        }
        match claude.and_then(|claude| offered(&claude)) {
            Some(catalog) if !catalog.is_empty() => {
                self.lock().catalog = catalog.clone();
                catalog
            }
            _ if !known.is_empty() => known,
            _ => vec![Offered {
                id: MODEL.to_owned(),
                flag: String::new(),
                efforts: Vec::new(),
            }],
        }
    }

    /// Everything a chat's session said after its call stopped waiting.
    fn collect(&self, key: &str) -> String {
        let Some(session) = self.session(key) else {
            return "This chat has no Claude Code yet.".to_owned();
        };
        match self.take(&session.id) {
            Taken::Answer(answer) => answer,
            Taken::Waiting { asking: true } => "Claude Code is asking the user something. They can answer it from Settings › Plugins › Claude Code.".to_owned(),
            _ => "Nothing new from Claude Code.".to_owned(),
        }
    }

    /// The newest chats' sessions for the page, what went wrong, and whether
    /// Claude Code is in the model list.
    fn view(&self) -> View {
        let state = self.lock();
        let chats = state
            .chats
            .iter()
            .rev()
            .take(5)
            .map(|(_, session)| {
                let doing = match state.live.get(&session.id) {
                    Some(live) if live.asking => "asking you",
                    Some(live) if !live.ended => "working",
                    _ => "stopped",
                };
                let name = session.folder.file_name().map_or_else(
                    || session.folder.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                let model = Some(session.model.as_str()).filter(|model| !model.is_empty());
                let line = format!(
                    "{name} · {} · {} · {doing} · {}",
                    model.unwrap_or("Claude Code's model"),
                    session.mode,
                    short(&session.id)
                );
                (line, short(&session.id).to_owned(), session.folder.clone())
            })
            .collect();
        View {
            chats,
            notes: state.note.iter().chain(&state.unlisted).cloned().collect(),
            listed: state.model.is_some() && state.unlisted.is_none(),
        }
    }
}

/// A session a turn waits on, for as long as it waits.
struct Expecting<'a> {
    hub: &'a Hub,
    short: String,
}

impl<'a> Expecting<'a> {
    fn new(hub: &'a Hub, short: &str) -> Self {
        hub.lock().expecting.push(short.to_owned());
        Self {
            hub,
            short: short.to_owned(),
        }
    }
}

impl Drop for Expecting<'_> {
    fn drop(&mut self) {
        let mut state = self.hub.lock();
        if let Some(at) = state
            .expecting
            .iter()
            .position(|short| *short == self.short)
        {
            state.expecting.remove(at);
        }
    }
}

/// One message for a chat's Claude Code, and how to run it.
struct Ask<'a> {
    /// The chat, known by its key, or None for its first message.
    key: Option<&'a str>,
    folder: &'a Path,
    mode: Mode,
    /// `--model` and `--effort`, or None to keep the session's.
    model: Option<&'a str>,
    effort: Option<&'a str>,
    /// The chat so far, for a session that starts after the chat did.
    earlier: &'a str,
    message: &'a str,
    /// Hand Claude Code's permission questions back as the reply, for the
    /// user to answer with their next message.
    questions: bool,
}

/// What the agent calls to hand work to Claude Code.
struct HandOff {
    hub: Arc<Hub>,
}

impl Tool for HandOff {
    fn name(&self) -> &'static str {
        "claude_code"
    }
    fn description(&self) -> &'static str {
        "Hand a task or question to Claude Code, Anthropic's coding agent, on this computer, and return its answer. This chat has a Claude Code session of its own in the chat's folder and mode, which remembers earlier messages. Claude Code works with its own tools; when it needs the user's permission, they answer it in Claude Code. Leave out message to collect an answer that was still coming."
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "message": {
                    "type": "string",
                    "description": "What to ask Claude Code, as you would ask a colleague: the task, what done looks like, and anything it cannot see. Leave out to collect a late answer."
                }
            }
        })
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn mutates(&self) -> bool {
        true
    }
    fn untrusted(&self) -> bool {
        true
    }

    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let key = format!("chat {}", ctx.conversation);
        let Some(message) = args["message"]
            .as_str()
            .map(str::trim)
            .filter(|m| !m.is_empty())
        else {
            return Ok(self.hub.collect(&key));
        };
        let seen = self.hub.lock().seen.clone();
        let mode = match () {
            () if ctx.plan => Mode::Plan,
            () if seen.conversation == ctx.conversation => seen.mode,
            () => seen.default,
        };
        let folder = self
            .hub
            .work_in(ctx.workspace.clone().or_else(home).unwrap_or_default());
        // Claude Code's model and effort stay as the session has them, and
        // its questions are asked in its own session: the agent is not the
        // user, and must not answer them.
        let asked = Ask {
            key: Some(&key),
            folder: &folder,
            mode,
            model: None,
            effort: None,
            earlier: "",
            message,
            questions: false,
        };
        let (answer, session) = self.hub.ask(asked, |_| ctx.cancel.sleep(STEP)).map_err(|why| {
            if why.contains("Workspace not trusted") {
                format!("{TRUST_ASKED} `{}` before, and must be trusted there first. The user can pick claude-code as the model in a chat in that folder and reply trust, or run claude there once.", folder.display())
            } else {
                why
            }
        })?;
        self.hub.bind(key, session);
        Ok(answer)
    }
}

/// The arguments that run `message` as the next prompt: a new session, the
/// same one resumed, or, with other options, Claude Code's copy of it, which
/// takes no option over from the one it copies.
fn launch_args(
    had: Option<&Session>,
    wanted: &Session,
    settings: &Path,
    message: &str,
) -> Vec<String> {
    let mut args = vec!["--bg".to_owned()];
    if let Some(had) = had {
        args.extend(["--resume".to_owned(), had.id.clone()]);
    }
    if had.is_none_or(|had| !had.runs_as(wanted)) {
        args.extend([
            "--settings".to_owned(),
            settings.display().to_string(),
            "--permission-mode".to_owned(),
            wanted.mode.clone(),
        ]);
        for (flag, value) in [("--model", &wanted.model), ("--effort", &wanted.effort)] {
            if !value.is_empty() {
                args.extend([flag.to_owned(), value.clone()]);
            }
        }
    }
    // A message starting with a dash is the prompt, not an option.
    args.extend(["--".to_owned(), message.to_owned()]);
    args
}

/// The `--effort` a request asks for, empty for Claude Code's own.
fn claude_effort(request: &Value) -> &'static str {
    let asked = request["reasoning_effort"].as_str().unwrap_or_default();
    LEVELS
        .iter()
        .find(|(level, _)| *level == asked)
        .map_or("", |(level, _)| level)
}

/// The models Claude Code offers now, which its SDK's handshake answers
/// as `/model` lists them, without a prompt or a word to the model; None
/// when Claude Code cannot be asked.
fn offered(claude: &Path) -> Option<Vec<Offered>> {
    let mut child = Command::new(claude)
        .args([
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
        ])
        .current_dir(home().unwrap_or_else(std::env::temp_dir))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let hello = json!({"type": "control_request", "request_id": "models", "request": {"subtype": "initialize"}});
    // Closed after the one line, so Claude Code ends once it has answered.
    let mut stdin = child.stdin.take()?;
    let _ = writeln!(stdin, "{hello}");
    drop(stdin);
    let stdout = child.stdout.take()?;
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let answer = BufReader::new(stdout)
            .lines()
            .map_while(Result::ok)
            .filter_map(|line| serde_json::from_str::<Value>(&line).ok())
            .find(|line| line["type"] == "control_response");
        let _ = tx.send(answer);
    });
    let answer = rx.recv_timeout(OFFERED_LIMIT).ok().flatten();
    let _ = child.kill();
    let _ = child.wait();
    let models = answer?["response"]["response"]["models"]
        .as_array()?
        .clone();
    Some(catalog(&models))
}

/// The picker's models for what Claude Code's handshake listed: its own
/// pick as `claude-code`, each other as `claude-code-` and its name.
fn catalog(models: &[Value]) -> Vec<Offered> {
    let mut catalog: Vec<Offered> = Vec::new();
    for model in models {
        let Some(value) = model["value"].as_str() else {
            continue;
        };
        let (id, flag) = if value == "default" {
            (MODEL.to_owned(), String::new())
        } else {
            let name = model["displayName"].as_str().unwrap_or(value);
            (format!("{MODEL}-{}", slug(name)), value.to_owned())
        };
        let mut efforts: Vec<String> = model["supportedEffortLevels"]
            .as_array()
            .map(|levels| {
                levels
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        // Ultracode works at the hardest level a model offers.
        if efforts.iter().any(|level| level == "xhigh") {
            efforts.push("ultracode".to_owned());
        }
        if !catalog.iter().any(|offered| offered.id == id) {
            catalog.push(Offered { id, flag, efforts });
        }
    }
    catalog
}

/// `name` in lower case with anything but letters and digits as one dash.
fn slug(name: &str) -> String {
    name.to_lowercase()
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

/// Start a background session with `args` in `folder`; its short id.
fn background(claude: &Path, args: &[String], folder: &Path) -> Result<String, String> {
    let mut command = Command::new(claude);
    command.args(args).current_dir(folder).stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    let done = command
        .output()
        .map_err(|e| format!("Cannot start Claude Code: {e}."))?;
    let said = format!(
        "{}{}",
        String::from_utf8_lossy(&done.stdout),
        String::from_utf8_lossy(&done.stderr)
    );
    backgrounded(&said)
}

/// The short id in what `claude --bg` printed, or why it did not start.
fn backgrounded(said: &str) -> Result<String, String> {
    let short = said.lines().find_map(|line| {
        let rest = line.trim().strip_prefix("backgrounded · ")?;
        rest.split(" · ")
            .next()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    });
    match short {
        Some(short) => Ok(short.to_owned()),
        None if said.trim().is_empty() => Err("Claude Code did not start.".to_owned()),
        None => Err(format!("Claude Code did not start: {}", said.trim())),
    }
}

/// Stop background session `short`; its conversation is kept. One already
/// stopped is left as it is.
fn stop(claude: &Path, short: &str) {
    let _ = Command::new(claude)
        .args(["stop", short])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// The short id Claude Code's commands take: the start of the session's.
fn short(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

/// Where `claude` is: on the PATH, else where its installers put it, since
/// an app opened from the Dock does not get the shell's PATH.
fn claude_program() -> Option<PathBuf> {
    let name = if cfg!(windows) {
        "claude.exe"
    } else {
        "claude"
    };
    let path = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    let installed = home()
        .map(|home| vec![home.join(".local/bin"), home.join(".claude/local")])
        .unwrap_or_default();
    let shared = ["/opt/homebrew/bin", "/usr/local/bin"].map(PathBuf::from);
    path.into_iter()
        .chain(installed)
        .chain(shared)
        .map(|dir| dir.join(name))
        .find(|program| program.is_file())
}

/// Claude Code's `--permission-mode` for Letronna's mode, which are the same
/// six.
fn claude_mode(mode: Mode) -> &'static str {
    match mode {
        Mode::Default => "default",
        Mode::AcceptEdits => "acceptEdits",
        Mode::Plan => "plan",
        Mode::Auto => "auto",
        Mode::DontAsk => "dontAsk",
        Mode::Bypass => "bypassPermissions",
    }
}

/// The mode a turn runs in: Plan when the request's own prompt says so,
/// else the mode of the window's chat it comes from, known by its first
/// message and folder, else the chat's on screen when it comes from that
/// folder, else the mode sessions start in, which the terminal shares.
fn mode_for(request: &Value, folder: &Path, seen: &Seen) -> Mode {
    let plan = system_text(request).is_some_and(|text| text.contains("Plan mode is on"));
    if plan {
        return Mode::Plan;
    }
    let here = |chat: &Option<PathBuf>| {
        chat.clone()
            .or_else(home)
            .is_some_and(|chat| same_folder(&chat, folder))
    };
    let first = first_user(request).map(|text| words(&text));
    let chat = seen
        .chats
        .iter()
        .find(|(said, chat, _)| Some(said) == first.as_ref() && here(chat));
    match chat {
        Some((_, _, mode)) => *mode,
        None if here(&seen.folder) => seen.mode,
        None => seen.default,
    }
}

/// The chat before its newest message, as words Claude Code can read, for
/// a session that starts after the chat did; empty for a chat's first
/// message. A long chat keeps its newest part.
fn earlier(request: &Value) -> String {
    let Some(messages) = request["messages"].as_array() else {
        return String::new();
    };
    let said: Vec<String> = messages
        .iter()
        .take(messages.len().saturating_sub(1))
        .filter_map(|message| {
            let who = match message["role"].as_str()? {
                "user" => "User",
                "assistant" => "Assistant",
                _ => return None,
            };
            let text = text_of(&message["content"])?;
            let text = text.trim();
            (!text.is_empty()).then(|| format!("{who}: {text}"))
        })
        .collect();
    let mut kept = Vec::new();
    let mut size = 0;
    for line in said.iter().rev() {
        size += line.len() + 2;
        if size > EARLIER_CHARS {
            kept.push("(Earlier messages left out.)".to_owned());
            break;
        }
        kept.push(line.clone());
    }
    if kept.is_empty() {
        return String::new();
    }
    kept.reverse();
    format!(
        "This chat began in Letronna before this session. What was said so far:\n\n{}\n\n---\n\n",
        kept.join("\n\n")
    )
}

/// `text` as a whole streamed reply.
fn said(text: &str) -> String {
    format!(
        "{}data: {}\n\ndata: [DONE]\n\n",
        event(text),
        chunk(json!({}), json!("stop"))
    )
}

/// Whether the reply before the newest message asked whether Claude Code
/// may work in a folder.
fn asked_trust(request: &Value) -> bool {
    request["messages"]
        .as_array()
        .and_then(|messages| messages.iter().rev().nth(1))
        .filter(|message| message["role"] == "assistant")
        .and_then(|message| text_of(&message["content"]))
        .is_some_and(|text| text.starts_with(TRUST_ASKED))
}

/// The request as it was before Claude Code asked to be trusted, so the
/// message that asked for it is the newest again.
fn before_trust(request: &Value) -> Value {
    let mut request = request.clone();
    if let Some(messages) = request["messages"].as_array_mut() {
        messages.truncate(messages.len().saturating_sub(2));
    }
    request
}

/// The folder Claude Code works in for a chat's `folder`: never the home
/// folder, which Claude Code trusts one interactive session at a time and
/// never in the background, but one of the plugin's own in `dir`.
fn work_in(folder: PathBuf, home: Option<&Path>, dir: &Path) -> PathBuf {
    let homeless =
        folder.as_os_str().is_empty() || home.is_some_and(|home| same_folder(home, &folder));
    if !homeless {
        return folder;
    }
    let work = dir.join(WORK_DIR);
    let _ = std::fs::create_dir_all(&work);
    work
}

/// Note in the folder's own Claude Code settings that `servers` stay off,
/// as the question Claude Code asks about them first leaves them, and where
/// it notes the answer itself.
fn leave_off(folder: &Path, servers: &[String]) -> Result<(), String> {
    if servers.is_empty() {
        return Ok(());
    }
    let dir = folder.join(".claude");
    let path = dir.join("settings.local.json");
    let unreadable = || {
        format!(
            "{} could not be read, so the MCP servers were not left off.",
            path.display()
        )
    };
    let mut settings: Value = match std::fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(|_| unreadable())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(_) => return Err(unreadable()),
    };
    if !settings.is_object() {
        return Err(unreadable());
    }
    let mut off: Vec<Value> = settings["disabledMcpjsonServers"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    for server in servers {
        if !off.iter().any(|name| name == server.as_str()) {
            off.push(json!(server));
        }
    }
    settings["disabledMcpjsonServers"] = Value::Array(off);
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot write {}: {e}.", dir.display()))?;
    let next = path.with_extension("json.letronna");
    let text = serde_json::to_string_pretty(&settings).map_err(|_| unreadable())?;
    std::fs::write(&next, text)
        .and_then(|()| std::fs::rename(&next, &path))
        .map_err(|e| format!("Cannot write {}: {e}.", path.display()))
}

/// The MCP servers the `.mcp.json` files from `folder` up name, nearest
/// first, that the user's Claude Code settings at `settings` have not
/// turned on: Claude Code stops to ask about each before it starts.
fn undecided_servers(folder: &Path, settings: Option<&Path>) -> Vec<String> {
    let project = folder.join(".claude");
    let sources = [
        settings.map(Path::to_owned),
        Some(project.join("settings.json")),
        Some(project.join("settings.local.json")),
    ];
    let mut decided: Vec<String> = Vec::new();
    for source in sources.into_iter().flatten() {
        let chosen: Value = std::fs::read_to_string(&source)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        if chosen["enableAllProjectMcpServers"] == true {
            return Vec::new();
        }
        for list in ["enabledMcpjsonServers", "disabledMcpjsonServers"] {
            let names = chosen[list].as_array().into_iter().flatten();
            decided.extend(names.filter_map(Value::as_str).map(str::to_owned));
        }
    }
    let mut found: Vec<String> = Vec::new();
    for dir in folder.ancestors() {
        let Ok(text) = std::fs::read_to_string(dir.join(".mcp.json")) else {
            continue;
        };
        let Ok(listed) = serde_json::from_str::<Value>(&text) else {
            continue;
        };
        let Some(servers) = listed["mcpServers"].as_object() else {
            continue;
        };
        for name in servers.keys() {
            if !decided.contains(name) && !found.contains(name) {
                found.push(name.clone());
            }
        }
    }
    found
}

/// The user's own Claude Code settings.
fn claude_settings() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir).join("settings.json")),
        None => home().map(|home| home.join(".claude").join("settings.json")),
    }
}

/// The last words background session `short` shows on its screen.
fn screen(claude: &Path, short: &str) -> Option<String> {
    let shown = Command::new(claude)
        .args(["logs", short])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    let text = plain(&String::from_utf8_lossy(&shown.stdout));
    // Words only: the screen's borders are lines of box drawing.
    let words: Vec<&str> = text
        .split_whitespace()
        .filter(|word| word.chars().any(char::is_alphanumeric))
        .collect();
    let last = words[words.len().saturating_sub(40)..].join(" ");
    (!last.is_empty()).then_some(last)
}

/// `text` with its terminal escapes as spaces, which is where they moved
/// the cursor between words.
fn plain(text: &str) -> String {
    let mut out = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(if c.is_control() && c != '\n' { ' ' } else { c });
            continue;
        }
        match chars.next() {
            // A control sequence ends at its first letter or symbol.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // An operating system command ends at a bell or ESC \\.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' {
                        break;
                    }
                    if c == '\u{1b}' {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {}
        }
        out.push(' ');
    }
    out
}

/// Claude Code's own settings file, where it notes which folders it is
/// trusted in.
fn claude_config() -> Option<PathBuf> {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(dir) => Some(PathBuf::from(dir).join(".claude.json")),
        None => home().map(|home| home.join(".claude.json")),
    }
}

/// Note in Claude Code's settings at `config` that it is trusted in
/// `folder`, as its own trust prompt does when the user says yes. Written
/// beside it and moved over it, so Claude Code never reads half of it, and
/// kept readable by its owner only.
fn trust_in(config: &Path, folder: &Path) -> Result<(), String> {
    let unreadable =
        || "Claude Code's settings could not be read, so the trust was not noted.".to_owned();
    let mut settings: Value = match std::fs::read_to_string(config) {
        Ok(text) => serde_json::from_str(&text).map_err(|_| unreadable())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
        Err(_) => return Err(unreadable()),
    };
    if !settings.is_object() {
        return Err(unreadable());
    }
    let key = folder
        .canonicalize()
        .unwrap_or_else(|_| folder.to_owned())
        .display()
        .to_string();
    if !settings["projects"].is_object() {
        settings["projects"] = json!({});
    }
    if !settings["projects"][&key].is_object() {
        settings["projects"][&key] = json!({});
    }
    settings["projects"][&key]["hasTrustDialogAccepted"] = json!(true);
    let next = config.with_extension("json.letronna");
    let text = serde_json::to_string_pretty(&settings).map_err(|_| unreadable())?;
    write_private(&next, &text)?;
    std::fs::rename(&next, config).map_err(|e| format!("Cannot write {}: {e}.", config.display()))
}

/// The text of a request's first message from the user.
fn first_user(request: &Value) -> Option<String> {
    request["messages"]
        .as_array()?
        .iter()
        .find(|message| message["role"] == "user")
        .and_then(|message| text_of(&message["content"]))
}

/// `text` with its spacing made plain, since a chat may keep the words
/// spaced its own way.
fn words(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The chat a request is from, known by its first exchange, or None for a
/// chat's first message, which starts a new session.
fn chat_key(request: &Value) -> Option<String> {
    let messages = request["messages"].as_array()?;
    let said = |role: &str| {
        messages
            .iter()
            .filter(|message| message["role"] == role)
            .filter_map(|message| text_of(&message["content"]))
            .collect::<Vec<_>>()
    };
    let (asked, answered) = (said("user"), said("assistant"));
    if asked.len() < 2 {
        return None;
    }
    Some(key_of(&asked[0], answered.first()?))
}

/// The key of a chat whose first exchange is `asked` and `answered`.
fn key_of(asked: &str, answered: &str) -> String {
    let both = format!("{}\n{}", words(asked), words(answered));
    hex(&digest::digest(&digest::SHA256, both.as_bytes()).as_ref()[..16])
}

fn load_chats(dir: &Path) -> Vec<(String, Session)> {
    let Ok(text) = std::fs::read_to_string(dir.join(SESSIONS_FILE)) else {
        return Vec::new();
    };
    let Ok(Value::Array(chats)) = serde_json::from_str::<Value>(&text) else {
        return Vec::new();
    };
    chats
        .iter()
        .filter_map(|chat| {
            let text = |name: &str| chat[name].as_str().unwrap_or_default().to_owned();
            let session = Session {
                id: chat["session"].as_str()?.to_owned(),
                mode: chat["mode"].as_str()?.to_owned(),
                model: text("model"),
                effort: text("effort"),
                folder: PathBuf::from(chat["folder"].as_str()?),
                transcript: PathBuf::from(text("transcript")),
            };
            Some((chat["chat"].as_str()?.to_owned(), session))
        })
        .collect()
}

fn save_chats(dir: &Path, chats: &[(String, Session)]) {
    let list: Vec<Value> = chats
        .iter()
        .map(|(chat, session)| {
            json!({
                "chat": chat,
                "session": session.id,
                "mode": session.mode,
                "model": session.model,
                "effort": session.effort,
                "folder": session.folder.display().to_string(),
                "transcript": session.transcript.display().to_string(),
            })
        })
        .collect();
    let _ = std::fs::write(dir.join(SESSIONS_FILE), Value::Array(list).to_string());
}

/// The chat on screen in the window, if the window is open.
fn see(cx: &App) -> Option<Seen> {
    let chat = cx.try_global::<crate::ui::quick::MainChat>()?.0.read(cx);
    let conversation = chat.store.conversations.get(chat.store.active)?;
    let chats = (0..chat.store.conversations.len())
        .filter(|&at| chat.store.model(at).1.starts_with(MODEL))
        .filter_map(|at| {
            let conversation = &chat.store.conversations[at];
            let first = conversation
                .messages
                .iter()
                .find(|said| said.role == "user")?;
            Some((
                words(&first.content),
                conversation.workspace.clone(),
                chat.mode(conversation.id),
            ))
        })
        .collect();
    Some(Seen {
        conversation: conversation.id,
        folder: conversation.workspace.clone(),
        mode: chat.mode(conversation.id),
        default: chat.store.settings.mode,
        chats,
    })
}

/// One request to the model endpoint.
struct Request {
    method: String,
    path: String,
    /// Names in lower case.
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Request {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(named, _)| named == name)
            .map(|(_, value)| value.as_str())
    }
}

/// Read one HTTP/1.1 request, or None for anything that is not one.
fn request(stream: &TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream.try_clone().ok()?);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut words = line.split_whitespace();
    let (method, path) = (words.next()?.to_owned(), words.next()?.to_owned());
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 || headers.len() > 100 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        let (name, value) = line.split_once(':')?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
    }
    let length = match headers.iter().find(|(name, _)| name == "content-length") {
        Some((_, value)) => value.parse().ok()?,
        None => 0,
    };
    if length > BODY_LIMIT {
        return None;
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request {
        method,
        path,
        headers,
        body,
    })
}

/// Refuse what is not Letronna's own gateway call. A web page can make this
/// computer send a request here, but it cannot leave out its Origin, name
/// this address as the Host past a browser's rules, or know the key.
fn allowed(request: &Request, port: u16, key: &str) -> Result<(), (&'static str, &'static str)> {
    let host = request.header("host").unwrap_or_default();
    if host != format!("127.0.0.1:{port}") && host != format!("localhost:{port}") {
        return Err(("403 Forbidden", "Claude Code answers this computer only."));
    }
    if request.header("origin").is_some() {
        return Err(("403 Forbidden", "Claude Code does not answer web pages."));
    }
    let given = request
        .header("authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default();
    if !same(given, key) {
        return Err((
            "401 Unauthorized",
            "Wrong key for Claude Code. Turn the Claude Code plugin off and on to set the Custom gateway again.",
        ));
    }
    Ok(())
}

/// Compare two keys without the time taken telling how much matched.
fn same(given: &str, key: &str) -> bool {
    given.len() == key.len()
        && given
            .bytes()
            .zip(key.bytes())
            .fold(0, |differ, (a, b)| differ | (a ^ b))
            == 0
}

fn respond(mut stream: &TcpStream, status: &str, body: &Value) -> std::io::Result<()> {
    let body = body.to_string();
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )
}

fn failure(why: &str) -> Value {
    json!({"error": {"message": why}})
}

/// The model list: the models Claude Code offers, each with its effort
/// levels, and a window big enough that Letronna sends the session whole;
/// only its newest message is passed on.
fn models(catalog: &[Offered]) -> Value {
    let data: Vec<Value> = catalog
        .iter()
        .map(|offered| {
            let mut entry = json!({
                "id": offered.id,
                "object": "model",
                "owned_by": "anthropic",
                "context_length": 1_000_000,
                "max_output_tokens": 64_000,
            });
            if offered.efforts.is_empty() {
                entry["capabilities"] = json!({"reasoning": false});
            } else {
                let levels: Vec<Value> = offered
                    .efforts
                    .iter()
                    .map(|level| {
                        let label = LEVELS
                            .iter()
                            .find(|(known, _)| known == level)
                            .map_or(level.as_str(), |(_, label)| label);
                        json!({"effort": level, "label": label})
                    })
                    .collect();
                entry["reasoning_efforts"] = Value::Array(levels);
            }
            entry
        })
        .collect();
    json!({"object": "list", "data": data})
}

fn chunk(delta: Value, finish: Value) -> Value {
    json!({
        "object": "chat.completion.chunk",
        "model": MODEL,
        "choices": [{"index": 0, "delta": delta, "finish_reason": finish}],
    })
}

/// `text` as one event of the streamed reply.
fn event(text: &str) -> String {
    let delta = json!({"role": "assistant", "content": text});
    format!("data: {}\n\n", chunk(delta, Value::Null))
}

/// The text of a request's newest message, when it is the user's. Claude
/// Code keeps the conversation itself, so the rest is not sent again.
fn newest(request: &Value) -> Option<String> {
    let last = request["messages"].as_array()?.last()?;
    if last["role"] != "user" {
        return None;
    }
    let text = text_of(&last["content"])?;
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

/// The words of a request's system prompt.
fn system_text(request: &Value) -> Option<String> {
    let system = request["messages"]
        .as_array()?
        .iter()
        .find(|message| message["role"] == "system")?;
    text_of(&system["content"])
}

/// The project folder Letronna's system prompt names for the session the
/// request comes from, so a turn from the terminal goes to that session's
/// folder rather than the window's.
fn project_folder(request: &Value) -> Option<PathBuf> {
    const SAID: &str = "The project folder is `";
    let text = system_text(request)?;
    let start = text.find(SAID)? + SAID.len();
    let end = text[start..].find('`')?;
    Some(PathBuf::from(&text[start..start + end]))
}

/// A message's words, whether given as one string or as parts.
fn text_of(content: &Value) -> Option<String> {
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => Some(
            parts
                .iter()
                .filter_map(|part| part["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// Point the Custom gateway at the model endpoint while it runs, and take it
/// back once it stops; one the user set to another address is left alone.
/// False when the window is not open yet.
fn wire(hub: &Hub, cx: &mut App) -> bool {
    let Some(chat) = cx
        .try_global::<crate::ui::quick::MainChat>()
        .map(|main| main.0.clone())
    else {
        return false;
    };
    let Some(window) = cx.windows().into_iter().next() else {
        return false;
    };
    let wanted = hub
        .lock()
        .model
        .as_ref()
        .map(|model| (model.url(), model.key.clone()));
    let wired = window.update(cx, |_, window, cx| {
        chat.update(cx, |chat, cx| {
            // A model list already loading would miss the change, and no
            // second load starts over it: try again on a later tick.
            (!chat.loading_models).then(|| point(chat, wanted, window, cx))
        })
    });
    let Ok(Some(unlisted)) = wired else {
        return false;
    };
    hub.lock().unlisted = unlisted;
    hub.changed.store(true, Ordering::Relaxed);
    true
}

/// Set the Custom gateway to `wanted`, address and key, or clear it when it
/// is the plugin's and `wanted` is None; why it was left alone, if it was.
fn point(
    chat: &mut Chat,
    wanted: Option<(String, String)>,
    window: &mut Window,
    cx: &mut Context<Chat>,
) -> Option<String> {
    let now = chat.store.settings.custom_endpoint.clone();
    let (endpoint, key) = match wanted {
        Some((url, key)) if now.is_empty() || is_ours(&now) => (url, Some(key)),
        Some(_) => {
            return Some(format!(
                "The {CUSTOM} gateway has another address, so Claude Code is not in the model list. Remove that one in Settings, then turn this plugin off and on."
            ));
        }
        None if is_ours(&now) => (String::new(), None),
        None => return None,
    };
    let slot = key_slot(CUSTOM);
    if now == endpoint && chat.keys.get(slot).map(String::as_str) == key.as_deref() {
        return None;
    }
    match save_key(slot, key.as_deref()) {
        Ok(keys) => chat.keys = keys,
        Err(e) => return Some(format!("Could not save the {CUSTOM} gateway's key: {e}.")),
    }
    chat.store.settings.custom_endpoint = endpoint;
    chat.save();
    chat.load_models(window, cx);
    None
}

/// Whether the Custom gateway's address is the model endpoint's, on whichever
/// port it had.
fn is_ours(url: &str) -> bool {
    url.starts_with("http://127.0.0.1:") && url.ends_with(MODEL_PATH)
}

/// The `port.json` path this binary was started with as a hook, if it was.
pub(crate) fn hook_arg() -> Option<PathBuf> {
    let mut args = std::env::args_os();
    args.find(|arg| arg == HOOK_FLAG)?;
    args.next().map(PathBuf::from)
}

/// Run as Claude Code's hook: report what it was handed to Letronna, and
/// for a permission, wait for the user's answer in the chat and hand it to
/// Claude Code. Always exits 0: without an answer Claude Code goes on as it
/// would, asking in its own session.
pub(crate) fn hook(port_file: &Path) -> i32 {
    let mut input = String::new();
    if std::io::stdin().read_to_string(&mut input).is_err() {
        return 0;
    }
    let Ok(hook) = serde_json::from_str::<Value>(&input) else {
        return 0;
    };
    let Some(mut answer) = report(port_file, &hook) else {
        return 0;
    };
    if hook["hook_event_name"] == "PermissionRequest"
        && let Some(decision) =
            read(&mut answer).filter(|decision| decision["behavior"].is_string())
    {
        let said = json!({"hookSpecificOutput": {"hookEventName": "PermissionRequest", "decision": decision}});
        println!("{said}");
    }
    0
}

/// Connect, prove each other, and report `hook`; the connection, for what
/// Letronna answers, when it got the report.
fn report(port_file: &Path, hook: &Value) -> Option<BufReader<TcpStream>> {
    let (port, token) = read_link(port_file)?;
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).ok()?;
    let ours = random_hex(16).ok()?;
    let _ = stream.set_read_timeout(Some(HANDSHAKE_LIMIT));
    let mut lines = stream.try_clone().map(BufReader::new).ok()?;
    say(&stream, &json!({"nonce": ours})).ok()?;
    let proof = read(&mut lines)?;
    // Whatever answered does not know the token: tell it nothing.
    if !verify(
        &token,
        "letronna",
        &ours,
        proof["proof"].as_str().unwrap_or_default(),
    ) {
        return None;
    }
    let mine = sign(&token, "hook", proof["nonce"].as_str().unwrap_or_default());
    say(&stream, &json!({"proof": mine, "hook": hook})).ok()?;
    // An answer can take as long as the user does.
    let _ = stream.set_read_timeout(None);
    Some(lines)
}

/// What a permission request asks for, in a few words.
fn question(hook: &Value) -> String {
    let tool = hook["tool_name"].as_str().unwrap_or("a tool");
    let input = &hook["tool_input"];
    let text = |name: &str| {
        input[name]
            .as_str()
            .map(|text| text.chars().take(500).collect::<String>())
    };
    let what = match tool {
        "Bash" | "PowerShell" => {
            text("command").map(|command| format!("run `{}`", command.replace('`', "'")))
        }
        "Edit" | "MultiEdit" => text("file_path").map(|path| format!("edit {path}")),
        "Write" => text("file_path").map(|path| format!("write {path}")),
        "NotebookEdit" => text("notebook_path").map(|path| format!("edit {path}")),
        "WebFetch" => text("url").map(|url| format!("fetch {url}")),
        _ => None,
    };
    what.unwrap_or_else(|| format!("use {tool}"))
}

/// The answer to a permission question in the user's words: a plain yes
/// allows it, and anything else denies it with what they said, which
/// Claude Code goes on from.
fn decision(said: &str) -> Value {
    let word = said.trim().trim_end_matches(['.', '!']).to_lowercase();
    let yes = [
        "allow", "allow it", "yes", "y", "ok", "okay", "sure", "go ahead", "do it", "approve",
        "trust", "trust it",
    ];
    let no = ["deny", "no", "n", "don't", "dont", "stop"];
    if yes.contains(&word.as_str()) {
        json!({"behavior": "allow"})
    } else if no.contains(&word.as_str()) {
        json!({"behavior": "deny", "message": "The user said no."})
    } else {
        json!({"behavior": "deny", "message": format!("The user did not allow it, and said: {}", said.trim())})
    }
}

/// What a turn's end hands back: its last message, or why it stopped.
fn answer(hook: &Value) -> String {
    let said = hook["last_assistant_message"]
        .as_str()
        .unwrap_or_default()
        .trim();
    if hook["hook_event_name"] == "StopFailure" {
        let error = hook["error"].as_str().unwrap_or("unknown");
        let why = format!("Claude Code stopped with an error ({error}).");
        return if said.is_empty() {
            why
        } else {
            format!("{why}\n\n{said}")
        };
    }
    if said.is_empty() {
        "Claude Code finished without saying anything.".to_owned()
    } else {
        said.to_owned()
    }
}

/// Claude Code's settings for Letronna: hooks that report a session's start
/// and end, each turn's end, and a question for the user, and one that waits
/// for the user's answer to a permission.
fn hooks(command: &str) -> Value {
    let hook =
        |timeout| json!([{"hooks": [{"type": "command", "command": command, "timeout": timeout}]}]);
    let report = hook(HOOK_SECS);
    json!({
        "hooks": {
            "PermissionRequest": hook(QUESTION_SECS),
            "SessionStart": report,
            "Stop": report,
            "StopFailure": report,
            "Notification": report,
            "SessionEnd": report,
        }
    })
}

/// The line Claude Code's shell runs for each hook.
fn hook_command(exe: &Path, port_file: &Path) -> String {
    format!("{} {HOOK_FLAG} {}", quoted(exe), quoted(port_file))
}

/// `text` as one word to a POSIX shell, which runs Claude Code's hooks and
/// the start script (Git Bash on Windows, which takes forward slashes).
fn quoted(text: impl AsRef<std::ffi::OsStr>) -> String {
    let text = Path::new(text.as_ref()).display().to_string();
    let text = if cfg!(windows) {
        text.replace('\\', "/")
    } else {
        text
    };
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// The script that runs `claude` with `args` in `folder`.
fn script(folder: &Path, args: &[&str]) -> String {
    let words: Vec<String> = args.iter().map(quoted).collect();
    format!(
        "#!/bin/sh\ncd {} || exit 1\ncommand -v claude >/dev/null || {{ echo 'Claude Code is not installed: see https://claude.com/claude-code'; exit 1; }}\nexec claude {}\n",
        quoted(folder),
        words.join(" "),
    )
}

/// Open the user's own terminal in `folder`, running `claude` with `args`.
fn launch(dir: &Path, folder: &Path, args: &[&str]) -> Result<(), String> {
    let tries = terminals(dir, folder, args)?;
    let mut tried = Vec::new();
    for (program, args) in tries {
        let mut command = Command::new(&program);
        command.args(&args).current_dir(folder);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NEW_CONSOLE: u32 = 0x0000_0010;
            if program == "powershell.exe" {
                command.creation_flags(CREATE_NEW_CONSOLE);
            }
        }
        if let Ok(mut child) = command.spawn() {
            // Reaped on a thread of its own, so an exited launcher is not
            // left behind until Letronna quits.
            std::thread::spawn(move || child.wait());
            return Ok(());
        }
        tried.push(program);
    }
    Err(format!(
        "Cannot open a terminal: {} did not start.",
        tried.join(" or ")
    ))
}

/// The terminals to try, as program and arguments.
fn terminals(
    dir: &Path,
    folder: &Path,
    args: &[&str],
) -> Result<Vec<(String, Vec<String>)>, String> {
    if cfg!(windows) {
        let mut wt = vec![
            "-d".to_owned(),
            folder.display().to_string(),
            "claude".to_owned(),
        ];
        wt.extend(args.iter().map(|arg| arg.to_string()));
        // PowerShell's own quotes: a ' inside is written twice.
        let words: Vec<String> = args
            .iter()
            .map(|arg| format!("'{}'", arg.replace('\'', "''")))
            .collect();
        let powershell = vec![
            "-NoExit".to_owned(),
            "-Command".to_owned(),
            format!("& claude {}", words.join(" ")),
        ];
        return Ok(vec![
            ("wt.exe".to_owned(), wt),
            ("powershell.exe".to_owned(), powershell),
        ]);
    }
    // Terminal.app runs a `.command` file in a login shell, so `claude` is
    // found as the user's own terminal finds it.
    let name = if cfg!(target_os = "macos") {
        "start.command"
    } else {
        "start.sh"
    };
    let path = dir.join(name);
    std::fs::write(&path, script(folder, args))
        .map_err(|e| format!("Cannot write {}: {e}.", path.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| format!("Cannot write {}: {e}.", path.display()))?;
    }
    let path = path.display().to_string();
    if cfg!(target_os = "macos") {
        return Ok(vec![(
            "open".to_owned(),
            vec!["-a".to_owned(), "Terminal".to_owned(), path],
        )]);
    }
    let mut found = Vec::new();
    if let Ok(terminal) = std::env::var("TERMINAL") {
        let mut words = terminal.split_whitespace().map(str::to_owned);
        if let Some(program) = words.next() {
            let mut args: Vec<String> = words.collect();
            args.extend(["-e".to_owned(), path.clone()]);
            found.push((program, args));
        }
    }
    // Debian's and Ubuntu's name for whichever terminal is the default.
    found.push((
        "x-terminal-emulator".to_owned(),
        vec!["-e".to_owned(), path],
    ));
    Ok(found)
}

/// The folder of the session open in the main window, if it has one.
fn chat_folder(cx: &App) -> Option<PathBuf> {
    let chat = cx.try_global::<crate::ui::quick::MainChat>()?.0.read(cx);
    chat.store
        .conversations
        .get(chat.store.active)?
        .workspace
        .clone()
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Whether two paths name one folder, through links such as macOS's /tmp.
fn same_folder(a: &Path, b: &Path) -> bool {
    a == b
        || matches!(
            (a.canonicalize(), b.canonicalize()),
            (Ok(a), Ok(b)) if a == b
        )
}

/// Write a file only its owner can read.
fn write_private(path: &Path, text: &str) -> Result<(), String> {
    // A file kept from before keeps its old permissions when opened.
    let _ = std::fs::remove_file(path);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options
        .open(path)
        .and_then(|mut file| file.write_all(text.as_bytes()))
        .map_err(|e| format!("Cannot write {}: {e}.", path.display()))
}

fn read_link(port_file: &Path) -> Option<(u16, String)> {
    let link: Value = serde_json::from_str(&std::fs::read_to_string(port_file).ok()?).ok()?;
    let port = u16::try_from(link["port"].as_u64()?).ok()?;
    Some((port, link["token"].as_str()?.to_owned()))
}

/// The next line from the other side as JSON, or None when it is gone.
fn read(lines: &mut impl BufRead) -> Option<Value> {
    let mut line = String::new();
    match lines.read_line(&mut line) {
        Ok(0) | Err(_) => None,
        Ok(_) => serde_json::from_str(&line).ok(),
    }
}

fn say(mut stream: &TcpStream, value: &Value) -> std::io::Result<()> {
    stream.write_all(format!("{value}\n").as_bytes())
}

/// The proof that `who` knows `token`, over the other side's `nonce`. Who
/// is part of it, so a proof cannot be sent back to the side that made it.
fn sign(token: &str, who: &str, nonce: &str) -> String {
    let key = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
    hex(hmac::sign(&key, format!("{who} {nonce}").as_bytes()).as_ref())
}

fn verify(token: &str, who: &str, nonce: &str, proof: &str) -> bool {
    let key = hmac::Key::new(hmac::HMAC_SHA256, token.as_bytes());
    unhex(proof)
        .is_some_and(|tag| hmac::verify(&key, format!("{who} {nonce}").as_bytes(), &tag).is_ok())
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let mut buf = vec![0u8; bytes];
    ring::rand::SystemRandom::new()
        .fill(&mut buf)
        .map_err(|_| {
            "This computer would not give a random number for Claude Code's link.".to_owned()
        })?;
    Ok(hex(&buf))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(text.get(at..at + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{
        Ask, Hub, MODEL_PATH, Mode, PORT_FILE, SESSIONS_KEPT, Seen, Session, Taken, accept, answer,
        backgrounded, catalog, chat_key, claude_effort, claude_mode, decision, earlier, hooks,
        is_ours, key_of, launch_args, leave_off, listen, load_chats, mode_for, model_port, models,
        newest, plain, project_folder, question, read, read_link, report, say, serving, sign, slug,
        trust_in, undecided_servers, verify, work_in, write_private,
    };
    #[cfg(unix)]
    use super::{Offered, TRUST_ASKED, offered, quoted, script};
    use serde_json::{Value, json};
    use std::io::{BufReader, Read, Write};
    use std::net::{Ipv4Addr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "letronna-claude-code-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A hub listening in `dir`, as `start` sets one up, with no Claude
    /// Code to ask.
    fn hub(dir: &Path) -> Arc<Hub> {
        let hub = hub_running(dir, Path::new("/bin/letronna"));
        hub.lock().claude = Some(dir.join("no-claude"));
        hub
    }

    /// A hub listening in `dir` whose hooks run `exe`.
    fn hub_running(dir: &Path, exe: &Path) -> Arc<Hub> {
        let hub = Arc::new(Hub::default());
        let (listener, link) = listen(dir, exe).unwrap();
        let (endpoint, model) = serving(dir, 0).unwrap();
        let mut state = hub.lock();
        state.run = 1;
        state.link = Some(link);
        state.model = Some(model);
        drop(state);
        let hooks = hub.clone();
        std::thread::spawn(move || accept(hooks, listener, 1, Hub::meet));
        let turns = hub.clone();
        std::thread::spawn(move || accept(turns, endpoint, 1, Hub::serve));
        hub
    }

    impl Hub {
        /// `ask` with its parts spelled out, Claude Code's questions left
        /// to Claude Code.
        fn ask_with(
            &self,
            key: Option<&str>,
            folder: &Path,
            mode: Mode,
            (model, effort): (Option<&str>, Option<&str>),
            (earlier, message): (&str, &str),
            pause: impl FnMut(bool) -> bool,
        ) -> Result<(String, Session), String> {
            let asked = Ask {
                key,
                folder,
                mode,
                model,
                effort,
                earlier,
                message,
                questions: false,
            };
            self.ask(asked, pause)
        }
    }

    /// Reports `hook` as the hook does; whether Letronna took it.
    fn sent(port_file: &Path, hook: &Value) -> bool {
        report(port_file, hook).is_some()
    }

    fn hook_input(id: &str, event: &str, said: &str) -> Value {
        // A transcript that is there, as Claude Code's are.
        let transcript = std::env::temp_dir().join(format!("letronna-claude-code-{id}.jsonl"));
        std::fs::write(&transcript, "").unwrap();
        json!({
            "session_id": id,
            "hook_event_name": event,
            "cwd": "/work/site",
            "transcript_path": transcript.display().to_string(),
            "last_assistant_message": said,
        })
    }

    fn until(mut ready: impl FnMut() -> bool) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn a_turns_end_reported_by_a_hook_is_its_sessions_answer() {
        let dir = scratch("report");
        let hub = hub(&dir);
        let port_file = dir.join(PORT_FILE);
        assert!(sent(
            &port_file,
            &hook_input("s1-long-id", "SessionStart", "")
        ));
        until(|| hub.started("s1-long") == Some("s1-long-id".into()));
        assert_eq!(hub.take("s1-long-id"), Taken::Waiting { asking: false });
        let asking = json!({"session_id": "s1-long-id", "hook_event_name": "Notification", "notification_type": "permission_prompt"});
        assert!(sent(&port_file, &asking));
        until(|| hub.take("s1-long-id") == Taken::Waiting { asking: true });
        assert!(sent(
            &port_file,
            &hook_input("s1-long-id", "Stop", "  All 12 pass.  ")
        ));
        until(|| hub.take("s1-long-id") == Taken::Answer("All 12 pass.".into()));
        assert!(sent(
            &port_file,
            &hook_input("s1-long-id", "SessionEnd", "")
        ));
        until(|| hub.take("s1-long-id") == Taken::Gone);
        hub.stop();
        assert!(!dir.join(PORT_FILE).exists());
    }

    #[test]
    fn what_a_session_says_before_it_is_seen_to_start_is_not_taken() {
        let dir = scratch("late");
        let hub = hub(&dir);
        let port_file = dir.join(PORT_FILE);
        // A stopped run's end, arriving after the next run was asked for.
        assert!(sent(&port_file, &hook_input("s1", "Stop", "old answer")));
        assert!(sent(&port_file, &hook_input("s1", "SessionEnd", "")));
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(hub.take("s1"), Taken::Waiting { asking: false });
        assert!(hub.lock().live.is_empty());
        hub.stop();
    }

    #[test]
    fn a_session_a_turn_waits_on_is_heard_even_when_its_start_was_not() {
        let dir = scratch("no-start");
        let hub = hub(&dir);
        let port_file = dir.join(PORT_FILE);
        let waiting = super::Expecting::new(&hub, "abcd1234");
        // A run stopped before ends late; that is not taken in.
        assert!(sent(
            &port_file,
            &hook_input("abcd1234-full", "SessionEnd", "")
        ));
        std::thread::sleep(Duration::from_millis(100));
        assert!(hub.lock().live.is_empty());
        assert!(sent(
            &port_file,
            &hook_input("abcd1234-full", "Stop", "Done.")
        ));
        until(|| hub.take("abcd1234-full") == Taken::Answer("Done.".into()));
        assert!(hub.lock().live["abcd1234-full"].transcript.is_file());
        drop(waiting);
        assert!(hub.lock().expecting.is_empty());
        hub.stop();
    }

    #[test]
    fn a_hook_tells_nothing_to_a_listener_that_does_not_know_the_token() {
        let dir = scratch("squatter");
        let squatter = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = squatter.local_addr().unwrap().port();
        write_private(
            &dir.join(PORT_FILE),
            &json!({"port": port, "token": "the-real-token"}).to_string(),
        )
        .unwrap();
        let heard = std::thread::spawn(move || {
            let (stream, _) = squatter.accept().unwrap();
            let mut lines = BufReader::new(stream.try_clone().unwrap());
            let hello = read(&mut lines).unwrap();
            let nonce = hello["nonce"].as_str().unwrap();
            let guess = sign("a-guess", "letronna", nonce);
            say(&stream, &json!({"proof": guess, "nonce": "00"})).unwrap();
            read(&mut lines)
        });
        let hook = hook_input("s1", "Stop", "a private answer");
        assert!(!sent(&dir.join(PORT_FILE), &hook));
        assert_eq!(heard.join().unwrap(), None);
    }

    #[test]
    fn letronna_takes_no_report_from_a_hook_that_does_not_know_the_token() {
        let dir = scratch("stranger");
        let hub = hub(&dir);
        let (port, _) = read_link(&dir.join(PORT_FILE)).unwrap();
        let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        let mut lines = BufReader::new(stream.try_clone().unwrap());
        say(&stream, &json!({"nonce": "ab"})).unwrap();
        let proof = read(&mut lines).unwrap();
        let guess = sign("a-guess", "hook", proof["nonce"].as_str().unwrap());
        let report = json!({"proof": guess, "hook": hook_input("s1", "SessionStart", "")});
        say(&stream, &report).unwrap();
        assert_eq!(read(&mut lines), None);
        assert!(hub.lock().live.is_empty());
        hub.stop();
    }

    #[test]
    fn a_proof_cannot_be_sent_back_to_the_side_that_made_it() {
        let proof = sign("token", "letronna", "n1");
        assert!(verify("token", "letronna", "n1", &proof));
        assert!(!verify("token", "hook", "n1", &proof));
        assert!(!verify("other", "letronna", "n1", &proof));
        assert!(!verify("token", "letronna", "n1", "zz"));
    }

    #[test]
    fn the_hooks_report_and_let_claude_code_go_on() {
        let settings = hooks("letronna --claude-code-hook port.json");
        for event in [
            "PermissionRequest",
            "SessionStart",
            "Stop",
            "StopFailure",
            "Notification",
            "SessionEnd",
        ] {
            let hook = &settings["hooks"][event][0]["hooks"][0];
            assert_eq!(
                hook["command"], "letronna --claude-code-hook port.json",
                "{event}"
            );
            assert_eq!(hook["asyncRewake"], Value::Null, "{event}");
        }
    }

    #[test]
    fn a_failed_turn_says_why_before_what_it_said() {
        let hook = json!({"hook_event_name": "StopFailure", "error": "rate_limit", "last_assistant_message": "Half done."});
        assert_eq!(
            answer(&hook),
            "Claude Code stopped with an error (rate_limit).\n\nHalf done."
        );
        let quiet = json!({"hook_event_name": "Stop", "last_assistant_message": ""});
        assert_eq!(
            answer(&quiet),
            "Claude Code finished without saying anything."
        );
    }

    fn session(mode: &str) -> Session {
        Session {
            id: "59638618-70a5-4f31-946d-c4296dff4e1d".into(),
            mode: mode.into(),
            folder: "/work/site".into(),
            ..Session::default()
        }
    }

    #[test]
    fn a_session_resumes_as_it_was_and_other_options_make_a_copy_with_the_settings() {
        let settings = Path::new("/data/settings.json");
        let wanted = Session {
            model: "opus".into(),
            effort: "high".into(),
            ..session("acceptEdits")
        };
        let fresh = launch_args(None, &wanted, settings, "-2 plus 10?");
        assert_eq!(
            fresh,
            [
                "--bg",
                "--settings",
                "/data/settings.json",
                "--permission-mode",
                "acceptEdits",
                "--model",
                "opus",
                "--effort",
                "high",
                "--",
                "-2 plus 10?"
            ]
        );
        let same = launch_args(Some(&wanted), &wanted, settings, "go on");
        assert_eq!(
            same,
            [
                "--bg",
                "--resume",
                "59638618-70a5-4f31-946d-c4296dff4e1d",
                "--",
                "go on"
            ]
        );
        for other in [
            Session {
                mode: "plan".into(),
                ..wanted.clone()
            },
            Session {
                model: "sonnet".into(),
                ..wanted.clone()
            },
            Session {
                effort: String::new(),
                ..wanted.clone()
            },
        ] {
            let changed = launch_args(Some(&wanted), &other, settings, "go on");
            assert!(changed.windows(2).any(|pair| pair[0] == "--resume"));
            assert!(
                changed
                    .windows(2)
                    .any(|pair| pair == ["--settings", "/data/settings.json"])
            );
            assert!(
                changed
                    .windows(2)
                    .any(|pair| pair == ["--permission-mode", other.mode.as_str()])
            );
            assert_eq!(
                changed.contains(&"--effort".to_owned()),
                !other.effort.is_empty()
            );
        }
    }

    /// What Claude Code's handshake listed, trimmed to what is read.
    fn listed() -> Value {
        let levels = json!(["low", "medium", "high", "xhigh", "max"]);
        json!([
            {"value": "default", "displayName": "Default (recommended)", "supportedEffortLevels": levels},
            {"value": "opus", "displayName": "Opus", "supportedEffortLevels": levels},
            {"value": "claude-fable-5-1[1m]", "displayName": "Fable", "supportedEffortLevels": ["low", "high"]},
            {"value": "haiku", "displayName": "Haiku"},
        ])
    }

    #[test]
    fn the_picker_offers_the_models_claude_code_lists_with_their_effort_levels() {
        let offered = catalog(listed().as_array().unwrap());
        let named: Vec<(&str, &str)> = offered
            .iter()
            .map(|model| (model.id.as_str(), model.flag.as_str()))
            .collect();
        assert_eq!(
            named,
            [
                ("claude-code", ""),
                ("claude-code-opus", "opus"),
                ("claude-code-fable", "claude-fable-5-1[1m]"),
                ("claude-code-haiku", "haiku"),
            ]
        );
        assert_eq!(
            offered[0].efforts.last().map(String::as_str),
            Some("ultracode")
        );
        assert_eq!(offered[2].efforts, ["low", "high"]);
        assert!(offered[3].efforts.is_empty());
        let list = models(&offered);
        assert_eq!(
            list["data"][1]["reasoning_efforts"][5],
            json!({"effort": "ultracode", "label": "Ultracode"})
        );
        assert_eq!(
            list["data"][1]["reasoning_efforts"][3]["label"],
            "Extra high"
        );
        assert_eq!(list["data"][3]["capabilities"]["reasoning"], false);
        assert_eq!(
            claude_effort(&json!({"reasoning_effort": "ultracode"})),
            "ultracode"
        );
        assert_eq!(claude_effort(&json!({"reasoning_effort": "turbo"})), "");
        assert_eq!(slug("Default (recommended)"), "default-recommended");
    }

    #[cfg(unix)]
    #[test]
    fn claude_code_is_asked_which_models_it_offers_without_a_prompt() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("offered");
        let program = dir.join("claude");
        let answer = json!({"type": "control_response", "response": {"subtype": "success", "request_id": "models", "response": {"models": listed()}}});
        let script = format!(
            "#!/bin/sh\nprintf '%s\\n' \"$*\" > '{args}'\nread hello\nprintf '%s\\n' \"$hello\" > '{hello}'\necho '{{\"type\":\"system\"}}'\necho '{answer}'\n",
            args = dir.join("args").display(),
            hello = dir.join("hello").display(),
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        let offered = offered(&program).unwrap();
        assert_eq!(offered.len(), 4);
        let args = std::fs::read_to_string(dir.join("args")).unwrap();
        assert!(args.starts_with("-p --input-format stream-json"), "{args}");
        let hello: Value =
            serde_json::from_str(&std::fs::read_to_string(dir.join("hello")).unwrap()).unwrap();
        assert_eq!(hello["request"]["subtype"], "initialize");
        assert!(super::offered(&dir.join("no-claude")).is_none());
    }

    #[test]
    fn the_short_id_is_read_from_what_claude_code_printed() {
        assert_eq!(
            backgrounded("backgrounded · 59638618 · math question answering\n").unwrap(),
            "59638618"
        );
        let copy = "note: background session 59638618 keeps its own saved options, so the flags you passed started a copy as ca836cee.\nbackgrounded · ca836cee\n";
        assert_eq!(backgrounded(copy).unwrap(), "ca836cee");
        let refused = backgrounded(
            "Workspace not trusted. Run `claude` in /work once and accept the trust prompt, then retry.\n",
        );
        assert!(refused.unwrap_err().contains("Workspace not trusted"));
    }

    #[test]
    fn every_letronna_mode_is_one_claude_code_knows() {
        let modes: Vec<&str> = Mode::OFFERED.into_iter().map(claude_mode).collect();
        assert_eq!(
            modes,
            [
                "default",
                "acceptEdits",
                "plan",
                "auto",
                "dontAsk",
                "bypassPermissions"
            ]
        );
    }

    #[test]
    fn a_turn_runs_in_its_own_chats_mode_or_plan_when_its_prompt_says_so() {
        let seen = Seen {
            conversation: 7,
            folder: Some("/work/site".into()),
            mode: Mode::Bypass,
            default: Mode::AcceptEdits,
            // Another chat in the same folder, running at the same time.
            chats: vec![(
                "fix the build".into(),
                Some("/work/site".into()),
                Mode::Default,
            )],
        };
        let asked = |system: &str, first: &str| {
            json!({"messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": first},
            ]})
        };
        let here = Path::new("/work/site");
        assert_eq!(
            mode_for(&asked("You are Letronna.", "rename the tests"), here, &seen),
            Mode::Bypass
        );
        assert_eq!(
            mode_for(&asked("You are Letronna.", "fix the build"), here, &seen),
            Mode::Default
        );
        assert_eq!(
            mode_for(
                &asked("Plan mode is on: you can read.", "fix the build"),
                here,
                &seen
            ),
            Mode::Plan
        );
        let elsewhere = Path::new("/work/other");
        assert_eq!(
            mode_for(
                &asked("You are Letronna.", "fix the build"),
                elsewhere,
                &seen
            ),
            Mode::AcceptEdits
        );
    }

    #[test]
    fn a_chat_is_known_by_its_first_exchange_from_its_second_message() {
        let first = json!({"messages": [
            {"role": "system", "content": "You are Letronna."},
            {"role": "user", "content": "rename the tests"},
        ]});
        assert_eq!(chat_key(&first), None);
        let second = json!({"messages": [
            {"role": "system", "content": "You are Letronna."},
            {"role": "user", "content": "rename the tests"},
            {"role": "assistant", "content": "Renamed  all 4.\n"},
            {"role": "user", "content": "now run them"},
        ]});
        let key = key_of("rename the tests", "Renamed all 4.");
        assert_eq!(chat_key(&second), Some(key.clone()));
        assert_ne!(key_of("rename the tests", "Renamed all 5."), key);
    }

    #[test]
    fn the_chats_sessions_are_kept_on_disk_newest_last_and_bounded() {
        let dir = scratch("kept");
        let hub = hub(&dir);
        for at in 0..SESSIONS_KEPT + 3 {
            hub.bind(format!("chat {at}"), session("plan"));
        }
        hub.bind("chat 5".into(), session("default"));
        let kept = load_chats(&dir);
        assert_eq!(kept.len(), SESSIONS_KEPT);
        assert_eq!(kept.last().unwrap().0, "chat 5");
        assert_eq!(kept.last().unwrap().1.mode, "default");
        assert!(kept.iter().all(|(chat, _)| chat != "chat 0"));
        hub.stop();
    }

    /// Sends `request` to the hub's model endpoint and reads the whole reply.
    fn call(hub: &Hub, request: &str) -> String {
        let port = hub.lock().model.as_ref().unwrap().port;
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream
            .write_all(request.replace("PORT", &port.to_string()).as_bytes())
            .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    fn key(hub: &Hub) -> String {
        hub.lock().model.as_ref().unwrap().key.clone()
    }

    fn post(key: &str, body: &Value) -> String {
        let body = body.to_string();
        format!(
            "POST {MODEL_PATH}/chat/completions HTTP/1.1\r\nHost: 127.0.0.1:PORT\r\nAuthorization: Bearer {key}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        )
    }

    #[test]
    fn the_model_endpoint_answers_only_letronnas_own_calls() {
        let dir = scratch("endpoint");
        let hub = hub(&dir);
        let key = key(&hub);
        let get = |headers: &str| {
            call(
                &hub,
                &format!("GET {MODEL_PATH}/models HTTP/1.1\r\n{headers}\r\n"),
            )
        };
        let listed = get(&format!(
            "Host: 127.0.0.1:PORT\r\nAuthorization: Bearer {key}\r\n"
        ));
        assert!(listed.starts_with("HTTP/1.1 200"), "{listed}");
        assert!(listed.contains("\"id\":\"claude-code\""), "{listed}");
        let keyless = get("Host: 127.0.0.1:PORT\r\n");
        assert!(keyless.starts_with("HTTP/1.1 401"), "{keyless}");
        let page = get(&format!(
            "Host: 127.0.0.1:PORT\r\nOrigin: https://example.com\r\nAuthorization: Bearer {key}\r\n"
        ));
        assert!(page.starts_with("HTTP/1.1 403"), "{page}");
        let rebound = get(&format!(
            "Host: attacker.example:PORT\r\nAuthorization: Bearer {key}\r\n"
        ));
        assert!(rebound.starts_with("HTTP/1.1 403"), "{rebound}");
        hub.stop();
    }

    #[test]
    fn a_call_that_does_not_stream_is_not_passed_to_claude_code() {
        let dir = scratch("compaction");
        let hub = hub(&dir);
        let body = json!({
            "stream": false,
            "messages": [{"role": "user", "content": "Summarise the conversation."}],
        });
        let reply = call(&hub, &post(&key(&hub), &body));
        assert!(reply.starts_with("HTTP/1.1 400"), "{reply}");
        assert!(reply.contains("chat turns only"), "{reply}");
        hub.stop();
    }

    /// A stand-in `claude` that notes each command line and, for `--bg`,
    /// says it backgrounded a session numbered in turn, 00000001 first;
    /// with a `trusted` file beside it, only in a folder that file names.
    #[cfg(unix)]
    fn fake_claude(dir: &Path) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let program = dir.join("claude");
        let script = format!(
            "#!/bin/sh\nhere=$(pwd -P)\ncd '{dir}' || exit 1\nif [ \"$1\" = --bg ]; then\n  if [ -f trusted ] && ! grep -qF \"\\\"$here\\\"\" trusted; then echo \"Workspace not trusted. Run \\`claude\\` in $here once and accept the trust prompt, then retry.\"; exit 1; fi\n  while ! mkdir lock 2>/dev/null; do sleep 0.01; done; n=$(( $(cat count 2>/dev/null || echo 0) + 1 )); echo $n > count; rmdir lock\n  short=$(printf %08d $n)\n  printf '%s %s\\n' \"$short\" \"$(printf '%s' \"$*\" | tr '\\n' ' ')\" >> calls.txt\n  echo \"backgrounded · $short\"\nelse\n  printf '%s\\n' \"$*\" >> calls.txt\nfi\n",
            dir = dir.display()
        );
        std::fs::write(&program, script).unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755)).unwrap();
        program
    }

    /// The sessions the stand-in started, as their short id and arguments.
    #[cfg(unix)]
    fn started(dir: &Path) -> Vec<(String, String)> {
        std::fs::read_to_string(dir.join("calls.txt"))
            .unwrap_or_default()
            .lines()
            .filter(|line| line.contains(" --bg "))
            .filter_map(|line| line.split_once(' '))
            .map(|(short, args)| (short.to_owned(), args.to_owned()))
            .collect()
    }

    /// A hub whose `claude` is the stand-in, with `folder` on screen.
    #[cfg(unix)]
    fn faked(dir: &Path, folder: &Path, mode: Mode) -> Arc<Hub> {
        let hub = hub(dir);
        hub.lock().claude = Some(fake_claude(dir));
        hub.lock().catalog = ["", "opus", "sonnet"]
            .into_iter()
            .map(|flag| Offered {
                id: if flag.is_empty() {
                    "claude-code".into()
                } else {
                    format!("claude-code-{flag}")
                },
                flag: flag.into(),
                efforts: vec!["high".into()],
            })
            .collect();
        hub.lock().seen = Seen {
            conversation: 7,
            folder: Some(folder.to_owned()),
            mode,
            default: Mode::Default,
            chats: Vec::new(),
        };
        hub
    }

    /// Sends a streamed turn on `model` with `messages` and `effort`.
    #[cfg(unix)]
    fn turn_on(
        hub: &Arc<Hub>,
        model: &str,
        effort: &str,
        messages: Value,
    ) -> std::thread::JoinHandle<String> {
        let body = json!({"model": model, "stream": true, "reasoning_effort": effort, "messages": messages});
        let request = post(&key(hub), &body);
        let hub = hub.clone();
        std::thread::spawn(move || call(&hub, &request))
    }

    /// Claude Code session `short` starts, works and ends its turn with
    /// `said`, as its hooks report it.
    #[cfg(unix)]
    fn answers(dir: &Path, short: &str, said: &str) {
        let port_file = dir.join(PORT_FILE);
        let id = format!("{short}-0000-full-id");
        assert!(sent(&port_file, &hook_input(&id, "SessionStart", "")));
        assert!(sent(&port_file, &hook_input(&id, "Stop", said)));
    }

    #[cfg(unix)]
    #[test]
    fn a_new_chat_starts_a_session_and_its_next_message_resumes_it_with_its_options() {
        let dir = scratch("model-turn");
        let folder = dir.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let hub = faked(&dir, &folder, Mode::AcceptEdits);
        let system = format!(
            "You are Letronna. The project folder is `{}`.",
            folder.display()
        );
        let first = turn_on(
            &hub,
            "claude-code-opus",
            "high",
            json!([
                {"role": "system", "content": system},
                {"role": "user", "content": "rename the tests"},
            ]),
        );
        until(|| started(&dir).len() == 1);
        answers(&dir, "00000001", "Renamed all 4.");
        let reply = first.join().unwrap();
        assert!(reply.contains("\"content\":\"Renamed all 4.\""), "{reply}");
        assert!(reply.trim_end().ends_with("data: [DONE]"), "{reply}");
        let (_, args) = &started(&dir)[0];
        assert!(
            args.contains(
                "--permission-mode acceptEdits --model opus --effort high -- rename the tests"
            ),
            "{args}"
        );
        let calls = std::fs::read_to_string(dir.join("calls.txt")).unwrap();
        assert!(calls.contains("stop 00000001"), "{calls}");
        let chat = |model: &str| {
            turn_on(
                &hub,
                model,
                "high",
                json!([
                    {"role": "system", "content": system},
                    {"role": "user", "content": "rename the tests"},
                    {"role": "assistant", "content": "Renamed all 4."},
                    {"role": "user", "content": "now run them"},
                ]),
            )
        };
        // The same options: the same session goes on.
        let second = chat("claude-code-opus");
        until(|| started(&dir).len() == 2);
        answers(&dir, "00000002", "All 4 pass.");
        assert!(second.join().unwrap().contains("All 4 pass."));
        let (_, args) = &started(&dir)[1];
        assert_eq!(args, "--bg --resume 00000001-0000-full-id -- now run them");
        // Another model: Claude Code's copy, in which the chat goes on.
        let third = chat("claude-code-sonnet");
        until(|| started(&dir).len() == 3);
        answers(&dir, "00000003", "Still 4.");
        assert!(third.join().unwrap().contains("Still 4."));
        let (_, args) = &started(&dir)[2];
        assert!(
            args.starts_with("--bg --resume 00000002-0000-full-id --settings"),
            "{args}"
        );
        assert!(
            args.contains("--model sonnet --effort high -- now run them"),
            "{args}"
        );
        hub.stop();
    }

    #[cfg(unix)]
    #[test]
    fn chats_at_once_each_have_a_claude_code_of_their_own() {
        let dir = scratch("at-once");
        let folder = dir.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let hub = faked(&dir, &folder, Mode::Default);
        let system = format!(
            "You are Letronna. The project folder is `{}`.",
            folder.display()
        );
        let asked = ["fix the build", "write the changelog"];
        let turns: Vec<_> = asked
            .iter()
            .map(|said| {
                turn_on(
                    &hub,
                    "claude-code",
                    "",
                    json!([
                        {"role": "system", "content": system},
                        {"role": "user", "content": said},
                    ]),
                )
            })
            .collect();
        until(|| started(&dir).len() == 2);
        for (short, args) in started(&dir) {
            let said = args.rsplit(" -- ").next().unwrap().to_owned();
            answers(&dir, &short, &format!("done: {said}"));
        }
        for (turn, said) in turns.into_iter().zip(asked) {
            let reply = turn.join().unwrap();
            assert!(
                reply.contains(&format!("\"content\":\"done: {said}\"")),
                "{reply}"
            );
        }
        let kept = hub.lock().chats.clone();
        assert_eq!(kept.len(), 2);
        assert_ne!(kept[0].1.id, kept[1].1.id);
        hub.stop();
    }

    #[cfg(unix)]
    #[test]
    fn a_chat_whose_session_claude_code_no_longer_has_goes_on_in_a_new_one_told_the_chat_so_far() {
        let dir = scratch("gone");
        let folder = dir.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let hub = faked(&dir, &folder, Mode::AcceptEdits);
        let key = key_of("rename the tests", "Renamed all 4.");
        hub.bind(
            key.clone(),
            Session {
                id: "deadbeef-gone".into(),
                mode: "acceptEdits".into(),
                folder: folder.clone(),
                transcript: dir.join("removed.jsonl"),
                ..Session::default()
            },
        );
        let system = format!(
            "You are Letronna. The project folder is `{}`.",
            folder.display()
        );
        let turn = turn_on(
            &hub,
            "claude-code",
            "",
            json!([
                {"role": "system", "content": system},
                {"role": "user", "content": "rename the tests"},
                {"role": "assistant", "content": "Renamed all 4."},
                {"role": "user", "content": "now run them"},
            ]),
        );
        until(|| started(&dir).len() == 1);
        answers(&dir, "00000001", "All 4 pass.");
        assert!(turn.join().unwrap().contains("All 4 pass."));
        let (_, args) = &started(&dir)[0];
        assert!(!args.contains("--resume"), "{args}");
        assert!(args.contains("User: rename the tests"), "{args}");
        assert!(args.contains("Assistant: Renamed all 4."), "{args}");
        assert!(args.ends_with("---  now run them"), "{args}");
        assert_eq!(hub.session(&key).unwrap().id, "00000001-0000-full-id");
        hub.stop();
    }

    #[test]
    fn the_chat_so_far_goes_oldest_first_and_keeps_its_newest_part() {
        let first = json!({"messages": [
            {"role": "system", "content": "You are Letronna."},
            {"role": "user", "content": "hello"},
        ]});
        assert_eq!(earlier(&first), "");
        let long = "x".repeat(super::EARLIER_CHARS / 2);
        let chat = json!({"messages": [
            {"role": "system", "content": "You are Letronna."},
            {"role": "user", "content": format!("first {long}")},
            {"role": "assistant", "content": format!("second {long}")},
            {"role": "tool", "content": "42"},
            {"role": "user", "content": "third"},
            {"role": "assistant", "content": "fourth"},
            {"role": "user", "content": "the newest"},
        ]});
        let told = earlier(&chat);
        assert!(told.contains("(Earlier messages left out.)"), "{told}");
        assert!(!told.contains("first"), "{told}");
        let (second, third) = (
            told.find("Assistant: second").unwrap(),
            told.find("User: third").unwrap(),
        );
        assert!(second < third && told.contains("Assistant: fourth"));
        assert!(!told.contains("the newest") && !told.contains("42"));
    }

    #[test]
    fn a_permission_is_asked_in_a_few_words_and_answered_in_the_users() {
        let asks =
            |tool: &str, input: Value| question(&json!({"tool_name": tool, "tool_input": input}));
        assert_eq!(
            asks("Bash", json!({"command": "touch made.txt"})),
            "run `touch made.txt`"
        );
        assert_eq!(
            asks("Write", json!({"file_path": "/work/a.rs"})),
            "write /work/a.rs"
        );
        assert_eq!(
            asks("WebFetch", json!({"url": "https://example.com"})),
            "fetch https://example.com"
        );
        assert_eq!(asks("mcp__db__query", json!({})), "use mcp__db__query");
        assert_eq!(decision("Allow."), json!({"behavior": "allow"}));
        assert_eq!(decision(" yes "), json!({"behavior": "allow"}));
        assert_eq!(decision("no")["behavior"], "deny");
        let instead = decision("use git rm instead");
        assert_eq!(instead["behavior"], "deny");
        assert!(
            instead["message"]
                .as_str()
                .unwrap()
                .contains("use git rm instead")
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_permission_claude_code_asks_for_is_asked_in_the_chat_and_the_turn_goes_on() {
        let dir = scratch("question");
        let folder = dir.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let hub = faked(&dir, &folder, Mode::Default);
        let system = format!(
            "You are Letronna. The project folder is `{}`.",
            folder.display()
        );
        let port_file = dir.join(PORT_FILE);
        let id = "00000001-0000-full-id";
        let first = turn_on(
            &hub,
            "claude-code",
            "",
            json!([
                {"role": "system", "content": system},
                {"role": "user", "content": "make a file"},
            ]),
        );
        until(|| started(&dir).len() == 1);
        assert!(sent(&port_file, &hook_input(id, "SessionStart", "")));
        // The hook waits for the user's answer.
        let waiting = {
            let port_file = port_file.clone();
            std::thread::spawn(move || {
                let asks = json!({
                    "session_id": id,
                    "hook_event_name": "PermissionRequest",
                    "tool_name": "Bash",
                    "tool_input": {"command": "touch made.txt"},
                });
                let mut answer = report(&port_file, &asks).unwrap();
                read(&mut answer)
            })
        };
        let reply = first.join().unwrap();
        assert!(
            reply.contains("Claude Code wants to run `touch made.txt`"),
            "{reply}"
        );
        let calls = std::fs::read_to_string(dir.join("calls.txt")).unwrap();
        assert!(!calls.contains("stop 00000001"), "{calls}");
        let asked = reply_text(&reply);
        let second = turn_on(
            &hub,
            "claude-code",
            "",
            json!([
                {"role": "system", "content": system},
                {"role": "user", "content": "make a file"},
                {"role": "assistant", "content": asked},
                {"role": "user", "content": "allow"},
            ]),
        );
        assert_eq!(waiting.join().unwrap(), Some(json!({"behavior": "allow"})));
        assert!(sent(&port_file, &hook_input(id, "Stop", "Made made.txt.")));
        let reply = second.join().unwrap();
        assert!(reply.contains("Made made.txt."), "{reply}");
        // The same turn went on: nothing new was started for the answer.
        assert_eq!(started(&dir).len(), 1);
        let calls = std::fs::read_to_string(dir.join("calls.txt")).unwrap();
        assert!(calls.contains("stop 00000001"), "{calls}");
        hub.stop();
    }

    #[cfg(unix)]
    #[test]
    fn the_agents_hand_off_leaves_claude_codes_questions_to_claude_code() {
        let dir = scratch("agent-question");
        let folder = dir.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let hub = faked(&dir, &folder, Mode::Default);
        let port_file = dir.join(PORT_FILE);
        let asking = {
            let hub = hub.clone();
            let folder = folder.clone();
            std::thread::spawn(move || {
                let mut steps = 0;
                hub.ask_with(
                    None,
                    &folder,
                    Mode::Default,
                    (None, None),
                    ("", "make a file"),
                    |_| {
                        std::thread::sleep(Duration::from_millis(20));
                        steps += 1;
                        steps < 500
                    },
                )
            })
        };
        until(|| started(&dir).len() == 1);
        let id = "00000001-0000-full-id";
        assert!(sent(&port_file, &hook_input(id, "SessionStart", "")));
        let asks = json!({"session_id": id, "hook_event_name": "PermissionRequest", "tool_name": "Bash", "tool_input": {"command": "touch made.txt"}});
        let mut answer = report(&port_file, &asks).unwrap();
        // No answer comes, so Claude Code asks in its own session.
        assert_eq!(read(&mut answer), None);
        assert!(sent(
            &port_file,
            &hook_input(id, "Stop", "Made it after all.")
        ));
        assert_eq!(asking.join().unwrap().unwrap().0, "Made it after all.");
        hub.stop();
    }

    /// The words a streamed reply carried.
    fn reply_text(reply: &str) -> String {
        reply
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter_map(|data| serde_json::from_str::<Value>(data).ok())
            .filter_map(|event| {
                event["choices"][0]["delta"]["content"]
                    .as_str()
                    .map(str::to_owned)
            })
            .collect()
    }

    #[test]
    fn a_chat_with_no_project_works_in_a_folder_of_the_plugins_never_the_home_folder() {
        let dir = scratch("work-in");
        let home = dir.join("home");
        std::fs::create_dir_all(&home).unwrap();
        assert_eq!(work_in(home.clone(), Some(&home), &dir), dir.join("work"));
        assert_eq!(work_in(PathBuf::new(), Some(&home), &dir), dir.join("work"));
        assert!(dir.join("work").is_dir());
        let project = dir.join("project");
        assert_eq!(work_in(project.clone(), Some(&home), &dir), project);
    }

    #[test]
    fn trust_is_noted_as_claude_codes_own_prompt_notes_it_and_the_rest_is_kept() {
        let dir = scratch("trust");
        let folder = dir.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let config = dir.join(".claude.json");
        let before = json!({"userID": "u", "projects": {"/other": {"hasTrustDialogAccepted": true, "allowedTools": []}}});
        std::fs::write(&config, before.to_string()).unwrap();
        trust_in(&config, &folder).unwrap();
        let after: Value =
            serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
        let key = folder.canonicalize().unwrap().display().to_string();
        assert_eq!(after["projects"][&key]["hasTrustDialogAccepted"], true);
        assert_eq!(after["projects"]["/other"], before["projects"]["/other"]);
        assert_eq!(after["userID"], "u");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&config).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        std::fs::write(&config, "not json").unwrap();
        assert!(trust_in(&config, &folder).is_err());
        assert_eq!(std::fs::read_to_string(&config).unwrap(), "not json");
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_claude_code_is_not_trusted_in_is_asked_about_in_the_chat() {
        let dir = scratch("untrusted");
        let folder = dir.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let hub = faked(&dir, &folder, Mode::Default);
        // The stand-in reads the trust from the same file it is noted in.
        let config = dir.join("trusted");
        std::fs::write(&config, "{}").unwrap();
        hub.lock().config = Some(config.clone());
        let system = format!(
            "You are Letronna. The project folder is `{}`.",
            folder.display()
        );
        let first = turn_on(
            &hub,
            "claude-code",
            "",
            json!([
                {"role": "system", "content": system},
                {"role": "user", "content": "make the tests pass"},
            ]),
        );
        let reply = first.join().unwrap();
        let asked = reply_text(&reply);
        assert!(asked.starts_with(TRUST_ASKED), "{asked}");
        assert!(started(&dir).is_empty());
        let messages = |answer: &str| {
            json!([
                {"role": "system", "content": system},
                {"role": "user", "content": "make the tests pass"},
                {"role": "assistant", "content": asked},
                {"role": "user", "content": answer},
            ])
        };
        let refused = reply_text(
            &turn_on(&hub, "claude-code", "", messages("not now"))
                .join()
                .unwrap(),
        );
        assert!(refused.starts_with("Claude Code stays out of"), "{refused}");
        let trusting = turn_on(&hub, "claude-code", "", messages("trust"));
        until(|| started(&dir).len() == 1);
        answers(&dir, "00000001", "All pass.");
        assert!(trusting.join().unwrap().contains("All pass."));
        // The message that asked for Claude Code went, not the answer.
        let (_, args) = &started(&dir)[0];
        assert!(args.ends_with("-- make the tests pass"), "{args}");
        let key = super::key_of("make the tests pass", &asked);
        assert_eq!(hub.session(&key).unwrap().id, "00000001-0000-full-id");
        hub.stop();
    }

    #[test]
    fn the_mcp_servers_project_files_name_are_found_unless_the_user_turned_them_on() {
        let dir = scratch("servers");
        let folder = dir.join("site");
        std::fs::create_dir_all(&folder).unwrap();
        let servers = |names: &[&str]| {
            let listed: serde_json::Map<String, Value> = names
                .iter()
                .map(|name| (name.to_string(), json!({"command": "x"})))
                .collect();
            json!({"mcpServers": listed}).to_string()
        };
        std::fs::write(dir.join(".mcp.json"), servers(&["ida", "db"])).unwrap();
        std::fs::write(folder.join(".mcp.json"), servers(&["local"])).unwrap();
        let settings = dir.join("settings.json");
        assert_eq!(undecided_servers(&folder, None), ["local", "ida", "db"]);
        std::fs::write(
            &settings,
            json!({"enabledMcpjsonServers": ["db"]}).to_string(),
        )
        .unwrap();
        assert_eq!(
            undecided_servers(&folder, Some(&settings)),
            ["local", "ida"]
        );
        // What the folder's own settings chose is chosen, as Claude Code
        // notes it when asked: left off here.
        leave_off(&folder, &["ida".into()]).unwrap();
        assert_eq!(undecided_servers(&folder, Some(&settings)), ["local"]);
        leave_off(&folder, &["local".into()]).unwrap();
        let local: Value = serde_json::from_str(
            &std::fs::read_to_string(folder.join(".claude/settings.local.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(local["disabledMcpjsonServers"], json!(["ida", "local"]));
        std::fs::write(
            &settings,
            json!({"enableAllProjectMcpServers": true}).to_string(),
        )
        .unwrap();
        assert!(undecided_servers(&folder, Some(&settings)).is_empty());
    }

    #[test]
    fn a_sessions_screen_is_read_as_its_words() {
        let shown = "\u{1b}[2J\u{1b}[HNew\u{1b}[1CMCP\u{1b}[1Cserver\u{1b}]0;title\u{7}found ❯\u{1b}[38;2;1;2;3mContinue";
        let words: Vec<String> = plain(shown).split_whitespace().map(str::to_owned).collect();
        assert_eq!(words, ["New", "MCP", "server", "found", "❯", "Continue"]);
    }

    #[test]
    fn only_the_users_newest_words_are_passed_on() {
        let asked = json!({"messages": [
            {"role": "user", "content": "first"},
            {"role": "user", "content": [
                {"type": "text", "text": "look at this"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,AA"}},
            ]},
        ]});
        assert_eq!(newest(&asked).as_deref(), Some("look at this"));
        let after_a_tool = json!({"messages": [{"role": "tool", "content": "42"}]});
        assert_eq!(newest(&after_a_tool), None);
    }

    #[test]
    fn the_folder_comes_from_the_prompt_of_the_session_that_asked() {
        let parts = json!({"messages": [
            {"role": "system", "content": [
                {"type": "text", "text": "You are Letronna."},
                {"type": "text", "text": "The project folder is `/work/it's here`.", "cache_control": {"type": "ephemeral"}},
            ]},
            {"role": "user", "content": "hi"},
        ]});
        assert_eq!(
            project_folder(&parts),
            Some(PathBuf::from("/work/it's here"))
        );
        let none = json!({"messages": [
            {"role": "system", "content": "No project folder is attached."},
        ]});
        assert_eq!(project_folder(&none), None);
    }

    #[test]
    fn each_data_folder_keeps_a_model_port_of_its_own() {
        let one = model_port(Path::new("/Users/a/Library/Application Support/Letronna"));
        assert_eq!(
            one,
            model_port(Path::new("/Users/a/Library/Application Support/Letronna"))
        );
        assert_ne!(one, model_port(Path::new("/tmp/scratch/letronna")));
        assert!((47821..48821).contains(&one));
    }

    #[test]
    fn the_custom_gateway_is_taken_back_only_when_it_is_the_plugins() {
        assert!(is_ours("http://127.0.0.1:47821/claude-code/v1"));
        assert!(is_ours("http://127.0.0.1:50123/claude-code/v1"));
        assert!(!is_ours("http://127.0.0.1:8080/v1"));
        assert!(!is_ours("https://example.com/claude-code/v1"));
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_with_a_quote_or_a_space_stays_one_word_to_the_shell() {
        let folder = Path::new("/work/it's my site");
        let said = std::process::Command::new("sh")
            .args(["-c", &format!("printf %s {}", quoted(folder))])
            .output()
            .unwrap();
        assert_eq!(
            String::from_utf8(said.stdout).unwrap(),
            "/work/it's my site"
        );
        let script = script(folder, &["attach", "59638618"]);
        assert!(script.contains("cd '/work/it'\\''s my site' || exit 1"));
        assert!(script.contains("exec claude 'attach' '59638618'"));
    }

    #[cfg(unix)]
    #[test]
    fn only_its_owner_can_read_the_port_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        let hub = hub(&dir);
        let mode = std::fs::metadata(dir.join(PORT_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        hub.stop();
    }

    #[test]
    #[ignore = "needs Claude Code installed and signed in, and the window built (cargo build -p letronna-gui); the folder must be one Claude Code trusts"]
    fn a_live_claude_code_session_follows_the_chat() {
        let dir = scratch("live");
        let folder = dir.join("work");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("notes.txt"), "the kettle is on\n").unwrap();
        // The window's binary sits one folder above this test's own.
        let test = std::env::current_exe().unwrap();
        let exe = test
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("letronna-gui");
        let hub = hub_running(&dir, &exe);
        let offered = hub.catalog(true);
        eprintln!("offered: {offered:?}");
        assert!(offered.iter().any(|model| model.id == "claude-code-opus"));
        let wait = |_| {
            std::thread::sleep(Duration::from_millis(250));
            true
        };
        // Asked as a person would: wording like "answer with its words
        // only" reads as a probe to Claude's safeguards.
        let (said, first) = hub
            .ask_with(
                None,
                &folder,
                Mode::AcceptEdits,
                (Some(""), Some("")),
                (
                    "",
                    "What files are in this folder, and what does notes.txt say?",
                ),
                wait,
            )
            .unwrap();
        eprintln!("first: {said} ({})", first.id);
        assert!(said.to_lowercase().contains("kettle"), "{said}");
        hub.bind("chat 1".into(), first.clone());
        let (said, again) = hub
            .ask_with(
                Some("chat 1"),
                &folder,
                Mode::AcceptEdits,
                (None, None),
                ("", "What did you just tell me notes.txt says?"),
                wait,
            )
            .unwrap();
        eprintln!("resumed: {said} ({})", again.id);
        assert_eq!(again.id, first.id);
        assert!(said.to_lowercase().contains("kettle"), "{said}");
        hub.bind("chat 1".into(), again);
        let (said, copy) = hub
            .ask_with(
                Some("chat 1"),
                &folder,
                Mode::Plan,
                (Some("sonnet"), None),
                ("", "Which file did we talk about?"),
                wait,
            )
            .unwrap();
        eprintln!("in plan mode: {said} ({})", copy.id);
        assert_ne!(copy.id, first.id);
        assert_eq!(
            (copy.mode.as_str(), copy.model.as_str()),
            ("plan", "sonnet")
        );
        assert!(said.contains("notes.txt"), "{said}");
        // A chat whose session Claude Code no longer has goes on in a new
        // one, told the chat so far.
        let gone = Session {
            transcript: dir.join("removed.jsonl"),
            ..copy
        };
        hub.bind("chat 1".into(), gone.clone());
        let chat = json!({"messages": [
            {"role": "user", "content": "My favourite colour is teal."},
            {"role": "assistant", "content": "Noted: teal."},
            {"role": "user", "content": "What is my favourite colour?"},
        ]});
        let (said, fresh) = hub
            .ask_with(
                Some("chat 1"),
                &folder,
                Mode::AcceptEdits,
                (None, None),
                (&earlier(&chat), "What is my favourite colour?"),
                wait,
            )
            .unwrap();
        eprintln!("gone, then: {said} ({})", fresh.id);
        assert_ne!(fresh.id, gone.id);
        assert!(said.to_lowercase().contains("teal"), "{said}");
        // A permission Claude Code asks for is asked in the chat, and the
        // user's answer lets the same turn go on. An ask rule makes touch
        // ask whatever the user allows.
        let settings = dir.join(super::SETTINGS_FILE);
        let mut hooks: Value =
            serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
        hooks["permissions"] = json!({"ask": ["Bash(touch:*)"]});
        std::fs::write(&settings, hooks.to_string()).unwrap();
        let made = folder.join("made.txt");
        let asks = |key, message| Ask {
            key,
            folder: &folder,
            mode: Mode::Default,
            model: None,
            effort: None,
            earlier: "",
            message,
            questions: true,
        };
        let make = "Create an empty file named made.txt in this folder with the touch command.";
        let (said, session) = hub.ask(asks(None, make), wait).unwrap();
        eprintln!("asks: {said}");
        assert!(
            said.starts_with("Claude Code wants to run `touch"),
            "{said}"
        );
        assert!(!made.exists());
        hub.bind("chat 3".into(), session);
        let (said, _) = hub.ask(asks(Some("chat 3"), "allow"), wait).unwrap();
        eprintln!("allowed: {said}");
        assert!(made.exists(), "{said}");
        // A folder Claude Code was never trusted in is asked about in the
        // chat, and the user's yes sends the message that asked for it.
        let untrusted = std::env::var_os("LETRONNA_UNTRUSTED").map(PathBuf::from);
        if let Some(untrusted) = untrusted {
            std::fs::create_dir_all(&untrusted).unwrap();
            hub.lock().catalog = super::catalog(&[json!({"value": "default"})]);
            let system = format!(
                "You are Letronna. The project folder is `{}`.",
                untrusted.display()
            );
            let first = json!({"model": "claude-code", "stream": true, "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": "What is 6 times 7?"},
            ]});
            let asked = reply_text(&call(&hub, &post(&key(&hub), &first)));
            eprintln!("trust asked: {asked}");
            assert!(asked.starts_with(super::TRUST_ASKED), "{asked}");
            let trusting = json!({"model": "claude-code", "stream": true, "messages": [
                {"role": "system", "content": system},
                {"role": "user", "content": "What is 6 times 7?"},
                {"role": "assistant", "content": asked},
                {"role": "user", "content": "trust"},
            ]});
            let reply = call(&hub, &post(&key(&hub), &trusting));
            let said = reply_text(&reply);
            eprintln!("trusted: {said}");
            assert!(said.contains("42"), "{said}");
        }
        hub.stop();
    }
}
