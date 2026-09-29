use crate::prelude::*;
use letronna_core::agent;

pub(crate) const BRAND: &str = "Letronna";

pub(crate) const GREETINGS: &[&str] = &[
    "Letronna is ready.",
    "Letronna is listening.",
    "What are we building today?",
    "Tell Letronna the goal.",
    "Ready when you are.",
];

actions!(letronna, [NewSession, Dismiss, ToggleSidebar, OpenSettings]);

/// How near the bottom still counts as at it: a line or two, so a reply
/// that grew a little since the last look is still followed.
const FOLLOW_SLACK: Pixels = px(48.);

/// The chat column's widest, gutters included: about 90 characters of
/// text, which reads as one measure. A wider window gets wider margins
/// rather than longer lines.
const COLUMN: Pixels = px(816.);

pub(crate) enum Verdict {
    Allow {
        remember: bool,
    },
    /// Allow, and keep a rule allowing calls like it in this project.
    AllowHere,
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
            Page::Messaging => "Message Letronna from a chat app, and get answers back there.",
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
    /// Where the browser is when it is not on this computer.
    pub(crate) browser_address: Entity<InputState>,
    /// Every model on every set-up gateway, as (gateway, model).
    pub(crate) models: Vec<(String, String)>,
    /// Which list the model menu shows: `chat`, or one of the media kinds.
    pub(crate) model_kind: &'static str,
    /// The hosted models that make images, speech and video, with prices.
    pub(crate) media_models: Vec<letronna_core::media::Offer>,
    /// The model picked for each media kind, as `media.json` keeps it.
    pub(crate) media_choices: std::collections::BTreeMap<String, String>,
    pub(crate) model_open: bool,
    pub(crate) model_search: Entity<InputState>,
    pub(crate) keys: std::collections::HashMap<String, String>,
    pub(crate) gateway_pick: Entity<String>,
    pub(crate) presence_pick: Entity<String>,
    pub(crate) settings_page: Entity<&'static str>,
    pub(crate) settings_open: bool,
    /// Held by the settings panel while it is open, so escape reaches it.
    pub(crate) settings_focus: FocusHandle,
    /// Held by the pages without a composer, so the window's shortcuts still
    /// reach the chat there.
    pub(crate) page_focus: FocusHandle,
    pub(crate) settings_backdrop: Option<std::sync::Arc<RenderImage>>,
    pub(crate) grains: crate::ui::grain::Grains,
    pub(crate) settings_closing: bool,
    pub(crate) rename: Entity<InputState>,
    pub(crate) page: Page,
    pub(crate) sidebar_open: bool,
    pub(crate) greeting: &'static str,
    /// Who is signed in, as `account.json` last said; shared with the terminal.
    pub(crate) account: Option<letronna_core::auth::Account>,
    /// What the last sign-in, refresh or sign-out had to say.
    pub(crate) account_note: Option<String>,
    /// The teams the account is in, for the Account page's billing choice.
    pub(crate) teams: Vec<letronna_core::auth::Team>,
    pub(crate) signing_in: bool,
    /// A messaging token is being checked with its service.
    pub(crate) connecting: bool,
    /// The sign-in page the browser was sent to, while the window waits on
    /// it, so it can be opened again or copied into another browser.
    pub(crate) sign_in_url: Option<String>,
    /// Ends the wait for the browser at once, for the Cancel button.
    pub(crate) sign_in_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) loading_models: bool,
    pub(crate) gateway_error: Option<String>,
    pub(crate) busy: bool,
    /// The turn in flight, if any. Owns the streamed reply and the tool queue.
    pub(crate) turn: Option<agent::Turn>,
    /// Stops the tool the turn in flight is running, when set.
    pub(crate) turn_stop: letronna_core::cancel::Cancel,
    /// Whether the call at the front of the turn is running, rather than
    /// waiting on its card; a stop answers the two differently.
    pub(crate) call_running: bool,
    /// The session id currently receiving a streamed reply, if any.
    pub(crate) streaming_id: Option<u64>,
    /// The id of the queued call the approval card is up for, decided once
    /// when the queue stopped on it.
    pub(crate) asking: Option<String>,
    /// The rule "Always allow" keeps for the call the card is up for, worked
    /// out once when the card came up rather than on every frame.
    pub(crate) offer: Option<letronna_core::permissions::Rule>,
    /// Tools the user allowed for the rest of this run of the app, by the
    /// session they were allowed in. Keyed like `unattended`: "stop asking"
    /// about a tool in one chat is not a yes to it in every other chat.
    pub(crate) session_allow: letronna_core::gate::Grants,
    /// The rules file error last shown, so that one is shown once.
    pub(crate) rules_trouble: Option<String>,
    /// The user's rules as last read.
    pub(crate) rules: Result<Vec<letronna_core::permissions::Rule>, String>,
    /// The user's and the project's rules files' modified times when `rules`
    /// was read, so that the queue moving reads them again only once one
    /// changed.
    pub(crate) rules_read: Option<[Option<std::time::SystemTime>; 2]>,
    /// Sessions running an unattended schedule, which run tools without
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
    /// Whether the message box holds nothing to send.
    pub(crate) composer_empty: bool,
    pub(crate) schedules: Vec<letronna_core::schedule::Schedule>,
    /// Id of the schedule being edited on the Scheduled jobs page.
    pub(crate) editing: Option<u64>,
    pub(crate) job_name: Entity<InputState>,
    pub(crate) job_prompt: Entity<InputState>,
    pub(crate) messaging: letronna_core::messaging::Config,
    /// A chat that has messaged the bot but has not been allowed yet.
    pub(crate) pairing: Option<(i64, String)>,
    /// Sessions a schedule opened, so their result can be reported onward.
    pub(crate) scheduled: std::collections::HashSet<u64>,
    /// Sessions opened for a remote chat, so the reply goes back to it.
    pub(crate) remote: std::collections::HashMap<u64, letronna_core::messaging::Source>,
    /// The session each remote chat is talking in, so a follow-up keeps its
    /// context. `/new` from the chat starts another.
    pub(crate) remote_session: std::collections::HashMap<letronna_core::messaging::Source, u64>,
    /// Bumped when a turn is stopped. The stream thread of the stopped turn
    /// cannot be interrupted, and a tool that does not listen to `turn_stop`
    /// runs on; what they send back carries the old number and is dropped.
    pub(crate) generation: u64,
    /// What reads the reply in flight. Kept rather than detached, so that
    /// Stop drops the receiver at once, which is how the worker learns to
    /// give up, even in the middle of a wait to try again.
    pub(crate) reading: Option<Task<()>>,
    /// A remote chat that has been sent the card the turn is waiting on, and
    /// the call it is about. Its next message answers it.
    pub(crate) remote_waiting: Option<(
        letronna_core::messaging::Source,
        letronna_core::chat::ToolCall,
    )>,
    pub(crate) messaging_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) token_input: Entity<InputState>,
    pub(crate) webhook_input: Entity<InputState>,
    pub(crate) discord_token_input: Entity<InputState>,
    pub(crate) discord_owner_input: Entity<InputState>,
    pub(crate) discord_servers_input: Entity<InputState>,
    /// The webhook's name, once it has been checked.
    pub(crate) hook_name: Option<String>,
    /// The connected bot's @name, once the token has been checked.
    pub(crate) bot_name: Option<String>,
    /// The Discord bot's name, once its token has been checked.
    pub(crate) discord_bot_name: Option<String>,
    /// Selected row on the Messaging page.
    pub(crate) channel: usize,
    pub(crate) skills: Vec<letronna_core::skills::Skill>,
    /// Why the skills, or the old skills.json, could not be read; shown
    /// above the list.
    pub(crate) skills_error: Option<String>,
    /// The name of the skill being edited, or empty for a new one.
    pub(crate) editing_skill: Option<String>,
    pub(crate) skill_name: Entity<InputState>,
    pub(crate) skill_prompt: Entity<InputState>,
    /// What you sent while a reply was running in its session, oldest
    /// first. It goes in at the reply's next step, as Claude Code does, or
    /// as the next prompt if the reply ends first.
    pub(crate) follow_ups: Vec<String>,
    /// Queued text a stopped or failed reply never took, on its way back
    /// into the box; set where there is no window to hand, put there when
    /// the window next draws.
    pub(crate) returned_follow_ups: Option<String>,
    /// Where to add skills from: a GitHub repository, a link or a path.
    pub(crate) skill_source: Entity<InputState>,
    /// Skills are being added, which waits on the network.
    pub(crate) adding_skills: bool,
    /// What the last add or removal of skills came to, or why it failed.
    pub(crate) skills_note: Option<Result<String, String>>,
    /// The skills the last add left out, each with why.
    pub(crate) skills_refused: Vec<String>,
    /// Your message being changed in place, by session id and place.
    pub(crate) editing_message: Option<(u64, usize)>,
    /// The box a message is changed in.
    pub(crate) edit_message: Entity<InputState>,
    /// Thoughts and tool outputs shown open, by session id and place.
    pub(crate) thinking_open: std::collections::HashSet<(u64, usize)>,
    /// The video playing in the chat, if one is, and its newest frame.
    pub(crate) video: Option<crate::platform::video::Playing>,
    pub(crate) video_frame: Option<std::sync::Arc<RenderImage>>,
    /// The right panel's state: shown or hidden, its width and its tabs.
    pub(crate) panel: crate::ui::panel::Panel,
    /// The browser the agent uses, shown beside the chat while it does, and
    /// the session whose browser it shows: each session has its own.
    pub(crate) browser: Entity<crate::ui::browser::BrowserView>,
    pub(crate) browser_for: u64,
    /// The composer holds the draft of the session `draft_for`; the other
    /// sessions' drafts wait here, by session id.
    pub(crate) drafts: std::collections::HashMap<u64, String>,
    pub(crate) draft_for: u64,
    pub(crate) error: Option<String>,
    /// Where updating stands, for the About page and the status bar.
    pub(crate) updating: crate::ui::update::Updating,
    /// A new version to announce, the next time the window draws.
    pub(crate) update_note: Option<String>,
    pub(crate) scroll: ScrollHandle,
    /// Whether the chat keeps to its newest message as more arrives.
    pub(crate) following: bool,
    /// Where the chat was scrolled to at the last draw, to tell which way
    /// it has moved since.
    pub(crate) followed_offset: Pixels,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl Chat {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Sessions are separate files now, so a damaged one costs that
        // session rather than the history.
        let (mut store, error) = (letronna_core::store::load(), None);
        crate::theme::apply(&store.settings.appearance, window, cx);
        letronna_core::browser::share_browser(store.settings.browser_shared);
        std::thread::spawn(letronna_core::browser::close_left_over);
        if store.conversations.is_empty() {
            store.conversations.push(Conversation {
                id: store.reserve_id(),
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
            store.settings.endpoint = std::env::var("AGENT_BASE_URL")
                .unwrap_or_else(|_| gateway(&store.settings.gateway).base.into());
        }
        let keys = load_keys();
        let saved_key = keys
            .get(key_slot(&store.settings.gateway))
            .cloned()
            .or_else(|| std::env::var("AGENT_API_KEY").ok())
            .unwrap_or_default();
        // No model yet: the list is loaded from the gateway and one is picked there.
        if store.settings.model.is_empty() {
            store.settings.model = std::env::var("AGENT_MODEL").unwrap_or_default();
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
        let browser_address = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("wss://… or http://address:9222")
                .masked(true)
                .default_value(
                    keys.get(letronna_core::browser::ELSEWHERE)
                        .cloned()
                        .unwrap_or_default(),
                )
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
                .default_value(letronna_core::messaging::load().telegram.token)
        });
        let webhook_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("https://discord.com/api/webhooks/...")
                .default_value(letronna_core::messaging::load().discord.webhook)
        });
        let discord_token_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("Bot token from the Developer Portal")
                .default_value(letronna_core::messaging::load().discord.bot_token)
        });
        let discord_owner_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Your Discord user ID")
                .default_value(letronna_core::messaging::load().discord.owner)
        });
        let discord_servers_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Server IDs, separated by commas")
                .default_value(letronna_core::messaging::load().discord.servers.join(", "))
        });
        let skill_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Review these changes"));
        let skill_prompt = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 10)
                .placeholder("What should Letronna do?")
        });
        let skill_source = cx.new(|cx| {
            InputState::new(window, cx).placeholder("GitHub repo, link to a SKILL.md, or a folder")
        });
        let job_prompt = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 6)
                .placeholder("Run the tests and tell me what broke.")
        });
        let edit_message = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(1, 12)
        });
        let subscriptions = vec![
            cx.subscribe_in(&model_search, window, |this, _, event, window, cx| {
                // Enter takes the first match, so a model can be picked by
                // typing a few letters of it. Only on the text tab: the
                // image, speech and video tabs list other models.
                if matches!(event, InputEvent::PressEnter { .. })
                    && this.model_kind == "chat"
                    && let Some((gateway, id)) = this.model_matches(cx).into_iter().next()
                {
                    this.choose_model(gateway, id, window, cx);
                }
                cx.notify()
            }),
            cx.subscribe_in(&composer, window, |this, input, event, window, cx| {
                match event {
                    InputEvent::PressEnter { secondary: false } => this.send(window, cx),
                    // Send lights up once there is something to send. Redraw
                    // only when that flips, not on every key.
                    InputEvent::Change => {
                        let empty = input.read(cx).value().trim().is_empty();
                        if empty != this.composer_empty {
                            this.composer_empty = empty;
                            cx.notify();
                        }
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(&search, window, |_, _, _: &InputEvent, _, cx| cx.notify()),
            cx.subscribe_in(&edit_message, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { secondary: false }) {
                    this.send_edit(window, cx);
                }
            }),
            cx.subscribe_in(&answer, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.submit_answer(window, cx);
                }
            }),
            cx.subscribe_in(&skill_source, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.add_skills(window, cx);
                }
            }),
            // Only a window left to follow the system hears it change; the
            // other settings pin the window's own look.
            cx.observe_window_appearance(window, |this, window, cx| {
                if crate::theme::follows_system(&this.store.settings.appearance) {
                    // As when the theme is picked: the blurred snapshot behind
                    // settings wears the old colours.
                    this.settings_backdrop = None;
                    crate::theme::apply(&this.store.settings.appearance, window, cx);
                }
            }),
        ];
        // The message when the old skills.json will not move across, or the
        // skills cannot be read at all; the Skills page shows it too.
        let workspace = store.conversations[store.active].workspace.as_deref();
        let moved = letronna_core::skills::migrate();
        let (skills, skills_error) = match letronna_core::skills::list(workspace) {
            Ok(skills) => (skills, moved.err()),
            Err(why) => (Vec::new(), Some(why)),
        };
        let draft_for = store.conversations[store.active].id;
        let mut this = Self {
            page: Page::Chat,
            sidebar_open: true,
            greeting,
            account: letronna_core::auth::account(),
            account_note: None,
            teams: Vec::new(),
            signing_in: false,
            connecting: false,
            sign_in_url: None,
            sign_in_cancel: None,
            loading_models: false,
            gateway_error: None,
            store,
            composer,
            search,
            endpoint,
            model,
            key,
            browser_address,
            models: Vec::new(),
            model_kind: "chat",
            media_models: Vec::new(),
            media_choices: letronna_core::media::choices(),
            model_open: false,
            model_search,
            keys,
            gateway_pick,
            presence_pick,
            settings_page,
            settings_open: false,
            settings_focus: cx.focus_handle(),
            page_focus: cx.focus_handle(),
            settings_backdrop: None,
            grains: crate::ui::grain::Grains::new(cx),
            settings_closing: false,
            rename,
            busy: false,
            turn: None,
            turn_stop: Default::default(),
            call_running: false,
            streaming_id: None,
            asking: None,
            offer: None,
            session_allow: letronna_core::gate::Grants::new(),
            rules_trouble: None,
            rules: Ok(Vec::new()),
            rules_read: None,
            unattended: std::collections::HashSet::new(),
            answer,
            job_tick: false,
            plan_mode: false,
            composer_empty: true,
            schedules: letronna_core::schedule::load(),
            editing: None,
            job_name,
            job_prompt,
            messaging: letronna_core::messaging::load(),
            pairing: None,
            scheduled: std::collections::HashSet::new(),
            remote: std::collections::HashMap::new(),
            remote_session: std::collections::HashMap::new(),
            generation: 0,
            reading: None,
            remote_waiting: None,
            messaging_stop: None,
            token_input,
            webhook_input,
            discord_token_input,
            discord_owner_input,
            discord_servers_input,
            hook_name: None,
            bot_name: None,
            discord_bot_name: None,
            channel: 0,
            skills,
            skills_error: skills_error.clone(),
            editing_skill: None,
            skill_name,
            skill_prompt,
            follow_ups: Vec::new(),
            returned_follow_ups: None,
            skill_source,
            adding_skills: false,
            skills_note: None,
            skills_refused: Vec::new(),
            editing_message: None,
            edit_message,
            thinking_open: std::collections::HashSet::new(),
            video: None,
            video_frame: None,
            panel: Default::default(),
            browser: cx.new(|cx| crate::ui::browser::BrowserView::new(window, cx)),
            browser_for: 0,
            drafts: std::collections::HashMap::new(),
            draft_for,
            error: error.or(skills_error),
            updating: Default::default(),
            update_note: None,
            scroll: ScrollHandle::new(),
            following: true,
            followed_offset: px(0.),
            _subscriptions: subscriptions,
        };
        let widen = this.follow_browser_layout(window, cx);
        this._subscriptions.push(widen);
        this.load_models(window, cx);
        // Directly, not through `focus_composer`: the window's root does
        // not exist yet to ask about dialogs, and there can be none.
        this.composer
            .update(cx, |input, cx| input.focus(window, cx));
        Self::watch_schedules(cx);
        Self::watch_updates(cx);
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
        let keys = self.keys.clone();
        let custom = self.store.settings.custom_endpoint.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            // The media models come from Letronna whatever the gateway, and
            // a list that will not load only leaves their tabs out.
            let media = letronna_core::media::catalogue().unwrap_or_default();
            let _ = tx.send_blocking((all_models(&keys, &custom), media));
        });
        let _ = window;
        cx.spawn(async move |this, cx| {
            if let Ok(((models, errors), media)) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.loading_models = false;
                    this.models = models;
                    this.media_models = media;
                    // One gateway failing still leaves the others usable.
                    this.gateway_error = (!errors.is_empty()).then(|| errors.join(" · "));
                    if this.models.is_empty() {
                        this.error = this.gateway_error.clone();
                    }
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// A model and the gateway it is on, chosen together.
    pub(crate) fn choose_model(
        &mut self,
        gateway: String,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model
            .update(cx, |input, cx| input.set_value(id.clone(), window, cx));
        self.store.settings.gateway = gateway;
        self.store.settings.model = id;
        self.model_open = false;
        self.focus_composer(window, cx);
        self.save();
        cx.notify();
    }

    /// The model a media kind's tool uses, or `None` for the cheapest working.
    pub(crate) fn choose_media(
        &mut self,
        kind: &'static str,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        match letronna_core::media::choose(kind, model.as_deref()) {
            Ok(()) => self.media_choices = letronna_core::media::choices(),
            Err(why) => self.error = Some(why),
        }
        self.model_open = false;
        cx.notify();
    }

    pub(crate) fn save(&mut self) {
        if let Err(error) = self.store.save() {
            letronna_core::diag::event(
                "save_failed",
                &[("error", error.kind().to_string().into())],
            );
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
            id: self.store.reserve_id(),
            title: "New session".into(),
            updated: backend::now(),
            workspace,
            ..Default::default()
        });
        self.store.active = self.store.conversations.len() - 1;
        self.error = None;
        self.page = Page::Chat;
        self.swap_draft(window, cx);
        self.focus_composer(window, cx);
        self.save();
        cx.notify();
    }

    pub(crate) fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let prompt = self.composer.read(cx).value().trim().to_owned();
        if self.busy && !prompt.is_empty() {
            return self.queue_follow_up(prompt, window, cx);
        }
        if self.busy {
            return;
        }
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
        if self.dispatch(prompt, None, cx).is_err() {
            self.open_settings(window, cx);
        }
    }

    /// Send a prompt in the active session. Split out of `send` so a schedule
    /// can fire one with no window in hand. `Err` means the gateway is not
    /// usable; the caller decides whether to open Settings about it.
    pub(crate) fn dispatch(
        &mut self,
        prompt: String,
        remote: Option<letronna_core::chat::Remote>,
        cx: &mut Context<Self>,
    ) -> std::result::Result<(), ()> {
        let saved = self
            .keys
            .get(key_slot(&self.store.settings.gateway))
            .cloned()
            .unwrap_or_default();
        let (endpoint, key) = resolve(
            &self.store.settings.gateway,
            &self.store.settings.custom_endpoint,
            &saved,
        );
        let connection = Connection {
            endpoint,
            model: self.model.read(cx).value().to_string(),
            key,
            tools: true,
            workspace: None,
            plan: self.plan_mode,
            effort: self.store.settings.effort.clone(),
            search: self.store.settings.search,
            // A schedule's run and a chat app's turn may have nobody watching.
            unattended: remote.is_some()
                || self
                    .scheduled
                    .contains(&self.store.conversations[self.store.active].id),
            remote,
            conversation: self.store.conversations[self.store.active].id,
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
        // A browser closed during the last turn shows again in this one.
        self.browser.update(cx, |view, _| view.dismissed = false);
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
        self.follow();
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
            .position(|s| letronna_core::schedule::is_due(s, now))
        else {
            return;
        };
        self.schedules[index].last_run = backend::now();
        letronna_core::schedule::save(&self.schedules);
        let schedule = self.schedules[index].clone();
        self.start_scheduled(&schedule, cx);
    }

    /// Open a fresh session for a schedule and send its prompt.
    pub(crate) fn start_scheduled(
        &mut self,
        schedule: &letronna_core::schedule::Schedule,
        cx: &mut Context<Self>,
    ) {
        self.store.conversations.push(Conversation {
            id: self.store.reserve_id(),
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
        if self.dispatch(schedule.prompt.clone(), None, cx).is_err() {
            // The gateway is not usable, so the run never started and
            // `finish` will not come round to report it.
            self.scheduled.remove(&session);
            self.unattended.remove(&session);
            let error = self.error.clone().unwrap_or_default();
            self.send_report(schedule.name.clone(), format!("Did not start: {error}"), cx);
        }
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
        cx.notify();
        let generation = self.generation;
        self.reading = Some(cx.spawn(async move |this, cx| {
            while let Ok(event) = receiver.recv().await {
                let finished = matches!(event, Event::Finished(_));
                let current = this.update(cx, |this, cx| {
                    let current = this.generation == generation;
                    if current {
                        this.on_event(event, cx);
                    }
                    current
                });
                if !matches!(current, Ok(true)) || finished {
                    break;
                }
            }
        }));
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        turn.absorb(&event);
        match event {
            Event::Finished(Err(error)) => {
                self.error = Some(error.to_string());
                self.finish(cx);
            }
            Event::Finished(Ok(())) => self.on_reply(cx),
            _ => {}
        }
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
        let continuation = turn.continuation();
        // The session may have been switched or deleted while it streamed.
        let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) else {
            self.finish(cx);
            return;
        };
        let conversation = &mut self.store.conversations[ix];
        conversation.updated = backend::now();
        // A reply cut off inside a tool call, with no text before it, leaves
        // nothing to keep.
        if !reply.content.is_empty() || !calls.is_empty() {
            conversation.messages.push(reply);
        }
        match continuation {
            Ok(Some(notice)) => {
                conversation.messages.push(notice);
                self.save();
                self.run_turn(cx);
                return;
            }
            Ok(None) => {}
            Err(error) => self.error = Some(error),
        }
        self.save();
        let more = self.turn.as_mut().is_some_and(|turn| turn.begin(calls));
        if more {
            self.advance_tools(cx);
        } else {
            self.finish(cx);
        }
    }

    /// Reads the rules files again where either changed since last time,
    /// which the terminal may have done, and shows a new error in them once.
    fn read_rules(&mut self) {
        let user = letronna_core::permissions::user_file();
        let workspace = self.call_context().workspace;
        let project = workspace
            .as_deref()
            .map(|folder| folder.join(letronna_core::permissions::PROJECT_FILE));
        let modified = |file: &std::path::Path| {
            std::fs::metadata(file)
                .and_then(|meta| meta.modified())
                .ok()
        };
        let read = Some([modified(&user), project.as_deref().and_then(modified)]);
        if read == self.rules_read {
            return;
        }
        self.rules = letronna_core::permissions::load(&user);
        self.rules_read = read;
        let trouble = letronna_core::permissions::trouble(&self.rules, workspace.as_deref());
        if trouble != self.rules_trouble {
            if let Some(error) = &trouble {
                self.error = Some(error.clone());
            }
            self.rules_trouble = trouble;
        }
    }

    /// Run the next queued call, or stop on one that needs the user.
    pub(crate) fn advance_tools(&mut self, cx: &mut Context<Self>) {
        self.read_rules();
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        let next = turn.next(|call| self.decide(call, &self.rules));
        let asking = match &next {
            agent::Next::AskUser(call) => Some(call.id.clone()),
            _ => None,
        };
        if asking != self.asking {
            self.offer = match &next {
                agent::Next::AskUser(call) => {
                    letronna_core::permissions::suggest(call, &self.call_context())
                }
                _ => None,
            };
        }
        self.asking = asking;
        if let agent::Next::AskUser(call) | agent::Next::Run(call) = &next {
            self.show_browser(&call.name, cx);
        }
        match next {
            agent::Next::Done => self.finish(cx),
            agent::Next::AskUser(call) => {
                // A plan that did not come through whole goes back to the
                // model; approving it would lift plan mode for nothing.
                if call.name == letronna_core::tools::EXIT_PLAN
                    && let Err(error) = letronna_core::tools::plan_of(&call.arguments)
                {
                    return self.record_result(Err(error), cx);
                }
                self.ask_remote(call, cx);
                cx.notify()
            }
            agent::Next::Run(call) if call.name == letronna_core::tools::TODO => {
                self.apply_todo(&call.arguments.clone(), cx)
            }
            agent::Next::Run(call) => self.execute_call(call, false, cx),
            agent::Next::Refuse(_, why) => self.record_result(Err(why), cx),
            agent::Next::Ask => {
                let results = self
                    .turn
                    .as_mut()
                    .map(|turn| turn.take_results())
                    .unwrap_or_default();
                if let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) {
                    let conversation = &mut self.store.conversations[ix];
                    conversation.messages.extend(results);
                    // What you sent while it worked goes in now: after the
                    // tool results, before the model is asked again, so it
                    // reads it before going on.
                    conversation
                        .messages
                        .extend(self.follow_ups.drain(..).map(|text| Message {
                            role: "user".into(),
                            content: text,
                            folded: true,
                            ..Default::default()
                        }));
                    self.save();
                    self.run_turn(cx);
                } else {
                    self.finish(cx);
                }
            }
        }
    }

    /// Run one call off the UI thread and continue the queue when it returns;
    /// `approved` when the user said yes to it on its card.
    fn execute_call(&mut self, call: backend::ToolCall, approved: bool, cx: &mut Context<Self>) {
        let mut ctx = letronna_core::tools::ToolContext {
            approved,
            ..self.call_context()
        };
        ctx.cancel = self.turn_stop.clone();
        self.call_running = true;
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(letronna_core::tools::run(&call.name, &call.arguments, &ctx));
        });
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            if let Ok(result) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.call_running = false;
                        this.record_result(result, cx)
                    }
                });
            }
        })
        .detach();
    }

    /// Record the outcome of the current call and move on.
    pub(crate) fn record_result(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        if let Some(turn) = self.turn.as_mut() {
            turn.finish_call(result);
        }
        self.advance_tools(cx);
    }

    /// Stop any running poller and start one if messaging is set up. Called
    /// whenever the settings change, so the worker always matches them.
    pub(crate) fn restart_messaging(&mut self, cx: &mut Context<Self>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        if let Some(stop) = self.messaging_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        let telegram = &self.messaging.telegram;
        let discord = &self.messaging.discord;
        if !telegram.ready() && !discord.bot_ready() {
            return;
        }
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (tx, rx) = async_channel::unbounded();
        if telegram.ready() {
            letronna_core::messaging::poll(telegram.token.clone(), stop.clone(), tx.clone());
        }
        if discord.bot_ready() {
            letronna_core::messaging::discord_listen(discord.clone(), stop.clone(), tx);
        }
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

    fn tell_remote(
        &self,
        source: &letronna_core::messaging::Source,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        use letronna_core::messaging::{self, Source};
        let source = source.clone();
        let text = text.to_owned();
        let telegram = self.messaging.telegram.token.clone();
        let discord = self.messaging.discord.bot_token.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = match source {
                    Source::Telegram(chat) => messaging::send(&telegram, chat, &text),
                    Source::Discord(channel) => messaging::discord_post(&discord, &channel, &text),
                };
            })
            .detach();
    }

    /// A message arrived from a bot. A Telegram chat the user has not
    /// approved is held for approval rather than acted on; Discord only ever
    /// delivers the owner's DMs, so there is nothing to approve.
    fn on_remote(&mut self, message: letronna_core::messaging::Incoming, cx: &mut Context<Self>) {
        use letronna_core::messaging::Source;
        let source = message.source;
        if let Source::Telegram(chat) = source
            && self.messaging.telegram.chat != Some(chat)
        {
            self.pairing = Some((chat, message.from));
            cx.notify();
            return;
        }
        if message.said.trim() == "/stop" {
            if !self.busy {
                self.tell_remote(&source, "Nothing is running.", cx);
                return;
            }
            // `stop_turn` tells the chat that owns the turn; this one may not be it.
            let owner = self
                .streaming_id
                .and_then(|id| self.remote.get(&id))
                .cloned();
            self.stop_turn(cx);
            if owner.as_ref() != Some(&source) {
                self.tell_remote(&source, "Stopped.", cx);
            }
            return;
        }
        if let Some((waiting, call)) = self.remote_waiting.clone()
            && waiting == source
        {
            use letronna_core::messaging::{CardAnswer, card_answer};
            let verdict = match card_answer(&call.name, &call.arguments, &message.said) {
                CardAnswer::Answer(text) => Verdict::Answer(text),
                CardAnswer::Allow => Verdict::Allow { remember: false },
                CardAnswer::Deny => Verdict::Deny,
            };
            self.resolve_call(verdict, cx);
            return;
        }
        if self.busy {
            self.tell_remote(
                &source,
                "Still working on something. Send /stop to cancel it, or send this again \
                 once it is done.",
                cx,
            );
            return;
        }
        // Telegram sends /start when a chat first opens the bot.
        if matches!(message.said.trim(), "/new" | "/start") {
            self.remote_session.remove(&source);
            self.tell_remote(&source, "Started a new session.", cx);
            return;
        }
        // Carry on in the chat's session unless it has since been deleted.
        match self
            .remote_session
            .get(&source)
            .and_then(|id| self.store.index_of(*id))
        {
            Some(ix) => self.store.active = ix,
            None => {
                let workspace = self.store.conversations[self.store.active]
                    .workspace
                    .clone();
                self.store.conversations.push(Conversation {
                    id: self.store.reserve_id(),
                    title: message.text.chars().take(40).collect(),
                    updated: backend::now(),
                    workspace,
                    ..Default::default()
                });
                self.store.active = self.store.conversations.len() - 1;
            }
        }
        let id = self.store.conversations[self.store.active].id;
        self.remote_session.insert(source.clone(), id);
        let allow_tools = match source {
            Source::Telegram(_) => self.messaging.telegram.allow_tools,
            Source::Discord(_) => self.messaging.discord.allow_tools,
        };
        self.remote.insert(id, source);
        // Nobody on the far end can answer an approval card, so unless the
        // user has said otherwise a remote message may look but not touch.
        let was_plan = self.plan_mode;
        self.plan_mode = !allow_tools;
        let _ = self.dispatch(message.text, Some(message.remote), cx);
        self.plan_mode = was_plan;
    }

    /// A scheduled run has no one watching, so post how it ended to every
    /// channel set up to hear it. A failure is reported too: silence would
    /// read as nothing having run at all.
    fn report_scheduled(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.streaming_id else { return };
        if !self.scheduled.remove(&id) {
            return;
        }
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        let conversation = &self.store.conversations[ix];
        let body = match &self.error {
            Some(error) => format!("Did not finish: {error}"),
            None => conversation
                .messages
                .iter()
                .rev()
                .find(|m| m.role == "assistant" && !m.content.is_empty())
                .map(|m| m.content.clone())
                .unwrap_or_else(|| "That did not produce an answer.".to_owned()),
        };
        let title = conversation.title.clone();
        self.send_report(title, body, cx);
    }

    fn send_report(&self, title: String, body: String, cx: &mut Context<Self>) {
        let config = self.messaging.clone();
        cx.background_executor()
            .spawn(async move {
                let _ = letronna_core::messaging::report(&config, &title, &body);
            })
            .detach();
    }

    /// Send the finished reply back to the chat that asked for it.
    fn answer_remote(&mut self, cx: &mut Context<Self>) {
        let Some(id) = self.streaming_id else { return };
        let Some(source) = self.remote.remove(&id) else {
            return;
        };
        // Only this turn's reply: the session carries on across messages, so
        // an earlier answer is further up and must not be sent again.
        let reply = match &self.error {
            Some(error) => format!("Did not finish: {error}"),
            None => self
                .store
                .index_of(id)
                .and_then(|ix| {
                    self.store.conversations[ix]
                        .messages
                        .iter()
                        .rev()
                        .take_while(|m| m.role != "user")
                        .find(|m| m.role == "assistant" && !m.content.is_empty())
                })
                .map(|m| m.content.clone())
                .unwrap_or_else(|| "That did not produce an answer.".to_owned()),
        };
        self.tell_remote(&source, &reply, cx);
    }

    /// The context tools of the session running now are called in.
    fn call_context(&self) -> letronna_core::tools::ToolContext {
        self.turn
            .as_ref()
            .map(agent::Turn::tool_context)
            .unwrap_or_default()
    }

    /// What becomes of `call` in the session running now, from the window's
    /// settings, grants and `rules`.
    pub(crate) fn decide(
        &self,
        call: &backend::ToolCall,
        rules: &Result<Vec<letronna_core::permissions::Rule>, String>,
    ) -> letronna_core::gate::Outcome {
        let unattended = self
            .streaming_id
            .is_some_and(|id| self.unattended.contains(&id));
        // A chat app is kept in plan mode by its owner's setting, which nobody
        // on the far end may lift, so nobody there can approve a plan either:
        // run, the tool refuses and says so.
        let chat_app = call.name == letronna_core::tools::EXIT_PLAN
            && self
                .turn
                .as_ref()
                .is_some_and(|turn| turn.connection.remote.is_some());
        if chat_app {
            return letronna_core::gate::Outcome::Run;
        }
        let ctx = self
            .turn
            .as_ref()
            .map(|turn| letronna_core::tools::ToolContext::of(&turn.connection))
            .unwrap_or_default();
        let gate = letronna_core::gate::Gate::for_window(
            &ctx,
            &self.session_allow,
            rules,
            self.store.settings.skip_approvals,
            unattended,
        );
        letronna_core::gate::decide(call, &gate)
    }

    /// Store the model's checklist on the session so the composer can show it.
    fn apply_todo(&mut self, arguments: &str, cx: &mut Context<Self>) {
        let mut todos = Vec::new();
        let message = letronna_core::store::apply_todo(&mut todos, arguments);
        if message.is_ok()
            && let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id))
        {
            self.store.conversations[ix].todos = todos;
            self.save();
        }
        self.record_result(message, cx);
    }

    /// A remote session cannot see the card, so send it to the chat; the
    /// chat's next message answers it. Without this the turn waits on a card
    /// nobody is looking at, and every later message is turned away as busy.
    fn ask_remote(&mut self, call: letronna_core::chat::ToolCall, cx: &mut Context<Self>) {
        let Some(source) = self
            .streaming_id
            .and_then(|id| self.remote.get(&id))
            .cloned()
        else {
            return;
        };
        if self
            .remote_waiting
            .as_ref()
            .is_some_and(|(_, waiting)| waiting.id == call.id)
        {
            return;
        }
        let ctx = self
            .turn
            .as_ref()
            .map(|turn| letronna_core::tools::ToolContext::of(&turn.connection))
            .unwrap_or_default();
        let preview = letronna_core::tools::preview(&call.name, &call.arguments, &ctx);
        let text = letronna_core::messaging::card_text(&call.name, &call.arguments, &preview);
        self.tell_remote(&source, &text, cx);
        self.remote_waiting = Some((source, call));
    }

    /// The user answered the approval card, here or from a chat.
    pub(crate) fn resolve_call(&mut self, verdict: Verdict, cx: &mut Context<Self>) {
        self.remote_waiting = None;
        let Some((call, true)) = self
            .turn
            .as_ref()
            .and_then(|turn| front_call(turn, self.asking.as_deref()))
        else {
            return;
        };
        self.asking = None;
        let offer = self.offer.take();
        if call.name == letronna_core::tools::EXIT_PLAN {
            let approved = matches!(verdict, Verdict::Allow { .. });
            // The Plan switch goes off with it, so the next message carries
            // on out of plan mode too.
            if approved {
                self.plan_mode = false;
            }
            if let Some(turn) = self.turn.as_mut() {
                turn.answer_plan(approved);
            }
            self.advance_tools(cx);
            return;
        }
        // Answered: until the queue moves on, the card shows the call running.
        cx.notify();
        match verdict {
            Verdict::Answer(text) => self.record_result(Ok(text), cx),
            Verdict::Deny => self.record_result(Err(letronna_core::tools::DECLINED.into()), cx),
            Verdict::Allow { remember } => {
                if remember && let Some(id) = self.streaming_id {
                    self.session_allow
                        .entry(id)
                        .or_default()
                        .insert(call.name.clone());
                }
                self.execute_call(call, true, cx);
            }
            Verdict::AllowHere => {
                // Into the user's own rules, held to this project: the
                // project's file may not allow anything.
                let file = letronna_core::permissions::user_file();
                let kept = offer.map(|rule| letronna_core::permissions::keep(&file, rule));
                if let Some(Err(error)) = kept {
                    self.error = Some(error);
                }
                self.execute_call(call, true, cx);
            }
        }
    }

    /// Decide the call the card is up for again after an approval setting
    /// changed, so it runs at once if it no longer needs the user.
    pub(crate) fn approvals_changed(&mut self, cx: &mut Context<Self>) {
        let waiting = self
            .turn
            .as_ref()
            .and_then(|turn| front_call(turn, self.asking.as_deref()))
            .is_some_and(|(_, waiting)| waiting);
        // Advancing past a call that is already running would start it twice.
        if !waiting {
            return;
        }
        self.advance_tools(cx);
        // The chat that was sent the card must not answer a call that ran.
        if self.asking.is_none() {
            self.remote_waiting = None;
        }
    }

    /// How far above the bottom of the chat, in pixels.
    fn distance_from_bottom(&self) -> Pixels {
        self.scroll.max_offset().height + self.scroll.offset().y
    }

    /// Go to the newest message and stay with it as more arrives.
    pub(crate) fn follow(&mut self) {
        self.following = true;
        self.scroll.scroll_to_bottom();
    }

    /// Stay at the bottom while following, and stop following once the user
    /// scrolls up to reread. Distance from the bottom alone cannot say which
    /// happened: a reply's markdown is laid out a moment after it arrives, so
    /// the bottom runs ahead of the view on its own. What the user does is
    /// move the view up and away from the bottom. Content that shrinks also
    /// moves it up, but pinned to the bottom, and that must not count.
    fn keep_following(&mut self) {
        let offset = self.scroll.offset().y;
        let near_bottom = self.distance_from_bottom() <= FOLLOW_SLACK;
        // The offset grows towards zero going up.
        if offset > self.followed_offset && !near_bottom {
            self.following = false;
        } else if offset < self.followed_offset && near_bottom {
            self.following = true;
        }
        self.followed_offset = offset;
        if self.following {
            self.scroll.scroll_to_bottom();
        }
    }

    /// A way back down once the chat has been scrolled up. Following stops
    /// there on purpose, so without this the only way back to a reply still
    /// arriving is to wheel down to it.
    fn jump_to_latest(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        if self.following || self.distance_from_bottom() <= FOLLOW_SLACK {
            return None;
        }
        Some(
            h_flex()
                .absolute()
                .bottom_3()
                .left_0()
                .right_0()
                .justify_center()
                .child(
                    Button::new("jump-to-latest")
                        .icon(Icon::new(IconName::ArrowDown))
                        .small()
                        .rounded_full()
                        // It sits over the text, so it needs an edge to be
                        // told apart from it.
                        .border_1()
                        .border_color(cx.theme().primary.opacity(0.5))
                        .tooltip("Jump to latest")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.follow();
                            cx.notify();
                        })),
                ),
        )
    }

    /// End the turn in flight, from the Stop button or `/stop` in a chat.
    /// The chat or schedule that started it hears "Stopped." rather than an
    /// error. What the turn got to is kept: the reply so far, or the batch's
    /// results with every call left answered as stopped.
    pub(crate) fn stop_turn(&mut self, cx: &mut Context<Self>) {
        if !self.busy {
            return;
        }
        self.generation += 1;
        // Stopping is not a go-ahead for what was queued: it goes back in
        // the box rather than being sent.
        self.return_follow_ups();
        // Reaches the running tool, so a wait or a command ends now rather
        // than running on unseen. Taken, so the next turn starts unstopped.
        std::mem::take(&mut self.turn_stop).set();
        self.reading = None;
        self.remote_waiting = None;
        if let Some(id) = self.streaming_id {
            if let Some(source) = self.remote.remove(&id) {
                self.tell_remote(&source, "Stopped.", cx);
            }
            if self.scheduled.remove(&id) {
                let title = self
                    .store
                    .index_of(id)
                    .map(|ix| self.store.conversations[ix].title.clone())
                    .unwrap_or_default();
                self.send_report(title, "Stopped.".into(), cx);
            }
        }
        self.keep_stopped();
        self.finish(cx);
    }

    /// Save what a stopped turn got to, so the model hears next time what ran
    /// and what did not.
    fn keep_stopped(&mut self) {
        let model = self.store.settings.model.clone();
        let running = std::mem::take(&mut self.call_running);
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        if running {
            turn.finish_call(Err(agent::STOPPED_WHILE_RUNNING.into()));
        }
        let kept = turn.stop(&model);
        let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) else {
            return;
        };
        if kept.is_empty() {
            return;
        }
        let conversation = &mut self.store.conversations[ix];
        conversation.updated = backend::now();
        conversation.messages.extend(kept);
        self.save();
    }

    /// Escape that nothing nearer took: close what is open over the chat,
    /// or else stop the reply, which is what escape does in the terminal.
    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_message.is_some() {
            self.cancel_edit(window, cx);
        } else if self.settings_open {
            self.close_settings(window, cx);
        } else if self.model_open {
            // Back to the box, as picking a model does; the search field
            // had focus, and closing it left typing going nowhere.
            self.model_open = false;
            self.focus_composer(window, cx);
            cx.notify();
        } else if self.busy_elsewhere().is_none() {
            // Not a reply in another session, which nothing here shows.
            self.stop_turn(cx);
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        self.answer_remote(cx);
        self.report_scheduled(cx);
        let session = self.streaming_id;
        // The grant is for the run, not the session: a follow-up typed there
        // has someone watching, and a deleted session's id is handed out again.
        if let Some(id) = self.streaming_id {
            self.unattended.remove(&id);
        }
        self.busy = false;
        self.streaming_id = None;
        self.turn = None;
        plugins::emit(cx, plugins::AppEvent::ChatIdle);
        self.send_follow_ups(session, cx);
        cx.notify();
    }

    /// Queue what was typed while a reply runs, for its next step. Only in
    /// the session that is working: elsewhere it would land in the wrong
    /// conversation.
    fn queue_follow_up(&mut self, prompt: String, window: &mut Window, cx: &mut Context<Self>) {
        let here = self.store.conversations[self.store.active].id;
        let refused = if self.streaming_id != Some(here) {
            Some("Another session is working. Send this when it is done, or stop it first.")
        } else if prompt.chars().count() > 12_000 {
            Some("Please keep each message under 12,000 characters.")
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
        self.follow_ups.push(prompt);
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.follow();
        cx.notify();
    }

    /// A reply that ended before a step could take what was queued: send it
    /// as the next prompt, in the session it was written for, whichever one
    /// is on screen now.
    fn send_follow_ups(&mut self, session: Option<u64>, cx: &mut Context<Self>) {
        if self.follow_ups.is_empty() {
            return;
        }
        // A reply that failed would most likely fail again.
        let Some(ix) = session
            .and_then(|id| self.store.index_of(id))
            .filter(|_| self.error.is_none())
        else {
            return self.return_follow_ups();
        };
        let text = std::mem::take(&mut self.follow_ups).join("\n\n");
        let viewing = self.store.active;
        self.store.active = ix;
        let sent = self.dispatch(text.clone(), None, cx).is_ok();
        self.store.active = viewing;
        if !sent {
            self.returned_follow_ups = Some(text);
        }
    }

    /// Take a queued message back before it goes: into the box to change it,
    /// or away altogether.
    pub(crate) fn unqueue(
        &mut self,
        n: usize,
        to_box: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if n >= self.follow_ups.len() {
            return;
        }
        let text = self.follow_ups.remove(n);
        if to_box {
            self.composer.update(cx, |input, cx| {
                let typed = input.value().trim().to_owned();
                let value = if typed.is_empty() {
                    text
                } else {
                    format!("{text}\n\n{typed}")
                };
                input.set_value(value, window, cx);
            });
            self.focus_composer(window, cx);
        }
        cx.notify();
    }

    /// Put what was queued back where it was written, for the next draw.
    fn return_follow_ups(&mut self) {
        if self.follow_ups.is_empty() {
            return;
        }
        let text = std::mem::take(&mut self.follow_ups).join("\n\n");
        self.returned_follow_ups = Some(match self.returned_follow_ups.take() {
            Some(earlier) => format!("{earlier}\n\n{text}"),
            None => text,
        });
    }
}

impl Render for Chat {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The panel shows the browser of the session on screen.
        let on_screen = self.store.conversations[self.store.active].id;
        if self.browser_for != on_screen {
            self.browser_for = on_screen;
            letronna_core::browser::show_conversation(on_screen);
        }
        // And the composer holds its draft, however the session was switched
        // to: a click, a schedule, or a message from a chat app.
        self.swap_draft(window, cx);
        self.keep_following();
        // What a stopped or failed reply did not take goes back in the box,
        // ahead of anything typed since.
        if let Some(text) = self.returned_follow_ups.take() {
            self.composer.update(cx, |input, cx| {
                let typed = input.value().trim().to_owned();
                let value = if typed.is_empty() {
                    text
                } else {
                    format!("{text}\n\n{typed}")
                };
                input.set_value(value, window, cx);
            });
        }
        // A job starting or exiting happens off the UI thread, so the footer
        // chips need a poll while any of them is alive. It stops on its own.
        if !self.job_tick && letronna_core::jobs::list().iter().any(|job| job.running) {
            self.job_tick = true;
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(1))
                        .await;
                    let alive = letronna_core::jobs::list().iter().any(|job| job.running);
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
        if let Some(note) = self.update_note.take() {
            window.push_notification(gpui_component::notification::Notification::info(note), cx);
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
                            .plain(cx)
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
        // The column is given a width outright, from the window's, rather
        // than a maximum: the markdown renderer reports an intrinsic width
        // that beats a max width and drags the chat past the composer, but
        // it cannot push back on a definite one. Worked out here, not by
        // measuring the pane, so a resize does not lag a frame behind.
        let pane = window.viewport_size().width
            - if self.sidebar_open {
                crate::ui::sidebar::SIDEBAR_WIDTH
            } else {
                px(0.)
            }
            // Beside the docked panel, less again.
            - self.panel_width(window, cx);
        let column = pane.min(COLUMN).max(px(0.));

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
            // everything the model did in reply under a single label. Each
            // turn knows its place, which its buttons and menu work from.
            use crate::ui::actions::{Span, shows_bar};
            let messages = &conversation.messages;
            // A menu a turn opened takes the focus from inside that turn. That
            // is not the turn being picked out: showing its full row would
            // take away the button the menu hangs from, and the menu with it.
            let in_menu = window
                .context_stack()
                .iter()
                .any(|context| context.contains("PopupMenu"));
            let mut turns: Vec<(Vec<AnyElement>, Span, FocusHandle)> = Vec::new();
            let mut assistant_turn = false;
            for (i, m) in messages.iter().enumerate() {
                let is_user = m.role == "user";
                if is_user || !assistant_turn {
                    let span = Span {
                        first: i,
                        last: i,
                        yours: is_user,
                    };
                    turns.push((Vec::new(), span, self.turn_focus(i, window, cx)));
                }
                let labeled = is_user || !assistant_turn;
                assistant_turn = !is_user;
                let (rows, span, focus) = turns.last_mut().unwrap();
                span.last = i;
                // A reply that only asked for tools has nothing to show of
                // its own; the tool rows speak for it. Kept only when it
                // carries the turn's label or visible thinking, so it adds
                // no empty gap.
                let thinking = self.store.settings.thinking && !m.reasoning.is_empty();
                if m.role == "assistant"
                    && m.content.is_empty()
                    && !m.tool_calls.is_empty()
                    && !labeled
                    && !thinking
                {
                    continue;
                }
                // A turn is finished at its last message, unless the reply
                // to it is still coming in.
                let ends = messages.get(i + 1).is_none_or(|next| next.role == "user")
                    && !(streaming_here && i + 1 == messages.len());
                let bar = (is_user || ends).then(|| {
                    let focused = focus.contains_focused(window, cx) && !in_menu;
                    let shown = shows_bar(*span, messages.len(), streaming_here, focused);
                    self.turn_bar(*span, shown, cx)
                });
                rows.push(self.message_row(i, m, labeled, false, bar, window, cx));
            }
            // The turn a reply is still coming in to has no buttons or menu:
            // it is not finished, and it owns the session until it is.
            let live_turn = streaming_here.then(|| {
                if !assistant_turn {
                    turns.push((
                        Vec::new(),
                        Span {
                            first: messages.len(),
                            last: messages.len(),
                            yours: false,
                        },
                        self.turn_focus(messages.len(), window, cx),
                    ));
                }
                turns.len() - 1
            });
            if streaming_here {
                let (rows_here, _, _) = turns.last_mut().unwrap();
                // While tools are queued the card below speaks for the turn;
                // showing a half-built reply as well would be two spinners.
                let idle = self.turn.as_ref().is_some_and(|turn| {
                    let next = turn.next(|_| letronna_core::gate::Outcome::Run);
                    matches!(next, agent::Next::Done)
                });
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
                    // Keyed by the place the reply will have once it is
                    // stored, so finishing does not start its markdown or
                    // its fade-in over again.
                    rows_here.push(self.message_row(
                        conversation.messages.len(),
                        &live,
                        !assistant_turn,
                        true,
                        None,
                        window,
                        cx,
                    ));
                }
                if let Some(card) = self.approval_card(window, cx) {
                    rows_here.push(card);
                }
                for (n, text) in self.follow_ups.iter().enumerate() {
                    rows_here.push(self.queued_row(n, text, cx));
                }
            }
            let rows: Vec<AnyElement> = turns
                .into_iter()
                .enumerate()
                // No min_w_0 or clipping here: both break list layout in the
                // markdown renderer, which sizes items against this width.
                // Long lines are held in by the row and code-block widths.
                .map(|(n, (turn, span, focus))| {
                    // A hairline before each of your prompts but the first
                    // parts the exchanges, where a card used to.
                    let group = v_flex()
                        .w_full()
                        .gap_3()
                        .when(span.yours && n > 0, |group| {
                            group.border_t_1().border_color(cx.theme().border).pt_3()
                        })
                        .children(turn);
                    if live_turn == Some(n) {
                        return group.into_any_element();
                    }
                    // Its own id, so each turn's menu keeps its own state.
                    // Tab picks a turn out; a press on it does not, or
                    // opening a tool's output would take the keys from the
                    // composer (text inside still takes focus to select).
                    div()
                        .id(self.row_id("turn", span.first))
                        .w_full()
                        .track_focus(&focus)
                        .on_any_mouse_down(|_, window, _| window.prevent_default())
                        .child(group.context_menu(self.turn_menu(span, cx)))
                        .into_any_element()
                })
                .collect();
            // The scrollbar must sit outside the scrolling element, or it
            // scrolls away with the content. The scrolling box spans the
            // pane, so the bar is at its edge and the wheel works over the
            // margins too; only the column inside it is narrow.
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .w_full()
                .child(
                    div()
                        .id("messages")
                        .size_full()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .child(
                            h_flex().w_full().justify_center().child(
                                v_flex()
                                    .w(column)
                                    .flex_shrink_0()
                                    .px_6()
                                    .py_6()
                                    .gap_3()
                                    .children(rows),
                            ),
                        ),
                )
                .vertical_scrollbar(&self.scroll)
                .children(self.jump_to_latest(cx))
                .into_any_element()
        };

        let main = v_flex()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(self.page != Page::Chat, |this| {
                this.track_focus(&self.page_focus)
            })
            .child(body)
            .when(self.page == Page::Chat, |this| {
                this.child(
                    v_flex()
                        .w(column)
                        .mx_auto()
                        .px_6()
                        .pb_2()
                        .children(self.busy_note(cx))
                        .children(self.todo_panel(cx))
                        .child(self.composer(pane < px(600.), cx)),
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
            .key_context("Chat")
            .on_action(cx.listener(|this, _: &NewSession, window, cx| this.new_chat(window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| {
                this.sidebar_open = !this.sidebar_open;
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                if !this.settings_open {
                    this.open_settings(window, cx)
                }
            }))
            .on_action(
                cx.listener(|this, _: &crate::ui::panel::TogglePanel, window, cx| {
                    if !window.has_active_dialog(cx) && !this.settings_open {
                        this.toggle_panel(window, cx)
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::panel::BrowserTab, window, cx| {
                    if !window.has_active_dialog(cx) && !this.settings_open {
                        this.show_tab(crate::ui::panel::PanelTab::Browser, window, cx)
                    }
                }),
            )
            .child(
                div()
                    .relative()
                    .flex_shrink_0()
                    .child(self.title_bar(window, cx))
                    .children(self.grain(&self.grains.title_bar)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .when(self.sidebar_open, |this| this.child(self.sidebar(cx)))
                    .child(main)
                    .children(self.right_panel(window, cx)),
            )
            .children(self.settings_overlay(window, cx))
            .children(Root::render_dialog_layer(window, cx))
    }
}

/// The call at the front of the queue, and whether the approval card is up
/// for it rather than it running.
pub(crate) fn front_call(
    turn: &agent::Turn,
    asking: Option<&str>,
) -> Option<(backend::ToolCall, bool)> {
    // `Ask` shows whatever is at the front, running or not. Which of the two
    // it is was decided when the queue stopped on it: asking the settings
    // again here showed a waiting call as running once approvals were
    // switched off, and nothing ever ran it.
    match turn.next(|_| letronna_core::gate::Outcome::Ask) {
        agent::Next::AskUser(call) => {
            let waiting = asking == Some(call.id.as_str());
            Some((call, waiting))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::front_call;
    use letronna_core::agent::{Next, Turn};
    use letronna_core::chat::{Connection, ToolCall};
    use letronna_core::gate::{Gate, Grants, decide};
    use letronna_core::tools::ToolContext;

    /// A turn whose queue holds one `write_file` call.
    fn writing() -> Turn {
        let mut turn = Turn::new(Connection::default());
        turn.begin(vec![ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            arguments: "{}".into(),
        }]);
        turn
    }

    #[test]
    fn the_card_is_up_only_for_the_call_the_queue_stopped_on() {
        let mut turn = Turn::new(Connection::default());
        turn.begin(vec![ToolCall {
            id: "call-1".into(),
            name: "write_file".into(),
            arguments: "{}".into(),
        }]);
        assert!(
            front_call(&turn, Some("call-1"))
                .is_some_and(|(call, waiting)| call.id == "call-1" && waiting)
        );
        // Answered, or let run when the queue reached it.
        assert!(front_call(&turn, None).is_some_and(|(_, waiting)| !waiting));
        // Stopped on an earlier call, which has left the queue since.
        assert!(front_call(&turn, Some("call-0")).is_some_and(|(_, waiting)| !waiting));
        assert!(front_call(&Turn::new(Connection::default()), Some("call-1")).is_none());
    }

    #[test]
    fn a_call_that_was_answered_or_never_asked_about_has_no_card() {
        let turn = writing();
        assert!(front_call(&turn, None).is_some_and(|(_, waiting)| !waiting));
        assert!(front_call(&turn, Some("an-earlier-call")).is_some_and(|(_, waiting)| !waiting));
        assert!(front_call(&Turn::new(Connection::default()), Some("call-1")).is_none());
    }

    #[test]
    fn a_card_stays_up_when_approvals_are_switched_off_until_the_call_is_decided_again() {
        let turn = writing();
        let (ctx, none) = (ToolContext::default(), Grants::new());
        let decided = |skip_approvals| {
            let gate = Gate {
                ctx: &ctx,
                skip_approvals,
                grants: &none,
                human: true,
                rules: &[],
                broken: None,
            };
            turn.next(|call| decide(call, &gate))
        };
        let Next::AskUser(asked) = decided(false) else {
            panic!("write_file should wait for the user while approvals are on");
        };
        // The card keeps its buttons rather than showing a call as running
        // that nothing started.
        assert!(front_call(&turn, Some(&asked.id)).is_some_and(|(_, waiting)| waiting));
        // Deciding again with approvals off runs it.
        assert!(matches!(decided(true), Next::Run(_)));
    }
}
