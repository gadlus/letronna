//! Anthropic's Messages API, spoken at the edge for a key of Anthropic's own.
//!
//! Every other route takes OpenAI's chat completions, and Anthropic's copy of
//! that API has no prompt caching, ignores the effort and sends no thinking
//! back. So a request is still built as a chat completion, as for any
//! gateway, and turned into a Messages request only on its way out, and what
//! comes back is turned into what a chat completion would have sent. The
//! agent loop, the fallback chain, cache-miss attribution and `-p` JSON never
//! see the difference.
//!
//! Thinking comes back signed, and goes back with the reply it came in,
//! unchanged and in its place among the reply's blocks. Its signature binds
//! everything sent before it, and Letronna changes what it has already sent:
//! the plan note moves to the newest message, old results become stubs, and
//! the system prompt carries the date and the summary of dropped turns. An
//! account made on or after 2026-08-31 is refused such a request outright,
//! so every request that thinks asks for `drop_block`, which leaves out a
//! block whose history changed instead.

use reqwest::blocking::RequestBuilder;
use serde_json::{Value, json};
use std::collections::HashMap;

/// The host of Anthropic's own API.
const HOST: &str = "api.anthropic.com";
/// The API version every request names: the one the Messages API came out as.
const VERSION: &str = "2023-06-01";
/// The beta under which a request says what to do with a thinking block
/// whose history changed.
const BINDING: &str = "thinking-binding-controls-2026-08-01";
/// The reply cap sent when a request names none, which Messages requires:
/// the 8192 tokens a turn's reply is given before its model is known.
const REPLY_TOKENS: u64 = 8_192;

/// Whether requests to `endpoint` go out as Messages requests: Anthropic's
/// own API, whether its gateway or Custom points there.
pub(crate) fn speaks(endpoint: &str) -> bool {
    crate::chat::host(endpoint).is_some_and(|host| host == HOST) || stands_in(endpoint)
}

/// The local servers tests stand in for Anthropic's API with, by base URL.
#[cfg(test)]
pub(crate) static STAND_INS: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[cfg(test)]
fn stands_in(endpoint: &str) -> bool {
    let endpoint = endpoint.trim();
    STAND_INS
        .lock()
        .is_ok_and(|bases| bases.iter().any(|base| endpoint.starts_with(base.as_str())))
}

#[cfg(not(test))]
fn stands_in(_: &str) -> bool {
    false
}

/// `request` with the key and the API version every call to Anthropic carries.
pub(crate) fn keyed(request: RequestBuilder, key: &str) -> RequestBuilder {
    let request = request.header("anthropic-version", VERSION);
    match key.trim() {
        "" => request,
        key => request.header("x-api-key", key),
    }
}

/// `request` carrying `posted`, a Messages request, keyed, and naming the
/// binding beta when `posted` uses it.
pub(crate) fn sign(request: RequestBuilder, key: &str, posted: &Value) -> RequestBuilder {
    let request = keyed(request.json(posted), key);
    match posted.pointer("/thinking/block_binding") {
        Some(_) => request.header("anthropic-beta", BINDING),
        None => request,
    }
}

/// What a request may say about thinking, beyond what its chat body says.
#[derive(Clone, Copy)]
pub(crate) struct Thinking {
    /// The model has not turned adaptive thinking down.
    pub allowed: bool,
    /// The route has not turned the binding beta down.
    pub bound: bool,
}

/// `chat`, a chat completion request, as a Messages request.
///
/// Tool results, and the pictures that follow them as the user's, become one
/// user turn with the results first, as Anthropic requires. Thinking is asked
/// for when the request sets an effort, or sends thinking back, which only a
/// model that thinks unasked has given; a model that thinks only when asked
/// is otherwise left to answer as it would.
pub(crate) fn request(chat: &Value, thinking: Thinking) -> Value {
    let mut system = Vec::new();
    let mut turns: Vec<(&str, Vec<Value>)> = Vec::new();
    for message in chat["messages"].as_array().into_iter().flatten() {
        let (role, content) = match message["role"].as_str().unwrap_or_default() {
            "system" => {
                system.extend(blocks(&message["content"]));
                continue;
            }
            "assistant" => ("assistant", said(message, thinking.allowed)),
            "tool" => ("user", vec![result(message)]),
            _ => ("user", blocks(&message["content"])),
        };
        // Anthropic refuses a turn with nothing in it.
        if content.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((last, so_far)) if *last == role => so_far.extend(content),
            _ => turns.push((role, content)),
        }
    }
    let replays = turns
        .iter()
        .any(|(_, content)| content.iter().any(is_thinking));
    let reply = chat["max_tokens"].as_u64();
    let mut body = json!({
        "model": chat["model"],
        "max_tokens": reply.or(chat["max_completion_tokens"].as_u64()).unwrap_or(REPLY_TOKENS),
        "messages": turns
            .into_iter()
            .map(|(role, content)| json!({"role": role, "content": content}))
            .collect::<Vec<_>>(),
    });
    if !system.is_empty() {
        body["system"] = system.into();
    }
    if let Some(stream) = chat.get("stream") {
        body["stream"] = stream.clone();
    }
    // The gateway's own web search is OpenRouter's, and has no tool here.
    let tools: Vec<Value> = (chat["tools"].as_array().into_iter().flatten())
        .filter_map(offered)
        .collect();
    if !tools.is_empty() {
        body["tools"] = tools.into();
    }
    // One field says both: no call at all for the wrap-up, which leaves
    // nothing to run side by side, or at most one call a reply.
    if chat["tool_choice"] == "none" {
        body["tool_choice"] = json!({"type": "none"});
    } else if chat["parallel_tool_calls"] == false {
        body["tool_choice"] = json!({"type": "auto", "disable_parallel_tool_use": true});
    }
    let effort = chat
        .get("reasoning_effort")
        .filter(|level| level.is_string());
    if let Some(level) = effort {
        body["output_config"] = json!({"effort": level});
    }
    if thinking.allowed && (effort.is_some() || replays) {
        body["thinking"] = json!({"type": "adaptive"});
        if thinking.bound {
            body["thinking"]["block_binding"] = json!({"prefix_mismatch_behavior": "drop_block"});
        }
    }
    body
}

/// Whether `posted`, a Messages request, sends thinking back.
pub(crate) fn replays(posted: &Value) -> bool {
    (posted["messages"].as_array().into_iter().flatten())
        .filter_map(|turn| turn["content"].as_array())
        .flatten()
        .any(is_thinking)
}

fn is_thinking(block: &Value) -> bool {
    matches!(
        block["type"].as_str(),
        Some("thinking" | "redacted_thinking")
    )
}

/// A chat message's text and pictures as content blocks, cache markers kept.
fn blocks(content: &Value) -> Vec<Value> {
    let parts = match content {
        Value::String(text) => vec![json!({"type": "text", "text": text})],
        Value::Array(parts) => parts.clone(),
        _ => Vec::new(),
    };
    parts
        .into_iter()
        .filter_map(|part| match part["type"].as_str()? {
            // Anthropic refuses a text block with nothing but spaces in it.
            "text"
                if part["text"]
                    .as_str()
                    .is_some_and(|text| !text.trim().is_empty()) =>
            {
                Some(part)
            }
            "image_url" => picture(part["image_url"]["url"].as_str()?),
            _ => None,
        })
        .collect()
}

/// The picture a data URL holds, as an image block.
fn picture(url: &str) -> Option<Value> {
    let (kind, data) = url.strip_prefix("data:")?.split_once(";base64,")?;
    Some(json!({"type": "image", "source": {"type": "base64", "media_type": kind, "data": data}}))
}

/// A tool's result as the block that answers its call.
fn result(message: &Value) -> Value {
    let content: Value = match &message["content"] {
        Value::String(text) => text.as_str().into(),
        parts => blocks(parts).into(),
    };
    json!({"type": "tool_result", "tool_use_id": call_id(&message["tool_call_id"]), "content": content})
}

/// A call's id as Anthropic takes it, which is letters, digits, `_` and `-`
/// alone: another route's, as `functions.read_file:0`, has `_` for the rest,
/// the same for the call and its result so the two still match.
fn call_id(id: &Value) -> Value {
    let id = id.as_str().unwrap_or_default();
    let allowed = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    id.replace(|c: char| !allowed(c), "_").into()
}

/// An assistant message's blocks: its text and calls, with the thinking it
/// came with put back at the place `Places` gave each block of it, unless
/// `thinking` is off.
fn said(message: &Value, thinking: bool) -> Vec<Value> {
    let mut rest = blocks(&message["content"]);
    for call in message["tool_calls"].as_array().into_iter().flatten() {
        let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
        // Anthropic takes the input as an object, and only an object.
        let input = serde_json::from_str::<Value>(arguments)
            .ok()
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        rest.push(json!({"type": "tool_use", "id": call_id(&call["id"]),
            "name": call["function"]["name"], "input": input}));
    }
    if !thinking {
        return rest;
    }
    let mut thought: Vec<(u64, Value)> = (message["reasoning_details"].as_array())
        .into_iter()
        .flatten()
        .filter(|block| is_thinking(block))
        .map(|block| {
            let mut block = block.clone();
            let at = (block.as_object_mut())
                .and_then(|fields| fields.remove("index"))
                .and_then(|at| at.as_u64())
                .unwrap_or(0);
            (at, block)
        })
        .collect();
    thought.sort_by_key(|(at, _)| *at);
    let mut rest = rest.into_iter();
    let mut content = Vec::new();
    for (at, block) in thought {
        while (content.len() as u64) < at {
            let Some(next) = rest.next() else {
                break;
            };
            content.push(next);
        }
        content.push(block);
    }
    content.extend(rest);
    content
}

/// Where each block of a reply goes among those `said` rebuilds it from, in
/// which all of its text is one block: text takes a place only where it
/// first comes, so a thinking block after several stays after the same calls.
#[derive(Default)]
struct Places {
    taken: u64,
    texted: bool,
}

impl Places {
    /// The place of the next call or thinking block.
    fn next(&mut self) -> u64 {
        self.taken += 1;
        self.taken - 1
    }

    /// Note `text`, which takes a place the first time it holds more than spaces.
    fn text(&mut self, text: &str) {
        if !self.texted && !text.trim().is_empty() {
            self.texted = true;
            self.taken += 1;
        }
    }
}

/// A chat completion's tool as Messages offers it, its cache marker kept.
fn offered(tool: &Value) -> Option<Value> {
    let function = tool.get("function")?;
    let schema = (function.get("parameters").cloned()).unwrap_or_else(|| json!({"type": "object"}));
    let mut offered = json!({"name": function["name"], "input_schema": schema});
    if let Some(description) = function.get("description") {
        offered["description"] = description.clone();
    }
    if let Some(marker) = tool.get("cache_control") {
        offered["cache_control"] = marker.clone();
    }
    Some(offered)
}

/// A reply that was not streamed, as a chat completion.
pub(crate) fn reply(message: &Value) -> Value {
    // An error has the envelope a chat completion's error has.
    if message.get("error").is_some() {
        return message.clone();
    }
    let (mut text, mut reasoning) = (String::new(), String::new());
    let (mut calls, mut thought) = (Vec::new(), Vec::new());
    let mut places = Places::default();
    for block in message["content"].as_array().into_iter().flatten() {
        match block["type"].as_str().unwrap_or_default() {
            "text" => {
                let piece = block["text"].as_str().unwrap_or_default();
                places.text(piece);
                text.push_str(piece);
            }
            "tool_use" => {
                places.next();
                calls.push(json!({"index": calls.len(), "id": block["id"],
                    "type": "function", "function": {"name": block["name"],
                    "arguments": block["input"].to_string()}}));
            }
            "thinking" | "redacted_thinking" => {
                reasoning.push_str(block["thinking"].as_str().unwrap_or_default());
                let mut block = block.clone();
                block["index"] = places.next().into();
                thought.push(block);
            }
            _ => {}
        }
    }
    let mut counts = Counts::default();
    json!({
        "choices": [{
            "message": {"role": "assistant", "content": text, "reasoning": reasoning,
                "tool_calls": calls, "reasoning_details": thought},
            "finish_reason": finish(message["stop_reason"].as_str().unwrap_or_default()),
        }],
        "usage": counts.read(&message["usage"]),
    })
}

/// A stop reason under the name a chat completion gives it, where it has one.
fn finish(stop: &str) -> &str {
    match stop {
        // The reply ran into the window rather than its own cap, and is as cut off.
        "model_context_window_exceeded" => "length",
        stop => stop,
    }
}

/// The input a reply's usage has counted so far, which a stream gives at its
/// start and may give again at its end.
#[derive(Default)]
struct Counts {
    input: u64,
    cache_read: u64,
    cache_write: u64,
}

impl Counts {
    /// Take in Anthropic's `usage`, giving it back as a chat completion's.
    fn read(&mut self, usage: &Value) -> Value {
        let fields = [
            (&mut self.input, "input_tokens"),
            (&mut self.cache_read, "cache_read_input_tokens"),
            (&mut self.cache_write, "cache_creation_input_tokens"),
        ];
        for (count, field) in fields {
            if let Some(tokens) = usage[field].as_u64() {
                *count = tokens;
            }
        }
        // Anthropic counts cache hits and writes beside the input, where
        // OpenAI counts them inside it, as the prompt the rest is read by.
        let prompt = (self.input)
            .saturating_add(self.cache_read)
            .saturating_add(self.cache_write);
        let mut counted = json!({
            "prompt_tokens": prompt,
            "cache_read_input_tokens": self.cache_read,
            "cache_creation_input_tokens": self.cache_write,
        });
        if let Some(tokens) = usage["output_tokens"].as_u64() {
            counted["completion_tokens"] = tokens.into();
        }
        counted
    }
}

/// What one event of a Messages stream comes to.
pub(crate) enum Said {
    /// The chunk a chat completion would have streamed in its place.
    Chunk(Value),
    /// Nothing to pass on yet, such as a ping or a signature.
    Nothing,
    /// The reply is over.
    Over,
}

/// A Messages stream as it arrives, keeping what one event cannot say alone:
/// the input counted at its start, and each thinking block so far.
#[derive(Default)]
pub(crate) struct Stream {
    counts: Counts,
    /// Thinking blocks still streaming, by their index in the stream, each
    /// with its place among the blocks it goes back with.
    thinking: HashMap<u64, Value>,
    places: Places,
}

impl Stream {
    /// Take in one event's data.
    pub(crate) fn event(&mut self, event: &Value) -> Said {
        let at = event["index"].as_u64();
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                Said::Chunk(json!({"usage": self.counts.read(&event["message"]["usage"])}))
            }
            "content_block_start" => {
                at.map_or(Said::Nothing, |at| self.begin(at, &event["content_block"]))
            }
            "content_block_delta" => at.map_or(Said::Nothing, |at| self.more(at, &event["delta"])),
            "content_block_stop" => at.map_or(Said::Nothing, |at| self.end(at)),
            "message_delta" => {
                let stop = event["delta"]["stop_reason"].as_str().map(finish);
                Said::Chunk(json!({
                    "usage": self.counts.read(&event["usage"]),
                    "choices": [{"delta": {}, "finish_reason": stop}],
                }))
            }
            "message_stop" => Said::Over,
            // An error has the envelope a chat completion's error has.
            "error" => Said::Chunk(event.clone()),
            _ => Said::Nothing,
        }
    }

    /// A block beginning at `at`.
    fn begin(&mut self, at: u64, block: &Value) -> Said {
        match block["type"].as_str().unwrap_or_default() {
            "tool_use" => {
                self.places.next();
                Said::Chunk(call(at, &block["id"], &block["name"], ""))
            }
            "thinking" | "redacted_thinking" => {
                let mut block = block.clone();
                block["index"] = self.places.next().into();
                self.thinking.insert(at, block);
                Said::Nothing
            }
            "text" => match block["text"].as_str().filter(|text| !text.is_empty()) {
                Some(text) => {
                    self.places.text(text);
                    Said::Chunk(delta(json!({"content": text})))
                }
                None => Said::Nothing,
            },
            _ => Said::Nothing,
        }
    }

    /// More of the block at `at`.
    fn more(&mut self, at: u64, more: &Value) -> Said {
        let piece = |field: &str| more[field].as_str().unwrap_or_default().to_owned();
        match more["type"].as_str().unwrap_or_default() {
            "text_delta" => {
                self.places.text(&piece("text"));
                Said::Chunk(delta(json!({"content": piece("text")})))
            }
            "input_json_delta" => {
                Said::Chunk(call(at, &Value::Null, &Value::Null, &piece("partial_json")))
            }
            "thinking_delta" => {
                self.add(at, "thinking", &piece("thinking"));
                Said::Chunk(delta(json!({"reasoning": piece("thinking")})))
            }
            "signature_delta" => {
                self.add(at, "signature", &piece("signature"));
                Said::Nothing
            }
            _ => Said::Nothing,
        }
    }

    /// Add `piece` to `field` of the thinking block at `at`.
    fn add(&mut self, at: u64, field: &str, piece: &str) {
        let Some(block) = self.thinking.get_mut(&at) else {
            return;
        };
        let so_far = block[field].as_str().unwrap_or_default();
        block[field] = format!("{so_far}{piece}").into();
    }

    /// The end of the block at `at`: a thinking block is kept whole, with
    /// its place among the reply's, to be sent back there.
    fn end(&mut self, at: u64) -> Said {
        let Some(block) = self.thinking.remove(&at) else {
            return Said::Nothing;
        };
        Said::Chunk(delta(json!({"reasoning_details": [block]})))
    }
}

/// A chat completion chunk whose delta is `delta`.
fn delta(delta: Value) -> Value {
    json!({"choices": [{"delta": delta}]})
}

/// A chunk carrying a piece of the call that is block `at`.
fn call(at: u64, id: &Value, name: &Value, arguments: &str) -> Value {
    let id = id.as_str().unwrap_or_default();
    let name = name.as_str().unwrap_or_default();
    delta(json!({"tool_calls": [{"index": at, "id": id,
        "function": {"name": name, "arguments": arguments}}]}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::{Connection, Event, Message, Replay, ToolCall, start};
    use std::time::{Duration, Instant};

    type Asked = std::thread::JoinHandle<Vec<(String, String)>>;

    /// Thinking on a route that has turned down neither it nor the binding.
    const ASKED: Thinking = Thinking {
        allowed: true,
        bound: true,
    };

    /// A local server at `path` answering each request with the next of
    /// `replies`, given as status, content type and body; it hands back each
    /// request's head, in lower case, and body.
    fn server(path: &str, replies: Vec<(&'static str, &'static str, String)>) -> (String, Asked) {
        use std::io::Write;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}{path}", server.local_addr().unwrap());
        // Polled, so a request that never comes fails the test instead of
        // hanging it.
        server.set_nonblocking(true).unwrap();
        let worker = std::thread::spawn(move || {
            let mut asked = Vec::new();
            for (status, kind, reply) in replies {
                let deadline = Instant::now() + Duration::from_secs(20);
                let mut socket = loop {
                    match server.accept() {
                        Ok((socket, _)) => break socket,
                        Err(_) if Instant::now() < deadline => {
                            std::thread::sleep(Duration::from_millis(10));
                        }
                        Err(e) => panic!("request {} never came: {e}", asked.len() + 1),
                    }
                };
                socket.set_nonblocking(false).unwrap();
                socket
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                asked.push(heard(&mut socket));
                write!(socket, "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            asked
        });
        (endpoint, worker)
    }

    /// `server`, standing in for Anthropic's own API at a path no other
    /// test's server has, so another that later gets the same port is not
    /// taken for it.
    fn stand_in(replies: Vec<(&'static str, &'static str, String)>) -> (String, Asked) {
        let (endpoint, worker) = server("/anthropic/v1", replies);
        STAND_INS.lock().unwrap().push(endpoint.clone());
        (endpoint, worker)
    }

    /// One request's head, in lower case, and body, as `socket` brings them.
    fn heard(socket: &mut std::net::TcpStream) -> (String, String) {
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
            let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
            let length: usize = (head.lines())
                .find_map(|line| line.strip_prefix("content-length: "))
                .map_or(0, |n| n.parse().unwrap());
            if bytes.len() >= end + 4 + length {
                let body = String::from_utf8_lossy(&bytes[end + 4..end + 4 + length]);
                return (head, body.into_owned());
            }
        }
    }

    /// `events` as a Messages stream sends them, each named on its own line.
    fn sse(events: &[Value]) -> String {
        (events.iter())
            .map(|event| {
                format!(
                    "event: {}\ndata: {event}\n\n",
                    event["type"].as_str().unwrap()
                )
            })
            .collect()
    }

    fn message_start() -> Value {
        json!({"type": "message_start", "message": {"id": "msg_01", "type": "message",
            "role": "assistant", "content": [], "model": "claude-opus-5-5", "stop_reason": null,
            "usage": {"input_tokens": 12, "cache_creation_input_tokens": 500,
                "cache_read_input_tokens": 3000, "output_tokens": 1}}})
    }

    /// A reply that thinks, says a line and calls a tool, then thinks again
    /// out of sight and calls another, as Anthropic streams it.
    fn thinks_and_calls() -> String {
        sse(&[
            message_start(),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "thinking", "thinking": "", "signature": ""}}),
            json!({"type": "ping"}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "The file "}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "thinking_delta", "thinking": "comes first."}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "signature_delta", "signature": "EqQBCkgIARAB"}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "content_block_start", "index": 1,
                "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 1,
                "delta": {"type": "text_delta", "text": "Reading it."}}),
            json!({"type": "content_block_stop", "index": 1}),
            json!({"type": "content_block_start", "index": 2, "content_block":
                {"type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {}}}),
            json!({"type": "content_block_delta", "index": 2,
                "delta": {"type": "input_json_delta", "partial_json": "{\"path\": "}}),
            json!({"type": "content_block_delta", "index": 2,
                "delta": {"type": "input_json_delta", "partial_json": "\"a.rs\"}"}}),
            json!({"type": "content_block_stop", "index": 2}),
            json!({"type": "content_block_start", "index": 3,
                "content_block": {"type": "redacted_thinking", "data": "UmVkYWN0ZWQ="}}),
            json!({"type": "content_block_stop", "index": 3}),
            json!({"type": "content_block_start", "index": 4, "content_block":
                {"type": "tool_use", "id": "toolu_02", "name": "list_dir", "input": {}}}),
            json!({"type": "content_block_delta", "index": 4,
                "delta": {"type": "input_json_delta", "partial_json": "{\"path\": \".\"}"}}),
            json!({"type": "content_block_stop", "index": 4}),
            json!({"type": "message_delta", "delta": {"stop_reason": "tool_use",
                "stop_sequence": null}, "usage": {"output_tokens": 87}}),
            json!({"type": "message_stop"}),
        ])
    }

    /// The same reply as a chat completion streams it.
    fn thinks_and_calls_as_chat() -> String {
        let chunks = [
            json!({"choices": [{"delta": {"role": "assistant", "reasoning": "The file "}}]}),
            json!({"choices": [{"delta": {"reasoning": "comes first."}}]}),
            json!({"choices": [{"delta": {"content": "Reading it."}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0, "id": "toolu_01",
                "function": {"name": "read_file", "arguments": "{\"path\": "}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 0,
                "function": {"arguments": "\"a.rs\"}"}}]}}]}),
            json!({"choices": [{"delta": {"tool_calls": [{"index": 1, "id": "toolu_02",
                "function": {"name": "list_dir", "arguments": "{\"path\": \".\"}"}}]}}]}),
            json!({"choices": [{"delta": {}, "finish_reason": "tool_calls"}],
                "usage": {"prompt_tokens": 3512, "completion_tokens": 87,
                    "prompt_tokens_details": {"cached_tokens": 3000, "cache_write_tokens": 500}}}),
        ];
        let data: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
        format!("{data}data: [DONE]\n\n")
    }

    /// A reply of `text` alone, as Anthropic streams it.
    fn says(text: &str) -> String {
        sse(&[
            message_start(),
            json!({"type": "content_block_start", "index": 0,
                "content_block": {"type": "text", "text": ""}}),
            json!({"type": "content_block_delta", "index": 0,
                "delta": {"type": "text_delta", "text": text}}),
            json!({"type": "content_block_stop", "index": 0}),
            json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
                "usage": {"output_tokens": 5}}),
            json!({"type": "message_stop"}),
        ])
    }

    /// A connection to `endpoint` in a conversation of its own, so no other
    /// test's cache misses come into it.
    fn on(endpoint: &str, conversation: u64) -> Connection {
        Connection {
            endpoint: endpoint.into(),
            model: "claude-opus-5-5".into(),
            conversation,
            ..Default::default()
        }
    }

    fn said(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: text.into(),
            ..Default::default()
        }
    }

    fn calling(id: &str, name: &str, arguments: &str) -> ToolCall {
        ToolCall {
            id: id.into(),
            name: name.into(),
            arguments: arguments.into(),
        }
    }

    fn answered(id: &str, name: &str, text: &str) -> Message {
        Message {
            tool_call_id: id.into(),
            name: name.into(),
            ..said("tool", text)
        }
    }

    /// What a reply passed on, event by event, leaving out what differs by
    /// wire or by run: drafts, cache misses and speed; and what it wants back.
    fn heard_from(events: async_channel::Receiver<Event>) -> (Vec<String>, Option<Replay>) {
        let (mut heard, mut replay) = (Vec::new(), None);
        while let Ok(event) = events.recv_blocking() {
            let line = match event {
                Event::Delta(text) => format!("text {text}"),
                Event::Reasoning(text) => format!("thinking {text}"),
                Event::Usage {
                    completion,
                    prompt,
                    cached,
                    cache_write,
                    ..
                } => format!(
                    "usage {prompt} in, {cached} read, {cache_write} written, {completion} out"
                ),
                Event::ToolCalls(calls) => format!("calls {calls:?}"),
                Event::Retrying(retry) => format!("retry {:?}", retry.kind),
                Event::Replay(kept) => {
                    replay = Some(kept);
                    continue;
                }
                Event::Finished(result) => {
                    heard.push(format!("finished {:?}", result.map_err(|f| f.to_string())));
                    break;
                }
                _ => continue,
            };
            heard.push(line);
        }
        (heard, replay)
    }

    #[test]
    fn recorded_messages_sse_gives_the_same_chunks_and_usage() {
        let (endpoint, worker) =
            stand_in(vec![("200 OK", "text/event-stream", thinks_and_calls())]);
        let (chat, chat_worker) = server(
            "/v1",
            vec![("200 OK", "text/event-stream", thinks_and_calls_as_chat())],
        );
        let ask = vec![said("user", "Look at a.rs.")];
        let (messages, replay) = heard_from(start(on(&endpoint, 0xA401), ask.clone()));
        let (completions, _) = heard_from(start(on(&chat, 0xA402), ask));
        assert_eq!(messages, completions);
        assert!(
            messages.contains(&"usage 3512 in, 3000 read, 500 written, 87 out".to_owned()),
            "{messages:?}"
        );
        assert_eq!(messages.last().unwrap(), "finished Ok(())");
        // The thinking is kept whole, with where each block came.
        let kept = replay.unwrap().reasoning_details;
        assert_eq!(
            kept,
            [
                json!({"type": "thinking", "thinking": "The file comes first.",
                    "signature": "EqQBCkgIARAB", "index": 0}),
                json!({"type": "redacted_thinking", "data": "UmVkYWN0ZWQ=", "index": 3}),
            ]
        );
        let (head, body) = &worker.join().unwrap()[0];
        assert!(head.starts_with("post /anthropic/v1/messages "), "{head}");
        assert!(head.contains("anthropic-version: 2023-06-01"), "{head}");
        // Nothing asks a model to think before it has, without an effort.
        assert!(!head.contains("anthropic-beta"), "{head}");
        let body: Value = serde_json::from_str(body).unwrap();
        assert!(body.get("thinking").is_none() && body.get("stream_options").is_none());
        assert_eq!(body["stream"], true);
        assert_eq!(
            body["messages"],
            json!([{"role": "user", "content": [{"type": "text", "text": "Look at a.rs.",
                "cache_control": {"type": "ephemeral"}}]}])
        );
        chat_worker.join().unwrap();
    }

    #[test]
    fn a_second_round_replays_thinking_in_order_with_markers() {
        let (endpoint, worker) = stand_in(vec![
            ("200 OK", "text/event-stream", thinks_and_calls()),
            ("200 OK", "text/event-stream", says("Done.")),
        ]);
        let c = on(&endpoint, 0xA403);
        let mut messages = vec![said("user", "Look at a.rs.")];
        let (_, replay) = heard_from(start(c.clone(), messages.clone()));
        messages.push(Message {
            model: c.model.clone(),
            tool_calls: vec![
                calling("toolu_01", "read_file", r#"{"path": "a.rs"}"#),
                calling("toolu_02", "list_dir", r#"{"path": "."}"#),
            ],
            replay: replay.map(Box::new),
            ..said("assistant", "Reading it.")
        });
        messages.push(answered("toolu_01", "read_file", "fn main() {}"));
        messages.push(answered("toolu_02", "list_dir", "a.rs"));
        let (heard, _) = heard_from(start(c, messages));
        assert_eq!(heard.last().unwrap(), "finished Ok(())");
        let (head, body) = &worker.join().unwrap()[1];
        let body: Value = serde_json::from_str(body).unwrap();
        let turns = body["messages"].as_array().unwrap();
        assert_eq!(turns.len(), 3);
        let reply = turns[1]["content"].as_array().unwrap();
        let kinds: Vec<&str> = reply.iter().map(|b| b["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "thinking",
                "text",
                "tool_use",
                "redacted_thinking",
                "tool_use"
            ]
        );
        // Back as they came, without the place they were kept with.
        assert_eq!(
            reply[0],
            json!({"type": "thinking", "thinking": "The file comes first.",
                "signature": "EqQBCkgIARAB"})
        );
        assert_eq!(
            reply[3],
            json!({"type": "redacted_thinking", "data": "UmVkYWN0ZWQ="})
        );
        assert_eq!(reply[2]["input"], json!({"path": "a.rs"}));
        // Both results in the one turn after the calls, and as the newest two
        // messages, each marked; with the system prompt, three markers.
        let results = turns[2]["content"].as_array().unwrap();
        let ids: Vec<&str> = (results.iter())
            .map(|r| r["tool_use_id"].as_str().unwrap())
            .collect();
        assert_eq!(ids, ["toolu_01", "toolu_02"]);
        let marker = json!({"type": "ephemeral"});
        for result in results {
            assert_eq!(result["content"][0]["cache_control"], marker);
        }
        let system = body["system"].as_array().unwrap();
        assert_eq!(system.last().unwrap()["cache_control"], marker);
        assert_eq!(body.to_string().matches("cache_control").count(), 3);
        assert_eq!(
            body["thinking"],
            json!({"type": "adaptive",
                "block_binding": {"prefix_mismatch_behavior": "drop_block"}})
        );
        assert!(
            head.contains("anthropic-beta: thinking-binding-controls-2026-08-01"),
            "{head}"
        );
    }

    #[test]
    fn a_request_after_the_plan_note_moved_carries_drop_block() {
        let (endpoint, worker) = stand_in(vec![
            ("200 OK", "text/event-stream", says("Reading it.")),
            ("200 OK", "text/event-stream", says("Done.")),
        ]);
        let c = on(&endpoint, 0xA404);
        let thought = Replay {
            endpoint: endpoint.clone(),
            model: c.model.clone(),
            reasoning_details: vec![json!({"type": "thinking", "thinking": "A plan first.",
                "signature": "c2lnbmVk", "index": 0})],
            extra_content: Default::default(),
            reasoning_items: Default::default(),
        };
        let todo = r#"{"items": [{"text": "Read a.rs", "status": "doing"}]}"#;
        let mut messages = vec![
            said("user", "Plan, then read a.rs."),
            Message {
                model: c.model.clone(),
                tool_calls: vec![calling("toolu_01", crate::tools::TODO, todo)],
                replay: Some(Box::new(thought)),
                ..said("assistant", "")
            },
            answered("toolu_01", crate::tools::TODO, "Checklist set."),
        ];
        heard_from(start(c.clone(), messages.clone()));
        messages.push(Message {
            model: c.model.clone(),
            tool_calls: vec![calling("toolu_02", "read_file", r#"{"path": "a.rs"}"#)],
            ..said("assistant", "")
        });
        messages.push(answered("toolu_02", "read_file", "fn main() {}"));
        heard_from(start(c, messages));
        let asked = worker.join().unwrap();
        let sent = |at: usize| serde_json::from_str::<Value>(&asked[at].1).unwrap();
        let (first, second) = (sent(0), sent(1));
        let note = "[>] Read a.rs";
        // The note rode on the todo's result, then moved on to the newest:
        // the history the thinking was signed over is not what it was.
        assert!(first["messages"][2].to_string().contains(note), "{first}");
        assert!(
            !second["messages"][2].to_string().contains(note),
            "{second}"
        );
        assert!(second["messages"][4].to_string().contains(note), "{second}");
        assert_eq!(second["messages"][1]["content"][0]["signature"], "c2lnbmVk");
        assert_eq!(
            second["thinking"]["block_binding"],
            json!({"prefix_mismatch_behavior": "drop_block"})
        );
        assert!(
            asked[1]
                .0
                .contains("anthropic-beta: thinking-binding-controls-2026-08-01"),
            "{}",
            asked[1].0
        );
    }

    #[test]
    fn an_overloaded_reply_is_retried() {
        let overloaded = json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}});
        let (endpoint, worker) = stand_in(vec![
            ("529 Overloaded", "application/json", overloaded.to_string()),
            // Overloaded again, inside a stream that began with 200.
            (
                "200 OK",
                "text/event-stream",
                sse(&[message_start(), overloaded]),
            ),
            ("200 OK", "text/event-stream", says("Hello")),
        ]);
        let ask = vec![said("user", "Hi")];
        let (heard, _) = heard_from(start(on(&endpoint, 0xA405), ask));
        assert_eq!(heard[..2], ["retry Overloaded", "retry Overloaded"]);
        assert!(heard.contains(&"text Hello".to_owned()), "{heard:?}");
        assert_eq!(heard.last().unwrap(), "finished Ok(())");
        assert_eq!(worker.join().unwrap().len(), 3);
    }

    #[test]
    fn prompt_tokens_include_cache_reads_and_writes() {
        let usage = json!({"input_tokens": 12, "cache_creation_input_tokens": 500,
            "cache_read_input_tokens": 3000, "output_tokens": 87});
        // A reply that was not streamed, as a summary's is.
        let whole = reply(&json!({"type": "message", "role": "assistant",
            "content": [{"type": "text", "text": "Summed up."}],
            "stop_reason": "end_turn", "usage": usage}));
        let counted = crate::usage::Usage::read(&whole["usage"]);
        assert_eq!(
            (counted.input, counted.cached, counted.cache_write),
            (3512, 3000, 500)
        );
        assert_eq!(counted.output, 87);
        // A stream counts the input at its start and the output at its end.
        let mut stream = Stream::default();
        let Said::Chunk(begun) = stream.event(&message_start()) else {
            panic!("message_start said nothing");
        };
        assert_eq!(begun["usage"]["prompt_tokens"], 3512);
        let end = json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"},
            "usage": {"output_tokens": 87}});
        let Said::Chunk(ended) = stream.event(&end) else {
            panic!("message_delta said nothing");
        };
        assert_eq!(
            ended["usage"],
            json!({"prompt_tokens": 3512, "completion_tokens": 87,
                "cache_read_input_tokens": 3000, "cache_creation_input_tokens": 500})
        );
    }

    #[test]
    fn the_model_list_is_asked_for_with_the_key_and_whole() {
        let list = json!({"data": [{"type": "model", "id": "claude-listed-5",
            "display_name": "Claude Listed 5", "created_at": "2026-09-01T00:00:00Z",
            "max_input_tokens": 900_000, "max_tokens": 64_000}],
            "has_more": false, "first_id": "claude-listed-5", "last_id": "claude-listed-5"});
        let (endpoint, worker) = stand_in(vec![("200 OK", "application/json", list.to_string())]);
        let models = crate::chat::list_models(&endpoint, "sk-ant-test").unwrap();
        assert_eq!(models, ["claude-listed-5"]);
        let (head, _) = &worker.join().unwrap()[0];
        assert!(
            head.starts_with("get /anthropic/v1/models?limit=1000 "),
            "{head}"
        );
        assert!(head.contains("x-api-key: sk-ant-test"), "{head}");
        assert!(head.contains("anthropic-version: 2023-06-01"), "{head}");
        assert!(!head.contains("authorization"), "{head}");
        let facts = crate::models::facts(&endpoint, "claude-listed-5");
        assert_eq!(
            (facts.context, facts.max_output),
            (Some(900_000), Some(64_000))
        );
    }

    #[test]
    fn a_summary_on_anthropic_is_asked_for_as_a_message() {
        let summary = json!({"id": "msg_02", "type": "message", "role": "assistant",
            "model": "claude-opus-5-5", "content": [{"type": "text", "text": "They asked about a.rs."}],
            "stop_reason": "end_turn", "usage": {"input_tokens": 40,
                "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0, "output_tokens": 9}});
        let (endpoint, worker) =
            stand_in(vec![("200 OK", "application/json", summary.to_string())]);
        let covered = [
            said("user", "What is in a.rs, summed up on Anthropic?"),
            said("assistant", "A main function."),
        ];
        let c = on(&endpoint, 0xA406);
        let (text, usage) = crate::compact::manual(&c, &covered, 10_000).unwrap();
        assert_eq!(text, "They asked about a.rs.");
        assert_eq!(
            usage.map(|u| (u.usage.input, u.usage.output)),
            Some((40, 9))
        );
        let (head, body) = &worker.join().unwrap()[0];
        assert!(head.starts_with("post /anthropic/v1/messages "), "{head}");
        let body: Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["stream"], false);
        assert_eq!(body["system"][0]["type"], "text");
        assert_eq!(body["messages"][0]["role"], "user");
        assert!(body.get("thinking").is_none());
    }

    #[test]
    fn parallel_calls_off_asks_for_one_call_a_reply() {
        let tool = json!({"type": "function", "function": {"name": "read_file",
            "parameters": {"type": "object"}}});
        let ask = |extra: Value| {
            let mut chat = json!({"model": "claude-opus-5", "tools": [tool],
                "messages": [{"role": "user", "content": "Read a.rs."}]});
            for (field, value) in extra.as_object().unwrap() {
                chat[field] = value.clone();
            }
            request(&chat, ASKED)
        };
        assert!(ask(json!({})).get("tool_choice").is_none());
        let one = json!({"type": "auto", "disable_parallel_tool_use": true});
        assert_eq!(
            ask(json!({"parallel_tool_calls": false}))["tool_choice"],
            one
        );
        // The wrap-up offers no call at all, so there is none to make in parallel.
        let wrap_up = ask(json!({"parallel_tool_calls": false, "tool_choice": "none"}));
        assert_eq!(wrap_up["tool_choice"], json!({"type": "none"}));
        assert!(
            ask(json!({"parallel_tool_calls": true}))
                .get("tool_choice")
                .is_none()
        );
    }

    #[test]
    fn another_routes_call_ids_go_out_as_anthropic_takes_them() {
        let chat = json!({"model": "claude-opus-5", "messages": [
            {"role": "user", "content": "Read a.rs."},
            {"role": "assistant", "content": "", "tool_calls": [{"id": "functions.read_file:0",
                "type": "function", "function": {"name": "read_file",
                "arguments": "{\"path\": \"a.rs\"}"}}]},
            {"role": "tool", "tool_call_id": "functions.read_file:0", "content": "fn main() {}"},
            {"role": "assistant", "content": "", "tool_calls": [{"id": "toolu_01-a",
                "type": "function", "function": {"name": "list_dir", "arguments": "{}"}}]},
            {"role": "tool", "tool_call_id": "toolu_01-a", "content": "a.rs"},
        ]});
        let posted = request(&chat, ASKED);
        let turns = posted["messages"].as_array().unwrap();
        assert_eq!(turns[1]["content"][0]["id"], "functions_read_file_0");
        assert_eq!(
            turns[2]["content"][0]["tool_use_id"],
            "functions_read_file_0"
        );
        // Anthropic's own go out as they came.
        assert_eq!(turns[3]["content"][0]["id"], "toolu_01-a");
        assert_eq!(turns[4]["content"][0]["tool_use_id"], "toolu_01-a");
    }

    #[test]
    fn thinking_after_several_texts_goes_back_after_the_same_calls() {
        let thinking =
            |text: &str| json!({"type": "thinking", "thinking": text, "signature": "c2ln"});
        let text = |text: &str| json!({"type": "text", "text": text});
        let calling = |id: &str| {
            json!({"type": "tool_use", "id": id, "name": "read_file",
            "input": {"path": "a.rs"}})
        };
        let content = [
            thinking("First a.rs."),
            text("Reading a.rs."),
            calling("toolu_01"),
            text(" And b.rs."),
            calling("toolu_02"),
            thinking("Then c.rs."),
            calling("toolu_03"),
        ];
        let whole = reply(&json!({"content": content, "stop_reason": "tool_use", "usage": {}}));
        let message = &whole["choices"][0]["message"];
        // Streamed, the blocks are kept with the same places.
        let mut stream = Stream::default();
        let mut streamed = Vec::new();
        for (at, block) in content.iter().enumerate() {
            let mut start = block.clone();
            for field in ["thinking", "text", "signature"] {
                if start.get(field).is_some() {
                    start[field] = "".into();
                }
            }
            stream.event(
                &json!({"type": "content_block_start", "index": at, "content_block": start}),
            );
            let more = match block["type"].as_str().unwrap() {
                "thinking" => vec![
                    json!({"type": "thinking_delta", "thinking": block["thinking"]}),
                    json!({"type": "signature_delta", "signature": "c2ln"}),
                ],
                "text" => vec![json!({"type": "text_delta", "text": block["text"]})],
                _ => Vec::new(),
            };
            for delta in more {
                stream.event(&json!({"type": "content_block_delta", "index": at, "delta": delta}));
            }
            if let Said::Chunk(chunk) =
                stream.event(&json!({"type": "content_block_stop", "index": at}))
            {
                streamed.extend_from_slice(
                    chunk["choices"][0]["delta"]["reasoning_details"]
                        .as_array()
                        .unwrap(),
                );
            }
        }
        assert_eq!(&Value::from(streamed), &message["reasoning_details"]);
        let mut said = message.clone();
        said["role"] = "assistant".into();
        let chat = json!({"model": "claude-opus-5", "messages": [
            {"role": "user", "content": "Read a.rs and b.rs."}, said]});
        let posted = request(&chat, ASKED);
        let sent: Vec<String> = (posted["messages"][1]["content"].as_array().unwrap().iter())
            .map(|block| match block["type"].as_str().unwrap() {
                "thinking" => format!("thinking {}", block["thinking"].as_str().unwrap()),
                "tool_use" => format!("call {}", block["id"].as_str().unwrap()),
                kind => kind.to_owned(),
            })
            .collect();
        assert_eq!(
            sent,
            [
                "thinking First a.rs.",
                "text",
                "call toolu_01",
                "call toolu_02",
                "thinking Then c.rs.",
                "call toolu_03"
            ]
        );
    }
}
