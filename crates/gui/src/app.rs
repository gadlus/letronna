use crate::prelude::*;
use letronna_core::{agent, agents, approvals, goal, inbox, notify};

pub(crate) const BRAND: &str = "Letronna";

pub(crate) const GREETINGS: &[&str] = &[
    "Letronna is ready.",
    "Letronna is listening.",
    "What are we building today?",
    "Tell Letronna the goal.",
    "Ready when you are.",
];

actions!(
    letronna,
    [
        NewSession,
        Dismiss,
        ToggleSidebar,
        OpenSettings,
        SendNow,
        OpenActivity
    ]
);

/// How near the bottom still counts as at it: a line or two, so a reply
/// that grew a little since the last look is still followed.
const FOLLOW_SLACK: Pixels = px(48.);

/// The composer's own slash commands, without the slash, which win over any
/// the user, a project or a skill adds.
const OWN_COMMANDS: &[&str] = &["btw", "init"];

/// The chat column's widest, gutters included: about 90 characters of
/// text, which reads as one measure. A wider window gets wider margins
/// rather than longer lines.
pub(crate) const COLUMN: Pixels = px(816.);

/// How far past the top and bottom of the chat turns are measured ahead:
/// about a screen, so scrolling does not show a turn being sized, while a
/// long session is still never laid out whole.
const OVERDRAW: Pixels = px(800.);

/// Sessions kept with their messages read in, the one on screen among them.
/// Switching between a handful is instant; beyond that the least recently
/// opened is let go and read again from disk, a few milliseconds, when it is
/// next opened. Running, chat-app and scheduled sessions come on top.
const OPEN_SESSIONS: usize = 8;
/// How long after the system says a session file changed the folder is
/// looked at: one save is a few writes, and they are taken in together.
const SESSIONS_SETTLE: std::time::Duration = std::time::Duration::from_millis(200);

pub(crate) enum Verdict {
    Allow {
        remember: bool,
    },
    /// Allow, and keep a rule allowing calls like it in this project.
    AllowHere,
    /// With the reason the user gave, empty when they gave none.
    Deny(String),
    /// The reply to an `ask_user` question.
    Answer(String),
}

/// A side question asked with /btw, answered on a fork of its session and
/// shown under the conversation rather than in it.
pub(crate) struct Aside {
    /// The session it was asked about, the only one it shows under.
    pub(crate) session: u64,
    pub(crate) question: String,
    /// None while the fork works on it; the reply carries what it cost.
    pub(crate) answer: Option<Result<Message, String>>,
    pub(crate) cancel: letronna_core::cancel::Cancel,
    /// When it was asked, which tells its answer from an earlier one's.
    asked: std::time::Instant,
}

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum Page {
    Chat,
    Capabilities,
    Plugins,
    Messaging,
    Artifacts,
    Jobs,
    Activity,
}
impl Page {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Page::Chat => "Chat",
            Page::Capabilities => "Capabilities",
            Page::Plugins => "Plugins",
            Page::Messaging => "Messaging",
            Page::Artifacts => "Artifacts",
            Page::Jobs => "Scheduled jobs",
            Page::Activity => "Activity",
        }
    }
    pub(crate) fn blurb(self) -> &'static str {
        match self {
            Page::Chat => "",
            Page::Capabilities => "Skills, tools and MCP servers Letronna can reach.",
            Page::Plugins => "Skills, commands, styles and MCP servers that come as one.",
            Page::Messaging => "Message Letronna from a chat app, and get answers back there.",
            Page::Artifacts => "Files, images and links produced in sessions will collect here.",
            // Jobs renders its own page; this blurb is only a fallback.
            Page::Jobs => "Prompts that run on a timer.",
            Page::Activity => {
                "Helpers your agents handed part of their work to: what each is doing, and a way \
                 to read, steer or stop it."
            }
        }
    }
}

pub(crate) struct Chat {
    pub(crate) store: Store,
    pub(crate) composer: Entity<InputState>,
    pub(crate) search: Entity<InputState>,
    /// What the sidebar's search found in what was said.
    pub(crate) found: Vec<crate::ui::search::Found>,
    /// The search waiting for typing to pause, or running.
    pub(crate) finding: Option<Task<()>>,
    pub(crate) endpoint: Entity<InputState>,
    pub(crate) model: Entity<InputState>,
    /// The models to carry on with, written as a comma-separated list.
    pub(crate) fallbacks: Entity<InputState>,
    pub(crate) key: Entity<InputState>,
    /// The quick entry shortcut, as typed on the Shortcuts settings page.
    pub(crate) hotkey: Entity<InputState>,
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
    /// The Usage page's totals, added up when it was last opened.
    pub(crate) usage_report: Option<letronna_core::usage::Report>,
    /// The Usage page's insights over the last weeks, added up beside it.
    pub(crate) usage_insights: Option<letronna_core::insights::Insights>,
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
    /// The session the first-run setup page is drawn over: the empty chat
    /// with nothing reachable yet, or the one open when the account menu
    /// brought it back. Any other session shows its own conversation.
    pub(crate) show_setup: Option<u64>,
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
    /// Sessions running a schedule that decides without the user, in the
    /// schedule's mode. Keyed by session so the grant cannot reach the
    /// user's own chats: a job saying "do not ask me at 9am" is not
    /// permission to stop asking everywhere else.
    pub(crate) unattended: std::collections::HashMap<u64, letronna_core::gate::Mode>,
    /// Helpers' calls waiting on the user, oldest first, each shown as a card
    /// in its own session. One dropped unanswered is declined.
    pub(crate) agent_asks: std::collections::VecDeque<approvals::Request>,
    /// Text field on the `ask_user` card.
    pub(crate) answer: Entity<InputState>,
    /// Text field on a tool's card for why it was denied.
    pub(crate) deny_reason: Entity<InputState>,
    /// Whether the footer is already polling background jobs.
    pub(crate) job_tick: bool,
    /// Whether the message box holds nothing to send.
    pub(crate) composer_empty: bool,
    /// What the composer offers for a `/` or `@` being typed.
    pub(crate) completer: crate::ui::complete::Completer,
    /// The mode picked on each session's chip; a session not in here is in
    /// the Default mode setting.
    pub(crate) modes: std::collections::HashMap<u64, letronna_core::gate::Mode>,
    pub(crate) schedules: Vec<letronna_core::schedule::Schedule>,
    /// Id of the schedule being edited on the Scheduled jobs page.
    pub(crate) editing: Option<u64>,
    pub(crate) job_name: Entity<InputState>,
    pub(crate) job_prompt: Entity<InputState>,
    /// The name field on the New profile dialog.
    pub(crate) profile_name: Entity<InputState>,
    /// The active profile's name for the title bar, once there are several.
    pub(crate) profile_label: Option<String>,
    pub(crate) messaging: letronna_core::messaging::Config,
    /// The code the Messaging page shows for pairing a Telegram chat.
    pub(crate) pairing: Option<letronna_core::messaging::pairing::Code>,
    /// The code the Messaging page shows for pairing a Slack member.
    pub(crate) slack_code: Option<letronna_core::messaging::pairing::Code>,
    /// What the Slack listener last said it was doing; none while it is off.
    pub(crate) slack_state: Option<letronna_core::messaging::SlackState>,
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
    /// What keeps a chat app's typing indicator up while its turn runs.
    pub(crate) typing: Option<Task<()>>,
    /// Calls the turn in flight started that have not come back. The queue
    /// moves on only once none are, so a batch finishes before the next step.
    pub(crate) outstanding: usize,
    /// A remote chat that has been sent the card the turn is waiting on, and
    /// the call it is about. Its next message answers it.
    pub(crate) remote_waiting: Option<(
        letronna_core::messaging::Source,
        letronna_core::chat::ToolCall,
    )>,
    /// Messages with a voice note, each held while its chat is asked whether
    /// voice notes may be transcribed there. The chat's next words answer.
    pub(crate) voice_waiting: std::collections::HashMap<
        letronna_core::messaging::Source,
        letronna_core::messaging::Incoming,
    >,
    /// Sessions whose turn in flight answers a voice note.
    pub(crate) speak_back: std::collections::HashSet<u64>,
    /// What reads replies aloud, there only while the Voice setting is on, so
    /// the sound device is held open only then.
    pub(crate) voice: Option<letronna_core::speech::Voice>,
    pub(crate) messaging_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// The Slack listener's own off switch and the settings it runs with:
    /// reopening its socket costs a call Slack limits, so a save that leaves
    /// Slack alone leaves it running.
    pub(crate) slack_listener: Option<(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        letronna_core::messaging::Slack,
    )>,
    pub(crate) token_input: Entity<InputState>,
    pub(crate) webhook_input: Entity<InputState>,
    pub(crate) discord_token_input: Entity<InputState>,
    pub(crate) discord_owner_input: Entity<InputState>,
    pub(crate) discord_servers_input: Entity<InputState>,
    pub(crate) slack_app_input: Entity<InputState>,
    pub(crate) slack_bot_input: Entity<InputState>,
    /// The webhook's name, once it has been checked.
    pub(crate) hook_name: Option<String>,
    /// The connected bot's @name, once the token has been checked.
    pub(crate) bot_name: Option<String>,
    /// The Discord bot's name, once its token has been checked.
    pub(crate) discord_bot_name: Option<String>,
    /// The Slack app's bot name, once its tokens have been checked.
    pub(crate) slack_bot_name: Option<String>,
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
    /// An MCP server's prompt is being fetched for the line in the box, so
    /// Enter does not ask for another meanwhile.
    pub(crate) fetching_prompt: bool,
    /// Where to add skills from: a GitHub repository, a link or a path.
    pub(crate) skill_source: Entity<InputState>,
    /// Skills are being added, which waits on the network.
    pub(crate) adding_skills: bool,
    /// What the last add or removal of skills came to, or why it failed.
    pub(crate) skills_note: Option<Result<String, String>>,
    /// The skills the last add left out, each with why.
    pub(crate) skills_refused: Vec<String>,
    /// The Capabilities page's tabs beside Skills.
    pub(crate) capabilities: crate::ui::capabilities::Capabilities,
    /// The Plugins page: what is installed and the marketplaces.
    pub(crate) plugins: crate::ui::plugin_page::PluginsPage,
    /// The side question on show, if one is.
    pub(crate) aside: Option<Aside>,
    /// A failed turn set to be asked again once its provider's wait is over.
    pub(crate) waiting_retry: Option<crate::ui::messages::WaitingRetry>,
    /// Sessions a notice woke, marked in the sidebar until they are next on
    /// screen.
    pub(crate) unseen: std::collections::HashSet<u64>,
    /// What calls the user back while the window is hidden or behind others.
    pub(crate) notices: notify::Notices,
    /// Whether the window is the one in front, as it last said.
    pub(crate) in_front: bool,
    /// Sessions the window has said it kept its own copy of, when another
    /// Letronna wrote there too, so that is said once.
    pub(crate) kept_own: std::collections::HashSet<u64>,
    /// Set by Stop for the `finish` it causes, so stopping does not start a
    /// turn for notices in the same moment.
    pub(crate) stopping: bool,
    /// The helpers as last read, for the Activity page, the status bar's pill
    /// and the sidebar's dots.
    pub(crate) helpers: Vec<crate::ui::activity::Helper>,
    /// Whether the helpers are being read again on a timer.
    pub(crate) helper_tick: bool,
    /// The helper whose transcript the Activity page shows.
    pub(crate) viewing: Option<crate::ui::activity::Viewing>,
    /// The helper the steer box is open for, and the box.
    pub(crate) steering: Option<letronna_core::driver::AgentId>,
    pub(crate) steer: Entity<InputState>,
    /// The Activity page's, for the keyboard to land on.
    pub(crate) activity_focus: FocusHandle,
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
    /// The panel's Changes tab, and the project's git status.
    pub(crate) changes: crate::ui::changes::Changes,
    /// The browser the agent uses, shown beside the chat while it does, and
    /// the session whose browser it shows: each session has its own.
    pub(crate) browser: Entity<crate::ui::browser::BrowserView>,
    pub(crate) browser_for: u64,
    /// The Files tab's tree and preview.
    pub(crate) files: crate::ui::files::Files,
    /// The composer holds the draft of the session `draft_for`; the other
    /// sessions' drafts wait here, by session id.
    pub(crate) drafts: std::collections::HashMap<u64, String>,
    pub(crate) draft_for: u64,
    /// Pictures pasted, dropped or picked for each session's next message,
    /// saved already, by session id.
    pub(crate) attachments: std::collections::HashMap<u64, Vec<PathBuf>>,
    pub(crate) error: Option<String>,
    /// Where updating stands, for the About page and the status bar.
    pub(crate) updating: crate::ui::update::Updating,
    /// A passing note for the next time the window draws: a new version, or
    /// a turn stopped at its round cap.
    pub(crate) note: Option<String>,
    /// Hook failures to show once, the next time the window is drawn.
    pub(crate) hook_note: Option<String>,
    /// The session whose goal is being looked at after its turn, and what
    /// ends the look when the user stops or sends.
    pub(crate) goal_check: Option<(u64, letronna_core::cancel::Cancel)>,
    /// How many times this turn a Stop hook has sent the model back to work.
    pub(crate) stop_blocks: u32,
    /// The chat's turns, laid out only where they are on screen.
    pub(crate) list: ListState,
    /// The turns of the session on screen, as the list shows them.
    pub(crate) turns: Vec<crate::ui::actions::Span>,
    /// The session and the number of turns the list was last told of, so
    /// that only what changed since is measured again.
    pub(crate) listed: (u64, usize),
    /// How a session's loop ended, to say the next time the window draws.
    pub(crate) loop_note: Option<String>,
    /// Whether the chat keeps to its newest message as more arrives.
    pub(crate) following: bool,
    /// Where the chat was scrolled to at the last draw, to tell which way
    /// it has moved since.
    pub(crate) followed_offset: Pixels,
    /// Sessions with their messages read in, the one opened last first.
    pub(crate) open: std::collections::VecDeque<u64>,
    /// What the Artifacts page lists, read from the sessions on disk each
    /// time it opens; None while that is under way.
    pub(crate) artifacts: Option<Vec<letronna_core::store::Artifact>>,
    /// The session and message a search hit opened, which the chat is held
    /// at until it is scrolled by hand or another session is shown.
    pub(crate) reveal: Option<(u64, usize)>,
    /// Where holding it last put the chat, to tell a scroll by hand from it.
    pub(crate) revealed: Option<ListOffset>,
    /// Where the message it opened was last drawn, and whether the chat has
    /// yet to be moved for it since.
    pub(crate) hit_at: Option<(Pixels, bool)>,
    /// The list of modes, while the mode chip has it open.
    pub(crate) mode_menu: Option<Entity<gpui_component::menu::PopupMenu>>,
    /// Whether a frame has been drawn, before which the window has no focus
    /// path to ask about.
    pub(crate) drawn: bool,
    /// The mic button's own state: recording, or sending what it heard off
    /// to be transcribed. `None` the rest of the time; nothing runs then.
    pub(crate) mic: Option<crate::ui::voice::Voice>,
    pub(crate) _subscriptions: Vec<Subscription>,
}

impl Chat {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Sessions are separate files now, so a damaged one costs that
        // session rather than the history. Only listed: each is read in
        // when it is opened, so starting costs the same however much was
        // said before.
        let started = std::time::Instant::now();
        let (mut store, error) = (letronna_core::store::load_listing(), None);
        store.active = store
            .conversations
            .iter()
            .position(|c| c.id == store.settings.active_id)
            .unwrap_or(store.conversations.len().saturating_sub(1));
        letronna_core::diag::event(
            "listed",
            &[
                ("sessions", store.conversations.len().into()),
                ("ms", letronna_core::diag::millis(started)),
                ("rss", letronna_core::diag::rss_bytes().into()),
            ],
        );
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
        if store.settings.quick_entry_hotkey.is_empty() {
            store.settings.quick_entry_hotkey = crate::platform::hotkey::DEFAULT.into();
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
        let fallbacks = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("None. Such as gpt-5-mini, OpenRouter:deepseek/deepseek-chat")
                .default_value(store.settings.fallbacks.join(", "))
        });
        let key = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("API key")
                .masked(true)
                .default_value(saved_key)
        });
        let hotkey = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder(crate::platform::hotkey::DEFAULT)
                .default_value(store.settings.quick_entry_hotkey.clone())
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
        let deny_reason =
            cx.new(|cx| InputState::new(window, cx).placeholder("Why not? (optional)"));
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
        let slack_app_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("xapp-...")
                .default_value(letronna_core::messaging::load().slack.app_token)
        });
        let slack_bot_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("xoxb-...")
                .default_value(letronna_core::messaging::load().slack.bot_token)
        });
        let skill_name =
            cx.new(|cx| InputState::new(window, cx).placeholder("Review these changes"));
        let skill_prompt = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(3, 10)
                .placeholder("What should Letronna do?")
        });
        let plugins = crate::ui::plugin_page::PluginsPage::new(window, cx);
        let skill_source = cx.new(|cx| {
            InputState::new(window, cx).placeholder("GitHub repo, link to a SKILL.md, or a folder")
        });
        let job_prompt = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(2, 6)
                .placeholder("Run the tests and tell me what broke.")
        });
        let profile_name = cx.new(|cx| InputState::new(window, cx).placeholder("Work"));
        let edit_message = cx.new(|cx| {
            InputState::new(window, cx)
                .multi_line(true)
                .auto_grow(1, 12)
        });
        let steer =
            cx.new(|cx| InputState::new(window, cx).placeholder("What it should know or do now"));
        let (capabilities, filled) = crate::ui::capabilities::Capabilities::new(window, cx);
        let subscriptions = vec![
            filled,
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
                        this.update_completion(cx);
                    }
                    _ => {}
                }
            }),
            cx.subscribe_in(&search, window, |this, _, event: &InputEvent, _, cx| {
                if matches!(event, InputEvent::Change) {
                    this.find_in_messages(cx);
                }
                cx.notify();
            }),
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
            cx.subscribe_in(&deny_reason, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.deny_call(window, cx);
                }
            }),
            cx.subscribe_in(&skill_source, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.add_skills(window, cx);
                }
            }),
            cx.subscribe_in(&plugins.source, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.add_plugin(window, cx);
                }
            }),
            cx.subscribe_in(
                &plugins.market_source,
                window,
                |this, _, event, window, cx| {
                    if matches!(event, InputEvent::PressEnter { .. }) {
                        this.add_market(window, cx);
                    }
                },
            ),
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
            // Also on coming to the front, for a change the system did not
            // report, as on some network folders; and the user may have
            // committed or edited elsewhere, so the changes are read again.
            cx.observe_window_activation(window, |this, window, cx| {
                if window.is_window_active() {
                    this.follow_sessions(cx);
                    this.refresh_changes(cx);
                }
            }),
            cx.subscribe_in(&steer, window, |this, _, event, window, cx| {
                if matches!(event, InputEvent::PressEnter { .. }) {
                    this.send_steer(window, cx);
                }
            }),
            cx.observe_window_activation(window, |this, window, cx| {
                this.files_on_front(window, cx)
            }),
        ];
        // The message when the old skills.json will not move across, or the
        // skills cannot be read at all; the Skills tab shows it too.
        let workspace = store.conversations[store.active].workspace.as_deref();
        let moved = letronna_core::skills::migrate();
        let (skills, skills_error) = match letronna_core::skills::list(workspace) {
            Ok(skills) => (skills, moved.err()),
            Err(why) => (Vec::new(), Some(why)),
        };
        let draft_for = store.conversations[store.active].id;
        let account = letronna_core::auth::account();
        // Nothing reachable yet: no account, and no key or URL saved for a
        // gateway that needs one. Ollama needs neither, so it is not asked.
        let active = &store.conversations[store.active];
        let show_setup = crate::ui::setup::needed(
            account.is_some(),
            keys.values().any(|k| !k.trim().is_empty()),
            !store.settings.custom_endpoint.trim().is_empty(),
            active.messages.is_empty(),
        )
        .then_some(active.id);
        let mut this = Self {
            page: Page::Chat,
            show_setup,
            sidebar_open: true,
            greeting,
            account,
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
            found: Vec::new(),
            finding: None,
            endpoint,
            model,
            fallbacks,
            key,
            hotkey,
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
            usage_report: None,
            usage_insights: None,
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
            unattended: std::collections::HashMap::new(),
            agent_asks: std::collections::VecDeque::new(),
            answer,
            deny_reason,
            job_tick: false,
            composer_empty: true,
            completer: Default::default(),
            modes: std::collections::HashMap::new(),
            schedules: letronna_core::schedule::load(),
            editing: None,
            job_name,
            job_prompt,
            profile_name,
            profile_label: crate::ui::profiles::title_label(),
            messaging: letronna_core::messaging::load(),
            pairing: None,
            slack_code: None,
            slack_state: None,
            scheduled: std::collections::HashSet::new(),
            remote: std::collections::HashMap::new(),
            remote_session: std::collections::HashMap::new(),
            generation: 0,
            reading: None,
            typing: None,
            outstanding: 0,
            remote_waiting: None,
            messaging_stop: None,
            slack_listener: None,
            voice_waiting: std::collections::HashMap::new(),
            speak_back: std::collections::HashSet::new(),
            voice: None,
            token_input,
            webhook_input,
            discord_token_input,
            discord_owner_input,
            discord_servers_input,
            slack_app_input,
            slack_bot_input,
            hook_name: None,
            bot_name: None,
            discord_bot_name: None,
            slack_bot_name: None,
            channel: 0,
            skills,
            skills_error: skills_error.clone(),
            editing_skill: None,
            skill_name,
            skill_prompt,
            follow_ups: Vec::new(),
            returned_follow_ups: None,
            fetching_prompt: false,
            skill_source,
            adding_skills: false,
            skills_note: None,
            skills_refused: Vec::new(),
            capabilities,
            plugins,
            aside: None,
            waiting_retry: None,
            unseen: std::collections::HashSet::new(),
            notices: notify::Notices::new(Box::new(crate::platform::notify::desktop())),
            in_front: window.is_window_active(),
            kept_own: std::collections::HashSet::new(),
            stopping: false,
            helpers: Vec::new(),
            helper_tick: false,
            viewing: None,
            steering: None,
            steer,
            activity_focus: cx.focus_handle(),
            editing_message: None,
            edit_message,
            thinking_open: std::collections::HashSet::new(),
            video: None,
            video_frame: None,
            panel: Default::default(),
            changes: crate::ui::changes::Changes::new(window, cx),
            browser: cx.new(|cx| crate::ui::browser::BrowserView::new(window, cx)),
            browser_for: 0,
            files: crate::ui::files::Files::new(cx),
            drafts: std::collections::HashMap::new(),
            draft_for,
            attachments: std::collections::HashMap::new(),
            error: error
                .or(skills_error)
                .or_else(letronna_core::vault::trouble),
            updating: Default::default(),
            note: None,
            hook_note: None,
            goal_check: None,
            stop_blocks: 0,
            list: ListState::new(0, ListAlignment::Top, OVERDRAW),
            turns: Vec::new(),
            listed: (0, 0),
            loop_note: None,
            following: true,
            followed_offset: px(0.),
            open: std::collections::VecDeque::new(),
            artifacts: None,
            reveal: None,
            revealed: None,
            hit_at: None,
            mode_menu: None,
            drawn: false,
            mic: None,
            _subscriptions: subscriptions,
        };
        let widen = this.follow_browser_layout(window, cx);
        this._subscriptions.push(widen);
        let front = cx.observe_window_activation(window, |this, window, _| {
            this.in_front = window.is_window_active();
        });
        this._subscriptions.push(front);
        let point_comment = this.follow_point_comments(window, cx);
        this._subscriptions.push(point_comment);
        // The tab getting the keys is a moment the user may have committed
        // or edited elsewhere, like the window coming to the front.
        let focus = this.changes.focus.clone();
        let tab = cx.on_focus_in(&focus, window, |this, _, cx| this.refresh_changes(cx));
        this._subscriptions.push(tab);
        this.load_models(window, cx);
        // Directly, not through `focus_composer`: the window's root does
        // not exist yet to ask about dialogs, and there can be none.
        this.composer
            .update(cx, |input, cx| input.focus(window, cx));
        Self::watch_schedules(cx);
        Self::watch_updates(cx);
        Self::watch_inbox(cx);
        Self::watch_sessions(cx);
        Self::serve_approvals(cx);
        Self::watch_ended(cx);
        Self::watch_clicks(window, cx);
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

    /// A model and the gateway it is on, chosen together, for the session on
    /// screen and new sessions.
    pub(crate) fn choose_model(
        &mut self,
        gateway: String,
        id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.model
            .update(cx, |input, cx| input.set_value(id.clone(), window, cx));
        self.store.choose_model(self.store.active, &gateway, &id);
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
        for conversation in &mut self.store.conversations {
            if std::mem::take(&mut conversation.diverged) && self.kept_own.insert(conversation.id) {
                self.note = Some(format!(
                    "\"{}\" was also changed elsewhere, so this window kept its own copy.",
                    conversation.title
                ));
            }
        }
    }

    /// Take in what the terminal wrote to the sessions since the window last
    /// looked. The session a reply is running in, and one with a message
    /// being changed, are left until that is done.
    pub(crate) fn follow_sessions(&mut self, cx: &mut Context<Self>) {
        let replying = self.streaming_id;
        let editing = self.editing_message.map(|(id, _)| id);
        if self
            .store
            .follow(|id| Some(id) == replying || Some(id) == editing)
        {
            cx.notify();
        }
    }

    /// Read in the session on screen, off the UI thread, the first time it
    /// is shown since it was listed or let go of; and let go of the sessions
    /// opened longest ago.
    fn open_on_screen(&mut self, cx: &mut Context<Self>) {
        let id = self.store.conversations[self.store.active].id;
        if self.open.front() == Some(&id) {
            return;
        }
        let known = self.open.iter().position(|&o| o == id);
        if let Some(at) = known {
            self.open.remove(at);
        }
        self.open.push_front(id);
        self.let_go();
        if known.is_some() || !self.store.conversations[self.store.active].unfilled() {
            return;
        }
        let read = cx
            .background_executor()
            .spawn(async move { letronna_core::store::messages(id) });
        cx.spawn(async move |this, cx| {
            let Some(messages) = read.await else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                // Let go of again before the read came back: it stays out.
                let Some(ix) = this.store.index_of(id).filter(|_| this.open.contains(&id)) else {
                    return;
                };
                this.store.conversations[ix].filled_with(messages);
                cx.notify();
            });
        })
        .detach();
    }

    /// Read in session `ix` now, for what cannot wait for the read.
    pub(crate) fn fill_now(&mut self, ix: usize) {
        let conversation = &mut self.store.conversations[ix];
        if !conversation.unfilled() {
            return;
        }
        letronna_core::store::fill(conversation);
        // First in line to be let go of again, unless it is opened.
        let id = conversation.id;
        if !self.open.contains(&id) {
            self.open.push_back(id);
        }
    }

    /// Let go of the messages of the sessions opened longest ago, beyond
    /// `OPEN_SESSIONS`. Never the one on screen, nor one a reply, a chat app
    /// or a schedule is writing to: added to while let go of, it would hold
    /// only the new messages, and the next step would be sent those alone.
    fn let_go(&mut self) {
        let mut at = self.open.len();
        // The front is the session on screen.
        while self.open.len() > OPEN_SESSIONS && at > 1 {
            at -= 1;
            let id = self.open[at];
            let busy = self.streaming_id == Some(id)
                || self.scheduled.contains(&id)
                || self.remote.contains_key(&id)
                || self.remote_session.values().any(|&s| s == id);
            if busy {
                continue;
            }
            // Deleted since, it has nothing to let go of.
            let gone = self
                .store
                .index_of(id)
                .is_none_or(|ix| self.store.conversations[ix].unload());
            if gone {
                self.open.remove(at);
            }
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
        // `/loop` and `/heartbeat` are for Letronna, not the model, and are
        // taken while a reply runs too: `/loop stop` most of all.
        let (name, rest) = prompt
            .split_once(char::is_whitespace)
            .unwrap_or((prompt.as_str(), ""));
        let here = self.store.conversations[self.store.active].id;
        // A chat app's session is woken only by its next message, which
        // would carry the ticks, and what they did, into the chat.
        let done = if matches!(name, "/loop" | "/heartbeat")
            && self.remote_session.values().any(|&s| s == here)
        {
            Some(Err(
                "A chat app's session wakes only for its messages, so a loop there would never come round.".to_owned(),
            ))
        } else {
            letronna_core::wake::command(here, name, rest)
        };
        if let Some(done) = done {
            match done {
                Ok(said) => {
                    self.composer
                        .update(cx, |input, cx| input.set_value("", window, cx));
                    window.push_notification(
                        gpui_component::notification::Notification::info(said),
                        cx,
                    );
                }
                Err(why) => {
                    // Kept as typed, without the line Enter added.
                    self.composer
                        .update(cx, |input, cx| input.set_value(prompt, window, cx));
                    self.error = Some(why);
                }
            }
            cx.notify();
            return;
        }
        if prompt.starts_with("/mcp__") {
            return self.send_mcp_prompt(prompt, window, cx);
        }
        // Asked beside the session, so it waits for no reply to end.
        if let Some(question) = side_question(&prompt) {
            let question = question.to_owned();
            self.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
            return self.ask_aside(question, cx);
        }
        if let Some(args) = goal_command(&prompt) {
            let command = goal::parse(args);
            let ix = self.store.active;
            // A look at the goal being changed would act on the old one.
            let here = self.store.conversations[ix].id;
            if command != goal::Command::Show
                && let Some((_, check)) = self.goal_check.take_if(|(id, _)| *id == here)
            {
                check.set();
            }
            match goal::apply_command(&mut self.store.conversations[ix].goal, command) {
                Ok(said) => self.note = Some(said),
                Err(error) => self.error = Some(error),
            }
            self.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.save();
            cx.notify();
            return;
        }
        // An added command fills the composer, as a skill's Use does, so what
        // it gives is read before the next send sends it.
        match self.added_command(&prompt) {
            Some(Ok(given)) => {
                self.composer.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.insert(given, window, cx);
                });
                return;
            }
            Some(Err(error)) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
            None => {}
        }
        if self.busy && !prompt.is_empty() {
            return self.queue_follow_up(prompt, window, cx);
        }
        if self.busy {
            return;
        }
        let images = self.attached().to_vec();
        if prompt.is_empty() && images.is_empty() {
            self.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
            return;
        }
        if prompt.chars().count() > 12_000 {
            self.error = Some("Please keep each message under 12,000 characters.".into());
            cx.notify();
            return;
        }
        let Some(prompt) = self.prompt_for(prompt, cx) else {
            return;
        };
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        let session = self.store.conversations[self.store.active].id;
        if self.dispatch(prompt, images, None, cx).is_err() {
            self.open_settings(window, cx);
        } else {
            self.attachments.remove(&session);
        }
    }

    /// Fetch the MCP server's prompt that `typed` names, off the window's
    /// thread, with the line left in the box meanwhile. It is sent as though
    /// typed, or queued behind the answer being written, only while the box
    /// still holds that line in that session; otherwise, or when the fetch
    /// fails, a notice says so and the box is left as the user has it.
    fn send_mcp_prompt(&mut self, typed: String, window: &mut Window, cx: &mut Context<Self>) {
        if self.fetching_prompt {
            self.error = Some("Still waiting on an MCP server for the last prompt.".into());
            cx.notify();
            return;
        }
        self.fetching_prompt = true;
        // Without the line Enter added.
        self.composer
            .update(cx, |input, cx| input.set_value(typed.clone(), window, cx));
        let session = self.store.conversations[self.store.active].id;
        let (tx, rx) = async_channel::bounded(1);
        let line = typed.clone();
        std::thread::spawn(move || {
            let (command, rest) = line.split_once(char::is_whitespace).unwrap_or((&line, ""));
            let _ = tx.send_blocking(letronna_core::mcp::expand(command, rest));
        });
        cx.spawn_in(window, async move |this, cx| {
            let expanded = rx
                .recv()
                .await
                .unwrap_or_else(|_| Err("The MCP server's prompt could not be fetched.".into()));
            let _ = this.update_in(cx, |this, window, cx| {
                this.fetching_prompt = false;
                let here = this.store.conversations[this.store.active].id == session
                    && this.draft_for == session;
                let unchanged = this.composer.read(cx).value().trim() == typed;
                let command = typed.split_whitespace().next().unwrap_or_default();
                let refused = if !here || !unchanged {
                    Some(format!(
                        "{command} was not sent: its answer came after the box or the session changed."
                    ))
                } else if this.busy && this.streaming_id != Some(session) {
                    Some(
                        "Another session is working. Send this when it is done, or stop it first."
                            .into(),
                    )
                } else {
                    None
                };
                match (expanded, refused) {
                    (Err(why), _) | (Ok(_), Some(why)) => {
                        this.error = Some(why);
                        cx.notify();
                    }
                    (Ok(text), None) if this.busy => this.queue_follow_up(text, window, cx),
                    (Ok(text), None) => {
                        this.composer
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        // Pictures attached go with it, as with a typed message.
                        let images = this.attached().to_vec();
                        if this.dispatch(text, images, None, cx).is_err() {
                            this.open_settings(window, cx);
                        } else {
                            this.attachments.remove(&session);
                        }
                    }
                }
            });
        })
        .detach();
    }

    /// What `/name args` gives, or why it was refused, when the user, the
    /// project or a skill added a command by that name and the window has
    /// none of its own.
    fn added_command(&self, prompt: &str) -> Option<Result<String, String>> {
        use letronna_core::slash;
        let (name, args) = slash::split(prompt)?;
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .as_deref();
        let commands = slash::list(workspace, OWN_COMMANDS);
        slash::find(&commands, name).map(|command| slash::fill(command, args))
    }

    /// What sending `text` in the active session asks the model: `/init`
    /// becomes its prompt, or `None` with the reason shown when there is no
    /// project folder for it.
    pub(crate) fn prompt_for(&mut self, text: String, cx: &mut Context<Self>) -> Option<String> {
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .as_deref();
        match letronna_core::init::command(&text, workspace) {
            Some(Ok(prompt)) => Some(prompt.to_owned()),
            Some(Err(why)) => {
                self.error = Some(why);
                cx.notify();
                None
            }
            None => Some(text),
        }
    }

    /// Asks `question` about the active session on a fork of it, whose answer
    /// shows under the conversation and never goes into it.
    fn ask_aside(&mut self, question: String, cx: &mut Context<Self>) {
        let ix = self.store.active;
        self.fill_now(ix);
        let mut connection = match self.connection_for(ix, None) {
            Ok(connection) => connection,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        connection.workspace = self.store.conversations[ix].workspace.clone();
        let messages = self.store.conversations[ix].messages.clone();
        if let Some(earlier) = self.aside.take() {
            earlier.cancel.set();
        }
        let cancel = letronna_core::cancel::Cancel::default();
        let asked = std::time::Instant::now();
        self.aside = Some(Aside {
            session: connection.conversation,
            question: question.clone(),
            answer: None,
            cancel: cancel.clone(),
            asked,
        });
        let (sender, receiver) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let answer = letronna_core::agents::btw(&connection, messages, &question, cancel);
            let _ = sender.send_blocking(answer);
        });
        cx.spawn(async move |this, cx| {
            if let Ok(answer) = receiver.recv().await {
                let _ = this.update(cx, |this, cx| {
                    // One closed or asked again since has no place for it.
                    if let Some(aside) = this.aside.as_mut().filter(|a| a.asked == asked) {
                        aside.answer = Some(answer);
                        cx.notify();
                    }
                });
            }
        })
        .detach();
        self.follow();
        cx.notify();
    }

    /// Send a prompt in the active session. Split out of `send` so a schedule
    /// can fire one with no window in hand. `Err` means the gateway is not
    /// usable; the caller decides whether to open Settings about it.
    pub(crate) fn dispatch(
        &mut self,
        prompt: String,
        images: Vec<PathBuf>,
        remote: Option<letronna_core::chat::Remote>,
        cx: &mut Context<Self>,
    ) -> std::result::Result<(), ()> {
        // What is said goes after what was said before, and the model is
        // sent all of it.
        self.fill_now(self.store.active);
        let from_chat_app = remote.is_some();
        // Asking anything at all means the setup page has done its job.
        self.show_setup = None;
        let connection = match self.connection_for(self.store.active, remote) {
            Ok(connection) => connection,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return Err(());
            }
        };
        self.store.settings.endpoint = connection.endpoint.clone();
        self.store.pin(self.store.active);
        // What is still being said of the last answer is not wanted now, but
        // only the user asking again says so: a chat's or a schedule's turn
        // is not read aloud, and would cut off the answer they are hearing.
        self.match_voice();
        let session = self.store.conversations[self.store.active].id;
        let own = !self.remote.contains_key(&session) && !self.scheduled.contains(&session);
        if let (Some(voice), true) = (&mut self.voice, own) {
            voice.hush();
        }
        // A new message is the user's next step, whatever a look at the goal
        // was about to say. Only one turn runs at a time, so a look in
        // another session ends too, and says so rather than going quiet.
        if let Some((id, check)) = self.goal_check.take() {
            check.set();
            if id != self.store.conversations[self.store.active].id {
                self.note = Some(
                    "The goal check in another session was skipped: only one turn runs at \
                     a time. It runs again after that session's next turn."
                        .into(),
                );
            }
        }
        let conversation = &mut self.store.conversations[self.store.active];
        goal::interrupted(&mut conversation.goal);
        // Name an untitled session after its first prompt, but leave a name
        // that was set deliberately, such as a scheduled job's.
        if conversation.messages.is_empty()
            && (conversation.title.is_empty() || conversation.title == "New session")
        {
            let shown = letronna_core::init::typed(&prompt).unwrap_or(&prompt);
            conversation.title = shown.chars().take(40).collect();
        }
        conversation.updated = backend::now();
        // What came in meanwhile goes first, so the prompt is read last.
        conversation
            .messages
            .extend(inbox::fold(&inbox::take(conversation.id, true)));
        // Only what the user typed here: a chat app's message is someone
        // else's, and must not read the user's files by naming them, nor
        // hide its end from the window behind the marker.
        let content = if from_chat_app {
            letronna_core::references::inert(&prompt)
        } else {
            letronna_core::references::expand(&prompt, conversation.workspace.as_deref())
        };
        conversation.messages.push(Message {
            role: "user".into(),
            content,
            images,
            ..Default::default()
        });
        let target_id = conversation.id;
        let model = connection.model.clone();
        self.busy = true;
        self.streaming_id = Some(target_id);
        // What the session's kept helpers wake on from now on.
        agents::connected(&connection);
        // Off this thread, as it may read hooks.json; long before the model
        // asks for its first call.
        let conversation = connection.conversation;
        std::thread::spawn(move || letronna_core::hooks::new_turn(conversation));
        self.turn =
            Some(agent::Turn::new(connection).verifying(self.store.settings.verifies_edits()));
        // A browser closed during the last turn shows again in this one.
        self.browser.update(cx, |view, _| view.dismissed = false);
        self.page = Page::Chat;
        let convo_title = self.store.conversations[self.store.active].title.clone();
        plugins::emit(
            cx,
            plugins::AppEvent::ChatStarted {
                model,
                title: convo_title,
            },
        );
        self.error = None;
        self.save();
        self.follow();
        self.run_turn(cx);
        Ok(())
    }

    /// The quick entry bar's send: like [`Self::dispatch`], but window-free,
    /// since it runs from a window other than the one this `Chat` renders in.
    /// `new_session` opens a fresh conversation first; otherwise the prompt
    /// lands in whichever one was last active. `Err` is the reason for the
    /// bar to show, and leaves the sessions as they were.
    pub(crate) fn quick_dispatch(
        &mut self,
        prompt: String,
        images: Vec<PathBuf>,
        new_session: bool,
        cx: &mut Context<Self>,
    ) -> Result<(), String> {
        // A second turn started over a running one would replace its agent
        // loop; the composer queues a follow-up here, which the bar cannot.
        if self.busy {
            return Err("A reply is still running. Send this when it is done.".into());
        }
        if prompt.chars().count() > 12_000 {
            return Err("Please keep each message under 12,000 characters.".into());
        }
        let previous = self.store.active;
        if new_session {
            let workspace = self.store.conversations[previous].workspace.clone();
            self.store.conversations.push(Conversation {
                id: self.store.reserve_id(),
                title: "New session".into(),
                updated: backend::now(),
                workspace,
                ..Default::default()
            });
            self.store.active = self.store.conversations.len() - 1;
        }
        let sent = match self.prompt_for(prompt, cx) {
            Some(prompt) => self.dispatch(prompt, images, None, cx).is_ok(),
            None => false,
        };
        if sent {
            return Ok(());
        }
        // Shown in the bar, which stays open, not behind it in the window.
        let why = self
            .error
            .take()
            .unwrap_or_else(|| "That could not be sent.".into());
        if new_session {
            self.store.conversations.pop();
            self.store.active = previous;
        }
        cx.notify();
        Err(why)
    }

    /// A checked connection for a turn in the session at `ix`, on the gateway
    /// and model chosen now.
    pub(crate) fn connection_for(
        &self,
        ix: usize,
        remote: Option<letronna_core::chat::Remote>,
    ) -> std::result::Result<Connection, String> {
        // Each session keeps the model it was on, and the gateway with it.
        let (gateway, model) = self.store.model(ix);
        let saved = self
            .keys
            .get(key_slot(gateway))
            .cloned()
            .unwrap_or_default();
        let (endpoint, key) = resolve(gateway, &self.store.settings.custom_endpoint, &saved);
        let session = self.store.conversations[ix].id;
        let connection = Connection {
            endpoint,
            model: model.to_owned(),
            key,
            tools: true,
            workspace: None,
            plan: self.mode(session) == letronna_core::gate::Mode::Plan,
            effort: self.store.effort(ix).to_owned(),
            search: self.store.settings.search,
            // A schedule's run and a chat app's turn may have nobody watching.
            unattended: remote.is_some() || self.scheduled.contains(&session),
            remote,
            conversation: session,
            wrap_up: false,
        };
        backend::validate(&connection)?;
        Ok(connection)
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

    /// Wake an idle session whenever a notice arrives for one.
    fn watch_inbox(cx: &mut Context<Self>) {
        let doorbell = inbox::listen();
        cx.spawn(async move |this, cx| {
            while doorbell.recv().await.is_ok() {
                // A helper that ended rings too, and its card goes with it.
                let rang = this.update(cx, |this, cx| {
                    this.drop_ended_asks(cx);
                    this.wake_idle(cx)
                });
                if rang.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Follow the sessions whenever the system says a session file changed.
    /// Nothing runs between changes, so a window left alone, shown or not,
    /// costs nothing; where the system cannot watch the folder, coming to
    /// the front still catches up.
    fn watch_sessions(cx: &mut Context<Self>) {
        use ::notify::Watcher as _;
        let folder = letronna_core::store::sessions_dir();
        let _ = std::fs::create_dir_all(&folder);
        // One waiting is enough: the folder is looked at whole.
        let (tx, rx) = async_channel::bounded(1);
        let watcher =
            ::notify::recommended_watcher(move |event: ::notify::Result<::notify::Event>| {
                // Not the index, locks or temp files beside them.
                let session = event.is_ok_and(|event| {
                    event
                        .paths
                        .iter()
                        .any(|path| path.extension().is_some_and(|x| x == "jsonl"))
                });
                if session {
                    let _ = tx.try_send(());
                }
            });
        let Ok(mut watcher) = watcher else {
            return;
        };
        if watcher
            .watch(&folder, ::notify::RecursiveMode::NonRecursive)
            .is_err()
        {
            return;
        }
        cx.spawn(async move |this, cx| {
            // Watching stops when this is dropped.
            let _watcher = watcher;
            while rx.recv().await.is_ok() {
                cx.background_executor().timer(SESSIONS_SETTLE).await;
                let _ = rx.try_recv();
                if this
                    .update(cx, |this, cx| this.follow_sessions(cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Take helpers' approval requests for as long as the window is open.
    fn serve_approvals(cx: &mut Context<Self>) {
        let served = approvals::serve();
        let (tx, rx) = async_channel::unbounded();
        // `serve` hands back a blocking receiver, so a thread of its own
        // carries each request over rather than one of the executor's.
        std::thread::spawn(move || {
            while let Ok(request) = served.recv() {
                if tx.send_blocking(request).is_err() {
                    break;
                }
            }
        });
        cx.spawn(async move |this, cx| {
            while let Ok(request) = rx.recv().await {
                if this
                    .update(cx, |this, cx| this.on_agent_ask(request, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
    }

    /// Call the user back about the helpers and workflow runs that end.
    fn watch_ended(cx: &mut Context<Self>) {
        let ended = notify::listen();
        cx.spawn(async move |this, cx| {
            while let Ok(ended) = ended.recv().await {
                let told = this.update(cx, |this, _| {
                    this.tell(ended.session, notify::Why::Ended(&ended.what))
                });
                if told.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Bring the window back on the session a notification was about when
    /// it is clicked.
    fn watch_clicks(window: &mut Window, cx: &mut Context<Self>) {
        let clicks = crate::platform::notify::clicks();
        cx.spawn_in(window, async move |this, cx| {
            while let Ok(id) = clicks.recv().await {
                let opened = this.update_in(cx, |this, window, cx| this.open_told(id, window, cx));
                if opened.is_err() {
                    break;
                }
            }
        })
        .detach();
    }

    /// Show the window, on session `id` if it is still there.
    fn open_told(&mut self, id: u64, window: &mut Window, cx: &mut Context<Self>) {
        crate::platform::window::show(window);
        let Some(index) = self.store.index_of(id) else {
            return;
        };
        self.store.active = index;
        self.error = None;
        self.page = Page::Chat;
        self.follow();
        self.focus_composer(window, cx);
        self.save();
        cx.notify();
    }

    /// Call the user back about session `id`, when the window is hidden or
    /// behind others and they want to be. A chat app's session is answered
    /// in the chat, and a deleted one has nothing to show.
    fn tell(&mut self, id: u64, why: notify::Why) {
        let remote =
            self.remote.contains_key(&id) || self.remote_session.values().any(|&s| s == id);
        let Some(ix) = self.store.index_of(id).filter(|_| !remote) else {
            return;
        };
        let title = self.store.conversations[ix].title.clone();
        let (on, away) = (self.store.settings.notifications, !self.in_front);
        let now = std::time::Instant::now();
        self.notices
            .post(on, away, now, || notify::note(id, &title, why));
    }

    /// Call the user back about a turn in session `id` that ended by itself:
    /// with its answer, or with what went wrong when it has none.
    fn tell_ended_turn(&mut self, id: u64, scheduled: bool) {
        let answer = self
            .store
            .index_of(id)
            .and_then(|ix| self.store.conversations[ix].messages.last())
            .filter(|last| last.role == "assistant" && !last.content.is_empty())
            .map(|last| last.content.clone());
        let error = self.error.clone();
        let why = match (&answer, &error) {
            (Some(answer), _) if scheduled => notify::Why::Scheduled(answer),
            (Some(answer), _) => notify::Why::Finished(answer),
            (None, Some(error)) => notify::Why::Failed(error),
            (None, None) => notify::Why::Finished(""),
        };
        self.tell(id, why);
    }

    /// A helper's call waiting on a yes: answered at once where its session's
    /// rules already decide it, otherwise put up as a card in that session.
    fn on_agent_ask(&mut self, request: approvals::Request, cx: &mut Context<Self>) {
        // Core has one request out per run and tool at a time, so a card
        // still up for the same was given up on: its helper stopped waiting.
        self.agent_asks
            .retain(|queued| queued.run != request.run || queued.call.name != request.call.name);
        cx.notify();
        // With its session gone no card could show; dropped, it is declined.
        let Some(ix) = self.store.index_of(request.root) else {
            return;
        };
        // Its run may have ended before it got here, since the doorbell that
        // says so comes by another way.
        if run_ended(request.root, request.run) {
            return;
        }
        if let Some(decision) = self.decided(&request.call, request.root) {
            request.answer(decision);
            return;
        }
        // Nobody in a chat app sees a helper's card, and one waiting here
        // would hold up the chat's answer until someone came to the window.
        if self.remote_session.values().any(|&id| id == request.root) {
            return;
        }
        if !(self.page == Page::Chat && self.store.active == ix) {
            let title = &self.store.conversations[ix].title;
            self.error = Some(format!(
                "Helper {} in \"{title}\" is waiting for your answer.",
                request.label
            ));
        }
        let mut ctx = letronna_core::tools::ToolContext::default();
        ctx.conversation = request.root;
        ctx.workspace = self.store.conversations[ix].workspace.clone();
        letronna_core::hooks::card(ctx, &request.call.name);
        let why = notify::Why::Waiting(&request.call, Some(&request.label));
        self.tell(request.root, why);
        self.agent_asks.push_back(request);
    }

    /// What session `root`'s own rules make of a helper's `call`, or None
    /// when only the user can say: the gate the session itself goes through,
    /// in its mode and with what was allowed there.
    fn decided(&self, call: &backend::ToolCall, root: u64) -> Option<approvals::Decision> {
        let mut ctx = letronna_core::tools::ToolContext::default();
        ctx.conversation = root;
        ctx.workspace = self
            .store
            .index_of(root)
            .and_then(|ix| self.store.conversations[ix].workspace.clone());
        let gate = letronna_core::gate::Gate::for_window(
            &ctx,
            &self.session_allow,
            &self.rules,
            self.mode(root),
            self.unattended.get(&root).copied(),
        );
        match letronna_core::gate::decide(call, &gate) {
            letronna_core::gate::Outcome::Run => Some(approvals::Decision::Once),
            letronna_core::gate::Outcome::Refuse(why) => Some(approvals::Decision::Deny(why)),
            letronna_core::gate::Outcome::Ask => None,
        }
    }

    /// Answer the helpers' cards their sessions' rules now decide, after a
    /// rule changed under them.
    fn decide_agent_asks(&mut self, cx: &mut Context<Self>) {
        for request in std::mem::take(&mut self.agent_asks) {
            match self.decided(&request.call, request.root) {
                Some(decision) => request.answer(decision),
                None => self.agent_asks.push_back(request),
            }
        }
        cx.notify();
    }

    /// Take down the cards of helpers that have ended, since nothing waits
    /// for their answers any more.
    fn drop_ended_asks(&mut self, cx: &mut Context<Self>) {
        let before = self.agent_asks.len();
        self.agent_asks
            .retain(|request| !run_ended(request.root, request.run));
        if self.agent_asks.len() != before {
            cx.notify();
        }
    }

    /// The user answered helper `label`'s card for call `call_id` in session
    /// `root`, if it is still up.
    pub(crate) fn answer_agent(
        &mut self,
        root: u64,
        label: &str,
        call_id: &str,
        decision: approvals::Decision,
        cx: &mut Context<Self>,
    ) {
        let at = self.agent_asks.iter().position(|request| {
            request.root == root && request.label == label && request.call.id == call_id
        });
        if let Some(request) = at.and_then(|at| self.agent_asks.remove(at)) {
            request.answer(decision);
        }
        cx.notify();
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
        // The run is in the schedule's own mode, never the Default mode
        // setting; a schedule picking any mode, Auto among them, goes here.
        self.modes.insert(session, schedule.mode);
        // Nobody is necessarily watching, so a schedule that decides
        // without the user skips the approval cards that would otherwise
        // stall it -- but only in the session it just opened.
        if schedule.mode != letronna_core::gate::Mode::Default {
            self.unattended.insert(session, schedule.mode);
        }
        if self
            .dispatch(schedule.prompt.clone(), Vec::new(), None, cx)
            .is_err()
        {
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
        // Read aloud only for whoever is at the window: a chat's or a
        // schedule's run is answered where it came from.
        let aloud = self
            .streaming_id
            .is_some_and(|id| !self.remote.contains_key(&id) && !self.scheduled.contains(&id));
        if let (Some(voice), true) = (&mut self.voice, aloud) {
            match &event {
                Event::Delta(text) => voice.hear(text),
                Event::Finished(_) => voice.end(),
                _ => {}
            }
        }
        match event {
            Event::Finished(Err(error)) => {
                let kept = self.keep_failure(&error);
                let on_screen =
                    self.streaming_id == Some(self.store.conversations[self.store.active].id);
                // `finish` reports it to a schedule or a chat app, and holds
                // back what was queued, by this.
                self.error = Some(error.to_string());
                self.finish(cx);
                // Said where the turn failed, so not again as a toast; in a
                // session off screen the toast is all that says it happened.
                if kept && on_screen {
                    self.error = None;
                }
            }
            Event::Finished(Ok(())) => self.on_reply(cx),
            // Only the switch a failure caused: the requests after it go to
            // the same fallback until the cooldown ends, and say nothing new.
            Event::Switched(switch) if switch.after.is_some() => {
                self.note = Some(switch.to_string());
            }
            // Kept with the session, so a restart need not ask for it again.
            Event::Summarized { upto, text } => {
                if let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) {
                    self.store.conversations[ix].summarized(upto, text);
                }
            }
            _ => {}
        }
        cx.notify();
    }

    /// A reply finished: store it, then either run what it asked for or stop.
    fn on_reply(&mut self, cx: &mut Context<Self>) {
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        let model = turn.connection.model.clone();
        let mut reply = turn.take_reply(&model);
        let continuation = turn.continuation(&mut reply);
        let calls = reply.tool_calls.clone();
        // The session may have been switched or deleted while it streamed.
        let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) else {
            self.finish(cx);
            return;
        };
        let conversation = &mut self.store.conversations[ix];
        conversation.updated = backend::now();
        // A reply with nothing in it -- empty, or cut off inside a tool call
        // with no text before it -- leaves nothing to keep.
        if !reply.content.is_empty() || !calls.is_empty() {
            conversation.messages.push(reply);
        }
        match continuation {
            Ok(Some(nudge)) => {
                conversation.messages.push(nudge.notice);
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
        } else if let Some(note) = self.turn.as_ref().and_then(agent::Turn::capped) {
            // Past the limit on rounds; a Stop hook must not undo the limit.
            self.note = Some(note);
            self.finish(cx);
        } else {
            self.stop_hooks(cx);
        }
    }

    /// The model has finished: let the Stop hooks look, off the UI thread,
    /// then end the turn or send the model back to what one says is left.
    fn stop_hooks(&mut self, cx: &mut Context<Self>) {
        if self.stop_blocks >= letronna_core::hooks::STOP_BLOCKS {
            self.note = Some(letronna_core::hooks::STOP_CAPPED.into());
            return self.finish(cx);
        }
        if !letronna_core::hooks::wanted(letronna_core::hooks::Event::Stop) {
            return self.finish(cx);
        }
        let last = self
            .streaming_id
            .and_then(|id| self.store.index_of(id))
            .and_then(|ix| self.store.conversations[ix].messages.last())
            .filter(|message| message.role == "assistant")
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let again = self.stop_blocks > 0;
        let mut ctx = self.call_context();
        ctx.cancel = self.turn_stop.clone();
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(letronna_core::hooks::stop(&ctx, &last, again));
        });
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            if let Ok(feedback) = rx.recv().await {
                let _ = this.update(cx, |this, cx| {
                    if this.generation == generation {
                        this.after_stop_hooks(feedback, cx)
                    }
                });
            }
        })
        .detach();
    }

    /// Ends the turn, or sends the model back with what a Stop hook said, at
    /// most `STOP_BLOCKS` times in a row.
    fn after_stop_hooks(&mut self, feedback: Option<String>, cx: &mut Context<Self>) {
        self.show_hook_problems();
        let ix = self.streaming_id.and_then(|id| self.store.index_of(id));
        match (feedback, ix) {
            (Some(feedback), Some(ix)) => {
                self.stop_blocks += 1;
                self.store.conversations[ix].messages.push(Message {
                    role: "user".into(),
                    content: feedback,
                    folded: true,
                    ..Default::default()
                });
                self.save();
                self.run_turn(cx);
            }
            _ => self.finish(cx),
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

    /// Run the next queued calls, together where they may, or stop on one
    /// that needs the user.
    pub(crate) fn advance_tools(&mut self, cx: &mut Context<Self>) {
        // What comes after a batch waits until every call of it is back.
        if self.outstanding > 0 {
            return;
        }
        self.read_rules();
        let Some(turn) = self.turn.as_ref() else {
            return;
        };
        // A call the gate would not simply run ends the batch, and is dealt
        // with below once the batch is back.
        let batch = until_repeated_id(turn.next_batch(
            |call| self.decide(call, &self.rules) != letronna_core::gate::Outcome::Run,
            agent::MAX_BATCH,
        ));
        if !batch.is_empty() {
            self.asking = None;
            for call in batch {
                self.execute_call(call, false, cx);
            }
            cx.notify();
            return;
        }
        let next = turn.next(|call| self.decide(call, &self.rules));
        let asking = match &next {
            agent::Next::AskUser(call) => Some(call.id.clone()),
            _ => None,
        };
        let fresh = asking.is_some() && asking != self.asking;
        if asking != self.asking {
            self.offer = match &next {
                // Nothing to remember a sign-in by: each is asked.
                agent::Next::AskUser(call) => {
                    letronna_core::permissions::suggest(call, &self.call_context())
                        .filter(|_| !letronna_core::tools::asked_each_time(&call.name))
                }
                _ => None,
            };
            if let agent::Next::AskUser(call) = &next {
                // What holds every call for a yes, beside each card.
                if let Some(error) = letronna_core::hooks::trouble() {
                    self.hook_note = Some(error);
                }
                letronna_core::hooks::card(self.call_context(), &call.name);
            }
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
                if let Some(id) = self.streaming_id.filter(|_| fresh) {
                    self.tell(id, notify::Why::Waiting(&call, None));
                }
                self.ask_remote(call, cx);
                // The card shows only with its session on screen, and until
                // it is answered no other session can start, so say which one
                // is waiting. A chat app's session had it sent to the chat.
                let on_screen = self.page == Page::Chat
                    && self.streaming_id == Some(self.store.conversations[self.store.active].id);
                if let Some(ix) = self
                    .streaming_id
                    .filter(|id| !on_screen && !self.remote.contains_key(id))
                    .and_then(|id| self.store.index_of(id))
                {
                    let title = &self.store.conversations[ix].title;
                    self.error = Some(format!("\"{title}\" is waiting for your answer."));
                }
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
                    let session = conversation.id;
                    // The files the tools wrote are noted on these results,
                    // which are in the conversation only from here.
                    self.refresh_changes(cx);
                    let looped = self.turn.as_ref().and_then(agent::Turn::looping);
                    if let Some(why) = looped.map(str::to_owned) {
                        // An error, so a schedule's or chat's report says why.
                        self.error = Some(why);
                        self.save();
                        self.finish(cx);
                    // A hook's `continue: false`, shown with the results.
                    } else if letronna_core::hooks::halted(session) {
                        self.save();
                        self.finish(cx);
                    } else {
                        self.fold_and_ask(ix, cx);
                    }
                } else {
                    self.finish(cx);
                }
            }
        }
    }

    /// Ask the model again, with what came in meanwhile after the tool
    /// results or the reply cut short.
    fn fold_and_ask(&mut self, ix: usize, cx: &mut Context<Self>) {
        let conversation = &mut self.store.conversations[ix];
        // After the results, which have to follow the calls they answer, and
        // before what you typed, so yours is read last.
        conversation
            .messages
            .extend(inbox::fold(&inbox::take(conversation.id, false)));
        // What you sent while it worked goes in now, before the model is
        // asked again, so it reads it before going on, with its `@` paths
        // attached as a message sent on its own has them.
        let workspace = conversation.workspace.clone();
        conversation
            .messages
            .extend(self.follow_ups.drain(..).map(|text| Message {
                role: "user".into(),
                content: letronna_core::references::expand(&text, workspace.as_deref()),
                folded: true,
                ..Default::default()
            }));
        self.save();
        self.run_turn(cx);
    }

    /// Send what is queued now, cutting short a reply still coming in; a
    /// running tool is never stopped, and what is queued goes in when it
    /// returns.
    pub(crate) fn send_now(&mut self, cx: &mut Context<Self>) {
        let here = self.store.conversations[self.store.active].id;
        if self.streaming_id != Some(here)
            || self.follow_ups.is_empty()
            || !self.turn.as_ref().is_some_and(cuts_in)
        {
            return;
        }
        let Some(ix) = self.store.index_of(here) else {
            return;
        };
        let model = self.store.settings.model.clone();
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        // What the stream still sends belongs to the reply cut short, and is
        // dropped; dropping it also ends the stream.
        self.generation += 1;
        let reply = turn.take_reply(&model);
        // Kept as far as it got, so the model reads what it had said before
        // you cut in. With no words yet there is nothing to keep, and some
        // providers refuse an empty reply.
        if !reply.content.trim().is_empty() || !reply.tool_calls.is_empty() {
            self.store.conversations[ix].messages.push(reply);
        }
        self.fold_and_ask(ix, cx);
    }

    /// Run one call off the UI thread and continue the queue when it returns;
    /// `approved` when the user said yes to it on its card.
    fn execute_call(&mut self, call: backend::ToolCall, approved: bool, cx: &mut Context<Self>) {
        let mut ctx = self.call_context();
        ctx.approved = approved;
        ctx.cancel = self.turn_stop.clone();
        self.call_running = true;
        let (id, name) = (call.id.clone(), call.name.clone());
        let (tx, rx) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let _ = tx.send_blocking(letronna_core::tools::run(&call.name, &call.arguments, &ctx));
        });
        self.outstanding += 1;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            // A tool that panicked still comes back, or the rest of its
            // batch would wait for it forever.
            let result = rx
                .recv()
                .await
                .unwrap_or_else(|_| Err(format!("`{name}` stopped without finishing.")));
            let _ = this.update(cx, |this, cx| {
                if this.generation == generation {
                    this.record_ran(&id, result, cx)
                }
            });
        })
        .detach();
    }

    /// Record the outcome of a call that ran, and move on once the rest of
    /// its batch is back too.
    fn record_ran(&mut self, id: &str, result: Result<String, String>, cx: &mut Context<Self>) {
        self.outstanding = self.outstanding.saturating_sub(1);
        self.call_running = self.outstanding > 0;
        if let Some(turn) = self.turn.as_mut() {
            turn.finish_id(id, result);
        }
        self.files.changed();
        self.show_hook_problems();
        // The calls still running are drawn from the queue, which just lost one.
        cx.notify();
        self.advance_tools(cx);
    }

    /// Record the outcome of the current call and move on.
    pub(crate) fn record_result(&mut self, result: Result<String, String>, cx: &mut Context<Self>) {
        if let Some(turn) = self.turn.as_mut() {
            turn.finish_call(result);
        }
        self.files.changed();
        // A tool may have run git; the files it wrote are counted once its
        // results join the conversation.
        self.refresh_changes(cx);
        self.show_hook_problems();
        self.advance_tools(cx);
    }

    /// Hook failures since the last look, for a notice the next draw shows.
    fn show_hook_problems(&mut self) {
        let problems = letronna_core::hooks::problems();
        if problems.is_empty() {
            return;
        }
        // A few lines, so a burst of failures cannot fill the window; the
        // rest are counted.
        const LINES: usize = 3;
        let mut note = problems[..problems.len().min(LINES)].join("\n");
        if problems.len() > LINES {
            note.push_str(&format!("\n… and {} more", problems.len() - LINES));
        }
        self.hook_note = Some(note);
    }

    /// Stop any running poller and start one if messaging is set up. Called
    /// whenever the settings change, so the worker always matches them; the
    /// Slack listener is reopened only when its own settings changed.
    pub(crate) fn restart_messaging(&mut self, cx: &mut Context<Self>) {
        use std::sync::atomic::{AtomicBool, Ordering};
        if let Some(stop) = self.messaging_stop.take() {
            stop.store(true, Ordering::Relaxed);
        }
        let slack = self.messaging.slack.clone();
        let unchanged = slack.ready()
            && self
                .slack_listener
                .as_ref()
                .is_some_and(|(_, running)| *running == slack);
        if !unchanged && let Some((stop, _)) = self.slack_listener.take() {
            stop.store(true, Ordering::Relaxed);
            self.slack_state = None;
        }
        let open_slack = slack.ready() && !unchanged;
        let telegram = &self.messaging.telegram;
        let discord = &self.messaging.discord;
        if !telegram.ready() && !discord.bot_ready() && !open_slack {
            return;
        }
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let (tx, rx) = async_channel::unbounded();
        if telegram.ready() {
            letronna_core::messaging::poll(telegram.token.clone(), stop.clone(), tx.clone());
        }
        if discord.bot_ready() {
            letronna_core::messaging::discord_listen(discord.clone(), stop.clone(), tx.clone());
        }
        if open_slack {
            let slack_stop = std::sync::Arc::new(AtomicBool::new(false));
            let (state, states) = async_channel::unbounded();
            letronna_core::messaging::slack_listen(slack.clone(), slack_stop.clone(), tx, state);
            self.slack_listener = Some((slack_stop.clone(), slack));
            // The page shows what the listener says, not what the settings
            // hope: a refused token stops it.
            cx.spawn(async move |this, cx| {
                while let Ok(state) = states.recv().await {
                    let shown = this.update(cx, |this, cx| {
                        // One since replaced may still be winding down.
                        if !slack_stop.load(Ordering::Relaxed) {
                            this.slack_state = Some(state);
                            cx.notify();
                        }
                    });
                    if shown.is_err() {
                        break;
                    }
                }
            })
            .detach();
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
        self.tell_remote_card(source, text, None, cx);
    }

    /// Send to a chat, with Allow and Deny under it when `card` is the id of
    /// the call it asks about.
    fn tell_remote_card(
        &self,
        source: &letronna_core::messaging::Source,
        text: &str,
        card: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let config = self.messaging.clone();
        let source = source.clone();
        let text = text.to_owned();
        cx.background_executor()
            .spawn(async move {
                let _ = letronna_core::messaging::tell(&config, &source, &text, card.as_deref());
            })
            .detach();
    }

    /// Show `source` that its turn is at work, for as long as it runs and is
    /// not waiting on the chat. The service drops the indicator after a few
    /// seconds, so it is sent again on a timer that lives only as long as
    /// the turn, and a new turn's replaces it.
    fn keep_typing(&mut self, source: letronna_core::messaging::Source, cx: &mut Context<Self>) {
        let every = source.typing_every();
        self.typing = Some(cx.spawn(async move |this, cx| {
            loop {
                let working = this.update(cx, |this, _| {
                    let running = this
                        .streaming_id
                        .and_then(|id| this.remote.get(&id))
                        .is_some_and(|turn| *turn == source);
                    // A card sent is the chat's turn to answer.
                    let asking = this
                        .remote_waiting
                        .as_ref()
                        .is_some_and(|(waiting, _)| *waiting == source);
                    running.then(|| (!asking).then(|| this.messaging.clone()))
                });
                let Ok(Some(config)) = working else {
                    return;
                };
                if let Some(config) = config {
                    let source = source.clone();
                    cx.background_executor()
                        .spawn(async move {
                            let _ = letronna_core::messaging::typing(&config, &source);
                        })
                        .detach();
                }
                cx.background_executor().timer(every).await;
            }
        }));
    }

    /// A tap on a card's Allow or Deny. It answers the card only while that
    /// card is the one waiting, as a typed yes or no would; a tap on an
    /// older card, or a second tap, only loses its buttons.
    fn on_press(
        &mut self,
        source: letronna_core::messaging::Source,
        press: letronna_core::messaging::Press,
        cx: &mut Context<Self>,
    ) {
        let answers = self
            .remote_waiting
            .as_ref()
            .is_some_and(|(waiting, call)| *waiting == source && press.answers(&call.id));
        let config = self.messaging.clone();
        let allow = press.allow;
        cx.background_executor()
            .spawn(async move {
                let _ = letronna_core::messaging::settle(&config, &press, answers);
            })
            .detach();
        if answers {
            self.resolve_call(
                match allow {
                    true => Verdict::Allow { remember: false },
                    false => Verdict::Deny(String::new()),
                },
                cx,
            );
        }
    }

    /// A message arrived from a bot. A Telegram chat that is not paired is
    /// heard only as a try at the pairing code, and gets no reply unless it
    /// is right; Discord only ever delivers the owner's messages. Slack hands
    /// on a DM from anyone until a member is paired, heard the same way.
    fn on_remote(&mut self, message: letronna_core::messaging::Incoming, cx: &mut Context<Self>) {
        use letronna_core::gate::Mode;
        use letronna_core::messaging::{Source, pairing};
        let source = message.source.clone();
        // A tap answers only as the owner's own words would.
        let may_answer = pairing::may_answer(&self.messaging, &message);
        if let Some(press) = message.press {
            if may_answer {
                self.on_press(source, press, cx);
            }
            return;
        }
        if let Source::Telegram(chat) = source
            && self.messaging.telegram.chat != Some(chat)
        {
            let now = std::time::Instant::now();
            match pairing::pair(
                &mut self.pairing,
                &mut self.messaging.telegram,
                &message,
                now,
            ) {
                Some(pairing::Attempt::Paired) => {
                    // Saved without restarting the poller: a new one starts
                    // without the offset and could hear the code again, now
                    // as the paired chat's first message.
                    if let Err(error) = letronna_core::messaging::save(&self.messaging) {
                        self.error = Some(error);
                    }
                    self.tell_remote(
                        &source,
                        "Paired. Letronna answers this chat now, and only your answers \
                         count for what it asks here.",
                        cx,
                    );
                    cx.notify();
                }
                Some(_) => cx.notify(),
                None => {}
            }
            return;
        }
        // Slack hears only its paired member, so who may answer is who is
        // heard at all; before one is paired a DM is a try at the code.
        if let Source::Slack { .. } = source
            && !pairing::may_answer(&self.messaging, &message)
        {
            let now = std::time::Instant::now();
            match pairing::pair_slack(
                &mut self.slack_code,
                &mut self.messaging.slack,
                &message,
                now,
            ) {
                Some(pairing::Attempt::Paired) => {
                    // The listener hears only the member it started with.
                    if let Err(error) = letronna_core::messaging::save(&self.messaging) {
                        self.error = Some(error);
                    }
                    self.tell_remote(&source, "Paired. Send your message.", cx);
                    self.restart_messaging(cx);
                    cx.notify();
                }
                Some(_) => cx.notify(),
                None => {}
            }
            return;
        }
        // A thread reply that does not mention the app is a follow-up, or a
        // card's answer, only where that thread is a session already.
        if message.needs_session && !self.remote_session.contains_key(&source) {
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
        // In a group anyone may write, but only the owner's own words answer;
        // anyone else's go on as any message would while a turn runs.
        if let Some((waiting, call)) = self.remote_waiting.clone()
            && waiting == source
            && pairing::may_answer(&self.messaging, &message)
        {
            use letronna_core::messaging::{CardAnswer, card_answer};
            let verdict = match card_answer(&call.name, &call.arguments, &message.said) {
                CardAnswer::Answer(text) => Verdict::Answer(text),
                CardAnswer::Allow => Verdict::Allow { remember: false },
                CardAnswer::Deny(reason) => Verdict::Deny(reason),
            };
            self.resolve_call(verdict, cx);
            return;
        }
        use letronna_core::messaging::files;
        // The chat's answer to whether its voice notes are transcribed is
        // kept, and the message that raised it taken in again. A message
        // with a file in it is no answer; its own voice note asks anew.
        if message.files.is_empty()
            && let Some(held) = self.voice_waiting.remove(&source)
        {
            files::choose_voice(&mut self.messaging, &source, &message.said);
            if let Err(error) = letronna_core::messaging::save(&self.messaging) {
                self.error = Some(error);
            }
            return self.on_remote(held, cx);
        }
        if self.busy {
            // Slack takes a leading / as one of its own commands.
            let stop = match source {
                Source::Slack { .. } => "!stop",
                _ => "/stop",
            };
            let text = format!(
                "Still working on something. Send {stop} to cancel it, or send this again \
                 once it is done."
            );
            self.tell_remote(&source, &text, cx);
            return;
        }
        // Telegram sends /start when a chat first opens the bot.
        if matches!(message.said.trim(), "/new" | "/start") {
            self.remote_session.remove(&source);
            self.tell_remote(&source, "Started a new session.", cx);
            return;
        }
        let mut message = letronna_core::messaging::Incoming {
            source: source.clone(),
            ..message
        };
        // Fetched only now, from a chat allowed to talk here and with nothing
        // running, into the folder of the session they are for.
        if files::unfetched(&message) {
            let config = self.messaging.clone();
            let into = self.remote_folder(&message);
            return self.on_remote_after(move || files::fetch(&config, message, into), cx);
        }
        if files::unheard(&message) {
            match files::voice_choice(&self.messaging, &source) {
                None => return self.ask_voice(message, cx),
                Some(true) => return self.on_remote_after(move || files::hear(message, true), cx),
                Some(false) => message = files::hear(message, false),
            }
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
            Source::Slack { .. } => self.messaging.slack.allow_tools,
        };
        self.remote.insert(id, source.clone());
        // The message itself says it was a voice note: a flag kept per chat
        // would outlive a message held for its transcript or turned away
        // while busy, and give a later typed one a paid spoken answer.
        self.speak_back.remove(&id);
        if message.spoken {
            self.speak_back.insert(id);
        }
        // Nobody on the far end can answer an approval card, so unless the
        // user has said otherwise a remote message may look but not touch.
        // With tools it gets the Default mode setting, but never above
        // Default: whoever writes in the chat drives it, and the chip that
        // would say what runs unasked is not in front of them.
        let mode = if allow_tools {
            Mode::narrow(Mode::Default, self.store.settings.mode)
        } else {
            Mode::Plan
        };
        self.modes.insert(id, mode);
        let (prompt, pictures) = files::prompt(&message);
        if self
            .dispatch(prompt, pictures, Some(message.remote), cx)
            .is_ok()
        {
            self.keep_typing(source, cx);
        }
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
                        .take_while(|m| m.role != "user" || m.notice)
                        .find(|m| m.role == "assistant" && !m.content.is_empty())
                })
                .map(|m| m.content.clone())
                .unwrap_or_else(|| "That did not produce an answer.".to_owned()),
        };
        let made = self
            .store
            .index_of(id)
            .map(|ix| letronna_core::messaging::files::made(&self.store.conversations[ix].messages))
            .unwrap_or_default();
        // Files cannot go to Slack, so a voice note is only for the others.
        let voiced = self.speak_back.remove(&id)
            && self.store.settings.voice
            && self.error.is_none()
            && !matches!(source, letronna_core::messaging::Source::Slack { .. });
        if made.is_empty() && !voiced {
            return self.tell_remote(&source, &reply, cx);
        }
        // One task, so the pictures and sound the turn made follow the reply.
        let config = self.messaging.clone();
        cx.background_executor()
            .spawn(async move {
                let mut made = made;
                let note = voiced.then(|| letronna_core::speech::voice_note(&reply));
                if let Some(Ok(note)) = &note {
                    made.push(note.clone());
                }
                letronna_core::messaging::files::answer(&config, &source, &reply, &made);
                if let Some(Err(why)) = note {
                    let line = format!("Not read aloud: {why}");
                    let _ = letronna_core::messaging::tell(&config, &source, &line, None);
                }
            })
            .detach();
    }

    /// Do `work` on a chat's message off this thread -- fetch its files,
    /// transcribe its voice note -- and take in what it gives back as if it
    /// had just arrived, so it meets every check again.
    fn on_remote_after(
        &mut self,
        work: impl FnOnce() -> letronna_core::messaging::Incoming + Send + 'static,
        cx: &mut Context<Self>,
    ) {
        cx.spawn(async move |this, cx| {
            let message = cx.background_executor().spawn(async move { work() }).await;
            let _ = this.update(cx, |this, cx| this.on_remote(message, cx));
        })
        .detach();
    }

    /// The project a chat's files are saved in: its session's, or the one a
    /// new session would get. None where other people read the chat, whose
    /// files are not the user's to put there.
    fn remote_folder(&self, message: &letronna_core::messaging::Incoming) -> Option<PathBuf> {
        if message.remote.shared {
            return None;
        }
        let ix = self
            .remote_session
            .get(&message.source)
            .and_then(|id| self.store.index_of(*id))
            .unwrap_or(self.store.active);
        self.store.conversations[ix].workspace.clone()
    }

    /// Hold a message with a voice note and ask its chat, once, whether voice
    /// notes may be transcribed there. The price is looked up first, which
    /// takes the network.
    fn ask_voice(&mut self, message: letronna_core::messaging::Incoming, cx: &mut Context<Self>) {
        let source = message.source.clone();
        self.voice_waiting.insert(source.clone(), message);
        cx.spawn(async move |this, cx| {
            let question = cx
                .background_executor()
                .spawn(async { letronna_core::messaging::files::voice_question() })
                .await;
            let _ = this.update(cx, |this, cx| this.tell_remote(&source, &question, cx));
        })
        .detach();
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
            .and_then(|id| self.unattended.get(&id).copied());
        let mode = self
            .streaming_id
            .map_or(self.store.settings.mode, |id| self.mode(id));
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
            mode,
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
        // A question is answered in words; a tool is allowed or not.
        let card = (call.name != letronna_core::tools::ASK_USER).then(|| call.id.clone());
        self.tell_remote_card(&source, &text, card, cx);
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
            // The session leaves plan mode with it, so the next message
            // carries on out of plan mode too.
            if approved && let Some(id) = self.streaming_id {
                self.modes.insert(id, letronna_core::gate::Mode::Default);
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
            Verdict::Deny(reason) => {
                self.record_result(Err(letronna_core::tools::declined(&reason)), cx)
            }
            Verdict::Allow { remember } => {
                if remember && let Some(id) = self.streaming_id {
                    self.session_allow
                        .entry(id)
                        .or_default()
                        .insert(letronna_core::gate::grant(&call));
                    // Its helpers waiting on the same tool are allowed too.
                    self.decide_agent_asks(cx);
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

    /// The mode `session` is in: its schedule's while that runs, which the
    /// gate follows, then its chip's, then the Default mode setting.
    pub(crate) fn mode(&self, session: u64) -> letronna_core::gate::Mode {
        let running = self.unattended.get(&session);
        let picked = running.or(self.modes.get(&session)).copied();
        picked.unwrap_or(self.store.settings.mode)
    }

    /// The mode the session on screen is in.
    pub(crate) fn active_mode(&self) -> letronna_core::gate::Mode {
        let session = self.store.conversations.get(self.store.active);
        session.map_or(self.store.settings.mode, |session| self.mode(session.id))
    }

    /// Puts the session on screen in `mode`, then decides the call a card is
    /// up for again, so it runs or shows its refusal if the mode says so.
    pub(crate) fn set_mode(&mut self, mode: letronna_core::gate::Mode, cx: &mut Context<Self>) {
        let Some(session) = self.store.conversations.get(self.store.active) else {
            return;
        };
        self.modes.insert(session.id, mode);
        agents::set_plan(mode == letronna_core::gate::Mode::Plan);
        self.approvals_changed(cx);
        cx.notify();
    }

    /// Decide the call the card is up for again after an approval setting
    /// changed, so it runs at once if it no longer needs the user.
    pub(crate) fn approvals_changed(&mut self, cx: &mut Context<Self>) {
        self.decide_agent_asks(cx);
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
    pub(crate) fn distance_from_bottom(&self) -> Pixels {
        self.list.max_offset_for_scrollbar().height + self.list.scroll_px_offset_for_scrollbar().y
    }

    /// Go to the newest message and stay with it as more arrives.
    pub(crate) fn follow(&mut self) {
        self.following = true;
        self.reveal = None;
        self.revealed = None;
        self.hit_at = None;
        self.scroll_to_bottom();
    }

    /// Put the end of the last turn at the bottom of the chat.
    fn scroll_to_bottom(&self) {
        self.list.scroll_to(ListOffset {
            item_ix: self.list.item_count(),
            offset_in_item: px(0.),
        });
    }

    /// Group the session on screen into turns, tell the list which of them
    /// changed since the last draw, and keep to the bottom while following.
    fn lay_out_turns(&mut self) {
        let conversation = &self.store.conversations[self.store.active];
        let streaming_here = self.busy && self.streaming_id == Some(conversation.id);
        let turns = crate::ui::actions::turns(&conversation.messages, streaming_here);
        // The turns that start where they did. What the window says changes
        // at the end, but what another Letronna wrote there is taken in
        // whole, and can differ from any turn on.
        let kept = (self.turns.iter().zip(&turns))
            .take_while(|(was, now)| (was.first, was.yours) == (now.first, now.yours))
            .count();
        self.turns = turns;
        let (id, count) = (conversation.id, self.turns.len());
        let (listed, before) = self.listed;
        if listed != id {
            // Another session opens at its latest message, however it was
            // switched to, unless a search hit opened it. One more item
            // follows the turns, for what waits under them.
            self.list.reset(count + 1);
            self.following = !self.holding_hit();
        } else if before != count || kept < before {
            // Only those from the first that changed are measured again. One
            // that grows in place is on screen while it does, and what is
            // on screen is measured again at every draw.
            self.list.splice(kept..before, count - kept);
        }
        self.listed = (id, count);
        self.keep_following();
    }

    /// Turn `n` of the session on screen, as the list draws it. The turn a
    /// reply is coming in to also has the reply so far, the card it waits
    /// on and what was queued behind it.
    fn turn_row(
        &self,
        n: usize,
        column: Pixels,
        in_menu: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::ui::actions::shows_bar;
        let Some(&span) = self.turns.get(n) else {
            return self.under_turns(column, window, cx);
        };
        let conversation = &self.store.conversations[self.store.active];
        let messages = &conversation.messages;
        let streaming_here = self.busy && self.streaming_id == Some(conversation.id);
        // The turn a reply is still coming in to has no buttons or menu:
        // it is not finished, and it owns the session until it is.
        let live = streaming_here && n + 1 == self.turns.len();
        let focus = self.turn_focus(span.first, window, cx);
        let mut rows = Vec::new();
        // The message a search hit opened, marked on the first row drawn
        // from it on: a reply that only called tools has none of its own.
        let mut hit = self
            .reveal
            .filter(|&(id, at)| id == conversation.id && (span.first..=span.last).contains(&at));
        let said = messages.iter().enumerate();
        for (i, m) in said.skip(span.first).take(span.last + 1 - span.first) {
            let labeled = i == span.first;
            // A reply that only asked for tools has nothing to show of its
            // own; the tool rows speak for it. Kept only when it carries
            // the turn's label or visible thinking, so it adds no empty gap.
            let thinking = self.store.settings.thinking && !m.reasoning.is_empty();
            if m.role == "assistant"
                && m.content.is_empty()
                && !m.tool_calls.is_empty()
                && !labeled
                && !thinking
            {
                continue;
            }
            // A turn is finished at its last message, unless the reply to
            // it is still coming in.
            let ends = messages.get(i + 1).is_none_or(|next| next.role == "user")
                && !(streaming_here && i + 1 == messages.len());
            let bar = (!m.notice && (span.yours || ends)).then(|| {
                let focused = focus.contains_focused(window, cx) && !in_menu;
                let shown = shows_bar(span, messages.len(), streaming_here, focused);
                self.turn_bar(span, shown, cx)
            });
            let row = self.message_row(i, m, labeled, false, bar, window, cx);
            rows.push(if hit.take_if(|&mut (_, at)| i >= at).is_some() {
                self.mark_hit(row, cx)
            } else {
                row
            });
        }
        // Past the last row drawn, the last row stands for it.
        if let Some(row) = rows.pop_if(|_| hit.is_some()) {
            rows.push(self.mark_hit(row, cx));
        }
        if live {
            // While tools are queued the card below speaks for the turn;
            // showing a half-built reply as well would be two spinners.
            let idle = self.turn.as_ref().is_some_and(|turn| {
                let next = turn.next(|_| letronna_core::gate::Outcome::Run);
                matches!(next, agent::Next::Done)
            });
            if let Some(reply) = self.turn.as_ref().filter(|_| idle).map(|turn| Message {
                role: "assistant".into(),
                content: turn.live.content.clone(),
                model: turn.connection.model.clone(),
                reasoning: turn.live.reasoning.clone(),
                tokens: turn.live.tokens,
                per_second: turn.live.per_second,
                prompt_tokens: turn.live.prompt_tokens,
                cached_tokens: turn.live.cached_tokens,
                window: turn.live.window,
                ..Default::default()
            }) {
                // Keyed by the place the reply will have once it is stored,
                // so finishing does not start its markdown or its fade-in
                // over again. Labelled when it opens the turn.
                let opens = span.first == messages.len();
                rows.push(self.message_row(messages.len(), &reply, opens, true, None, window, cx));
            }
            if let Some(card) = self.approval_card(window, cx) {
                rows.push(card);
            }
            for (k, text) in self.follow_ups.iter().enumerate() {
                rows.push(self.queued_row(k, text, cx));
            }
        }
        // No min_w_0 or clipping here: both break list layout in the
        // markdown renderer, which sizes items against this width. Long
        // lines are held in by the row and code-block widths.
        let group = v_flex()
            .w_full()
            .gap_3()
            // A hairline before each of your prompts but the first parts the
            // exchanges, where a card used to.
            .when(span.yours && n > 0, |group| {
                group.border_t_1().border_color(cx.theme().border).pt_3()
            })
            .children(rows);
        // A notice has no menu either: it is no place to go back to.
        let body = if live || messages.get(span.first).is_some_and(|m| m.notice) {
            group.into_any_element()
        } else {
            // Its own id, so each turn's menu keeps its own state. Tab picks
            // a turn out; a press on it does not, or opening a tool's output
            // would take the keys from the composer (text inside still takes
            // focus to select).
            div()
                .id(self.row_id("turn", span.first))
                .w_full()
                .track_focus(&focus)
                .on_any_mouse_down(|_, window, _| window.prevent_default())
                .child(group.context_menu(self.turn_menu(span, cx)))
                .into_any_element()
        };
        h_flex()
            .w_full()
            .justify_center()
            .child(
                // The chat's margin above the first turn and below the last
                // is theirs: as the list's own padding, it is left out of the
                // scroll range the following works from.
                v_flex()
                    .w(column)
                    .flex_shrink_0()
                    .px_6()
                    .map(|this| if n == 0 { this.pt_6() } else { this.pt_3() })
                    .child(body),
            )
            .into_any_element()
    }

    /// The list's last item, under the turns: last, where the eye is, a
    /// helper's card and the answer to a question asked aside, which do not
    /// wait on a turn; and the chat's margin below the last turn.
    fn under_turns(
        &self,
        column: Pixels,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let session = self.store.conversations[self.store.active].id;
        let mut rows: Vec<AnyElement> = Vec::new();
        rows.extend(self.helper_card(session, window, cx));
        rows.extend(self.aside_row(session, window, cx));
        h_flex()
            .w_full()
            .justify_center()
            .child(
                v_flex()
                    .w(column)
                    .flex_shrink_0()
                    .px_6()
                    .gap_3()
                    .when(!rows.is_empty(), |this| {
                        if self.turns.is_empty() {
                            this.pt_6()
                        } else {
                            this.pt_3()
                        }
                    })
                    .pb_6()
                    .children(rows),
            )
            .into_any_element()
    }

    /// Stay at the bottom while following, and stop following once the user
    /// scrolls up to reread. Distance from the bottom alone cannot say which
    /// happened: a reply's markdown is laid out a moment after it arrives, so
    /// the bottom runs ahead of the view on its own. What the user does is
    /// move the view up and away from the bottom. Content that shrinks also
    /// moves it up, but pinned to the bottom, and that must not count.
    fn keep_following(&mut self) {
        let offset = self.list.scroll_px_offset_for_scrollbar().y;
        // Held at a search hit, the chat stays put. Where it is still counts,
        // so a scroll by hand after it lets go is told the right way round.
        if self.holding_hit() {
            self.followed_offset = offset;
            self.place_hit();
            return;
        }
        let near_bottom = self.distance_from_bottom() <= FOLLOW_SLACK;
        // The offset grows towards zero going up.
        if offset > self.followed_offset && !near_bottom {
            self.following = false;
        } else if offset < self.followed_offset && near_bottom {
            self.following = true;
        }
        self.followed_offset = offset;
        if self.following {
            self.scroll_to_bottom();
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
        // A look at the goal between turns stops as a turn does, but only
        // the look of the session on screen, where the Stop button shows.
        let on_screen = self.store.conversations[self.store.active].id;
        if let Some((id, check)) = self.goal_check.take_if(|(id, _)| *id == on_screen) {
            check.set();
            if let Some(ix) = self.store.index_of(id) {
                goal::interrupted(&mut self.store.conversations[ix].goal);
                self.save();
            }
            cx.notify();
        }
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
        if let Some(voice) = &mut self.voice {
            voice.hush();
        }
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
        self.stopping = true;
        self.finish(cx);
    }

    /// Save what a stopped turn got to, so the model hears next time what ran
    /// and what did not.
    fn keep_stopped(&mut self) {
        let running = std::mem::take(&mut self.call_running);
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        if running {
            turn.finish_call(Err(agent::STOPPED_WHILE_RUNNING.into()));
        }
        let model = turn.connection.model.clone();
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
        } else if let Some(aside) = self.aside_to_close() {
            aside.cancel.set();
            cx.notify();
        } else if self.page == Page::Activity && self.steering.is_some() {
            // Escape in the steer box backs out of it; the reply carries on.
            self.close_steer(window, cx);
        } else if self.page == Page::Activity && self.viewing.is_some() {
            // And out of a helper's transcript, back to the list.
            self.open_activity(window, cx);
        } else if self.busy && self.busy_elsewhere().is_none() {
            // Not a reply in another session, which nothing here shows. Ahead
            // of the sidebar's filter, which a saved choice would leave
            // swallowing the key that stops a running reply.
            self.stop_turn(cx);
        } else {
            self.clear_session_filter(cx);
        }
    }

    /// Takes the aside escape closes rather than stopping a reply, if any.
    fn aside_to_close(&mut self) -> Option<Aside> {
        let here = self.store.conversations[self.store.active].id;
        let replying = self.streaming_id.filter(|_| self.busy);
        self.aside
            .take_if(|aside| escape_closes_aside(aside.session, here, replying))
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        let stopped = std::mem::take(&mut self.stopping);
        let session = self.streaming_id;
        // Before the chat app is answered and the schedule reported, which
        // forget that the session was theirs.
        if let Some(id) = session.filter(|_| !stopped) {
            self.tell_ended_turn(id, self.scheduled.contains(&id));
        }
        self.answer_remote(cx);
        // The reply clears the indicator; nothing may raise it again.
        self.typing = None;
        self.report_scheduled(cx);
        // The session's loop goes round again from the turn that took its
        // tick, and a stop ends it.
        if let Some(id) = session {
            let answer = self
                .store
                .index_of(id)
                .and_then(|ix| self.store.conversations[ix].messages.last())
                .filter(|m| m.role == "assistant")
                .map_or("", |m| m.content.as_str());
            if let Some(said) = letronna_core::wake::turn_ended(id, (!stopped).then_some(answer)) {
                self.loop_note = Some(said);
            }
        }
        // The grant is for the run, not the session: a follow-up typed there
        // has someone watching, and a deleted session's id is handed out again.
        if let Some(id) = self.streaming_id {
            self.unattended.remove(&id);
        }
        self.busy = false;
        self.streaming_id = None;
        self.turn = None;
        // Calls a stop left running come back to a generation that is gone,
        // so the next turn must not wait for them.
        self.outstanding = 0;
        self.stop_blocks = 0;
        plugins::emit(cx, plugins::AppEvent::ChatIdle);
        // The reply may have changed files, or committed them.
        self.refresh_changes(cx);
        self.send_follow_ups(session, cx);
        if stopped && let Some(ix) = session.and_then(|id| self.store.index_of(id)) {
            goal::interrupted(&mut self.store.conversations[ix].goal);
            self.save();
        }
        // Stop means stop, and after a failure the next turn would most
        // likely fail too: what is waiting goes in at the next ring or prompt.
        // A loop's tick is the user's standing order, which a stop does not
        // hold back, or the loop would stall with no ring to come.
        let ticking = || {
            inbox::pending()
                .into_iter()
                .any(letronna_core::wake::waiting)
        };
        // A goal is looked at first, and the notices wait for its answer.
        if (!stopped || ticking())
            && self.error.is_none()
            && !session.is_some_and(|id| self.review_goal(id, cx))
        {
            self.wake_idle(cx);
        }
        if self.error.is_none()
            && let Some(why) = self.voice.as_ref().and_then(|v| v.trouble())
        {
            self.error = Some(format!("Not read aloud: {why}"));
        }
        cx.notify();
    }

    /// Open or close what reads replies aloud to match the Voice setting.
    pub(crate) fn match_voice(&mut self) {
        let wanted = self.store.settings.voice;
        if wanted == self.voice.is_some() {
            return;
        }
        self.voice =
            wanted.then(|| letronna_core::speech::Voice::start(crate::platform::audio::speaker()));
    }

    /// Looks at the goal of session `id`, whose turn ended by itself, off
    /// the UI thread, then carries on toward it or says why not. False when
    /// there is nothing to look at, or a message the user sent went first.
    fn review_goal(&mut self, id: u64, cx: &mut Context<Self>) -> bool {
        if self.busy {
            return false;
        }
        let Some(ix) = self.store.index_of(id) else {
            return false;
        };
        let Some(standing) = goal::active(&self.store.conversations[ix].goal) else {
            return false;
        };
        let mut connection = match self.connection_for(ix, None) {
            Ok(connection) => connection,
            Err(error) => {
                self.error = Some(error);
                return false;
            }
        };
        // Where the check command runs.
        connection.workspace = self.store.conversations[ix].workspace.clone();
        let reply = (self.store.conversations[ix].messages.iter())
            .rfind(|message| message.role == "assistant")
            .map(|message| message.content.clone())
            .unwrap_or_default();
        let cancel = letronna_core::cancel::Cancel::default();
        if let Some((_, earlier)) = self.goal_check.replace((id, cancel.clone())) {
            earlier.set();
        }
        let (sender, receiver) = async_channel::bounded(1);
        let (stop, judged) = (cancel.clone(), standing.clone());
        std::thread::spawn(move || {
            let _ = sender.send_blocking(goal::review(&judged, &connection, &reply, &stop));
        });
        cx.spawn(async move |this, cx| {
            if let Ok(step) = receiver.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.after_goal_review(id, &cancel, &standing, step, cx)
                });
            }
        })
        .detach();
        true
    }

    /// Acts on what the look at session `id`'s goal, `judged`, came to,
    /// unless the user stopped, sent something or changed the goal meanwhile.
    fn after_goal_review(
        &mut self,
        id: u64,
        cancel: &letronna_core::cancel::Cancel,
        judged: &goal::Goal,
        step: goal::Step,
        cx: &mut Context<Self>,
    ) {
        if cancel.is_set() {
            return;
        }
        self.goal_check = None;
        let Some(ix) = self.store.index_of(id).filter(|_| !self.busy) else {
            return cx.notify();
        };
        if !goal::settle(&mut self.store.conversations[ix].goal, judged, &step) {
            return self.wake_idle(cx);
        }
        self.save();
        match step {
            goal::Step::Continue { notice, message } => {
                self.note = Some(notice);
                self.start_in(id, goal::continuation(message), cx);
            }
            goal::Step::Done(notice)
            | goal::Step::Pause { notice, .. }
            | goal::Step::Deferred(notice) => {
                self.note = Some(notice);
                self.wake_idle(cx);
            }
            goal::Step::Cleared => self.wake_idle(cx),
        }
        cx.notify();
    }

    /// Start a turn for notices that came while nothing ran: in the session
    /// on screen if it has any, otherwise in the one waiting longest.
    fn wake_idle(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let on_screen = self.store.conversations[self.store.active].id;
        // A chat app hears only the answer to what it sent, so a turn started
        // in its session would answer nobody; its next message takes them.
        let wakes = |id| {
            self.store.index_of(id).is_some() && !self.remote_session.values().any(|&s| s == id)
        };
        let Some(id) = woken(on_screen, &inbox::pending(), wakes) else {
            return;
        };
        let Some(notice) = inbox::fold(&inbox::take(id, true)) else {
            return;
        };
        self.start_in(id, notice, cx);
    }

    /// Start a turn in a session with a notice, leaving the view where it is.
    fn start_in(&mut self, id: u64, notice: Message, cx: &mut Context<Self>) {
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        // Kept even when no turn can start, so the next prompt there reads
        // it rather than it being lost. After what was said before, which a
        // session let go of has to be read back for.
        self.fill_now(ix);
        let conversation = &mut self.store.conversations[ix];
        conversation.updated = backend::now();
        conversation.messages.push(notice);
        self.unseen.insert(id);
        self.save();
        let connection = match self.connection_for(ix, None) {
            Ok(connection) => connection,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.begin_turn(id, connection, cx);
    }

    /// Ask again in the session `id` where its last turn failed, carrying on
    /// from where it stopped rather than from its prompt, so tools that ran
    /// before the failure are not run again.
    pub(crate) fn retry_failed(&mut self, id: u64, cx: &mut Context<Self>) {
        self.waiting_retry = None;
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        // Something said or asked since makes the failure old news.
        if self.busy || !self.ends_failed(id) {
            cx.notify();
            return;
        }
        let connection = match self.connection_for(ix, None) {
            Ok(connection) => connection,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        if self.store.conversations[self.store.active].id == id {
            self.follow();
        } else {
            self.unseen.insert(id);
        }
        self.begin_turn(id, connection, cx);
    }

    /// Run a turn in the session `id` on what it already holds.
    fn begin_turn(&mut self, id: u64, connection: Connection, cx: &mut Context<Self>) {
        self.busy = true;
        self.streaming_id = Some(id);
        agents::connected(&connection);
        let conversation = connection.conversation;
        std::thread::spawn(move || letronna_core::hooks::new_turn(conversation));
        self.turn =
            Some(agent::Turn::new(connection).verifying(self.store.settings.verifies_edits()));
        self.error = None;
        self.run_turn(cx);
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
        } else if prompt == letronna_core::init::COMMAND {
            // Folded into a turn as it runs, it would reach the model as typed.
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
        let sent = self.dispatch(text.clone(), Vec::new(), None, cx).is_ok();
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
        let drawn = std::mem::replace(&mut self.drawn, true);
        // The panel shows the browser of the session on screen.
        let on_screen = self.store.conversations[self.store.active].id;
        if self.browser_for != on_screen {
            self.browser_for = on_screen;
            letronna_core::browser::show_conversation(on_screen);
        }
        // And the composer holds its draft, however the session was switched
        // to: a click, a schedule, or a message from a chat app.
        self.swap_draft(window, cx);
        // And the Changes tab and the status bar speak of its folder.
        self.keep_changes_current(cx);
        if self.page == Page::Chat {
            self.unseen.remove(&on_screen);
        }
        self.open_on_screen(cx);
        self.lay_out_turns();
        self.keep_files_current(window, cx);
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
        // Helpers work off the UI thread too, and a timer reads them while a
        // turn runs or one works.
        self.watch_helpers(cx);
        if let Some(note) = self.note.take() {
            window.push_notification(gpui_component::notification::Notification::info(note), cx);
        }
        if let Some(note) = self.loop_note.take() {
            window.push_notification(gpui_component::notification::Notification::info(note), cx);
        }
        if let Some(note) = self.hook_note.take() {
            window.push_notification(
                gpui_component::notification::Notification::warning(note),
                cx,
            );
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
        } else if self.page == Page::Capabilities {
            self.capabilities_page(window, cx).into_any_element()
        } else if self.page == Page::Plugins {
            self.plugins_page(cx).into_any_element()
        } else if self.page == Page::Messaging {
            self.messaging_page(cx).into_any_element()
        } else if self.page == Page::Activity {
            self.activity_page(window, cx)
        } else if self.page != Page::Chat {
            self.page_view(self.page, cx).into_any_element()
        } else if self.show_setup == Some(conversation.id) {
            self.setup_page(window, cx).into_any_element()
        } else if conversation.messages.is_empty()
            && !streaming_here
            // A helper's card needs the chat body even with nothing said.
            && !self.agent_asks.iter().any(|ask| ask.root == conversation.id)
        {
            if conversation.unfilled() {
                // Still being read in, which takes a moment: blank rather
                // than the greeting of a new session.
                div().flex_1().into_any_element()
            } else {
                self.empty_state(window, cx).into_any_element()
            }
        } else {
            // A menu a turn opened takes the focus from inside that turn. That
            // is not the turn being picked out: showing its full row would
            // take away the button the menu hangs from, and the menu with it.
            // Before the first frame is drawn there is no focus path, and
            // gpui panics if asked for one; no menu can be open yet anyway.
            let in_menu = drawn
                && window
                    .context_stack()
                    .iter()
                    .any(|context| context.contains("PopupMenu"));
            let row = cx.processor(move |this, n: usize, window, cx| {
                this.turn_row(n, column, in_menu, window, cx)
            });
            // The scrollbar must sit outside the list, or it scrolls away
            // with the content. The list spans the pane, so the bar is at
            // its edge and the wheel works over the margins too; only the
            // column each turn is drawn in is narrow.
            div()
                .relative()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .w_full()
                .child(list(self.list.clone(), row).size_full())
                .vertical_scrollbar(&self.list)
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
            // Around the page's focus, so its tab keys reach whatever in it
            // holds the keys.
            .when(self.page == Page::Capabilities, |this| {
                this.key_context("Capabilities")
            })
            .child(body)
            .when(self.page == Page::Chat, |this| {
                this.on_drop(cx.listener(|this, dropped: &ExternalPaths, window, cx| {
                    this.attach_files(dropped.paths().to_vec(), window, cx)
                }))
                .child(
                    v_flex()
                        .w(column)
                        .mx_auto()
                        .px_6()
                        .pb_2()
                        .children(self.busy_note(cx))
                        .children(self.todo_panel(cx))
                        .children(self.goal_line(cx))
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
                cx.listener(|this, _: &crate::ui::usage::OpenUsage, window, cx| {
                    this.open_usage(window, cx)
                }),
            )
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
            .on_action(
                cx.listener(|this, _: &crate::ui::files::FilesTab, window, cx| {
                    if !window.has_active_dialog(cx) && !this.settings_open {
                        this.open_files_tab(window, cx)
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::changes::ChangesTab, window, cx| {
                    if !window.has_active_dialog(cx) && !this.settings_open {
                        this.show_changes(window, cx)
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &OpenActivity, window, cx| this.open_activity(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::palette::OpenPalette, window, cx| {
                    this.open_palette(window, cx)
                }),
            )
            .on_action(cx.listener(
                |this, _: &crate::ui::capabilities::OpenCapabilities, window, cx| {
                    this.open_capabilities(window, cx)
                },
            ))
            .on_action(cx.listener(
                |this, _: &crate::ui::capabilities::NextCapabilityTab, window, cx| {
                    this.step_capability(1, window, cx)
                },
            ))
            .on_action(cx.listener(
                |this, _: &crate::ui::capabilities::PrevCapabilityTab, window, cx| {
                    this.step_capability(-1, window, cx)
                },
            ))
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

/// Whether the approval card is up for the call at the front of the queue,
/// as opposed to that call running or about to.
pub(crate) fn card_is_up(turn: &agent::Turn, asking: Option<&str>) -> bool {
    front_call(turn, asking).is_some_and(|(_, waiting)| waiting)
}

/// The session to wake, of those with notices waiting that `wakes` allows:
/// the one on screen, else the one waiting longest.
fn woken(on_screen: u64, pending: &[u64], wakes: impl Fn(u64) -> bool) -> Option<u64> {
    pending
        .iter()
        .copied()
        .filter(|&id| wakes(id))
        .min_by_key(|&id| id != on_screen)
}

/// Whether sending now cuts in at once, which it does only while a reply is
/// coming in and never while its tools run or wait on you.
pub(crate) fn cuts_in(turn: &agent::Turn) -> bool {
    matches!(
        turn.next(|_| letronna_core::gate::Outcome::Run),
        agent::Next::Done
    )
}

/// Whether the helper heading run `run` in session `root` has ended, which
/// stops every helper under it. Only helpers in the background are listed,
/// so the card of one a turn waits on stays until it is answered.
fn run_ended(root: u64, run: u64) -> bool {
    use letronna_core::tasks::{self, Key, State};
    tasks::list(root).iter().any(|task| {
        task.key == Key::Agent(run) && !matches!(task.state, State::Running | State::Idle)
    })
}

/// The calls of `batch` before the first whose id an earlier one shares.
fn until_repeated_id(mut batch: Vec<backend::ToolCall>) -> Vec<backend::ToolCall> {
    // Results come back by id, and nothing makes a reply's ids unique, so
    // two calls that share one would each have the other's result filed
    // under them. The later one waits for the next batch.
    let repeated =
        (1..batch.len()).find(|&at| batch[..at].iter().any(|call| call.id == batch[at].id));
    if let Some(at) = repeated {
        batch.truncate(at);
    }
    batch
}

/// The calls running while `outstanding` have not come back, in the order
/// the model made them.
pub(crate) fn running_calls(turn: &agent::Turn, outstanding: usize) -> Vec<backend::ToolCall> {
    // A batch is the leading run of the queue, and each call leaves the
    // queue as it comes back, so those still out are the first left.
    let batch = turn.next_batch(|_| false, outstanding);
    if !batch.is_empty() {
        return batch;
    }
    // A call that may not run beside others only ever runs alone, at the front.
    front_call(turn, None)
        .map(|(call, _)| call)
        .into_iter()
        .collect()
}

/// Whether escape closes an aside asked in session `asked` rather than
/// stopping a reply: only with that session on screen as `here`, and only
/// once `replying`, the session a reply runs in, is not that one.
fn escape_closes_aside(asked: u64, here: u64, replying: Option<u64>) -> bool {
    asked == here && replying != Some(here)
}

/// What follows `/goal` in `prompt`, when that is what it is.
fn goal_command(prompt: &str) -> Option<&str> {
    let rest = prompt.strip_prefix("/goal")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then_some(rest)
}

/// The question a /btw line asks, empty when it names none, or None for any
/// other line.
fn side_question(prompt: &str) -> Option<&str> {
    let rest = prompt.strip_prefix("/btw")?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

#[cfg(test)]
mod tests {
    use super::woken;
    use letronna_core::gate::Mode;
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{
        card_is_up, cuts_in, escape_closes_aside, front_call, run_ended, running_calls,
        side_question, until_repeated_id,
    };
    use letronna_core::agent::{MAX_BATCH, Next, Turn};
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
    fn a_queued_call_needs_you_only_while_its_card_is_up() {
        let turn = writing();
        // Queued, but running or about to, so nothing waits on the user.
        assert!(!card_is_up(&turn, None));
        assert!(card_is_up(&turn, Some("call-1")));
        assert!(!card_is_up(
            &Turn::new(Connection::default()),
            Some("call-1")
        ));
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
        let decided = |mode| {
            let gate = Gate {
                ctx: &ctx,
                mode,
                grants: &none,
                rules: &[],
                broken: None,
                human: true,
            };
            turn.next(|call| decide(call, &gate))
        };
        let Next::AskUser(asked) = decided(Mode::Default) else {
            panic!("write_file should wait for the user while approvals are on");
        };
        // The card keeps its buttons rather than showing a call as running
        // that nothing started.
        assert!(front_call(&turn, Some(&asked.id)).is_some_and(|(_, waiting)| waiting));
        // Deciding again with approvals off runs it.
        assert!(matches!(decided(Mode::Bypass), Next::Run(_)));
    }

    #[test]
    fn the_session_on_screen_is_woken_first() {
        let any = |_| true;
        assert_eq!(woken(7, &[3, 7, 5], any), Some(7));
        assert_eq!(woken(9, &[3, 7, 5], any), Some(3));
        assert_eq!(woken(3, &[3, 7, 5], |id| id != 3), Some(7));
        assert_eq!(woken(3, &[3], |id| id != 3), None);
        assert_eq!(woken(3, &[], any), None);
    }

    #[test]
    fn sending_now_cuts_into_a_reply_but_never_into_a_tool() {
        let mut turn = Turn::new(Connection::default());
        assert!(cuts_in(&turn), "the first reply is coming in");
        let call = ToolCall {
            id: "call-1".into(),
            name: "run_command".into(),
            arguments: "{}".into(),
        };
        assert!(turn.begin(vec![call]));
        assert!(!cuts_in(&turn), "its tool is running or waiting on you");
        turn.finish_call(Ok("done".into()));
        assert!(!cuts_in(&turn), "its result is on the way to the model");
        turn.take_results();
        assert!(cuts_in(&turn), "the next reply is coming in");
    }

    /// A call to `name` with its own id.
    fn call(id: &str, name: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
        }
    }

    #[test]
    fn calls_that_run_together_show_until_each_comes_back() {
        let reads = [
            call("a", "read_file"),
            call("b", "list_dir"),
            call("c", "search"),
        ];
        let write = call("d", "write_file");
        let mut turn = Turn::new(Connection::default());
        turn.begin([reads.to_vec(), vec![write.clone()]].concat());
        let batch = turn.next_batch(|_| false, MAX_BATCH);
        assert_eq!(batch, reads);
        assert_eq!(running_calls(&turn, batch.len()), reads);

        // The middle one comes back first; the other two still show, in order.
        turn.finish_id("b", Ok("listed".into()));
        assert_eq!(
            running_calls(&turn, 2),
            [reads[0].clone(), reads[2].clone()]
        );
        turn.finish_id("c", Ok("found".into()));
        turn.finish_id("a", Ok("read".into()));
        // The write never joined them, and shows alone once it runs.
        assert!(turn.next_batch(|_| false, MAX_BATCH).is_empty());
        assert_eq!(running_calls(&turn, 1), [write]);
        turn.finish_id("d", Ok("written".into()));

        // However they came back, the model reads the results in its own order.
        assert_eq!(turn.next(|_| letronna_core::gate::Outcome::Run), Next::Ask);
        let order: Vec<String> = turn
            .take_results()
            .into_iter()
            .map(|result| result.tool_call_id)
            .collect();
        assert_eq!(order, ["a", "b", "c", "d"]);
    }

    #[test]
    fn calls_that_share_an_id_run_one_after_the_other() {
        let first = ToolCall {
            arguments: r#"{"path":"x.rs"}"#.into(),
            ..call("same", "read_file")
        };
        let second = ToolCall {
            arguments: r#"{"path":"y.rs"}"#.into(),
            ..call("same", "read_file")
        };
        let mut turn = Turn::new(Connection::default());
        turn.begin(vec![first.clone(), second.clone(), call("c", "list_dir")]);
        let batch = until_repeated_id(turn.next_batch(|_| false, MAX_BATCH));
        assert_eq!(running_calls(&turn, batch.len()), batch);
        assert_eq!(batch, [first]);
        turn.finish_id("same", Ok("x".into()));

        // The second goes out once the first is back, with what follows it.
        let batch = until_repeated_id(turn.next_batch(|_| false, MAX_BATCH));
        assert_eq!(batch, [second, call("c", "list_dir")]);
        turn.finish_id("c", Ok("listed".into()));
        turn.finish_id("same", Ok("y".into()));
        assert_eq!(turn.next(|_| letronna_core::gate::Outcome::Run), Next::Ask);
        let results: Vec<(String, String)> = turn
            .take_results()
            .into_iter()
            .map(|result| (result.args, result.content))
            .collect();
        assert_eq!(
            results,
            [
                (r#"{"path":"x.rs"}"#.to_owned(), "x".to_owned()),
                (r#"{"path":"y.rs"}"#.to_owned(), "y".to_owned()),
                ("{}".to_owned(), "listed".to_owned()),
            ]
        );
    }

    #[test]
    fn a_card_goes_when_the_helper_heading_its_run_ends() {
        use letronna_core::tasks::{self, Key, State, Task};
        // Far from any session or helper id another test uses.
        let (root, run) = (9_101, (1 << 40) + 9_101);
        tasks::register(Task::new(Key::Agent(run), root, None, "helper"));
        assert!(!run_ended(root, run));
        tasks::finish(&Key::Agent(run), State::Stopped, None);
        assert!(run_ended(root, run));
        // A run with nothing listed, as one a turn waits on, never looks ended.
        assert!(!run_ended(root, run + 1));
    }

    #[test]
    fn escape_stops_the_reply_on_screen_before_it_closes_that_sessions_aside() {
        assert!(escape_closes_aside(1, 1, None));
        assert!(!escape_closes_aside(1, 1, Some(1)));
        // A reply running elsewhere is not the one on screen.
        assert!(escape_closes_aside(1, 1, Some(2)));
        // An aside of another session is hidden, so escape is not for it.
        assert!(!escape_closes_aside(1, 2, None));
    }

    #[test]
    fn only_a_btw_line_asks_a_side_question() {
        assert_eq!(
            side_question("/btw what did we decide?"),
            Some("what did we decide?")
        );
        assert_eq!(side_question("/btw"), Some(""));
        assert_eq!(side_question("/btwx"), None);
        assert_eq!(side_question("tell me /btw"), None);
    }
}
