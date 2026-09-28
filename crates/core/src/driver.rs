//! Runs an agent's turn to its end with no window.
//!
//! A helper, a `letronna -p` run or a workflow's agent has nobody watching it
//! stream and nobody to click a card, so this is one more caller of
//! `agent::Turn` beside the window and the terminal: the same loop, with the
//! parts that need a person swapped for what works without one. A call that
//! needs a yes goes to `approvals`, where the session's own rules or its user
//! answer it; a question is refused with advice, since nobody is there to
//! answer it; the checklist is kept on the agent's transcript; the browser is
//! used unseen, with no panel to open; and messages sent meanwhile are read
//! from the inbox after each batch of results and before the run counts as
//! done.
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
use crate::inbox::{self, Notice};
use crate::store::{Todo, Transcript};
use crate::tasks::{self, Key};
use crate::tools::{self, Caller, ToolContext};
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

/// What a helper is told when a call it needed a yes for did not get one.
const NOT_APPROVED: &str = "This was not approved, so it did not run. Do not try to get the same result another way: leave it, and say in your report what you could not do.";

/// Which tools an agent may call, before the deny set is taken out.
pub enum Allow {
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
    /// A model call failed, or its transcript could not be written.
    Failed(String),
    /// Its budget, or one above it, was spent.
    OverBudget,
}

/// A run on a thread of its own; joining it gives the report.
pub type Handle = std::thread::JoinHandle<Report>;

/// Runs the agent on this thread until it answers, is stopped, or runs out of rounds or budget.
pub fn run(mut spec: Spec) -> Report {
    let started = Instant::now();
    let mut turn = Turn::with_limit(spec.connection.clone(), spec.rounds);
    let mut report = Report {
        status: Outcome::Done,
        text: String::new(),
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
            spec.transcript
                .conversation
                .messages
                .extend(turn.take_results());
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
    report.text = last_said(&spec.transcript.conversation.messages);
    report.seconds = started.elapsed().as_secs();
    report
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
        let Some(reply) = ask(spec, turn)? else {
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
        if calls.is_empty() {
            // Mail that came while it worked may change the answer, so it
            // carries the run on rather than going unread.
            let mail = inbox::fold(&inbox::take(spec.id, true));
            let answered = mail.is_none();
            spec.transcript.conversation.messages.extend(mail);
            save(&mut spec.transcript)?;
            if answered {
                return Ok(Outcome::Done);
            }
            continue;
        }
        save(&mut spec.transcript)?;
        if !turn.begin(calls) {
            return Ok(Outcome::RoundLimit);
        }
        report.rounds += 1;
        spec.status.rounds.store(report.rounds, Ordering::Relaxed);
        work_through(spec, turn, report)?;
    }
}

/// Works through one reply's calls, running together the ones that may, then
/// hands back their results and any mail that cannot wait, leaving early at a stop.
fn work_through(spec: &mut Spec, turn: &mut Turn, report: &mut Report) -> Result<(), String> {
    loop {
        if spec.cancel.is_set() {
            return Ok(());
        }
        // A tool allowed for the whole run would get its yes at once from
        // `approvals::ask`, so it counts as allowed here and can join a batch.
        let gate = |name: &str| {
            refusal(&spec.allow, name).is_none()
                && tools::needs_approval(name)
                && !approvals::granted(spec.run, name)
        };
        let batch = turn.next_batch(gate, MAX_BATCH);
        if !batch.is_empty() {
            for (call, outcome) in batch.iter().zip(together(spec, &batch)) {
                // One a stop kept from starting gets no result, as a lone call does.
                if let Some(outcome) = outcome {
                    turn.finish_id(&call.id, outcome);
                }
            }
            continue;
        }
        let outcome = match turn.next(gate) {
            Next::Done => return Ok(()),
            Next::Ask => {
                let messages = &mut spec.transcript.conversation.messages;
                messages.extend(turn.take_results());
                // After the results, which have to follow the calls they answer,
                // and folded into the run, so the window keeps its task.
                let mail = inbox::fold(&inbox::take(spec.id, false));
                messages.extend(mail.map(|mail| Message {
                    folded: true,
                    ..mail
                }));
                return save(&mut spec.transcript);
            }
            Next::Run(call) => dispatch(spec, &call, report),
            Next::AskUser(call) => {
                if approvals::ask(spec.root, spec.run, &spec.label, &call, &context(spec)) {
                    dispatch(spec, &call, report)
                } else if spec.cancel.is_set() {
                    None
                } else {
                    Some(Err(NOT_APPROVED.into()))
                }
            }
        };
        let Some(outcome) = outcome else {
            return Ok(());
        };
        turn.finish_call(outcome);
    }
}

/// Makes one call the way this agent may, or gives None when a stop came before it began.
fn dispatch(
    spec: &mut Spec,
    call: &ToolCall,
    report: &mut Report,
) -> Option<Result<String, String>> {
    if let Some(refused) = refusal(&spec.allow, &call.name) {
        return Some(Err(refused));
    }
    if call.name == tools::TODO {
        return Some(checklist(&mut spec.transcript, &call.arguments));
    }
    let outcome = off_thread(call, &context(spec))?;
    if outcome.is_ok() && matches!(call.name.as_str(), "write_file" | "edit_file") {
        let args = tools::parse_args(&call.arguments).unwrap_or_default();
        if let Some(path) = args["path"].as_str()
            && !report.files.iter().any(|file| file == path)
        {
            report.files.push(path.to_owned());
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
                let refused = refusal(&spec.allow, &call.name);
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
fn refusal(allow: &Allow, name: &str) -> Option<String> {
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

/// Calls the model with the conversation so far and reads the reply to its
/// end, or gives None when a stop ended it first.
fn ask(spec: &Spec, turn: &mut Turn) -> Result<Option<Message>, String> {
    let extra = Extra {
        cancel: spec.cancel.clone(),
        ..Extra::default()
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
        turn.absorb(&event);
        if let Event::Finished(result) = event {
            return match result {
                Ok(()) => Ok(Some(turn.take_reply(&spec.connection.model))),
                Err(_) if spec.cancel.is_set() => Ok(None),
                Err(error) => Err(error),
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
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener, TcpStream};
    use std::path::{Path, PathBuf};
    use std::sync::mpsc;
    use std::thread::JoinHandle;
    use std::time::Duration;

    /// Far longer than any run here takes, so only a hang fails a test.
    const WAIT: Duration = Duration::from_secs(60);
    /// Where the tests' agent ids start, clear of every session id as real ones are.
    const AGENTS: AgentId = 1 << 40;

    /// A workspace holding `notes.txt`, emptied and reused on each run.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-driver-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
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
        assert_eq!(results(&sent[1]), ["hello from the notes"]);
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
            (Some("tool"), Some("hello from the notes"))
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

        assert_eq!(results(&sent[1]), ["Checklist updated: 1 of 2 done."]);
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
                "hello from the notes",
                "more.txt",
                "`search` is not available to this helper.",
                "and more"
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
        // What answering a card with "allow for this run" leaves behind.
        let requests = approvals::serve();
        let earlier = ToolCall {
            id: "earlier".into(),
            name: "web_fetch".into(),
            arguments: "{}".into(),
        };
        let asked = std::thread::spawn(move || {
            approvals::ask(7, id, "helper", &earlier, &ToolContext::default())
        });
        let request = requests.recv_timeout(WAIT).unwrap();
        request.answer(approvals::Decision::ForRun);
        assert!(asked.join().unwrap());

        // Both requests are taken before either is answered, so fetches made
        // one after another fail: the first waits out its timeout unanswered.
        let pages = TcpListener::bind("127.0.0.1:0").unwrap();
        let site = format!("http://{}", pages.local_addr().unwrap());
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

        assert_eq!(
            results(&sent[1]),
            [
                format!("{site}/first\n\nfirst"),
                format!("{site}/second\n\nsecond")
            ]
        );
    }
}
