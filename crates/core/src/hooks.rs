//! The user's own commands, run at points in a session: before and after a
//! tool call, when the model has finished its turn, and when a card waits
//! for the user.
//!
//! `hooks.json` in the data folder takes the shape Claude Code's settings
//! give hooks, so a script written for it carries over: the JSON it reads on
//! stdin, exit code 2 to block with stderr as the reason, and a `matcher` on
//! tool names that also answers to Claude Code's names for the tools that
//! have one. Only the user writes it: the file tools cannot reach the data
//! folder, and a project cannot add hooks, since whoever wrote a repo may
//! not be whoever runs it.
//!
//! A hook can only add a block. The PreToolUse hooks run inside `tools::run`,
//! on the call's worker thread, after the gate -- the rules, the mode, the
//! floor and the card -- so a hook sees only a call that would run, and
//! nothing it prints can let through what the gate stopped: there is no way
//! to say "allow" here at all. Any other failure, a timeout included, is
//! shown to the user and the call goes on, as in Claude Code. A hooks.json
//! that cannot be used, or holds what hooks here cannot be, is never taken
//! for none: until it is fixed the gate holds every call but a question for
//! the user's yes, since a guard in it may be what the user counts on.
//!
//! The file is read once and looked at again, by its modified time, at most
//! once a turn, on the thread that starts it rather than the window's.
//!
//! A command runs as its own words when it holds no shell syntax, which
//! spares every call a shell's start-up (PowerShell's is some 0.3 s), and
//! through the shell `run_command` uses when it has pipes, variables, `~` or
//! globs, starts with one of the shell's own words, or on Windows names no
//! .exe, as scripts written for Claude Code often do. Either way its whole
//! process tree is ended on a timeout or a stop, the way a job's is. A
//! running hook is looked at every `POLL` rather than waited on: a blocking
//! wait would need a thread per hook holding the child, and then the kill
//! could come after the reap, when the group's id may have passed on.

use crate::tools::ToolContext;
use regex::Regex;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

/// The user's hooks, in the data folder.
const FILE: &str = "hooks.json";

/// How long a hook gets unless it names its own `timeout`: Claude Code's
/// default, so a script written for it has the time it expects.
const TIMEOUT: Duration = Duration::from_secs(60);

/// How often a running hook is looked at, as a foreground command is: soon
/// enough after it ends that no one feels the wait.
const POLL: Duration = Duration::from_millis(50);

/// A Stop hook can send the model back to work this many times in a row
/// before the turn ends anyway: room for a fix and a retest or two, while a
/// hook that always blocks cannot keep a turn going for ever.
pub const STOP_BLOCKS: u32 = 3;

/// What the user is told when the model finishes after `STOP_BLOCKS` sends
/// back, and the Stop hooks are not asked a time more.
pub const STOP_CAPPED: &str =
    "Stop hooks sent it back as often as a turn allows, so they were not run again.";

/// What a hook prints is cut to this many bytes before the model or the
/// screen gets it: a quarter of the cap on a tool result, which leaves the
/// tool's own output the rest.
const SAID: usize = crate::chat::MAX_TOOL_CHARS / 4;

/// Failures kept for a frontend to show; past this the oldest go, so
/// calls no frontend reports on, such as a helper's, keep no more than this.
const UNSHOWN: usize = 20;

/// A failure is shown to the user as one line of about this many
/// characters: the hook, how it failed and the end of what it said, as a
/// terminal line or a notice holds them, where a traceback would not fit.
const SHOWN: usize = 160;

/// Words sh runs itself, which as a program are not found or cannot do what
/// they do in the shell, such as `exit 2` or `cd dir`.
const BUILTINS: &[&str] = &[
    "exit", "cd", "test", "[", ".", "source", "export", "eval", "exec", "set", "unset", "read",
    "return", "shift", "trap", "ulimit", "umask", "alias", "command", "type", "wait",
];

/// Claude Code's names for the arguments that have one here, added beside
/// Letronna's in what a hook reads, so `tool_input.file_path` is there.
const CLAUDE_NAMES: &[&str] = &["file_path", "old_string", "new_string", "run_in_background"];

/// What a call gets once a hook has ended its turn, when it was queued
/// behind the one that did.
const ENDED: &str = "Not run: a hook ended the turn.";

/// Claude Code's names for the tools that have a counterpart here, so a
/// matcher written for it, such as `Bash` or `Edit|Write`, still fits.
const ALIASES: &[(&str, &str)] = &[
    ("run_command", "Bash"),
    ("write_file", "Write"),
    ("edit_file", "Edit"),
    ("read_file", "Read"),
    ("search", "Grep"),
    ("web_fetch", "WebFetch"),
    ("search_web", "WebSearch"),
];

/// Hook failures, and what hooks asked to tell the user, that no frontend
/// has shown yet, oldest first.
static FAILURES: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// hooks.json as last read.
static READ: Mutex<Option<Read>> = Mutex::new(None);

/// The conversations whose turn a hook has ended, until their loop has seen it.
static HALTS: Mutex<Vec<u64>> = Mutex::new(Vec::new());

/// What hooks.json held when it was read, and its modified time and length
/// then, or `None` where there was no file.
struct Read {
    stamp: Option<(SystemTime, u64)>,
    hooks: Arc<Result<Hooks, String>>,
}

/// The points in a session a hook can run at.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Event {
    /// A call the gate let through is about to run; a hook may block it.
    PreToolUse,
    /// A call ran and succeeded; a hook may tell the model something.
    PostToolUse,
    /// The model has finished its turn; a hook may send it back to work.
    Stop,
    /// A card waits for the user.
    Notification,
}

impl Event {
    const ALL: [Event; 4] = [
        Self::PreToolUse,
        Self::PostToolUse,
        Self::Stop,
        Self::Notification,
    ];

    /// The event's key in hooks.json, which is Claude Code's name for it.
    fn name(self) -> &'static str {
        match self {
            Self::PreToolUse => "PreToolUse",
            Self::PostToolUse => "PostToolUse",
            Self::Stop => "Stop",
            Self::Notification => "Notification",
        }
    }
}

/// The hooks in hooks.json, by event.
#[derive(Default)]
pub struct Hooks(BTreeMap<String, Vec<Group>>);

/// Hooks that run together when their matcher fits.
struct Group {
    matcher: Matcher,
    hooks: Vec<Hook>,
}

/// The tool names, or kinds of notification, a group runs for.
#[derive(Debug)]
enum Matcher {
    All,
    /// A name, or names joined by `|`, each matched whole.
    Names(Vec<String>),
    /// Anything else, as a regular expression found anywhere in the name.
    Pattern(Regex),
}

#[derive(Deserialize)]
struct Wire {
    hooks: Option<BTreeMap<String, Vec<WireGroup>>>,
    /// Anything else, such as the rest of a settings file pasted whole.
    #[serde(flatten)]
    rest: BTreeMap<String, Value>,
}

#[derive(Deserialize)]
struct WireGroup {
    #[serde(default)]
    matcher: String,
    hooks: Vec<Hook>,
}

/// One hook: a command, and how long it may take.
#[derive(Deserialize)]
struct Hook {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    command: String,
    /// In seconds.
    timeout: Option<u64>,
}

impl Hooks {
    /// Whether any hook is set for `event`, which costs no process.
    pub fn has(&self, event: Event) -> bool {
        self.0
            .get(event.name())
            .is_some_and(|groups| groups.iter().any(|group| !group.hooks.is_empty()))
    }
}

impl Matcher {
    /// Claude Code's reading of a matcher: empty or `*` for everything, plain
    /// names for exactly those, anything else a regular expression.
    fn of(text: &str) -> Result<Self, String> {
        let text = text.trim();
        if text.is_empty() || text == "*" {
            return Ok(Self::All);
        }
        if text
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|')
        {
            return Ok(Self::Names(text.split('|').map(str::to_owned).collect()));
        }
        Regex::new(text)
            .map(Self::Pattern)
            .map_err(|error| format!("the matcher `{text}` is not a regular expression: {error}"))
    }

    /// Whether the group runs for `name`, or for Claude Code's name for it.
    fn fits(&self, name: &str) -> bool {
        // A patch writes and edits files, so what guards those guards it.
        let patch = name == crate::patch::APPLY_PATCH;
        if patch && (self.fits("write_file") || self.fits("edit_file")) {
            return true;
        }
        let fits = |name: &str| match self {
            Self::All => true,
            Self::Names(names) => names.iter().any(|n| n == name),
            Self::Pattern(pattern) => pattern.is_match(name),
        };
        let alias = ALIASES
            .iter()
            .find(|(own, _)| *own == name)
            .map(|(_, theirs)| *theirs);
        fits(name) || alias.is_some_and(fits)
    }
}

/// The user's hooks file in the data folder.
#[cfg(not(test))]
pub fn file() -> PathBuf {
    crate::chat::data_dir().join(FILE)
}

/// In tests, a file that is never there, so no test runs the hooks of
/// whoever runs the tests; the tests of hooks load theirs by path.
#[cfg(test)]
pub fn file() -> PathBuf {
    std::env::temp_dir()
        .join(format!("letronna-no-hooks-{}", std::process::id()))
        .join(FILE)
}

/// The hooks in `file`, none where it does not exist or holds only space. It
/// is only ever read: one that cannot be used is left as it is for the user
/// to fix.
pub fn load(file: &Path) -> Result<Hooks, String> {
    let text = match std::fs::read_to_string(file) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Hooks::default());
        }
        Err(error) => return Err(format!("Could not read {}: {error}", file.display())),
    };
    parse(&text).map_err(|error| format!("{} cannot be used: {error}", file.display()))
}

/// Reads hooks.json's shape, refusing what would leave a hook the user wrote
/// quietly never running: events outside `"hooks"`, an event or a type of
/// hook that does not run here, or a hook with no command.
fn parse(text: &str) -> Result<Hooks, String> {
    // An editor that empties the file before writing it, or a `touch`,
    // leaves nothing to guard with rather than a broken guard.
    if text.trim().is_empty() {
        return Ok(Hooks::default());
    }
    let wire: Wire = serde_json::from_str(text).map_err(|error| error.to_string())?;
    if let Some(event) = Event::ALL
        .iter()
        .find(|event| wire.rest.contains_key(event.name()))
    {
        return Err(format!(
            "`{}` must sit inside \"hooks\", as in Claude Code's settings",
            event.name()
        ));
    }
    let mut hooks = BTreeMap::new();
    for (name, groups) in wire.hooks.unwrap_or_default() {
        if !Event::ALL.iter().any(|event| event.name() == name) {
            return Err(format!(
                "hooks do not run at `{name}` here, only at PreToolUse, PostToolUse, \
                 Stop and Notification"
            ));
        }
        let groups = groups
            .into_iter()
            .map(|group| {
                if let Some(hook) = group.hooks.iter().find(|hook| hook.kind != "command") {
                    return Err(format!(
                        "a {name} hook is of type `{}`; only command hooks run here",
                        hook.kind
                    ));
                }
                if group
                    .hooks
                    .iter()
                    .any(|hook| hook.command.trim().is_empty())
                {
                    return Err(format!("a {name} hook has no command"));
                }
                Ok(Group {
                    matcher: Matcher::of(&group.matcher)?,
                    hooks: group.hooks,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        hooks.insert(name, groups);
    }
    Ok(Hooks(hooks))
}

/// Readies the hooks for a turn in `conversation`: an end a hook asked for
/// in an earlier turn is forgotten, and hooks.json is read again if its
/// modified time or length changed. It reads the disk, so never on a UI
/// thread.
pub fn new_turn(conversation: u64) {
    lock(&HALTS).retain(|&halted| halted != conversation);
    reread();
}

/// Reads hooks.json again if it changed since it was last read.
fn reread() {
    let file = file();
    let stamp = std::fs::metadata(&file).ok().map(|meta| {
        (
            meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            meta.len(),
        )
    });
    if lock(&READ).as_ref().is_some_and(|read| read.stamp == stamp) {
        return;
    }
    // Read with the lock let go, so the gate asking on a window's thread
    // never waits on the disk.
    let hooks = Arc::new(load(&file));
    *lock(&READ) = Some(Read { stamp, hooks });
}

/// The hooks as last read.
fn cached() -> Option<Arc<Result<Hooks, String>>> {
    lock(&READ).as_ref().map(|read| read.hooks.clone())
}

/// The hooks as last read, read now if they never were.
pub(crate) fn current() -> Arc<Result<Hooks, String>> {
    if let Some(hooks) = cached() {
        return hooks;
    }
    reread();
    cached().unwrap_or_else(|| Arc::new(Ok(Hooks::default())))
}

/// Whether the hooks as last read run anything at `event`. It never reads
/// the file, so a window can ask.
pub fn wanted(event: Event) -> bool {
    cached().is_some_and(|hooks| hooks.as_ref().as_ref().is_ok_and(|hooks| hooks.has(event)))
}

/// Why hooks.json cannot be used, as last read, if it cannot. It never reads
/// the file, so the gate can ask on a window's thread.
pub fn trouble() -> Option<String> {
    cached()?.as_ref().as_ref().err().cloned()
}

/// Whether a hook ended the turn in `conversation` since it began, which
/// forgets it: the loop that asks ends the turn rather than ask the model
/// again.
pub fn halted(conversation: u64) -> bool {
    let mut halts = lock(&HALTS);
    let had = halts.contains(&conversation);
    halts.retain(|&halted| halted != conversation);
    had
}

/// Whether a hook ended the turn in `conversation`, still to be seen.
fn halting(conversation: u64) -> bool {
    lock(&HALTS).contains(&conversation)
}

/// Ends the turn `ctx` is in, as a hook's `continue: false` asks. With no
/// conversation in particular there is no loop to end, and nothing is kept
/// that could end another's.
fn halt(ctx: &ToolContext) {
    if ctx.conversation != 0 {
        lock(&HALTS).push(ctx.conversation);
    }
}

/// `mutex` locked, even after a panic elsewhere: what it guards is only ever
/// replaced whole, so it is never left half written.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Runs the PreToolUse hooks for a call the gate let through: `Err` with the
/// reason the model gets when one blocks it or ends the turn, or when
/// hooks.json cannot be used and the user did not say yes to the call;
/// otherwise what they added for the model, as lines to add to the result.
pub(crate) fn before(
    hooks: &Result<Hooks, String>,
    tool: &str,
    args: &Value,
    ctx: &ToolContext,
) -> Result<String, String> {
    let hooks = match hooks {
        Ok(hooks) => hooks,
        // The gate held the call for the user's yes while the file is
        // broken, and a question touches nothing a hook could guard.
        Err(_)
            if ctx.approved
                || [crate::tools::ASK_USER, crate::tools::EXIT_PLAN].contains(&tool) =>
        {
            return Ok(String::new());
        }
        Err(error) => {
            return Err(format!(
                "Refused: {error}. Until it is fixed a call runs only once the user \
                 says yes to it, since a hook in it may be what guards this call."
            ));
        }
    };
    if halting(ctx.conversation) {
        return Err(ENDED.into());
    }
    if !hooks.has(Event::PreToolUse) {
        return Ok(String::new());
    }
    let own = json!({ "tool_name": tool, "tool_input": tool_input(tool, args, ctx) });
    let input = input(Event::PreToolUse, ctx, own);
    let heard = hear(hooks, Event::PreToolUse, tool, &input, ctx);
    report(heard.problems);
    // The call's result, which the user sees as the model does.
    if let Some(why) = heard.halt {
        halt(ctx);
        return Err(format!("A PreToolUse hook ended the turn: {why}"));
    }
    if let Some(why) = heard.blocks.into_iter().next() {
        return Err(format!("A PreToolUse hook blocked this call: {why}"));
    }
    Ok(heard
        .context
        .iter()
        .map(|said| format!("\n\n[PreToolUse hook] {said}"))
        .collect())
}

/// Runs the PostToolUse hooks for a call that succeeded with `output`: what
/// they said for the model, as lines to add to the output, or nothing.
pub(crate) fn after(
    hooks: &Result<Hooks, String>,
    tool: &str,
    args: &Value,
    output: &str,
    ctx: &ToolContext,
) -> String {
    // One that cannot be used has nothing to run; the call ran on the
    // user's yes.
    let Ok(hooks) = hooks else {
        return String::new();
    };
    if !hooks.has(Event::PostToolUse) {
        return String::new();
    }
    let args = tool_input(tool, args, ctx);
    let own = json!({ "tool_name": tool, "tool_input": args, "tool_response": output });
    let input = input(Event::PostToolUse, ctx, own);
    let heard = hear(hooks, Event::PostToolUse, tool, &input, ctx);
    report(heard.problems);
    let mut said: String = heard
        .blocks
        .into_iter()
        .chain(heard.context)
        .map(|said| format!("\n\n[PostToolUse hook] {said}"))
        .collect();
    if let Some(why) = heard.halt {
        let why = format!("A PostToolUse hook ended the turn: {why}");
        said.push_str(&format!("\n\n[{why}]"));
        report(vec![why]);
        halt(ctx);
    }
    said
}

/// Runs the Stop hooks once the model has finished a turn with `last` as its
/// reply: what one says is left to do, as the message that sends the model
/// back to it, if any. `again` when a Stop hook already did this turn.
pub fn stop(ctx: &ToolContext, last: &str, again: bool) -> Option<String> {
    let hooks = current();
    // One that cannot be used is shown to the user beside the cards it holds.
    let Ok(hooks) = hooks.as_ref() else {
        return None;
    };
    if !hooks.has(Event::Stop) {
        return None;
    }
    let own = json!({ "stop_hook_active": again, "last_assistant_message": last });
    let heard = hear(hooks, Event::Stop, "", &input(Event::Stop, ctx, own), ctx);
    report(heard.problems);
    // The turn is ending anyway; `continue: false` only keeps it ended.
    if let Some(why) = heard.halt {
        report(vec![format!("A Stop hook ended the turn: {why}")]);
        return None;
    }
    let why = heard.blocks.into_iter().next()?;
    Some(format!("Stop hook feedback:\n{why}"))
}

/// Runs the Notification hooks for the card a call of `tool` puts up, on a
/// thread of their own, since nothing waits on them; their failures are
/// kept for `problems`.
pub fn card(ctx: ToolContext, tool: &str) {
    if !wanted(Event::Notification) {
        return;
    }
    let brand = crate::BRAND;
    let (kind, message) = if tool == crate::tools::ASK_USER {
        ("idle_prompt", format!("{brand} has a question for you"))
    } else {
        (
            "permission_prompt",
            format!("{brand} needs your permission to use {tool}"),
        )
    };
    // A stop of its own, so ending the turn does not cut a message to the
    // user's phone off half sent.
    let ctx = ToolContext {
        cancel: crate::cancel::Cancel::default(),
        ..ctx
    };
    std::thread::spawn(move || notify(&ctx, kind, &message));
}

/// Runs the Notification hooks: `kind` is Claude Code's `notification_type`,
/// and `message` what the notification says.
fn notify(ctx: &ToolContext, kind: &str, message: &str) {
    let hooks = current();
    let Ok(hooks) = hooks.as_ref() else {
        return;
    };
    let own = json!({ "notification_type": kind, "message": message });
    let input = input(Event::Notification, ctx, own);
    let mut heard = hear(hooks, Event::Notification, kind, &input, ctx);
    // Nothing waits on a notification, so exit code 2 is only a failure.
    heard.problems.extend(
        heard
            .blocks
            .into_iter()
            .map(|why| format!("A Notification hook tried to block: {why}")),
    );
    report(heard.problems);
}

/// The hook failures since the last look, oldest first, each one short line
/// for the user.
pub fn problems() -> Vec<String> {
    std::mem::take(&mut *lock(&FAILURES))
}

/// Keeps `problems` for a frontend to show, each cut to one line, dropping
/// the oldest past `UNSHOWN`.
fn report(problems: Vec<String>) {
    if problems.is_empty() {
        return;
    }
    let mut kept = lock(&FAILURES);
    kept.extend(problems.iter().map(|problem| shown(problem)));
    let over = kept.len().saturating_sub(UNSHOWN);
    kept.drain(..over);
}

/// `problem` on one line of at most about `SHOWN` characters, keeping its
/// start, which names the hook and how it failed, and its end, where a
/// script's error usually is.
fn shown(problem: &str) -> String {
    let line = crate::tools::one_line(problem);
    let count = line.chars().count();
    if count <= SHOWN {
        return line;
    }
    let head: String = line.chars().take(SHOWN / 2).collect();
    let tail: String = line.chars().skip(count - SHOWN / 2).collect();
    format!("{} … {}", head.trim_end(), tail.trim_start())
}

/// The JSON a hook reads on stdin: the fields Claude Code sends every hook,
/// then `own`, the event's.
fn input(event: Event, ctx: &ToolContext, own: Value) -> Value {
    let cwd = ctx
        .workspace
        .clone()
        .or_else(|| std::env::current_dir().ok())
        .map(|dir| dir.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mut input = json!({
        "session_id": ctx.conversation.to_string(),
        "cwd": cwd,
        "hook_event_name": event.name(),
    });
    // The session's file, for a hook that reads back what was said; a
    // helper's run keeps its own elsewhere.
    let transcript = crate::store::sessions_dir().join(format!("{}.jsonl", ctx.conversation));
    if ctx.conversation != 0 && transcript.is_file() {
        input["transcript_path"] = transcript.to_string_lossy().into();
    }
    if let (Some(input), Value::Object(own)) = (input.as_object_mut(), own) {
        input.extend(own);
    }
    input
}

/// The call's arguments as a hook reads them: Letronna's, with Claude Code's
/// names beside them where it has others, and the file as the absolute path
/// Claude Code gives, so a guard written for its Edit or Write finds it.
fn tool_input(tool: &str, args: &Value, ctx: &ToolContext) -> Value {
    let mut input = args.clone();
    crate::tools::add_aliases(tool, &mut input, CLAUDE_NAMES);
    let Some(object) = input.as_object_mut() else {
        return input;
    };
    if let Some(path) = object.get("file_path").and_then(Value::as_str) {
        let path = match &ctx.workspace {
            Some(dir) => dir.join(path),
            None => PathBuf::from(path),
        };
        object.insert("file_path".into(), path.to_string_lossy().into());
    }
    // One replacement in a list is Claude Code's Edit all the same.
    if let Some([edit]) = args["edits"].as_array().map(Vec::as_slice)
        && !object.contains_key("old_string")
    {
        object.insert("old_string".into(), edit["old"].clone());
        object.insert("new_string".into(), edit["new"].clone());
    }
    input
}

/// What an event's hooks made of it.
#[derive(Debug, Default)]
struct Heard {
    /// Why hooks blocked: exit code 2's stderr, or the reason JSON gave.
    blocks: Vec<String>,
    /// Why a hook asked to end the turn, with `continue: false`.
    halt: Option<String>,
    /// What a hook added for the model without blocking.
    context: Vec<String>,
    /// Hooks that failed without blocking, for the user.
    problems: Vec<String>,
}

/// Runs the hooks set for `event` whose matcher fits `subject`, one after
/// another. A PreToolUse or Stop hook's block or end is the answer, so the
/// rest do not run; PostToolUse and Notification hooks run whatever an
/// earlier one said, as a formatter after a linter should.
fn hear(hooks: &Hooks, event: Event, subject: &str, input: &Value, ctx: &ToolContext) -> Heard {
    let mut heard = Heard::default();
    let input = input.to_string();
    let groups = hooks.0.get(event.name()).map_or(&[][..], Vec::as_slice);
    // Claude Code runs every Stop hook whatever its matcher says.
    let fitting = groups
        .iter()
        .filter(|group| event == Event::Stop || group.matcher.fits(subject));
    let decisive = matches!(event, Event::PreToolUse | Event::Stop);
    for hook in fitting.flat_map(|group| &group.hooks) {
        // The user's own stop is no failure to tell them of; `tools::run`
        // keeps a call stopped here from running.
        if ctx.cancel.is_set() {
            break;
        }
        let shown: String = crate::tools::one_line(&hook.command)
            .chars()
            .take(60)
            .collect();
        let failed = |why: &str| format!("The {} hook `{shown}` {why}", event.name());
        let ran = match spawn(hook, &input, ctx) {
            Ok(ran) => ran,
            Err(_) if ctx.cancel.is_set() => break,
            Err(why) if event == Event::PreToolUse => {
                heard
                    .problems
                    .push(failed(&format!("{why}, so the call went on without it.")));
                continue;
            }
            Err(why) => {
                heard.problems.push(failed(&format!("{why}.")));
                continue;
            }
        };
        match ran.code {
            Some(0) => said(&ran.stdout, ctx.approved, &mut heard),
            Some(2) if ran.stderr.trim().is_empty() => heard
                .blocks
                .push(format!("`{shown}` exited with code 2 and said nothing.")),
            Some(2) => heard.blocks.push(clip(&ran.stderr)),
            code => {
                let code = code.map_or("was killed by a signal".into(), |code| {
                    format!("failed with exit code {code}")
                });
                let stderr = clip(&ran.stderr);
                let tail = if stderr.is_empty() { "." } else { ": " };
                heard
                    .problems
                    .push(failed(&format!("{code}{tail}{stderr}")));
            }
        }
        if decisive && (!heard.blocks.is_empty() || heard.halt.is_some()) {
            break;
        }
    }
    heard
}

/// Reads the JSON a hook that exited 0 printed, as Claude Code does: an end
/// to the turn, a block or a denial with its reason, context for the model,
/// or a message for the user. "allow" means nothing here, and "ask" blocks
/// unless the user has just said yes to this very call on its card, since no
/// card can be shown once it runs. A call a hook would rewrite is blocked:
/// the gate approved the call as it was, not as the hook would make it.
fn said(stdout: &str, approved: bool, heard: &mut Heard) {
    let Ok(out) = serde_json::from_str::<Value>(stdout.trim()) else {
        return;
    };
    let text = |value: &Value| clip(value.as_str().unwrap_or_default());
    if out["continue"] == false {
        let reason = text(&out["stopReason"]);
        heard.halt = Some(if reason.is_empty() {
            "a hook gave no reason.".into()
        } else {
            reason
        });
    }
    let own = &out["hookSpecificOutput"];
    let decision = own["permissionDecision"].as_str();
    let ask = decision == Some("ask") && !approved;
    if !own["updatedInput"].is_null() {
        heard.blocks.push(
            "a hook asked to change this call's input, which Letronna does not do, so the \
             call did not run."
                .into(),
        );
    } else if out["decision"] == "block" || decision == Some("deny") || ask {
        let reason = [&out["reason"], &own["permissionDecisionReason"]]
            .into_iter()
            .map(text)
            .find(|reason| !reason.is_empty());
        heard.blocks.push(match reason {
            Some(reason) => reason,
            None if ask => "a hook wants the user asked about this call first.".into(),
            None => "a hook gave no reason.".into(),
        });
    }
    let context = text(&own["additionalContext"]);
    if !context.is_empty() {
        heard.context.push(context);
    }
    let message = text(&out["systemMessage"]);
    if !message.is_empty() {
        heard.problems.push(format!("A hook says: {message}"));
    }
}

/// `text` trimmed and cut to `SAID` bytes, keeping its end, where a script's
/// error usually is.
fn clip(text: &str) -> String {
    let text = text.trim();
    if text.len() <= SAID {
        return text.to_owned();
    }
    let from = (text.len() - SAID..text.len())
        .find(|&at| text.is_char_boundary(at))
        .unwrap_or(text.len());
    format!("…{}", &text[from..])
}

/// How a hook's process ended.
struct Ran {
    code: Option<i32>,
    stdout: String,
    stderr: String,
}

/// Runs `hook` with `input` on its stdin, in the session's folder, ending
/// its whole process tree when it outlives its timeout or the call is
/// stopped; `Err` says why it did not finish.
fn spawn(hook: &Hook, input: &str, ctx: &ToolContext) -> Result<Ran, String> {
    let cwd = ctx.workspace.as_deref();
    let windows = cfg!(windows);
    let path = std::env::var_os("PATH");
    let words = argv(&hook.command, windows).filter(|words| {
        words.first().is_some_and(|program| {
            startable(program, windows, path.as_deref(), &|file| file.is_file())
        })
    });
    let mut command = match words {
        Some(words) => {
            let Some((program, args)) = words.split_first() else {
                return Err("has no command".into());
            };
            let mut command = std::process::Command::new(program);
            command.args(args);
            if let Some(dir) = cwd {
                command.current_dir(dir);
            }
            no_window(&mut command);
            command
        }
        None => crate::tools::shell(&shell_line(&hook.command, windows), cwd),
    };
    // Scripts written for Claude Code find the project through this.
    if let Some(dir) = cwd {
        command.env("CLAUDE_PROJECT_DIR", dir);
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut command, 0);
    let mut child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("could not start: {error}"))?;
    // At once, so the hook has had as little time as possible to start
    // anything that would escape the tree.
    let tree = match crate::jobs::Tree::of(&child) {
        Ok(tree) => tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        end(&tree, &mut child);
        return Err("could not be given its input".into());
    };
    // On a thread of its own: a hook that never reads its input would
    // otherwise hold this up once the pipe filled.
    let input = input.to_owned();
    std::thread::spawn(move || {
        let _ = std::io::Write::write_all(&mut stdin, input.as_bytes());
    });
    let (sender, open) = std::sync::mpsc::channel();
    let kept = [
        crate::tools::forward(stdout, sender.clone()),
        crate::tools::forward(stderr, sender),
    ];
    let limit = hook
        .timeout
        .map_or(TIMEOUT, |seconds| Duration::from_secs(seconds.max(1)));
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {}
            Err(error) => {
                end(&tree, &mut child);
                return Err(format!("could not be waited on: {error}"));
            }
        }
        if started.elapsed() > limit {
            end(&tree, &mut child);
            return Err(format!(
                "timed out after {} s and was stopped",
                limit.as_secs()
            ));
        }
        if !ctx.cancel.sleep(POLL) {
            end(&tree, &mut child);
            return Err("was stopped with the call".into());
        }
    };
    // Something the hook left running may hold its output open; what came
    // before the grace ran out is all there is.
    crate::tools::pipes_closed(&open);
    Ok(Ran {
        code: status.code(),
        stdout: crate::tools::take(&kept[0]).into_text().0,
        stderr: crate::tools::take(&kept[1]).into_text().0,
    })
}

/// Ends a hook that is still running and everything it started, then reaps
/// it: killed before it is reaped, its process group's id cannot yet have
/// passed to another process.
fn end(tree: &crate::jobs::Tree, child: &mut Child) {
    tree.kill();
    let _ = child.wait();
}

/// Keeps a console window from flashing up for a hook on Windows, where
/// Letronna owns none; `tools::shell` does the same for a shell.
fn no_window(command: &mut std::process::Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

/// Whether `program` starts as it is, with no shell: not one of sh's own
/// words, and on Windows an .exe, named or found on `path`, since Rust's
/// start looks for nothing else where PowerShell would also run a .cmd, .bat
/// or .ps1 of that name, as npm's `npx` is. `is_file` looks at the disk.
fn startable(
    program: &str,
    windows: bool,
    path: Option<&OsStr>,
    is_file: &dyn Fn(&Path) -> bool,
) -> bool {
    if !windows {
        return !BUILTINS.contains(&program);
    }
    let name = program.rsplit(['\\', '/']).next().unwrap_or(program);
    if let Some((_, extension)) = name.rsplit_once('.') {
        return extension.eq_ignore_ascii_case("exe");
    }
    let exe = format!("{program}.exe");
    if name.len() < program.len() {
        return is_file(Path::new(&exe));
    }
    path.is_some_and(|path| std::env::split_paths(path).any(|dir| is_file(&dir.join(&exe))))
}

/// `command` as the shell is given it. On Windows a last line hands on a
/// program's exit code, which PowerShell's `-Command` otherwise turns into 1,
/// so exit code 2 still blocks; one that exits itself never reaches it.
fn shell_line(command: &str, windows: bool) -> String {
    if !windows {
        return command.to_owned();
    }
    format!("{command}\nif ($LASTEXITCODE) {{ exit $LASTEXITCODE }}")
}

/// `line` as the words a program is started with, or `None` where it needs
/// a shell: pipes, redirects, variables, `~`, globs, a variable set before
/// the program, or a quote left open. A backslash escapes only where the
/// shell is sh; on Windows it separates a path.
fn argv(line: &str, windows: bool) -> Option<Vec<String>> {
    const SHELL: &str = "|&;<>()$`*?[]{}~#\n";
    // PowerShell also reads these as its own syntax, an array or a splat.
    const POWERSHELL: &str = ",@";
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' | '\r' => words.extend(word.take()),
            '\'' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => word.push(c),
                    }
                }
            }
            '"' => {
                let word = word.get_or_insert_with(String::new);
                loop {
                    match chars.next()? {
                        '"' => break,
                        '$' | '`' => return None,
                        '\\' if !windows => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => word.push(c),
                            c => word.extend(['\\', c]),
                        },
                        c => word.push(c),
                    }
                }
            }
            '\\' if !windows => word.get_or_insert_with(String::new).push(chars.next()?),
            c if SHELL.contains(c) || (windows && POWERSHELL.contains(c)) => return None,
            c => word.get_or_insert_with(String::new).push(c),
        }
    }
    words.extend(word);
    if words.first().is_some_and(|program| program.contains('=')) {
        return None;
    }
    Some(words)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-hooks-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A shell script in `dir` that runs `body`, by its path.
    #[cfg(unix)]
    fn script(dir: &Path, name: &str, body: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path.to_string_lossy().into_owned()
    }

    /// Hooks with one command for `event`, on every tool.
    #[cfg(unix)]
    fn one(event: &str, command: &str, timeout: Option<u64>) -> Hooks {
        let mut hook = json!({ "type": "command", "command": command });
        if let Some(seconds) = timeout {
            hook["timeout"] = seconds.into();
        }
        parse(&json!({ "hooks": { event: [{ "matcher": "", "hooks": [hook] }] } }).to_string())
            .unwrap()
    }

    fn words(list: &[&str]) -> Option<Vec<String>> {
        Some(list.iter().map(|word| word.to_string()).collect())
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_exiting_2_blocks_the_call_with_its_stderr_as_the_reason() {
        let hooks = Ok(one(
            "PreToolUse",
            "sh -c 'echo no deleting >&2; exit 2'",
            None,
        ));
        let args = json!({ "command": "rm -rf build" });
        let why = before(&hooks, "run_command", &args, &ToolContext::default()).unwrap_err();
        assert!(
            why.contains("blocked") && why.contains("no deleting"),
            "{why}"
        );
        // A builtin of the shell is run by the shell, not looked for as a program.
        let hooks = Ok(one("PreToolUse", "exit 2", None));
        let why = before(&hooks, "run_command", &args, &ToolContext::default()).unwrap_err();
        assert!(why.contains("exited with code 2"), "{why}");
    }

    #[cfg(unix)]
    #[test]
    fn any_other_failure_is_reported_and_the_call_goes_on() {
        let ctx = ToolContext::default();
        let failing = one("PreToolUse", "sh -c 'echo broken >&2; exit 1'", None);
        let heard = hear(&failing, Event::PreToolUse, "run_command", &json!({}), &ctx);
        assert!(heard.blocks.is_empty());
        assert!(
            heard.problems[0].contains("exit code 1: broken"),
            "{heard:?}"
        );
        let missing = one("PreToolUse", "/no/such/hook", None);
        let heard = hear(&missing, Event::PreToolUse, "run_command", &json!({}), &ctx);
        assert!(heard.blocks.is_empty());
        let problem = &heard.problems[0];
        assert!(
            problem.contains("could not start") && problem.contains("went on without it"),
            "{heard:?}"
        );
        // Shown as one short line, however much the hook printed.
        let long = format!("The Stop hook `x` failed:\n{}", "trace line\n".repeat(200));
        let line = shown(&long);
        assert!(
            !line.contains('\n') && line.chars().count() <= SHOWN + 3,
            "{line}"
        );
        assert!(line.starts_with("The Stop hook") && line.ends_with("trace line"));
    }

    #[cfg(unix)]
    #[test]
    fn a_timeout_ends_the_hook_and_everything_it_started() {
        let dir = scratch("timeout");
        let pid = dir.join("pid");
        let line = format!("sh -c 'sleep 30 & echo $! > {}; wait'", pid.display());
        let hooks = one("PreToolUse", &line, Some(1));
        let started = Instant::now();
        let heard = hear(
            &hooks,
            Event::PreToolUse,
            "run_command",
            &json!({}),
            &Default::default(),
        );
        assert!(started.elapsed() < Duration::from_secs(10));
        assert!(heard.blocks.is_empty());
        assert!(
            heard.problems[0].contains("timed out after 1 s"),
            "{heard:?}"
        );
        // The sleep is the hook's child: ending only the shell would leave it.
        let pid: i32 = std::fs::read_to_string(&pid)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let gone = (0..40).any(|_| {
            std::thread::sleep(Duration::from_millis(50));
            // SAFETY: signal 0 only asks whether the process is there.
            (unsafe { libc::kill(pid, 0) }) != 0
        });
        assert!(gone, "the hook's child {pid} outlived its timeout");
    }

    #[cfg(unix)]
    #[test]
    fn stopping_while_a_guard_runs_ends_it_and_the_call_never_runs() {
        let dir = scratch("stop");
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let stop = ctx.cancel.clone();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            stop.set();
        });
        let started = Instant::now();
        let hooks = Ok(one("PreToolUse", "sleep 30", None));
        let tool = crate::tools::find("write_file").unwrap();
        let args = json!({ "path": "out.txt", "content": "x" }).to_string();
        let why = crate::tools::run_found(tool.as_ref(), &args, &ctx, &hooks).unwrap_err();
        assert!(started.elapsed() < Duration::from_secs(5));
        assert_eq!(why, crate::agent::STOPPED_BEFORE_START);
        assert!(!dir.join("out.txt").exists());
        // The user's own stop is not a failure to tell them about.
        let heard = hear(
            &one("Stop", "sleep 30", None),
            Event::Stop,
            "",
            &json!({}),
            &ctx,
        );
        assert!(
            heard.problems.is_empty() && heard.blocks.is_empty(),
            "{heard:?}"
        );
    }

    #[test]
    fn a_hooks_file_that_cannot_be_used_holds_calls_for_a_yes_and_is_left_alone() {
        let dir = scratch("broken");
        let file = dir.join("hooks.json");
        let broken = r#"{"hooks": {"PreToolUse": ["#;
        std::fs::write(&file, broken).unwrap();
        let loaded = load(&file);
        let error = loaded.as_ref().err().unwrap();
        assert!(error.contains("hooks.json cannot be used"), "{error}");
        let args = json!({ "path": "notes.txt" });
        let why = before(&loaded, "read_file", &args, &ToolContext::default()).unwrap_err();
        assert!(why.contains("Until it is fixed"), "{why}");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), broken);
        // The user said yes on a card showing the error, and a question
        // touches nothing a hook could guard.
        let approved = ToolContext {
            approved: true,
            ..Default::default()
        };
        assert!(before(&loaded, "read_file", &args, &approved).is_ok());
        let asked = before(
            &loaded,
            crate::tools::ASK_USER,
            &json!({}),
            &Default::default(),
        );
        assert!(asked.is_ok());
        // A matcher that is not a regular expression breaks the file too,
        // rather than quietly matching nothing.
        let bad = r#"{"hooks": {"PreToolUse": [{"matcher": "run_(", "hooks": []}]}}"#;
        std::fs::write(&file, bad).unwrap();
        let error = load(&file).err().unwrap();
        assert!(error.contains("not a regular expression"), "{error}");
        assert!(load(&dir.join("missing.json")).is_ok_and(|hooks| !hooks.has(Event::Stop)));
        // An emptied or touched file is no hooks at all.
        std::fs::write(&file, " \n").unwrap();
        assert!(load(&file).is_ok_and(|hooks| !hooks.has(Event::PreToolUse)));
    }

    #[test]
    fn a_hook_the_file_holds_that_would_never_run_breaks_it() {
        let hook = json!([{ "hooks": [{ "type": "command", "command": "guard" }] }]);
        let error = |file: Value| parse(&file.to_string()).err().unwrap_or_default();
        let outside = error(json!({ "PreToolUse": hook }));
        assert!(outside.contains("inside \"hooks\""), "{outside}");
        let unknown = error(json!({ "hooks": { "preToolUse": hook } }));
        assert!(unknown.contains("`preToolUse`"), "{unknown}");
        let prompt = json!([{ "hooks": [{ "type": "prompt", "command": "guard" }] }]);
        let prompt = error(json!({ "hooks": { "Stop": prompt } }));
        assert!(prompt.contains("of type `prompt`"), "{prompt}");
        let empty = json!([{ "hooks": [{ "type": "command", "command": " " }] }]);
        let empty = error(json!({ "hooks": { "PreToolUse": empty } }));
        assert!(empty.contains("has no command"), "{empty}");
        // The rest of a settings file pasted whole is let be.
        let pasted = json!({ "model": "x", "hooks": { "Stop": hook } });
        assert!(parse(&pasted.to_string()).is_ok_and(|hooks| hooks.has(Event::Stop)));
    }

    #[test]
    fn tests_never_read_the_hooks_of_whoever_runs_them() {
        assert_ne!(file(), crate::chat::data_dir().join(FILE));
        assert!(!file().exists());
        assert!(
            current()
                .as_ref()
                .as_ref()
                .is_ok_and(|hooks| !hooks.has(Event::PreToolUse))
        );
    }

    #[test]
    fn a_matcher_is_a_name_a_list_or_a_regular_expression_and_a_patch_is_an_edit() {
        let fits = |matcher: &str, name: &str| Matcher::of(matcher).unwrap().fits(name);
        assert!(fits("", "read_file") && fits("*", "run_command"));
        assert!(fits("run_command", "run_command") && !fits("run", "run_command"));
        assert!(fits("Bash", "run_command") && !fits("Bash", "read_file"));
        assert!(fits("Edit|Write", "edit_file") && fits("Edit|Write", "write_file"));
        assert!(!fits("Edit|Write", "read_file"));
        assert!(fits("Edit", "apply_patch") && fits("write_file", "apply_patch"));
        assert!(!fits("Bash", "apply_patch"));
        assert!(fits("^web_", "web_fetch") && fits("Web.*", "web_fetch"));
        assert!(!fits("mcp__.*", "run_command"));
        assert!(Matcher::of("(").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_hooks_allow_never_beats_a_deny() {
        let dir = scratch("allow");
        let ran = dir.join("ran");
        let allow = script(
            &dir,
            "allow.sh",
            &format!(
                "touch '{}'\necho '{{\"hookSpecificOutput\": {{\"permissionDecision\": \"allow\"}}}}'",
                ran.display()
            ),
        );
        let tool = crate::tools::find("write_file").unwrap();
        let args = json!({ "path": "out.txt", "content": "x" }).to_string();
        let planning = ToolContext {
            workspace: Some(dir.clone()),
            plan: true,
            ..Default::default()
        };
        let hooks = Ok(one("PreToolUse", &allow, None));
        assert!(crate::tools::run_found(tool.as_ref(), &args, &planning, &hooks).is_err());
        // Plan mode refused it before any hook, so the hook never ran.
        assert!(!dir.join("out.txt").exists() && !ran.exists());
        // One hook allowing and the next blocking is a block.
        let deny = script(&dir, "deny.sh", "echo 'not here' >&2\nexit 2");
        let both = json!({ "hooks": { "PreToolUse": [{ "hooks": [
            { "type": "command", "command": allow },
            { "type": "command", "command": deny },
        ] }] } });
        let both = Ok(parse(&both.to_string()).unwrap());
        let ctx = ToolContext {
            plan: false,
            ..planning
        };
        let why = crate::tools::run_found(tool.as_ref(), &args, &ctx, &both).unwrap_err();
        assert!(why.contains("not here"), "{why}");
        assert!(ran.exists() && !dir.join("out.txt").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_reads_the_call_on_stdin_as_claude_code_sends_it() {
        let dir = scratch("stdin");
        let seen = dir.join("seen.json");
        let body = format!(
            "cat > '{}'\necho \"$CLAUDE_PROJECT_DIR\" > '{}'",
            seen.display(),
            dir.join("project").display()
        );
        let hooks = Ok(one("PreToolUse", &script(&dir, "log.sh", &body), None));
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            conversation: 7,
            ..Default::default()
        };
        let args = json!({ "command": "cargo test" });
        before(&hooks, "run_command", &args, &ctx).unwrap();
        let seen: Value = serde_json::from_str(&std::fs::read_to_string(&seen).unwrap()).unwrap();
        assert_eq!(seen["hook_event_name"], "PreToolUse");
        assert_eq!(seen["session_id"], "7");
        assert_eq!(seen["cwd"], dir.to_string_lossy().as_ref());
        assert_eq!(seen["tool_name"], "run_command");
        assert_eq!(seen["tool_input"]["command"], "cargo test");
        let project = std::fs::read_to_string(dir.join("project")).unwrap();
        assert_eq!(project.trim(), dir.to_string_lossy());
    }

    #[test]
    fn claude_codes_argument_names_sit_beside_letronnas() {
        let ctx = ToolContext {
            workspace: Some(PathBuf::from("/work")),
            ..Default::default()
        };
        let edit = json!({ "path": "src/a.rs", "edits": [{ "old": "x", "new": "y" }] });
        let input = tool_input("edit_file", &edit, &ctx);
        assert_eq!(input["path"], "src/a.rs");
        let file = PathBuf::from("/work").join("src/a.rs");
        assert_eq!(input["file_path"], file.to_string_lossy().as_ref());
        assert_eq!(
            (&input["old_string"], &input["new_string"]),
            (&json!("x"), &json!("y"))
        );
        let command = json!({ "command": "make", "background": true });
        let input = tool_input("run_command", &command, &ctx);
        assert_eq!(input["run_in_background"], true);
        assert!(input.get("file_path").is_none());
    }

    #[test]
    fn json_on_stdout_can_deny_ask_or_end_the_turn_but_never_allow() {
        let heard = |stdout: &str, approved: bool| {
            let mut heard = Heard::default();
            said(stdout, approved, &mut heard);
            heard
        };
        let deny = r#"{"hookSpecificOutput": {"permissionDecision": "deny",
            "permissionDecisionReason": "not on main"}}"#;
        assert_eq!(heard(deny, true).blocks, ["not on main"]);
        let allow = r#"{"hookSpecificOutput": {"permissionDecision": "allow"}}"#;
        assert!(heard(allow, false).blocks.is_empty());
        let ask = r#"{"hookSpecificOutput": {"permissionDecision": "ask"}}"#;
        assert!(!heard(ask, false).blocks.is_empty() && heard(ask, true).blocks.is_empty());
        let block = r#"{"decision": "block", "reason": "tests fail"}"#;
        assert_eq!(heard(block, false).blocks, ["tests fail"]);
        let context = r#"{"hookSpecificOutput": {"additionalContext": "lint is clean"}}"#;
        assert_eq!(heard(context, false).context, ["lint is clean"]);
        assert!(heard("just some text", false).blocks.is_empty());
        let end =
            r#"{"continue": false, "stopReason": "deploy freeze", "systemMessage": "frozen"}"#;
        let ended = heard(end, false);
        assert_eq!(ended.halt.as_deref(), Some("deploy freeze"));
        assert_eq!(ended.problems, ["A hook says: frozen"]);
        // A rewrite the gate never saw is refused, even beside an allow.
        let rewrite = r#"{"hookSpecificOutput": {"permissionDecision": "allow",
            "updatedInput": {"command": "make --dry-run"}}}"#;
        assert!(heard(rewrite, true).blocks[0].contains("does not do"));
    }

    #[cfg(unix)]
    #[test]
    fn a_hook_ending_the_turn_refuses_what_was_queued_behind_it() {
        let conversation = 4_240_001;
        let ctx = ToolContext {
            conversation,
            ..Default::default()
        };
        new_turn(conversation);
        let end = r#"echo '{"continue": false, "stopReason": "deploy freeze"}'"#;
        let hooks = Ok(one("PreToolUse", end, None));
        let args = json!({ "command": "make" });
        let why = before(&hooks, "run_command", &args, &ctx).unwrap_err();
        assert!(why.contains("ended the turn: deploy freeze"), "{why}");
        let none = Ok(Hooks::default());
        assert_eq!(
            before(&none, "read_file", &json!({}), &ctx).unwrap_err(),
            ENDED
        );
        assert!(halted(conversation) && !halted(conversation));
        assert!(before(&none, "read_file", &json!({}), &ctx).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn a_post_tool_hook_speaks_to_the_model_and_a_stop_hook_holds_the_turn() {
        let ctx = ToolContext::default();
        let hooks = Ok(one(
            "PostToolUse",
            "sh -c 'echo now run the tests >&2; exit 2'",
            None,
        ));
        let said = after(&hooks, "write_file", &json!({}), "Wrote 1 line.", &ctx);
        assert_eq!(said, "\n\n[PostToolUse hook] now run the tests");
        // Every PostToolUse hook runs, whatever an earlier one said.
        let two = json!({ "hooks": { "PostToolUse": [{ "hooks": [
            { "type": "command", "command": "sh -c 'echo lint >&2; exit 2'" },
            { "type": "command", "command": "sh -c 'echo format >&2; exit 2'" },
        ] }] } });
        let two = Ok(parse(&two.to_string()).unwrap());
        let said = after(&two, "edit_file", &json!({}), "Edited.", &ctx);
        assert!(said.contains("lint") && said.contains("format"), "{said}");
        // A Stop hook runs whatever its matcher says.
        let stop = r#"{"hooks": {"Stop": [{"matcher": "Bash", "hooks": [{"type": "command",
            "command": "sh -c 'echo two tests fail >&2; exit 2'"}]}]}}"#;
        let heard = hear(&parse(stop).unwrap(), Event::Stop, "", &json!({}), &ctx);
        assert_eq!(heard.blocks, ["two tests fail"]);
    }

    #[test]
    fn a_command_runs_as_its_words_unless_it_needs_a_shell() {
        let guard = "python3 /opt/hooks/guard.py --strict";
        let split = words(&["python3", "/opt/hooks/guard.py", "--strict"]);
        assert_eq!(argv(guard, false), split);
        let quoted = r#"'/my hooks/guard' "a \"b\"" c\ d"#;
        assert_eq!(
            argv(quoted, false),
            words(&["/my hooks/guard", "a \"b\"", "c d"])
        );
        let windows = r"C:\hooks\guard.exe --strict";
        assert_eq!(
            argv(windows, true),
            words(&[r"C:\hooks\guard.exe", "--strict"])
        );
        for line in [
            "jq -r .tool_input.command | grep rm",
            "$CLAUDE_PROJECT_DIR/hooks/check.sh",
            "\"$HOME\"/check.sh",
            "~/hooks/check.sh",
            "MODE=strict guard",
            "guard > log",
            "'open",
        ] {
            assert_eq!(argv(line, false), None, "{line}");
        }
        assert_eq!(argv("guard a,b", false), words(&["guard", "a,b"]));
        assert_eq!(argv("guard a,b", true), None);
    }

    #[test]
    fn only_a_program_that_starts_as_it_is_skips_the_shell() {
        let nothing = |_: &Path| false;
        assert!(startable("python3", false, None, &nothing));
        assert!(!startable("exit", false, None, &nothing));
        assert!(!startable("[", false, None, &nothing));
        // On Windows only an .exe starts without PowerShell: npm's npx is a .cmd.
        let bin = PathBuf::from("bin");
        let path = std::env::join_paths([&bin]).unwrap();
        let only = |name: &'static str| move |file: &Path| file == Path::new(name);
        let found = only("bin/node.exe");
        assert!(startable("node", true, Some(&path), &found));
        assert!(!startable("npx", true, Some(&path), &found));
        assert!(!startable("exit", true, Some(&path), &found));
        assert!(startable(r"C:\hooks\guard.exe", true, None, &nothing));
        assert!(!startable(r"C:\hooks\guard.ps1", true, None, &nothing));
        assert!(!startable(r"C:\hooks\guard.cmd", true, None, &nothing));
        let there = only(r"C:\hooks\guard.exe");
        assert!(startable(r"C:\hooks\guard", true, None, &there));
    }

    #[test]
    fn powershell_hands_on_the_exit_code_a_block_needs() {
        let line = shell_line("guard.cmd | Out-Null", true);
        assert_eq!(
            line,
            "guard.cmd | Out-Null\nif ($LASTEXITCODE) { exit $LASTEXITCODE }"
        );
        assert_eq!(shell_line("jq . | guard", false), "jq . | guard");
    }
}
