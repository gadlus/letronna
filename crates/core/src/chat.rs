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
    /// Set on a user message that went into a turn already running, such as a
    /// follow-up typed while its tools ran, rather than starting a turn.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub folded: bool,
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

/// Budget for the window of a model whose own is not known, and the least
/// any model gets, in estimated tokens: the 24,000 characters every model
/// had, at four characters a token of English. Ollama serves a far smaller
/// window than a model was trained for and its list does not say how much,
/// so this is what a local model keeps.
const CONTEXT_TOKENS: usize = 6_000;
/// A single tool result can be far larger than anything the model needs to
/// read. The chat keeps the whole thing; the request gets the ends. This is
/// the small window's share and the least any gets; a bigger window sends
/// half its room in characters.
const MAX_TOOL_CHARS: usize = 8_000;
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

/// About what `m` costs in a request that sends `tool_chars` of a tool
/// result: its text as sent, the calls it makes with their arguments, and
/// its role.
fn message_tokens(m: &Message, tool_chars: usize) -> usize {
    // A long tool result goes out trimmed to its ends. A string never has
    // fewer bytes than characters, so a short one is never trimmed.
    let content = if m.role == "tool" && m.content.len() > tool_chars {
        tokens(&trim(&m.content, tool_chars))
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
fn token_sums(messages: &[Message], tool_chars: usize) -> Vec<usize> {
    let picture = screenshot(messages).map(|(after, _)| after);
    let mut sums = Vec::with_capacity(messages.len() + 1);
    let mut total = 0;
    sums.push(total);
    for (i, m) in messages.iter().enumerate() {
        total += message_tokens(m, tool_chars);
        if picture == Some(i) {
            total += PICTURE_TOKENS;
        }
        sums.push(total);
    }
    sums
}

/// Messages in the small window, whatever the token budget says. Guards
/// against a long run of tiny messages; a bigger window holds as many more.
const MAX_MESSAGES: usize = 60;
/// The most a window holds by default, in estimated tokens, however much the
/// model could take. Until requests carry cache markers every round of a
/// turn is billed for the whole window again, so a bigger one costs more on
/// every round rather than once; the `context_cap` setting changes it.
const CAP_TOKENS: usize = 64_000;
/// The reply asked for when the model's output limit is not known: what
/// every request asked for before any limit was, which the smallest limits
/// in use, such as deepseek-chat's, still accept.
const REPLY_TOKENS: usize = 8_192;
/// The longest reply asked for, whatever the model could write: room kept
/// for the reply is room the window cannot use, and some gateways hold
/// credit against all of it before the reply starts. 32,000 is a long file
/// written in one call.
const MAX_REPLY_TOKENS: usize = 32_000;

/// How much of a conversation one request carries, in estimated tokens
/// unless said otherwise.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Budget {
    /// What the window's messages may cost together.
    tokens: usize,
    /// How far apart the places are where the window may start.
    step: usize,
    /// The most messages the window holds, whatever they cost.
    messages: usize,
    /// The most characters of one tool result that are sent.
    tool_chars: usize,
    /// The longest reply asked for, as `max_tokens`.
    reply: usize,
}

impl Budget {
    /// The window every model had before its own could be known.
    const SMALL: Self = Self {
        tokens: CONTEXT_TOKENS,
        step: CONTEXT_TOKENS / 2,
        messages: MAX_MESSAGES,
        tool_chars: MAX_TOOL_CHARS,
        reply: REPLY_TOKENS,
    };
}

/// The budget for the next request over `connection`: its model's window as
/// far as it is known, the user's cap of `cap` tokens (0 for the default),
/// and what the system prompt and the tools take.
fn budget(connection: &Connection, cap: u64) -> Budget {
    let facts = crate::models::facts(&connection.endpoint, connection.model.trim());
    let cap = match cap {
        0 => CAP_TOKENS,
        cap => cap as usize,
    };
    // A request without messages is the part every request carries.
    let fixed = tokens(&body(connection, &[], None, &Budget::SMALL).to_string());
    sized(facts.context, facts.max_output, cap, fixed)
}

/// The budget for a model that reads `context` tokens and writes `output`,
/// where they are known, under a cap of `cap` tokens, when `fixed` tokens of
/// every request go to the system prompt and the tools.
///
/// Only what the messages may cost depends on `fixed`. The step, the message
/// guard and the share of a tool result come from the window and the cap
/// alone, so the places the window can start stay put when memory is
/// written or a tool comes or goes: the window may move on a step sooner,
/// but never to a new place, which would miss the prompt cache and be
/// summarized again. The one exception, a prompt that takes more than half
/// the room, halves the step, which adds places but moves none.
fn sized(context: Option<u64>, output: Option<u64>, cap: usize, fixed: usize) -> Budget {
    let Some(context) = context else {
        // Without the window a longer reply than every model was asked for
        // could be more than it holds, so a known limit only lowers it.
        return Budget {
            reply: output.map_or(REPLY_TOKENS, |n| (n as usize).min(REPLY_TOKENS)),
            ..Budget::SMALL
        };
    };
    // A model may list an output limit as big as its window, while the prompt
    // and the reply asked for must fit in it together: the reply gets a
    // quarter of it at most.
    let reply = output
        .map_or(REPLY_TOKENS, |n| (n as usize).min(MAX_REPLY_TOKENS))
        .min(context as usize / 4);
    // A quarter of what the reply leaves stays free: the estimate is no
    // tokenizer, the summary rides on top, and the turn in progress, which
    // is never trimmed, can run past the window by a few tool results.
    let room = ((context as usize).saturating_sub(reply) / 4 * 3)
        .min(cap)
        .max(CONTEXT_TOKENS);
    let tokens = room.saturating_sub(fixed).max(CONTEXT_TOKENS);
    // A step longer than what the messages may cost would leave the window
    // nowhere to start but the current turn, on every request, once the
    // history past a place outgrew that before reaching the next one.
    let mut step = room / 2;
    while step > tokens {
        step /= 2;
    }
    Budget {
        tokens,
        step,
        messages: MAX_MESSAGES * room / CONTEXT_TOKENS,
        // Half the room in characters, about an eighth of it in tokens: the
        // turn in progress, which is never trimmed, still holds several
        // results, and from a room of 48,000 up the most a tool returns,
        // 24,000 characters, goes out whole.
        tool_chars: (room / 2).max(MAX_TOOL_CHARS),
        reply,
    }
}

/// Where the context window starts.
///
/// Providers cache on an exact prefix, so what matters as much as the size of
/// the window is that it stops moving. Trimming the oldest turn each time the
/// budget is exceeded would shift the prefix on every single request and miss
/// the cache every time — exactly in the long agent sessions that cost the
/// most. Instead the window can only start at the first turn past each whole
/// step of tokens from the beginning of the session, places that appending
/// cannot move. When it no longer fits it jumps to the next one that does,
/// about half the window on, and then stays put for many turns, so requests
/// in between are byte-identical from the front.
///
/// The latest user turn always goes in whatever its size: dropping it (a single
/// huge tool result can exceed the budget by itself) would leave the model with
/// no prompt at all, and it answers by introducing itself. A message folded into
/// a running turn is part of it, so a follow-up never pushes the turn's own
/// request out into the summary.
///
/// This is deliberately stateless: the same message list always yields the same
/// start, so nothing needs saving and two requests over the same history agree.
fn window_start(messages: &[Message], budget: &Budget) -> usize {
    let last_user = messages.iter().rposition(opens_turn).unwrap_or(0);
    let sums = token_sums(messages, budget.tool_chars);
    let total = sums[messages.len()];
    // Toward the steps a message counts as at least its share of a window
    // full by the message guard, so a run of short ones, which meets the
    // guard long before the budget, still steps on. The screenshot is left
    // out: it is counted at the newest one, and a new one must not move the
    // places behind it.
    let least = CONTEXT_TOKENS / MAX_MESSAGES;
    let mut marks = vec![0];
    for (i, m) in messages.iter().enumerate() {
        marks.push(marks[i] + message_tokens(m, budget.tool_chars).max(least));
    }
    let mut start = 0;
    while start < last_user
        && (total - sums[start] > budget.tokens || messages.len() - start > budget.messages)
    {
        // On to the first user turn past the next whole step from the
        // beginning, so the window never begins midway through a tool
        // exchange and where it can begin depends on nothing after it.
        let next = (marks[start] / budget.step + 1) * budget.step;
        start = (start + 1..=last_user)
            .find(|i| marks[*i] >= next && opens_turn(&messages[*i]))
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

fn body(
    connection: &Connection,
    window: &[Message],
    summary: Option<&str>,
    budget: &Budget,
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
    let shot = screenshot(window)
        .filter(|_| sees_images(&connection.model))
        .and_then(|(after, path)| Some((after, crate::media::data_url(&path)?)));
    // The window never trims the turn in progress, and a helper's whole run
    // is one turn, so without this every round would send every result the
    // turn has had again. The session keeps them whole; only the request
    // is shortened.
    let old = old_before(window, budget);
    let stubs = stubs(window, old);
    for (i, m) in window.iter().enumerate() {
        let mut wired = wire_message(m, budget.tool_chars);
        if let Some(stub) = &stubs[i] {
            wired["content"] = stub.as_str().into();
        }
        if i < old {
            for (at, call) in m.tool_calls.iter().enumerate() {
                if call.name != "write_file" {
                    continue;
                }
                // The round's results follow its calls, and a declined write
                // comes back as an error too.
                let wrote = window[i + 1..]
                    .iter()
                    .take_while(|r| r.role == "tool")
                    .any(|r| r.tool_call_id == call.id && !r.is_error);
                if let Some(counted) = written(&call.arguments, wrote) {
                    wired["tool_calls"][at]["function"]["arguments"] = counted.into();
                }
            }
        }
        context.push(wired);
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
        "max_tokens": budget.reply,
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

/// Whether a request for this window carries a picture.
fn shows_picture(connection: &Connection, window: &[Message]) -> bool {
    sees_images(&connection.model) && screenshot(window).is_some_and(|(_, path)| path.is_file())
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

fn wire_message(m: &Message, tool_chars: usize) -> Value {
    let content = if m.role == "tool" {
        trim(&m.content, tool_chars)
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

/// Results and file bodies shorter than this, in characters, go out as they
/// are however old or repeated: a line saying what one was costs about as
/// much, and the model reads it without calling again.
const STUB_CHARS: usize = 200;
/// The newest tool results always go out whole, however full the window:
/// the model is most likely still working from them.
const KEEP_RESULTS: usize = 5;
/// How many tool results the boundary of the old ones moves by at once.
/// Each move changes messages already sent, so the prompt cache misses from
/// the first of them on; moving every eighth result keeps the requests in
/// between identical from the front.
const STUB_STEP: usize = 8;

/// Where the old part of `window` ends. Nothing is old until the window,
/// with every result whole, costs more than half the budget; then what is
/// old ends at the round holding the result on the last whole step of
/// `STUB_STEP` results that leaves the newest `KEEP_RESULTS` out.
///
/// The cost is taken without stubs, so the room they free cannot turn them
/// off again on the next request, and results are only ever added, so the
/// boundary only moves forward until the window itself moves on. A round --
/// a reply's calls and their results -- is never split, so the one the model
/// has not answered yet is never old.
fn old_before(window: &[Message], budget: &Budget) -> usize {
    if token_sums(window, budget.tool_chars)[window.len()] <= budget.tokens / 2 {
        return 0;
    }
    let results: Vec<usize> = (0..window.len())
        .filter(|&i| window[i].role == "tool")
        .collect();
    let boundary = results.len().saturating_sub(KEEP_RESULTS) / STUB_STEP * STUB_STEP;
    if boundary == 0 {
        return 0;
    }
    window[..results[boundary]]
        .iter()
        .rposition(|m| m.role != "tool")
        .unwrap_or(0)
}

/// What goes on the wire instead of each tool result in `window`, where
/// something shorter will do: before `old`, a line saying what the result
/// was and how to get it again; from there on, a pointer back to the last
/// result of the same call when it was identical.
///
/// It is the newer copy that points back, so everything before it goes out
/// as it did last time; that also covers a file read again unchanged. The
/// pointer names the call by its tool and arguments, not by its id: a
/// provider that sends no ids gets `call_0` in every round, and many chat
/// templates never show ids to the model at all.
fn stubs(window: &[Message], old: usize) -> Vec<Option<String>> {
    // Running something the user has to approve again is not for a line of
    // ours to suggest, so those only say what was cleared.
    let gated: Vec<String> = if old == 0 {
        Vec::new()
    } else {
        crate::tools::all()
            .iter()
            .filter(|t| t.needs_approval())
            .map(|t| t.name().to_owned())
            .collect()
    };
    let mut last: HashMap<(&str, &str), &str> = HashMap::new();
    window
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if m.role != "tool" {
                return None;
            }
            let size = m.content.chars().count();
            if i >= old {
                let before = last.insert((m.name.as_str(), m.args.as_str()), m.content.as_str());
                return (size >= STUB_CHARS && before == Some(m.content.as_str())).then(|| {
                    format!(
                        "[Same result as the last {} call with these arguments, above]",
                        m.name
                    )
                });
            }
            // Asking again would put the question to the user again.
            if size < STUB_CHARS || m.name == crate::tools::ASK_USER {
                return None;
            }
            let again = if gated.contains(&m.name) {
                ""
            } else {
                "; call it again if you need it"
            };
            Some(format!(
                "[{} result of {size} chars cleared to save room{again}]",
                m.name
            ))
        })
        .collect()
}

/// A `write_file` call's arguments with the file's text as its length, still
/// a JSON object with the path, when the text is long enough to be worth it;
/// `wrote` says whether the write went through, so a declined or failed one
/// is not claimed as written.
fn written(arguments: &str, wrote: bool) -> Option<String> {
    let mut args: Value = serde_json::from_str(arguments).ok()?;
    let content = args.get_mut("content")?;
    let size = content.as_str()?.chars().count();
    if size < STUB_CHARS {
        return None;
    }
    let outcome = if wrote { "written" } else { "not written" };
    *content = format!("[{size} chars {outcome}]").into();
    Some(args.to_string())
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
    tool_calls: Vec<(usize, String, String, String)>,
}

/// Merge streamed fragments into whole calls, ordered by index.
fn merge_tool_calls(calls: &mut Vec<ToolCall>, chunk: &Chunk) {
    for (index, id, name, arguments) in &chunk.tool_calls {
        while calls.len() <= *index {
            calls.push(ToolCall::default());
        }
        let call = &mut calls[*index];
        call.id.push_str(id);
        call.name.push_str(name);
        call.arguments.push_str(arguments);
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
                .enumerate()
                .map(|(i, c)| {
                    let text = |v: &Value| v.as_str().unwrap_or_default().to_owned();
                    (
                        c["index"].as_u64().map(|n| n as usize).unwrap_or(i),
                        text(&c["id"]),
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
        let budget = budget(&connection, crate::store::settings().context_cap);
        let start = window_start(&messages, &budget);
        let result = stream(&connection, &messages, start, &budget, &sender);
        let _ = sender.send_blocking(Event::Finished(result));
    });
    receiver
}

fn stream(
    connection: &Connection,
    messages: &[Message],
    start: usize,
    budget: &Budget,
    sender: &async_channel::Sender<Event>,
) -> Result<(), Failure> {
    let endpoint = validate(connection).map_err(|e| Failure::new(FailureKind::Settings, e))?;
    // What the window no longer holds goes in as a summary rather than not at
    // all. It is made here, on the worker, so the wait is part of the reply's.
    let (dropped, window) = messages.split_at(start);
    let summary = crate::compact::summary(connection, dropped, budget.step);
    let mut request = client()
        .map_err(|e| Failure::new(FailureKind::Unknown, e))?
        .post(endpoint)
        .timeout(Duration::from_secs(120))
        .json(&body(connection, window, summary.as_deref(), budget));
    if !connection.key.trim().is_empty() {
        request = request.bearer_auth(connection.key.trim());
    }
    // A rate limit or a dropped connection part way through an agent loop
    // would otherwise lose the whole task, though it passes on its own.
    let mut tried = 0;
    let mut timeouts = 0;
    loop {
        tried += 1;
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
                    "Cannot reach the model. Check that your server is running and the endpoint is correct (120-second request timeout).",
                )
            }
            Ok(response) if response.status().is_success() => {
                match read(response, sender, &mut stands, &mut slow) {
                    Ok(()) => return Ok(()),
                    Err(failure) => failure,
                }
            }
            Ok(response) => {
                let status = response.status();
                // A model that cannot take pictures turns the whole request down,
                // each provider in words of its own. Send it again without the
                // screenshot, which that model is spared from then on. A timeout
                // or a rate limit is not about the picture, and passes by itself.
                if status.is_client_error()
                    && !matches!(status.as_u16(), 408 | 429)
                    && shows_picture(connection, window)
                {
                    forget_images(&connection.model);
                    return stream(connection, messages, start, budget, sender);
                }
                let headers = response.headers().clone();
                let body = response.json::<Value>().unwrap_or_default();
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
        if !crate::retry::pause(retry.wait, || sender.is_closed()) {
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
    // An event stream can end early for a reason that passes, as when a
    // gateway keeps it open with comments and then gives up on the model.
    // Any other reply, a web page say, would come back the same every time.
    *stands = !response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.contains("text/event-stream"));
    let mut received = false;
    let mut complete = false;
    let mut completion_tokens = 0u64;
    let mut prompt_tokens = 0u64;
    let mut cached_tokens = 0u64;
    let mut calls: Vec<ToolCall> = Vec::new();
    let began = std::time::Instant::now();
    for line in BufReader::new(response).lines() {
        let line = line.map_err(|e| {
            *slow = ran_out_reading(&e);
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
        // A tool call is not shown until the stream ends, but one begun is an
        // answer under way like any text: another go is billed again, and a
        // server that never sends [DONE] would cut it off again.
        *stands |= !chunk.tool_calls.is_empty();
        merge_tool_calls(&mut calls, &chunk);
        if let Some(text) = chunk.reasoning {
            *stands = true;
            sender
                .send_blocking(Event::Reasoning(text))
                .map_err(|_| Failure::new(FailureKind::Stopped, "Chat closed."))?;
        }
        if let Some(text) = chunk.content {
            received = true;
            *stands = true;
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
        let start = window_start(&messages, &Budget::SMALL);
        let payload = body(&connection(), &messages[start..], None, &Budget::SMALL);
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
        assert!(message_tokens(&messages[2], MAX_TOOL_CHARS) > CONTEXT_TOKENS);
        let start = window_start(&messages, &Budget::SMALL);
        let payload = body(&connection(), &messages[start..], None, &Budget::SMALL);
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
        let payload = body(
            &connection(),
            &messages,
            Some("They chose Postgres."),
            &Budget::SMALL,
        );
        let system = payload["messages"][0]["content"].as_str().unwrap();
        assert!(
            system.ends_with("no longer fits:\nThey chose Postgres."),
            "{system}"
        );
        let without = body(&connection(), &messages, None, &Budget::SMALL);
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
        let wired = wire_message(&reply, MAX_TOOL_CHARS);
        assert_eq!(wired["tool_calls"][0]["function"]["arguments"], "{}");
    }

    #[test]
    fn assembles_streamed_tool_calls() {
        let mut calls = Vec::new();
        for data in [
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"read_file","arguments":""}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"path\":"}}]}}]}"#,
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"a.rs\"}"}}]}}]}"#,
        ] {
            merge_tool_calls(&mut calls, &parse_event(data).unwrap());
        }
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
                &Budget::SMALL,
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
                &Budget::SMALL,
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
            starts.push(window_start(&messages, &Budget::SMALL));
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
        let sums = token_sums(&messages, MAX_TOOL_CHARS);
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

        assert_eq!(window_start(&messages, &Budget::SMALL), 2);
        let start = window_start(&messages, &Budget::SMALL);
        let payload = body(&connection(), &messages[start..], None, &Budget::SMALL);
        let context = payload["messages"].as_array().unwrap();
        assert_eq!(context[1]["content"], "the turn's first request");

        // Stopped, or failed, before the model answered its last results, the
        // turn is still over: the next prompt starts one of its own, and the
        // window moves on to it rather than to the follow-ups before it.
        messages.push(said("user", "the next request"));
        assert_eq!(window_start(&messages, &Budget::SMALL), messages.len() - 1);
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
        let payload = body(&connection(), &messages, None, &Budget::SMALL);
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
            body(&c, &messages, None, &Budget::SMALL)["messages"]
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
        assert_eq!(
            message_tokens(&said, MAX_TOOL_CHARS),
            MESSAGE_TOKENS + 10_000
        );
        let sent = message_tokens(&result, MAX_TOOL_CHARS) - MESSAGE_TOKENS;
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
        assert!(message_tokens(&messages[1], MAX_TOOL_CHARS) > 10_000);
        // The turn that wrote it no longer fits, so the window moves on.
        assert_eq!(window_start(&messages, &Budget::SMALL), 4);
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
        let sums = token_sums(&messages, MAX_TOOL_CHARS);
        let pictured: Vec<usize> = (0..messages.len())
            .filter(|&i| sums[i + 1] - sums[i] != message_tokens(&messages[i], MAX_TOOL_CHARS))
            .collect();
        // The newer screenshot, after the last result of its batch; the
        // older one is never sent, so it costs only its text.
        let last = messages.len() - 1;
        assert_eq!(pictured, [last]);
        assert_eq!(
            sums[last + 1] - sums[last],
            message_tokens(&messages[last], MAX_TOOL_CHARS) + PICTURE_TOKENS
        );
    }

    /// A plain exchange of about 300 tokens, numbered so no two are alike.
    fn exchange(i: usize) -> [Message; 2] {
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
    }

    #[test]
    fn a_bigger_model_keeps_more_history() {
        let messages: Vec<Message> = (0..400).flat_map(exchange).collect();
        let sums = token_sums(&messages, MAX_TOOL_CHARS);
        let kept = |budget: &Budget| sums[messages.len()] - sums[window_start(&messages, budget)];
        let big = sized(Some(200_000), Some(64_000), CAP_TOKENS, 4_000);
        let (small, large) = (kept(&Budget::SMALL), kept(&big));
        assert!(small <= CONTEXT_TOKENS, "{small}");
        // No more than the cap leaves beside the prompt, and several times
        // the small window even just after a step.
        assert!(large <= CAP_TOKENS - 4_000, "{large}");
        assert!(large > 4 * CONTEXT_TOKENS, "{large} against {small}");

        // A run of short messages is not cut off at sixty.
        let short: Vec<Message> = (0..300)
            .map(|i| Message {
                role: if i % 2 == 0 { "user" } else { "assistant" }.into(),
                content: format!("ok {i}"),
                ..Default::default()
            })
            .collect();
        let start = window_start(&short, &Budget::SMALL);
        assert!(short.len() - start <= MAX_MESSAGES, "{start}");
        assert!(short.len() - start >= MAX_MESSAGES / 2, "{start}");
        assert_eq!(window_start(&short, &big), 0);

        // And a tool result as long as any tool returns goes out whole.
        let result = Message {
            role: "tool".into(),
            content: "z".repeat(24_000),
            ..Default::default()
        };
        let sent = |budget: &Budget| {
            let wired = wire_message(&result, budget.tool_chars);
            wired["content"].as_str().unwrap().len()
        };
        assert!(sent(&Budget::SMALL) < MAX_TOOL_CHARS + 64);
        assert_eq!(sent(&big), 24_000);
    }

    #[test]
    fn boundaries_do_not_move_when_memory_changes() {
        // Memory written, an MCP server connected: more of every request is
        // taken before the first message.
        let lean = sized(Some(200_000), Some(64_000), CAP_TOKENS, 2_000);
        let full = sized(Some(200_000), Some(64_000), CAP_TOKENS, 12_000);
        assert!(full.tokens < lean.tokens);
        assert_eq!((full.step, full.messages), (lean.step, lean.messages));

        let starts = |budget: &Budget, turns: usize| {
            let mut messages = Vec::new();
            let mut starts = std::collections::BTreeSet::new();
            for i in 0..turns {
                messages.extend(exchange(i));
                starts.insert(window_start(&messages, budget));
            }
            starts
        };
        // The fuller prompt moves the window sooner, but only ever to a turn
        // the leaner one starts at too once it has grown that far, so the
        // summaries and the cached prefixes are the same ones.
        let fuller = starts(&full, 600);
        let leaner = starts(&lean, 700);
        assert!(fuller.len() > 3, "{fuller:?}");
        assert!(fuller.is_subset(&leaner), "{fuller:?} against {leaner:?}");
    }

    #[test]
    fn an_unknown_model_keeps_the_small_window() {
        // However big the cap and however small the prompt.
        assert_eq!(sized(None, None, CAP_TOKENS, 2_000), Budget::SMALL);
        assert_eq!(sized(None, None, 1_000_000, 0), Budget::SMALL);
        // A window known to be too small for more, or a cap below it, gets it
        // as the least, with a reply no longer than a quarter of the window.
        assert_eq!(
            sized(Some(8_192), None, CAP_TOKENS, 2_000),
            Budget {
                reply: 2_048,
                ..Budget::SMALL
            }
        );
        assert_eq!(sized(Some(200_000), None, 1_000, 2_000), Budget::SMALL);
        // Ollama's list says nothing and its models are in no table.
        assert_eq!(budget(&connection(), 0), Budget::SMALL);
    }

    #[test]
    fn a_known_model_is_sized_from_its_facts() {
        let gpt = Connection {
            endpoint: "https://sized.test/v1".into(),
            model: "gpt-4o".into(),
            ..connection()
        };
        // The family table's output limit, and its window under the default
        // cap less what the prompt takes.
        let default = budget(&gpt, 0);
        assert_eq!(default.reply, 16_384);
        assert_eq!(default.step, CAP_TOKENS / 2);
        assert!(default.tokens < CAP_TOKENS, "{default:?}");
        // Or under the user's own cap.
        assert_eq!(budget(&gpt, 20_000).step, 10_000);
    }

    #[test]
    fn a_prompt_over_half_the_room_still_leaves_history_in_the_window() {
        // A 32K window whose prompt, memory and tools take 12,000 tokens has
        // less left for the messages than half its room.
        let budget = sized(Some(32_768), None, CAP_TOKENS, 12_000);
        assert!(budget.step <= budget.tokens, "{budget:?}");
        let mut messages = Vec::new();
        for i in 0..400 {
            messages.extend(exchange(i));
            // The window never falls back to the current turn alone, which
            // would drop a little more of the session on every request.
            let start = window_start(&messages, &budget);
            assert!(i == 0 || start < messages.len() - 2, "{i}: {start}");
        }
    }

    #[test]
    fn a_new_screenshot_does_not_move_where_the_window_starts() {
        let browse = |id: &str, picture: &str| {
            [
                Message {
                    role: "assistant".into(),
                    tool_calls: vec![ToolCall {
                        id: id.into(),
                        name: crate::browser::BROWSE.into(),
                        arguments: "{}".into(),
                    }],
                    ..Default::default()
                },
                Message {
                    role: "tool".into(),
                    tool_call_id: id.into(),
                    name: crate::browser::BROWSE.into(),
                    content: format!("Saved to /pictures/{picture}.jpg\nA screenshot."),
                    ..Default::default()
                },
            ]
        };
        let said = |text: &str| Message {
            role: "user".into(),
            content: text.into(),
            ..Default::default()
        };
        let budget = Budget::SMALL;
        let mut held = 0;
        for n in 20..80 {
            // A screenshot early on, long since out of the window.
            let mut messages = vec![said("look at it")];
            messages.extend(browse("a", "old"));
            messages.extend((0..n).flat_map(exchange));
            messages.push(said("look again"));
            let before = window_start(&messages, &budget);
            // The next round takes a new one.
            messages.extend(browse("b", "new"));
            let sums = token_sums(&messages, budget.tool_chars);
            let fits = sums[messages.len()] - sums[before] <= budget.tokens
                && messages.len() - before <= budget.messages;
            if fits {
                held += 1;
                assert_eq!(window_start(&messages, &budget), before, "{n}");
            }
        }
        assert!(held > 10, "{held}");
    }

    #[test]
    fn the_cap_limits_a_huge_window() {
        let opus = sized(Some(1_000_000), Some(128_000), CAP_TOKENS, 5_000);
        assert_eq!(opus.tokens, CAP_TOKENS - 5_000);
        assert_eq!(opus.step, CAP_TOKENS / 2);
        // A cap of the user's own lets more in.
        let raised = sized(Some(1_000_000), Some(128_000), 300_000, 5_000);
        assert_eq!(raised.tokens, 295_000);
        // But no more than three quarters of what the reply leaves.
        let gpt = sized(Some(128_000), Some(16_384), 1_000_000, 5_000);
        assert_eq!(gpt.tokens, (128_000 - 16_384) / 4 * 3 - 5_000);
    }

    #[test]
    fn max_tokens_rises_only_when_the_output_limit_is_known() {
        let asked = |budget: Budget| body(&connection(), &[], None, &budget)["max_tokens"].clone();
        assert_eq!(asked(sized(Some(200_000), None, CAP_TOKENS, 0)), 8_192);
        assert_eq!(
            asked(sized(Some(200_000), Some(16_384), CAP_TOKENS, 0)),
            16_384
        );
        assert_eq!(
            asked(sized(Some(200_000), Some(64_000), CAP_TOKENS, 0)),
            32_000
        );
        // Not without the window, which a longer reply could outgrow.
        assert_eq!(asked(sized(None, Some(64_000), CAP_TOKENS, 0)), 8_192);
        // A limit known to be lower than the old fixed ask lowers it.
        assert_eq!(
            asked(sized(Some(128_000), Some(4_096), CAP_TOKENS, 0)),
            4_096
        );
        assert_eq!(asked(sized(None, Some(4_096), CAP_TOKENS, 0)), 4_096);
        // A model listed as writing as much as it reads still has room to
        // read, and the prompt, the messages and the reply fit together.
        let even = sized(Some(32_768), Some(32_768), CAP_TOKENS, 3_500);
        assert_eq!(even.reply, 8_192);
        assert!(3_500 + even.tokens + even.reply <= 32_768, "{even:?}");
    }

    /// A reply calling `name` once with `arguments`, and the result it got.
    fn round(id: &str, name: &str, arguments: String, result: String) -> [Message; 2] {
        [
            Message {
                role: "assistant".into(),
                tool_calls: vec![ToolCall {
                    id: id.into(),
                    name: name.into(),
                    arguments: arguments.clone(),
                }],
                ..Default::default()
            },
            Message {
                role: "tool".into(),
                tool_call_id: id.into(),
                name: name.into(),
                args: arguments,
                content: result,
                ..Default::default()
            },
        ]
    }

    /// A turn that reads `files` files one a round, each of about `chars`
    /// characters and numbered so no two are alike.
    fn reads(files: usize, chars: usize) -> Vec<Message> {
        let mut messages = vec![Message {
            role: "user".into(),
            content: "read them all".into(),
            ..Default::default()
        }];
        for i in 0..files {
            messages.extend(round(
                &format!("r{i}"),
                "read_file",
                format!(r#"{{"path":"{i}.txt"}}"#),
                format!("{i} {}", "x".repeat(chars)),
            ));
        }
        messages
    }

    /// The tool results a request for `window` sends, in order.
    fn results_sent(window: &[Message], budget: &Budget) -> Vec<String> {
        body(&connection(), window, None, budget)["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .map(|m| m["content"].as_str().unwrap().to_owned())
            .collect()
    }

    /// How many of `sent` go out as a line saying what they were.
    fn cleared(sent: &[String]) -> usize {
        sent.iter()
            .filter(|s| s.contains("cleared to save room"))
            .count()
    }

    #[test]
    fn a_repeated_result_points_back_at_the_first() {
        let mut messages = reads(3, 1_000);
        let file = messages[2].content.clone();
        let first = messages[1].tool_calls[0].arguments.clone();
        // The first file read twice more, unchanged, and a short result twice.
        messages.extend(round("r3", "read_file", first.clone(), file.clone()));
        messages.extend(round("r4", "read_file", first, file.clone()));
        messages.extend(round("e0", "edit_file", "{}".into(), "Edited.".into()));
        messages.extend(round("e1", "edit_file", "{}".into(), "Edited.".into()));
        let sent = results_sent(&messages, &Budget::SMALL);
        let pointer = "[Same result as the last read_file call with these arguments, above]";
        assert_eq!(sent[0], file);
        assert_eq!(sent[1], messages[4].content);
        assert_eq!(sent[3], pointer);
        assert_eq!(sent[4], pointer);
        // Pointing back at a result this short would save nothing.
        assert_eq!(sent[5..], ["Edited.", "Edited."]);
        // And the window is too small for anything to be cleared yet.
        assert_eq!(cleared(&sent), 0);
    }

    #[test]
    fn a_pointer_names_the_call_not_its_id() {
        // A provider that sends no ids gets the same one in every round.
        let read = |path: &str, text: &str| {
            let arguments = json!({ "path": path }).to_string();
            round("call_0", "read_file", arguments, text.repeat(300))
        };
        let mut messages = vec![Message {
            role: "user".into(),
            content: "read them".into(),
            ..Default::default()
        }];
        messages.extend(read("a.rs", "a"));
        messages.extend(read("b.rs", "b"));
        // a.rs again, unchanged.
        messages.extend(read("a.rs", "a"));
        // b.rs now holds what a.rs does, but it is not the same call.
        messages.extend(read("b.rs", "a"));
        // a.rs changed and changed back: the last read of it differs.
        messages.extend(read("a.rs", "c"));
        messages.extend(read("a.rs", "a"));
        let sent = results_sent(&messages, &Budget::SMALL);
        assert_eq!(
            sent[2],
            "[Same result as the last read_file call with these arguments, above]"
        );
        assert_eq!(sent[3], "a".repeat(300));
        assert_eq!(sent[5], "a".repeat(300));
    }

    #[test]
    fn a_cleared_result_is_never_pointed_at() {
        let mut messages = reads(20, 800);
        let first = messages[1].tool_calls[0].arguments.clone();
        let file = messages[2].content.clone();
        // The first file read again, unchanged, once its first read is cleared.
        messages.extend(round("again", "read_file", first, file.clone()));
        let sent = results_sent(&messages, &Budget::SMALL);
        assert_eq!(cleared(&sent[..1]), 1);
        assert_eq!(sent[20], file);
    }

    #[test]
    fn a_cleared_command_is_not_offered_again() {
        let mut messages = vec![Message {
            role: "user".into(),
            content: "migrate it".into(),
            ..Default::default()
        }];
        let command = json!({"command": "./migrate.sh"}).to_string();
        messages.extend(round("c", "run_command", command, "done ".repeat(100)));
        messages.extend(reads(16, 2_000).into_iter().skip(1));
        let sent = results_sent(&messages, &Budget::SMALL);
        // Running it again is for the user to approve, not for a stub to offer.
        assert_eq!(
            sent[0],
            "[run_command result of 500 chars cleared to save room]"
        );
        assert!(sent[1].ends_with("; call it again if you need it]"));
    }

    #[test]
    fn old_results_are_stubbed_only_past_half_the_budget() {
        let budget = Budget::SMALL;
        let cost = |window: &[Message]| token_sums(window, budget.tool_chars)[window.len()];
        // Twenty results of 400 characters come to less than half the budget.
        let light = reads(20, 400);
        assert!(cost(&light) <= budget.tokens / 2, "{}", cost(&light));
        let sent = results_sent(&light, &budget);
        assert_eq!(cleared(&sent), 0);
        // Twice as long they come to more: the oldest eight go out as a line
        // each, and the twelve after them whole.
        let heavy = reads(20, 800);
        assert!(cost(&heavy) > budget.tokens / 2, "{}", cost(&heavy));
        let sent = results_sent(&heavy, &budget);
        assert_eq!(cleared(&sent[..8]), 8);
        assert_eq!(
            sent[0],
            "[read_file result of 802 chars cleared to save room; call it again if you need it]"
        );
        for (i, result) in sent.iter().enumerate().skip(8) {
            assert_eq!(*result, heavy[2 + 2 * i].content, "{i}");
        }
    }

    #[test]
    fn the_boundary_moves_in_steps_of_eight() {
        // Past half the budget from the sixth result on.
        let turn = reads(40, 2_000);
        let requests: Vec<Vec<Value>> = (1..=40)
            .map(|files| {
                let payload = body(&connection(), &turn[..1 + 2 * files], None, &Budget::SMALL);
                payload["messages"].as_array().unwrap().clone()
            })
            .collect();
        // A request goes out as the one before it did, from the front, but
        // for when the boundary moves, which it does as the 13th, 21st, 29th
        // and 37th result come in: always forward, and always leaving the
        // five newest results whole at least.
        let moved: Vec<usize> = (1..40)
            .filter(|&n| {
                let (before, after) = (&requests[n - 1], &requests[n]);
                after[..before.len()] != before[..]
            })
            .map(|n| n + 1)
            .collect();
        assert_eq!(moved, [13, 21, 29, 37]);
        let counts: Vec<usize> = requests
            .iter()
            .map(|messages| {
                let sent: Vec<String> = messages
                    .iter()
                    .filter_map(|m| m["content"].as_str().filter(|_| m["role"] == "tool"))
                    .map(str::to_owned)
                    .collect();
                cleared(&sent)
            })
            .collect();
        assert_eq!(counts[11], 0);
        assert_eq!(&counts[12..20], [8; 8]);
        assert_eq!(counts[39], 32);
    }

    #[test]
    fn the_round_the_model_has_not_answered_is_never_cleared() {
        let mut messages = reads(6, 2_000);
        // Twelve files read at once, whose results the model has not seen.
        let calls: Vec<ToolCall> = (0..12)
            .map(|i| ToolCall {
                id: format!("b{i}"),
                name: "read_file".into(),
                arguments: "{}".into(),
            })
            .collect();
        messages.push(Message {
            role: "assistant".into(),
            tool_calls: calls,
            ..Default::default()
        });
        for i in 0..12 {
            messages.push(Message {
                role: "tool".into(),
                tool_call_id: format!("b{i}"),
                name: "read_file".into(),
                content: format!("batch {i} {}", "z".repeat(2_000)),
                ..Default::default()
            });
        }
        // Eighteen results put the boundary at the ninth, inside the batch:
        // it goes back to where the batch starts instead.
        let sent = results_sent(&messages, &Budget::SMALL);
        assert_eq!(cleared(&sent[..6]), 6);
        assert_eq!(cleared(&sent[6..]), 0);
    }

    #[test]
    fn an_old_answer_from_the_user_is_never_cleared() {
        let mut messages = vec![Message {
            role: "user".into(),
            content: "set it up".into(),
            ..Default::default()
        }];
        let answer = format!("Use the second option. {}", "Because ".repeat(40));
        messages.extend(round(
            "q",
            crate::tools::ASK_USER,
            r#"{"question":"Which one?"}"#.into(),
            answer.clone(),
        ));
        messages.extend(reads(16, 2_000).into_iter().skip(1));
        let sent = results_sent(&messages, &Budget::SMALL);
        // As old as the seven reads after it, but sent whole.
        assert_eq!(sent[0], answer);
        assert_eq!(cleared(&sent[1..8]), 7);
    }

    #[test]
    fn stored_messages_are_never_changed() {
        let mut messages = reads(20, 800);
        let newest = messages[messages.len() - 1].content.clone();
        let call = messages[messages.len() - 2].tool_calls[0].arguments.clone();
        messages.extend(round("again", "read_file", call, newest));
        let stored = serde_json::to_string(&messages).unwrap();
        let first = body(&connection(), &messages, None, &Budget::SMALL);
        let wire = first.to_string();
        assert!(wire.contains("[read_file result of 802 chars cleared to save room"));
        assert!(wire.contains("[Same result as the last read_file call"));
        // The session still has every result whole, so the window is placed
        // and summarized from them, and the same history makes the same
        // request.
        assert_eq!(serde_json::to_string(&messages).unwrap(), stored);
        assert_eq!(messages[2].content.len(), 802);
        assert_eq!(body(&connection(), &messages, None, &Budget::SMALL), first);
    }

    #[test]
    fn old_write_file_bodies_become_a_count() {
        let write = |id: &str, path: &str| {
            let arguments = json!({"path": path, "content": "y".repeat(5_000)});
            round(
                id,
                "write_file",
                arguments.to_string(),
                "Wrote 5000 bytes.".into(),
            )
        };
        let mut messages = vec![Message {
            role: "user".into(),
            content: "write them".into(),
            ..Default::default()
        }];
        messages.extend(write("w0", "old.txt"));
        let mut declined = write("w2", "no.txt");
        declined[1].content = crate::tools::DECLINED.into();
        declined[1].is_error = true;
        messages.extend(declined);
        messages.extend(reads(13, 800).into_iter().skip(1));
        messages.extend(write("w1", "new.txt"));
        let payload = body(&connection(), &messages, None, &Budget::SMALL);
        let arguments = |id: &str| -> Value {
            let call = payload["messages"]
                .as_array()
                .unwrap()
                .iter()
                .flat_map(|m| m["tool_calls"].as_array().cloned().unwrap_or_default())
                .find(|c| c["id"] == id)
                .unwrap();
            serde_json::from_str(call["function"]["arguments"].as_str().unwrap()).unwrap()
        };
        // Still an object with the path, so it reads as the call it was.
        assert_eq!(
            arguments("w0"),
            json!({"path": "old.txt", "content": "[5000 chars written]"})
        );
        // A write the user declined is not claimed as written.
        assert_eq!(
            arguments("w2"),
            json!({"path": "no.txt", "content": "[5000 chars not written]"})
        );
        // The newest write goes out whole.
        assert_eq!(arguments("w1")["content"].as_str().unwrap().len(), 5_000);
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
}
