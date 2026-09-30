//! Driving one turn of the agent loop in a terminal.
//!
//! The loop itself lives in `letronna_core::agent`; this supplies the parts
//! that differ in a terminal: waiting on a blocking channel rather than a
//! frame, and asking the user by reading a line rather than drawing a card.

use letronna_core::agent::{Next, Nudge, Turn};
use letronna_core::agents;
use letronna_core::approvals::{self, Decision, Request};
use letronna_core::cancel::Cancel;
use letronna_core::chat::{self, Connection, Event, Message, ToolCall};
use letronna_core::gate::{self, Gate, Grants, Mode};
use letronna_core::inbox;
use letronna_core::notify::{self, Why};
use letronna_core::speech::{self, Voice};
use letronna_core::store::{Conversation, Store};
use letronna_core::wake;
use letronna_core::{goal, hooks, permissions, tools};
use std::sync::mpsc;
use std::time::Duration;

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
    /// Whether the user stopped the last turn, which keeps a ring from
    /// starting another before they say something.
    pub stopped: bool,
    /// What calls the user back while the terminal is not looked at.
    pub notices: letronna_core::notify::Notices,
    /// Helpers and workflow runs that ended, once the box is up to say so.
    pub ended: Option<async_channel::Receiver<letronna_core::notify::Ended>>,
    /// Pictures `/image` attached, which go with the next message.
    pub images: Vec<std::path::PathBuf>,
    /// The queue that reads replies aloud, there only while `/voice` is on.
    pub voice: Option<Voice>,
}

impl Runner {
    /// Where a session's requests go and the key or sign-in token they carry.
    pub fn route(&self, session: usize) -> (String, String) {
        let (gateway, _) = self.store.model(session);
        let saved = letronna_core::store::load_keys()
            .get(letronna_core::gateway::key_slot(gateway))
            .cloned()
            .unwrap_or_default();
        let custom = &self.store.settings.custom_endpoint;
        letronna_core::gateway::resolve(gateway, custom, &saved)
    }

    pub fn connection(&self, session: usize) -> Connection {
        let workspace = self.store.conversations[session].workspace.clone();
        let settings = &self.store.settings;
        let (endpoint, key) = self.route(session);
        Connection {
            endpoint,
            model: self.store.model(session).1.to_owned(),
            key,
            tools: true,
            workspace,
            plan: self.mode == Mode::Plan,
            remote: None,
            effort: self.store.effort(session).to_owned(),
            search: settings.search,
            conversation: self.store.conversations[session].id,
            unattended: false,
            wrap_up: false,
        }
    }

    /// Start or stop reading replies aloud to match the setting.
    pub fn match_voice(&mut self) {
        // Only for someone at a terminal: a piped run would be heard by
        // nobody, and ends before the speech would.
        let wanted = self.store.settings.voice && self.human;
        if wanted == self.voice.is_some() {
            return;
        }
        self.voice = wanted.then(|| {
            Voice::start(Box::new(|| {
                Ok(Box::new(speech::System) as Box<dyn speech::Output>)
            }))
        });
    }

    /// Send one prompt and run the agent to completion. Returns the final
    /// answer, or an error worth showing.
    pub fn ask(&mut self, session: usize, prompt: &str) -> Result<String, String> {
        let connection = self.connection(session);
        chat::validate(&connection)?;
        self.store.pin(session);

        {
            let conversation = &mut self.store.conversations[session];
            if conversation.messages.is_empty() && conversation.title.is_empty() {
                let shown = letronna_core::init::typed(prompt).unwrap_or(prompt);
                conversation.title = shown.chars().take(40).collect();
            }
            conversation.updated = chat::now();
            goal::interrupted(&mut conversation.goal);
        }
        // What came meanwhile goes in ahead of the prompt, so the user's
        // words are the last thing the model reads.
        self.take_notices(session, true);
        let workspace = self.store.conversations[session].workspace.as_deref();
        let content = letronna_core::references::expand(prompt, workspace);
        self.store.conversations[session].messages.push(Message {
            role: "user".into(),
            content,
            images: std::mem::take(&mut self.images),
            ..Default::default()
        });
        self.persist();

        let answer = self.run(connection, session);
        self.pursue(session, answer)
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
        let answer = self.run(connection, session);
        self.pursue(session, answer)
    }

    /// After a turn that ended by itself, looks at the session's goal and
    /// carries on toward it, turn after turn, until the look says to stop.
    /// A stop, or anything typed, ends it where it is.
    fn pursue(
        &mut self,
        session: usize,
        mut answer: Result<String, String>,
    ) -> Result<String, String> {
        loop {
            if self.stopped || self.bar.finished() {
                goal::interrupted(&mut self.store.conversations[session].goal);
                self.persist();
                self.show_goal();
                return answer;
            }
            // What was typed is the user's next turn, which the goal waits for.
            if answer.is_err() || self.bar.has_queued() {
                return answer;
            }
            let Some(standing) = goal::active(&self.store.conversations[session].goal) else {
                return answer;
            };
            let connection = self.connection(session);
            let reply = (self.store.conversations[session].messages.iter())
                .rfind(|message| message.role == "assistant")
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let cancel = Cancel::default();
            let (sender, receiver) = mpsc::sync_channel(1);
            let (asking, stop, judged) = (connection.clone(), cancel.clone(), standing.clone());
            std::thread::spawn(move || {
                let _ = sender.send(goal::review(&judged, &asking, &reply, &stop));
            });
            self.bar.working(true);
            self.bar.wait_line_about("goal check");
            let waited = self.wait_on(&receiver);
            self.bar.wait_line(false);
            self.bar.working(false);
            let step = match waited {
                Ok(_) if self.bar.has_queued() => goal::Step::Cleared,
                Ok(step) => step,
                Err(Unheard::Stopped) => {
                    // Stopped as a turn is, so a ring waits for the user too.
                    self.stopped = true;
                    render::blank();
                    render::said(&render::dim("●"), &render::dim("Stopped."));
                    goal::Step::Cleared
                }
                Err(Unheard::Gone) => goal::Step::Pause {
                    notice: "The goal check ended without a verdict. The goal is paused; \
                             /goal resume carries on."
                        .into(),
                    why: "the check failed".into(),
                },
            };
            cancel.set();
            let conversation = &mut self.store.conversations[session];
            if !goal::settle(&mut conversation.goal, &standing, &step) {
                return answer;
            }
            let (notice, message) = match step {
                goal::Step::Continue { notice, message } => (notice, Some(message)),
                goal::Step::Done(notice)
                | goal::Step::Pause { notice, .. }
                | goal::Step::Deferred(notice) => (notice, None),
                goal::Step::Cleared => {
                    goal::interrupted(&mut conversation.goal);
                    self.persist();
                    self.show_goal();
                    return answer;
                }
            };
            render::blank();
            render::said(&render::dim("●"), &render::dim(&notice));
            self.show_goal();
            let Some(message) = message else {
                self.persist();
                return answer;
            };
            let conversation = &mut self.store.conversations[session];
            conversation.messages.push(goal::continuation(message));
            conversation.updated = chat::now();
            self.persist();
            answer = self.run(connection, session);
        }
    }

    /// Ask a side question about the session on a fork of it, with no tools
    /// and one request, waiting where escape can stop it. Nothing of it is kept.
    pub fn btw(&mut self, session: usize, question: &str) -> Result<Message, String> {
        let connection = self.connection(session);
        chat::validate(&connection)?;
        let messages = self.store.conversations[session].messages.clone();
        let cancel = Cancel::default();
        let (stop, question) = (cancel.clone(), question.to_owned());
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = sender.send_blocking(agents::btw(&connection, messages, &question, stop));
        });
        self.bar.working(true);
        self.bar.wait_line_about("btw");
        let answer = loop {
            self.bar.pump();
            if self.bar.interrupted() {
                cancel.set();
                break Err("Stopped.".to_owned());
            }
            match receiver.try_recv() {
                Ok(answer) => break answer,
                Err(async_channel::TryRecvError::Empty) => continue,
                Err(async_channel::TryRecvError::Closed) => {
                    break Err("The side question stopped without an answer.".to_owned());
                }
            }
        };
        self.bar.wait_line(false);
        self.bar.working(false);
        answer
    }

    /// Run the agent to completion on the conversation as it stands.
    fn run(&mut self, connection: Connection, session: usize) -> Result<String, String> {
        self.bar.show_helpers_of(connection.conversation);
        // What the session's kept helpers wake on from now on.
        agents::connected(&connection);
        hooks::new_turn(connection.conversation);
        let mut turn = Turn::new(connection).verifying(self.store.settings.verifies_edits());
        self.stopped = false;
        // What is still being said of the last answer is not wanted now.
        if let Some(voice) = &mut self.voice {
            voice.hush();
        }
        self.bar.working(true);
        // The tab says so too, for whoever has switched away to something
        // else while this runs.
        let busy = crate::title::Busy::start();
        let answer = self.work(&mut turn, session);
        drop(busy);
        if let Some(why) = self.voice.as_ref().and_then(Voice::trouble) {
            render::line(&render::red(&format!("Not read aloud: {why}")));
        }
        // Stopping is not a go-ahead for what was queued: it goes back in
        // the box rather than being sent, as it does in the window.
        if self.stopped {
            self.bar.hand_back();
        } else {
            let id = turn.connection.conversation;
            match &answer {
                Ok(text) => self.tell(id, Why::Finished(text)),
                Err(error) => self.tell(id, Why::Failed(error)),
            }
        }
        self.tell_ended();
        self.bar.working(false);
        // The session's loop goes round again from the turn that took its
        // tick, and a stop ends it.
        let id = self.store.conversations[session].id;
        let ended = (!self.stopped).then(|| answer.as_deref().unwrap_or_default());
        if let Some(said) = wake::turn_ended(id, ended) {
            render::blank();
            render::line(&render::dim(&said));
            // Nothing else paints until the next key.
            crate::screen::settle();
        }
        answer
    }

    /// Call the user back about session `id`, when they have looked away
    /// from the terminal and want to be.
    fn tell(&mut self, id: u64, why: Why) {
        let title = self
            .store
            .index_of(id)
            .map(|ix| self.store.conversations[ix].title.clone())
            .unwrap_or_default();
        let now = std::time::Instant::now();
        let (on, away) = (self.store.settings.notifications, self.bar.away(now));
        self.notices
            .post(on, away, now, || notify::note(id, &title, why));
    }

    /// Call the user back about the helpers and workflow runs that ended
    /// since last time.
    pub fn tell_ended(&mut self) {
        let ended: Vec<notify::Ended> = self
            .ended
            .as_ref()
            .map(|ended| std::iter::from_fn(|| ended.try_recv().ok()).collect())
            .unwrap_or_default();
        for ended in ended {
            self.tell(ended.session, Why::Ended(&ended.what));
        }
    }

    fn work(&mut self, turn: &mut Turn, session: usize) -> Result<String, String> {
        // A reply cut off at the output limit goes on in the next one, and
        // the answer is both.
        let mut answer = String::new();
        let mut sent_back = 0;
        loop {
            let Some(mut reply) = self.stream_once(turn, session)? else {
                self.keep_stopped(turn, session);
                // Ctrl-C stops the turn and leaves; escape only stops the
                // turn, so say so in the one case where you stay.
                if !self.bar.finished() {
                    render::blank();
                    render::said(&render::dim("●"), &render::dim("Stopped."));
                }
                self.stopped = true;
                return Ok(String::new());
            };
            // A reply cut short to send what was queued has nothing to run:
            // what was typed goes in after it, and the model is asked again.
            let hurried = self.bar.hurried();
            let empty = reply.content.is_empty();
            let continuation = continuation(turn, &mut reply, hurried);
            if empty && !reply.content.is_empty() {
                // Taken from its thinking, which was never shown as the answer.
                render::blank();
                let mut out = Stream::opening_with(render::bullet());
                out.push(&reply.content);
                out.end();
            }
            answer.push_str(&reply.content);
            let calls = reply.tool_calls.clone();
            self.store.conversations[session].updated = chat::now();
            // A reply with nothing in it -- empty, or cut off inside a tool
            // call with no text before it -- leaves nothing to keep.
            if !reply.content.is_empty() || !calls.is_empty() {
                self.store.conversations[session].messages.push(reply);
            }
            match continuation {
                Ok(Some(nudge)) => {
                    render::blank();
                    render::said(&render::dim("●"), &render::dim(nudge.why));
                    self.store.conversations[session]
                        .messages
                        .push(nudge.notice);
                    self.persist();
                    continue;
                }
                Ok(None) => {}
                Err(error) => {
                    self.persist();
                    return Err(error);
                }
            }
            self.persist();

            if !hurried && !turn.begin(calls) {
                // Past the limit on rounds the turn ends as it always did; a
                // Stop hook sending the model back would undo the limit.
                if let Some(note) = turn.capped() {
                    render::blank();
                    render::said(&render::dim("●"), &render::dim(&note));
                    return Ok(answer);
                }
                if sent_back >= hooks::STOP_BLOCKS {
                    render::blank();
                    render::said(&render::dim("●"), &render::dim(hooks::STOP_CAPPED));
                    return Ok(answer);
                }
                let Some(feedback) = self.stop_hooks(turn, &answer, sent_back > 0) else {
                    return Ok(answer);
                };
                sent_back += 1;
                render::blank();
                render::said(&render::dim("●"), &render::dim(&tools::one_line(&feedback)));
                self.store.conversations[session].messages.push(Message {
                    role: "user".into(),
                    content: feedback,
                    folded: true,
                    ..Default::default()
                });
                self.persist();
                answer.clear();
                continue;
            }
            if !hurried && !self.work_through(turn, session)? {
                self.stopped = true;
                return Ok(answer);
            }
            // Asked to leave, by ctrl-c while a tool ran, which stopped only
            // that tool, or by ctrl-d: the turn ends here, so nothing queued
            // is sent and the model is not asked again.
            if self.bar.finished() {
                self.stopped = true;
                return Ok(answer);
            }
            // Notices go in after the results, which have to follow the
            // calls they answer, and before what was typed, so the user's
            // words are still read last.
            self.take_notices(session, false);
            // What was typed while it worked goes in after the results, so
            // the model reads it before carrying on rather than once the turn
            // is over. It waited above the box until now, and the box puts
            // it into the conversation as it is taken.
            let typed = self.bar.take_typed();
            if !typed.is_empty() {
                let conversation = &mut self.store.conversations[session];
                let workspace = conversation.workspace.clone();
                conversation
                    .messages
                    .extend(typed.into_iter().map(|typed| Message {
                        role: "user".into(),
                        content: letronna_core::references::expand(&typed, workspace.as_deref()),
                        folded: true,
                        ..Default::default()
                    }));
                self.persist();
            }
            answer.clear();
        }
    }

    /// One model call, printed as it streams.
    /// One model call, printed as it streams. `None` when the user stopped
    /// waiting for it.
    fn stream_once(&mut self, turn: &mut Turn, session: usize) -> Result<Option<Message>, String> {
        let receiver = waitable(chat::start(
            turn.connection.clone(),
            self.store.conversations[session].messages.clone(),
        ));
        // The model's name lives under the input box, so an answer only needs
        // its bullet here.
        let mut out = Stream::opening_with(render::bullet());
        let mut opened = false;
        let mut thought = false;
        let mut explained = false;
        self.bar.wait_line(true);
        loop {
            // Keys first, then whatever the model has sent.
            let event = match self.wait_on_reply(&receiver) {
                Ok(event) => event,
                // Sent now, what was queued goes in after as much of the reply as
                // came. Any calls it asked for were never run, so they are left out.
                Err(Unheard::Stopped) if self.bar.hurried() => {
                    self.bar.wait_line(false);
                    out.end();
                    if let Some(voice) = &mut self.voice {
                        voice.end();
                    }
                    turn.live.calls.clear();
                    break;
                }
                Err(Unheard::Stopped) => {
                    // Dropping the receiver is enough: the worker finishes
                    // into a channel nobody is listening to and goes away.
                    self.bar.wait_line(false);
                    out.end();
                    if let Some(voice) = &mut self.voice {
                        voice.hush();
                    }
                    return Ok(None);
                }
                Err(Unheard::Gone) => {
                    return Err("The model connection closed unexpectedly.".to_owned());
                }
            };
            // The waiting line says why while a failed try waits to be made
            // again or earlier turns are summarized, and goes back to
            // waiting once the reply answers, which may be with thinking
            // that is not shown.
            let why = match &event {
                Event::Retrying(retry) => Some(retry.to_string()),
                Event::Compacting => Some("Compacting…".to_owned()),
                _ => None,
            };
            if let Some(why) = why {
                self.bar.wait_line_about(&why);
                explained = true;
            } else if explained {
                self.bar.wait_line(true);
                explained = false;
            }
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
                if let Some(voice) = &mut self.voice {
                    voice.hear(text);
                }
            }
            // Kept with the session, so a restart need not ask for it again.
            if let Event::Summarized { upto, text } = &event {
                self.store.conversations[session].summarized(*upto, text.clone());
            }
            // Only the switch a failure caused: the requests after it go to
            // the same fallback until the cooldown ends, and say nothing new.
            if let Event::Switched(switch) = &event
                && switch.after.is_some()
            {
                render::blank();
                render::said(&render::dim("●"), &render::dim(&switch.to_string()));
            }
            turn.absorb(&event);
            if let Event::Finished(result) = event {
                self.bar.wait_line(false);
                out.end();
                if let Some(voice) = &mut self.voice {
                    voice.end();
                }
                // Catch up on anything a throttled paint skipped.
                crate::screen::settle();
                result.map_err(|failure| failure.to_string())?;
                break;
            }
        }
        let model = turn.connection.model.clone();
        let reply = turn.take_reply(&model);
        // A reply that only asked for a tool has nothing to caption; the tool
        // line below speaks for it.
        if reply.tokens > 0 && !reply.content.is_empty() {
            render::line(&render::dim(&usage(&reply)));
        }
        Ok(Some(reply))
    }

    /// Run the queued calls, asking first where the rules say to. False when
    /// the user stopped the turn, whose finished results are kept.
    fn work_through(&mut self, turn: &mut Turn, session: usize) -> Result<bool, String> {
        loop {
            let ctx = turn.tool_context();
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
            // Calls that may go together run at once; one the gate would not
            // simply run ends the batch, and is dealt with below on its own.
            let batch = turn.next_batch(
                |call| gate::decide(call, &gate) != gate::Outcome::Run,
                letronna_core::agent::MAX_BATCH,
            );
            if !batch.is_empty() {
                self.run_batch(turn, batch);
                continue;
            }
            match turn.next(|call| gate::decide(call, &gate)) {
                Next::Done => return Ok(true),
                Next::Ask => {
                    let results = turn.take_results();
                    self.store.conversations[session].messages.extend(results);
                    self.persist();
                    // A hook's `continue: false`, shown with the results.
                    return Ok(!hooks::halted(turn.connection.conversation));
                }
                Next::Run(call) if call.name == tools::TODO => self.apply_todo(turn, session, call),
                Next::Run(call) => {
                    if !self.run_call(turn, call, false) {
                        self.keep_stopped(turn, session);
                        return Ok(false);
                    }
                }
                Next::Refuse(call, why) => {
                    let summary = tools::summary(&call.name, &call.arguments);
                    render::blank();
                    render::tool(&call.name, &summary, &tools::one_line(&why), true);
                    turn.finish_call(Err(why));
                }
                Next::AskUser(call) => {
                    // What holds every call for a yes, beside each card.
                    if let Some(error) = hooks::trouble() {
                        render::line(&render::red(&error));
                    }
                    // A plan that did not come through whole goes back to the
                    // model without a question.
                    if call.name != tools::EXIT_PLAN || tools::plan_of(&call.arguments).is_ok() {
                        self.tell(turn.connection.conversation, Why::Waiting(&call, None));
                    }
                    hooks::card(ctx, &call.name);
                    let consulted = self.consult(turn, call)?;
                    show_hook_problems();
                    if !consulted {
                        self.keep_stopped(turn, session);
                        return Ok(false);
                    }
                }
            }
        }
    }

    /// Save what a stopped turn got to, so the model hears next time what ran
    /// and what did not.
    fn keep_stopped(&mut self, turn: &mut Turn, session: usize) {
        let model = turn.connection.model.clone();
        let kept = turn.stop(&model);
        if kept.is_empty() {
            return;
        }
        let conversation = &mut self.store.conversations[session];
        conversation.updated = chat::now();
        conversation.messages.extend(kept);
        self.persist();
    }

    /// Runs `call` and shows what came of it; `approved` when the user said
    /// yes to it just now. False when the user stopped it, which ends the turn.
    fn run_call(
        &mut self,
        turn: &mut Turn,
        call: letronna_core::chat::ToolCall,
        approved: bool,
    ) -> bool {
        let mut ctx = turn.tool_context();
        ctx.approved = approved;
        let summary = tools::summary(&call.name, &call.arguments);
        let ran = self.run_off_the_loop(&call, ctx, &summary);
        let stopped = ran.is_none();
        let outcome =
            ran.unwrap_or_else(|| Err(letronna_core::agent::STOPPED_WHILE_RUNNING.into()));
        print_call(&call.name, &summary, &outcome);
        show_hook_problems();
        turn.finish_call(outcome);
        !stopped
    }

    /// Let the Stop hooks look at the finished turn, whose reply was `last`,
    /// off the loop so escape still works: what one says is left to do, if
    /// anything. `again` when one already sent the model back this turn.
    fn stop_hooks(&mut self, turn: &Turn, last: &str, again: bool) -> Option<String> {
        if !hooks::wanted(hooks::Event::Stop) {
            return None;
        }
        let ctx = turn.tool_context();
        let stop = ctx.cancel.clone();
        let last = last.to_owned();
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = sender.send(hooks::stop(&ctx, &last, again));
        });
        self.bar.wait_line_about("Stop hooks");
        let feedback = match self.wait_on(&receiver) {
            Ok(feedback) => feedback,
            Err(Unheard::Stopped) => {
                stop.set();
                None
            }
            Err(Unheard::Gone) => None,
        };
        self.bar.wait_line(false);
        show_hook_problems();
        feedback
    }

    /// Keep the model's checklist on the session, as the window does, and
    /// show it over the box.
    fn apply_todo(&mut self, turn: &mut Turn, session: usize, call: letronna_core::chat::ToolCall) {
        let todos = &mut self.store.conversations[session].todos;
        let outcome = letronna_core::store::apply_todo(todos, &call.arguments);
        self.bar.set_plan(&self.store.conversations[session].todos);
        let summary = tools::summary(&call.name, &call.arguments);
        render::blank();
        match &outcome {
            Ok(said) => render::tool(&call.name, &summary, said, false),
            Err(error) => render::tool(&call.name, &summary, &tools::one_line(error), true),
        }
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
        let ctx = turn.tool_context();
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
                show_hook_problems();
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
    /// Stopping sets the call's token, which ends a wait or a running command
    /// at once. Anything else is left to finish into a channel nobody is
    /// reading; what it was doing to the disk it will still do, and a
    /// background command it started keeps running under `list_jobs` where it
    /// can be stopped by name. `None` when the user stopped it.
    fn run_off_the_loop(
        &mut self,
        call: &letronna_core::chat::ToolCall,
        ctx: tools::ToolContext,
        summary: &str,
    ) -> Option<Result<String, String>> {
        let (name, arguments) = (call.name.clone(), call.arguments.clone());
        let stop = ctx.cancel.clone();
        let (sender, receiver) = mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let _ = sender.send(tools::run(&name, &arguments, &ctx));
        });

        self.bar
            .wait_line_about(&format!("{}({summary})", call.name));
        let outcome = match self.wait_on(&receiver) {
            Ok(outcome) => Some(outcome),
            Err(Unheard::Stopped) => {
                stop.set();
                None
            }
            Err(Unheard::Gone) => Some(Err(format!("`{}` stopped without finishing.", call.name))),
        };
        self.bar.wait_line(false);
        outcome
    }

    /// What comes over `receiver`, pumping the keys meanwhile, or why nothing
    /// did. Between pumps it blocks on the channel for as long as the pump
    /// does not wait for keys itself, so a wait with no box up does not spin.
    /// `wait_on` for a reply, which a "send now" also cuts short.
    fn wait_on_reply<T>(&mut self, receiver: &mpsc::Receiver<T>) -> Result<T, Unheard> {
        let patience = self.bar.patience();
        wait_on(receiver, patience, || {
            self.bar.pump();
            // Helpers working in the background ask while the model talks
            // too. Only the rules answer here: a question left open would
            // hold up the reply, which has a deadline of its own.
            self.answer_helpers(false);
            self.bar.interrupted() || self.bar.hurried()
        })
    }

    fn wait_on<T>(&mut self, receiver: &mpsc::Receiver<T>) -> Result<T, Unheard> {
        let patience = self.bar.patience();
        wait_on(receiver, patience, || {
            self.bar.pump();
            // A helper started by this call asks for its yes while the call
            // waits on it, so unanswered here the two would wait on each other.
            self.answer_helpers(true);
            self.bar.interrupted()
        })
    }

    /// Ask the user about a call. False means they ended the turn.
    fn consult(
        &mut self,
        turn: &mut Turn,
        call: letronna_core::chat::ToolCall,
    ) -> Result<bool, String> {
        if call.name == tools::ASK_USER {
            let args = tools::parse_args_for(&call.name, &call.arguments).unwrap_or_default();
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
        if call.name == tools::EXIT_PLAN {
            return Ok(self.put_plan(turn, &call));
        }

        let ctx = turn.tool_context();
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
                detail = card_lines(&tools::preview(&call.name, &call.arguments, &ctx));
            }
        }

        let question = format!("Letronna wants to {}", intent(&call.name, &summary));
        // Offered only where a rule could cover the call, as the third
        // choice, so the others keep their places.
        // A call asked about each time, as a sign-in is, offers neither.
        let each_time = tools::asked_each_time(&call.name);
        let rule = permissions::suggest(&call, &ctx).filter(|_| !each_time);
        let here = rule
            .as_ref()
            .map(|rule| format!("Yes, and always allow `{}` here", rule.pattern));
        let mut choices = vec!["Yes"];
        // An upload is asked about each time, so there is no stopping that.
        let args = tools::parse_args_for(&call.name, &call.arguments).unwrap_or_default();
        if gate::grants_for_session(&call.name, &args) && !each_time {
            choices.push("Yes, and stop asking for this");
        }
        choices.extend(here.as_deref());
        choices.extend(["No, tell it why not", "No, and stop here"]);
        let picked = self.bar.choose(&question, detail, &choices);

        match picked.and_then(|at| choices.get(at).copied()) {
            Some("Yes") => Ok(self.run_call(turn, call, true)),
            Some("Yes, and stop asking for this") => {
                self.allowed
                    .entry(turn.connection.conversation)
                    .or_default()
                    .insert(gate::grant(&call));
                Ok(self.run_call(turn, call, true))
            }
            Some("No, and stop here") | None => Ok(false),
            Some("No, tell it why not") => {
                render::tool(&call.name, &summary, "declined", true);
                // None when the user left instead, which ends the turn as
                // stopping here does.
                let Some(reason) = self.bar.reason("Why not?") else {
                    return Ok(false);
                };
                turn.finish_call(Err(tools::declined(&reason)));
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

    /// Print the plan made in plan mode and ask whether to carry it out.
    /// False means the user ended the turn.
    fn put_plan(&mut self, turn: &mut Turn, call: &letronna_core::chat::ToolCall) -> bool {
        // A plan that did not come through whole goes back to the model;
        // approving it would lift plan mode for nothing.
        let plan = match tools::plan_of(&call.arguments) {
            Ok(plan) => plan,
            Err(error) => {
                render::blank();
                render::tool(&call.name, "", &tools::one_line(&error), true);
                turn.finish_call(Err(error));
                return true;
            }
        };
        render::blank();
        let mut shown = Stream::opening_with(render::bullet());
        shown.push(&plan);
        shown.end();
        let picked = self.bar.choose(
            "Carry out this plan?",
            Vec::new(),
            &[
                "Yes, carry it out",
                "No, keep planning",
                "No, and stop here",
            ],
        );
        let approved = match picked {
            Some(0) => true,
            Some(1) => false,
            _ => return false,
        };
        // Off for the session, not only this turn: the next prompt's
        // connection is built from it, and the footer says so.
        if approved {
            self.mode = letronna_core::gate::Mode::Default;
            let note = crate::banner::footer(self);
            self.bar.set_footer(note);
        }
        turn.answer_plan(approved);
        render::blank();
        let outcome = if approved {
            "approved; plan mode is off"
        } else {
            "keep planning"
        };
        render::tool(&call.name, "", outcome, false);
        true
    }

    /// Start answering helpers' requests for a yes: in the box's loops when it
    /// is up, and otherwise on a thread of its own, so that a helper is heard
    /// however the main thread waits on the call it runs under. `terminal`
    /// says whether there is someone to ask without the box.
    pub fn serve_helpers(&mut self, terminal: bool) {
        if self.bar.serve() {
            return;
        }
        let mode = self.mode;
        let rules = letronna_core::permissions::load(&letronna_core::permissions::user_file());
        let requests = approvals::serve();
        std::thread::spawn(move || {
            // The session's grants are kept on the main thread, so here only
            // the mode and the rules answer for the user.
            let none = Grants::new();
            for request in requests {
                let decision = match settled(&request, mode, &none, &rules, terminal) {
                    Some(Decision::Deny(why)) => {
                        declined(&request, &why);
                        Decision::Deny(why)
                    }
                    Some(decision) => decision,
                    // A bar that was never installed asks on the line, as the
                    // session's own cards are asked without the box.
                    None => ask_about(&mut Bar::new(String::new()), &request),
                };
                request.answer(decision);
            }
        });
    }

    /// Answer every helper waiting on a yes that the session's own rules
    /// settle, and ask the user about the rest -- or, when `ask` is false,
    /// keep the first of those back for a later call to ask.
    pub fn answer_helpers(&mut self, ask: bool) {
        while let Some(request) = self.bar.asked() {
            let rules = letronna_core::permissions::load(&letronna_core::permissions::user_file());
            let decision = match settled(&request, self.mode, &self.allowed, &rules, true) {
                Some(decision) => decision,
                None if ask => {
                    let why = Why::Waiting(&request.call, Some(&request.label));
                    self.tell(request.root, why);
                    ask_about(&mut self.bar, &request)
                }
                None => {
                    self.bar.hold(request);
                    return;
                }
            };
            request.answer(decision);
        }
    }

    pub fn persist(&mut self) {
        if let Err(error) = self.store.save() {
            letronna_core::diag::event(
                "save_failed",
                &[("error", error.kind().to_string().into())],
            );
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
            self.bar.set_plan(&conversation.todos);
        }
        self.show_goal();
        // Each session has its own model, and the footer names it.
        let note = crate::banner::footer(self);
        self.bar.set_footer(note);
    }

    /// Show the open session's goal over the box, as the window does, or
    /// nothing when it has none.
    pub fn show_goal(&mut self) {
        let conversation = self.store.conversations.get(self.store.active);
        let line = conversation.and_then(|c| c.goal.as_ref().map(goal::Goal::line));
        self.bar.set_goal(line);
    }

    /// A session of its own.
    pub fn start(&mut self, workspace: Option<std::path::PathBuf>) -> usize {
        self.store.conversations.push(Conversation {
            id: self.store.reserve_id(),
            title: String::new(),
            updated: chat::now(),
            workspace,
            ..Default::default()
        });
        let index = self.store.conversations.len() - 1;
        self.store.active = index;
        self.bar.set_plan(&[]);
        self.bar.set_goal(None);
        let note = crate::banner::footer(self);
        self.bar.set_footer(note);
        index
    }
}

/// Why a wait on another thread ended with nothing from it.
#[derive(Debug, PartialEq)]
enum Unheard {
    /// The user stopped waiting.
    Stopped,
    /// The thread went away without sending anything.
    Gone,
}

/// What comes over `receiver`, asking `stopped` before each look at it; a
/// look blocks up to `patience`.
/// The notice sending the model back after `reply`, if it needs one, and
/// never for a reply `hurried` cut short: what was typed goes in next, and
/// a notice would be answered by a reply that the same hurry cuts short at
/// once, empty, until the turn ends saying the model returned no text.
fn continuation(
    turn: &mut Turn,
    reply: &mut Message,
    hurried: bool,
) -> Result<Option<Nudge>, String> {
    if !hurried {
        return turn.continuation(reply);
    }
    // Some routes turn down an assistant message with no content.
    if reply.content.trim().is_empty() {
        reply.content.clear();
    }
    Ok(None)
}

fn wait_on<T>(
    receiver: &mpsc::Receiver<T>,
    patience: Duration,
    mut stopped: impl FnMut() -> bool,
) -> Result<T, Unheard> {
    loop {
        if stopped() {
            return Err(Unheard::Stopped);
        }
        match receiver.recv_timeout(patience) {
            Ok(value) => return Ok(value),
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(Unheard::Gone),
        }
    }
}

/// `events` passed on to a channel a wait can block on for a while, which
/// an `async_channel` cannot do outside an executor. The thread passing
/// them on ends when either side goes away.
fn waitable<T: Send + 'static>(events: async_channel::Receiver<T>) -> mpsc::Receiver<T> {
    let (sender, receiver) = mpsc::sync_channel(1);
    std::thread::spawn(move || {
        while let Ok(event) = events.recv_blocking() {
            if sender.send(event).is_err() {
                return;
            }
        }
    });
    receiver
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
    // The fences' tags are for the model; here a few words say where the
    // text came from.
    let (text, from) = match outcome {
        Ok(text) | Err(text) => letronna_core::untrusted::shown(name, text),
    };
    let from = from.map(|from| format!(", {from}")).unwrap_or_default();
    match outcome {
        Ok(_) => {
            let lines = text.lines().count();
            let count = if lines == 1 {
                "1 line".to_owned()
            } else {
                format!("{lines} lines")
            };
            render::tool(name, summary, &format!("{count}{from}"), false);
            if !text.trim().is_empty() {
                render::detail(&text, DETAIL_LINES);
            }
        }
        Err(_) => render::tool(
            name,
            summary,
            &format!("{}{from}", tools::one_line(&text)),
            true,
        ),
    }
}

/// What the model is told about a call the user stopped while it ran.
fn stopped(name: &str) -> String {
    format!("The user stopped `{name}` while it was running. Ask before starting it again.")
}

/// Shows the hook failures since the last look, each once.
fn show_hook_problems() {
    for problem in hooks::problems() {
        render::line(&render::red(&problem));
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

/// A tool's preview as the lines of an approval card, cut as its output is
/// and saying so: a card that stopped without a word would be approved as
/// if the call ended there.
fn card_lines(preview: &str) -> Vec<String> {
    let total = preview.lines().count();
    let mut lines: Vec<String> = preview
        .lines()
        .take(DETAIL_LINES)
        .map(str::to_owned)
        .collect();
    if total > DETAIL_LINES {
        lines.push(format!("… {} more lines", total - DETAIL_LINES));
    }
    lines
}

pub fn intent(name: &str, summary: &str) -> String {
    match name {
        "write_file" => format!("write {summary}"),
        "edit_file" => format!("edit {summary}"),
        // Every path, since the card below shows only the patch's first lines.
        "apply_patch" => format!("change {summary}"),
        "run_command" => "run a command".to_owned(),
        "browse" | "use_page" => summary.to_owned(),
        "search_web" => "search the web".to_owned(),
        other => format!("use {other}"),
    }
}

/// What the session's own gate makes of a helper's call, or `None` when
/// only the user can say: with no terminal to ask on, that is a no.
pub fn settled(
    request: &Request,
    mode: Mode,
    allowed: &Grants,
    rules: &Result<Vec<letronna_core::permissions::Rule>, String>,
    terminal: bool,
) -> Option<Decision> {
    let workspace = std::env::current_dir().ok();
    judged(
        &request.call,
        request.root,
        workspace,
        mode,
        allowed,
        rules,
        terminal,
    )
}

/// `settled` for a call made by session `root` working in `workspace`,
/// which after `-c` or `-r` is the folder the session was made in, not the
/// one the program was started from.
pub fn judged(
    call: &ToolCall,
    root: u64,
    workspace: Option<std::path::PathBuf>,
    mode: Mode,
    allowed: &Grants,
    rules: &Result<Vec<letronna_core::permissions::Rule>, String>,
    terminal: bool,
) -> Option<Decision> {
    let mut ctx = tools::ToolContext::default();
    ctx.conversation = root;
    ctx.workspace = workspace;
    let gate = Gate::for_terminal(&ctx, allowed, rules, mode, terminal);
    match gate::decide(call, &gate) {
        gate::Outcome::Run => Some(Decision::Once),
        gate::Outcome::Refuse(why) => Some(Decision::Deny(why)),
        gate::Outcome::Ask => (!terminal).then(|| Decision::Deny(String::new())),
    }
}

/// Put a helper's request to the user: in the box when `bar` is up, on the
/// line when not.
fn ask_about(bar: &mut Bar, request: &Request) -> Decision {
    let call = &request.call;
    let mut ctx = tools::ToolContext::default();
    ctx.conversation = request.root;
    hooks::card(ctx, &call.name);
    let summary = tools::summary(&call.name, &call.arguments);
    let question = format!(
        "Helper {} wants to {}",
        request.label,
        intent(&call.name, &summary)
    );
    let detail = card_lines(&request.preview);
    let picked = bar.interject(
        &question,
        detail,
        &["Yes", "Yes, and stop asking it for this", "No"],
    );
    match picked {
        Some(0) => Decision::Once,
        Some(1) => Decision::ForRun,
        _ => Decision::Deny(String::new()),
    }
}

/// Say under the tool that a helper's call was declined, for `why` or for
/// want of anyone to ask, since nothing else would show why the helper came
/// back without what it went for.
fn declined(request: &Request, why: &str) {
    let call = &request.call;
    let summary = tools::summary(&call.name, &call.arguments);
    let why = if why.is_empty() {
        format!(
            "declined for helper {}: nobody is here to ask (a mode that allows it would)",
            request.label
        )
    } else {
        format!(
            "declined for helper {}: {}",
            request.label,
            tools::one_line(why)
        )
    };
    render::blank();
    render::tool(&call.name, &summary, &why, true);
}

pub(crate) fn usage(reply: &Message) -> String {
    let mut parts = vec![format!("{} out", reply.tokens)];
    if reply.per_second > 0. {
        parts.push(format!("{:.1} tok/s", reply.per_second));
    }
    let input = reply.input_tokens();
    if input > 0 {
        parts.push(format!("{input} in"));
        parts.extend(reply.fullness());
        if reply.cached_tokens > 0 {
            parts.push(format!("{}% cached", 100 * reply.cached_tokens / input));
        }
    }
    parts.join(" · ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::{Unheard, wait_on, waitable};
    use serde_json::json;
    use std::cell::Cell;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn a_long_preview_ends_its_card_with_how_many_lines_were_left_out() {
        let short = card_lines("one\ntwo");
        assert_eq!(short, ["one", "two"]);
        let long = (1..=30)
            .map(|n| n.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let card = card_lines(&long);
        assert_eq!(card.len(), DETAIL_LINES + 1);
        assert_eq!(card[DETAIL_LINES - 1], DETAIL_LINES.to_string());
        assert_eq!(
            card[DETAIL_LINES],
            format!("… {} more lines", 30 - DETAIL_LINES)
        );
    }

    #[test]
    fn a_wait_returns_what_was_sent() {
        let (sender, receiver) = mpsc::sync_channel(1);
        sender.send("done").unwrap();
        let waited = wait_on(&receiver, Duration::from_secs(5), || false);
        assert_eq!(waited, Ok("done"));
    }

    #[test]
    fn a_look_that_times_out_hands_back_to_the_keys() {
        let (_sender, receiver) = mpsc::sync_channel::<&str>(1);
        let started = Instant::now();
        let looks = Cell::new(0);
        let waited = wait_on(&receiver, Duration::from_millis(20), || {
            looks.set(looks.get() + 1);
            looks.get() > 1
        });
        // One look found nothing, waited out its patience and asked again.
        assert_eq!(waited, Err(Unheard::Stopped));
        assert_eq!(looks.get(), 2);
        assert!(started.elapsed() >= Duration::from_millis(20));
    }

    #[test]
    fn a_thread_that_goes_away_unheard_is_gone() {
        let (sender, receiver) = mpsc::sync_channel::<&str>(1);
        drop(sender);
        let waited = wait_on(&receiver, Duration::from_millis(20), || false);
        assert_eq!(waited, Err(Unheard::Gone));
    }

    #[test]
    fn a_wait_with_no_keys_to_read_blocks_rather_than_spins() {
        let (sender, receiver) = mpsc::sync_channel(1);
        let late = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            let _ = sender.send("done");
        });
        let started = std::time::Instant::now();
        let looks = Cell::new(0u128);
        let waited = wait_on(&receiver, Duration::from_millis(20), || {
            looks.set(looks.get() + 1);
            false
        });
        assert_eq!(waited, Ok("done"));
        // Each look but the last waits its 20 ms; a spinning wait would
        // look millions of times.
        let most = started.elapsed().as_millis() / 20 + 2;
        assert!(looks.get() <= most, "{} looks, at most {most}", looks.get());
        late.join().unwrap();
    }

    #[test]
    fn the_models_events_come_through_in_order_and_then_it_is_gone() {
        let (sender, events) = async_channel::bounded(64);
        let receiver = waitable(events);
        sender.send_blocking(1).unwrap();
        sender.send_blocking(2).unwrap();
        drop(sender);
        let patience = Duration::from_millis(20);
        assert_eq!(wait_on(&receiver, patience, || false), Ok(1));
        assert_eq!(wait_on(&receiver, patience, || false), Ok(2));
        assert_eq!(wait_on(&receiver, patience, || false), Err(Unheard::Gone));
    }

    #[test]
    fn a_reply_nobody_waits_for_lets_the_models_worker_go() {
        let (sender, events) = async_channel::bounded(64);
        drop(waitable(events));
        // The thread passing events on notices on the next one and lets go
        // of the model's channel, so the worker's sends start failing.
        let _ = sender.send_blocking(1);
        let started = Instant::now();
        while !sender.is_closed() {
            assert!(started.elapsed() < Duration::from_secs(5), "still open");
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    static MET: AtomicUsize = AtomicUsize::new(0);

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
    fn a_reply_cut_short_to_send_what_was_typed_is_never_sent_back() {
        let edited = || {
            let mut turn = Turn::new(Connection {
                tools: true,
                workspace: Some("project".into()),
                ..Default::default()
            })
            .verifying(true);
            turn.begin(vec![ToolCall {
                id: "edit-1".into(),
                name: "edit_file".into(),
                arguments: "{}".into(),
            }]);
            turn.finish_call(Ok("edited".into()));
            turn.take_results();
            turn
        };
        let reply = |turn: &mut Turn, text: &str| {
            turn.absorb(&Event::Delta(text.into()));
            turn.take_reply("some-model")
        };
        let mut turn = edited();
        let mut cut = reply(&mut turn, "Now I will run the tes");
        assert!(matches!(continuation(&mut turn, &mut cut, true), Ok(None)));
        let mut blank = reply(&mut turn, " ");
        assert!(matches!(
            continuation(&mut turn, &mut blank, true),
            Ok(None)
        ));
        assert!(blank.content.is_empty());
        // Nor did being cut short use up the check for the answer that comes.
        let mut done = reply(&mut turn, "Fixed it.");
        assert!(continuation(&mut turn, &mut done, false).unwrap().is_some());
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

    fn made(name: &str, arguments: serde_json::Value) -> ToolCall {
        ToolCall {
            id: format!("{name}-1"),
            name: name.into(),
            arguments: arguments.to_string(),
        }
    }

    #[test]
    fn an_edit_is_judged_in_the_folder_the_session_works_in() {
        let dir = std::env::temp_dir().join(format!("letronna-judged-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let (project, elsewhere) = (dir.join("project"), dir.join("elsewhere"));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&elsewhere).unwrap();
        let rules = Ok(Vec::new());
        let judge = |workspace: &std::path::Path, path: &str| {
            let call = made("write_file", json!({ "path": path, "content": "x" }));
            let (none, mode) = (Grants::new(), Mode::AcceptEdits);
            judged(
                &call,
                1,
                Some(workspace.to_owned()),
                mode,
                &none,
                &rules,
                false,
            )
        };
        assert_eq!(judge(&project, "new.txt"), Some(Decision::Once));
        let inside = project.join("new.txt").display().to_string();
        assert_eq!(judge(&project, &inside), Some(Decision::Once));
        // The same path is another folder's when the session is not here.
        assert!(matches!(
            judge(&elsewhere, &inside),
            Some(Decision::Deny(_))
        ));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn bypass_still_holds_back_a_denied_or_risky_call() {
        use letronna_core::permissions::{Effect, Rule};
        let dir = std::env::temp_dir().join(format!("letronna-bypass-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let rules = Ok(vec![Rule {
            tool: "run_command".into(),
            pattern: "git push".into(),
            effect: Effect::Deny,
            project: None,
        }]);
        let judge = |line: &str| {
            let call = made("run_command", json!({ "command": line }));
            let (none, mode) = (Grants::new(), Mode::Bypass);
            judged(&call, 1, Some(dir.clone()), mode, &none, &rules, false)
        };
        assert_eq!(judge("echo hi"), Some(Decision::Once));
        assert!(
            matches!(judge("git push origin main"), Some(Decision::Deny(why)) if !why.is_empty())
        );
        assert!(matches!(judge("git reset --hard"), Some(Decision::Deny(why)) if !why.is_empty()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_patch_question_names_every_file() {
        let patch = "*** Begin Patch\n*** Update File: a.rs\n@@\n-a\n+b\n\
                     *** Delete File: b.rs\n*** End Patch";
        let args = json!({ "patch": patch }).to_string();
        let summary = tools::summary("apply_patch", &args);
        assert_eq!(intent("apply_patch", &summary), "change a.rs, b.rs");
    }
}
