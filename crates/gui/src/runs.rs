//! Runs beside the chat: `/bg`, chat-app messages and scheduled jobs.
//!
//! Each is a driver run in a session of its own, so none takes over the
//! window's one turn or the view. The window stays the only writer of each
//! session it holds: a run it starts works on a copy, in a transcript kept
//! with the session's helpers, and when the run ends its new messages are
//! applied to the session here and saved as a turn's are. A scheduled run
//! writes a new session of its own, which the sidebar lists as running, with
//! its stop, while the window follows what the run writes there. A chat
//! message for a session with a run going is left for that run as a `Later`
//! notice, which it reads before it counts as done, rather than racing it.
//!
//! A run of another Letronna's runner, such as a terminal's, is seen only
//! through its session file, which the window follows as it does every
//! session another Letronna writes: with no running mark and no stop.

use crate::prelude::*;
use letronna_core::approvals;
use letronna_core::cancel::Cancel;
use letronna_core::chat::Remote;
use letronna_core::driver::{self, Allow, Outcome, Spec};
use letronna_core::inbox::{self, Notice, When};
use letronna_core::messaging::Source;
use std::collections::BTreeMap;

/// Runs the window starts at once: a few chats and `/bg` tasks side by
/// side, without crowding the provider's rate limit. The rest wait for a
/// place rather than being refused, since somebody asked for each.
pub(crate) const AT_ONCE: usize = 4;

/// A run beside the chat, as the sidebar lists it and a stop reaches it.
pub(crate) struct Run {
    /// Its session's title, or the scheduled job's name.
    pub(crate) label: String,
    pub(crate) cancel: Cancel,
    /// The chat app that sent it, which hears its answer.
    pub(crate) source: Option<Source>,
    /// How the chat app is described to the model, kept for a queued start.
    remote: Option<Remote>,
    /// Whether the window holds its session and applies the run to it; a
    /// scheduled run writes a new session of its own until it ends.
    pub(crate) held: bool,
    /// Waiting for a place, not started yet.
    pub(crate) queued: bool,
}

/// A held run's end, sent from its thread.
pub(crate) struct Finished {
    session: u64,
    /// Messages the session had when the run began, its prompt included.
    from: usize,
    /// What the run added after them.
    added: Vec<Message>,
    /// Its checklist as the run left it.
    todos: Vec<Todo>,
    /// Its answer, or why there is none, for the chat app that sent it.
    text: String,
    stopped: bool,
    failed: bool,
}

/// How a held run ended, for what follows it.
#[derive(Clone, Copy)]
struct End {
    stopped: bool,
    failed: bool,
}

/// The prompt of a `/bg <prompt>` line, which runs beside the chat.
pub(crate) fn background_prompt(line: &str) -> Option<&str> {
    let rest = line.trim().strip_prefix("/bg")?;
    // "/bgx" is not the command, and "/bg" alone asks for nothing.
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    Some(rest.trim()).filter(|prompt| !prompt.is_empty())
}

/// Leave chat message `text` from `place` for the run holding session `id`,
/// as a `Later` notice it reads before it answers; false when none holds it.
fn leave_for_run(runs: &BTreeMap<u64, Run>, id: u64, place: &str, text: &str) -> bool {
    if !runs.contains_key(&id) {
        return false;
    }
    let notice = Notice {
        from: place.into(),
        when: When::Later,
        text: text.into(),
    };
    inbox::push(id, notice);
    true
}

/// Take a finished run into its session, once: only while the run is
/// listed, which this ends, and only onto the history it started from.
/// Gives the run back, or `None` for an end already taken.
fn apply(
    runs: &mut BTreeMap<u64, Run>,
    conversations: &mut [Conversation],
    finished: &mut Finished,
) -> Option<Run> {
    let run = runs.remove(&finished.session)?;
    // Sends and edits wait while a run holds the session, so it is always
    // as the run left it; were it not, what the run added stays unapplied.
    if let Some(conversation) = conversations
        .iter_mut()
        .find(|conversation| conversation.id == finished.session)
        && conversation.messages.len() == finished.from
    {
        conversation.messages.append(&mut finished.added);
        conversation.todos = std::mem::take(&mut finished.todos);
        conversation.updated = backend::now();
    }
    Some(run)
}

/// What a run came to, as a chat app is told it.
fn said(report: &driver::Report) -> String {
    match &report.status {
        Outcome::Failed(error) => format!("Did not finish: {error}"),
        Outcome::Stopped => "Stopped.".into(),
        _ if report.text.trim().is_empty() => "That did not produce an answer.".into(),
        _ => report.text.clone(),
    }
}

impl Chat {
    /// Take each held run's end as it comes.
    pub(crate) fn watch_runs(ends: async_channel::Receiver<Finished>, cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            while let Ok(finished) = ends.recv().await {
                if this
                    .update(cx, |this, cx| this.finish_run(finished, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// `/bg <prompt>`: a new session on the open one's project, run beside
    /// the chat while the view stays where it is.
    pub(crate) fn run_in_background(&mut self, prompt: &str, cx: &mut Context<Self>) {
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .clone();
        // Typed here, so its references read the user's files as a sent
        // prompt's do.
        let content = letronna_core::references::expand(prompt, workspace.as_deref());
        let id = self.store.reserve_id();
        self.store.conversations.push(Conversation {
            id,
            title: prompt.chars().take(40).collect(),
            updated: backend::now(),
            workspace,
            ..Default::default()
        });
        let message = Message {
            role: "user".into(),
            content,
            ..Default::default()
        };
        self.run_beside(id, message, None, None, cx);
    }

    /// Put `message` to session `id` as a run beside the chat, started once
    /// fewer than `AT_ONCE` are going; `source` hears its answer.
    pub(crate) fn run_beside(
        &mut self,
        id: u64,
        message: Message,
        source: Option<Source>,
        remote: Option<Remote>,
        cx: &mut Context<Self>,
    ) {
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        // The run starts from what was said before, which a session let go
        // of has to be read back for.
        self.fill_now(ix);
        let conversation = &mut self.store.conversations[ix];
        conversation.updated = backend::now();
        // What came in meanwhile goes first, so the prompt is read last.
        conversation
            .messages
            .extend(inbox::fold(&inbox::take(id, true)));
        conversation.messages.push(message);
        let label = conversation.title.clone();
        self.save();
        let run = Run {
            label,
            cancel: Cancel::default(),
            source,
            remote,
            held: true,
            queued: true,
        };
        self.runs.insert(id, run);
        self.start_queued(cx);
        cx.notify();
    }

    /// Start queued runs, in session order, while there is a place.
    fn start_queued(&mut self, cx: &mut Context<Self>) {
        loop {
            let going = self.runs.values().filter(|r| r.held && !r.queued).count();
            let next = self.runs.iter().find(|(_, run)| run.queued);
            let (true, Some((&id, _))) = (going < AT_ONCE, next) else {
                return;
            };
            self.start_run(id, cx);
        }
    }

    /// Start run `id`, or end it at once saying why it could not start.
    fn start_run(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(run) = self.runs.get_mut(&id) else {
            return;
        };
        run.queued = false;
        let (cancel, remote) = (run.cancel.clone(), run.remote.clone());
        if let Err(error) = self.spawn_run(id, cancel, remote)
            && let Some(run) = self.runs.remove(&id)
        {
            let end = End {
                stopped: false,
                failed: true,
            };
            self.ended(run, id, &format!("Did not start: {error}"), end, cx);
        }
    }

    /// Run session `id` on a copy of it, in a transcript with its helpers'
    /// that only the run writes, and send its end back when it is done.
    fn spawn_run(&self, id: u64, cancel: Cancel, remote: Option<Remote>) -> Result<(), String> {
        let ix = self.store.index_of(id).ok_or("its session was deleted.")?;
        let mut connection = self.connection_for(ix, remote)?;
        let conversation = &self.store.conversations[ix];
        connection.workspace = conversation.workspace.clone();
        // What the user asked for, for the judge in Auto mode.
        connection.typed = letronna_core::judge::typed(&conversation.messages).into();
        let workspace = conversation.workspace.clone();
        let (n, mut transcript) = Transcript::create(id, &conversation.title, workspace)?;
        transcript.conversation.messages = conversation.messages.clone();
        transcript.conversation.todos = conversation.todos.clone();
        let from = conversation.messages.len();
        let path = transcript.path.clone();
        let spec = Spec {
            connection,
            transcript,
            id,
            owner: id,
            root: id,
            run: id,
            label: conversation.title.clone(),
            allow: Allow::All,
            rounds: letronna_core::agent::MAX_ROUNDS,
            cancel,
            budget: None,
            status: Default::default(),
            depth: 0,
            fork: false,
            from: 0,
            schema: None,
            watch: None,
            // Its own gate, as a helper's: a call that needs a yes gets a
            // card, in the window or in the chat that sent it.
            decide: None,
        };
        let ends = self.run_ends.clone();
        let copy = path.clone();
        let spawned = driver::spawn(spec, move |report| {
            // Read back once the run is done with it, then let it go: the
            // session keeps what it says.
            let (added, todos) = Transcript::open(id, n)
                .map(|mut read| {
                    let messages = &mut read.conversation.messages;
                    (
                        messages.split_off(from.min(messages.len())),
                        read.conversation.todos,
                    )
                })
                .unwrap_or_default();
            let _ = std::fs::remove_file(&copy);
            let _ = ends.send_blocking(Finished {
                session: id,
                from,
                added,
                todos,
                text: said(report),
                stopped: report.status == Outcome::Stopped,
                failed: matches!(report.status, Outcome::Failed(_)),
            });
        });
        if let Err(error) = spawned {
            let _ = std::fs::remove_file(path);
            return Err(error);
        }
        Ok(())
    }

    /// Take a held run's end into its session and save it.
    fn finish_run(&mut self, mut finished: Finished, cx: &mut Context<Self>) {
        let Some(run) = apply(&mut self.runs, &mut self.store.conversations, &mut finished) else {
            return;
        };
        if !finished.added.is_empty() {
            self.error = Some(format!(
                "\"{}\" changed while it ran beside the chat, so what the run did was not added.",
                run.label
            ));
        }
        self.save();
        let end = End {
            stopped: finished.stopped,
            failed: finished.failed,
        };
        self.ended(run, finished.session, &finished.text, end, cx);
    }

    /// What follows a held run's end: its answer to the chat that sent it,
    /// what came too late for it, and a place for the next.
    fn ended(&mut self, run: Run, id: u64, text: &str, end: End, cx: &mut Context<Self>) {
        match &run.source {
            Some(source) => {
                // A card still up in the chat is for a call nothing waits on now.
                self.chat_asks.remove(source);
                let answered = !end.stopped && !end.failed;
                self.answer_run(source, id, text, answered, cx);
            }
            None if end.failed => self.error = Some(format!("\"{}\": {text}", run.label)),
            None => {}
        }
        self.typing.remove(&id);
        // Nothing waits on its cards now; dropped, they are declined.
        self.agent_asks
            .retain(|request| request.root != id || request.run != id);
        let open = self
            .store
            .conversations
            .get(self.store.active)
            .map(|c| c.id);
        if open != Some(id) && self.store.index_of(id).is_some() {
            self.unseen.insert(id);
        }
        // Stop means stop, and after a failure the next would most likely
        // fail too: what came meanwhile waits for the next message.
        if !end.stopped && !end.failed {
            match run.source {
                // A chat message that came after the run last looked, which
                // the chat was told the run would read.
                Some(source) => {
                    if let Some(mail) = inbox::fold(&inbox::take(id, true)) {
                        self.run_beside(id, mail, Some(source.clone()), run.remote, cx);
                        self.keep_typing(source, id, cx);
                    }
                }
                None => self.wake_idle(cx),
            }
        }
        self.start_queued(cx);
        cx.notify();
    }

    /// Stop session `id`'s run: a queued one never starts, and one going
    /// ends at its next step, keeping what it did so far.
    pub(crate) fn stop_run(&mut self, id: u64, cx: &mut Context<Self>) {
        let Some(run) = self.runs.get(&id) else {
            return;
        };
        if !run.queued {
            // It stays listed until its end comes, so nothing else starts
            // in its session before what it did is in.
            run.cancel.set();
            cx.notify();
            return;
        }
        if let Some(run) = self.runs.remove(&id) {
            let end = End {
                stopped: true,
                failed: false,
            };
            self.ended(run, id, "Stopped.", end, cx);
        }
    }

    /// A scheduled run started here: listed as running, and stoppable, until
    /// its end brings its session in.
    pub(crate) fn scheduled_started(
        &mut self,
        started: letronna_core::schedule::Started,
        cx: &mut Context<Self>,
    ) {
        let run = Run {
            label: started.name,
            cancel: started.cancel,
            source: None,
            remote: None,
            held: false,
            queued: false,
        };
        self.runs.insert(started.session, run);
        cx.notify();
    }

    /// Whether a run holds the open session, which then takes no send or
    /// edit until the run's end is in.
    pub(crate) fn run_holds_open(&self) -> bool {
        self.store
            .conversations
            .get(self.store.active)
            .is_some_and(|c| self.runs.contains_key(&c.id))
    }

    /// A chat app's message for session `id`: left for the run holding it,
    /// if one does, and the chat told so. False when no run holds it.
    pub(crate) fn chat_to_run(
        &mut self,
        source: &Source,
        id: u64,
        place: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        if !leave_for_run(&self.runs, id, place, text) {
            return false;
        }
        let noted =
            "Still working. This goes to the run in progress, which reads it before it answers.";
        self.tell_remote(source, noted, cx);
        true
    }

    /// A call of a chat app's run that needs a yes: its card goes to the
    /// chat, whose next message answers it. Dropped, which declines it, when
    /// no chat's run is going there.
    pub(crate) fn ask_chat(&mut self, request: approvals::Request, cx: &mut Context<Self>) {
        let Some(source) = self
            .runs
            .get(&request.root)
            .and_then(|run| run.source.clone())
        else {
            return;
        };
        let (call, preview) = (&request.call, &request.preview);
        let text = letronna_core::messaging::card_text(&call.name, &call.arguments, preview);
        // A question is answered in words; a tool is allowed or not.
        let card = (call.name != letronna_core::tools::ASK_USER).then(|| call.id.clone());
        self.tell_remote_card(&source, &text, card, cx);
        // A card replaced is dropped, which declines it.
        self.chat_asks.insert(source, request);
    }

    /// Answer the card chat `source` was sent with what it said, if one is up.
    pub(crate) fn answer_chat(&mut self, source: &Source, said: &str) -> bool {
        use letronna_core::messaging::{CardAnswer, card_answer};
        let Some(request) = self.chat_asks.remove(source) else {
            return false;
        };
        let decision = match card_answer(&request.call.name, &request.call.arguments, said) {
            CardAnswer::Allow => approvals::Decision::Once,
            CardAnswer::Deny(reason) => approvals::Decision::Deny(reason),
            // A run beside the chat asks no questions, so words are a no.
            CardAnswer::Answer(_) => approvals::Decision::Deny(String::new()),
        };
        request.answer(decision);
        true
    }

    /// The runs beside the chat, each with its stop. A held run's row opens
    /// its session; a scheduled one's session comes in when it ends.
    pub(crate) fn runs_section(&self, cx: &mut Context<Self>) -> Option<Div> {
        if self.runs.is_empty() {
            return None;
        }
        let mut rows = v_flex().gap_0p5().child(self.section("Running", cx));
        for (&id, run) in &self.runs {
            let stopping = run.cancel.is_set();
            let state = match (run.queued, stopping) {
                (true, _) => "waiting",
                (_, true) => "stopping",
                _ => "running",
            };
            let row = h_flex()
                .id(("run", id as usize))
                .h(px(26.))
                .px_2()
                .gap_2()
                .items_center()
                .text_sm()
                .child(
                    Icon::new(AppIcon::Play)
                        .size_3()
                        .text_color(cx.theme().primary),
                )
                .child(div().flex_1().min_w_0().truncate().child(run.label.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(state),
                )
                .child(
                    Button::new(("stop-run", id as usize))
                        .ghost()
                        .xsmall()
                        .icon(Icon::new(AppIcon::Stop))
                        .tooltip("Stop")
                        .disabled(stopping)
                        .on_click(cx.listener(move |this, _, _, cx| this.stop_run(id, cx))),
                )
                .when(run.held, |row| {
                    row.cursor_pointer()
                        .on_click(cx.listener(move |this, _, window, cx| {
                            let Some(ix) = this.store.index_of(id) else {
                                return;
                            };
                            this.store.active = ix;
                            this.error = None;
                            this.page = Page::Chat;
                            this.follow();
                            this.focus_composer(window, cx);
                            cx.notify();
                        }))
                });
            rows = rows.child(row);
        }
        Some(rows)
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute here.
    use super::{Finished, Run, apply, background_prompt, leave_for_run};
    use letronna_core::cancel::Cancel;
    use letronna_core::chat::Message;
    use letronna_core::inbox::{self, When};
    use letronna_core::store::Conversation;
    use std::collections::BTreeMap;

    fn said(content: &str) -> Message {
        Message {
            role: "assistant".into(),
            content: content.into(),
            ..Default::default()
        }
    }

    fn held() -> Run {
        Run {
            label: "tests".into(),
            cancel: Cancel::default(),
            source: None,
            remote: None,
            held: true,
            queued: false,
        }
    }

    fn finished(from: usize) -> Finished {
        Finished {
            session: 7,
            from,
            added: vec![said("all green")],
            todos: Vec::new(),
            text: "all green".into(),
            stopped: false,
            failed: false,
        }
    }

    #[test]
    fn bg_takes_the_rest_of_the_line_as_its_prompt() {
        assert_eq!(
            background_prompt("/bg run the tests"),
            Some("run the tests")
        );
        assert_eq!(background_prompt("  /bg   tidy up  "), Some("tidy up"));
        assert_eq!(background_prompt("/bg"), None);
        assert_eq!(background_prompt("/bg   "), None);
        assert_eq!(background_prompt("/bgx run"), None);
        assert_eq!(background_prompt("run /bg later"), None);
    }

    #[test]
    fn a_chat_message_for_a_busy_session_is_left_as_a_later_notice() {
        // Far above any session the other tests use, since the inbox is shared.
        let (busy, idle) = (9_000_000_001, 9_000_000_002);
        let runs = BTreeMap::from([(busy, held())]);
        assert!(leave_for_run(
            &runs,
            busy,
            "a Telegram chat",
            "and the docs"
        ));
        assert!(!leave_for_run(&runs, idle, "a Telegram chat", "hello"));

        // Not taken in the middle of a turn, only once the run looks before it answers.
        assert!(inbox::take(busy, false).is_empty());
        let waiting = inbox::take(busy, true);
        assert_eq!(waiting.len(), 1);
        assert_eq!(waiting[0].when, When::Later);
        assert_eq!(waiting[0].text, "and the docs");
        assert!(inbox::take(idle, true).is_empty());
    }

    #[test]
    fn a_finished_run_is_applied_to_its_session_once() {
        let mut runs = BTreeMap::from([(7, held())]);
        let mut sessions = vec![Conversation {
            id: 7,
            messages: vec![said("prompt")],
            ..Default::default()
        }];
        assert!(apply(&mut runs, &mut sessions, &mut finished(1)).is_some());
        assert_eq!(sessions[0].messages.len(), 2);
        assert!(runs.is_empty());

        // The same end again finds no run listed, and adds nothing.
        assert!(apply(&mut runs, &mut sessions, &mut finished(1)).is_none());
        assert_eq!(sessions[0].messages.len(), 2);
    }

    #[test]
    fn a_run_is_not_applied_over_a_session_that_changed_meanwhile() {
        let mut runs = BTreeMap::from([(7, held())]);
        let mut sessions = vec![Conversation {
            id: 7,
            messages: vec![said("prompt"), said("typed meanwhile")],
            ..Default::default()
        }];
        let mut end = finished(1);
        assert!(apply(&mut runs, &mut sessions, &mut end).is_some());
        assert_eq!(sessions[0].messages.len(), 2);
        assert_eq!(end.added.len(), 1, "what the run did is kept to say so");
    }
}
