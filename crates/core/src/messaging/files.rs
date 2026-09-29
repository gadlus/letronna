//! Files to and from a chat app.
//!
//! In: a photo, document or voice note that comes with a message is only
//! described by the listener -- where to fetch it and what it weighs -- and
//! fetched by the app once it has checked the chat is one allowed to talk,
//! into the `media/` of the session it is for; Letronna's own where other
//! people read the chat, since they are not the user. The name is the
//! sender's, so the saved file is named by `media::save` from a slug of it.
//! A photo small enough goes to the model as a picture, anything else is
//! named by where it was saved, and a voice note is transcribed, which costs
//! credit, only once its chat has said yes to that.
//!
//! Out: `send_file`, and what a media tool made in the turn, sent after the
//! reply. A file leaving this computer reaches other people's servers, so
//! the tool asks first, sends only what the project or the media folder
//! holds and never a file of keys, and hides a saved key in any text it
//! sends, as every message to a chat app does.

use super::{Config, Incoming, Source};
use crate::chat::Message;
use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const SEND_FILE: &str = "send_file";
/// The most a file coming in may weigh: Telegram hands a bot files up to
/// 20 MB, and Discord's own default cap is 20 MiB, so one bound for both
/// keeps a chat from filling the disk.
const RECEIVE_BYTES: u64 = 20_000_000;
/// The most files one message brings; both apps allow ten.
const MOST_FILES: usize = 10;
/// What a bot may send on Telegram: 50 MB as a document or audio.
const TELEGRAM_BYTES: u64 = 50_000_000;
/// And 10 MB as a photo; a larger picture goes as a document.
const TELEGRAM_PHOTO_BYTES: u64 = 10_000_000;
/// Discord's default cap on each file, 20 MiB. A boosted server takes more,
/// but a bot cannot see that before it tries.
const DISCORD_BYTES: u64 = 20 * 1024 * 1024;
/// Telegram cuts a caption at 1,024 characters; Discord's message holds 2,000.
const CAPTION_CHARS: usize = 1000;
/// Long enough for 50 MB over a slow line.
const TRANSFER_TIME: Duration = Duration::from_secs(300);
/// The media tools whose files go to the chat after the reply unasked.
const MAKERS: [&str; 3] = ["make_image", "speak", "make_video"];

/// What a file that came with a message is to the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Photo,
    Document,
    Voice,
}

/// Where a file that came with a message is fetched from.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Origin {
    /// Telegram's id for it, which `getFile` turns into an address.
    Telegram(String),
    /// An address on Discord's own servers.
    Discord(String),
}

/// A file that came with a chat message.
#[derive(Clone, Debug)]
pub struct Received {
    pub kind: Kind,
    origin: Origin,
    /// What the sender called it, which names the saved file.
    name: String,
    /// What the chat app says it weighs; checked again as it arrives.
    size: u64,
    /// Where it was saved, once fetched.
    pub saved: Option<PathBuf>,
}

impl Received {
    fn new(kind: Kind, origin: Origin, name: &str, size: Option<u64>) -> Self {
        Received {
            kind,
            origin,
            name: name.to_owned(),
            size: size.unwrap_or_default(),
            saved: None,
        }
    }
}

/// The messages in a `getUpdates` response that carry a photo, document or
/// voice note instead of text, which `parse_updates` passes over. An
/// album's photos come as one message each and are put back together, so
/// the second does not find the first's turn running.
pub(super) fn telegram_media(body: &Value) -> Vec<Incoming> {
    let mut found: Vec<(Option<String>, Incoming)> = Vec::new();
    for update in body["result"].as_array().into_iter().flatten() {
        let message = &update["message"];
        let Some(chat) = message["chat"]["id"].as_i64() else {
            continue;
        };
        let files = telegram_files(message);
        if files.is_empty() {
            continue;
        }
        let caption = message["caption"].as_str().unwrap_or_default().trim();
        let album = message["media_group_id"].as_str().map(str::to_owned);
        let source = Source::Telegram(chat);
        if let Some((_, earlier)) = found
            .iter_mut()
            .find(|(a, m)| album.is_some() && *a == album && m.source == source)
        {
            earlier.files.extend(files);
            earlier.files.truncate(MOST_FILES);
            if earlier.text.is_empty() {
                earlier.text = caption.to_owned();
                earlier.said = caption.to_owned();
            }
            continue;
        }
        let from = &message["from"];
        // As `parse_updates` has it: a group's replies are read by everyone.
        let group = chat < 0;
        let incoming = Incoming {
            source,
            from: from["first_name"]
                .as_str()
                .or_else(|| from["username"].as_str())
                .unwrap_or("someone")
                .to_owned(),
            text: caption.to_owned(),
            said: caption.to_owned(),
            files,
            // As `parse_updates` has it: a forward, or a post in a chat's name, is
            // no one's own words.
            author: match message["forward_origin"].is_null() && message["sender_chat"].is_null() {
                true => from["id"].as_i64().map(|id| id.to_string()),
                false => None,
            },
            needs_session: false,
            spoken: false,
            press: None,
            remote: crate::chat::Remote {
                place: match group {
                    true => "a Telegram group".into(),
                    false => "a Telegram chat".into(),
                },
                shared: group,
                history: None,
            },
        };
        found.push((album, incoming));
    }
    found.into_iter().map(|(_, message)| message).collect()
}

/// The files on one Telegram message. Of a photo's sizes, the largest a
/// model is sent, or the largest there is when none is that small.
fn telegram_files(message: &Value) -> Vec<Received> {
    let file = |kind, part: &Value, name: &str| {
        let id = part["file_id"].as_str()?;
        let name = part["file_name"].as_str().unwrap_or(name);
        let origin = Origin::Telegram(id.to_owned());
        Some(Received::new(
            kind,
            origin,
            name,
            part["file_size"].as_u64(),
        ))
    };
    let sizes = message["photo"].as_array().cloned().unwrap_or_default();
    let fits = |p: &&Value| {
        p["file_size"]
            .as_u64()
            .is_some_and(|s| s <= crate::media::PICTURE_BYTES as u64)
    };
    let photo = sizes.iter().rev().find(fits).or(sizes.last());
    [
        photo.and_then(|p| file(Kind::Photo, p, "photo")),
        file(Kind::Document, &message["document"], "document"),
        file(Kind::Voice, &message["voice"], "voice note.ogg"),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The files on a Discord message. Only from Discord's own servers: the
/// address comes in the message, and nothing else is fetched on its word.
pub(super) fn discord_attachments(message: &Value) -> Vec<Received> {
    // Discord's flag for a voice message, whose one file is the recording.
    let voice = message["flags"]
        .as_u64()
        .is_some_and(|flags| flags & (1 << 13) != 0);
    let files = message["attachments"].as_array().into_iter().flatten();
    files
        .filter_map(|file| {
            let url = file["url"].as_str().filter(|url| on_discord(url))?;
            let image = file["content_type"]
                .as_str()
                .is_some_and(|t| t.starts_with("image/"));
            let kind = match (voice, image) {
                (true, _) => Kind::Voice,
                (false, true) => Kind::Photo,
                (false, false) => Kind::Document,
            };
            let name = file["filename"].as_str().unwrap_or("file");
            let origin = Origin::Discord(url.to_owned());
            Some(Received::new(kind, origin, name, file["size"].as_u64()))
        })
        .take(MOST_FILES)
        .collect()
}

fn on_discord(url: &str) -> bool {
    [
        "https://cdn.discordapp.com/",
        "https://media.discordapp.net/",
    ]
    .iter()
    .any(|host| url.starts_with(host))
}

/// Whether `message` has files still to fetch.
pub fn unfetched(message: &Incoming) -> bool {
    message.files.iter().any(|file| file.saved.is_none())
}

/// `message` with its files fetched and saved in the `media/` of the
/// project `into`, or Letronna's own. One that cannot be had is left out,
/// and the message says so.
pub fn fetch(config: &Config, message: Incoming, into: Option<PathBuf>) -> Incoming {
    fetch_via(super::API, config, message, into)
}

fn fetch_via(api: &str, config: &Config, mut message: Incoming, into: Option<PathBuf>) -> Incoming {
    let ctx = ToolContext {
        workspace: into,
        ..Default::default()
    };
    for mut file in std::mem::take(&mut message.files) {
        if file.saved.is_none() {
            let fetched = download(api, config, &file).and_then(|bytes| keep(&ctx, &file, &bytes));
            match fetched {
                Ok(path) => file.saved = Some(path),
                Err(error) => {
                    let what = what(file.kind);
                    note(
                        &mut message.text,
                        &format!("({what} sent with this was lost: {error})"),
                    );
                    continue;
                }
            }
        }
        message.files.push(file);
    }
    message
}

fn what(kind: Kind) -> &'static str {
    match kind {
        Kind::Photo => "A photo",
        Kind::Document => "A file",
        Kind::Voice => "A voice note",
    }
}

/// The file's bytes, refused past `RECEIVE_BYTES` whatever it said it
/// weighed. Errors never carry the address: Telegram's holds the token.
fn download(api: &str, config: &Config, file: &Received) -> Result<Vec<u8>, String> {
    if file.size > RECEIVE_BYTES {
        return Err(too_big(file.size, RECEIVE_BYTES));
    }
    let client = client()?;
    let (url, app) = match &file.origin {
        Origin::Discord(url) => (url.clone(), "Discord"),
        Origin::Telegram(id) => {
            let token = config.telegram.token.trim();
            let body: Value = client
                .get(format!("{api}/bot{token}/getFile"))
                .query(&[("file_id", id)])
                .send()
                .map_err(|_| "Cannot reach Telegram.".to_owned())?
                .json()
                .map_err(|_| "Telegram sent an unreadable reply.".to_owned())?;
            let path = body["result"]["file_path"]
                .as_str()
                .ok_or("Telegram would not hand the file over.")?;
            (format!("{api}/file/bot{token}/{path}"), "Telegram")
        }
    };
    let reply = client
        .get(url)
        .send()
        .map_err(|_| format!("Cannot reach {app}."))?
        .error_for_status()
        .map_err(|_| format!("{app} would not hand the file over."))?;
    let mut bytes = Vec::new();
    reply
        .take(RECEIVE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| format!("The file from {app} was cut off."))?;
    if bytes.len() as u64 > RECEIVE_BYTES {
        return Err(too_big(bytes.len() as u64, RECEIVE_BYTES));
    }
    Ok(bytes)
}

/// Save a fetched file as `media::save` names what the media tools make:
/// from a slug of the sender's name for it and the time, so nothing the
/// sender chose reaches the file system but letters, digits and dashes.
fn keep(ctx: &ToolContext, file: &Received, bytes: &[u8]) -> Result<PathBuf, String> {
    let name = Path::new(&file.name);
    let about = name.file_stem().unwrap_or_default().to_string_lossy();
    let given = name
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .filter(|e| e.len() <= 8 && e.chars().all(|c| c.is_ascii_alphanumeric()));
    let extension = match (crate::media::picture_kind(bytes), file.kind) {
        (Some(kind), _) => kind.to_owned(),
        (None, Kind::Voice) => given.unwrap_or_else(|| "ogg".into()),
        (None, _) => given.unwrap_or_else(|| "bin".into()),
    };
    crate::media::save(ctx, &about, &extension, bytes)
}

fn too_big(size: u64, limit: u64) -> String {
    // Telegram's limits are round decimal megabytes, and read best as such.
    let most = match limit % 1_000_000 {
        0 => format!("{} MB", limit / 1_000_000),
        _ => crate::media::size(limit as usize),
    };
    format!(
        "it is {}, and the most is {most}",
        crate::media::size(size as usize)
    )
}

fn note(text: &mut String, line: &str) {
    if !text.is_empty() {
        text.push_str("\n\n");
    }
    text.push_str(line);
}

/// The prompt `message` makes, and its pictures: a picture small enough to
/// show goes with it, which the request names itself, and any other file
/// is named by where it was saved.
pub fn prompt(message: &Incoming) -> (String, Vec<PathBuf>) {
    let mut text = message.text.clone();
    let mut pictures = Vec::new();
    for file in &message.files {
        let Some(path) = &file.saved else {
            continue;
        };
        let shows = crate::media::is_picture(path)
            && std::fs::metadata(path).is_ok_and(|m| m.len() <= crate::media::PICTURE_BYTES as u64);
        match shows {
            true => pictures.push(path.clone()),
            false => note(
                &mut text,
                &format!("(Attached a file, saved to {}.)", path.display()),
            ),
        }
    }
    (text, pictures)
}

/// A chat's key in `Config::voice`.
fn chat_key(source: &Source) -> String {
    match source {
        Source::Telegram(chat) => format!("telegram:{chat}"),
        Source::Discord(channel) => format!("discord:{channel}"),
        Source::Slack { channel, .. } => format!("slack:{channel}"),
    }
}

/// Whether `message` has a voice note saved and not yet put into words.
pub fn unheard(message: &Incoming) -> bool {
    message
        .files
        .iter()
        .any(|file| file.kind == Kind::Voice && file.saved.is_some())
}

/// What the chat `source` answered `voice_question`, if it has been asked.
pub fn voice_choice(config: &Config, source: &Source) -> Option<bool> {
    config.voice.get(&chat_key(source)).copied()
}

/// Keep the chat's answer to `voice_question`. As on a card, only a plain
/// yes allows it: credit is spent on nothing less.
pub fn choose_voice(config: &mut Config, source: &Source, reply: &str) -> bool {
    let allowed = matches!(super::card_answer("", "", reply), super::CardAnswer::Allow);
    config.voice.insert(chat_key(source), allowed);
    allowed
}

/// What a chat is asked before its first voice note is transcribed, with
/// the price when the hosted model list can be had.
pub fn voice_question() -> String {
    let offers = crate::media::catalogue().unwrap_or_default();
    let chosen = crate::media::choices().remove("transcription");
    question(&offers, chosen.as_deref())
}

/// The question, priced by the model the user picked, else the cheapest.
fn question(offers: &[crate::media::Offer], chosen: Option<&str>) -> String {
    let mut transcribers = offers.iter().filter(|o| o.kind == "transcription");
    let first = transcribers.clone().next();
    let offer = transcribers
        .find(|o| Some(o.id.as_str()) == chosen)
        .or(first);
    let cost = match offer {
        Some(offer) => format!("costs {}, paid", offer.price()),
        None => "is paid".to_owned(),
    };
    format!(
        "Transcribing voice notes {cost} from your Letronna credit; allow it for this \
         chat? Reply yes or no."
    )
}

/// `message` with its voice notes in words: transcribed when `allowed`,
/// else only named, so the model knows one came and where it is.
pub fn hear(message: Incoming, allowed: bool) -> Incoming {
    hear_with(message, allowed, crate::media::transcribe_path)
}

fn hear_with(
    mut message: Incoming,
    allowed: bool,
    transcribe: impl Fn(&Path) -> Result<String, String>,
) -> Incoming {
    for file in std::mem::take(&mut message.files) {
        let (Kind::Voice, Some(path)) = (file.kind, &file.saved) else {
            message.files.push(file);
            continue;
        };
        let at = path.display();
        let line = match allowed.then(|| transcribe(path)) {
            None => format!(
                "(Sent a voice note, saved to {at}; this chat has not allowed transcribing it.)"
            ),
            Some(Ok(words)) if words.is_empty() => {
                format!("(Sent a voice note, saved to {at}; no speech was heard in it.)")
            }
            Some(Ok(words)) => format!("(Sent a voice note, saved to {at}. It says:)\n{words}"),
            Some(Err(error)) => {
                format!("(Sent a voice note, saved to {at}; it could not be transcribed: {error})")
            }
        };
        note(&mut message.text, &line);
        message.spoken = true;
    }
    message
}

/// Where a file is sent.
#[derive(Debug, PartialEq, Eq)]
enum To {
    Telegram { token: String, chat: i64 },
    Discord { token: String, channel: String },
}

impl To {
    fn of(config: &Config, source: &Source) -> Result<Self, String> {
        Ok(match source {
            Source::Telegram(chat) => To::Telegram {
                token: config.telegram.token.clone(),
                chat: *chat,
            },
            Source::Discord(channel) => To::Discord {
                token: config.discord.bot_token.clone(),
                channel: channel.clone(),
            },
            Source::Slack { .. } => return Err("Files cannot go to Slack yet.".into()),
        })
    }

    /// The chat a session talks in: a Discord session carries its channel
    /// with the history `read_chat` reads, and Telegram answers the one
    /// chat it is paired with.
    fn session(ctx: &ToolContext, config: &Config) -> Result<Self, String> {
        let remote = ctx
            .remote
            .as_ref()
            .ok_or("send_file works only in a session a chat app started.")?;
        if let Some(history) = &remote.history {
            return Ok(To::Discord {
                token: history.token.clone(),
                channel: history.channel.clone(),
            });
        }
        match (config.telegram.ready(), config.telegram.chat) {
            (true, Some(chat)) => To::of(config, &Source::Telegram(chat)),
            _ => Err("Telegram is no longer set up to answer this chat.".into()),
        }
    }

    fn api(&self) -> &'static str {
        match self {
            To::Telegram { .. } => super::API,
            To::Discord { .. } => super::DISCORD_API,
        }
    }

    fn limit(&self) -> u64 {
        match self {
            To::Telegram { .. } => TELEGRAM_BYTES,
            To::Discord { .. } => DISCORD_BYTES,
        }
    }
}

/// The file `raw` names, if it may leave this computer: one in the project,
/// or in Letronna's own media folder, which holds what chats sent and what
/// was made with no project; and never a file of keys, whatever the card
/// said.
fn pick(raw: &str, ctx: &ToolContext) -> Result<PathBuf, String> {
    let path = match own_media(raw, ctx) {
        Some(path) => path,
        None => {
            let path = crate::tools::resolve(raw, ctx)?;
            let project = ctx
                .workspace
                .as_ref()
                .and_then(|w| std::fs::canonicalize(w).ok());
            if !project.is_some_and(|project| path.starts_with(project)) {
                return Err(format!(
                    "`{raw}` is outside the project; only its files and Letronna's media \
                     folder can be sent."
                ));
            }
            path
        }
    };
    if crate::tools::secret(&path) {
        return Err(format!(
            "`{raw}` holds keys, passwords or tokens, so it is not sent."
        ));
    }
    if !path.is_file() {
        return Err(format!("`{raw}` is not a file."));
    }
    Ok(path)
}

/// `raw` as a file in Letronna's own `media/`, which `resolve` keeps out of
/// reach with the rest of that folder.
fn own_media(raw: &str, ctx: &ToolContext) -> Option<PathBuf> {
    // As `resolve` checks before opening anything: on Windows the opening
    // alone can sign in to another computer.
    if crate::paths::unsafe_raw(raw, ctx.workspace.as_deref()).is_some() {
        return None;
    }
    let media = std::fs::canonicalize(crate::chat::data_dir().join("media")).ok()?;
    let base = ctx
        .workspace
        .clone()
        .or_else(crate::tools::home)
        .unwrap_or_default();
    let path = std::fs::canonicalize(base.join(raw)).ok()?;
    path.starts_with(&media).then_some(path)
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(TRANSFER_TIME)
        .build()
        .map_err(|_| "Cannot initialize connection.".to_owned())
}

/// Send the file at `path` to `to`, with `caption` under it. Text leaves
/// as every message to a chat app does, with a saved key hidden: the
/// caption, the name and a file that is text.
fn upload(api: &str, to: &To, path: &Path, caption: &str) -> Result<(), String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let size = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if size > to.limit() {
        return Err(format!(
            "`{name}` was not sent: {}.",
            too_big(size, to.limit())
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let bytes = match std::str::from_utf8(&bytes) {
        Ok(text) if !text.contains('\0') => super::outgoing(text).into_bytes(),
        _ => bytes,
    };
    let named = PathBuf::from(super::outgoing(&name));
    let caption = super::trim_to(&super::outgoing(caption), CAPTION_CHARS);
    let (request, app) = match to {
        To::Telegram { token, chat } => {
            let (method, field) = telegram_method(path, size);
            let chat = chat.to_string();
            let mut fields = vec![("chat_id", chat.as_str())];
            if !caption.is_empty() {
                fields.push(("caption", caption.as_str()));
            }
            let (boundary, form) = crate::media::form(&fields, field, &named, &bytes);
            let request = client()?
                .post(format!("{api}/bot{}/{method}", token.trim()))
                .header(reqwest::header::CONTENT_TYPE, multipart(&boundary))
                .body(form);
            (request, "Telegram")
        }
        To::Discord { token, channel } => {
            let payload = json!({ "content": caption }).to_string();
            let fields = match caption.is_empty() {
                true => Vec::new(),
                false => vec![("payload_json", payload.as_str())],
            };
            let (boundary, form) = crate::media::form(&fields, "files[0]", &named, &bytes);
            let request = client()?
                .post(format!("{api}/channels/{channel}/messages"))
                .header("Authorization", format!("Bot {}", token.trim()))
                .header(reqwest::header::CONTENT_TYPE, multipart(&boundary))
                .body(form);
            (request, "Discord")
        }
    };
    let reply = request.send().map_err(|_| format!("Cannot reach {app}."))?;
    if reply.status().is_success() {
        return Ok(());
    }
    let status = reply.status().as_u16();
    // Telegram says why in `description`, Discord in `message`.
    let body: Value = reply.json().unwrap_or_default();
    let why = body["description"].as_str().or(body["message"].as_str());
    Err(match why {
        Some(why) => format!("{app} refused `{name}`: {why}"),
        None => format!("{app} refused `{name}` ({status})."),
    })
}

fn multipart(boundary: &str) -> String {
    format!("multipart/form-data; boundary={boundary}")
}

/// The Bot API method and field a file goes by: a picture Telegram can show
/// as a photo, audio it plays as a track or a voice note, and anything else
/// as a document.
fn telegram_method(path: &Path, size: u64) -> (&'static str, &'static str) {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" | "jpg" | "jpeg" | "webp" if size <= TELEGRAM_PHOTO_BYTES => ("sendPhoto", "photo"),
        "mp3" | "m4a" => ("sendAudio", "audio"),
        // A spoken reply comes as Opus in Ogg, which Telegram shows as a
        // voice note to play in place rather than a track.
        "ogg" | "oga" | "opus" => ("sendVoice", "voice"),
        _ => ("sendDocument", "document"),
    }
}

/// Send `paths` to the chat `source`, one at a time and unasked: what a
/// media tool made in the turn. Returns a line for the chat about each one
/// that could not go.
pub fn send_files(config: &Config, source: &Source, paths: &[PathBuf]) -> Vec<String> {
    let to = match To::of(config, source) {
        Ok(to) => to,
        Err(why) => return vec![why],
    };
    send_files_via(to.api(), &to, paths)
}

/// Send the turn's reply to `source`, then what it made, then a line on
/// anything that could not go, in that order.
pub fn answer(config: &Config, source: &Source, reply: &str, made: &[PathBuf]) {
    let _ = say(config, source, reply);
    for failed in send_files(config, source, made) {
        let _ = say(config, source, &failed);
    }
}

/// Send `text` to `source` as any reply goes.
fn say(config: &Config, source: &Source, text: &str) -> Result<(), String> {
    super::tell(config, source, text, None)
}

fn send_files_via(api: &str, to: &To, paths: &[PathBuf]) -> Vec<String> {
    paths
        .iter()
        .filter_map(|path| upload(api, to, path, "").err())
        .collect()
}

/// What a media tool made in the turn that ends `messages` -- a picture,
/// speech, a video -- oldest first, which goes to the chat after the reply;
/// less what `send_file` already sent.
pub fn made(messages: &[Message]) -> Vec<PathBuf> {
    let turn: Vec<&Message> = messages
        .iter()
        .rev()
        .take_while(|m| m.role != "user" || m.notice)
        .collect();
    let sent: Vec<&str> = turn
        .iter()
        .filter(|m| m.role == "tool" && m.name == SEND_FILE && !m.is_error)
        .map(|m| m.content.as_str())
        .collect();
    let mut made: Vec<PathBuf> = turn
        .iter()
        .filter(|m| m.role == "tool" && !m.is_error && MAKERS.contains(&m.name.as_str()))
        .filter_map(|m| crate::media::saved_by(&m.name, &m.content))
        .filter(|path| {
            // By name, which `send_file` reports: the two may spell the
            // folder differently, and a made file's name is its own.
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            !sent.iter().any(|s| s.contains(&format!("`{name}`")))
        })
        .collect();
    made.reverse();
    made
}

/// Sends a file to the chat app the session is talking in.
pub struct SendFile;

impl Tool for SendFile {
    fn name(&self) -> &'static str {
        SEND_FILE
    }
    fn description(&self) -> &'static str {
        "Send a file to the chat app this session is talking in: one in the project, or one in Letronna's media folder, such as a file the user sent. A picture arrives as a photo, MP3 or M4A audio as a track, anything else as a document. Telegram takes up to 50 MB from a bot (10 MB as a photo), Discord up to 20 MB. A picture, speech or video made this turn is sent after the reply without this."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "The file, relative to the workspace, or a path in Letronna's media folder."},
            "caption": {"type": "string", "description": "A line to show with it."}
        }, "required": ["path"]})
    }
    // It sends the user's data to someone else's servers, which is as much
    // a change as a write, and plan mode is not for it.
    fn needs_approval(&self) -> bool {
        true
    }
    fn mutates(&self) -> bool {
        true
    }
    fn offered(&self, connection: &crate::chat::Connection) -> bool {
        connection.remote.is_some()
    }
    fn preview(&self, args: &Value, ctx: &ToolContext) -> String {
        let place = ctx
            .remote
            .as_ref()
            .map_or("the chat", |remote| remote.place.as_str());
        let mut text = format!(
            "Send {} to {place}",
            args["path"].as_str().unwrap_or_default()
        );
        if let Some(caption) = args["caption"].as_str().filter(|c| !c.trim().is_empty()) {
            text.push_str(&format!("\n\n{}", super::quote(caption)));
        }
        text
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let raw = args["path"]
            .as_str()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or("`path` is required.")?;
        let to = To::session(ctx, &super::load())?;
        let path = pick(raw, ctx)?;
        let caption = args["caption"].as_str().unwrap_or_default();
        upload(to.api(), &to, &path, caption)?;
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        let place = ctx
            .remote
            .as_ref()
            .map_or("the chat", |remote| remote.place.as_str());
        Ok(format!("Sent `{name}` to {place}."))
    }
}

#[cfg(test)]
mod tests {
    use super::{Incoming, Kind, Origin, Received, Source, To};
    use crate::tools::ToolContext;
    use serde_json::json;
    use std::path::{Path, PathBuf};

    /// A request the fake API was sent.
    struct Asked {
        path: String,
        head: String,
        body: String,
    }

    /// A fake of a chat app's API: it answers each request in turn with the
    /// status and body given and hands back what each one asked, so nothing
    /// reaches a real bot or account.
    fn fake(answers: &[(&str, &[u8])]) -> (String, std::thread::JoinHandle<Vec<Asked>>) {
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let answers: Vec<(String, Vec<u8>)> = answers
            .iter()
            .map(|(status, body)| (status.to_string(), body.to_vec()))
            .collect();
        let worker = std::thread::spawn(move || {
            answers
                .iter()
                .map(|(status, body)| answer(&server, status, body))
                .collect()
        });
        (base, worker)
    }

    fn answer(server: &std::net::TcpListener, status: &str, reply: &[u8]) -> Asked {
        use std::io::{Read, Write};
        let (mut socket, _) = server.accept().unwrap();
        socket
            .set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut bytes = Vec::new();
        let mut block = [0; 65536];
        let (head, body) = loop {
            let n = socket.read(&mut block).unwrap();
            assert_ne!(n, 0);
            bytes.extend_from_slice(&block[..n]);
            let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                continue;
            };
            let head = String::from_utf8_lossy(&bytes[..end]).into_owned();
            let len: usize = head
                .to_lowercase()
                .lines()
                .find_map(|l| l.strip_prefix("content-length: "))
                .map_or(0, |n| n.parse().unwrap());
            if bytes.len() >= end + 4 + len {
                break (head, bytes[end + 4..end + 4 + len].to_vec());
            }
        };
        let top = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            reply.len()
        );
        socket.write_all(top.as_bytes()).unwrap();
        socket.write_all(reply).unwrap();
        Asked {
            path: head.split_whitespace().nth(1).unwrap().to_owned(),
            head,
            body: String::from_utf8_lossy(&body).into_owned(),
        }
    }

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-chat-files-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn incoming(text: &str, files: Vec<Received>) -> Incoming {
        Incoming {
            source: Source::Telegram(42),
            from: "user_a".into(),
            text: text.into(),
            said: text.into(),
            files,
            author: None,
            needs_session: false,
            spoken: false,
            press: None,
            remote: Default::default(),
        }
    }

    fn file(kind: Kind, origin: Origin, name: &str, size: u64) -> Received {
        Received::new(kind, origin, name, Some(size))
    }

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n\0\0\0\rIHDR";

    #[test]
    fn telegram_photos_documents_and_voice_notes_come_in_as_files() {
        let photo = |id: &str, size: u64| json!({"file_id": id, "file_size": size});
        let body = json!({"ok": true, "result": [
            {"update_id": 1, "message": {"chat": {"id": 42}, "text": "plain words"}},
            {"update_id": 2, "message": {"chat": {"id": 42}, "from": {"first_name": "user_a"},
                "media_group_id": "album", "caption": " what is this ",
                "photo": [photo("small", 900), photo("fits", 90_000), photo("huge", 9_000_000)]}},
            {"update_id": 3, "message": {"chat": {"id": 42}, "media_group_id": "album",
                "photo": [photo("second", 900)]}},
            {"update_id": 4, "message": {"chat": {"id": 42}, "document": {
                "file_id": "doc", "file_name": "report.pdf", "file_size": 1234}}},
            {"update_id": 5, "message": {"chat": {"id": -100}, "voice": {
                "file_id": "said", "file_size": 999, "mime_type": "audio/ogg"}}},
            {"update_id": 6, "message": {"chat": {"id": 42}, "sticker": {"file_id": "no"}}}
        ]});
        let messages = super::telegram_media(&body);
        assert_eq!(messages.len(), 3);
        // An album is one message, with the caption one of its photos had,
        // and each photo at the largest size a model is sent.
        let album = &messages[0];
        assert_eq!(
            (album.text.as_str(), album.from.as_str()),
            ("what is this", "user_a")
        );
        let origins: Vec<&Origin> = album.files.iter().map(|f| &f.origin).collect();
        assert_eq!(
            origins,
            [
                &Origin::Telegram("fits".into()),
                &Origin::Telegram("second".into())
            ]
        );
        assert!(
            album
                .files
                .iter()
                .all(|f| f.kind == Kind::Photo && f.saved.is_none())
        );
        let document = &messages[1].files[0];
        assert_eq!(
            (document.kind, document.name.as_str()),
            (Kind::Document, "report.pdf")
        );
        assert!(messages[1].text.is_empty());
        let voice = &messages[2];
        assert_eq!(voice.files[0].kind, Kind::Voice);
        assert_eq!(voice.source, Source::Telegram(-100));
        assert!(voice.remote.shared && voice.remote.history.is_none());
    }

    #[test]
    fn discord_files_come_only_from_discords_own_servers() {
        let message = json!({"attachments": [
            {"url": "https://cdn.discordapp.com/a/1/cat.png", "filename": "cat.png",
                "content_type": "image/png", "size": 10},
            {"url": "https://media.discordapp.net/a/2/notes.txt", "filename": "notes.txt",
                "content_type": "text/plain"},
            {"url": "http://127.0.0.1:9/secret", "filename": "local.png",
                "content_type": "image/png"}
        ]});
        let files = super::discord_attachments(&message);
        let kinds: Vec<(Kind, &str)> = files.iter().map(|f| (f.kind, f.name.as_str())).collect();
        assert_eq!(
            kinds,
            [(Kind::Photo, "cat.png"), (Kind::Document, "notes.txt")]
        );
        let mut voice = message.clone();
        voice["flags"] = json!(1 << 13);
        let files = super::discord_attachments(&voice);
        assert!(files.iter().all(|f| f.kind == Kind::Voice));
    }

    #[test]
    fn a_telegram_file_is_fetched_by_id_and_saved_under_a_clean_name() {
        let project = scratch("fetch");
        let found = br#"{"ok": true, "result": {"file_path": "documents/file_7.pdf"}}"#;
        let (api, worker) = fake(&[
            ("200 OK", found),
            ("200 OK", b"%PDF-1.4 report"),
            (
                "200 OK",
                br#"{"ok": true, "result": {"file_path": "photos/p.jpg"}}"#,
            ),
            ("200 OK", PNG),
            (
                "200 OK",
                br#"{"ok": true, "result": {"file_path": "gone"}}"#,
            ),
            ("404 Not Found", b""),
        ]);
        let telegram = |id: &str| Origin::Telegram(id.into());
        let message = incoming(
            "have a look",
            vec![
                file(Kind::Document, telegram("doc"), "../..\\Evil Name?.pdf", 15),
                file(Kind::Photo, telegram("pic"), "photo", 0),
                // Said to be too big, so never asked for.
                file(Kind::Document, telegram("big"), "big.zip", 30_000_000),
                file(Kind::Document, telegram("lost"), "lost.txt", 3),
            ],
        );
        let mut config = super::Config::default();
        config.telegram.token = "123:abc".into();
        let message = super::fetch_via(&api, &config, message, Some(project.clone()));
        let asked = worker.join().unwrap();
        assert_eq!(asked[0].path, "/bot123:abc/getFile?file_id=doc");
        assert_eq!(asked[1].path, "/file/bot123:abc/documents/file_7.pdf");
        assert!(!super::unfetched(&message));
        let saved: Vec<&Path> = message
            .files
            .iter()
            .filter_map(|f| f.saved.as_deref())
            .collect();
        assert_eq!(saved.len(), 2);
        assert!(
            saved
                .iter()
                .all(|p| p.parent() == Some(&project.join("media")))
        );
        let name = saved[0].file_name().unwrap().to_string_lossy();
        assert!(
            name.starts_with("evil-name-") && name.ends_with(".pdf"),
            "{name}"
        );
        assert_eq!(std::fs::read(saved[0]).unwrap(), b"%PDF-1.4 report");
        // The picture is named for what its bytes are.
        assert!(saved[1].to_string_lossy().ends_with(".png"));
        // The picture goes to the model; the rest is named, lost ones too.
        let (text, pictures) = super::prompt(&message);
        assert_eq!(pictures, [saved[1].to_path_buf()]);
        assert!(text.starts_with("have a look\n\n"), "{text}");
        assert!(
            text.contains(&format!(
                "(Attached a file, saved to {}.)",
                saved[0].display()
            )),
            "{text}"
        );
        assert!(
            text.contains("(A file sent with this was lost: it is 28.6 MB, and the most is 20 MB)"),
            "{text}"
        );
        assert!(
            text.contains(
                "(A file sent with this was lost: Telegram would not hand the file over.)"
            ),
            "{text}"
        );
        // No error names the address, which carries the bot's token.
        assert!(!text.contains("123:abc"));
        std::fs::remove_dir_all(project).unwrap();
    }

    #[test]
    fn a_discord_file_is_fetched_from_its_address() {
        let data = scratch("discord-fetch");
        let (api, worker) = fake(&[("200 OK", b"OggS voice")]);
        let voice = file(
            Kind::Voice,
            Origin::Discord(format!("{api}/a/v")),
            "voice-message.ogg",
            10,
        );
        let message = super::fetch_via(
            &api,
            &Default::default(),
            incoming("", vec![voice]),
            Some(data.clone()),
        );
        assert_eq!(worker.join().unwrap()[0].path, "/a/v");
        let saved = message.files[0].saved.clone().unwrap();
        assert!(saved.to_string_lossy().ends_with(".ogg"));
        assert!(super::unheard(&message));
        std::fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn a_file_goes_by_the_method_that_suits_it_with_saved_keys_hidden() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data = scratch("upload");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        let key = "a-saved-key-of-no-known-shape";
        std::fs::write(data.join("keys.json"), format!(r#"{{"OpenAI":"{key}"}}"#)).unwrap();
        std::fs::write(data.join("chart.png"), PNG).unwrap();
        std::fs::write(data.join("notes.txt"), format!("uses {key}\n")).unwrap();
        std::fs::write(data.join("song.mp3"), b"ID3tune").unwrap();
        let telegram = To::Telegram {
            token: "123:abc".into(),
            chat: 42,
        };
        let discord = To::Discord {
            token: "bot-token".into(),
            channel: "5".into(),
        };
        let (api, worker) = fake(&[("200 OK", &b"{}"[..]); 4]);
        let send =
            |to: &To, name: &str, caption: &str| super::upload(&api, to, &data.join(name), caption);
        send(&telegram, "chart.png", "The chart").unwrap();
        send(&telegram, "notes.txt", &format!("with {key}")).unwrap();
        send(&telegram, "song.mp3", "").unwrap();
        send(&discord, "chart.png", "").unwrap();
        let asked = worker.join().unwrap();
        let paths: Vec<&str> = asked.iter().map(|a| a.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "/bot123:abc/sendPhoto",
                "/bot123:abc/sendDocument",
                "/bot123:abc/sendAudio",
                "/channels/5/messages"
            ]
        );
        assert!(asked[0].body.contains("name=\"chat_id\"\r\n\r\n42\r\n"));
        assert!(
            asked[0]
                .body
                .contains("name=\"caption\"\r\n\r\nThe chart\r\n")
        );
        assert!(
            asked[0]
                .body
                .contains("name=\"photo\"; filename=\"chart.png\"")
        );
        // A text file and its caption leave with the key hidden.
        let marker = crate::redact::MARKER;
        assert!(!asked[1].body.contains(key));
        assert!(asked[1].body.contains(&format!("uses {marker}")));
        assert!(asked[1].body.contains(&format!("with {marker}")));
        assert!(
            asked[2]
                .body
                .contains("name=\"audio\"; filename=\"song.mp3\"")
        );
        assert!(!asked[2].body.contains("caption"));
        assert!(
            asked[3]
                .head
                .to_lowercase()
                .contains("authorization: bot bot-token")
        );
        assert!(
            asked[3]
                .body
                .contains("name=\"files[0]\"; filename=\"chart.png\"")
        );
        assert!(!asked[3].body.contains("payload_json"));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        std::fs::remove_dir_all(data).unwrap();
    }

    #[test]
    fn a_file_larger_than_the_app_takes_is_not_sent() {
        let dir = scratch("too-big");
        let big = dir.join("film.mp4");
        let file = std::fs::File::create(&big).unwrap();
        file.set_len(super::DISCORD_BYTES * 2).unwrap();
        // An address that refuses connections: any attempt would fail loudly.
        let discord = To::Discord {
            token: "t".into(),
            channel: "5".into(),
        };
        let refused = super::upload("http://127.0.0.1:1", &discord, &big, "").unwrap_err();
        assert_eq!(
            refused,
            "`film.mp4` was not sent: it is 40.0 MB, and the most is 20.0 MB."
        );
        file.set_len(super::TELEGRAM_BYTES + 1).unwrap();
        let telegram = To::Telegram {
            token: "t".into(),
            chat: 1,
        };
        let refused = super::upload("http://127.0.0.1:1", &telegram, &big, "").unwrap_err();
        assert!(refused.ends_with("the most is 50 MB."), "{refused}");
        // A picture too big for a photo goes as a document.
        let method = super::telegram_method(Path::new("a.PNG"), 12_000_000);
        assert_eq!(method, ("sendDocument", "document"));
        assert_eq!(
            super::telegram_method(Path::new("a.PNG"), 1),
            ("sendPhoto", "photo")
        );
        assert_eq!(
            super::telegram_method(Path::new("reply.ogg"), 1),
            ("sendVoice", "voice")
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn only_the_project_and_the_media_folder_can_be_sent_and_never_keys() {
        let _turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let data = scratch("pick");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &data) };
        // Apart from the data folder, whose files are out of reach.
        let project = scratch("pick-project");
        std::fs::create_dir_all(data.join("media")).unwrap();
        std::fs::write(data.join("media").join("voice-note.ogg"), b"OggS").unwrap();
        std::fs::write(data.join("keys.json"), b"{}").unwrap();
        std::fs::write(project.join("a.txt"), b"a").unwrap();
        std::fs::write(project.join(".env"), b"KEY=1").unwrap();
        let outside = scratch("pick-outside");
        std::fs::write(outside.join("b.txt"), b"b").unwrap();
        let mut ctx = ToolContext {
            workspace: Some(project.clone()),
            ..Default::default()
        };
        let pick = |raw: &Path, ctx: &ToolContext| super::pick(&raw.to_string_lossy(), ctx);
        assert!(pick(Path::new("a.txt"), &ctx).is_ok());
        let media = data.join("media").join("voice-note.ogg");
        assert!(pick(&media, &ctx).is_ok());
        let far = pick(&outside.join("b.txt"), &ctx).unwrap_err();
        assert!(far.contains("outside"), "{far}");
        // The rest of Letronna's folder stays out of reach, wherever it is.
        let own = pick(&data.join("keys.json"), &ctx).unwrap_err();
        assert!(
            own.contains("out of reach") || own.contains("outside"),
            "{own}"
        );
        assert!(pick(Path::new("missing.txt"), &ctx).is_err());
        // Refused even once the card said yes, which lets a read through.
        ctx.approved = true;
        let keys = pick(Path::new(".env"), &ctx).unwrap_err();
        assert!(keys.ends_with("so it is not sent."), "{keys}");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
        for dir in [data, project, outside] {
            std::fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn send_file_is_there_only_in_a_chat_app_and_finds_its_chat() {
        use crate::chat::{Connection, History, Remote};
        let names = |remote: Option<Remote>| {
            let connection = Connection {
                remote,
                ..Default::default()
            };
            crate::tools::specs(&connection)
                .iter()
                .any(|t| t["function"]["name"] == super::SEND_FILE)
        };
        assert!(!names(None));
        assert!(names(Some(Remote::default())));
        // Where other people read the chat, nothing on this computer is.
        let shared = Remote {
            shared: true,
            ..Default::default()
        };
        assert!(!names(Some(shared)));
        let refused = crate::tools::run(super::SEND_FILE, r#"{"path":"a"}"#, &Default::default());
        assert!(
            refused
                .unwrap_err()
                .contains("not available in this session")
        );

        let mut config = super::Config::default();
        config.telegram.enabled = true;
        config.telegram.token = "123:abc".into();
        config.telegram.chat = Some(42);
        let mut ctx = ToolContext {
            remote: Some(Remote::default()),
            ..Default::default()
        };
        let telegram = To::Telegram {
            token: "123:abc".into(),
            chat: 42,
        };
        assert_eq!(To::session(&ctx, &config), Ok(telegram));
        config.telegram.chat = None;
        assert!(To::session(&ctx, &config).is_err());
        ctx.remote = Some(Remote {
            history: Some(History {
                token: "bot-token".into(),
                channel: "5".into(),
                ..Default::default()
            }),
            ..Default::default()
        });
        let discord = To::Discord {
            token: "bot-token".into(),
            channel: "5".into(),
        };
        assert_eq!(To::session(&ctx, &config), Ok(discord));
    }

    #[test]
    fn what_the_turn_made_follows_the_reply_unless_it_was_sent_already() {
        use crate::chat::Message;
        let said = |role: &str, content: &str| Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        };
        let tool = |name: &str, content: &str, is_error: bool| Message {
            role: "tool".into(),
            name: name.into(),
            content: content.into(),
            is_error,
            ..Default::default()
        };
        let messages = [
            said("user", "draw a dog"),
            tool("make_image", "Saved to /p/media/dog.png", false),
            said("assistant", "Here."),
            said("user", "now a cat and say hi"),
            tool("make_image", "Saved to /p/media/cat.png\nA cat.", false),
            tool("speak", "Saved to /p/media/hi.mp3", false),
            tool("make_image", "Saved to /p/media/failed.png", true),
            tool("read_file", "Saved to /p/media/looked-at.png", false),
            tool(super::SEND_FILE, "Sent `hi.mp3` to a Telegram chat.", false),
            said("assistant", "Done."),
        ];
        assert_eq!(super::made(&messages), [PathBuf::from("/p/media/cat.png")]);
        assert!(super::made(&messages[..3]) == [PathBuf::from("/p/media/dog.png")]);
    }

    #[test]
    fn a_voice_note_is_put_in_words_only_once_its_chat_allows_it() {
        let mut config = super::Config::default();
        let here = Source::Telegram(42);
        let there = Source::Discord("5".into());
        assert_eq!(super::voice_choice(&config, &here), None);
        assert!(super::choose_voice(&mut config, &here, " Yes "));
        assert!(!super::choose_voice(&mut config, &there, "maybe later"));
        assert_eq!(super::voice_choice(&config, &here), Some(true));
        assert_eq!(super::voice_choice(&config, &there), Some(false));

        let offer = |id: &str, usd: f64| crate::media::Offer {
            id: id.into(),
            kind: "transcription".into(),
            usd,
            unit: "minute".into(),
            ..Default::default()
        };
        let offers = [offer("cheap", 0.003), offer("picked", 0.006)];
        let asked = super::question(&offers, None);
        assert!(asked.starts_with("Transcribing voice notes costs $0.003 / minute, paid"));
        assert!(asked.ends_with("allow it for this chat? Reply yes or no."));
        assert!(super::question(&offers, Some("picked")).contains("$0.006 / minute"));
        let unpriced = super::question(&[], None);
        assert!(unpriced.starts_with("Transcribing voice notes is paid from your Letronna credit"));

        let mut voice = file(
            Kind::Voice,
            Origin::Telegram("v".into()),
            "voice note.ogg",
            4,
        );
        voice.saved = Some(PathBuf::from("/m/voice-note.ogg"));
        let message = || incoming("listen", vec![voice.clone()]);
        assert!(super::unheard(&message()));
        let declined = super::hear_with(message(), false, |_| panic!("no transcript unasked"));
        assert!(!super::unheard(&declined) && declined.files.is_empty());
        assert_eq!(
            declined.text,
            "listen\n\n(Sent a voice note, saved to /m/voice-note.ogg; this chat has not \
             allowed transcribing it.)"
        );
        let heard = super::hear_with(message(), true, |_| Ok("buy milk".into()));
        assert!(
            heard
                .text
                .ends_with("saved to /m/voice-note.ogg. It says:)\nbuy milk")
        );
        let failed = super::hear_with(message(), true, |_| Err("Sign in first.".into()));
        assert!(
            failed
                .text
                .ends_with("it could not be transcribed: Sign in first.)")
        );
    }

    #[test]
    fn only_a_message_that_brought_a_voice_note_is_marked_spoken() {
        let mut voice = file(Kind::Voice, Origin::Telegram("v".into()), "note.ogg", 4);
        voice.saved = Some(PathBuf::from("/m/note.ogg"));
        let heard = super::hear_with(incoming("", vec![voice]), true, |_| Ok("hi".into()));
        assert!(heard.spoken);
        let declined = super::hear_with(incoming("", vec![]), false, |_| Ok("hi".into()));
        assert!(!declined.spoken);
        assert!(!incoming("typed", vec![]).spoken);
    }
}
