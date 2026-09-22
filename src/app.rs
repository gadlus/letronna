use crate::core::agent;
use crate::prelude::*;

pub(crate) const BRAND: &str = "Letronna";

pub(crate) const GREETINGS: &[&str] = &[
    "Letronna is ready.",
    "Letronna is listening.",
    "What are we building today?",
    "Tell Letronna the goal.",
    "Ready when you are.",
];

actions!(letronna, [NewSession]);

pub(crate) enum Verdict {
    Allow {
        remember: bool,
    },
    Deny,
    /// The reply to an `ask_user` question.
    Answer(String),
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Page {
    Chat,
    Skills,
    Messaging,
    Artifacts,
    Jobs,
}
impl Page {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Page::Chat => "Chat",
            Page::Skills => "Skills",
            Page::Messaging => "Messaging",
            Page::Artifacts => "Artifacts",
            Page::Jobs => "Scheduled jobs",
        }
    }
    pub(crate) fn blurb(self) -> &'static str {
        match self {
            Page::Chat => "",
            Page::Skills => {
                "Reusable playbooks Letronna can load into a session. Nothing installed yet."
            }
            Page::Messaging => {
                "Connect Telegram, Discord, Slack or email gateways. No channel configured yet."
            }
            Page::Artifacts => "Files, images and links produced in sessions will collect here.",
            // Jobs renders its own page; this blurb is only a fallback.
            Page::Jobs => "Prompts that run on a timer.",
        }
    }
}

pub(crate) struct Chat {
    pub(crate) store: Store,
    pub(crate) composer: Entity<InputState>,
    pub(crate) search: Entity<InputState>,
    pub(crate) endpoint: Entity<InputState>,
    pub(crate) model: Entity<InputState>,
    pub(crate) key: Entity<InputState>,
    pub(crate) models: Vec<String>,
    pub(crate) model_open: bool,
    pub(crate) model_search: Entity<InputState>,
    pub(crate) keys: std::collections::HashMap<String, String>,
    pub(crate) gateway_pick: Entity<String>,
    pub(crate) presence_pick: Entity<String>,
    pub(crate) settings_page: Entity<&'static str>,
    pub(crate) settings_open: bool,
    pub(crate) settings_backdrop: Option<std::sync::Arc<RenderImage>>,
    pub(crate) settings_closing: bool,
    pub(crate) rename: Entity<InputState>,
    pub(crate) page: Page,
    pub(crate) sidebar_open: bool,
    pub(crate) greeting: &'static str,
    pub(crate) free_remaining: Option<u64>,
    pub(crate) loading_models: bool,
    pub(crate) gateway_error: Option<String>,
    pub(crate) busy: bool,
    /// The turn in flight, if any. Owns the streamed reply and the tool queue.
    pub(crate) turn: Option<agent::Turn>,
    /// The session id currently receiving a streamed reply, if any.
    pub(crate) streaming_id: Option<u64>,
    /// Tools the user allowed for the rest of this run of the app.
    pub(crate) session_allow: std::collections::HashSet<String>,
    /// Sessions started by an unattended schedule, which run tools without
    /// asking. Keyed by session so the grant cannot reach the user's own
    /// chats: a job saying "do not ask me at 9am" is not permission to stop
    /// asking everywhere else.
    pub(crate) unattended: std::collections::HashSet<u64>,
    /// Text field on the `ask_user` card.
    pub(crate) answer: Entity<InputState>,
    /// Whether the footer is already polling background jobs.
    pub(crate) job_tick: bool,
    /// Plan mode: the model may read and ask, but not change anything.
    pub(crate) plan_mode: bool,
    pub(crate) schedules: Vec<crate::core::schedule::Schedule>,
    /// Id of the schedule being edited on the Scheduled jobs page.
    pub(crate) editing: Option<u64>,
    pub(crate) job_name: Entity<InputState>,
    pub(crate) job_prompt: Entity<InputState>,
    pub(crate) messaging: crate::core::messaging::Config,
    /// A chat that has messaged the bot but has not been allowed yet.
    pub(crate) pairing: Option<(i64, String)>,
    /// Sessions a schedule opened, so their result can be reported onward.
    pub(crate) scheduled: std::collections::HashSet<u64>,
    /// Sessions opened for a remote chat, so the reply goes back to it.
    pub(crate) remote: std::collections::HashMap<u64, i64>,
    pub(crate) messaging_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) token_input: Entity<InputState>,
    pub(crate) webhook_input: Entity<InputState>,
    /// The webhook's name, once it has been checked.
    pub(crate) hook_name: Option<String>,
    /// The connected bot's @name, once the token has been checked.
    pub(crate) bot_name: Option<String>,
    /// Selected row on the Messaging page.
    pub(crate) channel: usize,
    pub(crate) skills: Vec<crate::core::skills::Skill>,
    pub(crate) editing_skill: Option<u64>,
    pub(crate) skill_name: Entity<InputState>,
    pub(crate) skill_prompt: Entity<InputState>,
    pub(crate) thinking_open: std::collections::HashSet<usize>,
    pub(crate) error: Option<String>,
    pub(crate) scroll: ScrollHandle,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl Chat {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Sessions are separate files now, so a damaged one costs that
        // session rather than the history.
        let (mut store, error) = (crate::core::store::load(), None);
        if store.conversations.is_empty() {
            store.conversations.push(Conversation {
                title: "New session".into(),
                updated: backend::now(),
                ..Default::default()
            });
        }
        store.assign_ids();
        store.active = store.active.min(store.conversations.len() - 1);
        if store.settings.gateway.is_empty() {
            store.settings.gateway = GATEWAYS[0].name.into();
        }
        if store.settings.endpoint.is_empty() {
            store.settings.endpoint =
                std::env::var("AGENT_BASE_URL").unwrap_or_else(|_| Upstream.into());
        }
        let keys = load_keys();
        let saved_key = keys
            .get(key_slot(&store.settings.gateway))
            .cloned()
            .or_else(|| std::env::var("AGENT_API_KEY").ok())
            .unwrap_or_default();
        if store.settings.model.is_empty() {
            store.settings.model =
                std::env::var("AGENT_MODEL").unwrap_or_else(|_| "pool-free/gpt-oss-20b".into());
        }
        let greeting = GREETINGS[(backend::now() % GREETINGS.len() as u64) as usize];
        let composer = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(1, 6)
                .placeholder(greeting)
        });
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search sessions…"));
        let endpoint =
            cx.new(|cx| InputState::new(window, cx).default_value(store.settings.endpoint.clone()));
        let model = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Model ID")
                .default_value(store.settings.model.clone())
        });
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("API key")
                .masked(true)
                .default_value(saved_key)
        });
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models…"));
        let gateway_pick = cx.new(|_| store.settings.gateway.clone());
        let presence_pick = cx.new(|_| store.settings.presence.clone());
        let settings_page = cx.new(|_| "Gateways");
        let rename = cx.new(|cx| InputState::new(window, cx).placeholder("Session name"));
        let answer = cx.new(|cx| InputState::new(window, cx).placeholder("Your answer…"));
        let job_name = cx.new(|cx| InputState::new(window, cx).placeholder("Morning check"));
        let token_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Bot token from @BotFather")
                .default_value(crate::core::messaging::load().telegram.token)
        });
        let webhook_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://discord.com/api/webhooks/...")
                .default_value(crate::core::messaging::load().discord.webhook)
        });
        let skill_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Review these changes"));
        let skill_prompt = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 10)
                .placeholder("What should Letronna do?")
        });
        let job_prompt = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 6)
                .placeholder("Run the tests and tell me what broke.")
        });
        let subscriptions = vec![
            cx.subscribe_in(&model_search, window, |_, _, _: &InputEvent, _, cx| {
                cx.notify()
            }),
            cx.subscribe_in(&composer, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { secondary: false }) {
                    this.send(window, cx);
                }
            }),
            cx.subscribe_in(&search, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&answer, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_answer(window, cx);
                }
            }),
        ];
        let mut this = Self {
            page: Page::Chat,
            sidebar_open: true,
            greeting,
            free_remaining: None,
            loading_models: false,
            gateway_error: None,
            store,
            composer,
            search,
            endpoint,
            model,
            key,
            models: Vec::new(),
            model_open: false,
            model_search,
            keys,
            gateway_pick,
            presence_pick,
            settings_page,
            settings_open: false,
            settings_backdrop: None,
            settings_closing: false,
            rename,
            busy: false,
            turn: None,
            streaming_id: None,
            session_allow: std::collections::HashSet::new(),
            unattended: std::collections::HashSet::new(),
            answer,
            job_tick: false,
            plan_mode: false,
            schedules: crate::core::schedule::load(),
            editing: None,
            job_name,
            job_prompt,
            messaging: crate::core::messaging::load(),
            pairing: None,
            scheduled: std::collections::HashSet::new(),
            remote: std::collections::HashMap::new(),
            messaging_stop: None,
            token_input,
            webhook_input,
            hook_name: None,
            bot_name: None,
            channel: 0,
            skills: crate::core::skills::load(),
            editing_skill: None,
            skill_name,
            skill_prompt,
            thinking_open: std::collections::HashSet::new(),
            error,
            scroll: ScrollHandle::new(),
            _subscriptions: subscriptions,
        };
        this.load_models(window, cx);
        Self::watch_schedules(cx);
        this.restart_messaging(cx);
        if this.store.settings.presence == "off" {
            plugins::set_enabled(cx, plugins::DISCORD, false);
            this.store.settings.presence = "detailed".into();
            this.save();
        }
        plugins::emit(
            cx,
            plugins::AppEvent::PresenceMode(plugins::PresenceMode::from_str(
                &this.store.settings.presence,
            )),
        );
        this
    }

    pub(crate) fn load_models(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading_models {
            return;
        }
        self.loading_models = true;
        let base = self.endpoint.read(cx).value().to_string();
        let key = self.key.read(cx).value().to_string();
        let guest = self.store.settings.gateway == GATEWAYS[0].name;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let result = if guest {
                backend::free_models()
            } else {
                backend::list_models(&base, &key)
            };
            let _ = tx.send_blocking(result);
        });
        let _ = window;
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.loading_models = false;
                    match result {
                        Ok(models) => {
                            this.models = models;
                            this.gateway_error = None;
                        }
                        Err(error) => {
                            this.gateway_error = Some(error.clone());
                            this.error = Some(error);
                        }
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    pub(crate) fn choose_model(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.model
            .update(cx, |input, cx| input.set_value(id.clone(), window, cx));
        if id.starts_with("pool-free/") {
            self.endpoint
                .update(cx, |input, cx| input.set_value(Upstream, window, cx));
        }
        self.store.settings.model = id;
        self.model_open = false;
        self.save();
        cx.notify();
    }

    pub(crate) fn save(&mut self) {
        if self.store.save().is_err() {
            self.error =
                Some("Could not save chat history. Your current chat remains in memory.".into());
        }
    }

    pub(crate) fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A new session usually continues on the same project.
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .clone();
        self.store.conversations.push(Conversation {
            id: self.store.next_id(),
            title: "New session".into(),
            updated: backend::now(),
            workspace,
            ..Default::default()
        });
        self.store.active = self.store.conversations.len() - 1;
        self.error = None;
        self.page = Page::Chat;
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.save();
        cx.notify();
    }

    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let prompt = self.composer.read(cx).value().trim().to_owned();
        if prompt.is_empty() {
            self.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
            return;
        }
        if prompt.chars().count() > 12_000 {
            self.error = Some("Please keep each message under 12,000 characters.".into());
            cx.notify();
            return;
        }
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        if self.dispatch(prompt, cx).is_err() {
            self.open_settings(window, cx);
        }
    }

    /// Send a prompt in the active session. Split out of `send` so a schedule
    /// can fire one with no window in hand. `Err` means the gateway is not
    /// usable; the caller decides whether to open Settings about it.
    pub(crate) fn dispatch(
        &mut self,
        prompt: String,
        cx: &mut Context<Self>,
    ) -> std::result::Result<(), ()> {
        let connection = Connection {
            endpoint: self.endpoint.read(cx).value().to_string(),
            model: self.model.read(cx).value().to_string(),
            key: self.key.read(cx).value().to_string(),
            guest: self.store.settings.gateway == GATEWAYS[0].name,
            tools: self.store.settings.gateway != GATEWAYS[0].name,
            workspace: None,
            plan: self.plan_mode,
        };
        if let Err(error) = backend::validate(&connection) {
            self.error = Some(error);
            cx.notify();
            return Err(());
        }
        self.store.settings.endpoint = connection.endpoint.clone();
        self.store.settings.model = connection.model.clone();
        let conversation = &mut self.store.conversations[self.store.active];
        // Name an untitled session after its first prompt, but leave a name
        // that was set deliberately, such as a scheduled job's.
        if conversation.messages.is_empty()
            && (conversation.title.is_empty() || conversation.title == "New session")
        {
            conversation.title = prompt.chars().take(40).collect();
        }
        conversation.updated = backend::now();
        conversation.messages.push(Message {
            role: "user".into(),
            content: prompt,
            ..Default::default()
        });
        let target_id = conversation.id;
        self.busy = true;
        self.streaming_id = Some(target_id);
        self.turn = Some(agent::Turn::new(connection));
        self.page = Page::Chat;
        let convo_title = self.store.conversations[self.store.active].title.clone();
        plugins::emit(
            cx,
            plugins::AppEvent::ChatStarted {
                model: self.store.settings.model.clone(),
                title: convo_title,
            },
        );
        self.error = None;
        self.save();
        self.scroll.scroll_to_bottom();
        self.run_turn(cx);
        Ok(())
    }

    /// Check every half minute for a schedule that is due and run it. Only one
    /// fires per check, so a backlog drains one at a time instead of opening a
    /// dozen sessions at once.
    pub(crate) fn watch_schedules(cx: &mut Context<Self>) {
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(30))
                    .await;
                if this.update(cx, |this, cx| this.run_due(cx)).is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    fn run_due(&mut self, cx: &mut Context<Self>) {
        // A run in progress owns the agent loop; wait rather than interleave.
        if self.busy {
            return;
        }
        let now = chrono::Local::now();
        let Some(index) = self
            .schedules
            .iter()
            .position(|s| crate::core::schedule::is_due(s, now))
        else {
            return;
        };
        self.schedules[index].last_run = backend::now();
        crate::core::schedule::save(&self.schedules);
        let schedule = self.schedules[index].clone();
        self.start_scheduled(&schedule, cx);
    }

    /// Open a fresh session for a schedule and send its prompt.
    pub(crate) fn start_scheduled(
        &mut self,
        schedule: &crate::core::schedule::Schedule,
        cx: &mut Context<Self>,
    ) {
        self.store.conversations.push(Conversation {
            id: self.store.next_id(),
            title: schedule.name.clone(),
            updated: backend::now(),
            workspace: schedule.workspace.clone(),
            ..Default::default()
        });
        self.store.active = self.store.conversations.len() - 1;
        self.page = Page::Chat;
        let session = self.store.conversations[self.store.active].id;
        self.scheduled.insert(session);
        // Nobody is necessarily watching, so an unattended schedule skips the
        // approval cards that would otherwise stall it -- but only in the
        // session it just opened.
        if schedule.unattended {
            self.unattended.insert(session);
        }
        let _ = self.dispatch(schedule.prompt.clone(), cx);
    }

    /// One model call, streamed. The decisions all live in `core::agent`;
    /// this only supplies gpui's way of waiting and drawing.
    fn run_turn(&mut self, cx: &mut Context<Self>) {
        let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) else {
            self.finish(cx);
            return;
        };
        let Some(turn) = self.turn.as_mut() else {
            self.finish(cx);
            return;
        };
        turn.connection.workspace = self.store.conversations[ix].workspace.clone();
        let receiver = backend::start(
            turn.connection.clone(),
            self.store.conversations[ix].messages.clone(),
        );
        turn.live = agent::Live::default();
        self.follow();
        cx.notify();
        cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let finished = matches!(event, Event::Finished(_));
                if this
                    .update(cx, |this, cx| this.on_event(event, cx))
                    .is_err()
                    || finished
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        turn.absorb(&event);
        if let Some(n) = turn.live.free_remaining {
            self.free_remaining = Some(n);
        }
        match event {
            Event::Finished(Err(error)) => {
                self.error = Some(error);
                self.finish(cx);
            }
            Event::Finished(Ok(())) => self.on_reply(cx),
            _ => {}
        }
        self.follow();
        cx.notify();
    }

    /// A reply finished: store it, then either run what it asked for or stop.
    fn on_reply(&mut self, cx: &mut Context<Self>) {
        let model = self.store.settings.model.clone();
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        let reply = turn.take_reply(&model);
        let calls = reply.tool_calls.clone();
        // The session may have been switched or deleted while it streamed.
        let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) else {
            self.finish(cx);
            return;
        };
        let conversation = &mut self.store.conversations[ix];
        conversation.updated = backend::now();
        conversation.messages.push(reply);
        self.save();
        let more = self.turn.as_mut().is_some_and(|turn| turn.begin(calls));
        if more {
            self.advance_tools(cx);
        } else {
            self.finish(cx);
        }
    }

    /// Run the next queued call, or stop on one that needs the user.
    pub(crate) fn advance_tools(&mut self, cx: &mut Context<Self>) {
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        let gate = |name: &str| self.needs_user(name);
        match turn.next(gate) {
            agent::Next::Done => self.finish(cx),
            agent::Next::AskUser(_) => cx.notify(),
            agent::Next::Run(call) if call.name == crate::core::tools::TODO => {
                self.apply_todo(&call.arguments.clone(), cx)
            }
            agent::Next::Run(call) => self.execute_call(call, cx),
            agent::Next::Ask => {
                let results = self
                    .turn
                    .as_mut()
                    .map(|turn| turn.take_results())
                    .unwrap_or_default();
                if let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) {
                    self.store.conversations[ix].messages.extend(results);
                    self.save();
                    self.run_turn(cx);
                } else {
                    self.finish(cx);
                }
            }
        }
    }

    /// Run one call off the UI thread and continue the queue when it returns.
    fn execute_call(&mut self, call: backend::ToolCall, cx: &mut Context<Self>) {
        let ctx = crate::core::tools::ToolContext {
            workspace: self
                .turn
                .as_ref()
                .and_then(|turn| turn.connection.workspace.clone()),
        };
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(crate::core::tools::run(&call.name, &call.arguments, &ctx));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = this.update(cx, |this, cx| this.record_result(result, cx));
            }
        })
        .detach();
    }

    /// Record the outcome of the current call and move on.
    pub(crate) fn record_result(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        if let Some(turn) = self.turn.as_mut() {
            turn.finish_call(result);
        }
        self.follow();
        self.advance_tools(cx);
    }

    /// Stop any running poller and start one if messaging is set up. Called
    /// whenever the settings change, so the worker always matches them.
    pub(crate) fn restart_messaging(&mut self, cx: &mut Context<Self>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        if let Some(stop) = self.messaging_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        if !self.messaging.telegram.ready() {
            return;
        }
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (tx, rx) = async_channel::unbounded();
        crate::core::messaging::poll(self.messaging.telegram.token.clone(), stop.clone(), tx);
        self.messaging_stop = Some(stop);
        cx.spawn(async move |this, cx| {
            while let Ok(message) = rx.recv().await {
                if this
                    .update(cx, |this, cx| this.on_remote(message, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    fn tell_remote(&self, chat: i64, text: &str, cx: &mut Context<Self>) {
        let token = self.messaging.telegram.token.clone();
        let text = text.to_owned();
        cx.background_executor()
            .spawn(async move {
                let _ = crate::core::messaging::send(&token, chat, &text);
            })
            .detach();
    }

    /// A message arrived from the bot. Anything from a chat the user has not
    /// approved is held for approval rather than acted on.
    fn on_remote(&mut self, message: crate::core::messaging::Incoming, cx: &mut Context<Self>) {
        if self.messaging.telegram.chat != Some(message.chat) {
            self.pairing = Some((message.chat, message.from));
            cx.notify();
            return;
        }
        if self.busy {
            self.tell_remote(
                message.chat,
                "Busy with something else right now. Send that again in a moment.",
                cx,
            );
            return;
        }
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .clone();
        self.store.conversations.push(Conversation {
            id: self.store.next_id(),
            title: message.text.chars().take(40).collect(),
            updated: backend::now(),
            workspace,
            ..Default::default()
        });
        self.store.active = self.store.conversations.len() - 1;
        let id = self.store.conversations[self.store.active].id;
        self.remote.insert(id, message.chat);
        // Nobody on the far end can answer an approval card, so unless the
        // user has said otherwise a remote message may look but not touch.
        let was_plan = self.plan_mode;
        self.plan_mode = !self.messaging.telegram.allow_tools;
        let _ = self.dispatch(message.text, cx);
        self.plan_mode = was_plan;
    }

    /// A scheduled run has no one watching, so post what it concluded to
    /// Discord if a webhook is set up. One-way is enough here: the point is
    /// to find out what happened while away.
    fn report_scheduled(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.streaming_id else { return };
        if !self.scheduled.remove(&id) || !self.messaging.discord.ready() {
            return;
        }
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        let conversation = &self.store.conversations[ix];
        let Some(reply) = conversation
            .messages
            .iter()
            .rev()
            .find(|m| m.role == "assistant" && !m.content.is_empty())
        else {
            return;
        };
        let text = format!(
            "**{}**
{}",
            conversation.title, reply.content
        );
        let webhook = self.messaging.discord.webhook.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = crate::core::messaging::discord_send(&webhook, &text);
            })
            .detach();
    }

    /// Send the finished reply back to the chat that asked for it.
    fn answer_remote(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.streaming_id else { return };
        let Some(chat) = self.remote.remove(&id) else {
            return;
        };
        let reply = self
            .store
            .index_of(id)
            .and_then(|ix| {
                self.store.conversations[ix]
                    .messages
                    .iter()
                    .rev()
                    .find(|m| m.role == "assistant" && !m.content.is_empty())
            })
            .map(|m| m.content.clone())
            .unwrap_or_else(|| "That did not produce an answer.".to_owned());
        self.tell_remote(chat, &reply, cx);
    }

    /// Whether this call has to wait for the user. `ask_user` always does:
    /// it is a question, not an action to permit.
    pub(crate) fn needs_user(&self, name: &str) -> bool {
        let unattended = self
            .streaming_id
            .is_some_and(|id| self.unattended.contains(&id));
        agent::needs_user(
            name,
            self.store.settings.skip_approvals,
            &self.session_allow,
            unattended,
        )
    }

    /// Store the model's checklist on the session so the composer can show it.
    fn apply_todo(&mut self, arguments: &str, cx: &mut Context<Self>) {
        let result = crate::core::tools::parse_args(arguments).and_then(|args| {
            let items = args["items"]
                .as_array()
                .ok_or_else(|| "`items` is required.".to_owned())?;
            let todos: Vec<Todo> = items
                .iter()
                .map(|item| Todo {
                    text: item["text"].as_str().unwrap_or_default().to_owned(),
                    status: item["status"].as_str().unwrap_or("pending").to_owned(),
                })
                .filter(|todo| !todo.text.is_empty())
                .collect();
            let done = todos.iter().filter(|t| t.status == "done").count();
            Ok((todos, done))
        });
        let message = match result {
            Ok((todos, done)) => {
                let total = todos.len();
                if let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) {
                    self.store.conversations[ix].todos = todos;
                    self.save();
                }
                Ok(format!("Checklist updated: {done} of {total} done."))
            }
            Err(error) => Err(error),
        };
        self.record_result(message, cx);
    }

    /// The user answered the approval card.
    pub(crate) fn resolve_call(&mut self, verdict: Verdict, cx: &mut Context<Self>) {
        let Some(call) = self
            .turn
            .as_ref()
            .and_then(|turn| match turn.next(|_| true) {
                agent::Next::AskUser(call) => Some(call),
                _ => None,
            })
        else {
            return;
        };
        match verdict {
            Verdict::Answer(text) => self.record_result(Ok(text), cx),
            Verdict::Deny => self.record_result(Err(crate::core::tools::DECLINED.into()), cx),
            Verdict::Allow { remember } => {
                if remember {
                    self.session_allow.insert(call.name.clone());
                }
                self.execute_call(call, cx);
            }
        }
    }

    /// Follow the stream only while the user is already at the bottom, so
    /// scrolling up to reread does not snap back on every token.
    pub(crate) fn follow(&self) {
        let slack = px(48.);
        let gap = self.scroll.max_offset().height + self.scroll.offset().y;
        if gap <= slack {
            self.scroll.scroll_to_bottom();
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.answer_remote(cx);
        self.report_scheduled(cx);
        self.busy = false;
        self.streaming_id = None;
        self.turn = None;
        plugins::emit(cx, plugins::AppEvent::ChatIdle);
        cx.notify();
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A job starting or exiting happens off the UI thread, so the footer
        // chips need a poll while any of them is alive. It stops on its own.
        if !self.job_tick && crate::core::jobs::list().iter().any(|job| job.running) {
            self.job_tick = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    let alive = crate::core::jobs::list().iter().any(|job| job.running);
                    let sent = this.update(cx, |this, cx| {
                        this.job_tick = alive;
                        cx.notify();
                    });
                    if sent.is_err() || !alive {
                        break;
                    }
                }
            })
            .detach();
        }
        if let Some(error) = self.error.take() {
            let text = error.clone();
            window.push_notification(
                gpui_component::notification::Notification::error(error)
                    .autohide(false)
                    .action(move |_, window, cx| {
                        let text = text.clone();
                        let copied = window.use_keyed_state("copied-error", cx, |_, _| false);
                        let done = *copied.read(cx);
                        Button::new("copy-error")
                            .ghost()
                            .xsmall()
                            .icon(Icon::new(if done {
                                IconName::Check
                            } else {
                                IconName::Copy
                            }))
                            .tooltip("Copy message")
                            .on_click(move |_, _, cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                copied.update(cx, |c, cx| {
                                    *c = true;
                                    cx.notify();
                                });
                                let copied = copied.clone();
                                cx.spawn(async move |cx| {
                                    cx.background_executor()
                                        .timer(std::time::Duration::from_secs(2))
                                        .await;
                                    let _ = copied.update(cx, |c, cx| {
                                        *c = false;
                                        cx.notify();
                                    });
                                })
                                .detach();
                            })
                    }),
                cx,
            );
        }
        let active = self.store.active;
        let conversation = &self.store.conversations[active];
        // Only the session the reply was sent from shows the live row.
        let streaming_here = self.busy && self.streaming_id == Some(conversation.id);

        let body: AnyElement = if self.page == Page::Jobs {
            self.schedules_page(cx).into_any_element()
        } else if self.page == Page::Artifacts {
            self.artifacts_page(cx).into_any_element()
        } else if self.page == Page::Skills {
            self.skills_page(cx).into_any_element()
        } else if self.page == Page::Messaging {
            self.messaging_page(cx).into_any_element()
        } else if self.page != Page::Chat {
            self.page_view(self.page, cx).into_any_element()
        } else if conversation.messages.is_empty() && !streaming_here {
            self.empty_state(cx).into_any_element()
        } else {
            // One turn per prompt, like Claude Code: the user's message, then
            // everything the model did in reply under a single label.
            let mut turns: Vec<Vec<AnyElement>> = Vec::new();
            let mut assistant_turn = false;
            for (i, m) in conversation.messages.iter().enumerate() {
                let is_user = m.role == "user";
                if is_user || !assistant_turn {
                    turns.push(Vec::new());
                }
                let labeled = is_user || !assistant_turn;
                assistant_turn = !is_user;
                turns
                    .last_mut()
                    .unwrap()
                    .push(self.message_row(i, m, labeled, false, window, cx));
            }
            if streaming_here {
                if !assistant_turn {
                    turns.push(Vec::new());
                }
                let rows_here = turns.last_mut().unwrap();
                // While tools are queued the card below speaks for the turn;
                // showing a half-built reply as well would be two spinners.
                let idle = self
                    .turn
                    .as_ref()
                    .is_some_and(|turn| matches!(turn.next(|_| false), agent::Next::Done));
                if let Some(live) = self.turn.as_ref().filter(|_| idle).map(|turn| Message {
                    role: "assistant".into(),
                    content: turn.live.content.clone(),
                    model: self.store.settings.model.clone(),
                    reasoning: turn.live.reasoning.clone(),
                    tokens: turn.live.tokens,
                    per_second: turn.live.per_second,
                    prompt_tokens: turn.live.prompt_tokens,
                    cached_tokens: turn.live.cached_tokens,
                    ..Default::default()
                }) {
                    rows_here.push(self.message_row(
                        usize::MAX,
                        &live,
                        !assistant_turn,
                        true,
                        window,
                        cx,
                    ));
                }
                if let Some(card) = self.approval_card(window, cx) {
                    rows_here.push(card);
                }
            }
            let rows: Vec<AnyElement> = turns
                .into_iter()
                // No min_w_0 or clipping here: both break list layout in the
                // markdown renderer, which sizes items against this width.
                // Long lines are held in by the row and code-block widths.
                .map(|turn| v_flex().w_full().gap_3().children(turn).into_any_element())
                .collect();
            // The scrollbar must sit outside the scrolling element, or it
            // scrolls away with the content.
            div()
                .relative()
                .flex_1()
                .min_h_0()
                // Constrain the scroll area itself rather than the column
                // inside it. A max width on the inner column loses to the
                // markdown renderer's intrinsic width, which then drags the
                // whole chat wider than the composer. Here min_w_0 pins this
                // box to 82%, so everything inside inherits a definite width
                // and cannot push back.
                .min_w_0()
                .w_full()
                .when(self.sidebar_open, |d| d.max_w(relative(0.82)))
                .mx_auto()
                .child(
                    div()
                        .id("messages")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(v_flex().w_full().px_6().py_6().gap_6().children(rows)),
                )
                .vertical_scrollbar(&self.scroll)
                .into_any_element()
        };

        let main = v_flex()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(body)
            .when(self.page == Page::Chat, |this| {
                this.child(
                    v_flex()
                        .w_full()
                        .when(self.sidebar_open, |d| d.max_w(relative(0.82)))
                        .mx_auto()
                        .px_6()
                        .pb_2()
                        .children(self.todo_panel(cx))
                        .child(self.composer(cx)),
                )
            })
            .child(self.status_bar(cx))
            .children(Root::render_notification_layer(window, cx));

        v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .font_family(BODY_FONT)
            .text_sm()
            .relative()
            .on_action(cx.listener(|this, _: &NewSession, window, cx| this.new_chat(window, cx)))
            .child(self.title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_open, |this| this.child(self.sidebar(cx)))
                    .child(main),
            )
            .children(self.settings_overlay(window, cx))
            .children(Root::render_dialog_layer(window, cx))
    }
}
