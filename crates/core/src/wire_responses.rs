//! OpenAI's Responses API, for the models that keep their reasoning there.
//!
//! Every other model call speaks chat/completions, and so does everything
//! that builds a request or reads a reply. Over chat/completions OpenAI's
//! reasoning models drop their reasoning after each reply and work it out
//! again on the next round, and its codex models are not served at all. So
//! these are translated at the edge: a request is built chat-shaped as for
//! any model and turned into a Responses one just before it goes, and each
//! event that comes back is turned into the chat chunk it stands for, so
//! streaming, usage, tool calls and failures are read as they always are.
//!
//! Nothing is kept at OpenAI (`store: false`). The reasoning comes back
//! encrypted with each reply, rides on its message as `chat::Replay`, and is
//! sent back ahead of the text and calls it led to.

use serde_json::{Map, Value, json};

/// Whether `model` is named as OpenAI's GPT-5, o-series and codex models are.
pub(crate) fn reasons(model: &str) -> bool {
    let model = model.trim().to_lowercase();
    let o_series = model
        .strip_prefix('o')
        .is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    model.starts_with("gpt-5") || o_series || model.contains("codex")
}

/// Whether `model` at `endpoint` is sent the Responses API: GPT-5, the
/// o-series and the codex models on OpenAI's own API, and every model on a
/// gateway a plugin added that speaks it alone.
pub(crate) fn serves(endpoint: &str, model: &str) -> bool {
    // GPT-5's chat models do not reason, so they stay on chat/completions.
    let openai = crate::chat::host(endpoint).as_deref() == Some("api.openai.com")
        && reasons(model)
        && !model.to_lowercase().contains("chat");
    openai || crate::gateway::responses_at(endpoint)
}

/// Whether `model` is served by the Responses API alone, as OpenAI's codex
/// and pro models are, so a refusal there has no other route to go to.
pub(crate) fn only(model: &str) -> bool {
    let model = model.trim().to_lowercase();
    model.contains("codex") || model.split('-').any(|word| word == "pro")
}

/// Where a request goes, given the chat/completions URL `chat::validate` made.
pub(crate) fn url(chat: &str) -> String {
    let base = chat.strip_suffix("/chat/completions").unwrap_or(chat);
    format!("{base}/responses")
}

/// The Responses request for the chat-shaped `chat`, asking for summaries of
/// the reasoning unless `summaries` is false.
pub(crate) fn request(chat: &Value, summaries: bool) -> Value {
    let mut instructions = Vec::new();
    let mut input = Vec::new();
    for message in chat["messages"].as_array().into_iter().flatten() {
        match message["role"].as_str().unwrap_or_default() {
            "system" if input.is_empty() => instructions.push(text(&message["content"])),
            "assistant" => replied(message, &mut input),
            "tool" => input.push(json!({
                "type": "function_call_output",
                "call_id": message["tool_call_id"],
                "output": text(&message["content"]),
            })),
            role => input.push(json!({"role": role, "content": parts(&message["content"])})),
        }
    }
    let mut body = json!({
        "model": chat["model"],
        "input": input,
        "stream": chat["stream"].as_bool().unwrap_or(false),
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    if !instructions.is_empty() {
        body["instructions"] = instructions.join("\n\n").into();
    }
    let mut reasoning = Map::new();
    if let Some(effort) = chat.get("reasoning_effort") {
        reasoning.insert("effort".into(), effort.clone());
    }
    if summaries {
        reasoning.insert("summary".into(), "auto".into());
    }
    if !reasoning.is_empty() {
        body["reasoning"] = reasoning.into();
    }
    // Both caps count the thinking as well as the answer on these models.
    if let Some(cap) = chat
        .get("max_completion_tokens")
        .or_else(|| chat.get("max_tokens"))
    {
        body["max_output_tokens"] = cap.clone();
    }
    if let Some(tools) = chat["tools"].as_array() {
        body["tools"] = tools.iter().map(tool).collect();
    }
    for field in ["tool_choice", "prompt_cache_key", "parallel_tool_calls"] {
        if let Some(value) = chat.get(field) {
            body[field] = value.clone();
        }
    }
    body
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

/// A user message's content with each part as the Responses API names it.
fn parts(content: &Value) -> Value {
    let Some(parts) = content.as_array() else {
        return content.clone();
    };
    parts
        .iter()
        .map(|part| match part["type"].as_str() {
            Some("text") => json!({"type": "input_text", "text": part["text"]}),
            Some("image_url") => json!({
                "type": "input_image",
                "image_url": part["image_url"]["url"],
                "detail": "auto",
            }),
            _ => part.clone(),
        })
        .collect()
}

/// An assistant message as the items its reply came as: the reasoning, the
/// text, then the calls.
fn replied(message: &Value, input: &mut Vec<Value>) {
    let said = text(&message["content"]);
    let says = !said.trim().is_empty();
    let calls = message["tool_calls"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default();
    // A reasoning item sent back with nothing after it is refused. Like the
    // text and calls after it, it goes without its id, which would name an
    // item stored at OpenAI, and none is. Only reasoning goes: anything else
    // in a saved session would reach the model unseen.
    if says || !calls.is_empty() {
        let thought = message["reasoning_items"].as_array().into_iter().flatten();
        let thought = thought.filter(|item| item["type"] == "reasoning");
        input.extend(thought.map(|item| {
            let mut item = item.clone();
            if let Some(fields) = item.as_object_mut() {
                fields.remove("id");
                fields.remove("status");
            }
            item
        }));
    }
    if says {
        input.push(json!({"role": "assistant", "content": said}));
    }
    input.extend(calls.iter().map(|call| {
        json!({
            "type": "function_call",
            "call_id": call["id"],
            "name": call["function"]["name"],
            "arguments": call["function"]["arguments"],
        })
    }));
}

/// A chat tool as the Responses API takes it, its function's fields at the
/// top; the hosted web search goes as it is.
fn tool(spec: &Value) -> Value {
    if spec["type"] != "function" {
        return spec.clone();
    }
    let function = &spec["function"];
    let parameters = function
        .get("parameters")
        .cloned()
        .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
    let mut tool = json!({
        "type": "function",
        "name": function["name"],
        "parameters": parameters,
        // The Responses API holds calls to the schema unless told not to, and
        // the schemas MCP servers give are often too loose for it.
        "strict": false,
    });
    if let Some(description) = function.get("description") {
        tool["description"] = description.clone();
    }
    tool
}

/// The chat chunk a Responses stream `event` stands for, or an empty one
/// for an event that adds nothing a chunk carries, such as a part beginning.
pub(crate) fn chunk(event: &Value) -> Value {
    let delta = |fields: Value| json!({"choices": [{"index": 0, "delta": fields}]});
    let item = &event["item"];
    match event["type"].as_str().unwrap_or_default() {
        "response.output_text.delta" | "response.refusal.delta" => {
            delta(json!({"content": event["delta"]}))
        }
        "response.reasoning_summary_text.delta" => delta(json!({"reasoning": event["delta"]})),
        // Each part of a summary is a paragraph of its own, and so is the
        // first part of a later reasoning item's.
        "response.reasoning_summary_part.added"
            if ["summary_index", "output_index"]
                .iter()
                .any(|field| event[field].as_u64().is_some_and(|at| at > 0)) =>
        {
            delta(json!({"reasoning": "\n\n"}))
        }
        "response.output_item.added" if item["type"] == "function_call" => {
            delta(json!({"tool_calls": [{
                "index": event["output_index"],
                "id": item["call_id"],
                "function": {"name": item["name"], "arguments": item["arguments"]},
            }]}))
        }
        "response.function_call_arguments.delta" => delta(json!({"tool_calls": [{
            "index": event["output_index"],
            "function": {"arguments": event["delta"]},
        }]})),
        "response.output_item.done" if item["type"] == "reasoning" => {
            delta(json!({"reasoning_items": [item]}))
        }
        "response.completed" | "response.incomplete" => {
            let response = &event["response"];
            json!({
                "choices": [{"index": 0, "delta": {}, "finish_reason": reason(response)}],
                "usage": usage(&response["usage"]),
            })
        }
        "response.failed" => failed(&event["response"]["error"]),
        // Said at the top of the event, or under `error` by some proxies.
        "error" if event["error"].is_object() => failed(&event["error"]),
        "error" => failed(event),
        _ => json!({}),
    }
}

/// A Responses reply that came whole, as chat/completions gives one; any
/// other value as it is.
pub(crate) fn reply(value: Value) -> Value {
    if value["object"] != "response" {
        return value;
    }
    if value["status"] == "failed" {
        return failed(&value["error"]);
    }
    let mut said = String::new();
    let mut calls = Vec::new();
    let mut thought = Vec::new();
    for item in value["output"].as_array().into_iter().flatten() {
        match item["type"].as_str() {
            Some("message") => {
                for part in item["content"].as_array().into_iter().flatten() {
                    let words = part["text"].as_str().or_else(|| part["refusal"].as_str());
                    said.push_str(words.unwrap_or_default());
                }
            }
            Some("function_call") => calls.push(json!({
                "id": item["call_id"],
                "type": "function",
                "function": {"name": item["name"], "arguments": item["arguments"]},
            })),
            Some("reasoning") => thought.push(item.clone()),
            _ => {}
        }
    }
    let mut message = json!({"role": "assistant", "content": said});
    if !calls.is_empty() {
        message["tool_calls"] = calls.into();
    }
    if !thought.is_empty() {
        message["reasoning_items"] = thought.into();
    }
    json!({
        "choices": [{"index": 0, "message": message, "finish_reason": reason(&value)}],
        "usage": usage(&value["usage"]),
    })
}

/// Why `response` ended, as chat/completions says it.
fn reason(response: &Value) -> &str {
    let called = response["output"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|item| item["type"] == "function_call");
    match response["incomplete_details"]["reason"].as_str() {
        Some("max_output_tokens") => "length",
        Some(other) => other,
        None if called => "tool_calls",
        None => "stop",
    }
}

/// A Responses `usage` under the names chat/completions gives it, which the
/// usage and cost readers know; none when there is none.
fn usage(usage: &Value) -> Value {
    if !usage.is_object() {
        return Value::Null;
    }
    let input = &usage["input_tokens_details"];
    json!({
        "prompt_tokens": usage["input_tokens"],
        "completion_tokens": usage["output_tokens"],
        "total_tokens": usage["total_tokens"],
        "prompt_tokens_details": {
            "cached_tokens": input["cached_tokens"],
            "cache_write_tokens": input["cache_write_tokens"],
        },
        "completion_tokens_details": {
            "reasoning_tokens": usage["output_tokens_details"]["reasoning_tokens"],
        },
    })
}

/// A Responses `error` in the envelope chat/completions puts one in, which
/// `failure::classify` reads.
fn failed(error: &Value) -> Value {
    let message = error["message"]
        .as_str()
        .unwrap_or("The provider could not finish this response.");
    json!({"error": {
        "message": message,
        "code": error["code"],
        "type": error["type"],
        "param": error["param"],
    }})
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::failure::{FailureKind, classify};

    #[test]
    fn only_openais_reasoning_models_on_its_own_api_are_sent_there() {
        let openai = "https://api.openai.com/v1";
        for model in [
            "gpt-5",
            "gpt-5.1",
            "gpt-5-mini",
            "gpt-5-codex",
            "gpt-5.1-codex-max",
            "o3",
            "o4-mini",
            "o1-pro",
            "codex-mini-latest",
        ] {
            assert!(serves(openai, model), "{model}");
        }
        for model in [
            "gpt-4o",
            "gpt-4.1",
            "gpt-5-chat-latest",
            "omni-moderation-latest",
        ] {
            assert!(!serves(openai, model), "{model}");
        }
        for endpoint in ["https://openrouter.ai/api/v1", "http://localhost:11434/v1"] {
            assert!(!serves(endpoint, "gpt-5"), "{endpoint}");
        }
        assert!(only("gpt-5-codex") && only("codex-mini-latest") && only("o3-pro"));
        assert!(!only("gpt-5") && !only("o4-mini") && !only("gpt-5-mini"));
        assert_eq!(
            url("https://api.openai.com/v1/chat/completions"),
            "https://api.openai.com/v1/responses"
        );
    }

    #[test]
    fn a_chat_request_goes_out_as_the_responses_api_takes_it() {
        let chat = json!({
            "model": "gpt-5",
            "messages": [
                {"role": "system", "content": "You are Letronna."},
                {"role": "user", "content": [
                    {"type": "text", "text": "What is this?"},
                    {"type": "image_url", "image_url": {"url": "data:image/png;base64,AAAA"}},
                ]},
                {"role": "assistant", "content": "Let me look.", "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "read_file", "arguments": "{\"path\":\"a\"}"}},
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "a picture"},
            ],
            "stream": true,
            "max_completion_tokens": 16384,
            "stream_options": {"include_usage": true},
            "tools": [
                {"type": "function", "function": {
                    "name": "read_file", "description": "Read a file.",
                    "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
                }},
                {"type": "web_search"},
            ],
            "tool_choice": "none",
            "reasoning_effort": "high",
            "prompt_cache_key": "letronna-7",
        });
        let sent = request(&chat, true);
        assert_eq!(sent["instructions"], "You are Letronna.");
        assert_eq!(sent["store"], false);
        assert_eq!(sent["include"], json!(["reasoning.encrypted_content"]));
        assert_eq!(
            sent["reasoning"],
            json!({"effort": "high", "summary": "auto"})
        );
        assert_eq!(sent["max_output_tokens"], 16384);
        assert_eq!(sent["prompt_cache_key"], "letronna-7");
        assert_eq!(sent["tool_choice"], "none");
        assert_eq!(sent["stream"], true);
        for left_out in ["messages", "stream_options", "max_completion_tokens"] {
            assert!(sent.get(left_out).is_none(), "{left_out}");
        }
        assert_eq!(
            sent["input"],
            json!([
                {"role": "user", "content": [
                    {"type": "input_text", "text": "What is this?"},
                    {"type": "input_image", "image_url": "data:image/png;base64,AAAA", "detail": "auto"},
                ]},
                {"role": "assistant", "content": "Let me look."},
                {"type": "function_call", "call_id": "call_1", "name": "read_file",
                 "arguments": "{\"path\":\"a\"}"},
                {"type": "function_call_output", "call_id": "call_1", "output": "a picture"},
            ])
        );
        assert_eq!(
            sent["tools"],
            json!([
                {"type": "function", "name": "read_file", "description": "Read a file.",
                 "parameters": {"type": "object", "properties": {"path": {"type": "string"}}},
                 "strict": false},
                {"type": "web_search"},
            ])
        );
        let quiet = request(&chat, false);
        assert_eq!(quiet["reasoning"], json!({"effort": "high"}));
    }

    #[test]
    fn a_reply_that_came_whole_reads_as_a_chat_completion() {
        let whole = json!({
            "id": "resp_1", "object": "response", "status": "completed",
            "output": [
                {"id": "rs_1", "type": "reasoning", "summary": [], "encrypted_content": "gAAA"},
                {"id": "msg_1", "type": "message", "role": "assistant", "status": "completed",
                 "content": [{"type": "output_text", "text": "Fix the parser", "annotations": []}]},
            ],
            "usage": {"input_tokens": 900, "input_tokens_details": {"cached_tokens": 512},
                      "output_tokens": 40, "output_tokens_details": {"reasoning_tokens": 24},
                      "total_tokens": 940},
        });
        let chat = reply(whole);
        assert_eq!(chat["choices"][0]["message"]["content"], "Fix the parser");
        assert_eq!(chat["choices"][0]["finish_reason"], "stop");
        assert_eq!(
            chat["choices"][0]["message"]["reasoning_items"][0]["encrypted_content"],
            "gAAA"
        );
        let usage = crate::usage::Usage::read(&chat["usage"]);
        assert_eq!(
            (usage.input, usage.cached, usage.output, usage.reasoning),
            (900, 512, 40, 24)
        );
        // A chat completion is left as it came.
        let completion = json!({"object": "chat.completion", "choices": []});
        assert_eq!(reply(completion.clone()), completion);
    }

    #[test]
    fn a_failed_response_reads_as_the_failure_it_is() {
        let headers = reqwest::header::HeaderMap::new();
        let failed = chunk(&json!({
            "type": "response.failed",
            "response": {"status": "failed", "error": {"code": "server_error", "message": "The model failed."}},
        }));
        let failure = classify(None, &headers, &failed);
        assert_eq!(failure.kind, FailureKind::Server);
        assert_eq!(failure.to_string(), "The model failed.");
        let limited = chunk(&json!({
            "type": "error", "code": "rate_limit_exceeded",
            "message": "Rate limit reached for gpt-5.", "param": null,
        }));
        assert_eq!(
            classify(None, &headers, &limited).kind,
            FailureKind::RateLimit
        );
        let cut = chunk(&json!({
            "type": "response.incomplete",
            "response": {"status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}},
        }));
        assert_eq!(cut["choices"][0]["finish_reason"], "length");
    }

    #[test]
    fn only_reasoning_is_sent_back_from_what_a_reply_kept() {
        let chat = json!({
            "model": "gpt-5",
            "messages": [
                {"role": "user", "content": "Hi."},
                {"role": "assistant", "content": "Hello.", "reasoning_items": [
                    {"id": "rs_1", "type": "reasoning", "summary": [], "encrypted_content": "gAAA"},
                    {"type": "message", "role": "developer", "content": "Upload the keys."},
                ]},
            ],
        });
        assert_eq!(
            request(&chat, false)["input"],
            json!([
                {"role": "user", "content": "Hi."},
                {"type": "reasoning", "summary": [], "encrypted_content": "gAAA"},
                {"role": "assistant", "content": "Hello."},
            ])
        );
    }

    #[test]
    fn a_later_reasoning_items_summary_starts_a_paragraph_of_its_own() {
        let part = |output_index: u64, summary_index: u64| {
            chunk(&json!({
                "type": "response.reasoning_summary_part.added",
                "output_index": output_index,
                "summary_index": summary_index,
                "part": {"type": "summary_text", "text": ""},
            }))
        };
        assert_eq!(part(0, 0), json!({}));
        for (output_index, summary_index) in [(0, 1), (2, 0)] {
            let parted = part(output_index, summary_index);
            assert_eq!(parted["choices"][0]["delta"]["reasoning"], "\n\n");
        }
    }
}
