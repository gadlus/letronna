//! Tools the model may call. Each one is a `Tool`; the registry below is what
//! gets advertised to the provider and dispatched on `tool_calls`.
//!
//! Read-only tools run without asking. Tools that change the workspace or
//! run a command return `needs_approval`, and the chat shows a card first.
//! `ask_user` is answered by the user, and only runs here when nobody is there
//! to answer it.

use crate::budget::Budget;
use crate::cancel::Cancel;
use crate::chat::{Connection, Remote};
use crate::driver::AgentId;
use crate::grep::{SEARCH_FILES, SEARCH_TIME};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Where a session is allowed to look.
#[derive(Clone, Default)]
pub struct ToolContext {
    /// The folder attached to the session. Relative paths resolve here.
    pub workspace: Option<PathBuf>,
    /// Set when a chat app is driving the session: where `read_chat` reads,
    /// and whether other people can see the replies.
    pub remote: Option<Remote>,
    /// The conversation the call is made in, which keeps a browser of its
    /// own and hears when its jobs exit; 0 for none in particular.
    pub conversation: u64,
    /// Where the file tools note what they write, for a rewind to put back.
    pub files: crate::history::Log,
    /// Stops this call and anything it started.
    pub cancel: Cancel,
    /// Plan mode, in which a tool that changes things is refused when named.
    pub plan: bool,
    /// The user said yes to this very call on its card, which lets a read
    /// the gate asked about reach a file of keys.
    pub approved: bool,
    /// Who is calling, for tools that start other agents. Crate-private, so
    /// a plugin's tool never sees the connection's key.
    pub(crate) caller: Caller,
}

/// Who is making a call, as a tool that starts another agent needs to know.
#[derive(Clone, Default)]
pub(crate) struct Caller {
    /// What the calling agent talks to the model on, which a helper's is made from.
    pub connection: Option<Arc<Connection>>,
    /// The helper making the call, or None for a session's own agent.
    pub agent: Option<AgentId>,
    /// The session the work is for.
    pub root: u64,
    /// How many helpers deep the calling agent is: 0 for a session's own.
    pub depth: u8,
    /// What the calling helper's tree spends from, or None outside a helper.
    pub budget: Option<Arc<Budget>>,
    /// The tree of helpers the call is made in, which an approval for the run covers.
    pub run: Option<u64>,
}

impl ToolContext {
    pub fn of(connection: &Connection) -> Self {
        Self {
            workspace: connection.workspace.clone(),
            remote: connection.remote.clone(),
            conversation: connection.conversation,
            files: crate::history::Log::default(),
            cancel: Cancel::default(),
            plan: connection.plan,
            approved: false,
            caller: Caller {
                connection: Some(Arc::new(connection.clone())),
                root: connection.conversation,
                ..Caller::default()
            },
        }
    }

    /// Other people can read the replies, so nothing on this computer is in
    /// reach: what a tool finds would be posted to all of them.
    pub(crate) fn shared(&self) -> bool {
        self.remote.as_ref().is_some_and(|r| r.shared)
    }
}

pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn description(&self) -> &'static str;
    /// JSON schema for the arguments object.
    fn parameters(&self) -> Value;
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String>;
    /// Whether the user must approve each call before it runs.
    fn needs_approval(&self) -> bool {
        false
    }
    /// Whether the tool changes anything outside Letronna. Plan mode hides
    /// these so the model can look and propose but not act.
    fn mutates(&self) -> bool {
        false
    }
    /// Whether the tool reads or changes this computer. Withheld where other
    /// people can read the replies; a tool has to opt out to be offered there,
    /// so a plugin's tool stays out unless it says otherwise.
    fn machine(&self) -> bool {
        true
    }
    /// Only offered when the session has a chat history to read.
    fn needs_history(&self) -> bool {
        false
    }
    /// Whether to offer the tool in this session; a call is re-checked with
    /// only the workspace, chat, plan mode and session id it carries.
    fn offered(&self, _: &Connection) -> bool {
        true
    }

    /// Whether it may run at the same time as other such calls from one reply,
    /// off unless a tool opts in so one that writes, waits or drives the
    /// browser never runs beside another.
    fn concurrent(&self) -> bool {
        false
    }

    /// Whether what it returns is words someone other than the user wrote,
    /// which reach the model fenced and without invisible characters. Its
    /// errors are Letronna's: one quoting a page or a server fences the quote
    /// itself, with `untrusted::quoted`.
    fn untrusted(&self) -> bool {
        false
    }

    /// Whether the output comes from outside the user's files -- a command,
    /// a page, a server -- so words shaped like any vendor's key are hidden
    /// in it too, not only the keys Letronna holds.
    fn hides_key_shapes(&self) -> bool {
        false
    }
    /// What the approval card shows: a diff, the file body, the command line.
    fn preview(&self, args: &Value, ctx: &ToolContext) -> String {
        let _ = ctx;
        args.to_string()
    }
    /// What the auto-mode judge is shown of a call: what decides whether it
    /// is safe, whole, and nothing a page or file wrote.
    fn judge_input(&self, args: &Value, ctx: &ToolContext) -> String {
        self.preview(args, ctx)
    }
}

pub const ASK_USER: &str = "ask_user";
pub const TODO: &str = "todo";
pub const EXIT_PLAN: &str = "exit_plan";
/// The tool result the model gets when the user denies a call without saying why.
pub const DECLINED: &str = "The user declined this action. Ask before trying a different approach.";
/// The most of a denial's reason that reaches the model, in characters:
/// about a paragraph, which says why; anything longer is a paste that
/// belongs in a message of its own.
pub const REASON_CHARS: usize = 1000;
/// How a denial that came with a reason opens, before the reason itself.
const DECLINED_BECAUSE: &str = "The user declined: ";
/// The tool result the model gets when the user approves its plan.
pub const PLAN_APPROVED: &str = "The user approved this plan. Plan mode is off; carry it out now.";
/// The tool result the model gets when the user wants the plan changed.
pub const KEEP_PLANNING: &str =
    "Keep planning: the user wants changes. Ask what to change, or refine the plan.";
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// How much of a file one read shows, in bytes, which is never more
/// characters: under the chat's cap on a tool result with room for the
/// footer, so a page reaches the model whole instead of losing its middle.
const PAGE_BYTES: usize = crate::chat::MAX_TOOL_CHARS - 1_000;
/// A line of a file longer than this, in characters, is cut, so a minified
/// file on one line still leaves room in a page for the lines around it.
const LINE_CHARS: usize = 1_000;
/// The footer counts the lines after a page only this far: at disk speed that
/// is well under a second, where counting a log of many gigabytes would hold
/// the session up.
const COUNT_BYTES: u64 = 64 * 1024 * 1024;
/// A UTF-16 file is decoded whole, its line breaks being two bytes wide, so
/// only this much of one is read. Such files are exports and redirected shell
/// output, far smaller than this.
const UTF16_BYTES: u64 = 16 * 1024 * 1024;
/// How long a background command gets to print something before the tool
/// replies, so the first message can carry a port or an early crash.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(1200);
/// How long a command's output pipes get to close after its shell exits. A
/// child it left running, like `sleep 100 &` or a daemon, holds them open
/// for as long as it lives; 300 ms is plenty for output still in flight and
/// too short to feel.
const PIPE_GRACE: Duration = Duration::from_millis(300);

/// A tool's reply is cut to this many bytes, so no more characters: the cap
/// the chat puts on a tool result in a request, which then carries the cut
/// whole instead of trimming its middle again, marker line and all. A quarter
/// goes to the start and the rest to the end, because a build, a test run or
/// an install ends with its errors, its summary and the exit code.
const MAX_OUTPUT: usize = crate::chat::MAX_TOOL_CHARS;
/// Bytes kept free for the line that says what was left out, which with the
/// largest counts a `usize` can hold is still shorter than this, apart from
/// the name of the file the whole output was saved to.
const MARKER_ROOM: usize = 100;
/// A saved output keeps its first this many bytes: some 600 pages of
/// `read_file`, past anything a model reads through, while a command that
/// prints without end costs the disk no more than this for the call.
const SPILL_BYTES: usize = 4 * 1024 * 1024;
/// A command's output to one pipe is kept this far from its start, and
/// `KEEP_TAIL` from its end, with only a count of the bytes and lines in
/// between: a quarter and three quarters of 4 MiB, the split of the cut to
/// `MAX_OUTPUT`, which then always has whole lines at both ends, while a
/// command printing gigabytes keeps no more than 8 MiB for its two pipes.
const KEEP_HEAD: usize = 1024 * 1024;
const KEEP_TAIL: usize = 3 * 1024 * 1024;

/// Tools contributed by enabled plugins. Kept here rather than read from the
/// plugin registry because tools run on worker threads with no `App` to hand.
fn plugin_tools() -> &'static Mutex<Vec<Arc<dyn Tool>>> {
    static EXTRA: OnceLock<Mutex<Vec<Arc<dyn Tool>>>> = OnceLock::new();
    EXTRA.get_or_init(Default::default)
}

/// Replace the plugin-contributed tools. The registry calls this at startup and
/// whenever a plugin is switched on or off.
pub fn set_plugin_tools(tools: Vec<Arc<dyn Tool>>) {
    if let Ok(mut slot) = plugin_tools().lock() {
        *slot = tools;
    }
}

pub fn all() -> Vec<Arc<dyn Tool>> {
    let mut tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(ReadFile),
        Arc::new(ListDir),
        Arc::new(Search),
        Arc::new(WriteFile),
        Arc::new(EditFile),
        Arc::new(RunCommand),
        Arc::new(ReadJob),
        Arc::new(StopJob),
        Arc::new(Wait),
        Arc::new(crate::wake::WakeMe),
        Arc::new(AskUser),
        Arc::new(ExitPlan),
        Arc::new(Todo),
        Arc::new(crate::messaging::ReadChat),
        Arc::new(Sessions),
        Arc::new(crate::memory::Remember),
        Arc::new(crate::skills::UseSkill),
        Arc::new(SearchSessions),
        Arc::new(crate::web::WebFetch),
        Arc::new(crate::media::MakeImage),
        Arc::new(crate::media::Speak),
        Arc::new(crate::media::Transcribe),
        Arc::new(crate::media::MakeVideo),
        Arc::new(crate::browser::Browse),
        Arc::new(crate::browser::UsePage),
        Arc::new(crate::agents::Agent),
        Arc::new(crate::agents::Agents),
    ];
    // The user's own search service, once one is set up.
    if crate::search::ready() {
        tools.push(Arc::new(crate::search::SearchWeb));
    }
    if let Ok(extra) = plugin_tools().lock() {
        for tool in extra.iter() {
            // A plugin cannot take over the name of a built-in tool.
            if !tools.iter().any(|t| t.name() == tool.name()) {
                tools.push(tool.clone());
            }
        }
    }
    // MCP servers' tools, once they have answered. Their names all start
    // with `mcp__`, so they cannot shadow one of the above either.
    tools.extend(crate::mcp::tools());
    tools
}

pub fn find(name: &str) -> Option<Arc<dyn Tool>> {
    all().into_iter().find(|t| t.name() == name)
}

pub fn parse_args(args: &str) -> Result<Value, String> {
    if args.trim().is_empty() {
        return Ok(json!({}));
    }
    serde_json::from_str(args).map_err(|e| {
        if e.is_eof() {
            "The arguments were cut off before the call was complete. Send less \
             content in one call: write the file in smaller pieces, or write a \
             short version first and append to it."
                .to_owned()
        } else {
            format!("Invalid arguments: {e}")
        }
    })
}

/// The file tools, which other agents' models all call with a file path.
pub(crate) const FILE_TOOLS: &[&str] = &["read_file", "write_file", "edit_file"];

/// Parameter names other agents use, as (theirs, ours, the tools it applies
/// to), since a model used to one of them reaches for its names first.
const ALIASES: &[(&str, &str, &[&str])] = &[
    ("file_path", "path", FILE_TOOLS),
    ("filepath", "path", FILE_TOOLS),
    ("absolute_path", "path", FILE_TOOLS),
    ("file", "path", FILE_TOOLS),
    ("file_text", "content", &["write_file"]),
    ("file_content", "content", &["write_file"]),
    ("contents", "content", &["write_file"]),
    ("old_string", "old", &["edit_file"]),
    ("old_str", "old", &["edit_file"]),
    ("new_string", "new", &["edit_file"]),
    ("new_str", "new", &["edit_file"]),
    ("dir_path", "path", &["list_dir", "search"]),
    ("directory", "path", &["list_dir", "search"]),
    ("cmd", "command", &["run_command"]),
    ("run_in_background", "background", &["run_command"]),
    ("is_background", "background", &["run_command"]),
    ("job_id", "id", &["read_job", "stop_job"]),
    ("bash_id", "id", &["read_job"]),
    ("shell_id", "id", &["stop_job"]),
    ("job_id", "job", &["wait"]),
];

/// Renames another agent's name for a parameter of tool `name` to Letronna's,
/// unless the call already has Letronna's; returns what was renamed.
pub fn rename_aliases(name: &str, args: &mut Value) -> Vec<String> {
    let mut changed = Vec::new();
    let Some(object) = args.as_object_mut() else {
        return changed;
    };
    for (theirs, ours, tools) in ALIASES {
        // A key already there, the call's own or an earlier row's, stands.
        if !tools.contains(&name) || object.contains_key(*ours) {
            continue;
        }
        let Some(value) = object.remove(*theirs) else {
            continue;
        };
        object.insert((*ours).to_owned(), value);
        changed.push(format!("`{theirs}` as `{ours}`"));
    }
    changed
}

/// Parses a top-level string the schema wants as another type, such as
/// `"true"` for a boolean or `"[1, 2]"` for an array, and returns what it
/// changed.
fn coerce(schema: &Value, args: &mut Value) -> Vec<String> {
    let mut changed = Vec::new();
    let (Some(properties), Some(object)) = (schema["properties"].as_object(), args.as_object_mut())
    else {
        return changed;
    };
    for (key, value) in object.iter_mut() {
        let Some(kind) = properties.get(key).and_then(|p| p["type"].as_str()) else {
            continue;
        };
        let Value::String(text) = value else {
            continue;
        };
        let Ok(parsed) = serde_json::from_str::<Value>(text.trim()) else {
            continue;
        };
        let fits = match kind {
            "boolean" => parsed.is_boolean(),
            "integer" => parsed.is_i64() || parsed.is_u64(),
            "number" => parsed.is_number(),
            "array" => parsed.is_array(),
            "object" => parsed.is_object(),
            _ => false,
        };
        if fits {
            *value = parsed;
            let article = if matches!(kind, "integer" | "array" | "object") {
                "an"
            } else {
                "a"
            };
            changed.push(format!("`{key}` as {article} {kind}"));
        }
    }
    changed
}

/// Renames aliases, then coerces values to the tool's schema, and returns
/// what was reinterpreted.
fn repair(tool: &dyn Tool, args: &mut Value) -> Vec<String> {
    let mut changed = rename_aliases(tool.name(), args);
    changed.extend(coerce(&tool.parameters(), args));
    changed
}

/// Parses a call's arguments as tool `name` runs them, renamed and coerced,
/// for a frontend that reads them itself, such as the todo checklist.
pub fn parse_args_for(name: &str, args: &str) -> Result<Value, String> {
    let mut args = parse_args(args)?;
    if let Some(tool) = find(name) {
        repair(&*tool, &mut args);
    }
    Ok(args)
}

/// The note that ends a result whose arguments were reinterpreted, so the
/// model uses the right ones next time; empty when none were.
fn reinterpreted(changed: &[String]) -> String {
    if changed.is_empty() {
        return String::new();
    }
    format!(
        "\n\n[Read {}. Use the tool's own parameter names and types.]",
        changed.join(", ")
    )
}

pub fn needs_approval(name: &str) -> bool {
    find(name).is_some_and(|t| t.needs_approval())
}

/// Whether a call to `name` may run beside the other such calls from its reply.
pub fn concurrent(name: &str) -> bool {
    find(name).is_some_and(|t| t.concurrent())
}

pub fn preview(name: &str, args: &str, ctx: &ToolContext) -> String {
    let shown = match (find(name), parse_args(args)) {
        (Some(t), Ok(mut v)) => {
            // The card shows what will run, under the names the tool reads.
            repair(&*t, &mut v);
            t.preview(&v, ctx)
        }
        _ => args.to_owned(),
    };
    // A card can go to a chat app as well as to the screen.
    crate::redact::scrub(&shown, false)
}

/// The plan an `exit_plan` call puts to the user, or why the call cannot be
/// put to them: arguments that were cut off or carry no plan.
pub fn plan_of(args: &str) -> Result<String, String> {
    let args = parse_args(args)?;
    match args["plan"].as_str() {
        Some(plan) if !plan.trim().is_empty() => Ok(plan.to_owned()),
        _ => Err("No plan came with the call: put the whole plan, in markdown, in `plan`.".into()),
    }
}

/// Whether `exit_plan` is offered: in plan mode, and only at the terminal or
/// the window, since a chat app's plan mode is its owner's to lift.
pub fn offers_plan(connection: &Connection) -> bool {
    connection.plan && connection.remote.is_none()
}

/// The `tools` array for a chat completion request. In plan mode the tools
/// that change things are left out entirely, so the model cannot try them,
/// but for `run_command`, which the gate lets run a command that only reads;
/// where other people read the replies, so is everything that touches this
/// computer; `read_chat` exists only where there is a chat to read, and
/// `exit_plan` only where there is a plan to hand over and someone to take it.
pub fn specs(connection: &Connection) -> Vec<Value> {
    let shared = connection.remote.as_ref().is_some_and(|r| r.shared);
    let history = connection
        .remote
        .as_ref()
        .is_some_and(|r| r.history.is_some());
    all()
        .iter()
        .filter(|t| !connection.plan || !t.mutates() || t.name() == "run_command")
        .filter(|t| t.name() != EXIT_PLAN || offers_plan(connection))
        .filter(|t| !shared || !t.machine())
        .filter(|t| history || !t.needs_history())
        .filter(|t| t.offered(connection))
        .map(|t| {
            json!({"type": "function", "function": {
                "name": t.name(),
                "description": t.description(),
                "parameters": t.parameters(),
            }})
        })
        .collect()
}

pub fn run(name: &str, args: &str, ctx: &ToolContext) -> Result<String, String> {
    let tool = find(name).ok_or_else(|| format!("Unknown tool `{name}`."))?;
    let started = Instant::now();
    let result = run_found(tool.as_ref(), args, ctx);
    crate::diag::event(
        "tool",
        &[
            ("name", name.into()),
            ("outcome", crate::diag::outcome(&result).into()),
            ("ms", crate::diag::millis(started)),
            ("bytes", result.as_ref().map_or(0, String::len).into()),
        ],
    );
    result
}

/// `run` for a tool already found: what every call goes through.
pub(crate) fn run_found(tool: &dyn Tool, args: &str, ctx: &ToolContext) -> Result<String, String> {
    // Only fitting tools are offered, but the model can name any of them;
    // refuse here too rather than trust the offer.
    let connection = Connection {
        workspace: ctx.workspace.clone(),
        remote: ctx.remote.clone(),
        conversation: ctx.conversation,
        plan: ctx.plan,
        ..Default::default()
    };
    if !tool.offered(&connection) {
        return Err(format!(
            "`{}` is not available in this session.",
            tool.name()
        ));
    }
    let mut args = parse_args(args)?;
    let note = reinterpreted(&repair(tool, &mut args));
    // The gate refuses these before any card, but a call can get here
    // without it; refuse again rather than trust the caller.
    if let Some(why) = crate::gate::refusal(tool, &args, ctx) {
        return Err(why);
    }
    // Hidden before the cut, which could split a key and leave half of it
    // showing, and before anything is wrapped around the result.
    let shapes = tool.hides_key_shapes();
    let ran = tool
        .run(&args, ctx)
        .map_err(|e| crate::redact::scrub(&e, shapes) + &note)?;
    let ran = crate::redact::scrub(&ran, shapes);
    // The note goes after the cut and outside any fence: it is Letronna's,
    // and never the part that gets lost. An untrusted tool's result is never
    // saved aside to page through: read back, it would come unfenced.
    let mut out = if tool.untrusted() {
        fenced(tool.name(), &args, &ran)?
    } else {
        fit(ran, ctx)
    };
    out.push_str(&note);
    Ok(out)
}

/// What an untrusted tool returned, fenced. A browse screenshot's first line
/// is Letronna's own, naming the file it saved, and stays above the fence,
/// where the window looks for it and nothing a page writes can reach.
fn fenced(name: &str, args: &Value, text: &str) -> Result<String, String> {
    let shot = name == crate::browser::BROWSE && args["action"] == "screenshot";
    match text.split_once('\n') {
        Some((own, rest)) if shot && crate::media::saved(own).is_some() => {
            let room = MAX_OUTPUT.saturating_sub(own.len() + 1);
            Ok(format!("{own}\n{}", crate::untrusted::fence(rest, room)?))
        }
        _ => crate::untrusted::fence(text, MAX_OUTPUT),
    }
}

/// `out` as it is when it fits; otherwise cut, with the whole of it saved
/// where the model can page through it and the cut saying where.
fn fit(out: String, ctx: &ToolContext) -> String {
    if out.len() <= MAX_OUTPUT {
        return out;
    }
    let note = match spill(&out, ctx) {
        Some(path) if out.len() > SPILL_BYTES => format!(
            "; its first {SPILL_BYTES} bytes are in {}, which read_file pages through",
            path.display()
        ),
        Some(path) => format!(
            "; all of it is in {}, which read_file pages through",
            path.display()
        ),
        None => String::new(),
    };
    keep_ends(out, MAX_OUTPUT, &note)
}

/// Where the outputs cut in `conversation` are saved whole: readable from
/// its own calls only, and removed with its session.
pub(crate) fn results_dir(conversation: u64) -> PathBuf {
    crate::chat::data_dir()
        .join("results")
        .join(conversation.to_string())
}

/// Save up to `SPILL_BYTES` of `text` for the model to read. Nothing is saved
/// outside a session, where nothing would ever remove it, or in a shared
/// chat, where `read_file` is out of reach.
fn spill(text: &str, ctx: &ToolContext) -> Option<PathBuf> {
    static CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    if ctx.conversation == 0 || ctx.shared() {
        return None;
    }
    let dir = results_dir(ctx.conversation);
    std::fs::create_dir_all(&dir).ok()?;
    // Not the model's call id: some servers repeat theirs every turn, and it
    // is no safe file name. Calls at the same moment differ in the count, and
    // the window and the terminal in the process.
    let millis = std::time::UNIX_EPOCH.elapsed().ok()?.as_millis();
    let call = CALLS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = dir.join(format!("{millis}-{}-{call}.txt", std::process::id()));
    let kept = &text[..text.floor_char_boundary(SPILL_BYTES)];
    if crate::store::write_private(&path, kept.as_bytes()).is_err() {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(path)
}

/// `text` as it is when it fits in `max` bytes; otherwise its first and last
/// whole lines, with a line between them saying how much was left out and
/// ending in `note`.
pub(crate) fn keep_ends(text: String, max: usize, note: &str) -> String {
    keep_ends_around(text, &[], max, note)
}

/// `keep_ends` for a text with `gaps` in it, in order, where output was
/// printed but not kept: they fall in the part left out, and its line
/// counts them too.
fn keep_ends_around(text: String, gaps: &[Gap], max: usize, note: &str) -> String {
    if text.len() <= max && gaps.is_empty() {
        return text;
    }
    let room = max.saturating_sub(MARKER_ROOM + note.len());
    let first = gaps.first().map_or(text.len(), |g| g.at);
    let last = gaps.last().map_or(0, |g| g.at);
    let head = line_end(&text, room / 4).min(first);
    let tail = line_start(&text, text.len().saturating_sub(room - room / 4)).max(last);
    let left = &text[head..tail];
    let lines = left.lines().count() + gaps.iter().map(|g| g.lines).sum::<usize>();
    let bytes = left.len() + gaps.iter().map(|g| g.bytes).sum::<usize>();
    let unit = if lines == 1 { "line" } else { "lines" };
    let gap = if head == 0 || text[..head].ends_with('\n') {
        ""
    } else {
        "\n"
    };
    format!(
        "{}{gap}… [{lines} {unit} ({bytes} bytes) left out{note}]\n{}",
        &text[..head],
        &text[tail..]
    )
}

/// Where the last line that ends by `at` ends, or `at` itself on a line
/// longer than that, so one huge line is cut rather than dropped.
fn line_end(text: &str, at: usize) -> usize {
    let at = text.floor_char_boundary(at);
    text[..at].rfind('\n').map_or(at, |i| i + 1)
}

/// Where the first line that starts at or after `at` starts, or `at` itself
/// when no other line follows, so one huge line is cut rather than dropped.
fn line_start(text: &str, at: usize) -> usize {
    let at = text.ceil_char_boundary(at);
    if at == 0 || text.as_bytes()[at - 1] == b'\n' {
        return at;
    }
    match text[at..].find('\n') {
        Some(i) if at + i + 1 < text.len() => at + i + 1,
        _ => at,
    }
}

/// The first `max` lines, with a note about the rest. Keeps cards and rows
/// from needing their own scroll area inside the chat.
pub fn clamp_lines(text: &str, max: usize) -> String {
    let total = text.lines().count();
    if total <= max {
        return text.to_owned();
    }
    let mut out: String = text.lines().take(max).collect::<Vec<_>>().join("\n");
    out.push_str(&format!("\n… {} more lines", total - max));
    out
}

/// The tool result for a call the user denied, with the reason they typed.
/// The reason is the user's own words, not a tool's output, so it goes in
/// as written rather than fenced off; with none, the result is `DECLINED`.
pub fn declined(reason: &str) -> String {
    let reason = reason.trim();
    if reason.is_empty() {
        return DECLINED.to_owned();
    }
    let mut kept: String = reason.chars().take(REASON_CHARS).collect();
    if kept.len() < reason.len() {
        kept.push('…');
    }
    format!("{DECLINED_BECAUSE}{kept}")
}

/// Whether a tool result is a denial, with a reason or without.
pub fn is_declined(result: &str) -> bool {
    result == DECLINED || result.starts_with(DECLINED_BECAUSE)
}

/// The argument the UI shows next to a tool name.
pub fn summary(name: &str, args: &str) -> String {
    let mut v: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    rename_aliases(name, &mut v);
    let text = match name {
        "search_sessions" if v["session"].is_u64() => {
            format!(
                "session {} #{}",
                v["session"],
                v["around"].as_u64().unwrap_or(0)
            )
        }
        "search" | "search_sessions" => v["pattern"]
            .as_str()
            .or(v["query"].as_str())
            .map(|q| format!("\"{q}\""))
            .or(v["glob"].as_str().map(str::to_owned))
            .unwrap_or_default(),
        "run_command" => v["command"].as_str().unwrap_or_default().to_owned(),
        "web_fetch" => v["url"].as_str().unwrap_or_default().to_owned(),
        "read_job" | "stop_job" => format!("#{}", v["id"].as_u64().unwrap_or(0)),
        "wait" => match (v["job"].as_u64(), v["until"].as_str()) {
            (Some(id), Some(until)) => format!("job #{id} until \"{until}\""),
            (Some(id), None) => format!("job #{id}"),
            _ => format!("{}s", v["seconds"].as_f64().unwrap_or(10.)),
        },
        ASK_USER => v["question"].as_str().unwrap_or_default().to_owned(),
        // The whole plan is too long for a row; its first line names it.
        EXIT_PLAN => v["plan"]
            .as_str()
            .and_then(|plan| plan.lines().find(|line| !line.trim().is_empty()))
            .unwrap_or_default()
            .trim_start_matches(['#', ' '])
            .to_owned(),
        "make_image" => v["prompt"].as_str().unwrap_or_default().to_owned(),
        "make_video" => v["prompt"]
            .as_str()
            .or(v["job"].as_str())
            .unwrap_or_default()
            .to_owned(),
        "speak" => v["text"].as_str().unwrap_or_default().to_owned(),
        "agent" => v["description"].as_str().unwrap_or_default().to_owned(),
        "agents" => format!(
            "{} {}",
            v["action"].as_str().unwrap_or_default(),
            v["to"].as_str().unwrap_or_default()
        ),
        crate::browser::BROWSE | "use_page" => crate::browser::summary(&v),
        TODO => {
            let items = v["items"].as_array().map(Vec::len).unwrap_or(0);
            format!("{items} items")
        }
        "memory" => format!(
            "{} {}",
            v["action"].as_str().unwrap_or_default(),
            v["target"].as_str().unwrap_or_default()
        ),
        "skill" => match v["file"].as_str() {
            Some(file) => format!("{} {file}", v["name"].as_str().unwrap_or_default()),
            None => v["name"].as_str().unwrap_or_default().to_owned(),
        },
        _ => v["path"].as_str().unwrap_or(".").to_owned(),
    };
    one_line(&text)
}

/// Collapse whitespace so a multi-line command cannot spill out of the
/// single-line row it is shown in.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where a write may land: inside the workspace only, and never in
/// Letronna's own folder or on the program that is running.
fn resolve_write(raw: &str, ctx: &ToolContext) -> Result<PathBuf, String> {
    let me = std::env::current_exe().ok();
    resolve_write_in(raw, ctx, &crate::chat::data_dir(), me.as_deref())
}

/// `resolve_write` with Letronna's folder at `data` and its program at `me`,
/// checked on the nearest existing ancestor since the file need not exist yet.
fn resolve_write_in(
    raw: &str,
    ctx: &ToolContext,
    data: &Path,
    me: Option<&Path>,
) -> Result<PathBuf, String> {
    let workspace = ctx
        .workspace
        .as_ref()
        .ok_or("No project folder is attached. Ask the user to attach one first.")?;
    let root = std::fs::canonicalize(workspace).map_err(|_| "The project folder is missing.")?;
    // Before the probe below touches it: on Windows the touch is the harm.
    if let Some(why) = crate::paths::unsafe_raw(raw, Some(workspace)) {
        return Err(format!("`{raw}` {why}; it stays out of reach."));
    }
    // Refused, not folded: on Linux and macOS a missing folder hid the `..`
    // after it from the check below, so `new/../../x` was written beside the
    // project. Folding by hand would judge `link/..` unlike the system, which
    // follows the link first.
    if Path::new(raw)
        .components()
        .any(|part| part == std::path::Component::ParentDir)
    {
        return Err(format!(
            "`{raw}` climbs up with `..`; give the path from the project folder without it."
        ));
    }
    let target = workspace.join(raw);
    let mut probe = target.clone();
    // Not `exists`, which follows links: a link to a missing file was passed
    // over, and the write then went through it to wherever it pointed.
    while probe.symlink_metadata().is_err() {
        probe = probe.parent().map(Path::to_path_buf).ok_or("Bad path.")?;
    }
    let existing = std::fs::canonicalize(&probe).map_err(|_| {
        format!(
            "`{raw}` goes through a link that leads nowhere, so where it lands cannot be checked."
        )
    })?;
    if !existing.starts_with(&root) {
        return Err(format!("`{raw}` is outside the project folder."));
    }
    if private(&existing, data) {
        return Err(format!(
            "`{raw}` is in Letronna's own folder, with its keys and settings; it stays out of reach."
        ));
    }
    let Ok(rest) = target.strip_prefix(&probe) else {
        return Err("Bad path.".into());
    };
    if me.is_some_and(|me| own_program(&existing.join(rest), me)) {
        return Err(format!(
            "`{raw}` is Letronna's own program or an update to it; it stays out of reach."
        ));
    }
    Ok(target)
}

/// Variables every command and every job starts with.
const SHELL_ENV: [(&str, &str); 5] = [
    ("PAGER", "cat"),
    ("GIT_PAGER", "cat"),
    ("GIT_EDITOR", "true"),
    ("NO_COLOR", "1"),
    ("TERM", "dumb"),
];
/// Whether `path` is the program `me`, or the `.new` or `.old` copy of it
/// that an update later moves into its place.
fn own_program(path: &Path, me: &Path) -> bool {
    let Ok(me) = std::fs::canonicalize(me) else {
        return false;
    };
    // The folder is canonical on both sides; the name is compared without
    // case, because Windows and macOS open `LETRONNA.NEW.EXE` as
    // `Letronna.new.exe`.
    [
        crate::update::beside(&me, "new"),
        crate::update::beside(&me, "old"),
        me,
    ]
    .iter()
    .any(|own| {
        own.parent() == path.parent()
            && own
                .file_name()
                .zip(path.file_name())
                .is_some_and(|(own, name)| own.eq_ignore_ascii_case(name))
    })
}

/// The platform shell, configured the way every caller needs it.
pub fn shell(command: &str, cwd: Option<&Path>) -> std::process::Command {
    use std::process::Command;
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command"])
            .arg(with_utf8_output(command));
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    // Output is read back as UTF-8, and Python writes to a pipe in the
    // locale's encoding, which on Windows is not. An encoding on its own
    // would make Python strict, and a file name holding bytes that are not
    // UTF-8 would then raise instead of printing.
    cmd.env("PYTHONIOENCODING", "utf-8:surrogateescape");
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    // No one reads a command's output as a terminal, so a pager or an editor
    // would wait for keys that never come until the timeout killed it, and
    // colour would reach the model as escape codes. Git takes a pager of `cat`
    // to mean none, and an editor of `true` keeps the message it was given,
    // so a commit without -m fails at once instead of hanging.
    cmd.envs(SHELL_ENV);
    // Letronna is a GUI-subsystem app, so it owns no console. Without this,
    // Windows hands the shell a brand new console window on every call and it
    // flashes up over the chat.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    // A line that only reads runs without asking, so a repository cannot be
    // left to pick a program for its `git status` to run.
    let risk = crate::command::assess(command, crate::command::Dialect::here(), cwd).risk;
    if risk == crate::command::Risk::ReadOnly {
        cmd.envs(crate::command::GUARDS.iter().copied());
    }
    cmd
}

/// Switches Windows PowerShell's output to UTF-8 where the session allows it.
const UTF8_PRELUDE: &str = "if ($ExecutionContext.SessionState.LanguageMode -eq 'FullLanguage') { \
     try { [Console]::OutputEncoding = [Text.UTF8Encoding]::new($false); \
     $OutputEncoding = [Console]::OutputEncoding } catch {} }\n";

/// The command behind a prelude that makes Windows PowerShell print UTF-8.
fn with_utf8_output(command: &str) -> String {
    // Windows PowerShell writes to a pipe in the console's code page, so an
    // accented letter arrives as a byte that is not UTF-8. ConstrainedLanguage
    // mode refuses the switch, hence the check in the prelude, and its
    // encoding writes no BOM, which would otherwise start the output. The
    // command starts a line of its own, so an error in it points at its own
    // columns. A script that opens with param( or using must keep it first,
    // so it runs as it is.
    let head = command.trim_start().to_ascii_lowercase();
    let must_lead = head
        .strip_prefix("param")
        .is_some_and(|rest| rest.trim_start().starts_with('('))
        || head
            .strip_prefix("using")
            .is_some_and(|rest| rest.starts_with(char::is_whitespace));
    if must_lead {
        return command.to_owned();
    }
    format!("{UTF8_PRELUDE}{command}")
}

fn run_shell(command: &str, cwd: Option<&Path>, cancel: &Cancel) -> Result<String, String> {
    use std::process::Stdio;
    let mut child = shell(command, cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start the shell: {e}"))?;
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        let _ = child.kill();
        return Err("Cannot read the command's output.".into());
    };
    let (sender, open) = std::sync::mpsc::channel();
    let kept = [forward(stdout, sender.clone()), forward(stderr, sender)];
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if started.elapsed() > COMMAND_TIMEOUT {
            let _ = child.kill();
            return Err(format!(
                "Timed out after {}s and was stopped. If this command is meant to \
                 keep running, such as a dev server, call it again with \
                 background: true.",
                COMMAND_TIMEOUT.as_secs()
            ));
        }
        if !cancel.sleep(std::time::Duration::from_millis(50)) {
            let _ = child.kill();
            return Err("Stopped, and the command was ended before it finished.".into());
        }
    };
    let held_open = !pipes_closed(&open);
    let (mut text, gap) = take(&kept[0]).into_text();
    let mut gaps: Vec<Gap> = gap.into_iter().collect();
    let (err, gap) = take(&kept[1]).into_text();
    if !err.trim().is_empty() {
        text.push_str("\n[stderr]\n");
        gaps.extend(gap.map(|g| Gap {
            at: text.len() + g.at,
            ..g
        }));
        text.push_str(&err);
    }
    if !status.success() {
        text.push_str(&format!("\n[exit code {}]", status.code().unwrap_or(-1)));
    }
    if held_open {
        // First, so the cut `run` makes to a long result cannot drop it.
        let len = text.len();
        text.insert_str(
            0,
            "[the command started something that is still running and holds \
             its output open; output after the command exited was not \
             collected. To follow a process like that, run it with \
             background: true]\n",
        );
        for gap in &mut gaps {
            gap.at += text.len() - len;
        }
    }
    if !gaps.is_empty() {
        // Only this knows where the gaps are, so it makes the cut, and the
        // one `run` makes then finds the result short enough.
        text = keep_ends_around(text, &gaps, MAX_OUTPUT, "");
    }
    Ok(text)
}

/// Removes CSI and OSC escape sequences, keeping the text between them.
pub(crate) fn strip_escapes(text: &str) -> String {
    // NO_COLOR and TERM=dumb stop most of these, but not every program checks,
    // and a colour code or a window title is only noise to the model.
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            out.push(c);
            continue;
        }
        if chars.next_if_eq(&'[').is_some() {
            while chars.next_if(|c| ('\x20'..='\x3f').contains(c)).is_some() {}
            chars.next_if(|c| ('\x40'..='\x7e').contains(c));
        } else if chars.next_if_eq(&']').is_some() {
            // A terminal ends an OSC at BEL or at ST (ESC \) and reads any
            // other escape as the start of the next sequence, so this does too.
            // No title or link holds a newline, so one ends there too, and an
            // OSC left open hides the rest of its line, not the whole output.
            while chars.next_if(|c| !"\x07\x1b\n".contains(*c)).is_some() {}
            if chars.next_if_eq(&'\x07').is_none() {
                let mut ahead = chars.clone();
                if ahead.next() == Some('\x1b') && ahead.next() == Some('\\') {
                    chars = ahead;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// What a command printed to one pipe: its start, its end and how much
/// there was in all, so the middle of a chatty command's output costs
/// nothing to hold.
#[derive(Default)]
struct Captured {
    head: Vec<u8>,
    tail: std::collections::VecDeque<u8>,
    bytes: usize,
    lines: usize,
}

/// Bytes and line breaks printed but not kept, and where in the text they
/// would have been.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Gap {
    at: usize,
    bytes: usize,
    lines: usize,
}

impl Captured {
    /// Counts `chunk` and keeps it where it falls in the start or the end.
    fn push(&mut self, chunk: &[u8]) {
        self.bytes += chunk.len();
        self.lines += line_breaks(chunk);
        let room = KEEP_HEAD.saturating_sub(self.head.len()).min(chunk.len());
        let (head, rest) = chunk.split_at(room);
        self.head.extend_from_slice(head);
        self.tail.extend(rest);
        let over = self.tail.len().saturating_sub(KEEP_TAIL);
        self.tail.drain(..over);
    }

    /// What was kept, as text without escape sequences, and the gap in it
    /// when anything between the start and the end was dropped.
    fn into_text(mut self) -> (String, Option<Gap>) {
        // A pipe cut short, by the kill or by what is kept, can end partway
        // through a key, and what is left of it can be told only while the
        // cut is still the end.
        let text = |bytes: &[u8]| {
            crate::redact::scrub_cut(&strip_escapes(&String::from_utf8_lossy(bytes)), true)
        };
        if self.bytes == self.head.len() + self.tail.len() {
            self.head.extend(self.tail);
            return (text(&self.head), None);
        }
        // Whole lines on both sides keep the count of lines in the gap
        // exact; a line longer than what is kept is cut inside instead.
        if let Some(end) = self.head.iter().rposition(|&b| b == b'\n') {
            self.head.truncate(end + 1);
        }
        let tail = Vec::from(self.tail);
        let tail = tail
            .iter()
            .position(|&b| b == b'\n')
            .map_or(&tail[..], |i| &tail[i + 1..]);
        let mut out = text(&self.head);
        let gap = Gap {
            at: out.len(),
            bytes: self.bytes - self.head.len() - tail.len(),
            lines: self.lines - line_breaks(&self.head) - line_breaks(tail),
        };
        out.push_str(&text(tail));
        (out, Some(gap))
    }
}

fn line_breaks(bytes: &[u8]) -> usize {
    bytes.iter().filter(|&&b| b == b'\n').count()
}

/// Keep what `pipe` yields in the returned capture until it closes or the
/// capture is taken, then drop `open`.
fn forward(
    mut pipe: impl std::io::Read + Send + 'static,
    open: std::sync::mpsc::Sender<()>,
) -> Arc<Mutex<Option<Captured>>> {
    let kept = Arc::new(Mutex::new(Some(Captured::default())));
    let slot = kept.clone();
    std::thread::spawn(move || {
        let _open = open;
        let mut block = [0u8; 4096];
        loop {
            match pipe.read(&mut block) {
                Ok(0) => break,
                Ok(n) => {
                    // Once the capture is taken the rest is dropped; reading
                    // on anyway keeps a child left behind from blocking on a
                    // full pipe, or dying of a broken one.
                    if let Ok(mut slot) = slot.lock()
                        && let Some(kept) = slot.as_mut()
                    {
                        kept.push(&block[..n]);
                    }
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(_) => break,
            }
        }
    });
    kept
}

/// Whether every reader's pipe closed within `PIPE_GRACE`, which each marks
/// by dropping its sender of `open`.
fn pipes_closed(open: &std::sync::mpsc::Receiver<()>) -> bool {
    use std::sync::mpsc::RecvTimeoutError;
    let closed = open.recv_timeout(PIPE_GRACE);
    matches!(closed, Err(RecvTimeoutError::Disconnected))
}

/// What a reader kept so far, which also stops it keeping more.
fn take(kept: &Mutex<Option<Captured>>) -> Captured {
    kept.lock()
        .ok()
        .and_then(|mut kept| kept.take())
        .unwrap_or_default()
}

struct WriteFile;
impl Tool for WriteFile {
    fn mutates(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "write_file"
    }
    fn description(&self) -> &'static str {
        "Create or overwrite a text file in the project folder. Parent folders are created. A file that exists keeps its line endings and byte order mark. Overwriting needs the whole file read first, in an earlier step; to change part of a file, use edit_file."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"},
            "content": {"type": "string"}
        }, "required": ["path", "content"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        args["content"].as_str().unwrap_or_default().to_owned()
    }
    fn judge_input(&self, args: &Value, _: &ToolContext) -> String {
        let content = args["content"].as_str().unwrap_or_default();
        format!("Write {}:\n{content}", path_arg(args))
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let path = resolve_write(path_arg(args), ctx)?;
        let content = args["content"].as_str().ok_or("`content` is required.")?;
        let change = crate::filestate::overwrite(ctx.conversation, &path, path_arg(args))?;
        let before = change.before().map(str::as_bytes);
        let text = crate::write::keep_format(before.unwrap_or_default(), content);
        if crate::redact::rewrites_marker(change.before().unwrap_or_default(), &text) {
            return Err(crate::redact::refused("`content`"));
        }
        crate::write::check_syntax(&path, before, &text)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        change.write(text.as_bytes(), &ctx.files)?;
        Ok(format!("Wrote {} bytes.", text.len()))
    }
}

struct EditFile;
impl Tool for EditFile {
    fn mutates(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "edit_file"
    }
    fn description(&self) -> &'static str {
        "Replace `old` with `new` in a file you have read. `old` must match exactly once, unless replace_all is set to change every match. Line endings, trailing spaces, indentation and curly quotes need not match; the file keeps its own. For several changes to one file, pass `edits` instead: they are made in order, and if one fails none is made."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"},
            "old": {"type": "string"},
            "new": {"type": "string"},
            "replace_all": {"type": "boolean", "description": "Change every match of `old`, not just one."},
            "edits": {"type": "array", "description": "Several replacements, instead of `old` and `new`.", "items": {
                "type": "object", "properties": {
                    "old": {"type": "string"},
                    "new": {"type": "string"},
                    "replace_all": {"type": "boolean"}
                }, "required": ["old", "new"]
            }}
        }, "required": ["path"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let mut out = String::new();
        for edit in edits(args).unwrap_or_default() {
            for line in edit.old.lines() {
                out.push_str("- ");
                out.push_str(line);
                out.push('\n');
            }
            for line in edit.new.lines() {
                out.push_str("+ ");
                out.push_str(line);
                out.push('\n');
            }
        }
        out
    }
    fn judge_input(&self, args: &Value, ctx: &ToolContext) -> String {
        format!("Edit {}:\n{}", path_arg(args), self.preview(args, ctx))
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let path = resolve_write(path_arg(args), ctx)?;
        let edits = edits(args)?;
        let change = crate::filestate::edit(ctx.conversation, &path, path_arg(args))?;
        let text = change.before().unwrap_or_default();
        let marker = crate::redact::MARKER;
        if !text.contains(marker) && edits.iter().any(|edit| edit.old.contains(marker)) {
            return Err(format!(
                "`old` has {marker} in it, which stands for a key or token the file holds \
                 and you were not shown, so it matches nothing. Match the text around it \
                 instead."
            ));
        }
        let done = crate::edit::apply(text, &edits)?;
        if done.replaced == 0 {
            return Ok("Already applied; nothing changed.".into());
        }
        if crate::redact::adds_marker(text, &done.text) {
            return Err(crate::redact::refused("`new`"));
        }
        crate::write::check_syntax(&path, Some(text.as_bytes()), &done.text)?;
        change.write(done.text.as_bytes(), &ctx.files)?;
        Ok(match (done.replaced, done.already) {
            (1, 0) => "Edited.".into(),
            (n, 0) => format!("Edited {n} places."),
            (_, n) => format!(
                "Edited. Already applied, so left alone: {n} of {} edits.",
                edits.len()
            ),
        })
    }
}

/// The replacements an edit_file call asks for: its own `old` and `new`
/// first, then each of `edits`.
fn edits(args: &Value) -> Result<Vec<crate::edit::Edit<'_>>, String> {
    let own = std::iter::once(args).filter(|a| !a["old"].is_null());
    let listed = args["edits"].as_array().into_iter().flatten();
    let mut edits = Vec::new();
    for edit in own.chain(listed) {
        edits.push(crate::edit::Edit {
            old: edit["old"].as_str().ok_or("`old` is required.")?,
            new: edit["new"].as_str().ok_or("`new` is required.")?,
            all: edit["replace_all"].as_bool().unwrap_or(false),
        });
    }
    if edits.is_empty() {
        return Err("`old` and `new` are required, or `edits`.".into());
    }
    Ok(edits)
}

struct RunCommand;
impl Tool for RunCommand {
    fn hides_key_shapes(&self) -> bool {
        true
    }
    fn mutates(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "run_command"
    }
    fn description(&self) -> &'static str {
        if cfg!(windows) {
            "Run a Windows PowerShell 5.1 command in the project folder. `&&` and `||` are syntax errors in 5.1: separate commands with `;`, or write `first; if ($?) { second }` to run the second only when the first succeeded. Waits for it to finish and returns its output, with a 2 minute limit. For anything meant to keep running, such as a dev server or a watcher, pass background: true instead."
        } else {
            "Run a shell (sh) command in the project folder. Waits for it to finish and returns its output, with a 2 minute limit. For anything meant to keep running, such as a dev server or a watcher, pass background: true instead."
        }
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "command": {"type": "string"},
            "background": {"type": "boolean", "description": "Leave the command running and return a job number straight away. A notice comes when it exits; read its output meanwhile with read_job, and end it with stop_job."},
            "notify_on": {"type": "array", "items": {"type": "string"}, "description": "With background: a notice when an output line contains one of these, such as \"ready\" or \"error\". At most one notice every 15 s; exiting always sends one."}
        }, "required": ["command"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        args["command"].as_str().unwrap_or_default().to_owned()
    }
    fn judge_input(&self, args: &Value, ctx: &ToolContext) -> String {
        // Never cut: the end of a command can undo whatever its start shows.
        let line = args["command"].as_str().unwrap_or_default();
        let folder = match &ctx.workspace {
            Some(folder) => folder.display().to_string(),
            None => "no folder".to_owned(),
        };
        let background = if args["background"].as_bool().unwrap_or(false) {
            ", left running in the background"
        } else {
            ""
        };
        format!("Command, run in {folder}{background}:\n{line}")
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let command = args["command"].as_str().ok_or("`command` is required.")?;
        let cwd = ctx
            .workspace
            .as_deref()
            .ok_or("No project folder is attached. Ask the user to attach one first.")?;
        let notify_on = notify_on(args)?;
        if !args["background"].as_bool().unwrap_or(false) {
            if !notify_on.is_empty() {
                return Err("`notify_on` needs background: true. A command that runs \
                            in the foreground returns all of its output anyway."
                    .into());
            }
            let before = crate::filestate::before(ctx.conversation);
            let out = run_shell(command, Some(cwd), &ctx.cancel);
            let note = crate::filestate::note(ctx.conversation, before, cwd);
            // The note goes first, so cutting long output short cannot drop it.
            return match out {
                Ok(text) => Ok(note + &text),
                Err(e) => Err(note + &e),
            };
        }
        let id = crate::jobs::start_watching(command, cwd, ctx.conversation, &notify_on)?;
        // Give it a moment to fail or announce a port, so the first reply says
        // something more useful than "started". A stop cuts the moment short,
        // but the job runs on either way, so it is still reported.
        ctx.cancel.sleep(SETTLE);
        // Read this way, what the reply shows never comes again as a notice.
        let log = crate::jobs::first_output(id).unwrap_or_default();
        let state = if crate::jobs::running(id).unwrap_or(false) {
            "running"
        } else {
            "already exited"
        };
        let watching = if notify_on.is_empty() {
            String::new()
        } else {
            let quoted: Vec<String> = notify_on.iter().map(|text| format!("\"{text}\"")).collect();
            format!(
                "\nWatching for {}: a notice comes on a matching line, and when it exits.",
                quoted.join(", ")
            )
        };
        Ok(format!(
            "Started as job #{id} ({state}). First output:\n{}{watching}",
            if log.trim().is_empty() {
                "[nothing yet]"
            } else {
                log.trim_end()
            }
        ))
    }
}

/// The user's own earlier conversations, by name and size only.
///
/// Asked "which sessions do I have", a model with only file tools either
/// guesses -- and guesses some other agent's command, because that is what it
/// has read most of -- or reads the session files, which pours an old
/// conversation into the current one. This answers the question directly and
/// carries nothing that was said.
struct Sessions;
impl Tool for Sessions {
    fn name(&self) -> &'static str {
        "list_sessions"
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "List the user's earlier Letronna sessions: id, title, when they were last used, how many messages, and which project folder. Use this when asked about past sessions or which one to pick up. It returns no conversation text. The user opens one with the /resume command, or `letronna --resume <id>`."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "limit": {"type": "integer", "description": "How many to list, newest first. Default 15."}
        }})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let limit = args["limit"].as_u64().unwrap_or(15).clamp(1, 60) as usize;
        let all = crate::store::summaries();
        let listed: Vec<String> = all
            .iter()
            // A session nobody said anything in is not worth offering.
            .filter(|s| s.messages > 0)
            .take(limit)
            .map(|s| {
                let title = match s.title.trim() {
                    "" => "untitled",
                    title => title,
                };
                let folder = s
                    .workspace
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "no project".into());
                format!(
                    "{}  {}  ({} messages, {}, {})",
                    s.id,
                    title,
                    s.messages,
                    folder,
                    crate::chat::ago(s.updated)
                )
            })
            .collect();
        if listed.is_empty() {
            return Ok("No earlier sessions.".into());
        }
        Ok(format!(
            "id  title  (messages, project, when)
{}

The user opens one with /resume, or `letronna --resume <id>`.",
            listed.join(
                "
"
            )
        ))
    }
}

/// Finds where earlier sessions talked about something. `list_sessions`
/// gives titles only, so "what did we decide about X" had no answer short of
/// the user opening sessions one by one.
struct SearchSessions;
impl Tool for SearchSessions {
    fn name(&self) -> &'static str {
        "search_sessions"
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "Search what was said in the user's earlier Letronna sessions, ignoring case. A match holds every word of the query (\"quoted words\" stay together), or when nothing does, some of them; matches on rarer words come first. Titles and tool call arguments are searched too; the current session is left out. Each match gives the session's id, title and age and the message's #. To read the messages around a match, call again with session and around. Use it when the user refers to something from an earlier conversation, or when an earlier session may already hold the answer."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "query": {"type": "string", "description": "The words to find. With session, the words to center long messages on."},
            "limit": {"type": "integer", "description": "How many matches at most. Default 20."},
            "after": {"type": "string", "description": "Only sessions last used on or after this date, such as 2026-09-01, or this many days ago, such as 7d."},
            "before": {"type": "string", "description": "Only sessions last used before this date, or this many days ago."},
            "session": {"type": "integer", "description": "Read the messages of this session around one message instead of searching."},
            "around": {"type": "integer", "description": "With session: the # of the message to read, with the three either side of it. Default 0."}
        }})
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let query = args["query"].as_str().unwrap_or_default().trim();
        if let Some(id) = args["session"].as_u64() {
            let around = args["around"].as_u64().unwrap_or(0) as usize;
            return read_around(id, around, query);
        }
        if query.is_empty() {
            return Err("Query is empty.".into());
        }
        let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
        let now = crate::chat::now();
        let date = |key: &str| {
            args[key]
                .as_str()
                .map(|text| day_or_days(text, now))
                .transpose()
        };
        let within = crate::store::Within {
            exclude: ctx.conversation,
            after: date("after")?,
            before: date("before")?,
        };
        let hits = crate::store::search(query, &within, limit);
        if hits.is_empty() {
            return Ok(format!("No earlier session mentions \"{query}\"."));
        }
        let lines: Vec<String> = hits
            .iter()
            .map(|hit| {
                let title = shown_title(&hit.title);
                let when = crate::chat::ago(hit.updated);
                let place = hit.ordinal.map(|n| format!("#{n} ")).unwrap_or_default();
                format!(
                    "{}  {title}  ({when}) {place}{}: {}",
                    hit.id, hit.role, hit.snippet
                )
            })
            .collect();
        Ok(format!(
            "id  title  (when) #message who: what was said\n{}\n\nRead around a match with session and around. The user opens one with /resume, or `letronna --resume <id>`.",
            lines.join("\n")
        ))
    }
}

/// Messages read either side of the one asked for.
const READ_SIDE: usize = 3;
/// Characters shown of each message read: seven of them with their labels
/// stay under the chat's cap on a tool result, which would cut their middle.
const READ_CHARS: usize = 1_000;

/// The messages of session `id` either side of message `around`, each cut
/// around the words of `query` when it is long.
fn read_around(id: u64, around: usize, query: &str) -> Result<String, String> {
    let Some(session) = crate::store::saved(id) else {
        return Err(format!("There is no saved session {id}."));
    };
    let count = session.messages.len();
    if around >= count {
        return Err(format!(
            "Session {id} has {count} messages, #0 to #{}.",
            count.saturating_sub(1)
        ));
    }
    let from = around.saturating_sub(READ_SIDE);
    let to = (around + READ_SIDE).min(count - 1);
    let title = shown_title(&session.title);
    let mut out = format!(
        "Session {id}  {title}  ({}), messages #{from} to #{to} of {count}:",
        crate::chat::ago(session.updated)
    );
    for (n, message) in session.messages.iter().enumerate().take(to + 1).skip(from) {
        let who = if message.notice {
            "notice"
        } else {
            &message.role
        };
        let text = crate::store::searched(message);
        let said = crate::store::excerpt(&text, query, READ_CHARS);
        out.push_str(&format!("\n#{n} {who}: {said}"));
    }
    Ok(out)
}

/// A session's title on one line and cut short, as a renamed session's
/// title can be any length and is shown on every line that names it.
fn shown_title(title: &str) -> String {
    match one_line(title) {
        title if title.is_empty() => "untitled".into(),
        title if title.chars().count() > 100 => title.chars().take(100).chain(['…']).collect(),
        title => title,
    }
}

/// Seconds since 1970 for an `after` or `before` argument: a date such as
/// 2026-09-01, from the start of that day here, or days ago such as 7d.
fn day_or_days(text: &str, now: u64) -> Result<u64, String> {
    let text = text.trim();
    if let Some(days) = text.strip_suffix('d').and_then(|d| d.parse::<u64>().ok()) {
        return Ok(now.saturating_sub(days.saturating_mul(86_400)));
    }
    let wrong = || format!("\"{text}\" is not a date such as 2026-09-01, nor days ago such as 7d.");
    let day = chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").map_err(|_| wrong())?;
    let start = day
        .and_hms_opt(0, 0, 0)
        .and_then(|t| t.and_local_timezone(chrono::Local).earliest())
        .ok_or_else(wrong)?;
    Ok(start.timestamp().max(0) as u64)
}

struct ReadJob;
impl Tool for ReadJob {
    fn hides_key_shapes(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "read_job"
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "Read what a background job has printed so far, and whether it is still running. Pass offset and limit to read a long log a page of lines at a time."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "id": {"type": "integer"},
            "offset": {"type": "integer", "description": "How many lines to skip, counting from the oldest line still kept."},
            "limit": {"type": "integer", "description": "How many lines to read from there."}
        }, "required": ["id"]})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let id = job_id(args)?;
        let log = crate::jobs::log(id)?;
        let state = if crate::jobs::running(id)? {
            "running".to_owned()
        } else {
            match crate::jobs::exit_code(id)? {
                Some(code) => format!("exited with code {code}"),
                None => "stopped".to_owned(),
            }
        };
        let (offset, limit) = (args["offset"].as_u64(), args["limit"].as_u64());
        if offset.is_some() || limit.is_some() {
            let lines: Vec<&str> = log.trim_end().lines().collect();
            let total = lines.len();
            let start = (offset.unwrap_or(0) as usize).min(total);
            let end = limit.map_or(total, |limit| {
                start.saturating_add(limit as usize).min(total)
            });
            if start == end {
                return Ok(format!(
                    "Job #{id} is {state}. No lines from line {}; the log has {total}.",
                    offset.unwrap_or(0).saturating_add(1)
                ));
            }
            return Ok(format!(
                "Job #{id} is {state}. Lines {} to {end} of {total}:\n{}",
                start + 1,
                lines[start..end].join("\n")
            ));
        }
        Ok(format!(
            "Job #{id} is {state}.\n{}",
            if log.trim().is_empty() {
                "[no output]"
            } else {
                log.trim_end()
            }
        ))
    }
}

struct StopJob;
impl Tool for StopJob {
    fn mutates(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "stop_job"
    }
    fn description(&self) -> &'static str {
        "Stop a background job."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "id": {"type": "integer"}
        }, "required": ["id"]})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let id = job_id(args)?;
        crate::jobs::stop(id)?;
        Ok(format!("Stopped job #{id}."))
    }
}

struct Wait;
impl Tool for Wait {
    fn hides_key_shapes(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "wait"
    }
    fn description(&self) -> &'static str {
        "Pause before continuing: for a number of seconds, or until a background job prints something. Use it to let a server come up before calling it; waiting for a job's `until` ends as soon as it prints. If you have wake_me, use it instead of a plain pause over 30 seconds."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "seconds": {"type": "number", "description": "How long to wait. Also the limit when waiting for output."},
            "job": {"type": "integer", "description": "Wait for this job to print `until`, or to exit."},
            "until": {"type": "string", "description": "Text to watch for in the job's output, such as `ready` or `listening`."}
        }})
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let seconds = args["seconds"].as_f64().unwrap_or(10.).clamp(0., 120.);
        let limit = std::time::Duration::from_secs_f64(seconds);
        let Some(id) = args["job"].as_u64() else {
            if !ctx.cancel.sleep(limit) {
                return Err("Stopped before the wait was over.".into());
            }
            return Ok(format!("Waited {seconds:.0}s."));
        };
        let until = args["until"].as_str().unwrap_or_default();
        let started = std::time::Instant::now();
        loop {
            let log = crate::jobs::log(id)?;
            if !until.is_empty() && log.contains(until) {
                return Ok(format!(
                    "Job #{id} printed `{until}` after {:.0}s.",
                    started.elapsed().as_secs_f32()
                ));
            }
            if !crate::jobs::running(id)? {
                return Ok(format!(
                    "Job #{id} exited before printing `{until}`. Output:\n{}",
                    log.trim_end()
                ));
            }
            if started.elapsed() >= limit {
                return Ok(format!(
                    "Job #{id} is still running after {seconds:.0}s{}. Latest output:\n{}",
                    if until.is_empty() {
                        String::new()
                    } else {
                        format!(" without printing `{until}`")
                    },
                    log.trim_end()
                ));
            }
            if !ctx.cancel.sleep(std::time::Duration::from_millis(250)) {
                return Err(format!("Stopped waiting. Job #{id} is still running."));
            }
        }
    }
}

fn job_id(args: &Value) -> Result<u64, String> {
    args["id"]
        .as_u64()
        .ok_or_else(|| "`id` is required and must be a job number.".to_owned())
}

/// The text a background command's output is watched for, from `notify_on`.
fn notify_on(args: &Value) -> Result<Vec<String>, String> {
    let bad = "`notify_on` must be a list of text to look for, such as [\"ready\", \"error\"].";
    let list = match &args["notify_on"] {
        Value::Null => return Ok(Vec::new()),
        Value::Array(list) => list,
        _ => return Err(bad.into()),
    };
    // An empty string is in every line, so it would watch for nothing in particular.
    list.iter()
        .map(|text| {
            text.as_str()
                .filter(|text| !text.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| bad.to_owned())
        })
        .collect()
}

/// Answered in the chat, and run only in an unattended session to say nobody will.
struct AskUser;
impl Tool for AskUser {
    fn name(&self) -> &'static str {
        ASK_USER
    }
    fn description(&self) -> &'static str {
        "Ask the user a question and wait for the answer. Use it when the task is ambiguous or needs a decision, such as a project name or framework."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "question": {"type": "string"},
            "options": {"type": "array", "items": {"type": "string"}, "description": "Optional choices to offer."}
        }, "required": ["question"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn machine(&self) -> bool {
        false
    }
    fn run(&self, _: &Value, _: &ToolContext) -> Result<String, String> {
        Ok("Nobody is available to answer; decide for yourself and say what you assumed.".into())
    }
}

/// The plan made in plan mode, put to the user. Like `ask_user` it is
/// answered by the user (see `Turn::answer_plan`), so it only runs when
/// nobody could be asked, and then it refuses.
struct ExitPlan;
impl Tool for ExitPlan {
    fn name(&self) -> &'static str {
        EXIT_PLAN
    }
    fn description(&self) -> &'static str {
        "Put your finished plan to the user for approval, written in markdown. If they approve, plan mode turns off and you carry the plan out straight away; if not, keep planning. Use it once the plan is ready, instead of asking whether to go ahead."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "plan": {"type": "string", "description": "The plan in markdown: what will change, where, and how it will be checked."}
        }, "required": ["plan"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        args["plan"].as_str().unwrap_or_default().to_owned()
    }
    fn run(&self, _: &Value, ctx: &ToolContext) -> Result<String, String> {
        // A chat app is in plan mode because its owner left it there, which
        // nobody on the far end may undo.
        let advice = if ctx.remote.is_some() {
            "A plan cannot be approved from a chat app, so plan mode stays on. \
             Give the plan as your answer; to carry it out from here, the owner \
             can switch on \"Let messages use tools\" on Letronna's Messaging page."
        } else {
            "Nobody is here to approve a plan, so plan mode stays on. Give the \
             plan as your answer and stop there, so the user can read it and \
             decide."
        };
        Err(advice.into())
    }
}

/// A checklist the model keeps for itself. The window and the terminal store
/// the items on the conversation and draw them above where you type; see
/// `store::apply_todo`.
struct Todo;
impl Tool for Todo {
    fn name(&self) -> &'static str {
        TODO
    }
    fn description(&self) -> &'static str {
        "Record or update your plan as a checklist the user can see. Send the whole list every time, with each item's status. Use it for work with several steps, and mark items done as you go."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "items": {"type": "array", "items": {"type": "object", "properties": {
                "text": {"type": "string"},
                "status": {"type": "string", "enum": ["pending", "doing", "done"]}
            }, "required": ["text", "status"]}}
        }, "required": ["items"]})
    }
    fn machine(&self) -> bool {
        false
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let items = args["items"].as_array().ok_or("`items` is required.")?;
        Ok(format!("Noted {} items.", items.len()))
    }
}

pub(crate) fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Resolve a user-supplied path and refuse anything outside the workspace or
/// the home folder. Canonicalising also defeats `..` and symlink escapes.
pub(crate) fn resolve(raw: &str, ctx: &ToolContext) -> Result<PathBuf, String> {
    // Before canonicalising, which opens the path: on Windows that alone
    // signs in to another computer, and these reads never ask first.
    if let Some(why) = crate::paths::unsafe_raw(raw, ctx.workspace.as_deref()) {
        return Err(format!("`{raw}` {why}; it stays out of reach."));
    }
    let base = ctx.workspace.clone().or_else(home).unwrap_or_default();
    let joined = base.join(raw);
    let path = std::fs::canonicalize(&joined).map_err(|_| format!("`{raw}` does not exist."))?;
    // Checked first: the saved outputs sit in Letronna's own folder, which
    // need not be under home either. Writing goes through `resolve_write`.
    if own_result(&path, ctx) {
        return Ok(path);
    }
    let allowed = [ctx.workspace.clone(), home()]
        .into_iter()
        .flatten()
        .filter_map(|p| std::fs::canonicalize(p).ok())
        .any(|root| path.starts_with(root));
    if !allowed {
        return Err(format!("`{raw}` is outside the workspace and home folder."));
    }
    if private(&path, &crate::chat::data_dir()) {
        return Err(format!(
            "`{raw}` is in Letronna's own folder, with the keys and sign-in; it stays out of reach."
        ));
    }
    // The gate asks before such a read; a call that reached here without
    // that yes, such as a helper's, is refused as before.
    if secret(&path) && !ctx.approved {
        return Err(format!(
            "`{raw}` holds keys, passwords or tokens, so it stays out of reach. Ask the user to paste what you need from it."
        ));
    }
    Ok(path)
}

/// Whether `path` is an output saved whole when it was cut in this same
/// conversation. Another conversation's stay private like the rest.
fn own_result(path: &Path, ctx: &ToolContext) -> bool {
    ctx.conversation != 0
        && std::fs::canonicalize(results_dir(ctx.conversation))
            .is_ok_and(|dir| path.starts_with(dir))
}

/// Whether `raw` names a file or folder that holds keys, passwords or
/// tokens, links followed, as `resolve` would find it.
pub(crate) fn holds_secrets(raw: &str, ctx: &ToolContext) -> bool {
    let base = ctx.workspace.clone().or_else(home).unwrap_or_default();
    std::fs::canonicalize(base.join(raw)).is_ok_and(|path| secret(&path))
}

/// Whether `path` lies in Letronna's own folder, `data`, which holds the API
/// keys, the sign-in token and the bot tokens. It sits under the home folder,
/// and reading there needs no approval, so a chat or a file that talked the
/// model into it would have had them read out.
pub(crate) fn private(path: &Path, data: &Path) -> bool {
    std::fs::canonicalize(data).is_ok_and(|data| path.starts_with(data))
}

/// Folders and files that hold keys, passwords or tokens (package
/// registries, the GitHub CLI, Docker, Kubernetes, other agents' sign-in),
/// matched as a run of names anywhere in a path so that a folder covers
/// everything in it; `_netrc` is what curl and Git read on Windows.
pub(crate) const SECRETS: &[&str] = &[
    ".ssh",
    ".aws",
    ".netrc",
    "_netrc",
    ".git-credentials",
    ".npmrc",
    ".pypirc",
    ".config/gh/hosts.yml",
    ".docker/config.json",
    ".kube/config",
    ".claude/.credentials.json",
    ".codex/auth.json",
];

/// Whether `path` is or lies in one of `SECRETS`, or is a `.env` file other
/// than the example and sample ones that projects commit.
pub(crate) fn secret(path: &Path) -> bool {
    // Without case, because Windows and macOS open `.SSH` as `.ssh`.
    let names: Vec<&std::ffi::OsStr> = path.iter().collect();
    let listed = SECRETS.iter().any(|entry| {
        let len = entry.split('/').count();
        names.windows(len).any(|run| {
            run.iter()
                .zip(entry.split('/'))
                .all(|(name, part)| name.eq_ignore_ascii_case(part))
        })
    });
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    listed
        || name == ".env"
        || name
            .strip_prefix(".env.")
            .is_some_and(|rest| !matches!(rest, "example" | "sample"))
}

pub(crate) fn path_arg(args: &Value) -> &str {
    args["path"].as_str().unwrap_or(".")
}

struct ReadFile;
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "Read a text file. Paths are relative to the workspace. Each line is shown as `N|text`, N being its line number; the `N|` is not part of the file. A long file comes a page at a time, and the page ends with the offset to continue from."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"},
            "offset": {"type": "integer", "description": "The line to start at, counting from 1."},
            "limit": {"type": "integer", "description": "How many lines to show at most."}
        }, "required": ["path"]})
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let raw = path_arg(args);
        let path = resolve(raw, ctx).map_err(|e| e + &similar(raw, ctx))?;
        let offset = args["offset"].as_u64().unwrap_or(1).max(1) as usize;
        let limit = args["limit"].as_u64().unwrap_or(u64::MAX).max(1) as usize;
        let before = crate::filestate::stat(&path);
        let (mut text, cut) = open_text(&path)?;
        let (shown, whole) = page(&mut text, offset, limit, cut)?;
        // A longer result loses its middle on the way to the model.
        let whole = whole && shown.chars().count() <= crate::chat::MAX_TOOL_CHARS;
        crate::filestate::read(ctx.conversation, &path, before, whole);
        Ok(shown)
    }
}

/// A file's text as UTF-8, without a byte order mark and with UTF-16 decoded,
/// and whether it stops at `UTF16_BYTES`; a NUL byte near the start marks a
/// binary file, which is refused.
fn open_text(path: &Path) -> Result<(Box<dyn BufRead>, bool), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(file);
    let start = reader.fill_buf().map_err(|e| e.to_string())?;
    let unit: fn([u8; 2]) -> u16 = match start {
        [0xFF, 0xFE, ..] => u16::from_le_bytes,
        [0xFE, 0xFF, ..] => u16::from_be_bytes,
        [0xEF, 0xBB, 0xBF, ..] => {
            reader.consume(3);
            return Ok((Box::new(reader), false));
        }
        _ if start.contains(&0) => return Err("Not a text file.".into()),
        _ => return Ok((Box::new(reader), false)),
    };
    reader.consume(2);
    let mut bytes = Vec::new();
    // One byte past the cap tells a file cut there from one that fits it.
    reader
        .take(UTF16_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    let cut = bytes.len() as u64 > UTF16_BYTES;
    bytes.truncate(UTF16_BYTES as usize);
    let units = bytes.as_chunks::<2>().0.iter().map(|&pair| unit(pair));
    let text: String = char::decode_utf16(units)
        .map(|c| c.unwrap_or(char::REPLACEMENT_CHARACTER))
        .collect();
    Ok((Box::new(std::io::Cursor::new(text.into_bytes())), cut))
}

/// Lines from `offset` on, numbered, until `limit` of them or `PAGE_BYTES`,
/// with a footer saying where the rest starts and a note on every page of a
/// `cut` file; and whether that showed all of the file, as it is.
fn page(
    reader: &mut impl BufRead,
    offset: usize,
    limit: usize,
    cut: bool,
) -> Result<(String, bool), String> {
    let mut out = String::new();
    let mut line = Vec::new();
    let (mut n, mut lossy, mut more, mut long) = (0, false, false, false);
    while next_line(reader, &mut line)? {
        n += 1;
        if n < offset {
            continue;
        }
        if n - offset == limit {
            more = true;
            break;
        }
        let text = String::from_utf8_lossy(&line);
        let shown = match text.char_indices().nth(LINE_CHARS) {
            Some(_) => {
                long = true;
                // Hidden before the cut, which could leave part of a key
                // showing; `next_line` may have cut the line already.
                let text = crate::redact::scrub_cut(&text, false);
                let at = text
                    .char_indices()
                    .nth(LINE_CHARS)
                    .map_or(text.len(), |(at, _)| at);
                format!("{n}|{} … [line cut]\n", &text[..at])
            }
            None => format!("{n}|{text}\n"),
        };
        if !out.is_empty() && out.len() + shown.len() > PAGE_BYTES {
            more = true;
            break;
        }
        // A bad byte shows only before the cut, as the character after the
        // valid ones; one past it, or a character split where `next_line`
        // stopped, is never seen.
        lossy |= std::str::from_utf8(&line)
            .is_err_and(|e| text[..e.valid_up_to()].chars().count() < LINE_CHARS);
        out.push_str(&shown);
    }
    if n == 0 {
        return Ok(("The file is empty.".into(), true));
    }
    let mb = UTF16_BYTES / (1024 * 1024);
    if out.is_empty() {
        let file = if cut {
            format!("The first {mb} MB of this UTF-16 file, which is all that is read,")
        } else {
            "The file".into()
        };
        return Err(format!(
            "{file} has {n} lines, so offset {offset} is past its end."
        ));
    }
    out.pop();
    if lossy {
        out.push_str("\n\n[Some bytes are not UTF-8 and show as \u{FFFD}.]");
    }
    // Without this a cut file's last page would read as the end of the file.
    if cut {
        out.push_str(&format!(
            "\n\n[Only the first {mb} MB of this UTF-16 file is read.]"
        ));
    }
    // Line `n` was read to find there is more, and is where the next page
    // starts. Past a cut there are lines that cannot be counted.
    if more {
        let rest = if cut { None } else { lines_left(reader) };
        let of = rest
            .map(|rest| format!(" of {}", n + rest))
            .unwrap_or_default();
        out.push_str(&format!(
            "\n\nLines {offset}-{}{of}. Continue with offset={n}.",
            n - 1
        ));
    }
    let whole = offset == 1 && !more && !long && !lossy && !cut;
    Ok((out, whole))
}

/// Reads the next line into `line`, without its line break and only as much
/// of it as a page could show; false at the end of the file.
fn next_line(reader: &mut impl BufRead, line: &mut Vec<u8>) -> Result<bool, String> {
    // One character more than a line shows, at up to four bytes each, so a
    // line stopped short here still reads as too long and is marked cut.
    let keep = 4 * (LINE_CHARS as u64 + 1);
    line.clear();
    let read = reader
        .by_ref()
        .take(keep)
        .read_until(b'\n', line)
        .map_err(|e| e.to_string())?;
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
    } else if read as u64 == keep {
        reader.skip_until(b'\n').map_err(|e| e.to_string())?;
    }
    Ok(read > 0)
}

/// How many lines are left in `reader`, or `None` once it runs past
/// `COUNT_BYTES`.
fn lines_left(reader: &mut impl BufRead) -> Option<usize> {
    let (mut lines, mut read, mut last) = (0, 0, b'\n');
    loop {
        let buf = reader.fill_buf().ok()?;
        let Some(&end) = buf.last() else {
            return Some(lines + usize::from(last != b'\n'));
        };
        lines += buf.iter().filter(|&&b| b == b'\n').count();
        last = end;
        let len = buf.len();
        reader.consume(len);
        read += len as u64;
        if read > COUNT_BYTES {
            return None;
        }
    }
}

/// Up to five names beside a missing `raw` that start like it, as a sentence
/// to add to the error, or nothing.
fn similar(raw: &str, ctx: &ToolContext) -> String {
    let wanted = Path::new(raw);
    let Some(file_name) = wanted.file_name() else {
        return String::new();
    };
    // Through `resolve`, so no name is given from a folder that may not be
    // read, and none for a path that is there but refused.
    let parent = wanted.parent().and_then(Path::to_str);
    let Ok(dir) = resolve(parent.filter(|p| !p.is_empty()).unwrap_or("."), ctx) else {
        return String::new();
    };
    if dir.join(file_name).exists() {
        return String::new();
    }
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return String::new();
    };
    let name = file_name.to_string_lossy().to_lowercase();
    let stem = match name.rsplit_once('.') {
        Some((stem, _)) if !stem.is_empty() => stem,
        _ => name.as_str(),
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.to_lowercase().starts_with(stem))
        .collect();
    if names.is_empty() {
        return String::new();
    }
    names.sort();
    let names: Vec<String> = names
        .iter()
        .take(5)
        .map(|n| format!("`{}`", wanted.with_file_name(n).display()))
        .collect();
    format!(" Did you mean {}?", names.join(", "))
}

struct ListDir;
impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "List the entries of a folder. Folders end with a slash."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "Defaults to the workspace root."}
        }})
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let path = resolve(path_arg(args), ctx)?;
        let mut names: Vec<String> = std::fs::read_dir(&path)
            .map_err(|e| e.to_string())?
            .filter_map(Result::ok)
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() { name + "/" } else { name }
            })
            .collect();
        names.sort();
        Ok(names.join("\n"))
    }
}

struct Search;
impl Tool for Search {
    fn name(&self) -> &'static str {
        "search"
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn description(&self) -> &'static str {
        "Search file contents under a folder with a regex (Rust syntax), matched line by \
         line, leaving out what .gitignore ignores and, outside a Git repository, target, \
         dist and build folders unless the glob names them. Mode content (the default) shows \
         each file's matching lines as `12: text` and context lines as `13- text`; files \
         lists the matching files, newest first; count gives matching lines per file. Mode \
         files with no pattern lists the files a glob or type picks. Ignores case unless \
         case_sensitive is set. Paths come back ready for read_file."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "pattern": {"type": "string", "description": "A regex. One that does not parse, or finds nothing, is tried as plain text."},
            "path": {"type": "string", "description": "Folder or file to search. Defaults to the workspace root."},
            "glob": {"type": "string", "description": "Only paths under `path` matching this, e.g. `*.rs` or `src/**/*.{ts,tsx}`; a leading `!` leaves them out instead."},
            "type": {"type": "string", "description": "Only files of this type: rust, py, ts, js, go, java, cpp, md and so on, or a bare extension."},
            "mode": {"type": "string", "enum": ["content", "files", "count"]},
            "context": {"type": "integer", "description": "Lines shown either side of each match, up to 5."},
            "case_sensitive": {"type": "boolean"},
            "limit": {"type": "integer", "description": "Results per page, 100 unless set."},
            "offset": {"type": "integer", "description": "Results to skip, to reach the next page."}
        }})
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let root = resolve(path_arg(args), ctx)?;
        // A workspace that holds the home folder is searched like the home
        // folder, whose hidden files are other programs' settings and tokens.
        let home = home().and_then(|h| std::fs::canonicalize(h).ok());
        let space = ctx
            .workspace
            .as_ref()
            .and_then(|w| std::fs::canonicalize(w).ok());
        let workspace = space
            .as_ref()
            .filter(|w| root.starts_with(w) && !home.as_ref().is_some_and(|h| h.starts_with(w)));
        // Outside the workspace the walk leaves hidden entries out, but it
        // never looks at its own root, which would be walked all the same.
        let base = home
            .iter()
            .chain(&space)
            .find(|base| root.starts_with(base));
        if workspace.is_none() && base.is_some_and(|base| crate::grep::hidden_below(base, &root)) {
            return Err(format!(
                "`{}` is hidden or in a hidden folder outside the workspace, where other \
                 programs keep their settings and tokens, so search leaves it out.",
                path_arg(args)
            ));
        }
        let query = crate::grep::Query::from_args(args, &root, workspace.map(PathBuf::as_path))?;
        crate::grep::search(&query, Instant::now() + SEARCH_TIME, SEARCH_FILES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_search_date_is_a_day_here_or_days_ago() {
        let now = 1_800_000_000;
        assert_eq!(day_or_days("7d", now), Ok(now - 7 * 86_400));
        assert_eq!(day_or_days(" 0d ", now), Ok(now));
        let day = day_or_days("2026-09-01", now).unwrap();
        let local = chrono::TimeZone::timestamp_opt(&chrono::Local, day as i64, 0).unwrap();
        assert_eq!(local.format("%F %T").to_string(), "2026-09-01 00:00:00");
        for wrong in ["yesterday", "7", "2026-13-01", "-3d"] {
            let refused = day_or_days(wrong, now);
            assert!(refused.is_err_and(|e| e.contains("7d")), "{wrong}");
        }
    }

    #[test]
    fn reads_lists_searches_within_workspace() {
        let dir = std::env::temp_dir().join("letronna-tools");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::write(dir.join("src/a.txt"), "hello\nWorld").unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        assert_eq!(
            run("read_file", r#"{"path":"src/a.txt"}"#, &ctx).unwrap(),
            "1|hello\n2|World"
        );
        assert_eq!(run("list_dir", "{}", &ctx).unwrap(), "src/");
        assert_eq!(
            run("search", r#"{"query":"world"}"#, &ctx).unwrap(),
            "src\\a.txt\n  2: World".replace('\\', std::path::MAIN_SEPARATOR_STR)
        );
        // The filesystem root is outside both the workspace and home.
        assert!(run("list_dir", r#"{"path":"/"}"#, &ctx).is_err());
        run(
            "write_file",
            r#"{"path":"new/b.txt","content":"one two"}"#,
            &ctx,
        )
        .unwrap();
        run(
            "edit_file",
            r#"{"path":"new/b.txt","old":"two","new":"three"}"#,
            &ctx,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("new/b.txt")).unwrap(),
            "one three"
        );
        assert!(
            run(
                "edit_file",
                r#"{"path":"new/b.txt","old":"zzz","new":""}"#,
                &ctx
            )
            .is_err()
        );
        assert!(
            run(
                "write_file",
                r#"{"path":"../escape.txt","content":""}"#,
                &ctx
            )
            .is_err()
        );
        let no_ws = ToolContext::default();
        assert!(run("write_file", r#"{"path":"x","content":""}"#, &no_ws).is_err());
        let out = run("run_command", r#"{"command":"echo hi"}"#, &ctx).unwrap();
        assert!(out.trim_start().starts_with("hi"), "{out}");
        assert!(needs_approval("run_command") && !needs_approval("read_file"));

        // Plugin tools join the registry and cannot shadow a built-in.
        struct Extra;
        impl Tool for Extra {
            fn name(&self) -> &'static str {
                "extra"
            }
            fn description(&self) -> &'static str {
                "test"
            }
            fn parameters(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }
            fn run(&self, _: &Value, _: &ToolContext) -> Result<String, String> {
                Ok("from a plugin".into())
            }
        }
        struct Impostor;
        impl Tool for Impostor {
            fn name(&self) -> &'static str {
                "read_file"
            }
            fn description(&self) -> &'static str {
                "test"
            }
            fn parameters(&self) -> Value {
                json!({"type": "object", "properties": {}})
            }
            fn run(&self, _: &Value, _: &ToolContext) -> Result<String, String> {
                Ok("hijacked".into())
            }
        }
        set_plugin_tools(vec![Arc::new(Extra), Arc::new(Impostor)]);
        assert_eq!(run("extra", "{}", &ctx).unwrap(), "from a plugin");
        assert_eq!(
            run("read_file", r#"{"path":"src/a.txt"}"#, &ctx).unwrap(),
            "1|hello\n2|World"
        );
        set_plugin_tools(Vec::new());
        assert!(run("extra", "{}", &ctx).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_multibyte_character_split_across_reads_survives() {
        // The é (0xC3 0xA9) comes in two reads, as it can from a pipe; the
        // bytes are joined before they are decoded.
        let mut kept = Captured::default();
        kept.push(b"caf\xc3");
        kept.push(b"\xa9 done");
        assert_eq!(kept.into_text(), ("café done".to_owned(), None));
    }

    #[test]
    fn invalid_bytes_become_replacement_characters_not_an_error() {
        // 0xE9 is é in Latin-1 and Windows-1252; alone it is not UTF-8.
        let print = if cfg!(windows) {
            "[Console]::OpenStandardOutput().Write([byte[]](0x63,0x61,0x66,0xE9,0x0A), 0, 5); \
             [Console]::OpenStandardError().Write([byte[]](0x62,0x61,0x64,0xE9,0x0A), 0, 5)"
        } else {
            r"printf 'caf\351\n'; printf 'bad\351\n' >&2"
        };
        let out = run_shell(print, None, &Cancel::default()).unwrap();
        assert_eq!(out, "caf\u{FFFD}\n\n[stderr]\nbad\u{FFFD}\n");
    }

    #[test]
    fn commands_are_asked_for_utf8_output() {
        let print = if cfg!(windows) {
            r#"Write-Output "café $env:PYTHONIOENCODING""#
        } else {
            r#"printf 'café %s\n' "$PYTHONIOENCODING""#
        };
        let out = run_shell(print, None, &Cancel::default()).unwrap();
        assert_eq!(out.trim_end(), "café utf-8:surrogateescape");
    }

    #[test]
    fn a_script_that_must_open_with_param_or_using_runs_as_it_is() {
        for kept in [
            "param($name) Write-Output $name",
            "  PARAM ($name) $name",
            "using namespace System.Text\n[StringBuilder]::new()",
        ] {
            assert_eq!(with_utf8_output(kept), kept);
        }
        for led in ["Get-ChildItem", "parameters.ps1", "usingcmd"] {
            let script = with_utf8_output(led);
            assert!(
                script.starts_with(UTF8_PRELUDE) && script.ends_with(led),
                "{script}"
            );
        }
    }

    #[test]
    fn letronnas_own_folder_is_out_of_reach() {
        let dir = std::env::temp_dir().join(format!("letronna-private-{}", std::process::id()));
        let data = dir.join("Letronna");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::write(data.join("keys.json"), "{}").unwrap();
        let keys = std::fs::canonicalize(data.join("keys.json")).unwrap();
        let beside = std::fs::canonicalize(&dir).unwrap();
        assert!(private(&keys, &data));
        assert!(!private(&beside, &data));
        // No such folder makes nothing private, rather than everything.
        assert!(!private(&keys, &dir.join("missing")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_file_pages_through_a_long_file_by_line() {
        let dir = std::env::temp_dir().join(format!("letronna-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let lines: Vec<String> = (1..=3000).map(|n| format!("line {n}")).collect();
        std::fs::write(dir.join("long.txt"), lines.join("\r\n") + "\r\n").unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let read = |args: Value| ReadFile.run(&args, &ctx);

        // A page stops under the wire's cap and says where the rest starts.
        let first = read(json!({"path": "long.txt"})).unwrap();
        assert!(first.starts_with("1|line 1\n2|line 2\n"), "{first}");
        assert!(first.len() < crate::chat::MAX_TOOL_CHARS, "{}", first.len());
        let (body, footer) = first.rsplit_once("\n\n").unwrap();
        let last = body.lines().count();
        assert_eq!(
            footer,
            format!("Lines 1-{last} of 3000. Continue with offset={}.", last + 1)
        );
        let next = read(json!({"path": "long.txt", "offset": last + 1})).unwrap();
        assert!(
            next.starts_with(&format!("{0}|line {0}\n", last + 1)),
            "{next}"
        );

        assert_eq!(
            read(json!({"path": "long.txt", "offset": 10, "limit": 2})).unwrap(),
            "10|line 10\n11|line 11\n\nLines 10-11 of 3000. Continue with offset=12."
        );
        // The last page needs no footer, and past it there is nothing to read.
        assert_eq!(
            read(json!({"path": "long.txt", "offset": 2999})).unwrap(),
            "2999|line 2999\n3000|line 3000"
        );
        let past = read(json!({"path": "long.txt", "offset": 3001})).unwrap_err();
        assert!(past.contains("3000 lines"), "{past}");

        // A line too long to show whole is cut on a character, and says so.
        let wide = format!("{}\nend", "é".repeat(5 * LINE_CHARS));
        std::fs::write(dir.join("wide.txt"), wide).unwrap();
        assert_eq!(
            read(json!({"path": "wide.txt"})).unwrap(),
            format!("1|{} … [line cut]\n2|end", "é".repeat(LINE_CHARS))
        );
        // A character split where the read stopped, or a bad byte past the
        // cut, is never shown, so it gets no note.
        let split = format!("{}\nend", "中".repeat(3 * LINE_CHARS));
        std::fs::write(dir.join("split.txt"), split).unwrap();
        assert_eq!(
            read(json!({"path": "split.txt"})).unwrap(),
            format!("1|{} … [line cut]\n2|end", "中".repeat(LINE_CHARS))
        );
        let bad = [&b"a".repeat(2 * LINE_CHARS)[..], b"\xFF\nend"].concat();
        std::fs::write(dir.join("bad.txt"), bad).unwrap();
        assert_eq!(
            read(json!({"path": "bad.txt"})).unwrap(),
            format!("1|{} … [line cut]\n2|end", "a".repeat(LINE_CHARS))
        );
        std::fs::write(dir.join("empty.txt"), "").unwrap();
        assert_eq!(
            read(json!({"path": "empty.txt"})).unwrap(),
            "The file is empty."
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_file_drops_a_byte_order_mark_and_decodes_utf16() {
        let dir = std::env::temp_dir().join(format!("letronna-read-bom-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let read = |path: &str| ReadFile.run(&json!({"path": path}), &ctx);
        let text = "\u{FEFF}é\r\nz";
        let utf16 =
            |unit: fn(u16) -> [u8; 2]| -> Vec<u8> { text.encode_utf16().flat_map(unit).collect() };
        std::fs::write(dir.join("bom.txt"), text).unwrap();
        std::fs::write(dir.join("le.txt"), utf16(u16::to_le_bytes)).unwrap();
        std::fs::write(dir.join("be.txt"), utf16(u16::to_be_bytes)).unwrap();
        for path in ["bom.txt", "le.txt", "be.txt"] {
            assert_eq!(read(path).unwrap(), "1|é\n2|z", "{path}");
        }

        // Text that is not UTF-8 still reads, with a note; a NUL means binary.
        std::fs::write(dir.join("latin1.txt"), b"caf\xE9").unwrap();
        let latin1 = read("latin1.txt").unwrap();
        assert!(latin1.starts_with("1|caf\u{FFFD}\n\n["), "{latin1}");
        std::fs::write(dir.join("bin.dat"), b"a\0b").unwrap();
        assert_eq!(read("bin.dat").unwrap_err(), "Not a text file.");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_file_says_it_reads_only_the_start_of_a_large_utf16_file() {
        let dir = std::env::temp_dir().join(format!("letronna-read-cut-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let read = |args: Value| ReadFile.run(&args, &ctx);
        // Each line is 16 bytes in UTF-16, so the cap falls on a line's end.
        let kept = UTF16_BYTES as usize / 16;
        let write = |lines: usize| {
            let text = format!("\u{FEFF}{}", "line 01\n".repeat(lines));
            let bytes: Vec<u8> = text.encode_utf16().flat_map(u16::to_le_bytes).collect();
            std::fs::write(dir.join("big.txt"), bytes).unwrap();
        };

        // Every page of a cut file says so, and none counts its lines.
        write(kept + 100);
        let note = "[Only the first 16 MB of this UTF-16 file is read.]";
        assert_eq!(
            read(json!({"path": "big.txt", "limit": 2})).unwrap(),
            format!("1|line 01\n2|line 01\n\n{note}\n\nLines 1-2. Continue with offset=3.")
        );
        assert_eq!(
            read(json!({"path": "big.txt", "offset": kept})).unwrap(),
            format!("{kept}|line 01\n\n{note}")
        );
        let past = read(json!({"path": "big.txt", "offset": kept + 1})).unwrap_err();
        assert!(past.starts_with("The first 16 MB"), "{past}");

        // A file that fills the cap exactly is read whole.
        write(kept);
        assert_eq!(
            read(json!({"path": "big.txt", "offset": kept})).unwrap(),
            format!("{kept}|line 01")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn read_file_suggests_names_like_a_missing_one() {
        let dir = std::env::temp_dir().join(format!("letronna-read-near-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("src")).unwrap();
        for name in ["tools.rs", "toolbox.rs", "other.rs"] {
            std::fs::write(dir.join("src").join(name), "").unwrap();
        }
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let missing = |path: &str| ReadFile.run(&json!({"path": path}), &ctx).unwrap_err();
        assert_eq!(
            missing("src/tool.rs"),
            "`src/tool.rs` does not exist. Did you mean `src\\toolbox.rs`, `src\\tools.rs`?"
                .replace("src\\", &format!("src{}", std::path::MAIN_SEPARATOR))
        );
        assert_eq!(missing("src/zzz.rs"), "`src/zzz.rs` does not exist.");
        assert_eq!(
            missing("nowhere/tool.rs"),
            "`nowhere/tool.rs` does not exist."
        );
        // A folder outside the workspace and home is never listed, though
        // its root has a name that starts this way.
        let outside = if cfg!(windows) { "/Windo" } else { "/us" };
        assert_eq!(missing(outside), format!("`{outside}` does not exist."));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn edit_file_makes_every_edit_or_none_and_keeps_the_file_as_it_was_written() {
        let dir = std::env::temp_dir().join(format!("letronna-edit-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.rs");
        std::fs::write(&file, "\u{FEFF}fn a() {\r\n    one();\r\n}\r\n").unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let edit = |args: Value| EditFile.run(&args, &ctx);
        let both = json!({"path": "a.rs", "edits": [
            {"old": "one();\n}", "new": "two();\n}"},
            {"old": "fn a()", "new": "fn b()"}
        ]});
        let preview = EditFile.preview(&both, &ctx);
        assert!(
            preview.contains("- fn a()") && preview.contains("+ two();"),
            "{preview}"
        );
        assert_eq!(edit(both).unwrap(), "Edited 2 places.");
        let written = "\u{FEFF}fn b() {\r\n    two();\r\n}\r\n";
        assert_eq!(std::fs::read_to_string(&file).unwrap(), written);

        let again = json!({"path": "a.rs", "old": "one();", "new": "two();"});
        assert_eq!(edit(again).unwrap(), "Already applied; nothing changed.");
        let half = json!({"path": "a.rs", "edits": [
            {"old": "fn b", "new": "fn c"},
            {"old": "missing", "new": "x"}
        ]});
        assert!(edit(half).is_err());
        assert_eq!(std::fs::read_to_string(&file).unwrap(), written);

        let every = json!({"path": "a.rs", "old": "()", "new": "(x)", "replace_all": true});
        assert_eq!(edit(every).unwrap(), "Edited 2 places.");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "\u{FEFF}fn b(x) {\r\n    two(x);\r\n}\r\n"
        );
        assert!(edit(json!({"path": "a.rs"})).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn write_file_keeps_the_files_format_and_neither_tool_breaks_a_manifest() {
        let dir = std::env::temp_dir().join(format!("letronna-write-tool-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.cs"), "\u{FEFF}class A {}\r\n").unwrap();
        std::fs::write(dir.join("package.json"), "{\"a\": 1}\n").unwrap();
        std::fs::write(dir.join("Cargo.toml"), "[package]\nname = \"a\"\n").unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let write = |args: Value| WriteFile.run(&args, &ctx);
        write(json!({"path": "a.cs", "content": "class B {\n}\n"})).unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.join("a.cs")).unwrap(),
            "\u{FEFF}class B {\r\n}\r\n"
        );

        let err = write(json!({"path": "package.json", "content": "{\"a\": 1,}"})).unwrap_err();
        assert!(err.contains("nothing was written"), "{err}");
        assert!(write(json!({"path": "new/tsconfig.json", "content": "{\n// ok\n}"})).is_ok());
        let edit = json!({"path": "Cargo.toml", "old": "name = \"a\"", "new": "name = a"});
        assert!(EditFile.run(&edit, &ctx).is_err());
        assert_eq!(
            std::fs::read_to_string(dir.join("package.json")).unwrap(),
            "{\"a\": 1}\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.join("Cargo.toml")).unwrap(),
            "[package]\nname = \"a\"\n"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_shared_chat_reaches_nothing_on_this_computer() {
        use crate::chat::History;
        let names = |remote: Option<Remote>| -> Vec<String> {
            specs(&Connection {
                tools: true,
                remote,
                ..Default::default()
            })
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap().to_owned())
            .collect()
        };
        let history = Some(History::default());
        let local = names(None);
        assert!(local.contains(&"read_file".to_owned()));
        assert!(!local.contains(&"read_chat".to_owned()));

        let dm = names(Some(Remote {
            history: history.clone(),
            ..Default::default()
        }));
        assert!(dm.contains(&"read_file".to_owned()) && dm.contains(&"read_chat".to_owned()));

        let server = names(Some(Remote {
            shared: true,
            history,
            ..Default::default()
        }));
        for kept in ["read_chat", "ask_user", "todo"] {
            assert!(server.contains(&kept.to_owned()), "{kept} missing");
        }
        for gone in [
            "read_file",
            "list_dir",
            "search",
            "write_file",
            "run_command",
        ] {
            assert!(
                !server.contains(&gone.to_owned()),
                "{gone} offered in a shared chat"
            );
        }
        // A Telegram group: shared, and no history to read.
        let group = names(Some(Remote {
            shared: true,
            ..Default::default()
        }));
        assert!(!group.contains(&"read_chat".to_owned()));

        // Named anyway, it is refused rather than run.
        let shared = ToolContext {
            remote: Some(Remote {
                shared: true,
                ..Default::default()
            }),
            ..Default::default()
        };
        let refused = run("read_file", r#"{"path": "x"}"#, &shared).unwrap_err();
        assert!(refused.contains("not available here"), "{refused}");
    }

    // Windows keeps the temp folder under AppData, which is hidden itself.
    #[cfg(unix)]
    #[test]
    fn a_workspace_holding_home_is_searched_like_home() {
        let dir = std::env::temp_dir().join(format!("letronna-holds-home-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".config/tool")).unwrap();
        for name in ["notes.txt", ".zshrc", ".config/tool/state"] {
            std::fs::write(dir.join(name), "token").unwrap();
        }
        let dir = std::fs::canonicalize(dir).unwrap();
        let top = dir.ancestors().last().unwrap().to_path_buf();
        let ctx = ToolContext {
            workspace: Some(top),
            ..Default::default()
        };
        let search = |path: &Path| Search.run(&json!({"pattern": "token", "path": path}), &ctx);
        let found = search(&dir).unwrap();
        assert!(
            found.contains("notes.txt") && !found.contains(".zshrc"),
            "{found}"
        );
        let refused = search(&dir.join(".config/tool")).unwrap_err();
        assert!(
            refused.contains("hidden folder outside the workspace"),
            "{refused}"
        );
        // A workspace that does not hold it searches its own hidden files.
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let found = Search.run(&json!({"pattern": "token"}), &ctx).unwrap();
        assert!(
            found.contains(".zshrc") && found.contains("state"),
            "{found}"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn commands_start_with_no_pager_no_editor_and_no_colour() {
        let print = if cfg!(windows) {
            r#"Write-Output "$env:PAGER $env:GIT_PAGER $env:GIT_EDITOR $env:NO_COLOR $env:TERM""#
        } else {
            r#"echo "$PAGER $GIT_PAGER $GIT_EDITOR $NO_COLOR $TERM""#
        };
        let out = shell(print, None).output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&out.stdout).trim(),
            "cat cat true 1 dumb"
        );
    }

    #[test]
    fn escape_sequences_go_and_the_text_between_them_stays() {
        for (raw, plain) in [
            ("\x1b[1;31merror\x1b[0m: bad", "error: bad"),
            ("\x1b[2K\x1b[?25lnext", "next"),
            ("\x1b]0;a title\x07after", "after"),
            (
                "\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\ done",
                "link done",
            ),
            ("\x1b]0;cut short\x1b[32mgreen", "green"),
            ("ok\n\x1b]0;no end\nline2\nline3\n", "ok\n\nline2\nline3\n"),
            ("héllo — ✓", "héllo — ✓"),
            // An escape that starts neither kind is left for the reader to see.
            ("a\x1bb", "a\x1bb"),
            // A killed process can stop halfway through one.
            ("text\x1b[3", "text"),
        ] {
            assert_eq!(strip_escapes(raw), plain, "{raw:?}");
        }
    }

    #[test]
    fn a_commands_colour_and_title_are_stripped_from_what_the_model_reads() {
        let print = if cfg!(windows) {
            r#"$e = [char]27; Write-Output "$e]0;title$([char]7)$e[31mred$e[0m plain""#
        } else {
            r#"printf '\033]0;title\007\033[31mred\033[0m plain\n'"#
        };
        let out = run_shell(print, None, &Cancel::default()).unwrap();
        assert_eq!(out.trim_end(), "red plain");
    }

    #[test]
    fn a_catastrophic_command_is_refused_however_it_was_approved() {
        // No project folder either, so were the refusal missing it still
        // could not start.
        let args = r#"{"command": "Remove-Item -Recurse -Force ~"}"#;
        let refused = run("run_command", args, &ToolContext::default()).unwrap_err();
        assert!(refused.contains("deletes the home folder"), "{refused}");
    }

    #[cfg(unix)]
    #[test]
    fn a_background_child_holding_the_pipe_does_not_hang_the_command() {
        let started = Instant::now();
        let out = run_shell("echo before; sleep 30 & echo $!", None, &Cancel::default()).unwrap();
        let took = started.elapsed();
        let lines: Vec<&str> = out.lines().collect();
        if let Some(pid) = lines.get(2) {
            let _ = std::process::Command::new("kill").arg(pid).status();
        }
        // Waiting on the pipe would take the whole 30 s the child sleeps.
        assert!(took < Duration::from_secs(10), "took {took:?}");
        let note = lines.first().copied().unwrap_or_default();
        assert!(note.contains("was not collected"), "{out}");
        assert_eq!(lines.get(1), Some(&"before"), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn output_written_before_exit_is_kept() {
        // Several chunks' worth, then stderr, right as the shell ends, with a
        // child still holding both pipes so only the grace can end the wait.
        let out = run_shell(
            "sleep 2 & head -c 20000 /dev/zero | tr '\\0' a; echo; echo warned >&2; exit 3",
            None,
            &Cancel::default(),
        )
        .unwrap();
        let (note, rest) = out.split_once('\n').unwrap();
        assert!(note.contains("was not collected"), "{out}");
        let (body, rest) = rest.split_once('\n').unwrap();
        assert_eq!(body, "a".repeat(20_000));
        assert_eq!(rest, "\n[stderr]\nwarned\n\n[exit code 3]");
    }

    /// A workspace of its own for a test, holding `a.txt` with `text` in it.
    fn workspace_with(label: &str, text: &str) -> (PathBuf, ToolContext) {
        let dir = std::env::temp_dir().join(format!("letronna-{label}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), text).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        (dir, ctx)
    }

    #[test]
    fn a_claude_style_file_path_reaches_path() {
        let (dir, ctx) = workspace_with("aliases", "");
        run(
            "write_file",
            r#"{"file_path":"a.txt","file_text":"one two"}"#,
            &ctx,
        )
        .unwrap();
        let edit = r#"{"file_path":"a.txt","old_string":"two","new_string":"three"}"#;
        // The approval shows the same call the tool will run.
        assert_eq!(summary("edit_file", edit), "a.txt");
        assert_eq!(preview("edit_file", edit, &ctx), "- two\n+ three\n");
        run("edit_file", edit, &ctx).unwrap();
        let read = run("read_file", r#"{"file_path":"a.txt"}"#, &ctx).unwrap();
        assert!(read.starts_with("1|one three\n\n"), "{read}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn string_numbers_and_booleans_are_coerced_by_the_schema() {
        let schema = json!({"type": "object", "properties": {
            "background": {"type": "boolean"},
            "id": {"type": "integer"},
            "seconds": {"type": "number"},
            "options": {"type": "array"},
            "command": {"type": "string"}
        }});
        let mut args = json!({"background": "true", "id": " 3 ", "seconds": "2.5",
            "options": "[\"a\", \"b\"]", "command": "42", "other": "1"});
        assert_eq!(coerce(&schema, &mut args).len(), 4);
        assert_eq!(
            args,
            json!({"background": true, "id": 3, "seconds": 2.5,
                "options": ["a", "b"], "command": "42", "other": "1"})
        );
        // What does not read as the type is left for the tool to refuse.
        let mut wrong = json!({"background": "yes", "id": "2.5", "options": "a"});
        assert!(coerce(&schema, &mut wrong).is_empty());
        assert_eq!(wrong["id"], "2.5");
        // And through a real tool: a string of seconds is waited, not 10s.
        let out = run("wait", r#"{"seconds": "0"}"#, &ToolContext::default()).unwrap();
        assert!(out.starts_with("Waited 0s."), "{out}");
    }

    #[test]
    fn a_present_key_is_never_overwritten_by_an_alias() {
        let mut args = json!({"path": "a.txt", "file_path": "b.txt", "old_string": "x"});
        let changed = rename_aliases("edit_file", &mut args);
        assert_eq!(changed, ["`old_string` as `old`"]);
        assert_eq!(
            args,
            json!({"path": "a.txt", "file_path": "b.txt", "old": "x"})
        );
        let (dir, ctx) = workspace_with("present", "kept");
        let both = r#"{"path":"a.txt","file_path":"missing.txt"}"#;
        assert_eq!(run("read_file", both, &ctx).unwrap(), "1|kept");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_result_says_what_was_reinterpreted() {
        let (dir, ctx) = workspace_with("note", "hello");
        assert_eq!(
            run("read_file", r#"{"file_path":"a.txt"}"#, &ctx).unwrap(),
            "1|hello\n\n[Read `file_path` as `path`. Use the tool's own parameter names and types.]"
        );
        let out = run("wait", r#"{"seconds": "0"}"#, &ctx).unwrap();
        assert!(
            out.ends_with(
                "[Read `seconds` as a number. Use the tool's own parameter names and types.]"
            ),
            "{out}"
        );
        // A failure carries the note too, since the model reads that as well.
        let missing = r#"{"file_path":"a.txt","old_string":"zzz","new_string":""}"#;
        let err = run("edit_file", missing, &ctx).unwrap_err();
        assert!(
            err.contains("`file_path` as `path`, `old_string` as `old`, `new_string` as `new`"),
            "{err}"
        );
        // A call in the tool's own terms gets no note.
        assert_eq!(
            run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap(),
            "1|hello"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn every_alias_leads_from_a_name_its_tool_lacks_to_one_it_has() {
        for (theirs, ours, names) in ALIASES {
            for name in *names {
                let tool = find(name).unwrap_or_else(|| panic!("{name} missing"));
                let properties = &tool.parameters()["properties"];
                assert!(properties.get(*ours).is_some(), "{name} has no `{ours}`");
                assert!(properties.get(*theirs).is_none(), "{name} has `{theirs}`");
            }
        }
    }

    #[test]
    fn a_frontend_reads_the_arguments_the_tool_runs_with() {
        let todo = r#"{"items": "[{\"text\": \"a\", \"status\": \"doing\"}]"}"#;
        let args = parse_args_for(TODO, todo).unwrap();
        assert_eq!(args["items"][0]["status"], "doing");
        let ask = r#"{"question": "Which?", "options": "[\"a\", \"b\"]"}"#;
        assert_eq!(
            parse_args_for(ASK_USER, ask).unwrap()["options"],
            json!(["a", "b"])
        );
        // A tool it does not know is parsed as sent.
        let unknown = parse_args_for("nope", r#"{"id": "3"}"#).unwrap();
        assert_eq!(unknown, json!({"id": "3"}));
    }

    #[test]
    fn a_stopped_wait_returns_at_once() {
        let ctx = ToolContext::default();
        let stop = ctx.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            stop.set();
        });
        let started = Instant::now();
        let out = Wait.run(&json!({"seconds": 60}), &ctx).unwrap_err();
        assert!(out.contains("Stopped"), "{out}");
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "waited {:?}",
            started.elapsed()
        );

        // Waiting on a job hears it too, and says the job is still running.
        let sleeper = if cfg!(windows) {
            "Start-Sleep 30"
        } else {
            "sleep 30"
        };
        let id = crate::jobs::start(sleeper, &std::env::temp_dir()).unwrap();
        let out = Wait.run(&json!({"job": id, "seconds": 60, "until": "ready"}), &ctx);
        crate::jobs::stop(id).unwrap();
        let out = out.unwrap_err();
        assert!(out.contains("still running"), "{out}");
    }

    #[test]
    fn read_job_pages_through_a_long_log() {
        let count = if cfg!(windows) {
            "1..300 | ForEach-Object { \"line $_\" }"
        } else {
            "i=1; while [ $i -le 300 ]; do echo line $i; i=$((i+1)); done"
        };
        let id = crate::jobs::start(count, &std::env::temp_dir()).unwrap();
        // Read once it has all been printed, so no page moves while it is read.
        let deadline = Instant::now() + Duration::from_secs(30);
        while !crate::jobs::log(id).unwrap().contains("line 300") && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(100));
        }
        let read = |args: Value| ReadJob.run(&args, &ToolContext::default()).unwrap();
        let page = read(json!({"id": id, "offset": 100, "limit": 3}));
        assert!(
            page.ends_with(" Lines 101 to 103 of 300:\nline 101\nline 102\nline 103"),
            "{page}"
        );
        let end = read(json!({"id": id, "offset": 298}));
        assert!(
            end.ends_with(" Lines 299 to 300 of 300:\nline 299\nline 300"),
            "{end}"
        );
        let past = read(json!({"id": id, "offset": 400, "limit": 10}));
        assert!(
            past.ends_with(" No lines from line 401; the log has 300."),
            "{past}"
        );
    }

    #[test]
    fn a_stopped_command_is_killed_and_says_so() {
        let dir = std::env::temp_dir().join(format!("letronna-stop-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // Only the first mark may appear: the second would mean it ran on.
        let command = if cfg!(windows) {
            "Set-Content started x; Start-Sleep 1; Set-Content finished x"
        } else {
            "echo x > started; sleep 1; echo x > finished"
        };
        let cancel = Cancel::default();
        let stop = cancel.clone();
        let mark = dir.join("started");
        std::thread::spawn(move || {
            // A cold PowerShell start can take seconds, and a stop before the
            // command runs would prove nothing.
            let deadline = Instant::now() + Duration::from_secs(30);
            while !mark.exists() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            stop.set();
        });
        let out = run_shell(command, Some(&dir), &cancel).unwrap_err();
        assert!(out.contains("Stopped"), "{out}");
        assert!(dir.join("started").exists(), "stopped before it ran");
        std::thread::sleep(Duration::from_secs(2));
        assert!(!dir.join("finished").exists(), "it ran on after the stop");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn plan_mode_refuses_a_tool_that_changes_things_even_when_named() {
        let dir = std::env::temp_dir().join(format!("letronna-plan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "hello").unwrap();
        let ctx = ToolContext::of(&Connection {
            workspace: Some(dir.clone()),
            plan: true,
            ..Default::default()
        });
        for (name, args) in [
            ("write_file", r#"{"path":"b.txt","content":"x"}"#),
            ("edit_file", r#"{"path":"a.txt","old":"hello","new":"bye"}"#),
            ("run_command", r#"{"command":"echo hi > c.txt"}"#),
        ] {
            let refused = run(name, args, &ctx).unwrap_err();
            assert!(refused.contains("plan mode"), "{name}: {refused}");
        }
        assert_eq!(std::fs::read_to_string(dir.join("a.txt")).unwrap(), "hello");
        assert!(!dir.join("b.txt").exists() && !dir.join("c.txt").exists());
        // Looking is still allowed.
        assert_eq!(
            run("read_file", r#"{"path":"a.txt"}"#, &ctx).unwrap(),
            "1|hello"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn secret_files_are_refused_but_env_examples_are_not() {
        let dir = std::env::temp_dir().join(format!("letronna-secrets-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        for folder in [
            ".ssh",
            ".aws",
            ".claude",
            ".codex",
            ".config/gh",
            ".docker",
            ".kube",
        ] {
            std::fs::create_dir_all(dir.join(folder)).unwrap();
        }
        let secrets = [
            ".ssh",
            ".ssh/id_ed25519",
            ".aws/credentials",
            ".netrc",
            ".git-credentials",
            ".npmrc",
            ".pypirc",
            ".config/gh/hosts.yml",
            ".docker/config.json",
            ".kube/config",
            ".claude/.credentials.json",
            ".codex/auth.json",
            ".env",
            ".env.local",
            ".ENV.production",
        ];
        let open = [
            ".env.example",
            ".env.sample",
            ".claude/settings.json",
            ".config/gh/config.yml",
        ];
        for name in secrets.iter().skip(1).chain(&open) {
            std::fs::write(dir.join(name), "KEY=value").unwrap();
        }
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        for name in secrets {
            let refused = resolve(name, &ctx).unwrap_err();
            assert!(refused.contains("out of reach"), "{name}: {refused}");
        }
        for name in open {
            let text = ReadFile.run(&json!({"path": name}), &ctx).unwrap();
            assert_eq!(text, "1|KEY=value", "{name}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_write_that_climbs_with_dots_is_refused_on_every_system() {
        let dir = std::env::temp_dir().join(format!("letronna-climb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let ctx = ToolContext {
            workspace: Some(project.clone()),
            ..Default::default()
        };
        for path in [
            "nonexistent/../../x.txt",
            "nonexistent/../x.txt",
            "../x.txt",
        ] {
            let args = json!({"path": path, "content": "escaped"});
            let refused = WriteFile.run(&args, &ctx).unwrap_err();
            assert!(refused.contains("`..`"), "{path}: {refused}");
        }
        assert!(!dir.join("x.txt").exists());
        assert!(!project.join("x.txt").exists());
        assert!(!project.join("nonexistent").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_write_that_lands_outside_the_project_is_refused_and_creates_nothing() {
        let dir = std::env::temp_dir().join(format!("letronna-outside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let project = dir.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let ctx = ToolContext {
            workspace: Some(project.clone()),
            ..Default::default()
        };
        let refuses = |path: &str| {
            let args = json!({"path": path, "content": "escaped"});
            let refused = WriteFile.run(&args, &ctx).unwrap_err();
            assert!(
                refused.contains("outside the project folder"),
                "{path}: {refused}"
            );
        };
        refuses(&dir.join("x.txt").display().to_string());
        // A link inside the project is judged by the folder it leads to.
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&dir, project.join("up")).unwrap();
            refuses("up/x.txt");
        }
        assert!(!dir.join("x.txt").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn writes_stay_out_of_letronnas_folder_and_off_its_program() {
        let dir = std::env::temp_dir().join(format!("letronna-guarded-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("Letronna");
        std::fs::create_dir_all(&data).unwrap();
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(data.join("settings.json"), "{}").unwrap();
        let me = dir.join("bin/letronna.exe");
        std::fs::write(&me, "program").unwrap();
        // The home folder attached as the project holds both.
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let write = |path: &str| resolve_write_in(path, &ctx, &data, Some(&me));
        for path in [
            "Letronna/settings.json",
            "Letronna/mcp.json",
            "Letronna/new/SOUL.md",
        ] {
            let refused = write(path).unwrap_err();
            assert!(refused.contains("own folder"), "{path}: {refused}");
        }
        for path in [
            "bin/letronna.exe",
            "bin/letronna.new.exe",
            "bin/letronna.old.exe",
            "bin/Letronna.NEW.exe",
        ] {
            let refused = write(path).unwrap_err();
            assert!(refused.contains("own program"), "{path}: {refused}");
        }
        for path in [
            "Letronna.md",
            "bin/other.exe",
            "bin/letronna.exe.txt",
            "notes/a.md",
        ] {
            assert_eq!(write(path).unwrap(), dir.join(path), "{path}");
        }

        // And the real thing refuses the program these tests run as.
        let running = std::env::current_exe().unwrap();
        let ctx = ToolContext {
            workspace: running.parent().map(Path::to_path_buf),
            ..Default::default()
        };
        let name = running.file_name().unwrap().to_string_lossy();
        let refused = resolve_write(&name, &ctx).unwrap_err();
        assert!(refused.contains("own program"), "{refused}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_write_through_a_link_that_leads_nowhere_is_refused_and_creates_nothing() {
        let dir = std::env::temp_dir().join(format!("letronna-dangling-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let data = dir.join("Letronna");
        std::fs::create_dir_all(&data).unwrap();
        // A cloned repo can carry this link; writing through it would create
        // the file it names, in Letronna's folder or anywhere else.
        std::os::unix::fs::symlink(data.join("mcp.json"), dir.join("notes.md")).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let refused = resolve_write_in("notes.md", &ctx, &data, None).unwrap_err();
        assert!(refused.contains("leads nowhere"), "{refused}");
        let args = json!({"path": "notes.md", "content": "{}"});
        let refused = WriteFile.run(&args, &ctx).unwrap_err();
        assert!(refused.contains("leads nowhere"), "{refused}");
        assert!(!data.join("mcp.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_plan_is_offered_only_in_plan_mode_and_refused_when_nobody_can_answer() {
        let offered = |plan: bool, remote: Option<Remote>| {
            specs(&Connection {
                tools: true,
                plan,
                remote,
                ..Default::default()
            })
            .iter()
            .any(|t| t["function"]["name"] == EXIT_PLAN)
        };
        assert!(offered(true, None));
        assert!(!offered(false, None));
        // A chat app's DMs stay in plan mode by default, and nobody there can
        // approve a plan.
        assert!(!offered(true, Some(Remote::default())));
        assert!(needs_approval(EXIT_PLAN));

        // Reaching `run` means nobody was asked: it refuses, and says what to do.
        let args = r#"{"plan":"Rename the thing"}"#;
        let unattended = ToolContext {
            plan: true,
            ..Default::default()
        };
        let refused = run(EXIT_PLAN, args, &unattended).unwrap_err();
        assert!(refused.contains("Nobody is here"), "{refused}");
        assert!(refused.contains("Give the plan"), "{refused}");
        let chat_app = ToolContext {
            remote: Some(Remote::default()),
            ..unattended.clone()
        };
        let refused = run(EXIT_PLAN, args, &chat_app).unwrap_err();
        assert!(refused.contains("chat app"), "{refused}");
        assert!(refused.contains("Let messages use tools"), "{refused}");

        // The card and the row show the plan, not its JSON.
        let plan = json!({"plan": "## Rename the thing\n\n1. Edit a.rs"}).to_string();
        assert_eq!(
            preview(EXIT_PLAN, &plan, &unattended),
            "## Rename the thing\n\n1. Edit a.rs"
        );
        assert_eq!(summary(EXIT_PLAN, &plan), "Rename the thing");
    }

    #[test]
    fn a_plan_that_is_cut_off_or_missing_goes_back_to_the_model() {
        let plan = json!({"plan": "## Rename the thing\n\n1. Edit a.rs"}).to_string();
        assert_eq!(
            plan_of(&plan).unwrap(),
            "## Rename the thing\n\n1. Edit a.rs"
        );
        // A long plan can run into the output limit mid-string.
        let cut = plan_of(r#"{"plan":"Rename the thing\n\n1. Ed"#).unwrap_err();
        assert!(cut.contains("cut off"), "{cut}");
        for empty in ["{}", "", r#"{"plan":"  \n"}"#, r#"{"plan":3}"#] {
            let missing = plan_of(empty).unwrap_err();
            assert!(missing.contains("No plan came"), "{empty}: {missing}");
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_network_path_is_refused_before_it_is_touched() {
        let dir = std::env::temp_dir().join(format!("letronna-network-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("bin")).unwrap();
        std::fs::write(dir.join("notes.txt"), "kept").unwrap();
        let me = dir.join("bin").join("letronna.exe");
        std::fs::write(&me, "program").unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        // `.invalid` never resolves, so a regression fails here rather than
        // reaching a real computer.
        for path in [
            r"\\nowhere.invalid\share\a.txt",
            "//nowhere.invalid/share/a.txt",
        ] {
            let refused = resolve(path, &ctx).unwrap_err();
            assert!(refused.contains("another computer"), "{path}: {refused}");
            let refused = resolve_write(path, &ctx).unwrap_err();
            assert!(refused.contains("another computer"), "{path}: {refused}");
        }
        // The file reads by its plain name, so only the check refuses it by
        // its device name.
        let plain = ReadFile.run(&json!({"path": "notes.txt"}), &ctx).unwrap();
        assert_eq!(plain, "1|kept");
        let device = format!(r"\\?\{}", dir.join("notes.txt").display());
        let refused = ReadFile.run(&json!({"path": device}), &ctx).unwrap_err();
        assert!(refused.contains("device path"), "{refused}");
        // Windows writes both as `letronna.new.exe`, the update later moved
        // over the program, but the own-program check saw the name with its
        // dot or stream still on and let them through.
        let data = dir.join("Letronna");
        for (path, reason) in [
            ("bin/letronna.new.exe.", "dot or a space"),
            ("bin/letronna.new.exe::$DATA", "hidden stream"),
        ] {
            let refused = resolve_write_in(path, &ctx, &data, Some(&me)).unwrap_err();
            assert!(refused.contains(reason), "{path}: {refused}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_long_output_is_cut_between_whole_lines() {
        let lines: Vec<String> = (1..=3000)
            .map(|n| format!("row {n} {}", "=".repeat(n % 40)))
            .collect();
        let text = lines.join("\n");
        let out = keep_ends(text, 2_000, "");
        let kept: Vec<&str> = out.lines().filter(|l| !l.starts_with("… [")).collect();
        assert!(kept.len() > 10, "{out}");
        for line in kept {
            assert!(lines.iter().any(|l| l == line), "cut line {line:?}");
        }
    }

    #[test]
    fn long_output_keeps_more_of_its_end_and_says_what_was_left_out() {
        let lines: Vec<String> = (1..=5000).map(|n| format!("line {n}")).collect();
        let text = lines.join("\n") + "\n[exit code 3]";
        let out = keep_ends(text.clone(), MAX_OUTPUT, "");
        assert!(out.len() <= MAX_OUTPUT, "{}", out.len());
        assert!(out.starts_with("line 1\nline 2\n"), "{out}");
        assert!(out.ends_with("line 5000\n[exit code 3]"), "{out}");
        let (head, rest) = out.split_once("… [").unwrap();
        let (marker, tail) = rest.split_once("]\n").unwrap();
        assert!(tail.len() > 2 * head.len(), "{} {}", head.len(), tail.len());
        // The counts add up to exactly what is missing.
        let left = text.len() - head.len() - tail.len();
        let missing = text.lines().count() - head.lines().count() - tail.lines().count();
        assert_eq!(marker, format!("{missing} lines ({left} bytes) left out"));
    }

    #[test]
    fn short_output_comes_back_as_it_was() {
        assert_eq!(keep_ends("a\nb\n".into(), 100, ""), "a\nb\n");
        let full = "x".repeat(100);
        assert_eq!(keep_ends(full.clone(), 100, ""), full);
    }

    #[test]
    fn one_huge_line_is_cut_inside_rather_than_dropped() {
        let out = keep_ends("x".repeat(1_000_000), MAX_OUTPUT, "");
        assert!(out.len() <= MAX_OUTPUT, "{}", out.len());
        let (head, rest) = out.split_once("\n… [1 line (").unwrap();
        let (_, tail) = rest.split_once("]\n").unwrap();
        assert!(head.len() > 1_000 && tail.len() > 3 * 1_000, "{out}");
    }

    #[test]
    fn a_cut_never_splits_a_character() {
        // Lossily decoded bytes and wide characters, cut at every offset.
        let bad = String::from_utf8_lossy(&[b'a', 0xff, 0xfe, b'\n']).into_owned();
        let text = format!("{}{}", bad.repeat(50), "é日🙂".repeat(200));
        for max in 150..400 {
            let out = keep_ends(text.clone(), max, "");
            assert!(out.len() <= max && out.contains(" left out]\n"), "{max}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_command_with_a_long_output_keeps_its_exit_code() {
        let ctx = ToolContext {
            workspace: Some(std::env::temp_dir()),
            ..Default::default()
        };
        let out = run(
            "run_command",
            r#"{"command":"seq 1 20000; echo failed >&2; exit 3"}"#,
            &ctx,
        )
        .unwrap();
        assert!(out.len() <= MAX_OUTPUT, "{}", out.len());
        assert!(out.starts_with("1\n2\n"), "{out}");
        assert!(
            out.ends_with("20000\n\n[stderr]\nfailed\n\n[exit code 3]"),
            "{out}"
        );
    }

    /// A data folder of its own, set as `AGENT_DATA_DIR` while the guard is
    /// held, with a project folder inside it.
    fn data_folder(name: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        let turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("letronna-spill-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
        (turn, dir)
    }

    fn in_conversation(conversation: u64, dir: &Path) -> ToolContext {
        ToolContext {
            workspace: Some(dir.join("project")),
            conversation,
            ..Default::default()
        }
    }

    fn long_text() -> String {
        (1..=5000).map(|n| format!("line {n}\n")).collect()
    }

    /// The file a cut output names as holding all of it.
    fn saved_in(out: &str) -> PathBuf {
        let (_, rest) = out.split_once("; all of it is in ").unwrap();
        PathBuf::from(rest.split_once(", which read_file").unwrap().0)
    }

    #[test]
    fn a_cut_output_is_saved_whole_and_read_file_pages_through_it() {
        let (_turn, dir) = data_folder("saved");
        let ctx = in_conversation(7, &dir);
        assert_eq!(fit("short\n".into(), &ctx), "short\n");
        assert!(!results_dir(7).exists(), "saved an output that was not cut");

        let out = fit(long_text(), &ctx);
        assert!(out.len() <= MAX_OUTPUT, "{}", out.len());
        let path = saved_in(&out);
        assert!(path.starts_with(dir.join("results").join("7")), "{path:?}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), long_text());
        let raw = path.to_str().unwrap();
        let page = run(
            "read_file",
            &json!({"path": raw, "offset": 2500, "limit": 2}).to_string(),
            &ctx,
        )
        .unwrap();
        assert!(
            page.starts_with("2500|line 2500\n2501|line 2501\n"),
            "{page}"
        );

        // Outside a session nothing would remove it, so nothing is saved.
        let out = fit(long_text(), &ToolContext::default());
        assert!(out.contains(" left out]\n") && !out.contains("all of it"));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_saved_output_is_never_written_or_read_from_another_conversation() {
        let (_turn, dir) = data_folder("private");
        let path = saved_in(&fit(long_text(), &in_conversation(7, &dir)));
        let raw = path.to_str().unwrap();
        let write = json!({"path": raw, "content": "changed"}).to_string();
        let read = json!({"path": raw}).to_string();

        // A project folder holding Letronna's own reaches everything but
        // what is private, so it shows the carve-out is for reading only, and
        // for the conversation that saved the output only.
        let holding = |conversation| ToolContext {
            workspace: Some(dir.clone()),
            conversation,
            ..Default::default()
        };
        let refused = run("write_file", &write, &in_conversation(7, &dir)).unwrap_err();
        assert!(refused.contains("outside the project"), "{refused}");
        let refused = run("write_file", &write, &holding(7)).unwrap_err();
        assert!(refused.contains("own folder"), "{refused}");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), long_text());
        assert!(run("read_file", &read, &holding(7)).is_ok());
        for other in [8, 0] {
            let refused = run("read_file", &read, &holding(other)).unwrap_err();
            assert!(refused.contains("own folder"), "{refused}");
        }
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn only_a_command_that_only_reads_runs_with_the_git_guards() {
        let dir = std::env::temp_dir().join(format!("letronna-guards-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        use std::ffi::OsStr;
        // How many of the guards the line runs with, each with its value.
        let guards = |line: &str| {
            let cmd = shell(line, Some(&dir));
            let set: Vec<_> = cmd.get_envs().collect();
            crate::command::GUARDS
                .iter()
                .filter(|(key, value)| set.contains(&(OsStr::new(key), Some(OsStr::new(value)))))
                .count()
        };
        let all = crate::command::GUARDS.len();
        assert_eq!(guards("git status"), all);
        // PowerShell has no `head`, so there the pipe is not known to only read.
        if cfg!(unix) {
            assert_eq!(guards("git log --oneline | head -n 3"), all);
        }
        assert_eq!(guards("git commit -m wip"), 0);
        assert_eq!(guards("git status > status.txt"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The numbers `seq` printed last before the marker in `out` and first
    /// after it, and the lines and bytes the marker says it left out.
    #[cfg(unix)]
    fn around_the_cut(out: &str) -> (usize, usize, usize, usize) {
        let (head, rest) = out.split_once("… [").unwrap();
        let (marker, tail) = rest.split_once(" bytes) left out]\n").unwrap();
        let (lines, bytes) = marker.split_once(" lines (").unwrap();
        let last = head.lines().last().unwrap().parse().unwrap();
        let first = tail.lines().next().unwrap().parse().unwrap();
        (last, first, lines.parse().unwrap(), bytes.parse().unwrap())
    }

    /// What `seq` prints for the numbers after `last` and before `first`.
    #[cfg(unix)]
    fn seq_between(last: usize, first: usize) -> (usize, usize) {
        let bytes = (last + 1..first).map(|n| n.to_string().len() + 1).sum();
        (first - last - 1, bytes)
    }

    #[cfg(unix)]
    #[test]
    fn the_end_of_a_megabyte_of_output_is_kept_and_counted() {
        let ctx = ToolContext {
            workspace: Some(std::env::temp_dir()),
            ..Default::default()
        };
        let command = r#"{"command":"seq 1 150000; exit 3"}"#;
        let out = run("run_command", command, &ctx).unwrap();
        assert!(out.len() <= MAX_OUTPUT, "{}", out.len());
        assert!(out.starts_with("1\n2\n"), "{out}");
        assert!(out.ends_with("149999\n150000\n\n[exit code 3]"), "{out}");
        let (last, first, lines, bytes) = around_the_cut(&out);
        assert_eq!((lines, bytes), seq_between(last, first), "{out}");
    }

    #[cfg(unix)]
    #[test]
    fn output_past_what_is_kept_still_ends_whole_and_counts_right() {
        // Some 6.9 MB, so the middle is never held, only counted.
        let ctx = ToolContext {
            workspace: Some(std::env::temp_dir()),
            ..Default::default()
        };
        let out = run("run_command", r#"{"command":"seq 1 999999"}"#, &ctx).unwrap();
        assert!(out.len() <= MAX_OUTPUT, "{}", out.len());
        assert!(out.starts_with("1\n2\n"), "{out}");
        assert!(out.ends_with("999998\n999999\n"), "{out}");
        let (last, first, lines, bytes) = around_the_cut(&out);
        assert_eq!((lines, bytes), seq_between(last, first), "{out}");
    }

    #[test]
    fn a_chatty_command_is_held_in_bounded_memory() {
        let mut kept = Captured::default();
        let block = b"y\n".repeat(2048);
        for _ in 0..16 * 1024 {
            kept.push(&block);
        }
        let all = 64 * 1024 * 1024;
        assert_eq!((kept.bytes, kept.lines), (all, all / 2));
        assert_eq!((kept.head.len(), kept.tail.len()), (KEEP_HEAD, KEEP_TAIL));
        let capacity = kept.tail.capacity();
        assert!(capacity < 2 * KEEP_TAIL, "{capacity}");
        // The end starts with a whole line, so the one cut in half goes too.
        let (text, gap) = kept.into_text();
        assert_eq!(text.len(), KEEP_HEAD + KEEP_TAIL - 2);
        let bytes = all - text.len();
        let gap = gap.unwrap();
        assert_eq!((gap.at, gap.bytes), (KEEP_HEAD, bytes));
        assert_eq!(gap.lines, bytes / 2);
    }

    #[test]
    fn output_that_fits_what_is_kept_comes_back_whole() {
        let all = "ab\n".repeat((KEEP_HEAD + KEEP_TAIL) / 3);
        let mut kept = Captured::default();
        for chunk in all.as_bytes().chunks(4096) {
            kept.push(chunk);
        }
        assert_eq!(kept.into_text(), (all, None));
    }

    #[cfg(unix)]
    #[test]
    fn random_bytes_past_what_is_kept_are_cut_safely() {
        let ctx = ToolContext {
            workspace: Some(std::env::temp_dir()),
            ..Default::default()
        };
        let command = r#"{"command":"head -c 6000000 /dev/urandom"}"#;
        let out = run("run_command", command, &ctx).unwrap();
        assert!(out.len() <= MAX_OUTPUT, "{}", out.len());
        assert!(out.contains(" left out]\n"), "{out}");
    }

    #[test]
    fn a_denial_passes_on_the_users_reason_cut_to_a_paragraph() {
        assert_eq!(declined(""), DECLINED);
        assert_eq!(declined("  \n "), DECLINED);
        assert_eq!(
            declined(" use the staging database instead\n"),
            "The user declined: use the staging database instead"
        );
        let long = "é".repeat(REASON_CHARS + 5);
        let out = declined(&long);
        let kept = out.strip_prefix("The user declined: ").unwrap();
        assert_eq!(kept.chars().count(), REASON_CHARS + 1);
        assert!(kept.ends_with("é…"), "{kept}");
        // Exactly at the cap is not cut.
        let full = "a".repeat(REASON_CHARS);
        assert!(!declined(&full).ends_with('…'));
        // Either way it still reads as a denial, not a failure.
        assert!(is_declined(DECLINED) && is_declined(&declined("not now")));
        assert!(!is_declined("No such file or directory"));
    }

    #[test]
    fn only_tools_that_bring_in_other_peoples_words_are_fenced() {
        let fenced: Vec<&str> = all()
            .iter()
            .filter(|t| t.untrusted())
            .map(|t| t.name())
            .filter(|name| !name.starts_with("mcp__"))
            .collect();
        // read_chat fences other people's lines itself, and not the user's.
        assert_eq!(fenced, ["web_fetch", "browse", "use_page"]);
        assert!(crate::search::SearchWeb.untrusted());
    }

    #[test]
    fn letronnas_own_error_stays_outside_any_fence() {
        let refused = run("read_chat", "{}", &ToolContext::default()).unwrap_err();
        assert_eq!(refused, "There is no chat to read in this session.");
    }

    #[test]
    fn a_screenshot_keeps_its_file_above_the_fence_and_a_page_cannot_forge_one() {
        let shot = format!(
            "{}/data/media/page-1.jpg\nA screenshot of https://example.com/, 800×600.",
            crate::media::SAVED
        );
        let args = json!({"action": "screenshot"});
        let out = fenced(crate::browser::BROWSE, &args, &shot).unwrap();
        assert_eq!(
            crate::media::saved_by(crate::browser::BROWSE, &out),
            Some(PathBuf::from("/data/media/page-1.jpg"))
        );
        assert!(out.contains("800×600.\n</untrusted-content-"), "{out}");
        // A page titled like the line, looked at rather than photographed.
        let look = fenced(crate::browser::BROWSE, &json!({"action": "look"}), &shot).unwrap();
        assert_eq!(crate::media::saved_by(crate::browser::BROWSE, &look), None);
    }

    #[test]
    fn no_write_puts_the_secret_marker_where_a_key_was() {
        let dir = std::env::temp_dir().join(format!("letronna-marker-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let config = "url = \"https://example.com\"\nkey = \"sk-real-0123456789abcdef\"\n";
        std::fs::write(dir.join("app.toml"), config).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let on_disk = || std::fs::read_to_string(dir.join("app.toml")).unwrap();
        let marker = crate::redact::MARKER;
        // What the model saw, written back whole.
        let seen = config.replace("sk-real-0123456789abcdef", marker);
        let write = json!({"path": "app.toml", "content": seen});
        let refused = WriteFile.run(&write, &ctx).unwrap_err();
        assert!(refused.contains(marker), "{refused}");
        let fresh = json!({"path": "new.toml", "content": format!("key = \"{marker}\"")});
        assert!(WriteFile.run(&fresh, &ctx).is_err());
        assert!(!dir.join("new.toml").exists());

        let line = format!("key = \"{marker}\"");
        let edit = json!({"path": "app.toml", "old": line, "new": "key = \"\""});
        let refused = EditFile.run(&edit, &ctx).unwrap_err();
        assert!(refused.contains("matches nothing"), "{refused}");
        let edit = json!({"path": "app.toml", "old": "https://example.com", "new": marker});
        assert!(EditFile.run(&edit, &ctx).is_err());
        assert_eq!(on_disk(), config);
        // The lines around it can still be changed.
        let edit = json!({"path": "app.toml", "old": "example.com", "new": "example.org"});
        EditFile.run(&edit, &ctx).unwrap();
        assert!(on_disk().contains("sk-real-"));

        let command = json!({"command": format!("echo '{marker}' > app.toml")}).to_string();
        let refused = run("run_command", &command, &ctx).unwrap_err();
        assert!(refused.starts_with("Refused"), "{refused}");
        assert!(on_disk().contains("sk-real-"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
