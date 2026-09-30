//! The agent loop, with no idea what is driving it.
//!
//! One user message means: call the model, run whatever tools it asks for,
//! call it again with the results, and keep going until it answers in prose.
//! That loop is the same whether a window is drawing approval cards or a
//! terminal is printing `[y/n]`, so it lives here and the frontends only
//! supply the parts that differ: how to reach the user, and how to wait.
//!
//! The shape is a state machine rather than a callback trait on purpose. A
//! window answers an approval later, from a click; a terminal answers it now,
//! from a keypress. Asking the loop to `await` a frontend would force both
//! into whichever model fitted the other worse. Instead the loop says what it
//! needs next and stops, and the caller comes back when it has it.

use crate::chat::{Connection, Event, Message, Replay, ToolCall};
use crate::gate::Outcome;
use crate::tools;
use std::collections::VecDeque;

/// Tool rounds per user message. Enough for a real investigation, small
/// enough that a looping model stops on its own.
pub const MAX_ROUNDS: usize = 25;

/// Times one user message may ask a reply cut off at the output limit to go
/// on. Three more replies make an answer four times the cap; one still
/// running past that is more likely looping than nearly done.
const MAX_CONTINUATIONS: usize = 3;

/// The notice that asks a reply cut off at the output limit to go on.
const CONTINUE: &str =
    "Your last reply was cut off at the output limit; continue exactly where it stopped.";

/// The notice for a reply cut off inside a tool call, which was not run.
const CALL_AGAIN: &str = "Your last reply was cut off at the output limit in the middle \
    of a tool call, so the call was not run. Make it again with less in it, splitting the \
    work over several calls if it needs more room.";

/// The notice after a reply with nothing in it, or only thinking, in the
/// words other agents send, which models already answer well.
const SPEAK_UP: &str = "Your previous response had no visible output. Please continue and \
    produce a user-visible response.";

/// The notice after a short answer that stops at what it is about to do.
const CONTINUE_NOW: &str = "Continue now.";

/// The notice when a turn that changed files is about to end without
/// running anything since.
const VERIFY: &str = "You changed files and have not run anything since. Before saying the \
    work is done, check it: run the project's build or tests, or say why that cannot be done here.";

/// Tools whose successful calls change files in the project.
const EDITS: [&str; 2] = ["write_file", "edit_file"];

/// The longest answer read as only announcing its next step: room for a
/// sentence or two before "Now let me...", too little for a finished answer
/// that happens to end on a plan.
const ANNOUNCEMENT_CHARS: usize = 400;

/// The notice at the round cap, sent with the tools still offered but none
/// allowed.
const WRAP_UP: &str = "You have reached the tool-round limit. Summarize what is done and \
    what remains, then stop.";

/// The answer a call gets when the turn is stopped while it runs; the front
/// end that stopped it records it before `Turn::stop`. The call may have
/// done part of its work, so the model is told to look first.
pub const STOPPED_WHILE_RUNNING: &str = "Stopped while it ran; check before retrying.";

/// The answer a call gets when the turn is stopped before it starts.
pub const STOPPED_BEFORE_START: &str = "Not run: stopped before it started.";

/// Calls from one reply run at once. Enough for a model reading around a
/// project in one go, few enough that a reply of dozens never starts dozens
/// of threads and requests together.
pub const MAX_BATCH: usize = 8;

/// What the caller should do next.
#[derive(Debug, PartialEq)]
pub enum Next {
    /// Send the conversation to the model again.
    Ask,
    /// Run this call. The caller has already decided it may.
    Run(ToolCall),
    /// This call needs the user: an approval, or a question to answer.
    AskUser(ToolCall),
    /// This call may not run, and the reason goes back to the model as its
    /// result with no card shown.
    Refuse(ToolCall, String),
    /// The turn is over.
    Done,
}

/// A notice sending the model back to work, and the line that tells the
/// user why.
pub struct Nudge {
    pub notice: Message,
    pub why: &'static str,
}

/// The reply being streamed in. Kept separate from `Message` so a half-built
/// answer is never mistaken for a finished one.
#[derive(Default, Clone)]
pub struct Live {
    pub content: String,
    pub reasoning: String,
    pub tokens: u64,
    pub per_second: f32,
    pub prompt_tokens: u64,
    pub cached_tokens: u64,
    /// The model's window as the model facts give it, once the usage is in.
    pub window: u64,
    pub cache_write_tokens: u64,
    pub reasoning_tokens: u64,
    /// What OpenRouter billed for the reply, in dollars.
    pub cost: Option<f64>,
    /// What summarizing older turns for the request took.
    pub summary_usage: Option<crate::usage::Usage>,
    /// Why the cache served far less of the request than of the last one.
    pub cache_miss: Option<crate::cache_miss::Reason>,
    pub calls: Vec<ToolCall>,
    pub replay: Option<Replay>,
    /// The try about to be made again after a failure, while it waits.
    pub retrying: Option<crate::retry::Retry>,
    /// Set while the turns the window dropped are summarized, until the
    /// reply starts.
    pub compacting: bool,
}

/// One user turn in flight.
pub struct Turn {
    pub connection: Connection,
    pub live: Live,
    rounds: usize,
    limit: usize,
    queue: VecDeque<ToolCall>,
    /// Tool results for the batch being worked through.
    results: Vec<Message>,
    /// What the calls' file writes kept, taken onto each result.
    files: crate::history::Log,
    /// Set when the reply being streamed stopped at the output limit: true
    /// when it stopped inside a tool call.
    cut_off: Option<bool>,
    continued: usize,
    /// The batch's call ids in the order the model made them, which its results keep.
    order: Vec<String>,
    /// An empty reply was sent back since the last tool round.
    asked_again: bool,
    /// An answer that stopped at its next step was sent back this turn.
    pushed_on: bool,
    /// Whether an answer after unchecked edits is sent back to check them.
    verify: bool,
    /// A file was changed since the last command ran.
    edited: bool,
    /// Unchecked edits were sent back this turn.
    asked_to_verify: bool,
}

impl Turn {
    pub fn new(connection: Connection) -> Self {
        Self {
            files: crate::history::Log::new(connection.conversation),
            connection,
            live: Live::default(),
            rounds: 0,
            limit: MAX_ROUNDS,
            queue: VecDeque::new(),
            results: Vec::new(),
            cut_off: None,
            continued: 0,
            order: Vec::new(),
            asked_again: false,
            pushed_on: false,
            verify: false,
            edited: false,
            asked_to_verify: false,
        }
    }

    /// This turn, sending back once an answer that follows file edits with
    /// no command run since, when `on`.
    pub fn verifying(self, on: bool) -> Self {
        Self { verify: on, ..self }
    }

    /// A turn that may run `rounds` tool rounds instead of `MAX_ROUNDS`.
    pub fn with_limit(connection: Connection, rounds: usize) -> Self {
        Self {
            limit: rounds,
            ..Self::new(connection)
        }
    }

    /// What a call of this turn runs with: its folder and session, and the
    /// log its file writes are noted in.
    pub fn tool_context(&self) -> tools::ToolContext {
        tools::ToolContext {
            files: self.files.clone(),
            ..tools::ToolContext::of(&self.connection)
        }
    }

    /// Fold a streamed event into the reply being built.
    pub fn absorb(&mut self, event: &Event) {
        // Whatever follows a retry comes from the new try, and whatever
        // follows the summary from the reply, so either is under way.
        self.live.retrying = None;
        self.live.compacting = false;
        match event {
            Event::Delta(text) => self.live.content.push_str(text),
            Event::Reasoning(text) => self.live.reasoning.push_str(text),
            Event::Usage {
                completion,
                per_second,
                prompt,
                cached,
                cache_write,
                reasoning,
                cost,
            } => {
                self.live.tokens = *completion;
                self.live.per_second = *per_second;
                self.live.prompt_tokens = *prompt;
                self.live.cached_tokens = *cached;
                let model = self.connection.model.trim();
                let facts = crate::models::facts(&self.connection.endpoint, model);
                self.live.window = facts.context.unwrap_or(0);
                self.live.cache_write_tokens = *cache_write;
                self.live.reasoning_tokens = *reasoning;
                self.live.cost = *cost;
            }
            Event::SummaryUsage(usage) => self.live.summary_usage = Some(*usage),
            Event::CacheMiss(reason) => self.live.cache_miss = Some(*reason),
            Event::ToolCalls(calls) => self.live.calls = calls.clone(),
            Event::Replay(replay) => self.live.replay = Some(replay.clone()),
            Event::Retrying(retry) => self.live.retrying = Some(*retry),
            Event::Compacting => self.live.compacting = true,
            // The front end keeps it with the session.
            Event::Summarized { .. } => {}
            Event::CutOff { in_call } => self.cut_off = Some(*in_call),
            Event::Finished(_) => {}
        }
    }

    /// The assistant message for the reply just finished, its cost settled,
    /// and a clean slate for the next one.
    pub fn take_reply(&mut self, model: &str) -> Message {
        let live = std::mem::take(&mut self.live);
        let mut reply = Message {
            role: "assistant".into(),
            content: live.content,
            model: model.to_owned(),
            reasoning: live.reasoning,
            tokens: live.tokens,
            per_second: live.per_second,
            prompt_tokens: live.prompt_tokens,
            cached_tokens: live.cached_tokens,
            window: live.window,
            cache_write_tokens: live.cache_write_tokens,
            reasoning_tokens: live.reasoning_tokens,
            cost: live.cost,
            summary_usage: live.summary_usage,
            cache_miss: live.cache_miss,
            at: crate::chat::now(),
            tool_calls: live.calls,
            replay: live.replay.map(Box::new),
            ..Default::default()
        };
        let usage = crate::usage::Usage::of(&reply).priced(&self.connection.endpoint, model);
        reply.cost = usage.cost;
        reply.cost_estimated = usage.estimated;
        reply
    }

    /// The notice sending the model back to work after the reply just
    /// finished, if it needs one: a reply cut off at the output limit, one
    /// with nothing in it, a short answer that stops at "Now let me...", or,
    /// when verifying, an answer that follows edits no command has checked.
    /// A second empty reply takes its thinking as the answer, and calls made
    /// in the wrap-up at the round cap are dropped unrun. An error when a
    /// reply is still cut off after `MAX_CONTINUATIONS` notices, or still
    /// empty with no thinking to take, so the turn ends saying why.
    pub fn continuation(&mut self, reply: &mut Message) -> Result<Option<Nudge>, String> {
        if let Some(in_call) = self.cut_off.take() {
            if self.continued >= MAX_CONTINUATIONS {
                return Err(format!(
                    "The reply was still cut off at the output limit after {MAX_CONTINUATIONS} \
                     continuations, so it stops here."
                ));
            }
            self.continued += 1;
            let notice = if in_call { CALL_AGAIN } else { CONTINUE };
            return Ok(Some(nudge(
                notice,
                "Cut off at the output limit; continuing.",
            )));
        }
        let wrap_up = self.connection.wrap_up;
        // None were allowed; a server that ignores that may send some anyway.
        if wrap_up {
            reply.tool_calls.clear();
        }
        if !reply.tool_calls.is_empty() {
            return Ok(None);
        }
        if reply.content.trim().is_empty() {
            // Some routes turn down an assistant message with no content, so
            // an empty reply is dropped rather than kept.
            reply.content.clear();
            if !self.asked_again && !wrap_up {
                self.asked_again = true;
                return Ok(Some(nudge(SPEAK_UP, "No answer came; asking again.")));
            }
            if !reply.reasoning.trim().is_empty() {
                reply.content = std::mem::take(&mut reply.reasoning);
            } else if !wrap_up {
                return Err("The model returned no text. Check model compatibility.".into());
            }
            return Ok(None);
        }
        if self.rounds > 0 && !self.pushed_on && !wrap_up && announces(&reply.content) {
            self.pushed_on = true;
            return Ok(Some(nudge(
                CONTINUE_NOW,
                "It stopped at its next step; asking it to go on.",
            )));
        }
        if self.verify && self.edited && !self.asked_to_verify && !wrap_up && self.runs_commands() {
            self.asked_to_verify = true;
            return Ok(Some(nudge(
                VERIFY,
                "Files changed with nothing run since; asking it to check them.",
            )));
        }
        Ok(None)
    }

    /// Whether this turn can run a command to check its edits: not in plan
    /// mode, in a project folder, and never from a chat app, whose shared
    /// chats cannot reach this computer at all.
    fn runs_commands(&self) -> bool {
        let connection = &self.connection;
        connection.tools
            && !connection.plan
            && connection.workspace.is_some()
            && connection.remote.is_none()
    }

    /// Queue the calls the model asked for. False when the turn should stop
    /// instead: nothing to run, no round allowed at all, or the wrap-up asked
    /// for at the round cap has come. At the cap the calls are answered as not
    /// run instead, and a notice after them asks for that wrap-up with no tools.
    pub fn begin(&mut self, calls: Vec<ToolCall>) -> bool {
        // With no round allowed nothing was done to sum up, so a call ends
        // the turn there: /btw makes one request.
        if calls.is_empty() || self.connection.wrap_up || self.limit == 0 {
            return false;
        }
        self.order = calls.iter().map(|call| call.id.clone()).collect();
        self.queue = calls.into();
        self.results.clear();
        if self.rounds >= self.limit {
            let why = format!(
                "Not run: the tool-round limit ({}) was reached.",
                self.limit
            );
            while !self.queue.is_empty() {
                self.finish_call(Err(why.clone()));
            }
            self.results.push(Message {
                role: "user".into(),
                content: WRAP_UP.into(),
                notice: true,
                ..Default::default()
            });
            self.connection.wrap_up = true;
            return true;
        }
        self.rounds += 1;
        self.asked_again = false;
        true
    }

    /// What to tell the user when the turn stopped at its round cap.
    pub fn capped(&self) -> Option<String> {
        self.connection.wrap_up.then(|| {
            format!(
                "Stopped after {} rounds; send \"continue\" to keep going.",
                self.limit
            )
        })
    }

    /// What to do next, given what the gate makes of the call in front.
    pub fn next(&self, gate: impl Fn(&ToolCall) -> Outcome) -> Next {
        let Some(call) = self.queue.front() else {
            return if self.results.is_empty() {
                Next::Done
            } else {
                Next::Ask
            };
        };
        match gate(call) {
            Outcome::Run => Next::Run(call.clone()),
            Outcome::Ask => Next::AskUser(call.clone()),
            Outcome::Refuse(why) => Next::Refuse(call.clone(), why),
        }
    }

    /// The calls at the front of the queue that may run at once, at most
    /// `max`, left queued until `finish_id` records each.
    /// `gate` says whether a call must wait for the user, from the whole call.
    pub fn next_batch(&self, gate: impl Fn(&ToolCall) -> bool, max: usize) -> Vec<ToolCall> {
        self.queue
            .iter()
            .take_while(|call| tools::concurrent(&call.name) && !gate(call))
            .take(max)
            .cloned()
            .collect()
    }

    /// Record the outcome of the call at the front of the queue.
    pub fn finish_call(&mut self, outcome: Result<String, String>) {
        let Some(call) = self.queue.pop_front() else {
            return;
        };
        let (content, is_error) = match outcome {
            Ok(out) => (out, false),
            Err(error) => (error, true),
        };
        // Any command tried, even one that failed or was declined, leaves
        // the check to the model; a failed edit changed nothing.
        if call.name == "run_command" {
            self.edited = false;
        } else if !is_error && EDITS.contains(&call.name.as_str()) {
            self.edited = true;
        }
        self.results.push(Message {
            role: "tool".into(),
            content,
            tool_call_id: call.id,
            name: call.name,
            args: call.arguments,
            is_error,
            files: self.files.take(),
            ..Default::default()
        });
    }

    /// Record the outcome of the call with this id, wherever it sits in the queue.
    pub fn finish_id(&mut self, call_id: &str, outcome: Result<String, String>) {
        let Some(at) = self.queue.iter().position(|call| call.id == call_id) else {
            return;
        };
        if let Some(call) = self.queue.remove(at) {
            // At the front, it is recorded exactly as a call run on its own is.
            self.queue.push_front(call);
            self.finish_call(outcome);
        }
    }

    /// The tool results collected for this batch, and at the round cap the
    /// notice after them, ready to append before the next model call.
    pub fn take_results(&mut self) -> Vec<Message> {
        let mut results = std::mem::take(&mut self.results);
        // Calls that ran together finish in whatever order their threads did;
        // put back in the order the model made them, the results read the
        // same however that went.
        // A notice answers no call, and stays after the results it follows.
        results.sort_by_key(|result| {
            self.order
                .iter()
                .position(|id| *id == result.tool_call_id)
                .unwrap_or(usize::MAX)
        });
        results
    }

    /// Record the user's answer to the `exit_plan` call at the front of the
    /// queue; approving turns plan mode off for the rest of the turn, so the
    /// tools that carry the plan out are offered and allowed again.
    pub fn answer_plan(&mut self, approved: bool) {
        let answer = if approved {
            self.connection.plan = false;
            tools::PLAN_APPROVED
        } else {
            tools::KEEP_PLANNING
        };
        self.finish_call(Ok(answer.to_owned()));
    }

    /// What to keep of a stopped turn: the reply streamed so far, marked
    /// interrupted, then the batch's results with every call still queued
    /// answered as never started, so the next request answers every call it
    /// carries.
    pub fn stop(&mut self, model: &str) -> Vec<Message> {
        let mut kept = Vec::new();
        if !self.live.content.trim().is_empty() {
            let mut reply = self.take_reply(model);
            // Calls that came with a reply cut short never ran; kept, each
            // would need an answer of its own. What the provider wanted back
            // went with the whole reply, calls and all.
            reply.tool_calls.clear();
            reply.replay = None;
            reply.interrupted = true;
            kept.push(reply);
        }
        while !self.queue.is_empty() {
            self.finish_call(Err(STOPPED_BEFORE_START.into()));
        }
        kept.append(&mut self.results);
        kept
    }
}

/// A notice for the model, with the line that tells the user why.
fn nudge(notice: &str, why: &'static str) -> Nudge {
    Nudge {
        notice: Message {
            role: "user".into(),
            content: notice.into(),
            notice: true,
            ..Default::default()
        },
        why,
    }
}

/// Whether `answer` is short and ends by announcing a step it did not take,
/// as in "Now let me run the tests:". Handing back to the user, as in "I'll
/// wait for your go-ahead" or "Let me know", never counts, however it is
/// worded: pushing on past it would act without them.
fn announces(answer: &str) -> bool {
    let answer = answer.trim();
    if answer.chars().count() > ANNOUNCEMENT_CHARS || answer.ends_with(['?', '!']) {
        return false;
    }
    let body = answer.trim_end_matches(['.', ':', '…']);
    // Split at a stop and a space, so a file name like main.rs stays whole.
    let start = [". ", "! ", "? ", "\n"]
        .iter()
        .filter_map(|stop| body.rfind(stop).map(|at| at + stop.len()))
        .max()
        .unwrap_or(0);
    let last = &body[start..];
    let last = last.trim().to_lowercase().replace('’', "'");
    let after_then = ["now", "next", "first", "then"]
        .iter()
        .find_map(|word| {
            last.strip_prefix(word)
                .filter(|rest| rest.starts_with([',', ' ']))
        })
        .map(|rest| rest.trim_start_matches([',', ' ']));
    let rest = after_then.unwrap_or(&last);
    if ["know", "wait", "stop", "pause"]
        .iter()
        .any(|word| rest.contains(word))
    {
        return false;
    }
    let opens = |with: &[&str]| with.iter().any(|opening| rest.starts_with(opening));
    opens(&["let me ", "i'll now ", "i will now "])
        || (after_then.is_some() && opens(&["i'll ", "i will ", "i'm going to ", "i am going to "]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str) -> ToolCall {
        ToolCall {
            id: format!("{name}-1"),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    fn turn() -> Turn {
        Turn::new(Connection {
            endpoint: "http://localhost:11434/v1".into(),
            model: "test".into(),
            key: "k".into(),
            tools: true,
            workspace: None,
            plan: false,
            remote: None,
            effort: String::new(),
            search: false,
            conversation: 0,
            unattended: false,
            wrap_up: false,
        })
    }

    /// Counts the rounds a turn runs before it refuses another, giving up at
    /// 1,000 so a turn that never stops fails the test instead of hanging it.
    fn rounds_before_stopping(mut turn: Turn) -> usize {
        for round in 0..1_000 {
            turn.begin(vec![call("read_file")]);
            if turn.next(|_| Outcome::Run) != Next::Run(call("read_file")) {
                return round;
            }
            turn.finish_call(Ok("again".into()));
            turn.take_results();
        }
        1_000
    }

    #[test]
    fn streams_a_reply_then_clears_for_the_next_one() {
        let mut turn = turn();
        turn.absorb(&Event::Delta("Hel".into()));
        turn.absorb(&Event::Delta("lo".into()));
        turn.absorb(&Event::Reasoning("thinking".into()));
        let summary = crate::usage::Usage {
            input: 900,
            output: 80,
            ..Default::default()
        };
        turn.absorb(&Event::SummaryUsage(summary));
        turn.absorb(&Event::Usage {
            completion: 12,
            per_second: 4.0,
            prompt: 300,
            cached: 256,
            cache_write: 30,
            reasoning: 5,
            cost: None,
        });
        turn.absorb(&Event::CacheMiss(crate::cache_miss::Reason::Memory));

        let reply = turn.take_reply("some-model");
        assert_eq!(reply.content, "Hello");
        assert_eq!(reply.reasoning, "thinking");
        assert_eq!(reply.model, "some-model");
        assert_eq!(reply.cached_tokens, 256);
        assert_eq!((reply.cache_write_tokens, reply.reasoning_tokens), (30, 5));
        assert_eq!(reply.summary_usage, Some(summary));
        assert_eq!(reply.cache_miss, Some(crate::cache_miss::Reason::Memory));
        assert!(reply.at > 0);
        // No price is known for this model, so no cost is made up.
        assert_eq!(reply.cost, None);
        // The next reply must not inherit any of that.
        assert!(turn.live.content.is_empty());
        let next = turn.take_reply("some-model");
        assert_eq!((next.tokens, next.summary_usage), (0, None));
        assert_eq!(next.cache_miss, None);
    }

    #[test]
    fn a_reply_carries_its_models_window_when_the_facts_know_it() {
        let usage = Event::Usage {
            completion: 12,
            per_second: 4.0,
            prompt: 76_000,
            cached: 0,
            cache_write: 0,
            reasoning: 0,
            cost: None,
        };
        let mut known = turn();
        known.connection.endpoint = "https://fullness.test/v1".into();
        known.connection.model = "claude-sonnet-4-5".into();
        known.absorb(&usage);
        let reply = known.take_reply("claude-sonnet-4-5");
        assert_eq!(reply.window, 200_000);
        assert_eq!(reply.fullness().as_deref(), Some("38% of 200K"));

        let mut unknown = turn();
        unknown.absorb(&usage);
        assert_eq!(unknown.take_reply("test").fullness(), None);
    }

    #[test]
    fn a_retry_is_shown_until_the_next_try_answers() {
        let mut turn = turn();
        let retry = crate::retry::Retry {
            kind: crate::failure::FailureKind::Overloaded,
            attempt: 2,
            of: 6,
            wait: std::time::Duration::from_secs(1),
        };
        turn.absorb(&Event::Retrying(retry));
        assert_eq!(turn.live.retrying, Some(retry));
        turn.absorb(&Event::Reasoning("thinking".into()));
        assert_eq!(turn.live.retrying, None);
    }

    #[test]
    fn runs_a_batch_then_asks_the_model_again() {
        let mut turn = turn();
        assert_eq!(turn.next(|_| Outcome::Run), Next::Done);

        assert!(turn.begin(vec![call("read_file"), call("list_dir")]));
        assert_eq!(turn.next(|_| Outcome::Run), Next::Run(call("read_file")));
        turn.finish_call(Ok("contents".into()));
        assert_eq!(turn.next(|_| Outcome::Run), Next::Run(call("list_dir")));
        turn.finish_call(Err("no such folder".into()));

        // Batch drained: the results go back to the model.
        assert_eq!(turn.next(|_| Outcome::Run), Next::Ask);
        let results = turn.take_results();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].content, "contents");
        assert!(!results[0].is_error);
        assert!(results[1].is_error);
        // Having handed them over, there is nothing left to send.
        assert_eq!(turn.next(|_| Outcome::Run), Next::Done);
    }

    #[test]
    fn stops_at_a_call_the_user_must_answer() {
        let mut turn = turn();
        turn.begin(vec![call("write_file")]);
        let gate = |queued: &ToolCall| {
            if queued.name == "write_file" {
                Outcome::Ask
            } else {
                Outcome::Run
            }
        };
        assert_eq!(turn.next(gate), Next::AskUser(call("write_file")));
        // Still waiting: asking twice must not skip it.
        assert_eq!(turn.next(gate), Next::AskUser(call("write_file")));
        // A denial is an outcome like any other, so the loop moves on.
        turn.finish_call(Err(tools::DECLINED.into()));
        assert_eq!(turn.next(gate), Next::Ask);
    }

    #[test]
    fn a_model_that_keeps_calling_tools_is_eventually_stopped() {
        let mut turn = turn();
        for round in 0..MAX_ROUNDS {
            assert!(turn.begin(vec![call("read_file")]), "round {round}");
            turn.finish_call(Ok("again".into()));
            turn.take_results();
        }
        assert_eq!(turn.capped(), None);
        // At the cap nothing more runs: every call is answered as not run,
        // and a notice after them asks for a wrap-up with no tools.
        assert!(turn.begin(vec![call("read_file"), call("list_dir")]));
        assert_eq!(turn.next(|_| Outcome::Run), Next::Ask);
        let sent = turn.take_results();
        let not_run = "Not run: the tool-round limit (25) was reached.";
        let answered: Vec<(&str, &str)> = sent[..2]
            .iter()
            .map(|m| (m.tool_call_id.as_str(), m.content.as_str()))
            .collect();
        assert_eq!(
            answered,
            [("read_file-1", not_run), ("list_dir-1", not_run)]
        );
        assert!(sent[..2].iter().all(|m| m.role == "tool" && m.is_error));
        assert_eq!((sent[2].role.as_str(), sent[2].notice), ("user", true));
        assert_eq!(sent[2].content, WRAP_UP);
        assert!(turn.connection.wrap_up);
        // The wrap-up asks for calls anyway: they are dropped unrun, and the
        // turn ends saying where it stopped.
        let mut summary = answer(&mut turn, "Read most of it; the tests remain.");
        summary.tool_calls = vec![call("read_file")];
        assert!(matches!(turn.continuation(&mut summary), Ok(None)));
        assert!(summary.tool_calls.is_empty());
        assert!(!turn.begin(summary.tool_calls));
        assert_eq!(
            turn.capped().as_deref(),
            Some("Stopped after 25 rounds; send \"continue\" to keep going.")
        );
        // An empty batch ends the turn rather than looping on nothing.
        assert!(!self::turn().begin(Vec::new()));
    }

    #[test]
    fn a_turn_with_a_higher_limit_goes_further() {
        let turn = Turn::with_limit(turn().connection, 60);
        assert_eq!(rounds_before_stopping(turn), 60);
    }

    #[test]
    fn a_new_turn_still_stops_at_twenty_five() {
        assert_eq!(rounds_before_stopping(turn()), 25);
    }

    #[test]
    fn a_turn_allowed_no_rounds_ends_at_a_call_without_asking_for_a_wrap_up() {
        let mut turn = Turn::with_limit(turn().connection, 0);
        assert!(!turn.begin(vec![call("read_file")]));
        assert!(turn.take_results().is_empty());
        assert!(!turn.connection.wrap_up);
        assert_eq!(turn.capped(), None);
    }

    #[test]
    fn approving_a_plan_ends_plan_mode_and_declining_keeps_it() {
        use crate::gate::{Gate, Grants, Mode, decide};
        use std::collections::HashSet;
        let ctx = tools::ToolContext::default();
        let granted = Grants::from([(0, HashSet::from([tools::EXIT_PLAN.to_owned()]))]);
        let decided = |skip_approvals, human| {
            let gate = Gate {
                ctx: &ctx,
                mode: if skip_approvals {
                    Mode::Bypass
                } else {
                    Mode::Default
                },
                grants: &granted,
                human,
                rules: &[],
                broken: None,
            };
            decide(&call(tools::EXIT_PLAN), &gate)
        };
        // No standing permission approves a plan; with nobody there it runs,
        // which refuses it.
        assert!(matches!(decided(true, true), Outcome::Ask));
        assert!(matches!(decided(false, false), Outcome::Run));

        let offers = |turn: &Turn, name: &str| {
            tools::specs(&turn.connection)
                .iter()
                .any(|t| t["function"]["name"] == name)
        };
        let none = Grants::new();
        let gate = |queued: &ToolCall| {
            let gate = Gate {
                ctx: &ctx,
                mode: Mode::Default,
                grants: &none,
                human: true,
                rules: &[],
                broken: None,
            };
            decide(queued, &gate)
        };
        let mut turn = turn();
        turn.connection.plan = true;
        assert!(offers(&turn, tools::EXIT_PLAN) && !offers(&turn, "write_file"));

        turn.begin(vec![call(tools::EXIT_PLAN)]);
        assert_eq!(turn.next(gate), Next::AskUser(call(tools::EXIT_PLAN)));
        turn.answer_plan(false);
        assert!(turn.connection.plan);
        let kept = turn.take_results();
        assert_eq!(
            kept[0].content,
            "Keep planning: the user wants changes. Ask what to change, or refine the plan."
        );
        assert!(!kept[0].is_error);

        turn.begin(vec![call(tools::EXIT_PLAN)]);
        turn.answer_plan(true);
        assert!(!turn.connection.plan);
        assert_eq!(
            turn.take_results()[0].content,
            "The user approved this plan. Plan mode is off; carry it out now."
        );
        // The rest of the turn can act on the plan, and has no plan to hand over.
        assert!(offers(&turn, "write_file") && !offers(&turn, tools::EXIT_PLAN));
    }

    #[test]
    fn a_reply_cut_off_at_the_limit_goes_on_three_times_then_stops() {
        let mut turn = turn();
        // A reply that ended on its own asks for nothing more.
        let mut done = answer(&mut turn, "Done.");
        assert!(matches!(turn.continuation(&mut done), Ok(None)));
        for time in 0..MAX_CONTINUATIONS {
            turn.absorb(&Event::Delta("part".into()));
            turn.absorb(&Event::CutOff { in_call: false });
            let mut reply = turn.take_reply("some-model");
            let nudge = turn
                .continuation(&mut reply)
                .unwrap()
                .expect("a continuation");
            // The fragment is kept, not thrown away.
            assert_eq!(reply.content, "part");
            assert!(nudge.notice.notice, "time {time}");
            assert_eq!(nudge.notice.role, "user");
            assert_eq!(nudge.notice.content, CONTINUE);
        }
        turn.absorb(&Event::CutOff { in_call: false });
        let mut reply = turn.take_reply("some-model");
        assert!(
            turn.continuation(&mut reply).is_err(),
            "went on past the cap"
        );
    }

    #[test]
    fn a_call_cut_off_at_the_limit_is_asked_for_again() {
        let mut turn = turn();
        turn.absorb(&Event::CutOff { in_call: true });
        let mut reply = turn.take_reply("some-model");
        assert!(reply.tool_calls.is_empty());
        let nudge = turn
            .continuation(&mut reply)
            .unwrap()
            .expect("a continuation");
        assert!(nudge.notice.notice);
        assert_eq!(nudge.notice.content, CALL_AGAIN);
        // Taken once: the next reply starts clean.
        let mut done = answer(&mut turn, "Done.");
        assert!(matches!(turn.continuation(&mut done), Ok(None)));
    }

    #[test]
    fn a_stop_keeps_the_results_of_the_calls_that_finished() {
        let mut turn = turn();
        turn.begin(vec![call("read_file"), call("list_dir")]);
        turn.finish_call(Ok("contents".into()));
        let kept = turn.stop("test");
        assert_eq!(kept[0].tool_call_id, "read_file-1");
        assert_eq!(kept[0].content, "contents");
        assert!(!kept[0].is_error);
    }

    #[test]
    fn the_call_running_at_a_stop_is_told_to_check_before_retrying() {
        let mut turn = turn();
        turn.begin(vec![call("run_command"), call("list_dir")]);
        // The front end that stopped it answers it as it gives up on it.
        turn.finish_call(Err(STOPPED_WHILE_RUNNING.into()));
        let kept = turn.stop("test");
        assert_eq!(kept[0].tool_call_id, "run_command-1");
        assert_eq!(
            kept[0].content,
            "Stopped while it ran; check before retrying."
        );
        assert!(kept[0].is_error);
        assert_eq!(kept[1].content, STOPPED_BEFORE_START);
    }

    #[test]
    fn calls_the_stop_came_before_are_answered_as_not_run() {
        let mut turn = turn();
        turn.begin(vec![call("write_file"), call("list_dir")]);
        // Stopped at the first call's card: neither ran.
        let kept = turn.stop("test");
        let answers: Vec<&str> = kept.iter().map(|m| m.content.as_str()).collect();
        let not_run = "Not run: stopped before it started.";
        assert_eq!(answers, [not_run, not_run]);
        assert!(kept.iter().all(|m| m.is_error));
    }

    #[test]
    fn a_stop_mid_reply_keeps_what_streamed_marked_interrupted() {
        let mut turn = turn();
        turn.absorb(&Event::Delta("Half an ans".into()));
        turn.absorb(&Event::ToolCalls(vec![call("read_file")]));
        let kept = turn.stop("some-model");
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].role, "assistant");
        assert_eq!(kept[0].content, "Half an ans");
        assert_eq!(kept[0].model, "some-model");
        assert!(kept[0].interrupted);
        // The call that came with it never ran, so there is none to answer.
        assert!(kept[0].tool_calls.is_empty());
        // Nothing streamed yet: nothing to keep.
        assert!(
            Turn::new(turn.connection.clone())
                .stop("some-model")
                .is_empty()
        );
    }

    #[test]
    fn after_a_stop_every_call_the_model_made_has_an_answer() {
        let mut turn = turn();
        let calls = vec![call("read_file"), call("run_command"), call("list_dir")];
        turn.begin(calls.clone());
        turn.finish_call(Ok("contents".into()));
        turn.finish_call(Err(STOPPED_WHILE_RUNNING.into()));
        let kept = turn.stop("test");
        let answered: Vec<&str> = kept.iter().map(|m| m.tool_call_id.as_str()).collect();
        let made: Vec<&str> = calls.iter().map(|c| c.id.as_str()).collect();
        assert_eq!(answered, made);
        assert!(kept.iter().all(|m| m.role == "tool"));
        // Nothing is left to run or send.
        assert_eq!(turn.next(|_| Outcome::Run), Next::Done);
    }

    #[test]
    fn reads_in_one_batch_run_together_and_come_back_in_order() {
        let mut turn = turn();
        let never = |_: &ToolCall| false;
        let names = [
            "read_file",
            "list_dir",
            "search",
            "web_fetch",
            "list_sessions",
            "search_sessions",
            "read_job",
        ];
        let reads = names.map(call);
        turn.begin([reads.to_vec(), vec![call("write_file")]].concat());
        assert_eq!(turn.next_batch(never, MAX_BATCH), reads);

        // They finish in whatever order their threads do.
        for read in reads.iter().rev() {
            turn.finish_id(&read.id, Ok(read.name.clone()));
        }
        // The write was never part of it, and now runs on its own.
        assert!(turn.next_batch(never, MAX_BATCH).is_empty());
        assert_eq!(turn.next(|_| Outcome::Run), Next::Run(call("write_file")));
        turn.finish_call(Ok("write_file".into()));
        assert_eq!(turn.next(|_| Outcome::Run), Next::Ask);
        let order: Vec<String> = turn.take_results().into_iter().map(|r| r.content).collect();
        let mut asked = names.to_vec();
        asked.push("write_file");
        assert_eq!(order, asked);

        // A reply of a dozen reads runs eight at a time.
        turn.begin(vec![call("read_file"); 12]);
        assert_eq!(turn.next_batch(never, MAX_BATCH).len(), 8);
    }

    #[test]
    fn a_call_asked_about_for_its_arguments_ends_the_batch() {
        // A read joins a batch by its tool, but a read of a file of keys is
        // asked about, and the gate sees the whole call to know that.
        let mut turn = turn();
        let keys = ToolCall {
            id: "keys".into(),
            name: "read_file".into(),
            arguments: r#"{"path": ".env"}"#.into(),
        };
        turn.begin(vec![call("read_file"), keys, call("list_dir")]);
        let asks = |queued: &ToolCall| queued.arguments.contains(".env");
        assert_eq!(turn.next_batch(asks, MAX_BATCH), [call("read_file")]);
    }

    #[test]
    fn a_call_that_needs_asking_ends_the_batch() {
        let mut turn = turn();
        turn.begin(vec![call("read_file"), call("web_fetch"), call("list_dir")]);
        let asks = |queued: &ToolCall| queued.name == "web_fetch";
        assert_eq!(turn.next_batch(asks, MAX_BATCH), [call("read_file")]);
        turn.finish_id("read_file-1", Ok("contents".into()));

        // Nothing runs past it while it waits for an answer.
        assert!(turn.next_batch(asks, MAX_BATCH).is_empty());
        assert_eq!(
            turn.next(|queued: &ToolCall| if asks(queued) {
                Outcome::Ask
            } else {
                Outcome::Run
            }),
            Next::AskUser(call("web_fetch"))
        );
        // Once allowed, the same fetch joins the read after it.
        let allowed = |_: &ToolCall| false;
        assert_eq!(
            turn.next_batch(allowed, MAX_BATCH),
            [call("web_fetch"), call("list_dir")]
        );
    }

    #[test]
    fn browse_and_memory_are_never_batched() {
        let never = |_: &ToolCall| false;
        for alone in [
            "browse",
            "use_page",
            "memory",
            tools::TODO,
            tools::ASK_USER,
            "wait",
        ] {
            // A name nothing answers to would pass without proving anything.
            assert!(tools::find(alone).is_some(), "{alone} is not a tool");
            let mut turn = turn();
            turn.begin(vec![call("read_file"), call(alone), call("list_dir")]);
            assert_eq!(
                turn.next_batch(never, MAX_BATCH),
                [call("read_file")],
                "{alone}"
            );
            turn.finish_id("read_file-1", Ok("contents".into()));
            assert!(turn.next_batch(never, MAX_BATCH).is_empty(), "{alone}");
            assert_eq!(turn.next(|_| Outcome::Run), Next::Run(call(alone)));
        }
    }

    /// The reply the model just finished, streamed as `text`.
    fn answer(turn: &mut Turn, text: &str) -> Message {
        turn.absorb(&Event::Delta(text.into()));
        turn.take_reply("some-model")
    }

    #[test]
    fn an_empty_reply_is_asked_for_again_once_then_its_thinking_is_the_answer() {
        let mut turn = turn();
        turn.absorb(&Event::Reasoning("The answer is 4.".into()));
        let mut reply = turn.take_reply("some-model");
        let nudge = turn.continuation(&mut reply).unwrap().expect("a nudge");
        assert_eq!(nudge.notice.content, SPEAK_UP);
        assert!(nudge.notice.notice);
        // Nothing is left in it to keep.
        assert!(reply.content.is_empty() && reply.tool_calls.is_empty());
        turn.absorb(&Event::Reasoning("Still just 4.".into()));
        let mut again = turn.take_reply("some-model");
        assert!(matches!(turn.continuation(&mut again), Ok(None)));
        assert_eq!(again.content, "Still just 4.");
        assert!(again.reasoning.is_empty());
    }

    #[test]
    fn two_empty_replies_with_no_thinking_end_the_turn_saying_so() {
        let mut turn = turn();
        let mut blank = answer(&mut turn, "  \n");
        assert!(turn.continuation(&mut blank).unwrap().is_some());
        // Some routes refuse an assistant message of only whitespace.
        assert!(blank.content.is_empty());
        let mut blank = turn.take_reply("some-model");
        assert_eq!(
            turn.continuation(&mut blank).err().as_deref(),
            Some("The model returned no text. Check model compatibility.")
        );
    }

    #[test]
    fn a_tool_round_earns_an_empty_reply_another_nudge() {
        let mut turn = turn();
        let mut blank = turn.take_reply("some-model");
        assert!(turn.continuation(&mut blank).unwrap().is_some());
        turn.begin(vec![call("read_file")]);
        turn.finish_call(Ok("contents".into()));
        turn.take_results();
        let mut blank = turn.take_reply("some-model");
        assert!(turn.continuation(&mut blank).unwrap().is_some());
    }

    #[test]
    fn an_answer_that_stops_at_its_next_step_is_pushed_on_once() {
        let mut turn = turn();
        // Before any tool has run there is nothing to go on with.
        let mut plan = answer(&mut turn, "Let me read the file.");
        assert!(matches!(turn.continuation(&mut plan), Ok(None)));
        turn.begin(vec![call("read_file")]);
        turn.finish_call(Ok("contents".into()));
        turn.take_results();
        let mut plan = answer(&mut turn, "Found it. Now let me run the tests:");
        let nudge = turn.continuation(&mut plan).unwrap().expect("a nudge");
        assert_eq!(nudge.notice.content, "Continue now.");
        // The announcement itself is kept.
        assert_eq!(plan.content, "Found it. Now let me run the tests:");
        let mut plan = answer(&mut turn, "Now let me run the tests:");
        assert!(matches!(turn.continuation(&mut plan), Ok(None)), "twice");
    }

    #[test]
    fn announcements_are_told_from_answers_and_hand_backs() {
        for said in [
            "Let me check the config file.",
            "The build passed. Now let me run the tests:",
            "Next, I'll update the README\u{2026}",
            "I\u{2019}ll now edit main.rs.",
            "Now I'm going to rerun the build",
        ] {
            assert!(announces(said), "{said}");
        }
        for said in [
            "The tests pass.",
            "Let me know if you want the other file changed too.",
            "I'll wait for your go-ahead before deleting it.",
            "Let me stop here until you confirm.",
            "I'm going to leave the config as it is.",
            "Shall I go on and run the tests?",
            "Now I see: the bug was the off-by-one in the loop.",
            &format!("{} Let me run the tests.", "Done. ".repeat(80)),
        ] {
            assert!(!announces(said), "{said}");
        }
    }

    /// A turn in a project folder that checks its edits, having run each of
    /// `calls` in a round of its own and succeeded.
    fn verifying_after(calls: &[&str]) -> Turn {
        let mut turn = turn().verifying(true);
        turn.connection.workspace = Some("project".into());
        for name in calls {
            turn.begin(vec![call(name)]);
            turn.finish_call(Ok("done".into()));
            turn.take_results();
        }
        turn
    }

    #[test]
    fn an_edit_with_no_command_after_it_is_sent_back_to_check_once() {
        let mut turn = verifying_after(&["read_file", "edit_file"]);
        let mut done = answer(&mut turn, "Fixed the off-by-one.");
        let nudge = turn.continuation(&mut done).unwrap().expect("a nudge");
        assert_eq!(nudge.notice.content, VERIFY);
        assert!(nudge.notice.notice && nudge.notice.role == "user");
        assert_eq!(done.content, "Fixed the off-by-one.");
        // It answers again without running anything: once is enough.
        let mut again = answer(&mut turn, "It is fixed.");
        assert!(matches!(turn.continuation(&mut again), Ok(None)));
        // Nor does another edit earn a second one.
        turn.begin(vec![call("write_file")]);
        turn.finish_call(Ok("written".into()));
        turn.take_results();
        let mut again = answer(&mut turn, "Also wrote the notes.");
        assert!(matches!(turn.continuation(&mut again), Ok(None)));
    }

    #[test]
    fn an_edit_checked_by_a_command_is_not_sent_back() {
        let mut turn = verifying_after(&["write_file", "run_command"]);
        let mut done = answer(&mut turn, "Written, and the tests pass.");
        assert!(matches!(turn.continuation(&mut done), Ok(None)));
        let mut turn = verifying_after(&["run_command", "edit_file"]);
        let mut done = answer(&mut turn, "Tests ran, then I tidied a comment.");
        assert!(
            turn.continuation(&mut done).unwrap().is_some(),
            "the edit came last"
        );
    }

    #[test]
    fn a_failed_edit_or_a_turn_without_edits_is_not_sent_back() {
        let mut turn = verifying_after(&["read_file"]);
        let mut done = answer(&mut turn, "The file says hello.");
        assert!(matches!(turn.continuation(&mut done), Ok(None)));
        let mut turn = verifying_after(&[]);
        turn.begin(vec![call("edit_file")]);
        turn.finish_call(Err("old_string not found".into()));
        turn.take_results();
        let mut done = answer(&mut turn, "The text was not there.");
        assert!(matches!(turn.continuation(&mut done), Ok(None)));
    }

    #[test]
    fn edits_are_not_sent_back_where_no_command_can_run() {
        let unchecked = |mut turn: Turn| {
            let mut done = answer(&mut turn, "Done.");
            matches!(turn.continuation(&mut done), Ok(None))
        };
        let mut plan = verifying_after(&["edit_file"]);
        plan.connection.plan = true;
        assert!(unchecked(plan), "plan mode");
        let mut no_folder = verifying_after(&["edit_file"]);
        no_folder.connection.workspace = None;
        assert!(unchecked(no_folder), "no project folder");
        let mut chat_app = verifying_after(&["edit_file"]);
        chat_app.connection.remote = Some(crate::chat::Remote::default());
        assert!(unchecked(chat_app), "a chat app");
        let mut shared = verifying_after(&["edit_file"]);
        shared.connection.remote = Some(crate::chat::Remote {
            shared: true,
            ..Default::default()
        });
        assert!(unchecked(shared), "a shared chat");
    }

    #[test]
    fn edits_are_not_sent_back_with_the_setting_off() {
        let mut turn = verifying_after(&["edit_file"]).verifying(false);
        let mut done = answer(&mut turn, "Done.");
        assert!(matches!(turn.continuation(&mut done), Ok(None)));
    }
}
