//! Approval requests from agents that are not on screen.
//!
//! A helper runs on a thread of its own, with no card to show, so when one of
//! its calls needs a yes it asks here and whichever frontend took `serve`
//! answers by asking the person. The helper's own gate has already settled
//! everything else, so only a call that still needs a yes comes here. Core
//! keeps no rules of its own except `ForRun`, remembered per run and rule --
//! the tool and pattern `permissions::scope` gives the call -- so a run of
//! many helpers asks once per rule rather than once per helper, and a yes to
//! `cargo test` for the run does not let `cargo run` through. That has to
//! hold when helpers ask at the same moment too, so a run has one request out
//! per rule at a time: the others wait for its answer, and ask for themselves
//! only if it was not `ForRun`. A call no rule would cover safely, such as a
//! command the floor cannot read, gets no rule: `ForRun` is a yes to it alone.
//! And a granted rule covers a later call only if that call still clears the
//! floor and the project's own rules, checked fresh by `gate::cleared`, so a
//! card answered for one mild call cannot wave through a worse one. What the
//! user told a session to stop asking about reaches its helpers the same
//! way, by rule, or for a tool no rule could narrow by what `gate::grant`
//! keeps: the tool, one MCP server's reads, one app for computer use.
//!
//! Each request carries its own reply channel, so an answer can only reach
//! the call it was given for: there is no id to match up, or to forge. And
//! every way of not answering is a no. With nobody serving, a request is
//! declined at once; a request dropped unanswered is declined; a stop sends
//! the waiting agent a no. An agent with nobody to ask stops short rather
//! than hanging or gaining power.

use crate::chat::ToolCall;
use crate::permissions;
use crate::tools::{self, ToolContext};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Condvar, Mutex, MutexGuard, PoisonError};

/// What was said to a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Decision {
    /// Run this call.
    Once,
    /// Run this call, and later calls in the same run that the same rule
    /// covers without asking.
    ForRun,
    /// Do not run it, for the reason the user typed, empty when they gave none.
    Deny(String),
    /// Run this call with these arguments in place of the model's, as the
    /// program driving the run changed them.
    Changed(String),
    /// Do not run it, for the reason the program driving the run gave, empty
    /// when it gave none.
    Declined(String),
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
    /// Why the helper's gate put the call to the user, such as Auto mode's
    /// judge being unsure, for the card; empty when it asks as it always does.
    pub why: String,
    /// The run and rule a `ForRun` answer covers, if any does.
    key: Option<Key>,
    reply: Sender<Decision>,
}

impl Request {
    /// Whether this asks again what `earlier` asked: the same rule in the
    /// same run, which has one request out at a time.
    pub fn repeats(&self, earlier: &Request) -> bool {
        self.key.is_some() && self.key == earlier.key
    }

    /// The pattern of the rule an answer of `ForRun` covers, or `None` when
    /// it would cover this call alone, so the card offers no such answer.
    pub fn for_run(&self) -> Option<&str> {
        self.key.as_ref().map(|(_, _, _, pattern)| pattern.as_str())
    }

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
        let _ = self.reply.send(Decision::Deny(String::new()));
    }
}

/// A run, or a session for its helpers, and the project, tool and pattern
/// of the rule an answer covers in it.
type Key = (u64, Option<PathBuf>, String, String);

/// Where requests go: the frontend that served last, or nobody.
static SERVER: Mutex<Option<Sender<Request>>> = Mutex::new(None);
/// What core remembers per run and tool, under one lock so that a caller
/// checks both and starts waiting in one step, with no gap to miss a wake in.
static ASKS: Mutex<Asks> = Mutex::new(Asks {
    granted: BTreeSet::new(),
    asking: BTreeSet::new(),
    sessions: BTreeSet::new(),
});
/// Wakes callers waiting in line when a request is settled or they are stopped.
static SETTLED: Condvar = Condvar::new();

struct Asks {
    /// The rules each run's calls may use without asking again. Kept for the
    /// life of the process: a run's number is never reused, and a run adds at
    /// most one entry per card answered.
    granted: BTreeSet<Key>,
    /// The rules each run has a request out for.
    asking: BTreeSet<Key>,
    /// What each session allowed its helpers, until it is forgotten.
    sessions: BTreeSet<Key>,
}

/// A run's one request out for a rule, which lets the next in line ask when dropped.
struct Turn(Key);

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

/// Whether run `run` was already allowed the calls the rule for `call` in
/// `ctx` covers, asking nobody.
pub(crate) fn granted(run: u64, call: &ToolCall, ctx: &ToolContext) -> bool {
    key(run, call, ctx).is_some_and(|key| lock(&ASKS).granted.contains(&key))
}

/// What a `ForRun` answer to `call` in run `run` covers, or `None` when it
/// covers only the call.
fn key(run: u64, call: &ToolCall, ctx: &ToolContext) -> Option<Key> {
    let project = ctx.workspace.clone();
    // A batch's card was a yes to its kinds of step together, not to each
    // step's tool, so it covers only batches of the same kinds.
    if crate::browser::batch_calls(call).is_some() {
        let kinds = crate::gate::run_grant(call);
        return Some((run, project, call.name.clone(), kinds));
    }
    permissions::scope(call, ctx).map(|(tool, pattern)| (run, project, tool, pattern))
}

/// Lets session `root`'s helpers make the calls `call` stands for without
/// asking, as the user told the session to stop asking about it.
pub fn allow_for_session(root: u64, call: &ToolCall, ctx: &ToolContext) {
    if let Some(key) = session_key(root, call, ctx) {
        lock(&ASKS).sessions.insert(key);
    }
}

/// Forgets what session `root` allowed its helpers, for a session removed,
/// whose id can be given to the next.
pub fn forget_session(root: u64) {
    lock(&ASKS).sessions.retain(|key| key.0 != root);
}

/// Whether session `root` allowed its helpers the calls the rule for `call`
/// in `ctx` covers.
pub(crate) fn allowed_by_session(root: u64, call: &ToolCall, ctx: &ToolContext) -> bool {
    session_key(root, call, ctx).is_some_and(|key| lock(&ASKS).sessions.contains(&key))
}

/// What a session's yes to `call` covers for its helpers: the rule the call
/// falls under, or for a tool none of whose arguments could narrow one what
/// the session's own grant keeps, `gate::grant`'s; `None` for a call like a
/// hidden command, which the yes covered alone.
fn session_key(root: u64, call: &ToolCall, ctx: &ToolContext) -> Option<Key> {
    let whole = || (root, None, call.name.clone(), crate::gate::grant(call));
    key(root, call, ctx).or_else(|| permissions::shapeless(&call.name).then(whole))
}

/// Whether `call` may run, waiting for the frontend's answer unless the agent
/// is stopped first; `why` goes on the card. A yes may carry changed
/// arguments to run instead, and a no the reason given, after who gave it,
/// empty without one. `lasting` when a yes for the run may cover later
/// calls, which it may not where the gate asks every time, such as for an
/// ask rule.
pub(crate) fn ask(
    root: u64,
    run: u64,
    label: &str,
    call: &ToolCall,
    ctx: &ToolContext,
    why: &str,
    lasting: bool,
) -> Result<Option<String>, String> {
    let (reply, answer) = mpsc::channel();
    let stop = reply.clone();
    // Set up before taking the lock, since a stopped token runs it at once.
    let _waker = ctx.cancel.on_set(move || {
        let _ = stop.send(Decision::Deny(String::new()));
        // Taken so that a caller between its check and its wait cannot miss this.
        drop(lock(&ASKS));
        SETTLED.notify_all();
    });
    let key = key(run, call, ctx).filter(|_| lasting);
    // The grant answers for this rule from whichever call first asked about
    // it, so it covers this one only if this one clears on its own. Worked
    // out before the lock, since letting go of it between the check and the
    // wait below would let a stop's wake land in the gap and be missed.
    let clears = crate::gate::cleared(call, ctx);
    let mut asks = lock(&ASKS);
    loop {
        if ctx.cancel.is_set() {
            return Err(String::new());
        }
        let Some(key) = &key else {
            break;
        };
        if clears && asks.granted.contains(key) {
            return Ok(None);
        }
        if asks.asking.insert(key.clone()) {
            break;
        }
        // Another call this rule covers in this run is waiting on a card,
        // and an answer of ForRun there covers this call too.
        asks = SETTLED.wait(asks).unwrap_or_else(PoisonError::into_inner);
    }
    drop(asks);
    let _turn = key.clone().map(Turn);
    let Some(server) = lock(&SERVER).clone() else {
        return Err(String::new());
    };
    let request = Request {
        root,
        run,
        label: label.to_owned(),
        call: call.clone(),
        preview: tools::preview(&call.name, &call.arguments, ctx),
        why: why.to_owned(),
        key: key.clone(),
        reply,
    };
    if server.send(request).is_err() {
        return Err(String::new());
    }
    match answer.recv() {
        Ok(Decision::Once) => Ok(None),
        Ok(Decision::Changed(arguments)) => Ok(Some(arguments)),
        Ok(Decision::ForRun) => {
            if let Some(key) = key {
                lock(&ASKS).granted.insert(key);
            }
            Ok(None)
        }
        Ok(Decision::Deny(reason)) => Err(why_given("The user said why", &reason)),
        // Not the user's words, so the agent is not told they are.
        Ok(Decision::Declined(reason)) => {
            Err(why_given("The program driving this run said why", &reason))
        }
        Err(_) => Err(String::new()),
    }
}

/// `reason` as the agent is told it, after who gave it, or nothing without one.
fn why_given(who: &str, reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        return String::new();
    }
    format!("{who}: {reason}")
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

    /// A call of `name` its card previews as `id`, running `line` or
    /// writing `id` to `line`.
    fn call(id: &str, name: &str, line: &str) -> ToolCall {
        let arguments = match name {
            "run_command" => serde_json::json!({"command": line}),
            _ => serde_json::json!({"path": line, "content": id}),
        };
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.to_string(),
        }
    }

    /// A context in a project of its own, where calls get rules.
    fn project() -> ToolContext {
        let dir = std::env::temp_dir().join("letronna-approvals");
        std::fs::create_dir_all(&dir).unwrap();
        ToolContext {
            workspace: Some(dir),
            ..Default::default()
        }
    }

    /// Asks on a thread of its own, as an agent would, and hands back the answer.
    fn asking(run: u64, call: ToolCall, ctx: ToolContext) -> Receiver<bool> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(ask(7, run, "helper", &call, &ctx, "auto: unsure — why", true).is_ok());
        });
        rx
    }

    #[test]
    fn with_nobody_serving_every_request_is_declined() {
        let _turn = lock(&SERVING);
        let quiet = project();
        *lock(&SERVER) = None;
        let test = call("a", "run_command", "cargo test");
        let never = asking(1901, test.clone(), quiet.clone());
        assert_eq!(never.recv_timeout(WAIT), Ok(false));

        // A frontend that went away is nobody.
        drop(serve());
        let gone = asking(1901, test.clone(), quiet.clone());
        assert_eq!(gone.recv_timeout(WAIT), Ok(false));

        // So is one that drops a request without answering it.
        let requests = serve();
        let left = asking(1901, test, quiet);
        drop(requests.recv_timeout(WAIT).unwrap());
        assert_eq!(left.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn an_answer_reaches_only_its_own_request() {
        let _turn = lock(&SERVING);
        let requests = serve();
        // Two rules, since calls under one rule in one run ask one at a time.
        let first = asking(1902, call("a", "write_file", "notes/a.txt"), project());
        let second = asking(1902, call("b", "run_command", "b"), project());
        let mut pending: Vec<Request> = (0..2)
            .map(|_| requests.recv_timeout(WAIT).unwrap())
            .collect();
        pending.sort_by(|x, y| x.call.id.cmp(&y.call.id));
        for request in &pending {
            assert_eq!(request.preview, request.call.id);
            assert_eq!((request.root, request.run), (7, 1902));
            assert_eq!(request.label, "helper");
            assert_eq!(request.why, "auto: unsure — why");
        }
        let b = pending.pop().unwrap();
        let a = pending.pop().unwrap();

        // Answered in the other order, and differently.
        b.answer(Decision::Once);
        assert_eq!(second.recv_timeout(WAIT), Ok(true));
        a.answer(Decision::Deny(String::new()));
        assert_eq!(first.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn allowing_for_a_run_covers_that_rule_in_that_run_only() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let test = |id| call(id, "run_command", "cargo test");
        let first = asking(1903, test("a"), project());
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.for_run(), Some("cargo test"));
        request.answer(Decision::ForRun);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));

        // What the rule `cargo test` covers asks no more in the run.
        let again = asking(
            1903,
            call("b", "run_command", "cargo test --release"),
            project(),
        );
        assert_eq!(again.recv_timeout(WAIT), Ok(true));
        assert!(requests.try_recv().is_err(), "the same rule asked again");
        assert!(granted(1903, &test("c"), &project()));
        assert!(!granted(1904, &test("c"), &project()));

        // Another command of the same tool is another rule, and asks.
        let run = call("d", "run_command", "cargo run");
        assert!(!granted(1903, &run, &project()));
        let other_rule = asking(1903, run, project());
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.call.id, "d");
        request.answer(Decision::Deny(String::new()));
        assert_eq!(other_rule.recv_timeout(WAIT), Ok(false));

        let other_run = asking(1904, test("e"), project());
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.run, 1904);
        request.answer(Decision::Deny(String::new()));
        assert_eq!(other_run.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn a_yes_for_the_run_lasts_only_where_a_rule_may_cover_the_call() {
        let _turn = lock(&SERVING);
        let requests = serve();
        // The floor cannot read what this runs, so no rule would cover it.
        let hidden = || call("a", "run_command", "sh -c \"$(cat run.txt)\"");
        let first = asking(1908, hidden(), project());
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.for_run(), None);
        request.answer(Decision::ForRun);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
        assert!(!granted(1908, &hidden(), &project()));

        // So it asks again, and two such calls are out at once.
        let again = asking(1908, hidden(), project());
        let other = asking(
            1908,
            call("b", "run_command", "sh -c \"$(cat b.txt)\""),
            project(),
        );
        let mut pending: Vec<Request> = (0..2)
            .map(|_| requests.recv_timeout(WAIT).unwrap())
            .collect();
        assert!(!pending[0].repeats(&pending[1]));
        for request in pending.drain(..) {
            request.answer(Decision::Deny(String::new()));
        }
        assert_eq!(again.recv_timeout(WAIT), Ok(false));
        assert_eq!(other.recv_timeout(WAIT), Ok(false));

        // Nor does a yes last where the gate asks every time, as for an ask rule.
        let (test, ctx) = (call("c", "run_command", "cargo test"), project());
        let asked = std::thread::spawn(move || ask(7, 1908, "helper", &test, &ctx, "", false));
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.for_run(), None);
        request.answer(Decision::ForRun);
        assert!(asked.join().unwrap().is_ok());
        assert!(!granted(
            1908,
            &call("d", "run_command", "cargo test"),
            &project()
        ));
    }

    #[test]
    fn agents_asking_at_once_wait_for_one_answer() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let write = |id| call(id, "write_file", "notes/a.txt");
        let first = asking(1906, write("a"), project());
        let request = requests.recv_timeout(WAIT).unwrap();
        let others = [
            asking(1906, write("b"), project()),
            // Beside it, in the same folder, is under the same rule.
            asking(1906, call("c", "write_file", "notes/c.txt"), project()),
        ];
        assert!(requests.recv_timeout(SETTLE).is_err(), "a second request");
        request.answer(Decision::ForRun);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
        for other in others {
            assert_eq!(other.recv_timeout(WAIT), Ok(true));
        }
        assert!(requests.try_recv().is_err(), "the same rule asked again");

        // Allowed once, the next in line asks for itself.
        let first = asking(1907, write("d"), project());
        let request = requests.recv_timeout(WAIT).unwrap();
        let next = asking(1907, write("e"), project());
        assert!(requests.recv_timeout(SETTLE).is_err(), "a second request");
        request.answer(Decision::Once);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.call.id, "e");
        request.answer(Decision::Deny(String::new()));
        assert_eq!(next.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn a_stopped_agent_stops_waiting_for_its_answer() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let test = |id| call(id, "run_command", "cargo test");
        let ctx = project();
        let waiting = asking(1905, test("a"), ctx.clone());
        let request = requests.recv_timeout(WAIT).unwrap();
        ctx.cancel.set();
        assert_eq!(waiting.recv_timeout(WAIT), Ok(false));
        // An answer that comes too late goes nowhere.
        request.answer(Decision::Once);

        // Once stopped, it asks nobody.
        let stopped = asking(1905, test("b"), ctx);
        assert_eq!(stopped.recv_timeout(WAIT), Ok(false));
        assert!(requests.try_recv().is_err(), "a stopped agent still asked");

        // Stopped while in line behind another agent's request, it stops waiting too.
        let first = asking(1905, test("c"), project());
        let request = requests.recv_timeout(WAIT).unwrap();
        let behind = project();
        let in_line = asking(1905, test("d"), behind.clone());
        assert!(requests.recv_timeout(SETTLE).is_err(), "a second request");
        behind.cancel.set();
        assert_eq!(in_line.recv_timeout(WAIT), Ok(false));
        request.answer(Decision::Once);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
    }

    #[test]
    fn a_yes_for_the_run_to_a_page_covers_its_host_alone() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let fetch = |url: &str| ToolCall {
            id: "fetch".into(),
            name: "web_fetch".into(),
            arguments: serde_json::json!({ "url": url }).to_string(),
        };
        let ctx = ToolContext::default();
        let first = asking(1909, fetch("https://docs.rs/serde"), ctx.clone());
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.for_run(), Some("docs.rs"));
        request.answer(Decision::ForRun);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
        assert!(granted(1909, &fetch("https://docs.rs/tokio"), &ctx));
        // Anywhere else, the gate goes on to the judge or a card.
        assert!(!granted(
            1909,
            &fetch("https://evil.example/?k=secret"),
            &ctx
        ));
    }

    #[test]
    fn what_a_session_stopped_asking_about_reaches_its_helpers_by_rule() {
        let ctx = project();
        allow_for_session(1910, &call("a", "run_command", "cargo test"), &ctx);
        assert!(allowed_by_session(
            1910,
            &call("b", "run_command", "cargo test --release"),
            &ctx
        ));
        assert!(!allowed_by_session(
            1910,
            &call("c", "run_command", "cargo run"),
            &ctx
        ));
        // Nor in another session, or another folder.
        let test = call("d", "run_command", "cargo test");
        assert!(!allowed_by_session(1911, &test, &ctx));
        assert!(!allowed_by_session(1910, &test, &ToolContext::default()));
        // A hidden command's yes covered it alone.
        let hidden = || call("e", "run_command", "sh -c \"$(cat run.txt)\"");
        allow_for_session(1910, &hidden(), &ctx);
        assert!(!allowed_by_session(1910, &hidden(), &ctx));
        // A tool no rule could narrow is allowed whole, until forgotten.
        let image = ToolCall {
            id: "f".into(),
            name: "make_image".into(),
            arguments: serde_json::json!({ "prompt": "a lighthouse" }).to_string(),
        };
        allow_for_session(1910, &image, &ctx);
        assert!(allowed_by_session(1910, &image, &ToolContext::default()));
        forget_session(1910);
        assert!(!allowed_by_session(1910, &image, &ctx));
        assert!(!allowed_by_session(1910, &test, &ctx));
    }

    #[test]
    fn a_no_carries_the_reason_the_user_gave() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let (tx, said) = mpsc::channel();
        std::thread::spawn(move || {
            let call = call("a", "run_command", "ls");
            let _ = tx.send(ask(
                7,
                1912,
                "helper",
                &call,
                &ToolContext::default(),
                "",
                true,
            ));
        });
        let request = requests.recv_timeout(WAIT).unwrap();
        request.answer(Decision::Deny("not on the main branch".into()));
        assert_eq!(
            said.recv_timeout(WAIT),
            Ok(Err("The user said why: not on the main branch".to_owned()))
        );
    }

    /// `cargo test` allowed for run `run` in `ctx`'s project, as a card's
    /// "for the run" leaves it.
    fn tests_allowed(run: u64, ctx: &ToolContext, requests: &Receiver<Request>) {
        let first = asking(run, call("a", "run_command", "cargo test"), ctx.clone());
        requests
            .recv_timeout(WAIT)
            .unwrap()
            .answer(Decision::ForRun);
        assert_eq!(first.recv_timeout(WAIT), Ok(true));
    }

    #[test]
    fn a_grant_does_not_cover_a_later_call_the_floor_refuses_or_asks_about() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let ctx = project();
        tests_allowed(1913, &ctx, &requests);
        // One the floor would refuse on its own, and one it keeps for a
        // person, still ask, though the run allowed a command.
        for (id, line) in [
            ("b", "Remove-Item -Recurse -Force ~"),
            ("c", "git reset --hard HEAD~1"),
        ] {
            let worse = asking(1913, call(id, "run_command", line), ctx.clone());
            let request = requests.recv_timeout(WAIT).unwrap();
            assert_eq!(request.call.id, id);
            assert_eq!(request.for_run(), None);
            request.answer(Decision::Deny(String::new()));
            assert_eq!(worse.recv_timeout(WAIT), Ok(false));
        }
    }

    #[test]
    fn a_grant_does_not_cover_a_later_call_the_projects_rule_denies() {
        let _turn = lock(&SERVING);
        let requests = serve();
        tests_allowed(1914, &project(), &requests);
        let denying = scratch("denies-run-command");
        std::fs::create_dir_all(denying.join(".letronna")).unwrap();
        std::fs::write(
            denying.join(".letronna/permissions.json"),
            r#"[{"tool": "run_command", "effect": "deny"}]"#,
        )
        .unwrap();
        let ctx = ToolContext {
            workspace: Some(denying),
            ..Default::default()
        };
        let denied = asking(1914, call("b", "run_command", "cargo test"), ctx);
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.call.id, "b");
        request.answer(Decision::Deny(String::new()));
        assert_eq!(denied.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn a_stop_reaches_a_call_its_runs_grant_does_not_cover() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let ctx = project();
        tests_allowed(1915, &ctx, &requests);
        let dangerous = |id: &str| call(id, "run_command", "git reset --hard HEAD~1");
        let out = asking(1915, dangerous("b"), ctx);
        let request = requests.recv_timeout(WAIT).unwrap();

        // Stopped at every point on the way to its wait, each one still
        // stops waiting rather than holding out for an answer.
        for pause in 0..200 {
            let behind = project();
            let in_line = asking(1915, dangerous("c"), behind.clone());
            std::thread::sleep(Duration::from_micros(pause * 5));
            behind.cancel.set();
            assert_eq!(in_line.recv_timeout(WAIT), Ok(false), "after {pause}");
            // One that got its card before the stop leaves it unanswered.
            while let Ok(stray) = requests.try_recv() {
                drop(stray);
            }
        }
        request.answer(Decision::Deny(String::new()));
        assert_eq!(out.recv_timeout(WAIT), Ok(false));
    }

    #[test]
    fn a_yes_for_the_run_to_a_batch_covers_only_batches_of_the_same_steps() {
        let batch = |steps: &str| ToolCall {
            id: "b".into(),
            name: "use_page".into(),
            arguments: format!(r#"{{"action": "batch", "steps": {steps}}}"#),
        };
        let hovers =
            batch(r#"[{"action": "hover", "ref": "e1"}, {"action": "click", "ref": "e2"}]"#);
        let kinds = "use_page batch of browse, use_page".to_owned();
        let ctx = project();
        assert_eq!(
            key(1916, &hovers, &ctx),
            Some((1916, ctx.workspace.clone(), "use_page".into(), kinds))
        );
        // Neither a yes to use_page nor one to browse is a yes to it, nor it
        // to them.
        let click = ToolCall {
            id: "c".into(),
            name: "use_page".into(),
            arguments: r#"{"action": "click", "ref": "e1"}"#.into(),
        };
        assert_ne!(key(1916, &click, &ctx), key(1916, &hovers, &ctx));
    }

    #[test]
    fn a_session_yes_to_one_servers_reads_does_not_reach_another_servers() {
        let read = |server: &str| ToolCall {
            id: "r".into(),
            name: crate::mcp::READ_RESOURCE.into(),
            arguments: format!(r#"{{"server": "{server}", "uri": "notes://today"}}"#),
        };
        let ctx = project();
        allow_for_session(1917, &read("near"), &ctx);
        assert!(allowed_by_session(1917, &read("near"), &ctx));
        assert!(!allowed_by_session(1917, &read("far"), &ctx));
        forget_session(1917);
    }

    #[test]
    fn a_program_can_change_the_call_or_say_why_not_as_itself() {
        let _turn = lock(&SERVING);
        let requests = serve();
        let declined = "The program driving this run said why: not now";
        let answers = [
            (
                1950,
                Decision::Changed("{}".into()),
                Ok(Some("{}".to_owned())),
            ),
            (
                1951,
                Decision::Declined(" not now ".into()),
                Err(declined.to_owned()),
            ),
        ];
        for (run, decision, expected) in answers {
            let (tx, said) = mpsc::channel();
            std::thread::spawn(move || {
                let call = call("a", "run_command", "ls");
                let _ = tx.send(ask(7, run, "helper", &call, &project(), "", true));
            });
            requests.recv_timeout(WAIT).unwrap().answer(decision);
            assert_eq!(said.recv_timeout(WAIT), Ok(expected));
        }
    }

    /// An empty folder for one test, which the next run of it replaces.
    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-approvals-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
