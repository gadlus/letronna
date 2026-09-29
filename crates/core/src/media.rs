//! Images, speech, transcription and video, made on Letronna's hosted models.
//!
//! These always go to the account API, whichever gateway the chat itself
//! uses: only Letronna sells them, and they are paid from the signed-in
//! account's credit. So every one of them asks first -- it spends money --
//! and its card says so. None is offered while signed out, when it could
//! only fail.
//!
//! What is made is saved as a file: in `media/` in the project folder, or in
//! Letronna's own data folder when no project is attached. The result starts
//! with `Saved to <path>`, which is how the window finds the file to show.

use crate::chat::Connection;
use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How a result names the file it saved; see `saved`.
pub(crate) const SAVED: &str = "Saved to ";
/// An image or a long reading can take minutes, and the API holds the reply
/// open until it is done.
const MAKE_TIME: Duration = Duration::from_secs(300);
/// A video renders for a few minutes; it is looked at this often while it does.
const VIDEO_POLL: Duration = Duration::from_secs(5);
/// Past this the tool stops waiting and says how to pick the video up later,
/// rather than holding the turn for however long the renderer takes.
const VIDEO_WAIT: Duration = Duration::from_secs(600);
/// The largest file the API takes for transcription.
const UPLOAD_BYTES: u64 = 25 * 1024 * 1024;

pub struct MakeImage;
impl Tool for MakeImage {
    fn name(&self) -> &'static str {
        "make_image"
    }
    fn description(&self) -> &'static str {
        "Make an image from a description on Letronna's hosted image models, and save it as a file. It is paid from the user's Letronna credit, a few cents an image, so use it only when the user asks for an image."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "prompt": {"type": "string", "description": "What the image should show, in detail."},
            "size": {"type": "string", "description": "Width x height in pixels, such as 1024x1024 or 1536x1024. Left out, the model's own."},
            "model": {"type": "string", "description": "A hosted image model's id. Left out, the one the user picked, or the cheapest working."}
        }, "required": ["prompt"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn offered(&self, _: &Connection) -> bool {
        crate::auth::signed_in()
    }
    fn mutates(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let mut what = format!(
            "An image of: {}",
            args["prompt"].as_str().unwrap_or_default()
        );
        if let Some(size) = args["size"].as_str() {
            what.push_str(&format!("\nSize: {size}"));
        }
        card(what, args, "image")
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let prompt = required(args, "prompt")?;
        let token = token()?;
        let mut body = json!({"prompt": prompt, "n": 1});
        pass(args, &mut body, &["size"], "image");
        let reply: Value = send(
            client(MAKE_TIME)?
                .post(endpoint("/images/generations"))
                .bearer_auth(&token)
                .json(&body),
        )?
        .json()
        .map_err(|_| "The image came back in a form Letronna could not read.".to_owned())?;
        // The reply is held open while the image is made, so a refusal can
        // arrive inside a success.
        if let Some(why) = refusal(&reply) {
            return Err(why);
        }
        let encoded = reply["data"][0]["b64_json"]
            .as_str()
            .ok_or("No image came back.")?;
        let bytes = base64(encoded).ok_or("The image came back damaged.")?;
        let path = save(ctx, prompt, image_extension(&bytes), &bytes)?;
        Ok(format!(
            "{SAVED}{}\nA {} image.",
            path.display(),
            size(bytes.len())
        ))
    }
}

pub struct Speak;
impl Tool for Speak {
    fn name(&self) -> &'static str {
        "speak"
    }
    fn description(&self) -> &'static str {
        "Read text aloud with a hosted voice model and save the audio as a file. It is paid from the user's Letronna credit by the character, so use it only when the user asks for speech or a voice-over."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "text": {"type": "string", "description": "What to say, exactly as it should be read."},
            "voice": {"type": "string", "description": "A voice the model knows, such as alloy. Left out, the model picks one."},
            "model": {"type": "string", "description": "A hosted speech model's id. Left out, the one the user picked, or the cheapest working."}
        }, "required": ["text"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn offered(&self, _: &Connection) -> bool {
        crate::auth::signed_in()
    }
    fn mutates(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let mut what = format!("Read aloud: {}", args["text"].as_str().unwrap_or_default());
        if let Some(voice) = args["voice"].as_str() {
            what.push_str(&format!("\nVoice: {voice}"));
        }
        card(what, args, "speech")
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let text = required(args, "text")?;
        let token = token()?;
        let mut body = json!({"input": text});
        pass(args, &mut body, &["voice"], "speech");
        let reply = send(
            client(MAKE_TIME)?
                .post(endpoint("/audio/speech"))
                .bearer_auth(&token)
                .json(&body),
        )?;
        let kind = reply
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .to_owned();
        let bytes = reply
            .bytes()
            .map_err(|_| "The audio was cut off on its way here.".to_owned())?;
        let path = save(ctx, text, audio_extension(&kind), &bytes)?;
        Ok(format!(
            "{SAVED}{}\n{} characters read aloud, {}.",
            path.display(),
            text.chars().count(),
            size(bytes.len())
        ))
    }
}

pub struct Transcribe;
impl Tool for Transcribe {
    fn name(&self) -> &'static str {
        "transcribe"
    }
    fn description(&self) -> &'static str {
        "Turn the speech in an audio or video file into text, on Letronna's hosted models. Files up to 25 MB. It is paid from the user's Letronna credit by the minute of audio."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "path": {"type": "string", "description": "The file, relative to the workspace."},
            "model": {"type": "string", "description": "A hosted transcription model's id. Left out, the one the user picked, or the cheapest working."}
        }, "required": ["path"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn offered(&self, _: &Connection) -> bool {
        crate::auth::signed_in()
    }
    // It changes nothing on disk, but it spends credit, which plan mode is
    // not for.
    fn mutates(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        card(
            format!("Transcribe {}", args["path"].as_str().unwrap_or_default()),
            args,
            "transcription",
        )
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let raw = required(args, "path")?;
        let path = crate::tools::resolve(raw, ctx)?;
        let length = std::fs::metadata(&path).map_err(|e| e.to_string())?.len();
        if length > UPLOAD_BYTES {
            return Err(format!(
                "`{raw}` is {}; files up to 25 MB can be transcribed. Cut it into parts first.",
                size(length as usize)
            ));
        }
        let audio = std::fs::read(&path).map_err(|e| e.to_string())?;
        let token = token()?;
        let (boundary, form) = form(&path, &audio, model_for(args, "transcription").as_deref());
        let reply: Value = send(
            client(MAKE_TIME)?
                .post(endpoint("/audio/transcriptions"))
                .bearer_auth(&token)
                .header(
                    reqwest::header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(form),
        )?
        .json()
        .map_err(|_| "The transcript came back in a form Letronna could not read.".to_owned())?;
        let text = reply["text"].as_str().unwrap_or_default().trim();
        if text.is_empty() {
            return Ok(format!("No speech was heard in `{raw}`."));
        }
        Ok(format!("Transcript of `{raw}`:\n\n{text}"))
    }
}

pub struct MakeVideo;
impl Tool for MakeVideo {
    fn name(&self) -> &'static str {
        "make_video"
    }
    fn description(&self) -> &'static str {
        "Make a short video from a description on Letronna's hosted video models, and save it as an MP4 file. It takes a few minutes. It is paid from the user's Letronna credit by the second of video, several cents a second, so use it only when the user asks for a video."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "prompt": {"type": "string", "description": "What the video should show, in detail."},
            "seconds": {"type": "integer", "description": "How long, such as 4 or 8. Each model makes only certain lengths; left out, the model's own."},
            "model": {"type": "string", "description": "A hosted video model's id. Left out, the one the user picked, or the cheapest working."},
            "job": {"type": "string", "description": "The id of a video an earlier call left still being made: wait for that one and save it, instead of starting another."}
        }})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn offered(&self, _: &Connection) -> bool {
        crate::auth::signed_in()
    }
    fn mutates(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        if let Some(job) = args["job"].as_str() {
            return format!("Wait for video {job}, already paid for, and save it.");
        }
        let long = match args["seconds"].as_u64() {
            Some(seconds) => format!("A {seconds}-second video"),
            None => "A video".to_owned(),
        };
        card(
            format!("{long} of: {}", args["prompt"].as_str().unwrap_or_default()),
            args,
            "video",
        )
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let token = token()?;
        let id = match args["job"].as_str() {
            Some(job) => job.to_owned(),
            None => {
                let mut body = json!({"prompt": required(args, "prompt")?});
                pass(args, &mut body, &["seconds"], "video");
                let job: Value = send(
                    client(MAKE_TIME)?
                        .post(endpoint("/videos"))
                        .bearer_auth(&token)
                        .json(&body),
                )?
                .json()
                .map_err(|_| "The video was started, but its id could not be read.".to_owned())?;
                job["id"]
                    .as_str()
                    .ok_or("The video could not be started.")?
                    .to_owned()
            }
        };
        let look = client(Duration::from_secs(30))?;
        let started = Instant::now();
        let seconds = loop {
            let job: Value = send(
                look.get(endpoint(&format!("/videos/{id}")))
                    .bearer_auth(&token),
            )?
            .json()
            .map_err(|_| "The video's progress could not be read.".to_owned())?;
            match job["status"].as_str() {
                Some("completed") => break job["seconds"].as_u64(),
                Some("failed") => {
                    return Err("The video could not be made. It is not charged.".into());
                }
                _ => {}
            }
            if started.elapsed() >= VIDEO_WAIT {
                return Ok(format!(
                    "The video is still being made. Call make_video again with job \"{id}\" to \
                     wait for it and save it; it is charged once, when it is done."
                ));
            }
            std::thread::sleep(VIDEO_POLL);
        };
        let bytes = send(
            client(MAKE_TIME)?
                .get(endpoint(&format!("/videos/{id}/content")))
                .bearer_auth(&token),
        )?
        .bytes()
        .map_err(|_| "The video was cut off on its way here.".to_owned())?;
        let about = args["prompt"].as_str().unwrap_or("video");
        let path = save(ctx, about, "mp4", &bytes)?;
        let long = seconds.map(|s| format!("{s}-second ")).unwrap_or_default();
        Ok(format!(
            "{SAVED}{}\nA {long}video, {}.",
            path.display(),
            size(bytes.len())
        ))
    }
}

/// The file a media tool's result says it saved, for the window to show.
pub fn saved(result: &str) -> Option<PathBuf> {
    result
        .lines()
        .next()?
        .strip_prefix(SAVED)
        .map(PathBuf::from)
}

/// The tools whose results start with `SAVED` and the file they wrote.
const SAVERS: [&str; 4] = ["make_image", "speak", "make_video", crate::browser::BROWSE];

/// The file `tool`'s result says it saved, believed only from `SAVERS`, and
/// from browse only when Windows would open it as a plain file.
pub fn saved_by(tool: &str, result: &str) -> Option<PathBuf> {
    // Any other result -- a file read out, a command's output -- can start
    // with the same words, and the window would then open that path.
    if !SAVERS.contains(&tool) {
        return None;
    }
    let path = saved(result)?;
    // A page's title heads browse's listing, so a page can write this line
    // and name any share. The media tools write theirs themselves, into the
    // project, which may sit on a share of its own.
    let named_by_page = tool == crate::browser::BROWSE;
    if named_by_page && crate::paths::unsafe_raw(&path.to_string_lossy(), None).is_some() {
        return None;
    }
    Some(path)
}

/// What the hosted models make besides text, as the model menus name each
/// kind, in the order they list them.
pub const KINDS: [(&str, &str); 4] = [
    ("image", "Image"),
    ("speech", "Speech"),
    ("transcription", "Transcription"),
    ("video", "Video"),
];

/// A hosted model that makes something, and what it costs.
#[derive(Clone, Debug, Default)]
pub struct Offer {
    pub id: String,
    /// One of `KINDS`.
    pub kind: String,
    pub maker: String,
    /// Dollars per `unit`, our markup included, as the models page shows it.
    pub usd: f64,
    pub unit: String,
}

impl Offer {
    /// The price as a menu shows it: "$0.022 / image".
    pub fn price(&self) -> String {
        let per = match self.unit.as_str() {
            "kchar" => "1K chars",
            other => other,
        };
        format!("{} / {per}", dollars(self.usd))
    }
}

/// Two figures under ten cents, cents above: $0.022, $0.0002, $0.26.
fn dollars(usd: f64) -> String {
    if usd >= 0.1 {
        return format!("${usd:.2}");
    }
    let places = (1. - usd.log10().floor()).clamp(2., 8.) as usize;
    let text = format!("{usd:.places$}");
    format!("${}", text.trim_end_matches('0'))
}

/// Every hosted model that makes images, speech, transcripts or video, with
/// what it costs, cheapest first. The list is public: no sign-in needed.
pub fn catalogue() -> Result<Vec<Offer>, String> {
    let list: Vec<Value> = client(Duration::from_secs(20))?
        .get(crate::auth::api("/models?kind=all"))
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|_| "Could not load the hosted models.".to_owned())?
        .json()
        .map_err(|_| "The model list came back in a form Letronna could not read.".to_owned())?;
    Ok(offers(&list))
}

/// The media models in a `/models?kind=all` list, cheapest first.
fn offers(list: &[Value]) -> Vec<Offer> {
    let mut offers: Vec<Offer> = list
        .iter()
        .filter_map(|m| {
            let kind = m["kind"].as_str()?;
            KINDS.iter().any(|(k, _)| *k == kind).then(|| Offer {
                id: m["id"].as_str().unwrap_or_default().to_owned(),
                kind: kind.to_owned(),
                maker: m["maker"].as_str().unwrap_or_default().to_owned(),
                usd: m["price"]["usd"].as_f64().unwrap_or_default(),
                unit: m["price"]["unit"].as_str().unwrap_or_default().to_owned(),
            })
        })
        .filter(|o| !o.id.is_empty() && o.usd > 0.)
        .collect();
    offers.sort_by(|a, b| a.usd.total_cmp(&b.usd));
    offers
}

/// The model picked in the model menu for each kind: what its tool uses when
/// a call names none. A kind with no pick uses the cheapest working.
pub fn choices() -> BTreeMap<String, String> {
    std::fs::read(choices_path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

/// Pick `model` for `kind`, or with `None` go back to the cheapest working.
pub fn choose(kind: &str, model: Option<&str>) -> Result<(), String> {
    let mut all = choices();
    match model {
        Some(model) => all.insert(kind.to_owned(), model.to_owned()),
        None => all.remove(kind),
    };
    let _ = std::fs::create_dir_all(crate::chat::data_dir());
    let bytes = serde_json::to_vec_pretty(&all).map_err(|e| e.to_string())?;
    std::fs::write(choices_path(), bytes).map_err(|e| format!("Cannot save the pick: {e}"))
}

fn choices_path() -> PathBuf {
    crate::chat::data_dir().join("media.json")
}

/// The signed-in account's token, or what to do about there being none.
fn token() -> Result<String, String> {
    crate::auth::account().map(|a| a.token).ok_or_else(|| {
        "Images, speech and video are made on Letronna's hosted models, which need \
         a Letronna account: sign in with /login first."
            .to_owned()
    })
}

fn endpoint(path: &str) -> String {
    format!("{}{path}", crate::auth::hosted_base())
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

/// Send a request, and hand back the reply or the API's own reason for
/// refusing it: out of credit, signed out, no such model.
fn send(request: reqwest::blocking::RequestBuilder) -> Result<reqwest::blocking::Response, String> {
    let reply = request.send().map_err(|e| {
        if e.is_timeout() {
            "Letronna's hosted models took too long to answer.".to_owned()
        } else {
            "Could not reach Letronna's hosted models.".to_owned()
        }
    })?;
    if reply.status().is_success() {
        return Ok(reply);
    }
    let status = reply.status().as_u16();
    let body: Value = reply.json().unwrap_or_default();
    Err(refusal(&body).unwrap_or_else(|| format!("Letronna's hosted models answered {status}.")))
}

/// The error an API reply carries, in either of the shapes it comes in.
fn refusal(body: &Value) -> Option<String> {
    body["error"]["message"]
        .as_str()
        .or(body["error"].as_str())
        .map(str::to_owned)
}

fn required<'a>(args: &'a Value, name: &str) -> Result<&'a str, String> {
    args[name]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("`{name}` is required."))
}

/// Copy the optional arguments that were given into the request body, and
/// the model: the one named, else the one picked for `kind`.
fn pass(args: &Value, body: &mut Value, names: &[&str], kind: &str) {
    for name in names {
        if !args[*name].is_null() {
            body[*name] = args[*name].clone();
        }
    }
    if let Some(model) = model_for(args, kind) {
        body["model"] = model.into();
    }
}

/// The model a call goes to: the one it names, else the one picked for
/// `kind` in the model menu. None leaves the choice to the API.
fn model_for(args: &Value, kind: &str) -> Option<String> {
    args["model"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| choices().remove(kind))
}

/// The approval card: what will be made, by which model, and who pays.
fn card(what: String, args: &Value, kind: &str) -> String {
    let model =
        model_for(args, kind).unwrap_or_else(|| "the cheapest hosted model working".to_owned());
    format!("{what}\n\nMade by {model}, paid from your Letronna credit.")
}

/// Save what was made under `media/`, named after what it shows and when.
pub(crate) fn save(
    ctx: &ToolContext,
    about: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    let dir = ctx
        .workspace
        .clone()
        .unwrap_or_else(crate::chat::data_dir)
        .join("media");
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot make {}: {e}", dir.display()))?;
    let name = format!(
        "{}-{}",
        slug(about),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let mut path = dir.join(format!("{name}.{extension}"));
    // Two made in the same second must not overwrite each other.
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{name}-{n}.{extension}"));
        n += 1;
    }
    std::fs::write(&path, bytes).map_err(|e| format!("Cannot save {}: {e}", path.display()))?;
    Ok(path)
}

/// The first words of a description, as a file name carries them anywhere.
fn slug(text: &str) -> String {
    let mut slug = String::new();
    for word in text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .take(6)
    {
        if slug.len() + word.len() >= 40 {
            break;
        }
        if !slug.is_empty() {
            slug.push('-');
        }
        slug.push_str(&word.to_ascii_lowercase());
    }
    if slug.is_empty() {
        "media".to_owned()
    } else {
        slug
    }
}

/// The multipart form the transcription route takes, and its boundary.
/// Written out here because reqwest's own needs a feature built into nothing
/// else.
fn form(path: &Path, audio: &[u8], model: Option<&str>) -> (String, Vec<u8>) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let boundary = format!("letronna-{stamp:x}");
    // A quote or line break would end the header early.
    let name: String = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "audio".into())
        .chars()
        .map(|c| if c == '"' || c.is_control() { '_' } else { c })
        .collect();
    let mut body = Vec::with_capacity(audio.len() + 512);
    if let Some(model) = model {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\n{model}\r\n"
            )
            .as_bytes(),
        );
    }
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\
             Content-Type: {}\r\n\r\n",
            audio_type(path)
        )
        .as_bytes(),
    );
    body.extend_from_slice(audio);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    (boundary, body)
}

/// What kind of audio a file is, from its name; some models refuse a file
/// they are not told the kind of.
fn audio_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "mp3" | "mpga" | "mpeg" => "audio/mpeg",
        "wav" => "audio/wav",
        "m4a" | "mp4" => "audio/mp4",
        "ogg" | "oga" | "opus" => "audio/ogg",
        "webm" => "audio/webm",
        "flac" => "audio/flac",
        _ => "application/octet-stream",
    }
}

/// The file type an image's first bytes say it is.
fn image_extension(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "jpg"
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]) {
        "webp"
    } else {
        "png"
    }
}

/// The file type audio came back as, from its content type.
fn audio_extension(content_type: &str) -> &'static str {
    match content_type.split(';').next().unwrap_or_default().trim() {
        "audio/wav" | "audio/x-wav" | "audio/wave" => "wav",
        "audio/ogg" | "audio/opus" => "ogg",
        "audio/flac" => "flac",
        "audio/aac" => "aac",
        _ => "mp3",
    }
}

fn size(bytes: usize) -> String {
    if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024. * 1024.))
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

/// Standard base64, as images come back from the API. Decoding it is a dozen
/// lines, and it would be the only use of a crate for it.
pub(crate) fn base64(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut buffer, mut bits) = (0u32, 0);
    for byte in text.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buffer >> bits) as u8);
        }
    }
    Some(out)
}

/// An image file as a `data:` address, which is how a chat request carries
/// a picture. None when the file cannot be read.
pub(crate) fn data_url(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    let kind = match image_extension(&bytes) {
        "jpg" => "jpeg",
        other => other,
    };
    Some(format!("data:image/{kind};base64,{}", to_base64(&bytes)))
}

/// Standard base64 the other way, for pictures sent to a model.
fn to_base64(bytes: &[u8]) -> String {
    const LETTERS: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let group = chunk.iter().enumerate().fold(0u32, |group, (at, byte)| {
            group | u32::from(*byte) << (16 - 8 * at)
        });
        for at in 0..4 {
            match at <= chunk.len() {
                true => out.push(LETTERS[(group >> (18 - 6 * at)) as usize & 63] as char),
                false => out.push('='),
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_what_the_api_sends() {
        assert_eq!(base64("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64("aGVsbG8gd29ybGQ=").unwrap(), b"hello world");
        assert_eq!(base64("YQ==").unwrap(), b"a");
        assert_eq!(base64("").unwrap(), b"");
        // Wrapped lines, as some encoders write them.
        assert_eq!(base64("aGVs\nbG8=").unwrap(), b"hello");
        // Every byte value survives, so an image does too.
        let all: Vec<u8> = (0..=255).collect();
        let encoded = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0BBQkNERUZHSElKS0xNTk9QUVJTVFVWV1hZWltcXV5fYGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6e3x9fn+AgYKDhIWGh4iJiouMjY6PkJGSk5SVlpeYmZqbnJ2en6ChoqOkpaanqKmqq6ytrq+wsbKztLW2t7i5uru8vb6/wMHCw8TFxsfIycrLzM3Oz9DR0tPU1dbX2Nna29zd3t/g4eLj5OXm5+jp6uvs7e7v8PHy8/T19vf4+fr7/P3+/w==";
        assert_eq!(base64(encoded).unwrap(), all);
        assert!(base64("not base64!").is_none());
    }

    #[test]
    fn base64_goes_back_the_way_it_came() {
        for bytes in [&b""[..], b"a", b"ab", b"abc", b"hello world"] {
            assert_eq!(base64(&to_base64(bytes)).unwrap(), bytes);
        }
        assert_eq!(to_base64(b"hello"), "aGVsbG8=");
        let all: Vec<u8> = (0..=255).collect();
        assert_eq!(base64(&to_base64(&all)).unwrap(), all);
        let dir = std::env::temp_dir().join(format!("letronna-data-url-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let picture = dir.join("shot.jpg");
        std::fs::write(&picture, [0xFF, 0xD8, 0xFF, 0xE0]).unwrap();
        assert_eq!(
            data_url(&picture).unwrap(),
            "data:image/jpeg;base64,/9j/4A=="
        );
        assert_eq!(data_url(&dir.join("gone.jpg")), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn files_are_named_after_what_they_show() {
        assert_eq!(
            slug("A small red lighthouse, at dusk!"),
            "a-small-red-lighthouse-at-dusk"
        );
        assert_eq!(slug("¿Qué?"), "qu");
        assert_eq!(slug("…"), "media");
        let long = slug("supercalifragilistic expialidocious antidisestablishmentarianism");
        assert!(long.len() < 40, "{long}");
        assert_eq!(image_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), "jpg");
        assert_eq!(image_extension(b"RIFF\0\0\0\0WEBPVP8 "), "webp");
        assert_eq!(image_extension(b"\x89PNG\r\n"), "png");
        assert_eq!(audio_extension("audio/wav; codecs=1"), "wav");
        assert_eq!(audio_extension("audio/mpeg"), "mp3");
    }

    #[test]
    fn made_files_land_in_media_and_never_overwrite() {
        let dir = std::env::temp_dir().join(format!("letronna-media-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let first = save(&ctx, "a cat", "png", b"one").unwrap();
        let second = save(&ctx, "a cat", "png", b"two").unwrap();
        assert_ne!(first, second);
        assert_eq!(first.parent().unwrap(), dir.join("media"));
        assert_eq!(std::fs::read(&first).unwrap(), b"one");
        assert_eq!(std::fs::read(&second).unwrap(), b"two");
        // The window finds the file from the result the model is given.
        let result = format!("{SAVED}{}\nA 1 KB image.", first.display());
        assert_eq!(saved(&result), Some(first));
        assert_eq!(saved("Transcript of `a.mp3`:"), None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_saved_file_is_believed_only_from_a_tool_that_saves_and_from_a_page_never_on_a_share() {
        let result = "Saved to media/a-cat.png\nA 1 KB image.";
        for name in SAVERS {
            assert!(crate::tools::find(name).is_some(), "{name} missing");
            assert_eq!(
                saved_by(name, result),
                Some(PathBuf::from("media/a-cat.png"))
            );
        }
        for name in ["read_file", "run_command", "web_fetch", "transcribe"] {
            assert_eq!(saved_by(name, result), None, "{name}");
        }
        let share = r"Saved to \\nowhere.invalid\s\a.png — https://example.com";
        assert_eq!(saved_by("browse", share).is_none(), cfg!(windows));
        // What the media tools made in a project on a share is still shown.
        let made = r"Saved to \\nas\p\app\media\a-cat.png";
        assert_eq!(
            saved_by("make_image", made),
            Some(PathBuf::from(r"\\nas\p\app\media\a-cat.png"))
        );
    }

    #[test]
    fn the_transcription_form_carries_the_file_and_the_model() {
        let (boundary, body) = form(
            Path::new("clips/say \"hi\".mp3"),
            b"ID3audio",
            Some("whisper-1"),
        );
        let text = String::from_utf8_lossy(&body);
        assert!(text.starts_with(&format!("--{boundary}\r\n")), "{text}");
        assert!(
            text.contains("name=\"model\"\r\n\r\nwhisper-1\r\n"),
            "{text}"
        );
        assert!(
            text.contains(
                "filename=\"say _hi_.mp3\"\r\nContent-Type: audio/mpeg\r\n\r\nID3audio\r\n"
            ),
            "{text}"
        );
        assert!(text.ends_with(&format!("--{boundary}--\r\n")), "{text}");
        let (_, bare) = form(Path::new("a.wav"), b"", None);
        assert!(!String::from_utf8_lossy(&bare).contains("name=\"model\""));
    }

    #[test]
    fn refusals_are_read_in_either_shape() {
        assert_eq!(
            refusal(&json!({"error": {"message": "Not enough credit."}})).as_deref(),
            Some("Not enough credit.")
        );
        assert_eq!(
            refusal(&json!({"error": "this device was signed out, run /login"})).as_deref(),
            Some("this device was signed out, run /login")
        );
        assert_eq!(refusal(&json!({"data": []})), None);
    }

    #[test]
    fn every_media_tool_asks_first_and_stays_out_of_plan_mode() {
        for name in ["make_image", "speak", "transcribe", "make_video"] {
            let tool = crate::tools::find(name).unwrap_or_else(|| panic!("{name} missing"));
            assert!(
                tool.needs_approval() && tool.mutates() && tool.machine(),
                "{name}"
            );
        }
        let card = MakeImage.preview(&json!({"prompt": "a cat"}), &ToolContext::default());
        assert!(
            card.contains("a cat") && card.contains("Letronna credit"),
            "{card}"
        );
    }

    #[test]
    fn the_menu_lists_media_models_cheapest_first_and_priced_plainly() {
        let list = json!([
            {"id": "claude-sonnet-5", "maker": "Anthropic", "kind": "chat"},
            {"id": "flux.2-pro", "maker": "Black Forest Labs", "kind": "image",
             "price": {"unit": "image", "usd": 0.032351}},
            {"id": "gemini-3.1-flash-lite-image", "maker": "Google", "kind": "image",
             "price": {"unit": "image", "usd": 0.02184}},
            {"id": "kokoro-82m", "maker": "Hexgrad", "kind": "speech",
             "price": {"unit": "kchar", "usd": 0.004108}},
            {"id": "whisper-large-v3-turbo", "kind": "transcription",
             "price": {"unit": "minute", "usd": 0.000205}},
            {"id": "veo-3.1", "kind": "video", "price": {"unit": "second", "usd": 0.4108}},
            {"id": "unpriced", "kind": "video"}
        ]);
        let offers = offers(list.as_array().unwrap());
        let ids: Vec<&str> = offers.iter().map(|o| o.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "whisper-large-v3-turbo",
                "kokoro-82m",
                "gemini-3.1-flash-lite-image",
                "flux.2-pro",
                "veo-3.1"
            ]
        );
        let prices: Vec<String> = offers.iter().map(Offer::price).collect();
        assert_eq!(
            prices,
            [
                "$0.0002 / minute",
                "$0.0041 / 1K chars",
                "$0.022 / image",
                "$0.032 / image",
                "$0.41 / second"
            ]
        );
    }

    #[test]
    #[ignore = "spends a few cents of real credit: sign in, then run with --ignored"]
    fn an_image_is_made_and_saved() {
        let dir = std::env::temp_dir().join(format!("letronna-media-live-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            ..Default::default()
        };
        let out = MakeImage
            .run(&json!({"prompt": "a small red lighthouse at dusk"}), &ctx)
            .unwrap();
        let path = saved(&out).unwrap();
        assert!(std::fs::metadata(&path).unwrap().len() > 1000, "{out}");
        println!("{out}");
    }
}
