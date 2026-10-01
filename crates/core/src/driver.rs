//! Runs an agent's turn to its end with no window.
//!
//! A helper, a `letronna -p` run or a workflow's agent has nobody watching it
//! stream and nobody to click a card, so this is one more caller of
//! `agent::Turn` beside the window and the terminal: the same loop, with the
//! parts that need a person swapped for what works without one. Each call
//! goes through the whole gate in the agent's own mode, narrowed from its
//! session's: the floor, the user's rules and what was allowed for the run,
//! and in Auto mode the judge, on the agent's own thread and connection, so
//! judges run side by side and off every screen. Only a call that still
//! needs a yes goes to `approvals`, where the user answers it, with why the
//! gate could not; a question is refused with advice, since nobody is there
//! to answer it; the checklist is kept on the agent's transcript; the
//! browser is used unseen, with no panel to open; and messages sent
//! meanwhile are read from the inbox after each batch of results and before
//! the run counts as done.
//!
//! A frontend that shows the run as it goes, as an editor does over ACP,
//! gives it a `Watch`, which hears each step as it is made. A run that is a
//! session's own turn, as an editor's is, gives it the session's whole gate
//! as `Decide`, which then settles each call in place of the agent's own.
//!
//! A run given a schema answers through one more tool, `structured_output`,
//! offered after the rest so the tools before it keep their cached bytes. Its
//! call is checked with `structured` rather than run, and an answer that fits
//! ends the run with the answer on the report. One that does not gets back its
//! problems and nothing more, twice at most, since past that a model starts
//! dropping fields it had right while fixing the rest; an agent that ends in
//! prose is reminded once. Past either, the run fails with no answer and says
//! why.
//!
//! Nothing here polls. Each wait -- for the model, for a tool, for an
//! approval -- is a blocking receive that a stop closes, so a stop is heard at
//! once, and the stop and the budget are checked again before every model
//! call. A run writes only to its own transcript, never into `sessions/`, so
//! every file still has one writer.

use crate::agent::{MAX_BATCH, Next, Turn};
use crate::approvals;
use crate::budget::Budget;
use crate::cancel::Cancel;
use crate::chat::{self, Connection, Event, Extra, Message, ToolCall};
use crate::gate::{self, Gate, Grants, Layer};
use crate::inbox::{self, Notice, When};
use crate::judge;
use crate::limits::{self, Permit};
use crate::permissions::{self, Rule};
use crate::store::{Todo, Transcript};
use crate::structured;
use crate::tasks::{self, Key};
use crate::tools::{self, Caller, ToolContext};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;

/// Names an agent while it runs: its mailbox, and the conversation its
/// browser and jobs are kept under.
pub type AgentId = u64;

/// Tools no helper may call whatever it was allowed, since each reaches past
/// its run: to the user, to what Letronna remembers, or to a later time.
pub(crate) const DENY: [&str; 5] = [tools::ASK_USER, "memory", "wake_me", "schedule", "workflow"];

/// What a helper is told when it asks the user something.
const NO_QUESTIONS: &str = "Questions are not available to helpers. Decide for yourself, or end with the open question in your report.";

/// What a scheduled run is told when it asks the user something.
const NOBODY_TO_ASK: &str = "Nobody is there to answer in a scheduled run. Decide for yourself and say in your answer what you assumed.";

/// What a helper is told when a call it needed a yes for did not get one.
const NOT_APPROVED: &str = "This was not approved, so it did not run. Do not try to get the same result another way: leave it, and say in your report what you could not do.";

/// How many times a round the provider turned away with a 429 is asked
/// again: rests of 2, 4 and 8 s ride out a burst, and a provider still
/// refusing after three is out of quota, which asking again will not fix.
const RETRIES: usize = 3;

/// The tool a run with a schema gives its answer through.
const STRUCTURED_OUTPUT: &str = "structured_output";

/// Answers turned down after the first before the run gives up: more
/// corrections than two make a model drop fields it had right.
const CORRECTIONS: usize = 2;

/// What an agent with a schema is told when it ends without giving its answer.
const REMINDER: &str = "You ended without calling structured_output, so your answer has not been given. Call structured_output now, once, with your final answer.";

/// What an answer cut off at the reply's token cap gets back, in place of the
/// usual advice to send a file in parts, which would leave an answer short.
const CUT_OFF: &str = "Your answer was cut off before it was complete, so it was not taken. Call structured_output again with the whole answer, worded more briefly: an answer cannot be sent in parts.";

/// What a run did, told as it happens to a frontend that shows the run live.
#[derive(Clone)]
pub enum Step {
    /// Words of the reply, as they stream in.
    Said(String),
    /// Words of the model's reasoning, as they stream in.
    Thought(String),
    /// A call the model asked for, before anything is decided about it.
    Asked(ToolCall),
    /// The call with this id is starting now, approved where it needed to be.
    Running(String),
    /// A call's result, by the call's id.
    Finished {
        id: String,
        output: String,
        failed: bool,
    },
    /// The checklist as a `todo` call left it.
    Checklist(Vec<Todo>),
}

/// Hears each step of a run on the run's own threads, so it must not block.
pub type Watch = Arc<dyn Fn(Step) + Send + Sync>;

/// Decides a call with the whole gate of the session a run speaks for.
pub type Decide = Arc<dyn Fn(&ToolCall, &ToolContext) -> crate::gate::Outcome + Send + Sync>;

/// Which tools an agent may call, before the deny set is taken out.
#[derive(Clone, Default)]
pub enum Allow {
    #[default]
    All,
    Only(Vec<String>),
}

/// Everything a run needs, decided by whoever starts it.
pub struct Spec {
    /// Its `conversation` should be the agent's id, which keys its browser and jobs.
    pub connection: Connection,
    /// Holds the conversation so far, the task at least, and takes each step as it is made.
    pub transcript: Transcript,
    pub id: AgentId,
    /// Hears about mail the agent never read.
    pub owner: u64,
    /// The session the work is for, whose rules answer approvals first.
    pub root: u64,
    /// The tree or workflow run the agent is part of, which an approval for the run covers.
    pub run: u64,
    /// The agent's name, as approval cards show it.
    pub label: String,
    pub allow: Allow,
    /// Tool rounds before it stops with what it has.
    pub rounds: usize,
    pub cancel: Cancel,
    pub budget: Option<Arc<Budget>>,
    pub status: Arc<Status>,
    /// How many helpers deep the agent is: 0 for a session's own, 1 for its helper.
    pub depth: u8,
    /// Whether it is a fork, started from its parent's messages, which may start no agent.
    pub fork: bool,
    /// Where its own messages start in its transcript: after its parent's, for a fork.
    pub from: usize,
    /// The shape its answer must have, given through `structured_output`
    /// instead of in prose; None for an answer in prose.
    pub schema: Option<Value>,
    /// Hears the run step by step; None when only the report is wanted.
    pub watch: Option<Watch>,
    /// The session's own gate, for a run that is a session's turn rather
    /// than a helper's; None asks about every call that needs a yes.
    pub decide: Option<Decide>,
}

/// How a run is getting on, in atomics a frontend reads while the run writes them.
#[derive(Default)]
pub struct Status {
    /// Tool rounds begun so far.
    pub rounds: AtomicUsize,
    /// Input tokens so far, cached ones included.
    pub input: AtomicU64,
    /// Input tokens the provider served from its cache so far.
    pub cached: AtomicU64,
    /// Tokens the model has written so far.
    pub output: AtomicU64,
}

/// How a run ended and what it came to.
#[derive(Debug)]
pub struct Report {
    pub status: Outcome,
    /// Its answer, or when it did not finish, the last thing it said.
    pub text: String,
    /// Its answer in the shape its schema asks for, once checked.
    pub value: Option<Value>,
    /// Tool rounds it ran.
    pub rounds: usize,
    /// Input tokens over the whole run, cached ones included.
    pub input: u64,
    pub cached: u64,
    pub output: u64,
    pub seconds: u64,
    /// Files it wrote or edited, as it named them, each once.
    pub files: Vec<String>,
    /// Jobs it left running, which are its owner's now.
    pub jobs: Vec<u64>,
}

/// Why a run ended.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// It answered.
    Done,
    /// It used every round it was given.
    RoundLimit,
    /// Somebody stopped it.
    Stopped,
    /// A model call failed, its transcript could not be written, or it never
    /// gave an answer in the shape its schema asks for.
    Failed(String),
    /// Its budget, or one above it, was spent.
    OverBudget,
}

/// A run on a thread of its own; joining it gives the report.
pub type Handle = std::thread::JoinHandle<Report>;

/// Runs the agent on this thread until it answers, is stopped, or runs out of rounds or budget.
pub fn run(spec: Spec) -> Report {
    run_keeping(spec).0
}

/// Runs the agent as `run` does and hands back its transcript too, for a
/// caller that carries on the same conversation with the next prompt.
pub fn run_keeping(mut spec: Spec) -> (Report, Transcript) {
    let started = Instant::now();
    crate::hooks::new_turn(spec.connection.conversation);
    let mut turn = Turn::with_limit(spec.connection.clone(), spec.rounds).unattended();
    let mut report = Report {
        status: Outcome::Done,
        text: String::new(),
        value: None,
        rounds: 0,
        input: 0,
        cached: 0,
        output: 0,
        seconds: 0,
        files: Vec::new(),
        jobs: Vec::new(),
    };
    let status = steps(&mut spec, &mut turn, &mut report)
        .and_then(|status| {
            // Calls that ran before a stop keep their results, so
            // whoever resumes the transcript knows what they did.
            keep_results(&mut spec, turn.take_results());
            save(&mut spec.transcript)?;
            Ok(status)
        })
        .unwrap_or_else(Outcome::Failed);
    // Before the unread mail is passed on, so a job that exits from here on
    // tells the owner itself, and one that exited already goes with the mail.
    report.jobs = tasks::reown(spec.id, spec.owner)
        .into_iter()
        .filter_map(|key| match key {
            Key::Job(id) => Some(id),
            _ => None,
        })
        .collect();
    forward_unread(&spec);
    report.status = status;
    // Only what it said: a fork's transcript opens with its parent's words.
    let messages = &spec.transcript.conversation.messages;
    report.text = last_said(messages.get(spec.from..).unwrap_or_default());
    report.seconds = started.elapsed().as_secs();
    (report, spec.transcript)
}

/// Runs the agent on a thread of its own, named for it, and hands its report
/// to `then` on that thread as it ends.
pub fn spawn(spec: Spec, then: impl FnOnce(&Report) + Send + 'static) -> Result<Handle, String> {
    std::thread::Builder::new()
        .name(format!("agent-{}", spec.id))
        .spawn(move || {
            let report = run(spec);
            then(&report);
            report
        })
        .map_err(|e| format!("Could not start the helper: {e}"))
}

/// Calls the model and works through what it asks for, round after round, until the run ends.
fn steps(spec: &mut Spec, turn: &mut Turn, report: &mut Report) -> Result<Outcome, String> {
    let mut sent_back = 0;
    // Before any call, since a schema that strays turns down every answer
    // with problems no agent can fix.
    if let Some(schema) = &spec.schema
        && let Err(problems) = structured::check_schema(schema)
    {
        return Ok(Outcome::Failed(format!(
            "The answer's schema cannot be checked: {}",
            problems.join("; ")
        )));
    }
    // Nor could it answer on a connection that offers no tools at all.
    if spec.schema.is_some() && !spec.connection.tools {
        return Ok(Outcome::Failed(
            "The model runs without tools here, so it cannot give its answer through structured_output.".into(),
        ));
    }
    let mut reminded = false;
    loop {
        if spec.cancel.is_set() {
            return Ok(Outcome::Stopped);
        }
        if spec
            .budget
            .as_ref()
            .is_some_and(|budget| budget.check().is_err())
        {
            return Ok(Outcome::OverBudget);
        }
        let Some(reply) = ask_in_line(spec, turn)? else {
            return Ok(Outcome::Stopped);
        };
        report.input += reply.input_tokens();
        report.cached += reply.cached_tokens;
        report.output += reply.tokens;
        spec.status.input.store(report.input, Ordering::Relaxed);
        spec.status.cached.store(report.cached, Ordering::Relaxed);
        spec.status.output.store(report.output, Ordering::Relaxed);
        if let Some(budget) = &spec.budget {
            budget.charge(&reply);
        }
        let calls = reply.tool_calls.clone();
        spec.transcript.conversation.messages.push(reply);
        for call in &calls {
            tell(spec, Step::Asked(call.clone()));
        }
        if calls.is_empty() {
            // Mail that came while it worked may change the answer, so it
            // carries the run on rather than going unread.
            let mut mail = inbox::fold(&inbox::take(spec.id, true));
            if mail.is_none() && spec.schema.is_some() {
                if reminded {
                    return Ok(Outcome::Failed(
                        "It ended without giving its answer through structured_output, even when reminded.".into(),
                    ));
                }
                // An answer in prose is not the one asked for, so the run
                // goes on once more for it.
                reminded = true;
                mail = inbox::fold(&[Notice {
                    from: "Letronna".into(),
                    when: When::Next,
                    text: REMINDER.into(),
                }]);
            }
            let answered = mail.is_none();
            spec.transcript.conversation.messages.extend(mail);
            save(&mut spec.transcript)?;
            if !answered {
                continue;
            }
            // A session's own agent, as `-p` runs it, hears its Stop hooks as a
            // turn in the terminal does; a helper's answer is its parent's to judge.
            let feedback = (spec.depth == 0 && sent_back < crate::hooks::STOP_BLOCKS)
                .then(|| {
                    let last = last_said(&spec.transcript.conversation.messages);
                    crate::hooks::stop(&context(spec), &last, sent_back > 0)
                })
                .flatten();
            let Some(feedback) = feedback else {
                return Ok(Outcome::Done);
            };
            sent_back += 1;
            spec.transcript.conversation.messages.push(Message {
                role: "user".into(),
                content: feedback,
                folded: true,
                ..Default::default()
            });
            save(&mut spec.transcript)?;
            continue;
        }
        save(&mut spec.transcript)?;
        // A wrap-up in prose is no answer for a run with a schema, so at the
        // cap it takes the answer the reply holds, if any, instead.
        let capped = spec.schema.is_some() && report.rounds >= spec.rounds;
        if capped || !turn.begin(calls) {
            return at_limit(spec, report);
        }
        report.rounds += 1;
        spec.status.rounds.store(report.rounds, Ordering::Relaxed);
        work_through(spec, turn, report)?;
        // A hook's `continue: false`, whose results are kept.
        if crate::hooks::halted(spec.connection.conversation) {
            return Ok(Outcome::Stopped);
        }
        if let Some(outcome) = settled(spec, report) {
            return Ok(outcome);
        }
        // After the answer, which a repeat in the same reply must not throw away.
        if let Some(why) = turn.looping() {
            return Ok(Outcome::Failed(why.to_owned()));
        }
    }
}

/// Works through one reply's calls, running together the ones that may, then
/// hands back their results and any mail that cannot wait, leaving early at a stop.
fn work_through(spec: &mut Spec, turn: &mut Turn, report: &mut Report) -> Result<(), String> {
    // Read once a reply, so a rule the user keeps meanwhile holds from the next.
    let rules = permissions::load(&permissions::user_file());
    loop {
        if spec.cancel.is_set() {
            return Ok(());
        }
        let decide = |call: &ToolCall| {
            let decide = spec.decide.as_ref()?;
            // The run's answer is no tool a session's gate knows; `gated`
            // lets it through to `dispatch`, which takes it.
            if call.name == STRUCTURED_OUTPUT && spec.schema.is_some() {
                return None;
            }
            Some(decide(call, &context(spec)))
        };
        // Only what the gate runs as the call stands joins a batch; one it
        // would put to the judge or the user, or refuse, is taken alone.
        let held = |call: &ToolCall| {
            let outcome = decide(call).unwrap_or_else(|| gated(spec, call, &rules, false).0);
            outcome != gate::Outcome::Run
        };
        let batch = turn.next_batch(held, MAX_BATCH);
        if !batch.is_empty() {
            for call in &batch {
                tell(spec, Step::Running(call.id.clone()));
            }
            for (call, outcome) in batch.iter().zip(together(spec, &batch)) {
                // One a stop kept from starting gets no result, as a lone call does.
                if let Some(outcome) = outcome {
                    turn.finish_id(&call.id, outcome);
                }
            }
            continue;
        }
        let (outcome, layer) = match turn.front() {
            Some(call) => match decide(call) {
                Some(outcome) if call_refusal(spec, call).is_none() => (outcome, None),
                _ => gated(spec, call, &rules, true),
            },
            None => (gate::Outcome::Run, None),
        };
        let outcome = match turn.next(|_| outcome.clone()) {
            Next::Done => return Ok(()),
            Next::Ask => {
                keep_results(spec, turn.take_results());
                // After the results, which have to follow the calls they
                // answer, and only if the run goes on: mail a run ending here
                // would never read is left for its owner.
                if settled(spec, report).is_none() && turn.looping().is_none() {
                    // Folded into the run, so the window keeps its task.
                    let mail = inbox::fold(&inbox::take(spec.id, false));
                    let mail = mail.map(|mail| Message {
                        folded: true,
                        ..mail
                    });
                    spec.transcript.conversation.messages.extend(mail);
                }
                return save(&mut spec.transcript);
            }
            Next::Run(call) => dispatch(spec, &call, false, report),
            Next::Refuse(_, why) => Some(Err(why)),
            Next::AskUser(call) => {
                // Only the judge's word says why, when it was unsure.
                let why = layer.as_ref().map(Layer::tag).unwrap_or_default();
                let (ctx, lasting) = (context(spec), lasting(spec, &call, &rules));
                match approvals::ask(spec.root, spec.run, &spec.label, &call, &ctx, &why, lasting) {
                    Ok(None) => dispatch(spec, &call, true, report),
                    // Only what was agreed to runs, never the model's own,
                    // and a yes to the model's call is none to a changed one.
                    Ok(Some(arguments)) => {
                        dispatch(spec, &ToolCall { arguments, ..call }, false, report)
                    }
                    Err(_) if spec.cancel.is_set() => None,
                    Err(why) => {
                        // So the same call from any agent of the run asks
                        // again rather than being judged afresh.
                        judge::stop(&call, &spec.connection.typed, spec.run);
                        match why.as_str() {
                            "" => Some(Err(NOT_APPROVED.into())),
                            why => Some(Err(format!("{NOT_APPROVED} {why}"))),
                        }
                    }
                }
            }
        };
        let Some(outcome) = outcome else {
            return Ok(());
        };
        turn.finish_call(outcome);
    }
}

/// What the gate makes of `call` from this agent: in the agent's own mode,
/// with `rules`, the user's, and what was allowed for its run or by its
/// session. `judged` puts what would ask to Auto mode's judge, on the
/// session's own model and given what the user typed in its session, with
/// the judge's word; otherwise the judge is never waited on.
fn gated(
    spec: &Spec,
    call: &ToolCall,
    rules: &Result<Vec<Rule>, String>,
    judged: bool,
) -> (gate::Outcome, Option<Layer>) {
    // Not a tool: `dispatch` checks the answer instead.
    if call.name == STRUCTURED_OUTPUT && spec.schema.is_some() {
        return (gate::Outcome::Run, None);
    }
    if let Some(refused) = call_refusal(spec, call) {
        return (gate::Outcome::Refuse(refused), None);
    }
    let ctx = context(spec);
    let yes = approvals::granted(spec.run, call, &ctx)
        || approvals::allowed_by_session(spec.root, call, &ctx);
    let grants = granting(&ctx, call, yes);
    let gate = gate_of(spec, &ctx, rules, &grants);
    if !judged {
        return (gate::decide(call, &gate), None);
    }
    // The session's own model judges, never one a helper's caller picked,
    // and what the judge keeps, a No and the limits on denials, holds for
    // the whole run rather than one helper.
    let judging = Connection {
        model: spec.connection.judge_model().to_owned(),
        conversation: spec.run,
        ..spec.connection.clone()
    };
    gate::settle(call, &gate, &judging, &spec.connection.typed)
}

/// Whether a yes to `call` for the run would let the gate run the calls it
/// covers, as a grant does; not where the floor, an ask rule or a rules file
/// that cannot be read asks every time.
fn lasting(spec: &Spec, call: &ToolCall, rules: &Result<Vec<Rule>, String>) -> bool {
    let ctx = context(spec);
    let grants = granting(&ctx, call, true);
    gate::decide(call, &gate_of(spec, &ctx, rules, &grants)) == gate::Outcome::Run
}

/// `call` allowed in `ctx`'s conversation when `yes`, as a yes for the run
/// counts: after the floor and the rules, before the judge, and kept as the
/// session's own grant keeps it, by `gate::grant`.
fn granting(ctx: &ToolContext, call: &ToolCall, yes: bool) -> Grants {
    let mut grants = Grants::new();
    if yes {
        grants.insert(ctx.conversation, HashSet::from([gate::grant(call)]));
    }
    grants
}

/// The gate for this agent's calls in `ctx`, with `rules` and `grants`.
fn gate_of<'a>(
    spec: &Spec,
    ctx: &'a ToolContext,
    rules: &'a Result<Vec<Rule>, String>,
    grants: &'a Grants,
) -> Gate<'a> {
    Gate {
        ctx,
        mode: spec.connection.mode,
        grants,
        rules: rules.as_deref().unwrap_or_default(),
        broken: rules.as_ref().err().map(String::as_str),
        human: spec.connection.human,
    }
}

/// Makes one call the way this agent may, `approved` when the user said yes
/// to it on its card, or gives None when a stop came before it began.
fn dispatch(
    spec: &mut Spec,
    call: &ToolCall,
    approved: bool,
    report: &mut Report,
) -> Option<Result<String, String>> {
    // Ahead of the allow list, which names only the tools that really run.
    if call.name == STRUCTURED_OUTPUT
        && let Some(schema) = &spec.schema
    {
        return Some(answer(schema, &call.arguments, report));
    }
    if let Some(refused) = call_refusal(spec, call) {
        return Some(Err(refused));
    }
    tell(spec, Step::Running(call.id.clone()));
    if call.name == tools::TODO {
        let outcome = checklist(&mut spec.transcript, &call.arguments);
        let todos = spec.transcript.conversation.todos.clone();
        tell(spec, Step::Checklist(todos));
        return Some(outcome);
    }
    let ctx = ToolContext {
        approved,
        ..context(spec)
    };
    let outcome = off_thread(call, &ctx)?;
    if outcome.is_ok() && matches!(call.name.as_str(), "write_file" | "edit_file") {
        let args = tools::parse_args(&call.arguments).unwrap_or_default();
        if let Some(path) = args["path"].as_str()
            && !report.files.iter().any(|file| file == path)
        {
            report.files.push(path.to_owned());
        }
    }
    if outcome.is_ok() && call.name == crate::patch::APPLY_PATCH {
        let args = tools::parse_args(&call.arguments).unwrap_or_default();
        for path in crate::patch::paths(&args).unwrap_or_default() {
            if !report.files.contains(&path) {
                report.files.push(path);
            }
        }
    }
    Some(outcome)
}

/// Runs a batch of calls at once, each on a thread of its own, giving their
/// outcomes in the batch's order and None for one a stop kept from starting.
fn together(spec: &Spec, batch: &[ToolCall]) -> Vec<Option<Result<String, String>>> {
    std::thread::scope(|scope| {
        let running: Vec<_> = batch
            .iter()
            .map(|call| {
                // Only tools that opt in are batched, and neither `todo` nor a
                // write is one, so nothing here needs `dispatch`'s special cases.
                let refused = refused(spec, &call.name);
                let ctx = context(spec);
                scope.spawn(move || match refused {
                    Some(refused) => Some(Err(refused)),
                    None => off_thread(call, &ctx),
                })
            })
            .collect();
        batch
            .iter()
            .zip(running)
            .map(|(call, running)| {
                running.join().unwrap_or_else(|_| {
                    Some(Err(format!("`{}` stopped without finishing.", call.name)))
                })
            })
            .collect()
    })
}

/// Why this agent may not call `name`, when it may not.
pub(crate) fn refusal(allow: &Allow, name: &str) -> Option<String> {
    if name == tools::ASK_USER {
        return Some(NO_QUESTIONS.into());
    }
    let allowed = match allow {
        Allow::All => true,
        Allow::Only(names) => names.iter().any(|allowed| allowed == name),
    };
    if DENY.contains(&name) || !allowed {
        return Some(format!("`{name}` is not available to this helper."));
    }
    None
}

/// Why this agent may not call `name`, when it may not: a scheduled run's
/// own agent as such, any other as `refusal` says.
fn refused(spec: &Spec, name: &str) -> Option<String> {
    if spec.depth > 0 || crate::schedule::running(spec.root).is_none() {
        return refusal(&spec.allow, name);
    }
    // It may remember things as a chat may, but has nobody to ask and no
    // frontend's loop to come back to it later.
    match name {
        tools::ASK_USER => Some(NOBODY_TO_ASK.into()),
        "wake_me" | "schedule" | "workflow" => {
            Some(format!("`{name}` is not available in a scheduled run."))
        }
        _ => None,
    }
}

/// Why this agent may not make `call`, when it may not: its tool, or for a
/// batch of browser steps the tool of any step, which runs as that tool's.
fn call_refusal(spec: &Spec, call: &ToolCall) -> Option<String> {
    let steps = crate::browser::batch_calls(call).and_then(Result::ok);
    let names = steps.iter().flatten().map(|step| step.name.as_str());
    std::iter::once(call.name.as_str())
        .chain(names)
        .find_map(|name| refused(spec, name))
}

/// What this agent's tool calls can reach, stopping when it stops.
fn context(spec: &Spec) -> ToolContext {
    let ctx = ToolContext::of(&spec.connection);
    ToolContext {
        cancel: spec.cancel.clone(),
        // A helper this agent starts is one deeper, in the same tree and
        // spending from the same budget.
        caller: Caller {
            agent: Some(spec.id),
            root: spec.root,
            depth: spec.depth,
            budget: spec.budget.clone(),
            run: Some(spec.run),
            allowed: match &spec.allow {
                Allow::All => None,
                Allow::Only(names) => Some(names.as_slice().into()),
            },
            fork: spec.fork,
            allow: spec.allow.clone(),
            ..ctx.caller.clone()
        },
        ..ctx
    }
}

/// Runs a tool on a thread of its own, so a stop ends the wait even for a
/// tool that does not listen for one, and gives None when a stop came before it began.
fn off_thread(call: &ToolCall, ctx: &ToolContext) -> Option<Result<String, String>> {
    // A stop that lands after the batch last looked must still keep a tool
    // that changes things from starting.
    if ctx.cancel.is_set() {
        return None;
    }
    let (sender, receiver) = async_channel::bounded(1);
    let (name, arguments, context) = (call.name.clone(), call.arguments.clone(), ctx.clone());
    std::thread::spawn(move || {
        let _ = sender.send_blocking(tools::run(&name, &arguments, &context));
    });
    let closer = receiver.clone();
    let _waker = ctx.cancel.on_set(move || {
        closer.close();
    });
    match receiver.recv_blocking() {
        Ok(outcome) => Some(outcome),
        // Its thread carries on, so the transcript must not say it never ran.
        Err(_) if ctx.cancel.is_set() => Some(Err(format!(
            "Stopped while `{}` was running; it may have finished anyway.",
            call.name
        ))),
        Err(_) => Some(Err(format!("`{}` stopped without finishing.", call.name))),
    }
}

/// Waits for a turn on the provider, then calls the model, asking again after
/// a 429 once the provider has had its rest.
fn ask_in_line(spec: &Spec, turn: &mut Turn) -> Result<Option<Message>, String> {
    let gate = limits::gate(&spec.connection.endpoint);
    let mut retries = 0;
    loop {
        let Some(permit) = gate.acquire(&spec.cancel) else {
            return Ok(None);
        };
        let throttled = gate.throttled();
        let error = match ask(spec, turn, permit) {
            Ok(Some(reply)) => {
                gate.answered();
                return Ok(Some(reply));
            }
            Ok(None) => return Ok(None),
            Err(error) => error,
        };
        // The error does not say which call a 429 was for, so one on this
        // provider while the call ran counts; that costs a call at most.
        if gate.throttled() == throttled || retries == RETRIES {
            return Err(error);
        }
        retries += 1;
        // Whatever came before the failure goes, so the next try starts clean.
        turn.take_reply(&spec.connection.model);
    }
}

/// Calls the model with the conversation so far and reads the reply to its
/// end, holding `permit` as long as the stream lasts, or gives None when a
/// stop ended it first.
fn ask(spec: &Spec, turn: &mut Turn, permit: Permit) -> Result<Option<Message>, String> {
    let extra = Extra {
        tools: spec.schema.iter().map(answer_tool).collect(),
        cancel: spec.cancel.clone(),
        guard: Some(Box::new(permit)),
    };
    let events = chat::start_with(
        spec.connection.clone(),
        spec.transcript.conversation.messages.clone(),
        extra,
    );
    // A read waiting on the provider cannot hear a stop, so the stop closes
    // this end instead of waiting for the next line.
    let closer = events.clone();
    let _waker = spec.cancel.on_set(move || {
        closer.close();
    });
    while let Ok(event) = events.recv_blocking() {
        // Told as it streams, so the words of a try a 429 cuts short stay
        // with the watcher though the reply drops them, as on a screen.
        match &event {
            Event::Delta(text) => tell(spec, Step::Said(text.clone())),
            Event::Reasoning(text) => tell(spec, Step::Thought(text.clone())),
            _ => {}
        }
        turn.absorb(&event);
        if let Event::Finished(result) = event {
            return match result {
                Ok(()) => Ok(Some(turn.take_reply(&spec.connection.model))),
                Err(_) if spec.cancel.is_set() => Ok(None),
                Err(error) => Err(error.to_string()),
            };
        }
    }
    if spec.cancel.is_set() {
        return Ok(None);
    }
    Err("The model connection closed unexpectedly.".into())
}

/// Keeps the checklist a `todo` call sends on the transcript, as the window
/// keeps a session's.
fn checklist(transcript: &mut Transcript, arguments: &str) -> Result<String, String> {
    let args = tools::parse_args(arguments)?;
    let items = args["items"].as_array().ok_or("`items` is required.")?;
    let todos: Vec<Todo> = items
        .iter()
        .map(|item| Todo {
            text: item["text"].as_str().unwrap_or_default().to_owned(),
            status: item["status"].as_str().unwrap_or("pending").to_owned(),
        })
        .filter(|todo| !todo.text.is_empty())
        .collect();
    let done = todos.iter().filter(|todo| todo.status == "done").count();
    let total = todos.len();
    // Written with the batch's results, like everything else the batch changed.
    transcript.conversation.todos = todos;
    Ok(format!("Checklist updated: {done} of {total} done."))
}

/// Keeps the answer a `structured_output` call gives on the report when it
/// fits `schema`, and otherwise gives back only what is wrong with it.
fn answer(schema: &Value, arguments: &str, report: &mut Report) -> Result<String, String> {
    let mut args = tools::parse_args(arguments).map_err(|problem| {
        match serde_json::from_str::<Value>(arguments) {
            Err(e) if e.is_eof() => CUT_OFF.to_owned(),
            _ => problem,
        }
    })?;
    structured::check(&parameters(schema), &args).map_err(|problems| problems.join("\n"))?;
    report.value = Some(if wrapped(schema) {
        args.get_mut("answer").map(Value::take).unwrap_or_default()
    } else {
        args
    });
    Ok("Answer received.".into())
}

/// How a run with a schema ends after a round's results, when they settle its
/// answer: given, or turned down once more than it may be corrected.
fn settled(spec: &Spec, report: &Report) -> Option<Outcome> {
    spec.schema.as_ref()?;
    if report.value.is_some() {
        return Some(Outcome::Done);
    }
    let turned_down: Vec<&str> = spec
        .transcript
        .conversation
        .messages
        .iter()
        .filter(|m| m.role == "tool" && m.name == STRUCTURED_OUTPUT && m.is_error)
        .map(|m| m.content.as_str())
        .collect();
    match turned_down.last() {
        Some(last) if turned_down.len() > CORRECTIONS => Some(Outcome::Failed(format!(
            "Its answer still did not fit the schema after {CORRECTIONS} corrections:\n{last}"
        ))),
        _ => None,
    }
}

/// How a run ends at its round limit: one with a schema still takes an answer
/// from the reply the limit turned away, since giving it starts no more work
/// and the answer is what the run is for.
fn at_limit(spec: &mut Spec, report: &mut Report) -> Result<Outcome, String> {
    let reply = spec.transcript.conversation.messages.last();
    let call = reply.and_then(|reply| {
        reply
            .tool_calls
            .iter()
            .find(|call| call.name == STRUCTURED_OUTPUT)
    });
    let (Some(schema), Some(call)) = (&spec.schema, call.cloned()) else {
        return Ok(Outcome::RoundLimit);
    };
    let (content, is_error) = match answer(schema, &call.arguments, report) {
        Ok(received) => (received, false),
        Err(problems) => (problems, true),
    };
    // Answered on the transcript, as a call in a round is; the reply's other
    // calls are left without results, as at any limit.
    spec.transcript.conversation.messages.push(Message {
        role: "tool".into(),
        content,
        tool_call_id: call.id,
        name: call.name,
        args: call.arguments,
        is_error,
        ..Default::default()
    });
    save(&mut spec.transcript)?;
    Ok(match report.value {
        Some(_) => Outcome::Done,
        None => Outcome::RoundLimit,
    })
}

/// The tool an agent with `schema` gives its answer through.
fn answer_tool(schema: &Value) -> Value {
    json!({"type": "function", "function": {
        "name": STRUCTURED_OUTPUT,
        "description": "Gives your final answer, in the shape the parameters describe, and ends your work: an answer written anywhere else is not read. Call it exactly once, when the task is done, with the whole answer. If it replies with problems, call it again with them fixed.",
        "parameters": parameters(schema),
    }})
}

/// `schema` as the tool's parameters, which providers take only as an object,
/// so an answer of any other shape is asked for as the field `answer`.
fn parameters(schema: &Value) -> Value {
    if !wrapped(schema) {
        return schema.clone();
    }
    json!({"type": "object", "required": ["answer"], "properties": {"answer": schema}})
}

/// Whether an answer to `schema` comes as the field `answer`, not being an object.
fn wrapped(schema: &Value) -> bool {
    schema.get("type").and_then(Value::as_str) != Some("object")
}

/// Passes mail that came too late to be read on to the owner, who can still act on it.
fn forward_unread(spec: &Spec) {
    for notice in inbox::take(spec.id, true) {
        inbox::push(
            spec.owner,
            Notice {
                text: format!(
                    "{} ended before it read this, from {}:\n{}",
                    spec.label, notice.from, notice.text
                ),
                from: spec.label.clone(),
                when: notice.when,
            },
        );
    }
}

/// The last thing the agent said, which is its answer when it finished.
pub(crate) fn last_said(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && !message.content.is_empty())
        .map(|message| message.content.clone())
        .unwrap_or_default()
}

/// Tells the run's watcher about `step`, if it has one.
fn tell(spec: &Spec, step: Step) {
    if let Some(watch) = &spec.watch {
        watch(step);
    }
}

/// Puts a batch's results on the transcript, telling the watcher each one.
fn keep_results(spec: &mut Spec, results: Vec<Message>) {
    for result in results.iter().filter(|message| message.role == "tool") {
        tell(
            spec,
            Step::Finished {
                id: result.tool_call_id.clone(),
                output: result.content.clone(),
                failed: result.is_error,
            },
        );
    }
    spec.transcript.conversation.messages.extend(results);
}

fn save(transcript: &mut Transcript) -> Result<(), String> {
    transcript
        .save()
        .map_err(|e| format!("Could not save the helper's conversation: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::inbox::When;
    use crate::store::Conversation;
    use crate::store::tests::Scratch;
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::Path;
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// Far longer than any run here takes, so only a hang fails a test.
    const WAIT: Duration = Duration::from_secs(60);
    /// Where the tests' agent ids start, clear of every session id as real ones are.
    const AGENTS: AgentId = 1 << 40;

    /// A workspace holding `notes.txt`, removed when dropped.
    fn scratch(name: &str) -> Scratch {
        let dir = Scratch::new(&format!("driver-{name}"));
        std::fs::write(dir.join("notes.txt"), "hello from the notes").unwrap();
        dir
    }

    /// Agent `id` with one task, working in `dir` against the provider at `endpoint`.
    fn spec(endpoint: String, dir: &Path, id: AgentId) -> Spec {
        let task = Message {
            role: "user".into(),
            content: "Read notes.txt and say what it says.".into(),
            ..Default::default()
        };
        Spec {
            connection: Connection {
                endpoint,
                model: "test-model".into(),
                tools: true,
                workspace: Some(dir.to_owned()),
                conversation: id,
                ..Default::default()
            },
            transcript: Transcript {
                path: dir.join("helper.jsonl"),
                conversation: Conversation {
                    id: 7,
                    title: "helper".into(),
                    messages: vec![task],
                    ..Default::default()
                },
            },
            id,
            owner: id + 1,
            root: 7,
            run: id,
            label: "helper".into(),
            allow: Allow::All,
            rounds: 50,
            cancel: Cancel::default(),
            budget: None,
            status: Arc::default(),
            depth: 1,
            fork: false,
            from: 0,
            schema: None,
            watch: None,
            decide: None,
        }
    }

    fn listen() -> (TcpListener, String) {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        (server, endpoint)
    }

    /// The body of the request coming in on `socket`.
    fn request(socket: &mut TcpStream) -> Value {
        let mut bytes = Vec::new();
        let mut block = [0; 65536];
        loop {
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
                return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
            }
        }
    }

    /// Sends `reply` whole and closes, as a provider's finished stream does.
    fn answer(socket: &mut TcpStream, reply: &str) {
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
    }

    /// A local provider at work on its thread, and where to reach it.
    struct Stub {
        address: SocketAddr,
        worker: JoinHandle<Vec<Value>>,
    }

    impl Stub {
        /// What each request carried, failing rather than hanging when the
        /// run asked for fewer replies than the stub was given.
        fn sent(self) -> Vec<Value> {
            if !self.worker.is_finished() {
                // An empty request ends the wait for one the run will never send.
                let _ = TcpStream::connect(self.address);
            }
            self.worker
                .join()
                .expect("the run asked for fewer replies than the stub was given")
        }
    }

    /// A provider that answers one request with each of `replies` in turn,
    /// and hands back what each request carried.
    fn provider(replies: Vec<String>) -> (String, Stub) {
        let (server, endpoint) = listen();
        let address = server.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            replies
                .iter()
                .map(|reply| {
                    let (mut socket, _) = server.accept().unwrap();
                    let sent = request(&mut socket);
                    answer(&mut socket, reply);
                    sent
                })
                .collect()
        });
        (endpoint, Stub { address, worker })
    }

    /// A streamed reply that says `text` and makes `calls`, taking 1,000 tokens in and 50 out.
    fn reply(text: &str, calls: &[(&str, Value)]) -> String {
        let calls: Vec<Value> = calls
            .iter()
            .enumerate()
            .map(|(i, (name, args))| {
                json!({"index": i, "id": format!("call-{i}"),
                    "function": {"name": name, "arguments": args.to_string()}})
            })
            .collect();
        let mut events = Vec::new();
        if !text.is_empty() {
            events.push(json!({"choices": [{"delta": {"content": text}}]}));
        }
        if !calls.is_empty() {
            events.push(json!({"choices": [{"delta": {"tool_calls": calls}}]}));
        }
        events.push(json!({"choices": [],
            "usage": {"prompt_tokens": 1_000, "completion_tokens": 50}}));
        let mut out: String = events
            .iter()
            .map(|event| format!("data: {event}\n\n"))
            .collect();
        out.push_str("data: [DONE]\n\n");
        out
    }

    fn read_notes() -> (&'static str, Value) {
        ("read_file", json!({"path": "notes.txt"}))
    }

    /// Runs `spec` the way `spawn` does and waits for its report.
    fn finished(spec: Spec) -> Report {
        let name = format!("agent-{}", spec.id);
        let handle = spawn(spec, |_| ()).unwrap();
        assert_eq!(handle.thread().name(), Some(name.as_str()));
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(handle.join());
        });
        rx.recv_timeout(WAIT).expect("the run never ended").unwrap()
    }

    /// The conversation a request sent.
    fn messages(body: &Value) -> &Vec<Value> {
        body["messages"].as_array().unwrap()
    }

    /// What a request sent back as tool results, in order.
    fn results(body: &Value) -> Vec<&str> {
        messages(body)
            .iter()
            .filter(|message| message["role"] == "tool")
            .map(|message| message["content"].as_str().unwrap())
            .collect()
    }

    /// A command's output as words on one line: PowerShell's `echo` puts
    /// each word on a line of its own, where sh's keeps them on one.
    fn words(output: &str) -> String {
        output.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    /// The records in a transcript file, in the order they were written.
    fn written(path: &Path) -> Vec<Value> {
        std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).unwrap())
            .collect()
    }

    fn notice(from: &str, when: When, text: &str) -> Notice {
        Notice {
            from: from.into(),
            when,
            text: text.into(),
        }
    }

    #[test]
    fn runs_a_tool_then_answers() {
        let dir = scratch("answers");
        let (endpoint, provider) = provider(vec![
            reply("", &[read_notes()]),
            reply("The notes say hello.", &[]),
        ]);
        let spec = spec(endpoint, &dir, AGENTS + 10);
        let status = spec.status.clone();
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(report.text, "The notes say hello.");
        assert_eq!(
            (report.rounds, report.input, report.output),
            (1, 2_000, 100)
        );
        assert_eq!(status.rounds.load(Ordering::Relaxed), 1);
        assert_eq!(status.output.load(Ordering::Relaxed), 100);
        assert_eq!(results(&sent[1]), ["1|hello from the notes"]);
        let roles: Vec<String> = written(&dir.join("helper.jsonl"))
            .iter()
            .filter(|record| record["type"] == "message")
            .map(|record| record["role"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(roles, ["user", "assistant", "tool", "assistant"]);
    }

    #[test]
    fn a_tool_it_may_not_use_is_refused_not_run() {
        let dir = scratch("refused");
        let (endpoint, provider) = provider(vec![
            reply(
                "",
                &[
                    ("write_file", json!({"path": "made.txt", "content": "made"})),
                    ("memory", json!({"action": "add", "text": "likes figs"})),
                ],
            ),
            reply("I could not do that.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 20);
        // Named in its list, a tool in the deny set is still refused.
        spec.allow = Allow::Only(vec!["read_file".into(), "memory".into()]);
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert!(!dir.join("made.txt").exists());
        assert!(report.files.is_empty());
        // Refused outright, where a call sent for approval would have been
        // declined with nobody serving.
        assert_eq!(
            results(&sent[1]),
            [
                "`write_file` is not available to this helper.",
                "`memory` is not available to this helper."
            ]
        );
    }

    #[test]
    fn a_batch_step_of_a_tool_it_may_not_use_is_refused_not_asked_about() {
        let dir = scratch("refused-step");
        let steps = json!([
            {"action": "click", "ref": "e1"},
            {"action": "hover", "ref": "e2"}
        ]);
        let (endpoint, provider) = provider(vec![
            reply(
                "",
                &[("use_page", json!({"action": "batch", "steps": steps}))],
            ),
            reply("I could not do that.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 100);
        // A hover is browse's, which this helper was not given.
        spec.allow = Allow::Only(vec!["use_page".into()]);
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        assert_eq!(
            results(&provider.sent()[1]),
            ["`browse` is not available to this helper."]
        );
    }

    #[test]
    fn ask_user_is_refused_with_advice() {
        let dir = scratch("question");
        let (endpoint, provider) = provider(vec![
            reply("", &[("ask_user", json!({"question": "Which framework?"}))]),
            reply("I kept the one already in use.", &[]),
        ]);
        let report = finished(spec(endpoint, &dir, AGENTS + 30));
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(
            results(&sent[1]),
            [
                "Questions are not available to helpers. Decide for yourself, or end with the open question in your report."
            ]
        );
    }

    #[test]
    fn a_message_mid_run_folds_in_after_the_results() {
        let _bell = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("mail");
        let (endpoint, provider) = provider(vec![
            reply("", &[read_notes()]),
            reply("The notes say hello.", &[]),
            reply("The notes say hello, and the hour is up.", &[]),
        ]);
        let id = AGENTS + 40;
        let spec = spec(endpoint, &dir, id);
        inbox::push(id, notice("main", When::Next, "also check the tests"));
        inbox::push(id, notice("timer", When::Later, "the hour is up"));
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(report.text, "The notes say hello, and the hour is up.");
        let second = messages(&sent[1]);
        let (result, mail) = (&second[second.len() - 2], &second[second.len() - 1]);
        assert_eq!(result["role"], "tool");
        assert_eq!(mail["role"], "user");
        let mail = mail["content"].as_str().unwrap();
        assert!(
            mail.starts_with("[Letronna notice") && mail.contains("also check the tests"),
            "{mail}"
        );
        assert!(
            !mail.contains("the hour is up"),
            "what could wait came mid-run"
        );
        // What could wait carried on the answer it was held back from.
        let third = messages(&sent[2]);
        let later = third.last().unwrap()["content"].as_str().unwrap();
        assert!(later.contains("the hour is up"), "{later}");
        assert!(inbox::take(id, true).is_empty());
        // Mail read mid-run is folded into it, so the window keeps the task;
        // mail that carries the run on after its answer starts a turn.
        let saved = written(&dir.join("helper.jsonl"));
        let folded: Vec<bool> = saved
            .iter()
            .filter(|message| message["notice"] == true)
            .map(|message| message["folded"] == true)
            .collect();
        assert_eq!(folded, [true, false]);
    }

    #[test]
    fn stopping_ends_it_between_steps_with_what_it_had() {
        let _bell = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("stop");
        let (server, endpoint) = listen();
        let address = server.local_addr().unwrap();
        let id = AGENTS + 50;
        let spec = spec(endpoint, &dir, id);
        let stop = spec.cancel.clone();
        let (hold, held) = mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let (mut first, _) = server.accept().unwrap();
            let asked = request(&mut first);
            answer(
                &mut first,
                &reply("Reading the notes first.", &[read_notes()]),
            );
            let (mut second, _) = server.accept().unwrap();
            let asked_again = request(&mut second);
            stop.set();
            // Never answered, so the run has to end without it.
            let _ = held.recv();
            vec![asked, asked_again]
        });
        let provider = Stub { address, worker };
        inbox::push(id, notice("main", When::Later, "then run the tests"));
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Stopped);
        drop(hold);
        provider.sent();

        assert_eq!(report.text, "Reading the notes first.");
        assert_eq!(report.rounds, 1);
        let last = written(&dir.join("helper.jsonl")).pop().unwrap();
        assert_eq!(
            (last["role"].as_str(), last["content"].as_str()),
            (Some("tool"), Some("1|hello from the notes"))
        );
        // Mail it never read goes to whoever started it.
        let forwarded = inbox::take(id + 1, true);
        let texts: Vec<&str> = forwarded.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(
            texts,
            ["helper ended before it read this, from main:\nthen run the tests"]
        );
    }

    #[test]
    fn over_budget_it_stops_before_the_next_call() {
        let dir = scratch("budget");
        // Only one answer: a second call would find nobody and fail the run.
        let (endpoint, provider) = provider(vec![reply("Reading first.", &[read_notes()])]);
        let mut spec = spec(endpoint, &dir, AGENTS + 60);
        let budget = Arc::new(Budget::new(1_000, None));
        spec.budget = Some(budget.clone());
        let report = finished(spec);
        assert_eq!(report.status, Outcome::OverBudget);
        let sent = provider.sent();

        assert_eq!(report.text, "Reading first.");
        assert_eq!(sent.len(), 1);
        assert_eq!(budget.spent(), 1_050);
        // The call it had asked for still ran and was kept.
        let last = written(&dir.join("helper.jsonl")).pop().unwrap();
        assert_eq!(last["role"], "tool");
    }

    #[test]
    fn todo_lands_on_the_transcript() {
        let dir = scratch("todo");
        let items = json!([
            {"text": "read the notes", "status": "done"},
            {"text": "answer", "status": "doing"}
        ]);
        let (endpoint, provider) = provider(vec![
            reply("", &[("todo", json!({ "items": items.clone() }))]),
            reply("Planned.", &[]),
        ]);
        let report = finished(spec(endpoint, &dir, AGENTS + 70));
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert!(results(&sent[1])[0].starts_with("Checklist updated: 1 of 2 done."));
        let meta = written(&dir.join("helper.jsonl"))
            .into_iter()
            .rfind(|record| record["type"] == "meta")
            .unwrap();
        assert_eq!(meta["todos"], items);
    }

    #[test]
    fn a_batch_of_reads_answers_in_the_order_asked() {
        let dir = scratch("batch");
        std::fs::create_dir_all(dir.join("docs")).unwrap();
        std::fs::write(dir.join("docs/more.txt"), "and more").unwrap();
        let (endpoint, provider) = provider(vec![
            reply(
                "",
                &[
                    read_notes(),
                    ("list_dir", json!({"path": "docs"})),
                    ("search", json!({"query": "hello"})),
                    ("read_file", json!({"path": "docs/more.txt"})),
                ],
            ),
            reply("Read them all.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 80);
        // A call refused inside a batch keeps its place like the rest.
        spec.allow = Allow::Only(vec!["read_file".into(), "list_dir".into()]);
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(
            results(&sent[1]),
            [
                "1|hello from the notes",
                "more.txt",
                "`search` is not available to this helper.",
                "1|and more"
            ]
        );
    }

    #[test]
    fn fetches_allowed_for_the_run_go_out_at_once() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("fetches");
        let id = AGENTS + 90;
        let pages = TcpListener::bind("127.0.0.1:0").unwrap();
        let site = format!("http://{}", pages.local_addr().unwrap());
        // What answering a card with "allow for this run" leaves behind, for
        // the site's host.
        let requests = approvals::serve();
        let earlier = ToolCall {
            id: "earlier".into(),
            name: "web_fetch".into(),
            arguments: json!({"url": format!("{site}/earlier")}).to_string(),
        };
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let asked =
            std::thread::spawn(move || approvals::ask(7, id, "helper", &earlier, &ctx, "", true));
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(request.for_run(), Some("127.0.0.1"));
        request.answer(approvals::Decision::ForRun);
        assert!(asked.join().unwrap().is_ok());

        // Both requests are taken before either is answered, so fetches made
        // one after another fail: the first waits out its timeout unanswered.
        std::thread::spawn(move || {
            let mut asked = Vec::new();
            for _ in 0..2 {
                let (mut socket, _) = pages.accept().unwrap();
                let mut head = Vec::new();
                let mut block = [0; 4096];
                while !head.windows(4).any(|w| w == b"\r\n\r\n") {
                    let n = socket.read(&mut block).unwrap();
                    assert_ne!(n, 0, "the request ended early");
                    head.extend_from_slice(&block[..n]);
                }
                let head = String::from_utf8_lossy(&head).into_owned();
                let page = head.split(' ').nth(1).unwrap().trim_start_matches('/');
                asked.push((socket, page.to_owned()));
            }
            for (mut socket, page) in asked {
                let _ = write!(
                    socket,
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
                    page.len()
                );
            }
        });
        let fetch = |page: &str| ("web_fetch", json!({"url": format!("{site}/{page}")}));
        let (endpoint, provider) = provider(vec![
            reply("", &[fetch("first"), fetch("second")]),
            reply("Fetched both.", &[]),
        ]);
        let report = finished(spec(endpoint, &dir, id));
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        // Each page comes back fenced, as anything from the web does.
        let got = results(&sent[1]);
        assert_eq!(got.len(), 2);
        assert!(
            got[0].contains(&format!("{site}/first\n\nfirst")),
            "{got:?}"
        );
        assert!(
            got[1].contains(&format!("{site}/second\n\nsecond")),
            "{got:?}"
        );
        assert!(
            got.iter()
                .all(|page| page.starts_with("<untrusted-content-"))
        );
    }

    #[test]
    fn a_scheduled_run_decides_in_its_schedules_mode_and_asks_no_questions() {
        use crate::gate::Mode;
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for (n, mode) in [Mode::Bypass, Mode::Default].into_iter().enumerate() {
            let dir = scratch(&format!("scheduled-{}", mode.name()));
            let session = AGENTS + 300 + n as u64;
            let _listed = crate::schedule::Listed::new(session, &job(&dir, mode));
            // A frontend serves the approvals, and turns down what reaches it.
            let requests = approvals::serve();
            let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
            let heard = asked.clone();
            std::thread::spawn(move || {
                for request in requests {
                    heard.lock().unwrap().push(request.root);
                    request.answer(approvals::Decision::Deny(String::new()));
                }
            });
            let (endpoint, provider) = provider(vec![
                reply(
                    "",
                    &[
                        ("write_file", json!({"path": "made.txt", "content": "made"})),
                        ("ask_user", json!({"question": "Which one?"})),
                        ("wake_me", json!({"in": "1h"})),
                    ],
                ),
                reply("Done.", &[]),
            ]);
            let report = finished(scheduled(endpoint, &dir, session, mode));
            assert_eq!(report.status, Outcome::Done);
            let sent = provider.sent();
            let got = results(&sent[1]);
            assert_eq!(got[1], NOBODY_TO_ASK);
            assert_eq!(got[2], "`wake_me` is not available in a scheduled run.");

            // Bypass runs the write; Default refuses it, as nobody is there
            // to say yes. Neither asks a frontend.
            assert!(asked.lock().unwrap().is_empty(), "a frontend was asked");
            if mode == Mode::Bypass {
                assert!(dir.join("made.txt").exists(), "{got:?}");
            } else {
                assert!(!dir.join("made.txt").exists());
                assert!(got[0].contains("nobody is here"), "{got:?}");
            }
        }
    }

    /// Answers yes to every request of run `run`, as `-p --yes` once did,
    /// counting them; the rest are dropped, which declines them.
    fn yes_to_all(run: u64) -> Arc<AtomicUsize> {
        let requests = approvals::serve();
        let asked = Arc::new(AtomicUsize::new(0));
        let counted = asked.clone();
        std::thread::spawn(move || {
            for request in requests.iter().filter(|request| request.run == run) {
                counted.fetch_add(1, Ordering::Relaxed);
                request.answer(approvals::Decision::Once);
            }
        });
        asked
    }

    #[test]
    fn a_deny_rule_holds_for_a_tool_that_needs_no_yes() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        for yes in [false, true] {
            let dir = scratch(&format!("denied-read-{yes}"));
            std::fs::create_dir_all(dir.join(".letronna")).unwrap();
            let rule = json!([{"tool": "read_file", "pattern": "notes.txt", "effect": "deny"}]);
            std::fs::write(dir.join(permissions::PROJECT_FILE), rule.to_string()).unwrap();
            let id = AGENTS + 200 + u64::from(yes);
            // Nobody serving declines every request, as `-p` without a
            // keyboard or --yes does.
            let asked = if yes {
                yes_to_all(id)
            } else {
                drop(approvals::serve());
                Arc::default()
            };
            let (endpoint, provider) = provider(vec![
                reply("", &[read_notes()]),
                reply("The notes are closed to me.", &[]),
            ]);
            let report = finished(spec(endpoint, &dir, id));
            assert_eq!(report.status, Outcome::Done);
            let sent = provider.sent();

            let got = results(&sent[1]);
            assert!(
                got[0]
                    .starts_with("Refused: a permission rule denies `read_file` for `notes.txt`."),
                "{got:?}"
            );
            assert_eq!(asked.load(Ordering::Relaxed), 0);
        }
    }

    #[test]
    fn a_hardline_command_is_refused_though_every_request_gets_a_yes() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("hardline");
        let id = AGENTS + 210;
        let asked = yes_to_all(id);
        // Named for the floor alone: no such program exists to run.
        let wipe = (
            "run_command",
            json!({"command": "mkfs.letronna-test disk.img"}),
        );
        let (endpoint, provider) = provider(vec![
            reply("", &[wipe]),
            reply("That is not mine to run.", &[]),
        ]);
        let report = finished(spec(endpoint, &dir, id));
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        let got = results(&sent[1]);
        assert!(
            got[0].starts_with("Refused: this command formats or wipes a disk."),
            "{got:?}"
        );
        assert_eq!(asked.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn a_fetch_the_projects_rule_denies_asks_despite_the_runs_grant() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("fetch-denied");
        std::fs::create_dir_all(dir.join(".letronna")).unwrap();
        std::fs::write(
            dir.join(".letronna/permissions.json"),
            r#"[{"tool": "web_fetch", "effect": "deny"}]"#,
        )
        .unwrap();
        let id = AGENTS + 95;
        let requests = approvals::serve();
        let earlier = ToolCall {
            id: "earlier".into(),
            name: "web_fetch".into(),
            arguments: "{}".into(),
        };
        let asked = std::thread::spawn(move || {
            approvals::ask(7, id, "helper", &earlier, &ToolContext::default(), "", true)
        });
        let request = requests.recv_timeout(WAIT).unwrap();
        request.answer(approvals::Decision::ForRun);
        assert!(asked.join().unwrap().is_ok());

        // Nothing listens there, so a fetch that went out unasked would
        // come back as an error and end the run without a request.
        let fetch = ("web_fetch", json!({"url": "http://127.0.0.1:9/secret"}));
        let (endpoint, provider) = provider(vec![
            reply("", &[fetch.clone(), fetch]),
            reply("Left it.", &[]),
        ]);
        let run = spec(endpoint, &dir, id);
        let (tx, report) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(finished(run));
        });
        // The gate refuses it by the project's rule, asking nobody.
        assert_eq!(report.recv_timeout(WAIT).unwrap().status, Outcome::Done);
        assert!(requests.try_recv().is_err());
        let sent = provider.sent();
        let refused = results(&sent[1]);
        assert_eq!(refused.len(), 2);
        assert!(refused.iter().all(|r| r.contains("denies")), "{refused:?}");
    }

    #[test]
    fn a_round_turned_away_with_a_429_is_asked_again_after_the_rest() {
        let dir = scratch("throttled");
        let (server, endpoint) = listen();
        let address = server.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let (mut first, _) = server.accept().unwrap();
            let asked = request(&mut first);
            // A rest of nothing, so the test does not sit out a real one.
            write!(first, "HTTP/1.1 429 Too Many Requests\r\nRetry-After: 0\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").unwrap();
            let (mut second, _) = server.accept().unwrap();
            let asked_again = request(&mut second);
            answer(&mut second, &reply("Done after the rest.", &[]));
            vec![asked, asked_again]
        });
        let provider = Stub { address, worker };
        let report = finished(spec(endpoint.clone(), &dir, AGENTS + 80));
        assert_eq!(report.status, Outcome::Done);
        provider.sent();

        assert_eq!(report.text, "Done after the rest.");
        assert_eq!(limits::gate(&endpoint).throttled(), 1);
    }

    /// Agents the bench runs at once, as the performance budget has them.
    const BENCH_AGENTS: usize = 64;
    /// Tool rounds each bench agent works through before it answers.
    const BENCH_ROUNDS: usize = 5;
    /// How long the bench's provider takes over each token it streams, as the
    /// budget has it.
    const TOKEN: Duration = Duration::from_millis(50);
    /// Streams the background gets on a local server: it takes two, and one
    /// is kept for the foreground.
    const LOCAL_STREAMS: usize = 1;
    /// How long the bench watches waiting agents for wake-ups, as the budget
    /// says to.
    const QUIET: Duration = Duration::from_secs(5);
    /// Threads the budget allows beyond agents, streams and tools, for
    /// reqwest's runtime, the timer thread and the like.
    const SPARE_THREADS: usize = 4;

    /// What the bench's provider saw.
    #[derive(Default)]
    struct Seen {
        /// Requests so far.
        taken: AtomicUsize,
        /// Streams open now.
        open: AtomicUsize,
        /// The most streams ever open at once.
        most: AtomicUsize,
        /// Nanoseconds spent answering, from each request read to its last line.
        served: AtomicU64,
    }

    /// A provider that takes each request on a thread of its own, as a real
    /// one takes streams side by side, so only the gate keeps them apart.
    /// `plan` gets the request's number and body and gives the reply; or it
    /// keeps the caller waiting, as a model still thinking does, and gives
    /// None to hang up.
    fn paced(
        plan: impl Fn(usize, &Value) -> Option<String> + Send + Sync + 'static,
    ) -> (String, Arc<Seen>) {
        let (server, endpoint) = listen();
        let seen = Arc::new(Seen::default());
        let (plan, counts) = (Arc::new(plan), seen.clone());
        std::thread::spawn(move || {
            for socket in server.incoming() {
                let Ok(mut socket) = socket else {
                    continue;
                };
                let n = counts.taken.fetch_add(1, Ordering::SeqCst);
                // Open from here, where its caller already holds a permit.
                let open = counts.open.fetch_add(1, Ordering::SeqCst) + 1;
                counts.most.fetch_max(open, Ordering::SeqCst);
                let (plan, counts) = (plan.clone(), counts.clone());
                std::thread::spawn(move || {
                    // Each event goes when written, not when the last is acknowledged.
                    let _ = socket.set_nodelay(true);
                    let body = request(&mut socket);
                    let began = Instant::now();
                    trickle(&mut socket, plan(n, &body).as_deref(), &counts.open);
                    let served = began.elapsed().as_nanos() as u64;
                    counts.served.fetch_add(served, Ordering::SeqCst);
                });
            }
        });
        (endpoint, seen)
    }

    /// Sends `reply` with `TOKEN` before each event that carries a token, and
    /// counts the stream as closed before its usage and [DONE] lines, after
    /// which its caller can give its permit back.
    fn trickle(socket: &mut TcpStream, reply: Option<&str>, open: &AtomicUsize) {
        let Some(reply) = reply else {
            open.fetch_sub(1, Ordering::SeqCst);
            return;
        };
        let _ = write!(
            socket,
            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            reply.len()
        );
        let events: Vec<&str> = reply.split_inclusive("\n\n").collect();
        // Every reply ends with its usage and [DONE], which carry no token.
        let (tokens, end) = events.split_at(events.len().saturating_sub(2));
        for event in tokens {
            std::thread::sleep(TOKEN);
            let _ = socket.write_all(event.as_bytes());
        }
        open.fetch_sub(1, Ordering::SeqCst);
        let _ = socket.write_all(end.concat().as_bytes());
    }

    /// The bench's agents against `endpoint`, each with a transcript of its own in `dir`.
    fn many(endpoint: &str, dir: &Path, first: AgentId) -> Vec<Spec> {
        (0..BENCH_AGENTS as u64)
            .map(|n| {
                let mut spec = spec(endpoint.to_owned(), dir, first + n);
                spec.transcript.path = dir.join(format!("{n}.jsonl"));
                // Written once already, as `Transcript::create` leaves a helper's.
                save(&mut spec.transcript).unwrap();
                spec
            })
            .collect()
    }

    /// Runs each agent on a thread of its own, as `spawn` does, and says when
    /// each ended, with its place in `specs` and its report.
    fn start_all(specs: Vec<Spec>) -> mpsc::Receiver<(usize, Report, Instant)> {
        let (done, ended) = mpsc::channel();
        for (n, spec) in specs.into_iter().enumerate() {
            let done = done.clone();
            std::thread::spawn(move || {
                let report = run(spec);
                let _ = done.send((n, report, Instant::now()));
            });
        }
        ended
    }

    /// What one look at the process found: its threads, their context
    /// switches and its seconds of processor time.
    type Look = (usize, u64, f64);

    /// What the whole process uses at one moment.
    #[derive(Clone, Copy)]
    struct Usage {
        threads: usize,
        /// Context switches its threads have made: one at least for every wake-up.
        switches: u64,
        /// Processor time, in seconds.
        cpu: f64,
    }

    impl Usage {
        /// What `look` found, or None where the bench cannot look without a
        /// new dependency; where it can, a look that failed fails the bench.
        fn looked(look: Option<Look>) -> Option<Self> {
            assert!(
                look.is_some() || cfg!(not(any(target_os = "linux", target_os = "macos"))),
                "could not read the process's threads, switches and processor time"
            );
            let (threads, switches, cpu) = look?;
            Some(Self {
                threads,
                switches,
                cpu,
            })
        }
    }

    /// What the process uses now.
    fn usage() -> Option<Usage> {
        Usage::looked(measured())
    }

    /// What the process uses at the start and at the end of `spell`, which
    /// this thread sleeps through.
    fn across(spell: Duration) -> Option<(Usage, Usage)> {
        let (first, last) = measured_across(spell);
        Some((Usage::looked(first)?, Usage::looked(last)?))
    }

    /// Reads /proc, where each thread keeps its own switches, leaving out the
    /// looking thread's, which are the bench's and not the agents'.
    #[cfg(target_os = "linux")]
    fn measured() -> Option<Look> {
        let looking = std::fs::read_link("/proc/thread-self").ok()?;
        let tasks: Vec<std::path::PathBuf> = std::fs::read_dir("/proc/self/task")
            .ok()?
            .filter_map(|task| Some(task.ok()?.path()))
            .collect();
        // Only live threads count theirs, which is enough for a quiet spell,
        // since none ends during one.
        let switches = tasks
            .iter()
            .filter(|task| task.file_name() != looking.file_name())
            .filter_map(|task| std::fs::read_to_string(task.join("status")).ok())
            .map(|status| {
                status
                    .lines()
                    .filter(|line| line.contains("ctxt_switches"))
                    .filter_map(|line| line.split_whitespace().next_back()?.parse::<u64>().ok())
                    .sum::<u64>()
            })
            .sum();
        // utime and stime, in the hundredths of a second Linux counts in.
        let stat = std::fs::read_to_string("/proc/self/stat").ok()?;
        let fields: Vec<&str> = stat.rsplit_once(')')?.1.split_whitespace().collect();
        let ticks = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
        Some((tasks.len(), switches, ticks as f64 / 100.0))
    }

    /// Looks before and after sleeping through `spell`.
    #[cfg(not(target_os = "macos"))]
    fn measured_across(spell: Duration) -> (Option<Look>, Option<Look>) {
        let first = measured();
        std::thread::sleep(spell);
        (first, measured())
    }

    /// Asks top, which reports a Mac process's threads, switches and time
    /// together without a library.
    #[cfg(target_os = "macos")]
    fn measured() -> Option<Look> {
        top(1, Duration::ZERO)?.pop()
    }

    /// Has top take both looks, so this thread sleeps through `spell` rather
    /// than waking to look: a look of its own makes more switches than the
    /// agents may in the whole spell.
    #[cfg(target_os = "macos")]
    fn measured_across(spell: Duration) -> (Option<Look>, Option<Look>) {
        let looks = top(2, spell).unwrap_or_default();
        (looks.first().copied(), looks.get(1).copied())
    }

    /// Runs top for `looks` looks, `apart` apart, and reads each one's row.
    #[cfg(target_os = "macos")]
    fn top(looks: usize, apart: Duration) -> Option<Vec<Look>> {
        let (looks, apart) = (looks.to_string(), apart.as_secs().to_string());
        let pid = std::process::id().to_string();
        let out = std::process::Command::new("top")
            .args(["-l", &looks, "-s", &apart, "-pid", &pid])
            .args(["-stats", "th,csw,time"])
            .output()
            .ok()?;
        let text = String::from_utf8(out.stdout).ok()?;
        let lines: Vec<&str> = text.lines().collect();
        // Each look's row comes under its own heading.
        lines
            .windows(2)
            .filter(|pair| pair[0].starts_with("#TH"))
            .map(|pair| {
                // A later look marks with + or - what changed since the last.
                let mut row = pair[1]
                    .split_whitespace()
                    .map(|field| field.trim_end_matches(['+', '-']));
                // "97", or "97/2" while two of them run.
                let threads = row.next()?.split('/').next()?.parse().ok()?;
                let switches = row.next()?.parse().ok()?;
                // "mm:ss.hh", or "hh:mm:ss" past an hour.
                let cpu = row.next()?.split(':').try_fold(0.0, |total, part| {
                    Some(total * 60.0 + part.parse::<f64>().ok()?)
                })?;
                Some((threads, switches, cpu))
            })
            .collect()
    }

    /// Nothing to read here without a new dependency, so the bench skips what
    /// needs it.
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    fn measured() -> Option<Look> {
        None
    }

    /// What the process uses once `threads` or fewer are left, giving threads
    /// on their way out a second to go.
    fn settled(threads: usize) -> Option<Usage> {
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let now = usage()?;
            if now.threads <= threads || Instant::now() > deadline {
                return Some(now);
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The middle and the longest of `times`.
    fn spread(mut times: Vec<Duration>) -> (Duration, Duration) {
        times.sort();
        let middle = times.get(times.len() / 2).copied().unwrap_or_default();
        (middle, times.last().copied().unwrap_or_default())
    }

    /// Every agent reads a file five times and answers, one token a reply,
    /// all in line for one local provider.
    fn busy_agents_share_one_provider() {
        let dir = scratch("bench");
        let (endpoint, seen) = paced(|_, body| {
            let replied = messages(body)
                .iter()
                .filter(|message| message["role"] == "assistant")
                .count();
            Some(if replied < BENCH_ROUNDS {
                reply("", &[read_notes()])
            } else {
                reply("The notes say hello.", &[])
            })
        });
        let calls = BENCH_AGENTS * (BENCH_ROUNDS + 1);
        // What the tokens alone take: one a call, each stream one call at a time.
        let tokens = TOKEN * (calls / LOCAL_STREAMS) as u32;
        let before = usage();
        let started = Instant::now();
        let ended = start_all(many(&endpoint, &dir, AGENTS + 1_000));
        let (mut finished, mut last, mut peak) = (0, started, 0);
        while finished < BENCH_AGENTS {
            match ended.recv_timeout(Duration::from_millis(100)) {
                Ok((_, report, at)) => {
                    assert_eq!(
                        (report.status, report.rounds, report.text.as_str()),
                        (Outcome::Done, BENCH_ROUNDS, "The notes say hello.")
                    );
                    (finished, last) = (finished + 1, at);
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Only a hang takes ten times its tokens, even on a busy machine.
                    let taken = started.elapsed();
                    assert!(
                        taken < tokens * 10,
                        "the agents were still at it after {taken:?}"
                    );
                    peak = usage().map_or(peak, |now| peak.max(now.threads));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => panic!("an agent's thread died"),
            }
        }
        let wall = last - started;
        let most = seen.most.load(Ordering::SeqCst);
        // What the provider took, oversleeping on a busy machine included, is
        // not the driver's; the rest of the wall time is.
        let served = Duration::from_nanos(seen.served.load(Ordering::SeqCst));
        let overhead = wall.saturating_sub(served / LOCAL_STREAMS as u32) / calls as u32;
        println!(
            "{BENCH_AGENTS} agents x {BENCH_ROUNDS} tool rounds, a token every {TOKEN:?}, {LOCAL_STREAMS} background stream:"
        );
        println!(
            "  {calls} calls in {wall:.1?}, the provider's {served:.1?} of it ({tokens:.1?} asked for): {overhead:.2?} added a call; at most {most} stream open"
        );
        assert_eq!(
            seen.taken.load(Ordering::SeqCst),
            calls,
            "a round was asked twice"
        );
        assert!(most <= LOCAL_STREAMS, "{most} streams open at once");
        // Under a tenth of a token a call, so the wall time is rounds times
        // latency times agents over streams, as the budget has it.
        assert!(
            overhead < TOKEN / 10,
            "the driver added {overhead:?} to each call"
        );
        let Some(before) = before else {
            println!("  threads and processor time: not measured on this system");
            return;
        };
        // reqwest's runtime, which the first call starts, stays; a thread of
        // an agent that has ended does not.
        let after = settled(before.threads + 1).expect("the process was looked at before");
        let added = peak.saturating_sub(before.threads);
        let left = after.threads.saturating_sub(before.threads);
        // At most a tool thread for each agent: read_file's end too soon to count.
        let ceiling = BENCH_AGENTS + LOCAL_STREAMS + BENCH_AGENTS + SPARE_THREADS;
        let cpu = after.cpu - before.cpu;
        println!(
            "  threads: {added} the agents' at the most seen (ceiling {ceiling}), {left} left once all were done; cpu {cpu:.2} s, {:.2} ms a call, the provider's and looking's included",
            cpu * 1000.0 / calls as f64
        );
        assert!(
            added <= ceiling,
            "the agents had {added} threads, over {ceiling}"
        );
        assert!(left <= 1, "{left} threads outlived the agents");
    }

    /// Half the agents sleep in a tool, one waits on a model that is still
    /// thinking and the rest wait in line behind it: nothing streams, nothing
    /// should wake, a stop should reach each at once, and once stopped none
    /// should keep a thread.
    fn waiting_agents_cost_nothing_and_stop_at_once() {
        let dir = scratch("bench-waiting");
        let in_tools = BENCH_AGENTS / 2;
        let (release, released) = async_channel::bounded::<()>(1);
        let (holding, held) = mpsc::channel();
        let (endpoint, seen) = paced(move |n, _| {
            if n < in_tools {
                return Some(reply("", &[("wait", json!({"seconds": 120}))]));
            }
            let _ = holding.send(n);
            let _ = released.recv_blocking();
            None
        });
        let specs = many(&endpoint, &dir, AGENTS + 2_000);
        let stops: Vec<Cancel> = specs.iter().map(|spec| spec.cancel.clone()).collect();
        let before = usage();
        let ended = start_all(specs);
        held.recv_timeout(WAIT).expect("the model was never asked");
        // Time for the last agent answered to start its tool.
        std::thread::sleep(Duration::from_millis(200));
        if let (Some(before), Some((quiet, after))) = (before, across(QUIET)) {
            let added = quiet.threads.saturating_sub(before.threads);
            // A driver for each agent, a thread for each tool and the stream;
            // the stub's thread holding it is one of the spares.
            let least = BENCH_AGENTS + in_tools + LOCAL_STREAMS;
            let ceiling = least + SPARE_THREADS;
            let woken = after.switches.saturating_sub(quiet.switches);
            let cpu = after.cpu - quiet.cpu;
            println!(
                "  waiting {QUIET:?}: {added} threads the agents' ({least} to {ceiling}), {woken} context switches, cpu {cpu:.2} s"
            );
            // Fewer, and some agents were not where the bench put them.
            assert!(
                (least..=ceiling).contains(&added),
                "the agents had {added} threads, not {least} to {ceiling}"
            );
            // Under one a second: one thread polling once a second makes five,
            // and an agent polling at all many more.
            assert!(
                woken < QUIET.as_secs(),
                "{woken} wake-ups while nothing happened"
            );
            assert!(
                cpu < 0.1,
                "{cpu:.2} s of processor time while nothing happened"
            );
        } else {
            println!("  threads, wake-ups and processor time: not measured on this system");
        }

        let (mut in_a_tool, mut waiting) = (Vec::new(), Vec::new());
        for (n, stop) in stops.iter().enumerate() {
            let asked = Instant::now();
            stop.set();
            let (which, report, at) = ended.recv_timeout(WAIT).expect("a stop never landed");
            assert_eq!((which, report.status), (n, Outcome::Stopped));
            let took = at.saturating_duration_since(asked);
            if report.rounds == 0 {
                waiting.push(took);
                continue;
            }
            // Stopped in its wait, not in line after a wait that ended early.
            let last = written(&dir.join(format!("{n}.jsonl"))).pop().unwrap();
            assert_eq!(
                (last["name"].as_str(), last["is_error"].as_bool()),
                (Some("wait"), Some(true)),
                "agent {n} was not in its wait"
            );
            in_a_tool.push(took);
        }
        drop(release);
        // The held stream gives its permit back once it has hung up, and after
        // anything it sent again, so by then every request has come.
        let gate = limits::gate(&endpoint);
        let (freed, free) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = freed.send(gate.acquire(&Cancel::default()).is_some());
        });
        assert_eq!(
            free.recv_timeout(WAIT),
            Ok(true),
            "the held stream kept its permit"
        );
        assert_eq!(
            in_a_tool.len(),
            in_tools,
            "the agents were not where the bench put them"
        );
        assert_eq!(
            seen.taken.load(Ordering::SeqCst),
            in_tools + 1,
            "an agent asked the model while it waited or after it stopped"
        );
        let ((model, slowest_model), (tool, slowest_tool)) = (spread(waiting), spread(in_a_tool));
        println!(
            "  stops, one at a time: waiting for the model {model:.2?} at the median (slowest {slowest_model:.2?}); in a tool, which saves the stopped call first, {tool:.2?} (slowest {slowest_tool:.2?})"
        );
        // A stop is the unblocking, and in a tool one short write besides.
        assert!(
            model.max(tool) < Duration::from_millis(1),
            "stops took {model:?} and {tool:?} at the median"
        );
        // A stop that waited for a poll would take its interval.
        let slowest = slowest_model.max(slowest_tool);
        assert!(
            slowest < Duration::from_millis(10),
            "a stop took {slowest:?}"
        );
        let Some(before) = before else {
            return;
        };
        let after = settled(before.threads).expect("the process was looked at before");
        let left = after.threads.saturating_sub(before.threads);
        println!("  {left} threads left once all were stopped");
        assert_eq!(left, 0, "{left} threads outlived the stopped agents");
    }

    #[test]
    #[ignore = "bench: AGENT_DATA_DIR=<an empty folder> cargo test -p letronna-core --release -- --ignored many_agents_keep_to_the_performance_budget --nocapture"]
    fn many_agents_keep_to_the_performance_budget() {
        // The timer thread every app has, waiting on a deadline an hour off,
        // so its wake-ups count too.
        crate::timers::at(Instant::now() + Duration::from_secs(3600), Box::new(|| {}));
        // One after the other, since each measures the whole process. Forks,
        // whose cached share the budget also promises, do not exist yet.
        busy_agents_share_one_provider();
        waiting_agents_cost_nothing_and_stop_at_once();
    }

    /// The shape of a vote on a claim: whether it holds, and why.
    fn vote() -> Value {
        json!({"type": "object", "required": ["real", "reason"], "properties": {
            "real": {"type": "boolean"},
            "reason": {"type": "string"},
        }})
    }

    fn answering(answer: Value) -> (&'static str, Value) {
        ("structured_output", answer)
    }

    #[test]
    fn a_good_answer_ends_the_run_with_the_checked_json() {
        let dir = scratch("answer");
        let given = json!({"real": true, "reason": "the notes say hello"});
        // Two replies only: a third call would find nobody and fail the run.
        let (endpoint, provider) = provider(vec![
            reply("", &[read_notes()]),
            reply("", &[answering(given.clone())]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 110);
        spec.schema = Some(vote());
        // The answer is taken whatever the list names.
        spec.allow = Allow::Only(vec!["read_file".into()]);
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(report.value, Some(given));
        let offered = sent[0]["tools"].as_array().unwrap().last().unwrap();
        assert_eq!(offered["function"]["name"], "structured_output");
        assert_eq!(offered["function"]["parameters"], vote());
        // Answered on the transcript, so a resume finds no call left hanging.
        let last = written(&dir.join("helper.jsonl")).pop().unwrap();
        assert_eq!(
            (last["name"].as_str(), last["content"].as_str()),
            (Some("structured_output"), Some("Answer received."))
        );
    }

    #[test]
    fn a_sessions_own_gate_lets_the_answer_through() {
        let dir = scratch("answer-gate");
        let given = json!({"real": true, "reason": "the notes say hello"});
        let (endpoint, _provider) = provider(vec![reply("", &[answering(given.clone())])]);
        let mut spec = spec(endpoint, &dir, AGENTS + 111);
        spec.schema = Some(vote());
        // A gate that runs nothing, as Auto mode's would refuse a tool it
        // does not know: the answer is not a tool, so it never meets it.
        spec.decide = Some(Arc::new(|_, _| {
            crate::gate::Outcome::Refuse("Refused: nothing runs.".into())
        }));
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.value, Some(given));
    }

    #[test]
    fn a_bad_answer_gets_two_corrections_then_null() {
        let dir = scratch("corrections");
        // Three replies only: a fourth call would fail the run another way.
        let (endpoint, provider) = provider(vec![
            reply("", &[answering(json!({"real": "yes"}))]),
            reply("", &[answering(json!({"real": true}))]),
            reply("", &[answering(json!({"real": true, "reason": 7}))]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 120);
        spec.schema = Some(vote());
        let report = finished(spec);
        let sent = provider.sent();

        assert_eq!(report.value, None);
        assert_eq!(
            report.status,
            Outcome::Failed(
                "Its answer still did not fit the schema after 2 corrections:\nreason: expected string"
                    .into()
            )
        );
        // Each correction carries what was wrong and nothing more.
        let problems = ["reason: missing\nreal: expected boolean", "reason: missing"];
        for (request, problems) in sent[1..].iter().zip(problems) {
            let last = messages(request).last().unwrap();
            assert_eq!(
                (last["role"].as_str(), last["content"].as_str()),
                (Some("tool"), Some(problems))
            );
        }
    }

    #[test]
    fn a_helper_that_never_calls_it_is_nudged_once() {
        let dir = scratch("reminded");
        let (endpoint, provider) = provider(vec![
            reply("It is real: the notes say hello.", &[]),
            reply("As I said, it is real.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 130);
        spec.schema = Some(vote());
        let report = finished(spec);
        let sent = provider.sent();

        assert_eq!(report.value, None);
        assert_eq!(
            report.status,
            Outcome::Failed(
                "It ended without giving its answer through structured_output, even when reminded."
                    .into()
            )
        );
        assert_eq!(report.text, "As I said, it is real.");
        let reminder = messages(&sent[1]).last().unwrap();
        assert_eq!(reminder["role"], "user");
        let reminder = reminder["content"].as_str().unwrap();
        assert!(
            reminder.starts_with("[Letronna notice") && reminder.contains(REMINDER),
            "{reminder}"
        );
    }

    #[test]
    fn an_answer_that_is_not_an_object_comes_in_an_answer_field() {
        let dir = scratch("wrapped");
        let files = json!({"type": "array", "items": {"type": "string"}});
        let (endpoint, provider) = provider(vec![reply(
            "",
            &[answering(json!({"answer": ["notes.txt"]}))],
        )]);
        let mut spec = spec(endpoint, &dir, AGENTS + 140);
        spec.schema = Some(files.clone());
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(report.value, Some(json!(["notes.txt"])));
        // Providers take only an object as a tool's parameters.
        let offered = sent[0]["tools"].as_array().unwrap().last().unwrap();
        let parameters = &offered["function"]["parameters"];
        assert_eq!(parameters["type"], "object");
        assert_eq!(parameters["properties"]["answer"], files);
    }

    #[test]
    fn a_schema_it_cannot_check_fails_before_any_call() {
        let dir = scratch("unchecked");
        let (endpoint, provider) = provider(Vec::new());
        let mut spec = spec(endpoint, &dir, AGENTS + 150);
        spec.schema = Some(json!({"type": "integer", "minimum": 1}));
        let report = finished(spec);
        assert!(provider.sent().is_empty());

        assert_eq!(
            report.status,
            Outcome::Failed(
                "The answer's schema cannot be checked: unsupported keyword \"minimum\"".into()
            )
        );
    }

    #[test]
    fn a_model_without_tools_fails_before_any_call() {
        let dir = scratch("toolless");
        let (endpoint, provider) = provider(Vec::new());
        let mut spec = spec(endpoint, &dir, AGENTS + 160);
        spec.schema = Some(vote());
        spec.connection.tools = false;
        let report = finished(spec);
        assert!(provider.sent().is_empty());

        assert_eq!(
            report.status,
            Outcome::Failed(
                "The model runs without tools here, so it cannot give its answer through structured_output."
                    .into()
            )
        );
    }

    #[test]
    fn an_answer_at_the_round_limit_is_still_taken() {
        let dir = scratch("limit");
        let given = json!({"real": true, "reason": "the notes say hello"});
        let (endpoint, provider) = provider(vec![
            reply("", &[read_notes()]),
            reply("", &[answering(given.clone())]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 170);
        spec.schema = Some(vote());
        spec.rounds = 1;
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        provider.sent();

        assert_eq!((report.value, report.rounds), (Some(given), 1));
        let last = written(&dir.join("helper.jsonl")).pop().unwrap();
        assert_eq!(
            (last["name"].as_str(), last["content"].as_str()),
            (Some("structured_output"), Some("Answer received."))
        );
    }

    #[test]
    fn an_answer_cut_off_is_asked_for_again_whole() {
        let dir = scratch("cut-off");
        let given = json!({"real": true, "reason": "the notes say hello"});
        let cut = json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "call-0",
            "function": {"name": "structured_output",
                "arguments": r#"{"real": true, "reason": "the no"#}}]}}]});
        let (endpoint, provider) = provider(vec![
            format!("data: {cut}\n\ndata: [DONE]\n\n"),
            reply("", &[answering(given.clone())]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 180);
        spec.schema = Some(vote());
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(report.value, Some(given));
        assert_eq!(results(&sent[1]), [CUT_OFF]);
    }

    #[test]
    fn mail_in_the_answers_round_goes_to_the_owner() {
        let _bell = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("answer-mail");
        let (endpoint, provider) = provider(vec![reply(
            "",
            &[answering(json!({"real": true, "reason": "it says hello"}))],
        )]);
        let id = AGENTS + 190;
        let mut spec = spec(endpoint, &dir, id);
        spec.schema = Some(vote());
        inbox::push(id, notice("main", When::Next, "also check the tests"));
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        provider.sent();

        // Not folded into a transcript the finished run would never read on.
        let forwarded = inbox::take(id + 1, true);
        let texts: Vec<&str> = forwarded.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(
            texts,
            ["helper ended before it read this, from main:\nalso check the tests"]
        );
    }

    #[test]
    fn a_run_that_repeats_one_call_stops_at_the_fifth_saying_why() {
        let dir = scratch("loops");
        std::fs::create_dir(dir.join("empty")).unwrap();
        let look = ("list_dir", json!({"path": "empty"}));
        let (endpoint, provider) = provider(vec![reply("", &[look]); 5]);
        let report = finished(spec(endpoint, &dir, AGENTS + 90));
        let sent = provider.sent();
        assert_eq!(
            report.status,
            Outcome::Failed(
                "Stopped going round in circles: it called `list_dir` 5 times with the same \
                 arguments and got the same result each time."
                    .into()
            )
        );
        assert_eq!(report.rounds, 5);
        // The model was told before it was stopped.
        let told = results(&sent[3]);
        assert!(!told[1].contains("Change your approach"));
        assert!(told[2].contains("made this exact call 3 times"));
    }

    #[test]
    fn an_answer_given_with_the_fifth_repeat_is_still_taken() {
        let dir = scratch("loops-answer");
        std::fs::create_dir(dir.join("empty")).unwrap();
        let look = ("list_dir", json!({"path": "empty"}));
        let given = json!({"real": true, "reason": "the folder is empty"});
        let mut replies = vec![reply("", std::slice::from_ref(&look)); 4];
        replies.push(reply("", &[look, answering(given.clone())]));
        let (endpoint, provider) = provider(replies);
        let mut spec = spec(endpoint, &dir, AGENTS + 91);
        spec.schema = Some(vote());
        let report = finished(spec);
        provider.sent();

        assert_eq!((report.status, report.value), (Outcome::Done, Some(given)));
    }

    /// `provider`, that also answers each request from Auto mode's judge,
    /// which streams nothing: with the code the judge was given and then
    /// `verdict`, or, for `None`, with no code, as a confused judge would.
    fn judged(replies: Vec<String>, verdict: Option<&'static str>) -> (String, Stub) {
        judged_in_turn(replies, vec![verdict])
    }

    /// `judged`, with the judge giving `verdicts` in turn, then the last again.
    fn judged_in_turn(replies: Vec<String>, verdicts: Vec<Option<&'static str>>) -> (String, Stub) {
        let (server, endpoint) = listen();
        let address = server.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let mut replies = replies.into_iter();
            let (mut verdicts, mut verdict) = (verdicts.into_iter(), None);
            let mut sent = Vec::new();
            while replies.len() > 0 {
                let (mut socket, _) = server.accept().unwrap();
                let body = request(&mut socket);
                if body["stream"] == false {
                    verdict = verdicts.next().unwrap_or(verdict);
                    let prompt = body["messages"][1]["content"].as_str().unwrap();
                    let code = prompt.strip_prefix("Code: ").unwrap();
                    let code = code.split_whitespace().next().unwrap();
                    let said =
                        verdict.map_or("ALLOW".into(), |verdict| format!("{code} {verdict}"));
                    let said = json!({"choices": [{"message": {"content": said}}]});
                    answer(&mut socket, &said.to_string());
                } else if let Some(reply) = replies.next() {
                    answer(&mut socket, &reply);
                }
                sent.push(body);
            }
            sent
        });
        (endpoint, Stub { address, worker })
    }

    /// A helper of a session in Auto mode, where the user typed `typed`.
    fn in_auto(endpoint: String, dir: &Path, id: AgentId, typed: &str) -> Spec {
        let mut spec = spec(endpoint, dir, id);
        spec.connection.mode = crate::gate::Mode::Auto;
        spec.connection.human = true;
        spec.connection.typed = vec![typed.to_owned()].into();
        spec
    }

    fn command(line: &str) -> (&'static str, Value) {
        ("run_command", json!({"command": line}))
    }

    #[test]
    fn a_helper_in_auto_runs_what_the_judge_allows_on_what_the_user_typed() {
        let dir = scratch("judge-allows");
        let (endpoint, provider) = judged(
            vec![reply("", &[command("echo judged")]), reply("Ran it.", &[])],
            Some("ALLOW"),
        );
        let mut spec = in_auto(endpoint, &dir, AGENTS + 200, "Say judged in the shell.");
        // A helper given a weaker model than the session's own.
        spec.connection.model = "weak-model".into();
        spec.connection.session_model = "session-model".into();
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        // The model, its judge on the session's model, the model again.
        assert_eq!(sent.len(), 3);
        assert_eq!(
            [&sent[0]["model"], &sent[1]["model"]],
            ["weak-model", "session-model"]
        );
        let prompt = sent[1]["messages"][1]["content"].as_str().unwrap();
        assert!(prompt.contains("Say judged in the shell."), "{prompt}");
        // The helper's task is not the user's word.
        assert!(!prompt.contains("Read notes.txt"), "{prompt}");
        let ran = results(&sent[2]);
        assert!(ran[0].contains("judged"), "{ran:?}");
    }

    #[test]
    fn a_helper_in_auto_asks_with_the_judges_doubt_or_refuses_with_nobody_there() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let requests = approvals::serve();
        let dir = scratch("judge-unsure");
        let replies = || vec![reply("", &[command("echo unsure")]), reply("Gave up.", &[])];
        let (endpoint, provider) = judged(replies(), None);
        let spec = in_auto(endpoint, &dir, AGENTS + 210, "Say unsure in the shell.");
        let running = spawn(spec, |_| ()).unwrap();
        let request = requests.recv_timeout(WAIT).unwrap();
        assert!(
            request.why.starts_with("auto: unsure — "),
            "{}",
            request.why
        );
        request.answer(approvals::Decision::Deny(String::new()));
        assert_eq!(running.join().unwrap().status, Outcome::Done);
        let sent = provider.sent();
        assert_eq!(results(&sent[2]), [NOT_APPROVED]);

        // With nobody to answer, the judge's doubt is a refusal, and no card.
        let (endpoint, provider) = judged(replies(), None);
        let mut spec = in_auto(endpoint, &dir, AGENTS + 220, "Say unsure in the shell.");
        spec.connection.human = false;
        assert_eq!(finished(spec).status, Outcome::Done);
        let sent = provider.sent();
        let refused = results(&sent[2]);
        assert!(
            refused[0].contains("nobody is here to decide"),
            "{refused:?}"
        );
        assert!(requests.try_recv().is_err(), "a card with nobody there");
    }

    #[test]
    fn a_call_the_user_said_no_to_asks_again_in_the_run_rather_than_being_judged() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let requests = approvals::serve();
        let dir = scratch("judge-denied");
        let retried = || reply("", &[command("echo retried")]);
        // The judge is unsure of the first, and would allow it after.
        let (endpoint, provider) = judged_in_turn(
            vec![retried(), retried(), reply("Gave up.", &[])],
            vec![None, Some("ALLOW")],
        );
        let run = AGENTS + 230;
        let running = spawn(in_auto(endpoint, &dir, run, "Tidy up."), |_| ()).unwrap();
        requests
            .recv_timeout(WAIT)
            .unwrap()
            .answer(approvals::Decision::Deny(String::new()));
        let again = requests.recv_timeout(WAIT).unwrap();
        assert!(again.why.contains("stopped"), "{}", again.why);
        again.answer(approvals::Decision::Deny(String::new()));
        assert_eq!(running.join().unwrap().status, Outcome::Done);
        let sent = provider.sent();
        let judged = sent.iter().filter(|body| body["stream"] == false).count();
        assert_eq!(judged, 1);
        assert_eq!(results(&sent[3]), [NOT_APPROVED, NOT_APPROVED]);

        // Another helper of the run is not judged on it either.
        let (endpoint, provider) =
            judged_in_turn(vec![retried(), reply("Ran it.", &[])], vec![Some("ALLOW")]);
        let mut spec = in_auto(endpoint, &dir, run + 1, "Tidy up.");
        spec.run = run;
        let running = spawn(spec, |_| ()).unwrap();
        requests
            .recv_timeout(WAIT)
            .unwrap()
            .answer(approvals::Decision::Once);
        assert_eq!(running.join().unwrap().status, Outcome::Done);
        let sent = provider.sent();
        assert_eq!(sent.len(), 2, "the judge was asked");
        assert!(results(&sent[1])[0].contains("retried"));
    }

    #[test]
    fn the_floor_rules_protected_paths_and_dont_ask_refuse_a_helper_with_no_card() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let requests = approvals::serve();
        let dir = scratch("refused");
        std::fs::write(dir.join("secret.txt"), "the secret").unwrap();
        std::fs::create_dir_all(dir.join(".letronna")).unwrap();
        let denied = r#"[{"tool": "read_file", "pattern": "secret.txt", "effect": "deny"}]"#;
        std::fs::write(dir.join(permissions::PROJECT_FILE), denied).unwrap();
        let write = |path: &str| ("write_file", json!({"path": path, "content": "x"}));
        let (endpoint, stub) = provider(vec![
            reply(
                "",
                &[
                    command("rm -rf /"),
                    ("read_file", json!({"path": "secret.txt"})),
                    write("notes/new.txt"),
                    write(".git/hooks/pre-commit"),
                ],
            ),
            reply("Done what I could.", &[]),
        ]);
        // Edits in the project run; anything else would ask, and nobody can answer.
        let mut editing = spec(endpoint, &dir, AGENTS + 230);
        editing.connection.mode = crate::gate::Mode::AcceptEdits;
        assert_eq!(finished(editing).status, Outcome::Done);
        let sent = stub.sent();
        let got = results(&sent[1]);
        assert!(got[0].contains("Letronna never runs that"), "{got:?}");
        assert!(got[1].contains("a permission rule denies"), "{got:?}");
        assert!(!got[2].starts_with("Refused"), "{got:?}");
        assert!(dir.join("notes/new.txt").exists());
        assert!(got[3].contains("nobody is here"), "{got:?}");
        assert!(!dir.join(".git/hooks/pre-commit").exists());

        // Don't ask refuses what would ask, with someone there to answer.
        let (endpoint, provider) = provider(vec![
            reply("", &[write("notes/other.txt")]),
            reply("Could not.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 240);
        spec.connection.mode = crate::gate::Mode::DontAsk;
        spec.connection.human = true;
        assert_eq!(finished(spec).status, Outcome::Done);
        let got = results(&provider.sent()[1]).join("");
        assert!(got.contains("Don't ask mode"), "{got}");
        assert!(
            requests.try_recv().is_err(),
            "a card for what the gate settled"
        );
    }

    #[test]
    fn a_yes_for_the_run_covers_its_rule_and_not_the_tools_other_calls() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("for-run");
        let id = AGENTS + 250;
        // What answering a card on `echo ran` with "allow for this run" leaves behind.
        let requests = approvals::serve();
        let earlier = ToolCall {
            id: "earlier".into(),
            name: "run_command".into(),
            arguments: json!({"command": "echo ran"}).to_string(),
        };
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let asked =
            std::thread::spawn(move || approvals::ask(7, id, "helper", &earlier, &ctx, "", true));
        requests
            .recv_timeout(WAIT)
            .unwrap()
            .answer(approvals::Decision::ForRun);
        assert!(asked.join().unwrap().is_ok());

        let (endpoint, provider) = provider(vec![
            reply("", &[command("echo ran again"), command("echo other")]),
            reply("One ran.", &[]),
        ]);
        assert_eq!(finished(spec(endpoint, &dir, id)).status, Outcome::Done);
        let sent = provider.sent();
        let got = results(&sent[1]);
        assert!(words(got[0]).contains("ran again"), "{got:?}");
        assert!(got[1].contains("nobody is here"), "{got:?}");
    }

    #[test]
    fn a_batch_stops_at_a_call_the_gate_would_not_run() {
        let dir = scratch("batch-gate");
        std::fs::write(dir.join("secret.txt"), "the secret").unwrap();
        std::fs::create_dir_all(dir.join(".letronna")).unwrap();
        let denied = r#"[{"tool": "read_file", "pattern": "secret.txt", "effect": "deny"}]"#;
        std::fs::write(dir.join(permissions::PROJECT_FILE), denied).unwrap();
        let (endpoint, provider) = provider(vec![
            reply(
                "",
                &[
                    read_notes(),
                    ("read_file", json!({"path": "secret.txt"})),
                    ("list_dir", json!({"path": "."})),
                ],
            ),
            reply("Read what I may.", &[]),
        ]);
        let report = finished(spec(endpoint, &dir, AGENTS + 260));
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();
        let got = results(&sent[1]);
        assert_eq!(got[0], "1|hello from the notes");
        assert!(got[1].contains("a permission rule denies"), "{got:?}");
        assert!(got[2].contains("notes.txt"), "{got:?}");
    }

    #[test]
    fn a_yes_for_the_run_does_not_last_where_an_ask_rule_asks_every_time() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let requests = approvals::serve();
        let dir = scratch("ask-rule");
        std::fs::create_dir_all(dir.join(".letronna")).unwrap();
        let asks = r#"[{"tool": "run_command", "pattern": "echo asked", "effect": "ask"}]"#;
        std::fs::write(dir.join(permissions::PROJECT_FILE), asks).unwrap();
        let (endpoint, provider) = provider(vec![
            reply("", &[command("echo asked once")]),
            reply("", &[command("echo asked twice")]),
            reply("Asked twice.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 270);
        spec.connection.human = true;
        let running = spawn(spec, |_| ()).unwrap();
        let request = requests.recv_timeout(WAIT).unwrap();
        // So the card offers no yes for the run, and one given anyway is a yes once.
        assert_eq!(request.for_run(), None);
        request.answer(approvals::Decision::ForRun);
        let request = requests.recv_timeout(WAIT).unwrap();
        assert!(request.preview.contains("twice"), "{}", request.preview);
        request.answer(approvals::Decision::Deny(String::new()));
        assert_eq!(running.join().unwrap().status, Outcome::Done);
        let sent = provider.sent();
        assert!(words(results(&sent[1])[0]).contains("asked once"));
        assert_eq!(results(&sent[2])[1], NOT_APPROVED);
    }

    /// A job in `mode` working in `dir`.
    fn job(dir: &Path, mode: gate::Mode) -> crate::schedule::Schedule {
        crate::schedule::Schedule {
            id: 1,
            name: "nightly".into(),
            prompt: "Tidy the notes folder.".into(),
            workspace: Some(dir.to_owned()),
            trigger: crate::schedule::Trigger::Every { minutes: 60 },
            enabled: true,
            last_run: 0,
            mode,
            by_model: false,
        }
    }

    /// Job `id` in `mode`, working in `dir`, as `schedule::run_of` sets its
    /// run up against the provider at `endpoint`.
    fn scheduled(endpoint: String, dir: &Path, id: AgentId, mode: gate::Mode) -> Spec {
        let job = job(dir, mode);
        let connection = Connection {
            endpoint,
            model: "test-model".into(),
            tools: true,
            workspace: Some(dir.to_owned()),
            ..Default::default()
        };
        let transcript = Transcript {
            path: dir.join("scheduled.jsonl"),
            conversation: Conversation {
                id,
                title: job.name.clone(),
                ..Default::default()
            },
        };
        crate::schedule::run_of(&job, connection, transcript)
    }

    /// An edit in the project, a command that changes things and one that
    /// throws work away, as a scheduled run's model makes them.
    fn tidying() -> Vec<String> {
        let calls = [
            ("write_file", json!({"path": "made.txt", "content": "made"})),
            command("mkdir made-dir"),
            command("git reset --hard HEAD~1"),
        ];
        vec![reply("", &calls), reply("Tidied what I could.", &[])]
    }

    /// Whether the edit and the command of `tidying` ran in `dir`, after
    /// checking that the last call was refused: by the floor, or by plan
    /// mode before the floor looked.
    fn tidied(dir: &Path, sent: &[Value]) -> (bool, bool) {
        let results = results(sent.last().unwrap());
        assert_eq!(results.len(), 3, "{results:?}");
        let refused = [
            "Refused: this command",
            "`run_command` in plan mode only runs commands that just read",
        ];
        assert!(
            refused.iter().any(|why| results[2].starts_with(why)),
            "{results:?}"
        );
        (dir.join("made.txt").exists(), dir.join("made-dir").exists())
    }

    #[test]
    fn the_schedule_tool_is_refused_to_a_scheduled_run_and_to_helpers() {
        let dir = scratch("schedule-tool");
        let mut spec = scheduled(String::new(), &dir, AGENTS + 380, gate::Mode::Bypass);
        let _listed = crate::schedule::Listed::new(spec.root, &job(&dir, gate::Mode::Bypass));
        let why = refused(&spec, "schedule");
        assert!(why.is_some_and(|why| why.contains("not available in a scheduled run")));
        spec.depth = 1;
        let why = refused(&spec, "schedule");
        assert!(why.is_some_and(|why| why.contains("not available to this helper")));
    }

    #[test]
    fn a_scheduled_run_runs_what_its_mode_lets_through_and_refuses_the_rest() {
        use gate::Mode;
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let requests = approvals::serve();
        let table = [
            (Mode::Default, (false, false)),
            (Mode::AcceptEdits, (true, false)),
            (Mode::Plan, (false, false)),
            (Mode::DontAsk, (false, false)),
            (Mode::Bypass, (true, true)),
        ];
        for (at, (mode, expected)) in (AGENTS + 400..).step_by(10).zip(table) {
            let dir = scratch(&format!("scheduled-each-{}", mode.name()));
            let (endpoint, provider) = provider(tidying());
            let report = finished(scheduled(endpoint, &dir, at, mode));
            assert_eq!(report.status, Outcome::Done, "{mode:?}");
            assert_eq!(tidied(&dir, &provider.sent()), expected, "{mode:?}");
        }
        assert!(requests.try_recv().is_err(), "a frontend was asked");
    }

    #[test]
    fn a_scheduled_run_in_auto_runs_what_the_judge_allows_on_the_jobs_prompt() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let requests = approvals::serve();
        let dir = scratch("scheduled-auto");
        let (endpoint, provider) = judged(tidying(), Some("ALLOW"));
        let spec = scheduled(endpoint, &dir, AGENTS + 460, gate::Mode::Auto);
        assert_eq!(finished(spec).status, Outcome::Done);
        let sent = provider.sent();

        // The edit ran unjudged, the command on the judge's word, and the
        // floor refused the last before any judge saw it.
        assert_eq!(tidied(&dir, &sent), (true, true));
        let judge: Vec<&Value> = sent.iter().filter(|body| body["stream"] == false).collect();
        assert_eq!(judge.len(), 1);
        assert_eq!(judge[0]["model"], "test-model");
        let prompt = judge[0]["messages"][1]["content"].as_str().unwrap();
        assert!(prompt.contains("Tidy the notes folder."), "{prompt}");
        assert!(requests.try_recv().is_err(), "a frontend was asked");
    }

    #[test]
    fn a_scheduled_run_in_auto_refuses_what_the_judge_is_unsure_of() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let requests = approvals::serve();
        let dir = scratch("scheduled-unsure");
        let (endpoint, provider) = judged(tidying(), None);
        let spec = scheduled(endpoint, &dir, AGENTS + 470, gate::Mode::Auto);
        assert_eq!(finished(spec).status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(tidied(&dir, &sent), (true, false));
        let refused = results(sent.last().unwrap())[1];
        assert!(refused.contains("nobody is here to decide"), "{refused}");
        assert!(requests.try_recv().is_err(), "a card with nobody there");
    }
}
