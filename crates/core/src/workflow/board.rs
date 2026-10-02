//! A workflow run as others see and steer it: each of its calls with its
//! label, phase, state and spend, and the pause, skip and retry asked of it.
//!
//! A run is driven from its own thread, which nothing else may touch, so a
//! control only marks the board under its lock and wakes the run, which acts
//! on the mark at its next turn. A pause holds queued agents back while the
//! running ones finish and answer. A skip gives the script null, stopping the
//! agent when it runs. A retry cannot give the script anything, as the script
//! has had its null already, so it works through the journal instead: it
//! drops the call's answer there, and the run's next resume runs it again.
//! Each of these, and each call's start and end, goes into the run's log.

use super::Arrival;
use super::journal::{self, Line};
use super::runlog::{Event, Log};
use crate::budget::Budget;
use crate::cancel::Cancel;
use crate::chat::Message;
use crate::driver::Status;
use async_channel::Sender;
use serde_json::Value;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// A run as its session sees it at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct RunView {
    /// Such as wf-3.
    pub name: String,
    /// Its meta's name.
    pub title: String,
    pub state: RunState,
    /// The phases its meta names, in order, then any others as its agents
    /// name them, each with its agents in the order they were called.
    pub phases: Vec<PhaseView>,
    /// Tokens it has spent over all its launches, and a guess at what its
    /// agents' replies still streaming come to, which the provider reports
    /// only once each ends.
    pub spent: u64,
    /// Tokens it may spend.
    pub budget: u64,
    /// How long this launch has run, or ran.
    pub seconds: u64,
}

/// One phase of a run and the agents called in it.
#[derive(Clone, Debug, PartialEq)]
pub struct PhaseView {
    /// Empty for agents called before any phase was named.
    pub title: String,
    pub agents: Vec<AgentView>,
}

/// One `agent()` call of a run.
#[derive(Clone, Debug, PartialEq)]
pub struct AgentView {
    /// Its place among the run's calls, from 1: `#3` names it to a skip or a
    /// retry when labels are not enough.
    pub number: usize,
    /// The label its call gave, or `wf-3 #3` for none.
    pub label: String,
    pub phase: String,
    pub state: AgentState,
    /// Tokens it has spent in this launch, and a guess at what its reply
    /// still streaming comes to; none when replayed.
    pub tokens: u64,
}

/// One `agent()` call of a run as it is opened: what it did and what it
/// answered.
#[derive(Clone)]
pub struct AgentTranscript {
    pub number: usize,
    pub label: String,
    pub state: AgentState,
    /// Its task, then what it said, the tools it called and what they gave
    /// back, as far as it has got: the task and the newest messages, each
    /// cut to its two ends when long. Empty for an agent that has not started
    /// in this launch, or whose replayed answer came from a launch that kept
    /// no transcript.
    pub messages: Vec<Message>,
    /// Messages left out between the task and the newest ones.
    pub cut: usize,
    /// Its final answer once it has one: its text, or the object its schema
    /// asked for.
    pub answer: Option<Value>,
}

/// How a run stands.
#[derive(Clone, Debug, PartialEq)]
pub enum RunState {
    /// Its agents are at work.
    Running,
    /// Held by a pause: running agents finish, and no queued one starts until
    /// it is resumed.
    Paused,
    /// Its script returned.
    Finished,
    /// It ended without finishing, as this word says -- "stopped", "failed",
    /// or "paused" over its budget or failures -- and a resume through the
    /// workflow tool picks it up.
    Ended(String),
}

/// How one call stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgentState {
    /// Queued for a permit, or held by a pause.
    Waiting,
    Running,
    /// It answered in this launch.
    Done,
    /// An earlier launch answered it, and the journal gave that back.
    Replayed,
    /// It ended without an answer: an error, or out of rounds or budget.
    Failed,
    /// Stopped with its run.
    Stopped,
    /// A skip gave the script null for it.
    Skipped,
    /// A retry set it to run again at the run's next resume.
    Retry,
}

impl AgentState {
    /// The state in a word or two, as a list shows it.
    pub fn word(self) -> &'static str {
        match self {
            Self::Waiting => "waiting",
            Self::Running => "running",
            Self::Done => "done",
            Self::Replayed => "replayed",
            Self::Failed => "failed",
            Self::Stopped => "stopped",
            Self::Skipped => "skipped",
            Self::Retry => "to retry",
        }
    }

    /// Whether the script had an answer for it.
    pub fn answered(self) -> bool {
        matches!(self, Self::Done | Self::Replayed)
    }
}

/// What a queued call is told as its turn to start comes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Go {
    Start,
    /// A skip reached it first.
    Skip,
    /// A pause holds it back.
    Wait,
}

/// A run's calls and the controls asked of it, shared by the run's thread and
/// whoever lists or steers the run.
pub(super) struct Board {
    shared: Mutex<Shared>,
    /// Wakes the run from its wait, so a control is acted on at once.
    wake: Sender<Arrival>,
    /// Written under the board's lock, so events land in the order they
    /// happened.
    log: Mutex<Log>,
}

/// One call as it is looked up to be opened.
pub(super) struct Called {
    pub(super) label: String,
    /// Its journal key.
    pub(super) key: String,
    pub(super) state: AgentState,
    /// Whether it started in this launch, rather than coming from the journal
    /// or being answered before it could start.
    pub(super) began: bool,
}

#[derive(Default)]
struct Shared {
    /// Set by a pause: no queued agent starts while it is.
    held: bool,
    /// The meta's phases, then the others as calls name them.
    phases: Vec<String>,
    /// By slot.
    calls: Vec<Call>,
}

struct Call {
    label: String,
    phase: String,
    /// Its journal key.
    key: String,
    state: AgentState,
    /// What it is charged on, once it starts.
    charged: Option<Arc<Budget>>,
    /// How its reply now streaming has got on, once it starts.
    status: Option<Arc<Status>>,
    /// Stops it, once it starts.
    cancel: Option<Cancel>,
    /// Set by a skip, until the run acts on it.
    skip: bool,
}

impl Board {
    pub(super) fn new(wake: Sender<Arrival>, log: Log) -> Self {
        Self {
            shared: Mutex::default(),
            wake,
            log: Mutex::new(log),
        }
    }

    fn shared(&self) -> MutexGuard<'_, Shared> {
        self.shared.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Logs `event` for the run, or for the call in `slot`, because of `why`.
    pub(super) fn note(&self, event: Event, slot: Option<usize>, why: &str) {
        let shared = self.shared();
        self.write(&shared, event, slot, why);
    }

    /// Logs `event` as `note` does, for a caller holding the lock.
    fn write(&self, shared: &Shared, event: Event, slot: Option<usize>, why: &str) {
        let agent = slot.and_then(|slot| Some((slot + 1, shared.calls.get(slot)?.label.as_str())));
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        log.note(event, agent, why);
    }

    /// Names the meta's phases, which show in that order before any agent runs.
    pub(super) fn declare(&self, phases: Vec<String>) {
        self.shared().phases = phases;
    }

    /// Takes the next call, which is `state` from the start: waiting, or
    /// replayed or skipped by the journal.
    pub(super) fn add(&self, label: String, phase: String, key: String, state: AgentState) {
        let mut shared = self.shared();
        if !shared.phases.contains(&phase) {
            shared.phases.push(phase.clone());
        }
        shared.calls.push(Call {
            label,
            phase,
            key,
            state,
            charged: None,
            status: None,
            cancel: None,
            skip: false,
        });
    }

    /// Whether the queued call in `slot` may start, marking it running,
    /// stopped by `cancel`, charged on `charged` and streaming as `status`
    /// says, when it may: asked under the lock a pause and a skip take, so
    /// neither can land between the asking and the start.
    pub(super) fn begin(
        &self,
        slot: usize,
        cancel: &Cancel,
        charged: &Arc<Budget>,
        status: &Arc<Status>,
    ) -> Go {
        let mut shared = self.shared();
        let held = shared.held;
        let Some(call) = shared.calls.get_mut(slot) else {
            return Go::Start;
        };
        if call.skip {
            return Go::Skip;
        }
        if held {
            return Go::Wait;
        }
        call.state = AgentState::Running;
        call.cancel = Some(cancel.clone());
        call.charged = Some(charged.clone());
        call.status = Some(status.clone());
        self.write(&shared, Event::Started, Some(slot), "");
        Go::Start
    }

    /// Marks how the call in `slot` ended, logging it with `why`.
    pub(super) fn end(&self, slot: usize, state: AgentState, why: &str) {
        let mut shared = self.shared();
        let Some(call) = shared.calls.get_mut(slot) else {
            return;
        };
        call.state = state;
        call.cancel = None;
        let event = match state {
            AgentState::Done => Event::Finished,
            AgentState::Failed => Event::Failed,
            AgentState::Stopped => Event::Stopped,
            AgentState::Skipped => Event::Skipped,
            _ => return,
        };
        self.write(&shared, event, Some(slot), why);
    }

    /// Whether a pause holds the run's queued agents back.
    pub(super) fn held(&self) -> bool {
        self.shared().held
    }

    /// Whether a skip was asked for the call in `slot`.
    pub(super) fn skipping(&self, slot: usize) -> bool {
        self.shared().calls.get(slot).is_some_and(|call| call.skip)
    }

    /// The waiting calls a skip was asked for.
    pub(super) fn skips(&self) -> Vec<usize> {
        let shared = self.shared();
        let calls = shared.calls.iter().enumerate();
        let asked = calls.filter(|(_, call)| call.skip && call.state == AgentState::Waiting);
        asked.map(|(slot, _)| slot).collect()
    }

    /// The call `agent` names among run `name`'s -- its label, or `#N` --
    /// as it stands now.
    pub(super) fn call(&self, name: &str, agent: &str) -> Result<Called, String> {
        let shared = self.shared();
        let slot = find(&shared.calls, name, agent)?;
        let call = &shared.calls[slot];
        Ok(Called {
            label: call.label.clone(),
            key: call.key.clone(),
            state: call.state,
            began: call.charged.is_some(),
        })
    }

    /// Calls taken, calls answered, and calls that failed.
    pub(super) fn counts(&self) -> (usize, usize, usize) {
        let shared = self.shared();
        let states = || shared.calls.iter().map(|call| call.state);
        let answered = states().filter(|state| state.answered()).count();
        let failed = states()
            .filter(|state| *state == AgentState::Failed)
            .count();
        (shared.calls.len(), answered, failed)
    }

    /// The run's phases, each with its agents as they stand.
    pub(super) fn phases(&self) -> Vec<PhaseView> {
        let shared = self.shared();
        let agents: Vec<AgentView> = (shared.calls.iter().enumerate())
            .map(|(slot, call)| AgentView {
                number: slot + 1,
                label: call.label.clone(),
                phase: call.phase.clone(),
                state: call.state,
                tokens: call.charged.as_ref().map_or(0, |charged| charged.spent())
                    + streaming(call),
            })
            .collect();
        let phases = shared.phases.iter().map(|title| PhaseView {
            title: title.clone(),
            agents: (agents.iter())
                .filter(|agent| agent.phase == *title)
                .cloned()
                .collect(),
        });
        phases.collect()
    }

    /// What the replies still streaming come to, as `streaming` guesses it.
    pub(super) fn streaming(&self) -> u64 {
        self.shared().calls.iter().map(streaming).sum()
    }

    /// Holds the queued agents of run `name` back; refused when a pause
    /// already does.
    pub(super) fn pause(&self, name: &str) -> Result<(), String> {
        let mut shared = self.shared();
        if shared.held {
            return Err(format!("{name} is paused already."));
        }
        shared.held = true;
        self.write(&shared, Event::Paused, None, "asked for by the user");
        Ok(())
    }

    /// Lets the queued agents of run `name` start again; refused when no
    /// pause holds them.
    pub(super) fn resume(&self, name: &str) -> Result<(), String> {
        let mut shared = self.shared();
        if !shared.held {
            return Err(format!("{name} is not paused."));
        }
        shared.held = false;
        self.write(&shared, Event::Resumed, None, "");
        drop(shared);
        self.poke();
        Ok(())
    }

    /// Asks run `name` to give its script null for `agent`, stopping it if it
    /// runs; refused unless it is waiting or running. One that answers before
    /// the stop reaches it keeps its answer.
    pub(super) fn skip(&self, name: &str, agent: &str) -> Result<(), String> {
        let mut shared = self.shared();
        let slot = find(&shared.calls, name, agent)?;
        let call = &mut shared.calls[slot];
        let label = &call.label;
        match call.state {
            AgentState::Waiting | AgentState::Running if call.skip => {
                return Err(format!("{label} is being skipped already."));
            }
            AgentState::Waiting | AgentState::Running => {}
            state if state.answered() => {
                return Err(format!("{label} has answered; there is nothing to skip."));
            }
            state => {
                let word = state.word();
                return Err(format!(
                    "{label} has ended ({word}); there is nothing to skip."
                ));
            }
        }
        call.skip = true;
        let cancel = call.cancel.clone();
        drop(shared);
        // Set outside the lock, as a stop runs whatever the agent waits on.
        if let Some(cancel) = cancel {
            cancel.set();
        }
        self.poke();
        Ok(())
    }

    /// Sets `agent` of run `name`, which failed, stopped or was skipped, to run
    /// again at the run's next resume: appends a `retry` line for its key to
    /// the journal at `path`, which drops whatever answer the key has there (a
    /// skip's null), so the resume runs it live, and every later call whose
    /// prompt embeds its answer gets a new key and runs live after it. The
    /// script has had null for it, so a run still going goes on without it.
    pub(super) fn retry(&self, name: &str, agent: &str, path: &Path) -> Result<(), String> {
        let mut shared = self.shared();
        let slot = find(&shared.calls, name, agent)?;
        let call = &mut shared.calls[slot];
        let label = &call.label;
        match call.state {
            AgentState::Failed | AgentState::Stopped | AgentState::Skipped => {}
            AgentState::Retry => {
                return Err(format!("{label} runs again at the next resume already."));
            }
            state if state.answered() => {
                return Err(format!(
                    "{label} answered; only an agent that failed, stopped or was skipped runs \
                     again."
                ));
            }
            state => {
                let word = state.word();
                return Err(format!(
                    "{label} is {word}; skip it first, then retry it for the next resume."
                ));
            }
        }
        // The run writes its line for a call before it marks the call ended,
        // so this one comes after it and has the last word.
        let key = call.key.clone();
        journal::append(path, &Line::Retry { key })?;
        call.state = AgentState::Retry;
        self.write(
            &shared,
            Event::Retried,
            Some(slot),
            "runs again at the next resume",
        );
        Ok(())
    }

    /// Wakes the run, which acts on the board at its next turn.
    fn poke(&self) {
        let _ = self.wake.try_send(Arrival::Wake);
    }
}

/// Tokens `call`'s reply still streaming has come to so far, at four
/// characters a token: the provider says only once the reply ends, which for
/// a long answer can be minutes. Shown, never charged.
fn streaming(call: &Call) -> u64 {
    let status = call.status.as_ref();
    status.map_or(0, |status| status.streamed.load(Ordering::Relaxed)) / 4
}

/// The slot of the call `agent` names among run `name`'s `calls`: its label,
/// or `#N` for its Nth call.
fn find(calls: &[Call], name: &str, agent: &str) -> Result<usize, String> {
    let agent = agent.trim();
    if let Some(number) = agent
        .strip_prefix('#')
        .and_then(|n| n.parse::<usize>().ok())
    {
        if number == 0 || number > calls.len() {
            let made = calls.len();
            return Err(format!(
                "{name} has no agent #{number}: it has made {made} calls."
            ));
        }
        return Ok(number - 1);
    }
    let named: Vec<usize> = (calls.iter().enumerate())
        .filter(|(_, call)| call.label == agent)
        .map(|(slot, _)| slot)
        .collect();
    match named[..] {
        [slot] => Ok(slot),
        [] => Err(format!("{name} has no agent labelled \"{agent}\".")),
        _ => {
            let numbers: Vec<String> = named.iter().map(|slot| format!("#{}", slot + 1)).collect();
            Err(format!(
                "{} agents of {name} are labelled \"{agent}\"; name one by its number: {}.",
                named.len(),
                numbers.join(", ")
            ))
        }
    }
}
