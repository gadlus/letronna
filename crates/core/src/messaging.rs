//! Talking to Letronna from somewhere other than this window.
//!
//! Telegram, because it is the one channel a desktop app can use without
//! being reachable from the internet: long-polling `getUpdates` needs no
//! public address, no webhook and no tunnel. The app already lives in the
//! tray, so this makes it answerable from a phone.
//!
//! Two things are deliberately locked down. A bot is public the moment it
//! exists, so messages are ignored until the user pairs one chat, with a
//! code only they can send from it, and nothing else is ever answered. And a remote message cannot see an approval
//! card, so remote sessions run in plan mode unless the user explicitly says
//! otherwise: the failure mode of getting that wrong is someone else running
//! commands on this machine.

mod format;

use crate::chat::{self as backend};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod pairing;

const API: &str = "https://api.telegram.org";
const DISCORD_API: &str = "https://discord.com/api/v10";
const GATEWAY: &str = "wss://gateway.discord.gg/?v=10&encoding=json";
/// Server messages, DMs, and message text. Without the last, Discord blanks
/// the message a reply points at unless that one mentions the bot too.
const INTENTS: u64 = (1 << 9) | (1 << 12) | (1 << 15);
const INTENTS_WITHOUT_CONTENT: u64 = (1 << 9) | (1 << 12);
/// How long the server holds a poll open with no messages.
const POLL_SECONDS: u32 = 25;
/// The longest message Telegram takes, counted after its HTML is read.
const TELEGRAM_PART: usize = 4096;
/// The longest message Discord takes.
const DISCORD_PART: usize = 2000;
const SLACK_API: &str = "https://slack.com/api";
/// Slack asks for message text under 4,000 characters, so a longer reply
/// goes out in parts of at most that.
const SLACK_PART: usize = 4000;
/// Slack cuts a message off past 40,000 characters, and nobody reads more
/// in a chat: a reply is trimmed to that first, which is ten parts.
const SLACK_TOTAL: usize = 40_000;
/// The trim above allows ten parts; line breaks can make them shorter, so
/// more are dropped rather than posted a second apart.
const SLACK_PARTS: usize = 10;
/// How long a read waits for a frame. Slack does not document how often it
/// pings, so the first quiet spell is answered with a ping of our own and
/// only a second one in a row means the connection is gone; it is also how
/// soon the off switch is noticed.
const SLACK_QUIET: std::time::Duration = std::time::Duration::from_secs(15);
/// Slack sends an event again when it thinks the ack was lost; this many
/// recent event IDs cover its three retries with room to spare.
const SLACK_SEEN: usize = 256;
/// A rate-limited post waits what Slack asks, but no longer than this, so a
/// reply is not held back for minutes.
const SLACK_WAIT_MAX: std::time::Duration = std::time::Duration::from_secs(30);
/// The first wait after a failed connection. It doubles with each failure
/// in a row, up to `SLACK_RETRY_MAX`.
const SLACK_RETRY: std::time::Duration = std::time::Duration::from_secs(2);
/// Opening a socket is limited to a few calls a minute, so a longer outage
/// settles at one attempt a minute.
const SLACK_RETRY_MAX: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub telegram: Telegram,
    #[serde(default)]
    pub discord: Discord,
    #[serde(default)]
    pub slack: Slack,
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
    /// Who paired `chat`, by Telegram user ID: in a group, the one member
    /// whose answers count.
    #[serde(default)]
    pub owner: Option<i64>,
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

/// Two-way over Socket Mode: Slack pushes events down a WebSocket the app
/// opens, so, as with Telegram, nothing here has to be reachable from the
/// internet. It hears one member, paired from the window: in a DM with the
/// app, and where they mention it in a channel it was invited to.
#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Slack {
    #[serde(default)]
    pub enabled: bool,
    /// The app-level token (xapp-) that opens the socket.
    #[serde(default)]
    pub app_token: String,
    /// The bot token (xoxb-) that posts the replies.
    #[serde(default)]
    pub bot_token: String,
    /// The one member ID the app answers. Empty until a member sends the
    /// pairing code.
    #[serde(default)]
    pub owner: String,
    /// As for Telegram: off unless the user says otherwise.
    #[serde(default)]
    pub allow_tools: bool,
}

impl Slack {
    /// Both tokens are in, so the app can listen and answer.
    pub fn ready(&self) -> bool {
        self.enabled && !self.app_token.trim().is_empty() && !self.bot_token.trim().is_empty()
    }
}

/// Where a message came from, and so where its answer goes.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    Telegram(i64),
    /// A channel ID: a DM, or a server channel the bot was mentioned in.
    Discord(String),
    /// A channel ID, and the thread to answer in: the mention's own, or the
    /// one it was written in. A DM is answered in place unless it was
    /// written in a thread.
    Slack {
        channel: String,
        thread: Option<String>,
    },
}

impl Source {
    /// How often to say again that a reply is being written: a little
    /// sooner than the service stops showing it, after 5 seconds on
    /// Telegram and 10 on Discord.
    pub fn typing_every(&self) -> std::time::Duration {
        std::time::Duration::from_secs(match self {
            Source::Telegram(_) => 4,
            Source::Discord(_) => 8,
            // Bots have no typing indicator on Slack; `typing` says nothing there.
            Source::Slack { .. } => 60,
        })
    }
}

/// A tap on an approval card's Allow or Deny.
#[derive(Clone, Debug)]
pub struct Press {
    pub allow: bool,
    /// The call the card asked about, as much of its id as a button holds.
    call: String,
    tap: Tap,
}

/// What answering a tap takes.
#[derive(Clone, Debug)]
enum Tap {
    /// The query to answer, and the message whose buttons go.
    Telegram {
        query: String,
        chat: i64,
        message: i64,
    },
    /// The interaction to answer, which edits its message too.
    Discord { id: String, token: String },
}

impl Press {
    /// Whether the tap is on the card for the call `id`, rather than on an
    /// older card.
    pub fn answers(&self, id: &str) -> bool {
        self.call == button_key(id)
    }
}

/// A button's data: the answer, and the call it is for. The id leaves for
/// the chat app like any text, so it is scrubbed too; one that held a key
/// would make buttons that answer nothing, and a typed reply still would.
fn button(allow: bool, call: &str) -> String {
    let answer = if allow { "allow" } else { "deny" };
    format!("{answer}:{}", button_key(&outgoing(call)))
}

/// As much of a call's id as fits in Telegram's 64 bytes of button data
/// beside the longer answer (Discord allows 100).
fn button_key(call: &str) -> &str {
    let mut end = call.len().min(64 - "allow:".len());
    while !call.is_char_boundary(end) {
        end -= 1;
    }
    &call[..end]
}

/// A button's data read back, or `None` for data this app did not send.
fn pressed(data: &str) -> Option<(bool, String)> {
    let (answer, call) = data.split_once(':')?;
    match answer {
        "allow" => Some((true, call.to_owned())),
        "deny" => Some((false, call.to_owned())),
        _ => None,
    }
}

/// A message that arrived from a chat service.
pub struct Incoming {
    pub source: Source,
    /// Who sent it, by the name they go by; from Slack, the member ID, as
    /// its events carry no name.
    pub from: String,
    /// The prompt: the sender's words, with any forward or replied-to
    /// message quoted in.
    pub text: String,
    /// The sender's own words alone, for commands and card answers, which a
    /// quoted reply would otherwise bury.
    pub said: String,
    /// The user ID of whoever wrote `said`, as the service vouches for it;
    /// `None` for a forward, which is someone else's words, and for a post in
    /// a chat's name, which any of its admins could have sent.
    pub author: Option<String>,
    /// Photos, documents and voice notes that came with it, fetched only
    /// once the app has checked who sent it.
    pub files: Vec<files::Received>,
    /// Where it came from, as the session is told it.
    pub remote: backend::Remote,
    /// A tap on a card's button rather than words, which answers the card
    /// as a typed yes or no would.
    pub press: Option<Press>,
    /// Heard only if its source is already a session: a reply in a Slack
    /// channel thread that does not mention the app.
    pub needs_session: bool,
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
    // Fenced, so what would run shows exactly as written rather than with
    // its asterisks and underscores read as formatting.
    format!(
        "Letronna wants to use {name}:\n{}\n\nTap Allow or Deny, or reply yes, or no and why \
         not. Anything else declines.",
        format::fenced(&trim_to(preview, 800))
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
/// for this message, so Telegram and Slack get it only once a chat or member
/// is paired: that is the one the bot may talk to. Returns what failed to
/// send.
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
    if config.slack.ready() && !config.slack.owner.trim().is_empty() {
        let text = format!("*{title}*\n{body}");
        if let Err(error) = slack_post(&config.slack.bot_token, &config.slack.owner, None, &text) {
            failed.push(error);
        }
    }
    if let (true, Some(chat)) = (config.telegram.ready(), config.telegram.chat) {
        let text = format!("**{title}**\n{body}");
        if let Err(error) = send_via(api, &config.telegram.token, chat, &text, None) {
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
        if let Some(press) = telegram_press(&update["callback_query"]) {
            messages.push(press);
            continue;
        }
        let message = &update["message"];
        let (Some(chat), Some(text)) = (
            message["chat"]["id"].as_i64(),
            message["text"].as_str().filter(|t| !t.trim().is_empty()),
        ) else {
            continue;
        };
        let from = sender(&message["from"]);
        // Telegram sets `from` itself, so it cannot be spoofed; but a forward
        // carries someone else's words, and anonymous admins and channels
        // all post under one shared `from` with `sender_chat` set.
        let author = match message["forward_origin"].is_null() && message["sender_chat"].is_null() {
            true => message["from"]["id"].as_i64().map(|id| id.to_string()),
            false => None,
        };
        // Group chats have negative IDs. Everyone in one reads the replies,
        // and the Bot API keeps no history for a bot to read back.
        let group = chat < 0;
        messages.push(Incoming {
            source: Source::Telegram(chat),
            from,
            text: text.to_owned(),
            said: text.to_owned(),
            author,
            files: Vec::new(),
            remote: backend::Remote {
                place: match group {
                    true => "a Telegram group".into(),
                    false => "a Telegram chat".into(),
                },
                shared: group,
                history: None,
            },
            press: None,
            needs_session: false,
        });
    }
    (messages, highest.map(|id| id + 1))
}

fn sender(from: &serde_json::Value) -> String {
    from["first_name"]
        .as_str()
        .or_else(|| from["username"].as_str())
        .unwrap_or("someone")
        .to_owned()
}

/// A tap on one of the bot's card buttons, from a `callback_query`.
fn telegram_press(query: &serde_json::Value) -> Option<Incoming> {
    let (allow, call) = pressed(query["data"].as_str()?)?;
    let chat = query["message"]["chat"]["id"].as_i64()?;
    let tap = Tap::Telegram {
        query: query["id"].as_str()?.to_owned(),
        chat,
        message: query["message"]["message_id"].as_i64()?,
    };
    Some(Incoming {
        source: Source::Telegram(chat),
        from: sender(&query["from"]),
        author: query["from"]["id"].as_i64().map(|id| id.to_string()),
        text: String::new(),
        said: String::new(),
        remote: backend::Remote::default(),
        press: Some(Press { allow, call, tap }),
        needs_session: false,
        files: Vec::new(),
    })
}

fn client() -> Option<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        // Comfortably longer than the long-poll, or every poll would time out.
        .timeout(std::time::Duration::from_secs(POLL_SECONDS as u64 + 15))
        .build()
        .ok()
}

/// Send `text` to the chat `source` names, in as many messages as the
/// service needs. `card`, the id of a call waiting for approval, puts Allow
/// and Deny buttons under the last one; a typed yes or no still answers it.
pub fn tell(
    config: &Config,
    source: &Source,
    text: &str,
    card: Option<&str>,
) -> Result<(), String> {
    match source {
        Source::Telegram(chat) => send_via(API, &config.telegram.token, *chat, text, card),
        Source::Discord(channel) => {
            discord_post_via(DISCORD_API, &config.discord.bot_token, channel, text, card)
        }
        // No buttons: a typed yes or no answers the card.
        Source::Slack { channel, thread } => {
            slack_post(&config.slack.bot_token, channel, thread.as_deref(), text)
        }
    }
}

/// Send to a Telegram chat: in parts, as HTML, and with a card's buttons
/// under the last part.
fn send_via(
    api: &str,
    token: &str,
    chat: i64,
    text: &str,
    card: Option<&str>,
) -> Result<(), String> {
    let client = client().ok_or("Cannot initialize connection.")?;
    let url = format!("{api}/bot{}/sendMessage", token.trim());
    let parts = format::split(&outgoing(text), TELEGRAM_PART);
    let last = parts.len().saturating_sub(1);
    for (i, part) in parts.iter().enumerate() {
        let mut body = serde_json::json!({
            "chat_id": chat,
            "text": format::telegram_html(part),
            "parse_mode": "HTML",
        });
        if let (true, Some(call)) = (i == last, card) {
            body["reply_markup"] = serde_json::json!({"inline_keyboard": [[
                {"text": "Allow", "callback_data": button(true, call)},
                {"text": "Deny", "callback_data": button(false, call)},
            ]]});
        }
        let response = client
            .post(&url)
            .json(&body)
            .send()
            .map_err(|_| "Cannot reach Telegram.".to_owned())?;
        // A part Telegram will not take as HTML goes again as the text it was
        // made from, rather than not at all. Any refusal, not only a parse
        // error: a message refused is a message not sent, so nothing doubles.
        let response = match response.status() == reqwest::StatusCode::BAD_REQUEST {
            true => {
                body["text"] = part.as_str().into();
                if let Some(fields) = body.as_object_mut() {
                    fields.remove("parse_mode");
                }
                client
                    .post(&url)
                    .json(&body)
                    .send()
                    .map_err(|_| "Cannot reach Telegram.".to_owned())?
            }
            false => response,
        };
        response
            .error_for_status()
            .map_err(|_| "Telegram rejected the message.".to_owned())?;
    }
    Ok(())
}

/// Show the chat `source` names that a reply is being written. The service
/// stops showing it after a few seconds, or when the next message arrives.
pub fn typing(config: &Config, source: &Source) -> Result<(), String> {
    typing_via((API, DISCORD_API), config, source)
}

fn typing_via(
    (telegram, discord): (&str, &str),
    config: &Config,
    source: &Source,
) -> Result<(), String> {
    let request = match source {
        Source::Slack { .. } => return Ok(()),
        Source::Telegram(chat) => client()
            .ok_or("Cannot initialize connection.")?
            .post(format!(
                "{telegram}/bot{}/sendChatAction",
                config.telegram.token.trim()
            ))
            .json(&serde_json::json!({"chat_id": chat, "action": "typing"})),
        Source::Discord(channel) => discord_bot_via(
            discord,
            reqwest::Method::POST,
            &config.discord.bot_token,
            &format!("/channels/{channel}/typing"),
        )?
        .json(&serde_json::json!({})),
    };
    request
        .send()
        .map_err(|_| "Cannot reach the chat app.".to_owned())?
        .error_for_status()
        .map_err(|_| "The chat app refused the typing indicator.".to_owned())?;
    Ok(())
}

/// Answer a tap on a card's button and take the buttons away, so the card
/// cannot be answered twice. `answered` says whether the tap answered the
/// card, or came after it had been answered some other way.
pub fn settle(config: &Config, press: &Press, answered: bool) -> Result<(), String> {
    settle_via((API, DISCORD_API), config, press, answered)
}

fn settle_via(
    (telegram, discord): (&str, &str),
    config: &Config,
    press: &Press,
    answered: bool,
) -> Result<(), String> {
    use serde_json::json;
    let said = outgoing(match (answered, press.allow) {
        (false, _) => "Already answered",
        (true, true) => "Allowed",
        (true, false) => "Denied",
    });
    let client = client().ok_or("Cannot initialize connection.")?;
    let requests = match &press.tap {
        Tap::Telegram {
            query,
            chat,
            message,
        } => {
            let url = |method: &str| {
                format!("{telegram}/bot{}/{method}", config.telegram.token.trim())
            };
            // The first stops the button's spinner and says what came of the
            // tap; the second takes the buttons off the card.
            vec![
                client
                    .post(url("answerCallbackQuery"))
                    .json(&json!({"callback_query_id": query, "text": said})),
                client.post(url("editMessageReplyMarkup")).json(&json!({
                    "chat_id": chat,
                    "message_id": message,
                    "reply_markup": {"inline_keyboard": []}
                })),
            ]
        }
        // Answering an interaction edits its message too: the two buttons
        // become one that cannot be pressed, saying what came of the tap.
        Tap::Discord { id, token } => vec![
            client
                .post(format!("{discord}/interactions/{id}/{token}/callback"))
                .json(&json!({"type": 7, "data": {"components": [{"type": 1, "components": [
                    {"type": 2, "style": 2, "label": said, "custom_id": "answered", "disabled": true}
                ]}]}})),
        ],
    };
    // Each is tried whatever became of the one before: a tap too old to
    // answer still has buttons to take away.
    let sent: Vec<Result<(), String>> = requests
        .into_iter()
        .map(|request| {
            request
                .send()
                .map_err(|_| "Cannot reach the chat app.".to_owned())?
                .error_for_status()
                .map_err(|_| "The chat app refused the answer to a tap.".to_owned())?;
            Ok(())
        })
        .collect();
    sent.into_iter().collect()
}

/// `text` cut to `max` characters, saying so: a card's preview, which is
/// there to be glanced at on a phone.
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
                .query(&[("timeout", POLL_SECONDS.to_string())])
                // Said every time: Telegram keeps whatever was asked for
                // last, and taps on a card's buttons come as callback queries.
                .query(&[("allowed_updates", r#"["message","callback_query"]"#)]);
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
    use super::{
        Config, Source, discord_post_via, discord_send, parse_updates, report, report_via,
        send_via, settle_via, typing_via,
    };
    use serde_json::{Value, json};

    /// Accept one request, answer it with `status`, and hand back its path and
    /// JSON body.
    fn one_request(status: &str) -> (String, std::thread::JoinHandle<(String, Value)>) {
        let (base, worker) = requests(&[status]);
        (
            base,
            std::thread::spawn(move || worker.join().unwrap().remove(0)),
        )
    }

    /// Accept a request for each of `statuses` in turn, answer it with that
    /// status, and hand back the paths and JSON bodies in order: a fake of
    /// the chat app's API, so nothing reaches a real one.
    fn requests(statuses: &[&str]) -> (String, std::thread::JoinHandle<Vec<(String, Value)>>) {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let statuses: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();
        let worker = std::thread::spawn(move || {
            statuses
                .iter()
                .map(|status| answer(&server, status))
                .collect()
        });
        (base, worker)
    }

    fn answer(server: &std::net::TcpListener, status: &str) -> (String, Value) {
        use std::io::{Read, Write};
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
        // HTML: Telegram would print Markdown asterisks as they are.
        assert_eq!(body["text"], "<b>Nightly</b>\nall green");
        assert_eq!(body["parse_mode"], "HTML");
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
        let data = crate::store::tests::Scratch::new("messaging-scrub");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        let key = "a-saved-key-of-no-known-shape";
        std::fs::write(data.join("keys.json"), format!(r#"{{"OpenAI":"{key}"}}"#)).unwrap();
        let marker = crate::redact::MARKER;

        let (api, worker) = one_request("200 OK");
        report_via(&api, &paired(), "Nightly", &format!("used {key}"));
        let (_, body) = worker.join().unwrap();
        assert_eq!(body["text"], format!("<b>Nightly</b>\nused {marker}"));
        // A question's card is built from the call's own arguments.
        let args = json!({ "question": format!("Use {key}?") }).to_string();
        let card = super::card_text(crate::tools::ASK_USER, &args, "");
        let (base, worker) = one_request("204 No Content");
        discord_send(&format!("{base}/api/webhooks/1/t"), &card).unwrap();
        let (_, body) = worker.join().unwrap();
        let sent = body["content"].as_str().unwrap_or_default();
        assert!(sent.starts_with(&format!("Use {marker}?")), "{sent}");
        // Hidden before the reply is cut, so a key where a part ends is
        // not left in halves; and the plain text sent again is hidden too.
        let long = format!("{}{key} and more", "word ".repeat(818));
        let (api, worker) = requests(&["400 Bad Request", "200 OK", "200 OK"]);
        send_via(&api, "123:abc", 42, &long, None).unwrap();
        let sent = json!(worker.join().unwrap()).to_string();
        assert!(
            !sent.contains(&key[..12]) && sent.contains(marker),
            "{sent}"
        );
        // And a card with buttons, on a bot's channel.
        let (base, worker) = one_request("200 OK");
        discord_post_via(&base, "t", "99", &format!("run {key}"), Some("c1")).unwrap();
        let (_, body) = worker.join().unwrap();
        assert_eq!(body["content"], format!("run {marker}"));
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
    fn a_long_reply_goes_in_parts_with_its_code_block_whole_in_each() {
        let code: Vec<String> = (0..400).map(|n| format!("let x{n} = {n};")).collect();
        let text = format!("Here it is:\n```rust\n{}\n```", code.join("\n"));
        let (api, worker) = requests(&["200 OK", "200 OK"]);
        send_via(&api, "123:abc", 42, &text, None).unwrap();
        let sent = worker.join().unwrap();
        assert!(
            sent.iter()
                .all(|(path, _)| path == "/bot123:abc/sendMessage")
        );
        let texts: Vec<&str> = sent
            .iter()
            .map(|(_, body)| body["text"].as_str().unwrap_or_default())
            .collect();
        let block = "<pre><code class=\"language-rust\">";
        assert!(texts[0].starts_with(&format!("Here it is:\n{block}")));
        assert!(texts[1].starts_with(block));
        assert!(texts.iter().all(|text| text.ends_with("</code></pre>")));
    }

    #[test]
    fn a_part_telegram_cannot_parse_goes_again_as_plain_text() {
        let (api, worker) = requests(&["400 Bad Request", "200 OK"]);
        send_via(&api, "123:abc", 42, "**Done** <3", None).unwrap();
        let sent = worker.join().unwrap();
        assert_eq!(sent[0].1["text"], "<b>Done</b> &lt;3");
        assert_eq!(sent[0].1["parse_mode"], "HTML");
        assert_eq!(sent[1].1, json!({"chat_id": 42, "text": "**Done** <3"}));
        // Refused both ways is refused.
        let (api, worker) = requests(&["400 Bad Request", "400 Bad Request"]);
        let refused = send_via(&api, "123:abc", 42, "hi", None);
        worker.join().unwrap();
        assert_eq!(refused, Err("Telegram rejected the message.".into()));
    }

    #[test]
    fn a_card_carries_allow_and_deny_and_a_tap_reads_back_as_one() {
        let (api, worker) = one_request("200 OK");
        send_via(&api, "123:abc", 42, "Letronna wants to", Some("call_1")).unwrap();
        let (_, body) = worker.join().unwrap();
        let buttons = &body["reply_markup"]["inline_keyboard"][0];
        assert_eq!(
            buttons[0],
            json!({"text": "Allow", "callback_data": "allow:call_1"})
        );
        assert_eq!(
            buttons[1],
            json!({"text": "Deny", "callback_data": "deny:call_1"})
        );
        let (base, worker) = one_request("200 OK");
        discord_post_via(&base, "t", "99", "Letronna wants to", Some("call_1")).unwrap();
        let (path, body) = worker.join().unwrap();
        assert_eq!(path, "/channels/99/messages");
        let buttons = &body["components"][0]["components"];
        assert_eq!(buttons[0]["custom_id"], "allow:call_1");
        assert_eq!(buttons[1]["custom_id"], "deny:call_1");

        let tap = |data: &str| {
            json!({"ok": true, "result": [{"update_id": 5, "callback_query": {
                "id": "q1", "from": {"first_name": "user_a"}, "data": data,
                "message": {"message_id": 7, "chat": {"id": 42}}}}]})
        };
        let (taps, offset) = parse_updates(&tap("deny:call_1"));
        assert_eq!(offset, Some(6));
        assert_eq!(taps[0].source, Source::Telegram(42));
        let press = taps[0].press.as_ref().unwrap();
        // It answers its own card, not whichever card waits now.
        assert!(!press.allow && press.answers("call_1") && !press.answers("call_2"));
        // A long id is cut to fit the button, and still answers.
        let long = "x".repeat(100);
        let data = super::button(true, &long);
        assert_eq!(data.len(), 64);
        let (taps, _) = parse_updates(&tap(&data));
        assert!(
            taps[0]
                .press
                .as_ref()
                .is_some_and(|p| p.allow && p.answers(&long))
        );
        // Data this app never sent is nothing to act on.
        assert!(parse_updates(&tap("run:rm -rf")).0.is_empty());
    }

    #[test]
    fn a_tap_is_answered_and_its_buttons_taken_away() {
        let tap = json!({"ok": true, "result": [{"update_id": 1, "callback_query": {
            "id": "q1", "from": {}, "data": "allow:c1",
            "message": {"message_id": 7, "chat": {"id": 42}}}}]});
        let press = parse_updates(&tap).0.remove(0).press.unwrap();
        let (api, worker) = requests(&["200 OK", "200 OK"]);
        settle_via((&api, "http://127.0.0.1:1"), &paired(), &press, true).unwrap();
        let sent = worker.join().unwrap();
        assert_eq!(sent[0].0, "/bot123:abc/answerCallbackQuery");
        assert_eq!(
            sent[0].1,
            json!({"callback_query_id": "q1", "text": "Allowed"})
        );
        assert_eq!(sent[1].0, "/bot123:abc/editMessageReplyMarkup");
        assert_eq!(sent[1].1["reply_markup"], json!({"inline_keyboard": []}));
        assert_eq!(
            (&sent[1].1["chat_id"], &sent[1].1["message_id"]),
            (&json!(42), &json!(7))
        );
        // A tap too old to answer still loses its buttons.
        let (api, worker) = requests(&["400 Bad Request", "200 OK"]);
        let settled = settle_via((&api, "http://127.0.0.1:1"), &paired(), &press, true);
        assert!(settled.is_err());
        assert_eq!(
            worker.join().unwrap()[1].0,
            "/bot123:abc/editMessageReplyMarkup"
        );

        // On Discord, answering the interaction is what edits the card.
        let discord = super::Discord {
            owner: "1".into(),
            ..Default::default()
        };
        let interaction = json!({"type": 3, "id": "i1", "token": "tok", "channel_id": "99",
            "user": {"id": "1"}, "data": {"custom_id": "deny:c1"}});
        let press = super::gateway_press(&interaction, &discord)
            .unwrap()
            .press
            .unwrap();
        let (base, worker) = one_request("204 No Content");
        settle_via(("http://127.0.0.1:1", &base), &paired(), &press, false).unwrap();
        let (path, body) = worker.join().unwrap();
        assert_eq!(path, "/interactions/i1/tok/callback");
        assert_eq!(body["type"], 7);
        let button = &body["data"]["components"][0]["components"][0];
        assert_eq!(
            (&button["label"], &button["disabled"]),
            (&json!("Already answered"), &json!(true))
        );
    }

    #[test]
    fn typing_shows_in_the_chat_the_turn_is_for() {
        let (api, worker) = one_request("200 OK");
        typing_via(
            (&api, "http://127.0.0.1:1"),
            &paired(),
            &Source::Telegram(42),
        )
        .unwrap();
        let (path, body) = worker.join().unwrap();
        assert_eq!(path, "/bot123:abc/sendChatAction");
        assert_eq!(body, json!({"chat_id": 42, "action": "typing"}));
        let (base, worker) = one_request("204 No Content");
        let channel = Source::Discord("99".into());
        typing_via(("http://127.0.0.1:1", &base), &paired(), &channel).unwrap();
        assert_eq!(worker.join().unwrap().0, "/channels/99/typing");
    }

    #[test]
    fn discord_gets_a_long_report_in_parts_under_its_limit() {
        let text = vec!["word ".repeat(60); 12].join("\n\n");
        let (base, worker) = requests(&["204 No Content", "204 No Content"]);
        discord_send(&format!("{base}/api/webhooks/1/t"), &text).unwrap();
        for (_, body) in worker.join().unwrap() {
            let content = body["content"].as_str().unwrap_or_default();
            assert!((1..=2000).contains(&content.encode_utf16().count()));
        }
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
    for part in format::split(&outgoing(text), DISCORD_PART) {
        client
            .post(webhook.trim())
            .json(&serde_json::json!({"content": part}))
            .send()
            .map_err(|_| "Cannot reach Discord.".to_owned())?
            .error_for_status()
            .map_err(|_| "Discord rejected the message.".to_owned())?;
    }
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
    discord_bot_via(DISCORD_API, method, token, path)
}

fn discord_bot_via(
    api: &str,
    method: reqwest::Method,
    token: &str,
    path: &str,
) -> Result<reqwest::blocking::RequestBuilder, String> {
    let client = client().ok_or("Cannot initialize connection.")?;
    Ok(client
        .request(method, format!("{api}{path}"))
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
    discord_post_via(DISCORD_API, token, channel, text, None)
}

/// Post in parts, with a card's buttons under the last part.
fn discord_post_via(
    api: &str,
    token: &str,
    channel: &str,
    text: &str,
    card: Option<&str>,
) -> Result<(), String> {
    let parts = format::split(&outgoing(text), DISCORD_PART);
    let last = parts.len().saturating_sub(1);
    for (i, part) in parts.into_iter().enumerate() {
        let mut body = serde_json::json!({"content": part});
        if let (true, Some(call)) = (i == last, card) {
            // Green and red, as Discord colours a yes and a no.
            body["components"] = serde_json::json!([{"type": 1, "components": [
                {"type": 2, "style": 3, "label": "Allow", "custom_id": button(true, call)},
                {"type": 2, "style": 4, "label": "Deny", "custom_id": button(false, call)},
            ]}]);
        }
        let path = format!("/channels/{channel}/messages");
        discord_bot_via(api, reqwest::Method::POST, token, &path)?
            .json(&body)
            .send()
            .map_err(|_| "Cannot reach Discord.".to_owned())?
            .error_for_status()
            .map_err(|_| "Discord rejected the message.".to_owned())?;
    }
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
        // Only the owner gets this far, but a forward is someone else's words.
        author: match message["message_snapshots"]
            .as_array()
            .is_some_and(|snapshots| !snapshots.is_empty())
        {
            true => None,
            false => author["id"].as_str().map(str::to_owned),
        },
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
        press: None,
        needs_session: false,
    })
}

/// A tap on one of the bot's card buttons, heard as its messages are: from
/// the owner only, in a DM or a listed server.
fn gateway_press(interaction: &serde_json::Value, config: &Discord) -> Option<Incoming> {
    // A message component; slash commands and the rest are not this bot's.
    if interaction["type"].as_u64() != Some(3) {
        return None;
    }
    // In a server the user comes inside the member, in a DM on its own.
    let user = match interaction["member"]["user"].is_object() {
        true => &interaction["member"]["user"],
        false => &interaction["user"],
    };
    if user["id"].as_str() != Some(config.owner.trim()) {
        return None;
    }
    if let Some(server) = interaction["guild_id"].as_str()
        && !config.servers.iter().any(|s| s.trim() == server)
    {
        return None;
    }
    let (allow, call) = pressed(interaction["data"]["custom_id"].as_str()?)?;
    let tap = Tap::Discord {
        id: interaction["id"].as_str()?.to_owned(),
        token: interaction["token"].as_str()?.to_owned(),
    };
    Some(Incoming {
        source: Source::Discord(interaction["channel_id"].as_str()?.to_owned()),
        from: user["username"].as_str().unwrap_or("you").to_owned(),
        author: user["id"].as_str().map(str::to_owned),
        text: String::new(),
        said: String::new(),
        remote: backend::Remote::default(),
        press: Some(Press { allow, call, tap }),
        needs_session: false,
        files: Vec::new(),
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
                // Arrives here as long as the app has no interactions URL
                // of its own set in the Developer Portal.
                Some("INTERACTION_CREATE") => {
                    let Some(press) = gateway_press(&payload["d"], config) else {
                        continue;
                    };
                    if sender.send_blocking(press).is_err() {
                        return Ended::Stopped;
                    }
                }
                _ => {}
            },
            _ => {}
        }
    }
}

/// Slack's errors that no retry will fix: the token is wrong or gone.
const SLACK_REFUSED: &[&str] = &[
    "invalid_auth",
    "not_authed",
    "account_inactive",
    "token_revoked",
    "token_expired",
    "not_allowed_token_type",
];

/// Call a Slack Web API method. Slack answers most failures with 200 and
/// `"ok": false`, so the error is its own code, `ratelimited` and the
/// seconds to wait, or `unreachable` or `unreadable` when there was no
/// answer to read.
fn slack_call(
    api: &str,
    method: &str,
    token: &str,
    body: Option<&serde_json::Value>,
) -> Result<serde_json::Value, String> {
    let client = client().ok_or("unreachable")?;
    let mut request = client
        .post(format!("{api}/{method}"))
        .bearer_auth(token.trim());
    if let Some(body) = body {
        request = request.json(body);
    }
    let response = request.send().map_err(|_| "unreachable".to_owned())?;
    if response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let seconds = response
            .headers()
            .get("retry-after")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(1);
        return Err(format!("ratelimited {seconds}"));
    }
    let reply: serde_json::Value = response.json().map_err(|_| "unreadable".to_owned())?;
    match reply["ok"].as_bool() {
        Some(true) => Ok(reply),
        _ => Err(reply["error"].as_str().unwrap_or("unreadable").to_owned()),
    }
}

/// How long a rate-limited call asked to wait, capped at `SLACK_WAIT_MAX`.
fn slack_retry_after(code: &str) -> Option<std::time::Duration> {
    let seconds = code.strip_prefix("ratelimited ")?.parse().ok()?;
    Some(std::time::Duration::from_secs(seconds).min(SLACK_WAIT_MAX))
}

/// A Slack error code as the person setting the app up can act on it.
fn slack_error(code: &str) -> String {
    match code {
        "unreachable" => "Cannot reach Slack.".into(),
        "unreadable" => "Slack sent an unreadable reply.".into(),
        "not_allowed_token_type" => "That is the wrong kind of token: the app-level token \
                                     starts xapp-, the bot token xoxb-."
            .into(),
        "missing_scope" => "The Slack app is missing a permission. Give the bot the scopes \
                            chat:write, im:history, app_mentions:read, channels:history \
                            and groups:history, then reinstall it."
            .into(),
        code if code.starts_with("ratelimited") => {
            "Slack is limiting how often the app can call it. Try again in a minute.".into()
        }
        "not_in_channel" | "channel_not_found" => {
            "The Slack app is not in that channel. Invite it first.".into()
        }
        code if SLACK_REFUSED.contains(&code) => "Slack did not accept that token.".into(),
        code => format!("Slack refused: {code}."),
    }
}

/// Confirm both tokens work before they are kept, and return the bot's
/// name. Opening a socket is the one way to try the app-level token; the
/// address it hands back is simply not used.
pub fn slack_check(app_token: &str, bot_token: &str) -> Result<String, String> {
    if !app_token.trim().starts_with("xapp-") {
        return Err(
            "The app-level token starts with xapp-. Make one under Basic Information, \
             with the connections:write scope."
                .into(),
        );
    }
    if !bot_token.trim().starts_with("xoxb-") {
        return Err(
            "The bot token starts with xoxb-. It is under OAuth & Permissions once the \
             app is installed."
                .into(),
        );
    }
    let (name, _) = slack_auth(SLACK_API, bot_token).map_err(|code| slack_error(&code))?;
    slack_socket_url(SLACK_API, app_token).map_err(|code| slack_error(&code))?;
    Ok(name)
}

/// The bot's name and member ID. The error is Slack's code.
fn slack_auth(api: &str, bot_token: &str) -> Result<(String, String), String> {
    let reply = slack_call(api, "auth.test", bot_token, None)?;
    Ok((
        reply["user"].as_str().unwrap_or("bot").to_owned(),
        reply["user_id"].as_str().unwrap_or_default().to_owned(),
    ))
}

/// A fresh socket address, good for one connection. The error is Slack's
/// code, so a refused token can be told from a network blip.
fn slack_socket_url(api: &str, app_token: &str) -> Result<String, String> {
    let reply = slack_call(api, "apps.connections.open", app_token, None)?;
    reply["url"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "unreadable".to_owned())
}

/// Post a reply where the message came from: the DM, or the thread under a
/// mention. A long reply goes out in parts rather than being cut off.
pub fn slack_post(
    bot_token: &str,
    channel: &str,
    thread: Option<&str>,
    text: &str,
) -> Result<(), String> {
    slack_post_via(SLACK_API, bot_token, channel, thread, text)
}

fn slack_post_via(
    api: &str,
    bot_token: &str,
    channel: &str,
    thread: Option<&str>,
    text: &str,
) -> Result<(), String> {
    for (index, part) in slack_parts(text).into_iter().enumerate() {
        // Slack takes about one message a second into a channel.
        if index > 0 {
            std::thread::sleep(std::time::Duration::from_secs(1));
        }
        // A link the model writes is not fetched by Slack's servers unasked.
        let mut body = serde_json::json!({
            "channel": channel,
            "text": part,
            "unfurl_links": false,
            "unfurl_media": false,
        });
        if let Some(thread) = thread {
            body["thread_ts"] = thread.into();
        }
        let mut sent = slack_call(api, "chat.postMessage", bot_token, Some(&body));
        // A burst of parts can be rate limited: wait as asked, once, rather
        // than lose the rest of the reply.
        if let Some(wait) = sent.as_ref().err().and_then(|code| slack_retry_after(code)) {
            std::thread::sleep(wait);
            sent = slack_call(api, "chat.postMessage", bot_token, Some(&body));
        }
        sent.map_err(|code| slack_error(&code))?;
    }
    Ok(())
}

/// `text` as Slack messages: keys hidden, the three characters its markup
/// reserves escaped, trimmed so the escaped text is what Slack keeps, and
/// split into at most `SLACK_PARTS` it takes whole, at a line break where
/// there is one.
fn slack_parts(text: &str) -> Vec<String> {
    let text = slack_trimmed(&outgoing(text));
    let mut parts = Vec::new();
    let mut rest = text.as_str();
    while let Some((end, _)) = rest.char_indices().nth(SLACK_PART) {
        let window = &rest[..end];
        // Never inside an escape: half of `&lt;` shows as garbage.
        let whole = match window.rfind('&') {
            Some(amp) if !window[amp..].contains(';') => amp,
            _ => end,
        };
        let cut = match window[..whole].rfind('\n') {
            Some(line) if line >= whole / 2 => line + 1,
            _ => whole,
        };
        parts.push(rest[..cut].to_owned());
        rest = &rest[cut..];
    }
    parts.push(rest.to_owned());
    if parts.len() > SLACK_PARTS {
        parts.truncate(SLACK_PARTS);
        parts[SLACK_PARTS - 1].push_str("\n\u{2026} (trimmed)");
    }
    parts
}

/// `text` escaped, cut where its escaped form would pass `SLACK_TOTAL`, so
/// an entity is never split and a reply full of `&` is not five times over.
fn slack_trimmed(text: &str) -> String {
    let mut length = 0;
    for (at, c) in text.char_indices() {
        length += match c {
            '&' => 5,
            '<' | '>' => 4,
            _ => 1,
        };
        if length > SLACK_TOTAL {
            return slack_escape(&format!("{}\n\u{2026} (trimmed)", &text[..at]));
        }
    }
    slack_escape(text)
}

/// Slack reads `&`, `<` and `>` as markup, so they go as entities.
fn slack_escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Slack's markup made readable: a mention as @ and the ID, a channel as #
/// and its name, a link as its address, the app's own mention dropped, and
/// the escaped `&`, `<` and `>` put back.
fn slack_text(raw: &str, bot_user: &str) -> String {
    let mut out = String::new();
    let mut rest = raw;
    while let Some(open) = rest.find('<') {
        let Some(len) = rest[open..].find('>') else {
            break;
        };
        out.push_str(&rest[..open]);
        let inner = &rest[open + 1..open + len];
        let (target, label) = match inner.split_once('|') {
            Some((target, label)) => (target, Some(label)),
            None => (inner, None),
        };
        if let Some(user) = target.strip_prefix('@') {
            if user != bot_user {
                out.push('@');
                out.push_str(label.unwrap_or(user));
            }
        } else if let Some(channel) = target.strip_prefix('#') {
            out.push('#');
            out.push_str(label.unwrap_or(channel));
        } else if let Some(special) = target.strip_prefix('!') {
            // @here and the like; a date or a user group carries its own text.
            match label {
                Some(label) => out.push_str(label),
                None => {
                    out.push('@');
                    out.push_str(special);
                }
            }
        } else {
            out.push_str(target);
        }
        rest = &rest[open + len + 1..];
    }
    out.push_str(rest);
    out.trim()
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

/// Whether a Socket Mode event is for this agent, and what it says. Anything
/// from a bot, this app's own replies included, and edits, joins and every
/// other subtype but a file share and a thread reply also sent to the channel
/// are dropped, so the app never answers itself. Until a member is paired a
/// DM from anyone comes through, so it can bring the pairing code; after
/// that, and in channels always, only the paired member is heard.
fn slack_event(event: &serde_json::Value, config: &Slack, bot_user: &str) -> Option<Incoming> {
    let files = match event["subtype"].as_str() {
        None | Some("thread_broadcast") => false,
        Some("file_share") => true,
        Some(_) => return None,
    };
    if !event["bot_id"].is_null() {
        return None;
    }
    let user = event["user"].as_str().filter(|user| *user != bot_user)?;
    let channel = event["channel"].as_str()?;
    let raw = event["text"].as_str().unwrap_or_default();
    let thread = event["thread_ts"].as_str();
    let (dm, needs_session) = match event["type"].as_str()? {
        "message" if event["channel_type"].as_str() == Some("im") => (true, false),
        // A mention in a DM arrives as a message too; taking both would
        // answer it twice.
        "app_mention" if !channel.starts_with('D') => (false, false),
        // A thread reply that mentions the app arrives as app_mention too.
        "message" if thread.is_some() && !raw.contains(&format!("<@{bot_user}")) => (false, true),
        _ => return None,
    };
    let owner = config.owner.trim();
    if user != owner && (!dm || !owner.is_empty()) {
        return None;
    }
    // Slack takes a leading / as one of its own commands, so these stand in.
    let said = match slack_text(raw, bot_user) {
        said if said.eq_ignore_ascii_case("!stop") => "/stop".to_owned(),
        said if said.eq_ignore_ascii_case("!new") => "/new".to_owned(),
        said => said,
    };
    let text = match files {
        true => format!("{said}\n\n(attached files are not read)")
            .trim()
            .to_owned(),
        false => said.clone(),
    };
    if text.is_empty() {
        return None;
    }
    // A mention is answered in a thread under it, so a channel is not
    // flooded and the thread becomes the session. A DM is one session, a
    // reply threaded under a card included.
    let thread = match dm {
        true => None,
        false => thread.or(event["ts"].as_str()),
    };
    Some(Incoming {
        source: Source::Slack {
            channel: channel.to_owned(),
            thread: thread.map(str::to_owned),
        },
        from: user.to_owned(),
        said,
        text,
        // Slack sets `user` itself, so it cannot be spoofed.
        author: Some(user.to_owned()),
        remote: backend::Remote {
            place: match dm {
                true => "a Slack DM".into(),
                false => "a Slack channel".into(),
            },
            shared: !dm,
            history: None,
        },
        needs_session,
        press: None,
        files: Vec::new(),
    })
}

/// What the listener does with one Socket Mode frame.
enum Envelope {
    /// Send this acknowledgement, then pass on the message it carried for
    /// the agent, if any.
    Ack(String, Option<Box<Incoming>>),
    /// Slack is about to close the connection: open a fresh one.
    Disconnect,
    /// Nothing to do: the hello, or something unreadable.
    Ignore,
}

/// Read one Socket Mode frame. Every envelope is acknowledged whatever it
/// holds, or Slack sends it again; only a message event reaches the agent.
fn slack_envelope(
    payload: &serde_json::Value,
    config: &Slack,
    bot_user: &str,
    seen: &mut SlackSeen,
) -> Envelope {
    if payload["type"].as_str() == Some("disconnect") {
        return Envelope::Disconnect;
    }
    let Some(id) = payload["envelope_id"].as_str() else {
        return Envelope::Ignore;
    };
    let message = match payload["type"].as_str() {
        // At least once: a repeat is acknowledged again but not acted on.
        Some("events_api")
            if payload["payload"]["event_id"]
                .as_str()
                .is_none_or(|event| seen.first(event)) =>
        {
            slack_event(&payload["payload"]["event"], config, bot_user).map(Box::new)
        }
        _ => None,
    };
    Envelope::Ack(serde_json::json!({"envelope_id": id}).to_string(), message)
}

/// The event IDs handed on lately, so one Slack sends again is dropped.
#[derive(Default)]
struct SlackSeen(std::collections::VecDeque<String>);

impl SlackSeen {
    /// Whether `id` is new, remembering it if so.
    fn first(&mut self, id: &str) -> bool {
        if self.0.iter().any(|seen| seen == id) {
            return false;
        }
        if self.0.len() == SLACK_SEEN {
            self.0.pop_front();
        }
        self.0.push_back(id.to_owned());
        true
    }
}

/// What the Slack listener is doing, for the settings page.
#[derive(Clone, Debug, PartialEq)]
pub enum SlackState {
    /// Opening a connection, or waiting to try again after one failed.
    Connecting,
    Listening,
    /// Slack refused a token, so the listener has stopped; why, in words.
    Refused(String),
}

/// What one listener thread keeps from one connection to the next.
struct SlackListener {
    config: Slack,
    /// The app's own member ID, for dropping its mention from a message.
    bot_user: String,
    seen: SlackSeen,
    quiet: std::time::Duration,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sender: async_channel::Sender<Incoming>,
    state: async_channel::Sender<SlackState>,
}

impl SlackListener {
    /// Nobody may be watching; the listener carries on either way.
    fn tell(&self, state: SlackState) {
        let _ = self.state.try_send(state);
    }
}

/// Hold a Socket Mode connection open until `stop` is set, opening a fresh
/// one when Slack asks or the old one drops. Runs on its own thread; each
/// message for the agent goes down `sender`, and each change of state down
/// `state`. While idle the thread wakes once per `SLACK_QUIET`, so `stop` is
/// noticed within that; a message that arrives after it is left
/// unacknowledged, and Slack hands it to the connection that replaced this
/// one. A refused token stops it: retrying cannot help.
pub fn slack_listen(
    config: Slack,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    sender: async_channel::Sender<Incoming>,
    state: async_channel::Sender<SlackState>,
) {
    std::thread::spawn(move || {
        use std::sync::atomic::Ordering;
        let mut listener = SlackListener {
            config,
            bot_user: String::new(),
            seen: SlackSeen::default(),
            quiet: SLACK_QUIET,
            stop,
            sender,
            state,
        };
        let mut wait = SLACK_RETRY;
        while !listener.stop.load(Ordering::Relaxed) {
            listener.tell(SlackState::Connecting);
            // A refused bot token would otherwise run the agent for every
            // message and lose every reply.
            let bot = &listener.config.bot_token;
            let checked = match listener.bot_user.is_empty() {
                true => slack_auth(SLACK_API, bot).map(|(_, id)| id),
                false => Ok(listener.bot_user.clone()),
            };
            let began = std::time::Instant::now();
            let opened = checked
                .and_then(|id| Ok((id, slack_socket_url(SLACK_API, &listener.config.app_token)?)));
            let ended = match opened {
                Err(code) if SLACK_REFUSED.contains(&code.as_str()) => {
                    return listener.tell(SlackState::Refused(slack_error(&code)));
                }
                Err(_) => Ended::Failed,
                Ok((id, url)) => {
                    listener.bot_user = id;
                    slack_session(&url, &mut listener)
                }
            };
            if let Ended::Stopped = ended {
                return;
            }
            let held = began.elapsed() >= SLACK_RETRY_MAX;
            let (pause, next) = slack_backoff(wait, &ended, held);
            wait = next;
            if !pause.is_zero() {
                listener.tell(SlackState::Connecting);
                std::thread::sleep(pause);
            }
        }
    });
}

/// The pause before the next connection and the wait after that one. A
/// connection that held a while was not what is failing, so the wait starts
/// over; Slack refreshes one every few hours and says so first, which is
/// routine, so the next one opens at once.
fn slack_backoff(
    wait: std::time::Duration,
    ended: &Ended,
    held: bool,
) -> (std::time::Duration, std::time::Duration) {
    let wait = if held { SLACK_RETRY } else { wait };
    match ended {
        Ended::Closed(None) if held => (std::time::Duration::ZERO, wait),
        _ => (wait, (wait * 2).min(SLACK_RETRY_MAX)),
    }
}

/// One connection, until it ends. A quiet spell is answered with a ping of
/// our own; a second in a row means the connection is gone even if TCP has
/// not noticed.
fn slack_session(url: &str, listener: &mut SlackListener) -> Ended {
    use std::sync::atomic::Ordering;
    use tungstenite::Message as Frame;
    let Some(mut socket) = slack_connect(url, listener.quiet) else {
        return Ended::Failed;
    };
    listener.tell(SlackState::Listening);
    let mut quiet = false;
    loop {
        let read = socket.read();
        if listener.stop.load(Ordering::Relaxed) {
            let _ = socket.close(None);
            return Ended::Stopped;
        }
        let frame = match read {
            Ok(frame) => frame,
            // tungstenite keeps its place across a timed-out read.
            Err(tungstenite::Error::Io(error)) if !quiet && slack_quiet(&error) => {
                quiet = true;
                if socket.send(Frame::Ping(Default::default())).is_err() {
                    return Ended::Failed;
                }
                continue;
            }
            Err(_) => return Ended::Failed,
        };
        quiet = false;
        let payload: serde_json::Value = match frame {
            Frame::Text(text) => match serde_json::from_str(text.as_str()) {
                Ok(payload) => payload,
                Err(_) => continue,
            },
            Frame::Close(_) => return Ended::Failed,
            _ => continue,
        };
        let config = &listener.config;
        match slack_envelope(&payload, config, &listener.bot_user, &mut listener.seen) {
            Envelope::Ignore => {}
            Envelope::Disconnect => {
                let _ = socket.close(None);
                return Ended::Closed(None);
            }
            // Acknowledged before it is acted on, well within Slack's three
            // seconds; one that cannot be is left for Slack to send again.
            Envelope::Ack(ack, message) => {
                if socket.send(Frame::text(ack)).is_err() {
                    return Ended::Failed;
                }
                if let Some(message) = message
                    && listener.sender.send_blocking(*message).is_err()
                {
                    return Ended::Stopped;
                }
            }
        }
    }
}

/// Open the socket with a timeout on every step -- the lookup, the connect
/// and each read, `quiet` -- so neither a stalled resolver nor the handshake
/// can hang the thread.
fn slack_connect(
    url: &str,
    quiet: std::time::Duration,
) -> Option<tungstenite::WebSocket<tungstenite::stream::MaybeTlsStream<std::net::TcpStream>>> {
    use std::net::ToSocketAddrs;
    let step = std::time::Duration::from_secs(10);
    let parsed = reqwest::Url::parse(url).ok()?;
    let host = (
        parsed.host_str()?.to_owned(),
        parsed.port_or_known_default()?,
    );
    // The system resolver has no timeout of its own, so it runs aside and a
    // stalled one is left to finish by itself.
    let (found, lookup) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = found.send(host.to_socket_addrs().map(Vec::from_iter));
    });
    let stream = lookup
        .recv_timeout(step)
        .ok()?
        .ok()?
        .iter()
        .find_map(|address| std::net::TcpStream::connect_timeout(address, step).ok())?;
    stream.set_read_timeout(Some(quiet)).ok()?;
    stream.set_write_timeout(Some(quiet)).ok()?;
    tungstenite::client_tls(url, stream)
        .ok()
        .map(|(socket, _)| socket)
}

/// A read that timed out: Unix says it would block, Windows that it timed
/// out.
fn slack_quiet(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
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
        assert!(text.starts_with("Letronna wants to use run_command:\n```\nls\n```"));
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
    fn a_tap_counts_from_the_owner_where_the_bot_listens_and_nowhere_else() {
        let tap = |extra: Value| {
            let mut tap = json!({"type": 3, "id": "i1", "token": "tok", "channel_id": "c1",
                "data": {"custom_id": "allow:call_1"}});
            for (key, value) in extra.as_object().into_iter().flatten() {
                tap[key] = value.clone();
            }
            super::gateway_press(&tap, &config())
        };
        let owner = json!({"id": "7"});
        let press = tap(json!({"user": owner})).and_then(|m| m.press).unwrap();
        assert!(press.allow && press.answers("call_1"));
        assert!(tap(json!({"guild_id": "500", "member": {"user": owner}})).is_some());
        // Someone else, a server not listed, or not a button at all.
        assert!(tap(json!({"user": {"id": "8"}})).is_none());
        assert!(tap(json!({"guild_id": "501", "member": {"user": owner}})).is_none());
        assert!(tap(json!({"user": owner, "type": 2})).is_none());
        assert!(tap(json!({"user": owner, "data": {"custom_id": "answered"}})).is_none());
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

#[cfg(test)]
mod slack_tests {
    use super::{
        Ended, Envelope, Incoming, SLACK_PART, SLACK_PARTS, SLACK_QUIET, SLACK_RETRY,
        SLACK_RETRY_MAX, SLACK_SEEN, SLACK_TOTAL, Slack, SlackListener, SlackSeen, SlackState,
        Source, slack_backoff, slack_check, slack_envelope, slack_event, slack_parts,
        slack_post_via, slack_session, slack_text,
    };
    use serde_json::{Value, json};

    const BOT: &str = "UBOT";

    fn paired() -> Slack {
        Slack {
            enabled: true,
            app_token: "xapp-1-test".into(),
            bot_token: "xoxb-test".into(),
            owner: "U7".into(),
            ..Default::default()
        }
    }

    fn dm(user: &str, text: &str) -> Value {
        json!({"type": "message", "channel_type": "im", "channel": "D1", "user": user,
               "text": text, "ts": "1700000000.000100"})
    }

    fn mention(user: &str, text: &str) -> Value {
        json!({"type": "app_mention", "channel": "C1", "user": user, "text": text,
               "ts": "1700000000.000200"})
    }

    fn envelope(event: Value) -> Value {
        json!({"envelope_id": "e1", "type": "events_api", "accepts_response_payload": false,
               "payload": {"type": "event_callback", "event_id": "Ev1", "event": event}})
    }

    #[test]
    fn an_event_is_acknowledged_by_its_envelope_id_and_its_message_passed_on() {
        let Envelope::Ack(ack, Some(message)) = slack_envelope(
            &envelope(dm("U7", "hello &amp; bye")),
            &paired(),
            BOT,
            &mut SlackSeen::default(),
        ) else {
            panic!("expected an acknowledged message");
        };
        assert_eq!(
            serde_json::from_str::<Value>(&ack).unwrap(),
            json!({"envelope_id": "e1"})
        );
        assert_eq!(
            message.source,
            Source::Slack {
                channel: "D1".into(),
                thread: None
            }
        );
        assert_eq!(message.text, "hello & bye");
        assert_eq!(message.said, "hello & bye");
        assert_eq!(message.from, "U7");
        assert_eq!(message.remote.place, "a Slack DM");
        assert!(!message.remote.shared);
        assert!(message.remote.history.is_none());
    }

    #[test]
    fn every_envelope_is_acknowledged_even_with_nothing_for_the_agent() {
        let command = json!({"envelope_id": "e2", "type": "slash_commands", "payload": {}});
        let Envelope::Ack(ack, None) =
            slack_envelope(&command, &paired(), BOT, &mut SlackSeen::default())
        else {
            panic!("a slash command is acknowledged and dropped");
        };
        assert_eq!(ack, r#"{"envelope_id":"e2"}"#);
        let stranger = envelope(dm("U8", "hi"));
        assert!(matches!(
            slack_envelope(&stranger, &paired(), BOT, &mut SlackSeen::default()),
            Envelope::Ack(_, None)
        ));
        let hello = json!({"type": "hello", "num_connections": 1});
        assert!(matches!(
            slack_envelope(&hello, &paired(), BOT, &mut SlackSeen::default()),
            Envelope::Ignore
        ));
        let refresh = json!({"type": "disconnect", "reason": "refresh_requested"});
        assert!(matches!(
            slack_envelope(&refresh, &paired(), BOT, &mut SlackSeen::default()),
            Envelope::Disconnect
        ));
    }

    #[test]
    fn the_apps_own_messages_and_edits_are_never_heard() {
        let mut own = dm("U7", "an earlier reply");
        own["bot_id"] = "B1".into();
        assert!(slack_event(&own, &paired(), BOT).is_none());
        let mut edit = dm("U7", "fixed a typo");
        edit["subtype"] = "message_changed".into();
        assert!(slack_event(&edit, &paired(), BOT).is_none());
        let mut paired_to_itself = paired();
        paired_to_itself.owner = BOT.into();
        assert!(slack_event(&dm(BOT, "hi"), &paired_to_itself, BOT).is_none());
        assert!(slack_event(&dm("U7", "   "), &paired(), BOT).is_none());
    }

    #[test]
    fn a_mention_is_answered_in_its_thread_and_only_from_the_paired_member() {
        let message = slack_event(&mention("U7", "<@UBOT> what changed?"), &paired(), BOT).unwrap();
        assert_eq!(message.text, "what changed?");
        assert_eq!(
            message.source,
            Source::Slack {
                channel: "C1".into(),
                thread: Some("1700000000.000200".into())
            }
        );
        assert!(message.remote.shared);
        assert_eq!(message.remote.place, "a Slack channel");
        let mut in_thread = mention("U7", "<@UBOT> and now?");
        in_thread["thread_ts"] = "1700000000.000001".into();
        let message = slack_event(&in_thread, &paired(), BOT).unwrap();
        assert_eq!(
            message.source,
            Source::Slack {
                channel: "C1".into(),
                thread: Some("1700000000.000001".into())
            }
        );
        assert!(slack_event(&mention("U8", "<@UBOT> hi"), &paired(), BOT).is_none());
        // Heard once, as the DM's message, not again as a mention.
        let mut in_dm = mention("U7", "<@UBOT> hi");
        in_dm["channel"] = "D1".into();
        assert!(slack_event(&in_dm, &paired(), BOT).is_none());
        // A channel's every message, if the app is subscribed to them.
        let mut chatter = dm("U7", "lunch?");
        chatter["channel_type"] = "channel".into();
        assert!(slack_event(&chatter, &paired(), BOT).is_none());
    }

    #[test]
    fn before_pairing_a_dm_asks_to_be_allowed_and_a_mention_is_ignored() {
        let unpaired = Slack {
            owner: String::new(),
            ..paired()
        };
        let message = slack_event(&dm("U8", "hi"), &unpaired, BOT).unwrap();
        assert_eq!(message.from, "U8");
        assert!(slack_event(&mention("U8", "<@UBOT> hi"), &unpaired, BOT).is_none());
        // Nor does a schedule report to an app nobody paired: this would
        // otherwise reach out to Slack.
        let config = super::Config {
            slack: unpaired,
            ..Default::default()
        };
        assert!(super::report(&config, "Nightly", "ok").is_empty());
    }

    #[test]
    fn slack_markup_reads_as_plain_text() {
        let raw = "<@UBOT> ask <@U9> in <#C2|general> about <https://example.com/a?b=1&amp;c=2|the page>, \
                   <!here>, 1 &lt; 2 &amp;&amp; 3 &gt; 2, &amp;lt; stays";
        assert_eq!(
            slack_text(raw, BOT),
            "ask @U9 in #general about https://example.com/a?b=1&c=2, @here, 1 < 2 && 3 > 2, \
             &lt; stays"
        );
        assert_eq!(slack_text("a <b", BOT), "a <b");
    }

    #[test]
    fn replies_are_escaped_and_split_at_lines_never_inside_an_escape() {
        assert_eq!(slack_parts("a < b & c > d"), ["a &lt; b &amp; c &gt; d"]);
        let line = format!("{}\n", "x".repeat(99));
        let long = line.repeat(SLACK_PART / 100 + 5);
        let parts = slack_parts(&long);
        assert_eq!(parts.len(), 2);
        assert!(parts.iter().all(|part| part.chars().count() <= SLACK_PART));
        assert!(parts[0].ends_with('\n'));
        assert_eq!(parts.concat(), long);
        // No line break to cut at, and an escape straddling the limit.
        let dense = format!("{}<{}", "y".repeat(SLACK_PART - 2), "z".repeat(10));
        let parts = slack_parts(&dense);
        assert_eq!(parts[0], "y".repeat(SLACK_PART - 2));
        assert!(parts[1].starts_with("&lt;z"));
        let huge = "w".repeat(SLACK_TOTAL * 2);
        let parts = slack_parts(&huge);
        assert_eq!(parts.len(), SLACK_PARTS);
        assert!(parts.last().unwrap().ends_with("… (trimmed)"));
        // The escaped text is what is trimmed, and never mid-escape.
        let amps = slack_parts(&"&".repeat(SLACK_TOTAL)).concat();
        assert!(amps.chars().count() <= SLACK_TOTAL + 20);
        assert!(amps.ends_with("&amp;\n… (trimmed)"));
        // Lines cut at half a part would make twenty; ten are posted.
        let halves = format!("{}\n", "v".repeat(SLACK_PART / 2)).repeat(19);
        let parts = slack_parts(&halves);
        assert_eq!(parts.len(), SLACK_PARTS);
        assert!(parts.last().unwrap().ends_with("… (trimmed)"));
    }

    #[test]
    fn a_token_must_look_right_before_slack_is_asked() {
        let wrong = slack_check("xoxb-1", "xoxb-2").unwrap_err();
        assert!(wrong.contains("xapp-"), "{wrong}");
        let wrong = slack_check("xapp-1", "xoxp-2").unwrap_err();
        assert!(wrong.contains("xoxb-"), "{wrong}");
    }

    #[test]
    fn a_file_share_keeps_its_words_and_a_broadcast_reply_is_a_message() {
        let mut shared = dm("U7", "see this");
        shared["subtype"] = "file_share".into();
        shared["files"] = json!([{"id": "F1"}]);
        let message = slack_event(&shared, &paired(), BOT).unwrap();
        assert_eq!(message.text, "see this\n\n(attached files are not read)");
        assert_eq!(message.said, "see this");
        shared["text"] = "".into();
        let message = slack_event(&shared, &paired(), BOT).unwrap();
        assert_eq!(message.text, "(attached files are not read)");
        let mut broadcast = mention("U7", "<@UBOT> also here");
        broadcast["subtype"] = "thread_broadcast".into();
        broadcast["thread_ts"] = "1700000000.000050".into();
        let message = slack_event(&broadcast, &paired(), BOT).unwrap();
        assert_eq!(message.text, "also here");
    }

    #[test]
    fn a_thread_reply_without_a_mention_is_heard_only_as_a_follow_up() {
        let reply = json!({"type": "message", "channel_type": "channel", "channel": "C1",
                           "user": "U7", "text": "yes", "ts": "1700000000.000300",
                           "thread_ts": "1700000000.000200"});
        let message = slack_event(&reply, &paired(), BOT).unwrap();
        assert!(message.needs_session);
        assert!(message.remote.shared);
        let thread = Some("1700000000.000200".to_owned());
        assert_eq!(
            message.source,
            Source::Slack {
                channel: "C1".into(),
                thread
            }
        );
        // One that mentions the app arrives as app_mention too.
        let mut mentioned = reply.clone();
        mentioned["text"] = "<@UBOT> yes".into();
        assert!(slack_event(&mentioned, &paired(), BOT).is_none());
        let mut chatter = reply.clone();
        chatter.as_object_mut().unwrap().remove("thread_ts");
        assert!(slack_event(&chatter, &paired(), BOT).is_none());
        let mut other = reply;
        other["user"] = "U8".into();
        assert!(slack_event(&other, &paired(), BOT).is_none());
        // In a DM a threaded reply, such as a card's answer, is the DM.
        let mut threaded = dm("U7", "2");
        threaded["thread_ts"] = "1700000000.000050".into();
        let message = slack_event(&threaded, &paired(), BOT).unwrap();
        assert!(!message.needs_session);
        let dm = Source::Slack {
            channel: "D1".into(),
            thread: None,
        };
        assert_eq!(message.source, dm);
    }

    #[test]
    fn bang_stop_and_bang_new_stand_in_for_the_slash_commands() {
        let said = |event: Value| slack_event(&event, &paired(), BOT).unwrap().said;
        assert_eq!(said(dm("U7", " !STOP ")), "/stop");
        assert_eq!(said(mention("U7", "<@UBOT> !new")), "/new");
        assert_eq!(said(dm("U7", "!stop that")), "!stop that");
    }

    #[test]
    fn a_group_dm_mention_is_shared_and_an_enterprise_member_id_matches_exactly() {
        let mut group = mention("U7", "<@UBOT> hi");
        group["channel"] = "G1".into();
        assert!(slack_event(&group, &paired(), BOT).unwrap().remote.shared);
        let grid = Slack {
            owner: "W0123".into(),
            ..paired()
        };
        assert!(slack_event(&dm("W0123", "hi"), &grid, BOT).is_some());
        assert!(slack_event(&dm("w0123", "hi"), &grid, BOT).is_none());
        assert!(slack_event(&dm("U0123", "hi"), &grid, BOT).is_none());
    }

    #[test]
    fn an_event_slack_sends_again_is_acknowledged_but_not_heard_twice() {
        let mut seen = SlackSeen::default();
        let first = envelope(dm("U7", "run it"));
        assert!(matches!(
            slack_envelope(&first, &paired(), BOT, &mut seen),
            Envelope::Ack(_, Some(_))
        ));
        let mut again = first.clone();
        again["envelope_id"] = "e9".into();
        again["retry_attempt"] = 1.into();
        let Envelope::Ack(ack, None) = slack_envelope(&again, &paired(), BOT, &mut seen) else {
            panic!("a repeat is acknowledged and dropped");
        };
        assert_eq!(ack, r#"{"envelope_id":"e9"}"#);
        // Only the latest are kept, so the ring stays small.
        for n in 0..SLACK_SEEN {
            assert!(seen.first(&format!("Ev-{n}")));
        }
        assert!(seen.first("Ev1"));
        assert!(!seen.first("Ev1"));
    }

    #[test]
    fn a_failing_connection_waits_longer_each_time_up_to_a_minute() {
        let mut wait = SLACK_RETRY;
        let mut pauses = Vec::new();
        for _ in 0..7 {
            let (pause, next) = slack_backoff(wait, &Ended::Failed, false);
            pauses.push(pause.as_secs());
            wait = next;
        }
        assert_eq!(pauses, [2, 4, 8, 16, 32, 60, 60]);
        // One that held a while starts over; Slack's refresh reopens at once,
        // unless it came right after connecting.
        let failed = slack_backoff(SLACK_RETRY_MAX, &Ended::Failed, true);
        assert_eq!(failed, (SLACK_RETRY, SLACK_RETRY * 2));
        let refresh = slack_backoff(SLACK_RETRY_MAX, &Ended::Closed(None), true);
        assert_eq!(refresh, (std::time::Duration::ZERO, SLACK_RETRY));
        let early = slack_backoff(SLACK_RETRY, &Ended::Closed(None), false);
        assert_eq!(early.0, SLACK_RETRY);
    }

    /// A Web API call: its path, bearer token and JSON body.
    type Call = (String, String, Value);

    /// Answer `count` Web API calls with `{"ok": true}`, the first with a
    /// rate limit instead when `limited`, and hand them back.
    fn slack_api(count: usize, limited: bool) -> (String, std::thread::JoinHandle<Vec<Call>>) {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            (0..count)
                .map(|index| {
                    let (mut socket, _) = server.accept().unwrap();
                    let mut bytes = Vec::new();
                    let mut block = [0; 8192];
                    let (head, body) = loop {
                        let n = socket.read(&mut block).unwrap();
                        assert_ne!(n, 0);
                        bytes.extend_from_slice(&block[..n]);
                        let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&bytes[..end]).to_string();
                        let len: usize = header(&head, "content-length: ").parse().unwrap();
                        if bytes.len() >= end + 4 + len {
                            break (head, bytes[end + 4..end + 4 + len].to_vec());
                        }
                    };
                    let (status, ok) = match limited && index == 0 {
                        true => (
                            "429 Too Many Requests\r\nRetry-After: 0",
                            r#"{"ok":false,"error":"ratelimited"}"#,
                        ),
                        false => ("200 OK", r#"{"ok":true}"#),
                    };
                    write!(
                        socket,
                        "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                         Content-Length: {}\r\nConnection: close\r\n\r\n{ok}",
                        ok.len()
                    )
                    .unwrap();
                    let path = head.split_whitespace().nth(1).unwrap().to_owned();
                    let token = header(&head, "authorization: bearer ");
                    (path, token, serde_json::from_slice(&body).unwrap())
                })
                .collect()
        });
        (base, worker)
    }

    fn header(head: &str, name: &str) -> String {
        head.lines()
            .find(|line| line.to_lowercase().starts_with(name))
            .map(|line| line[name.len()..].trim().to_owned())
            .unwrap()
    }

    #[test]
    fn a_long_reply_goes_to_its_thread_in_parts_with_no_token_in_it() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data = crate::store::tests::Scratch::new("slack-scrub");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        let config = super::Config {
            slack: Slack {
                app_token: "xapp-1-A0SAVED-0123456789".into(),
                bot_token: "xoxb-0123456789-saved".into(),
                ..paired()
            },
            ..Default::default()
        };
        super::save(&config);
        let marker = crate::redact::MARKER;

        let (api, worker) = slack_api(2, false);
        let text = format!(
            "{}\nthe keys are {} and {}",
            "x".repeat(SLACK_PART - 10),
            config.slack.app_token,
            config.slack.bot_token
        );
        let sent = slack_post_via(&api, "xoxb-given", "C1", Some("17.01"), &text);
        let calls = worker.join().unwrap();
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        sent.unwrap();
        for (path, token, body) in &calls {
            assert_eq!(path, "/chat.postMessage");
            assert_eq!(token, "xoxb-given");
            assert_eq!(body["channel"], "C1");
            assert_eq!(body["thread_ts"], "17.01");
            assert_eq!(body["unfurl_links"], false);
            assert_eq!(body["unfurl_media"], false);
        }
        assert_eq!(
            calls[1].2["text"],
            format!("the keys are {marker} and {marker}")
        );
    }

    #[test]
    fn a_rate_limited_part_is_sent_again_once_after_the_wait_slack_asks() {
        let (api, worker) = slack_api(2, true);
        let sent = slack_post_via(&api, "xoxb-given", "D1", None, "hello");
        let calls = worker.join().unwrap();
        sent.unwrap();
        assert_eq!(calls[0].2, calls[1].2);
        assert_eq!(calls[1].2["text"], "hello");
        assert_eq!(
            super::slack_retry_after("ratelimited 5").unwrap().as_secs(),
            5
        );
        assert_eq!(
            super::slack_retry_after("ratelimited 900")
                .unwrap()
                .as_secs(),
            30
        );
        assert!(super::slack_retry_after("invalid_auth").is_none());
    }

    type Reply = tungstenite::Result<tungstenite::Message>;

    /// A listener for `paired()` that reads with `quiet`, and the far ends of
    /// its message and state channels.
    fn listener(
        stop: bool,
        quiet: std::time::Duration,
    ) -> (
        SlackListener,
        async_channel::Receiver<Incoming>,
        async_channel::Receiver<SlackState>,
    ) {
        let (sender, messages) = async_channel::unbounded();
        let (state, states) = async_channel::unbounded();
        let listener = SlackListener {
            config: paired(),
            bot_user: BOT.into(),
            seen: SlackSeen::default(),
            quiet,
            stop: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(stop)),
            sender,
            state,
        };
        (listener, messages, states)
    }

    /// A local Socket Mode server: say hello, send `frames`, and hand back
    /// what the client sends in return.
    fn socket(frames: Vec<Value>) -> (String, std::thread::JoinHandle<Reply>) {
        use tungstenite::Message as Frame;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (stream, _) = server.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket
                .send(Frame::text(r#"{"type":"hello","num_connections":1}"#))
                .unwrap();
            // A stopped client may already have gone.
            for frame in &frames {
                let _ = socket.send(Frame::text(frame.to_string()));
            }
            let reply = socket.read();
            let refresh = json!({"type": "disconnect", "reason": "refresh_requested"});
            let _ = socket.send(Frame::text(refresh.to_string()));
            reply
        });
        (url, worker)
    }

    #[test]
    fn a_session_acknowledges_hands_on_and_ends_when_slack_asks() {
        let (url, worker) = socket(vec![envelope(dm("U7", "status?"))]);
        let (mut listener, receiver, states) = listener(false, SLACK_QUIET);
        let ended = slack_session(&url, &mut listener);
        assert!(matches!(ended, Ended::Closed(None)));
        assert_eq!(states.try_recv().unwrap(), SlackState::Listening);
        let ack = worker.join().unwrap().unwrap().into_text().unwrap();
        assert_eq!(ack.as_str(), r#"{"envelope_id":"e1"}"#);
        assert_eq!(receiver.try_recv().unwrap().text, "status?");
        assert!(receiver.try_recv().is_err());
    }

    #[test]
    fn a_stopped_session_leaves_the_next_envelope_for_slack_to_send_again() {
        let (url, worker) = socket(vec![envelope(dm("U7", "status?"))]);
        let (mut listener, receiver, _) = listener(true, SLACK_QUIET);
        let ended = slack_session(&url, &mut listener);
        assert!(matches!(ended, Ended::Stopped));
        let reply = worker.join().unwrap();
        assert!(!matches!(reply, Ok(tungstenite::Message::Text(_))));
        assert!(receiver.try_recv().is_err());
    }

    /// A local server that says hello and then nothing: it answers the
    /// client's pings while `answers`, and stays mute otherwise. It counts the
    /// pings, and asks for a refresh after three.
    fn quiet_socket(answers: bool) -> (String, std::thread::JoinHandle<usize>) {
        use tungstenite::Message as Frame;
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let (stream, _) = server.accept().unwrap();
            let mut socket = tungstenite::accept(stream).unwrap();
            socket
                .send(Frame::text(r#"{"type":"hello","num_connections":1}"#))
                .unwrap();
            if !answers {
                // Held open, so only the silence can end it.
                std::thread::sleep(std::time::Duration::from_secs(2));
                return 0;
            }
            let mut pings = 0;
            // Reading is what sends tungstenite's pong.
            while let Ok(frame) = socket.read() {
                if frame.is_ping() {
                    pings += 1;
                }
                if pings == 3 {
                    let refresh = json!({"type": "disconnect", "reason": "refresh_requested"});
                    let _ = socket.send(Frame::text(refresh.to_string()));
                    let _ = socket.read();
                    break;
                }
            }
            pings
        });
        (url, worker)
    }

    #[test]
    fn a_quiet_connection_is_pinged_and_kept_while_it_answers() {
        let quiet = std::time::Duration::from_millis(100);
        let (url, worker) = quiet_socket(true);
        let (mut listener, _, _) = listener(false, quiet);
        let ended = slack_session(&url, &mut listener);
        assert!(matches!(ended, Ended::Closed(None)));
        assert_eq!(worker.join().unwrap(), 3);
    }

    #[test]
    fn a_connection_silent_twice_in_a_row_is_given_up() {
        let quiet = std::time::Duration::from_millis(100);
        let (url, worker) = quiet_socket(false);
        let (mut listener, _, _) = listener(false, quiet);
        let began = std::time::Instant::now();
        let ended = slack_session(&url, &mut listener);
        assert!(matches!(ended, Ended::Failed));
        assert!(began.elapsed() < std::time::Duration::from_secs(1));
        worker.join().unwrap();
    }

    /// Talks to a real Slack app, and posts one message.
    #[test]
    #[ignore = "needs a Slack app with Socket Mode on: LETRONNA_TEST_SLACK_APP=xapp-... \
                LETRONNA_TEST_SLACK_BOT=xoxb-... LETRONNA_TEST_SLACK_TO=<member or channel \
                ID> cargo test -p letronna-core -- --ignored live_slack"]
    fn live_slack_check_and_post() {
        let env = |name| std::env::var(name).expect(name);
        let bot = env("LETRONNA_TEST_SLACK_BOT");
        let name = slack_check(&env("LETRONNA_TEST_SLACK_APP"), &bot).unwrap();
        println!("connected as {name}");
        let to = env("LETRONNA_TEST_SLACK_TO");
        super::slack_post(&bot, &to, None, "Letronna test: 1 < 2 & 3 > 2").unwrap();
    }
}
