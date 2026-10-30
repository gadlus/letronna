//! Google's own format, `generateContent`, for the models a gateway serves
//! in it alone, as OpenCode serves Gemini.
//!
//! Google's API has a copy of OpenAI's chat completions, which the Gemini
//! gateway speaks, thought signatures and all. A gateway that passes Gemini
//! on as Google serves it takes only Google's own format, so there a request
//! is still built chat-shaped, as every request is, and turned into a
//! `generateContent` one on its way out; each response that streams back is
//! turned into the chat chunk it stands for.
//!
//! Gemini signs the thinking behind its tool calls and refuses a call of the
//! turn in progress sent back without its signature. The signature rides on
//! the call as `extra_content.google.thought_signature`, where Google's copy
//! of chat completions puts it, so it is kept and given back as it is there.

use reqwest::blocking::RequestBuilder;
use serde_json::{Map, Value, json};
use std::collections::HashMap;

/// What a call goes back with when its signature was not kept, as one made
/// by another model: Google's own word for a call it should not check.
const UNSIGNED: &str = "skip_thought_signature_validator";

/// Whether `endpoint` speaks Google's format itself: a base ending in
/// `/v1beta`, as Google's API and the gateways that pass it on name theirs.
/// Google's copy of chat completions hangs off `/v1beta/openai` instead.
pub(crate) fn speaks(endpoint: &str) -> bool {
    reqwest::Url::parse(endpoint.trim())
        .is_ok_and(|url| url.path().trim_end_matches('/').ends_with("/v1beta"))
}

/// Where a request for `model` goes, given the chat/completions URL
/// `chat::validate` made: the model's own route, streamed or whole.
pub(crate) fn url(chat: &str, model: &str, stream: bool) -> String {
    let base = chat.strip_suffix("/chat/completions").unwrap_or(chat);
    let model = model.trim().trim_start_matches("models/");
    match stream {
        true => format!("{base}/models/{model}:streamGenerateContent?alt=sse"),
        false => format!("{base}/models/{model}:generateContent"),
    }
}

/// `request` carrying `key` as Google takes it: it reads no bearer.
pub(crate) fn keyed(request: RequestBuilder, key: &str) -> RequestBuilder {
    match key.trim() {
        "" => request,
        key => request.header("x-goog-api-key", key),
    }
}

/// Google's list of models as a `/models` list's entries: the models that
/// answer `generateContent`, by their bare ids, each with its window and
/// cap under the names the model facts read.
pub(crate) fn listed(list: &Value) -> Vec<Value> {
    let models = list["models"].as_array().into_iter().flatten();
    let answers = |model: &&Value| {
        let methods = model["supportedGenerationMethods"].as_array();
        methods.is_none_or(|methods| methods.iter().any(|m| m == "generateContent"))
    };
    models
        .filter(answers)
        .filter_map(|model| {
            let id = model["name"].as_str()?.trim_start_matches("models/");
            Some(json!({
                "id": id,
                "context_length": model["inputTokenLimit"],
                "max_completion_tokens": model["outputTokenLimit"],
            }))
        })
        .collect()
}

/// The `generateContent` request for the chat-shaped `chat`.
pub(crate) fn request(chat: &Value) -> Value {
    let mut system = Vec::new();
    let mut contents = Vec::new();
    // Each call's name by its id: a result names the function it answers.
    let mut named = HashMap::new();
    for message in chat["messages"].as_array().into_iter().flatten() {
        match message["role"].as_str().unwrap_or_default() {
            "system" | "developer" if contents.is_empty() => {
                let said = text(&message["content"]);
                if !said.trim().is_empty() {
                    system.push(said);
                }
            }
            "assistant" => add(&mut contents, "model", replied(message, &mut named)),
            "tool" => add(&mut contents, "user", vec![answer(message, &named)]),
            // The user's words and pictures, and a note the system makes
            // later on, which Google has no place for but the user's turn.
            _ => add(&mut contents, "user", parts(&message["content"])),
        }
    }
    for turn in &mut contents {
        answers_first(turn);
    }
    let mut body = json!({"contents": contents});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": system.join("\n\n")}]});
    }
    let tools = chat["tools"].as_array().into_iter().flatten();
    let declared: Vec<Value> = tools.filter_map(declared).collect();
    if !declared.is_empty() {
        body["tools"] = json!([{"functionDeclarations": declared}]);
        if let Some(mode) = choice(&chat["tool_choice"]) {
            body["toolConfig"] = json!({"functionCallingConfig": mode});
        }
    }
    let config = generation(chat);
    if !config.is_empty() {
        body["generationConfig"] = config.into();
    }
    body
}

/// What the chat body says of how to write the reply, as Google names it.
fn generation(chat: &Value) -> Map<String, Value> {
    let mut config = Map::new();
    // Both caps count the thinking as well as the answer on these models.
    let cap = (chat.get("max_completion_tokens"))
        .or_else(|| chat.get("max_tokens"))
        .filter(|cap| cap.is_u64());
    if let Some(cap) = cap {
        config.insert("maxOutputTokens".into(), cap.clone());
    }
    for (field, name) in [
        ("temperature", "temperature"),
        ("top_p", "topP"),
        ("seed", "seed"),
    ] {
        if let Some(value) = chat.get(field).filter(|value| value.is_number()) {
            config.insert(name.into(), value.clone());
        }
    }
    match &chat["stop"] {
        Value::String(stop) => {
            config.insert("stopSequences".into(), json!([stop]));
        }
        Value::Array(stops) if !stops.is_empty() => {
            config.insert("stopSequences".into(), stops.clone().into());
        }
        _ => {}
    }
    match chat
        .pointer("/response_format/type")
        .and_then(Value::as_str)
    {
        Some("json_object") => {
            config.insert("responseMimeType".into(), "application/json".into());
        }
        Some("json_schema") => {
            config.insert("responseMimeType".into(), "application/json".into());
            if let Some(schema) = chat.pointer("/response_format/json_schema/schema") {
                config.insert("responseJsonSchema".into(), schema.clone());
            }
        }
        _ => {}
    }
    let mut thinking = Map::new();
    // Only a streamed reply shows what the model thought.
    if chat["stream"] == true {
        thinking.insert("includeThoughts".into(), true.into());
    }
    if let Some(level) = chat["reasoning_effort"].as_str().and_then(level) {
        thinking.insert("thinkingLevel".into(), level.into());
    }
    if !thinking.is_empty() {
        config.insert("thinkingConfig".into(), thinking.into());
    }
    config
}

/// The thinking level Google names for an effort, of the four it knows.
fn level(effort: &str) -> Option<&'static str> {
    Some(match effort.trim().to_lowercase().as_str() {
        "none" | "minimal" => "minimal",
        "low" => "low",
        "medium" => "medium",
        "high" | "xhigh" | "max" => "high",
        _ => return None,
    })
}

/// Add `parts` to the conversation as `role`'s, to its last turn when that
/// is `role`'s already: a batch of calls is answered in one turn, which the
/// pictures a result brings sit inside.
fn add(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    let last = contents.last_mut().filter(|turn| turn["role"] == role);
    if let Some(so_far) = last.and_then(|turn| turn["parts"].as_array_mut()) {
        so_far.extend(parts);
        return;
    }
    contents.push(json!({"role": role, "parts": parts}));
}

/// A turn with its answers to calls ahead of anything else in it, as Google
/// wants them right after the calls they answer.
fn answers_first(turn: &mut Value) {
    if let Some(parts) = turn["parts"].as_array_mut() {
        parts.sort_by_key(|part| part.get("functionResponse").is_none());
    }
}

/// The text of a chat message's content, whether one string or a list of parts.
fn text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_owned();
    }
    let parts = content.as_array().into_iter().flatten();
    let texts: Vec<&str> = parts.filter_map(|part| part["text"].as_str()).collect();
    texts.join("\n\n")
}

/// A user message's words and pictures as Google's parts.
fn parts(content: &Value) -> Vec<Value> {
    let Some(parts) = content.as_array() else {
        let words = text(content);
        return match words.trim().is_empty() {
            true => Vec::new(),
            false => vec![json!({"text": words})],
        };
    };
    let part = |part: &Value| match part["type"].as_str()? {
        // Google refuses a part with no text in it.
        "text" => (part["text"].as_str())
            .filter(|text| !text.trim().is_empty())
            .map(|text| json!({"text": text})),
        "image_url" => picture(part["image_url"]["url"].as_str()?),
        _ => None,
    };
    parts.iter().filter_map(part).collect()
}

/// The picture a data URL holds, as an inline part.
fn picture(url: &str) -> Option<Value> {
    let (kind, data) = url.strip_prefix("data:")?.split_once(";base64,")?;
    Some(json!({"inlineData": {"mimeType": kind, "data": data}}))
}

/// An assistant message as the model's parts: its words, then its calls,
/// each signed as it came. Google checks the first call of each step and
/// signs no other, so only that one goes as unchecked when it has none.
fn replied(message: &Value, named: &mut HashMap<String, String>) -> Vec<Value> {
    let mut parts = Vec::new();
    let said = text(&message["content"]);
    if !said.trim().is_empty() {
        parts.push(json!({"text": said}));
    }
    let calls = message["tool_calls"].as_array().into_iter().flatten();
    for (at, call) in calls.enumerate() {
        let name = call["function"]["name"].as_str().unwrap_or_default();
        let id = call["id"].as_str().unwrap_or_default();
        named.insert(id.to_owned(), name.to_owned());
        let arguments = call["function"]["arguments"].as_str().unwrap_or_default();
        let args = (serde_json::from_str::<Value>(arguments).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| json!({}));
        let mut part = json!({"functionCall": {"name": name, "args": args}});
        let signed = call.pointer("/extra_content/google/thought_signature");
        match (signed.and_then(Value::as_str), at) {
            (Some(signature), _) => part["thoughtSignature"] = signature.into(),
            (None, 0) => part["thoughtSignature"] = UNSIGNED.into(),
            (None, _) => {}
        }
        parts.push(part);
    }
    parts
}

/// A tool's result as the answer to the call it names, or its words alone
/// when that call is no longer in the conversation.
fn answer(message: &Value, named: &HashMap<String, String>) -> Value {
    let output = text(&message["content"]);
    let id = message["tool_call_id"].as_str().unwrap_or_default();
    let name = (named.get(id).map(String::as_str))
        .or_else(|| message["name"].as_str())
        .filter(|name| !name.is_empty());
    match name {
        Some(name) => json!({"functionResponse": {
            "name": name,
            "response": {"name": name, "content": output},
        }}),
        None => json!({"text": output}),
    }
}

/// A chat tool as a function Google can call, its schema as it is: Google
/// takes JSON Schema whole under `parametersJsonSchema`. The hosted web
/// search is no function, and is left out.
fn declared(tool: &Value) -> Option<Value> {
    if tool["type"] != "function" {
        return None;
    }
    let function = &tool["function"];
    let parameters = (function.get("parameters").cloned())
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    let mut declared = json!({"name": function["name"], "parametersJsonSchema": parameters});
    if let Some(description) = function.get("description") {
        declared["description"] = description.clone();
    }
    Some(declared)
}

/// The chat body's `tool_choice` as Google's calling mode.
fn choice(choice: &Value) -> Option<Value> {
    Some(match choice {
        Value::String(mode) => json!({"mode": match mode.as_str() {
            "none" => "NONE",
            "required" => "ANY",
            "auto" => "AUTO",
            _ => return None,
        }}),
        Value::Object(_) => {
            let name = choice.pointer("/function/name")?;
            json!({"mode": "ANY", "allowedFunctionNames": [name]})
        }
        _ => return None,
    })
}

/// A stream of responses read as chat chunks, each call given an id of its
/// own where Google gave none.
#[derive(Default)]
pub(crate) struct Stream {
    /// The calls so far, which numbers the next.
    calls: usize,
    /// What this reply's ids start with: its response id, or else the time.
    mark: Option<String>,
}

impl Stream {
    /// The chat chunk `event`, one response of the stream, stands for.
    pub(crate) fn event(&mut self, event: &Value) -> Value {
        if event.get("error").is_some() {
            return event.clone();
        }
        if self.mark.is_none()
            && let Some(id) = event["responseId"].as_str().filter(|id| !id.is_empty())
        {
            self.mark = Some(id.to_owned());
        }
        let candidate = &event["candidates"][0];
        // A prompt Google declined comes back with no candidate at all.
        if candidate.is_null()
            && let Some(reason) = event.pointer("/promptFeedback/blockReason")
        {
            return json!({"error": {
                "message": format!("Google declined to answer this prompt ({}).", reason.as_str().unwrap_or("blocked")),
                "code": "prompt_blocked",
            }});
        }
        let mut delta = Map::new();
        let (mut said, mut thought, mut calls) = (String::new(), String::new(), Vec::new());
        for part in candidate["content"]["parts"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if let Some(call) = part.get("functionCall") {
                calls.push(self.call(call, &part["thoughtSignature"]));
            } else if let Some(text) = part["text"].as_str() {
                match part["thought"] == true {
                    true => thought.push_str(text),
                    false => said.push_str(text),
                }
            }
        }
        if !said.is_empty() {
            delta.insert("content".into(), said.into());
        }
        if !thought.is_empty() {
            delta.insert("reasoning".into(), thought.into());
        }
        if !calls.is_empty() {
            delta.insert("tool_calls".into(), calls.into());
        }
        let mut choice = json!({"index": 0, "delta": delta});
        if let Some(reason) = candidate["finishReason"].as_str() {
            match finish(reason, self.calls > 0) {
                Ok(reason) => choice["finish_reason"] = reason.into(),
                Err(error) => return error,
            }
        }
        let mut chunk = json!({"choices": [choice]});
        if let Some(usage) = usage(&event["usageMetadata"]) {
            chunk["usage"] = usage;
        }
        chunk
    }

    /// A call as a chat chunk carries it, whole, with its signature where
    /// Google's copy of chat completions puts one.
    fn call(&mut self, call: &Value, signature: &Value) -> Value {
        let at = self.calls;
        self.calls += 1;
        let mark = (self.mark).get_or_insert_with(|| format!("{:x}", crate::chat::now()));
        let id = match call["id"].as_str() {
            Some(id) if !id.is_empty() => id.to_owned(),
            _ => format!("call_{mark}_{at}"),
        };
        let args = match &call["args"] {
            Value::Null => "{}".to_owned(),
            args => args.to_string(),
        };
        let mut wired = json!({
            "index": at,
            "id": id,
            "type": "function",
            "function": {"name": call["name"], "arguments": args},
        });
        if let Some(signature) = signature.as_str() {
            wired["extra_content"] = json!({"google": {"thought_signature": signature}});
        }
        wired
    }
}

/// A response that came whole as a chat completion; an error, or anything
/// else that is not one, as it is.
pub(crate) fn reply(value: &Value) -> Value {
    if value.get("candidates").is_none() && value.get("promptFeedback").is_none() {
        return value.clone();
    }
    let chunk = Stream::default().event(value);
    if chunk.get("error").is_some() {
        return chunk;
    }
    let choice = &chunk["choices"][0];
    let mut message = choice["delta"].clone();
    message["role"] = "assistant".into();
    if message.get("content").is_none() {
        message["content"] = "".into();
    }
    let mut whole = json!({"choices": [{
        "index": 0,
        "message": message,
        "finish_reason": choice["finish_reason"],
    }]});
    if let Some(usage) = chunk.get("usage") {
        whole["usage"] = usage.clone();
    }
    whole
}

/// Why a reply ended, as chat/completions says it; a call Google could not
/// finish writing is a failure on its end, which another go may not repeat.
fn finish(reason: &str, called: bool) -> Result<String, Value> {
    Ok(match reason {
        "STOP" if called => "tool_calls".into(),
        "STOP" => "stop".into(),
        "MAX_TOKENS" => "length".into(),
        "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII" | "IMAGE_SAFETY"
        | "LANGUAGE" => "content_filter".into(),
        "MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL" | "TOO_MANY_TOOL_CALLS" => {
            return Err(json!({"error": {
                "message": format!("The model wrote a tool call it could not finish ({reason})."),
                "code": "server_error",
            }}));
        }
        other => other.to_lowercase(),
    })
}

/// Google's `usageMetadata` under the names chat/completions gives it. The
/// thinking is output, as it is billed, and its own count besides.
fn usage(usage: &Value) -> Option<Value> {
    let prompt = usage["promptTokenCount"].as_u64()?;
    let tools = usage["toolUsePromptTokenCount"].as_u64().unwrap_or(0);
    let answer = usage["candidatesTokenCount"].as_u64().unwrap_or(0);
    let thoughts = usage["thoughtsTokenCount"].as_u64().unwrap_or(0);
    Some(json!({
        "prompt_tokens": prompt + tools,
        "completion_tokens": answer + thoughts,
        "total_tokens": usage["totalTokenCount"],
        "prompt_tokens_details": {"cached_tokens": usage["cachedContentTokenCount"].as_u64().unwrap_or(0)},
        "completion_tokens_details": {"reasoning_tokens": thoughts},
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::{FailureKind, classify};

    #[test]
    fn a_chat_request_goes_out_as_google_takes_it() {
        let chat = json!({
            "model": "gemini-3.8-flash",
            "messages": [
                {"role": "system", "content": [
                    {"type": "text", "text": "You are Letronna.", "cache_control": {"type": "ephemeral"}},
                    {"type": "text", "text": "Today is Tuesday."},
                ]},
                {"role": "user", "content": [
                    {"type": "text", "text": "What is this?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                ]},
                {"role": "assistant", "content": "Let me look.", "tool_calls": [
                    {"id": "call_a", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"},
                     "extra_content": {"google": {"thought_signature": "c2lnbmVk"}}},
                    {"id": "call_b", "type": "function",
                     "function": {"name": "list_dir", "arguments": "{\"path\":\".\"}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_a", "content": "fn main() {}"},
                {"role": "user", "content": [
                    {"type": "text", "text": "The screenshot it took:"},
                    {"type": "image_url", "image_url": {"url": "data:image/jpeg;base64,BBBB"}},
                ]},
                {"role": "tool", "tool_call_id": "call_b", "content": "a.rs"},
            ],
            "stream": true,
            "max_tokens": 8192,
            "stream_options": {"include_usage": true},
            "tools": [
                {"type": "function", "function": {
                    "name": "read_file", "description": "Read a file.",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}},
                                   "additionalProperties": false},
                }},
                {"type": "web_search"},
            ],
            "tool_choice": "none",
            "reasoning_effort": "low",
            "parallel_tool_calls": false,
        });
        let sent = request(&chat);
        assert_eq!(
            sent["systemInstruction"],
            json!({"parts": [{"text": "You are Letronna.\n\nToday is Tuesday."}]})
        );
        assert_eq!(
            sent["contents"],
            json!([
                {"role": "user", "parts": [
                    {"text": "What is this?"},
                    {"inlineData": {"mimeType": "image/png", "data": "AAAA"}},
                ]},
                {"role": "model", "parts": [
                    {"text": "Let me look."},
                    {"functionCall": {"name": "read_file", "args": {"path": "a"}},
                     "thoughtSignature": "c2lnbmVk"},
                    {"functionCall": {"name": "list_dir", "args": {"path": "."}}},
                ]},
                // Both answers in one turn, ahead of the picture that came between.
                {"role": "user", "parts": [
                    {"functionResponse": {"name": "read_file",
                        "response": {"name": "read_file", "content": "fn main() {}"}}},
                    {"functionResponse": {"name": "list_dir",
                        "response": {"name": "list_dir", "content": "a.rs"}}},
                    {"text": "The screenshot it took:"},
                    {"inlineData": {"mimeType": "image/jpeg", "data": "BBBB"}},
                ]},
            ])
        );
        assert_eq!(
            sent["tools"],
            json!([{"functionDeclarations": [{
                "name": "read_file", "description": "Read a file.",
                "parametersJsonSchema": {"type": "object", "properties": {"path": {"type": "string"}},
                                         "additionalProperties": false},
            }]}])
        );
        assert_eq!(
            sent["toolConfig"],
            json!({"functionCallingConfig": {"mode": "NONE"}})
        );
        assert_eq!(
            sent["generationConfig"],
            json!({"maxOutputTokens": 8192,
                   "thinkingConfig": {"includeThoughts": true, "thinkingLevel": "low"}})
        );
        for left_out in [
            "messages",
            "model",
            "stream",
            "stream_options",
            "max_tokens",
        ] {
            assert!(sent.get(left_out).is_none(), "{left_out}");
        }
    }

    #[test]
    fn a_call_kept_without_its_signature_goes_back_unchecked() {
        let chat = json!({"messages": [
            {"role": "user", "content": "Hi."},
            {"role": "assistant", "content": "", "tool_calls": [
                {"id": "call_a", "function": {"name": "read_file", "arguments": "not json"}},
            ]},
            {"role": "tool", "tool_call_id": "call_a", "content": "a"},
            {"role": "tool", "tool_call_id": "gone", "content": "an older result"},
        ]});
        let sent = request(&chat);
        assert_eq!(
            sent["contents"][1],
            json!({"role": "model", "parts": [
                {"functionCall": {"name": "read_file", "args": {}},
                 "thoughtSignature": "skip_thought_signature_validator"},
            ]})
        );
        assert_eq!(
            sent["contents"][2]["parts"][1],
            json!({"text": "an older result"})
        );
        assert!(sent.get("generationConfig").is_none());
        assert!(sent.get("systemInstruction").is_none());
    }

    #[test]
    fn a_stream_reads_as_chat_chunks_with_its_calls_signed() {
        let mut stream = Stream::default();
        let thought = stream.event(&json!({"candidates": [{"content": {"role": "model",
            "parts": [{"text": "**Reading** the file first.", "thought": true}]}}],
            "responseId": "r1"}));
        assert_eq!(
            thought["choices"][0]["delta"],
            json!({"reasoning": "**Reading** the file first."})
        );
        let said = stream.event(&json!({"candidates": [{"content": {"role": "model",
            "parts": [{"text": "Reading it."}]}}]}));
        assert_eq!(
            said["choices"][0]["delta"],
            json!({"content": "Reading it."})
        );
        let calls = stream.event(&json!({"candidates": [{"content": {"role": "model", "parts": [
            {"functionCall": {"name": "read_file", "args": {"path": "a.rs"}}, "thoughtSignature": "c2ln"},
            {"functionCall": {"name": "list_dir", "args": {"path": "."}, "id": "fc_9"}},
        ]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 900, "candidatesTokenCount": 40,
                "thoughtsTokenCount": 24, "cachedContentTokenCount": 512, "totalTokenCount": 964}}));
        assert_eq!(
            calls["choices"][0]["delta"]["tool_calls"],
            json!([
                {"index": 0, "id": "call_r1_0", "type": "function",
                 "function": {"name": "read_file", "arguments": "{\"path\":\"a.rs\"}"},
                 "extra_content": {"google": {"thought_signature": "c2ln"}}},
                {"index": 1, "id": "fc_9", "type": "function",
                 "function": {"name": "list_dir", "arguments": "{\"path\":\".\"}"}},
            ])
        );
        assert_eq!(calls["choices"][0]["finish_reason"], "tool_calls");
        let usage = crate::usage::Usage::read(&calls["usage"]);
        assert_eq!(
            (usage.input, usage.cached, usage.output, usage.reasoning),
            (900, 512, 64, 24)
        );
    }

    #[test]
    fn a_whole_reply_reads_as_a_chat_completion() {
        let whole = reply(&json!({"candidates": [{"content": {"role": "model",
            "parts": [{"text": "Fix the parser"}]}, "finishReason": "MAX_TOKENS"}],
            "usageMetadata": {"promptTokenCount": 30, "candidatesTokenCount": 3}}));
        assert_eq!(whole["choices"][0]["message"]["content"], "Fix the parser");
        assert_eq!(whole["choices"][0]["message"]["role"], "assistant");
        assert_eq!(whole["choices"][0]["finish_reason"], "length");
        assert_eq!(whole["usage"]["prompt_tokens"], 30);
        // Anything else is left as it came.
        let error =
            json!({"error": {"code": 429, "message": "Quota.", "status": "RESOURCE_EXHAUSTED"}});
        assert_eq!(reply(&error), error);
    }

    #[test]
    fn what_google_turns_down_reads_as_the_failure_it_is() {
        let headers = reqwest::header::HeaderMap::new();
        let limited = Stream::default().event(&json!({"error": {"code": 429,
            "message": "Resource has been exhausted.", "status": "RESOURCE_EXHAUSTED"}}));
        assert_eq!(
            classify(None, &headers, &limited).kind,
            FailureKind::RateLimit
        );
        let blocked = reply(&json!({"promptFeedback": {"blockReason": "PROHIBITED_CONTENT"}}));
        let failure = classify(None, &headers, &blocked);
        assert_eq!(
            failure.to_string(),
            "Google declined to answer this prompt (PROHIBITED_CONTENT)."
        );
        let malformed = Stream::default().event(&json!({"candidates": [{"content": {"parts": []},
            "finishReason": "MALFORMED_FUNCTION_CALL"}]}));
        assert_eq!(
            classify(None, &headers, &malformed).kind,
            FailureKind::Server
        );
        let filtered =
            Stream::default().event(&json!({"candidates": [{"finishReason": "SAFETY"}]}));
        assert_eq!(filtered["choices"][0]["finish_reason"], "content_filter");
    }

    /// A local server at `/google/v1beta` answering each request with the
    /// next of `replies`, a Google event stream each; it hands back each
    /// request's head, in lower case, and body. The list of models the first
    /// request asks for on the side is not there.
    fn server(replies: Vec<String>) -> (String, std::thread::JoinHandle<Vec<(String, Value)>>) {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/google/v1beta", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let mut asked = Vec::new();
            let mut replies = replies.into_iter();
            while !replies.as_slice().is_empty() {
                let (mut socket, _) = server.accept().unwrap();
                let mut first = [0; 4];
                socket.peek(&mut first).unwrap();
                if &first == b"GET " {
                    write!(
                        socket,
                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                    )
                    .unwrap();
                    continue;
                }
                let reply = replies.next().unwrap();
                let mut bytes = Vec::new();
                let mut block = [0; 65536];
                let (head, body) = loop {
                    let n = socket.read(&mut block).unwrap();
                    bytes.extend_from_slice(&block[..n]);
                    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length: usize = (head.lines())
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .map_or(0, |n| n.parse().unwrap());
                    if bytes.len() >= end + 4 + length {
                        let body =
                            serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                        break (head, body);
                    }
                };
                asked.push((head, body));
                write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
            }
            asked
        });
        (endpoint, worker)
    }

    /// `responses` as Google streams them.
    fn sse(responses: &[Value]) -> String {
        responses
            .iter()
            .map(|r| format!("data: {r}\r\n\r\n"))
            .collect()
    }

    #[test]
    fn a_call_and_its_signature_go_back_on_the_next_round() {
        use crate::chat::{Connection, Event, Message, ToolCall, start};
        let thinks_and_calls = sse(&[
            json!({"candidates": [{"content": {"role": "model", "parts": [
                {"text": "The file comes first.", "thought": true}]}}], "responseId": "r1"}),
            json!({"candidates": [{"content": {"role": "model", "parts": [
                {"functionCall": {"name": "read_file", "args": {"path": "a.rs"}},
                 "thoughtSignature": "c2lnbmVk"}]}, "finishReason": "STOP"}],
                "usageMetadata": {"promptTokenCount": 120, "candidatesTokenCount": 9,
                    "thoughtsTokenCount": 30, "totalTokenCount": 159}}),
        ]);
        let done = sse(&[json!({"candidates": [{"content": {"role": "model",
            "parts": [{"text": "Done."}]}, "finishReason": "STOP"}],
            "usageMetadata": {"promptTokenCount": 200, "candidatesTokenCount": 2}})]);
        let (endpoint, worker) = server(vec![thinks_and_calls, done]);
        let c = Connection {
            endpoint,
            model: "gemini-test-flash".into(),
            key: "g-key".into(),
            conversation: 0x600D,
            ..Default::default()
        };
        let said = |role: &str, text: &str| Message {
            role: role.into(),
            content: text.into(),
            ..Default::default()
        };
        let heard = |events: async_channel::Receiver<Event>| {
            let (mut lines, mut replay) = (Vec::new(), None);
            while let Ok(event) = events.recv_blocking() {
                match event {
                    Event::Reasoning(text) => lines.push(format!("thinking {text}")),
                    Event::Delta(text) => lines.push(format!("text {text}")),
                    Event::ToolCalls(calls) => lines.push(format!("calls {}", calls.len())),
                    Event::Usage {
                        prompt,
                        completion,
                        reasoning,
                        ..
                    } => lines.push(format!(
                        "usage {prompt} in, {completion} out, {reasoning} thinking"
                    )),
                    Event::Replay(kept) => replay = Some(kept),
                    Event::Finished(result) => {
                        lines.push(format!("finished {:?}", result.map_err(|f| f.to_string())));
                        break;
                    }
                    _ => {}
                }
            }
            (lines, replay)
        };
        let mut messages = vec![said("user", "Look at a.rs.")];
        let (first, replay) = heard(start(c.clone(), messages.clone()));
        assert_eq!(
            first,
            [
                "thinking The file comes first.",
                "usage 120 in, 39 out, 30 thinking",
                "calls 1",
                "finished Ok(())",
            ]
        );
        let replay = replay.expect("the signature is kept");
        assert_eq!(
            replay.extra_content["call_r1_0"],
            json!({"google": {"thought_signature": "c2lnbmVk"}})
        );
        messages.push(Message {
            model: c.model.clone(),
            tool_calls: vec![ToolCall {
                id: "call_r1_0".into(),
                name: "read_file".into(),
                arguments: r#"{"path":"a.rs"}"#.into(),
            }],
            replay: Some(Box::new(replay)),
            ..said("assistant", "")
        });
        messages.push(Message {
            tool_call_id: "call_r1_0".into(),
            name: "read_file".into(),
            ..said("tool", "fn main() {}")
        });
        let (second, _) = heard(start(c, messages));
        assert_eq!(second.last().unwrap(), "finished Ok(())");
        let asked = worker.join().unwrap();
        let (head, body) = &asked[0];
        assert!(
            head.starts_with(
                "post /google/v1beta/models/gemini-test-flash:streamgeneratecontent?alt=sse "
            ),
            "{head}"
        );
        assert!(head.contains("x-goog-api-key: g-key"), "{head}");
        assert!(!head.contains("authorization"), "{head}");
        assert!(body["systemInstruction"]["parts"][0]["text"].is_string());
        let (_, body) = &asked[1];
        let turns = body["contents"].as_array().unwrap();
        assert_eq!(
            turns[1],
            json!({"role": "model", "parts": [{"functionCall":
                {"name": "read_file", "args": {"path": "a.rs"}}, "thoughtSignature": "c2lnbmVk"}]})
        );
        assert_eq!(
            turns[2]["parts"][0]["functionResponse"]["name"],
            "read_file"
        );
    }

    #[test]
    fn googles_list_reads_as_a_models_list_of_what_it_can_answer() {
        let list = json!({"models": [
            {"name": "models/gemini-test-flash", "inputTokenLimit": 1048576,
             "outputTokenLimit": 65536, "supportedGenerationMethods": ["generateContent", "countTokens"]},
            {"name": "models/text-embedding-test", "supportedGenerationMethods": ["embedContent"]},
        ]});
        assert_eq!(
            listed(&list),
            [json!({"id": "gemini-test-flash", "context_length": 1048576,
                    "max_completion_tokens": 65536})]
        );
        let facts = {
            crate::models::remember("https://g.test/v1beta", &listed(&list));
            crate::models::facts("https://g.test/v1beta", "gemini-test-flash")
        };
        assert_eq!(facts.context, Some(1_048_576));
    }

    #[test]
    fn a_v1beta_base_speaks_google_and_each_model_has_its_route() {
        assert!(speaks("https://opencode.ai/inference/google/v1beta"));
        assert!(speaks("https://generativelanguage.googleapis.com/v1beta/"));
        assert!(!speaks(
            "https://generativelanguage.googleapis.com/v1beta/openai"
        ));
        assert!(!speaks("https://opencode.ai/inference/openai/v1"));
        let chat = "https://opencode.ai/inference/google/v1beta/chat/completions";
        assert_eq!(
            url(chat, "gemini-4-argon", true),
            "https://opencode.ai/inference/google/v1beta/models/gemini-4-argon:streamGenerateContent?alt=sse"
        );
        assert_eq!(
            url(chat, "models/gemini-3-flash", false),
            "https://opencode.ai/inference/google/v1beta/models/gemini-3-flash:generateContent"
        );
    }
}
