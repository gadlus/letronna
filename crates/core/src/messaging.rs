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
const DISCORD_API: &str = "https://discord.com/api/v10";
const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// Server messages, DMs, and message text. Without the last, Discord blanks
/// the message a reply points at unless that one mentions the bot too.
const INTENTS: u64 = (1 << 9) | (1 << 12) | (1 << 15);
const INTENTS_WITHOUT_CONTENT: u64 = (1 << 9) | (1 << 12);
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
    /// Each chat's answer to whether its voice notes are transcribed, which
    /// costs credit, so it is asked once per chat; see `files::chat_key`.
    #[serde(default)]
    pub voice: std::collections::BTreeMap<String, bool>,
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

/// Two ways in. A webhook posts into a channel and cannot read anything
/// back, which is enough for reporting results. A bot is two-way over the
/// Gateway, but hears one user only: `owner` in a DM, and `owner` in a
/// listed server when the message mentions the bot or replies to it.
/// Anyone else, and any server not listed, is ignored.
#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Discord {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub webhook: String,
    #[serde(default)]
    pub bot_token: String,
    /// The one user ID the bot answers.
    #[serde(default)]
    pub owner: String,
    /// Servers where a mention reaches the bot. Empty means DMs only.
    #[serde(default)]
    pub servers: Vec<String>,
    /// As for Telegram: off unless the user says otherwise.
    #[serde(default)]
    pub allow_tools: bool,
}

impl Discord {
    /// The webhook can post.
    pub fn ready(&self) -> bool {
        self.enabled && self.webhook.trim().starts_with("https://")
    }

    /// The bot can listen and answer.
    pub fn bot_ready(&self) -> bool {
        self.enabled && !self.bot_token.trim().is_empty() && !self.owner.trim().is_empty()
    }
}

/// Where a message came from, and so where its answer goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Telegram(i64),
    /// A channel ID: a DM, or a server channel the bot was mentioned in.
    Discord(String),
}

/// A message that arrived from a chat service.
pub struct Incoming {
    pub source: Source,
    /// Who sent it, for the pairing prompt.
    pub from: String,
    /// The prompt: the sender's words, with any forward or replied-to
    /// message quoted in.
    pub text: String,
    /// The sender's own words alone, for commands and card answers, which a
    /// quoted reply would otherwise bury.
    pub said: String,
    /// Photos, documents and voice notes that came with it, fetched only
    /// once the app has checked who sent it.
    pub files: Vec<files::Received>,
    /// Where it came from, as the session is told it.
    pub remote: backend::Remote,
}

/// A card the app would show -- a question, or a tool waiting to be
/// allowed -- written out for a chat that cannot see it.
pub fn card_text(name: &str, arguments: &str, preview: &str) -> String {
    let args = crate::tools::parse_args_for(name, arguments).unwrap_or_default();
    if name == crate::tools::ASK_USER {
        let mut text = args["question"]
            .as_str()
            .unwrap_or("A question for you.")
            .to_owned();
        let options = card_options(&args);
        for (i, option) in options.iter().enumerate() {
            text.push_str(&format!("\n{}. {option}", i + 1));
        }
        text.push_str(match options.is_empty() {
            true => "\n\nReply with your answer.",
            false => "\n\nReply with a number, or your own answer.",
        });
        return text;
    }
    format!(
        "Letronna wants to use {name}:\n{}\n\nReply yes to allow it, or no and why not. \
         Anything else declines.",
        quote(&trim_to(preview, 800))
    )
}

pub enum CardAnswer {
    Answer(String),
    Allow,
    /// Declined, with the user's reason, empty when they gave none.
    Deny(String),
}

/// Read a chat reply as the answer to the card it was asked about. A number
/// picks that option; an approval needs a plain yes, so anything unclear
/// declines rather than runs, and what else was said goes with the no as the
/// reason.
pub fn card_answer(name: &str, arguments: &str, reply: &str) -> CardAnswer {
    let reply = reply.trim();
    if name == crate::tools::ASK_USER {
        let options =
            card_options(&crate::tools::parse_args_for(name, arguments).unwrap_or_default());
        let picked = reply
            .parse::<usize>()
            .ok()
            .and_then(|n| n.checked_sub(1))
            .and_then(|i| options.get(i));
        return CardAnswer::Answer(picked.cloned().unwrap_or_else(|| reply.to_owned()));
    }
    match reply.to_lowercase().as_str() {
        "yes" | "y" | "ok" | "allow" | "ya" | "iya" => CardAnswer::Allow,
        _ => CardAnswer::Deny(reason_after_no(reply).to_owned()),
    }
}

/// A declining reply without the no it opens with, as in "no, not that one".
fn reason_after_no(reply: &str) -> &str {
    let end = reply
        .find(|c: char| !c.is_alphabetic())
        .unwrap_or(reply.len());
    let (word, rest) = reply.split_at(end);
    match word.to_lowercase().as_str() {
        "no" | "n" | "nope" | "deny" | "tidak" => {
            rest.trim_start_matches(|c: char| c.is_whitespace() || ",.:;!-–—".contains(c))
        }
        _ => reply,
    }
}

fn card_options(args: &serde_json::Value) -> Vec<String> {
    args["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str())
        .map(str::to_owned)
        .collect()
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
        let _ = crate::store::write_private(&path(), &bytes);
    }
}

pub mod files;

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
    if config.discord.bot_ready() {
        let text = format!("**{title}**\n{body}");
        let sent = discord_open_dm(&config.discord.bot_token, &config.discord.owner)
            .and_then(|(channel, _)| discord_post(&config.discord.bot_token, &channel, &text));
        if let Err(error) = sent {
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
        // Group chats have negative IDs. Everyone in one reads the replies,
        // and the Bot API keeps no history for a bot to read back.
        let group = chat < 0;
        messages.push(Incoming {
            source: Source::Telegram(chat),
            from,
            text: text.to_owned(),
            said: text.to_owned(),
            files: Vec::new(),
            remote: backend::Remote {
                place: match group {
                    true => "a Telegram group".into(),
                    false => "a Telegram chat".into(),
                },
                shared: group,
                history: None,
            },
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
    let text = trim_reply(&outgoing(text));
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
            let (mut messages, next) = parse_updates(&body);
            messages.extend(files::telegram_media(&body));
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
    fn a_saved_key_never_reaches_a_chat_app() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data = std::env::temp_dir().join("letronna-messaging-scrub");
        let _ = std::fs::remove_dir_all(&data);
        std::fs::create_dir_all(&data).unwrap();
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        let key = "a-saved-key-of-no-known-shape";
        std::fs::write(data.join("keys.json"), format!(r#"{{"OpenAI":"{key}"}}"#)).unwrap();
        let marker = crate::redact::MARKER;

        let (api, worker) = one_request("200 OK");
        report_via(&api, &paired(), "Nightly", &format!("used {key}"));
        let (_, body) = worker.join().unwrap();
        assert_eq!(body["text"], format!("Nightly\nused {marker}"));
        // A question's card is built from the call's own arguments.
        let args = json!({ "question": format!("Use {key}?") }).to_string();
        let card = super::card_text(crate::tools::ASK_USER, &args, "");
        let (base, worker) = one_request("204 No Content");
        discord_send(&format!("{base}/api/webhooks/1/t"), &card).unwrap();
        let (_, body) = worker.join().unwrap();
        let sent = body["content"].as_str().unwrap_or_default();
        assert!(sent.starts_with(&format!("Use {marker}?")), "{sent}");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
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
        let failed = report(
            &config,
            "Letronna test",
            "A scheduled run would report here.",
        );
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
        assert_eq!(messages[0].source, super::Source::Telegram(42));
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
    fn a_telegram_group_is_shared_and_has_no_history() {
        let body = json!({"ok": true, "result": [
            {"update_id": 1, "message": {
                "chat": {"id": -100}, "from": {"first_name": "user_a"}, "text": "hi"}},
            {"update_id": 2, "message": {
                "chat": {"id": 42}, "from": {"first_name": "user_a"}, "text": "hi"}}
        ]});
        let (messages, _) = parse_updates(&body);
        assert!(messages[0].remote.shared);
        assert_eq!(messages[0].remote.place, "a Telegram group");
        assert!(!messages[1].remote.shared);
        assert!(messages.iter().all(|m| m.remote.history.is_none()));
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

/// `text` as it may leave for a chat app: other people read it there, so a
/// key or token Letronna holds is hidden, whether it came in a card, a
/// question's arguments or the model's reply.
fn outgoing(text: &str) -> String {
    crate::redact::scrub(text, false)
}

/// Post to a Discord channel. Send-only by nature: nothing comes back.
pub fn discord_send(webhook: &str, text: &str) -> Result<(), String> {
    let client = client().ok_or("Cannot initialize connection.")?;
    // Discord's own limit is 2000 characters.
    let text = trim_to(&outgoing(text), 1900);
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

fn discord_bot(
    method: reqwest::Method,
    token: &str,
    path: &str,
) -> Result<reqwest::blocking::RequestBuilder, String> {
    let client = client().ok_or("Cannot initialize connection.")?;
    Ok(client
        .request(method, format!("{DISCORD_API}{path}"))
        .header("Authorization", format!("Bot {}", token.trim())))
}

/// Confirm the bot token works and return the bot's name.
pub fn discord_bot_check(token: &str) -> Result<String, String> {
    let body: serde_json::Value = discord_bot(reqwest::Method::GET, token, "/users/@me")?
        .send()
        .map_err(|_| "Cannot reach Discord.".to_owned())?
        .json()
        .map_err(|_| "Discord sent an unreadable reply.".to_owned())?;
    if body["bot"].as_bool() != Some(true) {
        return Err("Discord did not accept that bot token.".into());
    }
    Ok(body["username"].as_str().unwrap_or("bot").to_owned())
}

/// Open (or find) the DM with the owner. Returns the channel and the newest
/// message already in it, so a new poller starts after the history.
/// Discord only lets a bot DM someone it shares a server with.
pub fn discord_open_dm(token: &str, owner: &str) -> Result<(String, Option<String>), String> {
    let body: serde_json::Value = discord_bot(reqwest::Method::POST, token, "/users/@me/channels")?
        .json(&serde_json::json!({"recipient_id": owner.trim()}))
        .send()
        .map_err(|_| "Cannot reach Discord.".to_owned())?
        .json()
        .map_err(|_| "Discord sent an unreadable reply.".to_owned())?;
    let channel = body["id"].as_str().ok_or_else(|| {
        "Discord would not open a DM with that user ID. The bot has to share a server \
         with you."
            .to_owned()
    })?;
    Ok((
        channel.to_owned(),
        body["last_message_id"].as_str().map(str::to_owned),
    ))
}

/// Send into a channel: the DM, or the server channel a mention came from.
pub fn discord_post(token: &str, channel: &str, text: &str) -> Result<(), String> {
    discord_bot(
        reqwest::Method::POST,
        token,
        &format!("/channels/{channel}/messages"),
    )?
    .json(&serde_json::json!({"content": trim_to(&outgoing(text), 1900)}))
    .send()
    .map_err(|_| "Cannot reach Discord.".to_owned())?
    .error_for_status()
    .map_err(|_| "Discord rejected the message.".to_owned())?;
    Ok(())
}

/// Whether a Gateway message is for this agent, and what it says. Only the
/// owner is heard: in a DM always, in a listed server only when the message
/// mentions the bot or replies to it, so sitting in a channel is not the
/// same as listening to it.
fn gateway_message(
    message: &serde_json::Value,
    config: &Discord,
    bot_id: &str,
) -> Option<Incoming> {
    let author = &message["author"];
    if author["id"].as_str() != Some(config.owner.trim()) || author["bot"].as_bool() == Some(true) {
        return None;
    }
    let server = message["guild_id"].as_str();
    if let Some(server) = server {
        if !config.servers.iter().any(|s| s.trim() == server) {
            return None;
        }
        let mentioned = message["mentions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|user| user["id"].as_str() == Some(bot_id));
        let replied_to_bot = message["referenced_message"]["author"]["id"].as_str() == Some(bot_id);
        if !mentioned && !replied_to_bot {
            return None;
        }
    }
    let channel = message["channel_id"].as_str()?.to_owned();
    let files = files::discord_attachments(message);
    let mut message = message.clone();
    message["content"] = serde_json::Value::String(readable(&message, bot_id, ""));
    Some(Incoming {
        source: Source::Discord(channel.clone()),
        from: author["username"].as_str().unwrap_or("you").to_owned(),
        text: message_text(&message, bot_id).or_else(|| (!files.is_empty()).then(String::new))?,
        said: own_text(&message).unwrap_or_default(),
        files,
        remote: backend::Remote {
            // The listener names the channel once it has looked it up.
            place: match server {
                Some(_) => "a Discord server channel".into(),
                None => "a Discord DM".into(),
            },
            shared: server.is_some(),
            history: Some(backend::History {
                token: config.bot_token.trim().to_owned(),
                channel,
                bot_id: bot_id.to_owned(),
                owner: config.owner.trim().to_owned(),
            }),
        },
    })
}

/// A server channel's name, for telling the model where it is.
fn channel_name(token: &str, channel: &str) -> Option<String> {
    let body: serde_json::Value =
        discord_bot(reqwest::Method::GET, token, &format!("/channels/{channel}"))
            .ok()?
            .send()
            .ok()?
            .json()
            .ok()?;
    body["name"].as_str().map(str::to_owned)
}

/// Mentions arrive as `<@id>`; each becomes a name the model can read. The
/// bot's own becomes `bot_as`: nothing in a prompt, where it is only how the
/// message was addressed, and a name in history, where it is part of what
/// was said.
fn readable(message: &serde_json::Value, bot_id: &str, bot_as: &str) -> String {
    let mut text = message["content"].as_str().unwrap_or_default().to_owned();
    for user in message["mentions"].as_array().into_iter().flatten() {
        let Some(id) = user["id"].as_str() else {
            continue;
        };
        let name = match id == bot_id {
            true => bot_as.to_owned(),
            false => format!("@{}", display_name(user, bot_id)),
        };
        text = text
            .replace(&format!("<@{id}>"), &name)
            .replace(&format!("<@!{id}>"), &name);
    }
    text.trim().to_owned()
}

/// What a message says, as the model should read it. A forward has no text
/// of its own -- it sits in a snapshot -- and a reply means little without
/// the message it answers, so both are quoted in after the user's own words.
fn message_text(message: &serde_json::Value, bot_id: &str) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(own) = own_text(message) {
        parts.push(own);
    }
    if let Some(forwarded) = forwarded_text(message) {
        parts.push(quoted_in("Forwarded message:", &forwarded, true));
    }
    if parts.is_empty() {
        return None;
    }
    let reference = &message["referenced_message"];
    let by = reference["author"]["id"].as_str();
    // A forward is always someone else's words; a message is, unless the
    // sender or Letronna wrote it.
    let (text, theirs) = match own_text(reference) {
        Some(text) => (
            Some(text),
            by != Some(bot_id) && by != message["author"]["id"].as_str(),
        ),
        None => (forwarded_text(reference), true),
    };
    if let Some(text) = text {
        let who = match by == Some(bot_id) {
            true => "your earlier message",
            false => reference["author"]["username"]
                .as_str()
                .unwrap_or("a message"),
        };
        parts.push(quoted_in(&format!("In reply to {who}:"), &text, theirs));
    }
    Some(parts.join("\n\n"))
}

/// `text` quoted in under `lead`, fenced when someone other than the sender
/// and Letronna wrote it: it is part of the prompt, but not the user's.
fn quoted_in(lead: &str, text: &str, theirs: bool) -> String {
    match theirs {
        true => crate::untrusted::quoted(lead, &quote(text)),
        false => format!("{lead}\n{}", quote(text)),
    }
}

fn own_text(message: &serde_json::Value) -> Option<String> {
    message["content"]
        .as_str()
        .filter(|t| !t.trim().is_empty())
        .map(str::to_owned)
}

fn forwarded_text(message: &serde_json::Value) -> Option<String> {
    own_text(&message["message_snapshots"][0]["message"])
}

fn quote(text: &str) -> String {
    text.lines()
        .map(|line| format!("> {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

enum Ended {
    Stopped,
    Closed(Option<u16>),
    Failed,
}

/// Hold a Gateway connection open until `stop` is set, reconnecting when it
/// drops. Runs on its own thread; each message for the agent goes down the
/// channel. Messages sent while reconnecting are missed rather than resumed:
/// a fresh session is simpler, and a missed one can be sent again.
pub fn discord_listen(
    config: Discord,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sender: async_channel::Sender<Incoming>,
) {
    std::thread::spawn(move || {
        use std::sync::atomic::Ordering;
        let mut intents = INTENTS;
        while !stop.load(Ordering::Relaxed) {
            match gateway_session(&config, intents, &stop, &sender) {
                Ended::Stopped => return,
                // The token is wrong; retrying cannot help.
                Ended::Closed(Some(4004)) => return,
                // Message Content is off in the Developer Portal. Mentions
                // still carry their own text, so carry on without it.
                Ended::Closed(Some(4014)) if intents == INTENTS => {
                    intents = INTENTS_WITHOUT_CONTENT;
                }
                _ => std::thread::sleep(std::time::Duration::from_secs(5)),
            }
        }
    });
}

fn gateway_session(
    config: &Discord,
    intents: u64,
    stop: &std::sync::atomic::AtomicBool,
    sender: &async_channel::Sender<Incoming>,
) -> Ended {
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};
    use tungstenite::{Message as Frame, stream::MaybeTlsStream};
    let Ok((mut socket, _)) = tungstenite::connect(GATEWAY) else {
        return Ended::Failed;
    };
    // Short reads, so the loop can heartbeat and notice `stop` between frames.
    if let MaybeTlsStream::Rustls(tls) = socket.get_ref() {
        let _ = tls.get_ref().set_read_timeout(Some(Duration::from_secs(1)));
    }
    let mut sequence = serde_json::Value::Null;
    let mut every: Option<Duration> = None;
    let mut next_beat = Instant::now();
    let mut acked = true;
    let mut bot_id = String::new();
    let mut names = std::collections::HashMap::<String, String>::new();
    loop {
        if stop.load(Ordering::Relaxed) {
            let _ = socket.close(None);
            return Ended::Stopped;
        }
        if let Some(every) = every
            && Instant::now() >= next_beat
        {
            // A heartbeat nobody acknowledged means the connection is gone,
            // even if TCP has not noticed yet.
            if !acked {
                return Ended::Failed;
            }
            let beat = serde_json::json!({"op": 1, "d": sequence});
            if socket.send(Frame::text(beat.to_string())).is_err() {
                return Ended::Failed;
            }
            acked = false;
            next_beat = Instant::now() + every;
        }
        let frame = match socket.read() {
            Ok(frame) => frame,
            Err(tungstenite::Error::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                continue;
            }
            Err(_) => return Ended::Failed,
        };
        let payload: serde_json::Value = match frame {
            Frame::Text(text) => match serde_json::from_str(text.as_str()) {
                Ok(payload) => payload,
                Err(_) => continue,
            },
            Frame::Close(close) => return Ended::Closed(close.map(|c| u16::from(c.code))),
            _ => continue,
        };
        if !payload["s"].is_null() {
            sequence = payload["s"].clone();
        }
        match payload["op"].as_u64() {
            // Hello: start heartbeating and say who this is.
            Some(10) => {
                let interval = payload["d"]["heartbeat_interval"]
                    .as_u64()
                    .unwrap_or(41_250);
                every = Some(Duration::from_millis(interval));
                next_beat = Instant::now() + Duration::from_millis(interval / 2);
                let identify = serde_json::json!({"op": 2, "d": {
                    "token": config.bot_token.trim(),
                    "intents": intents,
                    "properties": {
                        "os": std::env::consts::OS,
                        "browser": "letronna",
                        "device": "letronna"
                    }
                }});
                if socket.send(Frame::text(identify.to_string())).is_err() {
                    return Ended::Failed;
                }
            }
            Some(11) => acked = true,
            Some(1) => next_beat = Instant::now(),
            // Reconnect, or a session Discord will not keep: start over.
            Some(7) | Some(9) => return Ended::Failed,
            Some(0) => match payload["t"].as_str() {
                Some("READY") => {
                    bot_id = payload["d"]["user"]["id"]
                        .as_str()
                        .unwrap_or_default()
                        .to_owned();
                }
                Some("MESSAGE_CREATE") => {
                    let Some(mut message) = gateway_message(&payload["d"], config, &bot_id) else {
                        continue;
                    };
                    if let (true, Source::Discord(channel)) =
                        (message.remote.shared, &message.source)
                    {
                        let name = names.entry(channel.clone()).or_insert_with(|| {
                            channel_name(&config.bot_token, channel).unwrap_or_default()
                        });
                        if !name.is_empty() {
                            message.remote.place = format!("#{name} in a Discord server");
                        }
                    }
                    if sender.send_blocking(message).is_err() {
                        return Ended::Stopped;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

pub const READ_CHAT: &str = "read_chat";
/// Roughly what read_chat returns at most, in bytes. It stays under the tool
/// output cap, with room for the line heading it, on purpose: that cap cuts
/// the middle, and saves the whole aside, where a page of it read back would
/// come without its fences.
const HISTORY_BUDGET: usize = crate::chat::MAX_TOOL_CHARS - 100;

/// Reads the chat a remote session is happening in, including messages not
/// addressed to the bot. Discord only, and only the channel the session came
/// from: there is no argument to point it anywhere else.
pub struct ReadChat;

impl crate::tools::Tool for ReadChat {
    fn name(&self) -> &'static str {
        READ_CHAT
    }
    fn description(&self) -> &'static str {
        "Read recent messages in the chat this conversation is happening in, including \
         those not addressed to you. Oldest first, one per line: time, name, text."
    }
    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object", "properties": {
            "limit": {"type": "integer", "description": "How many of the latest messages, 1 to 500. Defaults to 100."}
        }})
    }
    fn machine(&self) -> bool {
        false
    }
    fn needs_history(&self) -> bool {
        true
    }
    fn run(
        &self,
        args: &serde_json::Value,
        ctx: &crate::tools::ToolContext,
    ) -> Result<String, String> {
        let history = ctx
            .remote
            .as_ref()
            .and_then(|r| r.history.as_ref())
            .ok_or("There is no chat to read in this session.")?;
        let limit = args["limit"].as_u64().unwrap_or(100).clamp(1, 500) as usize;
        let mut messages = Vec::new();
        let mut before: Option<String> = None;
        while messages.len() < limit {
            let want = (limit - messages.len()).min(100);
            let mut path = format!("/channels/{}/messages?limit={want}", history.channel);
            if let Some(id) = &before {
                path.push_str(&format!("&before={id}"));
            }
            let page: serde_json::Value = discord_bot(reqwest::Method::GET, &history.token, &path)?
                .send()
                .map_err(|_| "Cannot reach Discord.".to_owned())?
                .error_for_status()
                .map_err(|_| "Discord would not share this chat's messages.".to_owned())?
                .json()
                .map_err(|_| "Discord sent an unreadable reply.".to_owned())?;
            let page = page.as_array().cloned().unwrap_or_default();
            // Pages come newest first, so the last is where the next begins.
            before = page
                .last()
                .and_then(|m| m["id"].as_str())
                .map(str::to_owned);
            let done = page.len() < want;
            messages.extend(page);
            if done {
                break;
            }
        }
        render_history(
            &messages,
            (&history.bot_id, &history.owner),
            HISTORY_BUDGET,
            &chrono::Local,
        )
    }
}

/// Channel history as the model reads it: oldest first, one message per
/// line, the date only where the day changes, and each run of other
/// people's messages fenced. `own` is the bot's ID and the user's. Over
/// `budget` bytes the oldest lines go, since the recent end is what a
/// question is usually about.
fn render_history<Tz: chrono::TimeZone>(
    messages: &[serde_json::Value],
    own: (&str, &str),
    budget: usize,
    zone: &Tz,
) -> Result<String, String>
where
    Tz::Offset: std::fmt::Display,
{
    let mut ordered: Vec<&serde_json::Value> = messages.iter().collect();
    ordered.sort_by_key(|m| m["id"].as_str().and_then(|id| id.parse::<u64>().ok()));
    let lines: Vec<(String, String, bool)> = ordered
        .into_iter()
        .filter_map(|m| history_line(m, own, zone))
        .collect();
    if lines.is_empty() {
        return Ok("No messages in this chat yet.".into());
    }
    let mut used = 0;
    let mut day = None;
    let mut joins_a_fence = false;
    let kept = lines
        .iter()
        .rev()
        .take_while(|(date, line, theirs)| {
            used += line.len() + 1;
            // The day's heading, and one fence for each run of other
            // people's lines, counted going back from the newest.
            if day != Some(date) {
                used += date.len() + 7;
                day = Some(date);
            }
            if *theirs && !joins_a_fence {
                used += crate::untrusted::FENCE_BYTES + 1;
            }
            joins_a_fence = *theirs;
            used <= budget
        })
        .count();
    let left_out = lines.len() - kept;
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    let mut out = match left_out {
        0 => format!("{kept} message{}, oldest first.", plural(kept)),
        n => format!(
            "The last {kept} message{}, oldest first; {n} older one{} left out to fit.",
            plural(kept),
            plural(n)
        ),
    };
    let mut day = String::new();
    // Other people's lines not yet fenced.
    let mut run = String::new();
    for (date, line, theirs) in &lines[left_out..] {
        let mut text = String::new();
        if *date != day {
            text.push_str(&format!("-- {date} --\n"));
            day = date.clone();
        }
        text.push_str(line);
        if *theirs {
            run.push_str(&text);
            run.push('\n');
            continue;
        }
        fence_run(&mut out, &mut run)?;
        out.push('\n');
        out.push_str(&text);
    }
    fence_run(&mut out, &mut run)?;
    Ok(out)
}

/// `run`, other people's lines, fenced onto the end of `out`, and emptied.
fn fence_run(out: &mut String, run: &mut String) -> Result<(), String> {
    if run.is_empty() {
        return Ok(());
    }
    out.push('\n');
    out.push_str(&crate::untrusted::fence(run.trim_end(), usize::MAX)?);
    run.clear();
    Ok(())
}

/// One message as a line of history, with the day it was sent and whether
/// it is someone else's words, or `None` for one with nothing to read, such
/// as a member joining.
fn history_line<Tz: chrono::TimeZone>(
    message: &serde_json::Value,
    (bot_id, owner): (&str, &str),
    zone: &Tz,
) -> Option<(String, String, bool)>
where
    Tz::Offset: std::fmt::Display,
{
    let when = chrono::DateTime::parse_from_rfc3339(message["timestamp"].as_str()?)
        .ok()?
        .with_timezone(zone);
    let mut text = readable(message, bot_id, "@Letronna");
    if let Some(forwarded) = forwarded_text(message) {
        text = format!("{text} [forwarded] {forwarded}").trim().to_owned();
    }
    for file in message["attachments"].as_array().into_iter().flatten() {
        let name = file["filename"].as_str().unwrap_or("file");
        text.push_str(&format!(" [attachment: {name}]"));
    }
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let reference = &message["referenced_message"]["author"];
    let reply = match reference.is_object() {
        true => format!("(replying to {}) ", display_name(reference, bot_id)),
        false => String::new(),
    };
    // Later lines are indented, so every message still starts a line.
    let text = text.lines().collect::<Vec<_>>().join("\n    ");
    let author = display_name(&message["author"], bot_id);
    // Someone else wrote it, or it carries what someone else wrote.
    let by = message["author"]["id"].as_str();
    let theirs = (by != Some(bot_id) && by != Some(owner)) || forwarded_text(message).is_some();
    Some((
        when.format("%Y-%m-%d").to_string(),
        format!("{} {author}: {reply}{text}", when.format("%H:%M")),
        theirs,
    ))
}

/// The name a person goes by in Discord, or the bot as itself.
fn display_name(user: &serde_json::Value, bot_id: &str) -> String {
    if user["id"].as_str() == Some(bot_id) {
        return "Letronna (you)".into();
    }
    user["global_name"]
        .as_str()
        .or_else(|| user["username"].as_str())
        .unwrap_or("someone")
        .to_owned()
}

#[cfg(test)]
mod card_tests {
    use super::{CardAnswer, card_answer, card_text};

    const QUESTION: &str = r#"{"question": "Which one?", "options": ["A folder", "An app"]}"#;

    #[test]
    fn a_question_is_written_out_and_a_number_picks_an_option() {
        assert_eq!(
            card_text("ask_user", QUESTION, ""),
            "Which one?\n1. A folder\n2. An app\n\nReply with a number, or your own answer."
        );
        let answer = |reply| match card_answer("ask_user", QUESTION, reply) {
            CardAnswer::Answer(text) => text,
            _ => panic!("a question is answered, not allowed"),
        };
        assert_eq!(answer("2"), "An app");
        // Out of range, or not a number: the reply is the answer.
        assert_eq!(answer("3"), "3");
        assert_eq!(answer("a website"), "a website");
    }

    #[test]
    fn only_a_plain_yes_allows_a_tool() {
        let text = card_text("run_command", r#"{"command": "ls"}"#, "ls");
        assert!(text.starts_with("Letronna wants to use run_command:\n> ls"));
        assert!(matches!(
            card_answer("run_command", "{}", " Yes "),
            CardAnswer::Allow
        ));
        assert!(matches!(
            card_answer("run_command", "{}", "ya"),
            CardAnswer::Allow
        ));
        let reason = |reply| match card_answer("run_command", "{}", reply) {
            CardAnswer::Deny(reason) => reason,
            _ => panic!("{reply} should decline"),
        };
        assert_eq!(reason("yes but no"), "yes but no");
        assert_eq!(reason("what is it?"), "what is it?");
    }

    #[test]
    fn a_no_passes_on_what_follows_it_as_the_reason() {
        let reason = |reply| match card_answer("run_command", "{}", reply) {
            CardAnswer::Deny(reason) => reason,
            _ => panic!("{reply} should decline"),
        };
        assert_eq!(
            reason("No, use the staging database"),
            "use the staging database"
        );
        assert_eq!(reason("nope -- not on main"), "not on main");
        assert_eq!(reason(" no. "), "");
        assert_eq!(reason("n"), "");
        // A word that only starts like a no is part of the reason.
        assert_eq!(reason("not yet"), "not yet");
        assert_eq!(reason("nothing to add"), "nothing to add");
    }
}

#[cfg(test)]
mod discord_tests {
    use super::{Discord, Source, gateway_message, render_history};
    use crate::untrusted::without_ids;
    use serde_json::{Value, json};

    const BOT: &str = "99";

    fn config() -> Discord {
        Discord {
            enabled: true,
            bot_token: "t".into(),
            owner: "7".into(),
            servers: vec!["500".into()],
            ..Default::default()
        }
    }

    fn heard(message: Value) -> Option<String> {
        gateway_message(&message, &config(), BOT).map(|m| m.text)
    }

    #[test]
    fn a_dm_from_the_owner_is_heard_and_nobody_else() {
        let dm =
            json!({"channel_id": "dm1", "content": "hi", "author": {"id": "7", "username": "me"}});
        let message = gateway_message(&dm, &config(), BOT).unwrap();
        assert_eq!(message.source, Source::Discord("dm1".into()));
        assert_eq!(message.text, "hi");
        assert_eq!(
            heard(json!({"channel_id": "dm2", "content": "hi", "author": {"id": "8"}})),
            None
        );
        // The bot's own replies come back through the Gateway too.
        assert_eq!(
            heard(
                json!({"channel_id": "dm1", "content": "hi", "author": {"id": "7", "bot": true}})
            ),
            None
        );
    }

    #[test]
    fn a_server_needs_a_mention_or_a_reply_and_a_listed_server() {
        let mention = json!({
            "channel_id": "c1", "guild_id": "500", "content": "<@99> ask <@!8> about it",
            "author": {"id": "7", "username": "me"},
            "mentions": [{"id": "99", "username": "bot"}, {"id": "8", "username": "other"}]
        });
        assert_eq!(
            heard(mention.clone()).as_deref(),
            Some("ask @other about it")
        );

        let mut elsewhere = mention.clone();
        elsewhere["guild_id"] = json!("501");
        assert_eq!(heard(elsewhere), None);

        let mut stranger = mention;
        stranger["author"] = json!({"id": "8", "username": "other"});
        assert_eq!(heard(stranger), None);

        let chatter = json!({
            "channel_id": "c1", "guild_id": "500", "content": "just talking",
            "author": {"id": "7"}, "mentions": []
        });
        assert_eq!(heard(chatter), None);

        // Replying to the bot counts, even with the reply ping switched off.
        let reply = json!({
            "channel_id": "c1", "guild_id": "500", "content": "shorter please",
            "author": {"id": "7"}, "mentions": [],
            "referenced_message": {"content": "a long answer\nover two lines", "author": {"id": "99"}}
        });
        assert_eq!(
            heard(reply).as_deref(),
            Some(
                "shorter please\n\nIn reply to your earlier message:\n> a long answer\n> over two lines"
            )
        );
    }

    /// Shapes copied from real messages: a forward carries no content of its
    /// own, and a reply to someone else quotes them in.
    #[test]
    fn forwards_and_replies_arrive_with_their_text() {
        let forward = json!({
            "channel_id": "dm1", "content": "", "author": {"id": "7", "username": "me"},
            "message_reference": {"type": 1, "channel_id": "5", "message_id": "4"},
            "message_snapshots": [{"message": {"type": 19, "content": "ship it before the trip"}}]
        });
        assert_eq!(
            heard(forward.clone()).map(|t| without_ids(&t)).as_deref(),
            Some(
                "Forwarded message:\n<untrusted-content-ID>\n> ship it before the trip\n\
                 </untrusted-content-ID>"
            )
        );
        let reply_to_forward = json!({
            "channel_id": "dm1", "content": "is that right?", "author": {"id": "7"},
            "referenced_message": forward
        });
        assert_eq!(
            heard(reply_to_forward).map(|t| without_ids(&t)).as_deref(),
            Some(
                "is that right?\n\nIn reply to me:\n<untrusted-content-ID>\n\
                 > ship it before the trip\n</untrusted-content-ID>"
            )
        );
        let about_someone = json!({
            "channel_id": "c1", "guild_id": "500", "content": "<@99> is this true?",
            "author": {"id": "7"}, "mentions": [{"id": "99"}, {"id": "8", "username": "other"}],
            "referenced_message": {"content": "it ships friday", "author": {"id": "8", "username": "other"}}
        });
        assert_eq!(
            heard(about_someone).map(|t| without_ids(&t)).as_deref(),
            Some(
                "is this true?\n\nIn reply to other:\n<untrusted-content-ID>\n\
                 > it ships friday\n</untrusted-content-ID>"
            )
        );
        // The sender's own words are theirs, and are not fenced.
        let own = json!({
            "channel_id": "dm1", "content": "and this?", "author": {"id": "7"},
            "referenced_message": {"content": "my plan", "author": {"id": "7", "username": "me"}}
        });
        assert_eq!(
            heard(own).as_deref(),
            Some("and this?\n\nIn reply to me:\n> my plan")
        );
    }

    #[test]
    fn the_session_is_told_where_it_is() {
        let dm = json!({"channel_id": "dm1", "content": "hi", "author": {"id": "7"}});
        let remote = gateway_message(&dm, &config(), BOT).unwrap().remote;
        assert_eq!(remote.place, "a Discord DM");
        assert!(!remote.shared);
        let history = remote.history.unwrap();
        assert_eq!(
            (history.channel.as_str(), history.bot_id.as_str()),
            ("dm1", BOT)
        );

        let mention = json!({
            "channel_id": "c1", "guild_id": "500", "content": "<@99> hi",
            "author": {"id": "7"}, "mentions": [{"id": "99"}]
        });
        let remote = gateway_message(&mention, &config(), BOT).unwrap().remote;
        assert!(remote.shared);
        assert_eq!(remote.history.unwrap().channel, "c1");
    }

    #[test]
    fn history_reads_oldest_first_one_line_each() {
        // Newest first, as Discord pages them.
        let page = [
            json!({"id": "13", "timestamp": "2026-09-24T00:07:00+00:00", "type": 7,
                   "content": "", "author": {"id": "9", "username": "joiner"}}),
            json!({"id": "12", "timestamp": "2026-09-24T00:06:00+00:00", "content": "",
                   "author": {"id": "8", "username": "other"},
                   "message_snapshots": [{"message": {"content": "shipped"}}],
                   "attachments": [{"filename": "log.txt"}]}),
            json!({"id": "11", "timestamp": "2026-09-24T00:05:00+00:00", "content": "hi there",
                   "author": {"id": "99", "username": "bot", "bot": true},
                   "referenced_message": {"author": {"id": "7", "global_name": "Me"}}}),
            json!({"id": "10", "timestamp": "2026-09-23T23:50:00+00:00",
                   "content": "<@99> hello\nsecond line",
                   "author": {"id": "7", "username": "me", "global_name": "Me"},
                   "mentions": [{"id": "99", "username": "bot"}]}),
        ];
        let all = render_history(&page, (BOT, "7"), 10_000, &chrono::Utc).unwrap();
        // Only the other person's line is fenced: the user's and the bot's
        // are their own.
        assert_eq!(
            without_ids(&all),
            "3 messages, oldest first.\n\
             -- 2026-09-23 --\n\
             23:50 Me: @Letronna hello\n    second line\n\
             -- 2026-09-24 --\n\
             00:05 Letronna (you): (replying to Me) hi there\n\
             <untrusted-content-ID>\n\
             00:06 other: [forwarded] shipped [attachment: log.txt]\n\
             </untrusted-content-ID>"
        );
        // Over budget, the newest survive and the rest are counted.
        let cut = render_history(&page, (BOT, "7"), 150, &chrono::Utc).unwrap();
        assert_eq!(
            without_ids(&cut),
            "The last 1 message, oldest first; 2 older ones left out to fit.\n\
             <untrusted-content-ID>\n\
             -- 2026-09-24 --\n\
             00:06 other: [forwarded] shipped [attachment: log.txt]\n\
             </untrusted-content-ID>"
        );
        assert_eq!(
            render_history(&[], (BOT, "7"), 100, &chrono::Utc).unwrap(),
            "No messages in this chat yet."
        );
    }

    #[test]
    fn a_full_history_fits_one_result_with_its_fences() {
        // The user and someone else take turns, each on a day of their own,
        // so every other line opens a fence and a day.
        let page: Vec<Value> = (0..500)
            .map(|n| {
                json!({"id": n.to_string(), "content": "word ".repeat(40),
                       "timestamp": format!("2026-{:02}-{:02}T10:00:00+00:00", 1 + n / 28 % 12, 1 + n % 28),
                       "author": {"id": if n % 2 == 0 { "7" } else { "8" }, "username": "someone"}})
            })
            .collect();
        let out = render_history(&page, (BOT, "7"), super::HISTORY_BUDGET, &chrono::Utc).unwrap();
        assert!(out.len() <= crate::chat::MAX_TOOL_CHARS, "{}", out.len());
        assert!(out.matches("<untrusted-content-").count() > 10, "{out}");
    }

    /// Reads a real channel. `LETRONNA_TEST_DISCORD_BOT=<token>
    /// LETRONNA_TEST_DISCORD_CHANNEL=<id> cargo test -- --ignored`
    #[test]
    #[ignore]
    fn live_read_chat() {
        use crate::chat::{History, Remote};
        let env = |name| std::env::var(name).expect(name);
        let ctx = crate::tools::ToolContext {
            remote: Some(Remote {
                shared: true,
                history: Some(History {
                    token: env("LETRONNA_TEST_DISCORD_BOT"),
                    channel: env("LETRONNA_TEST_DISCORD_CHANNEL"),
                    bot_id: String::new(),
                    owner: String::new(),
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        let out = crate::tools::run("read_chat", r#"{"limit": 250}"#, &ctx).unwrap();
        // Only the summary line: the rest is other people's conversation.
        println!("{}", out.lines().next().unwrap());
        assert!(out.lines().count() > 1);
    }

    #[test]
    fn read_chat_needs_a_chat() {
        let refused = crate::tools::run("read_chat", "{}", &Default::default()).unwrap_err();
        assert_eq!(refused, "There is no chat to read in this session.");
    }
}
