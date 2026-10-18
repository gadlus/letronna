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
//! A chat's session that a `letronna serve` carries on is followed the
//! same way.

use crate::prelude::*;
use letronna_core::approvals;
use letronna_core::cancel::Cancel;
use letronna_core::chat::Remote;
use letronna_core::driver::{self, Allow, Outcome, Spec, Step};
use letronna_core::gate::Mode;
use letronna_core::inbox;
use letronna_core::messaging::pairing::{self, Attempt};
use letronna_core::messaging::{Config, Incoming, Source};
use letronna_core::remote::{self, AT_ONCE, Dispatcher, Host};
use std::collections::BTreeMap;

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
    /// What a held run has said and is doing so far, which its session
    /// shows until the run's messages come in at its end.
    pub(crate) live: Live,
    /// Messages typed in its session while it runs, sent there as the next
    /// prompt once it is done.
    pub(crate) follow_ups: Vec<String>,
}

/// A held run as its steps tell it, while its messages are still its own.
#[derive(Default)]
pub(crate) struct Live {
    /// Everything it has said so far, its replies a paragraph apart.
    pub(crate) said: String,
    /// The calls it asked for that have not come back, oldest first, each
    /// with the line that says what it does.
    calls: Vec<(String, String)>,
    /// How many of its calls have come back.
    pub(crate) done: usize,
    /// Whether a call came since it last said anything, so its next words
    /// start a reply of their own.
    after_call: bool,
}

impl Live {
    fn step(&mut self, step: &Step) {
        match step {
            Step::Said(text) => {
                if std::mem::take(&mut self.after_call) && !self.said.is_empty() {
                    self.said.push_str("\n\n");
                }
                self.said.push_str(text);
            }
            Step::Asked(call) => {
                self.after_call = true;
                let summary = letronna_core::tools::summary(&call.name, &call.arguments);
                let line = match letronna_core::tools::one_line(&summary) {
                    shown if shown.is_empty() => call.name.clone(),
                    shown => format!("{} {shown}", call.name),
                };
                self.calls.push((call.id.clone(), line));
            }
            Step::Finished { id, .. } => {
                self.after_call = true;
                self.calls.retain(|(asked, _)| asked != id);
                self.done += 1;
            }
            Step::Thought(_) | Step::Running(_) | Step::Checklist(_) => {}
        }
    }

    /// The line under its words: the call it is on or waits on a yes for, or
    /// that it is thinking before its next words; none while words come in.
    pub(crate) fn doing(&self) -> Option<String> {
        if let Some((_, line)) = self.calls.first() {
            return Some(line.clone());
        }
        (self.said.is_empty() || self.after_call).then(|| "Thinking…".to_owned())
    }
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

/// The window, as a chat app's messages find it.
struct WindowHost<'a, 'b> {
    chat: &'a mut Chat,
    cx: &'a mut Context<'b, Chat>,
}

impl Host for WindowHost<'_, '_> {
    fn dispatcher(&mut self) -> &mut Dispatcher {
        &mut self.chat.remote
    }

    fn config(&self) -> &Config {
        &self.chat.messaging
    }

    fn config_mut(&mut self) -> &mut Config {
        &mut self.chat.messaging
    }

    fn default_mode(&self) -> Mode {
        self.chat.store.settings.mode
    }

    fn tell_card(&mut self, source: &Source, text: &str, card: Option<String>) {
        self.chat.tell_remote_card(source, text, card, self.cx);
    }

    fn answer(&mut self, source: &Source, id: u64, text: &str, spoken: bool) {
        self.chat.answer_run(source, id, text, spoken, self.cx);
    }

    /// Tried against the code the Messaging page shows, which shows what
    /// came of it. A Slack member paired restarts the listener, which hears
    /// only the member it started with.
    fn pair(&mut self, message: &Incoming) -> Option<Attempt> {
        let chat = &mut *self.chat;
        let now = std::time::Instant::now();
        let tried = match message.source {
            Source::Slack { .. } => pairing::pair_slack(
                &mut chat.slack_code,
                &mut chat.messaging.slack,
                message,
                now,
            ),
            _ => pairing::pair(
                &mut chat.pairing,
                &mut chat.messaging.telegram,
                message,
                now,
            ),
        };
        if tried == Some(Attempt::Paired) && matches!(message.source, Source::Slack { .. }) {
            chat.restart_messaging(self.cx);
        }
        if tried.is_some() {
            self.cx.notify();
        }
        tried
    }

    fn trouble(&mut self, error: String) {
        self.chat.error = Some(error);
        self.cx.notify();
    }

    fn spawn(&mut self, work: Box<dyn FnOnce() + Send>) {
        self.cx
            .background_executor()
            .spawn(async move { work() })
            .detach();
    }

    fn after(&mut self, work: Box<dyn FnOnce() -> Incoming + Send>) {
        self.chat.on_remote_after(work, self.cx);
    }

    /// The session's project, or the open session's, which a new one gets.
    fn folder(&self, id: Option<u64>) -> Option<PathBuf> {
        let store = &self.chat.store;
        let ix = id.and_then(|id| store.index_of(id)).unwrap_or(store.active);
        store.conversations[ix].workspace.clone()
    }

    fn exists(&self, id: u64) -> bool {
        self.chat.store.index_of(id).is_some()
    }

    fn running(&self, id: u64) -> bool {
        self.chat.runs.contains_key(&id)
    }

    fn busy(&self, id: u64) -> bool {
        self.chat.streaming_id == Some(id)
    }

    /// On the open session's project, without taking the view.
    fn open(&mut self, title: String) -> Result<u64, String> {
        let store = &mut self.chat.store;
        let workspace = store.conversations[store.active].workspace.clone();
        let id = store.reserve_id();
        store.conversations.push(Conversation {
            id,
            title,
            updated: backend::now(),
            workspace,
            ..Default::default()
        });
        Ok(id)
    }

    fn run(&mut self, id: u64, mode: Mode, prompt: Message, source: Source, remote: Remote) {
        self.chat.modes.insert(id, mode);
        self.chat
            .run_beside(id, prompt, Some(source.clone()), Some(remote), self.cx);
        self.chat.keep_typing(source, id, self.cx);
    }

    fn stop(&mut self, id: u64) {
        self.chat.stop_run(id, self.cx);
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

    /// Take each held run's steps as they come, for its session's view.
    pub(crate) fn watch_run_steps(
        steps: async_channel::Receiver<(u64, Step)>,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            while let Ok((id, step)) = steps.recv().await {
                if this
                    .update(cx, |this, cx| this.run_stepped(id, step, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// One step of session `id`'s held run, kept for its view; its checklist
    /// goes on the session as a turn's does, for the strip over the box.
    fn run_stepped(&mut self, id: u64, step: Step, cx: &mut Context<Self>) {
        // One that comes after the run's end is in its messages already.
        let Some(run) = self.runs.get_mut(&id).filter(|run| run.held) else {
            return;
        };
        run.live.step(&step);
        let words = matches!(step, Step::Said(_));
        match step {
            Step::Checklist(todos) => {
                if let Some(ix) = self.store.index_of(id) {
                    self.store.conversations[ix].todos = todos;
                }
            }
            // Nothing it shows.
            Step::Thought(_) | Step::Running(_) => return,
            _ => {}
        }
        // Words are drawn only where they are said; a call also moves the
        // sidebar's marks.
        let on_screen = self.store.conversations[self.store.active].id == id;
        if on_screen || !words {
            cx.notify();
        }
    }

    /// Send `prompt` in the session on screen as a run beside the chat, for
    /// while the window's own reply runs in another session. `Err` as
    /// [`Self::dispatch`] gives it, with the reason shown.
    pub(crate) fn send_beside(
        &mut self,
        prompt: String,
        images: Vec<PathBuf>,
        cx: &mut Context<Self>,
    ) -> Result<(), ()> {
        let ix = self.store.active;
        // Whether it is new, and so named after this, depends on what was
        // said before, which a session let go of has to be read back for.
        self.fill_now(ix);
        // Checked here, where Settings can open about it, rather than once
        // the run starts.
        if let Err(error) = self.connection_for(ix, None) {
            self.error = Some(error);
            cx.notify();
            return Err(());
        }
        self.show_setup = None;
        self.store.pin(ix);
        let conversation = &mut self.store.conversations[ix];
        name_after(conversation, &prompt);
        let content = letronna_core::references::expand(&prompt, conversation.workspace.as_deref());
        let (id, title) = (conversation.id, conversation.title.clone());
        let model = self.store.model(ix).1.to_owned();
        plugins::emit(cx, plugins::AppEvent::ChatStarted { model, title });
        let message = Message {
            role: "user".into(),
            content,
            images,
            ..Default::default()
        };
        self.error = None;
        self.run_beside(id, message, None, None, cx);
        self.follow();
        Ok(())
    }

    /// A message typed in a session its own run holds: queued, and sent there
    /// as the next prompt once the run is done. A chat app's run or a
    /// scheduled one is the chat's or the job's, and takes none.
    pub(crate) fn queue_after_run(
        &mut self,
        prompt: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let here = self.store.conversations[self.store.active].id;
        let own = self
            .runs
            .get(&here)
            .is_some_and(|run| run.held && run.source.is_none());
        let refused = if !own {
            Some(
                "This session is running beside the chat. Wait for its answer, or stop it with \
                 Escape.",
            )
        } else if prompt.is_empty() {
            Some("Pictures go with a message, once this session's run is done.")
        } else if prompt.chars().count() > 12_000 {
            Some("Please keep each message under 12,000 characters.")
        } else if prompt == letronna_core::init::COMMAND {
            Some("/init starts a turn of its own. Send it when this one is done.")
        } else {
            None
        };
        if let Some(why) = refused {
            // Kept as typed, without the line Enter added.
            self.composer
                .update(cx, |input, cx| input.set_value(prompt, window, cx));
            self.error = Some(why.into());
            cx.notify();
            return;
        }
        if let Some(run) = self.runs.get_mut(&here) {
            run.follow_ups.push(prompt);
        }
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.follow();
        cx.notify();
    }

    /// Send what was typed while session `id`'s run went as its next prompt.
    fn continue_run(&mut self, id: u64, text: String, cx: &mut Context<Self>) {
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        let workspace = self.store.conversations[ix].workspace.as_deref();
        let message = Message {
            role: "user".into(),
            content: letronna_core::references::expand(&text, workspace),
            ..Default::default()
        };
        self.run_beside(id, message, None, None, cx);
    }

    /// Whether `request` comes from its session's own run beside the chat,
    /// rather than a helper: the helpers it starts join its run, but under
    /// names of their own.
    pub(crate) fn own_run(&self, request: &approvals::Request) -> bool {
        request.run == request.root
            && self
                .runs
                .get(&request.root)
                .is_some_and(|run| run.held && run.label == request.label)
    }

    /// Who a card says is asking: Letronna for a session's own run, which
    /// the user started there, or the helper by name.
    pub(crate) fn asker(&self, request: &approvals::Request) -> String {
        if self.own_run(request) {
            "Letronna".to_owned()
        } else {
            format!("Helper {}", request.label)
        }
    }

    /// Put `text` back in session `id`'s box, ahead of what is typed there.
    fn back_to_box(&mut self, id: u64, text: String) {
        if self.draft_for == id {
            // The box holds that session's draft, and takes it at its next draw.
            self.returned_follow_ups = Some(match self.returned_follow_ups.take() {
                Some(earlier) => format!("{earlier}\n\n{text}"),
                None => text,
            });
            return;
        }
        let draft = self.drafts.entry(id).or_default();
        *draft = if draft.trim().is_empty() {
            text
        } else {
            format!("{text}\n\n{draft}")
        };
    }

    /// What the held run in `session` has said and is doing, under the
    /// session's messages until the run's own come in at its end, and what
    /// was typed meanwhile under that.
    pub(crate) fn run_live(
        &self,
        session: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let run = self.runs.get(&session).filter(|run| run.held)?;
        let ix = self.store.index_of(session)?;
        let live = &run.live;
        let muted = cx.theme().muted_foreground;
        let line = if run.queued {
            Some(format!(
                "Waiting for a place: {AT_ONCE} sessions are running beside the chat."
            ))
        } else if run.cancel.is_set() {
            Some("Stopping…".to_owned())
        } else {
            live.doing()
        };
        let label = h_flex()
            .items_center()
            .gap_2()
            .text_xs()
            .font_family(HEADING_FONT)
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(cx.theme().link)
                    .child(self.store.model(ix).1.to_owned()),
            )
            .child(
                div()
                    .size(px(6.))
                    .rounded_full()
                    .bg(cx.theme().primary)
                    .opacity(crate::ui::busy::breathing(window, cx)),
            )
            .when(live.done > 0, |label| {
                let s = if live.done == 1 { "" } else { "s" };
                label.child(
                    div()
                        .text_color(muted)
                        .child(format!("{} call{s} done", live.done)),
                )
            });
        let said = (!live.said.is_empty()).then(|| {
            let id =
                SharedString::from(format!("run-said-{session}-{}", crate::theme::generation()));
            div().w_full().overflow_hidden().child(
                TextView::markdown(id, live.said.clone())
                    .selectable(!window.has_active_dialog(cx))
                    .line_height(px(24.)),
            )
        });
        let doing = line.map(|line| {
            h_flex()
                .gap_2()
                .items_center()
                .text_color(muted)
                .child(
                    crate::ui::busy::Spinner::new()
                        .xsmall()
                        .color(cx.theme().primary),
                )
                .child(div().min_w_0().truncate().child(line))
        });
        let queued = run.follow_ups.iter().map(|text| {
            v_flex()
                .w_full()
                .gap_1()
                .child(
                    div()
                        .text_xs()
                        .font_family(BODY_FONT)
                        .text_color(muted)
                        .child("Queued · goes in once this run is done"),
                )
                .child(
                    div()
                        .w_full()
                        .px_4()
                        .py_3()
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_color(cx.theme().border)
                        .text_color(muted)
                        .child(text.clone()),
                )
        });
        Some(
            v_flex()
                .w_full()
                .gap_3()
                .child(
                    v_flex()
                        .w_full()
                        .gap_1p5()
                        .child(label)
                        .children(said)
                        .children(doing),
                )
                .children(queued)
                .into_any_element(),
        )
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
            live: Live::default(),
            follow_ups: Vec::new(),
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
            // Told to its session's view as it goes, which has nothing else
            // of it until the run's messages come in at its end.
            watch: Some(std::sync::Arc::new({
                let steps = self.run_steps.clone();
                move |step| {
                    let _ = steps.try_send((id, step));
                }
            })),
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
                text: remote::said(report),
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
    fn ended(&mut self, mut run: Run, id: u64, text: &str, end: End, cx: &mut Context<Self>) {
        match &run.source {
            Some(source) => {
                let answered = !end.stopped && !end.failed;
                remote::ended(
                    &mut WindowHost { chat: self, cx },
                    source,
                    id,
                    text,
                    answered,
                );
            }
            None if end.failed => self.error = Some(format!("\"{}\": {text}", run.label)),
            None => {}
        }
        self.typing.remove(&id);
        // Nothing waits on the run's own cards now; dropped, they are
        // declined. Its helpers' stay, since one in the background outlives
        // the run that started it.
        self.agent_asks.retain(|request| {
            !the_run_asks(request.root, request.run, &request.label, id, &run.label)
        });
        let open = self
            .store
            .conversations
            .get(self.store.active)
            .map(|c| c.id);
        if open != Some(id) && self.store.index_of(id).is_some() {
            self.unseen.insert(id);
        }
        // What was typed in its session meanwhile goes on as the next prompt
        // there; after a stop or a failure it goes back in the box instead.
        let follow_ups = std::mem::take(&mut run.follow_ups);
        if !follow_ups.is_empty() {
            let text = follow_ups.join("\n\n");
            if end.stopped || end.failed {
                self.back_to_box(id, text);
            } else {
                self.continue_run(id, text, cx);
            }
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
            live: Live::default(),
            follow_ups: Vec::new(),
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

    /// A message from a chat app, dealt with as core deals with one.
    pub(crate) fn on_remote(&mut self, message: Incoming, cx: &mut Context<Self>) {
        remote::dispatch(&mut WindowHost { chat: self, cx }, message);
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
        remote::ask(&mut WindowHost { chat: self, cx }, source, request);
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
                        .plain(cx)
                        .xsmall()
                        .icon(Icon::new(AppIcon::Stop))
                        .tooltip("Stop")
                        .disabled(stopping)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            // Stopping it is not opening it: the row's own
                            // click must not follow.
                            cx.stop_propagation();
                            this.stop_run(id, cx)
                        })),
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

/// Whether a card for session `root` in run `run`, asked as `label`, is
/// held run `id`'s own, which is labelled `run_label`: the helpers it starts
/// join its run, but under names of their own.
fn the_run_asks(root: u64, run: u64, label: &str, id: u64, run_label: &str) -> bool {
    root == id && run == id && label == run_label
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute here.
    use super::{Finished, Live, Run, apply, background_prompt, the_run_asks};
    use letronna_core::cancel::Cancel;
    use letronna_core::chat::{Message, ToolCall};
    use letronna_core::driver::Step;
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
            live: Default::default(),
            follow_ups: Vec::new(),
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
    fn a_held_run_s_end_drops_its_own_cards_and_keeps_its_helpers() {
        // Its own, asked under its label in its own run.
        assert!(the_run_asks(7, 7, "Fix the build", 7, "Fix the build"));
        // A helper it started joins its run under a name of its own.
        assert!(!the_run_asks(7, 7, "explore", 7, "Fix the build"));
        // Another session's, or another run's.
        assert!(!the_run_asks(8, 8, "Fix the build", 7, "Fix the build"));
        assert!(!the_run_asks(7, 12, "Fix the build", 7, "Fix the build"));
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
    fn a_held_runs_steps_say_what_it_said_and_what_it_is_doing() {
        let mut live = Live::default();
        assert_eq!(live.doing().as_deref(), Some("Thinking…"));

        live.step(&Step::Said("Looking".into()));
        live.step(&Step::Thought("hmm".into()));
        live.step(&Step::Said(" at it.".into()));
        assert_eq!(live.said, "Looking at it.");
        assert_eq!(live.doing(), None, "its words say it is working");

        let call = ToolCall {
            id: "c1".into(),
            name: "run_command".into(),
            arguments: r#"{"command": "cargo\n  test"}"#.into(),
        };
        live.step(&Step::Asked(call));
        live.step(&Step::Running("c1".into()));
        assert_eq!(live.doing().as_deref(), Some("run_command cargo test"));

        live.step(&Step::Finished {
            id: "c1".into(),
            output: "ok".into(),
            failed: false,
        });
        assert_eq!(live.done, 1);
        assert_eq!(live.doing().as_deref(), Some("Thinking…"));

        // Its next reply reads as one of its own.
        live.step(&Step::Said("All green.".into()));
        assert_eq!(live.said, "Looking at it.\n\nAll green.");
        assert_eq!(live.doing(), None);
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
