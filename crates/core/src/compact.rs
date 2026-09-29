//! Old turns, summarized instead of forgotten.
//!
//! The context window (`chat::window_start`) drops the oldest messages in
//! steps once a conversation outgrows its budget, and whatever it dropped was
//! simply gone: a long session lost its goal, its decisions and the files it
//! had touched. Here the dropped messages are folded into a short summary
//! that rides at the end of the system prompt instead.
//!
//! The window moves in steps and the summary only changes when it does, so
//! the requests in between still share their prefix with the provider's
//! prompt cache. Each step folds what it newly dropped into the summary made
//! for the step before, so a summary is never rebuilt from the whole history.
//!
//! Summaries are kept in memory, keyed by what they cover, and never written
//! down: nothing about a conversation outlives its own file when it is
//! deleted. The price is summarizing a long session once more after a
//! restart, from its most recent part only.

use crate::chat::{Connection, Message};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::hash::{DefaultHasher, Hasher};
use std::sync::Mutex;
use std::time::Duration;

/// The most of the dropped conversation one summary is made from, in
/// characters. A fresh summary of a long session starts from its end.
const INPUT_CHARS: usize = 48_000;
/// How much of one tool result the summarizer is shown.
const TOOL_CHARS: usize = 1_500;
/// How much of a summary is kept, whatever the model wrote.
const SUMMARY_CHARS: usize = 4_000;

/// What the summarizer is asked to do.
const INSTRUCTIONS: &str = "You condense the older part of a conversation between a user and Letronna, an assistant with tools, so the conversation can go on without it. Write the summary for the assistant's own later use. Keep the user's goal and constraints; what has been done and what was found; decisions and why; files, commands and names that matter; open questions and next steps. Drop pleasantries and anything since superseded. When an earlier summary is given, fold the new messages into it. Plain text, under 300 words. Reply with the summary only.";

/// A summary per stretch of dropped messages, by `key`. `None` marks one that
/// could not be made, so a gateway that will not do it is not asked again on
/// every request.
type Cache = Mutex<HashMap<u64, Option<String>>>;

/// The summary of `dropped`, the messages the window no longer holds.
pub fn summary(connection: &Connection, dropped: &[Message]) -> Option<String> {
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    summary_in(cache, dropped, |earlier, transcript| {
        ask(connection, earlier, transcript)
    })
}

/// `summary`, with the cache and the model call handed in.
fn summary_in(
    cache: &Cache,
    dropped: &[Message],
    ask: impl FnOnce(Option<&str>, &str) -> Result<String, String>,
) -> Option<String> {
    if dropped.is_empty() {
        return None;
    }
    let keys = keys(dropped);
    let whole = keys[dropped.len()];
    let known = |key: u64| cache.lock().ok()?.get(&key).cloned();
    if let Some(made) = known(whole) {
        return made;
    }
    // The last step's summary, if there is one, covers a start of these
    // messages and ends on a user turn. Only what came after it is new.
    let earlier = (1..dropped.len())
        .rev()
        .filter(|&at| crate::chat::opens_turn(&dropped[at]))
        .find_map(|at| known(keys[at]).flatten().map(|text| (at, text)));
    let (from, earlier) = match &earlier {
        Some((at, text)) => (*at, Some(text.as_str())),
        None => (0, None),
    };
    let made = ask(earlier, &transcript(&dropped[from..]))
        .ok()
        .map(|text| cut(text.trim(), SUMMARY_CHARS))
        .filter(|text| !text.is_empty());
    if let Ok(mut cache) = cache.lock() {
        cache.insert(whole, made.clone());
    }
    made
}

/// A key for every start of `messages`: `keys[n]` stands for the first `n`.
fn keys(messages: &[Message]) -> Vec<u64> {
    let mut hasher = DefaultHasher::new();
    let mut keys = vec![hasher.finish()];
    for m in messages {
        for part in [&m.role, &m.content, &m.tool_call_id] {
            hasher.write(part.as_bytes());
            hasher.write_u8(0);
        }
        for call in &m.tool_calls {
            for part in [&call.id, &call.name, &call.arguments] {
                hasher.write(part.as_bytes());
                hasher.write_u8(0);
            }
        }
        keys.push(hasher.finish());
    }
    keys
}

/// The messages as the summarizer reads them, its most recent part when it
/// is too long.
fn transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for m in messages {
        match m.role.as_str() {
            "user" => out.push_str(&format!("User: {}\n\n", m.content)),
            "tool" => out.push_str(&format!(
                "Tool {} returned: {}\n\n",
                m.name,
                cut(&m.content, TOOL_CHARS)
            )),
            _ => {
                if !m.content.is_empty() {
                    out.push_str(&format!("Assistant: {}\n\n", m.content));
                }
                for call in &m.tool_calls {
                    out.push_str(&format!(
                        "Assistant used {} with {}\n\n",
                        call.name,
                        cut(&call.arguments, 300)
                    ));
                }
            }
        }
    }
    let total = out.chars().count();
    if total <= INPUT_CHARS {
        return out;
    }
    let tail: String = out.chars().skip(total - INPUT_CHARS).collect();
    format!("[the earliest part is left out]\n{tail}")
}

/// At most `max` characters of `text`, marked where it was cut.
fn cut(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept} …")
}

/// One summary from the model the conversation is using, not streamed.
fn ask(connection: &Connection, earlier: Option<&str>, transcript: &str) -> Result<String, String> {
    let endpoint = crate::chat::validate(connection)?;
    let input = match earlier {
        Some(earlier) => format!("Earlier summary:\n{earlier}\n\nNew messages:\n{transcript}"),
        None => format!("Messages:\n{transcript}"),
    };
    let body = json!({
        "model": connection.model.trim(),
        "messages": [
            {"role": "system", "content": INSTRUCTIONS},
            {"role": "user", "content": input},
        ],
        "stream": false,
        "max_tokens": 1500,
    });
    let mut request = post(endpoint)?.json(&body);
    if !connection.key.trim().is_empty() {
        request = request.bearer_auth(connection.key.trim());
    }
    let value: Value = request
        .send()
        .and_then(|r| r.error_for_status())
        .and_then(|r| r.json())
        .map_err(|e| e.to_string())?;
    crate::chat::unwrap_response(&value)?
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .ok_or_else(|| "No summary came back.".to_owned())
}

/// A summary request to `endpoint`, on the chat's connection but with a
/// timeout of its own.
fn post(endpoint: String) -> Result<reqwest::blocking::RequestBuilder, String> {
    // Without one the shared client would cut a summary off at reqwest's
    // default of 30 seconds.
    Ok(crate::chat::client()?
        .post(endpoint)
        .timeout(Duration::from_secs(60)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn said(role: &str, text: &str) -> Message {
        Message {
            role: role.into(),
            content: text.into(),
            ..Default::default()
        }
    }

    fn turns(count: usize) -> Vec<Message> {
        (0..count)
            .flat_map(|i| {
                [
                    said("user", &format!("question {i}")),
                    said("assistant", &format!("answer {i}")),
                ]
            })
            .collect()
    }

    #[test]
    fn a_summary_is_made_once_then_extended_with_only_what_is_new() {
        let cache = Cache::default();
        let messages = turns(4);
        let mut asked = Vec::new();

        let first = summary_in(&cache, &messages[..4], |earlier, transcript| {
            asked.push((earlier.map(str::to_owned), transcript.to_owned()));
            Ok("summary of two turns".into())
        });
        assert_eq!(first.as_deref(), Some("summary of two turns"));

        // The same stretch again comes from the cache.
        let again = summary_in(&cache, &messages[..4], |_, _| panic!("asked twice"));
        assert_eq!(again, first);

        // The window stepped on: the earlier summary is folded forward and
        // only the newly dropped turns are sent.
        let second = summary_in(&cache, &messages[..8], |earlier, transcript| {
            asked.push((earlier.map(str::to_owned), transcript.to_owned()));
            Ok("summary of four turns".into())
        });
        assert_eq!(second.as_deref(), Some("summary of four turns"));
        assert_eq!(asked[0].0, None);
        assert!(asked[0].1.contains("question 0"));
        assert_eq!(asked[1].0.as_deref(), Some("summary of two turns"));
        assert!(!asked[1].1.contains("question 1"), "{}", asked[1].1);
        assert!(asked[1].1.starts_with("User: question 2"), "{}", asked[1].1);
    }

    #[test]
    fn a_failed_summary_is_not_asked_for_again() {
        let cache = Cache::default();
        let messages = turns(2);
        assert!(summary_in(&cache, &messages, |_, _| Err("no".into())).is_none());
        assert!(summary_in(&cache, &messages, |_, _| panic!("asked again")).is_none());
        assert!(summary_in(&cache, &[], |_, _| panic!("nothing to summarize")).is_none());
    }

    #[test]
    fn a_long_transcript_keeps_its_most_recent_part() {
        let mut messages = vec![said("user", "the very first question")];
        messages.push(said("assistant", &"x".repeat(INPUT_CHARS)));
        messages.push(said("user", "the latest question"));
        let text = transcript(&messages);
        assert!(text.starts_with("[the earliest part is left out]"));
        assert!(!text.contains("the very first question"));
        assert!(text.contains("the latest question"));
    }

    #[test]
    fn a_summary_request_keeps_its_own_timeout() {
        let request = post("http://localhost:11434/v1/chat/completions".into())
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(request.timeout(), Some(&Duration::from_secs(60)));
    }
}
