use crate::failure::{Failure, FailureKind, classify};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    io::{BufRead, BufReader},
    path::PathBuf,
    sync::{Mutex, OnceLock},
    time::Duration,
};

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub reasoning: String,
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub per_second: f32,
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub cached_tokens: u64,
    /// Calls the assistant asked for in this turn.
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    /// For `role: "tool"` messages: the call this result answers.
    #[serde(default)]
    pub tool_call_id: String,
    #[serde(default)]
    pub name: String,
    /// For `role: "tool"`: the raw arguments, kept so the UI can show them.
    #[serde(default)]
    pub args: String,
    #[serde(default)]
    pub is_error: bool,
}

impl Message {
    /// All the input a reply was given. OpenAI counts cache hits inside
    /// `prompt_tokens`, but some gateways count them beside it, which shows
    /// as more cached than sent -- and as a cache rate over a hundred percent.
    pub fn input_tokens(&self) -> u64 {
        if self.cached_tokens > self.prompt_tokens {
            self.prompt_tokens + self.cached_tokens
        } else {
            self.prompt_tokens
        }
    }
}

#[derive(Clone, Default, Debug, PartialEq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON object as sent by the model.
    pub arguments: String,
}

/// How long ago, in the words a person would use. For listing sessions,
/// where the exact second is never the point.
pub fn ago(then: u64) -> String {
    let seconds = now().saturating_sub(then);
    match seconds {
        0..=90 => "just now".to_owned(),
        s if s < 3_600 => format!("{} minutes ago", s / 60),
        s if s < 7_200 => "an hour ago".to_owned(),
        s if s < 86_400 => format!("{} hours ago", s / 3_600),
        s if s < 172_800 => "yesterday".to_owned(),
        s => format!("{} days ago", s / 86_400),
    }
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone, Default)]
pub struct Connection {
    pub endpoint: String,
    pub model: String,
    pub key: String,
    /// Advertise tools to the model.
    pub tools: bool,
    /// Folder the session works in, shown to the model.
    pub workspace: Option<std::path::PathBuf>,
    /// Plan mode: read and ask only, nothing that changes the project.
    pub plan: bool,
    /// How hard to think, when the model can be told. Empty leaves it to the
    /// model, which is what most of them do well enough.
    pub effort: String,
    /// Let the gateway search the web for this turn.
    pub search: bool,
    /// Set when a chat app is driving the session rather than this window.
    pub remote: Option<Remote>,
    /// The session's id: each keeps a browser of its own.
    pub conversation: u64,
}

/// Where a remote conversation is happening, and what that allows.
#[derive(Clone, Default)]
pub struct Remote {
    /// As the model is told it: "a Discord DM", "#general in a Discord
    /// server", "a Telegram chat".
    pub place: String,
    /// Other people can read the replies, so nothing on this computer may be
    /// reached from here.
    pub shared: bool,
    /// Where `read_chat` reads. Discord only: the Telegram Bot API keeps no
    /// history a bot can ask for.
    pub history: Option<History>,
}

#[derive(Clone, Default)]
pub struct History {
    pub token: String,
    pub channel: String,
    /// The bot's own user ID, so its earlier messages read as its own.
    pub bot_id: String,
}

pub enum Event {
    Delta(String),
    Reasoning(String),
    Usage {
        completion: u64,
        per_second: f32,
        /// Input tokens, and how many of them the provider served from its
        /// prompt cache. Cached input costs a fraction of fresh input, so this
        /// is the number to watch when tuning the context window.
        prompt: u64,
        cached: u64,
    },
    /// Sent once, before `Finished`, when the model asked for tools.
    ToolCalls(Vec<ToolCall>),
    Finished(Result<(), Failure>),
}

pub fn validate(connection: &Connection) -> Result<String, String> {
    let url = reqwest::Url::parse(connection.endpoint.trim())
        .map_err(|_| "Enter a valid endpoint, such as http://localhost:11434/v1".to_owned())?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err("Endpoint must be an HTTP or HTTPS URL.".into());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("Use a base URL without credentials, query parameters, or fragments.".into());
    }
    if url.scheme() == "http"
        && !matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]"))
    {
        return Err("Use HTTPS for remote providers, or HTTP on localhost.".into());
    }
    if connection.model.trim().is_empty() {
        return Err("Enter the model ID served by your provider in Connection settings.".into());
    }
    let base = connection.endpoint.trim().trim_end_matches('/');
    // The Letronna gateway signs requests with this computer's account token.
    if connection.key.trim().is_empty() && base == crate::auth::hosted_base() {
        return Err(
            "Sign in with /login to use Letronna's hosted models, or pick another provider.".into(),
        );
    }
    Ok(if base.ends_with("/chat/completions") {
        base.into()
    } else {
        format!("{base}/chat/completions")
    })
}

/// Character budget for the whole conversation window.
const CONTEXT_CHARS: usize = 24_000;
/// A single tool result can be far larger than anything the model needs to
/// read. The chat keeps the whole thing; the request gets the ends.
const MAX_TOOL_CHARS: usize = 8_000;

fn budget(messages: &[Message]) -> usize {
    messages
        .iter()
        .map(|m| m.content.chars().count().min(MAX_TOOL_CHARS))
        .sum()
}

/// Messages in the window, whatever the character budget says. Guards against
/// a long run of tiny messages.
const MAX_MESSAGES: usize = 60;
/// How much the window drops when it no longer fits, in messages. The start is
/// always a whole number of these from the beginning, so appending a turn
/// cannot nudge it: it either stays exactly where it was or jumps a step.
const COMPACT_STEP: usize = 16;

/// Where the context window starts.
///
/// Providers cache on an exact prefix, so what matters as much as the size of
/// the window is that it stops moving. Trimming the oldest turn each time the
/// budget is exceeded would shift the prefix on every single request and miss
/// the cache every time — exactly in the long agent sessions that cost the
/// most. Instead the window halves when it no longer fits, and then stays put
/// for many turns, so requests in between are byte-identical from the front.
///
/// The latest user turn always goes in whatever its size: dropping it (a single
/// huge tool result can exceed the budget by itself) would leave the model with
/// no prompt at all, and it answers by introducing itself.
///
/// This is deliberately stateless: the same message list always yields the same
/// start, so nothing needs saving and two requests over the same history agree.
fn window_start(messages: &[Message]) -> usize {
    let last_user = messages.iter().rposition(|m| m.role == "user").unwrap_or(0);
    let mut start = 0;
    while start < last_user
        && (budget(&messages[start..]) > CONTEXT_CHARS || messages.len() - start > MAX_MESSAGES)
    {
        // Drop a fixed step, then round forward to a user turn so the window
        // never begins midway through a tool exchange.
        let next = (start + COMPACT_STEP).min(last_user);
        start = (next..=last_user)
            .find(|i| messages[*i].role == "user")
            .unwrap_or(last_user);
    }
    start
}

fn body(connection: &Connection, messages: &[Message], summary: Option<&str>) -> Value {
    let start = window_start(messages);
    // The user's SOUL.md, when there is one, says who Letronna is instead.
    let mut system = crate::memory::soul().unwrap_or_else(|| {
        String::from("You are Letronna, a personal assistant. Be clear, concise, and honest.")
    });
    let shared = connection.remote.as_ref().is_some_and(|r| r.shared);
    if connection.tools && !shared {
        // A chat app is not the terminal or the window, and /resume means
        // nothing there; the remote text below says where the session is.
        if connection.remote.is_none() {
            system.push_str(" You are Letronna, running in the user's terminal or desktop window; you are not any other assistant, and commands from other tools do not apply here. The user's earlier conversations are sessions: list_sessions shows them, and the user picks one up with the /resume command or `letronna --resume <id>`.");
        }
        system.push_str(" You have tools to read, search, write and edit files and to run commands in the user's project; use them instead of guessing. Writes and commands are shown to the user for approval; a denial is final for that action. Ask with ask_user when a decision is the user's to make, such as a project name. After changing code, verify it (build, test, or run) before reporting done.");
        match &connection.workspace {
            Some(w) => system.push_str(&format!(" The project folder is `{}`.", w.display())),
            None => system.push_str(" No project folder is attached: you can only read under the home folder. Ask the user to attach one before writing or running anything."),
        }
    } else if !connection.tools {
        system.push_str(" You have no tools or filesystem access on this gateway.");
    }
    if connection.plan && !shared {
        system.push_str(" Plan mode is on: you can read, search and ask, but you cannot write files or run commands, and those tools are unavailable. Work out what you would do and lay out the plan, then let the user turn plan mode off to carry it out.");
    }
    // Without this the model takes "the conversation" to mean something on
    // disk, and goes looking for it there.
    if let Some(remote) = &connection.remote {
        system.push_str(&format!(" You are talking through {}, and your reply is posted there as a chat message, so keep it short and conversational.", remote.place));
        if remote.history.is_some() && connection.tools {
            system.push_str(" You only see the messages addressed to you. When the user refers to the conversation, asks what was said, or wants a summary, read it with read_chat before answering.");
        }
        if remote.shared {
            system.push_str(" Other people in this chat can read your replies. Nothing on this computer is reachable from here: no files, folders or commands.");
        }
    }
    // Memory first: it changes rarely, so the prompt's start stays cacheable;
    // the summary of dropped turns changes as the conversation grows.
    system.push_str(&crate::memory::context(connection));
    if let Some(summary) = summary {
        system.push_str(&format!(
            "\n\nEarlier in this conversation, summarized because it no longer fits:\n{summary}"
        ));
    }
    let mut context = vec![json!({"role": "system", "content": system})];
    let window = &messages[start..];
    let shot = screenshot(window)
        .filter(|_| sees_images(&connection.model))
        .and_then(|(after, path)| Some((after, crate::media::data_url(&path)?)));
    for (i, m) in window.iter().enumerate() {
        context.push(wire_message(m));
        // A tool result carries text only, so the picture follows as the
        // user's: the one shape of it every provider takes.
        if let Some((after, picture)) = &shot
            && *after == i
        {
            context.push(json!({"role": "user", "content": [
                {"type": "text", "text": "The screenshot browse took:"},
                {"type": "image_url", "image_url": {"url": picture}}
            ]}));
        }
        // Every tool call must be answered or providers reject the whole
        // request. A call can be left dangling if the app closed on the card.
        for call in &m.tool_calls {
            let answered = window[i + 1..]
                .iter()
                .take_while(|n| n.role == "tool")
                .any(|n| n.tool_call_id == call.id);
            if !answered {
                context.push(json!({"role": "tool", "tool_call_id": call.id,
                    "content": "Interrupted before this ran; no result."}));
            }
        }
    }
    let mut body = json!({
        "model": connection.model.trim(),
        "messages": context,
        "stream": true,
        // Writing a file puts the whole body in one tool call, so a small cap
        // truncates the arguments mid-JSON.
        "max_tokens": 8192,
        "stream_options": {"include_usage": true}
    });
    if connection.tools {
        let mut tools = crate::tools::specs(connection);
        if connection.search {
            // The gateway's own search, alongside our functions rather than
            // instead of them: the model picks whichever it needs, and the
            // answer comes back with the pages it read attached.
            tools.push(json!({"type": "web_search"}));
        }
        body["tools"] = Value::Array(tools);
    }
    // Models that cannot be told how hard to think ignore this, which is why
    // it is safe to send to all of them. Measured on one that can: sixty-four
    // reasoning tokens at low against seven hundred at high.
    if !connection.effort.is_empty() {
        body["reasoning_effort"] = connection.effort.clone().into();
    }
    body
}

/// The newest screenshot `browse` took in `window`: where it goes -- after
/// the last result of its batch of calls -- and its file. The model is shown
/// that one only: the page has moved on since any older one, and a picture
/// is paid for again on every request it is in.
fn screenshot(window: &[Message]) -> Option<(usize, PathBuf)> {
    let at = window.iter().rposition(|m| {
        m.role == "tool"
            && m.name == crate::browser::BROWSE
            && !m.is_error
            && crate::media::saved(&m.content).is_some()
    })?;
    let path = crate::media::saved(&window[at].content)?;
    let batch = window[at..].iter().take_while(|m| m.role == "tool").count();
    Some((at + batch - 1, path))
}

/// Models that turned a picture down, which are sent none for the rest of
/// this run.
fn sightless() -> &'static Mutex<HashSet<String>> {
    static SIGHTLESS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    SIGHTLESS.get_or_init(Default::default)
}

fn sees_images(model: &str) -> bool {
    sightless()
        .lock()
        .map(|models| !models.contains(model))
        .unwrap_or(true)
}

fn forget_images(model: &str) {
    if let Ok(mut models) = sightless().lock() {
        models.insert(model.to_owned());
    }
}

/// Whether a request for this conversation carries a picture.
fn shows_picture(connection: &Connection, messages: &[Message]) -> bool {
    sees_images(&connection.model)
        && screenshot(&messages[window_start(messages)..]).is_some_and(|(_, path)| path.is_file())
}

/// Keep both ends of an oversized tool result; the middle is rarely the point.
fn trim(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let half = max / 2;
    let head: String = text.chars().take(half).collect();
    let tail: String = text
        .chars()
        .skip(text.chars().count().saturating_sub(half))
        .collect();
    format!("{head}\n… [middle trimmed] …\n{tail}")
}

fn wire_message(m: &Message) -> Value {
    let content = if m.role == "tool" {
        trim(&m.content, MAX_TOOL_CHARS)
    } else {
        m.content.clone()
    };
    let mut v = json!({"role": m.role, "content": content});
    if m.role == "tool" {
        v["tool_call_id"] = m.tool_call_id.clone().into();
    }
    if !m.tool_calls.is_empty() {
        v["tool_calls"] = m
            .tool_calls
            .iter()
            .map(|c| {
                // A call with no arguments streams none, but a gateway that
                // turns this into Anthropic's tool_use rejects "" as input:
                // it has to be an object.
                let arguments = if c.arguments.trim().is_empty() {
                    "{}"
                } else {
                    &c.arguments
                };
                json!({"id": c.id, "type": "function",
                    "function": {"name": c.name, "arguments": arguments}})
            })
            .collect();
    }
    v
}

/// Statuses worth one retry: gateway and edge failures that say nothing about
/// the request itself. 520-527 are Cloudflare's own (525 is a failed TLS
/// handshake between the edge and the origin).
pub(crate) fn transient(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 502 | 503 | 504 | 520..=527)
}

/// Turn a provider's complaint into something worth reading.
///
/// Two of them mean the same thing and neither says it: the route cannot
/// take a tool call back in the conversation. Google's adapter wants the
/// opaque `thought_signature` it issued, which the OpenAI-shaped protocol
/// has nowhere to carry, and then blames the id; the id is fine. It is not
/// something the caller can fix by trying again, so the message says what to
/// do instead.
fn explain(message: &str) -> String {
    let tool_history_unsupported = message.contains("thought_signature")
        || (message.contains("tool_calls") && message.contains("to be populated"));
    if tool_history_unsupported {
        return "This model cannot carry tool calls back into the conversation, so it                 stops as soon as it uses one. That is the route, not the request. Pick                 another model with /model -- the deepseek and claude routes handle                 tools."
            .to_owned();
    }
    message.chars().take(400).collect()
}

pub(crate) fn unwrap_response(value: &Value) -> Result<&Value, String> {
    if value.get("error").is_some() || value.get("success").is_some_and(|v| v != true) {
        let message = value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("The provider could not finish this response.");
        return Err(explain(message));
    }
    Ok(if value.get("success") == Some(&Value::Bool(true)) {
        &value["data"]
    } else {
        value
    })
}

#[derive(Default)]
struct Chunk {
    content: Option<String>,
    reasoning: Option<String>,
    completion_tokens: Option<u64>,
    prompt_tokens: Option<u64>,
    cached_tokens: Option<u64>,
    /// Fragments of `(index, id, name, arguments)`; the stream assembles them.
    tool_calls: Vec<(Option<usize>, String, String, String)>,
}

/// Streamed fragments put together into whole calls, in the order they began.
///
/// Gateways disagree on the shape. OpenAI sends the id and name once and then
/// the arguments in pieces. Some resend the id and the whole name with every
/// piece, so adding them up made `read_fileread_file`. Ollama sends several
/// calls whole, all at index zero, told apart only by their ids, and some
/// gateways send no index at all. So only the arguments are added up: an id
/// or a name replaces what was there, and an id or a name unlike the call's
/// own starts a new call.
#[derive(Default)]
struct Assembler {
    calls: Vec<ToolCall>,
    /// The call each index the gateway used is feeding now.
    slots: HashMap<usize, usize>,
    /// The call the last fragment fed, which one with no index carries on.
    active: Option<usize>,
}

impl Assembler {
    /// Add each fragment of a chunk to the call it belongs to.
    fn add(&mut self, chunk: &Chunk) {
        for (position, (index, id, name, arguments)) in chunk.tool_calls.iter().enumerate() {
            let open = match index {
                Some(index) => self.slots.get(index).copied(),
                // One array never holds two pieces of the same call, so only
                // the first can carry on the call before it.
                None if position == 0 => self.active,
                None => None,
            };
            // No gateway renames a call partway, so another name is another
            // call even when no id says so.
            let open = open.filter(|&slot| {
                let own = &self.calls[slot].name;
                name.is_empty() || own.is_empty() || own == name
            });
            let slot = match open {
                Some(slot) if id.is_empty() || self.calls[slot].id.is_empty() => slot,
                Some(slot) if self.calls[slot].id == *id => slot,
                _ => {
                    self.calls.push(ToolCall::default());
                    self.calls.len() - 1
                }
            };
            if let Some(index) = index {
                self.slots.insert(*index, slot);
            }
            self.active = Some(slot);
            let call = &mut self.calls[slot];
            if !id.is_empty() {
                call.id.clone_from(id);
            }
            if !name.is_empty() {
                call.name.clone_from(name);
            }
            call.arguments.push_str(arguments);
        }
    }
}

/// Give an id to any call that arrived without one.
///
/// The id is how a result is matched to its call, and most providers send it
/// with the first fragment -- but not all do, and one that does not leaves us
/// posting an empty string, which the next request is rejected for: "expected
/// the id of an assistant tool_calls array element to be populated". The
/// value only has to be unique within the turn and the same in both places,
/// so its position will do.
fn name_unnamed(calls: &mut [ToolCall]) {
    for (index, call) in calls.iter_mut().enumerate() {
        if call.id.trim().is_empty() {
            call.id = format!("call_{index}");
        }
    }
}

/// Call each tool by the name it was offered under.
///
/// Some gateways hand the name back in a case of their own -- `ListDir` for
/// `list_dir` -- and a call that cannot be found by name fails. Only case and
/// underscores are forgiven, so a tool the model made up still fails.
fn name_as_offered(calls: &mut [ToolCall]) {
    let loose = |name: &str| name.replace('_', "").to_lowercase();
    let tools = crate::tools::all();
    for call in calls {
        if let Some(tool) = tools.iter().find(|t| loose(t.name()) == loose(&call.name)) {
            call.name = tool.name().to_owned();
        }
    }
}

fn parse_event(data: &str) -> Result<Chunk, Failure> {
    let value: Value = serde_json::from_str(data).map_err(|_| {
        Failure::new(
            FailureKind::BadReply,
            "Provider returned an invalid stream event.",
        )
    })?;
    let inner = unwrap_response(&value)
        .map_err(|_| classify(None, &reqwest::header::HeaderMap::new(), &value))?;
    let delta = inner.pointer("/choices/0/delta");
    let content = delta
        .and_then(|d| d.get("content"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let reasoning = delta
        .and_then(|d| d.get("reasoning_content").or_else(|| d.get("reasoning")))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let completion_tokens = inner
        .pointer("/usage/completion_tokens")
        .and_then(Value::as_u64);
    let prompt_tokens = inner
        .pointer("/usage/prompt_tokens")
        .and_then(Value::as_u64);
    // OpenAI reports cache hits under prompt_tokens_details; DeepSeek uses its
    // own field. Gateways pass through whichever the upstream provider sent.
    let cached_tokens = inner
        .pointer("/usage/prompt_tokens_details/cached_tokens")
        .or_else(|| inner.pointer("/usage/prompt_cache_hit_tokens"))
        .and_then(Value::as_u64);
    let tool_calls = delta
        .and_then(|d| d["tool_calls"].as_array())
        .map(|calls| {
            calls
                .iter()
                .map(|c| {
                    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
                    // Some gateways number their calls rather than name them.
                    let id = match &c["id"] {
                        Value::Number(n) => n.to_string(),
                        v => text(v),
                    };
                    (
                        c["index"].as_u64().map(|n| n as usize),
                        id,
                        text(&c["function"]["name"]),
                        text(&c["function"]["arguments"]),
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(Chunk {
        content,
        reasoning,
        completion_tokens,
        prompt_tokens,
        cached_tokens,
        tool_calls,
    })
}

pub fn data_dir() -> std::path::PathBuf {
    if let Some(path) = std::env::var_os("AGENT_DATA_DIR") {
        return path.into();
    }
    std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            let under_home = if cfg!(target_os = "macos") {
                "Library/Application Support"
            } else {
                ".local/share"
            };
            std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(under_home))
        })
        .unwrap_or_else(std::env::temp_dir)
        .join("Letronna")
}

/// The one HTTP client every model call goes through.
///
/// A client per call meant a new runtime thread and a fresh TCP and TLS
/// handshake for each round of a turn -- up to `MAX_ROUNDS` of them, all to
/// the same gateway. This one keeps the connection open between rounds.
/// Timeouts are set per request, since a model list and a reply differ.
fn client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .map_err(|_| "Could not initialize the HTTP client.".to_owned())
        })
        .as_ref()
        .map_err(Clone::clone)
}

pub fn list_models(base: &str, key: &str) -> Result<Vec<String>, String> {
    let base = base.trim().trim_end_matches('/');
    let mut request = client()?
        .get(format!("{base}/models"))
        .timeout(Duration::from_secs(20));
    if !key.trim().is_empty() {
        request = request.bearer_auth(key.trim());
    }
    let response = request
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|e| match e.status() {
            Some(s) if s.as_u16() == 401 || s.as_u16() == 403 => {
                "The gateway rejected the API key.".to_owned()
            }
            _ => "Cannot load the model list from this gateway.".to_owned(),
        })?;
    let value: Value = response
        .json()
        .map_err(|_| "Invalid model list.".to_owned())?;
    let list = unwrap_response(&value)?["data"]
        .as_array()
        .ok_or("Invalid model list.")?;
    crate::models::remember(base, list);
    let models = list
        .iter()
        .filter_map(|m| m["id"].as_str().or_else(|| m["key"].as_str()))
        .map(str::to_owned)
        .collect();
    Ok(models)
}

pub fn start(connection: Connection, messages: Vec<Message>) -> async_channel::Receiver<Event> {
    let (sender, receiver) = async_channel::bounded(64);
    std::thread::spawn(move || {
        let result = stream(&connection, &messages, &sender);
        let _ = sender.send_blocking(Event::Finished(result));
    });
    receiver
}

fn stream(
    connection: &Connection,
    messages: &[Message],
    sender: &async_channel::Sender<Event>,
) -> Result<(), Failure> {
    let endpoint = validate(connection).map_err(|e| Failure::new(FailureKind::Settings, e))?;
    // What the window no longer holds goes in as a summary rather than not at
    // all. It is made here, on the worker, so the wait is part of the reply's.
    let dropped = &messages[..window_start(messages)];
    let summary = crate::compact::summary(connection, dropped);
    let mut request = client()
        .map_err(|e| Failure::new(FailureKind::Unknown, e))?
        .post(endpoint)
        .timeout(Duration::from_secs(120))
        .json(&body(connection, messages, summary.as_deref()));
    if !connection.key.trim().is_empty() {
        request = request.bearer_auth(connection.key.trim());
    }
    // A transient edge failure part way through an agent loop would otherwise
    // lose the whole task, so give it one more go before giving up.
    let mut attempt = 0;
    let response = loop {
        attempt += 1;
        let send = request
            .try_clone()
            .ok_or_else(|| Failure::new(FailureKind::Unknown, "Could not prepare the request."))?
            .send();
        match send {
            Ok(response) if transient(response.status()) && attempt == 1 => {
                std::thread::sleep(Duration::from_millis(600));
            }
            Ok(response) => break response,
            Err(_) if attempt == 1 => std::thread::sleep(Duration::from_millis(600)),
            Err(_) => {
                return Err(Failure::new(
                    FailureKind::Network,
                    "Cannot reach the model. Check that your server is running and the endpoint is correct (120-second request timeout).",
                ));
            }
        }
    };
    let status = response.status();
    if !status.is_success() {
        // A model that cannot take pictures turns the whole request down,
        // each provider in words of its own. Send it again without the
        // screenshot, which that model is spared from then on.
        if status.is_client_error() && shows_picture(connection, messages) {
            forget_images(&connection.model);
            return stream(connection, messages, sender);
        }
        let headers = response.headers().clone();
        let body = response.json::<Value>().unwrap_or_default();
        return Err(classify(Some(status.as_u16()), &headers, &body));
    }
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.contains("application/json"))
    {
        let value: Value = response
            .json()
            .map_err(|_| Failure::new(FailureKind::BadReply, "Invalid model response."))?;
        let content = unwrap_response(&value)
            .map_err(|_| classify(None, &reqwest::header::HeaderMap::new(), &value))?
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Failure::new(FailureKind::BadReply, "Model returned no text."))?
            .to_owned();
        sender
            .send_blocking(Event::Delta(content))
            .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        return Ok(());
    }
    let mut received = false;
    let mut complete = false;
    let mut completion_tokens = 0u64;
    let mut prompt_tokens = 0u64;
    let mut cached_tokens = 0u64;
    let mut assembler = Assembler::default();
    let began = std::time::Instant::now();
    for line in BufReader::new(response).lines() {
        let line = line.map_err(|_| {
            Failure::new(
                FailureKind::Network,
                "Connection interrupted before the response completed.",
            )
        })?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            complete = true;
            break;
        }
        let chunk = parse_event(data)?;
        if let Some(n) = chunk.completion_tokens {
            completion_tokens = n;
        }
        if let Some(n) = chunk.prompt_tokens {
            prompt_tokens = n;
        }
        if let Some(n) = chunk.cached_tokens {
            cached_tokens = n;
        }
        assembler.add(&chunk);
        if let Some(text) = chunk.reasoning {
            sender
                .send_blocking(Event::Reasoning(text))
                .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        }
        if let Some(text) = chunk.content {
            received = true;
            sender
                .send_blocking(Event::Delta(text))
                .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        }
    }
    if completion_tokens > 0 {
        let secs = began.elapsed().as_secs_f32().max(0.001);
        let _ = sender.send_blocking(Event::Usage {
            completion: completion_tokens,
            per_second: completion_tokens as f32 / secs,
            prompt: prompt_tokens,
            cached: cached_tokens,
        });
    }
    if !complete {
        return Err(Failure::new(
            FailureKind::Network,
            "The provider closed the stream early or does not support SSE chat completions.",
        ));
    }
    let mut calls = assembler.calls;
    calls.retain(|c| !c.name.is_empty());
    name_unnamed(&mut calls);
    name_as_offered(&mut calls);
    if !calls.is_empty() {
        received = true;
        sender
            .send_blocking(Event::ToolCalls(calls))
            .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
    }
    if !received {
        return Err(Failure::new(
            FailureKind::BadReply,
            "The model returned no text. Check model compatibility.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn connection() -> Connection {
        Connection {
            endpoint: "http://localhost:11434/v1/".into(),
            model: "test-model".into(),
            key: String::new(),
            tools: false,
            workspace: None,
            plan: false,
            remote: None,
            effort: String::new(),
            search: false,
            conversation: 0,
        }
    }
    #[test]
    fn data_dir_is_the_platform_convention() {
        // Both overrides win over HOME, so there is nothing to assert under them.
        if std::env::var_os("AGENT_DATA_DIR").is_some()
            || std::env::var_os("LOCALAPPDATA").is_some()
        {
            return;
        }
        let dir = data_dir();
        let expected = if cfg!(target_os = "macos") {
            "Library/Application Support/Letronna"
        } else {
            ".local/share/Letronna"
        };
        assert!(dir.ends_with(expected), "{dir:?}");
    }
    #[test]
    fn endpoint_validation() {
        assert_eq!(
            validate(&connection()).unwrap(),
            "http://localhost:11434/v1/chat/completions"
        );
        let mut c = connection();
        c.model.clear();
        assert!(validate(&c).is_err());
        c.model = "test".into();
        c.endpoint = "http://example.com/v1".into();
        assert!(validate(&c).is_err());
    }
    #[test]
    fn parses_text_and_provider_errors() {
        assert_eq!(
            parse_event(r#"{"choices":[{"delta":{"content":"hello"}}]}"#)
                .unwrap()
                .content,
            Some("hello".into())
        );
        assert!(parse_event(r#"{"error":{"message":"bad"}}"#).is_err());
        assert_eq!(
            parse_event(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#)
                .unwrap()
                .content,
            None
        );
    }
    #[test]
    #[ignore = "Talks to a real provider: set AGENT_BASE_URL, AGENT_API_KEY and AGENT_MODEL"]
    fn live_tool_loop() {
        let var = |name: &str| std::env::var(name).unwrap_or_else(|_| panic!("set {name}"));
        let connection = Connection {
            endpoint: var("AGENT_BASE_URL"),
            model: var("AGENT_MODEL"),
            key: var("AGENT_API_KEY"),
            tools: true,
            workspace: Some(std::env::current_dir().unwrap()),
            plan: false,
            remote: None,
            effort: String::new(),
            search: false,
            conversation: 0,
        };
        let ctx = crate::tools::ToolContext::of(&connection);
        let mut messages = vec![Message {
            role: "user".into(),
            content: "What is the package name in Cargo.toml? Read the file, then answer with just the name.".into(),
            ..Default::default()
        }];
        for round in 0..5 {
            let rx = start(connection.clone(), messages.clone());
            let mut reply = Message {
                role: "assistant".into(),
                ..Default::default()
            };
            loop {
                match rx.recv_blocking().unwrap() {
                    Event::Delta(t) => reply.content.push_str(&t),
                    Event::ToolCalls(calls) => reply.tool_calls = calls,
                    Event::Finished(r) => {
                        r.unwrap();
                        break;
                    }
                    _ => {}
                }
            }
            let calls = reply.tool_calls.clone();
            messages.push(reply);
            if calls.is_empty() {
                println!(
                    "answer after {round} tool rounds: {}",
                    messages.last().unwrap().content
                );
                assert!(messages.last().unwrap().content.contains("letronna"));
                assert!(round > 0, "model answered without reading the file");
                return;
            }
            for call in calls {
                println!("tool {} {}", call.name, call.arguments);
                let content =
                    crate::tools::run(&call.name, &call.arguments, &ctx).unwrap_or_else(|e| e);
                messages.push(Message {
                    role: "tool".into(),
                    content,
                    tool_call_id: call.id,
                    name: call.name,
                    ..Default::default()
                });
            }
        }
        panic!("no final answer");
    }
    #[test]
    fn context_keeps_complete_recent_turns() {
        let messages: Vec<_> = (0..30)
            .map(|i| Message {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: format!("message {i}"),
                ..Default::default()
            })
            .collect();
        let payload = body(&connection(), &messages, None);
        let context = payload["messages"].as_array().unwrap();
        assert_eq!(context.len(), 31);
        assert_eq!(context[1]["role"], "user");
        assert_eq!(context.last().unwrap()["content"], "message 29");
    }
    #[test]
    fn huge_tool_result_keeps_the_prompt() {
        // A tool result larger than the whole budget used to push the window
        // past the end, leaving the model nothing but the system prompt, which
        // it answered by introducing itself.
        let messages = vec![
            Message {
                role: "user".into(),
                content: "what is in the file?".into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                tool_calls: vec![ToolCall {
                    id: "a".into(),
                    name: "read_file".into(),
                    arguments: "{}".into(),
                }],
                ..Default::default()
            },
            Message {
                role: "tool".into(),
                tool_call_id: "a".into(),
                content: "x".repeat(CONTEXT_CHARS * 3),
                ..Default::default()
            },
        ];
        let payload = body(&connection(), &messages, None);
        let context = payload["messages"].as_array().unwrap();
        assert_eq!(context.len(), 4);
        assert_eq!(context[1]["content"], "what is in the file?");
        // The oversized result is trimmed rather than dropped.
        let sent = context[3]["content"].as_str().unwrap().chars().count();
        assert!(sent < MAX_TOOL_CHARS + 64, "{sent}");
    }
    #[test]
    fn a_summary_of_what_fell_out_rides_at_the_end_of_the_system_prompt() {
        let messages = [Message {
            role: "user".into(),
            content: "and now?".into(),
            ..Default::default()
        }];
        let payload = body(&connection(), &messages, Some("They chose Postgres."));
        let system = payload["messages"][0]["content"].as_str().unwrap();
        assert!(
            system.ends_with("no longer fits:\nThey chose Postgres."),
            "{system}"
        );
        let without = body(&connection(), &messages, None);
        let plain = without["messages"][0]["content"].as_str().unwrap();
        assert!(!plain.contains("summarized"));
    }
    #[test]
    fn the_two_ways_a_route_says_it_cannot_do_tools_read_the_same() {
        let google = super::explain(
            "Expected the 'id' of a(n) 'assistant' 'tool_calls' array element to be populated.",
        );
        let gemini =
            super::explain("Function call is missing a thought_signature in functionCall parts.");
        assert!(google.contains("/model"));
        assert_eq!(google, gemini);
        // Anything else is passed through as the provider wrote it.
        let other = super::explain("You have exceeded your quota.");
        assert_eq!(other, "You have exceeded your quota.");
    }

    #[test]
    fn a_call_without_an_id_is_given_one() {
        // Some providers stream the name and arguments but never an id.
        let mut calls = vec![
            ToolCall {
                id: String::new(),
                name: "ask_user".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "  ".into(),
                name: "read_file".into(),
                arguments: "{}".into(),
            },
            ToolCall {
                id: "given".into(),
                name: "write_file".into(),
                arguments: "{}".into(),
            },
        ];
        super::name_unnamed(&mut calls);
        assert_eq!(calls[0].id, "call_0");
        assert_eq!(calls[1].id, "call_1");
        // One the provider named keeps its own.
        assert_eq!(calls[2].id, "given");
        // Every call ends up with something to match a result against.
        assert!(calls.iter().all(|c| !c.id.trim().is_empty()));
    }

    #[test]
    fn a_tool_the_gateway_renamed_is_called_by_its_own_name() {
        let call = |name: &str| ToolCall {
            name: name.into(),
            ..Default::default()
        };
        let mut calls = vec![call("ListDir"), call("read_file"), call("ListEverything")];
        super::name_as_offered(&mut calls);
        assert_eq!(calls[0].name, "list_dir");
        assert_eq!(calls[1].name, "read_file");
        // Not one of ours under any case, so it still fails as unknown.
        assert_eq!(calls[2].name, "ListEverything");
    }

    #[test]
    fn every_call_shares_one_client() {
        let first = super::client().unwrap();
        let again = super::client().unwrap();
        assert!(std::ptr::eq(first, again));
    }

    #[test]
    fn a_call_with_no_arguments_goes_back_as_an_object() {
        let reply = Message {
            role: "assistant".into(),
            tool_calls: vec![ToolCall {
                id: "c1".into(),
                name: "list_dir".into(),
                arguments: String::new(),
            }],
            ..Default::default()
        };
        let wired = wire_message(&reply);
        assert_eq!(wired["tool_calls"][0]["function"]["arguments"], "{}");
    }

    /// The calls a stream of `data:` payloads comes to.
    fn assemble(events: &[&str]) -> Vec<ToolCall> {
        let mut assembler = Assembler::default();
        for data in events {
            assembler.add(&parse_event(data).unwrap());
        }
        assembler.calls
    }

    fn call(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    #[test]
    fn parallel_calls_each_at_their_own_index_come_out_in_order() {
        // OpenAI names a call in a piece of its own; llama.cpp starts the
        // arguments in the same piece.
        let openai = [
            r#"{"id":"chatcmpl-1","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":null,"tool_calls":[{"index":0,"id":"call_a","type":"function","function":{"name":"read_file","arguments":""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"call_b","type":"function","function":{"name":"list_dir","arguments":""}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"function":{"arguments":"{\"path\":\".\"}"}}]},"finish_reason":null}]}"#,
            r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
        ];
        assert_eq!(
            assemble(&openai),
            [
                call("call_a", "read_file", r#"{"path":"a.rs"}"#),
                call("call_b", "list_dir", r#"{"path":"."}"#),
            ]
        );
        let llama_cpp = [
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"Yq1u6lXIrXqZ5wD8","type":"function","function":{"name":"read_file","arguments":"{\"path\":"}}]},"finish_reason":null}],"object":"chat.completion.chunk"}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]},"finish_reason":null}],"object":"chat.completion.chunk"}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":1,"id":"Pm2kD9vQe7LwZx4N","type":"function","function":{"name":"list_dir","arguments":"{\"path\":\".\"}"}}]},"finish_reason":null}],"object":"chat.completion.chunk"}"#,
        ];
        assert_eq!(
            assemble(&llama_cpp),
            [
                call("Yq1u6lXIrXqZ5wD8", "read_file", r#"{"path":"a.rs"}"#),
                call("Pm2kD9vQe7LwZx4N", "list_dir", r#"{"path":"."}"#),
            ]
        );
    }

    #[test]
    fn a_name_and_id_sent_again_with_every_piece_are_not_doubled() {
        // MiniMax through NVIDIA NIM repeats both, which once made
        // `read_fileread_file` and failed as an unknown tool.
        let calls = assemble(&[
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"chatcmpl-tool-5c","type":"function","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"chatcmpl-tool-5c","type":"function","function":{"name":"read_file","arguments":"\"a.rs\"}"}}]}}]}"#,
        ]);
        assert_eq!(
            calls,
            [call("chatcmpl-tool-5c", "read_file", r#"{"path":"a.rs"}"#)]
        );
    }

    #[test]
    fn a_second_call_at_index_zero_with_a_new_id_is_its_own_call() {
        // Ollama sends each call whole at index zero, and Gemini relayed by a
        // gateway (the envelope here is OpenRouter's) can do the same. Two
        // reads of one tool used to glue into `{..}{..}`.
        let ollama = [
            r#"{"id":"chatcmpl-812","object":"chat.completion.chunk","model":"qwen3:8b","system_fingerprint":"fp_ollama","choices":[{"index":0,"delta":{"role":"assistant","content":"","tool_calls":[{"id":"call_4bq0x2mf","index":0,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]},"finish_reason":null}]}"#,
            r#"{"id":"chatcmpl-812","object":"chat.completion.chunk","model":"qwen3:8b","system_fingerprint":"fp_ollama","choices":[{"index":0,"delta":{"role":"assistant","content":"","tool_calls":[{"id":"call_9zt1c7ka","index":0,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"b.rs\"}"}}]},"finish_reason":null}]}"#,
        ];
        assert_eq!(
            assemble(&ollama),
            [
                call("call_4bq0x2mf", "read_file", r#"{"path":"a.rs"}"#),
                call("call_9zt1c7ka", "read_file", r#"{"path":"b.rs"}"#),
            ]
        );
        let gemini = [
            r#"{"id":"gen-1","provider":"Google","model":"google/gemini-2.5-flash","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"","reasoning_details":[{"type":"reasoning.encrypted","data":"CiQB0e2Kb3Jk","id":"tool_0_read_file_Qm9v","format":"google-gemini-v1","index":0}],"tool_calls":[{"index":0,"id":"tool_0_read_file_Qm9v","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]},"finish_reason":null}]}"#,
            r#"{"id":"gen-1","provider":"Google","model":"google/gemini-2.5-flash","object":"chat.completion.chunk","choices":[{"index":0,"delta":{"role":"assistant","content":"","tool_calls":[{"index":0,"id":"tool_1_read_file_YmFy","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"b.rs\"}"}}]},"finish_reason":"tool_calls"}]}"#,
        ];
        assert_eq!(
            assemble(&gemini),
            [
                call("tool_0_read_file_Qm9v", "read_file", r#"{"path":"a.rs"}"#),
                call("tool_1_read_file_YmFy", "read_file", r#"{"path":"b.rs"}"#),
            ]
        );
    }

    #[test]
    fn a_second_call_with_another_name_at_the_same_index_is_its_own_call() {
        // Calls sent one to a chunk with no id, at a shared index or at none,
        // must not let the second tool run with the first call's arguments.
        let expected = [
            call("", "read_file", r#"{"path":"a.rs"}"#),
            call("", "list_dir", r#"{"path":"."}"#),
        ];
        let shared_index = assemble(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"type":"function","function":{"name":"list_dir","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":\".\"}"}}]}}]}"#,
        ]);
        assert_eq!(shared_index, expected);
        let no_index = assemble(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"type":"function","function":{"name":"list_dir","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{\"path\":\".\"}"}}]}}]}"#,
        ]);
        assert_eq!(no_index, expected);
    }

    #[test]
    fn whole_calls_in_one_array_without_an_index_stay_apart() {
        // Google's own OpenAI-compatible endpoint sends every call at once on
        // the finishing chunk, with no index and at times no id.
        let calls = assemble(&[
            r#"{"choices":[{"delta":{"role":"assistant","tool_calls":[{"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}},{"type":"function","function":{"name":"read_file","arguments":"{\"path\":\"b.rs\"}"}}]},"finish_reason":"tool_calls","index":0}]}"#,
        ]);
        assert_eq!(
            calls,
            [
                call("", "read_file", r#"{"path":"a.rs"}"#),
                call("", "read_file", r#"{"path":"b.rs"}"#),
            ]
        );
    }

    #[test]
    fn a_piece_without_an_index_carries_on_the_call_before_it() {
        let calls = assemble(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_a","type":"function","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"id":"call_b","type":"function","function":{"name":"list_dir","arguments":"{}"}}]}}]}"#,
        ]);
        assert_eq!(
            calls,
            [
                call("call_a", "read_file", r#"{"path":"a.rs"}"#),
                call("call_b", "list_dir", "{}"),
            ]
        );
    }

    #[test]
    fn a_numeric_id_survives() {
        let calls = assemble(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":7,"type":"function","function":{"name":"read_file","arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":7,"function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
        ]);
        assert_eq!(calls, [call("7", "read_file", r#"{"path":"a.rs"}"#)]);
    }

    #[test]
    fn assembles_streamed_tool_calls() {
        let calls = assemble(&[
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
        ]);
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "c1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"a.rs"}"#);
        let names = |plan: bool| -> Vec<String> {
            body(
                &Connection {
                    tools: true,
                    plan,
                    ..connection()
                },
                &[],
                None,
            )["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| t["function"]["name"].as_str().unwrap().to_owned())
                .collect()
        };
        let full = names(false);
        assert!(full.contains(&"read_file".to_owned()));
        assert!(full.contains(&"write_file".to_owned()));
        // Plan mode offers the same tools minus everything that acts.
        let planning = names(true);
        assert!(planning.contains(&"read_file".to_owned()));
        for blocked in ["write_file", "edit_file", "run_command", "stop_job"] {
            assert!(
                !planning.contains(&blocked.to_owned()),
                "{blocked} in plan mode"
            );
        }
    }
    #[test]
    fn a_remote_session_is_told_where_it_is() {
        let system = |remote: Remote| -> String {
            body(
                &Connection {
                    tools: true,
                    remote: Some(remote),
                    ..connection()
                },
                &[],
                None,
            )["messages"][0]["content"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        let server = system(Remote {
            place: "#general in a Discord server".into(),
            shared: true,
            history: Some(History::default()),
        });
        assert!(server.contains("talking through #general in a Discord server"));
        assert!(server.contains("read_chat"));
        assert!(server.contains("Nothing on this computer is reachable"));
        // Nothing that invites a look around the disk.
        assert!(!server.contains("home folder"), "{server}");

        let telegram = system(Remote {
            place: "a Telegram chat".into(),
            ..Default::default()
        });
        assert!(telegram.contains("home folder"));
        assert!(!telegram.contains("read_chat"));
    }
    #[test]
    fn window_holds_still_so_the_cache_prefix_survives() {
        // Providers cache on an exact prefix, so the front of the request must
        // not move on every turn.
        let turn = |i: usize| {
            [
                Message {
                    role: "user".into(),
                    content: format!("question {i} {}", "x".repeat(600)),
                    ..Default::default()
                },
                Message {
                    role: "assistant".into(),
                    content: format!("answer {i} {}", "y".repeat(600)),
                    ..Default::default()
                },
            ]
        };
        let mut messages: Vec<Message> = Vec::new();
        let mut starts = Vec::new();
        for i in 0..40 {
            messages.extend(turn(i));
            starts.push(window_start(&messages));
        }

        // It must compact at some point, or the budget means nothing.
        let compactions = starts.windows(2).filter(|w| w[0] != w[1]).count();
        assert!(compactions > 0, "window never compacted: {starts:?}");
        // But rarely: sliding one turn at a time would move it nearly every turn.
        assert!(
            compactions < starts.len() / 4,
            "window moved {compactions} times in {} turns: {starts:?}",
            starts.len()
        );
        // And it only ever moves forward, onto a user turn.
        assert!(starts.windows(2).all(|w| w[0] <= w[1]), "{starts:?}");
        for start in &starts {
            assert_eq!(messages[*start].role, "user", "start {start} is not a turn");
        }
        assert!(budget(&messages[*starts.last().unwrap()..]) <= CONTEXT_CHARS);
    }

    #[test]
    fn transient_statuses_are_retried_not_blamed_on_the_user() {
        use reqwest::StatusCode;
        for code in [502, 503, 504, 520, 525, 527] {
            assert!(transient(StatusCode::from_u16(code).unwrap()), "{code}");
        }
        for code in [400, 401, 403, 404, 429, 500] {
            assert!(!transient(StatusCode::from_u16(code).unwrap()), "{code}");
        }
    }
    #[test]
    fn dangling_tool_calls_get_placeholder_results() {
        let messages = vec![
            Message {
                role: "user".into(),
                content: "go".into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                tool_calls: vec![
                    ToolCall {
                        id: "a".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                    ToolCall {
                        id: "b".into(),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    },
                ],
                ..Default::default()
            },
            Message {
                role: "tool".into(),
                tool_call_id: "b".into(),
                content: "ok".into(),
                ..Default::default()
            },
            Message {
                role: "user".into(),
                content: "again".into(),
                ..Default::default()
            },
        ];
        let payload = body(&connection(), &messages, None);
        let context = payload["messages"].as_array().unwrap();
        let tools: Vec<&str> = context
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["tool_call_id"].as_str().unwrap())
            .collect();
        assert_eq!(tools, ["a", "b"]);
    }
    #[test]
    fn streams_from_local_http_server() {
        use std::{
            io::{Read, Write},
            net::TcpListener,
        };
        let server = TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut block = [0; 4096];
            loop {
                let n = socket.read(&mut block).unwrap();
                bytes.extend_from_slice(&block[..n]);
                if let Some(header_end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..header_end]).to_lowercase();
                    let len: usize = headers
                        .lines()
                        .find_map(|l| l.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= header_end + 4 + len {
                        let payload: Value =
                            serde_json::from_slice(&bytes[header_end + 4..]).unwrap();
                        assert_eq!(payload["stream"], true);
                        break;
                    }
                }
                assert_ne!(n, 0);
            }
            let body =
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n";
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
        });
        let mut c = connection();
        c.endpoint = endpoint;
        let rx = start(
            c,
            vec![Message {
                role: "user".into(),
                content: "Hi".into(),
                ..Default::default()
            }],
        );
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "Hello"));
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished(Ok(()))
        ));
        worker.join().unwrap();
    }

    /// A conversation in which browse took two screenshots, the second in a
    /// batch with another call.
    fn screenshots(dir: &std::path::Path) -> Vec<Message> {
        std::fs::create_dir_all(dir).unwrap();
        let (old, new) = (dir.join("old.jpg"), dir.join("new.jpg"));
        std::fs::write(&old, [0xFF, 0xD8, 0xFF, 1]).unwrap();
        std::fs::write(&new, [0xFF, 0xD8, 0xFF, 2]).unwrap();
        let call = |id: &str, name: &str| ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: "{}".into(),
        };
        let result = |id: &str, name: &str, content: String| Message {
            role: "tool".into(),
            tool_call_id: id.into(),
            name: name.into(),
            content,
            ..Default::default()
        };
        vec![
            Message {
                role: "user".into(),
                content: "look at it".into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                tool_calls: vec![call("a", "browse")],
                ..Default::default()
            },
            result(
                "a",
                "browse",
                format!("Saved to {}\nA screenshot.", old.display()),
            ),
            Message {
                role: "assistant".into(),
                tool_calls: vec![call("b", "browse"), call("c", "read_file")],
                ..Default::default()
            },
            result(
                "b",
                "browse",
                format!("Saved to {}\nA screenshot.", new.display()),
            ),
            result("c", "read_file", "notes".into()),
        ]
    }

    #[test]
    fn the_newest_screenshot_is_shown_to_the_model() {
        let dir = std::env::temp_dir().join(format!("letronna-shots-{}", std::process::id()));
        let messages = screenshots(&dir);
        let sent = |model: &str| -> Vec<Value> {
            let c = Connection {
                model: model.into(),
                ..connection()
            };
            body(&c, &messages, None)["messages"]
                .as_array()
                .unwrap()
                .clone()
        };
        let context = sent("sees-images");
        let pictures: Vec<usize> = (0..context.len())
            .filter(|at| context[*at]["content"].is_array())
            .collect();
        // One picture, the newer, after the last result of its batch: a
        // call's result has to come straight after the call.
        assert_eq!(pictures, [context.len() - 1]);
        assert_eq!(context[context.len() - 2]["tool_call_id"], "c");
        let picture = &context[context.len() - 1]["content"][1]["image_url"]["url"];
        assert_eq!(picture, "data:image/jpeg;base64,/9j/Ag==");
        // A model that turned an image down is sent none.
        forget_images("cannot-see");
        assert!(sent("cannot-see").iter().all(|m| !m["content"].is_array()));
        // Nor is one sent for a file that has since been deleted.
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(sent("sees-images").iter().all(|m| !m["content"].is_array()));
    }

    /// The body of the next request to `socket`.
    fn request_body(socket: &mut std::net::TcpStream) -> String {
        use std::io::Read;
        let mut bytes = Vec::new();
        let mut block = [0; 65536];
        loop {
            let n = socket.read(&mut block).unwrap();
            assert_ne!(n, 0);
            bytes.extend_from_slice(&block[..n]);
            let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let length: usize = headers
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .unwrap()
                .parse()
                .unwrap();
            if bytes.len() >= end + 4 + length {
                return String::from_utf8_lossy(&bytes[end + 4..]).into_owned();
            }
        }
    }

    #[test]
    fn a_request_an_image_spoilt_is_sent_again_without_it() {
        use std::io::Write;
        let dir = std::env::temp_dir().join(format!("letronna-spoilt-{}", std::process::id()));
        let messages = screenshots(&dir);
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            // What DeepSeek says to a picture, then an answer.
            let replies = [
                (
                    "400 Bad Request",
                    "application/json",
                    r#"{"error":{"message":"unknown variant `image_url`, expected `text`"}}"#,
                ),
                (
                    "200 OK",
                    "text/event-stream",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"A form\"}}]}\n\ndata: [DONE]\n\n",
                ),
            ];
            let mut bodies = Vec::new();
            for (status, kind, reply) in replies {
                let (mut socket, _) = server.accept().unwrap();
                bodies.push(request_body(&mut socket));
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            bodies
        });
        let c = Connection {
            endpoint,
            model: "turns-images-down".into(),
            ..connection()
        };
        let rx = start(c, messages);
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "A form"));
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished(Ok(()))
        ));
        let bodies = worker.join().unwrap();
        assert!(bodies[0].contains("image_url"));
        assert!(!bodies[1].contains("image_url"));
        assert!(!sees_images("turns-images-down"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn cached_input_counted_beside_the_prompt_is_added_to_it() {
        let inside = Message {
            prompt_tokens: 3291,
            cached_tokens: 3200,
            ..Default::default()
        };
        assert_eq!(inside.input_tokens(), 3291);
        let beside = Message {
            prompt_tokens: 100,
            cached_tokens: 1833,
            ..Default::default()
        };
        assert_eq!(beside.input_tokens(), 1933);
    }

    #[test]
    fn an_error_partway_through_a_stream_is_classified_like_one_before_it() {
        // OpenRouter's event for a provider that failed after the stream began.
        let data = r#"{"id":"gen-1759000000-abc","object":"chat.completion.chunk","model":"anthropic/claude-sonnet-4","provider":"Anthropic","error":{"code":"server_error","message":"Provider disconnected unexpectedly"},"choices":[{"index":0,"delta":{"content":""},"finish_reason":"error"}]}"#;
        let Err(failure) = parse_event(data) else {
            panic!("an error event was read as a chunk");
        };
        assert_eq!(failure.kind, FailureKind::Server);
        assert_eq!(failure.to_string(), "Provider disconnected unexpectedly");
    }

    #[test]
    fn a_refused_request_ends_with_its_kind_and_the_wait_asked_for() {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            request_body(&mut socket);
            let reply = r#"{"error":{"message":"Rate limit reached for gpt-4o in organization org-abc on requests per min (RPM): Limit 500, Used 500, Requested 1.","type":"requests","param":null,"code":"rate_limit_exceeded"}}"#;
            write!(socket, "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 3\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        });
        let c = Connection {
            endpoint,
            ..connection()
        };
        let rx = start(
            c,
            vec![Message {
                role: "user".into(),
                content: "Hi".into(),
                ..Default::default()
            }],
        );
        let Event::Finished(Err(failure)) = rx.recv_blocking().unwrap() else {
            panic!("a refused request did not end in a failure");
        };
        assert_eq!(failure.kind, FailureKind::RateLimit);
        assert_eq!(failure.retry_after, Some(Duration::from_secs(3)));
        assert!(
            failure
                .to_string()
                .starts_with("Rate limit reached for gpt-4o")
        );
        worker.join().unwrap();
    }
}
