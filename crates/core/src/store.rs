//! Sessions on disk.
//!
//! One append-only file per session, `sessions/<id>.jsonl`, a JSON record per
//! line, rewritten whole only to take back what a rewind cut. Everything
//! shared between sessions lives in `settings.json`, and
//! `sessions/index.json` caches what each file sums up to for the listing.
//!
//! The shape matters because two Letronnas will run at once -- the window and
//! the terminal. A single file holding every session, rewritten whole on each
//! save, means whichever process writes last erases the other's work. Here a
//! session is appended to, and only by whoever is running it, so
//! two of them never tread on each other. A crash mid-write costs at most the
//! last line instead of the whole history.

use crate::chat::{self as backend, Message};
use serde::{Deserialize, Serialize};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;

/// Starts the name of every side question's conversation, which only the
/// sweep of them looks for.
const ASIDE: &str = "btw-";
/// How old a side question's conversation is before the sweep takes it: an
/// aside makes one request, which never takes this long, so an older one's
/// process is gone, while a younger one may be another process's, running.
const ASIDE_STALE: std::time::Duration = std::time::Duration::from_secs(60 * 60);

#[derive(Default, Serialize, Deserialize)]
pub struct Conversation {
    /// Stable identity, so a streaming reply lands in the session it was sent
    /// from even if the user switches or deletes sessions meanwhile.
    #[serde(default)]
    pub id: u64,
    pub title: String,
    pub messages: Vec<Message>,
    #[serde(default)]
    pub updated: u64,
    #[serde(default)]
    pub pinned: bool,
    /// Messages the file holds, counted while listing the sessions and
    /// before any of them are read. Only meaningful while `messages` is
    /// empty; ask `length()` rather than reading this.
    #[serde(skip)]
    pub counted: usize,
    /// Project folder attached to this session, if any.
    #[serde(default)]
    pub workspace: Option<PathBuf>,
    /// The git worktree made for the session from the folder it was given,
    /// which `workspace` is then in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<crate::worktree::Worktree>,
    /// The model's own checklist for the work in progress.
    #[serde(default)]
    pub todos: Vec<Todo>,
    /// The model this session talks to, with `gateway`; `None` until it is
    /// chosen or the session first sends, and in every session from before
    /// sessions had their own, which follow the settings' instead.
    #[serde(default)]
    pub model: Option<String>,
    /// The gateway `model` is on; the two are only ever set together.
    #[serde(default)]
    pub gateway: Option<String>,
    /// How hard the model thinks in this session; `None` follows the settings.
    #[serde(default)]
    pub effort: Option<String>,
    /// What the session keeps working toward after each turn, if anything.
    #[serde(default)]
    pub goal: Option<crate::goal::Goal>,
    /// When the model saves to memory in this session; `None` follows the
    /// settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_saving: Option<crate::memory::Saving>,
    /// Where this session keeps memory and what of it loads; `None` follows
    /// the settings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<crate::memory::Scope>,
    /// Messages already on disk. Everything past this is appended on save.
    /// Only `flush` and `rewind` should touch it; it is visible because
    /// callers build a Conversation with `..Default::default()`.
    #[serde(skip)]
    pub flushed: usize,
    /// The descriptive fields as last written, so save appends a `meta` line
    /// only when one of them actually changed.
    #[serde(skip)]
    pub written: Option<Meta>,
    /// A rewind not yet on disk: how many messages the session was cut back
    /// to. Only `rewind` and `flush` should touch it; it is visible for the
    /// same reason as `flushed`.
    #[serde(skip)]
    pub rewound: Option<usize>,
    /// Deleted by the other Letronna while this one had it open, so it is
    /// never written again, even once a new session takes its id. Only
    /// `flush` should touch it; it is visible for the same reason as `flushed`.
    #[serde(skip)]
    pub gone: bool,
    /// The newest summary the model made of the session's first messages:
    /// how many of them it covers, and the summary. Kept in the file, so a
    /// restart does not ask for it again.
    #[serde(skip)]
    pub summary: Option<(usize, String)>,
    /// Whether `summary` is in the file yet. Only the store should touch it;
    /// it is visible for the same reason as `flushed`.
    #[serde(skip)]
    pub summary_saved: bool,
    /// Where this copy stands in its file, so what another process adds
    /// there can be told apart and followed. Set by reading the file and
    /// kept by saving; a copy without it, as the terminal's are, is saved
    /// blind to the other's lines, as before. Only the store should touch
    /// it; it is visible for the same reason as `flushed`.
    #[serde(skip)]
    pub seen: Option<Seen>,
    /// Set when a save found another process had added messages here too,
    /// and kept this copy's over theirs, so the window can say so.
    #[serde(skip)]
    pub diverged: bool,
    /// Where this session came from, for one imported from another tool, or
    /// from a Letronna export, rather than started here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub imported_from: Option<crate::import::Source>,
    /// The summaries the user asked for that still hold, oldest first: how
    /// many of the first messages each covers, and the summary. The context
    /// window never starts before the newest one's end (`floor`).
    #[serde(skip)]
    pub asked: Vec<(usize, String)>,
    /// The model call that made the newest of them, until the file has it.
    /// Only the store should touch it; it is visible for the same reason as
    /// `flushed`.
    #[serde(skip)]
    pub call: Option<Call>,
    /// What Auto mode's judge made of calls in this session, oldest first.
    #[serde(default)]
    pub decisions: Vec<Judged>,
    /// Decisions already on disk; the rest are appended on save. Only the
    /// store should touch it; it is visible for the same reason as `flushed`.
    #[serde(skip)]
    pub decided: usize,
}

/// A summary read from a session's file, while the file is played.
struct Found {
    /// How many of the first messages it covers.
    upto: usize,
    text: String,
    /// Whether the user asked for it.
    manual: bool,
    /// For the user's, `compact::keys` of the messages it covers as its
    /// writer had them.
    key: Option<u64>,
}

impl Found {
    /// Whether it sums up the messages `keys` stand for, as its writer had
    /// them: always, but for one of the user's made from another view.
    fn holds(&self, keys: &[u64]) -> bool {
        self.key.is_none_or(|key| keys.get(self.upto) == Some(&key))
    }
}

/// A model call no reply carries -- a summary the user asked for -- kept
/// with its summary so the usage report counts it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Call {
    /// Seconds since 1970.
    pub at: u64,
    pub model: String,
    pub usage: crate::usage::Usage,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Meta {
    pub title: String,
    pub updated: u64,
    pub pinned: bool,
    pub workspace: Option<PathBuf>,
    pub todos: Vec<Todo>,
    // Left out while unset, so a session that never chose writes the same
    // meta line as before it could.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gateway: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<crate::goal::Goal>,
    pub imported_from: Option<crate::import::Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<crate::worktree::Worktree>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_saving: Option<crate::memory::Saving>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<crate::memory::Scope>,
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Todo {
    pub text: String,
    /// "pending", "doing" or "done".
    pub status: String,
}

/// Replace `todos` with the checklist a `todo` call sends and answer the
/// call with how far along it is; a call that cannot be read changes nothing.
pub fn apply_todo(todos: &mut Vec<Todo>, arguments: &str) -> Result<String, String> {
    *todos = parse_todos(arguments)?;
    let done = todos.iter().filter(|t| t.status == "done").count();
    Ok(format!(
        "Checklist updated: {done} of {} done.",
        todos.len()
    ))
}

/// The checklist the last readable `todo` call in `messages` set, or none.
pub fn todos_in(messages: &[Message]) -> Vec<Todo> {
    // One that cannot be read changed nothing, as `apply_todo` has it, so the
    // one before it still stands.
    messages
        .iter()
        .rev()
        .flat_map(|m| m.tool_calls.iter().rev())
        .filter(|call| call.name == crate::tools::TODO)
        .find_map(|call| parse_todos(&call.arguments).ok())
        .unwrap_or_default()
}

/// The checklist a `todo` call sets, from its arguments.
fn parse_todos(arguments: &str) -> Result<Vec<Todo>, String> {
    let args = crate::tools::parse_args_for(crate::tools::TODO, arguments)?;
    let items = args["items"]
        .as_array()
        .ok_or_else(|| "`items` is required.".to_owned())?;
    Ok(items
        .iter()
        .map(|item| Todo {
            text: item["text"].as_str().unwrap_or_default().to_owned(),
            status: item["status"].as_str().unwrap_or("pending").to_owned(),
        })
        .filter(|todo| !todo.text.is_empty())
        .collect())
}

/// What Auto mode's judge made of a call, kept in the session file so the
/// list of them outlives the program.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Judged {
    /// The call's id and the place its result takes among the messages:
    /// models that send no ids get `call_0` and so on in every reply.
    pub call_id: String,
    pub at: usize,
    /// The call as its tool line shows it, such as `run_command(cargo test)`.
    pub call: String,
    /// Its tag, such as "auto: allowed", or "stopped" when the user stopped
    /// it before the judge answered.
    pub tag: String,
}

impl Judged {
    /// `tag` for the call with `call_id`, shown as `call`, one of the calls
    /// of the last reply in `messages` that made it.
    pub fn of(messages: &[Message], call_id: &str, call: String, tag: String) -> Self {
        // Results follow the reply that asked for them, in the order it
        // made its calls.
        let at = messages
            .iter()
            .enumerate()
            .rev()
            .find_map(|(i, message)| {
                let place = message.tool_calls.iter().position(|c| c.id == call_id)?;
                Some(i + 1 + place)
            })
            .unwrap_or(messages.len());
        Self {
            call_id: call_id.to_owned(),
            at,
            call,
            tag,
        }
    }
}

/// A line in a session file.
#[derive(Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
// Made one at a time, for the line being read or written, so the size of
// the largest costs nothing.
#[allow(clippy::large_enum_variant)]
enum Record {
    /// Written once, when the session is created.
    Head {
        id: u64,
        created: u64,
    },
    /// The descriptive fields; the last one in the file wins.
    Meta(Meta),
    Message(Message),
    /// From here on the session is its first `to` messages. Written where
    /// both Letronnas said something and this copy's history wins, and by
    /// builds from before a rewind rewrote the file; still read.
    Rewind {
        to: usize,
    },
    /// The usage of a reply a rewind took out of the file, kept for the
    /// costs since it was paid for all the same. Never part of the session.
    Paid(Message),
    /// A summary the model made of the first `upto` messages, which stands
    /// in for them once the context window no longer holds them. The newest
    /// one a later rewind has not cut into is used. `manual` when the user
    /// asked for it, which the context window then never starts before.
    Summary {
        upto: usize,
        text: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        manual: bool,
        /// For the user's, `compact::keys` of the messages it covers as its
        /// writer had them: one made from a view of the session another
        /// Letronna has since changed does not match the file, and is not used.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        key: Option<u64>,
        /// The call that made the user's newest summary, which no reply
        /// carries, written with the first summary saved after it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call: Option<Call>,
    },
    /// A judge decision, appended as it is made.
    Auto(Judged),
}

/// Everything that is not a session.
#[derive(Serialize, Deserialize)]
pub struct Settings {
    pub endpoint: String,
    pub model: String,
    #[serde(default)]
    pub gateway: String,
    /// The URL pasted for the Custom gateway. Kept apart from `endpoint`,
    /// which follows whichever gateway the current model is on.
    #[serde(default)]
    pub custom_endpoint: String,
    #[serde(default)]
    pub presence: String,
    /// The mode a session starts in. Saves from before the modes kept
    /// "Skip tool approvals" here, which reads as Bypass when it was on.
    #[serde(default, alias = "skip_approvals")]
    pub mode: crate::gate::Mode,
    /// How hard the model should think: empty, or a level such as low or max;
    /// a model without that level is sent the nearest one it has.
    #[serde(default)]
    pub effort: String,
    /// The output style replies are written in, by name; empty, or a name
    /// no style has, for none.
    #[serde(default)]
    pub style: String,
    /// Show the model's thinking as it arrives, rather than only its answer.
    #[serde(default)]
    pub thinking: bool,
    /// Let the gateway search the web.
    #[serde(default)]
    pub search: bool,
    /// Which session was open last, by id rather than position.
    #[serde(default)]
    pub active_id: u64,
    /// Only look for updates when asked.
    #[serde(default)]
    pub manual_updates: bool,
    /// Every session shares one browser, its sign-ins remembered, rather
    /// than each having a private one, forgotten when Letronna quits.
    #[serde(default)]
    pub browser_shared: bool,
    /// The agent browses unseen: the browser panel does not open by itself
    /// when it uses the browser, though it can still be opened to look.
    #[serde(default)]
    pub browse_unseen: bool,
    /// A page or drawing the agent writes opens beside the chat by itself
    /// and follows every change to it; off, it waits in the window's
    /// Preview tab to be looked at.
    #[serde(default = "yes")]
    pub preview_opens_by_itself: bool,
    /// The window's colours: empty for Night, "paper", or "system" to follow
    /// the computer's light or dark mode.
    #[serde(default)]
    pub appearance: String,
    /// The terminal's colours: empty for the dark ones it has always used,
    /// or "auto" (light or dark as the terminal's background is), "light",
    /// "dark-colour-blind", "light-colour-blind" or "ansi" (the terminal's
    /// own sixteen).
    #[serde(default)]
    pub terminal_theme: String,
    /// A faint film grain on the window's chrome and empty pages, never over
    /// the transcript, code or diffs.
    #[serde(default = "yes")]
    pub grain: bool,
    /// Dialogs, panels, new rows and a new theme ease in and out; always off
    /// while the computer asks for reduced motion.
    #[serde(default = "yes")]
    pub motion: bool,
    /// How many steps larger the window draws its text than the usual size,
    /// or smaller below zero, as Cmd+= and Cmd+- step it.
    #[serde(default)]
    pub text_step: i8,
    /// Tell the computer when a run ends or needs an answer while nobody is
    /// looking at the window or the terminal.
    #[serde(default = "yes")]
    pub notifications: bool,
    /// The most tokens a request carries besides the reply, its system prompt
    /// and tools included, however much the model could take; 0 keeps the
    /// default of 64,000, and set however low the conversation still gets
    /// 6,000. Edited by hand with Letronna closed, since an open window or
    /// terminal saves over it.
    #[serde(default)]
    pub context_cap: u64,
    /// The window, in tokens, assumed for a model whose list and family say
    /// nothing of one, such as a model behind vLLM or a LiteLLM proxy; 0
    /// keeps the small window of 6,000, a limit an error gives away still
    /// lowers it, and `LETRONNA_CONTEXT_WINDOW` sets it for one run. Edited
    /// by hand with Letronna closed, as `context_cap` is.
    #[serde(default)]
    pub context_window: u64,
    /// The longest reply, in tokens, asked of a model whose output limit is
    /// not known; 0 keeps the usual 8,192, which is also the most it gets
    /// while its window is not known or set, and `LETRONNA_MAX_OUTPUT` sets
    /// it for one run. Edited by hand with Letronna closed.
    #[serde(default)]
    pub max_output: u64,
    /// How the reasoning a model such as DeepSeek or Kimi wants back with
    /// its tool calls goes out: empty or "full" sends every call of the
    /// turn in progress its own; "chunked" sends an empty one with the
    /// turn's calls older than the newest two once their reasoning comes to
    /// 8,000 tokens, and then a chunk of that at a time, so the requests in
    /// between keep the provider's prompt cache. Off until paired runs show
    /// what a model loses with its older thoughts; `LETRONNA_REASONING_REPLAY`
    /// sets it for one run. Edited by hand with Letronna closed.
    #[serde(default)]
    pub reasoning_replay: String,
    /// Whether a helper of a run nobody watches is sent the tools its
    /// session's own run is, less those it could never call, so the two
    /// share the provider's cached prefix: on unless this says "off", which
    /// `LETRONNA_HELPERS_SHARE_TOOLS` says for one run, to compare the two.
    #[serde(default)]
    pub helpers_share_tools: String,
    /// Whether a run nobody watches runs at once the commands of one reply
    /// that only read, as it runs its file reads: "on" turns it on. Off
    /// until paired runs show it sound; `LETRONNA_PARALLEL_READ_COMMANDS`
    /// sets it for one run.
    #[serde(default)]
    pub parallel_read_commands: String,
    /// The model helpers use when neither the call nor their type names one;
    /// empty for the model of the agent that starts them.
    #[serde(default)]
    pub helper_model: String,
    /// The model summaries, suggested commit messages and goal checks run
    /// on, on the session's gateway; empty for the session's own. See `side`.
    #[serde(default)]
    pub side_model: String,
    /// Whether a model names a session once its first reply is in; off, a
    /// session keeps the first line of what was asked. See `title`.
    #[serde(default = "yes")]
    pub auto_title: bool,
    /// The model that names sessions: empty to choose one automatically,
    /// `chat` for the session's own, or a model on the session's gateway.
    /// See `title`.
    #[serde(default)]
    pub title_model: String,
    /// How many helpers may run at once across Letronna; 0 for the usual 8.
    #[serde(default)]
    pub helpers_at_once: usize,
    /// How deep helpers may nest, a helper's own helper being depth 2; 0 for
    /// the usual 2.
    #[serde(default)]
    pub helper_depth: u8,
    /// Tokens the helpers one agent call starts may spend between them; 0 for
    /// the usual 500k.
    #[serde(default)]
    pub helper_budget: u64,
    /// Tools never offered to the model nor run, by name, an MCP server's by
    /// its full `mcp__server__tool` name; a name no tool has is passed over.
    #[serde(default, deserialize_with = "names")]
    pub disabled_tools: Vec<String>,
    /// Offers the `computer` tool, which sees and uses other apps' windows,
    /// asking once for each app; only where Letronna has a backend for the
    /// system. Edited by hand with Letronna closed, until the window has a
    /// switch for it.
    #[serde(default)]
    pub computer_use: bool,
    /// Offers the `script` tool, an experiment that runs JavaScript calling
    /// the read-only tools in one step; read as each session starts, so a
    /// session's tools stay the same throughout. Edited by hand until the
    /// window has a switch for it.
    #[serde(default)]
    pub script_tool: bool,
    /// Read replies aloud as they stream and answer a chat app's voice note
    /// with one, off by default because the speech is paid from credit.
    #[serde(default)]
    pub voice: bool,
    /// Where keys and tokens are kept on macOS and Windows: empty or "vault"
    /// for the Keychain or Credential Manager, or "files" for keys.json and
    /// the other files, which the next start moves them all back into, as
    /// an older Letronna needs. Edited by hand.
    #[serde(default)]
    pub keys_in: String,
    /// Send a turn that changed files back once to build or test them, when
    /// it is about to finish with no command run since.
    #[serde(default = "yes")]
    pub verify_edits: bool,
    /// Lets the model make several tool calls in one reply and run them
    /// together; false asks every request for one and runs them one at a
    /// time, as some benchmarks' graders require.
    #[serde(default = "yes")]
    pub parallel_tool_calls: bool,
    /// Ends each request of a turn that has made fifteen tool rounds with
    /// the rules still in force, such as checking edits before saying done;
    /// off until paired runs on long tasks show it costs nothing. Edited by
    /// hand.
    #[serde(default)]
    pub remind_rules: bool,
    /// Plan mode at the terminal or the window offers every tool, refusing
    /// at the call those that change things, and gives its rules at the end
    /// of each request instead of in the system message, so switching it
    /// keeps the provider's cached prefix. Off until paired runs show models
    /// keep to it; edited by hand with Letronna closed.
    #[serde(default)]
    pub plan_keeps_tools: bool,
    /// Models to carry a task on with, in order, when the chosen one is
    /// overloaded, out of credit or gone: `model` on the session's gateway,
    /// or `Gateway:model`; see `fallback`.
    #[serde(default, deserialize_with = "names")]
    pub fallbacks: Vec<String>,
    /// The order accounts are reached for when one is rate limited or out of
    /// quota, by tier: "subscription", "api_key", "credit", "free". Tiers left
    /// out follow in their usual order; see `providers::router`.
    #[serde(default)]
    pub provider_order: Vec<String>,
    /// When an account reaches its limit, go on with the same model on the
    /// next account of the same provider, in the order the accounts are
    /// kept; see `providers::accounts`. On unless turned off.
    #[serde(default = "yes")]
    pub switch_accounts: bool,
    /// Serve the connected accounts on 127.0.0.1 as an OpenAI-compatible
    /// endpoint; see `providers::local`. Off unless turned on.
    #[serde(default)]
    pub local_endpoint: bool,
    /// The port it listens on; 0 for `providers::local::DEFAULT_PORT`.
    #[serde(default)]
    pub local_port: u16,
    /// Let the phone app reach this computer through Letronna's relay,
    /// encrypted end to end; see `phone`. Off unless turned on.
    #[serde(default)]
    pub phone: bool,
    /// While `phone` is on, also let a phone on the same tailnet connect
    /// straight to this computer over Tailscale; see `phone::direct`. Off
    /// unless turned on.
    #[serde(default)]
    pub phone_direct: bool,
    /// The port it listens on; 0 for `phone::direct::PORT`.
    #[serde(default)]
    pub phone_port: u16,
    /// The sidebar's list narrowed to one state: "needs_you", "working",
    /// "unseen" or "pinned"; empty for no filter.
    #[serde(default)]
    pub session_filter: String,
    /// Group the sidebar's sessions by their project folder instead of by
    /// how long ago each was updated.
    #[serde(default)]
    pub group_by_project: bool,
    /// The global shortcut that opens the quick entry bar, e.g.
    /// "ctrl+shift+space". Empty until the window fills in its default.
    #[serde(default)]
    pub quick_entry_hotkey: String,
    /// A command whose first line of output the terminal shows under its
    /// footer, given the session as JSON on its input, as Claude Code's
    /// statusLine is; empty for none.
    #[serde(default)]
    pub status_line: String,
    /// How big a workflow Crew suggests: small, medium or large; empty for
    /// medium.
    #[serde(default)]
    pub crew: String,
    /// Sessions untouched for longer than this many days are deleted when
    /// Letronna starts with no other one open, at most once a day; 0 keeps
    /// them forever.
    #[serde(default)]
    pub keep_days: u32,
    /// Every few of the user's messages, look back over the session and add
    /// what is worth keeping to memory. Off unless chosen, since each look is
    /// a model call on the user's account, and only while memory is saved
    /// automatically.
    #[serde(default)]
    pub memory_review: bool,
    /// When the model saves to memory in a session that has not chosen for
    /// itself. A file from before the choice reads as only when asked, so an
    /// update stops the saves nobody asked for.
    #[serde(default)]
    pub memory_saving: crate::memory::Saving,
    /// Where memory is kept, and what of it loads, in a session that has not
    /// chosen for itself: everywhere, as before the choice, unless chosen.
    #[serde(default)]
    pub memory_scope: crate::memory::Scope,
    /// Search with DuckDuckGo, which needs no key, while no search service
    /// is chosen, after the gateway's search where it sells one; switched
    /// off, search_web searches through that gateway alone, or with none
    /// says how to turn it back on or add a key.
    #[serde(default = "yes")]
    pub keyless_search: bool,
    /// Which of Letronna's own web searches a search through the account
    /// asks for: "auto", its free search first and the paid one, from
    /// credit, when that finds nothing; "free" alone, using no credit; or
    /// "paid" alone. Empty, or anything else, is "auto".
    #[serde(default)]
    pub letronna_search: String,
    /// Name Letronna as co-author of what a session here commits, and put
    /// its mark on a pull request one opens. Letronna Cloud's sessions
    /// follow the account's choice instead (`Account::letronna_coauthor`).
    #[serde(default = "yes")]
    pub letronna_coauthor: bool,
    /// Answer with a model downloaded to this computer when the hosted one
    /// cannot be reached, rather than not at all.
    #[serde(default = "yes")]
    pub local_stand_in: bool,
    /// How find_tools ranks the tools it finds: "auto" or empty for Jev
    /// where it can be reached and the chat is hosted anyway, "jev" for Jev
    /// wherever it can be reached, "names" for the words alone (`jev`).
    #[serde(default)]
    pub tool_search: String,
    /// Where Jev is asked: empty for the user's Letronna account, or a
    /// System One address of their own, with its key in the `Jev` slot.
    #[serde(default)]
    pub jev_endpoint: String,
    /// The Jev model asked; empty for jev-latest.
    #[serde(default)]
    pub jev_model: String,
}

// Written out rather than derived so the film grain, motion, notifications,
// previews opening by themselves and keyless search start on for a first
// run too, as they do for a settings file from before them.
impl Default for Settings {
    fn default() -> Self {
        Settings {
            endpoint: String::new(),
            model: String::new(),
            context_cap: 0,
            context_window: 0,
            max_output: 0,
            reasoning_replay: String::new(),
            helpers_share_tools: String::new(),
            parallel_read_commands: String::new(),
            helper_model: String::new(),
            side_model: String::new(),
            auto_title: true,
            title_model: String::new(),
            helpers_at_once: 0,
            helper_depth: 0,
            helper_budget: 0,
            disabled_tools: Vec::new(),
            keep_days: 0,
            computer_use: false,
            script_tool: false,
            keys_in: String::new(),
            fallbacks: Vec::new(),
            provider_order: Vec::new(),
            switch_accounts: true,
            local_endpoint: false,
            phone: false,
            phone_direct: false,
            phone_port: 0,
            local_port: 0,
            memory_review: false,
            memory_saving: crate::memory::Saving::WhenAsked,
            memory_scope: crate::memory::Scope::Auto,
            keyless_search: true,
            letronna_search: String::new(),
            letronna_coauthor: true,
            local_stand_in: true,
            tool_search: String::new(),
            jev_endpoint: String::new(),
            jev_model: String::new(),
            gateway: String::new(),
            custom_endpoint: String::new(),
            presence: String::new(),
            mode: crate::gate::Mode::Default,
            effort: String::new(),
            style: String::new(),
            thinking: false,
            search: false,
            active_id: 0,
            manual_updates: false,
            browser_shared: false,
            browse_unseen: false,
            preview_opens_by_itself: true,
            appearance: String::new(),
            terminal_theme: String::new(),
            grain: true,
            motion: true,
            text_step: 0,
            notifications: true,
            voice: false,
            verify_edits: true,
            parallel_tool_calls: true,
            remind_rules: false,
            plan_keeps_tools: false,
            session_filter: String::new(),
            group_by_project: false,
            quick_entry_hotkey: String::new(),
            status_line: String::new(),
            crew: String::new(),
        }
    }
}

impl Settings {
    /// Whether turns check their edits: switched on, and with `run_command`
    /// left on, since the check is asked for with it.
    pub fn verifies_edits(&self) -> bool {
        self.verify_edits && !crate::tools::switched_off("run_command", &self.disabled_tools)
    }
}

fn yes() -> bool {
    true
}

/// A list of names as written by hand: one name alone is a list of one, and
/// any other shape is an empty list rather than failing the whole file,
/// which would be moved aside as unreadable.
fn names<'de, D: serde::Deserializer<'de>>(names: D) -> Result<Vec<String>, D::Error> {
    Ok(match serde_json::Value::deserialize(names)? {
        serde_json::Value::String(name) => vec![name],
        serde_json::Value::Array(names) => names
            .into_iter()
            .filter_map(|name| name.as_str().map(str::to_owned))
            .collect(),
        _ => Vec::new(),
    })
}

/// Settings that are chosen as one and so saved as one: a model id belongs
/// to its gateway and that gateway's endpoint.
const CHOSEN_TOGETHER: [&str; 3] = ["gateway", "model", "endpoint"];

#[derive(Default)]
pub struct Store {
    pub conversations: Vec<Conversation>,
    pub active: usize,
    pub settings: Settings,
    /// The settings as JSON when last read or written, so a save puts back
    /// only the fields changed here since; only loading, `save`,
    /// `choose_style` and `follow_custom` touch it.
    pub settings_written: serde_json::Value,
}

/// A session's name from what was said in it: its first line with words,
/// at most 40 characters, or "Untitled session" when nothing has words (a
/// picture alone, a blank line).
pub fn name_from(text: &str) -> String {
    match text.lines().map(str::trim).find(|line| !line.is_empty()) {
        Some(line) => line.chars().take(40).collect(),
        None => "Untitled session".into(),
    }
}

impl Conversation {
    /// The name to show: never blank, never starting on an empty line,
    /// whatever an older version kept.
    pub fn shown_title(&self) -> String {
        name_from(&self.title)
    }

    /// How long the conversation is, whether or not it has been read off the
    /// disk yet. Once it is in memory the messages are the truth; before
    /// that, the count taken while listing is.
    pub fn length(&self) -> usize {
        if self.messages.is_empty() {
            self.counted
        } else {
            self.messages.len()
        }
    }

    /// Put `message` just before the newest message, on disk too once it is
    /// saved, as what has to be read ahead of the user's prompt is.
    pub fn insert_before_last(&mut self, message: Message) {
        let at = self.messages.len().saturating_sub(1);
        self.messages.insert(at, message);
        self.write_again_from(at);
    }

    /// Take the newest message back, on disk too once it is saved, as a
    /// prompt a hook kept from the model is: nothing was done with it, so
    /// none of what a rewind undoes needs undoing.
    pub fn retract_last(&mut self) -> Option<Message> {
        let last = self.messages.pop()?;
        self.write_again_from(self.messages.len());
        Some(last)
    }

    /// Has the next save write the file again from message `at` on, as a
    /// rewind's does.
    fn write_again_from(&mut self, at: usize) {
        self.counted = self.counted.min(at);
        if self.flushed > at {
            self.flushed = at;
            self.rewound = Some(at);
        }
    }

    /// Take the conversation back to its first `len` messages, on disk too
    /// once it is saved.
    pub fn rewind(&mut self, len: usize) {
        if len >= self.messages.len() {
            return;
        }
        // Helpers the turns taken back set going would report into a
        // conversation that no longer asks for it, and keep changing files
        // a rewind may just have put back.
        crate::agents::stop_started_in(self.id, &self.messages[len..]);
        self.messages.truncate(len);
        // Replies from here on take the places of those taken back.
        crate::filestate::forget(self.id);
        crate::compact::forget(self.id, len);
        // Rewound to nothing, the messages are empty and the listing's count
        // is read again: left as it was, `length` would report the old
        // history and `fill` would read it back in.
        self.counted = self.counted.min(len);
        // The next save rewrites the file without what was taken back, and
        // appends anything said from here on. Rewinding again before a save
        // only cuts further.
        if self.flushed > len {
            self.flushed = len;
            self.rewound = Some(len);
        }
        // A summary of messages taken back no longer sums up the session.
        // The one in the file goes when the save cuts it.
        if self.summary.as_ref().is_some_and(|(upto, _)| *upto > len) {
            self.summary = None;
        }
        // Nor do the user's, and the window goes back to an earlier one of
        // theirs that still holds, as replay does.
        self.asked.retain(|(upto, _)| *upto <= len);
    }

    /// Where the context window starts at the earliest: the end of the
    /// newest summary the user asked for, or 0.
    pub fn floor(&self) -> usize {
        self.asked.last().map_or(0, |(upto, _)| *upto)
    }

    /// Keep the summary the user asked for, which the context window never
    /// starts before from now on. An error, with nothing kept, when the
    /// session no longer starts with the messages it was made of: they were
    /// rewound or edited, or the session was deleted and its id reused.
    pub fn compacted(&mut self, done: crate::chat::Compacted) -> Result<(), String> {
        let covered = self.messages.get(..done.upto).filter(|_| done.upto > 0);
        if covered.is_none_or(|covered| crate::compact::keys(covered)[done.upto] != done.key) {
            return Err(
                "The session changed while it was being summarized, so every turn is kept.".into(),
            );
        }
        // Saved again even when the same text is kept, so the file says
        // the user asked for it.
        self.asked.retain(|(upto, _)| *upto < done.upto);
        self.asked.push((done.upto, done.text.clone()));
        self.summary = Some((done.upto, done.text));
        self.summary_saved = false;
        self.call = done.usage.map(|usage| Call {
            at: crate::chat::now(),
            model: done.model,
            usage,
        });
        Ok(())
    }

    /// Keep the summary the model made of the first `upto` messages, which
    /// the next save writes down. One covering more messages than the
    /// session now has was made for a history since taken back.
    pub fn summarized(&mut self, upto: usize, text: String) {
        let kept = Some((upto, text));
        if upto > self.messages.len() || self.summary == kept {
            return;
        }
        self.summary = kept;
        self.summary_saved = false;
    }

    /// The summary a copy of the first `len` messages starts with: this
    /// session's, when the copy holds everything it covers.
    pub fn summary_within(&self, len: usize) -> Option<(usize, String)> {
        self.summary.clone().filter(|(upto, _)| *upto <= len)
    }

    /// Whether the messages are on disk only: listed or let go, and not
    /// read back in since.
    pub fn unfilled(&self) -> bool {
        self.counted > 0 && self.messages.is_empty()
    }

    /// Take in the messages, summaries and decisions read from this
    /// session's file, unless it has been filled since the read began.
    pub fn filled_with(&mut self, (messages, summary, asked, decisions): Contents) {
        if !self.unfilled() {
            return;
        }
        // The file is the count from here. One the other Letronna rewound to
        // nothing would otherwise stay counted, and read again every time.
        self.counted = messages.len();
        self.messages = messages;
        self.summary = summary;
        self.asked = asked;
        self.summary_saved = true;
        self.decided = decisions.len();
        self.decisions = decisions;
        // What has been written is what was just read, so a flush appends
        // only what is said from here on rather than the whole history again.
        // `written` stays as the listing or the last save left it, so a
        // title changed since and not saved yet still is on the next save.
        self.flushed = self.messages.len();
    }

    /// Let go of the messages, which `fill` reads back when they are wanted
    /// again. Only a session with all of it on disk can; false otherwise.
    pub fn unload(&mut self) -> bool {
        let saved = self.flushed == self.messages.len()
            && self.decided == self.decisions.len()
            && self.rewound.is_none()
            && (self.summary.is_none() || self.summary_saved)
            && self.written.as_ref() == Some(&self.meta());
        // Deleted by the other Letronna, it is held nowhere else.
        if !saved || self.gone {
            return false;
        }
        self.counted = self.length();
        self.messages = Vec::new();
        // Counted from what is held, so a save before the next fill finds
        // nothing to append.
        self.flushed = 0;
        true
    }

    /// Start this copy of `source`'s first messages with the summaries of it
    /// the copy holds all of: the user's, so its window starts where the
    /// source's does, and for its file the newest -- the user's newest when
    /// there is one, so that a restart keeps the floor too.
    pub fn copy_summaries(&mut self, source: &Conversation) {
        let len = self.messages.len();
        self.asked = (source.asked.iter())
            .filter(|(upto, _)| *upto <= len)
            .cloned()
            .collect();
        self.summary = (self.asked.last().cloned()).or_else(|| source.summary_within(len));
        self.summary_saved = false;
    }
}

impl Store {
    /// An id no existing session uses. Ids also name files, so a collision
    /// would merge two histories; the folder is checked as well as memory.
    fn next_id(&self) -> u64 {
        let highest = self.conversations.iter().map(|c| c.id).max().unwrap_or(0);
        let on_disk = session_ids().into_iter().max().unwrap_or(0);
        highest.max(on_disk) + 1
    }

    /// Claim an id for a new session by creating its empty file, so two
    /// Letronnas starting sessions at the same moment never get the same one.
    pub fn reserve_id(&self) -> u64 {
        // The highest id plus one is only where the search starts: another
        // process can read the same folder in the same instant. Creating the
        // file with create_new is atomic, so exactly one of them gets it.
        let _ = std::fs::create_dir_all(sessions_dir());
        let mut id = self.next_id();
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(session_path(id))
            {
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => id += 1,
                // Claimed, or the folder cannot be written at all. Then the
                // session carries on in memory and its save reports the
                // failure, as it did before ids were claimed.
                _ => return id,
            }
        }
    }

    /// Give back the numbers of sessions opened here and never written to,
    /// removing their reserved files, which are still empty.
    pub fn release_unused(&self) {
        for conversation in &self.conversations {
            let path = session_path(conversation.id);
            let only_reserved = std::fs::metadata(&path).is_ok_and(|stat| stat.len() == 0);
            if conversation.written.is_none() && only_reserved {
                let _ = std::fs::remove_file(path);
            }
        }
    }

    /// List the sessions made on disk since this store was read, by a
    /// scheduled run or another Letronna, without reading them.
    pub fn pick_up_new(&mut self) {
        for summary in summaries() {
            if self.index_of(summary.id).is_none() {
                self.conversations.push(listed(summary));
            }
        }
    }

    /// Give every session an id. Older saves have none.
    pub fn assign_ids(&mut self) {
        let mut next = self.next_id();
        for c in self.conversations.iter_mut() {
            if c.id == 0 {
                c.id = next;
                next += 1;
            }
        }
    }

    pub fn index_of(&self, id: u64) -> Option<usize> {
        self.conversations.iter().position(|c| c.id == id)
    }

    /// The session already imported from this source, if any: importing it
    /// again opens that one rather than making a second.
    pub fn find_by_import(&self, tool: crate::import::Tool, session_id: &str) -> Option<usize> {
        self.conversations.iter().position(|c| {
            c.imported_from
                .as_ref()
                .is_some_and(|s| s.tool == tool && s.session_id == session_id)
        })
    }

    /// The gateway and model the session at `index` talks to: its own, or
    /// the settings' when it has none or its gateway is no longer offered.
    pub fn model(&self, index: usize) -> (&str, &str) {
        let own = self.conversations.get(index).and_then(|c| {
            let (gateway, model) = (c.gateway.as_deref()?, c.model.as_deref()?);
            // Custom with no address left can send nowhere, as the Claude
            // Code plugin left it, so a session held to it follows the
            // settings, as one on a gateway no longer offered does.
            let nowhere = gateway == "Custom" && self.settings.custom_endpoint.trim().is_empty();
            let offered = crate::gateway::offered(gateway) && !nowhere;
            (offered && !model.is_empty()).then_some((gateway, model))
        });
        own.unwrap_or((&self.settings.gateway, &self.settings.model))
    }

    /// How hard the model thinks in the session at `index`: its own level,
    /// or the settings' when it has none.
    pub fn effort(&self, index: usize) -> &str {
        self.conversations
            .get(index)
            .and_then(|c| c.effort.as_deref())
            .unwrap_or(&self.settings.effort)
    }

    /// When the model saves to memory in the session at `index`: its own
    /// choice, or the settings' when it has none.
    pub fn memory_saving(&self, index: usize) -> crate::memory::Saving {
        self.conversations
            .get(index)
            .and_then(|c| c.memory_saving)
            .unwrap_or(self.settings.memory_saving)
    }

    /// Have the session at `index` save memory as `chosen` says from its next
    /// call on, or as the settings say for `None`.
    pub fn choose_memory_saving(&mut self, index: usize, chosen: Option<crate::memory::Saving>) {
        let Some(conversation) = self.conversations.get_mut(index) else {
            return;
        };
        conversation.memory_saving = chosen;
        crate::memory::choose(conversation.id, chosen);
    }

    /// Where the session at `index` keeps memory, and what of it loads: its
    /// own choice, or the settings' when it has none.
    pub fn memory_scope(&self, index: usize) -> crate::memory::Scope {
        self.conversations
            .get(index)
            .and_then(|c| c.memory_scope)
            .unwrap_or(self.settings.memory_scope)
    }

    /// Have the session at `index` keep memory where `chosen` says, and load
    /// only that, from its next request on, or as the settings say for `None`.
    pub fn choose_memory_scope(&mut self, index: usize, chosen: Option<crate::memory::Scope>) {
        let Some(conversation) = self.conversations.get_mut(index) else {
            return;
        };
        conversation.memory_scope = chosen;
        crate::memory::choose_scope(conversation.id, chosen);
    }

    /// Make the session at `index` a private chat: it loads and keeps only
    /// its own notes, and saves to them only when the user asks.
    pub fn make_private(&mut self, index: usize) {
        self.choose_memory_scope(index, Some(crate::memory::Scope::Chat));
        self.choose_memory_saving(index, Some(crate::memory::Saving::WhenAsked));
    }

    /// Put the session at `index` on `model` at `gateway`, and start new
    /// sessions there too. Sessions with a model of their own keep it.
    pub fn choose_model(&mut self, index: usize, gateway: &str, model: &str) {
        self.settings.gateway = gateway.to_owned();
        self.settings.model = model.to_owned();
        if let Some(conversation) = self.conversations.get_mut(index) {
            conversation.gateway = Some(gateway.to_owned());
            conversation.model = Some(model.to_owned());
        }
    }

    /// Have the session at `index` think `effort` hard, and new sessions too.
    pub fn choose_effort(&mut self, index: usize, effort: &str) {
        self.settings.effort = effort.to_owned();
        if let Some(conversation) = self.conversations.get_mut(index) {
            conversation.effort = Some(effort.to_owned());
        }
    }

    /// Have replies written in the style called `name`, empty for none.
    ///
    /// Requests read the style from the file, which the other process may
    /// have changed since this one last wrote, so the pick is written even
    /// when this copy already holds it; `save` would otherwise skip it.
    pub fn choose_style(&mut self, name: &str) {
        self.settings.style = name.to_owned();
        if let Some(fields) = self.settings_written.as_object_mut() {
            fields.remove("style");
        }
    }

    /// Hold the session at `index` to the model and effort it runs on now,
    /// before it sends: a session that followed the settings would otherwise
    /// change model whenever another session chose one.
    pub fn pin(&mut self, index: usize) {
        let (gateway, model) = self.model(index);
        let (gateway, model) = (gateway.to_owned(), model.to_owned());
        let effort = self.effort(index).to_owned();
        let Some(conversation) = self.conversations.get_mut(index) else {
            return;
        };
        conversation.gateway = Some(gateway);
        conversation.model = Some(model);
        conversation.effort = Some(effort);
    }

    /// Append whatever is new in each session, then the settings. Returns the
    /// first failure, so the caller can say the history is not safe.
    pub fn save(&mut self) -> std::io::Result<()> {
        std::fs::create_dir_all(sessions_dir())?;
        for conversation in self.conversations.iter_mut() {
            conversation.flush()?;
        }
        self.settings.active_id = self
            .conversations
            .get(self.active)
            .map(|c| c.id)
            .unwrap_or(0);
        let now = serde_json::to_value(&self.settings)?;
        // Most saves are for a message, with no setting moved, and those are
        // known without reading the file.
        if now == self.settings_written {
            return Ok(());
        }
        // Only what moved here goes back, so a setting the other process
        // changed meanwhile is not put back the way this one last saw it.
        let mut then = self.settings_written.clone();
        // But a model goes back with the gateway it was chosen on, or the
        // file could pair it with the one the other process chose. Only a
        // new model or gateway does that: a send sets the endpoint again and
        // the Custom URL is edited on its own, and neither is a pick to put
        // back over the other process's.
        let picked = ["gateway", "model"]
            .iter()
            .any(|&f| then.get(f) != now.get(f));
        if picked && let Some(fields) = then.as_object_mut() {
            for field in CHOSEN_TOGETHER {
                fields.remove(field);
            }
            // A Custom model was listed from the Custom URL, so it goes too.
            if self.settings.gateway == "Custom" {
                fields.remove("custom_endpoint");
            }
        }
        crate::state::update::<Settings>(&settings_path(), |disk| {
            crate::state::merge(disk, &then, &now)
        })
        .map_err(std::io::Error::other)?;
        self.settings_written = now;
        Ok(())
    }

    /// Take the Custom address the other process saved since this one last
    /// read or wrote the settings, unless this one has a new address of its
    /// own not yet saved. Whether it changed.
    pub fn follow_custom(&mut self) -> bool {
        let disk = current_settings().custom_endpoint;
        let written = (self.settings_written.get("custom_endpoint"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_owned();
        if disk == written || self.settings.custom_endpoint != written {
            return false;
        }
        if let Some(fields) = self.settings_written.as_object_mut() {
            fields.insert("custom_endpoint".into(), disk.clone().into());
        }
        self.settings.custom_endpoint = disk;
        true
    }

    /// Forget a session and remove its file, its helpers' conversations,
    /// the copies kept of the files it wrote, and its lock file if it can
    /// take the lock.
    pub fn remove(&mut self, index: usize) {
        if index >= self.conversations.len() {
            return;
        }
        let gone = self.conversations.remove(index);
        erase(gone.id, Lock::take(&session_path(gone.id)).ok());
    }

    /// Take in what another process wrote to the sessions since this one
    /// last looked: what it added to sessions here, and sessions it started.
    /// Those `busy` names, and any with something here not yet saved, are
    /// left as they are. Whether anything changed.
    pub fn follow(&mut self, busy: impl Fn(u64) -> bool) -> bool {
        let mut changed = false;
        for conversation in &mut self.conversations {
            if !busy(conversation.id) {
                changed |= conversation.follow();
            }
        }
        let known: std::collections::HashSet<u64> =
            self.conversations.iter().map(|c| c.id).collect();
        let mut started: Vec<Conversation> = session_ids()
            .into_iter()
            .filter(|id| !known.contains(id))
            // Reserved and not yet saved: it belongs to whoever reserved it.
            .filter(|&id| std::fs::metadata(session_path(id)).is_ok_and(|m| m.len() > 0))
            .filter_map(|id| replay(&session_path(id)))
            .collect();
        started.sort_by_key(|c| c.updated);
        changed |= !started.is_empty();
        // At the end, where the newest go: the places of the others, which
        // the active session is known by, stay as they were.
        self.conversations.extend(started);
        changed
    }
}

/// Remove session `id`'s file and what is kept beside it for it. Its lock
/// file goes only with `lock` held on it.
fn erase(id: u64, lock: Option<Lock>) {
    // First, so a helper still at work stops, and what it was writing
    // is not left behind in the folder that goes next.
    crate::agents::forget(id);
    let path = session_path(id);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(crate::prefilter::path_for(&path));
    // A lock file someone else holds stays: removed, the next writer
    // would lock a new file beside them and both would write.
    if lock.is_some() {
        let _ = std::fs::remove_file(path.with_extension("lock"));
    }
    let _ = std::fs::remove_dir_all(helpers_dir(id));
    let _ = std::fs::remove_dir_all(crate::tools::results_dir(id));
    let _ = std::fs::remove_dir_all(crate::memory::chat_folder(id));
    // A new session can be given the same id.
    crate::filestate::forget(id);
    crate::compact::forget(id, 0);
    crate::history::forget(id);
    crate::lsp::forget(id);
    crate::tasklist::forget(id);
    crate::memory::forget(id);
    // Nor be woken by this one's wake-up. One that cannot be taken back now
    // goes at the next look, which finds the session gone, unless a new
    // session took the id by then.
    let _ = crate::wake::cancel(id);
    #[cfg(feature = "workflows")]
    crate::script::forget(id);
}

/// The file marking when old sessions were last looked for.
fn pruned_path() -> PathBuf {
    backend::data_dir().join("pruned")
}

/// The file every running Letronna holds shared from its start. A prune
/// takes it alone, so it only runs while no other Letronna is open: one
/// that is has the sessions listed, and would write on into one deleted
/// under it -- dropped as deleted elsewhere, or appended to the new session
/// that took its freed id.
fn running_path() -> PathBuf {
    backend::data_dir().join("running.lock")
}

/// This process's hold on the running file, kept for as long as it runs.
static RUNNING: std::sync::Mutex<Option<std::fs::File>> = std::sync::Mutex::new(None);

/// Delete the sessions untouched for longer than the settings' `keep_days`,
/// if that is set, this has not run in the last day and no other Letronna
/// is open, then hold the running file for as long as this one runs.
/// Called at startup, before the sessions are read, so none of them is open
/// yet but the one the settings say was open last and those in `keep`, the
/// ones this start was asked to open, which are kept.
pub fn prune_old(keep: &[u64]) {
    let mut held = RUNNING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Only a start calls this, before anything is read, so this process's
    // own earlier hold is not one to wait for.
    *held = None;
    let _ = std::fs::create_dir_all(backend::data_dir());
    let Ok(running) = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(running_path())
    else {
        return;
    };
    let settings = current_settings();
    let marker = pruned_path();
    let recent = std::fs::metadata(&marker)
        .and_then(|meta| meta.modified())
        .is_ok_and(|at| at.elapsed().is_ok_and(|age| age.as_secs() < DAY));
    // With another Letronna open this waits for a later start, unmarked.
    if settings.keep_days != 0 && !recent && running.try_lock().is_ok() {
        let _ = std::fs::write(&marker, b"");
        let open = [keep, &[settings.active_id]].concat();
        prune(settings.keep_days, backend::now(), &open);
        let _ = running.unlock();
    }
    // Waits out another start's prune, so nothing is read half deleted.
    if running.lock_shared().is_ok() {
        *held = Some(running);
    }
}

/// Seconds in a day.
const DAY: u64 = 24 * 60 * 60;

/// Delete every session last touched more than `keep_days` before `now`,
/// except those in `open`, the newest, pinned ones and any another process
/// is writing to. Gives the ids deleted.
fn prune(keep_days: u32, now: u64, open: &[u64]) -> Vec<u64> {
    let before = now.saturating_sub(u64::from(keep_days) * DAY);
    let sessions = summaries();
    // What `-c` and a window with no session open go on with, and what is
    // left after a long break or a clock that jumped ahead.
    let newest = sessions.first().map(|s| s.updated);
    let mut deleted = Vec::new();
    for summary in sessions {
        let path = session_path(summary.id);
        // The file's time as well as the meta's: a save that changed no
        // descriptive field still touched the session.
        let written = std::fs::metadata(&path).map_or(0, |stat| modified(&stat) / 1_000_000_000);
        if summary.pinned
            || open.contains(&summary.id)
            || Some(summary.updated) == newest
            || summary.updated.max(written) >= before
        {
            continue;
        }
        // Held means another Letronna is writing it right now, so it is in
        // use whatever its times say.
        let Ok(lock) = Lock::take(&path) else {
            continue;
        };
        erase(summary.id, Some(lock));
        deleted.push(summary.id);
    }
    deleted
}

impl Conversation {
    fn meta(&self) -> Meta {
        Meta {
            title: self.title.clone(),
            updated: self.updated,
            pinned: self.pinned,
            workspace: self.workspace.clone(),
            todos: self.todos.clone(),
            model: self.model.clone(),
            gateway: self.gateway.clone(),
            effort: self.effort.clone(),
            goal: self.goal.clone(),
            imported_from: self.imported_from.clone(),
            worktree: self.worktree.clone(),
            memory_saving: self.memory_saving,
            memory_scope: self.memory_scope,
        }
    }

    /// Append the head (first time), new messages and the descriptive fields
    /// if they have moved since the last write, after rewriting the file
    /// without what a rewind took back.
    fn flush(&mut self) -> std::io::Result<()> {
        if self.gone {
            return Ok(());
        }
        // A session nothing was said in yet is not one: it gets its file with
        // its first message, as the terminal's does, rather than being listed
        // empty everywhere the moment New session is clicked.
        if self.written.is_none() && self.messages.is_empty() {
            return Ok(());
        }
        self.append_to(&session_path(self.id), true)
    }

    /// Flush into the file at `path` rather than the session's own.
    fn flush_to(&mut self, path: &std::path::Path) -> std::io::Result<()> {
        // A run from the terminal writes on into the session's own file, whose
        // set would otherwise go stale with every run and wait for a search.
        let session = path.parent() == Some(sessions_dir().as_path());
        self.append_to(path, session)
    }

    /// Flush into the file at `path`, keeping its term set up to date with
    /// `terms`: only sessions are searched, not helpers' conversations.
    fn append_to(&mut self, path: &std::path::Path, terms: bool) -> std::io::Result<()> {
        let meta = self.meta();
        let meta_changed = self.written.as_ref() != Some(&meta);
        let summary_new = self.summary.is_some() && !self.summary_saved;
        // Every session is flushed on every save, and most have nothing new.
        // Those are known without the file system: taking the lock and
        // checking the file first made each save touch every session on disk.
        let settled = self.flushed == self.messages.len()
            && self.decided == self.decisions.len()
            && self.rewound.is_none()
            && !meta_changed
            && !summary_new;
        if self.written.is_some() && settled {
            return Ok(());
        }
        let _lock = Lock::take(path)?;
        // A reserved session's file is there but empty, and still needs its head.
        let fresh = !std::fs::metadata(path).is_ok_and(|m| m.len() > 0);
        // Written before and gone now: the other Letronna deleted it. Writing
        // anyway brought it back as a fragment -- a head and whatever changed
        // since the last save, with none of the history before it. Remembered
        // rather than checked each time, because the freed id is handed out
        // again and this would then write into someone else's session.
        if fresh && self.written.is_some() {
            self.gone = true;
            return Ok(());
        }
        if !fresh && settled {
            return Ok(());
        }

        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .read(true)
            .open(path)?;
        // How many of the messages this copy wrote the other process left,
        // when it wrote here since this copy was read or written.
        let standing = match &self.seen {
            Some(seen) => match added_since(&mut file, seen)? {
                Some(theirs) => kept_by(&theirs, self.flushed),
                None => {
                    let had = &self.messages[..self.flushed.min(self.messages.len())];
                    // Replaced under this copy, as when a new session took
                    // the freed id: writing on would put this one's lines in
                    // it. Rewritten by the other's rewind, it is still this
                    // session, holding what the two share.
                    let Some(shared) = rewritten(&std::fs::read(path)?, seen.head, had) else {
                        self.gone = true;
                        return Ok(());
                    };
                    Some(shared)
                }
            },
            None => None,
        };
        let mine = self.flushed < self.messages.len() || self.rewound.is_some();
        // A rewind here rewrites the file without what it took back, so that
        // is gone from the disk and not only from the replay. A new file has
        // nothing to take back.
        let cutting = self.rewound.is_some() && !fresh;
        // Both said something since: this copy wins. Unless this copy is
        // rewriting the file anyway, a rewind to what the two share goes
        // after their lines, and this copy's own from there, so the file
        // reads back as this copy -- with their lines still in it.
        let clash = mine && standing.is_some();
        if clash && let Some(to) = standing {
            self.flushed = to;
            self.rewound = Some(to);
            self.diverged = true;
        }
        // Theirs may have come after this copy's, so this copy's go again.
        let meta_changed = meta_changed || clash;
        let summary_new = summary_new || (clash && self.summary.is_some());
        let before = file
            .metadata()
            .ok()
            .map(|stat| (stat.len(), modified(&stat)));
        let mut out = String::new();
        match self.rewound {
            Some(to) if cutting => out = cut(&std::fs::read(path)?, to),
            // A crash part way through a line leaves the file without its
            // last newline. Appending straight after it would glue the next
            // record to the torn one and lose both; ending the torn line
            // first loses one.
            _ => {
                if torn(&mut file)? {
                    out.push('\n');
                }
            }
        }
        if fresh {
            push(
                &mut out,
                &Record::Head {
                    id: self.id,
                    created: self.updated,
                },
            );
        }
        if meta_changed {
            push(&mut out, &Record::Meta(meta.clone()));
        }
        if !cutting && let Some(to) = self.rewound {
            push(&mut out, &Record::Rewind { to });
        }
        for message in &self.messages[self.flushed.min(self.messages.len())..] {
            push(&mut out, &Record::Message(message.clone()));
        }
        for decision in &self.decisions[self.decided.min(self.decisions.len())..] {
            push(&mut out, &Record::Auto(decision.clone()));
        }
        // After the rewind and the messages, which it may have been made
        // from: before them, the rewind would take it back on replay.
        if summary_new && let Some((upto, text)) = &self.summary {
            let manual = *upto > 0 && *upto == self.floor();
            let key = (self.messages.get(..*upto))
                .filter(|_| manual)
                .map(|covered| crate::compact::keys(covered)[*upto]);
            push(
                &mut out,
                &Record::Summary {
                    upto: *upto,
                    text: text.clone(),
                    manual,
                    key,
                    call: self.call.take(),
                },
            );
        }
        // Closed before the rewrite too, as Windows will not move a file
        // over one that is open.
        if cutting {
            drop(file);
            replace(path, out.as_bytes())?;
        } else {
            file.write_all(out.as_bytes())?;
            // Stamped once the file is closed, as Windows may only then
            // settle its modification time, and still under the lock.
            drop(file);
        }
        if terms
            && let Some(before) = before
            && let Ok(after) = std::fs::metadata(path)
        {
            // A rewritten file's set is made again from all of it, so none of
            // what was cut can still be searched for.
            let before = if cutting { (0, 0) } else { before };
            crate::prefilter::appended(path, before, (after.len(), modified(&after)), &out);
        }
        self.flushed = self.messages.len();
        self.decided = self.decisions.len();
        self.written = Some(meta);
        self.rewound = None;
        self.summary_saved = true;
        // With their lines in the file and not in this copy, the mark stays
        // before them, so following takes them in; a file the other rewrote
        // is then read in full. A file this copy rewrote is marked again at
        // its new end, or its next save would take it for replaced.
        // Read again from the closed file, whose modification time is settled.
        if self.seen.is_some() && (standing.is_none() || clash) {
            self.seen = std::fs::File::open(path)
                .and_then(|mut file| seen_at_end(&mut file, self.messages.len()))
                .ok();
        }
        Ok(())
    }

    /// Carry this copy on over `bytes`, lines of its file, and say how many
    /// of them were taken. A line that will not parse is skipped rather than
    /// failing the session: losing one message is better than losing the
    /// conversation it was part of.
    fn apply(&mut self, bytes: &[u8], summaries: &mut Vec<Found>) -> usize {
        let end = whole_lines(bytes);
        // Read leniently: a write cut part way through a character leaves
        // bytes that are not UTF-8, which would otherwise fail the whole
        // file rather than the one line they are on.
        for line in String::from_utf8_lossy(&bytes[..end]).lines() {
            if let Ok(record) = serde_json::from_str(line) {
                self.play(record, summaries);
            }
        }
        // A last line with no newline yet is still being written, and is
        // left to be read whole later -- unless it already is whole, a
        // record whose newline a crash cut off.
        match serde_json::from_str(&String::from_utf8_lossy(&bytes[end..])) {
            Ok(record) => {
                self.play(record, summaries);
                bytes.len()
            }
            Err(_) => end,
        }
    }

    /// Carry this copy on over one record of its file.
    fn play(&mut self, record: Record, summaries: &mut Vec<Found>) {
        match record {
            Record::Head { id, created } => {
                self.id = id;
                self.updated = created;
            }
            Record::Meta(meta) => {
                self.title = meta.title;
                self.updated = meta.updated;
                self.pinned = meta.pinned;
                self.workspace = meta.workspace;
                self.todos = meta.todos;
                self.model = meta.model;
                self.gateway = meta.gateway;
                self.effort = meta.effort;
                self.goal = meta.goal;
                self.imported_from = meta.imported_from;
                self.worktree = meta.worktree;
                self.memory_saving = meta.memory_saving;
                self.memory_scope = meta.memory_scope;
            }
            Record::Message(message) => self.messages.push(message),
            Record::Rewind { to } => {
                self.messages.truncate(to);
                summaries.retain(|found| found.upto <= to);
            }
            Record::Summary {
                upto,
                text,
                manual,
                key,
                ..
            } => {
                if upto <= self.messages.len() {
                    summaries.push(Found {
                        upto,
                        text,
                        manual: manual && upto > 0,
                        key,
                    });
                }
            }
            Record::Paid(_) => {}
            Record::Auto(decision) => self.decisions.push(decision),
        }
    }

    /// Mark everything in this copy as already in the file, which it was
    /// just read from, with the newest of `summaries` standing and the
    /// user's among them kept as theirs.
    fn caught_up(&mut self, mut summaries: Vec<Found>) {
        self.flushed = self.messages.len();
        self.decided = self.decisions.len();
        self.written = Some(self.meta());
        // One of the user's made from messages the file does not have --
        // another Letronna summed up its out-of-date view -- would hold the
        // window off turns it never saw.
        if summaries.iter().any(|found| found.key.is_some()) {
            let keys = crate::compact::keys(&self.messages);
            summaries.retain(|found| found.holds(&keys));
        }
        self.asked = (summaries.iter())
            .filter(|found| found.manual)
            .map(|found| (found.upto, found.text.clone()))
            .collect();
        self.summary = summaries.pop().map(|found| (found.upto, found.text));
        self.summary_saved = true;
        // The model is not asked again for what the file already has: the
        // newest, and the user's own when newer ones follow, as the window
        // still starts at one for a model that holds more, or after a rewind.
        for (upto, text) in self.asked.iter().chain(&self.summary) {
            if let Some(covered) = self.messages.get(..*upto) {
                crate::compact::remember(covered, text);
            }
        }
    }

    /// The summaries this copy holds, as `play` carries them on: the user's,
    /// already checked against the messages, then the newest when it is not
    /// the last of theirs.
    fn holding(&self) -> Vec<Found> {
        let mut summaries: Vec<Found> = (self.asked.iter())
            .map(|(upto, text)| Found {
                upto: *upto,
                text: text.clone(),
                manual: true,
                key: None,
            })
            .collect();
        if let Some((upto, text)) = &self.summary
            && self.asked.last() != self.summary.as_ref()
        {
            summaries.push(Found {
                upto: *upto,
                text: text.clone(),
                manual: false,
                key: None,
            });
        }
        summaries
    }

    /// Take in what another process added to the file since this copy was
    /// read or written: only the lines added, when the file just grew, and
    /// all of it again when it was replaced. Whether anything changed.
    fn follow(&mut self) -> bool {
        let unsaved = self.flushed != self.messages.len()
            || self.decided != self.decisions.len()
            || self.rewound.is_some()
            || self.written.as_ref() != Some(&self.meta())
            || (self.summary.is_some() && !self.summary_saved);
        // Never saved, it is not in the file. Listed and not read in, it is
        // read fresh when it is opened. And what this copy has not saved is
        // its own: the next save settles which copy the file keeps.
        if self.written.is_none() || (self.counted > 0 && self.messages.is_empty()) || unsaved {
            return false;
        }
        let path = session_path(self.id);
        // Gone, the next save knows not to bring it back.
        let Ok(stat) = std::fs::metadata(&path) else {
            return false;
        };
        let (size, mtime) = (stat.len(), modified(&stat));
        let Some(seen) = self.seen else {
            return self.read_again(&path);
        };
        if (size, mtime) == (seen.size, seen.mtime) {
            return false;
        }
        // Only a copy that is still what the mark was taken of can be carried
        // on from it; one read in again some other way since is read afresh.
        let resumed = (self.messages.len() == seen.messages)
            .then(|| std::fs::File::open(&path).ok())
            .flatten()
            .and_then(|mut file| read_on(&mut file, seen.len, seen.tail, size).ok().flatten());
        let Some((from, start, bytes)) = resumed else {
            return self.read_again(&path);
        };
        let mut summaries = self.holding();
        let taken = start + self.apply(&bytes[start..], &mut summaries);
        self.seen = Some(Seen {
            size,
            mtime,
            len: from + taken as u64,
            // All in `bytes`: they start that far before the old end.
            tail: tail_hash(&bytes[taken.saturating_sub(TAIL_CHECK as usize)..taken]),
            messages: self.messages.len(),
            head: seen.head,
        });
        self.caught_up(summaries);
        taken > start
    }

    /// Replace this copy with its file as it is now; whether there was one.
    fn read_again(&mut self, path: &std::path::Path) -> bool {
        let Some(read) = replay(path) else {
            return false;
        };
        *self = read;
        true
    }
}

fn push(out: &mut String, record: &Record) {
    if let Ok(line) = serde_json::to_string(record) {
        out.push_str(&line);
        out.push('\n');
    }
}

/// The session file `bytes` with every message past the first `to` taken
/// out, and the summaries of them, the rewinds already in it applied too.
/// A reply taken out leaves a `paid` record of its usage and nothing it
/// said, since it was paid for all the same. A line that will not parse is
/// left out, as replaying skips it anyway.
fn cut(bytes: &[u8], to: usize) -> String {
    enum Kept<'a> {
        Message(&'a str),
        Summary(usize, &'a str),
        Paid(&'a str),
    }
    /// Take out of `kept` what lies past the first `to` messages.
    fn back(kept: &mut Vec<Kept>, paid: &mut Vec<String>, to: usize) {
        let mut messages = 0;
        kept.retain(|entry| match entry {
            Kept::Message(line) => {
                messages += 1;
                if messages <= to {
                    return true;
                }
                if let Ok(Record::Message(reply)) = serde_json::from_str::<Record>(line)
                    && crate::usage::calls(&reply).next().is_some()
                {
                    let mut line = String::new();
                    push(&mut line, &Record::Paid(usage_only(reply)));
                    paid.push(line);
                }
                false
            }
            Kept::Summary(upto, _) if *upto <= to => true,
            // The call that made one the user asked for was paid for too.
            Kept::Summary(_, line) => {
                if let Ok(Record::Summary {
                    call: Some(call), ..
                }) = serde_json::from_str::<Record>(line)
                {
                    let mut line = String::new();
                    let made = Message {
                        role: "summary".into(),
                        model: call.model,
                        at: call.at,
                        summary_usage: Some(call.usage),
                        ..Default::default()
                    };
                    push(&mut line, &Record::Paid(made));
                    paid.push(line);
                }
                false
            }
            Kept::Paid(_) => true,
        });
    }
    let text = String::from_utf8_lossy(bytes);
    let (mut head, mut meta) = (None, None);
    let mut kept = Vec::new();
    // Where these go does not matter: only the costs read them.
    let mut paid = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        match serde_json::from_str::<Record>(line) {
            Ok(Record::Head { .. }) => head = head.or(Some(line)),
            Ok(Record::Meta(_)) => meta = Some(line),
            Ok(Record::Message(_)) => kept.push(Kept::Message(line)),
            Ok(Record::Summary { upto, .. }) => kept.push(Kept::Summary(upto, line)),
            // What the judge decided is kept as what was paid is: a record of
            // what happened, not part of the session a rewind takes back.
            Ok(Record::Paid(_) | Record::Auto(_)) => kept.push(Kept::Paid(line)),
            Ok(Record::Rewind { to }) => back(&mut kept, &mut paid, to),
            Err(_) => {}
        }
    }
    back(&mut kept, &mut paid, to);
    let mut out = String::new();
    for line in head.into_iter().chain(meta) {
        out.push_str(line);
        out.push('\n');
    }
    for entry in kept {
        let (Kept::Message(line) | Kept::Summary(_, line) | Kept::Paid(line)) = entry;
        out.push_str(line);
        out.push('\n');
    }
    out.extend(paid);
    out
}

/// `reply` with its counts and cost, and the tools it called for the counts
/// of those, and none of its words.
fn usage_only(reply: Message) -> Message {
    let tool_calls = reply
        .tool_calls
        .into_iter()
        .map(|call| crate::chat::ToolCall {
            id: call.id,
            name: call.name,
            arguments: String::new(),
        });
    Message {
        role: reply.role,
        model: reply.model,
        tokens: reply.tokens,
        prompt_tokens: reply.prompt_tokens,
        cached_tokens: reply.cached_tokens,
        cache_write_tokens: reply.cache_write_tokens,
        reasoning_tokens: reply.reasoning_tokens,
        cost: reply.cost,
        cost_estimated: reply.cost_estimated,
        summary_usage: reply.summary_usage,
        summary_model: reply.summary_model,
        cache_miss: reply.cache_miss,
        at: reply.at,
        tool_calls: tool_calls.collect(),
        ..Default::default()
    }
}

/// Put `bytes` in place of the file at `path` whole, through a temporary
/// file beside it, so a reader never sees half of it. The caller holds the
/// session's lock, which keeps writers off it meanwhile.
fn replace(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    // Synced before the rename, or a power cut could leave the session's
    // name on blocks never written: the whole history lost, not one line.
    let written = std::fs::File::create(&temp)
        .and_then(|mut file| file.write_all(bytes).and_then(|()| file.sync_all()))
        .and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    written
}

/// Whether the file stops part way through a line, as a write cut short leaves it.
fn torn(file: &mut std::fs::File) -> std::io::Result<bool> {
    if file.metadata()?.len() == 0 {
        return Ok(false);
    }
    file.seek(SeekFrom::End(-1))?;
    let mut last = [0u8];
    file.read_exact(&mut last)?;
    Ok(last[0] != b'\n')
}

/// Rebuild a session by replaying its file.
fn replay(path: &std::path::Path) -> Option<Conversation> {
    // Taken before the read: a write landing between the two only makes
    // the next look at the file read its end again.
    let mtime = modified(&std::fs::metadata(path).ok()?);
    let bytes = std::fs::read(path).ok()?;
    let mut conversation = Conversation::default();
    // Every summary still within the history so far: a rewind can take the
    // newest back and leave an earlier one standing.
    let mut summaries = Vec::new();
    let len = conversation.apply(&bytes, &mut summaries);
    // A crash in the first save can tear the head line itself. The file is
    // named after the session, so its id survives that.
    if conversation.id == 0 {
        conversation.id = path.file_stem()?.to_str()?.parse().ok()?;
    }
    if conversation.id == 0 {
        return None;
    }
    conversation.seen = Some(Seen {
        size: bytes.len() as u64,
        mtime,
        len: len as u64,
        tail: tail_hash(&bytes[len.saturating_sub(TAIL_CHECK as usize)..len]),
        messages: conversation.messages.len(),
        head: head_hash(&bytes),
    });
    conversation.caught_up(summaries);
    Some(conversation)
}

/// A helper's conversation, written in the same lines as a session but never
/// listed or searched as one.
pub struct Transcript {
    pub path: PathBuf,
    pub conversation: Conversation,
}

impl Transcript {
    /// Start a conversation for a new helper of `session`, and say which
    /// number it got there.
    pub fn create(
        session: u64,
        title: &str,
        workspace: Option<PathBuf>,
    ) -> Result<(u32, Self), String> {
        Self::create_in(&helpers_dir(session), "", session, title, workspace)
    }

    /// Start the throwaway conversation a side question about `session` runs
    /// in, apart from its helpers' and named so `sweep_asides` knows it.
    pub fn aside(session: u64, workspace: Option<PathBuf>) -> Result<Self, String> {
        Self::create_in(&asides_dir(), ASIDE, session, "btw", workspace).map(|(_, t)| t)
    }

    /// Start a conversation in `dir` as the first free `<prefix><n>.jsonl`.
    fn create_in(
        dir: &std::path::Path,
        prefix: &str,
        session: u64,
        title: &str,
        workspace: Option<PathBuf>,
    ) -> Result<(u32, Self), String> {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("Could not make a place for the helper's conversation: {e}"))?;
        let conversation = Conversation {
            id: session,
            title: title.into(),
            updated: backend::now(),
            workspace,
            ..Default::default()
        };
        for n in 1..=u32::MAX {
            let path = dir.join(format!("{prefix}{n}.jsonl"));
            // Making the file is what claims the number: of two processes
            // asking at once exactly one gets it, where counting and adding
            // one, as `next_id` does, would hand both the same.
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(format!("Could not start the helper's conversation: {e}")),
            };
            // The head goes in here because flush writes one only into a file
            // that is not there yet, and this one now is.
            let mut head = String::new();
            push(
                &mut head,
                &Record::Head {
                    id: session,
                    created: conversation.updated,
                },
            );
            let mut transcript = Self { path, conversation };
            file.write_all(head.as_bytes())
                .and_then(|_| transcript.save())
                .map_err(|e| format!("Could not start the helper's conversation: {e}"))?;
            return Ok((n, transcript));
        }
        Err("This session has no numbers left for another helper.".into())
    }

    /// Start a new session for a run nobody is watching, on the next number
    /// free on disk, reserved as the frontends reserve theirs.
    pub fn session(title: &str, workspace: Option<PathBuf>) -> Result<Self, String> {
        let id = Store::default().reserve_id();
        let conversation = Conversation {
            id,
            title: title.into(),
            updated: backend::now(),
            workspace,
            ..Default::default()
        };
        let mut transcript = Self {
            path: session_path(id),
            conversation,
        };
        transcript
            .save()
            .map_err(|e| format!("Could not start the session: {e}"))?;
        Ok(transcript)
    }

    /// Read back helper `n` of `session`, if it is there.
    pub fn open(session: u64, n: u32) -> Option<Self> {
        let path = helpers_dir(session).join(format!("{n}.jsonl"));
        let conversation = replay(&path)?;
        Some(Self { path, conversation })
    }

    /// Read back the conversation kept at `path`, such as a workflow agent's,
    /// if it is there.
    pub(crate) fn at(path: PathBuf) -> Option<Self> {
        let conversation = replay(&path)?;
        Some(Self { path, conversation })
    }

    /// Append whatever is new, as saving a session does. One with no path,
    /// for a run nobody picks up again, is kept in memory only.
    pub fn save(&mut self) -> std::io::Result<()> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        self.conversation.flush_to(&self.path)
    }
}

/// What a session looks like from the outside: enough to recognise it by,
/// and nothing that was said in it.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Summary {
    pub id: u64,
    pub title: String,
    pub updated: u64,
    pub messages: usize,
    pub workspace: Option<PathBuf>,
    pub pinned: bool,
    pub todos: Vec<Todo>,
    pub model: Option<String>,
    pub gateway: Option<String>,
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<crate::goal::Goal>,
    pub imported_from: Option<crate::import::Source>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<crate::worktree::Worktree>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_saving: Option<crate::memory::Saving>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_scope: Option<crate::memory::Scope>,
}

/// The sessions on disk, newest first, without loading what is in them.
///
/// Only the head and meta records of each file are read, so a caller asking
/// "which sessions are there" does not pull old conversations into a new
/// one. And only what the index has not seen is read: a file that has not
/// moved since the last listing is taken from `sessions/index.json`, and
/// one that has grown is read from where the index left off.
pub fn summaries() -> Vec<Summary> {
    let index = index_path();
    let known = read_index(&index);
    let (mut kept, mut out) = (Vec::new(), Vec::new());
    for id in session_ids() {
        if let Some((entry, summary)) = scan(&session_path(id), id, known.get(&id)) {
            kept.push(entry);
            out.push(summary);
        }
    }
    // Sessions deleted since leave it, as they are not in the folder.
    let moved =
        kept.len() != known.len() || kept.iter().any(|e| known.get(&e.summary.id) != Some(e));
    if moved {
        write_index(&index, kept);
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.updated));
    out
}

/// Bumped when what an index entry means changes, so an index an older
/// build wrote is rebuilt rather than trusted.
const INDEX_VERSION: u32 = 1;

/// Bytes before the end of what the index read that must be unchanged for a
/// grown file to be read on from there. A session file only grows at its
/// end, so a difference means it was replaced -- deleted and its id given to
/// a new session, or rewritten -- and it is read in full. A kilobyte reaches
/// back into the last record or two, which in a replaced file are other
/// words written at another time.
const TAIL_CHECK: u64 = 1024;

/// `sessions/index.json`: what each session file summed up to when it was
/// last listed, so the next listing reads only what has changed since.
///
/// It is a cache and nothing more. The session files stay the truth, and an
/// index that is missing, will not parse or is out of date costs a reading
/// of the files, never a session.
#[derive(Serialize, Deserialize)]
struct Index {
    version: u32,
    sessions: Vec<Indexed>,
}

/// One session file as the index knows it.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
struct Indexed {
    /// The file's size and modification time when it was read: while both
    /// hold, nothing in it has moved.
    size: u64,
    mtime: u64,
    /// How far the summary goes: the end of the last whole line, so a line
    /// still being written is read again next time rather than cut in two.
    len: u64,
    /// A hash of the `TAIL_CHECK` bytes before `len`.
    tail: u64,
    summary: Summary,
}

/// Where a copy of a session stands in its file: the file as this process
/// last read or wrote it, so what another process adds after that can be
/// told apart and read on its own.
#[derive(Clone, Copy, Default, PartialEq)]
pub struct Seen {
    /// The file's size and modification time then: while both hold, nothing
    /// in it has moved.
    size: u64,
    mtime: u64,
    /// How far the copy goes: the end of the last whole record.
    len: u64,
    /// A hash of the `TAIL_CHECK` bytes before `len`.
    tail: u64,
    /// How many messages the file held up to `len`, to tell a copy that has
    /// been read in again another way since.
    messages: usize,
    /// A hash of the file's first line, its head, which a rewind's rewrite
    /// keeps and a new session given the same id does not.
    head: u64,
}

fn index_path() -> PathBuf {
    sessions_dir().join("index.json")
}

/// The index by session id: empty when there is none, it will not parse, or
/// an older build wrote it.
fn read_index(path: &std::path::Path) -> std::collections::HashMap<u64, Indexed> {
    // Only ever moved into place whole, so reading it needs no lock.
    let Ok(bytes) = std::fs::read(path) else {
        return Default::default();
    };
    match serde_json::from_slice::<Index>(&bytes) {
        Ok(index) if index.version == INDEX_VERSION => index
            .sessions
            .into_iter()
            .map(|entry| (entry.summary.id, entry))
            .collect(),
        _ => Default::default(),
    }
}

/// Replace the index with `sessions`, whole. A failure is let go: the next
/// listing reads what changed from the files and tries again.
fn write_index(path: &std::path::Path, sessions: Vec<Indexed>) {
    // The window and the terminal both write it, and so can two threads of
    // one: the lock keeps them from sharing a temp file or moving one over
    // the other at the same moment, which Windows can refuse.
    let Ok(_lock) = Lock::take(path) else {
        return;
    };
    let index = Index {
        version: INDEX_VERSION,
        sessions,
    };
    let Ok(bytes) = serde_json::to_vec(&index) else {
        return;
    };
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    // Private like the settings: it holds every title and project folder.
    let written = write_private(&temp, &bytes).and_then(|()| std::fs::rename(&temp, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
}

/// Sum up the session file at `path`, reading only what `known` does not
/// already cover. Gives the index's new entry and the summary to list, which
/// also counts a last line with no newline yet; None for a file that is
/// empty or cannot be read.
fn scan(path: &std::path::Path, id: u64, known: Option<&Indexed>) -> Option<(Indexed, Summary)> {
    let stat = std::fs::metadata(path).ok()?;
    let (size, mtime) = (stat.len(), modified(&stat));
    // Reserved and not yet saved: it belongs to whoever reserved it.
    if size == 0 {
        return None;
    }
    if let Some(known) = known.filter(|k| (k.size, k.mtime, k.len) == (size, mtime, size)) {
        return Some((known.clone(), known.summary.clone()));
    }
    let mut file = std::fs::File::open(path).ok()?;
    // Read on from where the index stopped if the file has only been
    // appended to and what it read is still there. One that shrank or
    // changed in place was rewritten, and is read from the start.
    let appended = |k: &&Indexed| size > k.size || (size, mtime) == (k.size, k.mtime);
    let resumed = known.filter(appended).and_then(|k| {
        let (from, seen, bytes) = read_on(&mut file, k.len, k.tail, size).ok().flatten()?;
        Some((k.summary.clone(), from, seen, bytes))
    });
    let (mut summary, from, start, bytes) = match resumed {
        Some(resumed) => resumed,
        None => {
            let empty = Summary {
                id,
                ..Default::default()
            };
            (empty, 0, 0, read_range(&mut file, 0, size)?)
        }
    };
    let end = bytes[start..]
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(start, |at| start + at + 1);
    tally(&mut summary, &bytes[start..end]);
    let entry = Indexed {
        size,
        mtime,
        len: from + end as u64,
        // All in `bytes`: a resumed read starts that far before the old end.
        tail: tail_hash(&bytes[end.saturating_sub(TAIL_CHECK as usize)..end]),
        summary,
    };
    let mut listed = entry.summary.clone();
    tally(&mut listed, &bytes[end..]);
    Some((entry, listed))
}

/// Carry `summary` on over the records in `bytes`, lines of a session file.
fn tally(summary: &mut Summary, bytes: &[u8]) {
    // Leniently, as `replay` reads: a bad byte costs its own line only.
    let text = String::from_utf8_lossy(bytes);
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        // Messages are most of the file and none of them are wanted here, so
        // they are counted by their tag rather than parsed. The tag is
        // written first, which is what makes this safe; anything that does
        // not match still goes through serde.
        if line.starts_with("{\"type\":\"message\"") {
            summary.messages += 1;
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(Record::Head { created, .. }) => summary.updated = created,
            Ok(Record::Meta(meta)) => {
                summary.title = meta.title;
                summary.updated = meta.updated;
                summary.workspace = meta.workspace;
                summary.pinned = meta.pinned;
                summary.todos = meta.todos;
                summary.model = meta.model;
                summary.gateway = meta.gateway;
                summary.effort = meta.effort;
                summary.goal = meta.goal;
                summary.imported_from = meta.imported_from;
                summary.worktree = meta.worktree;
                summary.memory_saving = meta.memory_saving;
                summary.memory_scope = meta.memory_scope;
            }
            Ok(Record::Message(_)) => summary.messages += 1,
            Ok(Record::Rewind { to }) => summary.messages = summary.messages.min(to),
            Ok(Record::Summary { .. } | Record::Paid(_) | Record::Auto(_)) | Err(_) => {}
        }
    }
}

/// The file's bytes from `from` up to `to`, or fewer if it has shrunk since.
fn read_range(file: &mut std::fs::File, from: u64, to: u64) -> Option<Vec<u8>> {
    file.seek(SeekFrom::Start(from)).ok()?;
    let mut bytes = Vec::new();
    file.take(to.saturating_sub(from))
        .read_to_end(&mut bytes)
        .ok()?;
    Some(bytes)
}

/// When the file last changed, in nanoseconds since 1970; 0 where the system
/// does not say, which leaves the size alone to tell.
fn modified(stat: &std::fs::Metadata) -> u64 {
    stat.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .and_then(|since| u64::try_from(since.as_nanos()).ok())
        .unwrap_or(0)
}

/// A hash of `bytes` that every build computes alike, as the index outlives
/// the build that wrote it.
fn tail_hash(bytes: &[u8]) -> u64 {
    let digest = ring::digest::digest(&ring::digest::SHA256, bytes);
    digest
        .as_ref()
        .iter()
        .take(8)
        .fold(0, |hash, &byte| (hash << 8) | u64::from(byte))
}

/// A file the agent wrote, as the Artifacts page lists it.
pub struct Artifact {
    pub path: PathBuf,
    /// The title of the newest session that wrote it.
    pub session: String,
    pub written: u64,
    /// How many times it was written or edited across all sessions.
    pub writes: usize,
}

/// Every file written or edited in a session with a project folder, newest
/// first, one entry per file however often it was touched.
///
/// Read from the files, as most sessions are not in memory. Only sessions
/// with a project folder are opened, and in them only the lines naming a
/// write or an edit are parsed; the rest are counted by their tag.
pub fn artifacts() -> Vec<Artifact> {
    let mut found: Vec<Artifact> = Vec::new();
    let mut at = std::collections::HashMap::new();
    for summary in summaries() {
        let Some(workspace) = &summary.workspace else {
            continue;
        };
        for relative in written_in(&session_path(summary.id)) {
            let path = workspace.join(relative);
            let Some(&ix) = at.get(&path) else {
                at.insert(path.clone(), found.len());
                found.push(Artifact {
                    path,
                    session: summary.title.clone(),
                    written: summary.updated,
                    writes: 1,
                });
                continue;
            };
            let existing = &mut found[ix];
            existing.writes += 1;
            // Newest first, so a tie goes to the session already there.
            if summary.updated > existing.written {
                existing.written = summary.updated;
                existing.session = summary.title.clone();
            }
        }
    }
    found.sort_by_key(|a| std::cmp::Reverse(a.written));
    found
}

/// The paths the session file at `path` wrote, edited or patched
/// successfully, as the calls gave them, leaving out any a later rewind took
/// back.
fn written_in(path: &std::path::Path) -> Vec<String> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    // Leniently, as `replay` reads: a bad byte costs its own line only.
    let text = String::from_utf8_lossy(&bytes);
    let (mut count, mut writes) = (0, Vec::new());
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let message = line.starts_with("{\"type\":\"message\"");
        if message
            && !line.contains("\"write_file\"")
            && !line.contains("\"edit_file\"")
            && !line.contains("\"apply_patch\"")
        {
            count += 1;
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(Record::Message(m)) => {
                count += 1;
                for relative in wrote(&m) {
                    writes.push((count - 1, relative));
                }
            }
            Ok(Record::Rewind { to }) => {
                count = count.min(to);
                writes.retain(|(ix, _)| *ix < to);
            }
            // Counted by its tag all the same, as the listing counts it.
            Err(_) if message => count += 1,
            _ => {}
        }
    }
    writes.into_iter().map(|(_, relative)| relative).collect()
}

/// The paths a successful write, edit or patch result left a file at.
fn wrote(message: &Message) -> Vec<String> {
    if message.role != "tool" || message.is_error {
        return Vec::new();
    }
    crate::tools::written(&message.name, &message.args)
}

/// The file's bytes from `TAIL_CHECK` before `len` up to `size`, and where
/// `len` falls in them -- if what was read up to `len` is still there, so
/// the file has only been added to since; None if it was replaced.
fn read_on(
    file: &mut std::fs::File,
    len: u64,
    tail: u64,
    size: u64,
) -> std::io::Result<Option<(u64, usize, Vec<u8>)>> {
    let from = len.saturating_sub(TAIL_CHECK);
    file.seek(SeekFrom::Start(from))?;
    let mut bytes = Vec::new();
    file.take(size.saturating_sub(from))
        .read_to_end(&mut bytes)?;
    let Ok(seen) = usize::try_from(len - from) else {
        return Ok(None);
    };
    let same = bytes.get(..seen).is_some_and(|b| tail_hash(b) == tail);
    Ok(same.then_some((from, seen, bytes)))
}

/// How far the whole lines of `bytes` go: to just after the last newline.
fn whole_lines(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .rposition(|&b| b == b'\n')
        .map_or(0, |at| at + 1)
}

/// What the file holds past `seen`: nothing when it has not grown, the
/// bytes added when it only grew, and None when it no longer holds what
/// `seen` was taken from.
fn added_since(file: &mut std::fs::File, seen: &Seen) -> std::io::Result<Option<Vec<u8>>> {
    let stat = file.metadata()?;
    let size = stat.len();
    // The size alone would not tell: a rewind's rewrite can leave the file
    // as long as it was.
    if (size, modified(&stat)) == (seen.size, seen.mtime) {
        return Ok(Some(Vec::new()));
    }
    let read = read_on(file, seen.len, seen.tail, size)?;
    Ok(read.map(|(_, start, mut bytes)| bytes.split_off(start)))
}

/// How many of the first `had` messages survive `bytes`, lines another
/// process added to the file: all of them, or fewer after a rewind there.
/// None when the lines hold no whole record, as a write cut short leaves.
fn kept_by(bytes: &[u8], had: usize) -> Option<usize> {
    let end = whole_lines(bytes);
    let mut standing = None;
    for line in String::from_utf8_lossy(&bytes[..end]).lines() {
        // Messages are counted by their tag, as the listing counts them.
        if line.starts_with("{\"type\":\"message\"") {
            standing = standing.or(Some(had));
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(Record::Rewind { to }) => standing = Some(standing.unwrap_or(had).min(to)),
            Ok(_) => standing = standing.or(Some(had)),
            Err(_) => {}
        }
    }
    standing
}

/// Where a copy stands that has just written the end of the file, holding
/// `messages`: at that end.
fn seen_at_end(file: &mut std::fs::File, messages: usize) -> std::io::Result<Seen> {
    let stat = file.metadata()?;
    let size = stat.len();
    file.seek(SeekFrom::Start(0))?;
    let mut start = Vec::new();
    Read::by_ref(file)
        .take(HEAD_MAX as u64)
        .read_to_end(&mut start)?;
    let from = size.saturating_sub(TAIL_CHECK);
    file.seek(SeekFrom::Start(from))?;
    let mut end = Vec::new();
    file.take(size - from).read_to_end(&mut end)?;
    Ok(Seen {
        size,
        mtime: modified(&stat),
        len: size,
        tail: tail_hash(&end),
        messages,
        head: head_hash(&start),
    })
}

/// The most of a file's first line `head_hash` reads: a head record is far
/// shorter, and a first line that is not one need not be read to its end.
const HEAD_MAX: usize = 256;

/// A hash of the first line of `bytes`, a session file from its start.
fn head_hash(bytes: &[u8]) -> u64 {
    let start = &bytes[..bytes.len().min(HEAD_MAX)];
    let end = start
        .iter()
        .position(|&b| b == b'\n')
        .map_or(start.len(), |at| at + 1);
    tail_hash(&start[..end])
}

/// How many of `had`, the messages a copy last wrote to its file, `bytes`
/// still starts with, if `bytes` is that file as another process's rewind
/// rewrote it since: under the head the copy saw, `head`, and starting with
/// the copy's first message or holding none. None for another session.
fn rewritten(bytes: &[u8], head: u64, had: &[Message]) -> Option<usize> {
    if head_hash(bytes) != head {
        return None;
    }
    let mut file = Conversation::default();
    file.apply(bytes, &mut Vec::new());
    // Compared as written, as a message read back is the one written.
    let line = |message: &Message| serde_json::to_string(message).ok();
    let shared = file
        .messages
        .iter()
        .zip(had)
        .take_while(|(theirs, ours)| line(theirs).is_some_and(|l| Some(l) == line(ours)))
        .count();
    // A head is an id and the second it was made, which a new session given
    // the freed id could share; its first message it would not. A rewind to
    // the start leaves no message to tell by, and none of another's to write
    // this copy's among.
    (shared > 0 || file.messages.is_empty()).then_some(shared)
}

/// The model calls saved in the sessions and their helpers' conversations
/// written to since `since`, and all of session `session`'s. Replies a
/// rewind took back count too, as they were paid for all the same.
///
/// A branch copies the replies of the session it came from, so a reply that
/// kept its time is marked a copy wherever it is seen after its first place.
/// Sessions are numbered as they are made, which puts the original first.
pub fn spent(since: u64, session: u64) -> Vec<crate::usage::Spent> {
    let files = conversation_files();
    let mut seen = std::collections::HashSet::new();
    files
        .into_iter()
        .filter(|(id, path)| *id == session || written_since(path, since))
        .flat_map(|(id, path)| spent_in(&path, id, &mut seen))
        .collect()
}

/// How many times the model called each tool, by name, in the sessions and
/// their helpers' conversations since `since`. A branch copies the calls of
/// the session it came from, so each call is counted once.
pub fn tool_calls(since: u64) -> std::collections::BTreeMap<String, usize> {
    let mut counts = std::collections::BTreeMap::new();
    let mut seen = std::collections::HashSet::new();
    for (_, path) in conversation_files() {
        if !written_since(&path, since) {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        for line in String::from_utf8_lossy(&bytes).lines() {
            // Only replies carry calls, and those a rewind took back keep
            // theirs; see `spent_in`.
            if !line.starts_with("{\"type\":\"message\",\"role\":\"assistant\"")
                && !line.starts_with("{\"type\":\"paid\"")
            {
                continue;
            }
            let Ok(Record::Message(reply) | Record::Paid(reply)) = serde_json::from_str(line)
            else {
                continue;
            };
            // A reply saved before replies kept their time is in a file
            // written since, which is as near as it can be told.
            if reply.at != 0 && reply.at < since {
                continue;
            }
            for call in reply.tool_calls {
                // Ids are only unique within a reply, and are `call_0` for a
                // provider that gives none, so a copy is told by the time of
                // its reply too. One saved before replies kept their time
                // cannot be told from a copy, and is counted.
                let copy = reply.at != 0
                    && !seen.insert((reply.at, call.id, call.name.clone(), call.arguments));
                if !copy {
                    *counts.entry(call.name).or_default() += 1;
                }
            }
        }
    }
    counts
}

/// Every session file and helper transcript, with the session each belongs
/// to, in order.
fn conversation_files() -> Vec<(u64, PathBuf)> {
    let mut files: Vec<(u64, PathBuf)> = session_ids()
        .into_iter()
        .map(|id| (id, session_path(id)))
        .collect();
    let helpers = std::fs::read_dir(backend::data_dir().join("agents"));
    for dir in helpers.into_iter().flatten().filter_map(Result::ok) {
        let Some(id) = dir.file_name().to_str().and_then(|n| n.parse().ok()) else {
            continue;
        };
        let Ok(entries) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        let transcripts = entries.filter_map(Result::ok).map(|e| e.path());
        files.extend(
            transcripts
                .filter(|path| path.extension().is_some_and(|x| x == "jsonl"))
                .map(|path| (id, path)),
        );
    }
    files.sort();
    files
}

/// Keeps with session `session`'s helpers, where `spent` counts them, the
/// model calls saved in the throwaway conversation at `aside`, such as a
/// goal check's: what each took and cost, and nothing it said.
pub(crate) fn keep_calls(aside: &std::path::Path, session: u64) -> std::io::Result<()> {
    let Some(aside) = replay(aside) else {
        return Ok(());
    };
    let mut out = String::new();
    for reply in aside.messages.iter().filter(|m| m.role == "assistant") {
        let call = Message {
            role: reply.role.clone(),
            model: reply.model.clone(),
            tokens: reply.tokens,
            prompt_tokens: reply.prompt_tokens,
            cached_tokens: reply.cached_tokens,
            cache_write_tokens: reply.cache_write_tokens,
            reasoning_tokens: reply.reasoning_tokens,
            cost: reply.cost,
            cost_estimated: reply.cost_estimated,
            summary_usage: reply.summary_usage,
            summary_model: reply.summary_model.clone(),
            at: reply.at,
            ..Default::default()
        };
        push(&mut out, &Record::Message(call));
    }
    if out.is_empty() {
        return Ok(());
    }
    let dir = helpers_dir(session);
    std::fs::create_dir_all(&dir)?;
    // Not a number, so no helper's conversation is ever given this name.
    let path = dir.join("calls.jsonl");
    let _lock = Lock::take(&path)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .read(true)
        .open(&path)?;
    if torn(&mut file)? {
        out.insert(0, '\n');
    }
    file.write_all(out.as_bytes())
}

/// Whether the file at `path` was written to at `since` or later, or might
/// have been. Files are only appended to, so an older one holds nothing newer.
fn written_since(path: &std::path::Path, since: u64) -> bool {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .is_none_or(|age| age.as_secs() >= since)
}

/// The model calls saved in one conversation file of `session`, noting each
/// reply that kept its time in `seen`. A reply saved before replies kept
/// their time takes the session's last, and cannot be told from a copy.
fn spent_in(
    path: &std::path::Path,
    session: u64,
    seen: &mut std::collections::HashSet<(u64, String, u64, u64, u64)>,
) -> Vec<crate::usage::Spent> {
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    let text = String::from_utf8_lossy(&bytes);
    let mut updated = 0;
    let mut spent = Vec::new();
    for line in text.lines() {
        // Only replies carry usage, and the role is written straight after
        // the tag, so every other message is passed over unparsed.
        if line.starts_with("{\"type\":\"message\"")
            && !line.starts_with("{\"type\":\"message\",\"role\":\"assistant\"")
        {
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(Record::Head { created, .. }) => updated = updated.max(created),
            Ok(Record::Meta(meta)) => updated = updated.max(meta.updated),
            Ok(Record::Message(reply) | Record::Paid(reply)) => {
                // Two replies are not made in one second with the same model
                // and the same counts, but a reply and its copy are.
                let (at, model) = (reply.at, reply.model.clone());
                let key = (
                    at,
                    model,
                    reply.tokens,
                    reply.prompt_tokens,
                    reply.cached_tokens,
                );
                let copy = at > 0 && !seen.insert(key);
                spent.extend(crate::usage::calls(&reply).map(|(model, usage)| {
                    crate::usage::Spent {
                        session,
                        at: reply.at,
                        model: model.to_owned(),
                        usage,
                        copy,
                    }
                }))
            }
            // Only the file it was made in has the call: a copy's has not.
            Ok(Record::Summary {
                call: Some(call), ..
            }) => spent.push(crate::usage::Spent {
                session,
                at: call.at,
                model: call.model,
                usage: call.usage,
                copy: false,
            }),
            _ => {}
        }
    }
    for call in spent.iter_mut().filter(|call| call.at == 0) {
        call.at = updated;
    }
    spent
}

/// Each saved reply in the session and helper files written to at `since`
/// or later, with when it was made, leaving out the copies a branch took of
/// replies already passed. Files are read a line at a time, so a long
/// history costs the largest line in memory rather than the largest file.
pub(crate) fn replies(since: u64, mut visit: impl FnMut(u64, &Message)) {
    let files = conversation_files();
    let mut seen = std::collections::HashSet::new();
    for (_, path) in files.iter().filter(|(_, path)| written_since(path, since)) {
        replies_in(path, &mut seen, &mut visit);
    }
}

/// The replies in one conversation file, as `replies` hands them on.
fn replies_in(
    path: &std::path::Path,
    seen: &mut std::collections::HashSet<(u64, String, u64, u64, u64)>,
    visit: &mut impl FnMut(u64, &Message),
) {
    use std::io::BufRead;
    let Ok(file) = std::fs::File::open(path) else {
        return;
    };
    let mut reader = std::io::BufReader::new(file);
    let mut bytes = Vec::new();
    let mut updated = 0;
    // A reply saved before replies kept their time takes the session's
    // last, which the meta line after it says, so those few wait for the end.
    let mut undated = Vec::new();
    loop {
        bytes.clear();
        match reader.read_until(b'\n', &mut bytes) {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let line = String::from_utf8_lossy(&bytes);
        let line = line.trim_end();
        if line.starts_with("{\"type\":\"message\"")
            && !line.starts_with("{\"type\":\"message\",\"role\":\"assistant\"")
        {
            continue;
        }
        match serde_json::from_str::<Record>(line) {
            Ok(Record::Head { created, .. }) => updated = updated.max(created),
            Ok(Record::Meta(meta)) => updated = updated.max(meta.updated),
            Ok(Record::Message(mut reply) | Record::Paid(mut reply)) => {
                if reply.at == 0 {
                    // Only the counts and the tool names are wanted from it
                    // later, and clearing would keep what the text took.
                    reply.content = String::new();
                    reply.reasoning = String::new();
                    reply.replay = None;
                    for call in &mut reply.tool_calls {
                        call.id = String::new();
                        call.arguments = String::new();
                    }
                    undated.push(reply);
                    continue;
                }
                let key = (
                    reply.at,
                    reply.model.clone(),
                    reply.tokens,
                    reply.prompt_tokens,
                    reply.cached_tokens,
                );
                if seen.insert(key) {
                    visit(reply.at, &reply);
                }
            }
            _ => {}
        }
    }
    for reply in &undated {
        visit(updated, reply);
    }
}

/// A place in an earlier session where a search found what it looked for.
pub struct Hit {
    pub id: u64,
    pub title: String,
    pub updated: u64,
    /// Who said it, "notice" for a report sent as the user's turn, or
    /// "title" for a match in the session's title.
    pub role: String,
    /// Which message it is, counting from 0 as the session's messages do;
    /// None for a match in the title.
    pub ordinal: Option<usize>,
    /// The matching words with a little of what surrounds them, on one line.
    pub snippet: String,
}

/// Which sessions a search looks through.
#[derive(Clone, Copy, Default)]
pub struct Within {
    /// A session left out, such as the one asking, which already has what it
    /// said; 0 for none.
    pub exclude: u64,
    /// The one session looked through, the one asking too, with as many of
    /// its matches as asked for; 0 for every session.
    pub only: u64,
    /// Only sessions last used at or after this time, in seconds since 1970.
    pub after: Option<u64>,
    /// Only sessions last used before this time, in seconds since 1970.
    pub before: Option<u64>,
}

/// Matches taken from any one session, so a long one cannot fill the list.
const HITS_PER_SESSION: usize = 3;
/// Characters of context kept either side of a match.
const SNIPPET_SIDE: usize = 100;
/// Words of a query searched for at most: each is another look through every
/// session's text, and a query longer than this is a paste, not a search.
const MAX_TERMS: usize = 16;
/// How soon saying a word again stops adding to a match: BM25's usual k1,
/// under which no count is worth 2.2 of one, so a word weighing that much
/// more than another, said once, outranks any count of the other.
const REPEATS: f64 = 1.2;
/// Punctuation stripped from either end of a query's word, so "error:" or
/// "(postgres)" finds the word while "c++" and "#12" keep theirs.
const EDGES: &[char] = &[
    ',', '.', ';', ':', '!', '?', '(', ')', '[', ']', '{', '}', '\'', '`',
];

/// What was said in past sessions that holds every word of `query`,
/// ignoring case, or failing that any of them: best first, where a match on
/// a word few sessions use counts for more, and newest first among equals.
pub fn search(query: &str, within: &Within, limit: usize) -> Vec<Hit> {
    search_in(&sessions_dir(), query, within, limit)
}

/// Session `id` as saved, for reading back what was said around a hit.
pub fn saved(id: u64) -> Option<Conversation> {
    replay(&session_path(id))
}

fn search_in(dir: &std::path::Path, query: &str, within: &Within, limit: usize) -> Vec<Hit> {
    search_through(dir, query, within, limit, true)
}

/// As `search_in`, passing over the sessions whose term sets rule them out
/// with `prefilter`, which changes how long it takes and never what it finds.
fn search_through(
    dir: &std::path::Path,
    query: &str,
    within: &Within,
    limit: usize,
    prefilter: bool,
) -> Vec<Hit> {
    let terms = terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let written: Vec<String> = terms.iter().map(|t| on_disk(t)).collect();
    // Which words each session holds anywhere, told by its whole text
    // without parsing a line: most sessions hold none, and how many hold a
    // word says how rare it is.
    let (mut sessions, mut holding) = (0usize, vec![0usize; terms.len()]);
    let mut candidates: Vec<(PathBuf, bool)> = Vec::new();
    for (path, lower) in session_texts(dir, &written, prefilter) {
        sessions += 1;
        // Ruled out unread, so it holds none of the words.
        let Some(lower) = lower else {
            continue;
        };
        let held: Vec<bool> = written.iter().map(|t| lower.contains(t.as_str())).collect();
        for (count, held) in holding.iter_mut().zip(&held) {
            *count += usize::from(*held);
        }
        if held.contains(&true) {
            candidates.push((path, !held.contains(&false)));
        }
    }
    let weights: Vec<f64> = holding
        .iter()
        .map(|&n| match n {
            0 => 0.0,
            n => (1.0 + sessions as f64 / n as f64).ln(),
        })
        .collect();
    let every = candidates.iter().filter(|(_, all)| *all).map(|(p, _)| p);
    let mut found = ranked(every, &terms, &weights, true, within);
    // Nothing holds every word, which a natural question often asks for, so
    // the rare words it has still find their messages.
    if found.is_empty() && terms.len() > 1 {
        let any = candidates.iter().map(|(p, _)| p);
        found = ranked(any, &terms, &weights, false, within);
    }
    found.into_iter().take(limit).collect()
}

/// Each session file in `dir` with its text in lowercase, read one at a time
/// and leniently, as in `replay`: a search holds one file rather than them
/// all. With `prefilter`, a file whose term set shows it holds none of
/// `written` comes unread and without its text, and a file read for want of
/// a set that fits it is given one.
fn session_texts<'a>(
    dir: &std::path::Path,
    written: &'a [String],
    prefilter: bool,
) -> impl Iterator<Item = (PathBuf, Option<String>)> + 'a {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "jsonl"))
        .filter_map(move |path| {
            // Taken before the file is read, so a set made from it covers at
            // least what its stamp says, even if a save appends meanwhile.
            let stat = std::fs::metadata(&path).ok()?;
            let stamp = (stat.len(), modified(&stat));
            let filter = prefilter
                .then(|| crate::prefilter::read(&path, stamp))
                .flatten();
            if filter
                .as_ref()
                .is_some_and(|f| !written.iter().any(|t| f.may_hold(t)))
            {
                return Some((path, None));
            }
            let bytes = std::fs::read(&path).ok()?;
            let text = String::from_utf8(bytes)
                .unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned());
            let lower = text.to_lowercase();
            // Saved before sets were kept, by an older build, or since a
            // set was last made: the file is in hand, so it costs a hash.
            if prefilter && filter.is_none() && stamp.0 > 0 {
                let made = crate::prefilter::Filter::of(&lower);
                crate::prefilter::write(&path, stamp, &made);
            }
            Some((path, Some(lower)))
        })
}

/// The words of `query`, lowercase: "quoted words" stay together, the rest
/// are cut at spaces and lose the punctuation at their ends. A run of CJK
/// text has no spaces and so stays one word, found as it is written.
fn terms(query: &str) -> Vec<String> {
    let mut terms: Vec<String> = Vec::new();
    for (i, part) in query.split('"').enumerate() {
        let words: Vec<&str> = if i % 2 == 1 {
            vec![part.trim()]
        } else {
            part.split_whitespace()
                // Only punctuation, such as "->", is looked for as it is.
                .map(|w| match w.trim_matches(EDGES) {
                    "" => w,
                    word => word,
                })
                .collect()
        };
        for word in words.into_iter().map(str::to_lowercase) {
            if !word.is_empty() && !terms.contains(&word) {
                terms.push(word);
            }
        }
    }
    terms.truncate(MAX_TERMS);
    terms
}

/// How `term` is written inside a session file, where text is JSON: a tab
/// or a quote there takes two characters.
fn on_disk(term: &str) -> String {
    let quoted = serde_json::to_string(term).unwrap_or_default();
    quoted
        .get(1..quoted.len().saturating_sub(1))
        .unwrap_or(term)
        .to_owned()
}

/// The best matches in each session at `paths` that `within` allows, the
/// best session first: a match holds `every` word, or else at least one.
fn ranked<'a>(
    paths: impl Iterator<Item = &'a PathBuf>,
    terms: &[String],
    weights: &[f64],
    every: bool,
    within: &Within,
) -> Vec<Hit> {
    let mut found: Vec<(f64, u64, Vec<Hit>)> = Vec::new();
    for path in paths {
        // Read back as the session is loaded, so a rewind has taken back what
        // it dropped and each message's place is its place when opened.
        let Some(conversation) = replay(path) else {
            continue;
        };
        let updated = conversation.updated;
        if conversation.id == within.exclude
            || (within.only != 0 && conversation.id != within.only)
            || within.after.is_some_and(|t| updated < t)
            || within.before.is_some_and(|t| updated >= t)
        {
            continue;
        }
        let text_at = |ordinal: Option<usize>| match ordinal {
            None => std::borrow::Cow::from(&conversation.title),
            Some(i) => searched(&conversation.messages[i]),
        };
        let places = std::iter::once(None).chain((0..conversation.messages.len()).map(Some));
        let mut matches: Vec<(f64, Option<usize>)> = places
            .filter_map(|at| {
                Some((
                    score(&text_at(at).to_lowercase(), terms, weights, every)?,
                    at,
                ))
            })
            .collect();
        // Best first, and in the order said among equals, the title first.
        matches.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
        if within.only == 0 {
            matches.truncate(HITS_PER_SESSION);
        }
        let Some(&(best, _)) = matches.first() else {
            continue;
        };
        let hits = matches
            .into_iter()
            .filter_map(|(_, ordinal)| {
                let role = match ordinal.map(|i| &conversation.messages[i]) {
                    None => "title".into(),
                    // A notice has the user's role, but the snippet rarely
                    // reaches its header, and a job's output read back as
                    // the user's words becomes a request they never made.
                    Some(m) if m.notice => "notice".into(),
                    Some(m) => m.role.clone(),
                };
                Some(Hit {
                    id: conversation.id,
                    title: conversation.title.clone(),
                    updated,
                    role,
                    ordinal,
                    snippet: snippet(&text_at(ordinal), terms, weights)?,
                })
            })
            .collect();
        found.push((best, updated, hits));
    }
    found.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    found.into_iter().flat_map(|(_, _, hits)| hits).collect()
}

/// How well `lower` matches `terms`: each word's count, flattened so a long
/// output repeating a word does not bury a short answer, times its weight;
/// None when it lacks a word it needs.
fn score(lower: &str, terms: &[String], weights: &[f64], every: bool) -> Option<f64> {
    let mut total = None;
    for (term, weight) in terms.iter().zip(weights) {
        let count = lower.matches(term.as_str()).count();
        if count == 0 {
            if every {
                return None;
            }
            continue;
        }
        let count = count as f64;
        *total.get_or_insert(0.0) += count * (REPEATS + 1.0) / (count + REPEATS) * weight;
    }
    total
}

/// The text of `message` a search looks through: what was said and, for a
/// turn that called tools, each tool's name and arguments.
pub(crate) fn searched(message: &Message) -> std::borrow::Cow<'_, str> {
    if message.tool_calls.is_empty() {
        return std::borrow::Cow::from(&message.content);
    }
    let mut text = message.content.clone();
    for call in &message.tool_calls {
        text.push_str(&format!("\n{} {}", call.name, call.arguments));
    }
    std::borrow::Cow::from(text)
}

/// `text` cut around the rarest of `terms` it holds.
fn snippet(text: &str, terms: &[String], weights: &[f64]) -> Option<String> {
    let lower = text.to_lowercase();
    let (term, _) = terms
        .iter()
        .zip(weights)
        .filter(|(t, _)| lower.contains(t.as_str()))
        .max_by(|a, b| a.1.total_cmp(b.1))?;
    around(text, term, SNIPPET_SIDE)
}

/// `text` on one line, cut to about `width` characters around the first of
/// `query`'s words it holds, or from its start when it holds none.
pub(crate) fn excerpt(text: &str, query: &str, width: usize) -> String {
    let lower = text.to_lowercase();
    let first = terms(query)
        .into_iter()
        .filter(|t| t.chars().count() < width)
        .filter_map(|t| Some((lower.find(t.as_str())?, t)))
        .min();
    let (term, side) = match &first {
        Some((_, term)) => (term.as_str(), (width - term.chars().count()) / 2),
        // An empty word is found at the start, with all the width after it.
        None => ("", width),
    };
    around(text, term, side).unwrap_or_default()
}

/// The first place `term` (already lowercase) occurs in `text`, with `side`
/// characters either side, on one line.
fn around(text: &str, term: &str, side: usize) -> Option<String> {
    let lower = text.to_lowercase();
    // Counted in characters, since lowercasing can change a character's
    // length in bytes and the cut is made in the original.
    let at = lower[..lower.find(term)?].chars().count();
    let from = at.saturating_sub(side);
    let to = at + term.chars().count() + side;
    let cut: String = text.chars().skip(from).take(to - from).collect();
    let mut snippet = crate::tools::one_line(&cut);
    if from > 0 {
        snippet.insert(0, '…');
    }
    if text.chars().count() > to {
        snippet.push('…');
    }
    Some(snippet)
}

pub fn sessions_dir() -> PathBuf {
    backend::data_dir().join("sessions")
}

pub(crate) fn session_path(id: u64) -> PathBuf {
    session_in(&backend::data_dir(), id)
}

/// Where session `id` is saved in data folder `data`.
pub(crate) fn session_in(data: &std::path::Path, id: u64) -> PathBuf {
    data.join("sessions").join(format!("{id}.jsonl"))
}

/// Where the helpers of a session keep their conversations.
pub(crate) fn helpers_dir(session: u64) -> PathBuf {
    // Outside `sessions/`, where every file is taken for a session, and in
    // the data folder, which no tool may read.
    backend::data_dir().join("agents").join(session.to_string())
}

/// Where side questions keep their conversations while they run.
fn asides_dir() -> PathBuf {
    backend::data_dir().join("asides")
}

/// Removes the conversations, and their locks, that side questions left
/// behind when their process quit or crashed mid-answer, and only those.
fn sweep_asides() {
    let Ok(entries) = std::fs::read_dir(asides_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let stale = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|at| at.elapsed().is_ok_and(|age| age > ASIDE_STALE));
        let kind = name.ends_with(".jsonl") || name.ends_with(".lock");
        if name.starts_with(ASIDE) && kind && stale {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

pub fn settings_path() -> PathBuf {
    backend::data_dir().join("settings.json")
}

/// The old single-file store, kept only so it can be converted once.
fn legacy_path() -> PathBuf {
    backend::data_dir().join("chats.json")
}

/// A lock held while a session file is being written.
///
/// The window and the terminal share these files and both append to them, so
/// two of them can be part way through a line at the same time -- which is
/// one corrupt record and, because a record is a whole message, one message
/// lost. The lock is the operating system's, on a file beside the session
/// that stays there: the system lets go of it when its holder closes the file
/// or dies, so no lock is ever stale and no one removes a lock file from
/// under its holder.
pub struct Lock(std::fs::File);

impl Lock {
    /// Wait up to a second for the lock, and fail rather than write without it.
    pub(crate) fn take(path: &std::path::Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))?;
        for _ in 0..40 {
            match file.try_lock() {
                Ok(()) => return Ok(Self(file)),
                Err(std::fs::TryLockError::WouldBlock) => {
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(std::fs::TryLockError::Error(e)) => return Err(e),
            }
        }
        // A holder keeps it for one append, so a second of waiting means
        // something is stuck. Writing anyway could tear a line; failing
        // leaves the messages unwritten in memory, and the next save sends
        // them.
        Err(std::io::Error::other(
            "another Letronna is still writing this session",
        ))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // Closing the file lets go too, but Windows may take its time over
        // a lock left to the close.
        let _ = self.0.unlock();
    }
}

/// Whether session `id` is surely not saved: a file that merely cannot be
/// read now does not count, as its kept copies would go with the answer.
fn gone(id: u64) -> bool {
    std::fs::metadata(session_path(id)).is_err_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

fn session_ids() -> Vec<u64> {
    let Ok(entries) = std::fs::read_dir(sessions_dir()) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        // A lock left by a crash shares its session's stem.
        .filter(|e| e.path().extension().is_some_and(|x| x == "jsonl"))
        .filter_map(|e| {
            // Each session's lock file sits beside it under the same number.
            e.file_name()
                .to_str()
                .and_then(|name| name.strip_suffix(".jsonl"))
                .and_then(|s| s.parse().ok())
        })
        .collect()
}

/// Load every session, newest last, plus the settings.
/// The store with the sessions listed but not read.
///
/// Every session's messages are in memory after `load`, which is most of what
/// it costs and almost none of what is used: one conversation is open and the
/// rest are a list of titles. This reads the head and meta of each file and
/// leaves the messages for `fill`, which is what both frontends want: the
/// terminal opens one session, and the window a few it lets go of again.
pub fn load_listing() -> Store {
    migrate();
    crate::history::sweep(gone);
    sweep_asides();
    let mut conversations: Vec<Conversation> = summaries().into_iter().map(listed).collect();
    conversations.sort_by_key(|c| c.updated);
    let (settings, settings_written) = settings();
    Store {
        conversations,
        active: 0,
        settings,
        settings_written,
    }
}

/// A session as its listing describes it, its messages left on disk.
fn listed(s: Summary) -> Conversation {
    let mut conversation = Conversation {
        id: s.id,
        title: s.title,
        updated: s.updated,
        workspace: s.workspace,
        pinned: s.pinned,
        todos: s.todos,
        model: s.model,
        gateway: s.gateway,
        effort: s.effort,
        goal: s.goal,
        imported_from: s.imported_from,
        worktree: s.worktree,
        memory_saving: s.memory_saving,
        memory_scope: s.memory_scope,
        counted: s.messages,
        ..Default::default()
    };
    // Record what the file already says. Left empty, the first save
    // wrote a fresh meta line into every session -- one saying
    // unpinned, no todos, whatever the window had set there.
    conversation.written = Some(conversation.meta());
    conversation
}

/// Session `id` as its file holds it now, if it has one.
pub(crate) fn session(id: u64) -> Option<Conversation> {
    replay(&session_path(id))
}

/// Read a session's messages, if they are not already here.
pub fn fill(conversation: &mut Conversation) {
    if !conversation.unfilled() {
        return;
    }
    if let Some(read) = messages(conversation.id) {
        conversation.filled_with(read);
    }
}

/// A session's messages as its file has them, the summary saved of the
/// first of them, the summaries the user asked for that still hold, and what
/// Auto mode's judge made of its calls.
pub type Contents = (
    Vec<Message>,
    Option<(usize, String)>,
    Vec<(usize, String)>,
    Vec<Judged>,
);

/// Session `id`'s messages, summaries and decisions as its file has them,
/// for filling it off the thread that holds it; None when the file cannot be
/// read.
pub fn messages(id: u64) -> Option<Contents> {
    replay(&session_path(id)).map(|c| (c.messages, c.summary, c.asked, c.decisions))
}

/// The settings as the file has them now.
pub fn current_settings() -> Settings {
    settings().0
}

/// What sizes a request beyond the model's own limits, in tokens.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct Sizing {
    /// The context cap, 0 for the default.
    pub cap: u64,
    /// The window assumed for a model whose own is not known.
    pub window: Option<u64>,
    /// The output limit assumed for a model whose own is not known.
    pub output: Option<u64>,
    /// Whether the turn's older calls go out without their reasoning once
    /// it comes to a chunk: the `reasoning_replay` setting says "chunked".
    pub chunked_reasoning: bool,
}

/// The sizing the settings file sets, with `LETRONNA_CONTEXT_WINDOW`,
/// `LETRONNA_MAX_OUTPUT` and `LETRONNA_REASONING_REPLAY` in place of its
/// window, output limit and reasoning replay.
pub(crate) fn sizing() -> Sizing {
    let settings = settings().0;
    let set = |tokens: u64| Some(tokens).filter(|&tokens| tokens > 0);
    Sizing {
        cap: settings.context_cap,
        window: set(overridden(
            settings.context_window,
            "LETRONNA_CONTEXT_WINDOW",
        )),
        output: set(overridden(settings.max_output, "LETRONNA_MAX_OUTPUT")),
        chunked_reasoning: chunks_reasoning(
            &settings.reasoning_replay,
            "LETRONNA_REASONING_REPLAY",
        ),
    }
}

/// The number of tokens the environment variable `name` holds, or `saved`
/// when it holds none; a 0 there unsets the saved one for the run.
fn overridden(saved: u64, name: &str) -> u64 {
    std::env::var(name)
        .ok()
        .and_then(|value| value.trim().parse().ok())
        .unwrap_or(saved)
}

/// Whether the reasoning replay the environment variable `name` holds, or
/// `saved` when it holds none, is "chunked"; anything else, "full" or a word
/// no replay goes by, sends every call of the turn its reasoning.
fn chunks_reasoning(saved: &str, name: &str) -> bool {
    std::env::var(name)
        .ok()
        .unwrap_or_else(|| saved.to_owned())
        .trim()
        .eq_ignore_ascii_case("chunked")
}

/// Ways of working that paired benchmark runs have yet to settle, each a
/// setting with a variable that says it for one run, as the adapters start
/// Letronna with an empty data folder.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Experiments {
    /// `helpers_share_tools`: on unless it says "off".
    pub helpers_share_tools: bool,
    /// `parallel_read_commands` says "on".
    pub parallel_read_commands: bool,
}

/// The experiments as the settings file and the environment had them when
/// this process first asked: read once, since which tools a request holds
/// and how its calls run must not change part way through a session.
pub fn experiments() -> Experiments {
    static READ: std::sync::OnceLock<Experiments> = std::sync::OnceLock::new();
    *READ.get_or_init(|| experiments_of(&settings().0, |name| std::env::var(name).ok()))
}

/// The experiments `settings` turns on, with what `env` holds for each in
/// place of the setting.
fn experiments_of(settings: &Settings, env: impl Fn(&str) -> Option<String>) -> Experiments {
    let word = |saved: &str, name: &str| {
        let said = env(name).unwrap_or_else(|| saved.to_owned());
        said.trim().to_ascii_lowercase()
    };
    let shares = word(
        &settings.helpers_share_tools,
        "LETRONNA_HELPERS_SHARE_TOOLS",
    );
    Experiments {
        helpers_share_tools: !matches!(shares.as_str(), "off" | "0"),
        parallel_read_commands: matches!(
            word(
                &settings.parallel_read_commands,
                "LETRONNA_PARALLEL_READ_COMMANDS"
            )
            .as_str(),
            "on" | "1"
        ),
    }
}

/// The tools the settings file switches off, read again each time so a hand
/// edit counts from the next request.
pub(crate) fn disabled_tools() -> Vec<String> {
    settings().0.disabled_tools
}

/// Whether the settings file turns computer use on, read again each time as
/// `disabled_tools` is.
pub(crate) fn computer_use() -> bool {
    settings().0.computer_use
}

/// Whether the settings file offers the `script` tool, which a session reads
/// once, as it starts.
pub(crate) fn script_tool() -> bool {
    settings().0.script_tool
}

/// Whether the settings file lets one reply's tool calls run together, read
/// again each time as `disabled_tools` is.
pub(crate) fn parallel_tool_calls() -> bool {
    settings().0.parallel_tool_calls
}

/// Whether the settings file has long turns reminded of the rules in force,
/// read again each time as `disabled_tools` is.
pub(crate) fn remind_rules() -> bool {
    settings().0.remind_rules
}

/// Whether the settings file has plan mode keep every tool, read again each
/// time as `disabled_tools` is.
pub(crate) fn plan_keeps_tools() -> bool {
    settings().0.plan_keeps_tools
}

/// Whether the settings file lets searches go to DuckDuckGo with no key,
/// read again each time as `disabled_tools` is.
pub(crate) fn keyless_search() -> bool {
    settings().0.keyless_search
}

/// The variable that has a run name no co-author of its own when it is 0,
/// as Letronna Cloud starts its runs: the cloud credits its sessions'
/// commits itself, as the account chose.
pub const COAUTHOR: &str = "LETRONNA_COAUTHOR";

/// Whether a session names Letronna as co-author of its commits: the
/// settings file's choice, read again each time, unless `COAUTHOR` says 0.
pub(crate) fn letronna_coauthor() -> bool {
    std::env::var(COAUTHOR).map_or(true, |v| v.trim() != "0") && settings().0.letronna_coauthor
}

/// The search Letronna's own gateway is asked for (search.rs): "auto",
/// "free" or "paid", as `letronna_search` names it, read again each time.
pub fn letronna_search() -> &'static str {
    search_named(&settings().0.letronna_search)
}

/// One of Letronna's searches by its name in the settings: "auto" unless
/// it is "free" or "paid".
pub fn search_named(name: &str) -> &'static str {
    match name.trim() {
        "free" => "free",
        "paid" => "paid",
        _ => crate::search::AUTO,
    }
}

/// The settings file. A file that will not read at all is kept aside by
/// `state::read` as before.
fn read_settings_file() -> Settings {
    let path = settings_path();
    crate::state::read::<serde_json::Value>(&path)
        .ok()
        .filter(|saved| !saved.is_null())
        .and_then(from_saved)
        .unwrap_or_else(|| crate::state::read(&path).unwrap_or_default())
}

/// The settings, and as JSON what the file said before a retired gateway is
/// moved, so the move is saved like any other change.
pub(crate) fn settings() -> (Settings, serde_json::Value) {
    let mut s = read_settings_file();
    let read = serde_json::to_value(&s).unwrap_or_default();
    retire_gateway(&mut s);
    (s, read)
}

/// Settings from their file's `bytes`, or `None` when those do not parse.
#[cfg(test)]
pub(crate) fn read_settings(bytes: &[u8]) -> Option<Settings> {
    from_saved(serde_json::from_slice(bytes).ok()?)
}

/// Settings from a saved file's JSON, with a mode saved under both its
/// names -- a newer save beside an old switch -- read as the newer.
fn from_saved(mut saved: serde_json::Value) -> Option<Settings> {
    crate::gate::keep_mode(&mut saved, "skip_approvals");
    serde_json::from_value(saved).ok()
}

/// A gateway that is no longer offered moves to the Letronna account. Its
/// model is dropped too, since that id belonged to the old gateway; an empty
/// model is picked again from the new list. One a plugin added stays while
/// the plugin is on, which in the window is before the settings are read.
///
/// So does Custom with no address left, and Custom left at an address that
/// was never the user's: Letronna's own, which listed the account's models a
/// second time under a key that is not the account's, and so took a pick of
/// them by name; or the server of the Claude Code plugin, gone with it. A
/// model picked on Letronna's address is Letronna's and is kept.
fn retire_gateway(s: &mut Settings) {
    use crate::gateway::{LETRONNA, gateways};
    // Saves from before Custom had its own field kept its URL in `endpoint`.
    // But `endpoint` follows whichever gateway was last used, so one that is
    // a gateway's own -- Letronna's, for anyone signed in -- was never Custom's.
    if s.gateway == "Custom"
        && s.custom_endpoint.is_empty()
        && crate::gateway::named(&s.endpoint).is_none()
    {
        s.custom_endpoint = s.endpoint.clone();
    }
    let at_letronna = same_base(&s.custom_endpoint, &crate::auth::hosted_base());
    if at_letronna || claude_code_server(&s.custom_endpoint) {
        s.custom_endpoint.clear();
    }
    let no_address = s.gateway == "Custom" && s.custom_endpoint.trim().is_empty();
    if s.gateway.is_empty() || (crate::gateway::offered(&s.gateway) && !no_address) {
        return;
    }
    let keep_model = at_letronna && !claude_code_model(&s.model);
    s.gateway = LETRONNA.into();
    s.endpoint = gateways()[0].base.into();
    if !keep_model {
        s.model.clear();
    }
}

/// Whether two gateway addresses are the same, a slash or the chat route on
/// the end aside.
fn same_base(one: &str, other: &str) -> bool {
    let bare = |url: &str| {
        let url = url.trim().trim_end_matches('/');
        url.strip_suffix("/chat/completions")
            .unwrap_or(url)
            .to_owned()
    };
    !one.trim().is_empty() && bare(one) == bare(other)
}

/// Whether `url` is the address the Claude Code plugin served its models at
/// on this computer, which went with the plugin.
fn claude_code_server(url: &str) -> bool {
    let url = url.trim().trim_end_matches('/');
    url.starts_with("http://127.0.0.1:") && url.ends_with("/claude-code/v1")
}

/// Whether `model` is one the Claude Code plugin offered.
fn claude_code_model(model: &str) -> bool {
    model == "claude-code" || model.starts_with("claude-code-")
}

#[cfg(test)]
mod retire_tests {
    use super::*;

    #[test]
    fn a_session_is_named_by_its_first_line_with_words() {
        assert_eq!(
            name_from(
                "

  hello there
more"
            ),
            "hello there"
        );
        assert_eq!(name_from(""), "Untitled session");
        assert_eq!(
            name_from(
                " 
	"
            ),
            "Untitled session"
        );
        assert_eq!(name_from(&"x".repeat(50)).len(), 40);
    }

    #[test]
    fn a_gateway_a_plugin_added_stays_while_it_is_offered() {
        crate::gateway::add(crate::gateway::Added {
            name: "Plugged in",
            base: "http://127.0.0.1:9/v1".into(),
            key: String::new(),
            responses: false,
        });
        let mut s = Settings {
            gateway: "Plugged in".into(),
            model: "some-model".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        crate::gateway::remove("Plugged in");
        assert_eq!(
            (s.gateway.as_str(), s.model.as_str()),
            ("Plugged in", "some-model")
        );
        retire_gateway(&mut s);
        assert_eq!(s.gateway, "Letronna");
    }

    #[test]
    fn custom_at_letronnas_own_address_moves_to_the_account() {
        let hosted = crate::auth::hosted_base();
        // As a signed-in user's settings were left: Custom with no address of
        // its own, the last request's address being Letronna's.
        let mut s = Settings {
            gateway: "Custom".into(),
            endpoint: hosted.clone(),
            model: "claude-code".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (
                s.gateway.as_str(),
                s.custom_endpoint.as_str(),
                s.model.as_str()
            ),
            ("Letronna", "", "")
        );
        // Custom pointed at it by hand: the model is Letronna's, and kept.
        let mut s = Settings {
            gateway: "Custom".into(),
            custom_endpoint: format!("{hosted}/"),
            model: "deepseek/deepseek-chat".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (
                s.gateway.as_str(),
                s.custom_endpoint.as_str(),
                s.model.as_str()
            ),
            ("Letronna", "", "deepseek/deepseek-chat")
        );
        // As this was found on a Mac: Custom at Letronna's address with the
        // plugin's model, which Letronna never had.
        let mut s = Settings {
            gateway: "Custom".into(),
            custom_endpoint: hosted.clone(),
            endpoint: hosted.clone(),
            model: "claude-code".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (
                s.gateway.as_str(),
                s.custom_endpoint.as_str(),
                s.model.as_str()
            ),
            ("Letronna", "", "")
        );
        // Not in use, it only loses the address.
        let mut s = Settings {
            gateway: "OpenAI".into(),
            custom_endpoint: hosted.clone(),
            model: "gpt-x".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (s.gateway.as_str(), s.custom_endpoint.as_str()),
            ("OpenAI", "")
        );
    }

    #[test]
    fn the_claude_code_plugins_leftovers_go_with_it() {
        let mut s = Settings {
            gateway: "Custom".into(),
            custom_endpoint: "http://127.0.0.1:47821/claude-code/v1".into(),
            model: "claude-code-opus".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (
                s.gateway.as_str(),
                s.custom_endpoint.as_str(),
                s.model.as_str()
            ),
            ("Letronna", "", "")
        );
        // A server of the user's own keeps everything, whatever its models.
        let mut s = Settings {
            gateway: "Custom".into(),
            custom_endpoint: "http://localhost:8000/v1".into(),
            model: "claude-code".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (
                s.gateway.as_str(),
                s.custom_endpoint.as_str(),
                s.model.as_str()
            ),
            ("Custom", "http://localhost:8000/v1", "claude-code")
        );
        // An old save's URL in `endpoint` still becomes Custom's.
        let mut s = Settings {
            gateway: "Custom".into(),
            endpoint: "http://localhost:9000/v1".into(),
            model: "local".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (s.gateway.as_str(), s.custom_endpoint.as_str()),
            ("Custom", "http://localhost:9000/v1")
        );
    }

    #[test]
    fn a_session_held_to_custom_with_no_address_follows_the_settings() {
        let mut store = Store::default();
        store.settings.gateway = "Letronna".into();
        store.settings.model = "deepseek/deepseek-chat".into();
        store.conversations.push(Conversation {
            gateway: Some("Custom".into()),
            model: Some("claude-code".into()),
            ..Default::default()
        });
        assert_eq!(store.model(0), ("Letronna", "deepseek/deepseek-chat"));
        store.settings.custom_endpoint = "http://localhost:8000/v1".into();
        assert_eq!(store.model(0), ("Custom", "claude-code"));
    }

    #[test]
    fn old_gateways_move_to_letronna_and_known_ones_stay() {
        let mut s = Settings {
            gateway: "Retired".into(),
            model: "retired/some-model".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(s.gateway, "Letronna");
        assert_eq!(s.endpoint, "https://api.letronna.ai/v1");
        assert!(s.model.is_empty());

        let mut s = Settings {
            gateway: "OpenRouter".into(),
            model: "x/y".into(),
            ..Default::default()
        };
        retire_gateway(&mut s);
        assert_eq!(
            (s.gateway.as_str(), s.model.as_str()),
            ("OpenRouter", "x/y")
        );
    }
}

pub fn load() -> Store {
    migrate();
    crate::history::sweep(gone);
    sweep_asides();
    let mut conversations: Vec<Conversation> = session_ids()
        .into_iter()
        .filter_map(|id| replay(&session_path(id)))
        .collect();
    conversations.sort_by_key(|c| c.updated);
    let (settings, settings_written) = settings();
    let active = conversations
        .iter()
        .position(|c| c.id == settings.active_id)
        .unwrap_or(conversations.len().saturating_sub(1));
    Store {
        conversations,
        active,
        settings,
        settings_written,
    }
}

/// Convert a `chats.json` written by an earlier build, once. The original is
/// kept beside the new files rather than deleted, so a conversion that goes
/// wrong is recoverable.
fn migrate() {
    let legacy = legacy_path();
    if !legacy.exists() || sessions_dir().exists() {
        return;
    }
    #[derive(Deserialize)]
    struct Old {
        #[serde(default)]
        conversations: Vec<Conversation>,
        #[serde(default)]
        endpoint: String,
        #[serde(default)]
        model: String,
        #[serde(default)]
        gateway: String,
        #[serde(default)]
        presence: String,
        #[serde(default)]
        skip_approvals: crate::gate::Mode,
    }
    let Ok(bytes) = std::fs::read(&legacy) else {
        return;
    };
    let Ok(old) = serde_json::from_slice::<Old>(&bytes) else {
        return;
    };
    if std::fs::create_dir_all(sessions_dir()).is_err() {
        return;
    }
    let mut store = Store {
        conversations: old.conversations,
        active: 0,
        settings: Settings {
            endpoint: old.endpoint,
            model: old.model,
            gateway: old.gateway,
            presence: old.presence,
            mode: old.skip_approvals,
            active_id: 0,
            // Settings that did not exist when the old file was written.
            ..Default::default()
        },
        ..Default::default()
    };
    store.assign_ids();
    if store.save().is_ok() {
        let _ = std::fs::rename(&legacy, legacy.with_extension("json.migrated"));
    }
}

pub fn keys_path() -> PathBuf {
    backend::data_dir().join("keys.json")
}

/// Put one key in the vault, or the keys file where there is none, or with
/// `None` take it out, leaving every other slot as it is, and give back the
/// keys there are then. One at a time, so a key the vault would not give is
/// never saved over or cleared by a whole list that went without it.
pub fn save_key(
    slot: &str,
    key: Option<&str>,
) -> std::io::Result<std::collections::HashMap<String, String>> {
    std::fs::create_dir_all(backend::data_dir())?;
    let name = format!("{}{slot}", crate::vault::KEYS);
    let mut refused = None;
    crate::state::update::<std::collections::HashMap<String, String>>(&keys_path(), |disk| {
        let in_file = disk
            .get(slot)
            .and_then(|key| key.as_str())
            .unwrap_or_default();
        let kept = match key {
            Some(key) => crate::vault::save(&name, key, in_file),
            // Out of the vault whether or not it could be read, and a vault
            // that will not let it go is the error, the key left listed.
            None => crate::vault::remove(&name).map(|()| String::new()),
        };
        let kept = match kept {
            Ok(kept) => kept,
            Err(why) => {
                refused = Some(why);
                return;
            }
        };
        match (disk.as_object_mut(), kept.is_empty()) {
            (Some(keys), false) => {
                keys.insert(slot.to_owned(), kept.into());
            }
            (Some(keys), true) => {
                keys.remove(slot);
            }
            (None, false) => *disk = serde_json::json!({ slot: kept }),
            (None, true) => {}
        }
    })
    .map_err(std::io::Error::other)?;
    // A gateway's models are listed with its key, so the router asks again.
    crate::providers::router::forget();
    match refused {
        Some(why) => Err(std::io::Error::other(why)),
        None => Ok(load_keys()),
    }
}

/// Write a file only its owner may read: the keys, the sign-in, bot tokens.
/// On Linux and macOS a plain write leaves it readable by every account on
/// the machine (0644), so it is made 0600, a file from before included;
/// on Windows the user's own AppData is private already.
pub fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)?;
        // `mode` only applies to a new file.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(bytes)
    }
    #[cfg(not(unix))]
    std::fs::write(path, bytes)
}

/// Every saved key by slot: the keys file's, and the vault's for each slot
/// the file has none in.
pub fn load_keys() -> std::collections::HashMap<String, String> {
    let mut keys: std::collections::HashMap<String, String> =
        crate::state::read(&keys_path()).unwrap_or_default();
    for (slot, key) in crate::vault::under(crate::vault::KEYS) {
        if keys.get(&slot).is_none_or(|copy| copy.is_empty()) {
            keys.insert(slot, key);
        }
    }
    keys
}

pub fn bucket(updated: u64, now: u64) -> &'static str {
    let age = now.saturating_sub(updated);
    match age / 86_400 {
        0 => "TODAY",
        1 => "YESTERDAY",
        2..=6 => "EARLIER THIS WEEK",
        7..=13 => "LAST WEEK",
        _ => "OLDER",
    }
}

pub fn age_label(updated: u64, now: u64) -> String {
    let age = now.saturating_sub(updated);
    if updated == 0 {
        String::new()
    } else if age < 3600 {
        format!("{}m", (age / 60).max(1))
    } else if age < 86_400 {
        format!("{}h", age / 3600)
    } else {
        format!("{}d", age / 86_400)
    }
}

#[cfg(test)]
pub(crate) mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    #[cfg(unix)]
    use super::write_private;
    use super::{
        ASIDE_STALE, Conversation, HITS_PER_SESSION, Judged, Lock, SNIPPET_SIDE, Store, Todo,
        Transcript, Within, apply_todo, fill, load, load_listing, modified, replay, search,
        search_in, search_through, summaries, todos_in,
    };
    use crate::chat::{Message, ToolCall};

    /// Held by every test that points `AGENT_DATA_DIR` somewhere: it is one
    /// variable for the whole process, and tests run side by side.
    pub(crate) static DATA_DIR: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// A test's own empty folder, removed when dropped.
    pub(crate) struct Scratch(std::path::PathBuf);

    impl Scratch {
        /// A new folder `letronna-{name}` in the temporary folder, with this
        /// process's id and a number counted within it after the name, so
        /// neither another test nor another test run on the machine shares it.
        pub(crate) fn new(name: &str) -> Self {
            static MADE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let made = MADE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let dir =
                std::env::temp_dir().join(format!("letronna-{name}-{}-{made}", std::process::id()));
            // Left by a killed run that had this process's id.
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }
    }

    impl std::ops::Deref for Scratch {
        type Target = std::path::PathBuf;

        fn deref(&self) -> &std::path::PathBuf {
            &self.0
        }
    }

    impl AsRef<std::path::Path> for Scratch {
        fn as_ref(&self) -> &std::path::Path {
            &self.0
        }
    }

    impl AsRef<std::ffi::OsStr> for Scratch {
        fn as_ref(&self) -> &std::ffi::OsStr {
            self.0.as_os_str()
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Scratch {
        Scratch::new(&format!("store-{name}"))
    }

    #[cfg(unix)]
    #[test]
    fn a_private_file_is_readable_by_its_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("private");
        let file = dir.join("keys.json");
        // Written before, readable by everyone, as a plain write left it.
        std::fs::write(&file, "{}").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        write_private(&file, b"{\"OpenAI\":\"sk-1\"}").unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "{\"OpenAI\":\"sk-1\"}"
        );
        // And a new one starts that way.
        let fresh = dir.join("account.json");
        write_private(&fresh, b"{}").unwrap();
        assert_eq!(
            std::fs::metadata(&fresh).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    fn said(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: text.into(),
            ..Default::default()
        }
    }

    /// Session 1, holding `n` messages that each say their own place.
    fn numbered(n: usize) -> Conversation {
        Conversation {
            id: 1,
            messages: (0..n).map(|i| said("user", &i.to_string())).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn appends_only_what_is_new_and_replays_to_the_same_thing() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("append");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            title: "first".into(),
            updated: 100,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let after_first = std::fs::read_to_string(&path).unwrap();
        // head + meta + one message
        assert_eq!(after_first.lines().count(), 3);

        // Saving again with nothing new must not write anything.
        store.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after_first);

        // A new message appends one line; the rest of the file is untouched.
        store.conversations[0]
            .messages
            .push(said("assistant", "hi"));
        store.save().unwrap();
        let after_second = std::fs::read_to_string(&path).unwrap();
        assert!(after_second.starts_with(&after_first));
        assert_eq!(after_second.lines().count(), 4);

        // A title change appends a meta line, not a rewrite.
        store.conversations[0].title = "renamed".into();
        store.conversations[0].todos = vec![Todo {
            text: "step".into(),
            status: "doing".into(),
        }];
        store.save().unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .starts_with(&after_second)
        );

        let back = replay(&path).unwrap();
        assert_eq!(back.id, 1);
        assert_eq!(back.title, "renamed");
        assert_eq!(back.todos.len(), 1);
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].content, "hi");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn listing_then_saving_leaves_every_session_as_it_was() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("listing");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            title: "pinned in the window".into(),
            updated: 100,
            pinned: true,
            todos: vec![Todo {
                text: "step".into(),
                status: "doing".into(),
            }],
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::read_to_string(&path).unwrap();

        // What the terminal does every time it starts: list, then save.
        let mut listed = load_listing();
        assert!(listed.conversations[0].pinned);
        assert_eq!(listed.conversations[0].todos.len(), 1);
        listed.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // Opening the session and saving with nothing said adds nothing too.
        fill(&mut listed.conversations[0]);
        listed.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_retired_gateway_moves_to_letronna_in_the_window_too() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("retired");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::write(
            dir.join("settings.json"),
            r#"{"gateway":"Retired","endpoint":"https://retired.example/v1","model":"retired/some-model"}"#,
        )
        .unwrap();

        // The window reads everything with `load`, the terminal with
        // `load_listing`; both must see the same gateway.
        let window = load().settings;
        assert_eq!(window.gateway, "Letronna");
        assert!(window.model.is_empty());
        assert_eq!(load_listing().settings.gateway, window.gateway);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_torn_line_costs_one_message_not_the_session() {
        let dir = scratch("torn");
        let path = dir.join("torn.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"head\",\"id\":7,\"created\":5}\n\
             {\"type\":\"meta\",\"title\":\"kept\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"one\"}\n\
             {\"type\":\"message\",\"role\":\"user\",\"cont\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"three\"}\n",
        )
        .unwrap();
        let back = replay(&path).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.title, "kept");
        // The half-written line is dropped; the ones either side survive.
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].content, "three");
    }

    #[test]
    fn a_torn_last_line_is_sealed_before_the_next_append() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("torn-tail");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        // A write cut short: half a record and no newline after it.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\":\"message\",\"role\":\"us");
        std::fs::write(&path, text).unwrap();

        store.conversations[0].messages.push(said("user", "again"));
        store.save().unwrap();
        let back = replay(&path).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "again"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_bad_byte_costs_one_line() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("bad-byte");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let path = sessions.join("7.jsonl");
        // The third line was cut in the middle of the two bytes of an é.
        let mut bytes = b"{\"type\":\"head\",\"id\":7,\"created\":5}\n\
            {\"type\":\"meta\",\"title\":\"kept\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}\n\
            {\"type\":\"message\",\"role\":\"user\",\"content\":\"one\"}\n\
            {\"type\":\"message\",\"role\":\"user\",\"content\":\"caf"
            .to_vec();
        bytes.push(0xC3);
        bytes.extend_from_slice(
            b"\n{\"type\":\"message\",\"role\":\"user\",\"content\":\"three\"}\n",
        );
        std::fs::write(&path, bytes).unwrap();

        let back = replay(&path).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["one", "three"]);
        let listed = summaries();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].title, "kept");
        assert_eq!(
            search_in(&sessions, "three", &Within::default(), 10).len(),
            1
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_with_a_broken_head_keeps_its_id() {
        let dir = scratch("broken-head");
        let path = dir.join("7.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"head\",\"id\":7,\"cr\n\
             {\"type\":\"meta\",\"title\":\"kept\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();
        let back = replay(&path).unwrap();
        assert_eq!(back.id, 7);
        assert_eq!(back.title, "kept");
        assert_eq!(back.messages.len(), 1);
        let hits = search_in(&dir, "hello", &Within::default(), 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].id, 7);
    }

    /// A reply from the model making calls with these ids.
    fn calling(ids: &[&str]) -> Message {
        Message {
            role: "assistant".into(),
            tool_calls: ids
                .iter()
                .map(|id| crate::chat::ToolCall {
                    id: (*id).into(),
                    name: "run_command".into(),
                    arguments: "{}".into(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_decision_takes_the_place_of_its_calls_result() {
        // Models that send no ids get the same ones in every reply.
        let mut result = said("tool", "done");
        result.tool_call_id = "call_0".into();
        let messages = [
            said("user", "build it"),
            calling(&["call_0"]),
            result,
            calling(&["call_0", "call_1"]),
        ];
        let decided = |id: &str| Judged::of(&messages, id, String::new(), String::new()).at;
        assert_eq!(decided("call_0"), 4);
        assert_eq!(decided("call_1"), 5);
        assert_eq!(decided("unknown"), 4);
    }

    #[test]
    fn judge_decisions_are_appended_and_read_back_after_a_restart() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("decisions");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        let messages = vec![said("user", "build it"), calling(&["a"])];
        let made = Judged::of(
            &messages,
            "a",
            "run_command(cargo build)".into(),
            "auto: allowed".into(),
        );
        store.conversations.push(Conversation {
            id: 1,
            title: "judged".into(),
            updated: 100,
            messages,
            decisions: vec![made.clone()],
            ..Default::default()
        });
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let written = std::fs::read_to_string(&path).unwrap();
        assert!(
            written
                .lines()
                .last()
                .unwrap()
                .starts_with("{\"type\":\"auto\"")
        );
        store.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), written);

        assert_eq!(
            replay(&path).unwrap().decisions,
            std::slice::from_ref(&made)
        );
        // Listed, opened and saved again, as a restart does, adds nothing.
        let mut listed = load_listing();
        fill(&mut listed.conversations[0]);
        assert_eq!(listed.conversations[0].decisions, [made]);
        listed.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), written);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_file_from_before_decisions_still_loads() {
        let dir = scratch("undecided");
        let path = dir.join("old.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"head\",\"id\":3,\"created\":5}\n\
             {\"type\":\"meta\",\"title\":\"old\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"one\"}\n",
        )
        .unwrap();
        let back = replay(&path).unwrap();
        assert_eq!(back.messages.len(), 1);
        assert!(back.decisions.is_empty());
    }

    #[test]
    fn search_finds_what_was_said_newest_session_first() {
        let dir = scratch("search");
        let session = |id: u64, title: &str, updated: u64, said: &[(&str, &str)]| {
            let mut text = format!(
                "{{\"type\":\"head\",\"id\":{id},\"created\":{updated}}}\n\
                 {{\"type\":\"meta\",\"title\":\"{title}\",\"updated\":{updated},\"pinned\":false,\"workspace\":null,\"todos\":[]}}\n"
            );
            for (role, content) in said {
                text.push_str(&format!(
                    "{{\"type\":\"message\",\"role\":\"{role}\",\"content\":\"{content}\"}}\n"
                ));
            }
            std::fs::write(dir.join(format!("{id}.jsonl")), text).unwrap();
        };
        session(
            1,
            "Database",
            100,
            &[
                ("user", "Should we use Postgres or SQLite?"),
                ("assistant", "Postgres, for the concurrency."),
            ],
        );
        session(2, "Moving", 300, &[("user", "Moving to postgres now")]);
        session(3, "Unrelated", 200, &[("user", "nothing to see")]);

        let hits = search_in(&dir, "POSTGRES", &Within::default(), 10);
        let found: Vec<(u64, &str)> = hits.iter().map(|h| (h.id, h.role.as_str())).collect();
        assert_eq!(found, [(2, "user"), (1, "user"), (1, "assistant")]);
        assert_eq!(hits[1].title, "Database");
        assert_eq!(hits[1].snippet, "Should we use Postgres or SQLite?");
        assert_eq!(search_in(&dir, "POSTGRES", &Within::default(), 1).len(), 1);
        assert!(search_in(&dir, "mysql", &Within::default(), 10).is_empty());
    }

    #[test]
    fn a_search_hit_on_a_notice_is_not_the_users() {
        let dir = scratch("search-notice");
        std::fs::write(
            dir.join("1.jsonl"),
            "{\"type\":\"message\",\"role\":\"user\",\"content\":\"run the tests\"}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"the tests failed\",\"notice\":true}\n",
        )
        .unwrap();
        let roles: Vec<String> = search_in(&dir, "tests", &Within::default(), 10)
            .into_iter()
            .map(|hit| hit.role)
            .collect();
        assert_eq!(roles, ["user", "notice"]);
    }

    #[test]
    fn a_long_match_is_shown_with_its_surroundings_only() {
        let text = format!("{} the needle {}", "a".repeat(300), "b".repeat(300));
        let snippet = super::around(&text, "needle", SNIPPET_SIDE).unwrap();
        assert!(
            snippet.starts_with('…') && snippet.ends_with('…'),
            "{snippet}"
        );
        assert!(snippet.contains("the needle"));
        assert!(snippet.chars().count() < 220, "{}", snippet.chars().count());
    }

    /// Writes session `id` into `dir` with `messages`, each a JSON object of
    /// a message's fields.
    fn session_file(
        dir: &std::path::Path,
        id: u64,
        title: &str,
        updated: u64,
        messages: &[serde_json::Value],
    ) {
        let mut text = format!(
            "{}\n{}\n",
            serde_json::json!({"type": "head", "id": id, "created": updated}),
            serde_json::json!({"type": "meta", "title": title, "updated": updated,
                "pinned": false, "workspace": null, "todos": []}),
        );
        for message in messages {
            let mut line = message.clone();
            line["type"] = "message".into();
            text.push_str(&format!("{line}\n"));
        }
        std::fs::write(dir.join(format!("{id}.jsonl")), text).unwrap();
    }

    fn user(text: &str) -> serde_json::Value {
        serde_json::json!({"role": "user", "content": text})
    }

    #[test]
    fn a_search_needs_every_word_but_in_any_order() {
        let dir = scratch("search-every");
        session_file(
            &dir,
            1,
            "",
            100,
            &[user("tauri is set up"), user("the build is fine")],
        );
        session_file(
            &dir,
            2,
            "",
            50,
            &[user("hi"), user("an error during the tauri build")],
        );
        let hits = search_in(&dir, "Tauri build error", &Within::default(), 10);
        let found: Vec<(u64, Option<usize>)> = hits.iter().map(|h| (h.id, h.ordinal)).collect();
        assert_eq!(found, [(2, Some(1))]);
        assert_eq!(hits[0].snippet, "an error during the tauri build");
    }

    #[test]
    fn when_nothing_holds_every_word_the_rarest_words_find_it() {
        let dir = scratch("search-any");
        // The newest sessions all say "error"; one older one names the tool.
        for id in 1..=5 {
            session_file(&dir, id, "", 100 + id, &[user("an error again")]);
        }
        session_file(&dir, 9, "", 10, &[user("wrangler would not deploy")]);
        let hits = search_in(&dir, "wrangler error flamingo", &Within::default(), 10);
        let ids: Vec<u64> = hits.iter().map(|h| h.id).collect();
        assert_eq!(ids, [9, 5, 4, 3, 2, 1]);
        // One word is looked for once, not again with any of them.
        assert!(search_in(&dir, "flamingo", &Within::default(), 10).is_empty());
    }

    #[test]
    fn a_rare_word_said_once_outranks_a_common_one_said_often() {
        let dir = scratch("search-count");
        for id in 1..=4 {
            session_file(&dir, id, "", 100 + id, &[user("the cache")]);
        }
        let noisy = "cache ".repeat(50);
        session_file(&dir, 5, "", 10, &[user(&noisy), user("the proxy")]);
        let hits = search_in(&dir, "cache proxy", &Within::default(), 10);
        let found: Vec<(u64, Option<usize>)> = hits.iter().map(|h| (h.id, h.ordinal)).collect();
        assert_eq!(found[..2], [(5, Some(1)), (5, Some(0))]);
        let ids: Vec<u64> = hits[2..].iter().map(|h| h.id).collect();
        assert_eq!(ids, [4, 3, 2, 1]);
    }

    #[test]
    fn titles_and_tool_arguments_are_searched() {
        let dir = scratch("search-title-args");
        let call = serde_json::json!({"role": "assistant", "content": "Building it.",
            "tool_calls": [{"id": "1", "name": "run_command",
                "arguments": "{\"command\":\"npx wrangler deploy\"}"}]});
        session_file(&dir, 1, "Wrangler setup", 100, &[user("go"), call]);
        let hits = search_in(&dir, "wrangler", &Within::default(), 10);
        let found: Vec<(&str, Option<usize>)> =
            hits.iter().map(|h| (h.role.as_str(), h.ordinal)).collect();
        assert_eq!(found, [("title", None), ("assistant", Some(1))]);
        assert_eq!(hits[0].snippet, "Wrangler setup");
        assert!(
            hits[1].snippet.contains("run_command"),
            "{}",
            hits[1].snippet
        );
        assert!(
            hits[1].snippet.contains("npx wrangler deploy"),
            "{}",
            hits[1].snippet
        );
    }

    #[test]
    fn a_search_leaves_out_the_session_asking_and_keeps_to_its_dates() {
        let dir = scratch("search-within");
        for (id, updated) in [(1, 100), (2, 200), (3, 300)] {
            session_file(&dir, id, "", updated, &[user("the fig tree")]);
        }
        let ids = |within: Within| -> Vec<u64> {
            search_in(&dir, "fig", &within, 10)
                .iter()
                .map(|h| h.id)
                .collect()
        };
        let within = Within {
            exclude: 3,
            ..Within::default()
        };
        assert_eq!(ids(within), [2, 1]);
        let within = Within {
            after: Some(200),
            ..Within::default()
        };
        assert_eq!(ids(within), [3, 2]);
        let within = Within {
            before: Some(200),
            ..Within::default()
        };
        assert_eq!(ids(within), [1]);
    }

    #[test]
    fn cjk_text_and_escaped_characters_are_found() {
        let dir = scratch("search-cjk");
        session_file(&dir, 1, "", 100, &[user("我们用哪个数据库？迁移明天做。")]);
        session_file(
            &dir,
            2,
            "",
            90,
            &[user("the column\tname in C:\\Users\\me")],
        );
        let ids = |query: &str| -> Vec<u64> {
            let hits = search_in(&dir, query, &Within::default(), 10);
            hits.iter().map(|h| h.id).collect()
        };
        assert_eq!(ids("数据库"), [1]);
        assert_eq!(ids("数据库 迁移"), [1]);
        // On disk a tab and a backslash are written as two characters each.
        assert_eq!(ids("\"column\tname\""), [2]);
        assert_eq!(ids("c:\\users"), [2]);
    }

    #[test]
    fn a_query_is_cut_into_words_with_quoted_words_kept_together() {
        let words = super::terms("Why did \"cargo build\" fail? (tauri.conf.json) c++ -> WHY");
        assert_eq!(
            words,
            [
                "why",
                "did",
                "cargo build",
                "fail",
                "tauri.conf.json",
                "c++",
                "->"
            ]
        );
        let many: String = (0..40).map(|i| format!("w{i} ")).collect();
        assert_eq!(super::terms(&many).len(), super::MAX_TERMS);
        assert!(super::terms("  \"\" ").is_empty());
    }

    #[test]
    fn a_long_message_read_back_is_cut_around_the_query() {
        let text = format!("{} the needle {}", "a ".repeat(2_000), "b ".repeat(2_000));
        let cut = super::excerpt(&text, "haystack NEEDLE", 200);
        assert!(cut.starts_with('…') && cut.ends_with('…'), "{cut}");
        assert!(cut.contains("the needle"), "{cut}");
        assert!(cut.chars().count() <= 202, "{}", cut.chars().count());
        let start = super::excerpt(&text, "", 200);
        assert!(start.starts_with("a a") && start.ends_with('…'), "{start}");
        assert!(start.chars().count() <= 201, "{}", start.chars().count());
    }

    #[test]
    fn the_tool_reads_around_a_hit_under_the_tool_result_cap() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("search-read");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        let long = |n: usize| {
            user(&format!(
                "{} fig {n} {}",
                "a ".repeat(9_000),
                "b ".repeat(9_000)
            ))
        };
        let messages: Vec<serde_json::Value> = (0..12).map(long).collect();
        session_file(&sessions, 1, &"t".repeat(5_000), 100, &messages);
        session_file(&sessions, 2, "", 200, &[user("the fig asking")]);

        let ctx = crate::tools::ToolContext {
            conversation: 2,
            ..Default::default()
        };
        let found = crate::tools::run("search_sessions", r#"{"query":"fig"}"#, &ctx).unwrap();
        assert!(
            found.contains("#0 user") && !found.contains("asking"),
            "{found}"
        );

        let args = r#"{"session":1,"around":5,"query":"fig"}"#;
        let read = crate::tools::run("search_sessions", args, &ctx).unwrap();
        assert!(
            read.chars().count() < crate::chat::MAX_TOOL_CHARS,
            "{}",
            read.len()
        );
        assert!(
            read.contains("#2 user") && read.contains("#8 user"),
            "{read}"
        );
        assert!(
            !read.contains("#1 user") && !read.contains("#9 user"),
            "{read}"
        );
        assert!(read.contains("fig 5") && read.contains("fig 8"), "{read}");
        let args = r#"{"session":1,"around":12}"#;
        let past = crate::tools::run("search_sessions", args, &ctx);
        assert!(past.is_err_and(|e| e.contains("#0 to #11")));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn search_sessions_finds_text_in_the_current_session_by_query() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("search-current");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        // Numbered on disk as in memory, where the system prompt numbers
        // the user's kept words: after a rewind, a notice and a failure too.
        let mut store = Store::default();
        store.conversations.push(numbered(4));
        store.save().unwrap();
        store.conversations[0].rewind(2);
        store.conversations[0].messages.extend([
            Message {
                notice: true,
                ..said("user", "Job #2 finished.")
            },
            said(crate::failure::ROLE, "The gateway was busy."),
            said("user", "never touch the fig module"),
            said("assistant", "Understood."),
        ]);
        // More of its messages match than a search of every session gives
        // one session.
        for i in 0..HITS_PER_SESSION {
            let fig = said("user", &format!("the fig module, take {i}"));
            store.conversations[0].messages.push(fig);
        }
        store.save().unwrap();
        let at = (store.conversations[0].messages.iter())
            .position(|m| m.content.contains("fig module"))
            .unwrap();
        let ctx = crate::tools::ToolContext {
            conversation: 1,
            ..Default::default()
        };
        let search = |args: &str| crate::tools::run("search_sessions", args, &ctx);
        let everywhere = search(r#"{"query":"fig"}"#);
        let here = search(r#"{"session":1,"query":"fig module"}"#);
        let around = search(&format!(r#"{{"session":1,"around":{at}}}"#));
        let missing = search(r#"{"session":9,"query":"fig"}"#);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };

        // Searching every session still leaves the one asking out.
        let everywhere = everywhere.unwrap();
        assert!(everywhere.starts_with("No earlier session"), "{everywhere}");
        let here = here.unwrap();
        let said = format!("#{at} user: never touch the fig module");
        assert!(here.contains(&said), "{here}");
        assert_eq!(
            here.matches("fig module").count(),
            HITS_PER_SESSION + 1,
            "{here}"
        );
        assert!(around.unwrap().contains(&said));
        assert!(missing.is_err_and(|e| e.contains("no saved session 9")));
    }

    #[test]
    fn a_rewound_session_reads_back_shorter() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::read_to_string(&path).unwrap();

        // The file is cut back to the head, the meta and what is left.
        store.conversations[0].rewind(2);
        store.save().unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        let kept: String = before.split_inclusive('\n').take(4).collect();
        assert_eq!(after, kept);

        // It is written once: saving again with nothing new adds nothing.
        store.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);

        let back = replay(&path).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].content, "1");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn rewinding_twice_before_a_save_keeps_the_shorter() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-twice");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        store.conversations[0].rewind(4);
        store.conversations[0].rewind(2);
        store.save().unwrap();

        let back = replay(&dir.join("sessions/1.jsonl")).unwrap();
        assert_eq!(back.messages.len(), 2);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_message_put_before_a_saved_prompt_or_a_prompt_taken_back_reaches_the_file() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("before-last");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(3));
        store.save().unwrap();
        store.conversations[0].insert_before_last(said("user", "notice"));
        store.save().unwrap();
        let file = dir.join("sessions/1.jsonl");
        let back = replay(&file).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "notice", "2"]);

        let taken = store.conversations[0].retract_last();
        assert_eq!(taken.map(|m| m.content).as_deref(), Some("2"));
        store.save().unwrap();
        let back = replay(&file).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "notice"]);
        assert!(!std::fs::read_to_string(&file).unwrap().contains("\"2\""));
        // Before a save there is nothing on disk to write again.
        store.conversations[0].messages.push(said("user", "3"));
        store.conversations[0].insert_before_last(said("user", "next"));
        store.save().unwrap();
        let back = replay(&file).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "notice", "next", "3"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn what_is_said_after_a_rewind_follows_it() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-then-say");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        store.conversations[0].rewind(2);
        store.conversations[0].messages.push(said("user", "again"));
        store.save().unwrap();

        let back = replay(&dir.join("sessions/1.jsonl")).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "again"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_listing_counts_what_is_left() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-listing");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        store.conversations[0].rewind(2);
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 2);

        // Counting carries on from the rewind, not from the end of the file.
        store.conversations[0].messages.push(said("user", "again"));
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 3);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn search_skips_what_was_rewound() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-search");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            messages: vec![
                said("user", "one fig"),
                said("user", "two fig"),
                said("user", "three fig"),
                said("user", "four fig"),
            ],
            ..Default::default()
        });
        store.save().unwrap();
        store.conversations[0].rewind(1);
        store.conversations[0]
            .messages
            .push(said("user", "five fig"));
        store.save().unwrap();

        let sessions = dir.join("sessions");
        assert!(search_in(&sessions, "three", &Within::default(), 10).is_empty());
        // More hits than a session may show were taken back, which must not
        // hide the one said after them.
        let figs: Vec<String> = search_in(&sessions, "fig", &Within::default(), 10)
            .into_iter()
            .map(|hit| hit.snippet)
            .collect();
        assert_eq!(figs, ["one fig", "five fig"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    /// Each hit of a search of `dir` for `query` as its session, place, role
    /// and snippet, with the term sets passing sessions over or without.
    fn found_by(
        dir: &std::path::Path,
        query: &str,
        prefilter: bool,
    ) -> Vec<(u64, Option<usize>, String, String)> {
        search_through(dir, query, &Within::default(), 50, prefilter)
            .into_iter()
            .map(|h| (h.id, h.ordinal, h.role, h.snippet))
            .collect()
    }

    fn same_either_way(dir: &std::path::Path, queries: &[&str]) {
        for query in queries {
            let with = found_by(dir, query, true);
            assert_eq!(with, found_by(dir, query, false), "{query}");
        }
    }

    /// The size and time of the session file at `path`, which its term set
    /// must have been made at to be used.
    fn stamp(path: &std::path::Path) -> (u64, u64) {
        let stat = std::fs::metadata(path).unwrap();
        (stat.len(), modified(&stat))
    }

    #[test]
    fn search_finds_the_same_with_term_sets_as_without() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("prefilter-same");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut asked = said("assistant", "PostgreSQL, for the concurrency.");
        asked.tool_calls.push(ToolCall {
            id: "1".into(),
            name: "run_command".into(),
            arguments: r#"{"command":"psql -c 'select 1'"}"#.into(),
        });
        let conversations = [
            (
                "Database choice",
                vec![said("user", "Postgres or SQLite?"), asked],
            ),
            (
                "迁移",
                vec![
                    said("user", "数据库迁移完成了"),
                    said("assistant", "c++ and #12 -> done"),
                ],
            ),
            (
                "Quotes",
                vec![said(
                    "user",
                    "she said \"ship it\"\tthen left for Ωmega ΣΑΣ",
                )],
            ),
            (
                "Cache",
                (0..40)
                    .map(|i| said("user", &format!("cache {i}")))
                    .collect(),
            ),
        ];
        let mut store = Store::default();
        for (id, (title, messages)) in (1..).zip(conversations) {
            store.conversations.push(Conversation {
                id,
                title: title.into(),
                updated: 100 + id,
                messages,
                ..Default::default()
            });
        }
        store.save().unwrap();
        let sessions = dir.join("sessions");
        // Saved before sets were kept.
        let old = [user("postgres from long ago"), user("a cache of figs")];
        session_file(&sessions, 9, "Old notes", 50, &old);
        for id in 1..=4 {
            let path = sessions.join(format!("{id}.jsonl"));
            assert!(
                crate::prefilter::read(&path, stamp(&path)).is_some(),
                "{id}"
            );
        }

        same_either_way(
            &sessions,
            &[
                "postgres",
                "POSTGRES sqlite",
                "sql",
                "concurrency flamingo",
                "数据库",
                "迁移",
                "c++",
                "#12",
                "->",
                "\"ship it\"",
                "it\"",
                "psql",
                "'select",
                "database",
                "cache 3",
                "cache",
                "ωmega σας",
                "ςας",
                "zz",
                "flamingo",
                "the",
            ],
        );
        assert_eq!(found_by(&sessions, "postgres", true).len(), 3);
        assert_eq!(found_by(&sessions, "数据库", true).len(), 1);
        assert_eq!(found_by(&sessions, "\"ship it\"", true).len(), 1);
        let old = sessions.join("9.jsonl");
        assert!(crate::prefilter::read(&old, stamp(&old)).is_some());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_saved_before_term_sets_gets_one_when_first_searched() {
        let dir = scratch("prefilter-lazy");
        session_file(&dir, 1, "Old", 100, &[user("the fig tree")]);
        let path = dir.join("1.jsonl");
        assert!(!crate::prefilter::path_for(&path).exists());

        assert_eq!(found_by(&dir, "fig", true).len(), 1);
        assert!(crate::prefilter::read(&path, stamp(&path)).is_some());
        assert!(found_by(&dir, "flamingo", true).is_empty());
        // A set made from the file as it is now is taken at its word: one
        // that says it holds nothing has the file passed over unread.
        let nothing = crate::prefilter::Filter::of("");
        crate::prefilter::write(&path, stamp(&path), &nothing);
        assert!(found_by(&dir, "fig", true).is_empty());
        // Once the file moves, the set no longer fits and is made again.
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str("{\"type\":\"message\",\"role\":\"user\",\"content\":\"more figs\"}\n");
        std::fs::write(&path, text).unwrap();
        assert_eq!(found_by(&dir, "fig", true).len(), 2);
        let made = crate::prefilter::read(&path, stamp(&path)).unwrap();
        assert!(made.may_hold("more figs"));
    }

    #[test]
    fn term_sets_keep_up_with_edits_rewinds_and_replaced_files() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("prefilter-edits");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let sessions = dir.join("sessions");
        let path = sessions.join("1.jsonl");
        let queries = ["fig", "three", "orchard", "kiwi", "mango", "first"];

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            title: "first".into(),
            messages: vec![
                said("user", "one fig"),
                said("user", "two fig"),
                said("user", "three fig"),
            ],
            ..Default::default()
        });
        store.save().unwrap();
        same_either_way(&sessions, &queries);

        store.conversations[0].title = "Orchard notes".into();
        store.save().unwrap();
        assert!(crate::prefilter::read(&path, stamp(&path)).is_some());
        assert_eq!(found_by(&sessions, "orchard", true).len(), 1);
        same_either_way(&sessions, &queries);

        store.conversations[0].rewind(1);
        store.conversations[0]
            .messages
            .push(said("user", "five kiwi"));
        store.save().unwrap();
        assert!(crate::prefilter::read(&path, stamp(&path)).is_some());
        assert!(found_by(&sessions, "three", true).is_empty());
        assert_eq!(found_by(&sessions, "kiwi", true).len(), 1);
        same_either_way(&sessions, &queries);

        // Written over by something other than a save, its set left behind.
        session_file(&sessions, 1, "Replaced", 500, &[user("mango only")]);
        assert_eq!(found_by(&sessions, "mango", true).len(), 1);
        assert!(found_by(&sessions, "fig", true).is_empty());
        same_either_way(&sessions, &queries);

        store.remove(0);
        assert!(!crate::prefilter::path_for(&path).exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_run_writing_into_the_session_file_keeps_its_term_set() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("prefilter-run");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("sessions").join("1.jsonl");
        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            title: "Run".into(),
            messages: vec![said("user", "plant a fig")],
            ..Default::default()
        });
        store.save().unwrap();

        // As the terminal's one-shot run writes on into the session.
        let mut run = Transcript {
            path: path.clone(),
            conversation: store.conversations.remove(0),
        };
        run.conversation.messages.push(said("assistant", "planted"));
        run.save().unwrap();
        let set = crate::prefilter::read(&path, stamp(&path)).unwrap();
        assert!(set.may_hold("planted"));
        // A helper's conversation is never searched, and gets no set.
        let (_, mut helper) = Transcript::create(1, "helper", None).unwrap();
        helper.conversation.messages.push(said("user", "hello"));
        helper.save().unwrap();
        assert!(!crate::prefilter::path_for(&helper.path).exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_listed_session_rewound_to_the_start_stays_empty() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-start");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();

        // The terminal reads a listed session when it is opened, so opening
        // it again must not bring back what was just taken out of it.
        let mut listed = load_listing();
        fill(&mut listed.conversations[0]);
        listed.conversations[0].rewind(0);
        fill(&mut listed.conversations[0]);
        assert_eq!(listed.conversations[0].length(), 0);
        listed.save().unwrap();
        let back = replay(&dir.join("sessions/1.jsonl")).unwrap();
        assert!(back.messages.is_empty());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_leftover_lock_file_does_not_list_a_session_twice() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("leftover-lock");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();
        // What a process that died part way through a save leaves behind.
        std::fs::write(dir.join("sessions/1.lock"), "").unwrap();

        assert_eq!(summaries().len(), 1);
        assert_eq!(super::load().conversations.len(), 1);
    }

    #[test]
    fn a_setting_changed_in_one_process_survives_the_other_saving() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-merge");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("settings.json");
        // Laid out by hand, not the way a save lays it out.
        let by_hand = "{\"endpoint\":\"\",\"model\":\"a/one\"}";
        std::fs::write(&path, by_hand).unwrap();

        // The window and the terminal, both started from that file.
        let mut window = load_listing();
        let mut terminal = load_listing();
        // Saving with no setting changed leaves the file as it was.
        window.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), by_hand);

        terminal.settings.model = "b/two".into();
        terminal.save().unwrap();
        window.settings.effort = "high".into();
        window.save().unwrap();
        let back = load_listing().settings;
        assert_eq!(
            (back.model.as_str(), back.effort.as_str()),
            ("b/two", "high")
        );
    }

    #[test]
    fn a_todo_call_sets_the_whole_checklist_and_says_how_far_along_it_is() {
        let mut todos = vec![Todo {
            text: "an old plan".into(),
            status: "doing".into(),
        }];
        let said = apply_todo(
            &mut todos,
            r#"{"items":[{"text":"read","status":"done"},{"text":"write"},{"text":"","status":"doing"}]}"#,
        );
        assert_eq!(said.unwrap(), "Checklist updated: 1 of 2 done.");
        // A missing status is still to do, and an item with no words is dropped.
        let items: Vec<_> = todos
            .iter()
            .map(|t| (t.text.as_str(), t.status.as_str()))
            .collect();
        assert_eq!(items, [("read", "done"), ("write", "pending")]);
    }

    #[test]
    fn a_todo_call_that_cannot_be_read_leaves_the_checklist_as_it_was() {
        let plan = vec![Todo {
            text: "step".into(),
            status: "doing".into(),
        }];
        let mut todos = plan.clone();
        assert_eq!(
            apply_todo(&mut todos, r#"{"text":"step"}"#).unwrap_err(),
            "`items` is required."
        );
        assert!(apply_todo(&mut todos, r#"{"items":[{"text":"#).is_err());
        assert!(todos == plan);
    }

    #[test]
    fn the_checklist_is_the_last_one_left_standing() {
        let todo = |items: &str| Message {
            role: "assistant".into(),
            tool_calls: vec![ToolCall {
                id: "call".into(),
                name: crate::tools::TODO.into(),
                arguments: format!(r#"{{"items":[{items}]}}"#),
            }],
            ..Default::default()
        };
        let first = todo(r#"{"text":"plan","status":"done"}"#);
        let second = todo(r#"{"text":"build","status":"doing"}"#);
        assert_eq!(todos_in(&[first.clone(), second.clone()])[0].text, "build");
        assert_eq!(todos_in(&[first])[0].text, "plan");
        assert!(todos_in(&[]).is_empty());
        // A call that could not be read changed nothing, so the one before stands.
        let mut garbled = second.clone();
        garbled.tool_calls[0].arguments = r#"{"items": [{"text": "lost"#.into();
        assert_eq!(todos_in(&[second, garbled])[0].text, "build");
    }

    #[test]
    fn a_settings_file_that_will_not_parse_is_kept_beside_the_new_one() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-corrupt");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let torn = "{\"endpoint\":\"\",\"model\":\"a/one\",\"gatew";
        std::fs::write(dir.join("settings.json"), torn).unwrap();

        let mut store = load_listing();
        assert!(store.settings.model.is_empty());
        store.settings.model = "b/two".into();
        store.save().unwrap();

        let aside: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|p| p.to_string_lossy().contains("settings.json.corrupt-"))
            .collect();
        assert_eq!(aside.len(), 1);
        assert_eq!(std::fs::read_to_string(&aside[0]).unwrap(), torn);
        assert_eq!(load_listing().settings.model, "b/two");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_model_is_saved_with_the_gateway_it_was_chosen_on() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-pair");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::write(
            dir.join("settings.json"),
            "{\"endpoint\":\"https://api.openai.com/v1\",\"model\":\"gpt-a\",\"gateway\":\"OpenAI\"}",
        )
        .unwrap();

        let mut window = load_listing();
        let mut terminal = load_listing();
        terminal.settings.gateway = "OpenRouter".into();
        terminal.settings.model = "x/y".into();
        terminal.save().unwrap();
        // The window picks another model on the gateway it was already on.
        window.settings.model = "gpt-b".into();
        window.save().unwrap();
        let back = load_listing().settings;
        assert_eq!(
            (back.gateway.as_str(), back.model.as_str()),
            ("OpenAI", "gpt-b")
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_new_endpoint_or_custom_url_alone_leaves_the_other_process_model_be() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-endpoint");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        // The terminal's /model leaves the endpoint as it was, here empty.
        std::fs::write(
            dir.join("settings.json"),
            "{\"endpoint\":\"\",\"model\":\"gpt-a\",\"gateway\":\"OpenAI\"}",
        )
        .unwrap();

        let mut window = load_listing();
        let mut terminal = load_listing();
        terminal.settings.gateway = "OpenRouter".into();
        terminal.settings.model = "x/y".into();
        terminal.save().unwrap();
        // The window edits the Custom URL, then sends, which sets the
        // endpoint of the gateway it is still on.
        window.settings.custom_endpoint = "http://localhost:8000/v1".into();
        window.save().unwrap();
        window.settings.endpoint = "https://api.openai.com/v1".into();
        window.save().unwrap();
        let back = load_listing().settings;
        assert_eq!(
            (
                back.gateway.as_str(),
                back.model.as_str(),
                back.custom_endpoint.as_str()
            ),
            ("OpenRouter", "x/y", "http://localhost:8000/v1")
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_custom_model_is_saved_with_the_custom_url_it_was_listed_from() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-custom");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::write(
            dir.join("settings.json"),
            "{\"endpoint\":\"\",\"model\":\"gpt-a\",\"gateway\":\"OpenAI\",\"custom_endpoint\":\"http://one/v1\"}",
        )
        .unwrap();

        let mut window = load_listing();
        let mut terminal = load_listing();
        terminal.settings.custom_endpoint = "http://two/v1".into();
        terminal.save().unwrap();
        // The window picks a model from the list it loaded from the old URL.
        window.settings.gateway = "Custom".into();
        window.settings.model = "local/m".into();
        window.save().unwrap();
        let back = load_listing().settings;
        assert_eq!(
            (back.model.as_str(), back.custom_endpoint.as_str()),
            ("local/m", "http://one/v1")
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_custom_url_saved_elsewhere_is_followed_unless_one_is_waiting_here() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("settings-follow-custom");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::write(
            dir.join("settings.json"),
            "{\"gateway\":\"OpenAI\",\"model\":\"gpt-a\",\"custom_endpoint\":\"http://one/v1\"}",
        )
        .unwrap();

        let mut window = load_listing();
        let mut terminal = load_listing();
        assert!(!window.follow_custom());
        terminal.settings.custom_endpoint = "http://two/v1".into();
        terminal.save().unwrap();
        assert!(window.follow_custom());
        assert_eq!(window.settings.custom_endpoint, "http://two/v1");
        // Taken in, it is not written back as a change of the window's.
        window.save().unwrap();
        assert!(!window.follow_custom());
        // An address typed here and not saved yet stays.
        terminal.settings.custom_endpoint = "http://three/v1".into();
        terminal.save().unwrap();
        window.settings.custom_endpoint = "http://mine/v1".into();
        assert!(!window.follow_custom());
        assert_eq!(window.settings.custom_endpoint, "http://mine/v1");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_key_saved_for_one_slot_leaves_every_other_slot_as_the_file_has_it() {
        use std::collections::HashMap;
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("keys-slot");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("keys.json");
        // Saved by the terminal after the window last read the file.
        std::fs::write(&path, "{\"OpenAI\":\"sk-1\",\"Custom\":\"c-2\"}").unwrap();

        let saved = super::save_key("OpenRouter", Some("or-3")).unwrap();
        let all = HashMap::from([
            ("OpenAI".to_owned(), "sk-1".to_owned()),
            ("Custom".to_owned(), "c-2".to_owned()),
            ("OpenRouter".to_owned(), "or-3".to_owned()),
        ]);
        assert_eq!(saved, all);
        assert_eq!(super::load_keys(), all);

        // Taking a key out takes out that slot and nothing else.
        let saved = super::save_key("Custom", None).unwrap();
        assert_eq!(saved.len(), 2);
        assert!(!saved.contains_key("Custom"));
        assert_eq!(super::load_keys(), saved);

        // With no file, taking a key out writes none.
        std::fs::remove_file(&path).unwrap();
        assert!(super::save_key("OpenAI", None).unwrap().is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn a_session_deleted_elsewhere_is_not_written_back() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("deleted-elsewhere");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut terminal = Store::default();
        terminal.conversations.push(numbered(2));
        terminal.save().unwrap();
        let path = dir.join("sessions/1.jsonl");

        // The window lists the same session and deletes it.
        let mut window = load_listing();
        window.remove(0);
        assert!(!path.exists());

        // The terminal still has it open, and more is said and renamed there.
        terminal.conversations[0]
            .messages
            .push(said("user", "still here?"));
        terminal.conversations[0].title = "renamed".into();
        terminal.save().unwrap();
        assert!(!path.exists());

        // The window starts a new session, which is handed the freed id.
        let id = window.next_id();
        assert_eq!(id, 1);
        window.conversations.push(Conversation {
            id,
            title: "new".into(),
            messages: vec![said("user", "fresh")],
            ..Default::default()
        });
        window.save().unwrap();
        let new = std::fs::read_to_string(&path).unwrap();

        // The terminal says more again; none of it lands in the new session.
        terminal.conversations[0]
            .messages
            .push(said("user", "anyone?"));
        terminal.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), new);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_deleted_session_is_not_followed_back_into_the_list() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("deleted-followed");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut terminal = Store::default();
        terminal.conversations.push(numbered(2));
        terminal.conversations.push(Conversation {
            id: 2,
            messages: vec![said("user", "kept")],
            ..Default::default()
        });
        terminal.save().unwrap();

        // The window deletes session 1 and saves, as a delete does.
        let mut window = load_listing();
        let at = window.index_of(1).unwrap();
        window.remove(at);
        window.save().unwrap();

        // The terminal, which still has it, says more and is renamed there:
        // what the "the sessions changed" watch would then take in.
        terminal.conversations[0]
            .messages
            .push(said("user", "still here?"));
        terminal.conversations[0].title = "renamed".into();
        terminal.save().unwrap();
        window.follow(|_| false);
        let listed: Vec<u64> = window.conversations.iter().map(|c| c.id).collect();
        assert_eq!(listed, [2]);
        assert!(summaries().iter().all(|s| s.id != 1));
        // Nor does a listing from scratch, as the next start makes.
        let again: Vec<u64> = (load_listing().conversations.iter())
            .map(|c| c.id)
            .collect();
        assert_eq!(again, [2]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn appearance_round_trips() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("appearance");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        for (appearance, grain) in [("paper", false), ("system", true), ("", false)] {
            let mut store = Store::default();
            store.settings.appearance = appearance.into();
            store.settings.grain = grain;
            store.save().unwrap();
            let back = load().settings;
            assert_eq!((back.appearance.as_str(), back.grain), (appearance, grain));
        }
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_saved_style_comes_back_and_an_unknown_one_reads_as_none() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("style");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let old = super::read_settings(br#"{"endpoint": "", "model": "m"}"#).unwrap();
        assert_eq!(old.style, "");
        // A name no style has: one that did would change the prompt of the
        // tests running beside this one, which read the same settings file.
        let mut store = Store::default();
        store.settings.style = "gone".into();
        store.save().unwrap();
        assert_eq!(load().settings.style, "gone");
        assert_eq!(crate::styles::chosen(), None);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_style_picked_in_one_process_can_be_undone_in_the_other() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("style-two");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut window = load();
        let mut terminal = load();
        // A name no style has, as above.
        terminal.choose_style("gone");
        terminal.save().unwrap();
        assert_eq!(super::current_settings().style, "gone");
        // The window never saw that pick, so it already holds "" itself.
        window.choose_style("");
        window.save().unwrap();
        assert_eq!(super::current_settings().style, "");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn two_writers_of_one_session_never_interleave_their_lines() {
        use std::io::Write;
        let dir = scratch("lock-interleave");
        let path = dir.join("1.jsonl");
        let writer = |tag: &'static str| {
            let path = path.clone();
            std::thread::spawn(move || {
                for _ in 0..20 {
                    let _lock = Lock::take(&path).unwrap();
                    // One line in two writes with a pause between, as a slow
                    // append would be.
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .unwrap();
                    file.write_all(tag.as_bytes()).unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(2));
                    file.write_all(b" end\n").unwrap();
                }
            })
        };
        let (first, second) = (writer("first"), writer("second"));
        first.join().unwrap();
        second.join().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.lines().count(), 40);
        assert!(text.lines().all(|l| l == "first end" || l == "second end"));
    }

    #[test]
    fn a_save_that_cannot_take_the_lock_writes_nothing_and_the_next_catches_up() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("lock-timeout");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::read_to_string(&path).unwrap();

        store.conversations[0]
            .messages
            .push(said("assistant", "hi"));
        let held = Lock::take(&path).unwrap();
        assert!(store.save().is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        drop(held);
        store.save().unwrap();
        let back = replay(&path).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.messages[1].content, "hi");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_lock_whose_holder_is_gone_is_free_at_once() {
        let dir = scratch("lock-gone");
        let path = dir.join("1.jsonl");
        // What a process that dies holding the lock leaves behind: the file,
        // locked until the system closes it.
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(path.with_extension("lock"))
            .unwrap();
        holder.lock().unwrap();
        drop(holder);
        let started = std::time::Instant::now();
        assert!(Lock::take(&path).is_ok());
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn a_lock_file_is_never_removed_from_under_its_holder() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("lock-owner");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        for id in [1, 2] {
            store.conversations.push(Conversation {
                id,
                messages: vec![said("user", "hello")],
                ..Default::default()
            });
        }
        store.save().unwrap();
        // The lock files beside them are not sessions.
        assert_eq!(summaries().len(), 2);
        let (one, two) = (dir.join("sessions/1.jsonl"), dir.join("sessions/2.jsonl"));

        // Waiting in vain and letting go both leave the file where it is.
        let held = Lock::take(&one).unwrap();
        assert!(Lock::take(&one).is_err());
        assert!(one.with_extension("lock").exists());
        drop(held);
        assert!(one.with_extension("lock").exists());

        // Removing a session removes its lock file only if nobody holds it.
        let held = Lock::take(&two).unwrap();
        store.remove(1);
        assert!(!two.exists());
        assert!(two.with_extension("lock").exists());
        drop(held);
        store.remove(0);
        assert!(!one.exists());
        assert!(!one.with_extension("lock").exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn ids_reserved_at_the_same_time_differ() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("reserve");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        // Each thread is a Letronna starting up: its own empty store, the
        // same folder, all let go at once.
        let gate = std::sync::Arc::new(std::sync::Barrier::new(16));
        let racers: Vec<_> = (0..16)
            .map(|_| {
                let gate = gate.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    Store::default().reserve_id()
                })
            })
            .collect();
        let mut ids: Vec<u64> = racers.into_iter().map(|r| r.join().unwrap()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), 16);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_reserved_session_is_listed_once_saved_and_not_before() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("reserve-save");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        let id = store.reserve_id();
        assert!(summaries().is_empty());

        // The file is already there, and the first save still writes its head.
        store.conversations.push(Conversation {
            id,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 1);
        let back = replay(&dir.join(format!("sessions/{id}.jsonl"))).unwrap();
        assert_eq!(back.id, id);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_transcript_replays_like_a_session() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let (n, mut helper) = Transcript::create(7, "explore", Some("work".into())).unwrap();
        assert_eq!(
            helper.path,
            dir.join("agents").join("7").join(format!("{n}.jsonl"))
        );
        helper.conversation.messages = numbered(4).messages;
        helper.conversation.todos = vec![Todo {
            text: "look".into(),
            status: "doing".into(),
        }];
        helper.save().unwrap();
        helper.conversation.rewind(2);
        helper
            .conversation
            .messages
            .push(said("assistant", "again"));
        helper.save().unwrap();

        let mut back = Transcript::open(7, n).unwrap();
        assert_eq!(back.path, helper.path);
        assert_eq!(back.conversation.title, "explore");
        assert_eq!(back.conversation.workspace, Some("work".into()));
        assert_eq!(back.conversation.todos.len(), 1);
        let contents: Vec<&str> = back
            .conversation
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect();
        assert_eq!(contents, ["0", "1", "again"]);

        // Opened again, it appends what is new rather than all of it.
        let before = std::fs::read_to_string(&back.path).unwrap();
        back.save().unwrap();
        assert_eq!(std::fs::read_to_string(&back.path).unwrap(), before);
        back.conversation.messages.push(said("user", "more"));
        back.save().unwrap();
        let after = std::fs::read_to_string(&back.path).unwrap();
        assert!(after.starts_with(&before));
        assert_eq!(after.lines().count(), before.lines().count() + 1);
        assert!(Transcript::open(7, n + 1).is_none());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn listing_and_search_ignore_transcripts() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript-listing");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 1,
            title: "the session".into(),
            messages: vec![said("user", "a fig in the session")],
            ..Default::default()
        });
        store.save().unwrap();
        let (_, mut helper) = Transcript::create(1, "the helper", None).unwrap();
        helper
            .conversation
            .messages
            .push(said("user", "a fig in the helper"));
        helper.save().unwrap();

        let listed: Vec<u64> = summaries().iter().map(|s| s.id).collect();
        assert_eq!(listed, [1]);
        let loaded: Vec<String> = load().conversations.into_iter().map(|c| c.title).collect();
        assert_eq!(loaded, ["the session"]);
        assert_eq!(load_listing().conversations.len(), 1);
        let found: Vec<String> = search("fig", &Within::default(), 10)
            .into_iter()
            .map(|h| h.snippet)
            .collect();
        assert_eq!(found, ["a fig in the session"]);

        let ctx = crate::tools::ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let listed = crate::tools::run("list_sessions", "{}", &ctx).unwrap();
        assert!(
            listed.contains("the session") && !listed.contains("the helper"),
            "{listed}"
        );
        let found = crate::tools::run("search_sessions", r#"{"query":"fig"}"#, &ctx).unwrap();
        assert!(
            found.contains("in the session") && !found.contains("in the helper"),
            "{found}"
        );
        // Nor can the model open the file by its path: it is in the data
        // folder, even with that folder as the workspace.
        let args = serde_json::json!({ "path": helper.path }).to_string();
        let read = crate::tools::run("read_file", &args, &ctx);
        assert!(
            read.as_ref()
                .is_err_and(|e| e.contains("Letronna's own folder")),
            "{read:?}"
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn saved_keys_are_hidden_from_tool_results_and_cards() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let data = scratch("scrub-data");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        let keys = r#"{"OpenAI":"a-saved-key-of-no-known-shape","Custom":"sk-1"}"#;
        std::fs::write(data.join("keys.json"), keys).unwrap();
        let mcp =
            r#"{"mcpServers":{"db":{"command":"db","env":{"DB_TOKEN":"an-mcp-token-0123"}}}}"#;
        std::fs::write(data.join("mcp.json"), mcp).unwrap();
        let work = scratch("scrub-work");
        let notes = "saved a-saved-key-of-no-known-shape\nmcp an-mcp-token-0123\n\
                     shaped sk-proj-0123456789abcdef\nshort sk-1\n";
        std::fs::write(work.join("notes.txt"), notes).unwrap();
        let ctx = crate::tools::ToolContext {
            workspace: Some(work.to_path_buf()),
            ..Default::default()
        };
        let marker = crate::redact::MARKER;

        let read = crate::tools::run("read_file", r#"{"path":"notes.txt"}"#, &ctx).unwrap();
        assert!(read.contains(&format!("saved {marker}")), "{read}");
        assert!(read.contains(&format!("mcp {marker}")), "{read}");
        // A file's own key-shaped words are left for the model to edit.
        assert!(read.contains("shaped sk-proj-0123456789abcdef"), "{read}");
        assert!(read.contains("short sk-1"), "{read}");
        let ran = crate::tools::run("run_command", r#"{"command":"cat notes.txt"}"#, &ctx).unwrap();
        assert!(ran.contains(&format!("shaped {marker}")), "{ran}");
        assert!(
            !ran.contains("a-saved-key") && !ran.contains("an-mcp-token"),
            "{ran}"
        );
        let missing = r#"{"path":"a-saved-key-of-no-known-shape.txt"}"#;
        let error = crate::tools::run("read_file", missing, &ctx).unwrap_err();
        assert!(!error.contains("a-saved-key"), "{error}");
        let write = r#"{"path":"x.txt","content":"key a-saved-key-of-no-known-shape"}"#;
        let card = crate::tools::preview("write_file", write, &ctx);
        assert_eq!(card, format!("key {marker}"));
        // A file holding a hidden key and the marker's own text takes no
        // marker back, since which one it stands for cannot be told.
        let both = format!("{marker}\nkey a-saved-key-of-no-known-shape\n");
        let workspace = ctx.workspace.clone().unwrap();
        std::fs::write(workspace.join("both.txt"), &both).unwrap();
        let write = serde_json::json!({"path": "both.txt", "content": format!("key {marker}\n")});
        let refused = crate::tools::run("write_file", &write.to_string(), &ctx).unwrap_err();
        assert!(refused.starts_with("Refused"), "{refused}");
        assert_eq!(
            std::fs::read_to_string(workspace.join("both.txt")).unwrap(),
            both
        );
        // No other tool takes the marker at all.
        let fetch = serde_json::json!({ "url": format!("https://x.test/{marker}") }).to_string();
        let refused = crate::tools::run("web_fetch", &fetch, &ctx).unwrap_err();
        assert!(refused.contains("arguments of `web_fetch`"), "{refused}");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_key_cut_in_two_shows_none_of_itself() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let data = scratch("cut-data");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        let key = "QWERTYUIOPasdfghjklZXCVBNM0123456789qwerty";
        std::fs::write(data.join("keys.json"), format!(r#"{{"OpenAI":"{key}"}}"#)).unwrap();
        let work = scratch("cut-work");
        // Each puts the key across a cut: the end of what a command's output
        // is read to, the 1,000 characters a line shows, the bytes a line is
        // read to, the 300 a search shows around a match, and the oldest bytes
        // a job's log drops.
        std::fs::write(work.join("long.txt"), "a".repeat(8180) + key).unwrap();
        std::fs::write(work.join("wide.txt"), "b".repeat(980) + key + "\n").unwrap();
        std::fs::write(work.join("wider.txt"), "\u{1F600}".repeat(991) + key).unwrap();
        std::fs::write(work.join("job.txt"), key.to_owned() + &"c".repeat(63_990)).unwrap();
        let found = format!("needle{}{key}\n", "d".repeat(284));
        std::fs::write(work.join("found.txt"), found).unwrap();
        let ctx = crate::tools::ToolContext {
            workspace: Some(work.to_path_buf()),
            ..Default::default()
        };
        let shows_part = |text: &str| (0..=key.len() - 6).any(|at| text.contains(&key[at..at + 6]));

        let ran = crate::tools::run("run_command", r#"{"command":"cat long.txt"}"#, &ctx).unwrap();
        assert!(!shows_part(&ran), "{}", &ran[ran.len() - 80..]);
        for file in ["wide.txt", "wider.txt"] {
            let args = serde_json::json!({ "path": file }).to_string();
            let read = crate::tools::run("read_file", &args, &ctx).unwrap();
            assert!(read.contains("[line cut]") && !shows_part(&read), "{read}");
        }
        let search = r#"{"pattern":"needle","path":"found.txt"}"#;
        let search = crate::tools::run("search", search, &ctx).unwrap();
        assert!(
            search.contains("needle") && !shows_part(&search),
            "{search}"
        );
        let started = r#"{"command":"cat job.txt","background":true}"#;
        let started = crate::tools::run("run_command", started, &ctx).unwrap();
        assert!(!shows_part(&started), "{}", &started[..200]);
        let id: u64 = started["Started as job #".len()..]
            .split(' ')
            .next()
            .and_then(|id| id.parse().ok())
            .unwrap();
        let args = serde_json::json!({ "job": id, "seconds": 10 }).to_string();
        let waited = crate::tools::run("wait", &args, &ctx).unwrap();
        assert!(!shows_part(&waited), "{}", &waited[..200]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn removing_a_session_removes_its_helpers_and_its_task_list() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript-remove");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.conversations.push(Conversation {
            id: 2,
            ..numbered(1)
        });
        store.save().unwrap();
        for session in [1, 1, 2] {
            Transcript::create(session, "helper", None).unwrap();
        }
        let lists = dir.join("tasklists");
        std::fs::create_dir_all(&lists).unwrap();
        for list in ["1.jsonl", "1.lock", "2.jsonl"] {
            std::fs::write(lists.join(list), "").unwrap();
        }

        store.remove(0);
        assert!(!dir.join("sessions").join("1.jsonl").exists());
        assert!(!dir.join("agents").join("1").exists());
        assert!(!lists.join("1.jsonl").exists() && !lists.join("1.lock").exists());
        assert!(lists.join("2.jsonl").exists());
        assert!(Transcript::open(1, 1).is_none());
        // Another session's helpers are its own.
        assert!(Transcript::open(2, 1).is_some());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn two_transcripts_made_at_once_get_different_numbers() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript-race");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        // More than two, all let go together, to give a race every chance.
        let start = std::sync::Arc::new(std::sync::Barrier::new(8));
        let makers: Vec<_> = (0..8)
            .map(|_| {
                let start = start.clone();
                std::thread::spawn(move || {
                    start.wait();
                    Transcript::create(3, "helper", None).unwrap().0
                })
            })
            .collect();
        let mut numbers: Vec<u32> = makers.into_iter().map(|m| m.join().unwrap()).collect();
        numbers.sort();
        assert_eq!(numbers, (1..=8).collect::<Vec<u32>>());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn removing_a_session_removes_the_outputs_saved_for_it() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("remove-results");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();
        let (mine, other) = (crate::tools::results_dir(1), crate::tools::results_dir(2));
        for results in [&mine, &other] {
            std::fs::create_dir_all(results).unwrap();
            std::fs::write(results.join("1-0.txt"), "saved").unwrap();
        }
        store.remove(0);
        assert!(!mine.exists() && !dir.join("sessions/1.jsonl").exists());
        assert!(other.join("1-0.txt").is_file());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn removing_a_session_removes_the_copies_of_its_files() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("remove-history");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();
        let kept = crate::history::folder(1);
        std::fs::create_dir_all(&kept).unwrap();
        std::fs::write(kept.join("copy"), "old").unwrap();

        store.remove(0);
        assert!(!dir.join("sessions/1.jsonl").exists());
        assert!(!kept.exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_settings_file_without_appearance_loads_as_night_with_grain() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("appearance-old");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let file = dir.join("settings.json");
        // As a build from before the light theme wrote it.
        std::fs::write(
            &file,
            r#"{"endpoint":"https://api.letronna.ai/v1","model":"a/model","skip_approvals":true}"#,
        )
        .unwrap();

        let mut store = load();
        assert_eq!(store.settings.appearance, "");
        assert!(store.settings.grain);
        assert!(store.settings.motion);
        assert_eq!(store.settings.mode, crate::gate::Mode::Bypass);
        // Saved and read again, the grain is still on and nothing else moved.
        store.save().unwrap();
        let again = load().settings;
        assert_eq!(again.appearance, "");
        assert!(again.grain);
        assert_eq!(again.mode, crate::gate::Mode::Bypass);
        // A first run, with no settings file at all, starts the same way.
        std::fs::remove_file(&file).unwrap();
        let first = load().settings;
        assert_eq!((first.appearance.as_str(), first.grain), ("", true));
        assert!(first.motion);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_windows_text_size_starts_usual_and_is_kept_once_stepped() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("text-step");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        // As a build from before the text size wrote it.
        std::fs::write(dir.join("settings.json"), r#"{"model":"a/model"}"#).unwrap();
        let mut store = load();
        assert_eq!(store.settings.text_step, 0);
        store.settings.text_step = -2;
        store.save().unwrap();
        assert_eq!(load().settings.text_step, -2);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn previews_open_by_themselves_unless_turned_off() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("preview-opens");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let file = dir.join("settings.json");
        // As a build from before previews wrote it.
        std::fs::write(
            &file,
            r#"{"endpoint":"https://api.letronna.ai/v1","model":"a/model"}"#,
        )
        .unwrap();
        let mut store = load();
        assert!(store.settings.preview_opens_by_itself);
        // Turned off, it stays off once saved and read again.
        store.settings.preview_opens_by_itself = false;
        store.save().unwrap();
        assert!(!load().settings.preview_opens_by_itself);
        // A first run, with no settings file at all, opens them.
        std::fs::remove_file(&file).unwrap();
        assert!(load().settings.preview_opens_by_itself);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn edits_are_checked_unless_switched_off_or_run_command_is() {
        let read = |extra: &str| {
            let text = format!(r#"{{"endpoint": "", "model": "m"{extra}}}"#);
            super::read_settings(text.as_bytes()).unwrap()
        };
        // A file from before the setting starts it on, as a first run does.
        assert!(read("").verify_edits && super::Settings::default().verify_edits);
        assert!(read("").verifies_edits());
        assert!(!read(r#", "verify_edits": false"#).verifies_edits());
        assert!(!read(r#", "disabled_tools": ["run_command"]"#).verifies_edits());
    }

    #[test]
    fn letronnas_search_is_auto_unless_free_or_paid_is_named() {
        let read = |extra: &str| {
            let text = format!(r#"{{"endpoint": "", "model": "m"{extra}}}"#);
            super::read_settings(text.as_bytes()).unwrap()
        };
        assert_eq!(read("").letronna_search, "");
        assert_eq!(super::search_named(&read("").letronna_search), "auto");
        assert_eq!(
            super::search_named(&read(r#", "letronna_search": "free""#).letronna_search),
            "free"
        );
        assert_eq!(
            super::search_named(&read(r#", "letronna_search": " paid ""#).letronna_search),
            "paid"
        );
        assert_eq!(super::search_named("anything"), "auto");
    }

    #[test]
    fn keyless_search_is_on_unless_switched_off() {
        let read = |extra: &str| {
            let text = format!(r#"{{"endpoint": "", "model": "m"{extra}}}"#);
            super::read_settings(text.as_bytes()).unwrap()
        };
        // A file from before the setting starts it on, as a first run does.
        assert!(read("").keyless_search && super::Settings::default().keyless_search);
        assert!(!read(r#", "keyless_search": false"#).keyless_search);
    }

    #[test]
    fn a_settings_file_from_before_lets_this_computer_stand_in() {
        let read = |extra: &str| {
            let text = format!(r#"{{"endpoint": "", "model": "m"{extra}}}"#);
            super::read_settings(text.as_bytes()).unwrap()
        };
        assert!(read("").local_stand_in && super::Settings::default().local_stand_in);
        assert!(!read(r#", "local_stand_in": false"#).local_stand_in);
    }

    #[test]
    fn tool_calls_run_together_unless_the_file_says_otherwise() {
        let read = |extra: &str| {
            let text = format!(r#"{{"endpoint": "", "model": "m"{extra}}}"#);
            super::read_settings(text.as_bytes()).unwrap()
        };
        // A file from before the setting runs them together, as a first run does.
        assert!(read("").parallel_tool_calls);
        assert!(super::Settings::default().parallel_tool_calls);
        assert!(!read(r#", "parallel_tool_calls": false"#).parallel_tool_calls);
    }

    #[test]
    fn long_turns_are_reminded_of_the_rules_only_when_the_file_says_so() {
        let read = |extra: &str| {
            let text = format!(r#"{{"endpoint": "", "model": "m"{extra}}}"#);
            super::read_settings(text.as_bytes()).unwrap()
        };
        // A file from before the setting leaves it off, as a first run does.
        assert!(!read("").remind_rules);
        assert!(!super::Settings::default().remind_rules);
        assert!(read(r#", "remind_rules": true"#).remind_rules);
    }

    #[test]
    fn disabled_tools_written_by_hand_in_another_shape_never_cost_the_file() {
        let read = |disabled: &str| {
            let text = format!(r#"{{"endpoint": "", "model": "m", "disabled_tools": {disabled}}}"#);
            let settings = super::read_settings(text.as_bytes()).unwrap();
            assert_eq!(settings.model, "m");
            settings.disabled_tools
        };
        assert_eq!(
            read(r#"["speak", 3, "make_video"]"#),
            ["speak", "make_video"]
        );
        assert_eq!(read(r#""speak""#), ["speak"]);
        assert!(read("null").is_empty() && read(r#"{"speak": true}"#).is_empty());
    }

    #[test]
    fn a_model_chosen_in_one_session_leaves_the_others_on_theirs() {
        let mut store = Store {
            settings: super::Settings {
                gateway: "Letronna".into(),
                model: "a/first".into(),
                effort: "low".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        store.conversations.push(numbered(1));
        store.conversations.push(numbered(1));
        // The first has sent, so it is held to what it sent on.
        store.pin(0);
        store.choose_model(1, "OpenRouter", "b/second");
        store.choose_effort(1, "high");
        assert_eq!(store.model(0), ("Letronna", "a/first"));
        assert_eq!(store.effort(0), "low");
        assert_eq!(store.model(1), ("OpenRouter", "b/second"));
        assert_eq!(store.effort(1), "high");
        // A new session, like one that never chose, starts from the latest.
        store.conversations.push(Conversation::default());
        assert_eq!(store.model(2), ("OpenRouter", "b/second"));
        assert_eq!(store.effort(2), "high");
        // A gateway no longer offered falls back to the settings.
        store.conversations[0].gateway = Some("Retired".into());
        assert_eq!(store.model(0), ("OpenRouter", "b/second"));
        // As does a session that is not there.
        assert_eq!(store.model(9), ("OpenRouter", "b/second"));
    }

    #[test]
    fn a_session_s_model_is_read_back_and_older_sessions_follow_the_settings() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("session-model");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let sessions = dir.join("sessions");
        std::fs::create_dir_all(&sessions).unwrap();
        // Written before sessions had a model of their own.
        std::fs::write(
            sessions.join("2.jsonl"),
            "{\"type\":\"head\",\"id\":2,\"created\":5}\n\
             {\"type\":\"meta\",\"title\":\"old\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}\n\
             {\"type\":\"message\",\"role\":\"user\",\"content\":\"hello\"}\n",
        )
        .unwrap();

        let mut store = Store::default();
        store.settings.model = "a/first".into();
        store.conversations.push(numbered(1));
        store.choose_model(0, "OpenAI", "b/second");
        store.choose_effort(0, "max");
        store.save().unwrap();

        for mut back in [load(), load_listing()] {
            back.settings.model = "c/third".into();
            back.settings.gateway = "Ollama".into();
            let new = back.index_of(1).unwrap();
            let old = back.index_of(2).unwrap();
            assert_eq!(back.model(new), ("OpenAI", "b/second"));
            assert_eq!(back.effort(new), "max");
            assert_eq!(back.model(old), ("Ollama", "c/third"));
            // Listing read the model too, so saving writes no meta line over it.
            let before = std::fs::read_to_string(sessions.join("1.jsonl")).unwrap();
            back.save().unwrap();
            let after = std::fs::read_to_string(sessions.join("1.jsonl")).unwrap();
            assert_eq!(before, after);
        }
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_sessions_memory_choice_is_read_back_and_the_others_follow_the_settings() {
        use crate::memory::Saving;
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("session-memory");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        // Settings from before the choice save memory only when asked.
        let old = super::read_settings(br#"{"endpoint": "", "model": "m"}"#).unwrap();
        assert_eq!(old.memory_saving, Saving::WhenAsked);

        // Ids no other test's session has, as what a session chose is kept
        // for the whole process.
        let (chose, follows) = (9_300_001, 9_300_002);
        let mut store = Store::default();
        for id in [chose, follows] {
            store.conversations.push(Conversation { id, ..numbered(1) });
        }
        store.choose_memory_saving(0, Some(Saving::Ask));
        assert_eq!(crate::memory::saving(chose), Saving::Ask);
        store.settings.memory_saving = Saving::Auto;
        store.save().unwrap();
        let file = dir.join("sessions").join(format!("{follows}.jsonl"));
        let line = std::fs::read_to_string(file).unwrap();
        assert!(!line.contains("memory_saving"), "{line}");

        for back in [load(), load_listing()] {
            let (chose, follows) = (
                back.index_of(chose).unwrap(),
                back.index_of(follows).unwrap(),
            );
            assert_eq!(back.memory_saving(chose), Saving::Ask);
            assert_eq!(back.memory_saving(follows), Saving::Auto);
        }
        // "Use my default" lets it follow the settings again.
        store.choose_memory_saving(0, None);
        assert_eq!(store.memory_saving(0), Saving::Auto);
        store.remove(0);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_private_chat_keeps_to_its_own_notes_which_go_with_it() {
        use crate::memory::{Saving, Scope};
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("private-chat");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let id = 9_300_011;
        let mut store = Store::default();
        store.settings.memory_saving = Saving::Auto;
        store.conversations.push(Conversation { id, ..numbered(1) });
        assert_eq!(store.memory_scope(0), Scope::Auto);
        store.make_private(0);
        assert_eq!(store.memory_scope(0), Scope::Chat);
        assert_eq!(store.memory_saving(0), Saving::WhenAsked);
        assert_eq!(crate::memory::scope(id), Scope::Chat);
        store.save().unwrap();
        let back = load();
        let at = back.index_of(id).unwrap();
        assert_eq!(back.memory_scope(at), Scope::Chat);
        assert_eq!(back.memory_saving(at), Saving::WhenAsked);

        let notes = crate::memory::chat_folder(id);
        std::fs::create_dir_all(&notes).unwrap();
        std::fs::write(notes.join("MEMORY.md"), "Planning a trip.\n").unwrap();
        store.remove(0);
        assert!(!notes.exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    /// Session 1, holding `n` messages that each say `tag` and their place,
    /// so a summary of them is remembered apart from any other test's.
    fn tagged(n: usize, tag: &str) -> Conversation {
        Conversation {
            id: 1,
            messages: (0..n)
                .map(|i| said("user", &format!("{tag} {i}")))
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_summary_is_appended_once_and_read_back_with_what_it_covers() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("summary");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(tagged(6, "round trip"));
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::read_to_string(&path).unwrap();

        // One of more messages than the session has is not for this history.
        store.conversations[0].summarized(7, "too far".into());
        assert_eq!(store.conversations[0].summary, None);
        store.conversations[0].summarized(4, "The first four.".into());
        store.save().unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            after,
            format!("{before}{{\"type\":\"summary\",\"upto\":4,\"text\":\"The first four.\"}}\n")
        );
        // Written once, and the same summary handed over again is no news.
        store.conversations[0].summarized(4, "The first four.".into());
        store.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), after);

        let back = replay(&path).unwrap();
        assert_eq!(back.summary, Some((4, "The first four.".into())));
        assert_eq!(back.messages.len(), 6);
        // The listing counts messages, not the summary line.
        assert_eq!(summaries()[0].messages, 6);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    /// The user's summary of the first `upto` of `c`'s messages as they are.
    fn asked_for(c: &Conversation, upto: usize, text: &str) -> crate::chat::Compacted {
        crate::chat::Compacted {
            upto,
            text: text.into(),
            before: 0,
            after: 0,
            key: (c.messages.get(..upto)).map_or(0, |m| crate::compact::keys(m)[upto]),
            model: "m".into(),
            usage: Some(crate::usage::Usage {
                input: 900,
                output: 60,
                ..Default::default()
            }),
        }
    }

    #[test]
    fn a_summary_the_user_asked_for_keeps_its_floor_across_restarts_until_a_rewind() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("manual summary");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut store = Store::default();
        store.conversations.push(tagged(8, "asked for"));
        let path = dir.join("sessions/1.jsonl");
        let state = |c: &Conversation| (c.floor(), c.summary.clone());
        let ask = |c: &mut Conversation, upto, text| c.compacted(asked_for(c, upto, text));

        // None of nothing, nor of more than the session has.
        assert!(ask(&mut store.conversations[0], 0, "nothing").is_err());
        assert!(ask(&mut store.conversations[0], 9, "too far").is_err());
        assert_eq!(state(&store.conversations[0]), (0, None));
        ask(&mut store.conversations[0], 4, "The first four.").unwrap();
        store.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        let line = r#"{"type":"summary","upto":4,"text":"The first four.","manual":true,"key":"#;
        assert!(text.lines().last().unwrap().starts_with(line), "{text}");
        let four = Some((4, "The first four.".to_owned()));
        assert_eq!(state(&replay(&path).unwrap()), (4, four.clone()));

        // One the window makes later goes on from it, and the floor stays.
        store.conversations[0].summarized(6, "The first six.".into());
        store.save().unwrap();
        let six = Some((6, "The first six.".to_owned()));
        assert_eq!(state(&replay(&path).unwrap()), (4, six));

        // A rewind into a later one of theirs goes back to this one.
        ask(&mut store.conversations[0], 7, "The first seven.").unwrap();
        store.save().unwrap();
        assert_eq!(replay(&path).unwrap().floor(), 7);
        store.conversations[0].rewind(5);
        assert_eq!(state(&store.conversations[0]), (4, None));
        store.save().unwrap();
        assert_eq!(state(&replay(&path).unwrap()), (4, four));

        // One into it drops it, in memory and in the file alike.
        store.conversations[0].rewind(3);
        assert_eq!(state(&store.conversations[0]), (0, None));
        store.save().unwrap();
        assert_eq!(state(&replay(&path).unwrap()), (0, None));

        // Both calls are counted, once each, though no reply carries them.
        let mut seen = Default::default();
        let calls = super::spent_in(&path, 1, &mut seen);
        assert_eq!(calls.len(), 2, "{text}");
        assert!(
            calls
                .iter()
                .all(|c| (c.usage.input, c.model.as_str()) == (900, "m"))
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_summary_of_messages_the_session_no_longer_starts_with_is_not_kept() {
        let mut c = tagged(6, "landing");
        let done = asked_for(&c, 4, "Old four.");
        // Deleted with its id reused, say, or rewound and said again.
        c.messages[2].content = "something else".into();
        let kept = c.compacted(done);
        assert!(kept.is_err_and(|e| e.contains("changed")));
        assert_eq!((c.floor(), c.summary.clone()), (0, None));
        assert!(c.call.is_none());
    }

    #[test]
    fn a_summary_made_from_an_out_of_date_view_is_not_used_on_replay() {
        let dir = scratch("stale floor");
        let path = dir.join("1.jsonl");
        let mut c = tagged(6, "stale");
        c.compacted(asked_for(&c, 4, "Four.")).unwrap();
        c.flush_to(&path).unwrap();
        // Another Letronna's, over messages the file does not have.
        let wrong = r#"{"type":"summary","upto":5,"text":"Wrong.","manual":true,"key":1}"#;
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, format!("{text}{wrong}\n")).unwrap();
        let back = replay(&path).unwrap();
        assert_eq!((back.floor(), back.summary), (4, Some((4, "Four.".into()))));
    }

    #[test]
    fn a_copy_starts_where_the_users_summary_ends_and_keeps_it_in_its_file() {
        let dir = scratch("summary-fork-floor");
        let mut source = tagged(8, "fork floor");
        source.compacted(asked_for(&source, 4, "Four.")).unwrap();
        source.summarized(6, "Six.".into());
        let mut copy = Conversation {
            id: 2,
            messages: source.messages[..7].to_vec(),
            ..Default::default()
        };
        copy.copy_summaries(&source);
        assert_eq!(
            (copy.floor(), copy.summary.clone()),
            (4, Some((4, "Four.".into())))
        );
        let path = dir.join("2.jsonl");
        copy.flush_to(&path).unwrap();
        let back = replay(&path).unwrap();
        assert_eq!((back.floor(), back.summary), (4, Some((4, "Four.".into()))));
        // Its file holds no call: the source's counts it.
        assert!(super::spent_in(&path, 2, &mut Default::default()).is_empty());
        // A copy from before the user's summary has none of it.
        copy.messages.truncate(3);
        copy.copy_summaries(&source);
        assert_eq!((copy.floor(), copy.summary), (0, None));
    }

    #[test]
    fn a_followed_session_keeps_the_users_summary_and_its_floor() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-asked");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(6);
        let state = |store: &Store| {
            let open = &store.conversations[0];
            (open.floor(), open.summary.clone())
        };
        let four = Some((4, "The first four.".to_owned()));

        let asked = asked_for(&terminal.conversations[0], 4, "The first four.");
        terminal.conversations[0].compacted(asked).unwrap();
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(state(&window), (4, four.clone()));

        // A later one the window makes goes on from it, and the floor stays.
        terminal.conversations[0].messages.push(said("user", "6"));
        terminal.conversations[0].summarized(6, "The first six.".into());
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(state(&window), (4, Some((6, "The first six.".to_owned()))));

        // A rewind into that one goes back to the user's.
        terminal.conversations[0].rewind(5);
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(state(&window), (4, four));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_read_in_only_when_opened_keeps_the_users_summary() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("fill-asked");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut store = Store::default();
        store.conversations.push(tagged(6, "filled"));
        let asked = asked_for(&store.conversations[0], 4, "Four.");
        store.conversations[0].compacted(asked).unwrap();
        store.save().unwrap();
        let four = (4, Some((4, "Four.".to_owned())));

        // Off the thread that holds it, as the window reads it.
        let mut listed = load_listing();
        assert!(listed.conversations[0].unfilled());
        listed.conversations[0].filled_with(super::messages(1).unwrap());
        let open = &listed.conversations[0];
        assert_eq!((open.floor(), open.summary.clone()), four);

        // And on it, as the terminal does.
        let mut listed = load_listing();
        fill(&mut listed.conversations[0]);
        let open = &listed.conversations[0];
        assert_eq!((open.floor(), open.summary.clone()), four);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_restart_uses_the_kept_summary_rather_than_asking_again() {
        let dir = scratch("summary-restart");
        let path = dir.join("1.jsonl");
        let mut conversation = tagged(6, "restart");
        conversation.summarized(4, "Kept across the restart.".into());
        conversation.flush_to(&path).unwrap();

        // Read back as a new process would, nothing in memory but the file.
        let back = replay(&path).unwrap();
        let nowhere = crate::chat::Connection {
            endpoint: "http://127.0.0.1:9/v1".into(),
            ..Default::default()
        };
        let summary = crate::compact::summary(
            &nowhere,
            &back.messages[..4],
            0,
            || panic!("the model was asked for the summary again"),
            |_| panic!("the kept summary was handed over as new"),
            |_| panic!("asking for the summary again was paid for"),
        );
        assert_eq!(summary.as_deref(), Some("Kept across the restart."));
    }

    #[test]
    fn a_rewind_into_a_summary_drops_it_and_leaves_an_earlier_one() {
        let dir = scratch("summary-rewind");
        let path = dir.join("1.jsonl");
        let mut conversation = tagged(6, "rewind into");
        conversation.summarized(2, "The first two.".into());
        conversation.flush_to(&path).unwrap();
        conversation.summarized(4, "The first four.".into());
        conversation.flush_to(&path).unwrap();

        conversation.rewind(3);
        assert_eq!(conversation.summary, None);
        conversation.flush_to(&path).unwrap();
        // The file still has the one the rewind left whole.
        let back = replay(&path).unwrap();
        assert_eq!(back.messages.len(), 3);
        assert_eq!(back.summary, Some((2, "The first two.".into())));

        // Past both, neither stands.
        conversation.rewind(1);
        conversation.flush_to(&path).unwrap();
        assert_eq!(replay(&path).unwrap().summary, None);
    }

    #[test]
    fn a_rewind_that_leaves_what_a_summary_covers_keeps_it() {
        let dir = scratch("summary-rewind-after");
        let path = dir.join("1.jsonl");
        let mut conversation = tagged(6, "rewind after");
        conversation.summarized(4, "The first four.".into());
        conversation.flush_to(&path).unwrap();

        conversation.rewind(4);
        conversation.messages.push(said("user", "again"));
        conversation.flush_to(&path).unwrap();
        assert_eq!(conversation.summary, Some((4, "The first four.".into())));
        let back = replay(&path).unwrap();
        assert_eq!(back.messages.len(), 5);
        assert_eq!(back.summary, Some((4, "The first four.".into())));
    }

    #[test]
    fn a_session_file_from_before_summaries_reads_back_without_one() {
        let dir = scratch("summary-old");
        let path = dir.join("1.jsonl");
        // As a build from before summaries were kept wrote it.
        std::fs::write(
            &path,
            concat!(
                "{\"type\":\"head\",\"id\":1,\"created\":5}\n",
                "{\"type\":\"message\",\"role\":\"user\",\"content\":\"hello\"}\n",
                "{\"type\":\"message\",\"role\":\"assistant\",\"content\":\"hi\"}\n",
            ),
        )
        .unwrap();
        let mut back = replay(&path).unwrap();
        assert_eq!(back.messages.len(), 2);
        assert_eq!(back.summary, None);
        // Saving it again adds nothing for a summary it never had.
        let before = std::fs::read_to_string(&path).unwrap();
        back.flush_to(&path).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        // A summary line claiming more than the file holds is not used.
        std::fs::write(
            &path,
            format!("{before}{{\"type\":\"summary\",\"upto\":3,\"text\":\"x\"}}\n"),
        )
        .unwrap();
        assert_eq!(replay(&path).unwrap().summary, None);
    }

    #[test]
    fn a_copy_keeps_the_summary_only_when_it_holds_what_it_covers() {
        let dir = scratch("summary-fork");
        let mut source = tagged(6, "fork");
        source.summarized(4, "The first four.".into());
        assert_eq!(source.summary_within(6), source.summary);
        assert_eq!(source.summary_within(4), source.summary);
        assert_eq!(source.summary_within(3), None);

        // A copy writes the summary it started with into its own file.
        let path = dir.join("2.jsonl");
        let mut copy = Conversation {
            id: 2,
            messages: source.messages[..5].to_vec(),
            summary: source.summary_within(5),
            ..Default::default()
        };
        copy.flush_to(&path).unwrap();
        assert_eq!(
            replay(&path).unwrap().summary,
            Some((4, "The first four.".into()))
        );
    }

    /// A session file written by hand: session 3 with `title` and `said`
    /// messages.
    fn session_text(title: &str, said: usize) -> String {
        let mut text = format!(
            "{{\"type\":\"head\",\"id\":3,\"created\":5}}\n\
             {{\"type\":\"meta\",\"title\":\"{title}\",\"updated\":5,\"pinned\":false,\"workspace\":null,\"todos\":[]}}\n"
        );
        for i in 0..said {
            text.push_str(&format!(
                "{{\"type\":\"message\",\"role\":\"user\",\"content\":\"{i}\"}}\n"
            ));
        }
        text
    }

    #[test]
    fn listing_the_sessions_indexes_every_one_of_them() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("index-built");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.conversations.push(Conversation {
            id: 2,
            title: "second".into(),
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        assert_eq!(summaries().len(), 2);

        let index = super::read_index(&super::index_path());
        assert_eq!(index.len(), 2);
        for (id, entry) in &index {
            let size = std::fs::metadata(dir.join(format!("sessions/{id}.jsonl")))
                .unwrap()
                .len();
            assert_eq!((entry.size, entry.len), (size, size));
        }
        assert_eq!(index[&2].summary.title, "second");
        assert_eq!(index[&1].summary.messages, 2);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn an_unchanged_session_is_listed_from_the_index_without_being_read() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("index-used");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            title: "kept".into(),
            ..numbered(2)
        });
        store.save().unwrap();
        assert_eq!(summaries()[0].title, "kept");

        // Changed behind the index's back: the same size and time, other bytes.
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::metadata(&path).unwrap().modified().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"kept\"", "\"lost\"")).unwrap();
        let touch = |time| {
            let file = std::fs::File::options().write(true).open(&path).unwrap();
            file.set_modified(time).unwrap();
        };
        touch(before);
        assert_eq!(summaries()[0].title, "kept");

        // Once its time moves as well, it is read again, and says otherwise.
        touch(before + std::time::Duration::from_secs(1));
        assert_eq!(summaries()[0].title, "lost");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_grown_session_is_read_only_past_what_the_index_has() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("index-grown");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        let long = "x".repeat(2 * super::TAIL_CHECK as usize);
        store.conversations.push(Conversation {
            id: 1,
            title: "kept".into(),
            messages: vec![said("user", &long)],
            ..Default::default()
        });
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 1);

        // Changed where only a reading from the start would see it: before
        // the long message and the bytes checked at its end.
        let path = dir.join("sessions/1.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"kept\"", "\"lost\"")).unwrap();
        store.conversations[0].messages.push(said("user", "more"));
        store.save().unwrap();
        let listed = summaries();
        assert_eq!((listed[0].title.as_str(), listed[0].messages), ("kept", 2));

        std::fs::remove_file(dir.join("sessions/index.json")).unwrap();
        let listed = summaries();
        assert_eq!((listed[0].title.as_str(), listed[0].messages), ("lost", 2));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_that_shrank_or_was_replaced_is_read_again_in_full() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("index-shrunk");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("sessions/3.jsonl");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();

        std::fs::write(&path, session_text("long", 3)).unwrap();
        assert_eq!(summaries()[0].messages, 3);
        // Rewritten shorter, as a compacted session would be.
        std::fs::write(&path, session_text("short", 1)).unwrap();
        let listed = summaries();
        assert_eq!((listed[0].title.as_str(), listed[0].messages), ("short", 1));
        // Replaced by a longer one, as when a deleted session's id is reused.
        std::fs::write(&path, session_text("other", 4)).unwrap();
        let listed = summaries();
        assert_eq!((listed[0].title.as_str(), listed[0].messages), ("other", 4));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_line_caught_half_written_is_counted_once_it_is_finished() {
        use std::io::Write;
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("index-half-line");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("sessions/3.jsonl");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let append = |bytes: &str| {
            let mut file = std::fs::File::options().append(true).open(&path).unwrap();
            file.write_all(bytes.as_bytes()).unwrap();
        };

        std::fs::write(&path, session_text("kept", 1)).unwrap();
        append("{\"type\":\"message\",\"role\":\"user\",\"content\":\"tw");
        // Counted by its tag, as a reading of the whole file counts it.
        assert_eq!(summaries()[0].messages, 2);
        append("o\"}\n");
        assert_eq!(summaries()[0].messages, 2);
        append("{\"type\":\"message\",\"role\":\"user\",\"content\":\"three\"}\n");
        assert_eq!(summaries()[0].messages, 3);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_broken_or_missing_index_is_rebuilt_from_the_sessions() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("index-broken");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.conversations.push(Conversation {
            id: 2,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        assert_eq!(summaries().len(), 2);

        let index = dir.join("sessions/index.json");
        let counts = || {
            let mut listed: Vec<(u64, usize)> =
                summaries().iter().map(|s| (s.id, s.messages)).collect();
            listed.sort();
            listed
        };
        std::fs::write(&index, "{\"version\":1,\"sessions\":[{\"si").unwrap();
        assert_eq!(counts(), [(1, 2), (2, 1)]);
        assert_eq!(super::read_index(&index).len(), 2);
        // One from a build that meant something else by its entries.
        let text = std::fs::read_to_string(&index).unwrap();
        std::fs::write(&index, text.replace("\"version\":1", "\"version\":0")).unwrap();
        assert!(super::read_index(&index).is_empty());
        assert_eq!(counts(), [(1, 2), (2, 1)]);
        std::fs::remove_file(&index).unwrap();
        assert_eq!(counts(), [(1, 2), (2, 1)]);
        assert_eq!(super::read_index(&index).len(), 2);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_deleted_session_leaves_the_listing_and_the_index() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("index-deleted");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.conversations.push(Conversation {
            id: 2,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        store.save().unwrap();
        assert_eq!(summaries().len(), 2);

        store.remove(0);
        let listed: Vec<u64> = summaries().iter().map(|s| s.id).collect();
        assert_eq!(listed, [2]);
        let indexed: Vec<u64> = super::read_index(&super::index_path())
            .into_keys()
            .collect();
        assert_eq!(indexed, [2]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn listings_in_many_threads_at_once_leave_a_whole_index() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("index-threads");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        // Each thread writes its own session and lists them all as it goes,
        // so every one of them is rewriting the index at the same time.
        let writers: Vec<_> = (1..=6u64)
            .map(|id| {
                std::thread::spawn(move || {
                    let mut store = Store::default();
                    store.conversations.push(Conversation {
                        id,
                        ..Default::default()
                    });
                    for i in 0..10 {
                        store.conversations[0]
                            .messages
                            .push(said("user", &i.to_string()));
                        store.save().unwrap();
                        let listed = summaries();
                        let mine = listed.iter().find(|s| s.id == id).unwrap();
                        assert_eq!(mine.messages, i + 1);
                    }
                })
            })
            .collect();
        for writer in writers {
            writer.join().unwrap();
        }

        assert!(summaries().iter().all(|s| s.messages == 10));
        let index = super::index_path();
        let cached = super::read_index(&index);
        assert_eq!(cached.len(), 6);
        std::fs::remove_file(&index).unwrap();
        summaries();
        assert!(cached == super::read_index(&index));
        let temp = std::fs::read_dir(dir.join("sessions"))
            .unwrap()
            .filter_map(Result::ok)
            .any(|e| e.file_name().to_string_lossy().ends_with(".tmp"));
        assert!(!temp);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_let_go_of_reads_back_what_it_had() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("unload");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut store = Store::default();
        store.conversations.push(numbered(3));
        store.conversations[0].summarized(2, "the first two".into());
        store.save().unwrap();
        let path = dir.join("sessions/1.jsonl");
        let before = std::fs::read_to_string(&path).unwrap();

        let open = &mut store.conversations[0];
        assert!(open.unload());
        assert!(open.messages.is_empty());
        assert_eq!(open.length(), 3);
        // Saved while let go, nothing is written, above all not a rewind.
        store.save().unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

        let open = &mut store.conversations[0];
        fill(open);
        let contents: Vec<&str> = open.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "2"]);
        assert_eq!(open.summary, Some((2, "the first two".into())));
        open.messages.push(said("user", "3"));
        store.save().unwrap();
        let back = replay(&path).unwrap();
        let contents: Vec<&str> = back.messages.iter().map(|m| m.content.as_str()).collect();
        assert_eq!(contents, ["0", "1", "2", "3"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_with_anything_unsaved_is_kept() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("unload-unsaved");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut store = Store::default();
        store.conversations.push(numbered(3));
        // Never saved at all.
        assert!(!store.conversations[0].unload());
        store.save().unwrap();

        let open = &mut store.conversations[0];
        open.messages.push(said("user", "3"));
        assert!(!open.unload(), "a message not yet written");
        store.save().unwrap();
        let open = &mut store.conversations[0];
        open.rewind(1);
        assert!(!open.unload(), "a rewind not yet written");
        store.save().unwrap();
        let open = &mut store.conversations[0];
        open.title = "renamed".into();
        assert!(!open.unload(), "a new title not yet written");
        store.save().unwrap();
        let open = &mut store.conversations[0];
        open.summarized(1, "the first".into());
        assert!(!open.unload(), "a summary not yet written");
        store.save().unwrap();
        assert!(store.conversations[0].unload());
        assert_eq!(store.conversations[0].length(), 1);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rename_not_yet_saved_survives_the_session_being_read_in() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("fill-rename");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut store = Store::default();
        store.conversations.push(numbered(2));
        store.save().unwrap();

        let mut listed = load_listing();
        listed.conversations[0].title = "renamed".into();
        fill(&mut listed.conversations[0]);
        listed.save().unwrap();
        let back = replay(&dir.join("sessions/1.jsonl")).unwrap();
        assert_eq!(back.title, "renamed");
        assert_eq!(back.messages.len(), 2);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_read_that_lands_after_the_session_moved_on_is_dropped() {
        let mut open = Conversation {
            id: 1,
            counted: 2,
            ..Default::default()
        };
        // Something was said before the read came back: the session now
        // holds more than its file did, and the read would lose it.
        open.messages.push(said("user", "new"));
        open.filled_with((numbered(2).messages, None, Vec::new(), Vec::new()));
        assert_eq!(open.messages.len(), 1);
        assert_eq!(open.messages[0].content, "new");

        // A file emptied meanwhile is not counted, or read again, any more.
        let mut emptied = Conversation {
            id: 1,
            counted: 2,
            ..Default::default()
        };
        emptied.filled_with((Vec::new(), None, Vec::new(), Vec::new()));
        assert!(!emptied.unfilled());
        assert_eq!(emptied.length(), 0);
    }

    fn wrote(path: &str, name: &str, is_error: bool) -> Message {
        Message {
            role: "tool".into(),
            name: name.into(),
            args: format!(r#"{{"path":"{path}"}}"#),
            is_error,
            ..Default::default()
        }
    }

    #[test]
    fn artifacts_are_the_writes_on_disk_newest_first_with_repeats_counted() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("artifacts");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let session = |id, title: &str, updated, workspace: Option<&str>, messages| Conversation {
            id,
            title: title.into(),
            updated,
            workspace: workspace.map(std::path::PathBuf::from),
            messages,
            ..Default::default()
        };
        let mut file_path = wrote("", "write_file", false);
        file_path.args = r#"{"file_path":"notes.md","content":"x"}"#.into();
        let conversations = vec![
            session(
                1,
                "older",
                100,
                Some("/work"),
                vec![
                    wrote("a.txt", "write_file", false),
                    wrote("b.txt", "write_file", false),
                    // Failures never produced a file.
                    wrote("never.txt", "write_file", true),
                    // Reads are not artifacts.
                    wrote("c.txt", "read_file", false),
                    // Under the name the model sent it with.
                    file_path,
                ],
            ),
            session(
                2,
                "newer",
                200,
                Some("/work"),
                vec![
                    wrote("a.txt", "edit_file", false),
                    wrote("taken-back.txt", "write_file", false),
                ],
            ),
            // No project folder means no path to resolve against.
            session(
                3,
                "chat",
                300,
                None,
                vec![wrote("d.txt", "write_file", false)],
            ),
        ];
        let mut store = Store {
            conversations,
            ..Default::default()
        };
        store.save().unwrap();
        // What a rewind took back was never there, as far as the session goes.
        store.conversations[1].rewind(1);
        store.save().unwrap();

        let found = super::artifacts();
        let names: Vec<String> = found
            .iter()
            .map(|a| a.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["a.txt", "b.txt", "notes.md"]);
        // a.txt was written then edited, and reports the newer session.
        assert_eq!(found[0].writes, 2);
        assert_eq!(found[0].session, "newer");
        assert_eq!(found[0].written, 200);
        assert_eq!(found[1].writes, 1);
        assert_eq!(found[1].path, std::path::PathBuf::from("/work/b.txt"));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_patched_file_is_an_output_and_one_the_patch_deleted_or_moved_away_is_not() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("patched-artifacts");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let patched = |patch: &str, is_error| Message {
            role: "tool".into(),
            name: "apply_patch".into(),
            args: serde_json::json!({ "patch": patch }).to_string(),
            is_error,
            ..Default::default()
        };
        let applied = "*** Begin Patch\n*** Add File: new.txt\n+hi\n*** Delete File: gone.txt\n\
                       *** Update File: old.rs\n*** Move to: moved.rs\n@@\n-a\n+b\n\
                       *** Update File: kept.rs\n@@\n-a\n+b\n*** End Patch";
        let refused = "*** Begin Patch\n*** Add File: never.txt\n+hi\n*** End Patch";
        let mut store = Store {
            conversations: vec![Conversation {
                id: 1,
                title: "patched".into(),
                updated: 100,
                workspace: Some("/work".into()),
                messages: vec![patched(applied, false), patched(refused, true)],
                ..Default::default()
            }],
            ..Default::default()
        };
        store.save().unwrap();

        let names: Vec<String> = super::artifacts()
            .iter()
            .map(|a| a.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, ["new.txt", "moved.rs", "kept.rs"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn spent_counts_every_reply_its_summaries_and_helpers() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("spent");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::create_dir_all(dir.join("agents").join("1")).unwrap();
        // A reply from before replies kept their time, then one with a
        // summary made for it, which a rewind took back.
        let lines = [
            r#"{"type":"head","id":1,"created":100}"#,
            r#"{"type":"message","role":"user","content":"hi"}"#,
            r#"{"type":"message","role":"assistant","content":"old","model":"m","tokens":5,"prompt_tokens":50}"#,
            r#"{"type":"message","role":"assistant","content":"new","model":"m","tokens":7,"prompt_tokens":70,"at":300,"summary_usage":{"input":900,"output":60}}"#,
            r#"{"type":"rewind","to":2}"#,
            r#"{"type":"meta","title":"t","updated":200,"pinned":false,"workspace":null,"todos":[]}"#,
        ];
        std::fs::write(dir.join("sessions").join("1.jsonl"), lines.join("\n")).unwrap();
        let helper = [
            r#"{"type":"head","id":1,"created":150}"#,
            r#"{"type":"message","role":"assistant","content":"done","model":"h","tokens":3,"prompt_tokens":30,"at":160}"#,
        ];
        std::fs::write(
            dir.join("agents").join("1").join("1.jsonl"),
            helper.join("\n"),
        )
        .unwrap();

        // A branch of it, made later, which copied the reply that kept its time.
        let branch = [r#"{"type":"head","id":2,"created":400}"#, lines[3]];
        std::fs::write(dir.join("sessions").join("2.jsonl"), branch.join("\n")).unwrap();

        let (mut first, copied): (Vec<_>, Vec<_>) = super::spent(0, 1)
            .into_iter()
            .partition(|call| call.session == 1);
        first.sort_by_key(|call| (call.at, call.usage.input));
        let seen: Vec<(u64, &str, u64, bool)> = first
            .iter()
            .map(|call| (call.at, call.model.as_str(), call.usage.input, call.copy))
            .collect();
        let once = [
            (160, "h", 30),
            (200, "m", 50),
            (300, "m", 70),
            (300, "m", 900),
        ];
        assert_eq!(
            seen,
            once.map(|(at, model, input)| (at, model, input, false))
        );
        // The branch's reply and its summary are the same two calls again.
        assert_eq!(copied.len(), 2);
        assert!(copied.iter().all(|call| call.session == 2 && call.copy));
        // Files not written to since are passed over, but for the session asked about.
        assert_eq!(super::spent(u64::MAX, 1).len(), 4);
        assert!(super::spent(u64::MAX, 3).is_empty());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn undated_replies_wait_for_the_end_without_their_text() {
        let dir = scratch("undated");
        let path = dir.join("1.jsonl");
        let big = "x".repeat(100_000);
        let lines = [
            r#"{"type":"head","id":1,"created":100}"#.to_owned(),
            format!(
                r#"{{"type":"message","role":"assistant","content":"{big}","reasoning":"{big}","model":"m","tool_calls":[{{"id":"1","name":"write_file","arguments":"{big}"}}]}}"#
            ),
            r#"{"type":"meta","title":"t","updated":200,"pinned":false,"workspace":null,"todos":[]}"#.to_owned(),
        ];
        std::fs::write(&path, lines.join("\n")).unwrap();
        let mut seen = std::collections::HashSet::new();
        let mut held = Vec::new();
        super::replies_in(&path, &mut seen, &mut |at, reply: &Message| {
            let call = &reply.tool_calls[0];
            held.push((
                at,
                call.name.clone(),
                reply.content.capacity() + reply.reasoning.capacity(),
                call.arguments.capacity(),
            ));
        });
        assert_eq!(held, [(200, "write_file".to_owned(), 0, 0)]);
    }

    #[test]
    fn a_session_for_a_scheduled_run_takes_the_next_free_number_and_reads_back() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("transcript-session");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(Conversation {
            id: 4,
            ..numbered(1)
        });
        store.save().unwrap();
        let mut run = Transcript::session("Morning news", Some("work".into())).unwrap();
        assert_eq!(run.conversation.id, 5);
        run.conversation
            .messages
            .push(said("user", "what happened"));
        run.save().unwrap();
        assert_eq!(
            Transcript::session("again", None).unwrap().conversation.id,
            6
        );

        let back = super::session(5).unwrap();
        assert_eq!(back.title, "Morning news");
        assert_eq!(back.workspace, Some("work".into()));
        assert_eq!(back.messages.len(), 1);
        // A new session's number counts it as taken.
        assert_eq!(super::load().next_id(), 7);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_new_session_gets_its_file_with_its_first_message() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("first-message");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        let id = store.reserve_id();
        store.conversations.push(Conversation {
            id,
            title: "New session".into(),
            ..Default::default()
        });
        // Saved as anything else changing saves, with nothing said yet.
        store.save().unwrap();
        assert!(summaries().iter().all(|s| s.id != id), "listed empty");
        let reserved = std::fs::metadata(super::session_path(id)).unwrap();
        assert_eq!(reserved.len(), 0);

        store.conversations[0]
            .messages
            .push(said("user", "the first question"));
        store.save().unwrap();
        assert!(summaries().iter().any(|s| s.id == id && s.messages == 1));

        // One left unsent still gives its number back on the way out.
        let unsent = store.reserve_id();
        store.conversations.push(Conversation {
            id: unsent,
            title: "New session".into(),
            ..Default::default()
        });
        store.save().unwrap();
        store.release_unused();
        assert!(!super::session_path(unsent).exists());
        assert!(super::session_path(id).exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_scheduled_run_never_takes_the_number_of_a_session_not_saved_yet() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("claimed-number");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        for id in 1..=3 {
            store.conversations.push(Conversation { id, ..numbered(1) });
        }
        store.save().unwrap();
        let mut elsewhere = load_listing();
        // The terminal opens a fresh session and sends nothing yet.
        let id = store.reserve_id();
        assert_eq!(id, 4);
        store.conversations.push(Conversation {
            id,
            title: "terminal".into(),
            ..Default::default()
        });
        assert!(
            summaries().iter().all(|s| s.id != 4),
            "an empty reservation was listed"
        );

        let run = Transcript::session("Morning news", None).unwrap();
        assert_eq!(run.conversation.id, 5);
        store.conversations[3]
            .messages
            .push(said("user", "my own question"));
        store.save().unwrap();
        let mine = super::session(4).unwrap();
        assert_eq!(mine.title, "terminal");
        assert_eq!(mine.messages.len(), 1);
        assert_eq!(super::session(5).unwrap().title, "Morning news");
        elsewhere.pick_up_new();
        assert!([4, 5].iter().all(|&id| elsewhere.index_of(id).is_some()));

        // One opened and left unsent gives its number back on the way out.
        let unsent = store.reserve_id();
        store.conversations.push(Conversation {
            id: unsent,
            ..Default::default()
        });
        store.release_unused();
        assert!(!super::session_path(unsent).exists());
        assert!(super::session_path(4).exists() && super::session_path(5).exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_startup_sweep_takes_only_old_side_question_files() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("asides");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let old = Transcript::aside(1, None).unwrap().path;
        let running = Transcript::aside(1, None).unwrap().path;
        let other = dir.join("asides").join("notes.jsonl");
        std::fs::write(&other, "").unwrap();
        // The lock a save of each left behind, and one this feature never made.
        let [old_lock, running_lock] = [&old, &running].map(|path| {
            drop(Lock::take(path).unwrap());
            path.with_extension("lock")
        });
        let other_lock = dir.join("asides").join("notes.lock");
        std::fs::write(&other_lock, "").unwrap();
        let long_ago = std::time::SystemTime::now() - ASIDE_STALE * 2;
        for path in [&old, &other, &old_lock, &other_lock] {
            let file = std::fs::File::options().write(true).open(path).unwrap();
            file.set_modified(long_ago).unwrap();
        }
        load_listing();
        assert!(!old.exists() && !old_lock.exists());
        assert!(running.exists() && other.exists());
        assert!(running_lock.exists() && other_lock.exists());
        assert!(!dir.join("agents").exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    /// Session 1 saved with `n` numbered messages, then read in by the window
    /// as it starts and by the terminal as it opens the session: the window's
    /// copy follows the file, and the terminal's is saved blind, as before.
    fn window_and_terminal(n: usize) -> (Store, Store) {
        let mut first = Store::default();
        first.conversations.push(numbered(n));
        first.save().unwrap();
        let window = load();
        let mut terminal = load_listing();
        fill(&mut terminal.conversations[0]);
        (window, terminal)
    }

    fn contents(conversation: &Conversation) -> Vec<&str> {
        conversation
            .messages
            .iter()
            .map(|m| m.content.as_str())
            .collect()
    }

    fn append(path: &std::path::Path, bytes: &str) {
        use std::io::Write;
        let mut file = std::fs::File::options().append(true).open(path).unwrap();
        file.write_all(bytes.as_bytes()).unwrap();
    }

    #[test]
    fn what_the_terminal_appends_reaches_the_window() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-appended");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(2);
        let path = dir.join("sessions/1.jsonl");

        terminal.conversations[0]
            .messages
            .push(said("assistant", "from the terminal"));
        terminal.conversations[0].title = "renamed there".into();
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        let open = &window.conversations[0];
        assert_eq!(contents(open), ["0", "1", "from the terminal"]);
        assert_eq!(open.title, "renamed there");

        // Taken in as the file has it, so there is nothing to write back.
        let before = std::fs::read(&path).unwrap();
        window.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!window.follow(|_| false));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn only_the_lines_the_terminal_added_are_read() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-tail");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut first = Store::default();
        let long = "x".repeat(2 * super::TAIL_CHECK as usize);
        first.conversations.push(Conversation {
            id: 1,
            title: "kept".into(),
            messages: vec![said("user", &long)],
            ..Default::default()
        });
        first.save().unwrap();
        let mut window = load();

        // Changed where only a reading from the start would see it: before
        // the long message and the bytes checked at its end.
        let path = dir.join("sessions/1.jsonl");
        let text = std::fs::read_to_string(&path).unwrap();
        std::fs::write(&path, text.replace("\"kept\"", "\"lost\"")).unwrap();
        append(
            &path,
            "{\"type\":\"message\",\"role\":\"user\",\"content\":\"more\"}\n",
        );
        assert!(window.follow(|_| false));
        let open = &window.conversations[0];
        assert_eq!((open.title.as_str(), open.messages.len()), ("kept", 2));
        assert_eq!(open.messages[1].content, "more");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_file_replaced_under_the_window_is_read_again_in_full() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-replaced");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("sessions/3.jsonl");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::write(&path, session_text("long", 3)).unwrap();
        let mut window = load();
        let open = |store: &Store| {
            let c = &store.conversations[0];
            (c.title.clone(), c.messages.len())
        };

        // Rewritten shorter, as a compacted session would be.
        std::fs::write(&path, session_text("short", 1)).unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(open(&window), ("short".into(), 1));
        // Replaced by a longer one, as when a deleted session's id is reused.
        std::fs::write(&path, session_text("other", 4)).unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(open(&window), ("other".into(), 4));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_the_terminal_starts_joins_the_window_which_leaves_it_be() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-started");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(1);

        // Reserved and not yet saved, it is the terminal's alone.
        let id = terminal.reserve_id();
        assert!(!window.follow(|_| false));
        assert_eq!(window.conversations.len(), 1);

        terminal.conversations.push(Conversation {
            id,
            title: "started there".into(),
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        let started = &window.conversations[1];
        assert_eq!((started.id, started.title.as_str()), (id, "started there"));
        assert_eq!(contents(started), ["hello"]);

        let path = dir.join(format!("sessions/{id}.jsonl"));
        let before = std::fs::read(&path).unwrap();
        window.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_the_window_is_replying_in_is_followed_once_the_reply_ends() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-busy");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(1);

        terminal.conversations[0]
            .messages
            .push(said("user", "meanwhile"));
        terminal.save().unwrap();
        assert!(!window.follow(|id| id == 1));
        assert_eq!(contents(&window.conversations[0]), ["0"]);
        assert!(window.follow(|_| false));
        assert_eq!(contents(&window.conversations[0]), ["0", "meanwhile"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_decision_the_terminal_keeps_is_followed_into_the_window() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-decision");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(2);
        let path = dir.join("sessions/1.jsonl");

        let made = Judged::of(
            &terminal.conversations[0].messages,
            "a",
            "run_command(cargo build)".into(),
            "auto: allowed".into(),
        );
        terminal.conversations[0].decisions.push(made.clone());
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(window.conversations[0].decisions, [made]);
        // Taken in as already written, so the window's save adds nothing.
        let before = std::fs::read(&path).unwrap();
        window.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), before);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn when_both_said_something_the_window_keeps_its_own_copy_and_says_so() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-clash");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(2);
        let path = dir.join("sessions/1.jsonl");

        terminal.conversations[0]
            .messages
            .push(said("user", "in the terminal"));
        terminal.save().unwrap();
        let theirs = std::fs::read(&path).unwrap();
        // The window's reply ends, and is saved, before it has followed.
        window.conversations[0]
            .messages
            .push(said("assistant", "in the window"));
        window.save().unwrap();
        assert!(window.conversations[0].diverged);
        assert_eq!(
            contents(&replay(&path).unwrap()),
            ["0", "1", "in the window"]
        );
        // Only added to: the terminal's line is still in the file.
        assert!(std::fs::read(&path).unwrap().starts_with(&theirs));

        // Nothing is left to take in, or to say again.
        window.conversations[0].diverged = false;
        assert!(!window.follow(|_| false));
        window.save().unwrap();
        assert!(!window.conversations[0].diverged);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rewind_among_the_terminal_lines_does_not_cut_into_the_window_copy() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-clash-rewind");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(3);
        let path = dir.join("sessions/1.jsonl");

        terminal.conversations[0].rewind(1);
        terminal.conversations[0]
            .messages
            .push(said("user", "instead"));
        terminal.save().unwrap();
        window.conversations[0]
            .messages
            .push(said("assistant", "in the window"));
        window.save().unwrap();
        assert_eq!(
            contents(&replay(&path).unwrap()),
            ["0", "1", "2", "in the window"]
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rename_in_the_window_while_the_terminal_writes_keeps_both() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-rename");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(1);
        let path = dir.join("sessions/1.jsonl");

        terminal.conversations[0]
            .messages
            .push(said("user", "in the terminal"));
        terminal.save().unwrap();
        window.conversations[0].title = "renamed here".into();
        window.save().unwrap();
        assert!(!window.conversations[0].diverged);
        let back = replay(&path).unwrap();
        assert_eq!(back.title, "renamed here");
        assert_eq!(contents(&back), ["0", "in the terminal"]);

        assert!(window.follow(|_| false));
        let open = &window.conversations[0];
        assert_eq!(open.title, "renamed here");
        assert_eq!(contents(open), ["0", "in the terminal"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_line_the_terminal_is_still_writing_is_taken_in_once_whole() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-half-line");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, _) = window_and_terminal(1);
        let path = dir.join("sessions/1.jsonl");

        append(
            &path,
            "{\"type\":\"message\",\"role\":\"user\",\"content\":\"tw",
        );
        assert!(!window.follow(|_| false));
        assert_eq!(contents(&window.conversations[0]), ["0"]);
        append(&path, "o\"}\n");
        assert!(window.follow(|_| false));
        assert_eq!(contents(&window.conversations[0]), ["0", "two"]);
        assert!(!window.follow(|_| false));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_line_a_crash_cut_short_is_not_taken_for_the_terminal_writing() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-torn");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, _) = window_and_terminal(1);
        let path = dir.join("sessions/1.jsonl");

        append(&path, "{\"type\":\"message\",\"role\":\"us");
        window.conversations[0].messages.push(said("user", "again"));
        window.save().unwrap();
        assert!(!window.conversations[0].diverged);
        assert_eq!(contents(&replay(&path).unwrap()), ["0", "again"]);
        assert!(!window.follow(|_| false));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_replaced_under_the_window_is_not_written_into() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-replaced-save");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, _) = window_and_terminal(2);
        let path = dir.join("sessions/1.jsonl");

        // Deleted elsewhere, and its id given to a session started there.
        std::fs::remove_file(&path).unwrap();
        let mut terminal = Store::default();
        terminal.conversations.push(Conversation {
            id: 1,
            title: "another".into(),
            messages: vec![said("user", "not yours")],
            ..Default::default()
        });
        terminal.save().unwrap();
        let theirs = std::fs::read(&path).unwrap();

        window.conversations[0]
            .messages
            .push(said("user", "still here?"));
        window.save().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), theirs);
        assert!(window.conversations[0].gone);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_the_window_started_is_followed_after_its_first_save() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-new-here");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut window = load();
        let id = window.reserve_id();
        window.conversations.push(Conversation {
            id,
            messages: vec![said("user", "hello")],
            ..Default::default()
        });
        window.save().unwrap();

        let mut terminal = load_listing();
        fill(&mut terminal.conversations[0]);
        terminal.conversations[0]
            .messages
            .push(said("assistant", "from the terminal"));
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(
            contents(&window.conversations[0]),
            ["hello", "from the terminal"]
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn following_leaves_a_listed_session_to_be_read_when_it_is_opened() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-listed");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (_, mut terminal) = window_and_terminal(2);
        let mut listing = load_listing();

        terminal.conversations[0]
            .messages
            .push(said("user", "more"));
        terminal.save().unwrap();
        assert!(!listing.follow(|_| false));
        assert!(listing.conversations[0].messages.is_empty());
        assert_eq!(listing.conversations[0].length(), 2);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_goal_checks_calls_count_in_usage_without_what_it_said() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("goal-calls");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        for (at, answer) in [(500, "first verdict"), (560, "second verdict")] {
            let mut aside = Transcript::aside(7, None).unwrap();
            aside.conversation.messages = vec![
                said("user", "the question"),
                Message {
                    model: "judge".into(),
                    tokens: 40,
                    prompt_tokens: 800,
                    cost: Some(0.002),
                    at,
                    ..said("assistant", answer)
                },
            ];
            aside.save().unwrap();
            super::keep_calls(&aside.path, 7).unwrap();
        }
        let spent = super::spent(0, 7);
        assert_eq!(spent.len(), 2, "{spent:?}");
        assert!(spent.iter().all(|call| {
            (
                call.session,
                call.model.as_str(),
                call.usage.output,
                call.usage.input,
            ) == (7, "judge", 40, 800)
                && call.usage.cost == Some(0.002)
                && !call.copy
        }));
        let kept = std::fs::read_to_string(dir.join("agents/7/calls.jsonl")).unwrap();
        assert!(
            !kept.contains("verdict") && !kept.contains("question"),
            "{kept}"
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_goal_is_kept_with_its_session_and_cleared_on_disk_too() {
        let dir = scratch("goal");
        let path = dir.join("1.jsonl");
        let mut conversation = tagged(1, "goal");
        conversation.flush_to(&path).unwrap();
        // A session without one writes no more than before goals existed.
        assert!(!std::fs::read_to_string(&path).unwrap().contains("goal\""));
        let goal = crate::goal::Goal {
            continuations: 3,
            ..crate::goal::Goal::new("ship it", Some("cargo test".into()))
        };
        conversation.goal = Some(goal.clone());
        conversation.flush_to(&path).unwrap();
        assert_eq!(replay(&path).unwrap().goal, Some(goal));
        conversation.goal = None;
        conversation.flush_to(&path).unwrap();
        assert_eq!(replay(&path).unwrap().goal, None);
    }

    #[test]
    fn a_sessions_worktree_is_kept_in_its_file_and_its_listing() {
        let dir = scratch("worktree");
        let path = dir.join("1.jsonl");
        let mut conversation = tagged(1, "tree");
        conversation.flush_to(&path).unwrap();
        // A session without one writes no more than before sessions had them.
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains("worktree\"")
        );
        let tree = crate::worktree::Worktree {
            path: "/work/.letronna/worktrees/session-1".into(),
            branch: "letronna/session-1".into(),
            base: "0123abcd".into(),
            prefix: String::new(),
            checkout: Some("/work".into()),
        };
        conversation.worktree = Some(tree.clone());
        conversation.flush_to(&path).unwrap();

        assert_eq!(replay(&path).unwrap().worktree.as_ref(), Some(&tree));
        let (_, summary) = super::scan(&path, 1, None).unwrap();
        assert_eq!(super::listed(summary).worktree, Some(tree));
    }

    /// A fixed-seed source of made-up sessions, so every run of the bench
    /// searches the same bytes.
    #[cfg(target_os = "linux")]
    struct Dice(u64);

    #[cfg(target_os = "linux")]
    impl Dice {
        fn next(&mut self) -> u64 {
            // xorshift64*: good enough to vary text, and needs no crate.
            self.0 ^= self.0 >> 12;
            self.0 ^= self.0 << 25;
            self.0 ^= self.0 >> 27;
            self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
        }

        /// A number from `low` up to `high`, as likely to double as to halve,
        /// which is how message and output lengths spread.
        fn spread(&mut self, low: usize, high: usize) -> usize {
            let unit = (self.next() >> 11) as f64 / (1u64 << 53) as f64;
            (low as f64 * (high as f64 / low as f64).powf(unit)) as usize
        }

        /// `n` words of `vocab`, the first words far more often than the
        /// last, as in any text.
        fn words(&mut self, vocab: &[String], n: usize, between: &str) -> String {
            let picked: Vec<&str> = (0..n)
                .map(|_| vocab[self.spread(1, vocab.len() + 1) - 1].as_str())
                .collect();
            picked.join(between)
        }
    }

    /// The words the bench's sessions are written in: the everyday ones a
    /// coding session is full of, then thousands of random letters, which
    /// hold more runs of three than real text does and so make the term sets
    /// no smaller, and pass no fewer sessions, than real sessions would.
    #[cfg(target_os = "linux")]
    fn vocab() -> Vec<String> {
        let common = "the a to is in file error test build run fn let self return use \
                      struct impl value string path if else match none some ok err result \
                      config update check we it that this for with not be on";
        let mut dice = Dice(0xC0DE);
        let made = (0..4000).map(|_| {
            let length = dice.spread(3, 11);
            (0..length)
                .map(|_| char::from(b'a' + (dice.next() % 26) as u8))
                .collect::<String>()
        });
        let made: Vec<String> = made.collect();
        common
            .split_whitespace()
            .map(String::from)
            .chain(made)
            .collect()
    }

    /// Session `id` as a coding session reads: the user asks, the model calls
    /// tools and reads their output, and answers, over and over; a few to a
    /// few hundred messages, with outputs up to the most a tool returns.
    #[cfg(target_os = "linux")]
    fn made_up(id: u64, dice: &mut Dice, vocab: &[String]) -> Conversation {
        let length = dice.spread(4, 300);
        let mut messages = Vec::new();
        while messages.len() < length {
            let n = dice.spread(5, 80);
            messages.push(said("user", &dice.words(vocab, n, " ")));
            for _ in 0..dice.spread(1, 8) {
                let call = ToolCall {
                    id: format!("call_{}", dice.next()),
                    name: "read_file".into(),
                    arguments: format!("{{\"path\":\"src/{}.rs\"}}", dice.words(vocab, 1, "")),
                };
                let n = dice.spread(5, 150);
                messages.push(Message {
                    role: "assistant".into(),
                    content: dice.words(vocab, n, " "),
                    model: "some-model".into(),
                    tokens: dice.next() % 2000,
                    tool_calls: vec![call.clone()],
                    ..Default::default()
                });
                let n = dice.spread(40, 2400);
                messages.push(Message {
                    role: "tool".into(),
                    content: dice.words(vocab, n, if n.is_multiple_of(3) { "\n" } else { " " }),
                    tool_call_id: call.id,
                    name: call.name,
                    args: call.arguments,
                    ..Default::default()
                });
            }
            let n = dice.spread(20, 300);
            messages.push(said("assistant", &dice.words(vocab, n, " ")));
        }
        // One session in 500 mentions the rare word, once.
        if id % 500 == 7 {
            messages[0].content.push_str(" flamingo");
        }
        Conversation {
            id,
            title: dice.words(vocab, 4, " "),
            updated: 1_700_000_000 + id * 600,
            messages,
            ..Default::default()
        }
    }

    /// Take the sessions' files out of the page cache, so the next search
    /// reads them from the disk as the first one after a restart does.
    #[cfg(target_os = "linux")]
    fn evict(dir: &std::path::Path) {
        use std::os::fd::AsRawFd;
        for entry in std::fs::read_dir(dir).unwrap().flatten() {
            let Ok(file) = std::fs::File::open(entry.path()) else {
                continue;
            };
            // Dirty pages are not dropped, so they are written first.
            let _ = file.sync_all();
            unsafe { libc::posix_fadvise(file.as_raw_fd(), 0, 0, libc::POSIX_FADV_DONTNEED) };
        }
    }

    /// A figure of the process's memory from /proc, in kilobytes.
    #[cfg(target_os = "linux")]
    fn memory(field: &str) -> u64 {
        let status = std::fs::read_to_string("/proc/self/status").unwrap_or_default();
        let kb = status
            .lines()
            .find_map(|line| line.strip_prefix(field)?.trim().strip_suffix("kB"))
            .and_then(|kb| kb.trim().parse::<u64>().ok());
        kb.unwrap_or(0)
    }

    /// One search of `dir` for `query`: how long it took, its hits, and the
    /// memory it took beyond what the process held before, in kilobytes.
    #[cfg(target_os = "linux")]
    fn timed(
        dir: &std::path::Path,
        query: &str,
        prefilter: bool,
    ) -> (std::time::Duration, Vec<(u64, Option<usize>)>, u64) {
        // Resets the peak to what is held now.
        let _ = std::fs::write("/proc/self/clear_refs", "5");
        let before = memory("VmRSS:");
        let started = std::time::Instant::now();
        let hits = search_through(dir, query, &Within::default(), 10, prefilter);
        let took = started.elapsed();
        let hits = hits.into_iter().map(|h| (h.id, h.ordinal)).collect();
        (took, hits, memory("VmHWM:").saturating_sub(before))
    }

    /// The count and total size of the files in `dir` named `*.{extension}`.
    #[cfg(target_os = "linux")]
    fn files(dir: &std::path::Path, extension: &str) -> (u64, u64) {
        std::fs::read_dir(dir)
            .unwrap()
            .flatten()
            .filter(|e| e.path().extension().is_some_and(|x| x == extension))
            .filter_map(|e| e.metadata().ok())
            .fold((0, 0), |(n, bytes), m| (n + 1, bytes + m.len()))
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "bench: cargo test -p letronna-core --release -- --ignored session_search_at_scale --nocapture"]
    fn session_search_at_scale() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("bench");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let sessions = dir.join("sessions");
        let (vocab, mut dice) = (vocab(), Dice(0x5EED));
        let ms = |took: std::time::Duration| took.as_secs_f64() * 1000.0;
        let mut saved = 0u64;
        for count in [100u64, 1_000, 5_000] {
            // Saved as the app saves them, a batch at a time so the made-up
            // store is never all in memory at once.
            while saved < count {
                let mut store = Store::default();
                let batch = (count - saved).min(100);
                store.conversations = (saved + 1..=saved + batch)
                    .map(|id| made_up(id, &mut dice, &vocab))
                    .collect();
                store.save().unwrap();
                saved += batch;
            }
            let (_, bytes) = files(&sessions, "jsonl");
            let (sets, set_bytes) = files(&sessions, "terms");
            println!(
                "\n{count} sessions, {:.0} MB; term sets {:.1} KB each, {:.1} MB in all",
                bytes as f64 / 1e6,
                set_bytes as f64 / 1e3 / sets.max(1) as f64,
                set_bytes as f64 / 1e6,
            );
            println!("  term     prefilter  cold ms  warm ms  hits  peak KB");
            for (name, query) in [
                ("rare", "flamingo"),
                ("common", "error"),
                ("none", "quetzalcoatl"),
            ] {
                let mut found = Vec::new();
                for prefilter in [false, true] {
                    evict(&sessions);
                    let (cold, hits, peak) = timed(&sessions, query, prefilter);
                    // The best of three, once the files are in memory.
                    let warm = (0..3)
                        .map(|_| timed(&sessions, query, prefilter).0)
                        .min()
                        .unwrap();
                    println!(
                        "  {name:<8} {:<9} {:>8.1} {:>8.1} {:>5} {peak:>8}",
                        if prefilter { "with" } else { "without" },
                        ms(cold),
                        ms(warm),
                        hits.len(),
                    );
                    found.push(hits);
                }
                assert_eq!(found[0], found[1], "{name}");
                if name == "rare" {
                    assert_eq!(found[0].len() as u64, (count + 492) / 500);
                }
            }
            // The first search after an update to a build that keeps sets:
            // none yet, so it reads every file and makes each one's set.
            for entry in std::fs::read_dir(&sessions).unwrap().flatten() {
                if entry.path().extension().is_some_and(|x| x == "terms") {
                    std::fs::remove_file(entry.path()).unwrap();
                }
            }
            evict(&sessions);
            let (first, _, _) = timed(&sessions, "quetzalcoatl", true);
            evict(&sessions);
            let (next, _, _) = timed(&sessions, "quetzalcoatl", true);
            println!(
                "  sets made by the first search: {:.1} ms cold, then {:.1} ms cold",
                ms(first),
                ms(next)
            );
        }
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tool_calls_count_each_call_once_and_only_since_asked() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("tool-calls");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        std::fs::create_dir_all(dir.join("agents").join("1")).unwrap();
        let calls = |at: u64, calls: &[(&str, &str)]| {
            let calls: Vec<_> = calls
                .iter()
                .map(|(id, name)| serde_json::json!({"id": id, "name": name, "arguments": "{}"}))
                .collect();
            // Written as the store writes it, the role straight after the tag,
            // which is what the reader looks for before parsing a line.
            format!(
                r#"{{"type":"message","role":"assistant","content":"","at":{at},"tool_calls":{}}}"#,
                serde_json::Value::Array(calls)
            )
        };
        let old = calls(100, &[("a", "read_file")]);
        let new = calls(500, &[("b", "read_file"), ("c", "web_fetch")]);
        // A provider that gives no ids has every reply call its first `call_0`.
        let bare = calls(800, &[("call_0", "list_dir")]);
        let bare_later = calls(900, &[("call_0", "list_dir")]);
        let session = [
            r#"{"type":"head","id":1,"created":100}"#,
            &old,
            &new,
            &bare,
            &bare_later,
        ];
        std::fs::write(dir.join("sessions").join("1.jsonl"), session.join("\n")).unwrap();
        // A branch copied the newer reply, and a helper made a call of its own.
        let branch = [r#"{"type":"head","id":2,"created":600}"#, &new];
        std::fs::write(dir.join("sessions").join("2.jsonl"), branch.join("\n")).unwrap();
        let helper = calls(700, &[("d", "run_command")]);
        std::fs::write(dir.join("agents").join("1").join("1.jsonl"), helper).unwrap();

        let counted = super::tool_calls(300);
        let counted: Vec<(&str, usize)> = counted.iter().map(|(k, v)| (k.as_str(), *v)).collect();
        assert_eq!(
            counted,
            [
                ("list_dir", 2),
                ("read_file", 1),
                ("run_command", 1),
                ("web_fetch", 1)
            ]
        );
        assert_eq!(super::tool_calls(0)["read_file"], 2);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rewind_leaves_none_of_what_it_took_back_in_the_file() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-cut");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let path = dir.join("sessions/3.jsonl");
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        // An older build's rewind record, with what it took back still there.
        let mut text = session_text("cut", 2);
        text.push_str("{\"type\":\"message\",\"role\":\"user\",\"content\":\"secret one\"}\n");
        text.push_str("{\"type\":\"rewind\",\"to\":2}\n");
        std::fs::write(&path, text).unwrap();

        let mut conversation = super::session(3).unwrap();
        conversation.messages.push(said("user", "asked"));
        conversation.messages.push(Message {
            tokens: 7,
            prompt_tokens: 11,
            at: 9,
            ..said("assistant", "secret two")
        });
        conversation.flush().unwrap();
        conversation.rewind(2);
        conversation.flush().unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("secret") && !text.contains("asked"));
        // Nor in the terms a search looks at before it reads the file.
        let stat = std::fs::metadata(&path).unwrap();
        let stamp = (stat.len(), super::modified(&stat));
        let terms = crate::prefilter::read(&path, stamp).unwrap();
        assert!(!terms.may_hold("secret two"));
        assert!(!text.contains("rewind"));
        assert_eq!(replay(&path).unwrap().messages.len(), 2);
        // The reply taken back was paid for, so the costs still count it.
        let spent = super::spent(0, 3);
        assert_eq!(spent.len(), 1);
        assert_eq!((spent[0].usage.output, spent[0].at), (7, 9));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_index_reads_a_session_again_after_a_rewind_shrinks_it() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewind-index");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };

        let mut store = Store::default();
        store.conversations.push(numbered(6));
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 6);
        store.conversations[0].rewind(2);
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 2);
        let size = std::fs::metadata(dir.join("sessions/1.jsonl"))
            .unwrap()
            .len();
        let index = super::read_index(&super::index_path());
        assert_eq!((index[&1].size, index[&1].len), (size, size));

        // Grown past where the index stood before the cut, it is still read
        // in full rather than from that old place.
        let long = "y".repeat(2 * super::TAIL_CHECK as usize);
        store.conversations[0].messages.push(said("user", &long));
        store.save().unwrap();
        assert_eq!(summaries()[0].messages, 3);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    /// Session `id` saved `days` ago by both its meta and its file's time.
    fn saved_days_ago(store: &mut Store, id: u64, days: u64, now: u64) {
        let updated = now - days * super::DAY;
        store.conversations.push(Conversation {
            id,
            updated,
            ..numbered(1)
        });
        store.save().unwrap();
        let path = super::session_path(id);
        let file = std::fs::File::options().write(true).open(path).unwrap();
        let at = std::time::UNIX_EPOCH + std::time::Duration::from_secs(updated);
        file.set_modified(at).unwrap();
    }

    #[test]
    fn old_sessions_are_deleted_with_what_is_kept_beside_them() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("prune");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let now = crate::chat::now();

        let mut store = Store::default();
        saved_days_ago(&mut store, 1, 40, now);
        saved_days_ago(&mut store, 2, 5, now);
        saved_days_ago(&mut store, 3, 40, now);
        saved_days_ago(&mut store, 4, 40, now);
        saved_days_ago(&mut store, 5, 40, now);
        let path = super::session_path(3);
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now()).unwrap();
        // Pinned after its file was made old: the pin is a meta line.
        store.conversations[3].pinned = true;
        store.save().unwrap();
        let file = std::fs::File::options()
            .write(true)
            .open(super::session_path(4))
            .unwrap();
        file.set_modified(std::time::UNIX_EPOCH).unwrap();
        let sidecars = [
            super::helpers_dir(1),
            crate::tools::results_dir(1),
            crate::history::folder(1),
        ];
        for folder in &sidecars {
            std::fs::create_dir_all(folder).unwrap();
            std::fs::write(folder.join("1.jsonl"), "kept for 1").unwrap();
        }
        let tasklist = dir.join("tasklists/1.jsonl");
        std::fs::create_dir_all(dir.join("tasklists")).unwrap();
        std::fs::write(&tasklist, "").unwrap();

        // Session 1 is old, 2 is recent, 3 was written to lately, 4 is
        // pinned and 5 is the one open.
        let deleted = super::prune(30, now, &[5]);
        assert_eq!(deleted, [1]);
        assert!(!super::session_path(1).exists());
        assert!(!super::session_path(1).with_extension("lock").exists());
        assert!(sidecars.iter().all(|folder| !folder.exists()));
        assert!(!tasklist.exists());
        let mut left: Vec<u64> = summaries().iter().map(|s| s.id).collect();
        left.sort();
        assert_eq!(left, [2, 3, 4, 5]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_another_process_is_writing_is_not_deleted() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("prune-locked");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let now = crate::chat::now();

        let mut store = Store::default();
        saved_days_ago(&mut store, 1, 40, now);
        saved_days_ago(&mut store, 2, 5, now);
        let path = super::session_path(1);
        let held = Lock::take(&path).unwrap();
        assert!(super::prune(30, now, &[]).is_empty());
        assert!(path.exists() && path.with_extension("lock").exists());
        drop(held);
        assert_eq!(super::prune(30, now, &[]), [1]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn old_sessions_are_kept_unless_asked_and_looked_for_once_a_day() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("prune-off");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let now = crate::chat::now();
        assert_eq!(super::Settings::default().keep_days, 0);

        let mut store = Store::default();
        saved_days_ago(&mut store, 1, 4000, now);
        saved_days_ago(&mut store, 2, 5, now);
        super::prune_old(&[]);
        assert!(super::session_path(1).exists());
        assert!(!super::pruned_path().exists());

        std::fs::write(
            super::settings_path(),
            "{\"endpoint\":\"\",\"model\":\"\",\"keep_days\":30}",
        )
        .unwrap();
        // Looked for less than a day ago, so not again yet.
        std::fs::write(super::pruned_path(), "").unwrap();
        super::prune_old(&[]);
        assert!(super::session_path(1).exists());
        std::fs::remove_file(super::pruned_path()).unwrap();
        super::prune_old(&[]);
        assert!(!super::session_path(1).exists());
        assert!(super::pruned_path().exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rewind_in_the_window_leaves_it_following_the_file() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-own-rewind");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, _) = window_and_terminal(4);
        let path = dir.join("sessions/1.jsonl");

        window.conversations[0].rewind(2);
        window.save().unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("\"content\":\"3\""));
        // Marked again at the rewritten end, so nothing is read back.
        assert!(!window.follow(|_| false));

        let mut terminal = load_listing();
        fill(&mut terminal.conversations[0]);
        terminal.conversations[0]
            .messages
            .push(said("user", "after"));
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(contents(&window.conversations[0]), ["0", "1", "after"]);

        window.conversations[0]
            .messages
            .push(said("assistant", "and on"));
        window.save().unwrap();
        let open = &window.conversations[0];
        assert!(!open.gone && !open.diverged);
        assert_eq!(
            contents(&replay(&path).unwrap()),
            ["0", "1", "after", "and on"]
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rewind_in_the_terminal_reaches_the_window_in_full() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-their-rewind");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(4);
        let path = dir.join("sessions/1.jsonl");

        terminal.conversations[0].rewind(2);
        terminal.conversations[0]
            .messages
            .push(said("user", "instead"));
        terminal.save().unwrap();
        assert!(window.follow(|_| false));
        assert_eq!(contents(&window.conversations[0]), ["0", "1", "instead"]);

        // Read in again, it writes on into the rewritten file.
        window.conversations[0]
            .messages
            .push(said("assistant", "reply"));
        window.save().unwrap();
        let open = &window.conversations[0];
        assert!(!open.gone && !open.diverged);
        assert_eq!(
            contents(&replay(&path).unwrap()),
            ["0", "1", "instead", "reply"]
        );
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains("\"content\":\"3\""));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rename_in_the_window_after_a_rewind_there_keeps_both() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-rename-rewound");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(3);
        let path = dir.join("sessions/1.jsonl");

        terminal.conversations[0].rewind(1);
        terminal.save().unwrap();
        window.conversations[0].title = "renamed here".into();
        window.save().unwrap();
        let open = &window.conversations[0];
        assert!(!open.gone && !open.diverged);
        let back = replay(&path).unwrap();
        assert_eq!(back.title, "renamed here");
        assert_eq!(contents(&back), ["0"]);

        // The file the terminal rewrote is then taken in whole.
        assert!(window.follow(|_| false));
        let open = &window.conversations[0];
        assert_eq!(open.title, "renamed here");
        assert_eq!(contents(open), ["0"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_session_the_terminal_rewound_to_the_start_is_still_the_window_s() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-rewound-empty");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, mut terminal) = window_and_terminal(2);
        let path = dir.join("sessions/1.jsonl");

        terminal.conversations[0].rewind(0);
        terminal.save().unwrap();
        window.conversations[0].messages.push(said("user", "more"));
        window.save().unwrap();
        let open = &window.conversations[0];
        assert!(!open.gone && open.diverged);
        assert_eq!(contents(&replay(&path).unwrap()), ["0", "1", "more"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rewrite_that_leaves_the_file_as_long_is_still_noticed() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("follow-same-size");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let (mut window, _) = window_and_terminal(2);
        let path = dir.join("sessions/1.jsonl");

        let text = std::fs::read_to_string(&path).unwrap();
        let changed = text.replace("\"content\":\"1\"", "\"content\":\"x\"");
        assert_eq!(changed.len(), text.len());
        std::fs::write(&path, changed).unwrap();
        let file = std::fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1))
            .unwrap();
        drop(file);

        window.conversations[0].messages.push(said("user", "more"));
        window.save().unwrap();
        assert!(window.conversations[0].diverged);
        assert_eq!(contents(&replay(&path).unwrap()), ["0", "1", "more"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn old_sessions_wait_while_another_letronna_is_open() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("prune-running");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let now = crate::chat::now();
        let mut store = Store::default();
        saved_days_ago(&mut store, 1, 40, now);
        saved_days_ago(&mut store, 2, 5, now);
        std::fs::write(
            super::settings_path(),
            "{\"endpoint\":\"\",\"model\":\"\",\"keep_days\":30}",
        )
        .unwrap();

        // Another Letronna, running, holds the file as its start left it.
        let running = || {
            std::fs::File::options()
                .write(true)
                .create(true)
                .truncate(false)
                .open(super::running_path())
                .unwrap()
        };
        let other = running();
        other.lock_shared().unwrap();
        super::prune_old(&[]);
        assert!(super::session_path(1).exists());
        // Not marked, so the next start looks again.
        assert!(!super::pruned_path().exists());

        drop(other);
        super::prune_old(&[]);
        assert!(!super::session_path(1).exists());
        assert!(super::session_path(2).exists());
        // And this one now holds it, so no other start prunes meanwhile.
        assert!(running().try_lock().is_err());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_newest_session_and_one_asked_for_are_kept_however_old() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("prune-newest");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let now = crate::chat::now();
        let mut store = Store::default();
        saved_days_ago(&mut store, 1, 50, now);
        saved_days_ago(&mut store, 2, 45, now);
        saved_days_ago(&mut store, 3, 40, now);

        // 1 is the one `--resume 1` opens, 3 the one `-c` goes on with.
        assert_eq!(super::prune(30, now, &[1]), [2]);
        assert!(super::session_path(1).exists() && super::session_path(3).exists());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_rewrite_leaves_only_the_session_its_lock_and_terms_in_the_folder() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = scratch("rewrite-temp");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        let mut store = Store::default();
        store.conversations.push(numbered(4));
        store.save().unwrap();
        store.conversations[0].rewind(1);
        store.save().unwrap();

        assert_eq!(replay(&super::session_path(1)).unwrap().messages.len(), 1);
        let mut names: Vec<String> = std::fs::read_dir(dir.join("sessions"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "index.json")
            .collect();
        names.sort();
        assert_eq!(names, ["1.jsonl", "1.lock", "1.terms"]);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_number_in_the_environment_stands_in_for_a_saved_size() {
        let saved =
            br#"{"endpoint": "", "model": "", "context_window": 65536, "max_output": 16384}"#;
        let saved = super::read_settings(saved).unwrap();
        assert_eq!((saved.context_window, saved.max_output), (65_536, 16_384));
        // A name no other test reads, so setting it races nothing.
        let name = "LETRONNA_TEST_SIZE";
        assert_eq!(super::overridden(65_536, name), 65_536);
        unsafe { std::env::set_var(name, " 131072 ") };
        assert_eq!(super::overridden(65_536, name), 131_072);
        // A 0 unsets the saved size for the run, and what is no number is
        // passed over.
        unsafe { std::env::set_var(name, "0") };
        assert_eq!(super::overridden(65_536, name), 0);
        unsafe { std::env::set_var(name, "128k") };
        assert_eq!(super::overridden(65_536, name), 65_536);
        unsafe { std::env::remove_var(name) };
    }

    #[test]
    fn an_experiment_is_as_its_setting_says_unless_the_environment_says_otherwise() {
        let none = |_: &str| None;
        let default = super::experiments_of(&super::Settings::default(), none);
        assert_eq!(
            default,
            super::Experiments {
                helpers_share_tools: true,
                parallel_read_commands: false,
            }
        );
        let saved = br#"{"endpoint": "", "model": "", "helpers_share_tools": "Off",
            "parallel_read_commands": "on"}"#;
        let saved = super::read_settings(saved).unwrap();
        assert_eq!(
            super::experiments_of(&saved, none),
            super::Experiments {
                helpers_share_tools: false,
                parallel_read_commands: true,
            }
        );
        // For one run, the variable in place of the setting: an adapter
        // starts with no settings file to write one in.
        let env = |name: &str| match name {
            "LETRONNA_HELPERS_SHARE_TOOLS" => Some("on".to_owned()),
            "LETRONNA_PARALLEL_READ_COMMANDS" => Some("0".to_owned()),
            _ => None,
        };
        assert_eq!(super::experiments_of(&saved, env), default);
    }

    #[test]
    fn the_reasoning_replay_is_chunked_only_where_a_setting_says_so() {
        let saved = br#"{"endpoint": "", "model": "", "reasoning_replay": "chunked"}"#;
        assert_eq!(
            super::read_settings(saved).unwrap().reasoning_replay,
            "chunked"
        );
        assert_eq!(super::Settings::default().reasoning_replay, "");
        // A name no other test reads, so setting it races nothing.
        let name = "LETRONNA_TEST_REPLAY";
        assert!(!super::chunks_reasoning("", name));
        assert!(!super::chunks_reasoning("full", name));
        assert!(super::chunks_reasoning(" Chunked ", name));
        // The environment stands in for the saved word, either way.
        unsafe { std::env::set_var(name, "chunked") };
        assert!(super::chunks_reasoning("", name));
        unsafe { std::env::set_var(name, "full") };
        assert!(!super::chunks_reasoning("chunked", name));
        unsafe { std::env::remove_var(name) };
    }
}
