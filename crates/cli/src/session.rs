//! Driving one turn of the agent loop in a terminal.
//!
//! The loop itself lives in `letronna_core::agent`; this supplies the parts
//! that differ in a terminal: waiting on a blocking channel rather than a
//! frame, and asking the user by reading a line rather than drawing a card.

use letronna_core::agent::{Next, Turn};
use letronna_core::chat::{self, Connection, Event, Message};
use letronna_core::gate::{self, Gate, Grants, Mode};
use letronna_core::store::{Conversation, Store};
use letronna_core::{permissions, tools};

use crate::bar::Bar;

use crate::render::{self, Stream};

/// Lines of a tool's output to show before clipping.
const DETAIL_LINES: usize = 12;

pub struct Runner {
    pub store: Store,
    /// Tools approved for the rest of this run, by session.
    pub allowed: Grants,
    /// How much runs without asking: `--mode`, `--yes` or `--plan`, then
    /// `/mode` and `/plan`.
    pub mode: Mode,
    /// Someone can see a card and answer it; false for `-p` with no
    /// terminal on either end.
    pub human: bool,
    /// The rules file error last shown, so that one is shown once.
    pub trouble: Option<String>,
    /// The box at the foot of the window, pumped while a reply streams so
    /// that typing carries on through an answer.
    pub bar: Bar,
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
            plan: self.mode == Mode::Plan,
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
            let ctx = tools::ToolContext::of(&turn.connection);
            // Read afresh each time, since the window may have added one.
            let rules = permissions::load(&permissions::user_file());
            let trouble = permissions::trouble(&rules, ctx.workspace.as_deref());
            if trouble != self.trouble {
                if let Some(error) = &trouble {
                    render::line(&render::red(error));
                }
                self.trouble = trouble;
            }
            let gate = Gate::for_terminal(&ctx, &self.allowed, &rules, self.mode, self.human);
            match turn.next(|call| gate::decide(call, &gate)) {
                Next::Done => return Ok(true),
                Next::Ask => {
                    let results = turn.take_results();
                    self.store.conversations[session].messages.extend(results);
                    self.persist();
                    return Ok(true);
                }
                Next::Run(call) => self.run_call(turn, call, false),
                Next::Refuse(call, why) => {
                    let summary = tools::summary(&call.name, &call.arguments);
                    render::blank();
                    render::tool(&call.name, &summary, &tools::one_line(&why), true);
                    turn.finish_call(Err(why));
                }
                Next::AskUser(call) => {
                    if !self.consult(turn, call)? {
                        return Ok(false);
                    }
                }
            }
        }
    }

    /// Runs `call` and shows what came of it; `approved` when the user said
    /// yes to it just now.
    fn run_call(&mut self, turn: &mut Turn, call: letronna_core::chat::ToolCall, approved: bool) {
        let ctx = tools::ToolContext {
            approved,
            ..tools::ToolContext::of(&turn.connection)
        };
        let summary = tools::summary(&call.name, &call.arguments);
        let outcome = self.run_off_the_loop(&call, ctx, &summary);
        // Every turn of the conversation opens with a blank line, whoever is
        // speaking; a tool result that butts up against the question it came
        // from reads as part of it.
        render::blank();
        match &outcome {
            Ok(text) => {
                let lines = text.lines().count();
                let count = if lines == 1 {
                    "1 line".to_owned()
                } else {
                    format!("{lines} lines")
                };
                render::tool(&call.name, &summary, &count, false);
                if !text.trim().is_empty() {
                    render::detail(text, DETAIL_LINES);
                }
            }
            Err(error) => render::tool(&call.name, &summary, &tools::one_line(error), true),
        }
        turn.finish_call(outcome);
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
                break Err(format!(
                    "The user stopped `{}` while it was running. Ask before starting it again.",
                    call.name
                ));
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
        // Offered only where a rule could cover the call, as the third
        // choice, so the others keep their places.
        let rule = permissions::suggest(&call, &ctx);
        let here = rule
            .as_ref()
            .map(|rule| format!("Yes, and always allow `{}` here", rule.pattern));
        let mut choices = vec!["Yes", "Yes, and stop asking for this"];
        choices.extend(here.as_deref());
        choices.extend(["No, tell it why not", "No, and stop here"]);
        let picked = self.bar.choose(&question, detail, &choices);

        match picked.and_then(|at| choices.get(at).copied()) {
            Some("Yes") => {
                self.run_call(turn, call, true);
                Ok(true)
            }
            Some("Yes, and stop asking for this") => {
                self.allowed
                    .entry(turn.connection.conversation)
                    .or_default()
                    .insert(call.name.clone());
                self.run_call(turn, call, true);
                Ok(true)
            }
            Some("No, and stop here") | None => Ok(false),
            Some("No, tell it why not") => {
                render::tool(&call.name, &summary, "declined", true);
                turn.finish_call(Err(tools::DECLINED.into()));
                Ok(true)
            }
            Some(_) => {
                // Into the user's own rules, held to this project: the
                // project's file may not allow anything.
                let kept = rule.map(|rule| permissions::keep(&permissions::user_file(), rule));
                if let Some(Err(error)) = kept {
                    render::line(&render::red(&error));
                }
                self.run_call(turn, call, true);
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
