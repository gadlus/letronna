use crate::prelude::*;
use letronna_core::{agent, agents, approvals, crew, goal, inbox, notify, review};

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
        OpenActivity,
        NextRegion,
        PreviousRegion,
        FocusComposer,
        ShowKeys,
        ToggleCrew,
        DismissCrew,
        Quit
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
/// How much of the way to the bottom the chat goes each frame as a reply
/// comes in, and the furthest it glides rather than going at once.
const EASE_STEP: f32 = 0.35;
const EASE_REACH: Pixels = px(600.);

/// How far past the top and bottom of the chat turns are measured ahead:
/// about a screen, so scrolling does not show a turn being sized, while a
/// long session is still never laid out whole.
const OVERDRAW: Pixels = px(800.);

/// Sessions kept with their messages read in, the one on screen among them.
/// Switching between a handful is instant; beyond that the least recently
/// opened is let go and read again from disk, a few milliseconds, when it is
/// next opened. Running and chat-app sessions come on top.
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
    /// Allow a memory save, kept where its card's answer put it.
    Keep(letronna_core::memory::Scope),
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

#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum Page {
    Chat,
    Capabilities,
    Plugins,
    Messaging,
    Artifacts,
    Jobs,
    Activity,
    Workflows,
    Wallet,
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
            Page::Workflows => "Workflows",
            Page::Wallet => "Wallet",
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
            // Workflows renders its own page too.
            Page::Workflows => "Runs of helpers Letronna started.",
            // Wallet renders its own page too.
            Page::Wallet => "The wallets Letronna pays from.",
        }
    }
}

pub(crate) struct Chat {
    pub(crate) store: Store,
    pub(crate) composer: Entity<TextareaState>,
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
    /// Days a session is kept untouched before it is deleted; empty for ever.
    pub(crate) keep_days: Entity<InputState>,
    /// Every model on every set-up gateway, as (gateway, model).
    pub(crate) models: Vec<(String, String)>,
    /// Which list the model menu shows: `chat`, or one of the media kinds.
    pub(crate) model_kind: &'static str,
    /// The hosted models that make images, speech and video, with prices.
    pub(crate) media_models: Vec<letronna_core::media::Offer>,
    /// The model picked for each media kind, as `media.json` keeps it.
    pub(crate) media_choices: std::collections::BTreeMap<String, String>,
    /// The media kind each session's messages are made into, from a model
    /// picked on that kind's tab; a session not in here chats.
    pub(crate) media_kinds: std::collections::HashMap<u64, &'static str>,
    /// The turn running makes what a media model was asked for, and ends
    /// with it rather than asking the chat model about it.
    pub(crate) media_turn: bool,
    pub(crate) model_open: bool,
    /// The provider the model picker is narrowed to; None shows them all.
    pub(crate) model_provider: Option<String>,
    pub(crate) model_search: Entity<InputState>,
    pub(crate) keys: std::collections::HashMap<String, String>,
    pub(crate) gateway_pick: Entity<String>,
    pub(crate) presence_pick: Entity<String>,
    pub(crate) settings_page: Entity<&'static str>,
    /// The Usage page's totals, added up when it was last opened.
    pub(crate) usage_report: Option<letronna_core::usage::Report>,
    /// The Usage page's insights over the last weeks, added up beside it.
    pub(crate) usage_insights: Option<letronna_core::insights::Insights>,
    /// What fills the active session's context, for its status-bar
    /// percentage's popover; added up when it was last opened, and `None`
    /// while it is closed.
    pub(crate) context_report: Option<letronna_core::chat::ContextReport>,
    /// Settings' Memory page, which keeps its own state.
    pub(crate) memory: Entity<crate::ui::memory::MemoryPage>,
    pub(crate) settings_open: bool,
    /// Held by the settings panel while it is open, so escape reaches it.
    pub(crate) settings_focus: FocusHandle,
    /// Where the keys were when settings opened, given back when it closes.
    pub(crate) settings_before: Option<FocusHandle>,
    /// Held by the pages without a composer, so the window's shortcuts still
    /// reach the chat there.
    pub(crate) page_focus: FocusHandle,
    pub(crate) settings_backdrop: Option<std::sync::Arc<RenderImage>>,
    pub(crate) grains: crate::ui::grain::Grains,
    pub(crate) settings_closing: bool,
    /// Settings is open, but waits out the frame read behind it.
    pub(crate) settings_reading: bool,
    pub(crate) rename: Entity<InputState>,
    pub(crate) page: Page,
    /// The session the first-run setup page is drawn over: the empty chat
    /// with nothing reachable yet, or the one open when the account menu
    /// brought it back. Any other session shows its own conversation.
    pub(crate) show_setup: Option<u64>,
    pub(crate) sidebar_open: bool,
    /// The sidebar, the panel and the chat's rows coming in.
    pub(crate) motion: crate::ui::motion::Moves,
    pub(crate) greeting: &'static str,
    /// Who is signed in, as `account.json` last said; shared with the terminal.
    pub(crate) account: Option<letronna_core::auth::Account>,
    /// What the last sign-in, refresh or sign-out had to say.
    pub(crate) account_note: Option<String>,
    /// The teams the account is in, for the Account page's billing choice.
    pub(crate) teams: Vec<letronna_core::auth::Team>,
    /// What the last check of Jev from Settings › Search found: how long it
    /// took to answer, or why it could not be used.
    pub(crate) jev_checked: Option<Result<std::time::Duration, String>>,
    /// Jev is being checked.
    pub(crate) jev_checking: bool,
    pub(crate) signing_in: bool,
    /// A messaging token is being checked with its service.
    pub(crate) connecting: bool,
    /// The sign-in page the browser was sent to, while the window waits on
    /// it, so it can be opened again or copied into another browser.
    pub(crate) sign_in_url: Option<String>,
    /// Ends the wait for the browser at once, for the Cancel button.
    pub(crate) sign_in_cancel: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// A provider's browser sign-in from the Accounts page, while it waits.
    pub(crate) provider_sign_in: Option<std::sync::Arc<letronna_core::providers::auth::SignIn>>,
    pub(crate) loading_models: bool,
    pub(crate) gateway_error: Option<String>,
    pub(crate) busy: bool,
    /// The sessions a summary the user asked for is being made of.
    pub(crate) compacting: std::collections::HashSet<u64>,
    /// The turn in flight, if any. Owns the streamed reply and the tool queue.
    pub(crate) turn: Option<agent::Turn>,
    /// Stops the tool the turn in flight is running, when set.
    pub(crate) turn_stop: letronna_core::cancel::Cancel,
    /// The session the last memory review looks back over, and what stops it
    /// when the next turn begins, the exchange it read is taken back or the
    /// user leaves the session.
    pub(crate) review: (u64, letronna_core::cancel::Cancel),
    /// Whether the call at the front of the turn is running, rather than
    /// waiting on its card; a stop answers the two differently.
    pub(crate) call_running: bool,
    /// The session id currently receiving a streamed reply, if any.
    pub(crate) streaming_id: Option<u64>,
    /// The id of the queued call the approval card is up for, decided once
    /// when the queue stopped on it.
    pub(crate) asking: Option<String>,
    /// What the card for the call in `asking` shows.
    pub(crate) card_for: Option<crate::ui::approval::CardFor>,
    /// The rule "Always allow" keeps for the call the card is up for, worked
    /// out once when the card came up rather than on every frame.
    pub(crate) offer: Option<letronna_core::permissions::Rule>,
    /// Why Auto mode's judge left the call the card is up for to the user.
    pub(crate) why: Option<String>,
    /// The call in front that the judge is deciding off the UI thread, and
    /// the number its verdict comes back with.
    pub(crate) judging: Option<(String, u64)>,
    /// How many calls have gone to the judge, which numbers their verdicts.
    pub(crate) judged: u64,
    /// What let each call in Auto mode run, by session, for the tag under
    /// its row; the judge's own are kept in the session instead.
    pub(crate) tags: std::collections::HashMap<u64, Vec<Judged>>,
    /// A call the judge blocked, for a toast at the next frame.
    pub(crate) blocked: Option<String>,
    /// Tools the user allowed for the rest of this run of the app, by the
    /// session they were allowed in: "stop asking" about a tool in one chat
    /// is not a yes to it in every other chat.
    pub(crate) session_allow: letronna_core::gate::Grants,
    /// The rules file error last shown, so that one is shown once.
    pub(crate) rules_trouble: Option<String>,
    /// The user's rules as last read.
    pub(crate) rules: Result<Vec<letronna_core::permissions::Rule>, String>,
    /// The user's and the project's rules files' modified times when `rules`
    /// was read, so that the queue moving reads them again only once one
    /// changed.
    pub(crate) rules_read: Option<[Option<std::time::SystemTime>; 2]>,
    /// Helpers' calls waiting on the user, oldest first, each shown as a card
    /// in its own session. One dropped unanswered is declined.
    pub(crate) agent_asks: std::collections::VecDeque<approvals::Request>,
    /// Text field on the `ask_user` card.
    pub(crate) answer: Entity<InputState>,
    /// Text field on a tool's card for why it was denied.
    pub(crate) deny_reason: Entity<InputState>,
    /// Whether the message box holds nothing to send.
    pub(crate) composer_empty: bool,
    /// What the composer offers for a `/` or `@` being typed.
    pub(crate) completer: crate::ui::complete::Completer,
    /// The mode picked on each session's chip; a session not in here is in
    /// the Default mode setting.
    pub(crate) modes: std::collections::HashMap<u64, letronna_core::gate::Mode>,
    /// Whether the composer's words ask for Crew with `@crew`, kept as they
    /// change so the chat redraws only when the chip comes or goes.
    pub(crate) crew_asked: bool,
    /// Whether that chip was dismissed, so the message goes without Crew;
    /// it comes back once the word is gone and typed again.
    pub(crate) crew_dismissed: bool,
    /// Runs the schedules while this Letronna holds their lock, and is where
    /// every change to them goes.
    pub(crate) schedule_runner: letronna_core::schedule::Runner,
    /// The schedules as the runner last read them, for the page to show.
    pub(crate) schedules: Vec<letronna_core::schedule::Schedule>,
    /// Id of the schedule being edited on the Scheduled jobs page.
    pub(crate) editing: Option<u64>,
    pub(crate) job_name: Entity<InputState>,
    pub(crate) job_prompt: Entity<TextareaState>,
    /// The name field on the New profile dialog.
    pub(crate) profile_name: Entity<InputState>,
    /// The active profile's name for the title bar, once there are several.
    pub(crate) profile_label: Option<String>,
    pub(crate) workflows: crate::ui::workflows::Workflows,
    pub(crate) messaging: letronna_core::messaging::Config,
    /// The code the Messaging page shows for pairing a Telegram chat.
    pub(crate) pairing: Option<letronna_core::messaging::pairing::Code>,
    /// The code the Messaging page shows for pairing a Slack member.
    pub(crate) slack_code: Option<letronna_core::messaging::pairing::Code>,
    /// What the Slack listener last said it was doing; none while it is off.
    pub(crate) slack_state: Option<letronna_core::messaging::SlackState>,
    /// Runs beside the chat by session: held ones the window started, and
    /// scheduled ones its runner started.
    pub(crate) runs: std::collections::BTreeMap<u64, crate::runs::Run>,
    /// Where each held run's thread sends its end.
    pub(crate) run_ends: async_channel::Sender<crate::runs::Finished>,
    /// Where each held run's thread tells its steps, as they happen.
    pub(crate) run_steps: async_channel::Sender<(u64, letronna_core::driver::Step)>,
    /// The session each chat app is talking in, and the card each was sent.
    pub(crate) remote: letronna_core::remote::Dispatcher,
    /// Bumped when a turn is stopped. The stream thread of the stopped turn
    /// cannot be interrupted, and a tool that does not listen to `turn_stop`
    /// runs on; what they send back carries the old number and is dropped.
    pub(crate) generation: u64,
    /// What reads the reply in flight. Kept rather than detached, so that
    /// Stop drops the receiver at once, which is how the worker learns to
    /// give up, even in the middle of a wait to try again.
    pub(crate) reading: Option<Task<()>>,
    /// What keeps each chat app's typing indicator up while its run goes,
    /// by the run's session.
    pub(crate) typing: std::collections::HashMap<u64, Task<()>>,
    /// Calls the turn in flight started that have not come back. The queue
    /// moves on only once none are, so a batch finishes before the next step.
    pub(crate) outstanding: usize,
    /// What reads replies aloud, there only while the Voice setting is on, so
    /// the sound device is held open only then.
    pub(crate) voice: Option<letronna_core::speech::Voice>,
    pub(crate) messaging_stop: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Who answers the chat apps instead of this window, such as a
    /// `letronna serve`, while it does.
    pub(crate) heard_elsewhere: Option<letronna_core::remote::Holder>,
    /// A thread is waiting for the chat apps to be let go, to take them over.
    pub(crate) waiting_to_hear: bool,
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
    pub(crate) skill_prompt: Entity<TextareaState>,
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
    /// The Wallet page's and Settings › Wallet's own state.
    pub(crate) wallet: crate::ui::wallet::WalletPage,
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
    /// The helpers as last read, for the Activity page, the strip over the
    /// composer, the status bar's pill and the sidebar's dots.
    pub(crate) helpers: Vec<crate::ui::activity::Helper>,
    /// Whether the helpers are being read again on a timer.
    pub(crate) helper_tick: bool,
    /// Whether the strip over the composer has its list open.
    pub(crate) running_open: bool,
    /// The helper whose transcript the Activity page shows.
    pub(crate) viewing: Option<crate::ui::activity::Viewing>,
    /// The helper the steer box is open for, and the box.
    pub(crate) steering: Option<letronna_core::driver::AgentId>,
    pub(crate) steer: Entity<InputState>,
    /// The Activity page's, for the keyboard to land on.
    pub(crate) activity_focus: FocusHandle,
    /// The sidebar's, the transcript's and the panel's, which F6 moves
    /// between with the composer's own.
    pub(crate) sidebar_focus: FocusHandle,
    pub(crate) transcript_focus: FocusHandle,
    pub(crate) panel_focus: FocusHandle,
    /// The question card's answers, which take letter keys, or the
    /// permission card, which takes Enter and Escape.
    pub(crate) question_focus: FocusHandle,
    /// The part of the window F6 last moved to, ringed while it keeps the
    /// focus; a click anywhere takes the ring away.
    pub(crate) region: Option<crate::ui::keys::Region>,
    /// The session the sidebar's keys are on, by id: an index would move
    /// the keys to another session when one before it was deleted.
    pub(crate) picked: Option<u64>,
    /// The turn the keys were sent to, by session and first message. Its
    /// focus lives only while the list draws it, so it is handed over as
    /// the turn is drawn: kept here until the next draw, then `focusing`
    /// through that draw alone, so a turn off screen does not take the
    /// keys later, when it is scrolled to.
    pub(crate) focus_turn: Option<(u64, usize)>,
    pub(crate) focusing: Option<(u64, usize)>,
    /// A turn's menu opened from the keyboard, by the turn's first message,
    /// with what closes it once the keys leave it.
    pub(crate) turn_menu: Option<(usize, Entity<gpui_component::menu::PopupMenu>, Subscription)>,
    /// Your message being changed in place, by session id and place.
    pub(crate) editing_message: Option<(u64, usize)>,
    /// The box a message is changed in.
    pub(crate) edit_message: Entity<TextareaState>,
    /// Thoughts and tool outputs shown open, by session id and place.
    pub(crate) thinking_open: std::collections::HashSet<(u64, usize)>,
    /// Runs of tool calls shown as their rows rather than one line, by
    /// session id and the place of the run's first call.
    pub(crate) runs_open: std::collections::HashSet<(u64, usize)>,
    /// How long the calls run here took, for their rows.
    pub(crate) call_times: crate::ui::messages::CallTimes,
    /// The files the newest finished turn changed, for the card after it,
    /// and the session and length last read for it, read or not yet.
    pub(crate) turn_files: Option<crate::ui::actions::TurnFiles>,
    pub(crate) turn_files_for: Option<(u64, usize)>,
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
    /// The Jobs tab's choice of job, its output and the Run line.
    pub(crate) jobs: crate::ui::jobs::Jobs,
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
    /// When the chat on screen saves to memory and where, while the memory
    /// chip has its menu open.
    pub(crate) memory_menu: Option<Entity<gpui_component::menu::PopupMenu>>,
    /// Whether a frame has been drawn, before which the window has no focus
    /// path to ask about.
    pub(crate) drawn: bool,
    /// The mic button's own state: recording, or sending what it heard off
    /// to be transcribed. `None` the rest of the time; nothing runs then.
    pub(crate) mic: Option<crate::ui::voice::Voice>,
    /// The key that answers a permission card while it is held down, and
    /// whether its press answered one: its repeats never do.
    pub(crate) card_key: Option<(crate::ui::approval::CardKey, bool)>,
    pub(crate) _subscriptions: Vec<Subscription>,
    /// The sessions shown in this window, by id, the one shown last first,
    /// for the recent-session switcher; kept in memory only.
    pub(crate) recent: Vec<u64>,
    /// The recent-session switcher, while it shows.
    pub(crate) switcher: Option<crate::ui::switcher::Switcher>,
    /// Previews shown whole -- a tool's output, a long prompt, a reply's
    /// fences -- by session id and place.
    pub(crate) previews_open: std::collections::HashSet<(u64, usize)>,
    /// How long each reply thought, by session id and place, timed as it
    /// came in; a reply from before has none.
    pub(crate) thought_for: std::collections::HashMap<(u64, usize), std::time::Duration>,
    /// The reply thinking now, by session id and place, and since when.
    pub(crate) thinking_since: Option<((u64, usize), std::time::Instant)>,
    /// What each edit call changed, by session id and place, worked out
    /// once from its arguments rather than at every frame.
    pub(crate) edited: std::collections::HashMap<(u64, usize), crate::ui::messages::Edited>,
    /// How wide the chat's column was last drawn, its padding included,
    /// to tell how many rows a long line of a preview wraps to.
    pub(crate) column: Pixels,
    /// The line of the strip over the composer, which F6 and Tab stop at.
    pub(crate) strip_focus: FocusHandle,
    /// The whole strip, which holds the keys while its line or a row has them.
    pub(crate) strip_area: FocusHandle,
    /// Set by the key that shows what is running, for the strip's first row
    /// to take the keys as it is drawn, since it has no focus before then.
    pub(crate) strip_focus_first: bool,
    /// Sessions whose plan's list was closed by hand, which then rests as the
    /// strip's one line rather than opening by itself.
    pub(crate) plan_folded: std::collections::HashSet<u64>,
}

impl Chat {
    pub(crate) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Sessions are separate files now, so a damaged one costs that
        // session rather than the history. Only listed: each is read in
        // when it is opened, so starting costs the same however much was
        // said before.
        letronna_core::store::prune_old(&[]);
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
            store.settings.gateway = gateways()[0].name.into();
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
            TextareaState::new(window, cx)
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
        let keep_days = cx.new(|cx| {
            let days = store.settings.keep_days;
            InputState::new(window, cx)
                .placeholder("Never")
                .default_value(if days == 0 {
                    String::new()
                } else {
                    days.to_string()
                })
        });
        let model_search = cx.new(|cx| InputState::new(window, cx).placeholder("Search models…"));
        let gateway_pick = cx.new(|_| store.settings.gateway.clone());
        let presence_pick = cx.new(|_| store.settings.presence.clone());
        let settings_page = cx.new(|_| "Accounts");
        let memory = cx.new(|cx| crate::ui::memory::MemoryPage::new(window, cx));
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
            TextareaState::new(window, cx)
                .auto_grow(3, 10)
                .placeholder("What should Letronna do?")
        });
        let plugins = crate::ui::plugin_page::PluginsPage::new(window, cx);
        let skill_source = cx.new(|cx| {
            InputState::new(window, cx).placeholder("GitHub repo, link to a SKILL.md, or a folder")
        });
        let job_prompt = cx.new(|cx| {
            TextareaState::new(window, cx)
                .auto_grow(2, 6)
                .placeholder("Run the tests and tell me what broke.")
        });
        let profile_name = cx.new(|cx| InputState::new(window, cx).placeholder("Work"));
        let edit_message = cx.new(|cx| TextareaState::new(window, cx).auto_grow(1, 12));
        let steer =
            cx.new(|cx| InputState::new(window, cx).placeholder("What it should know or do now"));
        let (capabilities, filled) = crate::ui::capabilities::Capabilities::new(window, cx);
        let wallet = crate::ui::wallet::WalletPage::new(window, cx);
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
                    InputEvent::PressEnter {
                        secondary: false, ..
                    } => this.send(window, cx),
                    // Send lights up once there is something to send. Redraw
                    // only when that flips, not on every key.
                    InputEvent::Change => {
                        let empty = input.read(cx).value().trim().is_empty();
                        if empty != this.composer_empty {
                            this.composer_empty = empty;
                            cx.notify();
                        }
                        this.update_completion(cx);
                        this.crew_typed(cx);
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
                if matches!(
                    event,
                    InputEvent::PressEnter {
                        secondary: false,
                        ..
                    }
                ) {
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
                    crate::theme::apply(&this.store.settings.appearance, window, cx);
                    // As when the theme is picked: the blurred snapshot behind
                    // settings wears the old colours.
                    this.retake_backdrop(window, cx);
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
        let (schedule_runner, schedule_events) = letronna_core::schedule::Runner::start();
        // The message when the old skills.json will not move across, or the
        // skills cannot be read at all; the Skills tab shows it too.
        let workspace = store.conversations[store.active].workspace.as_deref();
        let moved = letronna_core::skills::migrate();
        let (skills, skills_error) = match letronna_core::skills::list(workspace) {
            Ok(skills) => (skills, moved.err()),
            Err(why) => (Vec::new(), Some(why)),
        };
        let draft_for = store.conversations[store.active].id;
        let (run_ends, ended_runs) = async_channel::unbounded();
        let (run_steps, run_stepped) = async_channel::unbounded();
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
            motion: Default::default(),
            greeting,
            account,
            account_note: None,
            teams: Vec::new(),
            jev_checked: None,
            jev_checking: false,
            signing_in: false,
            connecting: false,
            sign_in_url: None,
            sign_in_cancel: None,
            provider_sign_in: None,
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
            keep_days,
            models: Vec::new(),
            model_kind: "chat",
            media_models: Vec::new(),
            media_choices: letronna_core::media::choices(),
            media_kinds: std::collections::HashMap::new(),
            media_turn: false,
            model_open: false,
            model_provider: None,
            model_search,
            keys,
            gateway_pick,
            presence_pick,
            settings_page,
            usage_report: None,
            usage_insights: None,
            context_report: None,
            memory,
            settings_open: false,
            settings_focus: cx.focus_handle(),
            settings_before: None,
            page_focus: cx.focus_handle(),
            settings_backdrop: None,
            grains: crate::ui::grain::Grains::new(cx),
            settings_closing: false,
            settings_reading: false,
            rename,
            busy: false,
            compacting: Default::default(),
            turn: None,
            turn_stop: Default::default(),
            review: Default::default(),
            call_running: false,
            streaming_id: None,
            asking: None,
            card_for: None,
            offer: None,
            why: None,
            judging: None,
            judged: 0,
            tags: Default::default(),
            blocked: None,
            session_allow: letronna_core::gate::Grants::new(),
            rules_trouble: None,
            rules: Ok(Vec::new()),
            rules_read: None,
            agent_asks: std::collections::VecDeque::new(),
            answer,
            deny_reason,
            composer_empty: true,
            completer: Default::default(),
            modes: std::collections::HashMap::new(),
            crew_asked: false,
            crew_dismissed: false,
            schedules: schedule_runner.list(),
            schedule_runner,
            editing: None,
            job_name,
            job_prompt,
            profile_name,
            profile_label: crate::ui::profiles::title_label(),
            workflows: crate::ui::workflows::Workflows::new(cx),
            messaging: letronna_core::messaging::load(),
            pairing: None,
            slack_code: None,
            slack_state: None,
            runs: Default::default(),
            run_ends,
            run_steps,
            remote: Default::default(),
            generation: 0,
            reading: None,
            typing: Default::default(),
            outstanding: 0,
            messaging_stop: None,
            heard_elsewhere: None,
            waiting_to_hear: false,
            slack_listener: None,
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
            wallet,
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
            running_open: false,
            viewing: None,
            steering: None,
            steer,
            activity_focus: cx.focus_handle(),
            sidebar_focus: cx.focus_handle(),
            transcript_focus: cx.focus_handle(),
            panel_focus: cx.focus_handle(),
            question_focus: cx.focus_handle(),
            region: None,
            picked: None,
            focus_turn: None,
            focusing: None,
            turn_menu: None,
            editing_message: None,
            edit_message,
            thinking_open: std::collections::HashSet::new(),
            runs_open: std::collections::HashSet::new(),
            call_times: Default::default(),
            turn_files: None,
            turn_files_for: None,
            video: None,
            video_frame: None,
            panel: Default::default(),
            changes: crate::ui::changes::Changes::new(window, cx),
            browser: cx.new(|cx| crate::ui::browser::BrowserView::new(window, cx)),
            browser_for: 0,
            files: crate::ui::files::Files::new(cx),
            jobs: crate::ui::jobs::Jobs::new(window, cx),
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
            memory_menu: None,
            drawn: false,
            mic: None,
            card_key: None,
            _subscriptions: subscriptions,
            recent: Vec::new(),
            switcher: None,
            previews_open: std::collections::HashSet::new(),
            thought_for: std::collections::HashMap::new(),
            thinking_since: None,
            edited: std::collections::HashMap::new(),
            column: COLUMN,
            strip_focus: cx.focus_handle().tab_stop(true),
            strip_area: cx.focus_handle(),
            strip_focus_first: false,
            plan_folded: std::collections::HashSet::new(),
        };
        let shrink = this.follow_browser_shrink(window, cx);
        this._subscriptions.push(shrink);
        let checks = this.follow_browser_checks(window, cx);
        this._subscriptions.push(checks);
        let front = cx.observe_window_activation(window, |this, window, _| {
            this.in_front = window.is_window_active();
        });
        this._subscriptions.push(front);
        let point_comment = this.follow_point_comments(window, cx);
        this._subscriptions.push(point_comment);
        let run_line = this.follow_run_line(window, cx);
        this._subscriptions.push(run_line);
        // The tab getting the keys is a moment the user may have committed
        // or edited elsewhere, like the window coming to the front.
        let focus = this.changes.focus.clone();
        let tab = cx.on_focus_in(&focus, window, |this, _, cx| this.refresh_changes(cx));
        this._subscriptions.push(tab);
        // A new session nothing was said in gives back the number it held,
        // as the terminal's does on its way out.
        let quitting = cx.on_app_quit(|this, _| {
            this.store.release_unused();
            async {}
        });
        this._subscriptions.push(quitting);
        this.load_models(window, cx);
        // Directly, not through `focus_composer`: the window's root does
        // not exist yet to ask about dialogs, and there can be none.
        this.composer
            .update(cx, |input, cx| input.focus(window, cx));
        Self::watch_schedules(schedule_events, cx);
        Self::watch_runs(ended_runs, cx);
        Self::watch_run_steps(run_stepped, cx);
        Self::watch_updates(cx);
        Self::watch_inbox(cx);
        // After the inbox listens, as a wake-up is a notice left in it. Every
        // session's, since the window lists them all; a terminal open on one
        // fires its own first.
        letronna_core::wake::serve_every();
        Self::watch_sessions(cx);
        Self::serve_approvals(cx);
        Self::watch_ended(cx);
        Self::watch_clicks(window, cx);
        Self::watch_jobs(window, cx);
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
        // A text model means chatting again.
        let here = self.store.conversations[self.store.active].id;
        self.media_kinds.remove(&here);
        self.model_open = false;
        self.focus_composer(window, cx);
        self.save();
        cx.notify();
    }

    /// The model a media kind's tool uses, or `None` for the cheapest
    /// working, and what the session on screen makes its messages into.
    pub(crate) fn choose_media(
        &mut self,
        kind: &'static str,
        model: Option<String>,
        cx: &mut Context<Self>,
    ) {
        match letronna_core::media::choose(kind, model.as_deref()) {
            Ok(()) => {
                self.media_choices = letronna_core::media::choices();
                let here = self.store.conversations[self.store.active].id;
                self.media_kinds.insert(here, kind);
            }
            Err(why) => self.error = Some(why),
        }
        self.model_open = false;
        cx.notify();
    }

    pub(crate) fn save(&mut self) {
        if let Err(error) = letronna_core::providers::local::apply(&self.store.settings) {
            self.error = Some(error);
        }
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
        // The terminal's new sessions come in read whole.
        self.let_go();
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
    /// `OPEN_SESSIONS`. Never the one on screen, nor one a reply or a chat
    /// app is writing to: added to while let go of, it would hold only the
    /// new messages, and the next step would be sent those alone.
    fn let_go(&mut self) {
        // Sessions read in without being opened -- started by a run, a chat
        // app or the terminal -- are first in line, or nothing would ever
        // let go of them.
        for conversation in &self.store.conversations {
            if !conversation.messages.is_empty() && !self.open.contains(&conversation.id) {
                self.open.push_back(conversation.id);
            }
        }
        let mut at = self.open.len();
        // The front is the session on screen.
        while self.open.len() > OPEN_SESSIONS && at > 1 {
            at -= 1;
            let id = self.open[at];
            let busy = self.streaming_id == Some(id)
                || self.runs.contains_key(&id)
                || self.remote.holds(id);
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
                // What its edits changed goes with its messages, and is
                // worked out again if it is opened.
                self.edited.retain(|&(session, _), _| session != id);
            }
        }
    }

    pub(crate) fn new_chat(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // A new session usually continues on the same project. There may
        // be no session at all: the last one was just deleted.
        let workspace = (self.store.conversations.get(self.store.active))
            .and_then(|conversation| conversation.workspace.clone());
        // One nothing was written in yet is opened again rather than another
        // piled up beside it: the one on screen, otherwise the newest.
        let conversations = &self.store.conversations;
        let index = std::iter::once(self.store.active)
            .chain((0..conversations.len()).rev())
            .find(|&ix| conversations.get(ix).is_some_and(untouched))
            .unwrap_or(conversations.len());
        self.leave_for(index);
        if index == self.store.conversations.len() {
            self.store.conversations.push(Conversation {
                id: self.store.reserve_id(),
                title: "New session".into(),
                updated: backend::now(),
                workspace,
                ..Default::default()
            });
        } else {
            // To the top of the list, as a session made now would be, rather
            // than left below the sessions listed since it was made.
            let mut conversation = self.store.conversations.remove(index);
            conversation.updated = backend::now();
            conversation.workspace = workspace;
            self.store.conversations.push(conversation);
        }
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
        let done = if matches!(name, "/loop" | "/heartbeat") && self.remote.holds(here) {
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
        // Beside whatever runs here, so it neither waits for a reply nor
        // queues behind one.
        if let Some(task) = crate::runs::background_prompt(&prompt) {
            self.composer
                .update(cx, |input, cx| input.set_value("", window, cx));
            self.run_in_background(task, cx);
            return;
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
            Some(Ok(Given::Text(given))) => {
                self.composer.update(cx, |input, cx| {
                    input.set_value("", window, cx);
                    input.insert(given, window, cx);
                });
                return;
            }
            Some(Ok(Given::Run(asked))) => {
                if self.busy {
                    // Folded into a turn as it runs, it would reach the model
                    // as typed.
                    self.error = Some(
                        "A saved workflow starts a turn of its own. Send it when this one is \
                         done."
                            .into(),
                    );
                    cx.notify();
                    return;
                }
                self.composer
                    .update(cx, |input, cx| input.set_value("", window, cx));
                if self.run_saved(&prompt, asked, cx).is_err() {
                    self.open_settings(window, cx);
                }
                return;
            }
            Some(Err(error)) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
            None => {}
        }
        if self.run_holds_open() && (!prompt.is_empty() || !self.attached().is_empty()) {
            return self.queue_after_run(prompt, window, cx);
        }
        // A reply in another session leaves this one free: what is sent here
        // runs beside it rather than waiting for it.
        let elsewhere = self.busy_elsewhere().is_some();
        if self.busy && !elsewhere && !prompt.is_empty() {
            return self.queue_follow_up(prompt, window, cx);
        }
        if self.busy && !elsewhere {
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
        let session = self.store.conversations[self.store.active].id;
        let words = crate::ui::composer::crew_words(&prompt, self.crew_dismissed);
        tell_crew(session, &words, crew::Source::Composer);
        let Some(prompt) = self.prompt_for(prompt, cx) else {
            return;
        };
        // What a media model makes is paid from the Letronna account, so
        // signed out it is not sent, and the box keeps it as typed, without
        // the line Enter added.
        if self.making(&prompt, false).is_some() && !letronna_core::auth::signed_in() {
            self.composer
                .update(cx, |input, cx| input.set_value(prompt, window, cx));
            self.error = Some(
                "Images, speech and video are made on Letronna's hosted models, which need a \
                 Letronna account. Sign in first, or pick a text model."
                    .into(),
            );
            cx.notify();
            return;
        }
        self.composer
            .update(cx, |input, cx| input.set_value("", window, cx));
        // Setting the box raises no change, so the chip is put right here.
        self.crew_reset(cx);
        let sent = if elsewhere {
            self.send_beside(prompt, images, cx)
        } else {
            self.dispatch(prompt, images, None, cx)
        };
        if sent.is_err() {
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
                } else {
                    None
                };
                // Sent as a typed message is: after the run or the reply in
                // this session, or beside a reply in another.
                let elsewhere = this.busy_elsewhere().is_some();
                match (expanded, refused) {
                    (Err(why), _) | (Ok(_), Some(why)) => {
                        this.error = Some(why);
                        cx.notify();
                    }
                    (Ok(text), None) if this.run_holds_open() => {
                        this.queue_after_run(text, window, cx)
                    }
                    (Ok(text), None) if this.busy && !elsewhere => {
                        this.queue_follow_up(text, window, cx)
                    }
                    (Ok(text), None) => {
                        this.composer
                            .update(cx, |input, cx| input.set_value("", window, cx));
                        tell_crew(session, &typed, crew::Source::Composer);
                        // Pictures attached go with it, as with a typed message.
                        let images = this.attached().to_vec();
                        let sent = if elsewhere {
                            this.send_beside(text, images, cx)
                        } else {
                            this.dispatch(text, images, None, cx)
                        };
                        if sent.is_err() {
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
    /// project or a skill added a command by that name, or a saved workflow
    /// has it, and the window has none of its own.
    pub(crate) fn added_command(&self, prompt: &str) -> Option<Result<Given, String>> {
        use letronna_core::slash::{self, Origin};
        let (name, args) = slash::split(prompt)?;
        let workspace = self.store.conversations[self.store.active]
            .workspace
            .as_deref();
        let commands = slash::list(workspace, OWN_COMMANDS);
        let command = slash::find(&commands, name)?;
        if command.origin == Origin::Workflow {
            let asked = letronna_core::workflow::saved::prompt(&command.name, args);
            return Some(Ok(Given::Run(asked)));
        }
        Some(slash::fill(command, args).map(Given::Text))
    }

    /// Sends `asked`, the prompt a saved workflow's command `typed` gives,
    /// with the `workflow` tool granted for its turn, so the model can show
    /// the run's launch card; `Err` as [`Self::dispatch`] gives it.
    pub(crate) fn run_saved(
        &mut self,
        typed: &str,
        asked: String,
        cx: &mut Context<Self>,
    ) -> std::result::Result<(), ()> {
        let session = self.store.conversations[self.store.active].id;
        tell_crew(session, typed, crew::Source::Composer);
        // After `tell_crew`, which ends what the last command granted.
        crew::grant(session);
        self.dispatch(asked, Vec::new(), None, cx)
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
        let summaries = self.store.conversations[ix].asked.clone();
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
            let answer =
                letronna_core::agents::btw(&connection, messages, summaries, &question, cancel);
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

    /// Send a prompt in the active session. Split out of `send` so a chat app
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
        let making = self.making(&prompt, from_chat_app);
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
        // only the user asking again says so: a chat's turn is not read
        // aloud, and would cut off the answer they are hearing.
        self.match_voice();
        if let (Some(voice), false) = (&mut self.voice, from_chat_app) {
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
        name_after(conversation, &prompt);
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
            remote: from_chat_app,
            ..Default::default()
        });
        let target_id = conversation.id;
        let model = connection.model.clone();
        self.busy = true;
        self.streaming_id = Some(target_id);
        // What the user asked for, for the judge that decides the calls of
        // the helpers the session wakes; `run_turn` keeps it current.
        let mut connection = connection;
        connection.typed = letronna_core::judge::typed(&conversation.messages).into();
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
        match making {
            Some(kind) => self.make(kind, &prompt, cx),
            None => self.run_turn(cx),
        }
        Ok(())
    }

    /// The media kind the active session's messages are made into, or
    /// `None` for the chat model, which a chat app's message, `/init` and a
    /// saved workflow always go to: they are written for it.
    pub(crate) fn making(&self, prompt: &str, from_chat_app: bool) -> Option<&'static str> {
        let here = self.store.conversations[self.store.active].id;
        let for_chat = from_chat_app
            || letronna_core::init::typed(prompt).is_some()
            || letronna_core::workflow::saved::typed(prompt).is_some();
        self.media_kinds.get(&here).copied().filter(|_| !for_chat)
    }

    /// Make what `prompt` describes on the model picked for `kind`, as a
    /// reply that calls that kind's tool: its card asks first, since it is
    /// paid, and what it makes shows as any call's result does.
    fn make(&mut self, kind: &str, prompt: &str, cx: &mut Context<Self>) {
        let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) else {
            return self.finish(cx);
        };
        let (tool, field) = match kind {
            "speech" => ("speak", "text"),
            "video" => ("make_video", "prompt"),
            _ => ("make_image", "prompt"),
        };
        let mut args = serde_json::json!({});
        args[field] = prompt.into();
        let picked = self.media_choices.get(kind).cloned();
        if let Some(model) = &picked {
            args["model"] = model.as_str().into();
        }
        let conversation = &mut self.store.conversations[ix];
        // Its place in the session, so no two calls there share an id.
        let call = backend::ToolCall {
            id: format!("call_{tool}_{}", conversation.messages.len()),
            name: tool.into(),
            arguments: args.to_string(),
        };
        let label = crate::ui::composer::kind_label(kind);
        conversation.messages.push(Message {
            role: "assistant".into(),
            model: picked.unwrap_or_else(|| format!("{label} · Auto")),
            tool_calls: vec![call.clone()],
            at: backend::now(),
            ..Default::default()
        });
        let workspace = conversation.workspace.clone();
        self.media_turn = true;
        if let Some(turn) = self.turn.as_mut() {
            // Where it is saved, as `run_turn` sets it for a model's calls.
            turn.connection.workspace = workspace;
            turn.begin(vec![call]);
        }
        self.save();
        self.advance_tools(cx);
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
        // A reply in another session leaves this one free, and a new one is
        // free whatever runs.
        let previous = self.store.active;
        let landing = (!new_session).then(|| self.store.conversations[previous].id);
        if landing.is_some_and(|id| self.busy && self.streaming_id == Some(id)) {
            return Err("A reply is still running. Send this when it is done.".into());
        }
        if landing.is_some_and(|id| self.runs.contains_key(&id)) {
            return Err(
                "That session is running beside the chat. Send this when it is done.".into(),
            );
        }
        if prompt.chars().count() > 12_000 {
            return Err("Please keep each message under 12,000 characters.".into());
        }
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
        let session = self.store.conversations[self.store.active].id;
        tell_crew(session, &prompt, crew::Source::Composer);
        let sent = match self.prompt_for(prompt, cx) {
            Some(prompt) if self.busy => self.send_beside(prompt, images, cx).is_ok(),
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
        let mode = self.mode(session);
        // When the session saves to memory and where, as it chose, hold
        // from this connection's first request on: the terminal may have
        // chosen since its last turn here.
        let conversation = &self.store.conversations[ix];
        letronna_core::memory::choose(session, conversation.memory_saving);
        letronna_core::memory::choose_scope(session, conversation.memory_scope);
        let connection = Connection {
            endpoint,
            model: model.to_owned(),
            key,
            tools: true,
            workspace: None,
            plan: mode == letronna_core::gate::Mode::Plan,
            effort: self.store.effort(ix).to_owned(),
            search: self.store.settings.search,
            // A chat app's turn may have nobody watching.
            unattended: remote.is_some(),
            remote,
            conversation: session,
            wrap_up: false,
            mode,
            // A chat app's session has a human too, just reached through a
            // card sent to the chat by `on_agent_ask` rather than shown in
            // this window; only a schedule deciding without the user truly
            // has nobody there.
            human: self.unattended(session).is_none(),
            // Filled in by `run_turn`, once the prompt is in.
            typed: Default::default(),
            // Its own: the judge runs on `model`.
            session_model: String::new(),
            system: None,
            // The window shows Markdown whole.
            plain_terminal: false,
            // Someone is watching, so every tool goes out in the array.
            lean_tools: false,
            rare_on_demand: true,
        };
        backend::validate(&connection)?;
        Ok(connection)
    }

    /// Hear what the schedules' runner says: a run that starts is listed as
    /// running, with its stop, until it ends; one that ended is called back
    /// about as a turn that ended is; and why the schedules are not running
    /// is shown once, as an error.
    pub(crate) fn watch_schedules(
        events: async_channel::Receiver<letronna_core::schedule::Event>,
        cx: &mut Context<Self>,
    ) {
        use letronna_core::schedule::Event as Scheduled;
        cx.spawn(async move |this, cx| {
            while let Ok(event) = events.recv().await {
                let shown = this.update(cx, |this, cx| match event {
                    Scheduled::Started(run) => this.scheduled_started(run, cx),
                    Scheduled::Ended(run) => this.scheduled_ended(run, cx),
                    Scheduled::Trouble(trouble) => {
                        this.error = Some(trouble);
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

    /// Wake an idle session whenever a notice arrives for one.
    fn watch_inbox(cx: &mut Context<Self>) {
        let doorbell = inbox::listen();
        cx.spawn(async move |this, cx| {
            while doorbell.recv().await.is_ok() {
                // A helper that ended rings too, and its card goes with it.
                let rang = this.update(cx, |this, cx| {
                    this.drop_ended_asks(cx);
                    this.wake_idle(cx);
                    // A run that ended or held itself says so in a notice,
                    // and one the user stopped rings without one, which is
                    // when the Workflows page has news to show.
                    if this.page == Page::Workflows {
                        cx.notify();
                    }
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
        self.leave_for(index);
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
        let remote = self.remote.holds(id);
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
    fn tell_ended_turn(&mut self, id: u64) {
        let answer = self
            .store
            .index_of(id)
            .and_then(|ix| self.store.conversations[ix].messages.last())
            .filter(|last| last.role == "assistant" && !last.content.is_empty())
            .map(|last| last.content.clone());
        let error = self.error.clone();
        let why = match (&answer, &error) {
            (Some(answer), _) => notify::Why::Finished(answer),
            (None, Some(error)) => notify::Why::Failed(error),
            (None, None) => notify::Why::Finished(""),
        };
        self.tell(id, why);
    }

    /// A helper's call its own gate put to the user, put up as a card in its
    /// session.
    fn on_agent_ask(&mut self, request: approvals::Request, cx: &mut Context<Self>) {
        // Core has one request out per run and rule at a time, so a card
        // still up for the same was given up on: its helper stopped waiting.
        self.agent_asks.retain(|queued| !request.repeats(queued));
        cx.notify();
        // A scheduled run asks from a session the sessions folder's watch
        // may not have brought in yet.
        if self.store.index_of(request.root).is_none() {
            self.follow_sessions(cx);
        }
        // With its session gone no card could show; dropped, it is declined.
        let Some(ix) = self.store.index_of(request.root) else {
            return;
        };
        // Its run may have ended before it got here, since the doorbell that
        // says so comes by another way.
        if run_ended(request.root, request.run) {
            return;
        }
        // A schedule that decides without the user has nobody at its cards;
        // dropped, the request is declined.
        if self.unattended(request.root).is_some() {
            return;
        }
        // A chat app's run puts its card to the chat. Nobody there sees a
        // helper's, and one waiting here would hold up the chat's answer
        // until someone came to the window.
        if self.remote.holds(request.root) {
            self.ask_chat(request, cx);
            return;
        }
        if !(self.page == Page::Chat && self.store.active == ix) {
            let title = &self.store.conversations[ix].title;
            self.error = Some(if self.own_run(&request) {
                format!("\"{title}\" is waiting for your answer.")
            } else {
                format!(
                    "Helper {} in \"{title}\" is waiting for your answer.",
                    request.label
                )
            });
        }
        let mut ctx = letronna_core::tools::ToolContext::default();
        ctx.conversation = request.root;
        ctx.workspace = self.store.conversations[ix].workspace.clone();
        letronna_core::hooks::card(ctx, &request.call.name);
        let why = notify::Why::Waiting(&request.call, Some(&request.label));
        self.tell(request.root, why);
        self.agent_asks.push_back(request);
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

    /// Call the user back about a scheduled run that ended, in its session,
    /// which following the sessions folder brought into the sidebar as it
    /// ran; a run left without one says why as an error, and one that found
    /// nothing new says nothing. The schedules are read again for the time
    /// it ran.
    fn scheduled_ended(&mut self, run: letronna_core::schedule::Ended, cx: &mut Context<Self>) {
        self.schedules = self.schedule_runner.list();
        // Listed as running no longer: its stop has nothing left to stop.
        if let Some(session) = run.session {
            self.runs.remove(&session);
        }
        // The folder's watch waits for writes to settle, so this can come first.
        self.follow_sessions(cx);
        match run.session.filter(|&id| self.store.index_of(id).is_some()) {
            _ if run.quiet => {}
            Some(id) => self.tell(id, notify::Why::Scheduled(&run.text)),
            None => self.error = Some(format!("Scheduled job {}: {}", run.name, run.text)),
        }
        cx.notify();
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
        // The review read the conversation as it was; this turn goes first.
        review::stop(&self.review.1);
        turn.connection.workspace = self.store.conversations[ix].workspace.clone();
        // What the user asked for, for the judge that decides its helpers' calls.
        let typed = letronna_core::judge::typed(&self.store.conversations[ix].messages);
        turn.connection.typed = typed.into();
        let conversation = &self.store.conversations[ix];
        let receiver = backend::start_from(
            turn.connection.clone(),
            conversation.messages.clone(),
            conversation.floor(),
        );
        turn.live = agent::Live::default();
        cx.notify();
        let generation = self.generation;
        // The reply's text is paced onto the screen, a piece a frame, rather
        // than shown in the bursts it arrives in; with motion reduced it is
        // shown as it comes.
        let paced = !cx.reduce_motion();
        self.reading = Some(cx.spawn(async move |this, cx| {
            let mut pacer = crate::ui::pace::Pacer::default();
            let mut open = true;
            loop {
                if pacer.is_empty() {
                    if !open {
                        break;
                    }
                    match receiver.recv().await {
                        Ok(event) => pacer.push(event),
                        Err(_) => {
                            open = false;
                            continue;
                        }
                    }
                }
                while let Ok(event) = receiver.try_recv() {
                    pacer.push(event);
                }
                let batch = if paced { pacer.next() } else { pacer.all() };
                let mut ended = false;
                for event in batch {
                    let finished = matches!(event, Event::Finished(_));
                    let current = this.update(cx, |this, cx| {
                        let current = this.generation == generation;
                        if current {
                            this.on_event(event, cx);
                        }
                        current
                    });
                    if !matches!(current, Ok(true)) || finished {
                        ended = true;
                        break;
                    }
                }
                if ended {
                    break;
                }
                if !pacer.is_empty() {
                    cx.background_executor().timer(crate::ui::pace::FRAME).await;
                }
            }
        }));
    }

    fn on_event(&mut self, event: Event, cx: &mut Context<Self>) {
        self.time_thinking(&event);
        let Some(turn) = self.turn.as_mut() else {
            return;
        };
        turn.absorb(&event);
        // Read aloud only for whoever is at the window: a chat's run is
        // answered where it came from.
        let aloud = turn.connection.remote.is_none();
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
                // `finish` reports it to a chat app, and holds back what was
                // queued, by this.
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
        let auto = self.turn_mode() == letronna_core::gate::Mode::Auto;
        // A call the gate would not simply run ends the batch, and is dealt
        // with below once the batch is back. The judge is never asked here:
        // a call it would decide ends the batch too.
        let batch = until_repeated_id(turn.next_batch(
            |call| self.decide(call, &self.rules) != letronna_core::gate::Outcome::Run,
            agent::MAX_BATCH,
        ));
        if !batch.is_empty() {
            self.asking = None;
            for call in batch {
                if auto {
                    self.tag_unjudged(&call);
                }
                self.execute_call(call, false, false, None, cx);
            }
            cx.notify();
            return;
        }
        // Auto mode's judge waits on the network, so it decides off this
        // thread; every other mode decides here.
        if auto && let Some(call) = turn.front().cloned() {
            self.judge_front(call, cx);
            return;
        }
        let next = turn.next(|call| self.decide(call, &self.rules));
        self.go_on(next, None, cx);
    }

    /// Does what `next` says; `why` is the judge's reason for a call it
    /// leaves to the user.
    pub(crate) fn go_on(&mut self, next: agent::Next, why: Option<String>, cx: &mut Context<Self>) {
        self.why = why;
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
            // Numbered after the card before it, before that one goes.
            let place = match &next {
                agent::Next::AskUser(call) => self.card_place(call),
                _ => None,
            };
            self.card_for = match &next {
                agent::Next::AskUser(call) => self.turn.as_ref().map(|turn| {
                    let ctx = letronna_core::tools::ToolContext::of(&turn.connection);
                    crate::ui::approval::CardFor::of(call, &ctx).at(place)
                }),
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
                // The card shows only with its session on screen, and until
                // it is answered no other session can start, so say which one
                // is waiting.
                let on_screen = self.page == Page::Chat
                    && self.streaming_id == Some(self.store.conversations[self.store.active].id);
                if let Some(ix) = self
                    .streaming_id
                    .filter(|_| !on_screen)
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
            agent::Next::Run(call) => self.execute_call(call, false, false, None, cx),
            agent::Next::Sandboxed(call) => self.execute_call(call, false, true, None, cx),
            agent::Next::Refuse(_, why) => self.record_result(Err(why), cx),
            agent::Next::Ask => {
                let results = self
                    .turn
                    .as_mut()
                    .map(|turn| turn.take_results())
                    .unwrap_or_default();
                if let Some(ix) = self.streaming_id.and_then(|id| self.store.index_of(id)) {
                    let conversation = &mut self.store.conversations[ix];
                    let first = conversation.messages.len();
                    conversation.messages.extend(results);
                    let session = conversation.id;
                    (self.call_times).keep(session, first, &conversation.messages[first..]);
                    // The files the tools wrote are noted on these results,
                    // which are in the conversation only from here.
                    self.refresh_changes(cx);
                    let looped = self.turn.as_ref().and_then(agent::Turn::looping);
                    if let Some(why) = looped.map(str::to_owned) {
                        // An error, so a chat's report says why.
                        self.error = Some(why);
                        self.save();
                        self.finish(cx);
                    // A hook's `continue: false`, shown with the results;
                    // or what a media model made, which is the answer.
                    } else if letronna_core::hooks::halted(session) || self.media_turn {
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
        // After the results, which have to follow the calls they answer. What
        // you sent while it worked goes in now too, before the model is asked
        // again, so it reads it before going on.
        // Folded into the running turn, so the window keeps its task.
        let notices = inbox::fold(&inbox::take(conversation.id, false));
        conversation.messages.extend(notices.map(|notice| Message {
            folded: true,
            ..notice
        }));
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
    /// `approved` when the user said yes to it on its card, `sandboxed` when
    /// the gate let it run only inside the OS sandbox.
    fn execute_call(
        &mut self,
        call: backend::ToolCall,
        approved: bool,
        sandboxed: bool,
        keep_in: Option<letronna_core::memory::Scope>,
        cx: &mut Context<Self>,
    ) {
        let mut ctx = self.call_context();
        ctx.approved = approved;
        ctx.sandboxed = sandboxed;
        ctx.keep_in = keep_in;
        ctx.cancel = self.turn_stop.clone();
        self.call_running = true;
        let (id, name) = (call.id.clone(), call.name.clone());
        self.call_times.start(&id);
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
        self.call_times.stop(id);
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
    /// Slack listener is reopened only when its own settings changed. While
    /// another Letronna answers Telegram and Discord, such as a `letronna
    /// serve`, the window says so and takes them over once it lets go.
    pub(crate) fn restart_messaging(&mut self, cx: &mut Context<Self>) {
        use letronna_core::remote::{self, By, NotHeard};
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
        let (tx, rx) = async_channel::unbounded();
        let heard = remote::listen(&self.messaging, tx.clone(), By::Window);
        let elsewhere = match &heard {
            Err(NotHeard::Elsewhere(holder)) => Some(holder.clone()),
            _ => None,
        };
        if elsewhere != self.heard_elsewhere {
            self.heard_elsewhere = elsewhere;
            cx.notify();
        }
        if self.heard_elsewhere.is_some() {
            self.hear_when_free(cx);
        }
        // Slack is the window's own, heard beside whoever hears the rest.
        let stop = heard.ok().flatten();
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
        } else if stop.is_none() {
            return;
        }
        self.messaging_stop = stop;
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

    /// Take the chat apps over, as at startup, once the Letronna answering
    /// them lets them go. A thread blocks on their lock meanwhile, so nothing
    /// here ticks while the window waits.
    fn hear_when_free(&mut self, cx: &mut Context<Self>) {
        if self.waiting_to_hear {
            return;
        }
        let (tx, rx) = async_channel::bounded(1);
        let free = move || {
            let _ = tx.send_blocking(());
        };
        if letronna_core::remote::hear_when_free(letronna_core::remote::By::Window, free).is_err() {
            return;
        }
        self.waiting_to_hear = true;
        cx.spawn(async move |this, cx| {
            let free = rx.recv().await.is_ok();
            let _ = this.update(cx, |this, cx| {
                // A thread that ended without the lock is not waiting any
                // more; the next change to the settings starts another.
                this.waiting_to_hear = false;
                if free {
                    this.restart_messaging(cx);
                }
            });
        })
        .detach();
    }

    pub(crate) fn tell_remote(
        &self,
        source: &letronna_core::messaging::Source,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.tell_remote_card(source, text, None, cx);
    }

    /// Send to a chat, with Allow and Deny under it when `card` is the id of
    /// the call it asks about.
    pub(crate) fn tell_remote_card(
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

    /// Show `source` that its run in session `id` is at work, for as long as
    /// it goes and is not waiting on the chat. The service drops the
    /// indicator after a few seconds, so it is sent again on a timer that
    /// lives only as long as the run, and a new run's there replaces it.
    pub(crate) fn keep_typing(
        &mut self,
        source: letronna_core::messaging::Source,
        id: u64,
        cx: &mut Context<Self>,
    ) {
        let every = source.typing_every();
        let task = cx.spawn(async move |this, cx| {
            loop {
                let working = this.update(cx, |this, _| {
                    let run =
                        (this.runs.get(&id)).filter(|run| run.source.as_ref() == Some(&source));
                    // Waiting for a place, or on a card sent, which is the
                    // chat's turn to answer, it is not at work.
                    let at_work =
                        |run: &crate::runs::Run| !run.queued && !this.remote.asking(&source);
                    run.map(|run| at_work(run).then(|| this.messaging.clone()))
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
        });
        self.typing.insert(id, task);
    }

    /// Send what chat `source`'s run in session `id` came to, `reply`, back
    /// to the chat, with the pictures and sound it made; read aloud too,
    /// while the Voice setting is on, when it answered a voice note and was
    /// `spoken` to.
    pub(crate) fn answer_run(
        &mut self,
        source: &letronna_core::messaging::Source,
        id: u64,
        reply: &str,
        spoken: bool,
        cx: &mut Context<Self>,
    ) {
        let made = self
            .store
            .index_of(id)
            .map(|ix| letronna_core::messaging::files::made(&self.store.conversations[ix].messages))
            .unwrap_or_default();
        // Files cannot go to Slack, so a voice note is only for the others.
        let voiced = spoken
            && self.store.settings.voice
            && !matches!(source, letronna_core::messaging::Source::Slack { .. });
        if made.is_empty() && !voiced {
            return self.tell_remote(source, reply, cx);
        }
        // One task, so the pictures and sound the run made follow the reply.
        let (config, source, reply) = (self.messaging.clone(), source.clone(), reply.to_owned());
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
    pub(crate) fn on_remote_after(
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
        let unattended = self.streaming_id.and_then(|id| self.unattended(id));
        let mode = self.turn_mode();
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

    /// The user answered the approval card.
    pub(crate) fn resolve_call(&mut self, verdict: Verdict, cx: &mut Context<Self>) {
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
                    // Its helpers are let through by the rule of the call.
                    approvals::allow_for_session(id, &call, &self.call_context());
                }
                self.execute_call(call, true, false, None, cx);
            }
            Verdict::Keep(place) => self.execute_call(call, true, false, Some(place), cx),
            Verdict::AllowHere => {
                // Into the user's own rules, held to this project: the
                // project's file may not allow anything.
                let file = letronna_core::permissions::user_file();
                let kept = offer.map(|rule| letronna_core::permissions::keep(&file, rule));
                if let Some(Err(error)) = kept {
                    self.error = Some(error);
                }
                self.execute_call(call, true, false, None, cx);
            }
        }
    }

    /// The mode `session` is in: its schedule's while that runs, which the
    /// gate follows, then its chip's, then the Default mode setting.
    pub(crate) fn mode(&self, session: u64) -> letronna_core::gate::Mode {
        let running = letronna_core::schedule::running(session).map(|run| run.mode);
        let picked = running.or(self.modes.get(&session).copied());
        picked.unwrap_or(self.store.settings.mode)
    }

    /// What session `root`'s own rules make of `call`, or None when only
    /// the user can say: the gate the session itself goes through,
    /// in its mode and with what was allowed there.
    pub(crate) fn decided(
        &self,
        call: &backend::ToolCall,
        root: u64,
    ) -> Option<approvals::Decision> {
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
            self.unattended(root),
        );
        match letronna_core::gate::decide(call, &gate) {
            letronna_core::gate::Outcome::Run => Some(approvals::Decision::Once),
            letronna_core::gate::Outcome::Sandboxed => Some(approvals::Decision::Sandboxed),
            letronna_core::gate::Outcome::Refuse(why) => Some(approvals::Decision::Deny(why)),
            letronna_core::gate::Outcome::Ask => None,
        }
    }

    /// The mode a scheduled run going on here decides `session`'s calls in
    /// without the user, or None, as for every other session, when they wait
    /// for the user. Asked of core each time rather than kept here, so the
    /// grant ends with the run and never reaches the user's own chats.
    pub(crate) fn unattended(&self, session: u64) -> Option<letronna_core::gate::Mode> {
        letronna_core::schedule::running(session).and_then(|run| run.unattended())
    }

    /// The mode the session running now is in.
    pub(crate) fn turn_mode(&self) -> letronna_core::gate::Mode {
        self.streaming_id
            .map_or(self.store.settings.mode, |id| self.mode(id))
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
        let id = session.id;
        self.modes.insert(id, mode);
        // Helpers the turn starts from now on narrow from the new mode.
        let mode = self.mode(id);
        if let Some(turn) = self.turn.as_mut()
            && turn.connection.conversation == id
        {
            turn.connection.switch_to(mode);
        }
        agents::set_plan(mode == letronna_core::gate::Mode::Plan);
        self.approvals_changed(cx);
        cx.notify();
    }

    /// Decide the call the card is up for again after an approval setting
    /// changed, so it runs at once if it no longer needs the user.
    pub(crate) fn approvals_changed(&mut self, cx: &mut Context<Self>) {
        // A verdict on its way was asked for in Auto: out of Auto it is
        // dropped when it lands, and the call is decided again now.
        if self.judging.is_some() && self.turn_mode() != letronna_core::gate::Mode::Auto {
            self.judging = None;
            self.advance_tools(cx);
            return;
        }
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
    }

    /// Whether the chat page shows the conversation rather than the empty
    /// state, the blank of a session still being read in, or the setup page:
    /// once anything is said, a reply starts, or a helper asks.
    pub(crate) fn transcript_shown(&self) -> bool {
        let conversation = &self.store.conversations[self.store.active];
        let streaming_here = self.busy && self.streaming_id == Some(conversation.id);
        self.show_setup != Some(conversation.id)
            && (!conversation.messages.is_empty()
                || streaming_here
                // A helper's card needs the chat body even with nothing said.
                || self.agent_asks.iter().any(|ask| ask.root == conversation.id))
    }

    /// How far above the bottom of the chat, in pixels.
    pub(crate) fn distance_from_bottom(&self) -> Pixels {
        self.list.max_offset_for_scrollbar().y + self.list.scroll_px_offset_for_scrollbar().y
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

    /// Close part of the way to the bottom: a reply coming in glides the
    /// chat down rather than jumping it a line at a time. A long way, or
    /// with motion reduced, it goes at once. True while there is more to go.
    fn ease_to_bottom(&self, still: bool) -> bool {
        let at = -self.list.scroll_px_offset_for_scrollbar().y;
        let gap = self.list.max_offset_for_scrollbar().y - at;
        if still || gap <= px(1.) || gap > EASE_REACH {
            self.scroll_to_bottom();
            return false;
        }
        (self.list).set_offset_from_scrollbar(point(px(0.), -(at + gap * EASE_STEP)));
        true
    }

    /// Group the session on screen into turns, tell the list which of them
    /// changed since the last draw, and keep to the bottom while following.
    /// True while the chat is still gliding down to it.
    fn lay_out_turns(&mut self, still: bool) -> bool {
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
        let opened = listed != id;
        self.listed = (id, count);
        // A session just opened starts at its end, not gliding to it.
        self.keep_following(still || opened)
    }

    /// Turn `n` of the session on screen, as the list draws it. The turn a
    /// reply is coming in to also has the reply so far, the card it waits
    /// on and what was queued behind it. `tools` is the registry's, looked
    /// up by the first turn that has a run of calls to tell.
    fn turn_row(
        &mut self,
        n: usize,
        column: Pixels,
        in_menu: bool,
        tools: &std::cell::OnceCell<Vec<std::sync::Arc<dyn letronna_core::tools::Tool>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        use crate::ui::actions::shows_bar;
        use crate::ui::motion::{Fold, Row};
        let Some(&span) = self.turns.get(n) else {
            return self.under_turns(column, window, cx);
        };
        let session = self.store.conversations[self.store.active].id;
        let streaming_here = self.busy && self.streaming_id == Some(session);
        // The turn a reply is still coming in to has no buttons or menu:
        // it is not finished, and it owns the session until it is.
        let live = streaming_here && n + 1 == self.turns.len();
        let focus = self.turn_focus(span.first, window, cx);
        // Stepped to from the keyboard, and drawn: only now is its focus
        // there to take the keys.
        if !live && self.focusing == Some((session, span.first)) {
            self.focusing = None;
            window.focus(&focus, cx);
        }
        // What a preview's long lines wrap to depends on it.
        self.column = column;
        self.note_edits(span.first, span.last);
        let conversation = &self.store.conversations[self.store.active];
        let messages = &conversation.messages;
        let arrivals = &self.motion.rows;
        let mut rows = Vec::new();
        // The message a search hit opened, marked on the first row drawn
        // from it on: a reply that only called tools has none of its own.
        let mut hit = self
            .reveal
            .filter(|&(id, at)| id == conversation.id && (span.first..=span.last).contains(&at));
        // Runs of tool calls, each drawn as one line until it is opened.
        let shown_thinking = self.store.settings.thinking;
        let runs =
            letronna_core::tool_runs::find(messages, span.first..span.last + 1, shown_thinking);
        let tools = if runs.is_empty() {
            &[]
        } else {
            tools.get_or_init(letronna_core::tools::all).as_slice()
        };
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
                // A run beside the chat is still answering the last turn too.
                let answering = streaming_here || self.runs.contains_key(&session);
                let shown = shows_bar(span, messages.len(), answering, focused);
                self.turn_bar(span, shown, cx)
            });
            let run = letronna_core::tool_runs::holding(&runs, i);
            let folded = run.filter(|run| !self.runs_open.contains(&(conversation.id, run.first)));
            if let Some(run) = run.filter(|run| run.first == i) {
                let going =
                    live && letronna_core::tool_runs::at_the_end(messages, run, shown_thinking);
                let line = self.run_row(run, going, tools, window, cx);
                // A hit inside a folded run is its line's alone, the failed
                // calls kept in sight under it included.
                let marked =
                    folded.is_some() && hit.take_if(|&mut (_, at)| at <= run.last).is_some();
                let line = if marked {
                    self.mark_hit(line, cx)
                } else {
                    line
                };
                rows.push(arrivals.rise(Row::Message, run.first, line, window, cx));
                // What its calls made stays in sight, small, while folded.
                if folded.is_some() {
                    rows.extend(self.run_media(run, window, cx));
                }
            }
            // A failure is never folded away: it is what needs reading. A
            // command that exited badly is one too.
            if folded.is_some()
                && !letronna_core::tool_runs::failed(m)
                && !crate::ui::messages::exited_badly(m)
            {
                // A run that ends a stopped turn still carries its buttons.
                rows.extend(bar);
                continue;
            }
            let row = self.message_row(i, m, labeled, false, bar, window, cx);
            // The calls of a run just opened come in with it.
            let row = match run.filter(|_| folded.is_none()) {
                Some(run) => {
                    let key = (conversation.id, run.first);
                    (self.motion.opened).ease(Fold::Run, key, row, window, cx)
                }
                None => row,
            };
            let row = if hit.take_if(|&mut (_, at)| i >= at).is_some() {
                self.mark_hit(row, cx)
            } else {
                row
            };
            rows.push(arrivals.rise(Row::Message, i, row, window, cx));
        }
        // Past the last row drawn, the last row stands for it.
        if let Some(row) = rows.pop_if(|_| hit.is_some()) {
            rows.push(self.mark_hit(row, cx));
        }
        // The newest finished turn ends with the files it changed.
        if !live && let Some(card) = self.files_card(span, cx) {
            rows.push(arrivals.rise(Row::Files, span.first, card, window, cx));
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
                cache_write_tokens: turn.live.cache_write_tokens,
                window: turn.live.window,
                ..Default::default()
            }) {
                // Keyed by the place the reply will have once it is stored,
                // so finishing does not start its markdown or its fade-in
                // over again. Labelled when it opens the turn.
                let opens = span.first == messages.len();
                let row = self.message_row(messages.len(), &reply, opens, true, None, window, cx);
                rows.push(arrivals.rise(Row::Message, messages.len(), row, window, cx));
            }
            if let Some(card) = self.approval_card(window, cx) {
                let place = messages.len();
                rows.push(arrivals.rise(Row::Card, place, card, window, cx));
            }
            for (k, text) in self.follow_ups.iter().enumerate() {
                let row = self.queued_row(k, text, cx);
                let place = messages.len() + k;
                rows.push(arrivals.rise(Row::Queued, place, row, window, cx));
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
            let turn = div()
                .id(self.row_id("turn", span.first))
                .w_full()
                .track_focus(&focus)
                .on_any_mouse_down(|_, window, _| window.prevent_default())
                .child(group.context_menu(self.turn_menu(span, cx)));
            self.turn_keys(turn, span, focus, cx).into_any_element()
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
        rows.extend(self.run_live(session, window, cx));
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
                    .children(rows)
                    // Clear of the browser's preview over the pane's corner.
                    .children(
                        Some(self.browser_preview_room(column, window, cx))
                            .filter(|room| *room > px(0.))
                            .map(|room| div().flex_shrink_0().h(room)),
                    ),
            )
            .into_any_element()
    }

    /// Stay at the bottom while following, and stop following once the user
    /// scrolls up to reread. Distance from the bottom alone cannot say which
    /// happened: a reply's markdown is laid out a moment after it arrives, so
    /// the bottom runs ahead of the view on its own. What the user does is
    /// move the view up and away from the bottom. Content that shrinks also
    /// moves it up, but pinned to the bottom, and that must not count.
    fn keep_following(&mut self, still: bool) -> bool {
        let offset = self.list.scroll_px_offset_for_scrollbar().y;
        // Held at a search hit, the chat stays put. Where it is still counts,
        // so a scroll by hand after it lets go is told the right way round.
        if self.holding_hit() {
            self.followed_offset = offset;
            self.place_hit();
            return false;
        }
        let near_bottom = self.distance_from_bottom() <= FOLLOW_SLACK;
        // The offset grows towards zero going up.
        if offset > self.followed_offset && !near_bottom {
            self.following = false;
        } else if offset < self.followed_offset && near_bottom {
            self.following = true;
        }
        self.followed_offset = offset;
        self.following && self.ease_to_bottom(still)
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
                        .secondary()
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

    /// End the turn in flight, from the Stop button. What the turn got to is
    /// kept: the reply so far, or the batch's results with every call left
    /// answered as stopped.
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
        if let Some(voice) = &mut self.voice {
            voice.hush();
        }
        // While the call the judge decides is still in front, before
        // `keep_stopped` answers it as never started.
        self.stop_judge();
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
    /// or else stop the reply, which is what escape does in the terminal, or
    /// the run beside the chat that holds the open session.
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
        } else if self.context_report.is_some() {
            // Opened by a key, the context popover never has the focus its
            // own Escape needs; the status bar's chip keeps it.
            self.context_report = None;
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
        } else if self.run_holds_open() {
            // The window's own reply is never in a session a run holds.
            let id = self.store.conversations[self.store.active].id;
            self.stop_run(id, cx);
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
        let connection = self.turn.as_ref().map(|turn| turn.connection.clone());
        // Before the chat app is answered, which forgets that the session
        // was its.
        if let Some(id) = session.filter(|_| !stopped) {
            self.tell_ended_turn(id);
        }
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
        self.busy = false;
        self.streaming_id = None;
        self.turn = None;
        self.media_turn = false;
        self.judging = None;
        // A retried or rewound reply can come back at the same place, and
        // must not number its cards on from this turn's.
        self.card_for = None;
        // Nor be timed from this turn's thinking; thinking closed while it
        // came in rests shut.
        if let Some((at, _)) = self.thinking_since.take() {
            self.thinking_open.remove(&at);
        }
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
            // Not when a follow-up or a notice started the next turn already,
            // nor after a stop that only a loop's tick carried past.
            if let (Some(id), Some(connection)) = (session, connection)
                && !stopped
                && !self.busy
            {
                self.look_back(id, connection, cx);
            }
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
            goal::Step::Continue {
                notice, message, ..
            } => {
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

    /// Start a memory review in the background when the user chose them and
    /// the turn that ended in session `id` made one due; what it kept shows as
    /// a passing note. Never where a chat app or a schedule drives the session.
    fn look_back(&mut self, id: u64, connection: Connection, cx: &mut Context<Self>) {
        let Some(ix) = self.store.index_of(id) else {
            return;
        };
        let messages = &self.store.conversations[ix].messages;
        let unwatched =
            connection.remote.is_some() || connection.unattended || self.remote.holds(id);
        if !self.store.settings.memory_review || unwatched || !review::due(messages) {
            return;
        }
        self.review = (id, Default::default());
        // One that cannot start is skipped; the next is due in as many turns.
        let Ok(notes) = review::spawn(connection, messages.clone(), self.review.1.clone()) else {
            return;
        };
        cx.spawn(async move |this, cx| {
            if let Ok(note) = notes.recv().await {
                let _ = this.update(cx, |this, cx| {
                    this.note = Some(note);
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Stops the memory review of session `id`, if one runs: the user took
    /// back what it read, or left the session.
    pub(crate) fn stop_review(&self, id: u64) {
        if self.review.0 == id {
            review::stop(&self.review.1);
        }
    }

    /// Stops the memory review of the session on screen when the user goes
    /// to the one at `index` instead.
    pub(crate) fn leave_for(&self, index: usize) {
        if index != self.store.active
            && let Some(leaving) = self.store.conversations.get(self.store.active)
        {
            self.stop_review(leaving.id);
        }
    }

    /// Start a turn for notices that came while nothing ran: in the session
    /// on screen if it has any, otherwise in the one waiting longest.
    pub(crate) fn wake_idle(&mut self, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let on_screen = self.store.conversations[self.store.active].id;
        // A chat app hears only the answer to what it sent, so a turn started
        // in its session would answer nobody; its next message takes them. A
        // scheduled run going on here reads its own as it goes, and a run
        // beside the chat reads its session's own before it answers.
        let wakes = |id| {
            self.store.index_of(id).is_some()
                && !self.remote.holds(id)
                && letronna_core::schedule::running(id).is_none()
                && !self.runs.contains_key(&id)
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
    fn begin_turn(&mut self, id: u64, mut connection: Connection, cx: &mut Context<Self>) {
        self.busy = true;
        self.streaming_id = Some(id);
        // What the user asked for, for the judge, as `run_turn` keeps it.
        if let Some(ix) = self.store.index_of(id) {
            let typed = letronna_core::judge::typed(&self.store.conversations[ix].messages);
            connection.typed = typed.into();
        }
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
        crate::ui::motion::follow_setting(self.store.settings.motion, cx);
        // The panel shows the browser of the session on screen.
        let on_screen = self.store.conversations[self.store.active].id;
        if self.browser_for != on_screen {
            self.browser_for = on_screen;
            letronna_core::browser::show_conversation(on_screen);
        }
        // And the composer holds its draft, however the session was switched
        // to: a click, or a message from a chat app.
        self.swap_draft(window, cx);
        // And whether it is empty is read from it: GPUI Kit sends no change
        // when the text is set rather than typed, as when a send clears it.
        self.composer_empty = self
            .composer
            .read(cx)
            .text()
            .chars()
            .all(char::is_whitespace);
        // And the Changes tab and the status bar speak of its folder.
        self.keep_changes_current(cx);
        // And the switcher counts it as the one used last.
        self.note_recent(on_screen);
        if self.page == Page::Chat {
            self.unseen.remove(&on_screen);
        }
        // A helper's transcript is read in for the Activity page only, and
        // read again whenever the page is opened.
        if self.page != Page::Activity {
            self.viewing = None;
        }
        self.open_on_screen(cx);
        // What the session holds as it comes on screen was there already;
        // only what comes in while it is rises in.
        let shown = &self.store.conversations[self.store.active];
        (self.motion.rows).note(shown.id, shown.messages.len(), shown.unfilled());
        if self.lay_out_turns(cx.reduce_motion()) {
            window.request_animation_frame();
        }
        self.watch_turn_files(cx);
        self.keep_files_current(window, cx);
        self.keep_region(window, cx);
        // The turn the keys were sent to since the last draw takes them in
        // this one if the list draws it; if not, the transcript keeps them.
        self.focusing = self.focus_turn.take();
        self.keep_jobs_current(window, cx);
        self.keep_browser_seen(window, cx);
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
        self.toast_block(window, cx);
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
                                    copied.update(cx, |c, cx| {
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
        // The setup takes the whole window, as a first launch should look:
        // no sidebar, composer or status bar around it, only the title bar
        // to drag the window by.
        let welcome = self.page == Page::Chat && self.show_setup == Some(conversation.id);
        // An empty chat's grain runs behind the composer and the status bar
        // too, to the window's edge, so the page reads as one surface.
        let mut empty_chat = false;

        let body: AnyElement = if self.page == Page::Jobs {
            self.schedules_page(cx).into_any_element()
        } else if self.page == Page::Workflows {
            self.workflows_page(window, cx).into_any_element()
        } else if self.page == Page::Artifacts {
            self.artifacts_page(cx).into_any_element()
        } else if self.page == Page::Wallet {
            self.wallet_page(cx).into_any_element()
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
        } else if let Some(panel) = self.covering_panel(window, cx) {
            // The browser, in a room too narrow for it beside the chat, in
            // place of the conversation as the setup page is: the composer
            // stays under it.
            panel
        } else if welcome {
            div().into_any_element()
        } else if !self.transcript_shown() {
            if conversation.unfilled() {
                // Still being read in, which takes a moment: blank rather
                // than the greeting of a new session.
                div().flex_1().into_any_element()
            } else {
                empty_chat = true;
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
            // Looked up at most once a frame, and only for a turn with a run
            // of calls to tell: each lookup reads the disk.
            let tools = std::cell::OnceCell::new();
            let row = cx.processor(move |this, n: usize, window, cx| {
                this.turn_row(n, column, in_menu, &tools, window, cx)
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
                // The browser's preview at its corner is cut short in a
                // short window, rather than drawn over the title bar.
                .overflow_hidden()
                .track_focus(&self.transcript_focus)
                // As on the sidebar: a click leaves the keys where they
                // were, in the composer most often.
                .on_any_mouse_down(|_, window, _| window.prevent_default())
                .child(list(self.list.clone(), row).size_full())
                .vertical_scrollbar(&self.list)
                .children(self.jump_to_latest(cx))
                .children(self.browser_preview(pane, window, cx))
                .children(self.ring(crate::ui::keys::Region::Transcript, cx))
                .into_any_element()
        };

        // A new page, or another session in the chat, rises into place as it
        // fades in rather than replacing the last one all at once. Keyed by both, so it plays
        // afresh on every change and not on every frame.
        let body = if cx.reduce_motion() {
            body
        } else {
            let shown = if self.page == Page::Chat {
                conversation.id
            } else {
                0
            };
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .w_full()
                .relative()
                .child(body)
                .with_animation(
                    SharedString::from(format!("page-{:?}-{shown}", self.page)),
                    crate::ui::motion::enter(crate::ui::motion::GENTLE),
                    crate::ui::motion::rise,
                )
                .into_any_element()
        };

        // Built only where it shows: another page pays nothing for it.
        let strip = (self.page == Page::Chat)
            .then(|| self.running_strip(window, cx))
            .flatten();
        let main = v_flex()
            .relative()
            .flex_1()
            .min_w_0()
            .h_full()
            .when(self.page != Page::Chat, |this| {
                this.track_focus(&self.page_focus)
            })
            .children(
                empty_chat
                    .then(|| self.grain(&self.grains.empty_chat))
                    .flatten(),
            )
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
                        .children(self.goal_line(cx))
                        .children(strip)
                        .children(self.crew_chip(cx))
                        .child(self.composer(pane < px(600.), cx)),
                )
            })
            .child(self.status_bar(pane, cx))
            // The chat draws its own over the conversation; a page has
            // nowhere else to show that F6 put the keys on it.
            .when(self.page != Page::Chat, |this| {
                this.children(self.ring(crate::ui::keys::Region::Transcript, cx))
            });

        let sidebar_in =
            (self.motion.sidebar).track(self.sidebar_open, crate::ui::motion::STANDARD, window, cx);
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
            .on_action(cx.listener(|this, _: &ToggleCrew, _, cx| this.toggle_crew(cx)))
            .on_action(
                cx.listener(|this, _: &crate::ui::remember::MemoryMenu, window, cx| {
                    if !window.has_active_dialog(cx) && !this.settings_open {
                        this.open_memory_menu(window, cx)
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &DismissCrew, window, cx| this.dismiss_crew(window, cx)),
            )
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
                cx.listener(|this, _: &crate::ui::panel::WideBrowser, window, cx| {
                    if !window.has_active_dialog(cx) && !this.settings_open {
                        this.toggle_wide(window, cx)
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::panel::PreviewBrowser, window, cx| {
                    if !window.has_active_dialog(cx) && !this.settings_open {
                        this.toggle_browser_preview(window, cx)
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
                cx.listener(|this, _: &crate::ui::jobs::JobsTab, window, cx| {
                    if !window.has_active_dialog(cx) && !this.settings_open {
                        this.show_tab(crate::ui::panel::PanelTab::Jobs, window, cx)
                    }
                }),
            )
            .on_action(
                cx.listener(|this, _: &OpenActivity, window, cx| this.open_activity(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::activity::ShowRunning, window, cx| {
                    this.show_running(window, cx)
                }),
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
            .on_action(
                cx.listener(|this, _: &crate::ui::wallet::OpenWallet, window, cx| {
                    this.open_wallet(window, cx)
                }),
            )
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
            .on_action(
                cx.listener(|this, _: &crate::ui::switcher::SwitchSession, window, cx| {
                    this.switch_session(false, window, cx)
                }),
            )
            .on_action(cx.listener(
                |this, _: &crate::ui::switcher::SwitchSessionBack, window, cx| {
                    this.switch_session(true, window, cx)
                },
            ))
            .on_action(
                cx.listener(|this, _: &NextRegion, window, cx| {
                    this.cycle_region(false, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &PreviousRegion, window, cx| {
                this.cycle_region(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FocusComposer, window, cx| {
                this.focus_composer_anywhere(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowKeys, window, cx| this.show_keys(window, cx)))
            // From outside the turns, to the newest; a turn or the question
            // card steps from itself before this.
            .on_action(
                cx.listener(|this, _: &crate::ui::actions::PreviousTurn, window, cx| {
                    this.step_turn(None, false, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::actions::NextTurn, window, cx| {
                    this.step_turn(None, true, window, cx)
                }),
            )
            // Enter and Escape on a permission card, or in an empty composer
            // while one waits; a held key answers again only once it is up.
            .on_action(
                cx.listener(|this, _: &crate::ui::approval::AllowCall, window, cx| {
                    this.allow_by_key(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &crate::ui::approval::DenyCall, window, cx| {
                    this.deny_by_key(window, cx)
                }),
            )
            .capture_key_up(cx.listener(|this, event: &KeyUpEvent, _, _| this.card_key_up(event)))
            // Last, after every binding had its chance: a key that typed
            // nothing where the focus is goes to the composer.
            .on_key_down(cx.listener(Self::type_into_composer))
            // The ring is for the keyboard; a click puts the keys elsewhere.
            .capture_any_mouse_down(cx.listener(|this, _, _, cx| {
                if this.region.take().is_some() {
                    cx.notify();
                }
            }))
            .when(welcome, |this| {
                this.child(
                    TitleBar::new()
                        .bg(cx.theme().background)
                        .border_color(cx.theme().background),
                )
                .child(self.setup_page(window, cx))
            })
            .when(!welcome, |this| {
                this.child(
                    div()
                        .relative()
                        .flex_shrink_0()
                        .child(self.title_bar(window, cx))
                        .children(self.grain(&self.grains.title_bar)),
                )
            })
            .when(!welcome, |this| {
                this.child(
                    h_flex()
                        .flex_1()
                        .min_h_0()
                        .when(self.sidebar_open, |this| {
                            let sidebar = self.sidebar(window, cx).into_any_element();
                            // In from the left edge, in a box only while it moves.
                            this.child(if sidebar_in < 1. {
                                let moving = div().h_full().flex_shrink_0().relative();
                                crate::ui::motion::slide(moving, sidebar_in, -1.)
                                    .child(sidebar)
                                    .into_any_element()
                            } else {
                                sidebar
                            })
                        })
                        .child(main)
                        .children(self.right_panel(window, cx)),
                )
            })
            .children(self.settings_overlay(window, cx))
            .children(self.switcher(cx))
            // Last, over everything, as the colours of a new theme come in;
            // not in a frame read for the blur behind settings, which is to
            // show the window in them.
            .children(
                (!self.settings_reading)
                    .then(|| crate::ui::motion::veil_layer(window, cx))
                    .flatten(),
            )
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

/// Name an untitled session after its first prompt, but leave a name that
/// was set deliberately, such as a scheduled job's.
pub(crate) fn name_after(conversation: &mut Conversation, prompt: &str) {
    if conversation.messages.is_empty()
        && (conversation.title.is_empty() || conversation.title == "New session")
    {
        let saved = letronna_core::workflow::saved::typed(prompt);
        let shown = letronna_core::init::typed(prompt).or(saved.as_deref());
        let shown = shown.unwrap_or(prompt);
        conversation.title = letronna_core::store::name_from(shown);
    }
}

/// Whether a session is a new one nothing was said in yet: open in the
/// window, but listed nowhere and kept off the disk until its first message.
pub(crate) fn untouched(conversation: &Conversation) -> bool {
    conversation.messages.is_empty()
        && !conversation.unfilled()
        && conversation.title == "New session"
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

/// What an added command gives.
pub(crate) enum Given {
    /// Text for the composer, read before it is sent.
    Text(String),
    /// A saved workflow's prompt, sent at once with the `workflow` tool
    /// granted for its turn.
    Run(String),
}

/// Hands Crew what was said from `source` as a turn starts in `session`,
/// putting the notices it gives in the inbox ahead of the prompt; any such
/// prompt ends what a saved workflow's command granted.
pub(crate) fn tell_crew(session: u64, said: &str, source: crew::Source) {
    for notice in crew::prompt(session, said, source) {
        inbox::push(session, notice);
    }
}

#[cfg(test)]
mod tests {
    use super::woken;
    use letronna_core::gate::Mode;
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{
        card_is_up, cuts_in, escape_closes_aside, front_call, name_after, run_ended, running_calls,
        side_question, until_repeated_id, untouched,
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

    #[test]
    fn a_new_session_is_named_after_its_first_prompt_only() {
        use letronna_core::chat::Message;
        use letronna_core::store::Conversation;
        let mut new = Conversation {
            title: "New session".into(),
            ..Default::default()
        };
        name_after(&mut new, "fix the login page so it remembers me");
        assert_eq!(new.title, "fix the login page so it remembers me");

        // Once something was said, a later prompt leaves the name.
        new.messages.push(Message::default());
        name_after(&mut new, "and the logout");
        assert_eq!(new.title, "fix the login page so it remembers me");

        // As does a name given on purpose, as a scheduled job's.
        let mut named = Conversation {
            title: "Morning digest".into(),
            ..Default::default()
        };
        name_after(&mut named, "what is new");
        assert_eq!(named.title, "Morning digest");
    }

    #[test]
    fn only_a_new_session_nothing_was_said_in_is_opened_again() {
        use letronna_core::chat::Message;
        use letronna_core::store::Conversation;
        let blank = || Conversation {
            title: "New session".into(),
            ..Default::default()
        };
        assert!(untouched(&blank()));
        let said = Conversation {
            messages: vec![Message::default()],
            ..blank()
        };
        assert!(!untouched(&said));
        // Its messages let go of, it still has them on disk.
        let let_go = Conversation {
            counted: 2,
            ..blank()
        };
        assert!(!untouched(&let_go));
        // A name given on purpose, as a scheduled job's.
        let named = Conversation {
            title: "Morning digest".into(),
            ..Default::default()
        };
        assert!(!untouched(&named));
    }
}
