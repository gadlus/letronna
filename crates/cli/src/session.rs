//! Driving one turn of the agent loop in a terminal.
//!
//! The loop itself lives in `letronna_core::agent`; this supplies the parts
//! that differ in a terminal: waiting on a blocking channel rather than a
//! frame, and asking the user by reading a line rather than drawing a card.

use letronna_core::agent::{self, Next, Turn};
use letronna_core::chat::{self, Connection, Event, Message};
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
}

impl Runner {
    fn connection(&self, workspace: Option<std::path::PathBuf>) -> Connection {
        let settings = &self.store.settings;
        let guest = settings.gateway == letronna_core::gateway::GATEWAYS[0].name;
        Connection {
            endpoint: settings.endpoint.clone(),
            model: settings.model.clone(),
            key: letronna_core::store::load_keys()
                .get(letronna_core::gateway::key_slot(&settings.gateway))
                .cloned()
                .unwrap_or_default(),
            guest,
            tools: !guest,
            workspace,
            plan: self.plan,
        }
    }

    /// Send one prompt and run the agent to completion. Returns the final
    /// answer, or an error worth showing.
    pub fn ask(&mut self, session: usize, prompt: &str) -> Result<String, String> {
        let workspace = self.store.conversations[session].workspace.clone();
        let connection = self.connection(workspace);
        chat::validate(&connection)?;

        {
            let conversation = &mut self.store.conversations[session];
            if conversation.messages.is_empty() && conversation.title.is_empty() {
                conversation.title = prompt.chars().take(40).collect();
            }
            conversation.updated = chat::now();
            conversation.messages.push(Message {
                role: "user".into(),
                content: prompt.to_owned(),
                ..Default::default()
            });
        }
        self.persist();

        let mut turn = Turn::new(connection);
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
                return Ok(answer);
            }
        }
    }

    /// One model call, printed as it streams.
    /// One model call, printed as it streams. `None` when the user stopped
    /// waiting for it.
    fn stream_once(
        &mut self,
        turn: &mut Turn,
        session: usize,
    ) -> Result<Option<Message>, String> {
        let receiver = chat::start(
            turn.connection.clone(),
            self.store.conversations[session].messages.clone(),
        );
        // The model's name lives under the input box, so an answer only needs
        // its bullet here.
        let mut out = Stream::opening_with(render::bullet());
        let mut opened = false;
        let mut waiting = Some(crate::spinner::Spinner::start());
        loop {
            // Keys first, then whatever the model has sent: the pump waits a
            // few milliseconds for a keystroke, which is also what keeps this
            // loop from spinning.
            self.bar.pump();
            if self.bar.interrupted() {
                // Dropping the receiver is enough: the worker finishes into a
                // channel nobody is listening to and goes away.
                waiting.take();
                out.end();
                return Ok(None);
            }
            let event = match receiver.try_recv() {
                Ok(event) => event,
                Err(async_channel::TryRecvError::Empty) => continue,
                Err(async_channel::TryRecvError::Closed) => {
                    return Err("The model connection closed unexpectedly.".to_owned())
                }
            };
            if let Event::Delta(text) = &event {
                if !opened {
                    // The spinner was holding this line; the answer takes it
                    // over. The gap above it was left when the question was
                    // echoed, so nothing more is needed here.
                    waiting.take();
                    opened = true;
                }
                out.push(text);
            }
            turn.absorb(&event);
            if let Event::Finished(result) = event {
                waiting.take();
                out.end();
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
        let ctx = tools::ToolContext {
            workspace: turn.connection.workspace.clone(),
        };
        let outcome = tools::run(&call.name, &call.arguments, &ctx);
        let summary = tools::summary(&call.name, &call.arguments);
        match &outcome {
            Ok(text) => {
                let lines = text.lines().count();
                render::tool(&call.name, &summary, &format!("{lines} lines"), false);
                if !text.trim().is_empty() {
                    render::detail(text, DETAIL_LINES);
                }
            }
            Err(error) => render::tool(&call.name, &summary, &tools::one_line(error), true),
        }
        turn.finish_call(outcome);
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
                self.bar.wait().ok_or("No answer given.")?
            } else {
                let choices: Vec<&str> = options.iter().map(String::as_str).collect();
                let picked = self
                    .bar
                    .choose(&question, Vec::new(), &choices)
                    .ok_or("No answer given.")?;
                render::blank();
                render::said(&render::blue("●"), &bold_question(&question));
                render::said(&render::blue("›"), &render::bold(&options[picked]));
                options[picked].clone()
            };
            turn.finish_call(Ok(reply));
            return Ok(true);
        }

        let ctx = tools::ToolContext {
            workspace: turn.connection.workspace.clone(),
        };
        let summary = tools::summary(&call.name, &call.arguments);

        // A change to a file is shown as a diff above the question, where
        // there is room for it; anything else gets its preview inside the
        // box, which is where the eye already is.
        let mut detail = Vec::new();
        match crate::diff::of(&call.name, &call.arguments, &ctx) {
            Some(rows) => {
                render::blank();
                render::said(&render::blue("●"), &render::bold(&format!("{} {summary}", verb(&call.name))));
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

    /// Start a session, reusing the most recent one when asked to resume.
    pub fn open(&mut self, resume: bool, workspace: Option<std::path::PathBuf>) -> usize {
        if resume && !self.store.conversations.is_empty() {
            let last = self.store.conversations.len() - 1;
            self.store.active = last;
            return last;
        }
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
        other => format!("use {other}"),
    }
}

fn usage(reply: &Message) -> String {
    let mut parts = vec![format!("{} out", reply.tokens)];
    if reply.per_second > 0. {
        parts.push(format!("{:.1} tok/s", reply.per_second));
    }
    if reply.prompt_tokens > 0 {
        parts.push(format!("{} in", reply.prompt_tokens));
        if reply.cached_tokens > 0 {
            parts.push(format!(
                "{}% cached",
                100 * reply.cached_tokens / reply.prompt_tokens
            ));
        }
    }
    parts.join(" · ")
}
