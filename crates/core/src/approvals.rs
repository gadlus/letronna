//! Approval requests from agents that are not on screen.
//!
//! A helper runs on a thread of its own, with no card to show, so when one of
//! its calls needs a yes it asks here and whichever frontend took `serve`
//! answers: from the session's own rules where they already allow the call,
//! otherwise by asking the person. Core keeps no rules of its own except
//! `ForRun`, remembered per run and tool, so a run of many helpers asks once
//! per tool rather than once per helper. That has to hold when helpers ask at
//! the same moment too, so a run has one request out per tool at a time: the
//! others wait for its answer, and ask for themselves only if it was not
//! `ForRun`.
//!
//! Each request carries its own reply channel, so an answer can only reach
//! the call it was given for: there is no id to match up, or to forge. And
//! every way of not answering is a no. With nobody serving, a request is
//! declined at once; a request dropped unanswered is declined; a stop sends
//! the waiting agent a no. An agent with nobody to ask stops short rather
//! than hanging or gaining power.

use crate::chat::ToolCall;
use crate::tools::{self, ToolContext};
use std::collections::BTreeSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

/// What was said to a request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Run this call.
    Once,
    /// Run this call, and later calls of the same tool in the same run without asking.
    ForRun,
    /// Do not run it.
    Deny,
}

/// A call waiting for a yes, as the frontend that serves receives it.
pub struct Request {
    /// The session the agent works for, whose rules answer first.
    pub root: u64,
    /// The tree or workflow run the agent is part of, which `ForRun` covers.
    pub run: u64,
    /// The agent's name, for the card.
    pub label: String,
    pub call: ToolCall,
    /// What the approval card shows, from the tool's own preview.
    pub preview: String,
    reply: Sender<Decision>,
}

impl Request {
    /// Tells the agent waiting on this request what was decided.
    pub fn answer(self, decision: Decision) {
        let _ = self.reply.send(decision);
    }
}

impl Drop for Request {
    fn drop(&mut self) {
        // The agent reads only the first word, so after an answer this is
        // never heard; without one, a frontend that goes away with cards
        // still up does not leave their agents waiting for ever.
        let _ = self.reply.send(Decision::Deny);
    }
}

/// Where requests go: the frontend that served last, or nobody.
static SERVER: Mutex<Option<Sender<Request>>> = Mutex::new(None);
/// What core remembers per run and tool, under one lock so that a caller
/// checks both and starts waiting in one step, with no gap to miss a wake in.
static ASKS: Mutex<Asks> = Mutex::new(Asks {
    granted: BTreeSet::new(),
    asking: BTreeSet::new(),
});
/// Wakes callers waiting in line when a request is settled or they are stopped.
static SETTLED: Condvar = Condvar::new();

struct Asks {
    /// The tools each run may use without asking again. Kept for the life of
    /// the process: a run's number is never reused, and a run adds at most one
    /// entry per tool.
    granted: BTreeSet<(u64, String)>,
    /// The tools each run has a request out for.
    asking: BTreeSet<(u64, String)>,
}

/// A run's one request out for a tool, which lets the next in line ask when dropped.
struct Turn((u64, String));

impl Drop for Turn {
    fn drop(&mut self) {
        lock(&ASKS).asking.remove(&self.0);
        SETTLED.notify_all();
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Nothing that can panic runs under these locks, so a poisoned one is
    // still whole.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Sends every request from now on to the caller, instead of whoever served before.
pub fn serve() -> Receiver<Request> {
    let (requests, served) = mpsc::channel();
    *lock(&SERVER) = Some(requests);
    served
}

/// Whether run `run` was already allowed every call of `name`, asking nobody.
pub(crate) fn granted(run: u64, name: &str) -> bool {
    lock(&ASKS).granted.contains(&(run, name.to_owned()))
}

/// Whether `call` may run, waiting for the frontend's answer unless the agent is stopped first.
pub(crate) fn ask(root: u64, run: u64, label: &str, call: &ToolCall, ctx: &ToolContext) -> bool {
    let (reply, answer) = mpsc::channel();
    let stop = reply.clone();
    // Set up before taking the lock, since a stopped token runs it at once.
    let _waker = ctx.cancel.on_set(move || {
        let _ = stop.send(Decision::Deny);
        // Taken so that a caller between its check and its wait cannot miss this.
        drop(lock(&ASKS));
        SETTLED.notify_all();
    });
    let key = (run, call.name.clone());
    let mut asks = lock(&ASKS);
    loop {
        if ctx.cancel.is_set() {
            return false;
        }
        if asks.granted.contains(&key) {
            return true;
        }
        if asks.asking.insert(key.clone()) {
            break;
        }
        // Another call of this tool in this run is waiting on a card, and an
        // answer of ForRun there covers this call too.
        asks = SETTLED.wait(asks).unwrap_or_else(PoisonError::into_inner);
    }
    drop(asks);
    let _turn = Turn(key.clone());
    let Some(server) = lock(&SERVER).clone() else {
        return false;
    };
    let request = Request {
        root,
        run,
        label: label.to_owned(),
        call: call.clone(),
        preview: tools::preview(&call.name, &call.arguments, ctx),
        reply,
    };
    if server.send(request).is_err() {
        return false;
    }
    match answer.recv() {
        Ok(Decision::Once) => true,
        Ok(Decision::ForRun) => {
            lock(&ASKS).granted.insert(key);
            true
        }
        Ok(Decision::Deny) | Err(_) => false,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::time::Duration;

    /// Held by every test that serves, since requests go to one frontend at a time.
    pub(crate) static SERVING: Mutex<()> = Mutex::new(());
    /// Far longer than any answer takes, so only a hang fails a test.
    const WAIT: Duration = Duration::from_secs(30);
    /// Ample for a thread to start and reach its wait, so a request it raised
    /// wrongly would arrive within it; a slower machine can only let a check
    /// pass untested, never fail it.
    const SETTLE: Duration = Duration::from_millis(500);

    /// A call that `write_file` and `run_command` both preview as its id.
    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: format!(r#"{{"content": "{id}", "command": "{id}"}}"#),
        }
    }

    /// Asks on a thread of its own, as an agent would, and hands back the answer.
    fn asking(run: u64, call: ToolCall, ctx: ToolContext) -> Receiver<bool> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(ask(7, run, "helper", &call, &ctx));
        });
        rx
    }

    #[test]
    fn with_nobody_serving_every_request_is_declined() {
        let _turn = lock(&SERVING);
        let quiet = ToolContext::default();
        *lock(&SERVER) = None;
        let never = asking(1901, call("a", "run_command"), quiet.clone());
        assert_eq!(never.recv_timeout(WAIT), Ok(false));

        // A frontend that went away is nobody.
        drop(serve());
        let gone = asking(1901, call("b", "run_command"), quiet.clone());
        assert_eq!(gone.recv_timeout(WAIT), Ok(false));

        // So is one that drops a request without answering it.
        let requests = serve();
        let left = asking(1901, call("c", "run_command"), quiet);
        drop(requests.recv_timeout(WAIT).unwrap());
        assert_eq!(left.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn an_answer_reaches_only_its_own_request() {
        let _turn = lock(&SERVING);
        let requests = serve();
        // Two tools, since calls of one tool in one run ask one at a time.
        let first = asking(1902, call("a", "write_file"), ToolContext::default());
        let second = asking(1902, call("b", "run_command"), ToolContext::default());
        let mut pending: Vec<Request> = (0..2)
            .map(|_| requests.recv_timeout(WAIT).unwrap())
            .collect();
        pending.sort_by(|x, y| x.call.id.cmp(&y.call.id));
        for request in &pending {
            assert_eq!(request.preview, request.call.id);
            assert_eq!((request.root, request.run), (7, 1902));
            assert_eq!(request.label, "helper");
        }
        let b = pending.pop().unwrap();
        let a = pending.pop().unwrap();

        // Answered in the other order, and differently.
        b.answer(Decision::Once);
        assert_eq!(second.recv_timeout(WAIT), Ok(true));
        a.answer(Decision::Deny);
        assert_eq!(first.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn allowing_for_a_run_covers_that_run_only() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let first = asking(1903, call("a", "run_command"), ToolContext::default());
        let request = requests.recv_timeout(WAIT).unwrap();
        request.answer(Decision::ForRun);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));

        let again = asking(1903, call("b", "run_command"), ToolContext::default());
        assert_eq!(again.recv_timeout(WAIT), Ok(true));
        assert!(requests.try_recv().is_err(), "the same tool asked again");
        assert!(granted(1903, "run_command"));
        assert!(!granted(1904, "run_command") && !granted(1903, "write_file"));

        let other_run = asking(1904, call("c", "run_command"), ToolContext::default());
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.run, 1904);
        request.answer(Decision::Deny);
        assert_eq!(other_run.recv_timeout(WAIT), Ok(false));

        let other_tool = asking(1903, call("d", "write_file"), ToolContext::default());
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.call.name, "write_file");
        request.answer(Decision::Deny);
        assert_eq!(other_tool.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn agents_asking_at_once_wait_for_one_answer() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let first = asking(1906, call("a", "write_file"), ToolContext::default());
        let request = requests.recv_timeout(WAIT).unwrap();
        let others = [
            asking(1906, call("b", "write_file"), ToolContext::default()),
            asking(1906, call("c", "write_file"), ToolContext::default()),
        ];
        assert!(requests.recv_timeout(SETTLE).is_err(), "a second request");
        request.answer(Decision::ForRun);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
        for other in others {
            assert_eq!(other.recv_timeout(WAIT), Ok(true));
        }
        assert!(requests.try_recv().is_err(), "the same tool asked again");

        // Allowed once, the next in line asks for itself.
        let first = asking(1907, call("d", "write_file"), ToolContext::default());
        let request = requests.recv_timeout(WAIT).unwrap();
        let next = asking(1907, call("e", "write_file"), ToolContext::default());
        assert!(requests.recv_timeout(SETTLE).is_err(), "a second request");
        request.answer(Decision::Once);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.call.id, "e");
        request.answer(Decision::Deny);
        assert_eq!(next.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn a_stopped_agent_stops_waiting_for_its_answer() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let ctx = ToolContext::default();
        let waiting = asking(1905, call("a", "run_command"), ctx.clone());
        let request = requests.recv_timeout(WAIT).unwrap();
        ctx.cancel.set();
        assert_eq!(waiting.recv_timeout(WAIT), Ok(false));
        // An answer that comes too late goes nowhere.
        request.answer(Decision::Once);

        // Once stopped, it asks nobody.
        let stopped = asking(1905, call("b", "run_command"), ctx);
        assert_eq!(stopped.recv_timeout(WAIT), Ok(false));
        assert!(requests.try_recv().is_err(), "a stopped agent still asked");

        // Stopped while in line behind another agent's request, it stops waiting too.
        let first = asking(1905, call("c", "run_command"), ToolContext::default());
        let request = requests.recv_timeout(WAIT).unwrap();
        let behind = ToolContext::default();
        let in_line = asking(1905, call("d", "run_command"), behind.clone());
        assert!(requests.recv_timeout(SETTLE).is_err(), "a second request");
        behind.cancel.set();
        assert_eq!(in_line.recv_timeout(WAIT), Ok(false));
        request.answer(Decision::Once);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
    }
}
