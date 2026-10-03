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
//! the run counts as done. A reply cut off at the output limit, empty, or
//! stopping at its next step is sent back as the terminal sends it, and a
//! session's own agent is asked once to check edits nothing has run since.
//!
//! A frontend that shows the run as it goes, as an editor does over ACP,
//! gives it a `Watch`, which hears each step as it is made. A run that is a
//! session's own turn, as an editor's is, gives it the session's whole gate
//! as `Decide`, which then settles each call in place of the one above.
//! A run that keeps a tool its own way, as a memory review does, gives
//! `Allow::Own`: each call the gate lets through goes to its function
//! rather than to the tool.
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
//! A session's own run that would end with items open on the checklist it set
//! is sent back once, to finish them or say why not, and to end with its whole
//! answer again, since only its last message is printed or sent. A list an
//! earlier turn left is not counted, since the new prompt may be about
//! something else, nor is a `todo` call the run was refused, which set
//! nothing. A run someone watches live, as an editor's turn over ACP is, and
//! a chat app's are not sent back at all: a person answers what they end
//! with, as in the window.
//! A session's own run that would end without touching a file its request
//! names under `in/` is told so once, as a notice, to read it or say why not.
//! A run someone answers as it ends, an editor's turn over ACP or a chat
//! app's, is not, as the window's own turns are not.
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
use crate::contract::{self, Contract};
use crate::gate::{self, Gate, Grants, Layer};
use crate::inbox::{self, Notice, When};
use crate::inputs;
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

/// What a session's own run nobody watches is told when it asks the user
/// something.
const NOBODY_HERE: &str = "Nobody will answer questions in this run. Decide, record what you assumed, and finish the task.";

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

/// What a session's own run is told, followed by the items, when it would
/// end with items on its checklist not done.
const OPEN_ITEMS: &str = "Your checklist still has open items. Finish them, or say why each is not needed. If the task told you to stop at this point, say which items wait and end. Only your last message is passed on as your answer, so end with your whole answer again, not only what changed.";

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
    /// Every call the gate lets through goes to the caller's own function,
    /// which makes it its own way, as a memory review keeps memory.
    Own(Arc<Call>),
}

/// A caller's own way of making a call, giving its result.
pub type Call = dyn Fn(&ToolCall) -> Result<String, String> + Send + Sync;

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
    /// Characters of the reply still streaming, its words and its thinking,
    /// which no count above holds until the provider reports it at the end.
    pub streamed: AtomicU64,
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
    /// What was still wrong, as the run ended, with the files its request
    /// asked for: none for a helper, which is not held to them.
    pub contract: Vec<String>,
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
    // A session's own agent, as `-p` runs it, checks its edits as a turn in
    // the terminal does, when it may run a command to; a helper's answer is
    // its parent's to check.
    let verify = spec.depth == 0
        && crate::store::current_settings().verifies_edits()
        && refused(&spec, "run_command").is_none();
    let mut turn = Turn::with_limit(spec.connection.clone(), spec.rounds)
        .unattended()
        .verifying(verify);
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
        contract: Vec::new(),
    };
    let contract = required(&spec);
    let status = steps(&mut spec, &mut turn, &mut report, &contract)
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
    if let Some(workspace) = &spec.connection.workspace {
        report.contract = contract::problems(&contract, workspace);
    }
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

/// Calls the model and works through what it asks for, round after round,
/// until the run ends, sending it back once while `contract` is not met.
fn steps(
    spec: &mut Spec,
    turn: &mut Turn,
    report: &mut Report,
    contract: &Contract,
) -> Result<Outcome, String> {
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
    // A session's transcript holds its earlier turns too, and a checklist
    // one of them left is not this run's to finish.
    let begun = spec.transcript.conversation.messages.len();
    let mut nudged = false;
    let mut checked = false;
    // Found as the run starts, so a file it makes under `in/` is no input.
    let inputs = asked_to_read(spec);
    let mut pointed = false;
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
        let Some(mut reply) = ask_in_line(spec, turn)? else {
            keep_cut_short(spec, turn);
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
        // Taken before `continuation`, which drops calls made in the wrap-up
        // at the round cap: they still end the run at its limit.
        let calls = reply.tool_calls.clone();
        let next = turn.continuation(&mut reply);
        // A reply with nothing in it -- empty, or cut off inside a tool call
        // with no text before it -- leaves nothing to keep, and some routes
        // turn down an empty assistant message.
        if !reply.content.is_empty() || !reply.tool_calls.is_empty() {
            spec.transcript.conversation.messages.push(reply);
        }
        for call in &calls {
            tell(spec, Step::Asked(call.clone()));
        }
        match next {
            Ok(Some(nudge)) => {
                // Folded into the run, so the window keeps the task it is for.
                spec.transcript.conversation.messages.push(Message {
                    folded: true,
                    ..nudge.notice
                });
                save(&mut spec.transcript)?;
                continue;
            }
            Ok(None) => {}
            Err(why) => {
                save(&mut spec.transcript)?;
                return Ok(Outcome::Failed(why));
            }
        }
        if calls.is_empty() {
            // Past the round cap the wrap-up is the last word, as in the
            // terminal: mail or a Stop hook sending it back would undo the
            // limit, and unread mail goes on to the owner. A run that starts
            // wrapped up, as a quick look does, never reached a cap.
            if turn.capped().is_some() && !spec.connection.wrap_up {
                return Ok(Outcome::RoundLimit);
            }
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
            // Ending with its own plan unfinished is how part of a task gets
            // dropped, so a session's own run goes on once more for it; a
            // helper's answer is its parent's to judge, and a turn someone
            // watches live, as an editor's over ACP, or a chat app's is a
            // person's to answer, as the window's is. Past once, what is
            // still open was left on purpose. With no round or budget left,
            // going on could only end an answered run at its limit.
            if mail.is_none()
                && spec.depth == 0
                && spec.connection.remote.is_none()
                && spec.watch.is_none()
                && !nudged
                && report.rounds < spec.rounds
                && (spec.budget.as_ref()).is_none_or(|budget| budget.check().is_ok())
                && let Some(open) = open_checklist(
                    (spec.transcript.conversation.messages.get(begun..)).unwrap_or_default(),
                    &spec.transcript.conversation.todos,
                )
            {
                nudged = true;
                mail = inbox::fold(&[Notice {
                    from: "Letronna".into(),
                    when: When::Next,
                    text: open,
                }]);
            }
            // An input never opened is likely part of the task dropped, so the
            // run goes on once more to read it or say why not; past once, what
            // is left was left on purpose. With no round or budget left, going
            // on could only end an answered run at its limit.
            if mail.is_none()
                && !pointed
                && report.rounds < spec.rounds
                && (spec.budget.as_ref()).is_none_or(|budget| budget.check().is_ok())
            {
                let own =
                    (spec.transcript.conversation.messages.get(spec.from..)).unwrap_or_default();
                let unread = inputs::unread(&inputs, own);
                if !unread.is_empty() {
                    pointed = true;
                    let text = inputs::notice(&unread);
                    let notice = inbox::fold(&[Notice {
                        from: "Letronna".into(),
                        when: When::Next,
                        text,
                    }]);
                    // Folded, so it opens no turn of its own, as a Stop hook's word does not.
                    mail = notice.map(|notice| Message {
                        folded: true,
                        ..notice
                    });
                }
            }
            let answered = mail.is_none();
            spec.transcript.conversation.messages.extend(mail);
            save(&mut spec.transcript)?;
            if !answered {
                continue;
            }
            // Before the Stop hooks, so they judge the files once put right;
            // and once only, since a model sent back again starts rewriting
            // what it had right. With no round or budget left, going back
            // could only end an answered run at its limit.
            if !checked
                && report.rounds < spec.rounds
                && (spec.budget.as_ref()).is_none_or(|budget| budget.check().is_ok())
                && let Some(workspace) = &spec.connection.workspace
            {
                let problems = contract::problems(contract, workspace);
                if !problems.is_empty() {
                    checked = true;
                    // A notice, so nothing takes it for the user's words.
                    spec.transcript.conversation.messages.push(Message {
                        role: "user".into(),
                        content: contract::notice(&problems),
                        folded: true,
                        notice: true,
                        ..Default::default()
                    });
                    save(&mut spec.transcript)?;
                    continue;
                }
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
        // The round at the cap runs nothing: it only answers its calls as not
        // run and asks for the wrap-up.
        if turn.capped().is_none() {
            report.rounds += 1;
            spec.status.rounds.store(report.rounds, Ordering::Relaxed);
        }
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
            Next::Run(call) => dispatch(spec, &call, false, report, false),
            Next::Sandboxed(call) => dispatch(spec, &call, false, report, true),
            Next::Refuse(_, why) => Some(Err(why)),
            Next::AskUser(call) => {
                // Only the judge's word says why, when it was unsure.
                let why = layer.as_ref().map(Layer::tag).unwrap_or_default();
                let (ctx, lasting) = (context(spec), lasting(spec, &call, &rules));
                match approvals::ask(spec.root, spec.run, &spec.label, &call, &ctx, &why, lasting) {
                    // Only what was agreed to runs, never the model's own,
                    // and a yes to the model's call is none to a changed one.
                    Ok(allowed) => match allowed.arguments {
                        None => dispatch(spec, &call, true, report, allowed.sandboxed),
                        Some(arguments) => {
                            let changed = ToolCall { arguments, ..call };
                            dispatch(spec, &changed, false, report, allowed.sandboxed)
                        }
                    },
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
/// to it on its card and inside the OS sandbox when `sandboxed`, or gives
/// None when a stop came before it began.
fn dispatch(
    spec: &mut Spec,
    call: &ToolCall,
    approved: bool,
    report: &mut Report,
    sandboxed: bool,
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
    if let Allow::Own(own) = &spec.allow {
        return made(own.as_ref(), call, &spec.cancel);
    }
    if call.name == tools::TODO {
        let outcome = checklist(&mut spec.transcript, &call.arguments);
        let todos = spec.transcript.conversation.todos.clone();
        tell(spec, Step::Checklist(todos));
        return Some(outcome);
    }
    let ctx = ToolContext {
        approved,
        sandboxed,
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
                let allow = &spec.allow;
                scope.spawn(move || match (refused, allow) {
                    (Some(refused), _) => Some(Err(refused)),
                    (None, Allow::Own(own)) => made(own.as_ref(), call, &ctx.cancel),
                    (None, _) => off_thread(call, &ctx),
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
        // No tool runs by its name: the caller's function makes every call
        // the gate lets through, and refuses what it will not make.
        Allow::Own(_) => return None,
    };
    if DENY.contains(&name) || !allowed {
        return Some(format!("`{name}` is not available to this helper."));
    }
    None
}

/// Why this agent may not call `name`, when it may not: a scheduled run's
/// own agent as such, another nobody watches as such when it asks the user,
/// any other as `refusal` says.
fn refused(spec: &Spec, name: &str) -> Option<String> {
    if spec.depth > 0 || crate::schedule::running(spec.root).is_none() {
        // Not a helper, so not told to end on the open question: with
        // nobody to read it, that ends the run with the task undone.
        if name == tools::ASK_USER && unwatched(spec) {
            return Some(NOBODY_HERE.into());
        }
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

/// Whether this is a session's own run with nobody there to answer it, as
/// `letronna -p` with no terminal or a schedule is.
fn unwatched(spec: &Spec) -> bool {
    spec.depth == 0 && !spec.connection.human
}

/// The tools a run nobody watches may not call that its requests and its
/// forks' leave out; none for any other run.
fn hidden(spec: &Spec) -> Vec<String> {
    // A fork the agent tool starts begins with its parent's bytes and holds
    // its parent's Allow, so one of the session's own run leaves out what
    // that run does, and the two still share a cached prefix. A watched run
    // is told to ask with ask_user, and any other helper is offered the
    // whole array, though an unwatched parent's is shorter: those are
    // refused at the call instead.
    let own = spec.depth == 0 || (spec.fork && spec.depth == 1);
    if spec.connection.human || !own {
        return Vec::new();
    }
    // Every run with tools is told to keep what will matter with memory, and
    // a provider that checks a reply's calls against the array fails a call
    // of one it left out, where a refusal at the call costs a round.
    let told = |name: &&str| *name == "memory";
    // On demand, an MCP tool that --tools allows is reached only through
    // this pair, as a helper's is (`agents::usable`).
    let mcp = match &spec.allow {
        Allow::Only(names) => names.iter().any(|name| crate::mcp_find::is_mcp(name)),
        _ => false,
    };
    let reaching = |name: &&str| {
        mcp && [crate::mcp_find::FIND_TOOLS, crate::mcp_find::CALL_MCP_TOOL].contains(name)
    };
    tools::all()
        .iter()
        .map(|tool| tool.name())
        .filter(|name| !told(name) && !reaching(name))
        .filter(|name| refused(spec, name).is_some())
        .map(str::to_owned)
        .collect()
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
                Allow::Own(_) => Some(Arc::from([])),
            },
            fork: spec.fork,
            allow: spec.allow.clone(),
            ..ctx.caller.clone()
        },
        ..ctx
    }
}

/// Makes a call the caller's own way, or gives None when a stop came before
/// it began, as `off_thread` does.
fn made(own: &Call, call: &ToolCall, cancel: &Cancel) -> Option<Result<String, String>> {
    (!cancel.is_set()).then(|| own(call))
}

/// What a session's own run must leave in its workspace, as the request it
/// was started with names it; nothing for a helper, whose work is its
/// parent's to judge, for a run that may not write, or for one mail started.
fn required(spec: &Spec) -> Contract {
    let (0, Some(workspace)) = (spec.depth, &spec.connection.workspace) else {
        return Contract::default();
    };
    // Sent back, a run in Plan mode or with no tools could only be pointed
    // at calls it may not make.
    if spec.connection.plan || !spec.connection.tools {
        return Contract::default();
    }
    let messages = &spec.transcript.conversation.messages;
    let own = messages.get(spec.from..).unwrap_or_default();
    let Some(request) = own.iter().rfind(|m| chat::opens_turn(m)) else {
        return Contract::default();
    };
    // Mail, such as a job's output or a helper's report, is no request.
    if request.notice {
        return Contract::default();
    }
    contract::from_request(&request.content, workspace)
}

/// The input files a session's own run is asked to read, as the request it
/// was started with names them; none for a run whose end someone answers.
fn asked_to_read(spec: &Spec) -> Vec<String> {
    // A helper's work is its parent's to judge, and a person reads an
    // editor's turn over ACP or a chat app's as it ends, as in the window.
    let (0, None, None, Some(workspace)) = (
        spec.depth,
        &spec.watch,
        &spec.connection.remote,
        &spec.connection.workspace,
    ) else {
        return Vec::new();
    };
    if !spec.connection.tools {
        return Vec::new();
    }
    let messages = &spec.transcript.conversation.messages;
    let own = messages.get(spec.from..).unwrap_or_default();
    let Some(request) = own.iter().rfind(|m| chat::opens_turn(m)) else {
        return Vec::new();
    };
    // Mail, such as a job's output, is no request.
    if request.notice {
        return Vec::new();
    }
    let typed = crate::references::typed(&request.content);
    inputs::named(typed, workspace)
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
        let asked = ask(spec, turn, permit);
        // What streamed is in the reply's own count now, or went with it.
        spec.status.streamed.store(0, Ordering::Relaxed);
        let error = match asked {
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
        hide: hidden(spec),
        cancel: spec.cancel.clone(),
        guard: Some(Box::new(permit)),
        // A fork's is its parent's, so its requests start as the parent's do.
        floor: spec.transcript.conversation.floor(),
    };
    // The turn's, as the window and the terminal send, since at the round
    // cap it alone asks for the wrap-up with no calls.
    let events = chat::start_with(
        turn.connection.clone(),
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
        // Counted before it is told, so a watcher reads the count with it.
        if let Event::Delta(text) | Event::Reasoning(text) = &event {
            let streamed = &spec.status.streamed;
            streamed.fetch_add(text.len() as u64, Ordering::Relaxed);
        }
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

/// Keeps the reply a stop cut short, as far as it streamed and marked
/// interrupted, so the session shows what the model had said: what
/// `Turn::stop` keeps of the window's own turn, calls that never ran left out.
fn keep_cut_short(spec: &mut Spec, turn: &mut Turn) {
    let kept = turn.stop(&spec.connection.model);
    spec.transcript.conversation.messages.extend(kept);
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

/// What a run is sent back with when a `todo` call among its own `messages`
/// kept `todos` with items not done, or None otherwise.
fn open_checklist(messages: &[Message], todos: &[Todo]) -> Option<String> {
    // Only a call that ran keeps a list: one it was refused, or one that
    // could not be read, left `todos` as an earlier turn had it.
    let kept = messages
        .iter()
        .any(|message| message.role == "tool" && message.name == tools::TODO && !message.is_error);
    if !kept {
        return None;
    }
    let open: Vec<String> = todos
        .iter()
        .filter(|todo| todo.status != "done")
        .map(|todo| format!("- {} ({})", todo.text, todo.status))
        .collect();
    if open.is_empty() {
        return None;
    }
    // The items go last: a fold cuts a long notice off at its end.
    Some(format!("{OPEN_ITEMS}\n{}", open.join("\n")))
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

/// The last thing the agent said, which is its answer when it finished, with
/// every piece of a reply that went on after being cut off at the output limit.
pub fn last_said(messages: &[Message]) -> String {
    let said = |message: &Message| message.role == "assistant" && !message.content.is_empty();
    let Some(last) = messages.iter().rposition(said) else {
        return String::new();
    };
    // Each piece after the first follows the notice asking it to go on, and
    // takes up where the one before stopped, so they join as they are.
    let mut first = last;
    while let [.., before, notice] = &messages[..first]
        && notice.content == crate::agent::CONTINUE
        && said(before)
    {
        first -= 2;
    }
    messages[first..=last]
        .iter()
        .step_by(2)
        .map(|message| message.content.as_str())
        .collect()
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
pub(crate) mod tests {
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

    pub(crate) fn listen() -> (TcpListener, String) {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        (server, endpoint)
    }

    /// The body of the request coming in on `socket`.
    pub(crate) fn request(socket: &mut TcpStream) -> Value {
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
    pub(crate) fn answer(socket: &mut TcpStream, reply: &str) {
        write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
    }

    /// A local provider at work on its thread, and where to reach it.
    pub(crate) struct Stub {
        address: SocketAddr,
        worker: JoinHandle<Vec<Value>>,
    }

    impl Stub {
        /// What each request carried, failing rather than hanging when the
        /// run asked for fewer replies than the stub was given.
        pub(crate) fn sent(self) -> Vec<Value> {
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
    pub(crate) fn provider(replies: Vec<String>) -> (String, Stub) {
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
    pub(crate) fn reply(text: &str, calls: &[(&str, Value)]) -> String {
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

    /// A streamed reply that says `text` and stops at the output limit, inside
    /// `call` when there is one: a tool's name and its arguments as far as
    /// they got.
    fn cut(text: &str, call: Option<(&str, &str)>) -> String {
        let mut events = Vec::new();
        if !text.is_empty() {
            events.push(json!({"choices": [{"delta": {"content": text}}]}));
        }
        if let Some((name, begun)) = call {
            events.push(json!({"choices": [{"delta": {"tool_calls": [{"index": 0,
                "id": "call-0", "function": {"name": name, "arguments": begun}}]}}]}));
        }
        events.push(json!({"choices": [{"delta": {}, "finish_reason": "length"}]}));
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

    /// A code file, which arms the reminder to check edits as a .txt would not.
    fn write_made() -> (&'static str, Value) {
        ("write_file", json!({"path": "made.rs", "content": "made"}))
    }

    /// A data folder of the test's own while it is held, so a run at depth 0
    /// reads the settings at their defaults, verify_edits on, whatever the
    /// user's own say.
    struct Data {
        _folder: Scratch,
        _held: std::sync::MutexGuard<'static, ()>,
    }

    impl Drop for Data {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        }
    }

    fn data(name: &str) -> Data {
        let held = (crate::store::tests::DATA_DIR.lock()).unwrap_or_else(|e| e.into_inner());
        let folder = Scratch::new(&format!("driver-data-{name}"));
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*folder) };
        Data {
            _folder: folder,
            _held: held,
        }
    }

    /// The session's own gate letting every call run, as a yes for each would.
    fn runs_everything() -> Option<Decide> {
        Some(Arc::new(|_, _| crate::gate::Outcome::Run))
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
    fn a_reply_counts_what_it_has_streamed_until_it_ends() {
        let dir = scratch("streamed");
        let event = |event: Value| format!("data: {event}\n\n");
        let mut stream: String = (["Reading ", "the notes."].iter())
            .map(|text| event(json!({"choices": [{"delta": {"content": text}}]})))
            .collect();
        stream.push_str(&event(json!({"choices": [],
            "usage": {"prompt_tokens": 1_000, "completion_tokens": 50}})));
        stream.push_str("data: [DONE]\n\n");
        let (endpoint, provider) = provider(vec![stream]);
        let mut spec = spec(endpoint, &dir, AGENTS + 480);
        let status = spec.status.clone();
        let counts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (seen, counted) = (counts.clone(), status.clone());
        spec.watch = Some(Arc::new(move |step| {
            if let Step::Said(_) = step {
                let streamed = counted.streamed.load(Ordering::Relaxed);
                seen.lock().unwrap().push(streamed);
            }
        }));
        let report = finished(spec);
        provider.sent();
        assert_eq!(report.status, Outcome::Done);
        assert_eq!(*counts.lock().unwrap(), [8, 18]);
        // The provider's count holds it from here.
        assert_eq!(status.streamed.load(Ordering::Relaxed), 0);
        assert_eq!(status.output.load(Ordering::Relaxed), 50);
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
    fn a_run_of_its_own_makes_only_what_the_gate_lets_through() {
        let dir = scratch("own");
        let (endpoint, provider) = provider(vec![
            reply(
                "",
                &[
                    read_notes(),
                    read_notes(),
                    ("write_file", json!({"path": "made.txt", "content": "made"})),
                    (
                        "memory",
                        json!({"action": "add", "target": "user", "content": "x"}),
                    ),
                ],
            ),
            reply("Done.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 120);
        let made = Arc::new(std::sync::Mutex::new(Vec::new()));
        let making = made.clone();
        spec.allow = Allow::Own(Arc::new(move |call| {
            making.lock().unwrap().push(call.name.clone());
            Ok(format!("made {}", call.name))
        }));
        spec.decide = Some(Arc::new(|call, _| match call.name.as_str() {
            "write_file" => crate::gate::Outcome::Refuse("Not here.".into()),
            _ => crate::gate::Outcome::Run,
        }));
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        // The reads ran together, and a tool in the deny set is the
        // function's to make or refuse.
        assert_eq!(
            results(&provider.sent()[1]),
            [
                "made read_file",
                "made read_file",
                "Not here.",
                "made memory"
            ]
        );
        assert_eq!(*made.lock().unwrap(), ["read_file", "read_file", "memory"]);
        assert!(!dir.join("made.txt").exists());
        assert!(report.files.is_empty());
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
    fn a_depth_zero_ask_user_is_told_nobody_will_answer() {
        let dir = scratch("nobody-here");
        let (endpoint, provider) = provider(vec![
            reply("", &[("ask_user", json!({"question": "Which framework?"}))]),
            reply("I kept the one already in use.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 700);
        spec.depth = 0;
        assert_eq!(finished(spec).status, Outcome::Done);
        let sent = provider.sent();

        assert_eq!(results(&sent[1]), [NOBODY_HERE]);
    }

    /// The names of the tools the first request of `spec`'s run offers once
    /// `change` has been made to it, the run answered at once.
    fn first_offered(dir: &Path, id: AgentId, change: impl FnOnce(&mut Spec)) -> Vec<String> {
        let (endpoint, provider) = provider(vec![reply("Nothing to do.", &[])]);
        let mut spec = spec(endpoint, dir, id);
        change(&mut spec);
        assert_eq!(finished(spec).status, Outcome::Done);
        let sent = provider.sent();
        let tools = sent[0]["tools"].as_array().unwrap();
        let names = tools
            .iter()
            .filter_map(|tool| tool["function"]["name"].as_str());
        names.map(str::to_owned).collect()
    }

    #[test]
    fn ask_user_is_not_offered_to_a_run_nobody_watches() {
        let dir = scratch("unwatched");
        let offers = |names: &[String], name: &str| names.iter().any(|n| n == name);
        let unwatched = first_offered(&dir, AGENTS + 710, |spec| spec.depth = 0);
        let watched = first_offered(&dir, AGENTS + 720, |spec| {
            spec.depth = 0;
            spec.connection.human = true;
        });
        let helper = first_offered(&dir, AGENTS + 730, |_| ());

        assert!(offers(&unwatched, "read_file"), "{unwatched:?}");
        for name in DENY.into_iter().filter(|name| *name != "memory") {
            assert!(!offers(&unwatched, name), "{name}: {unwatched:?}");
        }
        // The prompt tells it to keep things with memory, so it stays.
        assert!(offers(&unwatched, "memory"), "{unwatched:?}");
        // Someone is there to be asked, so it goes out as it always has.
        assert!(offers(&watched, tools::ASK_USER), "{watched:?}");
        // A helper that is no fork is offered them all, refused at the call.
        assert!(offers(&helper, tools::ASK_USER), "{helper:?}");
        assert!(offers(&helper, "memory"), "{helper:?}");
    }

    #[test]
    fn a_fork_of_a_run_nobody_watches_is_offered_what_its_parent_is() {
        let dir = scratch("unwatched-fork");
        let session = AGENTS + 760;
        let _listed = crate::schedule::Listed::new(session, &job(&dir, gate::Mode::Default));
        let only = Allow::Only(vec!["read_file".into(), "list_dir".into()]);
        // A -p run with and without --tools, and a scheduled run.
        let runs = [(Allow::All, 7), (only, 7), (Allow::All, session)];
        for (n, (allow, root)) in runs.into_iter().enumerate() {
            let id = AGENTS + 770 + 10 * n as AgentId;
            let parent = first_offered(&dir, id, |spec| {
                spec.depth = 0;
                spec.root = root;
                spec.allow = allow.clone();
            });
            let fork = first_offered(&dir, id + 5, |spec| {
                spec.fork = true;
                spec.root = root;
                spec.allow = allow;
            });

            assert!(!parent.iter().any(|name| name == tools::ASK_USER));
            // Its first request starts as its parent's do, tools first.
            assert_eq!(fork, parent, "{root}");
        }
    }

    #[test]
    fn only_the_tools_named_by_allow_only_are_offered() {
        let dir = scratch("allow-only");
        let only = |names: &[&str]| Allow::Only(names.iter().map(|n| n.to_string()).collect());
        let offered = first_offered(&dir, AGENTS + 740, |spec| {
            spec.depth = 0;
            spec.allow = only(&["read_file", "list_dir"]);
        });
        assert_eq!(offered, ["read_file", "list_dir", "memory"]);
    }

    #[test]
    fn an_mcp_tool_allow_only_names_keeps_the_pair_that_reaches_it() {
        let dir = scratch("allow-mcp");
        let mut spec = spec(String::new(), &dir, AGENTS + 750);
        spec.depth = 0;
        let pair = [crate::mcp_find::FIND_TOOLS, crate::mcp_find::CALL_MCP_TOOL];
        let hides = |hidden: &[String], name: &str| hidden.iter().any(|n| n == name);
        spec.allow = Allow::Only(vec!["read_file".into()]);
        let no_mcp = hidden(&spec);
        spec.allow = Allow::Only(vec!["read_file".into(), "mcp__big__tool_1".into()]);
        let mcp = hidden(&spec);

        assert!(pair.iter().all(|name| hides(&no_mcp, name)), "{no_mcp:?}");
        assert!(hides(&mcp, "write_file"), "{mcp:?}");
        assert!(!pair.iter().any(|name| hides(&mcp, name)), "{mcp:?}");
    }

    #[test]
    fn a_chat_apps_call_gets_a_card_but_a_headless_run_still_refuses() {
        let _serving = crate::approvals::tests::SERVING
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let requests = approvals::serve();
        let dir = scratch("chat-app");
        let write = || ("write_file", json!({"path": "made.txt", "content": "made"}));

        // A chat app's session has a human too, just reached through a card
        // sent to the chat, so a call that needs a yes asks rather than
        // being refused outright.
        let (endpoint, stub) = provider(vec![reply("", &[write()]), reply("Wrote it.", &[])]);
        let mut chatting = spec(endpoint, &dir, AGENTS + 300);
        chatting.connection.remote = Some(chat::Remote {
            place: "a Telegram chat".into(),
            ..Default::default()
        });
        chatting.connection.human = true;
        let running = spawn(chatting, |_| ()).unwrap();
        let request = requests.recv_timeout(WAIT).unwrap();
        assert_eq!(
            request.root, 7,
            "a chat card routes by the session it names"
        );
        request.answer(approvals::Decision::Once);
        assert_eq!(running.join().unwrap().status, Outcome::Done);
        let _ = stub.sent();
        assert!(dir.join("made.txt").exists());

        // A headless run -- a schedule, or `-p` with no terminal to answer
        // in -- has nobody there at all, so it still refuses with no card.
        let (endpoint, provider) = provider(vec![reply("", &[write()]), reply("Left it.", &[])]);
        let mut spec = spec(endpoint, &dir, AGENTS + 310);
        spec.connection.human = false;
        assert_eq!(finished(spec).status, Outcome::Done);
        let sent = provider.sent();
        assert!(
            results(&sent[1])[0].contains("nobody is here to give it"),
            "{:?}",
            results(&sent[1])
        );
        assert!(requests.try_recv().is_err(), "a card with nobody there");
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
    fn a_reply_a_stop_cut_short_is_kept_as_far_as_it_streamed() {
        let dir = scratch("cut-short");
        let mut spec = spec("http://127.0.0.1:9/v1".into(), &dir, AGENTS + 55);
        let mut turn = Turn::new(spec.connection.clone());
        // Nothing streamed yet: nothing to keep.
        keep_cut_short(&mut spec, &mut turn);
        assert_eq!(spec.transcript.conversation.messages.len(), 1);
        turn.absorb(&Event::Delta("Reading the no".into()));
        turn.live.calls.push(ToolCall {
            id: "call-0".into(),
            name: "read_file".into(),
            arguments: "{\"pa".into(),
        });
        keep_cut_short(&mut spec, &mut turn);
        let kept = spec.transcript.conversation.messages.last().unwrap();
        assert_eq!(
            (kept.role.as_str(), kept.content.as_str(), kept.interrupted),
            ("assistant", "Reading the no", true)
        );
        assert!(kept.tool_calls.is_empty(), "a call that never ran was kept");
        let said = last_said(&spec.transcript.conversation.messages);
        assert_eq!(said, "Reading the no");
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

    /// A `todo` call that sets the checklist to `items`, each a text and its status.
    fn checklist_of(items: &[(&str, &str)]) -> (&'static str, Value) {
        let items: Vec<Value> = items
            .iter()
            .map(|(text, status)| json!({"text": text, "status": status}))
            .collect();
        ("todo", json!({ "items": items }))
    }

    /// A session's own run of the helper task, as `-p` starts one.
    fn own_run(endpoint: String, dir: &Path, id: AgentId) -> Spec {
        Spec {
            depth: 0,
            ..spec(endpoint, dir, id)
        }
    }

    /// The checklist notices among the messages a request sent.
    fn checklist_notices(body: &Value) -> Vec<&str> {
        messages(body)
            .iter()
            .filter_map(|message| message["content"].as_str())
            .filter(|content| content.contains(OPEN_ITEMS))
            .collect()
    }

    #[test]
    fn open_checklist_items_send_the_run_back_once() {
        let dir = scratch("open-items");
        let half = [("read the notes", "done"), ("write the summary", "doing")];
        let (endpoint, provider) = provider(vec![
            reply("", &[checklist_of(&half)]),
            reply("Planned.", &[]),
            reply(
                "",
                &[checklist_of(&[
                    ("read the notes", "done"),
                    ("write the summary", "done"),
                ])],
            ),
            reply("The summary is written.", &[]),
        ]);
        let report = finished(own_run(endpoint, &dir, AGENTS + 490));
        let sent = provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.text, "The summary is written.");
        assert!(checklist_notices(&sent[1]).is_empty());
        let sent_back = messages(&sent[2]).last().unwrap();
        assert_eq!(sent_back["role"], "user");
        let notice = sent_back["content"].as_str().unwrap();
        assert!(notice.starts_with("[Letronna notice"), "{notice}");
        assert!(notice.contains("- write the summary (doing)"), "{notice}");
        assert!(!notice.contains("read the notes (done)"), "{notice}");
        assert!(notice.contains("your whole answer again"), "{notice}");
        assert_eq!(checklist_notices(&sent[3]).len(), 1);
    }

    #[test]
    fn a_finished_checklist_lets_the_run_end() {
        let dir = scratch("finished-items");
        let (endpoint, provider) = provider(vec![
            reply("", &[checklist_of(&[("read the notes", "done")])]),
            reply("The notes say hello.", &[]),
        ]);
        let report = finished(own_run(endpoint, &dir, AGENTS + 500));
        let sent = provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.text, "The notes say hello.");
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn a_run_sent_back_once_for_its_checklist_may_then_end_with_items_open() {
        let dir = scratch("waiting-items");
        let (endpoint, provider) = provider(vec![
            reply(
                "",
                &[checklist_of(&[
                    ("item one", "done"),
                    ("item two", "pending"),
                ])],
            ),
            reply("Item one is done.", &[]),
            reply(
                "Item one is done. The task said to stop there, so item two waits.",
                &[],
            ),
        ]);
        let report = finished(own_run(endpoint, &dir, AGENTS + 510));
        let sent = provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(
            report.text,
            "Item one is done. The task said to stop there, so item two waits."
        );
        assert_eq!(sent.len(), 3);
        assert_eq!(checklist_notices(&sent[2]).len(), 1);
    }

    #[test]
    fn a_checklist_an_earlier_turn_left_open_does_not_send_the_run_back() {
        let dir = scratch("earlier-items");
        let (endpoint, provider) = provider(vec![reply("The notes say hello.", &[])]);
        let mut spec = own_run(endpoint, &dir, AGENTS + 520);
        let (name, args) = checklist_of(&[("tidy the folder", "doing")]);
        let earlier = vec![
            Message {
                role: "user".into(),
                content: "Tidy the folder.".into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                tool_calls: vec![ToolCall {
                    id: "call-0".into(),
                    name: name.into(),
                    arguments: args.to_string(),
                }],
                ..Default::default()
            },
            Message {
                role: "tool".into(),
                content: "Checklist updated: 0 of 1 done.".into(),
                tool_call_id: "call-0".into(),
                name: name.into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                content: "Started tidying.".into(),
                ..Default::default()
            },
        ];
        spec.transcript.conversation.messages.splice(0..0, earlier);
        spec.transcript.conversation.todos = vec![Todo {
            text: "tidy the folder".into(),
            status: "doing".into(),
        }];
        let report = finished(spec);
        let sent = provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.text, "The notes say hello.");
        assert_eq!(sent.len(), 1);
    }

    #[test]
    fn a_helper_ends_with_its_checklist_open() {
        let dir = scratch("helper-items");
        let (endpoint, provider) = provider(vec![
            reply("", &[checklist_of(&[("write the summary", "doing")])]),
            reply("Planned.", &[]),
        ]);
        let report = finished(spec(endpoint, &dir, AGENTS + 530));
        let sent = provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn a_chat_apps_run_ends_with_its_checklist_open() {
        let dir = scratch("chat-app-items");
        let (endpoint, provider) = provider(vec![
            reply("", &[checklist_of(&[("write the summary", "pending")])]),
            reply("Shall I write the summary?", &[]),
        ]);
        let mut spec = own_run(endpoint, &dir, AGENTS + 540);
        spec.connection.remote = Some(chat::Remote {
            place: "a Telegram chat".into(),
            ..Default::default()
        });
        let report = finished(spec);
        let sent = provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.text, "Shall I write the summary?");
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn a_run_someone_watches_live_ends_with_its_checklist_open() {
        let dir = scratch("watched-items");
        let (endpoint, provider) = provider(vec![
            reply("", &[checklist_of(&[("write the summary", "pending")])]),
            reply("Shall I write the summary?", &[]),
        ]);
        let mut spec = own_run(endpoint, &dir, AGENTS + 570);
        spec.watch = Some(Arc::new(|_| ()));
        let report = finished(spec);
        let sent = provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.text, "Shall I write the summary?");
        assert_eq!(sent.len(), 2);
    }

    #[test]
    fn a_checklist_the_run_was_refused_does_not_send_it_back() {
        let dir = scratch("refused-items");
        let (endpoint, provider) = provider(vec![
            reply("", &[checklist_of(&[("write the summary", "doing")])]),
            reply("The notes say hello.", &[]),
        ]);
        let mut spec = own_run(endpoint, &dir, AGENTS + 580);
        spec.allow = Allow::Only(vec!["read_file".into()]);
        let report = finished(spec);
        let sent = provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.text, "The notes say hello.");
        assert_eq!(sent.len(), 2);
        let refused = results(&sent[1])[0];
        assert!(refused.starts_with("`todo` is not available"), "{refused}");
    }

    #[test]
    fn a_run_with_no_round_or_budget_left_ends_with_its_checklist_open() {
        let open = [("write the summary", "doing")];
        // Its one round went on the checklist.
        let dir = scratch("last-round-items");
        let (endpoint, last_round) = provider(vec![
            reply("", &[checklist_of(&open)]),
            reply("The notes say hello.", &[]),
        ]);
        let mut spec = own_run(endpoint, &dir, AGENTS + 550);
        spec.rounds = 1;
        assert_eq!(finished(spec).status, Outcome::Done);
        assert_eq!(last_round.sent().len(), 2);

        // Each reply costs 1,050 tokens, so the answer spends the last of it.
        let dir = scratch("spent-items");
        let (endpoint, spent) = provider(vec![
            reply("", &[checklist_of(&open)]),
            reply("The notes say hello.", &[]),
        ]);
        let mut spec = own_run(endpoint, &dir, AGENTS + 560);
        spec.budget = Some(Arc::new(Budget::new(2_000, None)));
        assert_eq!(finished(spec).status, Outcome::Done);
        assert_eq!(spent.sent().len(), 2);
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
        let _data = data("scheduled");
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
            let mut replies = vec![
                reply(
                    "",
                    &[
                        ("write_file", json!({"path": "made.rs", "content": "made"})),
                        ("ask_user", json!({"question": "Which one?"})),
                        ("wake_me", json!({"in": "1h"})),
                    ],
                ),
                reply("Done.", &[]),
            ];
            // A scheduled run is a session's own, so the write Bypass runs
            // is sent back once to be checked.
            if mode == Mode::Bypass {
                replies.push(reply("Nothing here builds or tests.", &[]));
            }
            let (endpoint, provider) = provider(replies);
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
                assert!(dir.join("made.rs").exists(), "{got:?}");
            } else {
                assert!(!dir.join("made.rs").exists());
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

    /// A provider that makes the same call twice, then wraps up in prose.
    fn capped_after_one_round() -> (String, Stub) {
        provider(vec![
            reply("", &[read_notes()]),
            reply("", &[read_notes()]),
            reply("The notes say hello; nothing is left to do.", &[]),
        ])
    }

    #[test]
    fn the_wrap_up_at_the_round_cap_is_requested_with_no_tools() {
        let dir = scratch("wrap-up");
        let (endpoint, provider) = capped_after_one_round();
        let mut spec = spec(endpoint, &dir, AGENTS + 490);
        spec.rounds = 1;
        finished(spec);
        let sent = provider.sent();

        assert_eq!(sent[1].get("tool_choice"), None);
        // Still offered, so the cached prefix holds, but none may be used.
        assert_eq!(sent[2]["tool_choice"], "none");
        assert_eq!(sent[2]["tools"], sent[1]["tools"]);
        assert_eq!(
            results(&sent[2]).last(),
            Some(&"Not run: the tool-round limit (1) was reached.")
        );
        let asked = messages(&sent[2]).last().unwrap().to_string();
        assert!(asked.contains("reached the tool-round limit"), "{asked}");
    }

    #[test]
    fn a_prose_wrap_up_at_the_cap_ends_as_round_limit() {
        let dir = scratch("capped");
        let (endpoint, provider) = capped_after_one_round();
        let mut spec = spec(endpoint, &dir, AGENTS + 500);
        spec.rounds = 1;
        let report = finished(spec);
        provider.sent();

        assert_eq!(report.status, Outcome::RoundLimit);
        assert_eq!(report.text, "The notes say hello; nothing is left to do.");
    }

    #[test]
    fn the_cap_round_is_not_counted_as_a_round_run() {
        let dir = scratch("cap-round");
        let (endpoint, provider) = capped_after_one_round();
        let mut spec = spec(endpoint, &dir, AGENTS + 510);
        spec.rounds = 1;
        let status = spec.status.clone();
        let report = finished(spec);
        provider.sent();

        assert_eq!(report.rounds, 1);
        assert_eq!(status.rounds.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn a_run_that_starts_wrapped_up_still_ends_as_answered() {
        let dir = scratch("wrapped-up");
        let (endpoint, provider) = provider(vec![reply("It says hello.", &[])]);
        let mut spec = spec(endpoint, &dir, AGENTS + 520);
        // As a quick look runs: one request, no tools, no rounds.
        spec.connection.tools = false;
        spec.connection.wrap_up = true;
        spec.rounds = 0;
        let report = finished(spec);
        provider.sent();

        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.text, "It says hello.");
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

    #[test]
    fn a_write_cut_off_at_the_output_limit_is_asked_for_again_and_written() {
        let dir = scratch("cut-write");
        let whole = "# Report\n\nThe notes say hello.\n";
        let begun = r##"{"path": "report.md", "content": "# Rep"##;
        let (endpoint, provider) = provider(vec![
            cut("", Some(("write_file", begun))),
            reply(
                "",
                &[("write_file", json!({"path": "report.md", "content": whole}))],
            ),
            reply("Wrote report.md.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 500);
        spec.decide = runs_everything();
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        let asked = messages(&sent[1]).last().unwrap();
        assert_eq!(asked["role"], "user");
        let asked = asked["content"].as_str().unwrap();
        assert!(asked.contains("in the middle of a tool call"), "{asked}");
        assert_eq!(
            std::fs::read_to_string(dir.join("report.md")).unwrap(),
            whole
        );
        assert_eq!(report.text, "Wrote report.md.");
        // The cut reply had nothing to keep, and the notice is folded into
        // the run, so the window still holds the task.
        let saved = written(&dir.join("helper.jsonl"));
        let kept: Vec<(&str, bool)> = saved
            .iter()
            .filter(|record| record["type"] == "message")
            .map(|record| (record["role"].as_str().unwrap(), record["folded"] == true))
            .collect();
        let roles = [
            ("user", false),
            ("user", true),
            ("assistant", false),
            ("tool", false),
            ("assistant", false),
        ];
        assert_eq!(kept, roles);
    }

    #[test]
    fn an_empty_reply_is_asked_for_once_before_the_run_ends() {
        let dir = scratch("empty");
        let (endpoint, provider) = provider(vec![reply("", &[]), reply("", &[])]);
        let report = finished(spec(endpoint, &dir, AGENTS + 510));
        let sent = provider.sent();

        assert_eq!(
            report.status,
            Outcome::Failed("The model returned no text. Check model compatibility.".into())
        );
        let asked = messages(&sent[1]).last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(asked.contains("no visible output"), "{asked}");
        // Neither empty reply is kept, since some routes turn one down.
        assert!(messages(&sent[1]).iter().all(|m| m["role"] != "assistant"));
    }

    #[test]
    fn a_reply_still_cut_off_after_three_continuations_fails_the_run() {
        let dir = scratch("cut-four");
        let parts = ["One", "Two", "Three", "Four"].map(|part| cut(&format!("{part}."), None));
        let (endpoint, provider) = provider(parts.into());
        let report = finished(spec(endpoint, &dir, AGENTS + 520));
        let sent = provider.sent();

        assert_eq!(
            report.status,
            Outcome::Failed(
                "The reply was still cut off at the output limit after 3 continuations, so it \
                 stops here."
                    .into()
            )
        );
        for request in &sent[1..] {
            let asked = messages(request).last().unwrap()["content"]
                .as_str()
                .unwrap();
            assert!(
                asked.contains("continue exactly where it stopped"),
                "{asked}"
            );
        }
        // What it said is kept for the parent, every piece of it.
        assert_eq!(report.text, "One.Two.Three.Four.");
    }

    #[test]
    fn a_reply_cut_off_and_continued_is_answered_whole() {
        let dir = scratch("cut-continued");
        let (endpoint, provider) = provider(vec![cut("Part one.", None), reply(" Part two.", &[])]);
        let report = finished(spec(endpoint, &dir, AGENTS + 525));
        assert_eq!(report.status, Outcome::Done);
        provider.sent();

        assert_eq!(report.text, "Part one. Part two.");
    }

    #[test]
    fn a_short_answer_announcing_its_next_step_is_pushed_on_at_depth_zero() {
        let _data = data("pushed-on");
        let dir = scratch("pushed-on");
        let (endpoint, provider) = provider(vec![
            reply("", &[read_notes()]),
            reply("The notes say hello. Now let me check the tests.", &[]),
            reply("The notes say hello, and there are no tests.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 530);
        spec.depth = 0;
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        let asked = messages(&sent[2]).last().unwrap();
        assert_eq!(asked["content"], "Continue now.");
        assert_eq!(report.text, "The notes say hello, and there are no tests.");
    }

    #[test]
    fn an_answer_after_unchecked_edits_is_sent_back_to_verify_once_at_depth_zero() {
        let _data = data("verify");
        let dir = scratch("verify");
        let (endpoint, provider) = provider(vec![
            reply("", &[write_made()]),
            reply("Wrote made.txt.", &[]),
            reply("There is nothing here to build or test.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 540);
        spec.depth = 0;
        spec.decide = runs_everything();
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        let asked = messages(&sent[2]).last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(
            asked.contains("You changed files and no build or test has passed since"),
            "{asked}"
        );
        // Once: the answer after it ends the run.
        assert_eq!(report.text, "There is nothing here to build or test.");
    }

    #[test]
    fn a_helper_is_never_sent_back_to_verify() {
        // The settings ask for the check, so only the depth keeps it away.
        let _data = data("verify-helper");
        let dir = scratch("verify-helper");
        let (endpoint, provider) = provider(vec![
            reply("", &[write_made()]),
            reply("Wrote made.txt.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 550);
        spec.decide = runs_everything();
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        provider.sent();

        assert!(dir.join("made.rs").exists());
        assert_eq!(report.text, "Wrote made.txt.");
    }

    #[test]
    fn a_run_that_may_not_run_commands_is_never_sent_back_to_verify() {
        // The settings ask for the check, so only the tools allowed keep it away.
        let _data = data("verify-no-commands");
        let dir = scratch("verify-no-commands");
        let (endpoint, provider) = provider(vec![
            reply("", &[write_made()]),
            reply("Wrote made.txt.", &[]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 555);
        spec.depth = 0;
        spec.allow = Allow::Only(vec!["read_file".into(), "write_file".into()]);
        spec.decide = runs_everything();
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        provider.sent();

        assert!(dir.join("made.rs").exists());
        assert_eq!(report.text, "Wrote made.txt.");
    }

    #[test]
    fn calls_made_in_the_wrap_up_still_end_the_run_at_its_limit() {
        let dir = scratch("wrap-up");
        let (endpoint, provider) = provider(vec![
            reply("", &[read_notes()]),
            reply("", &[("list_dir", json!({"path": "."}))]),
            reply("Reading more.", &[read_notes()]),
        ]);
        let mut spec = spec(endpoint, &dir, AGENTS + 560);
        spec.rounds = 1;
        let report = finished(spec);
        assert_eq!(report.status, Outcome::RoundLimit);
        provider.sent();

        // Kept without the calls that never ran, so a resume finds none hanging.
        let last = written(&dir.join("helper.jsonl")).pop().unwrap();
        assert_eq!(last["content"], "Reading more.");
        assert!(last["tool_calls"].as_array().is_none_or(Vec::is_empty));
    }

    /// A session's own run in `dir`, asked to leave `out/summary.json`.
    fn asked_for_a_summary(endpoint: String, dir: &Path, id: AgentId) -> Spec {
        let mut spec = spec(endpoint, dir, id);
        spec.depth = 0;
        spec.transcript.conversation.messages[0].content =
            "Read notes.txt and write what it says to out/summary.json.".into();
        spec
    }

    #[test]
    fn a_missing_required_output_sends_the_run_back_once() {
        let dir = scratch("contract-missing");
        let (endpoint, provider) = provider(vec![reply("Done.", &[]), reply("Still done.", &[])]);
        let report = finished(asked_for_a_summary(endpoint, &dir, AGENTS + 490));
        // A third request would find no provider and fail the run.
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        let last = messages(&sent[1]).last().unwrap();
        assert_eq!(last["role"], "user");
        assert_eq!(
            last["content"],
            "[Letronna check] Before ending, compare these with the request:\n\
             - `out/summary.json` does not exist.\n\
             Fix the ones it asked for, in place; ignore any it did not ask for. \
             Only your last message is passed on as your answer, so end with your \
             whole answer again, not only what changed."
        );
        assert_eq!(report.contract, ["`out/summary.json` does not exist."]);
        // Folded in as a notice, so neither the window nor the judge takes
        // it for something the user typed.
        let saved: Vec<Value> = written(&dir.join("helper.jsonl"))
            .into_iter()
            .filter(|record| record["type"] == "message")
            .collect();
        let check = &saved[saved.iter().position(|m| m["role"] == "assistant").unwrap() + 1];
        assert_eq!(
            (&check["role"], &check["notice"], &check["folded"]),
            (&json!("user"), &json!(true), &json!(true))
        );
    }

    #[test]
    fn a_run_started_by_mail_is_not_held_to_the_files_the_mail_names() {
        let dir = scratch("contract-mail");
        let (endpoint, provider) = provider(vec![reply("Noted.", &[])]);
        let mut spec = asked_for_a_summary(endpoint, &dir, AGENTS + 520);
        let mail = &mut spec.transcript.conversation.messages[0];
        mail.content =
            "[Letronna notice: not from the user]\n- job 3: cannot write out/x.json".into();
        mail.notice = true;
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        assert_eq!(provider.sent().len(), 1);
        assert!(report.contract.is_empty());
    }

    #[test]
    fn a_run_whose_outputs_are_right_ends_without_a_check_notice() {
        let dir = scratch("contract-right");
        let summary = json!({"path": "out/summary.json", "content": r#"{"said": "hello"}"#});
        let (endpoint, provider) = provider(vec![
            reply("", &[("write_file", summary)]),
            reply("Done.", &[]),
            reply("Done; there is nothing here to build or test.", &[]),
        ]);
        let mut spec = asked_for_a_summary(endpoint, &dir, AGENTS + 500);
        // Edits in the project run, with nobody there to approve them.
        spec.connection.mode = crate::gate::Mode::AcceptEdits;
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        // The one note after "Done." is the reminder to check the edit (a
        // .json arms it), not a word on the outputs, which are right.
        let sent = provider.sent();
        assert_eq!(sent.len(), 3);
        let asked = messages(&sent[2]).last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(asked.contains("You changed files"), "{asked}");
        assert!(dir.join("out/summary.json").exists());
        assert!(report.contract.is_empty());
    }

    #[test]
    fn a_run_in_plan_mode_is_not_sent_back_for_a_missing_output() {
        let dir = scratch("contract-plan");
        let (endpoint, provider) = provider(vec![reply("Here is the plan.", &[])]);
        let mut spec = asked_for_a_summary(endpoint, &dir, AGENTS + 510);
        spec.connection.plan = true;
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        assert_eq!(provider.sent().len(), 1);
        assert!(report.contract.is_empty());
    }

    #[test]
    fn a_run_with_no_round_or_budget_left_is_not_sent_back_for_a_missing_output() {
        // Its one round went on reading the notes.
        let dir = scratch("contract-last-round");
        let (endpoint, last_round) = provider(vec![
            reply("", &[read_notes()]),
            reply("The notes say hello.", &[]),
        ]);
        let mut spec = asked_for_a_summary(endpoint, &dir, AGENTS + 900);
        spec.rounds = 1;
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        assert_eq!(report.text, "The notes say hello.");
        assert_eq!(last_round.sent().len(), 2);
        assert_eq!(report.contract, ["`out/summary.json` does not exist."]);

        // Each reply costs 1,050 tokens, so the answer spends the last of it.
        let dir = scratch("contract-spent");
        let (endpoint, spent) = provider(vec![
            reply("", &[read_notes()]),
            reply("The notes say hello.", &[]),
        ]);
        let mut spec = asked_for_a_summary(endpoint, &dir, AGENTS + 910);
        spec.budget = Some(Arc::new(Budget::new(2_000, None)));
        let report = finished(spec);
        assert_eq!(report.status, Outcome::Done);
        assert_eq!(spent.sent().len(), 2);
    }

    /// A session's own run in `dir`, asked to read `in/brief.md` and
    /// `in/data.json`, which it holds.
    fn asked_to_read_two(endpoint: String, dir: &Path, id: AgentId) -> Spec {
        std::fs::create_dir_all(dir.join("in")).unwrap();
        std::fs::write(dir.join("in/brief.md"), "The brief says hello.").unwrap();
        std::fs::write(dir.join("in/data.json"), "{}").unwrap();
        let mut spec = spec(endpoint, dir, id);
        spec.depth = 0;
        spec.transcript.conversation.messages[0].content =
            "Read in/brief.md and in/data.json, then say what they hold.".into();
        spec
    }

    /// The notice the last request of `sent` ends with, checked to be one.
    fn last_notice(sent: &[Value]) -> &str {
        let last = messages(sent.last().unwrap()).last().unwrap();
        assert_eq!(last["role"], "user");
        let notice = last["content"].as_str().unwrap();
        assert!(notice.starts_with("[Letronna notice"), "{notice}");
        notice
    }

    #[test]
    fn an_input_never_read_is_named_before_the_run_ends() {
        let dir = scratch("unread-input");
        let brief = ("read_file", json!({"path": "in/brief.md"}));
        let (endpoint, provider) = provider(vec![
            reply("", &[brief]),
            reply("The brief says hello.", &[]),
            reply("The data is empty, so the brief says it all: hello.", &[]),
        ]);
        let report = finished(asked_to_read_two(endpoint, &dir, AGENTS + 600));
        // A fourth request, a second notice, would find no provider.
        assert_eq!(report.status, Outcome::Done);
        assert_eq!(
            report.text,
            "The data is empty, so the brief says it all: hello."
        );
        let sent = provider.sent();

        assert_eq!(messages(&sent[1]).last().unwrap()["role"], "tool");
        let notice = last_notice(&sent);
        assert!(notice.contains("input files you have not read"), "{notice}");
        assert!(notice.ends_with("- `in/data.json`"), "{notice}");
        assert!(!notice.contains("brief"), "{notice}");
        // Folded into the run as a notice, so neither the window nor the
        // judge takes it for the user's words or a turn of their own.
        let saved = written(&dir.join("helper.jsonl"));
        let kept = saved
            .iter()
            .find(|record| record["content"].as_str() == Some(notice))
            .unwrap();
        assert_eq!(
            (&kept["notice"], &kept["folded"]),
            (&json!(true), &json!(true))
        );
    }

    #[test]
    fn an_input_read_by_a_command_counts_as_read() {
        let dir = scratch("inputs-by-command");
        let (endpoint, provider) = provider(vec![
            reply("", &[command("cat in/*.json")]),
            reply("The data is empty.", &[]),
            reply("The data is empty and the brief says hello.", &[]),
        ]);
        let report = finished(asked_to_read_two(endpoint, &dir, AGENTS + 610));
        assert_eq!(report.status, Outcome::Done);
        let notice = last_notice(&provider.sent()).to_owned();
        assert!(notice.ends_with("- `in/brief.md`"), "{notice}");
        assert!(!notice.contains("data.json"), "{notice}");
    }

    /// Checks that the run `asked_to_read_two` gives, as `change` leaves
    /// it, ends at its first answer with no notice.
    fn ends_untold(id: AgentId, change: impl FnOnce(&mut Spec)) {
        let dir = scratch(&format!("inputs-untold-{id}"));
        let (endpoint, provider) = provider(vec![reply("The brief says hello.", &[])]);
        let mut spec = asked_to_read_two(endpoint, &dir, id);
        change(&mut spec);
        assert_eq!(finished(spec).status, Outcome::Done, "{id}");
        assert_eq!(provider.sent().len(), 1, "{id}");
    }

    #[test]
    fn a_helper_an_editors_turn_or_a_chat_apps_run_is_not_told_of_unread_inputs() {
        ends_untold(AGENTS + 620, |spec| spec.depth = 1);
        ends_untold(AGENTS + 630, |spec| spec.watch = Some(Arc::new(|_| ())));
        ends_untold(AGENTS + 640, |spec| {
            spec.connection.remote = Some(Default::default());
        });
    }

    #[test]
    fn a_long_command_in_a_print_run_moves_to_the_background_and_reports_back() {
        let _bell = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = scratch("long-command");
        let id = AGENTS + 500;
        // As `-p` runs a prompt: the session's own agent, whose mail is its own.
        let mut top = spec(String::new(), &dir, id);
        (top.depth, top.owner) = (0, id);
        // It prints `after` well past the limit, which it could not do if it
        // had been stopped there.
        let command = if cfg!(windows) {
            "Write-Output before; Start-Sleep 3; Write-Output after"
        } else {
            "echo before; sleep 3; echo after"
        };
        let limit = Duration::from_secs(1);
        let said = tools::run_shell_in(command, &dir, &context(&top), limit).unwrap();
        let job: u64 = said
            .strip_prefix("Still running after 1s, so it carries on in the background as job #")
            .and_then(|rest| rest.split_once(' '))
            .and_then(|(job, _)| job.parse().ok())
            .unwrap_or_else(|| panic!("{said}"));
        // In before the run starts, so where the run reads it is known.
        let deadline = Instant::now() + WAIT;
        while !inbox::pending().contains(&id) {
            assert!(Instant::now() < deadline, "no exit notice for job #{job}");
            std::thread::sleep(Duration::from_millis(100));
        }
        let (endpoint, provider) = provider(vec![
            reply("", &[("wait", json!({"job": job, "seconds": 5}))]),
            reply("The build printed after.", &[]),
        ]);
        top.connection.endpoint = endpoint;
        let report = finished(top);
        assert_eq!(report.status, Outcome::Done);
        let sent = provider.sent();

        let waited = results(&sent[1]);
        let exited = format!("job #{job} exited with code 0");
        assert!(
            waited[0].starts_with(&format!("Job #{job} exited")),
            "{waited:?}"
        );
        let mail = messages(&sent[1]).last().unwrap()["content"]
            .as_str()
            .unwrap();
        assert!(mail.contains(&exited) && mail.contains("after"), "{mail}");
    }

    #[test]
    fn a_helpers_long_command_is_still_stopped_at_the_limit() {
        let dir = scratch("long-command-helper");
        // The command is this test's own, so no other test's job can pass for it.
        let command = if cfg!(windows) {
            "Start-Sleep 30; Write-Output driver-helper"
        } else {
            "sleep 30; echo driver-helper"
        };
        let helper = spec(String::new(), &dir, AGENTS + 510);
        let limit = Duration::from_secs(1);
        let out = tools::run_shell_in(command, &dir, &context(&helper), limit).unwrap_err();
        assert!(
            out.starts_with("Timed out after 1s and was stopped."),
            "{out}"
        );
        let jobs = crate::jobs::list();
        assert!(
            jobs.iter().all(|job| job.command != command),
            "it became a job"
        );
    }
}
