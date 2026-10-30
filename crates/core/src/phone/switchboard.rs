//! Where phones' connections come in, whichever way they reach this
//! computer: through letronna-api's relay (`relay`), or directly over
//! Tailscale (`direct`).
//!
//! Each way brings wires: the relay's one connection, which carries every
//! phone on it with a number for each, and a direct connection for each
//! phone, numbered 0. The ways only pass frames; the board answers each
//! phone's handshake, keeps its tunnel, and tells the holder what happens,
//! on a thread of its own that takes everything in the order it came. So a
//! phone is the same to the holder however it came, and is known by a
//! number the board gives it, never given again while this runs, so nothing
//! meant for a phone that left reaches one that came after it.
//!
//! Only one Letronna per data folder answers phones: two would answer the
//! same phones with the same key.

use super::direct::{self, Direct};
use super::noise::Keys;
use super::relay::{self, State};
use super::{Caller, Hello, Invite, Tunnel};
use std::collections::HashMap;
use std::fs::{File, TryLockError};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};

/// Phones connected at once: more than anyone carries, and a bound on what
/// a phone gone wrong can make this computer hold.
const LINES: usize = 8;
/// The lock in the data folder held by the Letronna that answers phones.
const LOCK: &str = "phone.lock";

/// What a phone that paired is told the user said, as the notifications
/// every message after is the form of.
pub(super) const PAIRED: &str = r#"{"jsonrpc":"2.0","method":"_letronna/paired"}"#;
pub(super) const REFUSED: &str = r#"{"jsonrpc":"2.0","method":"_letronna/refused"}"#;

pub(super) const ELSEWHERE: &str =
    "Another Letronna on this computer already answers your phones: a window, or letronna serve.";

/// How long things wait.
#[derive(Clone, Copy)]
pub(super) struct Timing {
    /// After a connection to the relay fails; it doubles with each failure
    /// in a row, up to `retry_most`, and a connection that held that long
    /// starts it over.
    pub retry: Duration,
    pub retry_most: Duration,
    /// A read while no phone is on a wire: how soon `stop` is noticed then.
    pub idle: Duration,
    /// A read while one is: how long what it is sent can wait for the read
    /// to give way, since each wire both reads and writes on one thread.
    pub busy: Duration,
    /// Silence this long is answered with a ping; twice this long, and the
    /// wire is taken for gone even if TCP has not noticed.
    pub quiet: Duration,
    /// How often the tailnet address is looked for again.
    pub look: Duration,
}

const REAL: Timing = Timing {
    retry: Duration::from_secs(2),
    retry_most: Duration::from_secs(60),
    idle: Duration::from_secs(5),
    busy: Duration::from_millis(25),
    quiet: Duration::from_secs(30),
    look: Duration::from_secs(30),
};

/// What happens to phones, for the window or `letronna serve` to act on.
#[derive(Debug)]
pub enum Event {
    /// How the connection to the relay is.
    State(State),
    /// Whether phones can reach this computer directly; only when that was
    /// asked for.
    Direct(Direct),
    /// A phone connected, as `phone`, a number no other phone is given:
    /// one paired before, once it has spoken, or one pairing with the QR
    /// code, which waits for `keep` or `refuse`.
    Arrived { phone: u64, caller: Caller },
    /// A whole message from a phone that may use this computer.
    Message { phone: u64, message: Vec<u8> },
    /// A phone went, or was sent away; `why` when something went wrong.
    Gone { phone: u64, why: Option<String> },
}

/// What the holder asks of the board, done in the order asked, between the
/// frames the wires bring.
pub(super) enum Order {
    Invite(Option<Invite>),
    Keep(u64),
    Refuse(u64),
    Send(u64, Vec<u8>),
    HangUp(u64),
}

/// What comes to the board: the holder's orders, and what the wires bring.
pub(super) enum Input {
    Order(Order),
    /// A wire opened, and what sends on it.
    Opened(u64, Sender<Out>),
    /// A frame from phone `number` on wire `wire`.
    Frame {
        wire: u64,
        number: u32,
        frame: Vec<u8>,
    },
    /// Phone `number` on wire `wire` went.
    Gone {
        wire: u64,
        number: u32,
    },
    /// A wire closed, and every phone on it went.
    Closed(u64),
    State(State),
    Direct(Direct),
    Stop,
}

/// What the board has a wire do.
pub(super) enum Out {
    /// Send phone `number` a frame.
    Frame(u32, Vec<u8>),
    /// Send phone `number` away.
    Gone(u32),
}

/// A number for a wire no other is given, whichever way it came.
pub(super) fn wire() -> u64 {
    static WIRES: AtomicU64 = AtomicU64::new(0);
    WIRES.fetch_add(1, Ordering::Relaxed) + 1
}

/// The phones' way in, kept open while this is held: the relay's
/// connection, tried again when it fails, and the direct one when asked.
pub struct Switchboard {
    input: Sender<Input>,
    stop: Arc<AtomicBool>,
    /// Held here, not by a thread, so a Switchboard started in place of
    /// this one can have it at once; the relay drops the older connection.
    _lock: File,
    #[cfg(test)]
    threads: Vec<std::thread::JoinHandle<()>>,
}

impl Switchboard {
    /// Answer phones as this computer, by `name`, the one shown on the
    /// phone, sending what happens down `events`: through the relay, and
    /// directly over Tailscale when `direct` gives a port (0 for
    /// `direct::PORT`). Refused while another Letronna with this data folder
    /// answers them.
    pub fn start(
        name: String,
        direct: Option<u16>,
        events: async_channel::Sender<Event>,
    ) -> Result<Self, String> {
        let direct = direct.map(|port| direct::Listen {
            port: match port {
                0 => direct::PORT,
                port => port,
            },
            find: direct::tailnet,
        });
        Self::start_with(name, events, relay::dial, direct, REAL)
    }

    pub(super) fn start_with(
        name: String,
        events: async_channel::Sender<Event>,
        dial: impl Fn() -> Result<relay::Dial, String> + Send + 'static,
        direct: Option<direct::Listen>,
        timing: Timing,
    ) -> Result<Self, String> {
        let lock = lock()?;
        let (input, inputs) = std::sync::mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let board = Board {
            name,
            events,
            input: inputs,
            wires: HashMap::new(),
            lines: HashMap::new(),
            on: HashMap::new(),
            count: 0,
            invite: None,
            direct: Vec::new(),
        };
        let spawn = |name: &str, run: Box<dyn FnOnce() + Send>| {
            std::thread::Builder::new()
                .name(name.into())
                .spawn(run)
                .map_err(|e| format!("Could not start answering phones: {e}"))
        };
        let mut threads = vec![spawn("phone-board", Box::new(move || board.run()))?];
        let (to, stopping) = (input.clone(), stop.clone());
        threads.push(spawn(
            "phone-relay",
            Box::new(move || relay::run(dial, to, stopping, timing)),
        )?);
        if let Some(listen) = direct {
            let (to, stopping) = (input.clone(), stop.clone());
            threads.push(spawn(
                "phone-direct",
                Box::new(move || direct::run(listen, to, stopping, timing)),
            )?);
        }
        #[cfg(not(test))]
        drop(threads);
        Ok(Switchboard {
            input,
            stop,
            _lock: lock,
            #[cfg(test)]
            threads,
        })
    }

    fn order(&self, order: Order) {
        let _ = self.input.send(Input::Order(order));
    }

    /// Let a phone pair with `invite`, the QR code about to be shown, or
    /// with none once it is closed.
    pub fn invite(&self, invite: Option<Invite>) {
        self.order(Order::Invite(invite));
    }

    /// Keep phone `phone`, which is pairing: the user said its digits match.
    pub fn keep(&self, phone: u64) {
        self.order(Order::Keep(phone));
    }

    /// Send phone `phone`, which is pairing, away: the user said no.
    pub fn refuse(&self, phone: u64) {
        self.order(Order::Refuse(phone));
    }

    /// Send `message` to phone `phone`, if it may use this computer.
    pub fn send(&self, phone: u64, message: Vec<u8>) {
        self.order(Order::Send(phone, message));
    }

    pub fn hang_up(&self, phone: u64) {
        self.order(Order::HangUp(phone));
    }

    /// What sends to phone `phone` from a thread of its own, as its agent's
    /// does (`bridge`).
    pub fn to(&self, phone: u64) -> impl FnMut(Vec<u8>) + Send + 'static {
        let input = self.input.clone();
        move |message| {
            let _ = input.send(Input::Order(Order::Send(phone, message)));
        }
    }
}

impl Drop for Switchboard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.input.send(Input::Stop);
    }
}

#[cfg(test)]
impl Switchboard {
    /// Stop, and wait for every thread to end, so a test's data folder is
    /// not let go while one may still read it.
    pub(super) fn finish(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.input.send(Input::Stop);
        for thread in std::mem::take(&mut self.threads) {
            let _ = thread.join();
        }
    }
}

/// The lock that says this Letronna answers phones.
fn lock() -> Result<File, String> {
    let path = crate::chat::data_dir().join(LOCK);
    let file = crate::schedule::open_lock(&path)
        .map_err(|e| format!("Could not start answering phones: {e}"))?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(ELSEWHERE.into()),
        Err(TryLockError::Error(e)) => Err(format!("Could not start answering phones: {e}")),
    }
}

/// A phone connected now.
struct Line {
    /// The wire it is on, and its number there.
    wire: u64,
    number: u32,
    tunnel: Tunnel,
    caller: Caller,
    /// Whether what it sends is read: a pairing phone's is not until the
    /// user says yes.
    trusted: bool,
    /// Whether the holder was told it arrived. A paired phone is, once a
    /// frame of its own comes after the handshake: its first message alone
    /// could be an old one sent again by anyone who saw it pass, and a
    /// handshake answered for that goes nowhere.
    told: bool,
}

struct Board {
    name: String,
    events: async_channel::Sender<Event>,
    input: Receiver<Input>,
    wires: HashMap<u64, Sender<Out>>,
    /// The phones connected, by the number the holder knows each by.
    lines: HashMap<u64, Line>,
    /// The same, by wire and number on it.
    on: HashMap<(u64, u32), u64>,
    /// Phones answered so far: the last number given.
    count: u64,
    /// The QR code shown now.
    invite: Option<Invite>,
    /// Where phones can reach this computer directly now, as they are told.
    direct: Vec<String>,
}

impl Board {
    fn run(mut self) {
        while let Ok(input) = self.input.recv() {
            match input {
                Input::Stop => return,
                Input::Order(order) => self.obey(order),
                Input::Opened(wire, out) => {
                    self.wires.insert(wire, out);
                }
                Input::Frame {
                    wire,
                    number,
                    frame,
                } => self.take(wire, number, &frame),
                Input::Gone { wire, number } => {
                    if let Some(phone) = self.on.remove(&(wire, number))
                        && let Some(line) = self.lines.remove(&phone)
                    {
                        self.gone(phone, &line, None);
                    }
                }
                Input::Closed(wire) => {
                    self.wires.remove(&wire);
                    let mut on: Vec<u64> = (self.lines.iter())
                        .filter(|(_, line)| line.wire == wire)
                        .map(|(&phone, _)| phone)
                        .collect();
                    on.sort_unstable();
                    for phone in on {
                        if let Some(line) = self.lines.remove(&phone) {
                            self.on.remove(&(line.wire, line.number));
                            self.gone(phone, &line, None);
                        }
                    }
                }
                Input::State(state) => self.tell(Event::State(state)),
                Input::Direct(direct) => {
                    self.direct = match &direct {
                        Direct::Answering(at) => vec![direct::url(at)],
                        _ => Vec::new(),
                    };
                    self.tell(Event::Direct(direct));
                }
            }
        }
    }

    /// Nobody may be listening; phones are answered either way.
    fn tell(&self, event: Event) {
        let _ = self.events.try_send(event);
    }

    fn out(&self, wire: u64, out: Out) {
        if let Some(sender) = self.wires.get(&wire) {
            let _ = sender.send(out);
        }
    }

    /// Tell the holder phone `phone` came, and note when a paired one was
    /// last seen.
    fn arrived(&self, phone: u64, caller: Caller) {
        if let Caller::Paired(known) = &caller {
            // When it was last seen is for the list in Settings; failing to
            // note it is no reason to turn the phone away.
            let _ = super::seen(&known.key, &known.name);
        }
        self.tell(Event::Arrived { phone, caller });
    }

    /// Tell the holder phone `phone` is gone, if it was told it came.
    fn gone(&self, phone: u64, line: &Line, why: Option<String>) {
        if line.told {
            self.tell(Event::Gone { phone, why });
        }
    }

    /// Take a frame from phone `number` on wire `wire`.
    fn take(&mut self, wire: u64, number: u32, frame: &[u8]) {
        let Some(&phone) = self.on.get(&(wire, number)) else {
            return match self.lines.len() < LINES {
                true => self.answer(wire, number, frame),
                false => self.out(wire, Out::Gone(number)),
            };
        };
        let Some(line) = self.lines.get_mut(&phone) else {
            return;
        };
        let whole = match line.tunnel.open(frame) {
            Ok(whole) => whole,
            Err(why) => return self.send_away(phone, Some(why)),
        };
        let trusted = line.trusted;
        let first = !std::mem::replace(&mut line.told, true);
        if first {
            let caller = line.caller.clone();
            self.arrived(phone, caller);
        }
        // A pairing phone's is not read before the yes.
        if let Some(message) = whole.filter(|_| trusted) {
            self.tell(Event::Message { phone, message });
        }
    }

    /// A new phone's first frame: answered, or the phone sent away. It is
    /// not told why, as the handshake that would carry it never finished; a
    /// phone not paired, or with an old code, is all it could be.
    fn answer(&mut self, wire: u64, number: u32, first: &[u8]) {
        let said = Hello {
            name: self.name.clone(),
            direct: self.direct.clone(),
        };
        // Read again for each, so one a locked vault would not give is
        // asked for again later rather than replaced.
        let answered = super::computer().and_then(|me| {
            super::answer(
                first,
                &me,
                &super::phones(),
                &mut self.invite,
                &said,
                Keys::random()?,
                Instant::now(),
            )
        });
        let Ok(answered) = answered else {
            return self.out(wire, Out::Gone(number));
        };
        self.out(wire, Out::Frame(number, answered.reply));
        self.count += 1;
        let phone = self.count;
        // A pairing phone waits on the user, who is asked at once. Its
        // first message cannot be an old one: the code it was made with
        // was good for that phone alone.
        let pairing = matches!(answered.caller, Caller::New { .. });
        if pairing {
            self.arrived(phone, answered.caller.clone());
        }
        self.on.insert((wire, number), phone);
        self.lines.insert(
            phone,
            Line {
                wire,
                number,
                tunnel: answered.tunnel,
                caller: answered.caller,
                trusted: !pairing,
                told: pairing,
            },
        );
    }

    fn obey(&mut self, order: Order) {
        match order {
            Order::Invite(invite) => self.invite = invite,
            Order::Send(phone, message) => {
                if self.lines.get(&phone).is_some_and(|line| line.trusted) {
                    self.say(phone, &message);
                }
            }
            Order::Keep(phone) => {
                let Some(Line {
                    caller: Caller::New { name, key, .. },
                    trusted: false,
                    ..
                }) = self.lines.get(&phone)
                else {
                    return;
                };
                match super::keep(name, key) {
                    Ok(()) => {
                        if let Some(line) = self.lines.get_mut(&phone) {
                            line.trusted = true;
                        }
                        self.say(phone, PAIRED.as_bytes());
                    }
                    Err(why) => self.send_away(phone, Some(why)),
                }
            }
            Order::Refuse(phone) => {
                if self.lines.get(&phone).is_some_and(|line| !line.trusted) {
                    self.say(phone, REFUSED.as_bytes());
                }
                self.send_away(phone, None);
            }
            Order::HangUp(phone) => self.send_away(phone, None),
        }
    }

    /// Send `message` to phone `phone`. One that cannot be sealed is a
    /// mistake on this side, and ends that phone's tunnel, as any error on
    /// one does.
    fn say(&mut self, phone: u64, message: &[u8]) {
        let Some(line) = self.lines.get_mut(&phone) else {
            return;
        };
        match line.tunnel.seal(message) {
            Ok(frames) => {
                let (wire, number) = (line.wire, line.number);
                for frame in frames {
                    self.out(wire, Out::Frame(number, frame));
                }
            }
            Err(why) => self.send_away(phone, Some(why)),
        }
    }

    fn send_away(&mut self, phone: u64, why: Option<String>) {
        if let Some(line) = self.lines.remove(&phone) {
            self.on.remove(&(line.wire, line.number));
            self.out(line.wire, Out::Gone(line.number));
            self.gone(phone, &line, why);
        }
    }
}

/// What tests of either way share: a data folder of the test's own, and
/// waiting on what the board tells.
#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::store::tests::{DATA_DIR, Scratch};
    use std::sync::MutexGuard;

    pub const FAST: Timing = Timing {
        retry: Duration::from_millis(50),
        retry_most: Duration::from_millis(200),
        idle: Duration::from_millis(50),
        busy: Duration::from_millis(10),
        quiet: Duration::from_secs(30),
        look: Duration::from_millis(100),
    };

    /// How long a test waits for anything before it fails.
    pub const PATIENCE: Duration = Duration::from_secs(10);

    /// A data folder of this test's own, held from other tests, and this
    /// computer's key in it. No vault: the board's thread and the test's
    /// would not share one.
    pub fn data(name: &str) -> (MutexGuard<'static, ()>, Scratch) {
        let held = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new(name);
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        super::super::computer().unwrap();
        (held, dir)
    }

    pub fn done(board: Switchboard) {
        board.finish();
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    /// The next event, waited for.
    pub fn event(events: &async_channel::Receiver<Event>) -> Event {
        let until = Instant::now() + PATIENCE;
        loop {
            match events.try_recv() {
                Ok(event) => return event,
                Err(async_channel::TryRecvError::Closed) => panic!("the board's thread ended"),
                Err(_) if Instant::now() > until => panic!("no event came"),
                Err(_) => std::thread::sleep(Duration::from_millis(5)),
            }
        }
    }

    /// The next event about a phone.
    pub fn next(events: &async_channel::Receiver<Event>) -> Event {
        loop {
            match event(events) {
                Event::State(_) | Event::Direct(_) => {}
                other => return other,
            }
        }
    }

    /// The next state the relay's connection is in.
    pub fn state(events: &async_channel::Receiver<Event>) -> State {
        loop {
            if let Event::State(state) = event(events) {
                return state;
            }
        }
    }

    /// The next word on reaching this computer directly.
    pub fn direct(events: &async_channel::Receiver<Event>) -> Direct {
        loop {
            if let Event::Direct(direct) = event(events) {
                return direct;
            }
        }
    }

    /// The number a phone arrived as.
    pub fn arrived(events: &async_channel::Receiver<Event>) -> u64 {
        match next(events) {
            Event::Arrived { phone, .. } => phone,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn only_one_letronna_answers_phones() {
        let (_held, _dir) = data("board-lock");
        let signed_out = || Err(relay::SIGNED_OUT.to_owned());
        let (sender, _events) = async_channel::unbounded();
        let first =
            Switchboard::start_with("Desk".into(), sender.clone(), signed_out, None, FAST).unwrap();
        let second = Switchboard::start_with("Desk".into(), sender.clone(), signed_out, None, FAST);
        assert_eq!(second.err().as_deref(), Some(ELSEWHERE));
        // Let go, it can be had at once by the one that replaces it.
        first.finish();
        let third = Switchboard::start_with("Desk".into(), sender, signed_out, None, FAST).unwrap();
        done(third);
    }
}
