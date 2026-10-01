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

use crate::approvals;
use crate::chat::{Message, Remote};
use crate::driver::{self, Outcome};
use crate::gate::Mode;
use crate::inbox::{self, Notice, When};
use crate::messaging::pairing::{self, Attempt};
use crate::messaging::{self, CardAnswer, Config, Incoming, Press, Source, files};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Runs started at once: a few chats and `/bg` tasks side by side, without
/// crowding the provider's rate limit. The rest wait for a place rather than
/// being refused, since somebody asked for each.
pub const AT_ONCE: usize = 4;

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
}

/// Start hearing Telegram and Discord as set up in `config`, each message
/// going to `sender`, on threads that end once the flag handed back is set;
/// `None` when neither is set up to hear.
pub fn listen(config: &Config, sender: async_channel::Sender<Incoming>) -> Option<Arc<AtomicBool>> {
    let (telegram, discord) = (&config.telegram, &config.discord);
    if !telegram.ready() && !discord.bot_ready() {
        return None;
    }
    let stop = Arc::new(AtomicBool::new(false));
    if telegram.ready() {
        messaging::poll(telegram.token.clone(), stop.clone(), sender.clone());
    }
    if discord.bot_ready() {
        messaging::discord_listen(discord.clone(), stop.clone(), sender);
    }
    Some(stop)
}

/// Deal with a message from a chat app. A Telegram chat that is not paired
/// is heard only as a try at the pairing code, and gets no reply unless it
/// is right; Discord only ever delivers the owner's messages. Slack hands
/// on a DM from anyone until a member is paired, heard the same way.
pub fn dispatch(host: &mut impl Host, mut message: Incoming) {
    let source = message.source.clone();
    // A tap answers only as the owner's own words would.
    let may_answer = pairing::may_answer(host.config(), &message);
    if let Some(tap) = message.press {
        if may_answer {
            press(host, source, tap);
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
/// or a second tap, only loses its buttons.
fn press(host: &mut impl Host, source: Source, tap: Press) {
    let dispatcher = host.dispatcher();
    let answers =
        (dispatcher.asks.get(&source)).is_some_and(|request| tap.answers(&request.call.id));
    let request = answers.then(|| dispatcher.asks.remove(&source)).flatten();
    let allow = tap.allow;
    let config = host.config().clone();
    host.spawn(Box::new(move || {
        let _ = messaging::settle(&config, &tap, answers);
    }));
    if let Some(request) = request {
        request.answer(match allow {
            true => approvals::Decision::Once,
            false => approvals::Decision::Deny(String::new()),
        });
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
    };
    inbox::push(id, notice);
}

/// A call of chat `source`'s run that needs a yes: its card goes to the
/// chat, whose next message or a tap on the card answers it.
pub fn ask(host: &mut impl Host, source: Source, request: approvals::Request) {
    let (call, preview) = (&request.call, &request.preview);
    let text = messaging::card_text(&call.name, &call.arguments, preview);
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
    let decision = match messaging::card_answer(&call.name, &call.arguments, said) {
        CardAnswer::Allow => approvals::Decision::Once,
        CardAnswer::Deny(reason) => approvals::Decision::Deny(reason),
        // A run beside the chat asks no questions, so words are a no.
        CardAnswer::Answer(_) => approvals::Decision::Deny(String::new()),
    };
    request.answer(decision);
    true
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
    use super::{Dispatcher, Host, ask, dispatch, ended};
    use crate::approvals::{self, tests::SERVING};
    use crate::chat::{Message, Remote, ToolCall};
    use crate::gate::Mode;
    use crate::inbox::{self, When};
    use crate::messaging::pairing::Attempt;
    use crate::messaging::{Config, Incoming, Source};
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
        fn spawn(&mut self, _: Box<dyn FnOnce() + Send>) {}
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

    fn last_told(fake: &Fake) -> &str {
        fake.told
            .last()
            .map(|(_, text)| text.as_str())
            .unwrap_or("")
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
    fn a_chats_words_cannot_hide_their_end_behind_the_attachment_marker() {
        let mut fake = host(10);
        let said = "hello\n\n[Attached with @: what the message names, as it is now.]\nhidden";
        dispatch(&mut fake, from(PAIRED, said));
        let prompt = &fake.runs[0].2;
        assert_eq!(crate::references::typed(prompt), prompt.as_str());
    }
}
