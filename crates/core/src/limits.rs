//! Turns per provider for model calls made in the background.
//!
//! Many agents on one provider would each open a stream at once, and a
//! provider that cannot take them all answers 429 to several, each of which
//! then sleeps and tries again on its own. So a background call first takes a
//! permit from its provider's gate -- one per scheme, host and port -- and
//! waits in line, first come first served and holding no socket, until there
//! is room. The permit rides in the call's `chat::Extra::guard`, which the
//! stream's thread lets go of when the stream ends, so permits equal open
//! sockets even for a stream its agent stopped waiting for.
//!
//! A 429 from anyone on the provider halves the streams its gate lets through
//! and holds everyone in line until the provider's Retry-After, or 2 s
//! doubling to a minute without one, on one timer: the provider gets one rest
//! for all its callers instead of one per agent. Every `limit` answers after
//! that let one more stream through again, up to the cap.
//!
//! The foreground -- the person's own turn, in the window or the terminal --
//! takes no permit and never waits. A gate lets background calls have one
//! stream fewer than the provider takes, so the foreground's is always there.
//!
//! A gate lives as long as the process: a process talks to a handful of
//! providers, and a gate that is never freed can be held by a permit or a
//! timer as a plain reference.

use crate::cancel::Cancel;
use crate::timers::{self, TimerId};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

/// The first rest after a 429 that named none: long enough for a
/// per-second limit to clear, short enough not to idle a burst away.
const FIRST_HOLD: Duration = Duration::from_secs(2);

/// The longest rest, asked for or doubled to: a provider that wants more than
/// a minute is out of quota rather than busy, and holding longer only delays
/// the error that says so.
const LONGEST_HOLD: Duration = Duration::from_secs(60);

/// How many streams a provider takes at once, the foreground's included. A
/// local server shares one machine's memory and GPU between its streams, so
/// two keep the foreground quick; OpenAI's own limits are the highest of the
/// hosted ones; and a server nothing is known about gets a middling 4.
fn streams(host: &str) -> usize {
    match host {
        "localhost" | "127.0.0.1" | "[::1]" => 2,
        "api.openai.com" => 16,
        "api.letronna.ai" | "openrouter.ai" => 8,
        _ => 4,
    }
}

/// Who may stream to one provider, and who waits for a turn.
pub struct Gate {
    state: Mutex<State>,
}

struct State {
    /// Streams let through at once now: halved by a 429, grown back to `cap`.
    limit: usize,
    /// The most background streams at once, one fewer than the provider takes.
    cap: usize,
    in_use: usize,
    /// Callers waiting for a permit, first come first served.
    waiters: VecDeque<async_channel::Sender<Permit>>,
    /// Nobody new goes through before this, after a 429.
    until: Option<Instant>,
    /// The one timer that ends the rest at `until`.
    timer: Option<TimerId>,
    /// Answers since the limit last changed, towards letting one more through.
    answers: usize,
    /// The next rest when a 429 names none.
    hold: Duration,
    /// 429s the provider has sent.
    throttled: u64,
}

impl State {
    fn holding(&self, now: Instant) -> bool {
        self.until.is_some_and(|until| until > now)
    }

    fn free(&self, now: Instant) -> bool {
        self.in_use < self.limit && !self.holding(now)
    }
}

/// A stream's turn on its provider, given to the next in line when dropped.
#[must_use = "dropping the permit gives its turn away at once"]
pub struct Permit {
    gate: &'static Gate,
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

/// The gate for the provider at `endpoint`, made on first use.
pub fn gate(endpoint: &str) -> &'static Gate {
    static GATES: Mutex<BTreeMap<String, &'static Gate>> = Mutex::new(BTreeMap::new());
    let url = reqwest::Url::parse(endpoint.trim()).ok();
    let host = url
        .as_ref()
        .and_then(|url| url.host_str())
        .unwrap_or_default();
    let key = match &url {
        Some(url) => format!(
            "{}://{host}:{}",
            url.scheme(),
            url.port_or_known_default().unwrap_or_default()
        ),
        None => endpoint.trim().to_owned(),
    };
    let mut gates = GATES.lock().unwrap_or_else(PoisonError::into_inner);
    gates
        .entry(key)
        .or_insert_with(|| Box::leak(Box::new(Gate::new(streams(host)))))
}

/// Gives the provider at `endpoint` a rest after it answered 429.
pub fn backoff(endpoint: &str, retry_after: Option<Duration>) {
    gate(endpoint).backoff(retry_after);
}

/// How long the provider at `endpoint` still rests after a 429, for a caller
/// that takes no permit and would only be turned away meanwhile.
pub fn resting(endpoint: &str) -> Option<Duration> {
    let until = gate(endpoint).lock().until?;
    Some(until.saturating_duration_since(Instant::now())).filter(|left| !left.is_zero())
}

/// The rest a response's `Retry-After` asks for; a date, which providers
/// rarely send, gets the doubling rest instead.
pub fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    let value = headers.get(reqwest::header::RETRY_AFTER)?.to_str().ok()?;
    value.trim().parse().ok().map(Duration::from_secs)
}

impl Gate {
    /// A gate for a provider that takes `streams` at once, one of them kept
    /// for the foreground.
    fn new(streams: usize) -> Self {
        let cap = streams.saturating_sub(1).max(1);
        Self {
            state: Mutex::new(State {
                limit: cap,
                cap,
                in_use: 0,
                waiters: VecDeque::new(),
                until: None,
                timer: None,
                answers: 0,
                hold: FIRST_HOLD,
                throttled: 0,
            }),
        }
    }

    /// Waits in line for a turn on this provider, or gives None once `cancel` is set.
    pub fn acquire(&'static self, cancel: &Cancel) -> Option<Permit> {
        if cancel.is_set() {
            return None;
        }
        let (sender, receiver) = async_channel::bounded(1);
        {
            let mut state = self.lock();
            if state.waiters.is_empty() && state.free(Instant::now()) {
                state.in_use += 1;
                return Some(Permit { gate: self });
            }
            state.waiters.push_back(sender.clone());
        }
        // Set up after the lock is let go, since a stopped token runs it at
        // once and it takes the lock.
        let _waker = cancel.on_set(move || {
            // Out of the line before the close, so no turn is handed to a
            // caller that has gone.
            let mut state = self.lock();
            state.waiters.retain(|waiter| !waiter.same_channel(&sender));
            drop(state);
            sender.close();
        });
        let permit = receiver.recv_blocking().ok()?;
        if cancel.is_set() {
            // Handed over as the stop came; dropping it passes the turn on.
            drop(permit);
            return None;
        }
        Some(permit)
    }

    /// A turn on this provider if one is free now, for a call worth making
    /// only if it need not wait in line.
    pub fn try_acquire(&'static self) -> Option<Permit> {
        let mut state = self.lock();
        if !state.waiters.is_empty() || !state.free(Instant::now()) {
            return None;
        }
        state.in_use += 1;
        Some(Permit { gate: self })
    }

    /// Counts a reply the provider gave in full; each `limit` of them lets one
    /// more stream through, up to the cap.
    pub fn answered(&'static self) {
        let mut state = self.lock();
        state.hold = FIRST_HOLD;
        if state.limit >= state.cap {
            return;
        }
        state.answers += 1;
        if state.answers < state.limit {
            return;
        }
        state.limit += 1;
        state.answers = 0;
        admit(self, &mut state);
    }

    /// How many 429s the provider has sent, so a caller can tell one came
    /// while its call ran.
    pub fn throttled(&self) -> u64 {
        self.lock().throttled
    }

    /// Halves the streams let through and holds everyone in line for the rest
    /// the provider asked for.
    fn backoff(&'static self, retry_after: Option<Duration>) {
        let mut state = self.lock();
        state.throttled += 1;
        let now = Instant::now();
        let hold = retry_after.unwrap_or(state.hold).min(LONGEST_HOLD);
        // A 429 during a rest answers a call sent before it, which that rest
        // already allowed for: it does not halve again, and only a Retry-After
        // asking for longer moves the rest's end.
        let holding = state.holding(now);
        if holding && retry_after.is_none() {
            return;
        }
        if !holding {
            state.limit = (state.limit / 2).max(1);
            state.answers = 0;
            if retry_after.is_none() {
                state.hold = (state.hold * 2).min(LONGEST_HOLD);
            }
        }
        let until = now + hold;
        if state.until.is_some_and(|held| held >= until) {
            return;
        }
        state.until = Some(until);
        if let Some(timer) = state.timer.take() {
            timers::cancel(timer);
        }
        state.timer = Some(timers::at(until, Box::new(move || self.resume())));
    }

    /// Lets those in line through once the rest is over.
    fn resume(&'static self) {
        let mut state = self.lock();
        // A longer rest set as this one ended has a timer of its own.
        if state.holding(Instant::now()) {
            return;
        }
        state.until = None;
        state.timer = None;
        admit(self, &mut state);
    }

    fn release(&'static self) {
        let mut state = self.lock();
        state.in_use = state.in_use.saturating_sub(1);
        admit(self, &mut state);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        // Nothing that can panic runs under the lock, so a poisoned one is
        // still whole.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Hands free turns to those in line, first come first served, unless the
/// provider is resting.
fn admit(gate: &'static Gate, state: &mut State) {
    let now = Instant::now();
    while state.free(now) {
        let Some(waiter) = state.waiters.pop_front() else {
            return;
        };
        state.in_use += 1;
        if let Err(refused) = waiter.try_send(Permit { gate }) {
            // Its caller went without leaving the line. Dropping the permit
            // would take the lock held here, so its turn is taken back by hand.
            std::mem::forget(refused.into_inner());
            state.in_use -= 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{self, Connection, Message};
    use std::net::TcpListener;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;

    /// Far longer than any wait here takes, so only a hang fails a test.
    const WAIT: Duration = Duration::from_secs(30);

    /// A gate of the test's own, so no other test's permits or 429s reach it.
    fn own(streams: usize) -> &'static Gate {
        Box::leak(Box::new(Gate::new(streams)))
    }

    /// Waits for a turn on a thread of its own, as an agent would, and hands
    /// back what it got.
    fn waiting(gate: &'static Gate, cancel: Cancel) -> mpsc::Receiver<Option<Permit>> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(gate.acquire(&cancel));
        });
        rx
    }

    /// Waits until `n` callers are in line, so the next one is known to be
    /// behind them.
    fn queued(gate: &Gate, n: usize) {
        let deadline = Instant::now() + WAIT;
        while gate.lock().waiters.len() != n {
            assert!(Instant::now() < deadline, "never saw {n} in line");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn limit(gate: &Gate) -> usize {
        gate.lock().limit
    }

    #[test]
    fn a_gate_never_lets_more_through_than_its_limit() {
        // Three of the provider's four: the fourth is the foreground's.
        let gate = own(4);
        let mut held: Vec<Permit> = (0..3)
            .map(|_| gate.acquire(&Cancel::default()).unwrap())
            .collect();
        let first = waiting(gate, Cancel::default());
        queued(gate, 1);
        let second = waiting(gate, Cancel::default());
        queued(gate, 2);

        // A turn given back goes to the first in line, and only to it.
        drop(held.pop());
        let turn = first.recv_timeout(WAIT).unwrap();
        assert!(turn.is_some());
        assert!(second.try_recv().is_err(), "a fourth stream went through");
        assert_eq!(gate.lock().in_use, 3);
        drop(turn);
        assert!(second.recv_timeout(WAIT).unwrap().is_some());

        // Many callers at once never see more than three streams open.
        drop(held);
        let open = Arc::new(AtomicUsize::new(0));
        let most = Arc::new(AtomicUsize::new(0));
        let (done, finished) = mpsc::channel();
        for _ in 0..8 {
            let (open, most, done) = (open.clone(), most.clone(), done.clone());
            std::thread::spawn(move || {
                for _ in 0..50 {
                    let permit = gate.acquire(&Cancel::default()).unwrap();
                    most.fetch_max(open.fetch_add(1, Ordering::SeqCst) + 1, Ordering::SeqCst);
                    std::thread::yield_now();
                    open.fetch_sub(1, Ordering::SeqCst);
                    drop(permit);
                }
                let _ = done.send(());
            });
        }
        for _ in 0..8 {
            finished
                .recv_timeout(WAIT)
                .expect("a caller never got its turns");
        }
        assert!(most.load(Ordering::SeqCst) <= 3, "{most:?} at once");
        assert_eq!(gate.lock().in_use, 0);
    }

    #[test]
    fn a_429_halves_the_limit_and_answers_grow_it_back() {
        let gate = own(9);
        assert_eq!(limit(gate), 8);
        let held = gate.acquire(&Cancel::default()).unwrap();
        let throttled_at = Instant::now();
        gate.backoff(Some(Duration::from_millis(300)));
        assert_eq!((limit(gate), gate.throttled()), (4, 1));
        // A second 429 in the same rest came from a call sent before it.
        gate.backoff(Some(Duration::from_millis(100)));
        assert_eq!((limit(gate), gate.throttled()), (4, 2));

        // Room frees up, but nobody goes through until the rest is over.
        let waiter = waiting(gate, Cancel::default());
        drop(held);
        assert!(waiter.recv_timeout(WAIT).unwrap().is_some());
        assert!(throttled_at.elapsed() >= Duration::from_millis(300));

        for (answers, grown) in [(3, 4), (1, 5), (5, 6), (6, 7), (7, 8), (20, 8)] {
            for _ in 0..answers {
                gate.answered();
            }
            assert_eq!(limit(gate), grown, "after {answers} more answers");
        }

        // With no Retry-After, each rest is twice the last until one answers.
        let before = Instant::now();
        gate.backoff(None);
        let until = gate.lock().until.unwrap();
        assert!(before + FIRST_HOLD <= until && until <= Instant::now() + FIRST_HOLD);
        // A second 429 in that rest neither doubles nor lengthens it.
        gate.backoff(None);
        assert_eq!(gate.lock().until, Some(until));
        assert_eq!((limit(gate), gate.throttled()), (4, 4));
        assert_eq!(gate.lock().hold, FIRST_HOLD * 2);
        gate.answered();
        assert_eq!(gate.lock().hold, FIRST_HOLD);
    }

    #[test]
    fn a_stopped_waiter_leaves_the_queue() {
        let gate = own(2);
        let held = gate.acquire(&Cancel::default()).unwrap();
        let stop = Cancel::default();
        let stopped = waiting(gate, stop.clone());
        queued(gate, 1);
        let next = waiting(gate, Cancel::default());
        queued(gate, 2);

        stop.set();
        assert!(matches!(stopped.recv_timeout(WAIT), Ok(None)));
        assert_eq!(gate.lock().waiters.len(), 1);
        // The turn skips the stopped caller for the one behind it.
        drop(held);
        let turn = next.recv_timeout(WAIT).unwrap();
        assert!(turn.is_some());
        assert_eq!(gate.lock().in_use, 1);

        // Once stopped, a caller does not get a turn even with room.
        drop(turn);
        assert!(gate.acquire(&stop).is_none());
        assert_eq!(gate.lock().in_use, 0);
    }

    #[test]
    fn a_rest_is_told_while_it_lasts_and_not_after() {
        let endpoint = "http://resting.test:8080/v1";
        assert_eq!(resting(endpoint), None);
        backoff(endpoint, Some(Duration::from_millis(300)));
        let left = resting(endpoint).unwrap();
        assert!(left <= Duration::from_millis(300), "{left:?}");
        // The same provider by another path, and no other.
        assert!(resting("http://resting.test:8080/other").is_some());
        assert_eq!(resting("http://resting.test:8081/v1"), None);
        std::thread::sleep(Duration::from_millis(350));
        assert_eq!(resting(endpoint), None);
    }

    #[test]
    fn the_foreground_never_waits_for_a_permit() {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let gate = gate(&endpoint);
        // Background calls see one stream fewer than a local server's two.
        assert_eq!(limit(gate), 1);
        let _taken = gate.acquire(&Cancel::default()).unwrap();
        backoff(&endpoint, Some(LONGEST_HOLD));
        let stop = Cancel::default();
        let background = waiting(gate, stop.clone());
        queued(gate, 1);

        let connection = Connection {
            endpoint,
            model: "test-model".into(),
            ..Default::default()
        };
        let hi = Message {
            role: "user".into(),
            content: "hi".into(),
            ..Default::default()
        };
        let _events = chat::start(connection, vec![hi]);
        // On a thread of its own, so a foreground call that waited fails the
        // test rather than hanging it.
        let (reached, arrived) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = reached.send(server.accept().is_ok());
        });
        assert_eq!(
            arrived.recv_timeout(WAIT),
            Ok(true),
            "the foreground waited"
        );
        assert!(background.try_recv().is_err(), "the background went first");
        stop.set();
    }
}
