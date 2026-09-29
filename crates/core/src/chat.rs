use crate::cancel::Cancel;
use crate::failure::{Failure, FailureKind, classify};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    any::Any,
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
    /// For `role: "tool"`: the files the call wrote, so a rewind can put them
    /// back. Left out of the saved line when there are none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<crate::history::FileChange>,
    /// Set on a user message that went into a turn already running, such as a
    /// follow-up typed while its tools ran, rather than starting a turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
    /// Set on a message that reports something that happened meanwhile
    /// rather than something the user said, and is sent to the model as the
    /// user's turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub notice: bool,
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
    /// Nobody may be watching, as in a schedule or a chat app, so a failed
    /// call is tried more times and may wait longer.
    pub unattended: bool,
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
    /// A try failed in a way that passes, and another follows after a wait.
    Retrying(crate::retry::Retry),
    /// Sent before `Finished` when the reply stopped at the output limit;
    /// `in_call` when it stopped inside a tool call, which is then dropped
    /// rather than run half written.
    CutOff {
        in_call: bool,
    },
    Finished(Result<(), Failure>),
}

/// Why the model stopped, as the provider said.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    /// A reason not known here, which still means the reply is over.
    Other,
}

impl FinishReason {
    /// Read a `finish_reason`, including the names Anthropic and Gemini give
    /// the same stops, which some gateways pass through unchanged.
    fn parse(reason: &str) -> Self {
        match reason.to_ascii_lowercase().as_str() {
            "stop" | "end_turn" | "stop_sequence" => Self::Stop,
            "length" | "max_tokens" => Self::Length,
            "tool_calls" | "function_call" | "tool_use" => Self::ToolCalls,
            "content_filter" | "safety" | "refusal" => Self::ContentFilter,
            _ => Self::Other,
        }
    }
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

/// Budget for the whole conversation window, in estimated tokens: the 24,000
/// characters it used to be, at four characters a token of English.
const CONTEXT_TOKENS: usize = 6_000;
/// A single tool result can be far larger than anything the model needs to
/// read. The chat keeps the whole thing; the request gets the ends.
pub(crate) const MAX_TOOL_CHARS: usize = 8_000;
/// What a message costs beyond its text: the role and separators a chat
/// template wraps it in, which OpenAI counts as three or four tokens.
const MESSAGE_TOKENS: usize = 4;
/// What the screenshot a request carries costs: a 1280×800 page -- browse
/// takes none wider -- is about 1,400 tokens as Anthropic counts pictures
/// (width × height / 750) and 1,100 as OpenAI does.
const PICTURE_TOKENS: usize = 1_500;

/// About how many tokens `text` is, without a tokenizer.
///
/// Tokenizers average about four bytes a token on English and code, which
/// is also how they fare on two-byte letters such as Cyrillic or accented
/// Latin. A character of three or four bytes -- CJK, kana, Hangul, Thai,
/// Indic scripts, emoji -- is rarely merged with its neighbours, so each
/// counts as a token by itself; at four bytes a token a page of Chinese
/// would count for three quarters of what it costs. Symbols of three bytes
/// count the same, which is more than a run of them costs: tokenizers merge
/// box drawing, blocks and arrows, as in a `tree` listing or a progress bar.
fn tokens(text: &str) -> usize {
    if text.is_ascii() {
        return text.len().div_ceil(4);
    }
    let (mut wide, mut narrow) = (0, text.len());
    for byte in text.bytes() {
        // Only the first byte of a three- or four-byte character is this high.
        let width = match byte {
            0xE0..=0xEF => 3,
            0xF0..=0xFF => 4,
            _ => continue,
        };
        wide += 1;
        narrow -= width;
    }
    wide + narrow.div_ceil(4)
}

/// About what `m` costs in a request: its text as sent, the calls it makes
/// with their arguments, and its role.
fn message_tokens(m: &Message) -> usize {
    // A long tool result goes out trimmed to its ends. A string never has
    // fewer bytes than characters, so a short one is never trimmed.
    let content = if m.role == "tool" && m.content.len() > MAX_TOOL_CHARS {
        tokens(&trim(&m.content, MAX_TOOL_CHARS))
    } else {
        tokens(&m.content)
    };
    let calls: usize = m
        .tool_calls
        .iter()
        .map(|c| tokens(&c.name) + tokens(&c.arguments))
        .sum();
    MESSAGE_TOKENS + content + calls
}

/// The estimate for every start of `messages`: `sums[n]` is the first `n`
/// messages', so any stretch of them costs one subtraction.
///
/// The screenshot is counted where it would go, whether or not the model
/// takes pictures, so the window does not move when one turns out not to.
fn token_sums(messages: &[Message]) -> Vec<usize> {
    let picture = screenshot(messages).map(|(after, _)| after);
    let mut sums = Vec::with_capacity(messages.len() + 1);
    let mut total = 0;
    sums.push(total);
    for (i, m) in messages.iter().enumerate() {
        total += message_tokens(m);
        if picture == Some(i) {
            total += PICTURE_TOKENS;
        }
        sums.push(total);
    }
    sums
}

/// Messages in the window, whatever the token budget says. Guards against
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
/// no prompt at all, and it answers by introducing itself. A message folded into
/// a running turn is part of it, so a follow-up never pushes the turn's own
/// request out into the summary.
///
/// This is deliberately stateless: the same message list always yields the same
/// start, so nothing needs saving and two requests over the same history agree.
fn window_start(messages: &[Message]) -> usize {
    let last_user = messages.iter().rposition(opens_turn).unwrap_or(0);
    let sums = token_sums(messages);
    let total = sums[messages.len()];
    let mut start = 0;
    while start < last_user
        && (total - sums[start] > CONTEXT_TOKENS || messages.len() - start > MAX_MESSAGES)
    {
        // Drop a fixed step, then round forward to a user turn so the window
        // never begins midway through a tool exchange.
        let next = (start + COMPACT_STEP).min(last_user);
        start = (next..=last_user)
            .find(|i| opens_turn(&messages[*i]))
            .unwrap_or(last_user);
    }
    start
}

/// Whether a message starts a turn: the user's, and not folded into one running.
pub(crate) fn opens_turn(message: &Message) -> bool {
    // Roles alone cannot tell a follow-up folded in after tool results from a
    // prompt sent after a step was stopped or failed, which follows them too.
    message.role == "user" && !message.folded
}

#[cfg(test)]
fn body(
    connection: &Connection,
    window: &[Message],
    summary: Option<&str>,
    plan: Option<&str>,
) -> Value {
    body_with(connection, window, summary, plan, &[])
}

/// The request, offering `extra` after every tool the connection offers.
fn body_with(
    connection: &Connection,
    window: &[Message],
    summary: Option<&str>,
    plan: Option<&str>,
    extra: &[Value],
) -> Value {
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
        system.push_str(" Plan mode is on: you can read, search and ask, but you cannot write files or run commands, and those tools are unavailable.");
        // Where exit_plan is offered, a plan also written into the reply
        // would reach the user twice.
        if connection.tools && crate::tools::offers_plan(connection) {
            system.push_str(" Work out what you would do, and when the plan is ready, put it to the user with exit_plan instead of writing it out in your reply; if they approve, plan mode turns off and you carry it out.");
        } else {
            system.push_str(" Work out what you would do and lay out the plan, then let the user turn plan mode off to carry it out.");
        }
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
    // Models reckon "today" from their training and guess who they are. The
    // date alone changes once a day, so the prefix above stays cached; the OS
    // says something about this computer, so a shared chat goes without it.
    system.push_str(&format!(
        "\n\nToday is {}.",
        chrono::Local::now().format("%A %Y-%m-%d")
    ));
    if !shared {
        let os = match std::env::consts::OS {
            "windows" => "Windows",
            "macos" => "macOS",
            "linux" => "Linux",
            other => other,
        };
        system.push_str(&format!(" This computer runs {os}."));
    }
    system.push_str(&format!(" The model behind you is {}.", connection.model));
    if let Some(summary) = summary {
        system.push_str(&format!(
            "\n\nEarlier in this conversation, summarized because it no longer fits:\n{summary}"
        ));
    }
    let mut context = vec![json!({"role": "system", "content": system})];
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
    // The context window can drop the call that made the plan, and a long
    // task is when that happens. The plan rides on the end of the request,
    // after everything a provider could have cached, and only on the wire:
    // the stored message never carries it.
    if let Some(note) = plan
        && let Some(last) = context.last_mut()
    {
        match &mut last["content"] {
            Value::String(text) => text.push_str(&format!("\n\n{note}")),
            Value::Array(parts) => parts.push(json!({"type": "text", "text": note})),
            _ => {}
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
        // Last, so the tools before them go out byte for byte as they would
        // without them, and a provider's cached prefix holds up to here.
        tools.extend_from_slice(extra);
        body["tools"] = Value::Array(tools);
    }
    // Most models that cannot be told how hard to think ignore this, so it
    // goes to all of them; one that refuses it is sent none after. Measured
    // on one that can: sixty-four reasoning tokens at low against seven
    // hundred at high.
    if !connection.effort.is_empty() {
        body["reasoning_effort"] = connection.effort.clone().into();
    }
    for (quirk, _) in QUIRKS {
        if has_quirk(connection, quirk) {
            apply_quirk(&mut body, quirk);
        }
    }
    body
}

/// The model's checklist as a note to end the request with, or none once
/// nothing on it is left to do.
fn checklist(messages: &[Message]) -> Option<String> {
    let todos = crate::store::todos_in(messages);
    if todos.iter().all(|t| t.status == "done") {
        return None;
    }
    let items: Vec<String> = todos
        .iter()
        .map(|t| {
            let mark = match t.status.as_str() {
                "done" => "[x]",
                "doing" => "[>]",
                _ => "[ ]",
            };
            format!("{mark} {}", t.text)
        })
        .collect();
    // Said to be from Letronna: it lands after a tool's output or the user's
    // words, and must not read as part of either.
    Some(format!(
        "(From Letronna, not part of the above: your checklist as your last todo call left it.)\n{}",
        items.join("\n")
    ))
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

/// Whether the request `sent` carries a picture.
fn shows_picture(sent: &Value) -> bool {
    sent["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|m| {
            m["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part["type"] == "image_url"))
        })
    })
}

/// A field some servers turn a whole request down for, which the request
/// can do without.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Quirk {
    /// The output cap goes as `max_completion_tokens`, the only name
    /// OpenAI's reasoning models take it by.
    CompletionTokens,
    Effort,
    StreamOptions,
    WebSearch,
    CacheControl,
}

/// Each quirk by what a refusal says when its field is the one at fault: the
/// field's name, or for the cap the name it asks for instead, since a server
/// out of room names both caps and a new name would not give it more.
const QUIRKS: [(Quirk, &str); 5] = [
    (Quirk::CompletionTokens, "'max_completion_tokens' instead"),
    (Quirk::Effort, "reasoning_effort"),
    (Quirk::StreamOptions, "stream_options"),
    (Quirk::WebSearch, "web_search"),
    (Quirk::CacheControl, "cache_control"),
];

/// The cap sent as `max_completion_tokens`, which counts a model's thinking
/// as well as its answer: the 8192 an answer gets elsewhere and as much again
/// to think in, the most every model `reasons_on_openai` matches will take
/// (GPT-5's chat models stop at 16384).
const COMPLETION_TOKENS: u64 = 16384;

/// Quirks learned from refusals, by endpoint and model, for the rest of this
/// run. Like `sightless` they are never saved: a refusal matched by mistake
/// then costs a feature until the next start rather than for good, and
/// learning it again costs one request.
fn learned() -> &'static Mutex<HashSet<(String, String, Quirk)>> {
    static LEARNED: OnceLock<Mutex<HashSet<(String, String, Quirk)>>> = OnceLock::new();
    LEARNED.get_or_init(Default::default)
}

/// The endpoint and model a quirk is learned for: a field one server turns
/// down, another serving the same model may take.
fn route(connection: &Connection) -> (String, String) {
    (
        connection.endpoint.trim().trim_end_matches('/').to_owned(),
        connection.model.trim().to_owned(),
    )
}

/// Whether requests to this model need `quirk`, learned this run or known
/// before the first one is sent.
fn has_quirk(connection: &Connection, quirk: Quirk) -> bool {
    if quirk == Quirk::CompletionTokens && reasons_on_openai(connection) {
        return true;
    }
    let (endpoint, model) = route(connection);
    learned()
        .lock()
        .is_ok_and(|quirks| quirks.contains(&(endpoint, model, quirk)))
}

/// OpenAI's o-series and GPT-5 models on OpenAI's own API, which refuse
/// `max_tokens`. Another route to them that does too says so, and is learned.
fn reasons_on_openai(connection: &Connection) -> bool {
    let on_openai = reqwest::Url::parse(connection.endpoint.trim())
        .is_ok_and(|url| url.host_str() == Some("api.openai.com"));
    let model = connection.model.trim().to_lowercase();
    on_openai
        && ["o1", "o3", "o4", "gpt-5"]
            .iter()
            .any(|family| model.starts_with(family))
}

/// Change `body` as `quirk` asks, saying whether it carried the field.
fn apply_quirk(body: &mut Value, quirk: Quirk) -> bool {
    let Some(fields) = body.as_object_mut() else {
        return false;
    };
    match quirk {
        Quirk::CompletionTokens => {
            if fields.remove("max_tokens").is_none() {
                return false;
            }
            fields.insert("max_completion_tokens".into(), COMPLETION_TOKENS.into());
            true
        }
        Quirk::Effort => fields.remove("reasoning_effort").is_some(),
        Quirk::StreamOptions => fields.remove("stream_options").is_some(),
        Quirk::WebSearch => {
            let Some(tools) = fields.get_mut("tools").and_then(Value::as_array_mut) else {
                return false;
            };
            let offered = tools.len();
            tools.retain(|tool| tool["type"] != "web_search");
            tools.len() < offered
        }
        // Markers ride on message parts, not at the top.
        Quirk::CacheControl => remove_everywhere(body, "cache_control"),
    }
}

/// Take `key` out of every object in `value`, saying whether there was one.
fn remove_everywhere(value: &mut Value, key: &str) -> bool {
    let mut found = false;
    match value {
        Value::Object(fields) => {
            found = fields.remove(key).is_some();
            for inner in fields.values_mut() {
                found |= remove_everywhere(inner, key);
            }
        }
        Value::Array(items) => {
            for inner in items {
                found |= remove_everywhere(inner, key);
            }
        }
        _ => {}
    }
    found
}

/// Learn from a refusal what the request `sent` can do without, saying
/// whether sending it again would now go differently.
///
/// A strict server turns the whole request down over one field it does not
/// take, and names it, in snake or camel case. A model that cannot see turns
/// a picture down in words of its own, most of which say image; any other
/// refusal of a request that happens to carry one is no reason to stop
/// showing that model pictures, though one that never says image ("content
/// must be a string") is then shown to the user rather than retried. Only
/// what the error names and the request carried is learned, and the next
/// request leaves it out, so each field costs at most one retry and a refusal
/// that repeats itself ends it.
fn learn(connection: &Connection, sent: &Value, error: &str) -> bool {
    // An HTML error page from a proxy says image in its icons and previews.
    let json = serde_json::from_str::<Value>(error).is_ok();
    let error = error.to_lowercase().replace('_', "");
    let mut learned_any = false;
    // Not "vision": it is also in "provisioned", as in a throughput limit.
    let about_images = json
        && ["image", "multimodal"]
            .iter()
            .any(|word| error.contains(word));
    if about_images && shows_picture(sent) {
        forget_images(&connection.model);
        learned_any = !sees_images(&connection.model);
    }
    let (endpoint, model) = route(connection);
    for (quirk, field) in QUIRKS {
        // A request that went out alongside this one may have learned it
        // first; this one still goes again without it.
        if error.contains(&field.replace('_', ""))
            && apply_quirk(&mut sent.clone(), quirk)
            && learned().lock().is_ok_and(|mut quirks| {
                quirks.insert((endpoint.clone(), model.clone(), quirk));
                true
            })
        {
            learned_any = true;
        }
    }
    learned_any
}

/// Keep both ends of an oversized tool result; the middle is rarely the point.
pub(crate) fn trim(text: &str, max: usize) -> String {
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

/// Gateway and edge failures that say nothing about the request itself.
/// 520-527 are Cloudflare's own (525 is a failed TLS handshake between the
/// edge and the origin).
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
        return "This model cannot carry tool calls back into the conversation, so it \
                stops as soon as it uses one. That is the route, not the request. Pick \
                another model with /model -- the deepseek and claude routes handle \
                tools."
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
    finish: Option<FinishReason>,
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
fn name_as_offered(calls: &mut [ToolCall], extra: &[Value]) {
    let loose = |name: &str| name.replace('_', "").to_lowercase();
    let tools = crate::tools::all();
    let extra = extra
        .iter()
        .filter_map(|spec| spec.pointer("/function/name")?.as_str());
    let offered: Vec<&str> = tools.iter().map(|t| t.name()).chain(extra).collect();
    for call in calls {
        if let Some(name) = offered.iter().find(|n| loose(n) == loose(&call.name)) {
            call.name = (*name).to_owned();
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
    parse_chunk(&value)
}

/// Read one stream event, or the whole reply of a server that does not
/// stream, which has under `message` what an event has under `delta`.
fn parse_chunk(value: &Value) -> Result<Chunk, Failure> {
    let inner = unwrap_response(value)
        .map_err(|_| classify(None, &reqwest::header::HeaderMap::new(), value))?;
    let delta = inner
        .pointer("/choices/0/delta")
        .or_else(|| inner.pointer("/choices/0/message"));
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
    // vLLM also sends a `stop_reason`, but there it is the stop string that
    // matched rather than why, so only `finish_reason` is read.
    let finish = inner
        .pointer("/choices/0/finish_reason")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(FinishReason::parse);
    Ok(Chunk {
        content,
        reasoning,
        completion_tokens,
        prompt_tokens,
        cached_tokens,
        tool_calls,
        finish,
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

/// How long a reply may go quiet before it is given up on: five minutes
/// covers a local model reading a long prompt, or a reasoning model thinking,
/// before it says anything.
const IDLE: Duration = Duration::from_secs(300);

/// The one HTTP client every model call goes through.
///
/// A client per call meant a new runtime thread and a fresh TCP and TLS
/// handshake for each round of a turn -- up to `MAX_ROUNDS` of them, all to
/// the same gateway. This one keeps the connection open between rounds.
/// A reply is bounded by how long it goes quiet, never by its length, and a
/// call that needs a deadline, such as a model list, sets its own.
pub(crate) fn client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .connect_timeout(Duration::from_secs(10))
                // In the blocking client this bounds the wait for the headers
                // and each read after them, not the whole body. A timeout on
                // the request instead is a deadline for the entire reply.
                .timeout(IDLE)
                // reqwest's own keepalive probes every 15 s and takes a minute
                // to find a peer that vanished mid-reply (165 s on Windows;
                // Linux already stops at 30 s). Probing every 5 s finds it in
                // about 30 s (65 s on Windows, which always makes ten).
                .tcp_keepalive(Duration::from_secs(15))
                .tcp_keepalive_interval(Duration::from_secs(5))
                .tcp_keepalive_retries(3)
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

/// What a caller can add to a model call beyond the connection and messages.
#[derive(Default)]
pub struct Extra {
    /// Offered after the connection's own tools, and not at all where it offers none.
    pub tools: Vec<Value>,
    /// Once set, ends the call before its next request or at the next line it receives.
    pub cancel: Cancel,
    /// Held until the stream is over, so whatever it carries lasts as long as the call.
    pub guard: Option<Box<dyn Any + Send>>,
}

pub fn start(connection: Connection, messages: Vec<Message>) -> async_channel::Receiver<Event> {
    start_with(connection, messages, Extra::default())
}

/// `start`, with tools, a stop signal and a guard of the caller's own.
pub fn start_with(
    connection: Connection,
    messages: Vec<Message>,
    extra: Extra,
) -> async_channel::Receiver<Event> {
    let (sender, receiver) = async_channel::bounded(64);
    std::thread::spawn(move || {
        let started = std::time::Instant::now();
        let result = stream(
            &connection,
            &messages,
            window_start(&messages),
            &extra,
            &sender,
        );
        // The call is over by now. Letting go before saying so means whoever
        // hears `Finished` never finds the guard still held.
        drop(extra.guard);
        crate::diag::event(
            "model_request",
            &[
                ("model", connection.model.as_str().into()),
                // The failure's own kind, known here since #65, not guessed from its words.
                (
                    "outcome",
                    match &result {
                        Ok(()) => "ok".into(),
                        Err(failure) => format!("{:?}", failure.kind).to_lowercase().into(),
                    },
                ),
                ("ms", crate::diag::millis(started)),
            ],
        );
        let _ = sender.send_blocking(Event::Finished(result));
    });
    receiver
}

fn stream(
    connection: &Connection,
    messages: &[Message],
    start: usize,
    extra: &Extra,
    sender: &async_channel::Sender<Event>,
) -> Result<(), Failure> {
    let endpoint = validate(connection).map_err(|e| Failure::new(FailureKind::Settings, e))?;
    // The facts about a model come from its gateway's list, which the window
    // reads when it opens and the terminal only for /model and /gateways, so
    // the first request to a host whose list is not known asks for it. This
    // request does not wait for it.
    crate::models::ask_for_list(&endpoint, &connection.key);
    // What the window no longer holds goes in as a summary rather than not at
    // all. It is made here, on the worker, so the wait is part of the reply's.
    let (dropped, window) = messages.split_at(start);
    let summary = crate::compact::summary(connection, dropped);
    let sent = body_with(
        connection,
        window,
        summary.as_deref(),
        checklist(messages).as_deref(),
        &extra.tools,
    );
    let mut request = client()
        .map_err(|e| Failure::new(FailureKind::Unknown, e))?
        .post(endpoint)
        .json(&sent);
    if !connection.key.trim().is_empty() {
        request = request.bearer_auth(connection.key.trim());
    }
    // A rate limit or a dropped connection part way through an agent loop
    // would otherwise lose the whole task, though it passes on its own.
    let mut tried = 0;
    let mut timeouts = 0;
    loop {
        tried += 1;
        // A stop that comes during the summary, a pause between tries (which
        // it cuts short) or ahead of a resend sends nothing.
        if extra.cancel.is_set() {
            return Err(Failure::new(FailureKind::Stopped, "Stopped."));
        }
        let send = request
            .try_clone()
            .ok_or_else(|| Failure::new(FailureKind::Unknown, "Could not prepare the request."))?
            .send();
        let mut stands = false;
        let mut slow = false;
        let failure = match send {
            Err(e) => {
                slow = ran_out(&e);
                Failure::new(
                    FailureKind::Network,
                    "Cannot reach the model. Check that your server is running and the endpoint is correct (5-minute wait for the reply to begin).",
                )
            }
            Ok(response) if response.status().is_success() => {
                match read(response, extra, sender, &mut stands, &mut slow) {
                    Ok(()) => return Ok(()),
                    Err(failure) => failure,
                }
            }
            Ok(response) => {
                let status = response.status();
                let headers = response.headers().clone();
                let error = response.text().unwrap_or_default();
                // Send it again without what the refusal named, which requests
                // to this model leave out from then on -- unless the turn was
                // stopped while this one was out. A timeout or a rate limit is
                // about no field, and passes by itself.
                if status.is_client_error()
                    && !matches!(status.as_u16(), 408 | 429)
                    && learn(connection, &sent, &error)
                    && !sender.is_closed()
                {
                    return stream(connection, messages, start, extra, sender);
                }
                let body = serde_json::from_str::<Value>(&error).unwrap_or_default();
                classify(Some(status.as_u16()), &headers, &body)
            }
        };
        if slow {
            timeouts += 1;
        }
        // Once part of the reply is on screen, another go would be shown
        // after it rather than in its place; read says when else a failure
        // stands. A reply that ran out of time may well run out again, each
        // try perhaps billed, so it gets one more go and no more.
        if stands || timeouts > 1 {
            return Err(failure);
        }
        let Some(retry) = crate::retry::after(&failure, tried, connection.unattended) else {
            return Err(failure);
        };
        sender
            .send_blocking(Event::Retrying(retry))
            .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        if !crate::retry::pause(retry.wait, || sender.is_closed() || extra.cancel.is_set()) {
            return Err(Failure::new(FailureKind::Stopped, "Chat closed."));
        }
    }
}

/// Whether a call ran out of time after it was sent; one that could not
/// connect in time was never sent, so it is not counted.
fn ran_out(e: &reqwest::Error) -> bool {
    e.is_timeout() && !e.is_connect()
}

/// Whether reading a reply broke off because the call ran out of time.
fn ran_out_reading(e: &std::io::Error) -> bool {
    e.get_ref()
        .and_then(|e| e.downcast_ref::<reqwest::Error>())
        .is_some_and(ran_out)
}

/// Pass a successful reply on as it arrives, setting `stands` once the model
/// has begun it or when it is no event stream, and `slow` if it runs out of
/// time.
fn read(
    response: reqwest::blocking::Response,
    extra: &Extra,
    sender: &async_channel::Sender<Event>,
    stands: &mut bool,
    slow: &mut bool,
) -> Result<(), Failure> {
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.contains("application/json"))
    {
        let value: Value = response
            .json()
            .map_err(|_| Failure::new(FailureKind::BadReply, "Invalid model response."))?;
        let began = std::time::Instant::now();
        let mut reply = Reply::default();
        reply.absorb(parse_chunk(&value)?, stands, sender)?;
        return reply.end(true, began, &extra.tools, sender);
    }
    // An event stream can end early for a reason that passes, as when a
    // gateway keeps it open with comments and then gives up on the model.
    // Any other reply, a web page say, would come back the same every time.
    *stands = !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.contains("text/event-stream"));
    let began = std::time::Instant::now();
    let mut reply = Reply::default();
    let mut done = false;
    for line in BufReader::new(response).lines() {
        let line = match line {
            Ok(line) => line,
            // Some servers close straight after saying why they finished,
            // and the reply is whole by then.
            Err(_) if reply.finish.is_some() => break,
            Err(e) => {
                *slow = ran_out_reading(&e);
                return Err(Failure::new(
                    FailureKind::Network,
                    "Connection interrupted before the response completed.",
                ));
            }
        };
        // A read waiting on the provider cannot be woken, so a stop is heard
        // when the next line arrives.
        if extra.cancel.is_set() {
            return Err(Failure::new(FailureKind::Stopped, "Stopped."));
        }
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            continue;
        };
        if data == "[DONE]" {
            done = true;
            break;
        }
        reply.absorb(parse_event(data)?, stands, sender)?;
    }
    reply.end(done, began, &extra.tools, sender)
}

/// A streamed reply as it arrives.
#[derive(Default)]
struct Reply {
    received: bool,
    completion_tokens: u64,
    prompt_tokens: u64,
    cached_tokens: u64,
    assembler: Assembler,
    finish: Option<FinishReason>,
}

impl Reply {
    /// Take in one chunk, passing its text on as it comes.
    fn absorb(
        &mut self,
        chunk: Chunk,
        stands: &mut bool,
        sender: &async_channel::Sender<Event>,
    ) -> Result<(), Failure> {
        if let Some(n) = chunk.completion_tokens {
            self.completion_tokens = n;
        }
        if let Some(n) = chunk.prompt_tokens {
            self.prompt_tokens = n;
        }
        if let Some(n) = chunk.cached_tokens {
            self.cached_tokens = n;
        }
        if chunk.finish.is_some() {
            self.finish = chunk.finish;
        }
        // A tool call is not shown until the stream ends, but one begun is an
        // answer under way like any text: another go is billed again, and a
        // server that never sends [DONE] would cut it off again.
        *stands |= !chunk.tool_calls.is_empty();
        self.assembler.add(&chunk);
        if let Some(text) = chunk.reasoning {
            *stands = true;
            sender
                .send_blocking(Event::Reasoning(text))
                .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        }
        if let Some(text) = chunk.content {
            self.received = true;
            *stands = true;
            sender
                .send_blocking(Event::Delta(text))
                .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        }
        Ok(())
    }

    /// Hand over the usage and the calls once the provider stops sending.
    ///
    /// A stream without `[DONE]` still counts when it said why it finished.
    fn end(
        self,
        done: bool,
        began: std::time::Instant,
        extra_tools: &[Value],
        sender: &async_channel::Sender<Event>,
    ) -> Result<(), Failure> {
        if self.completion_tokens > 0 {
            let secs = began.elapsed().as_secs_f32().max(0.001);
            let _ = sender.send_blocking(Event::Usage {
                completion: self.completion_tokens,
                per_second: self.completion_tokens as f32 / secs,
                prompt: self.prompt_tokens,
                cached: self.cached_tokens,
            });
        }
        if !done && self.finish.is_none() {
            return Err(Failure::new(
                FailureKind::Network,
                "The provider closed the stream early or does not support SSE chat completions.",
            ));
        }
        let mut calls = self.assembler.calls;
        calls.retain(|c| !c.name.is_empty());
        let cut_off = self.finish == Some(FinishReason::Length);
        // The limit falls inside the last call, so its arguments are half
        // written -- half a file, if it was a write. None of the calls is
        // handed over; the turn asks for them again, smaller.
        let in_call = cut_off && !calls.is_empty();
        if in_call {
            calls.clear();
        }
        name_unnamed(&mut calls);
        name_as_offered(&mut calls, extra_tools);
        let mut received = self.received;
        if !calls.is_empty() {
            received = true;
            sender
                .send_blocking(Event::ToolCalls(calls))
                .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        }
        if !received && !in_call {
            return Err(Failure::new(
                FailureKind::BadReply,
                match self.finish {
                    Some(FinishReason::Length) => {
                        "The model reached its output limit before writing anything. A lower                          effort leaves it more room."
                    }
                    Some(FinishReason::ContentFilter) => {
                        "The provider's content filter stopped the reply."
                    }
                    _ => "The model returned no text. Check model compatibility.",
                },
            ));
        }
        if cut_off {
            sender
                .send_blocking(Event::CutOff { in_call })
                .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        }
        Ok(())
    }
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
            unattended: false,
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
            unattended: false,
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
        let payload = body(
            &connection(),
            &messages[window_start(&messages)..],
            None,
            None,
        );
        let context = payload["messages"].as_array().unwrap();
        assert_eq!(context.len(), 31);
        assert_eq!(context[1]["role"], "user");
        assert_eq!(context.last().unwrap()["content"], "message 29");
    }
    #[test]
    fn huge_tool_result_keeps_the_prompt() {
        // A tool result larger than the whole budget used to push the window
        // past the end, leaving the model nothing but the system prompt, which
        // it answered by introducing itself. In CJK even the ends that are
        // sent are over the budget.
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
                content: "字".repeat(CONTEXT_TOKENS * 12),
                ..Default::default()
            },
        ];
        assert!(message_tokens(&messages[2]) > CONTEXT_TOKENS);
        let payload = body(
            &connection(),
            &messages[window_start(&messages)..],
            None,
            None,
        );
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
        let payload = body(&connection(), &messages, Some("They chose Postgres."), None);
        let system = payload["messages"][0]["content"].as_str().unwrap();
        assert!(
            system.ends_with("no longer fits:\nThey chose Postgres."),
            "{system}"
        );
        let without = body(&connection(), &messages, None, None);
        let plain = without["messages"][0]["content"].as_str().unwrap();
        assert!(!plain.contains("summarized"));
    }
    fn said(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }
    fn planned(items: &str) -> Message {
        Message {
            role: "assistant".into(),
            tool_calls: vec![ToolCall {
                id: "plan".into(),
                name: crate::tools::TODO.into(),
                arguments: format!(r#"{{"items":[{items}]}}"#),
            }],
            ..Default::default()
        }
    }
    #[test]
    fn the_open_checklist_rides_on_the_last_message_sent_and_is_never_stored() {
        let mut messages = vec![
            said("user", "build it"),
            planned(
                r#"{"text":"Read the code","status":"done"},{"text":"Write the parser","status":"doing"},{"text":"Test it","status":"pending"}"#,
            ),
            Message {
                tool_call_id: "plan".into(),
                ..said("tool", "Checklist updated: 1 of 3 done.")
            },
        ];
        // Long enough after the plan that the window steps past the call
        // that made it.
        for _ in 0..10 {
            messages.push(said("user", "go on"));
            messages.push(said("assistant", &"x".repeat(CONTEXT_TOKENS / 2)));
        }
        messages.push(said("user", "and now?"));
        assert!(window_start(&messages) > 1);

        // Sent as stream sends it: the window only, the plan from the whole.
        let window = &messages[window_start(&messages)..];
        let payload = body(&connection(), window, None, checklist(&messages).as_deref());
        let context = payload["messages"].as_array().unwrap();
        let last = context.last().unwrap()["content"].as_str().unwrap();
        assert!(last.starts_with("and now?\n\n(From Letronna"), "{last}");
        assert!(
            last.ends_with("left it.)\n[x] Read the code\n[>] Write the parser\n[ ] Test it"),
            "{last}"
        );
        let carrying = context
            .iter()
            .filter(|m| m["content"].as_str().is_some_and(|c| c.contains("[>]")))
            .count();
        assert_eq!(carrying, 1);
        assert_eq!(messages.last().unwrap().content, "and now?");
    }
    #[test]
    fn a_finished_checklist_is_not_sent() {
        let messages = [
            said("user", "build it"),
            planned(r#"{"text":"Read the code","status":"done"}"#),
            Message {
                tool_call_id: "plan".into(),
                ..said("tool", "Checklist updated: 1 of 1 done.")
            },
        ];
        let payload = body(&connection(), &messages, None, None);
        let context = payload["messages"].as_array().unwrap();
        assert_eq!(
            context.last().unwrap()["content"],
            "Checklist updated: 1 of 1 done."
        );
    }
    #[test]
    fn the_two_ways_a_route_says_it_cannot_do_tools_read_the_same() {
        let google = super::explain(
            "Expected the 'id' of a(n) 'assistant' 'tool_calls' array element to be populated.",
        );
        let gemini =
            super::explain("Function call is missing a thought_signature in functionCall parts.");
        assert!(google.contains("/model"));
        assert!(!google.contains("  "), "{google}");
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
        super::name_as_offered(&mut calls, &[]);
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
    fn the_model_is_told_the_date_the_computer_and_who_it_is() {
        let day = || chrono::Local::now().format("%A %Y-%m-%d").to_string();
        let system = |connection: &Connection| -> String {
            body(connection, &[], None, None)["messages"][0]["content"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        // Taken either side, so a run across midnight still finds one of them.
        let before = day();
        let here = system(&connection());
        let after = day();
        assert!(
            here.contains(&format!("Today is {before}."))
                || here.contains(&format!("Today is {after}.")),
            "{here}"
        );
        assert!(here.contains("This computer runs "), "{here}");
        assert!(here.contains("The model behind you is test-model."));

        // A shared chat keeps the date and the model but not the computer.
        let shared = system(&Connection {
            remote: Some(Remote {
                place: "#general in a Discord server".into(),
                shared: true,
                history: None,
            }),
            ..connection()
        });
        assert!(shared.contains("Today is "));
        assert!(shared.contains("The model behind you is test-model."));
        assert!(!shared.contains("This computer runs"), "{shared}");
    }

    #[test]
    fn plan_mode_names_exit_plan_only_where_it_is_offered() {
        let sent = |remote: Option<Remote>| -> (String, bool) {
            let payload = body(
                &Connection {
                    tools: true,
                    plan: true,
                    remote,
                    ..connection()
                },
                &[],
                None,
                None,
            );
            let offered = payload["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["function"]["name"] == crate::tools::EXIT_PLAN);
            let system = payload["messages"][0]["content"].as_str().unwrap();
            (system.to_owned(), offered)
        };
        let (window, offered) = sent(None);
        assert!(offered);
        assert!(
            window.contains("with exit_plan instead of writing it out"),
            "{window}"
        );
        // A chat app's DM sits in plan mode but has nobody to hand a plan to.
        let (dm, offered) = sent(Some(Remote {
            place: "a Telegram chat".into(),
            ..Default::default()
        }));
        assert!(!offered);
        assert!(dm.contains("Plan mode is on"), "{dm}");
        assert!(!dm.contains("exit_plan"), "{dm}");
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
        let sums = token_sums(&messages);
        assert!(sums[messages.len()] - sums[*starts.last().unwrap()] <= CONTEXT_TOKENS);
    }

    #[test]
    fn a_follow_up_during_a_long_turn_never_drops_the_turns_first_request() {
        let said = |role: &str, text: &str| Message {
            role: role.into(),
            content: text.into(),
            ..Default::default()
        };
        let folded = |text: &str| Message {
            folded: true,
            ..said("user", text)
        };
        // A tool round big enough that a few of them outgrow the budget.
        let round = |i: usize| {
            [
                Message {
                    role: "assistant".into(),
                    tool_calls: vec![ToolCall {
                        id: format!("call {i}"),
                        name: "read_file".into(),
                        arguments: "{}".into(),
                    }],
                    ..Default::default()
                },
                Message {
                    role: "tool".into(),
                    tool_call_id: format!("call {i}"),
                    content: "x".repeat(MAX_TOOL_CHARS),
                    ..Default::default()
                },
            ]
        };
        let mut messages = vec![
            said("user", "an earlier question"),
            said("assistant", "an earlier answer"),
            said("user", "the turn's first request"),
        ];
        messages.extend((0..8).flat_map(round));
        // Typed while the tools ran, and folded in after their results.
        messages.push(folded("a follow-up typed mid-turn"));
        messages.push(folded("another folded in with it"));
        messages.extend((8..10).flat_map(round));
        // Sent while a reply was still coming in, which it cut short.
        messages.push(said("assistant", "I will now rewrite the whole"));
        messages.push(folded("leave the tests alone"));
        messages.extend((10..12).flat_map(round));

        assert_eq!(window_start(&messages), 2);
        let payload = body(
            &connection(),
            &messages[window_start(&messages)..],
            None,
            None,
        );
        let context = payload["messages"].as_array().unwrap();
        assert_eq!(context[1]["content"], "the turn's first request");

        // Stopped, or failed, before the model answered its last results, the
        // turn is still over: the next prompt starts one of its own, and the
        // window moves on to it rather than to the follow-ups before it.
        messages.push(said("user", "the next request"));
        assert_eq!(window_start(&messages), messages.len() - 1);
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
        let payload = body(&connection(), &messages, None, None);
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
            body(&c, &messages, None, None)["messages"]
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
            // Longer than anyone watching is kept waiting, so it is not
            // tried again.
            write!(socket, "HTTP/1.1 429 Too Many Requests\r\nContent-Type: application/json\r\nRetry-After: 120\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
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
        assert_eq!(failure.retry_after, Some(Duration::from_secs(120)));
        assert!(
            failure
                .to_string()
                .starts_with("Rate limit reached for gpt-4o")
        );
        worker.join().unwrap();
    }

    #[test]
    fn ascii_counts_about_four_bytes_a_token() {
        assert_eq!(tokens(""), 0);
        assert_eq!(tokens("abcd"), 1);
        assert_eq!(tokens("hello"), 2);
        // The window's old budget of 24,000 characters is its new one.
        assert_eq!(tokens(&"x".repeat(24_000)), CONTEXT_TOKENS);
    }

    #[test]
    fn cjk_counts_every_character() {
        assert_eq!(tokens("你好世界"), 4);
        assert_eq!(tokens("こんにちは"), 5);
        assert_eq!(tokens("안녕하세요"), 5);
        assert_eq!(tokens("😀"), 1);
        // Two characters, then nine bytes of ASCII at four a token.
        assert_eq!(tokens("see 你好 here"), 2 + 3);
        // Two-byte letters count by their bytes, as ASCII does.
        assert_eq!(tokens("привет"), 3);
    }

    #[test]
    fn a_message_counts_as_it_is_sent() {
        let long = "x".repeat(40_000);
        let said = Message {
            role: "user".into(),
            content: long.clone(),
            ..Default::default()
        };
        let result = Message {
            role: "tool".into(),
            content: long,
            ..Default::default()
        };
        // What the user wrote goes out whole, a tool result as its ends.
        assert_eq!(message_tokens(&said), MESSAGE_TOKENS + 10_000);
        let sent = message_tokens(&result) - MESSAGE_TOKENS;
        assert!(sent.abs_diff(MAX_TOOL_CHARS / 4) < 16, "{sent}");
    }

    /// A turn that wrote a file bigger than the whole budget, then a new one.
    fn a_big_write() -> Vec<Message> {
        let arguments = format!(r#"{{"path":"big.txt","content":"{}"}}"#, "x".repeat(40_000));
        vec![
            Message {
                role: "user".into(),
                content: "write the big file".into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                tool_calls: vec![ToolCall {
                    id: "a".into(),
                    name: "write_file".into(),
                    arguments,
                }],
                ..Default::default()
            },
            Message {
                role: "tool".into(),
                tool_call_id: "a".into(),
                content: "Wrote big.txt".into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                content: "Done.".into(),
                ..Default::default()
            },
            Message {
                role: "user".into(),
                content: "thanks".into(),
                ..Default::default()
            },
        ]
    }

    #[test]
    fn tool_call_arguments_count_toward_the_window() {
        // A file written in one call goes out in full as the call's
        // arguments, however little the messages around it say.
        let messages = a_big_write();
        assert!(message_tokens(&messages[1]) > 10_000);
        // The turn that wrote it no longer fits, so the window moves on.
        assert_eq!(window_start(&messages), 4);
    }

    #[test]
    fn a_request_sends_its_window_and_a_summary_of_the_rest() {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            // The summary of what fell out, then the reply.
            let replies = [
                (
                    "application/json",
                    r#"{"choices":[{"message":{"content":"A big file was written."}}]}"#,
                ),
                (
                    "text/event-stream",
                    "data: {\"choices\":[{\"delta\":{\"content\":\"Welcome\"}}]}\n\ndata: [DONE]\n\n",
                ),
            ];
            let mut bodies = Vec::new();
            for (kind, reply) in replies {
                let (mut socket, _) = server.accept().unwrap();
                bodies.push(request_body(&mut socket));
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            bodies
        });
        let c = Connection {
            endpoint,
            ..connection()
        };
        let rx = start(c, a_big_write());
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "Welcome"));
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished(Ok(()))
        ));
        let bodies = worker.join().unwrap();
        // The turn that no longer fits is summed up, and only the summary of
        // it goes out with the window.
        assert!(bodies[0].contains("write the big file"));
        assert!(!bodies[1].contains("write the big file"));
        assert!(bodies[1].contains("A big file was written."));
        assert!(bodies[1].contains("thanks"));
    }

    #[test]
    fn a_picture_counts_only_where_it_is_sent() {
        let dir = std::env::temp_dir().join(format!("letronna-counted-{}", std::process::id()));
        let messages = screenshots(&dir);
        // Counted from the conversation alone, so the files need not be there.
        std::fs::remove_dir_all(&dir).unwrap();
        let sums = token_sums(&messages);
        let pictured: Vec<usize> = (0..messages.len())
            .filter(|&i| sums[i + 1] - sums[i] != message_tokens(&messages[i]))
            .collect();
        // The newer screenshot, after the last result of its batch; the
        // older one is never sent, so it costs only its text.
        let last = messages.len() - 1;
        assert_eq!(pictured, [last]);
        assert_eq!(
            sums[last + 1] - sums[last],
            message_tokens(&messages[last]) + PICTURE_TOKENS
        );
    }

    /// A server on this computer that answers each request with the next of
    /// `replies` (status, content type, body), then hands back the bodies it
    /// was sent.
    fn replay(
        replies: Vec<(&'static str, &'static str, &'static str)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for (status, kind, reply) in replies {
                let (mut socket, _) = server.accept().unwrap();
                bodies.push(request_body(&mut socket));
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            bodies
        });
        (endpoint, worker)
    }

    /// How a turn over `messages` ends, and the text it streamed.
    fn finish(connection: &Connection, messages: Vec<Message>) -> (Result<(), Failure>, String) {
        let rx = start(connection.clone(), messages);
        let mut text = String::new();
        loop {
            match rx.recv_blocking().unwrap() {
                Event::Delta(t) => text.push_str(&t),
                Event::Finished(result) => return (result, text),
                _ => {}
            }
        }
    }

    fn hello() -> Vec<Message> {
        vec![Message {
            role: "user".into(),
            content: "Hi".into(),
            ..Default::default()
        }]
    }

    #[test]
    fn reasoning_models_on_openai_get_max_completion_tokens() {
        let sent = |endpoint: &str, model: &str| {
            let c = Connection {
                endpoint: endpoint.into(),
                model: model.into(),
                ..connection()
            };
            body(&c, &[], None, None)
        };
        for model in ["o1", "o3-mini", "o4-mini", "gpt-5", "gpt-5-mini", "gpt-5.1"] {
            let body = sent("https://api.openai.com/v1", model);
            assert_eq!(body["max_completion_tokens"], COMPLETION_TOKENS, "{model}");
            assert!(body.get("max_tokens").is_none(), "{model}");
        }
        // Older models there, and the same names on other routes, keep the
        // field every other server knows.
        for (endpoint, model) in [
            ("https://api.openai.com/v1", "gpt-4o"),
            ("https://openrouter.ai/api/v1", "openai/o3"),
            ("http://localhost:11434/v1", "gpt-5"),
        ] {
            let body = sent(endpoint, model);
            assert_eq!(body["max_tokens"], 8192, "{model} at {endpoint}");
            assert!(body.get("max_completion_tokens").is_none(), "{model}");
        }
    }

    #[test]
    fn a_400_naming_stream_options_drops_it_and_retries_once() {
        // What a server that validates its input strictly says to the field.
        let refusal = r#"{"error":{"message":"[{'type': 'extra_forbidden', 'loc': ('body', 'stream_options'), 'msg': 'Extra inputs are not permitted', 'input': {'include_usage': True}}]","type":"BadRequestError","param":null,"code":400}}"#;
        let (endpoint, worker) = replay(vec![
            ("400 Bad Request", "application/json", refusal),
            (
                "200 OK",
                "text/event-stream",
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n",
            ),
            ("400 Bad Request", "application/json", refusal),
        ]);
        let c = Connection {
            endpoint,
            model: "strict-about-stream-options".into(),
            ..connection()
        };
        let (result, text) = finish(&c, hello());
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(text, "Hello");
        // The next turn leaves it out from the start, so the same refusal
        // again is not something another try would change: it ends the turn
        // after one request instead of reaching for a fourth.
        let (result, _) = finish(&c, hello());
        assert!(result.is_err_and(|e| e.to_string().contains("extra_forbidden")));
        let bodies = worker.join().unwrap();
        assert!(bodies[0].contains("stream_options"));
        assert!(!bodies[1].contains("stream_options"));
        assert!(!bodies[2].contains("stream_options"));
    }

    #[test]
    fn each_quirk_leaves_out_its_own_field_and_nothing_else() {
        let mut body = json!({
            "messages": [{"role": "user", "content": [
                {"type": "text", "text": "hi", "cache_control": {"type": "ephemeral"}}
            ]}],
            "max_tokens": 8192,
            "stream_options": {"include_usage": true},
            "reasoning_effort": "high",
            "tools": [{"type": "function"}, {"type": "web_search"}]
        });
        for (quirk, _) in QUIRKS {
            assert!(apply_quirk(&mut body, quirk));
            // Once is enough: what is gone cannot be the reason for a retry.
            assert!(!apply_quirk(&mut body, quirk));
        }
        assert_eq!(
            body,
            json!({
                "messages": [{"role": "user", "content": [{"type": "text", "text": "hi"}]}],
                "max_completion_tokens": COMPLETION_TOKENS,
                "tools": [{"type": "function"}]
            })
        );
    }

    #[test]
    fn the_image_latch_needs_the_error_to_name_images() {
        let dir = std::env::temp_dir().join(format!("letronna-latch-{}", std::process::id()));
        let messages = screenshots(&dir);
        let (endpoint, worker) = replay(vec![(
            "400 Bad Request",
            "application/json",
            r#"{"error":{"message":"This model's maximum context length is 65536 tokens. However, you requested 70412 tokens.","type":"invalid_request_error"}}"#,
        )]);
        let c = Connection {
            endpoint,
            model: "sees-but-is-full".into(),
            ..connection()
        };
        // A refusal for anything else is shown as it is, not answered by
        // sending the request again without the picture.
        let (result, _) = finish(&c, messages);
        assert!(result.is_err_and(|e| e.to_string().contains("maximum context length")));
        let bodies = worker.join().unwrap();
        assert!(bodies[0].contains("image_url"));
        assert!(sees_images("sees-but-is-full"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn learning_goes_by_what_the_refusal_says_about_the_field() {
        let c = Connection {
            model: "says-what-it-refuses".into(),
            effort: "high".into(),
            ..connection()
        };
        let sent = body(&c, &hello(), None, None);
        // A server out of room names both caps, and the same cap under the
        // other name would not give it more.
        assert!(!learn(
            &c,
            &sent,
            r#"{"error":{"message":"'max_tokens' or 'max_completion_tokens' is too large: 8192. This model's maximum context length is 32768 tokens and your request has 30000 input tokens (8192 > 32768 - 30000)."}}"#,
        ));
        assert!(learn(
            &c,
            &sent,
            r#"{"error":{"message":"Unsupported parameter: 'max_tokens' is not supported with this model. Use 'max_completion_tokens' instead.","param":"max_tokens"}}"#,
        ));
        // Some servers spell the field in camel case.
        assert!(learn(
            &c,
            &sent,
            r#"{"error":"Argument not supported on this model: reasoningEffort"}"#,
        ));
        let next = body(&c, &hello(), None, None);
        assert_eq!(next["max_completion_tokens"], COMPLETION_TOKENS);
        assert!(next.get("max_tokens").is_none());
        assert!(next.get("reasoning_effort").is_none());
    }

    #[test]
    fn a_refusal_is_retried_even_when_another_request_learned_it_first() {
        let c = Connection {
            model: "refuses-stream-options".into(),
            ..connection()
        };
        let sent = body(&c, &hello(), None, None);
        let refusal =
            r#"{"error":{"message":"Unrecognized request argument supplied: stream_options"}}"#;
        // Two requests that went out together, each refused in turn.
        assert!(learn(&c, &sent, refusal));
        assert!(learn(&c, &sent, refusal));
        // Sent without it, the same refusal is no reason to go again.
        assert!(!learn(&c, &body(&c, &hello(), None, None), refusal));
    }

    #[test]
    fn a_notice_goes_to_the_model_as_the_users_turn() {
        let messages = [
            Message {
                role: "user".into(),
                content: "run the tests in the background".into(),
                ..Default::default()
            },
            Message {
                role: "assistant".into(),
                content: "Started.".into(),
                ..Default::default()
            },
            Message {
                role: "user".into(),
                content: "job #7 exited with code 1".into(),
                notice: true,
                ..Default::default()
            },
        ];
        let payload = body(&connection(), &messages, None, None);
        let sent = payload["messages"].as_array().unwrap().last().unwrap();
        assert_eq!(sent["role"], "user");
        assert_eq!(sent["content"], "job #7 exited with code 1");
        // The flag is Letronna's own; a provider only ever sees a user turn.
        assert!(sent.get("notice").is_none(), "{sent}");
    }

    #[test]
    fn a_session_written_before_notices_still_loads() {
        let old: Message = serde_json::from_str(r#"{"role":"user","content":"hi"}"#).unwrap();
        assert!(!old.notice);
        // Only a notice carries the field, so everything else is written as before.
        let written = serde_json::to_string(&old).unwrap();
        assert!(!written.contains("notice"), "{written}");
        let notice = Message {
            notice: true,
            ..old
        };
        let line = serde_json::to_string(&notice).unwrap();
        assert!(serde_json::from_str::<Message>(&line).unwrap().notice);
    }

    #[test]
    fn a_run_of_notices_still_moves_the_window() {
        // A session driven by finished jobs or loop ticks hears from the user
        // once. Were only the user's own words a turn, the window could never
        // pass them and every request would carry the whole session.
        let said = |role: &str, notice: bool, i: usize| Message {
            role: role.into(),
            content: format!("{role} {i} {}", "x".repeat(600)),
            notice,
            ..Default::default()
        };
        let mut messages = vec![said("user", false, 0), said("assistant", false, 0)];
        for i in 1..60 {
            messages.push(said("user", true, i));
            messages.push(said("assistant", false, i));
        }
        let start = window_start(&messages);
        assert!(start > 0, "the window never left the user's message");
        assert!(messages[start].notice, "start {start} is not a notice");
        let sums = token_sums(&messages);
        assert!(sums[messages.len()] - sums[start] <= CONTEXT_TOKENS);
    }

    #[test]
    fn the_client_timeout_counts_silence_not_the_whole_reply() {
        // client() leans on this: in reqwest's blocking client the builder's
        // timeout bounds each read, where a per-request one is a deadline for
        // the whole body.
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/", server.local_addr().unwrap());
        let (hang_up, told) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            request_body(&mut socket);
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            // Twice the timeout in all, but never quiet for long.
            for i in 0..40 {
                writeln!(socket, "data: {i}").unwrap();
                std::thread::sleep(Duration::from_millis(50));
            }
            // Then quiet, which should end the read long before this gives up.
            let _ = told.recv_timeout(Duration::from_secs(60));
        });
        let client = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(1))
            .build()
            .unwrap();
        let response = client.post(address).body("{}").send().unwrap();
        let mut lines = BufReader::new(response).lines();
        for i in 0..40 {
            assert_eq!(lines.next().unwrap().unwrap(), format!("data: {i}"));
        }
        let Some(Err(stall)) = lines.next() else {
            panic!("a reply that went quiet was never cut");
        };
        let timed_out = stall
            .get_ref()
            .and_then(|e| e.downcast_ref::<reqwest::Error>())
            .is_some_and(reqwest::Error::is_timeout);
        assert!(timed_out, "{stall}");
        hang_up.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn a_call_that_sets_its_own_deadline_keeps_it() {
        // A model list's deadline must beat the shared client's far longer
        // idle timeout. Were it lost, the server hanging up at 60 s would end
        // the wait with some other error.
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = format!("http://{}/", server.local_addr().unwrap());
        let (hang_up, told) = std::sync::mpsc::channel::<()>();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            request_body(&mut socket);
            let _ = told.recv_timeout(Duration::from_secs(60));
        });
        let sent = super::client()
            .unwrap()
            .post(address)
            .body("{}")
            .timeout(Duration::from_secs(1))
            .send();
        assert!(matches!(&sent, Err(e) if e.is_timeout()), "{sent:?}");
        hang_up.send(()).unwrap();
        worker.join().unwrap();
    }

    #[test]
    #[ignore = "takes over two minutes: a local server trickles one reply past the old 120-second cap; run with --ignored arrives_whole"]
    fn a_reply_longer_than_two_minutes_arrives_whole() {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            request_body(&mut socket);
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            // A word every five seconds, 130 seconds in all.
            for _ in 0..26 {
                write!(
                    socket,
                    "data: {{\"choices\":[{{\"delta\":{{\"content\":\"word \"}}}}]}}\n\n"
                )
                .unwrap();
                std::thread::sleep(Duration::from_secs(5));
            }
            write!(socket, "data: [DONE]\n\n").unwrap();
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
        let mut reply = String::new();
        let result = loop {
            match rx.recv_blocking().unwrap() {
                Event::Delta(text) => reply.push_str(&text),
                Event::Finished(result) => break result,
                _ => {}
            }
        };
        assert!(result.is_ok(), "{result:?}");
        assert_eq!(reply, "word ".repeat(26));
        worker.join().unwrap();
    }

    #[test]
    fn an_extra_tool_the_gateway_renamed_is_called_by_its_own_name() {
        let mut calls = vec![ToolCall {
            name: "StructuredOutput".into(),
            ..Default::default()
        }];
        let answer = json!({"type": "function", "function": {"name": "structured_output"}});
        super::name_as_offered(&mut calls, &[answer]);
        assert_eq!(calls[0].name, "structured_output");
    }

    /// One question from the user.
    fn hi() -> Vec<Message> {
        vec![Message {
            role: "user".into(),
            content: "Hi".into(),
            ..Default::default()
        }]
    }

    /// `text` as one line of a streamed reply.
    fn line_of(text: &str) -> String {
        format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"{text}\"}}}}]}}\n\n")
    }

    /// Reads `rx` to its end, and says how it ended.
    fn outcome(rx: async_channel::Receiver<Event>) -> Result<(), String> {
        loop {
            if let Event::Finished(result) = rx.recv_blocking().unwrap() {
                return result.map_err(|e| e.to_string());
            }
        }
    }

    /// A provider that answers `calls` requests with "Hello", and hands back
    /// what each one was sent.
    fn provider(calls: usize) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let reply = line_of("Hello") + "data: [DONE]\n\n";
            (0..calls)
                .map(|_| {
                    let (mut socket, _) = server.accept().unwrap();
                    let sent = request_body(&mut socket);
                    write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
                    sent
                })
                .collect()
        });
        (endpoint, worker)
    }

    /// A provider that streams "Hello", sends `then` when told to, and keeps
    /// the connection open until the sender it hands back is dropped.
    fn held(then: String) -> (String, std::sync::mpsc::Sender<()>) {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let (go, told) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            request_body(&mut socket);
            // No length, so the reply runs until the connection closes, as a
            // provider's stream does.
            write!(
                socket,
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{}",
                line_of("Hello")
            )
            .unwrap();
            if told.recv().is_ok() {
                // A stopped call may have hung up already.
                let _ = socket.write_all(then.as_bytes());
                let _ = told.recv();
            }
        });
        (endpoint, go)
    }

    #[test]
    fn a_call_without_extras_sends_the_same_bytes() {
        // Store's tests point the data folder elsewhere for a moment, which
        // can change the prompt between two calls; a real difference shows on
        // every try.
        let same = (0..3).any(|_| {
            let (endpoint, worker) = provider(2);
            let c = Connection {
                endpoint,
                tools: true,
                ..connection()
            };
            assert_eq!(outcome(start(c.clone(), hi())), Ok(()));
            assert_eq!(
                outcome(start_with(c.clone(), hi(), Extra::default())),
                Ok(())
            );
            let before = serde_json::to_string(&body(&c, &hi(), None, None)).unwrap();
            worker.join().unwrap() == [before.clone(), before]
        });
        assert!(same, "a call without extras sent something else");
    }

    #[test]
    fn a_stopped_stream_ends_at_the_next_line() {
        let (endpoint, go) = held(line_of(" and more"));
        let extra = Extra::default();
        let stop = extra.cancel.clone();
        let c = Connection {
            endpoint,
            ..connection()
        };
        let rx = start_with(c, hi(), extra);
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "Hello"));
        stop.set();
        go.send(()).unwrap();
        // The provider never finishes, and what it sends after the stop is
        // not passed on.
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished(Err(e)) if e.to_string() == "Stopped."
        ));
    }

    #[test]
    fn a_call_stopped_before_it_is_sent_sends_nothing() {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let extra = Extra::default();
        extra.cancel.set();
        let c = Connection {
            endpoint: format!("http://{}/v1", server.local_addr().unwrap()),
            ..connection()
        };
        assert_eq!(outcome(start_with(c, hi(), extra)), Err("Stopped.".into()));
        server.set_nonblocking(true).unwrap();
        assert!(server.accept().is_err(), "a request went out");
    }

    #[test]
    fn the_guard_is_dropped_when_the_stream_ends() {
        use std::sync::Arc;
        let (endpoint, go) = held("data: [DONE]\n\n".into());
        let permit = Arc::new(());
        let extra = Extra {
            guard: Some(Box::new(permit.clone())),
            ..Default::default()
        };
        let c = Connection {
            endpoint,
            ..connection()
        };
        let rx = start_with(c, hi(), extra);
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "Hello"));
        assert_eq!(Arc::strong_count(&permit), 2, "let go mid-stream");
        go.send(()).unwrap();
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished(Ok(()))
        ));
        assert_eq!(Arc::strong_count(&permit), 1, "held after the end");
    }

    #[test]
    fn extra_tools_go_after_the_registered_ones() {
        let (endpoint, worker) = provider(1);
        let c = Connection {
            endpoint,
            tools: true,
            search: true,
            ..connection()
        };
        let answer = json!({"type": "function", "function": {
            "name": "structured_output", "parameters": {"type": "object"}
        }});
        let extra = Extra {
            tools: vec![answer.clone()],
            ..Default::default()
        };
        assert_eq!(outcome(start_with(c, hi(), extra)), Ok(()));
        let sent: Value = serde_json::from_str(&worker.join().unwrap()[0]).unwrap();
        let tools = sent["tools"].as_array().unwrap();
        assert_eq!(tools[0]["function"]["name"], "read_file");
        // After the gateway's own search as well, which follows the registry.
        assert_eq!(tools[tools.len() - 2], json!({"type": "web_search"}));
        assert_eq!(tools.last(), Some(&answer));
    }

    /// A server that answers each request it gets with the next of `replies`,
    /// given as status, content type and body, and hands back what it was sent.
    fn serve(
        replies: Vec<(&'static str, &'static str, &'static str)>,
    ) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        // Polled, so a request that never comes fails the test instead of
        // hanging it.
        server.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            let mut bodies = Vec::new();
            for (status, kind, reply) in replies {
                let deadline = std::time::Instant::now() + Duration::from_secs(20);
                let mut socket = loop {
                    match server.accept() {
                        Ok((socket, _)) => break socket,
                        Err(_) if std::time::Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => panic!("request {} never came: {e}", bodies.len() + 1),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                bodies.push(request_body(&mut socket));
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            bodies
        });
        (endpoint, worker)
    }

    /// Ask `endpoint` for a reply to "Hi".
    fn ask(endpoint: String) -> async_channel::Receiver<Event> {
        let c = Connection {
            endpoint,
            ..connection()
        };
        start(
            c,
            vec![Message {
                role: "user".into(),
                content: "Hi".into(),
                ..Default::default()
            }],
        )
    }

    #[test]
    fn a_call_that_ran_out_of_time_is_told_from_one_cut_off() {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            // No answer at all; the start of a stream and then nothing; and
            // part of a body before the server hangs up.
            let answers = [
                ("", true),
                (
                    "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\r\ndata: {}\n\n",
                    true,
                ),
                ("HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\ndata:", false),
            ];
            for (answer, hold) in answers {
                let (mut socket, _) = server.accept().unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                request_body(&mut socket);
                socket.write_all(answer.as_bytes()).unwrap();
                // Held until the client gives up and hangs up, or the read
                // timeout ends it should the client not.
                while hold && socket.read(&mut [0; 64]).is_ok_and(|n| n > 0) {}
            }
        });
        let send = || {
            client()
                .unwrap()
                .post(&url)
                .body("{}")
                // Long enough that only a server holding back runs it out,
                // however busy the machine running the tests.
                .timeout(Duration::from_secs(2))
                .send()
        };
        let broken = |response: reqwest::blocking::Response| {
            BufReader::new(response)
                .lines()
                .find_map(Result::err)
                .unwrap()
        };
        let Err(silence) = send() else {
            panic!("a server that never answered was heard");
        };
        assert!(ran_out(&silence));
        assert!(ran_out_reading(&broken(send().unwrap())));
        assert!(!ran_out_reading(&broken(send().unwrap())));
        worker.join().unwrap();
    }

    #[test]
    fn a_rate_limit_is_waited_out_and_the_call_made_again() {
        let (endpoint, worker) = serve(vec![
            (
                "429 Too Many Requests",
                "application/json",
                r#"{"error":{"message":"Rate limit reached.","code":"rate_limit_exceeded"}}"#,
            ),
            (
                "200 OK",
                "text/event-stream",
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n",
            ),
        ]);
        let rx = ask(endpoint);
        let Event::Retrying(retry) = rx.recv_blocking().unwrap() else {
            panic!("a rate limit was not tried again");
        };
        assert_eq!(retry.kind, FailureKind::RateLimit);
        assert_eq!((retry.attempt, retry.of), (2, 6));
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "Hello"));
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished(Ok(()))
        ));
        let bodies = worker.join().unwrap();
        assert_eq!(bodies[0], bodies[1], "the second try asked something else");
    }

    #[test]
    fn a_rate_limit_on_a_request_with_a_screenshot_is_waited_out_with_it() {
        let dir = std::env::temp_dir().join(format!("letronna-limited-{}", std::process::id()));
        let messages = screenshots(&dir);
        let (endpoint, worker) = serve(vec![
            (
                "429 Too Many Requests",
                "application/json",
                r#"{"error":{"message":"Rate limit reached.","code":"rate_limit_exceeded"}}"#,
            ),
            (
                "200 OK",
                "text/event-stream",
                "data: {\"choices\":[{\"delta\":{\"content\":\"A form\"}}]}\n\ndata: [DONE]\n\n",
            ),
        ]);
        let c = Connection {
            endpoint,
            model: "sees-through-a-rate-limit".into(),
            ..connection()
        };
        let rx = start(c, messages);
        let Event::Retrying(retry) = rx.recv_blocking().unwrap() else {
            panic!("a rate limit was taken for a picture turned down");
        };
        assert_eq!(retry.kind, FailureKind::RateLimit);
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "A form"));
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished(Ok(()))
        ));
        let bodies = worker.join().unwrap();
        assert!(bodies[0].contains("image_url"));
        assert!(bodies[1].contains("image_url"));
        assert!(sees_images("sees-through-a-rate-limit"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_reply_that_is_no_event_stream_is_not_tried_again() {
        let (endpoint, worker) = serve(vec![(
            "200 OK",
            "text/html; charset=utf-8",
            "<!doctype html><title>Welcome</title><p>Sign in to continue.</p>",
        )]);
        let rx = ask(endpoint);
        let Event::Finished(Err(failure)) = rx.recv_blocking().unwrap() else {
            panic!("a web page was taken for a stream that dropped");
        };
        assert!(
            failure.to_string().contains("does not support SSE"),
            "{failure}"
        );
        worker.join().unwrap();
    }

    #[test]
    fn a_stream_cut_off_after_it_began_is_not_tried_again() {
        let (endpoint, worker) = serve(vec![(
            "200 OK",
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        )]);
        let rx = ask(endpoint);
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "Hel"));
        let Event::Finished(Err(failure)) = rx.recv_blocking().unwrap() else {
            panic!("a reply already on screen was tried again");
        };
        assert_eq!(failure.kind, FailureKind::Network);
        worker.join().unwrap();
    }

    #[test]
    fn a_stream_cut_off_in_a_tool_call_is_not_tried_again() {
        let (endpoint, worker) = serve(vec![(
            "200 OK",
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"read_file\",\"arguments\":\"\"}}]}}]}\n\n",
        )]);
        let rx = ask(endpoint);
        let Event::Finished(Err(failure)) = rx.recv_blocking().unwrap() else {
            panic!("a tool call already begun was tried again");
        };
        assert_eq!(failure.kind, FailureKind::Network);
        worker.join().unwrap();
    }

    #[test]
    fn a_stream_that_ends_before_saying_anything_is_tried_again() {
        let (endpoint, worker) = serve(vec![
            ("200 OK", "text/event-stream", ": still here\n\n"),
            (
                "200 OK",
                "text/event-stream",
                "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"}}]}\n\ndata: [DONE]\n\n",
            ),
        ]);
        let rx = ask(endpoint);
        let Event::Retrying(retry) = rx.recv_blocking().unwrap() else {
            panic!("a stream that said nothing was not tried again");
        };
        assert_eq!(retry.kind, FailureKind::Network);
        assert!(matches!(rx.recv_blocking().unwrap(), Event::Delta(s) if s == "Hello"));
        assert!(matches!(
            rx.recv_blocking().unwrap(),
            Event::Finished(Ok(()))
        ));
        worker.join().unwrap();
    }

    #[test]
    fn every_finish_reason_is_read() {
        let finish = |reason: &str| {
            parse_event(&format!(
                r#"{{"choices":[{{"delta":{{}},"finish_reason":{reason}}}]}}"#
            ))
            .unwrap()
            .finish
        };
        assert_eq!(finish(r#""stop""#), Some(FinishReason::Stop));
        assert_eq!(finish(r#""length""#), Some(FinishReason::Length));
        assert_eq!(finish(r#""tool_calls""#), Some(FinishReason::ToolCalls));
        assert_eq!(
            finish(r#""content_filter""#),
            Some(FinishReason::ContentFilter)
        );
        // The same stops under the names other providers give them.
        assert_eq!(finish(r#""end_turn""#), Some(FinishReason::Stop));
        assert_eq!(finish(r#""max_tokens""#), Some(FinishReason::Length));
        assert_eq!(finish(r#""MAX_TOKENS""#), Some(FinishReason::Length));
        assert_eq!(finish(r#""tool_use""#), Some(FinishReason::ToolCalls));
        assert_eq!(finish(r#""something_new""#), Some(FinishReason::Other));
        // Every chunk but the last carries a null.
        assert_eq!(finish("null"), None);
    }

    /// Answer one request with `body` as `kind`, and collect the events the
    /// reply came to.
    fn reply_of(kind: &'static str, body: &'static str) -> Vec<Event> {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            request_body(&mut socket);
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        });
        let asked = vec![Message {
            role: "user".into(),
            content: "Hi".into(),
            ..Default::default()
        }];
        let rx = start(
            Connection {
                endpoint,
                ..connection()
            },
            asked,
        );
        let mut events = Vec::new();
        while let Ok(event) = rx.recv_blocking() {
            let last = matches!(event, Event::Finished(_));
            events.push(event);
            if last {
                break;
            }
        }
        worker.join().unwrap();
        events
    }

    #[test]
    fn a_stream_that_ends_after_a_finish_is_complete_without_done() {
        let events = reply_of(
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":\"stop\"}]}\n\n",
        );
        assert!(matches!(&events[0], Event::Delta(s) if s == "Hello"));
        assert!(matches!(events.last(), Some(Event::Finished(Ok(())))));
        // Neither a finish nor `[DONE]`: the reply may have been cut short.
        let events = reply_of(
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n",
        );
        assert!(matches!(events.last(), Some(Event::Finished(Err(_)))));
    }

    #[test]
    fn a_reply_cut_off_at_the_limit_keeps_its_text_and_says_so() {
        let events = reply_of(
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"The first half\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n\
             data: [DONE]\n\n",
        );
        assert!(matches!(&events[0], Event::Delta(s) if s == "The first half"));
        assert!(matches!(events[1], Event::CutOff { in_call: false }));
        assert!(matches!(events[2], Event::Finished(Ok(()))));
    }

    #[test]
    fn a_call_cut_off_at_the_limit_is_not_handed_over() {
        let events = reply_of(
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"write_file\",\"arguments\":\"{\\\"path\\\":\\\"a.rs\\\",\\\"content\\\":\\\"fn\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n\
             data: [DONE]\n\n",
        );
        assert!(!events.iter().any(|e| matches!(e, Event::ToolCalls(_))));
        assert!(matches!(events[0], Event::CutOff { in_call: true }));
        // Not an error: the turn goes on and asks for the call again.
        assert!(matches!(events[1], Event::Finished(Ok(()))));
    }

    #[test]
    fn a_reply_that_does_not_stream_brings_its_calls_and_usage() {
        let events = reply_of(
            "application/json",
            r#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"c1","type":"function","function":{"name":"read_file","arguments":"{\"path\":\"a.rs\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":120,"completion_tokens":9,"prompt_tokens_details":{"cached_tokens":64}}}"#,
        );
        assert!(matches!(
            events[0],
            Event::Usage {
                completion: 9,
                prompt: 120,
                cached: 64,
                ..
            }
        ));
        let Event::ToolCalls(calls) = &events[1] else {
            panic!("no calls");
        };
        assert_eq!(calls[0].id, "c1");
        assert_eq!(calls[0].name, "read_file");
        assert_eq!(calls[0].arguments, r#"{"path":"a.rs"}"#);
        assert!(matches!(events[2], Event::Finished(Ok(()))));
    }
}
