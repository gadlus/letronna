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
        let mut answer = String::new();
        loop {
            let reply = self.stream_once(&mut turn, session)?;
            answer = reply.content.clone();
            let calls = reply.tool_calls.clone();
            self.store.conversations[session].updated = chat::now();
            self.store.conversations[session].messages.push(reply);
            self.persist();

            if !turn.begin(calls) {
                return Ok(answer);
            }
            if !self.work_through(&mut turn, session)? {
                return Ok(answer);
            }
        }
    }

    /// One model call, printed as it streams.
    fn stream_once(&mut self, turn: &mut Turn, session: usize) -> Result<Message, String> {
        let receiver = chat::start(
            turn.connection.clone(),
            self.store.conversations[session].messages.clone(),
        );
        let mut out = Stream::default();
        let mut printed_header = false;
        loop {
            let event = receiver
                .recv_blocking()
                .map_err(|_| "The model connection closed unexpectedly.".to_owned())?;
            if let Event::Delta(text) = &event {
                if !printed_header {
                    render::blank();
                    render::line(&render::blue(&self.store.settings.model));
                    printed_header = true;
                }
                out.push(text);
            }
            turn.absorb(&event);
            if let Event::Finished(result) = event {
                out.end();
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
        Ok(reply)
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
            render::blank();
            render::line(&bold_question(&question));
            if let Some(options) = args["options"].as_array() {
                for (i, option) in options.iter().filter_map(|o| o.as_str()).enumerate() {
                    render::line(&render::dim(&format!("  {}. {option}", i + 1)));
                }
            }
            let reply = crate::input::read(&format!("{}  ", render::blue("›")))
                .ok_or("No answer given.")?;
            turn.finish_call(Ok(reply));
            return Ok(true);
        }

        let ctx = tools::ToolContext {
            workspace: turn.connection.workspace.clone(),
        };
        render::blank();
        render::line(&format!(
            "{} {}",
            render::bold("Letronna wants to"),
            render::bold(&intent(&call.name, &tools::summary(&call.name, &call.arguments)))
        ));
        render::detail(&tools::preview(&call.name, &call.arguments, &ctx), DETAIL_LINES);
        render::line(&render::dim("[y] once   [a] always   [n] no   [q] stop"));
        let answer = crate::input::read(&format!("{}  ", render::blue("›")))
            .unwrap_or_else(|| "n".into())
            .trim()
            .to_lowercase();

        match answer.chars().next().unwrap_or('n') {
            'y' => {
                self.run_call(turn, call);
                Ok(true)
            }
            'a' => {
                self.allowed.insert(call.name.clone());
                self.run_call(turn, call);
                Ok(true)
            }
            'q' => Ok(false),
            _ => {
                render::tool(&call.name, "", "declined", true);
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
