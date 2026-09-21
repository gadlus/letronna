//! Talking to Letronna from somewhere other than this window.
//!
//! Telegram, because it is the one channel a desktop app can use without
//! being reachable from the internet: long-polling `getUpdates` needs no
//! public address, no webhook and no tunnel. The app already lives in the
//! tray, so this makes it answerable from a phone.
//!
//! Two things are deliberately locked down. A bot is public the moment it
//! exists, so messages are ignored until the user approves one chat, and
//! nothing else is ever answered. And a remote message cannot see an approval
//! card, so remote sessions run in plan mode unless the user explicitly says
//! otherwise: the failure mode of getting that wrong is someone else running
//! commands on this machine.

use crate::prelude::*;

const API: &str = "https://api.telegram.org";
/// How long the server holds a poll open with no messages.
const POLL_SECONDS: u32 = 25;
/// Telegram rejects anything longer.
const MAX_REPLY: usize = 4000;

#[derive(Clone, Default, Serialize, Deserialize)]
pub(crate) struct Config {
    #[serde(default)]
    pub(crate) enabled: bool,
    #[serde(default)]
    pub(crate) token: String,
    /// The one chat allowed to talk to this agent.
    #[serde(default)]
    pub(crate) chat: Option<i64>,
    /// Let remote messages use tools that change things. Off by default: the
    /// approval card has no one to ask on the other end.
    #[serde(default)]
    pub(crate) allow_tools: bool,
}

impl Config {
    pub(crate) fn ready(&self) -> bool {
        self.enabled && !self.token.trim().is_empty()
    }
}

/// A message that arrived from Telegram.
pub(crate) struct Incoming {
    pub(crate) chat: i64,
    /// Who sent it, for the pairing prompt.
    pub(crate) from: String,
    pub(crate) text: String,
}

pub(crate) fn path() -> PathBuf {
    backend::data_dir().join("messaging.json")
}

pub(crate) fn load() -> Config {
    std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub(crate) fn save(config: &Config) {
    let _ = std::fs::create_dir_all(backend::data_dir());
    if let Ok(bytes) = serde_json::to_vec_pretty(config) {
        let _ = std::fs::write(path(), bytes);
    }
}

/// Pull the text messages out of a `getUpdates` response, with the offset to
/// ask for next. Pure, so the shape of the payload is pinned by a test rather
/// than discovered in production.
fn parse_updates(body: &serde_json::Value) -> (Vec<Incoming>, Option<u64>) {
    let mut messages = Vec::new();
    let mut highest = None;
    for update in body["result"].as_array().into_iter().flatten() {
        if let Some(id) = update["update_id"].as_u64() {
            highest = Some(highest.map_or(id, |seen: u64| seen.max(id)));
        }
        let message = &update["message"];
        let (Some(chat), Some(text)) = (
            message["chat"]["id"].as_i64(),
            message["text"].as_str().filter(|t| !t.trim().is_empty()),
        ) else {
            continue;
        };
        let from = message["from"]["first_name"]
            .as_str()
            .or_else(|| message["from"]["username"].as_str())
            .unwrap_or("someone")
            .to_owned();
        messages.push(Incoming {
            chat,
            from,
            text: text.to_owned(),
        });
    }
    (messages, highest.map(|id| id + 1))
}

fn client() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        // Comfortably longer than the long-poll, or every poll would time out.
        .timeout(std::time::Duration::from_secs(POLL_SECONDS as u64 + 15))
        .build()
        .ok()
}

/// Send a reply. Telegram caps message length, so long answers are trimmed
/// rather than silently dropped.
pub(crate) fn send(token: &str, chat: i64, text: &str) -> Result<(), String> {
    let client = client().ok_or("Cannot initialize connection.")?;
    let text = trim_reply(text);
    client
        .post(format!("{API}/bot{}/sendMessage", token.trim()))
        .json(&serde_json::json!({"chat_id": chat, "text": text}))
        .send()
        .map_err(|_| "Cannot reach Telegram.".to_owned())?
        .error_for_status()
        .map_err(|_| "Telegram rejected the message.".to_owned())?;
    Ok(())
}

fn trim_reply(text: &str) -> String {
    if text.chars().count() <= MAX_REPLY {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX_REPLY).collect();
    format!("{kept}\n… (trimmed)")
}

/// Confirm the token works and return the bot's name.
pub(crate) fn check(token: &str) -> Result<String, String> {
    let client = client().ok_or("Cannot initialize connection.")?;
    let body: serde_json::Value = client
        .get(format!("{API}/bot{}/getMe", token.trim()))
        .send()
        .map_err(|_| "Cannot reach Telegram.".to_owned())?
        .json()
        .map_err(|_| "Telegram sent an unreadable reply.".to_owned())?;
    if body["ok"].as_bool() != Some(true) {
        return Err("Telegram did not accept that bot token.".into());
    }
    Ok(body["result"]["username"]
        .as_str()
        .unwrap_or("bot")
        .to_owned())
}

/// Long-poll for messages until `stop` is set. Runs on its own thread; each
/// message goes down the channel for the app to deal with.
pub(crate) fn poll(
    token: String,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sender: async_channel::Sender<Incoming>,
) {
    std::thread::spawn(move || {
        use std::sync::atomic::Ordering;
        let Some(client) = client() else { return };
        let mut offset: Option<u64> = None;
        while !stop.load(Ordering::Relaxed) {
            let mut request = client
                .get(format!("{API}/bot{}/getUpdates", token.trim()))
                .query(&[("timeout", POLL_SECONDS.to_string())]);
            if let Some(offset) = offset {
                request = request.query(&[("offset", offset.to_string())]);
            }
            let Ok(response) = request.send() else {
                // Network blips are expected on a long poll; wait and retry
                // rather than giving up on the connection for good.
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            };
            let Ok(body) = response.json::<serde_json::Value>() else {
                std::thread::sleep(std::time::Duration::from_secs(5));
                continue;
            };
            let (messages, next) = parse_updates(&body);
            if let Some(next) = next {
                offset = Some(next);
            }
            for message in messages {
                if stop.load(Ordering::Relaxed) || sender.send_blocking(message).is_err() {
                    return;
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    // Not `use super::*`: the prelude re-exports gpui, whose `test` macro
    // would shadow the built-in attribute.
    use super::{MAX_REPLY, parse_updates, trim_reply};
    use serde_json::json;

    #[test]
    fn reads_messages_and_advances_the_offset() {
        let body = json!({"ok": true, "result": [
            {"update_id": 10, "message": {
                "chat": {"id": 42}, "from": {"first_name": "Wira"}, "text": "hello"}},
            // No text: a photo or a sticker is nothing to act on.
            {"update_id": 11, "message": {"chat": {"id": 42}, "from": {"first_name": "Wira"}}},
            // Blank text is equally useless.
            {"update_id": 12, "message": {
                "chat": {"id": 42}, "from": {"first_name": "Wira"}, "text": "   "}},
            {"update_id": 13, "message": {
                "chat": {"id": 7}, "from": {"username": "someone_else"}, "text": "hi"}}
        ]});
        let (messages, offset) = parse_updates(&body);
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].chat, 42);
        assert_eq!(messages[0].from, "Wira");
        assert_eq!(messages[0].text, "hello");
        // Falls back to the username when there is no first name.
        assert_eq!(messages[1].from, "someone_else");
        // Next poll must start past the highest update seen, including the
        // ones with nothing to act on, or they arrive again forever.
        assert_eq!(offset, Some(14));

        let (empty, none) = parse_updates(&json!({"ok": true, "result": []}));
        assert!(empty.is_empty());
        assert_eq!(none, None);
    }

    #[test]
    fn long_replies_are_trimmed_not_dropped() {
        let short = "all good";
        assert_eq!(trim_reply(short), short);
        let long = "x".repeat(MAX_REPLY + 500);
        let trimmed = trim_reply(&long);
        assert!(trimmed.chars().count() < MAX_REPLY + 20);
        assert!(trimmed.ends_with("… (trimmed)"));
    }
}
