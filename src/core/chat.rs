use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader},
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

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// JSON object as sent by the model.
    pub arguments: String,
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[derive(Clone)]
pub struct Connection {
    pub endpoint: String,
    pub model: String,
    pub key: String,
    /// Use the Upstream guest route (free catalog, cookie session) instead of the keyed API.
    pub guest: bool,
    /// Advertise tools. Off for the guest route, which does not forward them.
    pub tools: bool,
    /// Folder the session works in, shown to the model.
    pub workspace: Option<std::path::PathBuf>,
    /// Plan mode: read and ask only, nothing that changes the project.
    pub plan: bool,
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
    FreeRemaining(u64),
    /// Sent once, before `Finished`, when the model asked for tools.
    ToolCalls(Vec<ToolCall>),
    Finished(Result<(), String>),
}

pub fn uses_guest_route(connection: &Connection) -> bool {
    connection.guest
}

/// The marketplace API does not know the `pool-free/` prefix; with a key, send the bare id
/// so the router picks a provider. The guest route keeps the full id.
pub fn wire_model(connection: &Connection) -> &str {
    let model = connection.model.trim();
    if uses_guest_route(connection) {
        return model;
    }
    model.strip_prefix("pool-free/").unwrap_or(model)
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
    if uses_guest_route(connection) {
        if base != "https://pool.example/v1" {
            return Err("Upstream Free uses the Upstream base URL: https://pool.example/v1".into());
        }
        if !connection.model.starts_with("pool-free/") {
            return Err("Upstream Free only serves models from the free catalog. Switch to the Upstream gateway for marketplace models.".into());
        }
        return Ok("https://chat.pool.example/api/free/chat/completions".into());
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

fn body(connection: &Connection, messages: &[Message]) -> Value {
    let start = window_start(messages);
    let mut system =
        String::from("You are Letronna, a personal assistant. Be clear, concise, and honest.");
    if connection.tools {
        system.push_str(" You have tools to read, search, write and edit files and to run commands in the user's project; use them instead of guessing. Writes and commands are shown to the user for approval; a denial is final for that action. Ask with ask_user when a decision is the user's to make, such as a project name. After changing code, verify it (build, test, or run) before reporting done.");
        match &connection.workspace {
            Some(w) => system.push_str(&format!(" The project folder is `{}`.", w.display())),
            None => system.push_str(" No project folder is attached: you can only read under the home folder. Ask the user to attach one before writing or running anything."),
        }
    } else {
        system.push_str(" You have no tools or filesystem access on this gateway.");
    }
    if connection.plan {
        system.push_str(" Plan mode is on: you can read, search and ask, but you cannot write files or run commands, and those tools are unavailable. Work out what you would do and lay out the plan, then let the user turn plan mode off to carry it out.");
    }
    let mut context = vec![json!({"role": "system", "content": system})];
    let window = &messages[start..];
    for (i, m) in window.iter().enumerate() {
        context.push(wire_message(m));
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
        "model": wire_model(connection),
        "messages": context,
        "stream": true,
        // Writing a file puts the whole body in one tool call, so a small cap
        // truncates the arguments mid-JSON.
        "max_tokens": 8192,
        "stream_options": {"include_usage": true}
    });
    if connection.tools {
        body["tools"] = Value::Array(super::tools::specs(connection.plan));
    }
    body
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
                json!({"id": c.id, "type": "function",
                    "function": {"name": c.name, "arguments": c.arguments}})
            })
            .collect();
    }
    v
}

/// Statuses worth one retry: gateway and edge failures that say nothing about
/// the request itself. 520-527 are Cloudflare's own (525 is a failed TLS
/// handshake between the edge and the origin).
fn transient(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 502 | 503 | 504 | 520..=527)
}

fn unwrap_response(value: &Value) -> Result<&Value, String> {
    if value.get("error").is_some() || value.get("success").is_some_and(|v| v != true) {
        let message = value
            .pointer("/error/message")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("The provider could not finish this response.");
        return Err(message.chars().take(400).collect());
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

fn parse_event(data: &str) -> Result<Chunk, String> {
    let value: Value = serde_json::from_str(data)
        .map_err(|_| "Provider returned an invalid stream event.".to_owned())?;
    let inner = unwrap_response(&value)?;
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

pub fn list_models(base: &str, key: &str) -> Result<Vec<String>, String> {
    let base = base.trim().trim_end_matches('/');
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Cannot initialize connection.".to_owned())?;
    let mut request = client.get(format!("{base}/models"));
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
    let models = unwrap_response(&value)?["data"]
        .as_array()
        .ok_or("Invalid model list.")?
        .iter()
        .filter_map(|m| m["id"].as_str().or_else(|| m["key"].as_str()))
        .map(str::to_owned)
        .collect();
    Ok(models)
}

pub fn free_models() -> Result<Vec<String>, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Cannot initialize connection.".to_owned())?;
    let response = client
        .get("https://chat.pool.example/api/free/models")
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|_| "Cannot load the Upstream free model catalog.".to_owned())?;
    let value: Value = response
        .json()
        .map_err(|_| "Invalid model catalog.".to_owned())?;
    let models = value["data"]
        .as_array()
        .ok_or("Invalid model catalog.")?
        .iter()
        .filter_map(|m| m["key"].as_str())
        .filter(|id| id.starts_with("pool-free/"))
        .map(str::to_owned)
        .collect();
    Ok(models)
}

fn free_session(client: &reqwest::blocking::Client) -> Result<(String, u64), String> {
    let path = data_dir().join("upstream-session.txt");
    let stored = match std::fs::read_to_string(&path) {
        Ok(value) => value,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(_) => return Err("Cannot read the saved Upstream session.".into()),
    };
    let mut request = client.get("https://chat.pool.example/api/free/session");
    if !stored.is_empty() {
        request = request.header(reqwest::header::COOKIE, stored.trim());
    }
    let response = request
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|_| "Cannot open an Upstream free session.".to_owned())?;
    let cookie = response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|v| v.split(';').next())
        .find(|v| v.starts_with("pool_private_guest="))
        .map(str::to_owned)
        .unwrap_or(stored);
    let value: Value = response
        .json()
        .map_err(|_| "Invalid Upstream session response.".to_owned())?;
    let remaining = value["remaining"]
        .as_u64()
        .ok_or("Upstream returned no guest allowance.")?;
    if cookie.is_empty() {
        return Err("Upstream did not issue a guest session.".into());
    }
    std::fs::create_dir_all(data_dir())
        .and_then(|_| std::fs::write(path, &cookie))
        .map_err(|_| {
            "Cannot save the Upstream session. Check your data-folder permissions.".to_owned()
        })?;
    if remaining == 0 {
        return Err("Upstream guest replies are used up. Open Settings, pick the Upstream gateway and add your API key to keep going.".into());
    }
    Ok((cookie, remaining))
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
) -> Result<(), String> {
    let endpoint = validate(connection)?;
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(120))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| "Could not initialize the HTTP client.".to_owned())?;
    let mut request = client.post(endpoint).json(&body(connection, messages));
    let free_remaining = if uses_guest_route(connection) {
        let (cookie, remaining) = free_session(&client)?;
        request = request.header(reqwest::header::COOKIE, cookie);
        let _ = sender.send_blocking(Event::FreeRemaining(remaining));
        Some(remaining)
    } else {
        if !connection.key.trim().is_empty() {
            request = request.bearer_auth(connection.key.trim());
        }
        None
    };
    // A transient edge failure part way through an agent loop would otherwise
    // lose the whole task, so give it one more go before giving up.
    let mut attempt = 0;
    let response = loop {
        attempt += 1;
        let send = request
            .try_clone()
            .ok_or("Could not prepare the request.")?
            .send();
        match send {
            Ok(response) if transient(response.status()) && attempt == 1 => {
                std::thread::sleep(Duration::from_millis(600));
            }
            Ok(response) => break response,
            Err(_) if attempt == 1 => std::thread::sleep(Duration::from_millis(600)),
            Err(_) => return Err("Cannot reach the model. Check that your server is running and the endpoint is correct (120-second request timeout).".to_owned()),
        }
    };
    let status = response.status();
    if !status.is_success() {
        let detail = response
            .json::<Value>()
            .ok()
            .and_then(|v| unwrap_response(&v).err());
        return Err(detail.unwrap_or_else(|| {
            let code = status.as_u16();
            if transient(status) {
                format!(
                    "The gateway is having trouble right now (HTTP {code}). This is at                      the provider's end, not your settings. Try again in a moment, or                      switch gateway in Settings."
                )
            } else {
                format!("Provider returned HTTP {code}. Check connection settings and quota.")
            }
        }));
    }
    if response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|h| h.contains("application/json"))
    {
        let value: Value = response
            .json()
            .map_err(|_| "Invalid model response.".to_owned())?;
        let content = unwrap_response(&value)?
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .ok_or("Model returned no text.")?
            .to_owned();
        sender
            .send_blocking(Event::Delta(content))
            .map_err(|_| "Chat closed.".to_owned())?;
        if let Some(n) = free_remaining {
            let _ = sender.send_blocking(Event::FreeRemaining(n.saturating_sub(1)));
        }
        return Ok(());
    }
    let mut received = false;
    let mut complete = false;
    let mut completion_tokens = 0u64;
    let mut prompt_tokens = 0u64;
    let mut cached_tokens = 0u64;
    let mut calls: Vec<ToolCall> = Vec::new();
    let began = std::time::Instant::now();
    for line in BufReader::new(response).lines() {
        let line =
            line.map_err(|_| "Connection interrupted before the response completed.".to_owned())?;
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
        merge_tool_calls(&mut calls, &chunk);
        if let Some(text) = chunk.reasoning {
            sender
                .send_blocking(Event::Reasoning(text))
                .map_err(|_| "Chat closed.".to_owned())?;
        }
        if let Some(text) = chunk.content {
            received = true;
            sender
                .send_blocking(Event::Delta(text))
                .map_err(|_| "Chat closed.".to_owned())?;
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
        return Err(
            "The provider closed the stream early or does not support SSE chat completions.".into(),
        );
    }
    calls.retain(|c| !c.name.is_empty());
    if !calls.is_empty() {
        received = true;
        sender
            .send_blocking(Event::ToolCalls(calls))
            .map_err(|_| "Chat closed.".to_owned())?;
    }
    if !received {
        return Err("The model returned no text. Check model compatibility.".into());
    }
    if let Some(n) = free_remaining {
        let _ = sender.send_blocking(Event::FreeRemaining(n.saturating_sub(1)));
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
            guest: false,
            tools: false,
            workspace: None,
            plan: false,
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
    fn pool_free_contract() {
        assert_eq!(
            parse_event(
                r#"{"success":true,"data":{"choices":[{"delta":{"content":"Upstream reply"}}]}}"#
            )
            .unwrap()
            .content,
            Some("Upstream reply".into())
        );
        assert!(parse_event(r#"{"success":false,"message":"Quota exhausted"}"#).is_err());
        let c = Connection {
            endpoint: "https://pool.example/v1".into(),
            model: "pool-free/gpt-oss-20b".into(),
            key: String::new(),
            guest: true,
            tools: false,
            workspace: None,
            plan: false,
        };
        assert_eq!(
            validate(&c).unwrap(),
            "https://chat.pool.example/api/free/chat/completions"
        );
        let with_key = Connection {
            key: "sk-test".into(),
            guest: false,
            ..c
        };
        assert_eq!(
            validate(&with_key).unwrap(),
            "https://pool.example/v1/chat/completions"
        );
        assert_eq!(wire_model(&with_key), "gpt-oss-20b");
    }
    #[test]
    #[ignore = "Uses one real Upstream guest prompt; run explicitly"]
    fn live_pool_free_chat() {
        let rx = start(
            Connection {
                endpoint: "https://pool.example/v1".into(),
                model: "pool-free/gpt-oss-20b".into(),
                key: String::new(),
                guest: true,
                tools: false,
                workspace: None,
                plan: false,
            },
            vec![Message {
                role: "user".into(),
                content: "Reply with exactly: Native chat works.".into(),
                ..Default::default()
            }],
        );
        let mut text = String::new();
        loop {
            match rx.recv_blocking().unwrap() {
                Event::Delta(chunk) => text.push_str(&chunk),
                Event::Reasoning(_) | Event::Usage { .. } => {}
                Event::FreeRemaining(_) | Event::ToolCalls(_) => {}
                Event::Finished(result) => {
                    result.unwrap();
                    break;
                }
            }
        }
        assert!(!text.trim().is_empty());
        println!("Upstream live reply: {text}");
    }
    #[test]
    #[ignore = "Uses the saved Upstream key and one marketplace model; run explicitly"]
    fn live_tool_loop() {
        let keys: Value =
            serde_json::from_slice(&std::fs::read(data_dir().join("keys.json")).unwrap()).unwrap();
        let connection = Connection {
            endpoint: "https://pool.example/v1".into(),
            model: std::env::var("AGENT_MODEL").unwrap_or_else(|_| "gpt-4o-mini".into()),
            key: keys["Upstream"].as_str().unwrap().into(),
            guest: false,
            tools: true,
            workspace: Some(std::env::current_dir().unwrap()),
            plan: false,
        };
        let ctx = super::super::tools::ToolContext {
            workspace: connection.workspace.clone(),
        };
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
                let content = super::super::tools::run(&call.name, &call.arguments, &ctx)
                    .unwrap_or_else(|e| e);
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
        let payload = body(&connection(), &messages);
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
        let payload = body(&connection(), &messages);
        let context = payload["messages"].as_array().unwrap();
        assert_eq!(context.len(), 4);
        assert_eq!(context[1]["content"], "what is in the file?");
        // The oversized result is trimmed rather than dropped.
        let sent = context[3]["content"].as_str().unwrap().chars().count();
        assert!(sent < MAX_TOOL_CHARS + 64, "{sent}");
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
        let payload = body(&connection(), &messages);
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
}
