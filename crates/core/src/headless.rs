//! A Letronna with no window, for `letronna serve`: the chat apps' messages
//! and the schedules, on a machine nobody sits at.
//!
//! The window is the only writer of each session it holds, and a run it
//! starts works on a copy that it applies when the run ends. With no window
//! there is no such writer, so a chat's run here writes its session itself,
//! as a scheduled run writes its own: the prompt goes in when the message
//! comes, and the run appends what it says as it says it. What a message
//! does is `remote`'s, and what a run may do is as in the window: a call
//! that needs a yes has its card sent to the chat whose run it is, and one
//! nobody here could answer, such as a scheduled run's, is declined.
//!
//! Everything happens on the thread `serve` is called on, one `Input` at a
//! time, so nothing here needs a lock: the chats' messages, the cards runs
//! put up, the ends of runs, what the schedules' runner says, and the quit.

use crate::approvals;
use crate::cancel::Cancel;
use crate::chat::{self as backend, Connection, Remote};
use crate::driver::{self, Allow, Outcome, Spec};
use crate::gate::Mode;
use crate::messaging::pairing::Attempt;
use crate::messaging::{self, Config, Incoming, Source, files};
use crate::remote::{self, AT_ONCE, Dispatcher, Host};
use crate::schedule::{self, Event};
use crate::store::{self, Transcript};
use crate::{agent, gateway, inbox, judge, tools};
use async_channel::{Receiver, Sender};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread::JoinHandle;

/// Enough of a message to recognise it by on one status line.
const PREVIEW: usize = 60;

/// What `serve` deals with, one at a time.
pub enum Input {
    /// A chat app's message.
    Heard(Incoming),
    /// A chat app's message back from work done on it off this thread, its
    /// files fetched or its voice note heard, to be dealt with again.
    Worked(Incoming),
    /// A call of a run that needs a yes.
    Asked(approvals::Request),
    /// A chat's run ended.
    Ended(Ended),
    /// What the schedules' runner said.
    Scheduled(Event),
    /// Stop every run, and return once they have all ended.
    Quit,
}

/// How a chat's run ended, sent from its thread.
pub struct Ended {
    session: u64,
    /// Its answer, or why there is none, for its chat.
    text: String,
    stopped: bool,
    failed: bool,
}

/// A chat's run, going or waiting for a place.
struct Run {
    source: Source,
    remote: Remote,
    mode: Mode,
    cancel: Cancel,
    queued: bool,
}

/// What goes to a chat.
pub struct Outgoing {
    pub source: Source,
    pub text: String,
    /// The id of the call the text is the card for, which puts Allow and
    /// Deny under it.
    pub card: Option<String>,
    /// Pictures and sound a run made, which follow its answer.
    pub made: Vec<PathBuf>,
}

/// Says a message to a chat: the chat apps themselves, or a test's fake.
pub type Tell = Box<dyn Fn(Outgoing) + Send>;
/// Says a line of what happens, for whoever started this.
pub type Say = Box<dyn Fn(String) + Send>;

/// The chat apps' host when no window is.
pub struct Headless {
    remote: Dispatcher,
    config: Config,
    /// Where a chat's new session works.
    workspace: Option<PathBuf>,
    /// The chats' runs, by session.
    runs: BTreeMap<u64, Run>,
    /// The schedules' runs going here, by session, for a quit to stop.
    scheduled: BTreeMap<u64, Cancel>,
    tell: Tell,
    say: Say,
    /// Where a run's thread sends its end.
    inputs: Sender<Input>,
    quitting: bool,
}

/// Hear the chat apps set up in `config` into `inputs`. `None` when none is
/// set up; an error when another Letronna already hears them.
pub fn hear(config: &Config, inputs: Sender<Input>) -> Result<Option<Arc<AtomicBool>>, String> {
    let (sender, heard) = async_channel::unbounded();
    let listening = remote::listen(config, sender, remote::By::Serve).map_err(|e| e.to_string())?;
    forward(heard, inputs, Input::Heard);
    Ok(listening)
}

/// Take the cards runs put up from now on into `inputs`, instead of whoever
/// took them before.
pub fn answer_cards(inputs: Sender<Input>) {
    let served = approvals::serve();
    // `serve` hands back a blocking receiver, so a thread of its own
    // carries each request over.
    std::thread::spawn(move || {
        while let Ok(request) = served.recv() {
            if inputs.send_blocking(Input::Asked(request)).is_err() {
                return;
            }
        }
    });
}

/// Take what the schedules' runner says into `inputs`.
pub fn hear_schedules(events: Receiver<Event>, inputs: Sender<Input>) {
    forward(events, inputs, Input::Scheduled);
}

/// Carry each of `from` into `to` as an input, on a thread of its own.
fn forward<T: Send + 'static>(from: Receiver<T>, to: Sender<Input>, wrap: fn(T) -> Input) {
    std::thread::spawn(move || {
        while let Ok(item) = from.recv_blocking() {
            if to.send_blocking(wrap(item)).is_err() {
                return;
            }
        }
    });
}

/// Says to the chat apps in `config`, in the order said, on a thread of its
/// own so that a slow send holds up nothing else. The thread ends once the
/// `Tell` is dropped and everything it was given has gone, so a leaving
/// Letronna can wait for its last words to reach the chats.
pub fn chat_apps(config: &Config) -> (Tell, JoinHandle<()>) {
    let config = config.clone();
    let (said, to_send) = std::sync::mpsc::channel::<Outgoing>();
    let sender = std::thread::spawn(move || {
        for out in to_send {
            if out.made.is_empty() {
                let card = out.card.as_deref();
                let _ = messaging::tell(&config, &out.source, &out.text, card);
            } else {
                files::answer(&config, &out.source, &out.text, &out.made);
            }
        }
    });
    let tell = Box::new(move |out: Outgoing| {
        let _ = said.send(out);
    });
    (tell, sender)
}

/// A chat as a status line names it.
fn name(source: &Source) -> String {
    match source {
        Source::Telegram(chat) => format!("Telegram chat {chat}"),
        Source::Discord(channel) => format!("Discord channel {channel}"),
        Source::Slack { channel, .. } => format!("Slack channel {channel}"),
    }
}

/// The start of `text` on one line, with no control characters: a stranger
/// writing to the bot must not steer the terminal with escape sequences.
fn preview(text: &str) -> String {
    let line: String = tools::one_line(text)
        .chars()
        .filter(|c| !c.is_control())
        .collect();
    let mut kept: String = line.chars().take(PREVIEW).collect();
    if kept.len() < line.len() {
        kept.push('…');
    }
    kept
}

impl Headless {
    /// A host for the chats set up in `config`, whose new sessions work in
    /// `workspace`; what it is to deal with goes down the sender handed back.
    pub fn new(
        config: Config,
        workspace: Option<PathBuf>,
        tell: Tell,
        say: Say,
    ) -> (Self, Sender<Input>, Receiver<Input>) {
        let (inputs, taken) = async_channel::unbounded();
        let host = Self {
            remote: Dispatcher::default(),
            config,
            workspace,
            runs: BTreeMap::new(),
            scheduled: BTreeMap::new(),
            tell,
            say,
            inputs: inputs.clone(),
            quitting: false,
        };
        (host, inputs, taken)
    }

    /// Deal with each input as it comes, until every run a quit stopped has
    /// ended.
    pub fn serve(mut self, inputs: Receiver<Input>) {
        while let Ok(input) = inputs.recv_blocking() {
            match input {
                Input::Heard(message) => self.heard(message),
                Input::Worked(message) => self.take(message),
                Input::Asked(request) => self.asked(request),
                Input::Ended(ended) => self.finish(ended),
                Input::Scheduled(event) => self.on_schedule(event),
                Input::Quit => self.quit(),
            }
            if self.quitting && self.runs.is_empty() && self.scheduled.is_empty() {
                return;
            }
        }
    }

    fn heard(&mut self, message: Incoming) {
        let chat = name(&message.source);
        (self.say)(format!("{chat} said: {}", preview(&message.said)));
        self.take(message);
    }

    /// Deal with a chat's message, unless a quit has come.
    fn take(&mut self, message: Incoming) {
        if self.quitting {
            let text = "Letronna is stopping, so this was not run. Send it again once it is back.";
            Host::tell(self, &message.source, text);
            return;
        }
        remote::dispatch(self, message);
    }

    /// A run's call that needs a yes: to the chat whose run it is, or,
    /// dropped, declined when nobody here could answer it.
    fn asked(&mut self, request: approvals::Request) {
        let source = self
            .runs
            .get(&request.root)
            .filter(|_| !self.quitting)
            .map(|run| run.source.clone());
        let Some(source) = source else {
            let (tool, session) = (&request.call.name, request.root);
            (self.say)(format!(
                "Declined {tool} in session {session}: nobody here answers it."
            ));
            return;
        };
        remote::ask(self, source, request);
    }

    /// Take a chat's run's end: its chat hears it, and what came too late
    /// for it starts the next.
    fn finish(&mut self, ended: Ended) {
        let Some(run) = self.runs.remove(&ended.session) else {
            return;
        };
        let answered = !ended.stopped && !ended.failed;
        remote::ended(self, &run.source, ended.session, &ended.text, answered);
        let how = match (ended.stopped, ended.failed) {
            (true, _) => "stopped",
            (_, true) => "did not finish",
            _ => "answered",
        };
        (self.say)(format!("Session {}: {how}.", ended.session));
        // Stop means stop, and after a failure the next would most likely
        // fail too: what came meanwhile waits for the next message.
        if !ended.stopped
            && !ended.failed
            && !self.quitting
            && let Some(mail) = inbox::fold(&inbox::take(ended.session, true))
        {
            Host::run(self, ended.session, run.mode, mail, run.source, run.remote);
        }
        self.start_queued();
    }

    /// Keep what the schedules' runner says: its runs, for a quit to stop,
    /// and a line for each.
    fn on_schedule(&mut self, event: Event) {
        let line = match event {
            Event::Started(started) => {
                // Started as a quit came, so it stops at once.
                if self.quitting {
                    started.cancel.set();
                }
                let (job, session) = (&started.name, started.session);
                let line = format!("Scheduled job {job}: running in session {session}.");
                self.scheduled.insert(started.session, started.cancel);
                line
            }
            Event::Ended(ended) => {
                if let Some(session) = ended.session {
                    self.scheduled.remove(&session);
                }
                // A check that printed what it did last time started
                // nothing, and would say so every few minutes; a quiet run
                // that had a session said it was running, so its end is said.
                if ended.quiet && ended.session.is_none() {
                    return;
                }
                format!("Scheduled job {}: {}", ended.name, preview(&ended.text))
            }
            Event::Trouble(trouble) => trouble,
        };
        (self.say)(line);
    }

    /// Stop every run, the schedules' included, and start nothing more.
    fn quit(&mut self) {
        self.quitting = true;
        schedule::stop();
        for cancel in self.scheduled.values() {
            cancel.set();
        }
        let ids: Vec<u64> = self.runs.keys().copied().collect();
        for id in ids {
            Host::stop(self, id);
        }
    }

    /// Start queued runs, in session order, while there is a place.
    fn start_queued(&mut self) {
        while !self.quitting {
            let going = self.runs.values().filter(|run| !run.queued).count();
            let next = self.runs.iter().find(|(_, run)| run.queued);
            let (true, Some((&id, _))) = (going < AT_ONCE, next) else {
                return;
            };
            self.start(id);
        }
    }

    /// Start session `id`'s run, or end it at once saying why it could not.
    fn start(&mut self, id: u64) {
        let Some(run) = self.runs.get_mut(&id) else {
            return;
        };
        run.queued = false;
        let inputs = self.inputs.clone();
        let spawned = spec(id, run).and_then(|spec| {
            driver::spawn(spec, move |report| {
                let _ = inputs.send_blocking(Input::Ended(Ended {
                    session: id,
                    text: remote::said(report),
                    stopped: report.status == Outcome::Stopped,
                    failed: matches!(report.status, Outcome::Failed(_)),
                }));
            })
        });
        match spawned {
            Ok(_) => (self.say)(format!("Session {id}: running.")),
            Err(error) => {
                if let Some(run) = self.runs.remove(&id) {
                    let text = format!("Did not start: {error}");
                    remote::ended(self, &run.source, id, &text, false);
                }
            }
        }
    }
}

/// Session `id` as it is on disk, to be written in place.
fn open_session(id: u64) -> Result<Transcript, String> {
    let conversation = store::session(id).ok_or("its session is gone.")?;
    let path = store::session_path(id);
    Ok(Transcript { path, conversation })
}

/// A run of `run` on session `id` as it is on disk, on the gateway and model
/// set up now, writing the session as it goes.
fn spec(id: u64, run: &Run) -> Result<Spec, String> {
    let transcript = open_session(id)?;
    let conversation = &transcript.conversation;
    // Saves and keeps memory as the session chose, wherever it chose it.
    crate::memory::choose(id, conversation.memory_saving);
    crate::memory::choose_scope(id, conversation.memory_scope);
    let settings = store::current_settings();
    // Each session keeps the model it was on, and the gateway with it, as
    // in the window.
    let own = (conversation.gateway.as_deref())
        .zip(conversation.model.as_deref())
        .filter(|&(gateway, model)| {
            !model.is_empty() && gateway::GATEWAYS.iter().any(|g| g.name == gateway)
        });
    let (gateway, model) = own.unwrap_or((&settings.gateway, &settings.model));
    let saved = store::load_keys()
        .get(gateway::key_slot(gateway))
        .cloned()
        .unwrap_or_default();
    let (endpoint, key) = gateway::resolve(gateway, &settings.custom_endpoint, &saved);
    let connection = Connection {
        endpoint,
        model: model.to_owned(),
        key,
        tools: true,
        workspace: conversation.workspace.clone(),
        plan: run.mode == Mode::Plan,
        effort: (conversation.effort.clone()).unwrap_or_else(|| settings.effort.clone()),
        search: settings.search,
        // As in the window: nobody may be watching a chat app's run.
        unattended: true,
        remote: Some(run.remote.clone()),
        conversation: id,
        mode: run.mode,
        // As in the window: a chat's session has a human too, just reached
        // through a card sent to the chat rather than shown anywhere here.
        human: true,
        // What the user asked for, for the judge in Auto mode.
        typed: judge::typed(&conversation.messages).into(),
        ..Default::default()
    };
    backend::validate(&connection)?;
    let label = conversation.title.clone();
    Ok(Spec {
        connection,
        transcript,
        id,
        owner: id,
        root: id,
        run: id,
        label,
        allow: Allow::All,
        rounds: agent::MAX_ROUNDS,
        cancel: run.cancel.clone(),
        budget: None,
        status: Default::default(),
        depth: 0,
        fork: false,
        from: 0,
        schema: None,
        watch: None,
        // Its own gate, as in the window: a card of its comes to `asked`.
        decide: None,
    })
}

impl Host for Headless {
    fn dispatcher(&mut self) -> &mut Dispatcher {
        &mut self.remote
    }

    fn config(&self) -> &Config {
        &self.config
    }

    /// Read afresh first, so a change made in the window meanwhile is not
    /// written over when this one is saved.
    fn config_mut(&mut self) -> &mut Config {
        self.config = messaging::load();
        &mut self.config
    }

    /// Read afresh, so a change made in the window counts from the next message.
    fn default_mode(&self) -> Mode {
        store::current_settings().mode
    }

    fn tell_card(&mut self, source: &Source, text: &str, card: Option<String>) {
        (self.say)(format!("To {}: {}", name(source), preview(text)));
        (self.tell)(Outgoing {
            source: source.clone(),
            text: text.to_owned(),
            card,
            made: Vec::new(),
        });
    }

    /// In words alone, even to a voice note: reading it aloud is the
    /// window's, where the Voice setting is.
    fn answer(&mut self, source: &Source, id: u64, text: &str, _spoken: bool) {
        (self.say)(format!("To {}: {}", name(source), preview(text)));
        let made = store::session(id)
            .map(|session| files::made(&session.messages))
            .unwrap_or_default();
        (self.tell)(Outgoing {
            source: source.clone(),
            text: text.to_owned(),
            card: None,
            made,
        });
    }

    /// There is no code to try here: pairing is done in the window.
    fn pair(&mut self, message: &Incoming) -> Option<Attempt> {
        let (chat, from) = (name(&message.source), preview(&message.from));
        (self.say)(format!(
            "{chat} ({from}) is not the paired chat, so it was not answered. \
             Pair it in the window's Messaging settings."
        ));
        None
    }

    fn trouble(&mut self, error: String) {
        (self.say)(error);
    }

    fn spawn(&mut self, work: Box<dyn FnOnce() + Send>) {
        std::thread::spawn(work);
    }

    fn after(&mut self, work: Box<dyn FnOnce() -> Incoming + Send>) {
        let inputs = self.inputs.clone();
        std::thread::spawn(move || {
            let _ = inputs.send_blocking(Input::Worked(work()));
        });
    }

    /// The session's own, or the folder serve was started in.
    fn folder(&self, id: Option<u64>) -> Option<PathBuf> {
        match id.and_then(store::session) {
            Some(session) => session.workspace,
            None => self.workspace.clone(),
        }
    }

    fn exists(&self, id: u64) -> bool {
        store::session_path(id).exists()
    }

    fn running(&self, id: u64) -> bool {
        self.runs.contains_key(&id)
    }

    /// There is no turn here but the runs.
    fn busy(&self, _: u64) -> bool {
        false
    }

    fn open(&mut self, title: String) -> Result<u64, String> {
        let transcript = Transcript::session(&title, self.workspace.clone())?;
        Ok(transcript.conversation.id)
    }

    /// The prompt goes into the session now, after what came in meanwhile,
    /// and the run starts once there is a place.
    fn run(
        &mut self,
        id: u64,
        mode: Mode,
        prompt: backend::Message,
        source: Source,
        remote: Remote,
    ) {
        let written = open_session(id).and_then(|mut transcript| {
            let conversation = &mut transcript.conversation;
            conversation
                .messages
                .extend(inbox::fold(&inbox::take(id, true)));
            conversation.messages.push(prompt);
            conversation.updated = backend::now();
            transcript
                .save()
                .map_err(|e| format!("Could not save the session: {e}"))
        });
        if let Err(error) = written {
            remote::ended(self, &source, id, &format!("Did not start: {error}"), false);
            return;
        }
        let run = Run {
            source,
            remote,
            mode,
            cancel: Cancel::default(),
            queued: true,
        };
        self.runs.insert(id, run);
        self.start_queued();
    }

    /// A queued run never starts; one going ends at its next step, keeping
    /// what it did so far, and stays listed until its end comes.
    fn stop(&mut self, id: u64) {
        let Some(run) = self.runs.get(&id) else {
            return;
        };
        if !run.queued {
            run.cancel.set();
            return;
        }
        if let Some(run) = self.runs.remove(&id) {
            remote::ended(self, &run.source, id, "Stopped.", false);
            self.start_queued();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Headless, Input, Outgoing, PREVIEW, answer_cards, preview};
    use crate::approvals::{self, tests::SERVING};
    use crate::chat::{Remote, ToolCall};
    use crate::messaging::{Config, Incoming, Source};
    use crate::schedule::{Ended, Event};
    use crate::store::{self, tests::DATA_DIR, tests::Scratch};
    use crate::tools::ToolContext;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver};
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// Far longer than a run takes, so only a hang fails a test.
    const WAIT: Duration = Duration::from_secs(10);
    /// The Telegram chat the tests pair.
    const PAIRED: i64 = 42;

    /// A host started on its own thread, and what it said to the chats.
    struct Serving {
        inputs: async_channel::Sender<Input>,
        told: Receiver<(Source, String)>,
        host: JoinHandle<()>,
    }

    impl Serving {
        fn start(workspace: Option<PathBuf>) -> Self {
            let mut config = Config::default();
            config.telegram.chat = Some(PAIRED);
            config.telegram.owner = Some(1001);
            let (tx, told) = mpsc::channel();
            let tell = Box::new(move |out: Outgoing| {
                let _ = tx.send((out.source, out.text));
            });
            let (host, inputs, taken) = Headless::new(config, workspace, tell, Box::new(|_| ()));
            let host = std::thread::spawn(move || host.serve(taken));
            Self { inputs, told, host }
        }

        fn hear(&self, said: &str) {
            let message = Incoming {
                source: Source::Telegram(PAIRED),
                from: "user_a".into(),
                text: said.into(),
                said: said.into(),
                author: Some("1001".into()),
                files: Vec::new(),
                remote: Remote {
                    place: "a Telegram chat".into(),
                    ..Default::default()
                },
                press: None,
                needs_session: false,
                spoken: false,
            };
            self.inputs.send_blocking(Input::Heard(message)).unwrap();
        }

        fn next_told(&self) -> String {
            self.told
                .recv_timeout(WAIT)
                .expect("the chat heard nothing")
                .1
        }

        /// Quit, and wait for the host to return.
        fn quit(self) {
            self.inputs.send_blocking(Input::Quit).unwrap();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || tx.send(self.host.join().is_ok()));
            assert!(rx.recv_timeout(WAIT).expect("the host never returned"));
        }
    }

    /// A provider that streams `text` in answer to one request, and hands back
    /// what the request carried.
    fn provider(text: &str) -> (String, JoinHandle<String>) {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let event = serde_json::json!({"choices": [{"delta": {"content": text}}]});
        let reply = format!("data: {event}\n\ndata: [DONE]\n\n");
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            let mut bytes = Vec::new();
            let mut block = [0; 65536];
            let body = loop {
                let n = socket.read(&mut block).unwrap();
                assert_ne!(n, 0, "the request ended early");
                bytes.extend_from_slice(&block[..n]);
                let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = headers
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + length {
                    break String::from_utf8_lossy(&bytes[end + 4..end + 4 + length]).into_owned();
                }
            };
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            body
        });
        (endpoint, worker)
    }

    #[test]
    fn a_chats_message_is_answered_through_the_model_and_written_to_its_session() {
        let _data = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new("headless-answer");
        let (data, work) = (dir.join("data"), dir.join("work"));
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(&work).unwrap();
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        let (endpoint, provider) = provider("All green.");
        let settings = serde_json::json!({"endpoint": endpoint, "model": "stub",
            "gateway": "Custom", "custom_endpoint": endpoint});
        std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();

        let serving = Serving::start(Some(work.clone()));
        serving.hear("run the tests");
        assert_eq!(serving.next_told(), "All green.");
        assert!(provider.join().unwrap().contains("run the tests"));
        serving.hear("/stop");
        assert_eq!(serving.next_told(), "Nothing is running.");
        serving.quit();

        // Written by the run itself, as no window holds it.
        let listed = store::summaries();
        assert_eq!(listed.len(), 1);
        let session = store::session(listed[0].id).unwrap();
        assert_eq!(session.title, "run the tests");
        assert_eq!(session.workspace, Some(work));
        let said: Vec<_> = session
            .messages
            .iter()
            .map(|m| (m.role.as_str(), m.content.as_str(), m.remote))
            .collect();
        assert_eq!(
            said,
            [
                ("user", "run the tests", true),
                ("assistant", "All green.", false)
            ]
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_card_nobody_here_can_answer_is_declined_and_a_quit_with_nothing_going_returns() {
        let _serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
        let serving = Serving::start(None);
        answer_cards(serving.inputs.clone());
        let call = ToolCall {
            id: "1".into(),
            name: "run_command".into(),
            arguments: r#"{"command": "cargo test"}"#.into(),
        };
        let (tx, allowed) = mpsc::channel();
        std::thread::spawn(move || {
            let ctx = ToolContext::default();
            let _ = tx.send(approvals::ask(
                77,
                77,
                "a scheduled job",
                &call,
                &ctx,
                "",
                false,
            ));
        });
        assert!(allowed.recv_timeout(WAIT).unwrap().is_err());
        assert!(serving.told.try_recv().is_err(), "no chat was asked");
        serving.quit();
    }

    #[test]
    fn a_status_line_carries_no_escape_sequence_and_stays_short() {
        let sneaky = "hi \u{1b}]52;c;aGk=\u{7}there\u{1b}[2J\nsecond line";
        assert_eq!(preview(sneaky), "hi ]52;c;aGk=there[2J second line");
        let long = "word ".repeat(40);
        let kept = preview(&long);
        assert_eq!(kept.chars().count(), PREVIEW + 1);
        assert!(kept.ends_with('…'));
    }

    #[test]
    fn a_skipped_check_leaves_no_line_and_a_quiet_run_its_end() {
        let (tx, said) = mpsc::channel();
        let say = Box::new(move |line: String| {
            let _ = tx.send(line);
        });
        let (mut host, _inputs, _taken) =
            Headless::new(Config::default(), None, Box::new(|_| ()), say);
        let ended = |session, text: &str| {
            Event::Ended(Ended {
                schedule: 1,
                name: "Price".into(),
                session,
                text: text.into(),
                failed_early: false,
                quiet: true,
            })
        };
        host.on_schedule(ended(None, "Skipped: its check printed what it did."));
        host.on_schedule(ended(Some(7), "Nothing new, so nothing was sent."));
        let lines: Vec<String> = said.try_iter().collect();
        assert_eq!(
            lines,
            ["Scheduled job Price: Nothing new, so nothing was sent."]
        );
    }
}
