//! Workflow runs: many agents started by one script, kept in a journal so a
//! run that stops part way is picked back up where it was.
//!
//! A run is a folder, `workflows/wf-N` in the data folder, holding its
//! journal, its lock and each agent's transcript. The script's host drives it
//! from one thread: `agent` only queues a call, and `step` starts what the
//! run's permits allow, then blocks on the one channel every agent reports on
//! as it ends, so nothing polls. A nested workflow goes through the same run,
//! and so shares its permits, its count of calls, its journal, its budget and
//! its stop.
//!
//! On a resume, a call whose key has an answer in the journal gets it back at
//! no cost, whatever order the calls come in; one that was in flight at a
//! kill, or failed, runs again. Keys see only what an agent was told, which
//! misses one thing: what an agent that can write changes on disk. So once
//! one of those runs live, every agent started after it runs live too; and
//! when a later resume replays it, an answer after it that is older than its
//! own is not replayed, as it was found before what it wrote.
//!
//! Three failures in a row with the same error -- a bad key, a provider that
//! is down, credits run out -- pause the run and tell its session at once,
//! rather than let every agent after them fail the same way. The lock is made
//! with create_new and touched every 30 s from the host's own wait, so it is
//! kept fresh only while the run is really being driven, and one nobody has
//! touched for two minutes belongs to a run that is gone and can be taken over.

mod journal;

use crate::agent_defs::{self, AgentDef};
use crate::budget::{Budget, short};
use crate::cancel::Cancel;
use crate::chat::{self, Connection, Message};
use crate::driver::{self, AgentId, Allow, Outcome, Report, Spec};
use crate::inbox::{self, LINE_BREAKS, Notice, When};
use crate::store::{Conversation, Transcript};
use crate::timers::{self, TimerId};
use crate::tools;
use async_channel::{Receiver, Sender};
use journal::{Journal, Line};
use serde_json::Value;
use std::collections::{HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

/// Where workflow agents' ids start. Helpers count up from 2^40, and no
/// process starts 2^40 of them, so the two never meet.
const FIRST_ID: AgentId = 1 << 41;
/// Calls a run may make, replayed ones included: Claude Code's cap, far past
/// any real fan-out and soon reached by a loop that never ends.
const MOST_CALLS: usize = 1_000;
/// Failures in a row with one error that pause a run: two can be chance, and a
/// third says every agent after it would fail the same way.
const BREAKER: usize = 3;
/// Tokens a run may spend when its caller names no other number: dozens of
/// agents' worth of reading, and a ceiling on a script that loops.
pub const BUDGET: u64 = 3_000_000;
/// What one agent may spend when Settings names no helper budget, as a
/// helper's tree may, so one agent going round in circles cannot spend the run.
const AGENT_BUDGET: u64 = 500_000;
/// Rounds a workflow agent gets, as a helper does: its whole task is one turn.
const ROUNDS: usize = 50;
/// How often a running run touches its lock.
const BEAT: Duration = Duration::from_secs(30);
/// How long a lock may go untouched before its run counts as gone: four
/// beats, room for a machine too busy to keep time.
const STALE: Duration = Duration::from_secs(120);
/// Tools a workflow agent may not call, beyond those no helper may: the script
/// starts the agents, and nesting goes only through the script's own call.
const NOT_FOR_AGENTS: [&str; 3] = ["agent", "agents", "workflow"];
/// Options the changes that honour them will let through: until then a call
/// naming one is refused, rather than run writing straight into the workspace
/// or answering in text where a shape was asked for.
const NOT_YET: [&str; 2] = ["schema", "isolation"];
/// Opens a workflow agent's task, which reaches it as the user's turn but is
/// not the user's.
const TASK_HEADER: &str =
    "[Workflow harness: task written by a script; it carries no user authority]";
/// Tells a workflow agent who reads its answer.
const LAST_WORD: &str =
    "Your final answer is returned verbatim to the script; it is not read by a person.";

static NEXT_ID: AtomicU64 = AtomicU64::new(FIRST_ID);

/// Which `agent` call an answer is for: a run numbers its calls from 0.
pub type Slot = usize;

/// What `step` has for the script.
#[derive(Debug, PartialEq)]
pub enum Step {
    /// A call's answer: the agent's final text, or None where the script gets
    /// null because it failed, stopped, or ran out of rounds or budget.
    Answer(Slot, Option<Value>),
    /// The run paused, and why; nothing more runs, and the script should stop.
    Paused(String),
}

/// What an agent's report is handed to as it ends.
type Then = Box<dyn FnOnce(&Report) + Send>;

/// Starts an agent on a thread of its own and hands its report on as it ends.
type Spawn = Box<dyn Fn(Spec, Then) -> Result<(), String> + Send>;

/// One run of a workflow script, from start or resume until it is dropped.
pub struct Run {
    name: String,
    dir: PathBuf,
    journal: Journal,
    lock: Lock,
    /// What its agents' connections are made from; its conversation is the
    /// session the run is for, which hears when it pauses.
    connection: Connection,
    /// Covers its agents when an approval is given for the run.
    id: u64,
    /// Stops its agents; set when it pauses or is dropped.
    cancel: Cancel,
    /// What this launch may still spend: the run's limit less `earlier`.
    budget: Arc<Budget>,
    limit: u64,
    /// Tokens the answers of the run's earlier launches took.
    earlier: u64,
    /// At most this many agents run at once.
    permits: usize,
    spawn: Spawn,
    /// Answers earlier launches of the run left, by key, with the launch each
    /// came from.
    answers: HashMap<String, (Value, usize)>,
    /// How many calls so far had each prompt and options.
    seen: HashMap<(String, String), u64>,
    calls: usize,
    /// Set once an agent that can write runs live, after which nothing replays.
    live: bool,
    /// The newest launch among the answers of agents that can write replayed
    /// so far: an older answer after them was found before what they wrote.
    floor: usize,
    queue: VecDeque<Queued>,
    /// Answers the script has not been given yet.
    ready: VecDeque<(Slot, Option<Value>)>,
    running: usize,
    /// The number of the last agent transcript in its folder.
    transcripts: u64,
    arrivals: (Sender<Arrival>, Receiver<Arrival>),
    beat: TimerId,
    /// The last failure and how many agents in a row have ended with it.
    streak: Option<(String, usize)>,
    paused: Option<String>,
    /// Set once `step` has said the run paused.
    told: bool,
}

/// A call waiting for a permit: a plain struct rather than a thread.
struct Queued {
    slot: Slot,
    key: String,
    prompt: String,
    def: AgentDef,
    asked: Options,
}

/// What a call's options ask of its agent.
struct Options {
    /// The helper type it names, if any.
    kind: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    label: String,
    phase: String,
}

/// What comes in on a run's channel.
enum Arrival {
    Ended {
        slot: Slot,
        key: String,
        ended: Ended,
    },
    /// Time to touch the lock.
    Beat,
}

/// How an agent came out, as the journal and the breaker see it.
enum Ended {
    /// It answered, taking this many tokens.
    Answered(String, u64),
    /// It failed with this error, which the breaker counts.
    Failed(String),
    /// It used its rounds or its budget: it runs again on a resume, and says
    /// nothing about the agents after it.
    Unfinished,
    /// It was stopped, which leaves it to run again on a resume.
    Stopped,
}

impl Run {
    /// Starts a new run of `script` with `args`, or picks up the run named by
    /// `resume`, for the session `connection` belongs to; its agents work on
    /// copies of `connection` and spend at most `budget` tokens between them.
    pub fn open(
        resume: Option<&str>,
        connection: &Connection,
        script: &str,
        args: &Value,
        budget: u64,
        cancel: &Cancel,
    ) -> Result<Self, String> {
        let folder = chat::data_dir().join("workflows");
        let (name, dir) = match resume {
            Some(name) => existing(&folder, name)?,
            None => reserve(&folder)?,
        };
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let lock = Lock::take(&dir, &name, id)?;
        std::fs::create_dir_all(dir.join("agents"))
            .map_err(|e| format!("Could not make a place for the workflow's agents: {e}"))?;
        let (mut journal, lines) = Journal::open(&dir.join("journal.jsonl"))?;
        journal.write(&Line::Launched {
            script_sha256: journal::sha256(script),
            args: args.clone(),
        })?;
        let arrivals = async_channel::unbounded();
        let beat = timers::at(Instant::now() + BEAT, ring(&arrivals.0));
        // The budget is the run's, not a launch's: a resume only raises it
        // by naming a larger one.
        let earlier = journal::spent(&lines);
        Ok(Self {
            transcripts: last_transcript(&dir.join("agents")),
            name,
            dir,
            journal,
            lock,
            connection: connection.clone(),
            id,
            cancel: cancel.child(),
            budget: Arc::new(Budget::new(budget.saturating_sub(earlier), None)),
            limit: budget,
            earlier,
            permits: permits(),
            spawn: Box::new(|spec, then| driver::spawn(spec, then).map(drop)),
            answers: journal::answers(&lines),
            seen: HashMap::new(),
            calls: 0,
            live: false,
            floor: 0,
            queue: VecDeque::new(),
            ready: VecDeque::new(),
            running: 0,
            arrivals,
            beat,
            streak: None,
            paused: None,
            told: false,
        })
    }

    /// The run's name, such as wf-3.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Takes a call of the script's for an agent, and gives the slot its
    /// answer will come back under from `step`; refused when the call is
    /// malformed, the run has made its most calls, or its budget is spent.
    pub fn agent(&mut self, prompt: &str, options: &Value) -> Result<Slot, String> {
        if let Some(reason) = &self.paused {
            return Err(format!("Workflow {} is paused: {reason}", self.name));
        }
        if self.cancel.is_set() {
            return Err(format!("Workflow {} was stopped.", self.name));
        }
        if prompt.trim().is_empty() {
            return Err(
                "agent() needs a prompt: the whole task, as the helper sees nothing else.".into(),
            );
        }
        let asked = Options::read(options)?;
        let def = match &asked.kind {
            Some(kind) => agent_defs::find(kind, self.connection.workspace.as_deref())?,
            None => workflow_agent(),
        };
        if self.calls >= MOST_CALLS {
            return Err(format!(
                "Workflow agent() call cap reached ({MOST_CALLS}). A loop over budget.remaining() \
                 probably never ends."
            ));
        }
        self.spendable()?;
        let canonical = journal::canonical(options);
        let seen = self
            .seen
            .entry((prompt.to_owned(), canonical.clone()))
            .or_default();
        let key = journal::key(prompt, &canonical, *seen);
        *seen += 1;
        let slot = self.calls;
        self.calls += 1;
        let writes = !(self.connection.plan || def.plan);
        if !self.live
            && let Some((value, launch)) = self.answers.get(&key)
            && *launch >= self.floor
        {
            if writes {
                self.floor = *launch;
            }
            // Given out by `step`, never by this call, so the script's awaits
            // behave as they do for an agent that runs.
            self.ready.push_back((slot, Some(value.clone())));
            return Ok(slot);
        }
        // What it writes may change what any agent after it would find.
        self.live |= writes;
        self.queue.push_back(Queued {
            slot,
            key,
            prompt: prompt.to_owned(),
            def,
            asked,
        });
        Ok(slot)
    }

    /// What the script hears next: an answer, or that the run paused. Starts
    /// queued agents as permits free up and waits for one to end when there is
    /// nothing else; None once nothing is left to answer.
    pub fn step(&mut self) -> Option<Step> {
        loop {
            if let Some(reason) = self.paused.clone() {
                // Its agents were stopped as it paused; it waits for them to
                // end, so none is still writing when it is resumed.
                while self.running > 0 {
                    self.wait();
                }
                return (!std::mem::replace(&mut self.told, true)).then_some(Step::Paused(reason));
            }
            self.fill();
            if let Some((slot, value)) = self.ready.pop_front() {
                return Some(Step::Answer(slot, value));
            }
            if self.running == 0 {
                return None;
            }
            self.wait();
        }
    }

    /// Starts queued agents while permits are free; once the run is stopped,
    /// answers them with null instead.
    fn fill(&mut self) {
        while self.running < self.permits && self.paused.is_none() {
            let Some(queued) = self.queue.pop_front() else {
                return;
            };
            if self.cancel.is_set() {
                self.ready.push_back((queued.slot, None));
                continue;
            }
            if self.spendable().is_err() {
                return;
            }
            let (slot, key) = (queued.slot, queued.key.clone());
            match self.start(queued) {
                Ok(()) => self.running += 1,
                Err(error) => self.record(slot, key, Ended::Failed(error)),
            }
        }
    }

    /// Sets one queued agent off on a thread of its own, to report on the run's channel.
    fn start(&mut self, queued: Queued) -> Result<(), String> {
        let Queued {
            slot,
            key,
            prompt,
            def,
            asked,
        } = queued;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let (settings, _) = crate::store::settings();
        let helper_model = (!settings.helper_model.is_empty()).then_some(settings.helper_model);
        let connection = Connection {
            model: (asked.model.or(def.model.clone()).or(helper_model))
                .unwrap_or_else(|| self.connection.model.clone()),
            effort: (asked.effort.or(def.effort.clone()))
                .unwrap_or_else(|| self.connection.effort.clone()),
            // Only ever narrowed, as a helper's is.
            plan: self.connection.plan || def.plan,
            conversation: id,
            ..self.connection.clone()
        };
        let tools = usable(&connection, &def);
        let label = if asked.label.is_empty() {
            format!("{} #{}", self.name, slot + 1)
        } else {
            asked.label.clone()
        };
        self.transcripts += 1;
        let transcript = Transcript {
            path: self
                .dir
                .join("agents")
                .join(format!("{}.jsonl", self.transcripts)),
            conversation: Conversation {
                id: self.connection.conversation,
                title: label.clone(),
                updated: chat::now(),
                workspace: self.connection.workspace.clone(),
                messages: vec![task(&def, &tools, &prompt)],
                ..Default::default()
            },
        };
        self.journal.write(&Line::Started {
            key: key.clone(),
            label: asked.label,
            phase: asked.phase,
        })?;
        let agent_budget = match settings.helper_budget {
            0 => AGENT_BUDGET,
            n => n,
        };
        let charged = Arc::new(Budget::new(agent_budget, Some(self.budget.clone())));
        let spec = Spec {
            connection,
            transcript,
            id,
            owner: self.connection.conversation,
            root: self.connection.conversation,
            run: self.id,
            label,
            allow: Allow::Only(tools),
            rounds: def.rounds.unwrap_or(ROUNDS),
            cancel: self.cancel.child(),
            budget: Some(charged.clone()),
            status: Arc::default(),
            depth: 1,
            fork: false,
            from: 0,
            schema: None,
            watch: None,
        };
        let owed = Owed {
            arrivals: self.arrivals.0.clone(),
            slot,
            key: Some(key),
            charged,
        };
        (self.spawn)(spec, Box::new(move |report| owed.pay(report)))
    }

    /// Waits for what comes next on the run's channel and deals with it.
    fn wait(&mut self) {
        // The run holds a sender itself, so the channel never closes under it.
        let Ok(arrival) = self.arrivals.1.recv_blocking() else {
            self.running = 0;
            return;
        };
        match arrival {
            Arrival::Beat => {
                self.lock.touch();
                self.beat = timers::at(Instant::now() + BEAT, ring(&self.arrivals.0));
            }
            Arrival::Ended { slot, key, ended } => {
                self.running -= 1;
                self.record(slot, key, ended);
            }
        }
    }

    /// Writes how an agent ended into the journal, readies its answer, and
    /// pauses the run at the third failure in a row with one error.
    fn record(&mut self, slot: Slot, key: String, ended: Ended) {
        // A line that cannot be written costs only a rerun on the next resume,
        // so the answer still goes to the script.
        let value = match ended {
            Ended::Answered(text, tokens) => {
                self.streak = None;
                let value = Value::String(text);
                let _ = self.journal.write(&Line::Result {
                    key,
                    value: value.clone(),
                    tokens,
                });
                Some(value)
            }
            Ended::Failed(error) => {
                let _ = self.journal.write(&Line::Failed { key });
                let count = match &self.streak {
                    Some((same, count)) if *same == error => count + 1,
                    _ => 1,
                };
                if count >= BREAKER {
                    self.pause(format!(
                        "{BREAKER} helpers in a row failed with the same error: {error}"
                    ));
                }
                self.streak = Some((error, count));
                None
            }
            Ended::Unfinished => {
                let _ = self.journal.write(&Line::Failed { key });
                None
            }
            Ended::Stopped => None,
        };
        self.ready.push_back((slot, value));
    }

    /// Refuses another agent once the run's budget is spent, and pauses the run.
    fn spendable(&mut self) -> Result<(), String> {
        if self.budget.check().is_ok() {
            return Ok(());
        }
        let spent = format!(
            "{} of {} tokens",
            short(self.earlier + self.budget.spent()),
            short(self.limit)
        );
        self.pause(format!("its budget is spent ({spent})"));
        Err(format!("The workflow budget is spent ({spent})."))
    }

    /// Stops the run's agents, drops what is queued, says why in the journal,
    /// and tells the session at once.
    fn pause(&mut self, reason: String) {
        if self.paused.is_some() {
            return;
        }
        self.cancel.set();
        self.queue.clear();
        let _ = self.journal.write(&Line::Paused {
            reason: reason.clone(),
        });
        let name = &self.name;
        // A resume counts what the run already spent, so one with a spent
        // budget pauses again at once unless it names a larger one.
        let resume = if self.budget.check().is_err() {
            format!(
                "a larger budget, such as workflow {{\"resume\":\"{name}\",\"budget\":{}}}",
                self.limit.saturating_mul(2)
            )
        } else {
            format!("workflow {{\"resume\":\"{name}\"}}")
        };
        inbox::push(
            self.connection.conversation,
            Notice {
                from: format!("workflow {name}"),
                when: When::Next,
                text: format!(
                    "workflow {name} paused: {reason}\nWhat finished is kept: resume it with \
                     {resume}."
                ),
            },
        );
        self.paused = Some(reason);
    }
}

impl Drop for Run {
    fn drop(&mut self) {
        timers::cancel(self.beat);
        // Agents still running have nobody left to answer.
        self.cancel.set();
    }
}

impl Options {
    /// Reads a call's options, refusing any that are not an object or name
    /// something other than text where text belongs.
    fn read(options: &Value) -> Result<Self, String> {
        if !options.is_object() && !options.is_null() {
            return Err("agent() options must be an object, such as { label: \"...\" }.".into());
        }
        if let Some(name) = NOT_YET.iter().find(|name| !options[**name].is_null()) {
            return Err(format!("agent() option `{name}` is not supported yet."));
        }
        let text = |name: &str| match &options[name] {
            Value::Null => Ok(None),
            Value::String(text) => Ok(Some(text.trim().to_owned()).filter(|t| !t.is_empty())),
            _ => Err(format!("agent() option `{name}` must be a string.")),
        };
        Ok(Self {
            kind: text("type")?.or(text("agentType")?),
            model: text("model")?,
            effort: text("effort")?,
            label: text("label")?.unwrap_or_default(),
            phase: text("phase")?.unwrap_or_default(),
        })
    }
}

/// A run's claim on its folder, so two Letronnas never drive one run at once.
struct Lock {
    path: PathBuf,
    /// What this claim wrote into the file, telling it from a later one.
    token: String,
}

impl Lock {
    /// Claims run `name`'s folder, taking over a lock nobody has touched for
    /// a while, and refusing while its run is still going.
    fn take(dir: &Path, name: &str, id: u64) -> Result<Self, String> {
        let path = dir.join("lock");
        let token = format!("{} {id}", std::process::id());
        let running = || {
            format!(
                "Workflow {name} is still running. Stop it first with agents \
                 {{\"action\":\"stop\",\"to\":\"{name}\"}}."
            )
        };
        // A second go only after taking a stale lock away.
        for _ in 0..2 {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    if let Err(e) = file.write_all(token.as_bytes()) {
                        // Left behind, it would keep the run out for minutes.
                        let _ = std::fs::remove_file(&path);
                        return Err(format!("Could not lock workflow {name}: {e}"));
                    }
                    return Ok(Self { path, token });
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !stale(&path) {
                        return Err(running());
                    }
                    // Two taking over one stale lock in the same instant could
                    // both get it; resuming one run twice at once is not worth
                    // a lock server to rule out.
                    let _ = std::fs::remove_file(&path);
                }
                Err(e) => return Err(format!("Could not lock workflow {name}: {e}")),
            }
        }
        Err(running())
    }

    /// Marks the lock as still held.
    fn touch(&self) {
        let _ = std::fs::File::options()
            .write(true)
            .open(&self.path)
            .and_then(|file| file.set_modified(SystemTime::now()));
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Only its own: a run taken over as stale while it was still going
        // must not free the lock of the run that took over.
        if std::fs::read_to_string(&self.path).is_ok_and(|held| held == self.token) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Whether the lock at `path` has gone untouched long enough that its run is gone.
fn stale(path: &Path) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map(|touched| touched.elapsed().unwrap_or_default() >= STALE)
        .unwrap_or(true)
}

/// A beat on the run's channel, from the timer thread, which only sends.
fn ring(arrivals: &Sender<Arrival>) -> Box<dyn FnOnce() + Send> {
    let arrivals = arrivals.clone();
    Box::new(move || {
        let _ = arrivals.try_send(Arrival::Beat);
    })
}

/// Takes the first free run number in `folder`: making the run's folder is
/// what claims it, so two Letronnas starting runs at once get different ones.
fn reserve(folder: &Path) -> Result<(String, PathBuf), String> {
    std::fs::create_dir_all(folder)
        .map_err(|e| format!("Could not make a place for workflow runs: {e}"))?;
    for n in 1..=u32::MAX {
        let name = format!("wf-{n}");
        let dir = folder.join(&name);
        match std::fs::create_dir(&dir) {
            Ok(()) => return Ok((name, dir)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("Could not start a workflow run: {e}")),
        }
    }
    Err("No numbers are left for another workflow run.".into())
}

/// The folder of the run called `name`, refused unless it is one.
fn existing(folder: &Path, name: &str) -> Result<(String, PathBuf), String> {
    let numbered = name
        .strip_prefix("wf-")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    let dir = folder.join(name);
    if !numbered || !dir.join("journal.jsonl").is_file() {
        return Err(format!(
            "There is no workflow run \"{name}\"; runs are named wf-1, wf-2 and so on."
        ));
    }
    Ok((name.to_owned(), dir))
}

/// The highest number among the transcripts in `dir`, or 0 when it has none.
fn last_transcript(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name();
            name.to_str()?.strip_suffix(".jsonl")?.parse().ok()
        })
        .max()
        .unwrap_or(0)
}

/// Agents a run lets run at once: two cores left for the window and the
/// system, never fewer than two, and no more than sixteen, past which a
/// provider's own limit is what holds them back.
fn permits() -> usize {
    let cpus = std::thread::available_parallelism().map_or(4, |n| n.get());
    cpus.saturating_sub(2).clamp(2, 16)
}

/// What an agent owes its run: word of how it ended, sent once.
struct Owed {
    arrivals: Sender<Arrival>,
    slot: Slot,
    /// Taken as the word is sent.
    key: Option<String>,
    /// The agent's own budget, charged as budget.rs charges, cache and all.
    charged: Arc<Budget>,
}

impl Owed {
    /// Sends how the agent's `report` says it ended.
    fn pay(mut self, report: &Report) {
        let ended = ended(report, self.charged.spent());
        self.send(ended);
    }

    /// Sends `ended`, unless word has gone already.
    fn send(&mut self, ended: Ended) {
        if let Some(key) = self.key.take() {
            let slot = self.slot;
            let _ = self.arrivals.try_send(Arrival::Ended { slot, key, ended });
        }
    }
}

impl Drop for Owed {
    fn drop(&mut self) {
        // An agent whose thread panics never reports, and `step` would wait
        // for it for good; a start that failed is recorded where it failed.
        if std::thread::panicking() {
            self.send(Ended::Failed(
                "The helper stopped without finishing.".into(),
            ));
        }
    }
}

/// How an agent's run came out, with the `tokens` it was charged.
fn ended(report: &Report, tokens: u64) -> Ended {
    match &report.status {
        Outcome::Done => Ended::Answered(report.text.clone(), tokens),
        Outcome::Failed(error) => Ended::Failed(error.clone()),
        Outcome::RoundLimit | Outcome::OverBudget => Ended::Unfinished,
        Outcome::Stopped => Ended::Stopped,
    }
}

/// What a workflow agent is when its call names no type: one with every tool,
/// built in here so no project's file can reshape a run's agents.
fn workflow_agent() -> AgentDef {
    AgentDef {
        name: "workflow-agent".into(),
        prompt: "Carry the whole task through, then report.".into(),
        ..Default::default()
    }
}

/// The tools a workflow agent may use, in the order it is offered them: those
/// its connection offers and its type allows, less those no workflow agent may.
fn usable(connection: &Connection, def: &AgentDef) -> Vec<String> {
    tools::specs(connection)
        .iter()
        .filter_map(|spec| spec["function"]["name"].as_str())
        .filter(|name| {
            def.allows(name) && !driver::DENY.contains(name) && !NOT_FOR_AGENTS.contains(name)
        })
        .map(str::to_owned)
        .collect()
}

/// A workflow agent's first message: what it is, what it may use and who
/// reads its answer, then its task, indented so no line of it reads as the
/// start of something the user said.
fn task(def: &AgentDef, tools: &[String], prompt: &str) -> Message {
    let about = format!("You are a helper ({}). {}", def.name, def.prompt);
    let tools = if tools.is_empty() {
        "none".to_owned()
    } else {
        tools.join(", ")
    };
    let prompt: Vec<String> = prompt
        .replace("\r\n", "\n")
        .split(LINE_BREAKS)
        .map(|line| format!("    {line}"))
        .collect();
    Message {
        role: "user".into(),
        content: format!(
            "{TASK_HEADER}\n{}\nTools you may use: {tools}.\n{LAST_WORD}\nTask:\n{}",
            about.trim_end(),
            prompt.join("\n")
        ),
        notice: true,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::journal::{canonical, key};
    use super::*;
    use serde_json::json;
    use std::io::Read;
    use std::net::TcpListener;
    use std::sync::atomic::AtomicUsize;
    use std::sync::{Mutex, MutexGuard, mpsc};
    use std::thread::JoinHandle;

    /// Far longer than any run here takes, so only a hang fails a test.
    const WAIT: Duration = Duration::from_secs(60);
    /// The session the tests' runs are for.
    const SESSION: u64 = 4_343;

    /// Letronna's data and a project folder, both the test's own, kept from
    /// other tests until dropped.
    struct Data {
        dir: PathBuf,
        _locks: [MutexGuard<'static, ()>; 2],
    }

    impl Data {
        fn new(name: &str) -> Self {
            let locks = [
                crate::store::tests::DATA_DIR
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
                // A paused run leaves the session a notice.
                crate::inbox::tests::DOORBELL
                    .lock()
                    .unwrap_or_else(|e| e.into_inner()),
            ];
            // Left by another test's pause, a notice would run into this one's.
            inbox::take(SESSION, true);
            let dir = std::env::temp_dir().join(format!("letronna-workflow-{name}"));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(dir.join("data")).unwrap();
            std::fs::create_dir_all(dir.join("work")).unwrap();
            unsafe { std::env::set_var("AGENT_DATA_DIR", dir.join("data")) };
            Self { dir, _locks: locks }
        }

        /// The session's connection, to the provider at `endpoint`.
        fn connection(&self, endpoint: &str) -> Connection {
            Connection {
                endpoint: endpoint.into(),
                model: "test-model".into(),
                tools: true,
                workspace: Some(self.dir.join("work")),
                conversation: SESSION,
                ..Default::default()
            }
        }

        /// A new run, or run `resume` picked up, whose agents never reach a
        /// model: `spawn` stands in for the driver.
        fn run(&self, resume: Option<&str>, spawn: Spawn) -> Run {
            let connection = self.connection("http://127.0.0.1:9/v1");
            let mut run = Run::open(
                resume,
                &connection,
                "script",
                &json!({}),
                BUDGET,
                &Cancel::default(),
            )
            .unwrap();
            run.spawn = spawn;
            run
        }

        /// The lines of run `name`'s journal that parse.
        fn journal(&self, name: &str) -> Vec<Value> {
            std::fs::read_to_string(self.folder(name).join("journal.jsonl"))
                .unwrap()
                .lines()
                .filter_map(|line| serde_json::from_str(line).ok())
                .collect()
        }

        fn folder(&self, name: &str) -> PathBuf {
            self.dir.join("data").join("workflows").join(name)
        }
    }

    impl Drop for Data {
        fn drop(&mut self) {
            unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        }
    }

    fn explore() -> Value {
        json!({"type": "explore"})
    }

    /// The task an agent was given, as the script wrote it.
    fn asked(spec: &Spec) -> String {
        let first = &spec.transcript.conversation.messages[0].content;
        let (_, task) = first.split_once("\nTask:\n").unwrap();
        let lines: Vec<&str> = task
            .lines()
            .map(|line| line.strip_prefix("    ").unwrap_or(line))
            .collect();
        lines.join("\n")
    }

    fn report(status: Outcome, text: &str) -> Report {
        Report {
            status,
            text: text.into(),
            rounds: 1,
            input: 1_000,
            cached: 0,
            output: 50,
            seconds: 0,
            files: Vec::new(),
            jobs: Vec::new(),
            value: None,
        }
    }

    /// What every stand-in agent answers.
    fn done(task: &str) -> Report {
        report(Outcome::Done, &format!("{task} -> done"))
    }

    /// Tasks the agents of a run were given, in the order they started.
    type Asked = Arc<Mutex<Vec<String>>>;

    /// Stands in for the driver: each agent is answered with what `answer`
    /// makes of its task, on a thread of its own, and its task kept in `asked_for`.
    fn answering(asked_for: &Asked, answer: fn(&str) -> Report) -> Spawn {
        let log = asked_for.clone();
        Box::new(move |spec, then| {
            let task = asked(&spec);
            log.lock().unwrap().push(task.clone());
            let budget = spec.budget.clone().unwrap();
            std::thread::spawn(move || {
                let report = answer(&task);
                // As the driver charges its one reply.
                budget.charge(&Message {
                    prompt_tokens: report.input,
                    tokens: report.output,
                    ..Default::default()
                });
                then(&report)
            });
            Ok(())
        })
    }

    /// What `run` gives, `most` steps or until it has nothing more, failing
    /// rather than hanging.
    fn steps(mut run: Run, most: usize) -> (Run, Vec<Step>) {
        let (sent, got) = mpsc::channel();
        std::thread::spawn(move || {
            let mut steps = Vec::new();
            while steps.len() < most {
                let Some(step) = run.step() else {
                    break;
                };
                steps.push(step);
            }
            let _ = sent.send((run, steps));
        });
        got.recv_timeout(WAIT).expect("the run never answered")
    }

    /// Every answer, by the slot it is for.
    fn by_slot(steps: &[Step]) -> Vec<Option<Value>> {
        let mut answers = Vec::new();
        for step in steps {
            let Step::Answer(slot, value) = step else {
                panic!("the run paused: {step:?}");
            };
            if answers.len() <= *slot {
                answers.resize(slot + 1, None);
            }
            answers[*slot] = value.clone();
        }
        answers
    }

    fn text(value: &str) -> Option<Value> {
        Some(Value::String(value.into()))
    }

    /// The journal's lines of `kind`, by key.
    fn keys(lines: &[Value], kind: &str) -> Vec<String> {
        lines
            .iter()
            .filter(|line| line["type"] == kind)
            .map(|line| line["key"].as_str().unwrap().to_owned())
            .collect()
    }

    #[test]
    fn keys_ignore_labels_and_phases() {
        let options = json!({"type": "explore", "model": "m"});
        let plain = key("Review chat.rs", &canonical(&options), 0);
        let shown = json!({"phase": "Review", "model": "m", "label": "chat.rs", "type": "explore"});
        assert_eq!(key("Review chat.rs", &canonical(&shown), 0), plain);
        // Fixed, so journals written before a change still replay after it.
        assert_eq!(
            plain,
            "v1:3e3ab62a1db4e0bb1e946565b4d26c90e0e53a36681cb9ef3bfb09377867743b"
        );
        // Objects in any order at any depth, and no options at all, are one each.
        assert_eq!(
            canonical(
                &json!({"schema": {"b": 1, "a": {"y": [2, {"q": 1, "p": 0}], "x": 3}}, "label": "l"})
            ),
            r#"{"schema":{"a":{"x":3,"y":[2,{"p":0,"q":1}]},"b":1}}"#
        );
        assert_eq!(
            canonical(&Value::Null),
            canonical(&json!({"label": "only a label"}))
        );
        // Everything else is part of the key.
        let other = json!({"type": "general", "model": "m"});
        assert_ne!(key("Review chat.rs", &canonical(&other), 0), plain);
        assert_ne!(key("Review chat.rs", &canonical(&options), 1), plain);
        assert_ne!(key("Review store.rs", &canonical(&options), 0), plain);
    }

    #[test]
    fn keys_do_not_depend_on_finishing_order() {
        let data = Data::new("order");
        let held: Arc<Mutex<Vec<(String, Then)>>> = Arc::default();
        let mut run = data.run(
            None,
            Box::new(move |spec, then| {
                let mut held = held.lock().unwrap();
                held.push((asked(&spec), then));
                // The last to start answers first, and the first last.
                if held.len() == 3 {
                    for (task, then) in held.drain(..).rev() {
                        then(&done(&task));
                    }
                }
                Ok(())
            }),
        );
        run.permits = 3;
        for task in ["Read a.rs.", "Read b.rs.", "Read c.rs."] {
            run.agent(task, &explore()).unwrap();
        }
        let (run, first) = steps(run, usize::MAX);
        assert_eq!(first.len(), 3);
        drop(run);
        let lines = data.journal("wf-1");
        let started = keys(&lines, "started");
        let mut finished = keys(&lines, "result");
        finished.reverse();
        assert_eq!(finished, started, "they did not finish in reverse");

        // Resumed with the calls in yet another order, each gets its own answer back.
        let asked_again = Asked::default();
        let mut run = data.run(Some("wf-1"), answering(&asked_again, done));
        for task in ["Read c.rs.", "Read a.rs.", "Read b.rs."] {
            run.agent(task, &explore()).unwrap();
        }
        let (_run, again) = steps(run, usize::MAX);
        assert_eq!(
            by_slot(&again),
            [
                text("Read c.rs. -> done"),
                text("Read a.rs. -> done"),
                text("Read b.rs. -> done")
            ]
        );
        assert!(asked_again.lock().unwrap().is_empty());
    }

    /// A provider on this machine that takes `count` requests one after the
    /// other, answering each with `reply` of what it was sent, and says how
    /// many it took.
    fn provider(count: usize, reply: fn(&str) -> &'static str) -> (String, JoinHandle<usize>) {
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            for _ in 0..count {
                let (mut socket, _) = server.accept().unwrap();
                let mut bytes = Vec::new();
                let mut block = [0; 65536];
                let body = loop {
                    let n = socket.read(&mut block).unwrap();
                    assert_ne!(n, 0, "the request ended early");
                    bytes.extend_from_slice(&block[..n]);
                    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length: usize = head
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break String::from_utf8_lossy(&bytes[end + 4..end + 4 + length])
                            .into_owned();
                    }
                };
                let events = [
                    json!({"choices": [{"delta": {"content": reply(&body)}}]}),
                    json!({"choices": [], "usage": {"prompt_tokens": 1_000, "completion_tokens": 50}}),
                ];
                let mut stream: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
                stream.push_str("data: [DONE]\n\n");
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{stream}", stream.len()).unwrap();
            }
            count
        });
        (endpoint, worker)
    }

    #[test]
    fn a_finished_run_replays_without_calling_a_model() {
        let data = Data::new("replay");
        let (endpoint, provider) = provider(2, |body| {
            if body.contains("colour") {
                "Blue."
            } else {
                "A fig."
            }
        });
        let connection = data.connection(&endpoint);
        let script = |resume| {
            let mut run = Run::open(
                resume,
                &connection,
                "script",
                &json!({}),
                BUDGET,
                &Cancel::default(),
            )
            .unwrap();
            run.agent("Name a colour.", &explore()).unwrap();
            run.agent("Name a fruit.", &explore()).unwrap();
            let (run, answers) = steps(run, usize::MAX);
            (run, by_slot(&answers))
        };
        let (run, first) = script(None);
        assert_eq!(first, [text("Blue."), text("A fig.")]);
        assert_eq!(run.budget.spent(), 2_100);
        drop(run);
        // Nothing listens there now, so a call to the model would fail.
        assert_eq!(provider.join().unwrap(), 2);
        assert!(data.folder("wf-1").join("agents").join("2.jsonl").is_file());

        let (run, again) = script(Some("wf-1"));
        assert_eq!(again, first);
        assert_eq!(run.budget.spent(), 0);
        // The resume wrote that it began, and nothing after.
        let lines = data.journal("wf-1");
        let launched = lines.iter().filter(|line| line["type"] == "launched");
        assert_eq!(launched.count(), 2);
        assert_eq!(lines.last().unwrap()["type"], "launched");
    }

    /// Asks for what `find` finds, then has that checked, then asks something
    /// unrelated, as a script would, and gives the three answers.
    fn review(mut run: Run, find: &str) -> Vec<Option<Value>> {
        run.agent(find, &explore()).unwrap();
        let (mut run, mut answers) = steps(run, usize::MAX);
        let [Step::Answer(0, Some(Value::String(found)))] = &answers[..] else {
            panic!("{answers:?}");
        };
        run.agent(&format!("Check this finding: {found}"), &explore())
            .unwrap();
        run.agent("Summarise the README.", &explore()).unwrap();
        answers.extend(steps(run, usize::MAX).1);
        by_slot(&answers)
    }

    #[test]
    fn an_edited_prompt_reruns_it_and_what_embeds_its_result() {
        let data = Data::new("edited");
        let first = Asked::default();
        review(data.run(None, answering(&first, done)), "Find the bug.");
        assert_eq!(first.lock().unwrap().len(), 3);

        let again = Asked::default();
        let answers = review(
            data.run(Some("wf-1"), answering(&again, done)),
            "Find the bug, carefully.",
        );
        assert_eq!(
            *again.lock().unwrap(),
            [
                "Find the bug, carefully.",
                "Check this finding: Find the bug, carefully. -> done"
            ]
        );
        assert_eq!(answers[2], text("Summarise the README. -> done"));
    }

    #[test]
    fn a_live_writing_agent_reruns_everything_started_after_it() {
        let data = Data::new("writer");
        // A launch of a script that reads, fixes, then checks, stopped after
        // its first `calls` calls; gives the tasks that ran live.
        let launch = |resume, fix: &str, calls: usize| {
            let ran = Asked::default();
            let mut run = data.run(resume, answering(&ran, done));
            let script = [
                ("Read the notes.", explore()),
                (fix, json!({"type": "general"})),
                ("Check the tests.", explore()),
            ];
            for (task, options) in &script[..calls] {
                run.agent(task, options).unwrap();
            }
            let (_run, answers) = steps(run, usize::MAX);
            assert_eq!(by_slot(&answers)[0], text("Read the notes. -> done"));
            ran.lock().unwrap().clone()
        };
        assert_eq!(launch(None, "Fix the typo.", 3).len(), 3);
        // The check is unchanged, but it could find what the fix changed.
        assert_eq!(
            launch(Some("wf-1"), "Fix the typo in notes.txt.", 3),
            ["Fix the typo in notes.txt.", "Check the tests."]
        );
        // Stopped before the check was asked for, then resumed as it was: the
        // fix replays, and the check's answer is older than what it wrote.
        assert_eq!(
            launch(Some("wf-1"), "Fix the typo on line 3.", 2),
            ["Fix the typo on line 3."]
        );
        assert_eq!(
            launch(Some("wf-1"), "Fix the typo on line 3.", 3),
            ["Check the tests."]
        );
        assert!(launch(Some("wf-1"), "Fix the typo on line 3.", 3).is_empty());
    }

    #[test]
    fn an_agent_in_flight_at_the_kill_reruns() {
        let data = Data::new("killed");
        let held = Arc::new(Mutex::new(Vec::new()));
        let kept = held.clone();
        let mut run = data.run(
            None,
            Box::new(move |spec, then| {
                match asked(&spec).as_str() {
                    "Read the notes." => then(&done("Read the notes.")),
                    "Run the tests." => {
                        then(&report(Outcome::Failed("The provider is down.".into()), ""))
                    }
                    // Never answered: still going when the run is killed.
                    _ => kept.lock().unwrap().push(then),
                }
                Ok(())
            }),
        );
        run.permits = 3;
        for task in ["Read the notes.", "Read the logs.", "Run the tests."] {
            run.agent(task, &explore()).unwrap();
        }
        let (run, before) = steps(run, 2);
        assert_eq!(before.len(), 2);
        drop(run);
        // The kill also tore the line it was writing, inside an em dash.
        let path = data.folder("wf-1").join("journal.jsonl");
        std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap()
            .write_all(b"{\"type\":\"result\",\"value\":\"a \xe2\x80")
            .unwrap();

        let again = Asked::default();
        let mut run = data.run(Some("wf-1"), answering(&again, done));
        for task in ["Read the notes.", "Read the logs.", "Run the tests."] {
            run.agent(task, &explore()).unwrap();
        }
        let (_run, answers) = steps(run, usize::MAX);
        assert_eq!(
            by_slot(&answers),
            [
                text("Read the notes. -> done"),
                text("Read the logs. -> done"),
                text("Run the tests. -> done")
            ]
        );
        let mut rerun = again.lock().unwrap().clone();
        rerun.sort();
        assert_eq!(rerun, ["Read the logs.", "Run the tests."]);
        // Only the torn line fails to read; what came after it starts a line of its own.
        let bytes = std::fs::read(&path).unwrap();
        let unread = bytes
            .split(|&byte| byte == b'\n')
            .filter(|line| !line.is_empty() && serde_json::from_slice::<Value>(line).is_err())
            .count();
        assert_eq!(unread, 1);
        drop(held);
    }

    #[test]
    fn permits_bound_how_many_run_at_once() {
        static NOW: AtomicUsize = AtomicUsize::new(0);
        static MOST: AtomicUsize = AtomicUsize::new(0);
        let data = Data::new("permits");
        let mut run = data.run(
            None,
            Box::new(|spec, then| {
                let task = asked(&spec);
                std::thread::spawn(move || {
                    let now = NOW.fetch_add(1, Ordering::SeqCst) + 1;
                    MOST.fetch_max(now, Ordering::SeqCst);
                    // Held until another runs beside it, then a little
                    // longer, so a third let through too would be counted.
                    let deadline = Instant::now() + Duration::from_secs(5);
                    while MOST.load(Ordering::SeqCst) < 2 && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(5));
                    }
                    std::thread::sleep(Duration::from_millis(20));
                    NOW.fetch_sub(1, Ordering::SeqCst);
                    then(&done(&task));
                });
                Ok(())
            }),
        );
        run.permits = 2;
        for n in 0..6 {
            run.agent(&format!("Read part {n}."), &explore()).unwrap();
        }
        let (_run, answers) = steps(run, usize::MAX);
        assert_eq!(by_slot(&answers).iter().flatten().count(), 6);
        assert_eq!(MOST.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn the_thousand_and_first_agent_is_refused() {
        let data = Data::new("cap");
        let asked_for = Asked::default();
        let mut run = data.run(None, answering(&asked_for, done));
        // Refused before it is counted: one that cannot run is no call.
        assert_eq!(
            run.agent("Count to three.", &json!({"isolation": "worktree"})),
            Err("agent() option `isolation` is not supported yet.".into())
        );
        for n in 0..1_000 {
            assert_eq!(run.agent("Count to three.", &Value::Null), Ok(n));
        }
        assert_eq!(
            run.agent("Count to three.", &Value::Null),
            Err("Workflow agent() call cap reached (1000). A loop over budget.remaining() probably never ends.".into())
        );
        // Queued, not started: nothing ran before the host asked for an answer.
        assert!(asked_for.lock().unwrap().is_empty());
    }

    #[test]
    fn three_same_failures_pause_the_run() {
        let data = Data::new("breaker");
        let failing = |task: &str| {
            let error = if task == "Read part 0." {
                "The provider is down."
            } else {
                "HTTP 401: invalid key"
            };
            report(Outcome::Failed(error.into()), "")
        };
        let asked_for = Asked::default();
        let mut run = data.run(None, answering(&asked_for, failing));
        // One at a time, so the failures come in the order they were asked.
        run.permits = 1;
        for n in 0..6 {
            run.agent(&format!("Read part {n}."), &explore()).unwrap();
        }
        let (mut run, steps) = steps(run, usize::MAX);
        let reason = "3 helpers in a row failed with the same error: HTTP 401: invalid key";
        assert_eq!(
            steps,
            [
                Step::Answer(0, None),
                Step::Answer(1, None),
                Step::Answer(2, None),
                Step::Paused(reason.into())
            ]
        );
        assert_eq!(asked_for.lock().unwrap().len(), 4);
        assert_eq!(
            run.agent("Read part 6.", &explore()),
            Err(format!("Workflow wf-1 is paused: {reason}"))
        );
        let lines = data.journal("wf-1");
        assert_eq!(
            lines.last().unwrap(),
            &json!({"type": "paused", "reason": reason})
        );
        let heard = inbox::take(SESSION, true);
        assert_eq!(heard.len(), 1);
        assert_eq!(
            heard[0].text,
            format!(
                "workflow wf-1 paused: {reason}\nWhat finished is kept: resume it with workflow {{\"resume\":\"wf-1\"}}."
            )
        );
    }

    #[test]
    fn an_agent_whose_thread_dies_answers_null() {
        let data = Data::new("panic");
        let mut run = data.run(
            None,
            Box::new(|_spec, then| {
                std::thread::spawn(move || {
                    let _then = then;
                    panic!("the helper's thread died");
                });
                Ok(())
            }),
        );
        run.agent("Read the notes.", &explore()).unwrap();
        let (_run, steps) = steps(run, usize::MAX);
        assert_eq!(steps, [Step::Answer(0, None)]);
        assert_eq!(data.journal("wf-1").last().unwrap()["type"], "failed");
    }

    #[test]
    fn a_spent_budget_pauses_the_run_until_a_resume_raises_it() {
        let data = Data::new("budget");
        let connection = data.connection("http://127.0.0.1:9/v1");
        let ran = Asked::default();
        // Each agent spends 1,050 tokens, one at a time.
        let open = |resume, budget| {
            let mut run = Run::open(
                resume,
                &connection,
                "script",
                &json!({}),
                budget,
                &Cancel::default(),
            )
            .unwrap();
            run.spawn = answering(&ran, done);
            run.permits = 1;
            run
        };
        let script = |run: &mut Run| {
            for n in 0..3 {
                run.agent(&format!("Read part {n}."), &explore())?;
            }
            Ok::<(), String>(())
        };
        let mut run = open(None, 2_000);
        script(&mut run).unwrap();
        let (run, first) = steps(run, usize::MAX);
        let reason = "its budget is spent (2k of 2k tokens)";
        assert_eq!(
            first,
            [
                Step::Answer(0, text("Read part 0. -> done")),
                Step::Answer(1, text("Read part 1. -> done")),
                Step::Paused(reason.into())
            ]
        );
        drop(run);
        let heard = inbox::take(SESSION, true);
        assert_eq!(heard.len(), 1);
        assert_eq!(
            heard[0].text,
            format!(
                "workflow wf-1 paused: {reason}\nWhat finished is kept: resume it with a larger budget, such as workflow {{\"resume\":\"wf-1\",\"budget\":4000}}."
            )
        );

        // What earlier launches spent still counts.
        let mut run = open(Some("wf-1"), 2_000);
        assert_eq!(
            script(&mut run),
            Err("The workflow budget is spent (2k of 2k tokens).".into())
        );
        drop(run);
        assert_eq!(inbox::take(SESSION, true).len(), 1);

        let mut run = open(Some("wf-1"), 4_000);
        script(&mut run).unwrap();
        let (_run, last) = steps(run, usize::MAX);
        assert_eq!(by_slot(&last).iter().flatten().count(), 3);
        assert_eq!(
            *ran.lock().unwrap(),
            ["Read part 0.", "Read part 1.", "Read part 2."]
        );
    }

    #[test]
    fn a_running_run_refuses_a_resume_until_its_lock_goes_stale() {
        let data = Data::new("lock");
        let mut first = data.run(None, answering(&Asked::default(), done));
        let connection = data.connection("http://127.0.0.1:9/v1");
        let resume = || {
            Run::open(
                Some("wf-1"),
                &connection,
                "script",
                &json!({}),
                BUDGET,
                &Cancel::default(),
            )
        };
        assert_eq!(
            resume().err(),
            Some("Workflow wf-1 is still running. Stop it first with agents {\"action\":\"stop\",\"to\":\"wf-1\"}.".into())
        );
        let lock = data.folder("wf-1").join("lock");
        let age = |when| {
            std::fs::File::options()
                .write(true)
                .open(&lock)
                .unwrap()
                .set_modified(when)
                .unwrap()
        };
        // A beat while the run is being driven freshens a lock gone quiet.
        age(SystemTime::now() - STALE);
        first.arrivals.0.try_send(Arrival::Beat).unwrap();
        first.agent("Read the notes.", &explore()).unwrap();
        let (first, _) = steps(first, usize::MAX);
        assert!(!stale(&lock));
        assert!(resume().is_err());
        // As if the process driving it had died two minutes ago.
        age(SystemTime::now() - STALE);
        let second = resume().unwrap();
        // The run it took over from ends later without freeing the new claim.
        drop(first);
        assert!(lock.is_file());
        drop(second);
        assert!(!lock.exists());
    }
}
