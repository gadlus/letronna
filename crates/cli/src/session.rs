//! Driving one turn of the agent loop in a terminal.
//!
//! The loop itself lives in `letronna_core::agent`; this supplies the parts
//! that differ in a terminal: waiting on a blocking channel rather than a
//! frame, and asking the user by reading a line rather than drawing a card.

use letronna_core::agent::{self, Next, Turn};
use letronna_core::cancel::Cancel;
use letronna_core::chat::{self, Connection, Event, Message, ToolCall};
use letronna_core::inbox;
use letronna_core::store::{Conversation, Store};
use letronna_core::tools;
use std::collections::HashSet;

use crate::bar::Bar;

use crate::render::{self, Stream};

/// Lines of a tool's output to show before clipping.
const DETAIL_LINES: usize = 12;

pub struct Runner {
    pub store: Store,
    /// Tools approved for the rest of this run.
    pub allowed: HashSet<String>,
    /// Approve everything, for `--yes`.
    pub yes: bool,
    /// Read and search only; no writing, no running.
    pub plan: bool,
    /// The box at the foot of the window, pumped while a reply streams so
    /// that typing carries on through an answer.
    pub bar: Bar,
    /// Whether the user stopped the last turn, which keeps a ring from
    /// starting another before they say something.
    pub stopped: bool,
}

impl Runner {
    fn connection(&self, session: usize) -> Connection {
        let workspace = self.store.conversations[session].workspace.clone();
        let settings = &self.store.settings;
        let saved = letronna_core::store::load_keys()
            .get(letronna_core::gateway::key_slot(&settings.gateway))
            .cloned()
            .unwrap_or_default();
        let (endpoint, key) =
            letronna_core::gateway::resolve(&settings.gateway, &settings.custom_endpoint, &saved);
        Connection {
            endpoint,
            model: settings.model.clone(),
            key,
            tools: true,
            workspace,
            plan: self.plan,
            remote: None,
            effort: settings.effort.clone(),
            search: settings.search,
            conversation: self.store.conversations[session].id,
        }
    }

    /// Send one prompt and run the agent to completion. Returns the final
    /// answer, or an error worth showing.
    pub fn ask(&mut self, session: usize, prompt: &str) -> Result<String, String> {
        let connection = self.connection(session);
        chat::validate(&connection)?;

        {
            let conversation = &mut self.store.conversations[session];
            if conversation.messages.is_empty() && conversation.title.is_empty() {
                conversation.title = prompt.chars().take(40).collect();
            }
            conversation.updated = chat::now();
        }
        // What came meanwhile goes in ahead of the prompt, so the user's
        // words are the last thing the model reads.
        self.take_notices(session, true);
        self.store.conversations[session].messages.push(Message {
            role: "user".into(),
            content: prompt.to_owned(),
            ..Default::default()
        });
        self.persist();

        self.run(connection, session)
    }

    /// Run a turn on the notices waiting for an idle session, with nothing
    /// typed. Returns the final answer, or an error worth showing; nothing
    /// runs when no notices are waiting.
    pub fn resume(&mut self, session: usize) -> Result<String, String> {
        // Looked for before the connection is checked: a ring can outlive
        // the notices it rang for, taken in the middle of a turn, and a
        // session opened with none waiting has no reason to hear about its
        // connection.
        if !inbox::pending().contains(&self.store.conversations[session].id) {
            return Ok(String::new());
        }
        let connection = self.connection(session);
        chat::validate(&connection)?;
        self.take_notices(session, true);
        self.run(connection, session)
    }

    /// Run the agent to completion on the conversation as it stands.
    fn run(&mut self, connection: Connection, session: usize) -> Result<String, String> {
        let mut turn = Turn::new(connection);
        self.stopped = false;
        self.bar.working(true);
        // The tab says so too, for whoever has switched away to something
        // else while this runs.
        let busy = crate::title::Busy::start();
        let answer = self.work(&mut turn, session);
        drop(busy);
        self.bar.working(false);
        answer
    }

    fn work(&mut self, turn: &mut Turn, session: usize) -> Result<String, String> {
        loop {
            let Some(reply) = self.stream_once(turn, session)? else {
                // Ctrl-C stops the turn and leaves; escape only stops the
                // turn, so say so in the one case where you stay.
                if !self.bar.finished() {
                    render::blank();
                    render::said(&render::dim("●"), &render::dim("Stopped."));
                }
                self.stopped = true;
                return Ok(String::new());
            };
            let answer = reply.content.clone();
            let calls = reply.tool_calls.clone();
            self.store.conversations[session].updated = chat::now();
            self.store.conversations[session].messages.push(reply);
            self.persist();

            if !turn.begin(calls) {
                return Ok(answer);
            }
            if !self.work_through(turn, session)? {
                self.stopped = true;
                return Ok(answer);
            }
            // Notices go in after the results, which have to follow the
            // calls they answer, and before what was typed, so the user's
            // words are still read last.
            self.take_notices(session, false);
            // What was typed while it worked goes in after the results, so
            // the model reads it before carrying on rather than once the turn
            // is over. It is on the screen already, echoed when entered.
            let typed = self.bar.take_typed();
            if !typed.is_empty() {
                self.store.conversations[session]
                    .messages
                    .extend(typed.into_iter().map(|content| Message {
                        role: "user".into(),
                        content,
                        ..Default::default()
                    }));
                self.persist();
            }
        }
    }

    /// One model call, printed as it streams.
    /// One model call, printed as it streams. `None` when the user stopped
    /// waiting for it.
    fn stream_once(&mut self, turn: &mut Turn, session: usize) -> Result<Option<Message>, String> {
        let receiver = chat::start(
            turn.connection.clone(),
            self.store.conversations[session].messages.clone(),
        );
        // The model's name lives under the input box, so an answer only needs
        // its bullet here.
        let mut out = Stream::opening_with(render::bullet());
        let mut opened = false;
        let mut thought = false;
        self.bar.wait_line(true);
        loop {
            // Keys first, then whatever the model has sent: the pump waits a
            // few milliseconds for a keystroke, which is also what keeps this
            // loop from spinning.
            self.bar.pump();
            if self.bar.interrupted() {
                // Dropping the receiver is enough: the worker finishes into a
                // channel nobody is listening to and goes away.
                self.bar.wait_line(false);
                out.end();
                return Ok(None);
            }
            let event = match receiver.try_recv() {
                Ok(event) => event,
                Err(async_channel::TryRecvError::Empty) => continue,
                Err(async_channel::TryRecvError::Closed) => {
                    return Err("The model connection closed unexpectedly.".to_owned());
                }
            };
            if let Event::Reasoning(text) = &event
                && self.store.settings.thinking
            {
                {
                    if !thought {
                        self.bar.wait_line(false);
                        render::blank();
                        render::said(&render::dim("·"), &render::dim("thinking"));
                        thought = true;
                    }
                    // Dim, and its own column, so it cannot be mistaken for
                    // the answer -- it is the model talking to itself.
                    render::out(&render::dim(&text.replace('\n', " ")), false);
                }
            }
            if let Event::Delta(text) = &event {
                if !opened {
                    // Nothing more is needed above the answer: the gap was
                    // left when the question was echoed.
                    self.bar.wait_line(false);
                    if thought {
                        render::out("", true);
                        render::blank();
                    }
                    opened = true;
                }
                out.push(text);
            }
            turn.absorb(&event);
            if let Event::Finished(result) = event {
                self.bar.wait_line(false);
                out.end();
                // Catch up on anything a throttled paint skipped.
                crate::screen::settle();
                result?;
                break;
            }
        }
        let model = self.store.settings.model.clone();
        let reply = turn.take_reply(&model);
        // A reply that only asked for a tool has nothing to caption; the tool
        // line below speaks for it.
        if reply.tokens > 0 && !reply.content.is_empty() {
            render::line(&render::dim(&usage(&reply)));
        }
        Ok(Some(reply))
    }

    /// Run the queued calls, asking first where the rules say to. False when
    /// the user stopped the turn.
    fn work_through(&mut self, turn: &mut Turn, session: usize) -> Result<bool, String> {
        loop {
            let gate = |name: &str| agent::needs_user(name, self.yes, &self.allowed, false);
            let batch = turn.next_batch(gate, agent::MAX_BATCH);
            if !batch.is_empty() {
                self.run_batch(turn, batch);
                continue;
            }
            match turn.next(gate) {
                Next::Done => return Ok(true),
                Next::Ask => {
                    let results = turn.take_results();
                    self.store.conversations[session].messages.extend(results);
                    self.persist();
                    return Ok(true);
                }
                Next::Run(call) => self.run_call(turn, call),
                Next::AskUser(call) => {
                    if !self.consult(turn, call)? {
                        return Ok(false);
                    }
                }
            }
        }
    }

    fn run_call(&mut self, turn: &mut Turn, call: letronna_core::chat::ToolCall) {
        let ctx = tools::ToolContext::of(&turn.connection);
        let summary = tools::summary(&call.name, &call.arguments);
        let outcome = self.run_off_the_loop(&call, ctx, &summary);
        print_call(&call.name, &summary, &outcome);
        turn.finish_call(outcome);
    }

    /// Run calls that may go together at once, printing each one's lines in
    /// the order the model made them.
    ///
    /// A call's lines wait for those of every call before it, so the output
    /// reads as though they ran one after another, only sooner. Escape stops
    /// waiting for all of them: what finished keeps its result, and the rest
    /// are told they were stopped, as a lone call is.
    fn run_batch(&mut self, turn: &mut Turn, calls: Vec<ToolCall>) {
        let ctx = tools::ToolContext::of(&turn.connection);
        let mut batch = Batch::start(calls, &ctx, tools::run);
        let mut about = batch.waiting();
        self.bar.wait_line_about(&about);
        while !batch.done() {
            self.bar.pump();
            let ready = if self.bar.interrupted() {
                batch.stop()
            } else {
                batch.ready()
            };
            for (call, outcome) in ready {
                print_call(
                    &call.name,
                    &tools::summary(&call.name, &call.arguments),
                    &outcome,
                );
                turn.finish_id(&call.id, outcome);
            }
            // The line names the first call still running, which changes as
            // the ones ahead of it finish; its clock keeps going, since every
            // call in the batch started when it did.
            let now = batch.waiting();
            if !batch.done() && now != about {
                self.bar.rename_wait(&now);
                about = now;
            }
        }
        self.bar.wait_line(false);
    }

    /// Run a tool on a thread of its own, so the loop keeps reading keys and
    /// escape still means something.
    ///
    /// A command gets two minutes before it gives up, and a search of a large
    /// tree is not instant either. Running those on this thread meant the
    /// window went dead to the keyboard for as long as they took, escape
    /// included -- so the one moment you most want to call something off was
    /// the one moment nothing was listening.
    ///
    /// Stopping stops waiting. The thread is left to finish into a channel
    /// nobody is reading; what it was doing to the disk it will still do, and
    /// a command it started keeps running under `list_jobs` where it can be
    /// stopped by name.
    fn run_off_the_loop(
        &mut self,
        call: &letronna_core::chat::ToolCall,
        ctx: tools::ToolContext,
        summary: &str,
    ) -> Result<String, String> {
        let (name, arguments) = (call.name.clone(), call.arguments.clone());
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = sender.send_blocking(tools::run(&name, &arguments, &ctx));
        });

        self.bar
            .wait_line_about(&format!("{}({summary})", call.name));
        let outcome = loop {
            self.bar.pump();
            if self.bar.interrupted() {
                break Err(stopped(&call.name));
            }
            match receiver.try_recv() {
                Ok(outcome) => break outcome,
                Err(async_channel::TryRecvError::Empty) => continue,
                Err(async_channel::TryRecvError::Closed) => {
                    break Err(format!("`{}` stopped without finishing.", call.name));
                }
            }
        };
        self.bar.wait_line(false);
        outcome
    }

    /// Ask the user about a call. False means they ended the turn.
    fn consult(
        &mut self,
        turn: &mut Turn,
        call: letronna_core::chat::ToolCall,
    ) -> Result<bool, String> {
        if call.name == tools::ASK_USER {
            let args = tools::parse_args(&call.arguments).unwrap_or_default();
            let question = args["question"].as_str().unwrap_or("").to_owned();
            let options: Vec<String> = args["options"]
                .as_array()
                .map(|list| {
                    list.iter()
                        .filter_map(|o| o.as_str())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();

            // Offered answers are picked, not typed; an open question still
            // wants the line.
            let reply = if options.is_empty() {
                render::blank();
                render::said(&render::blue("●"), &bold_question(&question));
                self.bar.reply().ok_or("No answer given.")?
            } else {
                let choices: Vec<&str> = options.iter().map(String::as_str).collect();
                let picked = self
                    .bar
                    .choose(&question, Vec::new(), &choices)
                    .ok_or("No answer given.")?;
                render::blank();
                render::said(&render::blue("●"), &bold_question(&question));
                render::said(&render::blue("›"), &render::bold(&options[picked]));
                render::blank();
                options[picked].clone()
            };
            turn.finish_call(Ok(reply));
            return Ok(true);
        }

        let ctx = tools::ToolContext::of(&turn.connection);
        let summary = tools::summary(&call.name, &call.arguments);

        // A change to a file is shown as a diff above the question, where
        // there is room for it; anything else gets its preview inside the
        // box, which is where the eye already is.
        let mut detail = Vec::new();
        match crate::diff::of(&call.name, &call.arguments, &ctx) {
            Some(rows) => {
                render::blank();
                render::said(
                    &render::blue("●"),
                    &render::bold(&format!("{} {summary}", verb(&call.name))),
                );
                for row in rows {
                    render::out(&row, true);
                }
            }
            None => {
                detail = tools::preview(&call.name, &call.arguments, &ctx)
                    .lines()
                    .take(DETAIL_LINES)
                    .map(str::to_owned)
                    .collect();
            }
        }

        let question = format!("Letronna wants to {}", intent(&call.name, &summary));
        let picked = self.bar.choose(
            &question,
            detail,
            &[
                "Yes",
                "Yes, and stop asking for this",
                "No, tell it why not",
                "No, and stop here",
            ],
        );

        match picked {
            Some(0) => {
                self.run_call(turn, call);
                Ok(true)
            }
            Some(1) => {
                self.allowed.insert(call.name.clone());
                self.run_call(turn, call);
                Ok(true)
            }
            Some(3) | None => Ok(false),
            _ => {
                render::tool(&call.name, &summary, "declined", true);
                turn.finish_call(Err(tools::DECLINED.into()));
                Ok(true)
            }
        }
    }

    pub fn persist(&mut self) {
        if self.store.save().is_err() {
            render::line(&render::red("Could not write the session to disk."));
        }
    }

    /// Put a session's waiting notices into its conversation as one message
    /// and on the screen: mid-turn only those that cannot wait, and all of
    /// them when it is idle.
    fn take_notices(&mut self, session: usize, idle: bool) {
        let id = self.store.conversations[session].id;
        let Some(notices) = inbox::fold(&inbox::take(id, idle)) else {
            return;
        };
        render::notice(&notices.content);
        self.store.conversations[session].messages.push(notices);
        self.persist();
    }

    /// Start a session, reusing the most recent one when asked to resume.
    pub fn open(&mut self, resume: bool, workspace: Option<std::path::PathBuf>) -> usize {
        if resume && !self.store.conversations.is_empty() {
            let last = self.store.conversations.len() - 1;
            self.enter(last);
            return last;
        }
        self.start(workspace)
    }

    /// Open the session with this id, or `None` if there is no such session.
    pub fn open_id(&mut self, id: u64) -> Option<usize> {
        let index = self.store.conversations.iter().position(|c| c.id == id)?;
        self.enter(index);
        Some(index)
    }

    /// Make a session the open one, reading what was said in it. Startup
    /// lists the sessions without reading any of them, so this is where a
    /// conversation actually comes off the disk.
    pub fn enter(&mut self, index: usize) {
        self.store.active = index;
        if let Some(conversation) = self.store.conversations.get_mut(index) {
            letronna_core::store::fill(conversation);
        }
    }

    /// A session of its own.
    pub fn start(&mut self, workspace: Option<std::path::PathBuf>) -> usize {
        self.store.conversations.push(Conversation {
            id: self.store.next_id(),
            title: String::new(),
            updated: chat::now(),
            workspace,
            ..Default::default()
        });
        let index = self.store.conversations.len() - 1;
        self.store.active = index;
        index
    }
}

/// Calls from one reply running at once, each on a thread of its own, their
/// outcomes handed back in the order the model made the calls however their
/// threads finish, so what is printed and what the model reads keep that order.
struct Batch {
    calls: Vec<ToolCall>,
    /// Each call's outcome once its thread has sent it, until it is handed back.
    outcomes: Vec<Option<Result<String, String>>>,
    /// How many calls from the front have been handed back.
    shown: usize,
    /// Each outcome as it comes, with the place of the call it answers.
    arrivals: async_channel::Receiver<(usize, Result<String, String>)>,
    /// Set when the batch is stopped, for a tool that listens for it.
    cancel: Cancel,
}

impl Batch {
    /// Start every call on a thread of its own, running it with `run`.
    ///
    /// That is `tools::run` but in tests, where reaching the tool registry
    /// would start the MCP servers configured on the machine running them.
    fn start(
        calls: Vec<ToolCall>,
        ctx: &tools::ToolContext,
        run: fn(&str, &str, &tools::ToolContext) -> Result<String, String>,
    ) -> Self {
        let (sender, arrivals) = async_channel::unbounded();
        for (at, call) in calls.iter().enumerate() {
            let (name, arguments) = (call.name.clone(), call.arguments.clone());
            let (sender, ctx) = (sender.clone(), ctx.clone());
            std::thread::spawn(move || {
                let _ = sender.send_blocking((at, run(&name, &arguments, &ctx)));
            });
        }
        Self {
            outcomes: vec![None; calls.len()],
            calls,
            shown: 0,
            arrivals,
            cancel: ctx.cancel.clone(),
        }
    }

    /// Whether every call has been handed back.
    fn done(&self) -> bool {
        self.shown == self.calls.len()
    }

    /// The calls at the front whose outcomes are in, in order, and not yet handed back.
    fn ready(&mut self) -> Vec<(ToolCall, Result<String, String>)> {
        self.collect();
        let finished = self.outcomes[self.shown..]
            .iter()
            .take_while(|outcome| outcome.is_some())
            .count();
        self.hand_back(finished)
    }

    /// Every call not yet handed back, at once: those that finished with
    /// their outcomes, the rest told they were stopped.
    fn stop(&mut self) -> Vec<(ToolCall, Result<String, String>)> {
        // Taken in before the stop, so a call that finished keeps its result.
        self.collect();
        self.cancel.set();
        self.hand_back(self.calls.len() - self.shown)
    }

    /// The first call still running as the waiting line names it, and how
    /// many others are; empty once none is.
    fn waiting(&self) -> String {
        let mut running = self
            .calls
            .iter()
            .zip(&self.outcomes)
            .skip(self.shown)
            .filter(|(_, outcome)| outcome.is_none())
            .map(|(call, _)| call);
        let Some(first) = running.next() else {
            return String::new();
        };
        let label = format!(
            "{}({})",
            first.name,
            tools::summary(&first.name, &first.arguments)
        );
        match running.count() {
            0 => label,
            more => format!("{label} and {more} more"),
        }
    }

    /// Take in the outcomes that have arrived.
    fn collect(&mut self) {
        loop {
            match self.arrivals.try_recv() {
                Ok((at, outcome)) => {
                    if let Some(slot) = self.outcomes.get_mut(at) {
                        *slot = Some(outcome);
                    }
                }
                Err(async_channel::TryRecvError::Empty) => return,
                Err(async_channel::TryRecvError::Closed) => {
                    // Every thread has ended, so one that sent nothing died first.
                    let unsent = self.calls.iter().zip(&mut self.outcomes).skip(self.shown);
                    for (call, outcome) in unsent {
                        outcome.get_or_insert_with(|| {
                            Err(format!("`{}` stopped without finishing.", call.name))
                        });
                    }
                    return;
                }
            }
        }
    }

    /// Hand back the next `count` calls with their outcomes, telling any
    /// still running that it was stopped.
    fn hand_back(&mut self, count: usize) -> Vec<(ToolCall, Result<String, String>)> {
        let from = self.shown;
        self.shown += count;
        self.calls[from..self.shown]
            .iter()
            .zip(&mut self.outcomes[from..])
            .map(|(call, outcome)| {
                let outcome = outcome.take().unwrap_or_else(|| Err(stopped(&call.name)));
                (call.clone(), outcome)
            })
            .collect()
    }
}

/// Print a call that has run: what it was and on what, then how it went and
/// the first lines of what it gave back.
fn print_call(name: &str, summary: &str, outcome: &Result<String, String>) {
    // Every turn of the conversation opens with a blank line, whoever is
    // speaking; a tool result that butts up against the question it came
    // from reads as part of it.
    render::blank();
    match outcome {
        Ok(text) => {
            let lines = text.lines().count();
            let count = if lines == 1 {
                "1 line".to_owned()
            } else {
                format!("{lines} lines")
            };
            render::tool(name, summary, &count, false);
            if !text.trim().is_empty() {
                render::detail(text, DETAIL_LINES);
            }
        }
        Err(error) => render::tool(name, summary, &tools::one_line(error), true),
    }
}

/// What the model is told about a call the user stopped while it ran.
fn stopped(name: &str) -> String {
    format!("The user stopped `{name}` while it was running. Ask before starting it again.")
}

/// What the line above a diff says is about to happen.
fn verb(name: &str) -> &'static str {
    match name {
        "write_file" => "write",
        _ => "edit",
    }
}

fn bold_question(text: &str) -> String {
    render::bold(text)
}

fn intent(name: &str, summary: &str) -> String {
    match name {
        "write_file" => format!("write {summary}"),
        "edit_file" => format!("edit {summary}"),
        "run_command" => "run a command".to_owned(),
        "browse" | "use_page" => summary.to_owned(),
        "search_web" => "search the web".to_owned(),
        other => format!("use {other}"),
    }
}

fn usage(reply: &Message) -> String {
    let mut parts = vec![format!("{} out", reply.tokens)];
    if reply.per_second > 0. {
        parts.push(format!("{:.1} tok/s", reply.per_second));
    }
    let input = reply.input_tokens();
    if input > 0 {
        parts.push(format!("{input} in"));
        if reply.cached_tokens > 0 {
            parts.push(format!("{}% cached", 100 * reply.cached_tokens / input));
        }
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    type Outcome = Result<String, String>;

    /// Reads of `a`, `b` and `c`.
    fn reads() -> Vec<ToolCall> {
        ["a", "b", "c"]
            .into_iter()
            .map(|path| ToolCall {
                id: path.into(),
                name: "read_file".into(),
                arguments: json!({"path": path}).to_string(),
            })
            .collect()
    }

    /// Reads of `a`, `b` and `c`, whose outcomes the test sends rather than threads.
    fn fed() -> (Batch, async_channel::Sender<(usize, Outcome)>) {
        let calls = reads();
        let (sender, arrivals) = async_channel::unbounded();
        let batch = Batch {
            outcomes: vec![None; calls.len()],
            calls,
            shown: 0,
            arrivals,
            cancel: Cancel::default(),
        };
        (batch, sender)
    }

    /// Each call handed back as its id and outcome.
    fn ids(handed: Vec<(ToolCall, Outcome)>) -> Vec<(String, Outcome)> {
        handed
            .into_iter()
            .map(|(call, outcome)| (call.id, outcome))
            .collect()
    }

    fn ok(id: &str, text: &str) -> (String, Outcome) {
        (id.to_owned(), Ok(text.to_owned()))
    }

    fn failed(id: &str, error: &str) -> (String, Outcome) {
        (id.to_owned(), Err(error.to_owned()))
    }

    #[test]
    fn outcomes_come_back_in_call_order_however_they_finish() {
        let (mut batch, sender) = fed();
        sender.send_blocking((2, Ok("c".into()))).unwrap();
        // The last is in, but nothing before it is, so nothing can be printed yet.
        assert!(batch.ready().is_empty());
        sender.send_blocking((0, Ok("a".into()))).unwrap();
        assert_eq!(ids(batch.ready()), [ok("a", "a")]);
        sender
            .send_blocking((1, Err("No such file.".into())))
            .unwrap();
        assert_eq!(
            ids(batch.ready()),
            [failed("b", "No such file."), ok("c", "c")]
        );
        assert!(batch.done());
    }

    #[test]
    fn stopping_keeps_what_finished_and_stops_the_rest() {
        let (mut batch, sender) = fed();
        sender.send_blocking((0, Ok("a".into()))).unwrap();
        assert_eq!(ids(batch.ready()), [ok("a", "a")]);
        // Finished, but not yet taken in when the stop comes.
        sender.send_blocking((2, Ok("c".into()))).unwrap();
        let cancel = batch.cancel.clone();

        assert_eq!(
            ids(batch.stop()),
            [failed("b", &stopped("read_file")), ok("c", "c")]
        );
        assert!(batch.done());
        assert!(cancel.is_set());
        // What the stopped call says later goes nowhere.
        let _ = sender.send_blocking((1, Ok("b".into())));
        assert!(batch.ready().is_empty());
    }

    #[test]
    fn a_call_whose_thread_died_says_so() {
        let (mut batch, sender) = fed();
        sender.send_blocking((0, Ok("a".into()))).unwrap();
        drop(sender);
        let died = "`read_file` stopped without finishing.";
        assert_eq!(
            ids(batch.ready()),
            [ok("a", "a"), failed("b", died), failed("c", died)]
        );
        assert!(batch.done());
    }

    #[test]
    fn the_waiting_line_names_the_first_call_still_running() {
        let (mut batch, sender) = fed();
        assert_eq!(batch.waiting(), "read_file(a) and 2 more");
        sender.send_blocking((1, Ok("b".into()))).unwrap();
        assert!(batch.ready().is_empty());
        assert_eq!(batch.waiting(), "read_file(a) and 1 more");
        sender.send_blocking((0, Ok("a".into()))).unwrap();
        assert_eq!(batch.ready().len(), 2);
        assert_eq!(batch.waiting(), "read_file(c)");
        sender.send_blocking((2, Ok("c".into()))).unwrap();
        assert_eq!(batch.ready().len(), 1);
        assert_eq!(batch.waiting(), "");
    }

    /// How many calls have gone into `meet`.
    static MET: AtomicUsize = AtomicUsize::new(0);

    /// Waits for all three reads to be running and gives back its arguments,
    /// or gives up after five seconds.
    fn meet(_: &str, arguments: &str, _: &tools::ToolContext) -> Outcome {
        MET.fetch_add(1, Ordering::SeqCst);
        let give_up = Instant::now() + Duration::from_secs(5);
        while MET.load(Ordering::SeqCst) < 3 {
            if Instant::now() >= give_up {
                return Err(format!("{arguments} ran alone"));
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        Ok(arguments.to_owned())
    }

    #[test]
    fn the_calls_in_a_batch_run_at_once() {
        // Each call waits for the others to be running, so calls run one
        // after another would give up.
        let mut batch = Batch::start(reads(), &tools::ToolContext::default(), meet);
        let mut handed = Vec::new();
        let give_up = Instant::now() + Duration::from_secs(30);
        while !batch.done() && Instant::now() < give_up {
            handed.extend(batch.ready());
            std::thread::sleep(Duration::from_millis(10));
        }
        let path = |path: &str| json!({"path": path}).to_string();
        assert_eq!(
            ids(handed),
            [
                ok("a", &path("a")),
                ok("b", &path("b")),
                ok("c", &path("c"))
            ]
        );
    }
}
