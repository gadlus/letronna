//! Tools the model may call. Each one is a `Tool`; the registry below is what
//! gets advertised to the provider and dispatched on `tool_calls`.
//!
//! Read-only tools run without asking. Tools that change the workspace or
//! run a command return `needs_approval`, and the chat shows a card first.
//! `ask_user` is answered by the user, never run here.

use crate::chat::{Connection, Remote};
use serde_json::{Value, json};
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
    /// own; 0 for none in particular.
    pub conversation: u64,
    /// Plan mode, in which a tool that changes things is refused when named.
    pub plan: bool,
}

impl ToolContext {
    pub fn of(connection: &Connection) -> Self {
        Self {
            workspace: connection.workspace.clone(),
            remote: connection.remote.clone(),
            conversation: connection.conversation,
            plan: connection.plan,
        }
    }

    /// Other people can read the replies, so nothing on this computer is in
    /// reach: what a tool finds would be posted to all of them.
    fn shared(&self) -> bool {
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
    /// What the approval card shows: a diff, the file body, the command line.
    fn preview(&self, args: &Value, ctx: &ToolContext) -> String {
        let _ = ctx;
        args.to_string()
    }
}

pub const ASK_USER: &str = "ask_user";
pub const TODO: &str = "todo";
/// The tool result the model gets when the user denies a call.
pub const DECLINED: &str = "The user declined this action. Ask before trying a different approach.";
const COMMAND_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
/// A search stops after this long or this many files, and says so, rather
/// than walking a whole home folder while the session waits on it.
const SEARCH_TIME: Duration = Duration::from_secs(15);
const SEARCH_FILES: usize = 20_000;
/// Bigger files are skipped by search and cut short by read_file: neither
/// can show more than `MAX_OUTPUT`, and reading gigabytes to find that out
/// is how a tool hangs.
const SEARCH_FILE_BYTES: u64 = 1_000_000;
const READ_BYTES: u64 = 256 * 1024;
/// How long a background command gets to print something before the tool
/// replies, so the first message can carry a port or an early crash.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(1200);

const MAX_OUTPUT: usize = 24_000;
const SKIP_DIRS: &[&str] = &[".git", "node_modules", "target", ".next", "dist", "build"];

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
        Arc::new(AskUser),
        Arc::new(Todo),
        Arc::new(crate::messaging::ReadChat),
        Arc::new(Sessions),
        Arc::new(crate::memory::Remember),
        Arc::new(SearchSessions),
        Arc::new(crate::web::WebFetch),
        Arc::new(crate::media::MakeImage),
        Arc::new(crate::media::Speak),
        Arc::new(crate::media::Transcribe),
        Arc::new(crate::media::MakeVideo),
        Arc::new(crate::browser::Browse),
        Arc::new(crate::browser::UsePage),
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

pub fn needs_approval(name: &str) -> bool {
    find(name).is_some_and(|t| t.needs_approval())
}

pub fn preview(name: &str, args: &str, ctx: &ToolContext) -> String {
    match (find(name), parse_args(args)) {
        (Some(t), Ok(v)) => t.preview(&v, ctx),
        _ => args.to_owned(),
    }
}

/// The `tools` array for a chat completion request. In plan mode the tools
/// that change things are left out entirely, so the model cannot try them;
/// where other people read the replies, so is everything that touches this
/// computer; and `read_chat` exists only where there is a chat to read.
pub fn specs(connection: &Connection) -> Vec<Value> {
    let shared = connection.remote.as_ref().is_some_and(|r| r.shared);
    let history = connection
        .remote
        .as_ref()
        .is_some_and(|r| r.history.is_some());
    all()
        .iter()
        .filter(|t| !connection.plan || !t.mutates())
        .filter(|t| !shared || !t.machine())
        .filter(|t| history || !t.needs_history())
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
    // Only fitting tools are offered, but the model can name any of them;
    // refuse here too rather than trust the offer.
    if ctx.shared() && tool.machine() {
        return Err(format!(
            "`{name}` is not available here: other people can read this chat, so \
             nothing on this computer is in reach."
        ));
    }
    if ctx.plan && tool.mutates() {
        return Err(format!(
            "`{name}` is not available in plan mode: it changes things, and plan \
             mode can only look and propose. Describe the change instead."
        ));
    }
    let args = parse_args(args)?;
    let mut out = tool.run(&args, ctx)?;
    if out.len() > MAX_OUTPUT {
        let cut = out.floor_char_boundary(MAX_OUTPUT);
        out.truncate(cut);
        out.push_str("\n… [truncated]");
    }
    Ok(out)
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

/// The argument the UI shows next to a tool name.
pub fn summary(name: &str, args: &str) -> String {
    let v: Value = serde_json::from_str(args).unwrap_or(Value::Null);
    let text = match name {
        "search" | "search_sessions" => v["query"]
            .as_str()
            .map(|q| format!("\"{q}\""))
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
        "make_image" => v["prompt"].as_str().unwrap_or_default().to_owned(),
        "make_video" => v["prompt"]
            .as_str()
            .or(v["job"].as_str())
            .unwrap_or_default()
            .to_owned(),
        "speak" => v["text"].as_str().unwrap_or_default().to_owned(),
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
        _ => v["path"].as_str().unwrap_or(".").to_owned(),
    };
    one_line(&text)
}

/// Collapse whitespace so a multi-line command cannot spill out of the
/// single-line row it is shown in.
pub fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Where a write may land: inside the workspace only. The file need not
/// exist yet, so the check is on the nearest existing ancestor.
fn resolve_write(raw: &str, ctx: &ToolContext) -> Result<PathBuf, String> {
    let workspace = ctx
        .workspace
        .as_ref()
        .ok_or("No project folder is attached. Ask the user to attach one first.")?;
    let root = std::fs::canonicalize(workspace).map_err(|_| "The project folder is missing.")?;
    let target = workspace.join(raw);
    let mut probe = target.clone();
    while !probe.exists() {
        probe = probe.parent().map(Path::to_path_buf).ok_or("Bad path.")?;
    }
    let existing = std::fs::canonicalize(&probe).map_err(|e| e.to_string())?;
    if !existing.starts_with(&root) {
        return Err(format!("`{raw}` is outside the project folder."));
    }
    Ok(target)
}

/// The platform shell, configured the way every caller needs it.
pub fn shell(command: &str, cwd: Option<&Path>) -> std::process::Command {
    use std::process::Command;
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", command]);
        c
    } else {
        let mut c = Command::new("sh");
        c.args(["-c", command]);
        c
    };
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    // Letronna is a GUI-subsystem app, so it owns no console. Without this,
    // Windows hands the shell a brand new console window on every call and it
    // flashes up over the chat.
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

fn run_shell(command: &str, cwd: Option<&Path>) -> Result<String, String> {
    use std::io::Read;
    use std::process::Stdio;
    let mut child = shell(command, cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Cannot start the shell: {e}"))?;
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stdout.read_to_string(&mut s);
        s
    });
    let err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s);
        s
    });
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
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    let mut text = out.join().unwrap_or_default();
    let err = err.join().unwrap_or_default();
    if !err.trim().is_empty() {
        text.push_str("\n[stderr]\n");
        text.push_str(&err);
    }
    if !status.success() {
        text.push_str(&format!("\n[exit code {}]", status.code().unwrap_or(-1)));
    }
    Ok(text)
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
        "Create or overwrite a text file in the project folder. Parent folders are created."
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
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let path = resolve_write(path_arg(args), ctx)?;
        let content = args["content"].as_str().ok_or("`content` is required.")?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, content).map_err(|e| e.to_string())?;
        Ok(format!("Wrote {} bytes.", content.len()))
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
        "Replace one exact occurrence of `old` with `new` in a file. `old` must match exactly once."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"},
            "old": {"type": "string"},
            "new": {"type": "string"}
        }, "required": ["path", "old", "new"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let old = args["old"].as_str().unwrap_or_default();
        let new = args["new"].as_str().unwrap_or_default();
        let mut out = String::new();
        for line in old.lines() {
            out.push_str("- ");
            out.push_str(line);
            out.push('\n');
        }
        for line in new.lines() {
            out.push_str("+ ");
            out.push_str(line);
            out.push('\n');
        }
        out
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let path = resolve_write(path_arg(args), ctx)?;
        let old = args["old"].as_str().ok_or("`old` is required.")?;
        let new = args["new"].as_str().ok_or("`new` is required.")?;
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        match text.matches(old).count() {
            0 => return Err("`old` was not found in the file.".into()),
            1 => {}
            n => return Err(format!("`old` matches {n} times; include more context.")),
        }
        std::fs::write(&path, text.replacen(old, new, 1)).map_err(|e| e.to_string())?;
        Ok("Edited.".into())
    }
}

struct RunCommand;
impl Tool for RunCommand {
    fn mutates(&self) -> bool {
        true
    }
    fn name(&self) -> &'static str {
        "run_command"
    }
    fn description(&self) -> &'static str {
        if cfg!(windows) {
            "Run a PowerShell command in the project folder. Waits for it to finish and returns its output, with a 2 minute limit. For anything meant to keep running, such as a dev server or a watcher, pass background: true instead."
        } else {
            "Run a shell (sh) command in the project folder. Waits for it to finish and returns its output, with a 2 minute limit. For anything meant to keep running, such as a dev server or a watcher, pass background: true instead."
        }
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "command": {"type": "string"},
            "background": {"type": "boolean", "description": "Leave the command running and return a job number straight away. Read its output later with read_job, and end it with stop_job."}
        }, "required": ["command"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        args["command"].as_str().unwrap_or_default().to_owned()
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let command = args["command"].as_str().ok_or("`command` is required.")?;
        let cwd = ctx
            .workspace
            .as_deref()
            .ok_or("No project folder is attached. Ask the user to attach one first.")?;
        if !args["background"].as_bool().unwrap_or(false) {
            return run_shell(command, Some(cwd));
        }
        let id = crate::jobs::start(command, cwd)?;
        // Give it a moment to fail or announce a port, so the first reply says
        // something more useful than "started".
        std::thread::sleep(SETTLE);
        let log = crate::jobs::log(id).unwrap_or_default();
        let state = if crate::jobs::running(id).unwrap_or(false) {
            "running"
        } else {
            "already exited"
        };
        Ok(format!(
            "Started as job #{id} ({state}). First output:\n{}",
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
    fn description(&self) -> &'static str {
        "Search what was said in the user's earlier Letronna sessions for a word or phrase, ignoring case. Returns the matching lines with each session's id, title and age, newest sessions first. Use it when the user refers to something from an earlier conversation, or when an earlier session may already hold the answer."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "query": {"type": "string"},
            "limit": {"type": "integer", "description": "How many matches at most. Default 20."}
        }, "required": ["query"]})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let query = args["query"].as_str().unwrap_or_default().trim();
        if query.is_empty() {
            return Err("Query is empty.".into());
        }
        let limit = args["limit"].as_u64().unwrap_or(20).clamp(1, 50) as usize;
        let hits = crate::store::search(query, limit);
        if hits.is_empty() {
            return Ok(format!("No earlier session mentions \"{query}\"."));
        }
        let lines: Vec<String> = hits
            .iter()
            .map(|hit| {
                let title = match hit.title.trim() {
                    "" => "untitled",
                    title => title,
                };
                let when = crate::chat::ago(hit.updated);
                format!(
                    "{}  {title}  ({when}) {}: {}",
                    hit.id, hit.role, hit.snippet
                )
            })
            .collect();
        Ok(format!(
            "id  title  (when) who: what was said\n{}\n\nThe user opens one with /resume, or `letronna --resume <id>`.",
            lines.join("\n")
        ))
    }
}

struct ReadJob;
impl Tool for ReadJob {
    fn name(&self) -> &'static str {
        "read_job"
    }
    fn description(&self) -> &'static str {
        "Read what a background job has printed so far, and whether it is still running."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "id": {"type": "integer"}
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
    fn name(&self) -> &'static str {
        "wait"
    }
    fn description(&self) -> &'static str {
        "Pause before continuing: for a number of seconds, or until a background job prints something. Use it to let a server come up before calling it."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "seconds": {"type": "number", "description": "How long to wait. Also the limit when waiting for output."},
            "job": {"type": "integer", "description": "Wait for this job to print `until`, or to exit."},
            "until": {"type": "string", "description": "Text to watch for in the job's output, such as `ready` or `listening`."}
        }})
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let seconds = args["seconds"].as_f64().unwrap_or(10.).clamp(0., 120.);
        let limit = std::time::Duration::from_secs_f64(seconds);
        let Some(id) = args["job"].as_u64() else {
            std::thread::sleep(limit);
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
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
    }
}

fn job_id(args: &Value) -> Result<u64, String> {
    args["id"]
        .as_u64()
        .ok_or_else(|| "`id` is required and must be a job number.".to_owned())
}

/// Answered in the chat, not here.
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
        Err("ask_user is answered by the user.".into())
    }
}

/// A checklist the model keeps for itself. The app stores the items on the
/// conversation and draws them above the composer; see `Chat::apply_todo`.
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

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// Resolve a user-supplied path and refuse anything outside the workspace or
/// the home folder. Canonicalising also defeats `..` and symlink escapes.
pub(crate) fn resolve(raw: &str, ctx: &ToolContext) -> Result<PathBuf, String> {
    let base = ctx.workspace.clone().or_else(home).unwrap_or_default();
    let joined = base.join(raw);
    let path = std::fs::canonicalize(&joined).map_err(|_| format!("`{raw}` does not exist."))?;
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
    Ok(path)
}

/// Whether `path` lies in Letronna's own folder, `data`, which holds the API
/// keys, the sign-in token and the bot tokens. It sits under the home folder,
/// and reading there needs no approval, so a chat or a file that talked the
/// model into it would have had them read out.
fn private(path: &Path, data: &Path) -> bool {
    std::fs::canonicalize(data).is_ok_and(|data| path.starts_with(data))
}

fn path_arg(args: &Value) -> &str {
    args["path"].as_str().unwrap_or(".")
}

struct ReadFile;
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }
    fn description(&self) -> &'static str {
        "Read a text file. Paths are relative to the workspace."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string"}
        }, "required": ["path"]})
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        use std::io::Read;
        let path = resolve(path_arg(args), ctx)?;
        let file = std::fs::File::open(&path).map_err(|e| e.to_string())?;
        let mut bytes = Vec::new();
        file.take(READ_BYTES)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        let cut = bytes.len() as u64 == READ_BYTES;
        match String::from_utf8(bytes) {
            Ok(text) => Ok(text),
            // A cut can split the last character; anything else is binary.
            Err(e) if cut && e.utf8_error().error_len().is_none() => {
                let valid = e.utf8_error().valid_up_to();
                let mut bytes = e.into_bytes();
                bytes.truncate(valid);
                String::from_utf8(bytes).map_err(|_| "Not a text file.".into())
            }
            Err(_) => Err("Not a text file.".into()),
        }
    }
}

struct ListDir;
impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
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
    fn description(&self) -> &'static str {
        "Case-insensitive text search across files under a folder. Returns path:line: text."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "query": {"type": "string"},
            "path": {"type": "string", "description": "Folder to search. Defaults to the workspace root."}
        }, "required": ["query"]})
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let query = args["query"].as_str().unwrap_or("").to_lowercase();
        if query.is_empty() {
            return Err("Query is empty.".into());
        }
        let root = resolve(path_arg(args), ctx)?;
        Ok(search(&root, &query, SEARCH_TIME, SEARCH_FILES))
    }
}

/// The search itself, with its budget passed in so a test can use a tiny one.
fn search(root: &Path, query: &str, time: Duration, files: usize) -> String {
    let started = Instant::now();
    let mut scanned = 0;
    let mut stopped = false;
    let mut hits = Vec::new();
    walk(root, &mut |path, is_dir| {
        // Checked on folders too, or a deep tree of them runs unbounded.
        if scanned >= files || started.elapsed() >= time {
            stopped = true;
            return false;
        }
        if is_dir {
            return true;
        }
        scanned += 1;
        let Some(text) = searchable(path) else {
            return true;
        };
        // Most files hold no match at all, and lowercasing the whole file once
        // says so; lowercasing it line by line made a new string for every
        // line of every file searched.
        if !text.to_lowercase().contains(query) {
            return true;
        }
        for (n, line) in text.lines().enumerate() {
            if line.to_lowercase().contains(query) {
                let rel = path.strip_prefix(root).unwrap_or(path).display();
                hits.push(format!("{rel}:{}: {}", n + 1, line.trim()));
                if hits.len() >= 200 {
                    return false;
                }
            }
        }
        true
    });
    let mut out = match hits.is_empty() {
        true => "No matches.".to_owned(),
        false => hits.join("\n"),
    };
    if stopped {
        out.push_str(&format!(
            "\n… Stopped early after {scanned} files; search a narrower folder for \
             complete results."
        ));
    }
    out
}

/// A file worth searching: small enough, and text. The size comes from the
/// metadata so a huge file is never opened; a NUL byte marks a binary one.
fn searchable(path: &Path) -> Option<String> {
    if std::fs::metadata(path).ok()?.len() > SEARCH_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes[..bytes.len().min(8192)].contains(&0) {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Depth-first walk skipping build, VCS and hidden folders. A symlink is
/// never followed, so a link back up the tree cannot loop it. The callback
/// sees each folder before it is entered and each file, and returns false to
/// stop.
fn walk(dir: &Path, f: &mut dyn FnMut(&Path, bool) -> bool) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return true;
    };
    for entry in entries.filter_map(Result::ok) {
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_symlink() {
            continue;
        }
        let path = entry.path();
        if kind.is_dir() {
            let name = entry.file_name();
            let hidden = name.to_string_lossy().starts_with('.');
            if hidden
                || SKIP_DIRS.iter().any(|s| name == *s)
                || private(&path, &crate::chat::data_dir())
            {
                continue;
            }
            if !f(&path, true) || !walk(&path, f) {
                return false;
            }
        } else if !f(&path, false) {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
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
            "hello\nWorld"
        );
        assert_eq!(run("list_dir", "{}", &ctx).unwrap(), "src/");
        assert_eq!(
            run("search", r#"{"query":"world"}"#, &ctx).unwrap(),
            "src\\a.txt:2: World".replace('\\', std::path::MAIN_SEPARATOR_STR)
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
            "hello
World"
        );
        set_plugin_tools(Vec::new());
        assert!(run("extra", "{}", &ctx).is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn search_stays_bounded() {
        let dir = std::env::temp_dir().join(format!("letronna-search-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        std::fs::write(dir.join("ok.txt"), "a needle here").unwrap();
        std::fs::write(dir.join(".hidden/a.txt"), "needle").unwrap();
        std::fs::write(dir.join("bin.dat"), b"needle\0binary").unwrap();
        let big = format!("needle\n{}", "x".repeat(SEARCH_FILE_BYTES as usize));
        std::fs::write(dir.join("big.txt"), big).unwrap();
        // A link back up the tree would recurse until paths ran out.
        #[cfg(unix)]
        std::os::unix::fs::symlink(&dir, dir.join("loop")).unwrap();

        assert_eq!(
            search(&dir, "needle", SEARCH_TIME, SEARCH_FILES),
            "ok.txt:1: a needle here"
        );
        // Out of budget: stops at once and says why, rather than wandering on.
        for out in [
            search(&dir, "needle", SEARCH_TIME, 0),
            search(&dir, "needle", Duration::ZERO, SEARCH_FILES),
        ] {
            assert!(out.starts_with("No matches."), "{out}");
            assert!(out.contains("Stopped early after 0 files"), "{out}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn search_passes_over_files_without_a_match_and_numbers_the_rest() {
        let dir = std::env::temp_dir().join(format!("letronna-lines-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("none.txt"), "nothing\nto see\nhere").unwrap();
        std::fs::write(
            dir.join("some.txt"),
            "one\ntwo\nThe Needle\nfour\nneedle again",
        )
        .unwrap();
        assert_eq!(
            search(&dir, "needle", SEARCH_TIME, SEARCH_FILES),
            "some.txt:3: The Needle\nsome.txt:5: needle again"
        );
        std::fs::remove_dir_all(dir).unwrap();
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
    fn read_file_stops_at_its_limit() {
        let dir = std::env::temp_dir().join(format!("letronna-read-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // A two-byte character straddles the cut, which must not read as binary.
        let text = format!("{}é{}", "a".repeat(READ_BYTES as usize - 1), "b".repeat(10));
        std::fs::write(dir.join("long.txt"), text).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let out = ReadFile.run(&json!({"path": "long.txt"}), &ctx).unwrap();
        assert_eq!(out.len(), READ_BYTES as usize - 1);
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
            "hello"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }
}
