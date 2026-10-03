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
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};

/// Tool rounds per user message. Enough for a real investigation, small
/// enough that a looping model stops on its own.
pub const MAX_ROUNDS: usize = 25;

/// Times one user message may ask a reply cut off at the output limit to go
/// on. Three more replies make an answer four times the cap; one still
/// running past that is more likely looping than nearly done.
const MAX_CONTINUATIONS: usize = 3;

/// The notice that asks a reply cut off at the output limit to go on.
pub(crate) const CONTINUE: &str =
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

/// The notice when a turn that changed files is about to end with no
/// command since that ran clean and did more than read.
const VERIFY: &str = "You changed files and no build or test has passed since. Before saying \
    the work is done, check it: run the project's build or tests, or say why that cannot be \
    done here.";

/// Tools whose successful calls change files in the project.
const EDITS: [&str; 3] = ["write_file", "edit_file", crate::patch::APPLY_PATCH];

/// Extensions of files written to be read, such as a report or notes, which
/// no build or test checks.
const NOTES: [&str; 2] = ["md", "txt"];

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

/// Times the same call gets the same result before the model is told to
/// change approach: twice can be a check that something held.
const REPEATS_NOTED: usize = 3;

/// Times the same call gets the same result before an unattended turn ends,
/// since nobody is there to notice it going round in circles.
const REPEATS_STOPPED: usize = 5;

/// Tools made to be called again for news, where the same answer means only
/// that nothing has happened yet.
const POLLING: [&str; 4] = ["wait", "read_job", crate::messaging::READ_CHAT, "agents"];

/// Rounds in a row one tool fails, whatever its arguments, before the model
/// is told to change approach: one miss can be a slip, a second is a habit.
const FAILURES_NOTED: usize = 2;

/// Notes a turn gets about a tool that keeps failing: by a third the model
/// has heard it, and more would only crowd its results.
const FAILURE_NOTES: usize = 2;

/// How the answer to a call that never ran opens: one left at the round
/// cap, stopped before it started, or after a hook ended the turn.
const NOT_RUN: &str = "Not run: ";

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
    /// Run this call inside the OS sandbox, which is all the gate allowed.
    Sandboxed(ToolCall),
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
    /// The calls the reply has begun, the newest last, while it streams
    /// them; `calls` has them whole once it ends.
    pub drafts: Vec<crate::chat::Draft>,
    pub replay: Option<Replay>,
    /// The try about to be made again after a failure, while it waits.
    pub retrying: Option<crate::retry::Retry>,
    /// The fallback the reply comes from, when not the session's model.
    pub switched: Option<crate::fallback::Switch>,
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
    /// A file was changed since the last command that checked it.
    edited: bool,
    /// Unchecked edits were sent back this turn.
    asked_to_verify: bool,
    /// How often each call, by tool, arguments and result, has come back
    /// the same this turn.
    repeats: HashMap<(String, u64, u64), usize>,
    /// Each change made this turn, by tool and arguments, so only a new one
    /// starts the counts again.
    changes: HashSet<(String, u64)>,
    /// Each tool's failed rounds in a row this turn, with the round last
    /// counted.
    failures: HashMap<String, (usize, usize)>,
    /// Notes sent this turn about a tool that keeps failing.
    failure_notes: usize,
    /// Nobody is watching, so a call repeated without end stops the turn.
    unattended: bool,
    /// Why an unattended turn stopped going round in circles.
    looped: Option<String>,
    /// The settings turn parallel tool calls off, so no batch holds more
    /// than one.
    one_at_a_time: bool,
}

impl Turn {
    pub fn new(connection: Connection) -> Self {
        Self {
            files: crate::history::Log::new(connection.conversation),
            unattended: connection.unattended,
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
            repeats: HashMap::new(),
            changes: HashSet::new(),
            failures: HashMap::new(),
            failure_notes: 0,
            looped: None,
            one_at_a_time: !crate::store::parallel_tool_calls(),
        }
    }

    /// The same turn, ended by a call it keeps repeating whatever its
    /// connection says, for a run nobody watches.
    pub fn unattended(self) -> Self {
        Self {
            unattended: true,
            ..self
        }
    }

    /// Why an unattended turn must end: it made the same call with the same
    /// result `REPEATS_STOPPED` times.
    pub fn looping(&self) -> Option<&str> {
        self.looped.as_deref()
    }

    /// This turn, sending back once an answer that follows file edits with
    /// no command since that checked them, when `on`.
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
                let (endpoint, model) = self.served();
                let facts = crate::models::facts(endpoint, model.trim());
                self.live.window = facts.context.unwrap_or(0);
                self.live.cache_write_tokens = *cache_write;
                self.live.reasoning_tokens = *reasoning;
                self.live.cost = *cost;
            }
            Event::SummaryUsage(usage) => self.live.summary_usage = Some(*usage),
            Event::CacheMiss(reason) => self.live.cache_miss = Some(*reason),
            Event::ToolCalls(calls) => self.live.calls = calls.clone(),
            Event::Drafting(drafts) => self.live.drafts = drafts.clone(),
            Event::Replay(replay) => self.live.replay = Some(replay.clone()),
            Event::Retrying(retry) => self.live.retrying = Some(*retry),
            Event::Switched(switch) => self.live.switched = Some(switch.clone()),
            Event::Compacting => self.live.compacting = true,
            // The front end keeps it with the session.
            Event::Summarized { .. } => {}
            Event::CutOff { in_call } => self.cut_off = Some(*in_call),
            Event::Finished(_) => {}
        }
    }

    /// Where the reply being streamed comes from, as (endpoint, model): the
    /// session's model, or the fallback that stood in for it.
    fn served(&self) -> (&str, &str) {
        match &self.live.switched {
            Some(switch) => (&switch.endpoint, &switch.model),
            None => (&self.connection.endpoint, &self.connection.model),
        }
    }

    /// The assistant message for the reply just finished, its cost settled,
    /// and a clean slate for the next one. A reply a fallback wrote is
    /// labelled and priced as that model's rather than `model`'s.
    pub fn take_reply(&mut self, model: &str) -> Message {
        let live = std::mem::take(&mut self.live);
        let (endpoint, model) = match &live.switched {
            Some(switch) => (switch.endpoint.as_str(), switch.model.as_str()),
            None => (self.connection.endpoint.as_str(), model),
        };
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
            // As its request told it, so a later one tells only of a change.
            mode: self.connection.told_mode(),
            ..Default::default()
        };
        let usage = crate::usage::Usage::of(&reply).priced(endpoint, model);
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
                "Files changed with no passing check since; asking it to check them.",
            )));
        }
        Ok(None)
    }

    /// Whether this turn can run a command to check its edits: not in plan
    /// mode, in a project folder, and never from a chat app, whose shared
    /// chats cannot reach this computer at all.
    pub(crate) fn runs_commands(&self) -> bool {
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
                // It ends the turn it is in, as a nudge goes on with one.
                folded: true,
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

    /// The call waiting in front of the queue, for a gate that needs it
    /// decided off this thread before `next`.
    pub fn front(&self) -> Option<&ToolCall> {
        self.queue.front()
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
            Outcome::Sandboxed => Next::Sandboxed(call.clone()),
            Outcome::Ask => Next::AskUser(call.clone()),
            Outcome::Refuse(why) => Next::Refuse(call.clone(), why),
        }
    }

    /// The calls at the front of the queue that may run at once, at most
    /// `max` or one when the settings turn parallel calls off, up to the
    /// first one `held` says the gate does not run as it stands; left queued
    /// until `finish_id` records each.
    pub fn next_batch(&self, held: impl Fn(&ToolCall) -> bool, max: usize) -> Vec<ToolCall> {
        // The request asked for one call a reply, but a model may make
        // several anyway; each still runs only once the one before is back.
        let max = if self.one_at_a_time { 1 } else { max };
        self.queue
            .iter()
            .take_while(|call| tools::concurrent(&call.name) && !held(call))
            .take(max)
            .cloned()
            .collect()
    }

    /// Record the outcome of the call at the front of the queue.
    pub fn finish_call(&mut self, outcome: Result<String, String>) {
        let Some(call) = self.queue.pop_front() else {
            return;
        };
        let (mut content, is_error) = match outcome {
            Ok(out) => (out, false),
            Err(error) => (error, true),
        };
        let times = self.repeated(&call, &content, is_error);
        let failures = self.failed(&call.name, &content, is_error);
        if times >= REPEATS_NOTED {
            content.push_str(&format!(
                "\n\n[You have made this exact call {times} times this turn and got the same \
                 result each time. Change your approach instead of repeating it.]"
            ));
        } else if failures == FAILURES_NOTED && self.failure_notes < FAILURE_NOTES {
            self.failure_notes += 1;
            content.push_str(&format!(
                "\n\n[`{}` has failed {failures} times in a row. Read the error and change \
                 approach: re-read what it acts on, use another tool, or take a smaller step, \
                 rather than retrying with small changes.]",
                call.name
            ));
        }
        if times >= REPEATS_STOPPED && self.unattended && self.looped.is_none() {
            self.looped = Some(format!(
                "Stopped going round in circles: it called `{}` {times} times with the same \
                 arguments and got the same result each time.",
                call.name
            ));
        }
        let result = Message {
            role: "tool".into(),
            content,
            tool_call_id: call.id,
            name: call.name,
            args: call.arguments,
            is_error,
            files: self.files.take(),
            ..Default::default()
        };
        self.track_edits(&result);
        self.results.push(result);
    }

    /// Note what a finished call did to the edits waiting to be checked: an
    /// edit that changed more than notes adds to them, and a command that ran
    /// clean and did more than read, or that the user turned down, settles them.
    fn track_edits(&mut self, result: &Message) {
        if result.name == "run_command" {
            // A decline tells the model to ask before trying another way,
            // which a nudge to go and run something would cut across.
            let declined = result.is_error && tools::is_declined(&result.content);
            if declined || checks(result, self.connection.workspace.as_deref()) {
                self.edited = false;
            }
        } else if !result.is_error && EDITS.contains(&result.name.as_str()) && !only_notes(result) {
            self.edited = true;
        }
    }

    /// How many times this turn the call has now come back with this result;
    /// 0 for a tool made to be polled.
    fn repeated(&mut self, call: &ToolCall, content: &str, is_error: bool) -> usize {
        if POLLING.contains(&call.name.as_str()) {
            return 0;
        }
        // Parsed first, so spacing or key order does not make a call new.
        let arguments = serde_json::from_str::<serde_json::Value>(&call.arguments)
            .map(|args| sorted(args).to_string())
            .unwrap_or_else(|_| call.arguments.clone());
        let key = (
            call.name.clone(),
            hash(&arguments),
            hash(&(content, is_error)),
        );
        // A change not made before this turn is progress, so the same answer
        // to a call made before it, such as a `git add` saying nothing, is no
        // sign of a loop.
        if tools::find(&call.name).is_some_and(|tool| tool.mutates())
            && self.changes.insert((key.0.clone(), key.1))
        {
            self.repeats.clear();
        }
        let times = self.repeats.entry(key).or_default();
        *times += 1;
        *times
    }

    /// How many rounds in a row `name` has now failed this turn, or 0 when
    /// this result adds none: a success starts the count again, and a
    /// refusal, a stop, a polled tool's error or a schema answer's is not
    /// counted.
    fn failed(&mut self, name: &str, content: &str, is_error: bool) -> usize {
        if !is_error {
            self.failures.remove(name);
            return 0;
        }
        // A run with a schema corrects a bad answer itself, twice at most and
        // with only what was wrong.
        let corrected = name == crate::driver::STRUCTURED_OUTPUT;
        if POLLING.contains(&name) || corrected || refused_or_stopped(content) {
            return 0;
        }
        let (times, round) = self.failures.entry(name.to_owned()).or_default();
        // Calls made together were all made before any result was seen, so
        // none of them is a retry of another.
        if *round == self.rounds {
            return 0;
        }
        *round = self.rounds;
        *times += 1;
        *times
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

/// A digest of `value`, for counting repeats without keeping whole results.
fn hash(value: &impl Hash) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

/// `value` with every object's keys in order, so it is written out the same
/// whatever order they came in.
fn sorted(value: serde_json::Value) -> serde_json::Value {
    // Done by hand: with serde_json's `preserve_order`, which the window's
    // dependencies turn on, an object keeps the order it was read in.
    match value {
        serde_json::Value::Object(object) => {
            let mut fields: Vec<_> = object.into_iter().collect();
            fields.sort_by(|a, b| a.0.cmp(&b.0));
            fields
                .into_iter()
                .map(|(key, value)| (key, sorted(value)))
                .collect()
        }
        serde_json::Value::Array(items) => items.into_iter().map(sorted).collect(),
        other => other,
    }
}

/// Whether an error is a no from the user, the gate, a hook or Auto mode,
/// or a stop, rather than the call itself failing.
fn refused_or_stopped(error: &str) -> bool {
    // Another way round a refusal would only get past it.
    crate::gate::refused(error)
        || tools::is_declined(error)
        || error == STOPPED_WHILE_RUNNING
        || error.starts_with(NOT_RUN)
}

/// A notice for the model, with the line that tells the user why.
fn nudge(notice: &str, why: &'static str) -> Nudge {
    Nudge {
        notice: Message {
            role: "user".into(),
            content: notice.into(),
            notice: true,
            // Part of the turn it sends back to work: opening one, it could
            // start the window and leave that turn's request to a summary.
            folded: true,
            ..Default::default()
        },
        why,
    }
}

/// Whether a command's result checks the edits before it: it ran to the end
/// without failing, and did more than read, as `ls` or `git diff` only do.
// A failing command is still an `Ok` result, told apart only by the
// `[exit code N]` line it ends with.
fn checks(result: &Message, workspace: Option<&std::path::Path>) -> bool {
    if result.is_error || crate::tool_runs::exit_code(result).is_some() {
        return false;
    }
    let args = tools::parse_args_for(&result.name, &result.args).unwrap_or_default();
    let line = args["command"].as_str().unwrap_or_default();
    let dialect = crate::command::Dialect::here();
    crate::command::assess(line, dialect, workspace).risk != crate::command::Risk::ReadOnly
}

/// Whether every file an edit names is a note, such as a report, rather
/// than anything a build or test would run.
fn only_notes(result: &Message) -> bool {
    let args = tools::parse_args_for(&result.name, &result.args).unwrap_or_default();
    let paths = match result.name.as_str() {
        crate::patch::APPLY_PATCH => crate::patch::paths(&args).unwrap_or_default(),
        _ => args["path"]
            .as_str()
            .map(str::to_owned)
            .into_iter()
            .collect(),
    };
    let note = |path: &String| {
        let extension = std::path::Path::new(path).extension();
        let extension = extension.and_then(|e| e.to_str()).unwrap_or_default();
        NOTES
            .iter()
            .any(|note| extension.eq_ignore_ascii_case(note))
    };
    !paths.is_empty() && paths.iter().all(note)
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
            ..Default::default()
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
    fn a_reply_a_fallback_wrote_is_labelled_as_that_model() {
        let mut turn = turn();
        turn.absorb(&Event::Switched(crate::fallback::Switch {
            model: "small".into(),
            endpoint: "https://fallback.test/v1".into(),
            after: None,
        }));
        turn.absorb(&Event::Delta("Done.".into()));
        assert_eq!(turn.take_reply("test").model, "small");
        // The next request says again where it went, or it went to the session's.
        turn.absorb(&Event::Delta("Done.".into()));
        assert_eq!(turn.take_reply("test").model, "test");
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
    fn calls_being_written_show_in_the_live_reply_and_are_not_kept_with_it() {
        let mut turn = turn();
        let draft = crate::chat::Draft {
            name: "write_file".into(),
            subject: "src/app.rs".into(),
        };
        turn.absorb(&Event::Drafting(vec![draft.clone()]));
        assert_eq!(turn.live.drafts, [draft]);
        let reply = turn.take_reply("some-model");
        assert!(reply.tool_calls.is_empty());
        assert!(turn.live.drafts.is_empty());
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
    fn with_parallel_calls_switched_off_reads_run_one_at_a_time_in_order() {
        // Set here rather than saved, since turns started by tests beside
        // this one read the same settings file.
        let mut turn = Turn {
            one_at_a_time: true,
            ..turn()
        };
        let never = |_: &ToolCall| false;
        let reads = ["read_file", "list_dir", "search"].map(call);
        turn.begin(reads.to_vec());
        for read in &reads {
            assert_eq!(
                turn.next_batch(never, MAX_BATCH),
                std::slice::from_ref(read)
            );
            turn.finish_id(&read.id, Ok(read.name.clone()));
        }
        assert!(turn.next_batch(never, MAX_BATCH).is_empty());
        assert_eq!(turn.next(|_| Outcome::Run), Next::Ask);
        let order: Vec<String> = turn.take_results().into_iter().map(|r| r.content).collect();
        assert_eq!(order, ["read_file", "list_dir", "search"]);
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

    /// Makes a call to `name` with `arguments` in a round of its own, coming
    /// back `outcome`, and gives what the model was sent back.
    fn made(
        turn: &mut Turn,
        name: &str,
        arguments: serde_json::Value,
        outcome: Result<&str, &str>,
    ) -> String {
        let mut call = call(name);
        call.arguments = arguments.to_string();
        turn.begin(vec![call]);
        turn.finish_call(outcome.map(str::to_owned).map_err(str::to_owned));
        turn.take_results().remove(0).content
    }

    /// Whether an answer now is sent back to check the turn's edits.
    fn sent_back_to_verify(turn: &mut Turn) -> bool {
        let mut done = answer(turn, "Done.");
        let nudge = turn.continuation(&mut done).unwrap();
        nudge.is_some_and(|nudge| nudge.notice.content == VERIFY)
    }

    #[test]
    fn a_patch_counts_as_an_edit_to_verify() {
        let mut turn = verifying_after(&[]);
        let patch = "*** Begin Patch\n*** Update File: src/lib.rs\n@@\n-a\n+b\n*** End Patch";
        made(
            &mut turn,
            "apply_patch",
            serde_json::json!({ "patch": patch }),
            Ok("Done"),
        );
        assert!(sent_back_to_verify(&mut turn));
        // One that did not apply changed nothing.
        let mut turn = verifying_after(&[]);
        let failed = Err("A hunk did not apply; no file was changed.");
        made(
            &mut turn,
            "apply_patch",
            serde_json::json!({ "patch": patch }),
            failed,
        );
        assert!(!sent_back_to_verify(&mut turn));
    }

    #[test]
    fn listing_files_does_not_count_as_checking_edits() {
        // A command counts as only reading inside a project with a real place.
        let in_project = |mut turn: Turn| {
            turn.connection.workspace = Some(std::env::temp_dir());
            turn
        };
        let mut turn = in_project(verifying_after(&["edit_file"]));
        made(
            &mut turn,
            "run_command",
            serde_json::json!({ "command": "ls" }),
            Ok("src"),
        );
        assert!(sent_back_to_verify(&mut turn));
        let mut turn = in_project(verifying_after(&["edit_file"]));
        let test = serde_json::json!({ "command": "cargo test" });
        made(&mut turn, "run_command", test, Ok("test result: ok"));
        assert!(!sent_back_to_verify(&mut turn));
        // Under another agent's name for the parameter, too.
        let mut turn = in_project(verifying_after(&["edit_file"]));
        made(
            &mut turn,
            "run_command",
            serde_json::json!({ "cmd": "ls" }),
            Ok("src"),
        );
        assert!(sent_back_to_verify(&mut turn));
    }

    #[test]
    fn a_failed_test_run_does_not_count_as_checking_edits() {
        let test = serde_json::json!({ "command": "cargo test" });
        let mut turn = verifying_after(&["edit_file"]);
        let failed = Ok("test result: FAILED\n[exit code 101]");
        made(&mut turn, "run_command", test.clone(), failed);
        assert!(sent_back_to_verify(&mut turn));
        let mut turn = verifying_after(&["edit_file"]);
        made(&mut turn, "run_command", test, Err("Timed out after 120s."));
        assert!(sent_back_to_verify(&mut turn));
    }

    #[test]
    fn a_declined_command_leaves_the_check_to_the_user() {
        let mut turn = verifying_after(&["edit_file"]);
        let test = serde_json::json!({ "command": "cargo test" });
        made(&mut turn, "run_command", test, Err(tools::DECLINED));
        assert!(!sent_back_to_verify(&mut turn));
    }

    #[test]
    fn writing_a_report_alone_is_not_sent_back_to_verify() {
        let mut turn = verifying_after(&[]);
        let report = serde_json::json!({ "path": "out/REPORT.md", "content": "# Done" });
        made(&mut turn, "write_file", report, Ok("Wrote out/REPORT.md"));
        let notes = serde_json::json!({ "file_path": "notes.txt", "old": "a", "new": "b" });
        made(&mut turn, "edit_file", notes, Ok("Edited notes.txt"));
        let patch = "*** Begin Patch\n*** Add File: docs/plan.md\n+plan\n*** End Patch";
        made(
            &mut turn,
            "apply_patch",
            serde_json::json!({ "patch": patch }),
            Ok("Done"),
        );
        assert!(!sent_back_to_verify(&mut turn));
        // Code changed alongside a report still is.
        let code = serde_json::json!({ "path": "src/main.rs", "content": "fn main() {}" });
        made(&mut turn, "write_file", code, Ok("Wrote src/main.rs"));
        assert!(sent_back_to_verify(&mut turn));
    }

    #[test]
    fn a_nudge_does_not_open_a_turn() {
        // One turn that checks its edits earns every nudge there is.
        let mut turn = verifying_after(&["edit_file"]);
        let mut nudges = Vec::new();
        for in_call in [false, true] {
            turn.absorb(&Event::CutOff { in_call });
            let mut cut_off = turn.take_reply("some-model");
            nudges.extend(turn.continuation(&mut cut_off).unwrap());
        }
        let mut blank = turn.take_reply("some-model");
        nudges.extend(turn.continuation(&mut blank).unwrap());
        let mut announced = answer(&mut turn, "Now let me run the tests:");
        nudges.extend(turn.continuation(&mut announced).unwrap());
        let mut done = answer(&mut turn, "Fixed the off-by-one.");
        nudges.extend(turn.continuation(&mut done).unwrap());
        let mut notices: Vec<Message> = nudges.into_iter().map(|nudge| nudge.notice).collect();
        // And one at its round cap asks for the wrap-up.
        let mut capped = Turn::with_limit(self::turn().connection, 1);
        capped.begin(vec![call("read_file")]);
        capped.finish_call(Ok("contents".into()));
        capped.take_results();
        assert!(capped.begin(vec![call("read_file")]));
        notices.extend(capped.take_results().pop());
        let sent: Vec<&str> = notices.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(
            sent,
            [
                CONTINUE,
                CALL_AGAIN,
                SPEAK_UP,
                CONTINUE_NOW,
                VERIFY,
                WRAP_UP
            ]
        );

        // The request, then far more work than the window holds, with the
        // notices sent along the way.
        let round = |i: usize| {
            [
                Message {
                    role: "assistant".into(),
                    tool_calls: vec![ToolCall {
                        id: format!("call {i}"),
                        ..call("read_file")
                    }],
                    ..Default::default()
                },
                Message {
                    role: "tool".into(),
                    tool_call_id: format!("call {i}"),
                    content: "x".repeat(8_000),
                    ..Default::default()
                },
            ]
        };
        let said = |role: &str, text: &str| Message {
            role: role.into(),
            content: text.into(),
            ..Default::default()
        };
        let mut session = vec![
            said("user", "an earlier question"),
            said("assistant", "an earlier answer"),
            said("user", "the turn's request"),
        ];
        for (i, notice) in notices.iter().enumerate() {
            session.extend((2 * i..2 * i + 2).flat_map(round));
            session.push(notice.clone());
        }
        assert_eq!(crate::chat::window_start_within(8_000, &session), 2);
        // Each opening a turn of its own, they would move the window past it.
        for message in &mut session {
            message.folded = false;
        }
        let start = crate::chat::window_start_within(8_000, &session);
        assert!(start > 2 && session[start].notice, "start {start}");
    }

    #[test]
    fn a_changed_argument_or_result_starts_the_count_again() {
        let mut turn = turn();
        let mut other = call("read_file");
        other.arguments = r#"{"path": "b.rs"}"#.into();
        repeat(&mut turn, &call("read_file"), "same", 2);
        assert_eq!(repeat(&mut turn, &other, "same", 2), ["same", "same"]);
        assert_eq!(
            repeat(&mut turn, &call("read_file"), "changed", 1),
            ["changed"]
        );
        // Only spacing differs, so it is the same call and its third time.
        other.arguments = r#"{ "path":"b.rs" }"#.into();
        assert!(repeat(&mut turn, &other, "same", 1)[0].contains("3 times"));
    }

    #[test]
    fn a_new_change_between_repeats_starts_the_count_again() {
        let mut turn = turn().unattended();
        let mut stage = call("run_command");
        stage.arguments = r#"{"command": "git add -A"}"#.into();
        let mut edit = call("edit_file");
        for step in 0..6 {
            edit.arguments = format!(r#"{{"path": "a.rs", "old": "{step}", "new": "x"}}"#);
            repeat(&mut turn, &edit, "Edited a.rs.", 1);
            // `git add` says nothing when it works, so it answers the same every time.
            assert_eq!(repeat(&mut turn, &stage, "", 1), [""]);
        }
        assert_eq!(turn.looping(), None);
        // Making the same change again is no news, so it is still counted.
        for _ in 0..4 {
            repeat(&mut turn, &edit, "Edited a.rs.", 1);
            repeat(&mut turn, &stage, "", 1);
        }
        assert!(turn.looping().is_some());
    }

    #[test]
    fn an_unattended_turn_stops_at_the_fifth_repeat() {
        let mut turn = turn().unattended();
        repeat(&mut turn, &call("read_file"), "same", 4);
        assert_eq!(turn.looping(), None);
        repeat(&mut turn, &call("read_file"), "same", 1);
        assert_eq!(
            turn.looping(),
            Some(
                "Stopped going round in circles: it called `read_file` 5 times with the same \
                 arguments and got the same result each time."
            )
        );
        // A schedule's connection says so itself.
        let mut connection = self::turn().connection;
        connection.unattended = true;
        let mut scheduled = Turn::new(connection);
        repeat(&mut scheduled, &call("read_file"), "same", 5);
        assert!(scheduled.looping().is_some());
    }

    #[test]
    fn arguments_in_another_key_order_are_the_same_call() {
        let mut turn = turn();
        let mut read = call("read_file");
        read.arguments = r#"{"path": "a.rs", "range": {"from": 1, "to": 9}}"#.into();
        repeat(&mut turn, &read, "same", 2);
        read.arguments = r#"{"range": {"to": 9, "from": 1}, "path": "a.rs"}"#.into();
        assert!(repeat(&mut turn, &read, "same", 1)[0].contains("3 times"));
    }

    #[test]
    fn polling_tools_are_never_counted_as_repeats() {
        for polled in POLLING {
            assert!(tools::find(polled).is_some(), "{polled} is not a tool");
            let mut turn = turn().unattended();
            let sent = repeat(&mut turn, &call(polled), "still running", 8);
            assert!(sent.iter().all(|s| s == "still running"), "{polled}");
            assert_eq!(turn.looping(), None, "{polled}");
        }
    }

    /// Makes `call` in a round of its own, `times` times, each coming back
    /// `result`, and gives what the model was sent back each time.
    fn repeat(turn: &mut Turn, call: &ToolCall, result: &str, times: usize) -> Vec<String> {
        (0..times)
            .map(|_| {
                turn.begin(vec![call.clone()]);
                turn.finish_call(Ok(result.into()));
                turn.take_results().remove(0).content
            })
            .collect()
    }

    #[test]
    fn the_third_identical_result_tells_the_model_to_change_approach() {
        let mut turn = turn();
        let sent = repeat(&mut turn, &call("read_file"), "same", 4);
        assert_eq!(sent[..2], ["same", "same"]);
        assert_eq!(
            sent[2],
            "same\n\n[You have made this exact call 3 times this turn and got the same \
             result each time. Change your approach instead of repeating it.]"
        );
        assert!(sent[3].contains("made this exact call 4 times"));
        // Someone is watching, so it is told but never stopped.
        repeat(&mut turn, &call("read_file"), "same", 4);
        assert_eq!(turn.looping(), None);
        // The counts are the turn's own: the next starts from nothing.
        assert_eq!(
            repeat(&mut self::turn(), &call("read_file"), "same", 1),
            ["same"]
        );
    }

    /// What `edit_file` says when its `old` is not in the file.
    const MISSED: &str =
        "`old` was not found in the file. Read it again and copy the text exactly.";

    /// An edit of a.rs replacing `old`.
    fn edit(old: &str) -> serde_json::Value {
        serde_json::json!({ "path": "a.rs", "old": old, "new": "x" })
    }

    #[test]
    fn two_edit_misses_with_different_old_get_the_note_on_the_second() {
        let mut turn = turn();
        assert_eq!(
            made(&mut turn, "edit_file", edit("fn a()"), Err(MISSED)),
            MISSED
        );
        assert_eq!(
            made(&mut turn, "edit_file", edit("fn a( )"), Err(MISSED)),
            format!(
                "{MISSED}\n\n[`edit_file` has failed 2 times in a row. Read the error and \
                 change approach: re-read what it acts on, use another tool, or take a \
                 smaller step, rather than retrying with small changes.]"
            )
        );
        // Once a streak: the third miss goes back as it came.
        assert_eq!(
            made(&mut turn, "edit_file", edit("fn a(  )"), Err(MISSED)),
            MISSED
        );
    }

    #[test]
    fn a_success_between_misses_starts_the_count_again() {
        let mut turn = turn();
        made(&mut turn, "edit_file", edit("fn a()"), Err(MISSED));
        made(&mut turn, "edit_file", edit("fn b()"), Ok("Edited a.rs."));
        assert_eq!(
            made(&mut turn, "edit_file", edit("fn c()"), Err(MISSED)),
            MISSED
        );
    }

    #[test]
    fn a_command_exiting_one_twice_gets_no_note() {
        let mut turn = turn();
        let failed = "test result: FAILED\n[exit code 1]";
        for line in ["cargo test", "cargo test -- --nocapture"] {
            let test = serde_json::json!({ "command": line });
            assert_eq!(made(&mut turn, "run_command", test, Ok(failed)), failed);
        }
    }

    #[test]
    fn declines_refusals_and_stops_get_no_note() {
        let refusals = [
            tools::DECLINED.to_owned(),
            tools::declined("not that file"),
            "This was not approved, so it did not run. Too broad.".to_owned(),
            "Auto mode did not allow this: it deletes files. Do not retry it.".to_owned(),
            "A PreToolUse hook blocked this call: edits wait for review.".to_owned(),
            "`edit_file` is not available in plan mode: it changes things.".to_owned(),
            "Step 2 of the batch (click Pay) would be refused on its own, so none of it \
             ran: Refused: a permission rule denies `use_page`."
                .to_owned(),
            STOPPED_WHILE_RUNNING.to_owned(),
            STOPPED_BEFORE_START.to_owned(),
            "Not run: a hook ended the turn.".to_owned(),
        ];
        for refusal in refusals {
            let mut turn = turn();
            for old in ["fn a()", "fn b()", "fn c()"] {
                let sent = made(&mut turn, "edit_file", edit(old), Err(&refusal));
                assert_eq!(sent, refusal);
            }
        }
    }

    #[test]
    fn two_refusals_from_the_gate_get_no_note() {
        let ctx = tools::ToolContext::default();
        let none = crate::gate::Grants::new();
        // Nobody is here to give a command the yes it needs, as in `-p`.
        let gate = crate::gate::Gate {
            ctx: &ctx,
            mode: crate::gate::Mode::Default,
            grants: &none,
            rules: &[],
            broken: None,
            human: false,
        };
        let mut turn = turn();
        for line in ["cargo build", "cargo build --release"] {
            let mut build = call("run_command");
            build.arguments = serde_json::json!({ "command": line }).to_string();
            turn.begin(vec![build]);
            let Next::Refuse(_, why) = turn.next(|queued| crate::gate::decide(queued, &gate))
            else {
                panic!("`{line}` was not refused with nobody to approve it");
            };
            turn.finish_call(Err(why.clone()));
            assert_eq!(turn.take_results()[0].content, why);
        }
    }

    #[test]
    fn misses_made_together_count_as_one() {
        let mut turn = turn();
        let reads = ["a.rs", "b.rs", "c.rs"].map(|path| ToolCall {
            id: path.into(),
            arguments: serde_json::json!({ "path": path }).to_string(),
            ..call("read_file")
        });
        turn.begin(reads.into());
        for _ in 0..3 {
            turn.finish_call(Err("No such file.".into()));
        }
        let sent = turn.take_results();
        assert!(sent.iter().all(|result| result.content == "No such file."));
        // A miss after the model has seen them is the second in a row.
        let read = serde_json::json!({ "path": "d.rs" });
        let sent = made(&mut turn, "read_file", read, Err("No such file."));
        assert!(
            sent.contains("`read_file` has failed 2 times in a row"),
            "{sent}"
        );
    }

    #[test]
    fn a_third_streak_in_one_turn_gets_no_note() {
        let mut turn = turn();
        let mut notes = 0;
        for streak in 0..3 {
            for old in ["fn a()", "fn b()"] {
                let old = format!("{old} {streak}");
                let sent = made(&mut turn, "edit_file", edit(&old), Err(MISSED));
                notes += usize::from(sent != MISSED);
            }
            let fixed = edit(&format!("fn c() {streak}"));
            made(&mut turn, "edit_file", fixed, Ok("Edited a.rs."));
        }
        assert_eq!(notes, 2);
    }

    #[test]
    fn the_repeat_note_is_the_only_note_on_its_result() {
        let mut turn = turn();
        let (missing, gone) = (serde_json::json!({ "path": "a.rs" }), Err("No such file."));
        made(&mut turn, "read_file", missing.clone(), gone);
        let other = serde_json::json!({ "path": "b.rs" });
        made(&mut turn, "read_file", other, Ok("fn b() {}"));
        made(&mut turn, "read_file", missing.clone(), gone);
        let sent = made(&mut turn, "read_file", missing, gone);
        assert!(sent.contains("made this exact call 3 times"), "{sent}");
        assert!(!sent.contains("in a row"), "{sent}");
    }
}
