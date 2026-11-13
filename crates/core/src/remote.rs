//! Chat-app messages, dealt with the same way by whichever Letronna hears
//! them.
//!
//! Each chat talks in a session of its own, carried on from one message to
//! the next until it says `/new`. A message becomes a run in that session,
//! beside whatever else is going. One for a session a run holds is left for
//! that run as a `Later` notice, which it reads before it answers, rather
//! than racing it; a card a chat's run needs answered goes to the chat, whose
//! next message answers it. What differs between the window and a Letronna
//! with no window is the `Host`'s: who holds and writes the sessions, how a
//! run starts and stops, and how a chat asking to be paired is shown.
//!
//! A card left waiting in the window while nobody is there can go to the
//! owner's private chat as well, when they switch that on. It is the same
//! card either way, so whichever answer comes first settles it: an answer
//! at the desk takes the buttons off the phone's copy, and one from the
//! phone answers the window's as a click would. The phone may allow a call
//! once or deny it, never for the whole session, since whoever holds the
//! chat account is not the person at the desk.

use crate::approvals;
use crate::chat::{self as backend, Message, Remote, ToolCall};
use crate::driver::{self, Outcome};
use crate::gate::Mode;
use crate::inbox::{self, Notice, When};
use crate::messaging::pairing::{self, Attempt};
use crate::messaging::{self, CardAnswer, Config, Dm, Incoming, Press, Sent, Source, files};
use crate::notify::Note;
use std::collections::{HashMap, HashSet};
use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, PoisonError};

/// Runs started at once: a few chats and `/bg` tasks side by side, without
/// crowding the provider's rate limit. The rest wait for a place rather than
/// being refused, since somebody asked for each.
pub const AT_ONCE: usize = 4;

/// Why a Letronna does not hear the chat apps while another does.
const ELSEWHERE: &str = "Another Letronna already answers the chat apps: a window with them \
set up, or another `letronna serve`.";

/// The lock held by the Letronna that hears the chat apps, for as long as
/// any is set up. Telegram hands each message to one poller only and turns
/// a second away, and two Discord listeners would answer every message
/// twice, so only one Letronna may hear them. The system lets the lock go
/// when that Letronna quits, crashes or is killed. Kept with the data folder
/// it is in, since a profile switched to is a data folder with a lock of its
/// own.
static HEARING: Mutex<Option<(PathBuf, File)>> = Mutex::new(None);

/// The lock's file in the data folder.
const LOCK: &str = "messaging.lock";

/// Who holds the lock: "serve" or "window", then the process id.
const RECORD: &str = "messaging.holder";

/// What a host keeps for the chats between their messages.
#[derive(Default)]
pub struct Dispatcher {
    /// The session each chat is talking in, so a follow-up keeps its context.
    sessions: HashMap<Source, u64>,
    /// The card each chat was sent, which its next message or a tap on the
    /// card answers.
    asks: HashMap<Source, approvals::Request>,
    /// Messages with a voice note, each held while its chat is asked whether
    /// voice notes may be transcribed there. The chat's next words answer.
    voice: HashMap<Source, Incoming>,
    /// Sessions whose run answers a voice note, which is answered with one.
    spoken: HashSet<u64>,
    /// The window's card that went to the owner's DM as well, settled by
    /// whichever answer comes first. One at a time, so a typed answer there
    /// has one card it can be for.
    mirror: Option<Mirror>,
    /// Sessions with a card that went to the owner's DM, which hears how
    /// their run ends while nobody is at the window.
    away: HashSet<u64>,
    /// Plans the owner's DM was told wait in the window, by session, label
    /// and call, so it is told of each once however often the window asks.
    told: HashSet<(u64, String, String)>,
}

/// A card waiting in the window, sent to the owner's DM too.
struct Mirror {
    dm: Dm,
    /// The session it waits in.
    id: u64,
    /// The helper whose card it is, empty for the session's own turn: call
    /// ids repeat between helpers and turns, so they alone do not say which.
    label: String,
    call: ToolCall,
    /// What its buttons carry, its own, so a tap on an older copy whose
    /// buttons stayed never answers it.
    key: String,
    /// Its message in the DM, which the network gives back after the card
    /// may already have been answered at the desk.
    posted: Arc<Mutex<Posted>>,
}

/// Where a card sent to the owner's DM is.
#[derive(Debug, PartialEq)]
enum Posted {
    /// On its way.
    Going,
    /// In the chat, with the message its buttons are on, if it has any.
    Out(Option<Sent>),
    /// Settled in the window, so its copy is taken off once it is out.
    Settled,
    /// Never reached the chat, so it may be sent again.
    Failed,
}

impl Dispatcher {
    /// Whether a chat is talking in session `id`.
    pub fn holds(&self, id: u64) -> bool {
        self.sessions.values().any(|&session| session == id)
    }

    /// Whether chat `source` has a card up, which is its turn to answer.
    pub fn asking(&self, source: &Source) -> bool {
        self.asks.contains_key(source)
    }
}

/// What a chat app's messages need of the Letronna that hears them.
pub trait Host {
    /// What this host keeps for the chats.
    fn dispatcher(&mut self) -> &mut Dispatcher;
    /// The chat apps as they are set up now.
    fn config(&self) -> &Config;
    /// The same, to change: a chat pairing or choosing about voice notes.
    fn config_mut(&mut self) -> &mut Config;
    /// The Default mode setting, which a chat's run never goes above.
    fn default_mode(&self) -> Mode;
    /// Say `text` to chat `source`, with Allow and Deny under it when `card`
    /// is the id of the call it asks about.
    fn tell_card(&mut self, source: &Source, text: &str, card: Option<String>);
    /// Say `text` to chat `source`.
    fn tell(&mut self, source: &Source, text: &str) {
        self.tell_card(source, text, None);
    }
    /// Send what chat `source`'s run in session `id` came to, `text`, with
    /// the pictures and sound it made; `spoken` when it answers a voice
    /// note, which the host may answer with one.
    fn answer(&mut self, source: &Source, id: u64, text: &str, spoken: bool);
    /// A message from a chat that is not paired -- a Telegram chat other
    /// than the paired one, or a Slack member before one is paired -- as a
    /// try at the pairing code, if this host shows one; what came of it.
    fn pair(&mut self, message: &Incoming) -> Option<Attempt>;
    /// Show `error`, which went wrong with no chat to be told it.
    fn trouble(&mut self, error: String);
    /// Do `work`, which waits on the network, off this host's thread.
    fn spawn(&mut self, work: Box<dyn FnOnce() + Send>);
    /// Do `work` on a chat's message off this host's thread -- fetch its
    /// files, transcribe its voice note -- and `dispatch` what it gives back
    /// as if it had just arrived, so it meets every check again.
    fn after(&mut self, work: Box<dyn FnOnce() -> Incoming + Send>);
    /// The project session `id` works in, or the one a new session would
    /// get when there is none, where a chat's files are saved.
    fn folder(&self, id: Option<u64>) -> Option<PathBuf>;
    /// Whether session `id` is still there.
    fn exists(&self, id: u64) -> bool;
    /// Whether a run holds session `id`, going or waiting for a place.
    fn running(&self, id: u64) -> bool;
    /// Whether the host's own turn is going in session `id`.
    fn busy(&self, id: u64) -> bool;
    /// A new session titled `title` for a chat, by its id.
    fn open(&mut self, title: String) -> Result<u64, String>;
    /// Put `prompt` to session `id` as a run in `mode`, whose answer goes to
    /// chat `source`.
    fn run(&mut self, id: u64, mode: Mode, prompt: Message, source: Source, remote: Remote);
    /// Stop session `id`'s run.
    fn stop(&mut self, id: u64);
    /// Answer the card waiting in the window for call `call` of `label`, a
    /// helper's or empty for the session's own turn, in session `id` as a
    /// click there would, with what the owner's DM said to its copy: allow
    /// once, deny, or a question's answer, which opens with `FROM_DM` and is
    /// recorded as it comes. False when it waits there no longer. A
    /// Letronna with no window has none to answer.
    fn answer_card(&mut self, _id: u64, _label: &str, _call: &str, _answer: CardAnswer) -> bool {
        false
    }
    /// Whether Telegram and Discord hand their messages to this Letronna,
    /// rather than to another such as a `letronna serve`.
    fn hearing(&self) -> bool {
        hearing()
    }
}

/// Whether this Letronna holds `HEARING` for the data folder in use, and
/// so hears Telegram and Discord.
fn hearing() -> bool {
    let held = HEARING.lock().unwrap_or_else(PoisonError::into_inner);
    held.as_ref()
        .is_some_and(|(at, _)| *at == backend::data_dir())
}

/// Which Letronna hears the chat apps, as the record beside the lock says.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum By {
    Window,
    Serve,
}

impl By {
    /// The record's word for it.
    fn word(self) -> &'static str {
        match self {
            By::Window => "window",
            By::Serve => "serve",
        }
    }
}

/// The Letronna holding the lock, as far as its record says; a Letronna from
/// before the record, or one that could not write it, leaves both unknown.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Holder {
    pub by: Option<By>,
    pub pid: Option<u32>,
}

impl Holder {
    /// One line saying who answers the chat apps instead of this Letronna.
    pub fn line(&self) -> String {
        let who = match self.by {
            Some(By::Serve) => "letronna serve",
            Some(By::Window) => "another Letronna window",
            None => "another Letronna",
        };
        match self.pid {
            Some(pid) => format!("Chat apps answered by {who} (pid {pid})"),
            None => format!("Chat apps answered by {who}"),
        }
    }
}

/// Why this Letronna does not hear the chat apps.
#[derive(Debug, PartialEq)]
pub enum NotHeard {
    /// Another Letronna does, and may be waited for.
    Elsewhere(Holder),
    /// The system will not give the lock to anyone.
    Failed(String),
}

impl std::fmt::Display for NotHeard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotHeard::Elsewhere(_) => f.write_str(ELSEWHERE),
            NotHeard::Failed(error) => f.write_str(error),
        }
    }
}

/// Start hearing Telegram and Discord as set up in `config`, each message
/// going to `sender`, on threads that end once the flag handed back is set;
/// `None` when neither is set up to hear. Only one Letronna hears them at a
/// time, so this holds `HEARING` while either is set up, recorded as `by`,
/// and says who does when another Letronna holds it.
pub fn listen(
    config: &Config,
    sender: async_channel::Sender<Incoming>,
    by: By,
) -> Result<Option<Arc<AtomicBool>>, NotHeard> {
    let (telegram, discord) = (&config.telegram, &config.discord);
    let mut held = HEARING.lock().unwrap_or_else(PoisonError::into_inner);
    if !telegram.ready() && !discord.hears() {
        // So another Letronna can hear them.
        *held = None;
        return Ok(None);
    }
    let dir = backend::data_dir();
    if held.as_ref().is_none_or(|(at, _)| *at != dir) {
        // The last profile's goes first: its chat apps are not heard here now.
        *held = None;
        *held = Some((dir.clone(), take_lock(&dir, by)?));
    }
    let stop = Arc::new(AtomicBool::new(false));
    if telegram.ready() {
        messaging::poll(telegram.token.clone(), stop.clone(), sender.clone());
    }
    // A bot Letronna Cloud answers is left to it.
    if discord.hears() {
        messaging::discord_listen(discord.clone(), stop.clone(), sender);
    }
    Ok(Some(stop))
}

/// Wait on a thread for the Letronna that hears the chat apps to let them
/// go, then hold the lock as `by` and call `then`, which starts hearing them
/// as `listen` would. The system hands the lock over only once that
/// Letronna quits, crashes or lets the chat apps go, so it is never taken
/// from one still hearing them.
pub fn hear_when_free(by: By, then: impl FnOnce() + Send + 'static) -> Result<(), String> {
    let dir = backend::data_dir();
    wait_for_lock(&dir.clone(), by, move |file| {
        let mut held = HEARING.lock().unwrap_or_else(PoisonError::into_inner);
        // Taken meanwhile by `listen`, which already holds it for us.
        if held.is_none() {
            *held = Some((dir, file));
        }
        drop(held);
        then();
    })
}

/// Take the lock in data folder `dir` that says this Letronna hears the chat
/// apps, and record it as `by`, or say who holds it instead.
fn take_lock(dir: &Path, by: By) -> Result<File, NotHeard> {
    let refused = |e: std::io::Error| NotHeard::Failed(format!("Cannot hear the chat apps: {e}"));
    let file = crate::schedule::open_lock(&dir.join(LOCK)).map_err(refused)?;
    match file.try_lock() {
        Ok(()) => {
            record(dir, by);
            Ok(file)
        }
        Err(TryLockError::WouldBlock) => Err(NotHeard::Elsewhere(holder(dir))),
        Err(TryLockError::Error(e)) => Err(refused(e)),
    }
}

/// Block a thread of its own on the lock in data folder `dir`, and once it
/// is had, record it as `by` and hand it to `then`. Where the system cannot
/// wait on a lock, the thread gives up and nothing is heard until the chat
/// apps are set up again or Letronna starts again, rather than trying on a
/// timer.
fn wait_for_lock(
    dir: &Path,
    by: By,
    then: impl FnOnce(File) + Send + 'static,
) -> Result<(), String> {
    let refused = |e: std::io::Error| format!("Cannot wait for the chat apps: {e}");
    let file = crate::schedule::open_lock(&dir.join(LOCK)).map_err(refused)?;
    let dir = dir.to_path_buf();
    std::thread::Builder::new()
        .name("messaging-lock".into())
        .spawn(move || {
            if file.lock().is_err() {
                return;
            }
            record(&dir, by);
            then(file);
        })
        .map(|_| ())
        .map_err(refused)
}

/// Say beside the lock in data folder `dir` that this process, as `by`,
/// holds it. The record is a file of its own because Windows locks a file's
/// bytes against reading along with the file, so another Letronna could not
/// read it in the lock itself. Only a Letronna holding the lock writes it,
/// so while the lock is held it names the holder; one that cannot be
/// written only leaves the holder unknown. The last holder's record goes
/// first, so one that cannot be replaced is not left naming somebody else.
fn record(dir: &Path, by: By) {
    let _ = std::fs::remove_file(dir.join(RECORD));
    let _ = std::fs::write(
        dir.join(RECORD),
        format!("{} {}\n", by.word(), std::process::id()),
    );
}

/// Who holds the lock in data folder `dir`, as its record says.
fn holder(dir: &Path) -> Holder {
    let text = std::fs::read_to_string(dir.join(RECORD)).unwrap_or_default();
    let mut words = text.split_whitespace();
    let by = match words.next() {
        Some("serve") => Some(By::Serve),
        Some("window") => Some(By::Window),
        _ => None,
    };
    let pid = words.next().and_then(|pid| pid.parse().ok());
    Holder { by, pid }
}

/// Deal with a message from a chat app. A Telegram chat that is not paired
/// is heard only as a try at the pairing code, and gets no reply unless it
/// is right; Discord only ever delivers the owner's messages. Slack hands
/// on a DM from anyone until a member is paired, heard the same way.
pub fn dispatch(host: &mut impl Host, mut message: Incoming) {
    let source = message.source.clone();
    // A tap answers only as the owner's own words would.
    let may_answer = pairing::may_answer(host.config(), &message);
    // Where a card of the window's may be up.
    let dm = host.dispatcher().mirror.as_ref().map(|mirror| mirror.dm);
    let from_dm = may_answer && dm.is_some_and(|dm| dm.hears(&message));
    if let Some(tap) = message.press {
        if may_answer {
            press(host, source, tap, from_dm);
        }
        return;
    }
    if let Source::Telegram(chat) = source
        && host.config().telegram.chat != Some(chat)
    {
        if host.pair(&message) == Some(Attempt::Paired) {
            // Saved without restarting the poller: a new one starts without
            // the offset and could hear the code again, now as the paired
            // chat's first message.
            save(host);
            host.tell(
                &source,
                "Paired. Letronna answers this chat now, and only your answers \
                 count for what it asks here.",
            );
        }
        return;
    }
    // Slack hears only its paired member, so who may answer is who is heard
    // at all; before one is paired a DM is a try at the code. The host hears
    // the paired member alone from then on.
    if let Source::Slack { .. } = source
        && !may_answer
    {
        if host.pair(&message) == Some(Attempt::Paired) {
            save(host);
            host.tell(&source, "Paired. Send your message.");
        }
        return;
    }
    let session = host.dispatcher().sessions.get(&source).copied();
    // A thread reply that does not mention the app is a follow-up, or a
    // card's answer, only where that thread is a session already.
    if message.needs_session && session.is_none() {
        return;
    }
    // The chat's own run, wherever it is; its end says "Stopped.".
    let running = session.filter(|&id| host.running(id));
    if message.said.trim() == "/stop" {
        match running {
            Some(id) => host.stop(id),
            None => host.tell(&source, "Nothing is running."),
        }
        return;
    }
    // In a group anyone may write, but only the owner's own words answer;
    // anyone else's go on as any message would while a run goes.
    if may_answer && answer(host, &source, &message.said) {
        return;
    }
    if from_dm && answer_mirror(host, &message) {
        return;
    }
    // The chat's answer to whether its voice notes are transcribed is kept,
    // and the message that raised it taken in again. A message with a file
    // in it is no answer; its own voice note asks anew.
    if message.files.is_empty()
        && let Some(held) = host.dispatcher().voice.remove(&source)
    {
        files::choose_voice(host.config_mut(), &source, &message.said);
        save(host);
        return dispatch(host, held);
    }
    // Telegram sends /start when a chat first opens the bot.
    if matches!(message.said.trim(), "/new" | "/start") {
        host.dispatcher().sessions.remove(&source);
        host.tell(&source, "Started a new session.");
        return;
    }
    if let Some(id) = running {
        leave_for_run(id, &message.remote.place, &message.text);
        let noted =
            "Still working. This goes to the run in progress, which reads it before it answers.";
        host.tell(&source, noted);
        return;
    }
    // Fetched only now, from a chat allowed to talk here and with no run of
    // its own going, into the folder of the session they are for. None where
    // other people read the chat, whose files are not the user's to put there.
    if files::unfetched(&message) {
        let config = host.config().clone();
        let into = (!message.remote.shared)
            .then(|| host.folder(session))
            .flatten();
        return host.after(Box::new(move || files::fetch(&config, message, into)));
    }
    if files::unheard(&message) {
        match files::voice_choice(host.config(), &source) {
            None => return ask_voice(host, message),
            Some(true) => return host.after(Box::new(move || files::hear(message, true))),
            Some(false) => message = files::hear(message, false),
        }
    }
    // Carry on in the chat's session unless it has since been deleted,
    // beside whatever else is going.
    let session = session.filter(|&id| host.exists(id));
    // A turn of the host's own there already, which a run would race.
    if session.is_some_and(|id| host.busy(id)) {
        host.tell(
            &source,
            "Still working on something. Send this again once it is done.",
        );
        return;
    }
    let id = match session {
        Some(id) => id,
        None => match host.open(message.text.chars().take(40).collect()) {
            Ok(id) => id,
            Err(error) => {
                host.tell(&source, &format!("Did not start: {error}"));
                return;
            }
        },
    };
    let dispatcher = host.dispatcher();
    dispatcher.sessions.insert(source.clone(), id);
    // The message itself says it was a voice note: a flag kept per chat
    // would outlive a message held for its transcript or left for a run,
    // and give a later typed one a paid spoken answer.
    dispatcher.spoken.remove(&id);
    if message.spoken {
        dispatcher.spoken.insert(id);
    }
    // Ends what a command typed in this session granted its own turn.
    #[cfg(feature = "workflows")]
    for notice in crate::crew::prompt(id, &message.text, crate::crew::Source::ChatApp) {
        crate::inbox::push(id, notice);
    }
    let mode = mode_for(host.config(), &source, host.default_mode());
    let (text, images) = files::prompt(&message);
    let prompt = Message {
        role: "user".into(),
        // Someone else's words, which must not read the user's files by
        // naming them.
        content: crate::references::inert(&text),
        images,
        remote: true,
        ..Default::default()
    };
    host.run(id, mode, prompt, source, message.remote);
}

/// Save the chat apps' settings as they now are, showing why not when that
/// fails.
fn save(host: &mut impl Host) {
    if let Err(error) = messaging::save(host.config()) {
        host.trouble(error);
    }
}

/// A tap on a card's Allow or Deny. It answers the card only while that card
/// is the one waiting, as a typed yes or no would; a tap on an older card,
/// or a second tap, only loses its buttons. One in the owner's DM, `from_dm`,
/// may be on the window's card, unless the desk answered it first.
fn press(host: &mut impl Host, source: Source, tap: Press, from_dm: bool) {
    let dispatcher = host.dispatcher();
    let answers =
        (dispatcher.asks.get(&source)).is_some_and(|request| tap.answers(&request.call.id));
    let request = answers.then(|| dispatcher.asks.remove(&source)).flatten();
    let mirror = (from_dm && request.is_none())
        .then(|| dispatcher.mirror.take_if(|mirror| tap.answers(&mirror.key)))
        .flatten();
    let answers = match mirror {
        Some(mirror) => {
            let answer = match tap.allow {
                true => CardAnswer::Allow,
                false => CardAnswer::Deny(String::new()),
            };
            host.answer_card(mirror.id, &mirror.label, &mirror.call.id, answer)
        }
        None => answers,
    };
    let allow = tap.allow;
    let config = host.config().clone();
    host.spawn(Box::new(move || {
        let _ = messaging::settle(&config, &tap, answers);
    }));
    if let Some(request) = request {
        let decision = match allow {
            true => yes(&request),
            false => approvals::Decision::Deny(String::new()),
        };
        request.answer(decision);
    }
}

/// A yes to `request`: a memory save kept in the first place its card
/// offered, where it goes now, any other call run once.
fn yes(request: &approvals::Request) -> approvals::Decision {
    match request.places.first() {
        Some(&place) => approvals::Decision::Keep(place),
        None => approvals::Decision::Once,
    }
}

/// Hold a message with a voice note and ask its chat, once, whether voice
/// notes may be transcribed there. The price is looked up first, which takes
/// the network.
fn ask_voice(host: &mut impl Host, message: Incoming) {
    let source = message.source.clone();
    host.dispatcher().voice.insert(source.clone(), message);
    let config = host.config().clone();
    host.spawn(Box::new(move || {
        let question = files::voice_question();
        let _ = messaging::tell(&config, &source, &question, None);
    }));
}

/// The mode a run for chat `source` goes in. Unless the user has said
/// otherwise a remote message may look but not touch. With tools it gets
/// the Default mode `setting`, but never above Default: whoever writes in
/// the chat drives it, and the chip that would say what runs unasked is not
/// in front of them.
fn mode_for(config: &Config, source: &Source, setting: Mode) -> Mode {
    let allow_tools = match source {
        Source::Telegram(_) => config.telegram.allow_tools,
        Source::Discord(_) => config.discord.allow_tools,
        Source::Slack { .. } => config.slack.allow_tools,
    };
    if allow_tools {
        Mode::narrow(Mode::Default, setting)
    } else {
        Mode::Plan
    }
}

/// Leave chat message `text` from `place` for the run holding session `id`,
/// as a `Later` notice it reads before it answers.
fn leave_for_run(id: u64, place: &str, text: &str) {
    let notice = Notice {
        from: place.into(),
        when: When::Later,
        text: text.into(),
        files: Vec::new(),
    };
    inbox::push(id, notice);
}

/// A call of chat `source`'s run that needs a yes: its card goes to the
/// chat, whose next message or a tap on the card answers it, a memory
/// save's with where to keep it.
pub fn ask(host: &mut impl Host, source: Source, request: approvals::Request) {
    let (call, preview) = (&request.call, &request.preview);
    // A memory save shows the entry and where it may be kept.
    let text = match request.places.is_empty() {
        true => messaging::card_text(&call.name, &call.arguments, preview),
        false => messaging::keep_card_text(preview, &request.places),
    };
    // A question is answered in words; a tool is allowed or not.
    let card = (call.name != crate::tools::ASK_USER).then(|| call.id.clone());
    host.tell_card(&source, &text, card);
    // A card replaced is dropped, which declines it.
    host.dispatcher().asks.insert(source, request);
}

/// Answer the card chat `source` was sent with what it said, if one is up.
fn answer(host: &mut impl Host, source: &Source, said: &str) -> bool {
    let Some(request) = host.dispatcher().asks.remove(source) else {
        return false;
    };
    let call = &request.call;
    // A memory save's place, picked by its number.
    if let Some(place) = messaging::picked_place(said, &request.places) {
        request.answer(approvals::Decision::Keep(place));
        return true;
    }
    let decision = match messaging::card_answer(&call.name, &call.arguments, said) {
        CardAnswer::Allow => yes(&request),
        CardAnswer::Deny(reason) => approvals::Decision::Deny(reason),
        // A run beside the chat asks no questions, so words are a no.
        CardAnswer::Answer(_) => approvals::Decision::Deny(String::new()),
    };
    request.answer(decision);
    true
}

/// What came of putting a card waiting in the window to the owner's DM.
#[derive(Debug, PartialEq)]
pub enum Mirrored {
    /// Sent, to be answered there or at the desk, whichever is first.
    Sent,
    /// The DM was only told the session waits: only the window answers it.
    /// Asked again, the DM is not told again until it is settled.
    Told,
    /// Not yet: the DM has a card up, which one sent now would take the
    /// place of and so decline. Try again once it is answered.
    Later,
    /// Never: the switch is off, the owner has no private chat paired that
    /// this Letronna hears, or the session is a chat's own, whose cards go
    /// to that chat.
    No,
}

/// What a question's answer from the owner's DM opens with, as the window
/// records it: like a chat's prompt, it was written by whoever holds that
/// chat account, not at the desk.
pub const FROM_DM: &str = "[Answered in the owner's chat app, not at the desk]";

/// Said in the owner's DM under a card of the window's that waits there no
/// longer, where nothing else shows it: Telegram leaves no trace of buttons
/// taken off, and a question or a Slack card has none. Without it a later
/// yes there would go on as a new message to the DM's own session.
const GONE: &str = "That card is no longer waiting in the window.";

/// The owner's DM a card of the window's goes to while the switch is on.
/// Telegram and Discord hand their messages to whichever Letronna holds
/// `HEARING`, and a `letronna serve` holding it has no card to answer and
/// would take the owner's yes for a new chat, so they count only while
/// this Letronna hears them; the window always hears its Slack.
fn phone(host: &impl Host) -> Option<Dm> {
    let config = host.config();
    config.phone_cards?;
    messaging::owner_dm(config, host.hearing())
}

/// Put session `id`'s card for `call` of `label`, a helper's or empty for
/// the session's own turn, showing `preview`, to the owner's DM as well,
/// once it has waited in the window, titled `title`, with nobody there. A
/// tool can be allowed once or denied there and a question answered in
/// words; a plan to approve only says the session waits.
pub fn mirror(
    host: &mut impl Host,
    id: u64,
    label: &str,
    title: &str,
    call: &ToolCall,
    preview: &str,
) -> Mirrored {
    let Some(dm) = phone(host) else {
        return Mirrored::No;
    };
    let dispatcher = host.dispatcher();
    if dispatcher.holds(id) {
        return Mirrored::No;
    }
    let card = (id, label.to_owned(), call.id.clone());
    if dispatcher.told.contains(&card) {
        return Mirrored::Told;
    }
    if (dispatcher.mirror.as_ref()).is_some_and(|up| *lock(&up.posted) == Posted::Failed) {
        dispatcher.mirror = None;
    }
    if let Some(up) = &dispatcher.mirror {
        return match up.id == id && up.label == label && up.call.id == call.id {
            true => Mirrored::Sent,
            false => Mirrored::Later,
        };
    }
    let busy = (dispatcher.asks.keys())
        .chain(dispatcher.voice.keys())
        .any(|source| dm.may_be(source));
    if busy {
        return Mirrored::Later;
    }
    dispatcher.away.insert(id);
    let config = host.config().clone();
    let waiting = format!("\"{title}\" is waiting in the window.");
    if call.name == crate::tools::EXIT_PLAN {
        host.dispatcher().told.insert(card);
        host.spawn(Box::new(move || {
            let _ = messaging::tell_dm(&config, dm, &waiting, None);
        }));
        return Mirrored::Told;
    }
    let text = messaging::card_text(&call.name, &call.arguments, preview);
    let text = format!("{waiting}\n\n{text}");
    let key = fresh_key();
    // A question is answered in words; a tool is allowed or not.
    let buttons = (call.name != crate::tools::ASK_USER).then(|| key.clone());
    let posted = Arc::new(Mutex::new(Posted::Going));
    host.dispatcher().mirror = Some(Mirror {
        dm,
        id,
        label: label.to_owned(),
        call: call.clone(),
        key,
        posted: posted.clone(),
    });
    host.spawn(Box::new(move || {
        let Ok(sent) = messaging::tell_dm(&config, dm, &text, buttons.as_deref()) else {
            *lock(&posted) = Posted::Failed;
            return;
        };
        if let Some(sent) = arrived(&posted, sent) {
            take_off(&config, dm, sent.as_ref());
        }
    }));
    Mirrored::Sent
}

/// A key for a card's buttons that no earlier card's carry, even one sent
/// before Letronna last started: the time, to the nanosecond.
fn fresh_key() -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    format!("w{}", now.unwrap_or_default().as_nanos())
}

/// Answer the window's card in the owner's DM with what `message` said
/// there, if it is up there.
fn answer_mirror(host: &mut impl Host, message: &Incoming) -> bool {
    let source = &message.source;
    // A voice note or file with no words says nothing to the card, and a
    // voice question asked there meanwhile has the next words first.
    let wordless = message.said.trim().is_empty() && (message.spoken || !message.files.is_empty());
    if wordless || host.dispatcher().voice.contains_key(source) {
        return false;
    }
    // Words written before the card reached the chat were not about it.
    let out = |mirror: &mut Mirror| matches!(*lock(&mirror.posted), Posted::Out(_));
    let Some(mirror) = host.dispatcher().mirror.take_if(out) else {
        return false;
    };
    let call = &mirror.call;
    let answer = match messaging::card_answer(&call.name, &call.arguments, &message.said) {
        CardAnswer::Answer(text) => CardAnswer::Answer(format!("{FROM_DM}\n{text}")),
        answer => answer,
    };
    if !host.answer_card(mirror.id, &mirror.label, &call.id, answer) {
        host.tell(source, GONE);
    }
    true
}

/// The window's card for call `call` of `label` in session `id` went --
/// answered at the desk, or dropped with its run -- so its copy in the
/// owner's DM loses its buttons and says so, and an answer there does
/// nothing. A plan the DM was only told of may be told of again.
pub fn settled_in_window(host: &mut impl Host, id: u64, label: &str, call: &str) {
    let dispatcher = host.dispatcher();
    dispatcher
        .told
        .remove(&(id, label.to_owned(), call.to_owned()));
    let mirrored =
        |mirror: &mut Mirror| mirror.id == id && mirror.label == label && mirror.call.id == call;
    if let Some(mirror) = dispatcher.mirror.take_if(mirrored) {
        unbutton(host, &mirror);
    }
}

/// Session `id`'s run in the window ended, as `note` says. While nobody is
/// there, `away`, the owner's DM, sent a card of the session's, is told how,
/// once; a card of its still up there loses its buttons.
pub fn window_ended(host: &mut impl Host, id: u64, note: &Note, away: bool) {
    let dispatcher = host.dispatcher();
    dispatcher.told.retain(|(session, ..)| *session != id);
    if let Some(mirror) = dispatcher.mirror.take_if(|mirror| mirror.id == id) {
        unbutton(host, &mirror);
    }
    let sent = host.dispatcher().away.remove(&id);
    let Some(dm) = phone(host).filter(|_| sent && away) else {
        return;
    };
    let config = host.config().clone();
    let line = format!("{}\n{}", note.title, note.body);
    host.spawn(Box::new(move || {
        let _ = messaging::tell_dm(&config, dm, &line, None);
    }));
}

/// Take `mirror`'s copy in the DM off, now or as soon as it is out.
fn unbutton(host: &mut impl Host, mirror: &Mirror) {
    let was = std::mem::replace(&mut *lock(&mirror.posted), Posted::Settled);
    if let Posted::Out(sent) = was {
        let (config, dm) = (host.config().clone(), mirror.dm);
        host.spawn(Box::new(move || take_off(&config, dm, sent.as_ref())));
    }
}

/// Take the buttons off a card of the window's in `dm`, `sent` if it has
/// any, and say there it waits no longer unless Discord's buttons do.
fn take_off(config: &Config, dm: Dm, sent: Option<&Sent>) {
    if let Some(sent) = sent {
        let _ = messaging::unbutton(config, sent);
    }
    if !matches!(sent, Some(Sent::Discord { .. })) {
        let _ = messaging::tell_dm(config, dm, GONE, None);
    }
}

/// A card's message `sent` came back from the chat app: kept, or handed
/// back to be taken off when the window settled the card meanwhile.
fn arrived(posted: &Mutex<Posted>, sent: Option<Sent>) -> Option<Option<Sent>> {
    let mut posted = lock(posted);
    if *posted == Posted::Settled {
        return Some(sent);
    }
    *posted = Posted::Out(sent);
    None
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    // Nothing that can panic runs under these locks, so a poisoned one is
    // still whole.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Chat `source`'s run in session `id` ended, saying `text`, which the chat
/// is told; read aloud too where it `answered` a voice note, rather than
/// stopping or failing.
pub fn ended(host: &mut impl Host, source: &Source, id: u64, text: &str, answered: bool) {
    let dispatcher = host.dispatcher();
    // A card still up in the chat is for a call nothing waits on now.
    dispatcher.asks.remove(source);
    let spoken = dispatcher.spoken.remove(&id) && answered;
    host.answer(source, id, text, spoken);
}

/// What a run came to, as a chat app is told it.
pub fn said(report: &driver::Report) -> String {
    match &report.status {
        Outcome::Failed(error) => format!("Did not finish: {error}"),
        Outcome::Stopped => "Stopped.".into(),
        _ if report.text.trim().is_empty() => "That did not produce an answer.".into(),
        _ => report.text.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        By, Dispatcher, FROM_DM, GONE, Holder, Host, Mirrored, NotHeard, Posted, RECORD,
        answer_mirror, arrived, ask, dispatch, ended, holder, mirror, settled_in_window, take_lock,
        wait_for_lock, window_ended,
    };
    use crate::approvals::{self, tests::SERVING};
    use crate::chat::{Message, Remote, ToolCall};
    use crate::gate::Mode;
    use crate::inbox::{self, When};
    use crate::messaging::pairing::Attempt;
    use crate::messaging::tests::tap;
    use crate::messaging::{self, CardAnswer, Config, Dm, Incoming, Sent, Source};
    use crate::notify::Note;
    use crate::tools::ToolContext;
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::mpsc::{self, Receiver};
    use std::time::Duration;

    /// The Telegram chat the tests pair.
    const PAIRED: i64 = 42;
    /// The paired chat's owner, who writes every message the tests send.
    const OWNER: i64 = 1001;

    /// A host that keeps what it was asked to do.
    #[derive(Default)]
    struct Fake {
        remote: Dispatcher,
        config: Config,
        setting: Mode,
        told: Vec<(Source, String)>,
        pairing: Vec<(i64, String)>,
        sessions: HashSet<u64>,
        running: HashSet<u64>,
        busy: Option<u64>,
        runs: Vec<(u64, Mode, String)>,
        stopped: Vec<u64>,
        /// Each answer, and whether it was to be spoken.
        answers: Vec<(String, bool)>,
        troubles: Vec<String>,
        /// The id the next session opened gets.
        next: u64,
        /// Whether the window's cards still wait there to be answered.
        waiting: bool,
        /// Each of the window's cards answered from a chat: its session,
        /// label and call, and what was said.
        answered: Vec<(u64, String, String, String)>,
        /// Whether another Letronna hears Telegram and Discord.
        elsewhere: bool,
        /// How much work went off to the network.
        spawned: usize,
    }

    impl Host for Fake {
        fn dispatcher(&mut self) -> &mut Dispatcher {
            &mut self.remote
        }
        fn config(&self) -> &Config {
            &self.config
        }
        fn config_mut(&mut self) -> &mut Config {
            &mut self.config
        }
        fn default_mode(&self) -> Mode {
            self.setting
        }
        fn tell_card(&mut self, source: &Source, text: &str, _: Option<String>) {
            self.told.push((source.clone(), text.into()));
        }
        fn answer(&mut self, source: &Source, _: u64, text: &str, spoken: bool) {
            self.answers.push((text.into(), spoken));
            self.tell(source, text);
        }
        /// Never pairs, so nothing here saves the settings.
        fn pair(&mut self, message: &Incoming) -> Option<Attempt> {
            let chat = match &message.source {
                Source::Telegram(chat) => *chat,
                _ => 0,
            };
            self.pairing.push((chat, message.from.clone()));
            None
        }
        fn trouble(&mut self, error: String) {
            self.troubles.push(error);
        }
        /// Nothing here waits on the network.
        fn spawn(&mut self, _: Box<dyn FnOnce() + Send>) {
            self.spawned += 1;
        }
        fn after(&mut self, work: Box<dyn FnOnce() -> Incoming + Send>) {
            dispatch(self, work());
        }
        fn folder(&self, _: Option<u64>) -> Option<PathBuf> {
            None
        }
        fn exists(&self, id: u64) -> bool {
            self.sessions.contains(&id)
        }
        fn running(&self, id: u64) -> bool {
            self.running.contains(&id)
        }
        fn busy(&self, id: u64) -> bool {
            self.busy == Some(id)
        }
        fn open(&mut self, _title: String) -> Result<u64, String> {
            self.next += 1;
            self.sessions.insert(self.next);
            Ok(self.next)
        }
        fn run(&mut self, id: u64, mode: Mode, prompt: Message, _: Source, _: Remote) {
            assert!(prompt.remote, "a chat's prompt is marked as remote");
            self.runs.push((id, mode, prompt.content));
        }
        fn stop(&mut self, id: u64) {
            self.stopped.push(id);
        }
        fn answer_card(&mut self, id: u64, label: &str, call: &str, answer: CardAnswer) -> bool {
            let said = match answer {
                CardAnswer::Allow => "allow".to_owned(),
                CardAnswer::Deny(why) => format!("deny {why}").trim().to_owned(),
                CardAnswer::Answer(answer) => format!("answer {answer}"),
            };
            self.answered.push((id, label.into(), call.into(), said));
            self.waiting
        }
        fn hearing(&self) -> bool {
            !self.elsewhere
        }
    }

    /// A host with chat `PAIRED` paired; `next` numbers its sessions.
    fn host(next: u64) -> Fake {
        let mut fake = Fake {
            next,
            ..Default::default()
        };
        fake.config.telegram.chat = Some(PAIRED);
        fake.config.telegram.owner = Some(OWNER);
        fake
    }

    fn from(chat: i64, said: &str) -> Incoming {
        Incoming {
            source: Source::Telegram(chat),
            from: "user_a".into(),
            text: said.into(),
            said: said.into(),
            author: Some(OWNER.to_string()),
            files: Vec::new(),
            remote: Remote {
                place: "a Telegram chat".into(),
                ..Default::default()
            },
            press: None,
            needs_session: false,
            spoken: false,
        }
    }

    /// A host whose paired Telegram chat is the owner's own, with cards
    /// left waiting in its window sent there.
    fn dm_host() -> Fake {
        let mut fake = host(10);
        fake.config.telegram.enabled = true;
        fake.config.telegram.token = "123:abc".into();
        fake.config.telegram.chat = Some(OWNER);
        fake.config.phone_cards = Some(messaging::PHONE_CARDS);
        fake.waiting = true;
        fake
    }

    /// A call waiting on the user in the window.
    fn window_call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    /// The window's card sent to the DM came back from the chat app as
    /// message 7, with buttons when it has any.
    fn arrives(fake: &Fake) -> std::sync::Arc<std::sync::Mutex<Posted>> {
        let posted = fake.remote.mirror.as_ref().unwrap().posted.clone();
        let buttons = (fake.remote.mirror.as_ref())
            .is_some_and(|mirror| mirror.call.name != crate::tools::ASK_USER);
        let sent = buttons.then_some(Sent::Telegram {
            chat: OWNER,
            message: 7,
        });
        assert_eq!(arrived(&posted, sent), None);
        posted
    }

    /// A tap on the Allow or Deny, `answer`, of the window's card up in the DM.
    fn tap_mirror(fake: &Fake, answer: &str) -> Incoming {
        let key = &fake.remote.mirror.as_ref().unwrap().key;
        tap(OWNER, OWNER, &format!("{answer}:{key}"))
    }

    /// One of the window's cards answered from the DM, as the fake keeps it.
    fn answered(id: u64, label: &str, call: &str, said: &str) -> (u64, String, String, String) {
        (id, label.into(), call.into(), said.into())
    }

    fn last_told(fake: &Fake) -> &str {
        fake.told
            .last()
            .map(|(_, text)| text.as_str())
            .unwrap_or("")
    }

    /// An empty scratch data folder of the test's own, removed when dropped.
    fn scratch(name: &str) -> crate::store::tests::Scratch {
        crate::store::tests::Scratch::new(&format!("remote-{name}"))
    }

    #[test]
    fn only_one_letronna_hears_the_chat_apps_until_it_lets_go() {
        let dir = scratch("hearing");
        let first = take_lock(&dir, By::Serve).unwrap();
        let second = take_lock(&dir, By::Window).map(|_| ()).unwrap_err();
        assert!(
            second
                .to_string()
                .starts_with("Another Letronna already answers the chat apps")
        );
        drop(first);
        assert!(
            take_lock(&dir, By::Window).is_ok(),
            "let go when the first went"
        );
    }

    #[test]
    fn a_window_hears_the_chat_apps_when_free_and_defers_to_whoever_holds_them() {
        let dir = scratch("defer");
        let serve = Holder {
            by: Some(By::Serve),
            pid: Some(std::process::id()),
        };
        let first = take_lock(&dir, By::Serve).unwrap();
        assert_eq!(
            take_lock(&dir, By::Window).map(|_| ()),
            Err(NotHeard::Elsewhere(serve.clone()))
        );
        drop(first);
        let window = take_lock(&dir, By::Window).unwrap();
        assert_eq!(holder(&dir).by, Some(By::Window), "the window recorded");
        drop(window);
        // As a serve from before the record, which leaves it as it was.
        let old = take_lock(&dir, By::Serve).unwrap();
        std::fs::remove_file(dir.join(RECORD)).unwrap();
        assert_eq!(
            take_lock(&dir, By::Window).map(|_| ()),
            Err(NotHeard::Elsewhere(Holder::default()))
        );
        drop(old);
    }

    #[test]
    fn the_record_names_the_holder_and_its_pid_and_an_old_one_names_nobody() {
        let dir = scratch("record");
        assert_eq!(holder(&dir), Holder::default(), "no record at all");
        let held = take_lock(&dir, By::Serve).unwrap();
        let written = std::fs::read_to_string(dir.join(RECORD)).unwrap();
        assert_eq!(written, format!("serve {}\n", std::process::id()));
        let found = holder(&dir);
        assert_eq!(found.pid, Some(std::process::id()));
        assert_eq!(
            found.line(),
            format!(
                "Chat apps answered by letronna serve (pid {})",
                std::process::id()
            )
        );
        drop(held);
        std::fs::write(dir.join(RECORD), "").unwrap();
        assert_eq!(
            holder(&dir).line(),
            "Chat apps answered by another Letronna"
        );
        std::fs::write(dir.join(RECORD), "window").unwrap();
        assert_eq!(
            holder(&dir).line(),
            "Chat apps answered by another Letronna window"
        );
    }

    #[test]
    fn a_waiting_window_takes_the_chat_apps_over_only_once_serve_lets_go() {
        let dir = scratch("hand-over");
        let serve = take_lock(&dir, By::Serve).unwrap();
        let (tx, rx) = mpsc::channel();
        wait_for_lock(&dir, By::Window, move |file| {
            let _ = tx.send(file);
        })
        .unwrap();
        assert!(
            rx.recv_timeout(Duration::from_millis(300)).is_err(),
            "not taken from a serve still holding it"
        );
        assert_eq!(holder(&dir).by, Some(By::Serve));
        drop(serve);
        let window = rx.recv_timeout(Duration::from_secs(10)).unwrap();
        assert_eq!(holder(&dir).by, Some(By::Window));
        assert!(
            matches!(take_lock(&dir, By::Serve), Err(NotHeard::Elsewhere(_))),
            "a serve started now defers to the window"
        );
        drop(window);
    }

    #[test]
    fn a_chat_that_is_not_paired_is_heard_only_as_a_try_at_pairing() {
        let mut fake = host(10);
        dispatch(&mut fake, from(7, "run the tests"));
        assert_eq!(fake.pairing, vec![(7, "user_a".to_owned())]);
        assert!(fake.runs.is_empty() && fake.told.is_empty());
    }

    #[test]
    fn a_chat_carries_on_in_its_session_until_it_says_new() {
        let mut fake = host(10);
        dispatch(&mut fake, from(PAIRED, "hello"));
        dispatch(&mut fake, from(PAIRED, "and again"));
        assert_eq!(fake.runs[0].0, 11);
        assert_eq!(fake.runs[1].0, 11, "a follow-up keeps its session");
        assert!(fake.remote.holds(11));

        dispatch(&mut fake, from(PAIRED, "/new"));
        assert_eq!(last_told(&fake), "Started a new session.");
        assert!(!fake.remote.holds(11));
        dispatch(&mut fake, from(PAIRED, "something else"));
        assert_eq!(fake.runs[2].0, 12);

        // A session deleted meanwhile is not written to again.
        fake.sessions.remove(&12);
        dispatch(&mut fake, from(PAIRED, "still there?"));
        assert_eq!(fake.runs[3].0, 13);
    }

    #[test]
    fn a_chat_looks_but_does_not_touch_unless_tools_are_allowed_and_never_above_default() {
        let mut fake = host(10);
        fake.setting = Mode::Bypass;
        dispatch(&mut fake, from(PAIRED, "look"));
        assert_eq!(fake.runs[0].1, Mode::Plan);

        fake.config.telegram.allow_tools = true;
        dispatch(&mut fake, from(PAIRED, "touch"));
        assert_eq!(fake.runs[1].1, Mode::Default);
        fake.setting = Mode::Plan;
        dispatch(&mut fake, from(PAIRED, "touch less"));
        assert_eq!(fake.runs[2].1, Mode::Plan);
    }

    #[test]
    fn a_message_for_a_session_a_run_holds_is_left_as_a_later_notice() {
        let _bell = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        // Far above any session the other tests use, since the inbox is shared.
        let mut fake = host(9_000_000_100);
        dispatch(&mut fake, from(PAIRED, "run the tests"));
        let busy = fake.runs[0].0;
        fake.running.insert(busy);
        dispatch(&mut fake, from(PAIRED, "and the docs"));
        assert_eq!(fake.runs.len(), 1, "nothing races the run");
        assert!(last_told(&fake).starts_with("Still working. This goes to the run"));

        // Not taken in the middle of a turn, only once the run looks before it answers.
        assert!(inbox::take(busy, false).is_empty());
        let waiting = inbox::take(busy, true);
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].when, When::Later);
        assert_eq!(waiting[0].text, "and the docs");
        assert_eq!(waiting[0].from, "a Telegram chat");
    }

    #[test]
    fn the_hosts_own_turn_in_the_session_turns_a_message_away() {
        let mut fake = host(10);
        dispatch(&mut fake, from(PAIRED, "hello"));
        fake.busy = Some(11);
        dispatch(&mut fake, from(PAIRED, "again"));
        assert_eq!(fake.runs.len(), 1);
        assert!(last_told(&fake).starts_with("Still working on something."));
    }

    #[test]
    fn stop_stops_the_chats_own_run_and_says_so_when_none_is_going() {
        let mut fake = host(10);
        dispatch(&mut fake, from(PAIRED, "/stop"));
        assert_eq!(last_told(&fake), "Nothing is running.");

        dispatch(&mut fake, from(PAIRED, "hello"));
        fake.running.insert(11);
        dispatch(&mut fake, from(PAIRED, " /stop "));
        assert_eq!(fake.stopped, vec![11]);
        assert_eq!(fake.runs.len(), 1, "a stop is not a prompt");
    }

    /// A run's call asking for a yes on a thread of its own, as a run's gate
    /// does, and what it gets back: `Ok` for a yes, with the arguments it was
    /// changed to, if any.
    fn asking(root: u64) -> Receiver<Result<approvals::Allowed, String>> {
        let call = ToolCall {
            id: "1".into(),
            name: "run_command".into(),
            arguments: r#"{"command": "cargo test"}"#.into(),
        };
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let ctx = ToolContext::default();
            let _ = tx.send(approvals::ask(root, root, "chat", &call, &ctx, "", false));
        });
        rx
    }

    #[test]
    fn a_runs_card_goes_to_its_chat_and_the_chats_next_message_answers_it() {
        let _serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
        let served = approvals::serve();
        let wait = Duration::from_secs(10);
        let mut fake = host(10);
        dispatch(&mut fake, from(PAIRED, "run the tests"));
        fake.running.insert(11);
        let chat = Source::Telegram(PAIRED);

        let allowed = asking(11);
        ask(&mut fake, chat.clone(), served.recv_timeout(wait).unwrap());
        assert!(last_told(&fake).starts_with("Letronna wants to use run_command"));
        dispatch(&mut fake, from(PAIRED, "yes"));
        assert!(allowed.recv_timeout(wait).unwrap().is_ok());

        // Anything but a plain yes declines, and neither answer is a prompt.
        let declined = asking(11);
        ask(&mut fake, chat.clone(), served.recv_timeout(wait).unwrap());
        dispatch(&mut fake, from(PAIRED, "what does it do?"));
        assert!(declined.recv_timeout(wait).unwrap().is_err());
        assert_eq!(fake.runs.len(), 1);

        // A card still up when its run ends is declined, and the chat hears the end.
        let dropped = asking(11);
        ask(&mut fake, chat.clone(), served.recv_timeout(wait).unwrap());
        ended(&mut fake, &chat, 11, "Stopped.", false);
        assert!(dropped.recv_timeout(wait).unwrap().is_err());
        assert_eq!(last_told(&fake), "Stopped.");
        drop(served);
    }

    #[test]
    fn a_memory_save_goes_to_its_chat_with_its_entry_and_is_kept_where_answered() {
        use crate::memory::{self, Scope};
        let _serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
        let served = approvals::serve();
        let wait = Duration::from_secs(10);
        // An id no other test's session has, as a scope is kept per process.
        let session = 9_700_001;
        memory::choose_scope(session, Some(Scope::Auto));
        let saving = move || {
            let call = ToolCall {
                id: "1".into(),
                name: "memory".into(),
                arguments: r#"{"action": "add", "target": "user", "content": "Uses pnpm."}"#.into(),
            };
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let ctx = ToolContext {
                    conversation: session,
                    ..Default::default()
                };
                let asked = approvals::ask(session, session, "chat", &call, &ctx, "", false);
                let _ = tx.send(asked.map(|allowed| allowed.keep_in));
            });
            rx
        };
        let mut fake = host(10);
        dispatch(&mut fake, from(PAIRED, "remember I use pnpm"));
        fake.running.insert(11);
        let chat = Source::Telegram(PAIRED);

        let picked = saving();
        ask(&mut fake, chat.clone(), served.recv_timeout(wait).unwrap());
        let card = last_told(&fake);
        assert!(card.contains("Add: Uses pnpm."), "{card}");
        assert!(
            card.contains("1. Save everywhere\n2. Save for this chat only"),
            "{card}"
        );
        dispatch(&mut fake, from(PAIRED, "2"));
        assert_eq!(picked.recv_timeout(wait).unwrap(), Ok(Some(Scope::Chat)));

        // A yes keeps it where it goes now, the first place.
        let first = saving();
        ask(&mut fake, chat.clone(), served.recv_timeout(wait).unwrap());
        dispatch(&mut fake, from(PAIRED, "yes"));
        assert_eq!(first.recv_timeout(wait).unwrap(), Ok(Some(Scope::Auto)));

        let refused = saving();
        ask(&mut fake, chat.clone(), served.recv_timeout(wait).unwrap());
        dispatch(&mut fake, from(PAIRED, "no, not that"));
        assert!(refused.recv_timeout(wait).unwrap().is_err());
        // None of the answers is a prompt.
        assert_eq!(fake.runs.len(), 1);
        memory::forget(session);
        drop(served);
    }

    #[test]
    fn in_a_group_only_the_owners_words_answer_a_card() {
        let _serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
        let served = approvals::serve();
        let wait = Duration::from_secs(10);
        let mut fake = host(10);
        dispatch(&mut fake, from(PAIRED, "run the tests"));
        let chat = Source::Telegram(PAIRED);

        let allowed = asking(11);
        ask(&mut fake, chat.clone(), served.recv_timeout(wait).unwrap());
        let mut member = from(PAIRED, "yes");
        member.author = Some("7".into());
        dispatch(&mut fake, member);
        assert!(
            fake.remote.asking(&chat),
            "a member's yes leaves the card up"
        );
        assert_eq!(fake.runs[1].2, "yes", "and goes on as any message would");

        dispatch(&mut fake, from(PAIRED, "yes"));
        assert!(!fake.remote.asking(&chat));
        assert!(allowed.recv_timeout(wait).unwrap().is_ok());
        drop(served);
    }

    #[test]
    fn a_slack_member_before_one_is_paired_is_heard_only_as_a_try_at_pairing() {
        let mut fake = host(10);
        let mut message = from(PAIRED, "run the tests");
        message.source = Source::Slack {
            channel: "D1".into(),
            thread: None,
        };
        message.author = Some("U1".into());
        dispatch(&mut fake, message);
        assert_eq!(fake.pairing.len(), 1);
        assert!(fake.runs.is_empty() && fake.told.is_empty());
    }

    #[test]
    fn a_voice_note_is_answered_aloud_only_when_its_run_answered_it() {
        let mut fake = host(10);
        let chat = Source::Telegram(PAIRED);
        let spoken = || Incoming {
            spoken: true,
            ..from(PAIRED, "what is on today")
        };
        dispatch(&mut fake, spoken());
        ended(&mut fake, &chat, 11, "Two meetings.", true);
        dispatch(&mut fake, spoken());
        ended(&mut fake, &chat, 11, "Stopped.", false);
        // A typed message after a voice note gets a typed answer.
        dispatch(&mut fake, from(PAIRED, "and tomorrow"));
        ended(&mut fake, &chat, 11, "Nothing.", true);
        let spoken: Vec<bool> = fake.answers.iter().map(|(_, spoken)| *spoken).collect();
        assert_eq!(spoken, [true, false, false]);
    }

    #[test]
    fn a_group_chat_pairing_is_never_a_target() {
        let mut fake = dm_host();
        let call = window_call("w1", "run_command", r#"{"command": "cargo test"}"#);
        assert_eq!(
            messaging::owner_dm(&fake.config, true),
            Some(Dm::Telegram(OWNER))
        );
        // A group's ID is negative, and many people read it.
        fake.config.telegram.chat = Some(-100);
        assert_eq!(messaging::owner_dm(&fake.config, true), None);
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::No);
        // A Discord webhook posts into a server channel; only the bot's DM
        // with its owner will do.
        fake.config.discord.enabled = true;
        fake.config.discord.webhook = "https://discord.com/api/webhooks/1/t".into();
        assert_eq!(messaging::owner_dm(&fake.config, true), None);
        fake.config.discord.bot_token = "t".into();
        fake.config.discord.owner = "5".into();
        assert_eq!(messaging::owner_dm(&fake.config, true), Some(Dm::Discord));
        // And nothing goes with the switch off.
        fake.config.phone_cards = None;
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::No);
        assert_eq!(fake.spawned, 0);
        assert!(fake.remote.mirror.is_none() && fake.remote.away.is_empty());
    }

    #[test]
    fn a_dm_card_already_up_is_never_replaced() {
        let _serving = SERVING.lock().unwrap_or_else(|e| e.into_inner());
        let served = approvals::serve();
        let wait = Duration::from_secs(10);
        let mut fake = dm_host();
        let dm = Source::Telegram(OWNER);
        dispatch(&mut fake, from(OWNER, "run the tests"));
        fake.running.insert(11);
        let allowed = asking(11);
        ask(&mut fake, dm.clone(), served.recv_timeout(wait).unwrap());

        let call = window_call("w1", "run_command", r#"{"command": "cargo build"}"#);
        assert_eq!(
            mirror(&mut fake, 5, "", "Build", &call, ""),
            Mirrored::Later
        );
        assert!(fake.remote.mirror.is_none());
        dispatch(&mut fake, from(OWNER, "yes"));
        assert!(allowed.recv_timeout(wait).unwrap().is_ok());
        assert!(fake.answered.is_empty(), "the yes was the DM's own card's");

        assert_eq!(mirror(&mut fake, 5, "", "Build", &call, ""), Mirrored::Sent);
        let spawned = fake.spawned;
        assert_eq!(mirror(&mut fake, 5, "", "Build", &call, ""), Mirrored::Sent);
        assert_eq!(fake.spawned, spawned, "not sent twice");
        let other = window_call("w2", "run_command", r#"{"command": "ls"}"#);
        assert_eq!(
            mirror(&mut fake, 6, "", "List", &other, ""),
            Mirrored::Later
        );
        drop(served);
    }

    #[test]
    fn a_second_answer_does_nothing() {
        let mut fake = dm_host();
        let call = window_call("w1", "run_command", r#"{"command": "cargo test"}"#);
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::Sent);
        arrives(&fake);
        let (allow, deny) = (tap_mirror(&fake, "allow"), tap_mirror(&fake, "deny"));
        dispatch(&mut fake, allow);
        dispatch(&mut fake, deny);
        dispatch(&mut fake, from(OWNER, "no"));
        assert_eq!(fake.answered, [answered(5, "", "w1", "allow")]);
    }

    #[test]
    fn a_window_question_is_answered_from_the_dm_once_it_is_there() {
        let mut fake = dm_host();
        let args = r#"{"question": "Which branch?", "options": ["main", "dev"]}"#;
        let call = window_call("q1", crate::tools::ASK_USER, args);
        assert_eq!(
            mirror(&mut fake, 5, "", "Release", &call, ""),
            Mirrored::Sent
        );
        // Written before the question reached the chat, it was not about it.
        dispatch(&mut fake, from(OWNER, "2"));
        assert!(fake.answered.is_empty());
        arrives(&fake);
        // Only the owner's own words answer, and a voice note's words are
        // none of them.
        let mut forward = from(OWNER, "1");
        forward.author = None;
        dispatch(&mut fake, forward);
        let voice = Incoming {
            spoken: true,
            ..from(OWNER, "")
        };
        assert!(!answer_mirror(&mut fake, &voice));
        dispatch(&mut fake, from(OWNER, "2"));
        // The window records it as written in the chat app.
        let said = format!("answer {FROM_DM}\ndev");
        assert_eq!(fake.answered, [answered(5, "", "q1", &said)]);
    }

    #[test]
    fn a_desk_answer_takes_the_phone_buttons_off() {
        let mut fake = dm_host();
        let call = window_call("w1", "run_command", r#"{"command": "cargo test"}"#);
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::Sent);
        let posted = arrives(&fake);
        let allow = tap_mirror(&fake, "allow");
        let spawned = fake.spawned;
        settled_in_window(&mut fake, 5, "", "w1");
        assert_eq!(fake.spawned, spawned + 1, "the buttons come off");
        assert_eq!(*posted.lock().unwrap(), Posted::Settled);
        dispatch(&mut fake, allow);
        assert!(fake.answered.is_empty());

        // Answered before the card reached the chat, it loses them as it does.
        let call = window_call("w2", "run_command", r#"{"command": "ls"}"#);
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::Sent);
        let posted = fake.remote.mirror.as_ref().unwrap().posted.clone();
        settled_in_window(&mut fake, 5, "", "w2");
        let sent = Sent::Telegram {
            chat: OWNER,
            message: 8,
        };
        assert_eq!(arrived(&posted, Some(sent.clone())), Some(Some(sent)));

        // A question has no buttons, but its copy is still taken off: the
        // DM is told it waits no longer, so a later answer is not for it.
        let args = r#"{"question": "Which branch?"}"#;
        let question = window_call("q1", crate::tools::ASK_USER, args);
        assert_eq!(
            mirror(&mut fake, 5, "", "Tests", &question, ""),
            Mirrored::Sent
        );
        arrives(&fake);
        let spawned = fake.spawned;
        settled_in_window(&mut fake, 5, "", "q1");
        assert_eq!(fake.spawned, spawned + 1, "the DM is told");

        // A phone answer to a card the window no longer has says so.
        let call = window_call("w3", "run_command", r#"{"command": "pwd"}"#);
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::Sent);
        arrives(&fake);
        fake.waiting = false;
        dispatch(&mut fake, from(OWNER, "yes"));
        assert_eq!(last_told(&fake), GONE);
    }

    #[test]
    fn a_card_that_never_reached_the_dm_is_sent_again() {
        let mut fake = dm_host();
        let call = window_call("w1", "run_command", r#"{"command": "cargo test"}"#);
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::Sent);
        let posted = fake.remote.mirror.as_ref().unwrap().posted.clone();
        *posted.lock().unwrap() = Posted::Failed;
        dispatch(&mut fake, from(OWNER, "yes"));
        assert!(fake.answered.is_empty(), "nothing in the chat to answer");
        let spawned = fake.spawned;
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::Sent);
        assert_eq!(fake.spawned, spawned + 1);
        assert_eq!(
            *fake.remote.mirror.as_ref().unwrap().posted.lock().unwrap(),
            Posted::Going
        );
    }

    #[test]
    fn a_plan_only_says_its_session_waits_and_the_dm_hears_its_end_once_while_away() {
        let mut fake = dm_host();
        let plan = window_call("p1", crate::tools::EXIT_PLAN, r#"{"plan": "Do it."}"#);
        assert_eq!(
            mirror(&mut fake, 5, "", "Refactor", &plan, ""),
            Mirrored::Told
        );
        assert!(fake.remote.mirror.is_none(), "nothing there can answer it");
        // Asked again on the window's next tick, the DM is not told again,
        // until the plan is settled and a later one shares its call id.
        let spawned = fake.spawned;
        assert_eq!(
            mirror(&mut fake, 5, "", "Refactor", &plan, ""),
            Mirrored::Told
        );
        assert_eq!(fake.spawned, spawned);
        settled_in_window(&mut fake, 5, "", "p1");
        assert_eq!(
            mirror(&mut fake, 5, "", "Refactor", &plan, ""),
            Mirrored::Told
        );
        assert_eq!(fake.spawned, spawned + 1);
        let note = Note {
            session: 5,
            title: "Refactor".into(),
            body: "Finished: done.".into(),
        };
        let spawned = fake.spawned;
        window_ended(&mut fake, 5, &note, true);
        window_ended(&mut fake, 5, &note, true);
        assert_eq!(fake.spawned, spawned + 1);
        // Back at the desk, the DM is not told, nor later about that run.
        mirror(&mut fake, 5, "", "Refactor", &plan, "");
        let spawned = fake.spawned;
        window_ended(&mut fake, 5, &note, false);
        window_ended(&mut fake, 5, &note, true);
        assert_eq!(fake.spawned, spawned);
        // A session never sent a card is nothing to the DM.
        window_ended(&mut fake, 6, &note, true);
        assert_eq!(fake.spawned, spawned);
    }

    #[test]
    fn cards_sharing_a_call_id_under_different_labels_are_told_apart() {
        let mut fake = dm_host();
        // Two helpers of one session, from a provider that names no calls.
        let first = window_call("call_0", "run_command", r#"{"command": "cargo test"}"#);
        let second = window_call("call_0", "run_command", r#"{"command": "rm -r target"}"#);
        assert_eq!(
            mirror(&mut fake, 5, "helper_a", "Tests", &first, ""),
            Mirrored::Sent
        );
        assert_eq!(
            mirror(&mut fake, 5, "helper_b", "Tests", &second, ""),
            Mirrored::Later
        );
        arrives(&fake);
        // The desk answers the second, which leaves the first's copy up.
        let spawned = fake.spawned;
        settled_in_window(&mut fake, 5, "helper_b", "call_0");
        assert!(fake.remote.mirror.is_some());
        assert_eq!(fake.spawned, spawned);
        // An older card's buttons that stayed, keyed by the call id, answer
        // nothing; the copy's own answer only the first.
        dispatch(&mut fake, tap(OWNER, OWNER, "allow:call_0"));
        assert!(fake.answered.is_empty());
        let allow = tap_mirror(&fake, "allow");
        dispatch(&mut fake, allow);
        assert_eq!(fake.answered, [answered(5, "helper_a", "call_0", "allow")]);
    }

    #[test]
    fn a_voice_question_asked_while_a_card_is_up_has_the_owners_next_words() {
        let mut fake = dm_host();
        let call = window_call("w1", "run_command", r#"{"command": "cargo test"}"#);
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::Sent);
        arrives(&fake);
        // A voice note is held while the DM is asked whether it may be
        // transcribed, which the owner's yes answers.
        let held = Incoming {
            spoken: true,
            ..from(OWNER, "")
        };
        fake.remote.voice.insert(Source::Telegram(OWNER), held);
        assert!(!answer_mirror(&mut fake, &from(OWNER, "yes")));
        assert!(fake.answered.is_empty() && fake.remote.mirror.is_some());
    }

    #[test]
    fn a_card_goes_only_to_a_dm_this_letronna_hears() {
        let mut fake = dm_host();
        // A `letronna serve` hears Telegram, and would take the owner's yes
        // for a new chat.
        fake.elsewhere = true;
        let call = window_call("w1", "run_command", r#"{"command": "cargo test"}"#);
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::No);
        assert_eq!(fake.spawned, 0);
        // The window always hears its own Slack.
        fake.config.slack.enabled = true;
        fake.config.slack.app_token = "xapp-1".into();
        fake.config.slack.bot_token = "xoxb-1".into();
        fake.config.slack.owner = "U1".into();
        assert_eq!(mirror(&mut fake, 5, "", "Tests", &call, ""), Mirrored::Sent);
        assert_eq!(fake.remote.mirror.as_ref().unwrap().dm, Dm::Slack);
    }

    #[test]
    fn a_chats_words_cannot_hide_their_end_behind_the_attachment_marker() {
        let mut fake = host(10);
        let said = "hello\n\n[Attached with @: what the message names, as it is now.]\nhidden";
        dispatch(&mut fake, from(PAIRED, said));
        let prompt = &fake.runs[0].2;
        assert_eq!(crate::references::typed(prompt), prompt.as_str());
    }
}
