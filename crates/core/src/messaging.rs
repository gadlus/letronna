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

use crate::chat::{self as backend};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

const API: &str = "https://api.telegram.org";
/// How long the server holds a poll open with no messages.
const POLL_SECONDS: u32 = 25;
/// Telegram rejects anything longer.
const MAX_REPLY: usize = 4000;

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub telegram: Telegram,
    #[serde(default)]
    pub discord: Discord,
}

/// Two-way: Telegram hands out messages over long polling, so Letronna can
/// both hear and answer without being reachable from the internet.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Telegram {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub token: String,
    /// The one chat allowed to talk to this agent.
    #[serde(default)]
    pub chat: Option<i64>,
    /// Let remote messages use tools that change things. Off by default: the
    /// approval card has no one to ask on the other end.
    #[serde(default)]
    pub allow_tools: bool,
}

impl Telegram {
    pub fn ready(&self) -> bool {
        self.enabled && !self.token.trim().is_empty()
    }
}

/// One-way. A webhook posts into a channel and cannot read anything back:
/// Discord has no polling endpoint for bots, so hearing replies would mean
/// holding a Gateway WebSocket open. Reporting results is what this is for.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Discord {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub webhook: String,
}

impl Discord {
    pub fn ready(&self) -> bool {
        self.enabled && self.webhook.trim().starts_with("https://")
    }
}

/// A message that arrived from Telegram.
pub struct Incoming {
    pub chat: i64,
    /// Who sent it, for the pairing prompt.
    pub from: String,
    pub text: String,
}

pub fn path() -> PathBuf {
    backend::data_dir().join("messaging.json")
}

pub fn load() -> Config {
    std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(config: &Config) {
    let _ = std::fs::create_dir_all(backend::data_dir());
    if let Ok(bytes) = serde_json::to_vec_pretty(config) {
        let _ = std::fs::write(path(), bytes);
    }
}

/// Tell every channel that can take it how a scheduled run ended. Nobody asked
/// for this message, so Telegram gets it only once a chat is paired: that is
/// the one chat the bot may talk to. Returns what failed to send.
pub fn report(config: &Config, title: &str, body: &str) -> Vec<String> {
    report_via(API, config, title, body)
}

fn report_via(api: &str, config: &Config, title: &str, body: &str) -> Vec<String> {
    let mut failed = Vec::new();
    if config.discord.ready() {
        let text = format!("**{title}**\n{body}");
        if let Err(error) = discord_send(&config.discord.webhook, &text) {
            failed.push(error);
        }
    }
    if let (true, Some(chat)) = (config.telegram.ready(), config.telegram.chat) {
        // Sent as plain text, where Markdown asterisks would show literally.
        let text = format!("{title}\n{body}");
        if let Err(error) = send_via(api, &config.telegram.token, chat, &text) {
            failed.push(error);
        }
    }
    failed
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
pub fn send(token: &str, chat: i64, text: &str) -> Result<(), String> {
    send_via(API, token, chat, text)
}

fn send_via(api: &str, token: &str, chat: i64, text: &str) -> Result<(), String> {
    let client = client().ok_or("Cannot initialize connection.")?;
    let text = trim_reply(text);
    client
        .post(format!("{api}/bot{}/sendMessage", token.trim()))
        .json(&serde_json::json!({"chat_id": chat, "text": text}))
        .send()
        .map_err(|_| "Cannot reach Telegram.".to_owned())?
        .error_for_status()
        .map_err(|_| "Telegram rejected the message.".to_owned())?;
    Ok(())
}

fn trim_reply(text: &str) -> String {
    trim_to(text, MAX_REPLY)
}

/// Every chat service caps message length; trim rather than lose the message.
fn trim_to(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        return text.to_owned();
    }
    let kept: String = text.chars().take(max).collect();
    format!("{kept}\n\u{2026} (trimmed)")
}

/// Confirm the token works and return the bot's name.
pub fn check(token: &str) -> Result<String, String> {
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
pub fn poll(
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
    use super::{Config, MAX_REPLY, discord_send, parse_updates, report, report_via, trim_reply};
    use serde_json::{Value, json};

    /// Accept one request, answer it with `status`, and hand back its path and
    /// JSON body.
    fn one_request(status: &str) -> (String, std::thread::JoinHandle<(String, Value)>) {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let status = status.to_owned();
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            socket
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = Vec::new();
            let mut block = [0; 4096];
            let (head, body) = loop {
                let n = socket.read(&mut block).unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&block[..n]);
                let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                let len: usize = head
                    .to_lowercase()
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: ").map(str::to_owned))
                    .unwrap()
                    .parse()
                    .unwrap();
                if bytes.len() >= end + 4 + len {
                    break (head, bytes[end + 4..end + 4 + len].to_vec());
                }
            };
            write!(
                socket,
                "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            )
            .unwrap();
            let path = head.split_whitespace().nth(1).unwrap().to_owned();
            (path, serde_json::from_slice(&body).unwrap())
        });
        (base, worker)
    }

    fn paired() -> Config {
        let mut config = Config::default();
        config.telegram.enabled = true;
        config.telegram.token = "123:abc".into();
        config.telegram.chat = Some(42);
        config
    }

    #[test]
    fn a_schedule_reports_to_the_paired_telegram_chat() {
        let (api, worker) = one_request("200 OK");
        let failed = report_via(&api, &paired(), "Nightly", "all green");
        assert!(failed.is_empty(), "{failed:?}");
        let (path, body) = worker.join().unwrap();
        assert_eq!(path, "/bot123:abc/sendMessage");
        assert_eq!(body["chat_id"], 42);
        // Plain text: Telegram would print Markdown asterisks as they are.
        assert_eq!(body["text"], "Nightly\nall green");
    }

    #[test]
    fn a_schedule_does_not_message_a_bot_nobody_paired() {
        let mut config = paired();
        config.telegram.chat = None;
        // An address that refuses connections: any attempt would fail loudly.
        let failed = report_via("http://127.0.0.1:1", &config, "Nightly", "ok");
        assert!(failed.is_empty());
        // Nothing set up at all is equally quiet.
        assert!(report(&Config::default(), "Nightly", "ok").is_empty());
    }

    #[test]
    fn a_rejected_report_says_so() {
        let (api, worker) = one_request("401 Unauthorized");
        let failed = report_via(&api, &paired(), "Nightly", "ok");
        worker.join().unwrap();
        assert_eq!(failed, ["Telegram rejected the message."]);
    }

    #[test]
    fn discord_gets_the_content_field() {
        let (base, worker) = one_request("204 No Content");
        discord_send(&format!("{base}/api/webhooks/1/t"), "**Nightly**\nok").unwrap();
        let (path, body) = worker.join().unwrap();
        assert_eq!(path, "/api/webhooks/1/t");
        assert_eq!(body["content"], "**Nightly**\nok");
    }

    /// Posts for real. `LETRONNA_TEST_DISCORD=<webhook> cargo test -- --ignored`
    #[test]
    #[ignore]
    fn live_discord_report() {
        let webhook = std::env::var("LETRONNA_TEST_DISCORD").expect("LETRONNA_TEST_DISCORD");
        super::discord_check(&webhook).unwrap();
        let mut config = Config::default();
        config.discord.enabled = true;
        config.discord.webhook = webhook;
        let failed = report(&config, "Letronna test", "A scheduled run would report here.");
        assert!(failed.is_empty(), "{failed:?}");
    }

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

/// Post to a Discord channel. Send-only by nature: nothing comes back.
pub fn discord_send(webhook: &str, text: &str) -> Result<(), String> {
    let client = client().ok_or("Cannot initialize connection.")?;
    // Discord's own limit is 2000 characters.
    let text = trim_to(text, 1900);
    client
        .post(webhook.trim())
        .json(&serde_json::json!({"content": text}))
        .send()
        .map_err(|_| "Cannot reach Discord.".to_owned())?
        .error_for_status()
        .map_err(|_| "Discord rejected the message.".to_owned())?;
    Ok(())
}

/// Confirm the webhook exists and say what it is called.
pub fn discord_check(webhook: &str) -> Result<String, String> {
    let url = webhook.trim();
    if !url.starts_with("https://") {
        return Err("Paste the full webhook URL, starting with https://".into());
    }
    let client = client().ok_or("Cannot initialize connection.")?;
    let body: serde_json::Value = client
        .get(url)
        .send()
        .map_err(|_| "Cannot reach Discord.".to_owned())?
        .json()
        .map_err(|_| "That does not look like a Discord webhook.".to_owned())?;
    body["id"]
        .as_str()
        .ok_or_else(|| "Discord did not recognise that webhook.".to_owned())?;
    Ok(body["name"].as_str().unwrap_or("webhook").to_owned())
}
