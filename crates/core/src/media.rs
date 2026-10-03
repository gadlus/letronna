//! Images, speech, transcription and video, made on Letronna's hosted models.
//!
//! These always go to the account API, whichever gateway the chat itself
//! uses: only Letronna sells them, and they are paid from the signed-in
//! account's credit. So every one of them asks first -- it spends money --
//! and its card says so. None is offered while signed out, when it could
//! only fail. Editing a picture is the exception: it goes to the chat's own
//! gateway, since OpenAI and the servers copying it have the same route, and
//! is offered signed out whenever that gateway is not Letronna.
//!
//! What is made is saved as a file: in `media/` in the project folder, or in
//! Letronna's own data folder when no project is attached. The result starts
//! with `Saved to <path>`, which is how the window finds the file to show.
//! A video renders for minutes, so make_video hands the wait to a background
//! job and the line comes in that job's notice instead.
//! A picture read_file opened or an MCP server sent, and one the user
//! attached, is saved the same way but always in Letronna's own folder, as
//! browse's screenshots are: it belongs to the session, and looking writes
//! nothing into the project. The chat shows the newest few to the model.

use crate::chat::Connection;
use crate::tools::{Tool, ToolContext};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// How a result names the file it saved; see `saved`.
pub(crate) const SAVED: &str = "Saved to ";
/// An image or a long reading can take minutes, and the API holds the reply
/// open until it is done.
const MAKE_TIME: Duration = Duration::from_secs(300);
/// A spoken sentence is short; one that takes longer than this would be
/// heard too late to follow the reply.
const SPEECH_TIME: Duration = Duration::from_secs(60);
/// A video renders for a few minutes; it is looked at this often while it does.
const VIDEO_POLL: Duration = Duration::from_secs(5);
/// Past this its job gives up and says how to pick the video up later, so a
/// render the service has lost does not leave a job running for good.
const VIDEO_WAIT: Duration = Duration::from_secs(1800);
/// The largest file the API takes for transcription.
pub const UPLOAD_BYTES: u64 = 25 * 1024 * 1024;
/// The largest picture a model is sent: Anthropic refuses one over 5 MB of
/// base64, which is four thirds of the file.
pub const PICTURE_BYTES: usize = 5 * 1024 * 1024 / 4 * 3;

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
            client()?
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

pub struct EditImage;
impl Tool for EditImage {
    fn name(&self) -> &'static str {
        "edit_image"
    }
    fn description(&self) -> &'static str {
        "Change pictures as a description says -- edit one, combine several, or redraw only where a mask marks -- and save the result as a file. The pictures are files in the project, or ones already in this conversation by the path their result named. It goes to the chat's own gateway: on Letronna's hosted models it is paid from the user's credit, a few cents an image. Use it only when the user asks for a picture to be changed."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "images": {"type": "array", "items": {"type": "string"}, "description": "The pictures to edit, relative to the workspace, or a path in Letronna's media folder as an earlier result named it. Up to 16; the mask applies to the first."},
            "prompt": {"type": "string", "description": "What to change, or what to make of the pictures, in detail."},
            "mask": {"type": "string", "description": "A PNG the size of the first picture whose fully transparent parts are the only ones redrawn. Left out, the model decides."},
            "size": {"type": "string", "description": "Width x height in pixels, such as 1024x1024. Left out, the model's own."},
            "model": {"type": "string", "description": "An image model that can edit. Left out, the one the user picked, or the gateway's own."}
        }, "required": ["images", "prompt"]})
    }
    fn needs_approval(&self) -> bool {
        true
    }
    // Off Letronna it is the chat's own gateway that is paid, which needs no
    // account; on it, the credit does.
    fn offered(&self, connection: &Connection) -> bool {
        crate::auth::signed_in() || !hosted(&connection.endpoint)
    }
    fn mutates(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, ctx: &ToolContext) -> String {
        let named: Vec<String> = pictures(args).iter().map(|p| format!("`{p}`")).collect();
        let mut what = format!(
            "Edit {}: {}",
            named.join(", "),
            args["prompt"].as_str().unwrap_or_default()
        );
        if let Some(mask) = args["mask"].as_str() {
            what.push_str(&format!("\nOnly where `{mask}` is transparent."));
        }
        if let Some(size) = args["size"].as_str() {
            what.push_str(&format!("\nSize: {size}"));
        }
        let endpoint = ctx
            .caller
            .connection
            .as_deref()
            .map(|c| c.endpoint.clone())
            .unwrap_or_default();
        if hosted(&endpoint) {
            return card(what, args, "image");
        }
        let model = args["model"].as_str().unwrap_or("its own model");
        let at = match endpoint.trim() {
            "" => "the chat's gateway".to_owned(),
            endpoint => endpoint.to_owned(),
        };
        format!("{what}\n\nMade by {model} at {at}, paid to that gateway.")
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let prompt = required(args, "prompt")?;
        let Some(connection) = ctx.caller.connection.as_deref() else {
            return Err("Pictures are edited on the chat's gateway, and none was given.".into());
        };
        let chat = crate::chat::validate(connection)?;
        let base = chat.strip_suffix("/chat/completions").unwrap_or(&chat);
        let on_letronna = hosted(base);
        let place = if on_letronna {
            "Letronna's hosted models".to_owned()
        } else {
            format!("the gateway at {base}")
        };
        // The pick in the model menu is a hosted model's, which another
        // gateway would not know.
        let model = match on_letronna {
            true => model_for(args, "image"),
            false => args["model"].as_str().map(str::to_owned),
        };
        let rules = edit_rules(model.as_deref());
        let raws = pictures(args);
        if raws.is_empty() {
            return Err("`images` is required: the pictures to edit.".into());
        }
        if raws.len() > rules.most {
            return Err(format!(
                "{} pictures were given; {} takes at most {}.",
                raws.len(),
                model.as_deref().unwrap_or("an image edit"),
                rules.most
            ));
        }
        let mut images = Vec::new();
        for raw in &raws {
            let path = crate::messaging::files::pick(raw, ctx)?;
            images.push(upload_ready(&path, raw, rules.bytes, rules.png)?);
        }
        let mask = match args["mask"].as_str() {
            Some(raw) => {
                let path = crate::messaging::files::pick(raw, ctx)?;
                let mask = upload_ready(&path, raw, MASK_BYTES, true)?;
                let (first, shape) = (dimensions(&images[0].1), dimensions(&mask.1));
                if first != shape {
                    return Err(format!(
                        "The mask `{raw}` is {}, and `{}` is {}; a mask must be the size of the first picture.",
                        shown(shape),
                        raws[0],
                        shown(first)
                    ));
                }
                Some(mask)
            }
            None => None,
        };
        let mut fields = vec![("prompt", prompt.to_owned()), ("n", "1".to_owned())];
        if let Some(model) = &model {
            fields.push(("model", model.clone()));
        }
        if let Some(size) = args["size"].as_str() {
            fields.push(("size", size.to_owned()));
        }
        // dall-e-2 answers with a link unless asked, and the newer models
        // refuse the field.
        if rules.legacy {
            fields.push(("response_format", "b64_json".to_owned()));
        }
        let fields: Vec<(&str, &str)> = fields.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let field = if rules.legacy { "image" } else { "image[]" };
        let mut files: Vec<(&str, &Path, &[u8])> = images
            .iter()
            .map(|(name, bytes)| (field, name.as_path(), bytes.as_slice()))
            .collect();
        if let Some((name, bytes)) = &mask {
            files.push(("mask", name.as_path(), bytes.as_slice()));
        }
        let (boundary, body) = forms(&fields, &files);
        // The form holds a copy of each, so they go now rather than being
        // held beside it, up to 800 MB more, for as long as the upload runs.
        drop((images, mask));
        // Not followed, as the chat's own requests are not: a 307 would send
        // the pictures on to wherever the gateway pointed, plain HTTP and
        // this network included.
        let mut request = reqwest::blocking::Client::builder()
            .timeout(MAKE_TIME)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| e.to_string())?
            .post(format!("{base}/images/edits"))
            .header(
                reqwest::header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(body);
        if !connection.key.trim().is_empty() {
            request = request.bearer_auth(connection.key.trim());
        }
        let reply = request.send().map_err(|e| match e.is_timeout() {
            true => format!("No answer came from {place} in time."),
            false => format!("Could not reach {place}."),
        })?;
        let status = reply.status().as_u16();
        let reply: Value = reply.json().unwrap_or_default();
        if !(200..300).contains(&status) {
            return Err(edit_refusal(status, &reply, &place, model.as_deref()));
        }
        if let Some(why) = refusal(&reply) {
            return Err(why);
        }
        // A gateway picking dall-e-2 when no model is named, as OpenAI's
        // does, answers with a link; it is not fetched, since the gateway
        // could point it anywhere, but the paid edit is not called a failure.
        let Some(encoded) = reply["data"][0]["b64_json"].as_str() else {
            return Err(match reply["data"][0]["url"].as_str() {
                Some(link) => format!(
                    "The edit was made, but {place} sent it as a link, which is not \
                     fetched: {link}. Give the user the link rather than editing again; \
                     name a model that sends the picture itself, such as gpt-image-1, \
                     for it to be saved."
                ),
                None => "No edited image came back.".to_owned(),
            });
        };
        let bytes = base64(encoded).ok_or("The edited image came back damaged.")?;
        let path = save(ctx, prompt, image_extension(&bytes), &bytes)?;
        let from: Vec<String> = raws.iter().map(|raw| format!("`{raw}`")).collect();
        Ok(format!(
            "{SAVED}{}\nA {} image, edited from {}.",
            path.display(),
            size(bytes.len()),
            from.join(", ")
        ))
    }
}

/// The largest picture the edits route takes, as OpenAI documents it for
/// its gpt-image models, which most gateways copy.
const EDIT_BYTES: u64 = 50 * 1024 * 1024;
/// The largest mask, and the largest picture dall-e-2 takes.
const MASK_BYTES: u64 = 4 * 1024 * 1024;

/// What the edits route takes from a model, so a picture it would refuse is
/// refused here before it is uploaded and paid for.
struct EditRules {
    /// How many pictures at once.
    most: usize,
    /// How large each may be.
    bytes: u64,
    /// Whether each must be a PNG.
    png: bool,
    /// dall-e-2, which names the field `image` and sends links by default.
    legacy: bool,
}

fn edit_rules(model: Option<&str>) -> EditRules {
    let legacy = model.is_some_and(|m| m.starts_with("dall-e"));
    match legacy {
        true => EditRules {
            most: 1,
            bytes: MASK_BYTES,
            png: true,
            legacy,
        },
        false => EditRules {
            most: 16,
            bytes: EDIT_BYTES,
            png: false,
            legacy,
        },
    }
}

/// The `images` a call names; a single path is taken as a list of one,
/// which is how models often send it.
fn pictures(args: &Value) -> Vec<String> {
    let list = match &args["images"] {
        Value::String(one) => vec![one.as_str()],
        Value::Array(all) => all.iter().filter_map(Value::as_str).collect(),
        _ => Vec::new(),
    };
    list.into_iter()
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Whether `endpoint` is Letronna's own hosted gateway.
fn hosted(endpoint: &str) -> bool {
    // Named with the chat route too, as `run` takes it, so the card and the
    // offer say who is paid as the call does.
    let base = endpoint.trim().trim_end_matches('/');
    base.strip_suffix("/chat/completions").unwrap_or(base) == crate::auth::hosted_base()
}

/// The picture at `path` as it is uploaded, and the name it goes by: as it
/// is when the route takes it, else as a PNG. `most` is checked on the file
/// before it is read, and again on a PNG made of it.
fn upload_ready(
    path: &Path,
    raw: &str,
    most: u64,
    png: bool,
) -> Result<(PathBuf, Vec<u8>), String> {
    let length = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if length > most {
        return Err(format!(
            "`{raw}` is {}; an image edit takes pictures up to {}.",
            size(length as usize),
            size(most as usize)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    // Named by what it is, as a picture saved under the wrong extension
    // would be sent as the wrong type.
    match picture_kind(&bytes) {
        Some("png") => return Ok((path.with_extension("png"), bytes)),
        Some(kind @ ("jpg" | "webp")) if !png => return Ok((path.with_extension(kind), bytes)),
        _ => {}
    }
    let picture = image::load_from_memory(&bytes).map_err(|_| {
        format!("`{raw}` is not a picture that can be edited: a PNG, JPEG, WebP, GIF or BMP.")
    })?;
    // With an alpha channel, which a mask is read by; taken rather than
    // copied, as a small GIF can decode to hundreds of megabytes.
    let mut converted = Vec::new();
    image::DynamicImage::ImageRgba8(picture.into_rgba8())
        .write_to(
            &mut std::io::Cursor::new(&mut converted),
            image::ImageFormat::Png,
        )
        .map_err(|e| format!("`{raw}` could not be made a PNG: {e}"))?;
    if converted.len() as u64 > most {
        return Err(format!(
            "`{raw}` is {} as a PNG, which it has to be sent as; an image edit takes pictures up to {}.",
            size(converted.len()),
            size(most as usize)
        ));
    }
    Ok((path.with_extension("png"), converted))
}

/// A picture's width and height, read from its header alone.
fn dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

/// Width by height as a message says it.
fn shown(dimensions: Option<(u32, u32)>) -> String {
    dimensions.map_or("of a size that cannot be read".to_owned(), |(w, h)| {
        format!("{w}x{h}")
    })
}

/// Why the edits route turned a request down, in words that say what to do:
/// a gateway without the route answers 404 or 405, and one whose model
/// cannot edit usually says so about the model.
fn edit_refusal(status: u16, body: &Value, place: &str, model: Option<&str>) -> String {
    let why = refusal(body).or_else(|| body["detail"].as_str().map(str::to_owned));
    let about_model = body["error"]["param"] == "model"
        || body["error"]["code"]
            .as_str()
            .is_some_and(|code| code.contains("model"))
        || why
            .as_deref()
            .is_some_and(|why| why.to_ascii_lowercase().contains("model"));
    match status {
        404 | 400 | 422 if about_model => {
            let who = model.map_or("The model it picks".to_owned(), |m| format!("`{m}`"));
            let why = why.unwrap_or_else(|| format!("it answered {status}"));
            format!(
                "{who} cannot edit images on {place} ({why}). Name a model that can, such as gpt-image-1."
            )
        }
        404 | 405 => format!(
            "There is no image edits route on {place}: /images/edits answered {status}. \
             Pictures can be edited on Letronna's hosted models, or on an OpenAI-compatible \
             server that has that route."
        ),
        _ => why.unwrap_or_else(|| format!("The edit was turned down: {place} answered {status}.")),
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
        let mut body = json!({"input": text});
        pass(args, &mut body, &["voice"], "speech");
        let (bytes, extension) = read_aloud(&body, MAKE_TIME)?;
        let path = save(ctx, text, extension, &bytes)?;
        Ok(format!(
            "{SAVED}{}\n{} characters read aloud, {}.",
            path.display(),
            text.chars().count(),
            size(bytes.len())
        ))
    }
}

/// Read `text` aloud as `format` (wav, opus, mp3) with the picked speech model, for spoken replies.
pub fn speech(text: &str, format: &str) -> Result<(Vec<u8>, &'static str), String> {
    let mut body = json!({"input": text, "response_format": format});
    pass(&Value::Null, &mut body, &[], "speech");
    read_aloud(&body, SPEECH_TIME)
}

/// Send a speech request, and hand back the audio and its file type.
fn read_aloud(body: &Value, timeout: Duration) -> Result<(Vec<u8>, &'static str), String> {
    let token = token()?;
    let reply = send(
        client()?
            .post(endpoint("/audio/speech"))
            .timeout(timeout)
            .bearer_auth(&token)
            .json(body),
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
    Ok((bytes.to_vec(), audio_extension(&kind)))
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
        let text = transcribe(&path, args)?;
        if text.is_empty() {
            return Ok(format!("No speech was heard in `{raw}`."));
        }
        Ok(format!("Transcript of `{raw}`:\n\n{text}"))
    }
}

/// The speech in the audio file at `path` as text, empty when none was
/// heard, on the transcription model the user picked; paid from the
/// signed-in account's credit, so the caller asks first.
pub fn transcribe_path(path: &Path) -> Result<String, String> {
    transcribe(path, &Value::Null)
}

/// `transcribe_path` on the model `args` names, if it names one.
fn transcribe(path: &Path, args: &Value) -> Result<String, String> {
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let length = std::fs::metadata(path).map_err(|e| e.to_string())?.len();
    if length > UPLOAD_BYTES {
        return Err(format!(
            "`{name}` is {}; files up to 25 MB can be transcribed. Cut it into parts first.",
            size(length as usize)
        ));
    }
    let audio = std::fs::read(path).map_err(|e| e.to_string())?;
    let token = token()?;
    let model = model_for(args, "transcription");
    let fields: Vec<(&str, &str)> = model.iter().map(|m| ("model", m.as_str())).collect();
    let (boundary, form) = form(&fields, "file", path, &audio);
    let reply: Value = send(
        client()?
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
    Ok(reply["text"].as_str().unwrap_or_default().trim().to_owned())
}

pub struct MakeVideo;
impl Tool for MakeVideo {
    fn name(&self) -> &'static str {
        "make_video"
    }
    fn description(&self) -> &'static str {
        "Make a short video from a description on Letronna's hosted video models, and save it as an MP4 file. It renders for a few minutes as a background job: the call returns the job's number at once, read_job shows how far it has got, wait waits for it, stop_job cancels it, and a notice with the file's path comes when it is saved. It is paid from the user's Letronna credit by the second of video, several cents a second, so use it only when the user asks for a video."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "prompt": {"type": "string", "description": "What the video should show, in detail."},
            "seconds": {"type": "integer", "description": "How long, such as 4 or 8. Each model makes only certain lengths; left out, the model's own."},
            "model": {"type": "string", "description": "A hosted video model's id. Left out, the one the user picked, or the cheapest working."},
            "job": {"type": "string", "description": "The id of a video an earlier render left unsaved: pick that one up and save it, instead of starting another."}
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
        start_video(Hosted { token }, args, ctx)
    }
}

/// Where a video is rendered: Letronna's hosted models, or a stand-in when
/// testing.
trait Renderer: Send + 'static {
    /// Start rendering what `body` asks for, and say the render's id.
    fn start(&self, body: &Value) -> Result<String, String>;
    /// Where render `id` has got to.
    fn look(&self, id: &str) -> Result<Render, String>;
    /// The finished video.
    fn fetch(&self, id: &str) -> Result<Vec<u8>, String>;
    /// How long to leave between looks.
    fn every(&self) -> Duration {
        VIDEO_POLL
    }
}

/// Where a render has got to.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Render {
    /// Still going, with how far in percent if the service says.
    Going(Option<u64>),
    /// Finished, with how many seconds long if the service says.
    Done(Option<u64>),
    Failed,
}

/// The video routes of Letronna's hosted models.
struct Hosted {
    token: String,
}

impl Renderer for Hosted {
    fn start(&self, body: &Value) -> Result<String, String> {
        let job: Value = send(
            client()?
                .post(endpoint("/videos"))
                .bearer_auth(&self.token)
                .json(body),
        )?
        .json()
        .map_err(|_| "The video was started, but its id could not be read.".to_owned())?;
        let Some(id) = job["id"].as_str() else {
            return Err("The video could not be started.".into());
        };
        Ok(id.to_owned())
    }

    fn look(&self, id: &str) -> Result<Render, String> {
        let job: Value = send(
            client()?
                .get(endpoint(&format!("/videos/{id}")))
                .bearer_auth(&self.token),
        )?
        .json()
        .map_err(|_| "The video's progress could not be read.".to_owned())?;
        Ok(match job["status"].as_str() {
            Some("completed") => Render::Done(job["seconds"].as_u64()),
            Some("failed") => Render::Failed,
            _ => Render::Going(job["progress"].as_u64()),
        })
    }

    fn fetch(&self, id: &str) -> Result<Vec<u8>, String> {
        let bytes = send(
            client()?
                .get(endpoint(&format!("/videos/{id}/content")))
                .bearer_auth(&self.token),
        )?
        .bytes()
        .map_err(|_| "The video was cut off on its way here.".to_owned())?;
        Ok(bytes.to_vec())
    }
}

/// Start the render `args` asks for, or take up the one its `job` names, and
/// leave the waiting to a background job so the turn goes on meanwhile.
fn start_video(renderer: impl Renderer, args: &Value, ctx: &ToolContext) -> Result<String, String> {
    let render = match args["job"].as_str() {
        Some(job) => job.to_owned(),
        None => {
            let mut body = json!({"prompt": required(args, "prompt")?});
            pass(args, &mut body, &["seconds"], "video");
            renderer.start(&body)?
        }
    };
    let about = args["prompt"].as_str().unwrap_or("video").to_owned();
    let label = format!("make_video: {}", crate::tools::one_line(&about));
    let workspace = ctx.workspace.clone();
    let id = render.clone();
    let job = crate::jobs::start_work(&label, ctx.conversation, move |work| {
        let ctx = ToolContext {
            workspace,
            ..Default::default()
        };
        await_video(&renderer, &id, &about, &ctx, work)
    })?;
    Ok(format!(
        "Started as job #{job}: video {render} is rendering, which takes a few minutes. \
         read_job shows how far it has got, wait with job {job} waits for it, and stop_job \
         cancels it. A notice with the file's path comes when it is saved."
    ))
}

/// Wait for render `id`, noting its progress in the job's log, then save it
/// where the media tools save what they make.
fn await_video(
    renderer: &dyn Renderer,
    id: &str,
    about: &str,
    ctx: &ToolContext,
    work: &crate::jobs::Work,
) -> Result<String, String> {
    // A stop leaves the render to finish or not on the service, which has no
    // route to cancel one, so the log says how to take it up again.
    let stopped = || {
        Err(format!(
            "Stopped. The video may still finish on the hosted models; make_video \
             with job \"{id}\" saves it then."
        ))
    };
    work.say(&format!("Rendering video {id}."));
    let started = Instant::now();
    let mut said = None;
    let seconds = loop {
        match renderer.look(id)? {
            Render::Done(seconds) => break seconds,
            Render::Failed => return Err("The video could not be made. It is not charged.".into()),
            Render::Going(progress) => {
                if let Some(percent) = progress.filter(|p| said != Some(*p)) {
                    work.say(&format!("{percent}% rendered."));
                    said = Some(percent);
                }
            }
        }
        if started.elapsed() >= VIDEO_WAIT {
            return Err(format!(
                "The video is still being made after {} minutes. Call make_video again \
                 with job \"{id}\" to wait for it and save it; it is charged once, when \
                 it is done.",
                VIDEO_WAIT.as_secs() / 60
            ));
        }
        if !work.sleep(renderer.every()) {
            return stopped();
        }
    };
    let bytes = renderer.fetch(id)?;
    if work.stopped() {
        return stopped();
    }
    let path = save(ctx, about, "mp4", &bytes)?;
    let long = seconds.map(|s| format!("{s}-second ")).unwrap_or_default();
    Ok(format!(
        "{SAVED}{}\nA {long}video, {}.",
        path.display(),
        size(bytes.len())
    ))
}

/// The file a media tool's result says it saved, for the window to show.
pub fn saved(result: &str) -> Option<PathBuf> {
    result
        .lines()
        .next()?
        .strip_prefix(SAVED)
        .map(PathBuf::from)
}

/// The tools whose results start with `SAVED` and the file they wrote; an
/// MCP tool's too, which saves the pictures its server sends.
const SAVERS: [&str; 7] = [
    "make_image",
    "edit_image",
    "speak",
    "make_video",
    "read_file",
    crate::browser::BROWSE,
    crate::computer::COMPUTER,
];

/// The file `tool`'s result says it saved, believed only from `SAVERS`, and
/// from browse only when Windows would open it as a plain file.
pub fn saved_by(tool: &str, result: &str) -> Option<PathBuf> {
    // Any other result -- a file read out, a command's output -- can start
    // with the same words, and the window would then open that path.
    if !SAVERS.contains(&tool) && !crate::mcp::is_tool(tool) {
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
    let list: Vec<Value> = client()?
        .get(crate::auth::api("/models?kind=all"))
        .timeout(Duration::from_secs(20))
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

/// The one HTTP client every hosted-media call goes through, kept open
/// between calls. It allows `MAKE_TIME`, and a call that must answer sooner
/// sets its own timeout.
fn client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: std::sync::OnceLock<Result<reqwest::blocking::Client, String>> =
        std::sync::OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(MAKE_TIME)
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
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

/// Save a picture the user attached, in Letronna's own `media/` as browse's
/// screenshots are: it belongs to the session, not the project. `name` is
/// what it was called.
pub fn attach(name: &str, bytes: &[u8]) -> Result<PathBuf, String> {
    let kind =
        picture_kind(bytes).ok_or("Only a PNG, JPEG, GIF or WebP picture can be attached.")?;
    if bytes.len() > PICTURE_BYTES {
        return Err(format!(
            "The picture is {}; the most a model is sent is {}.",
            size(bytes.len()),
            size(PICTURE_BYTES)
        ));
    }
    save(&ToolContext::default(), name, kind, bytes)
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
    save_in(&dir, about, extension, bytes)
}

/// Save `bytes` in `dir`, named after what they show and when.
pub(crate) fn save_in(
    dir: &Path,
    about: &str,
    extension: &str,
    bytes: &[u8],
) -> Result<PathBuf, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("Cannot make {}: {e}", dir.display()))?;
    let name = format!(
        "{}-{}",
        slug(about),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    );
    let mut path = dir.join(format!("{name}.{extension}"));
    // Two made in the same second must not overwrite each other, even when
    // made at once, as two pictures read_file opens side by side can be.
    let mut n = 2;
    loop {
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match created {
            Ok(mut file) => {
                file.write_all(bytes)
                    .map_err(|e| format!("Cannot save {}: {e}", path.display()))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                path = dir.join(format!("{name}-{n}.{extension}"));
                n += 1;
            }
            Err(e) => return Err(format!("Cannot save {}: {e}", path.display())),
        }
    }
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

/// A multipart form of the text `fields` and then `bytes` as the file
/// `field`, named as `path` is, and its boundary: what the transcription
/// route and the chat apps' uploads take. Written out here because
/// reqwest's own needs a feature built into nothing else.
pub(crate) fn form(
    fields: &[(&str, &str)],
    field: &str,
    path: &Path,
    bytes: &[u8],
) -> (String, Vec<u8>) {
    forms(fields, &[(field, path, bytes)])
}

/// `form` with any number of files, each as its field, the path it is named
/// as, and its bytes: what the image edits route takes.
fn forms(fields: &[(&str, &str)], files: &[(&str, &Path, &[u8])]) -> (String, Vec<u8>) {
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    let boundary = format!("letronna-{stamp:x}");
    let total: usize = files.iter().map(|(_, _, bytes)| bytes.len()).sum();
    let mut body = Vec::with_capacity(total + 512);
    for (key, value) in fields {
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{key}\"\r\n\r\n{value}\r\n"
            )
            .as_bytes(),
        );
    }
    for (field, path, bytes) in files {
        // A quote or line break would end the header early.
        let name: String = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "file".into())
            .chars()
            .map(|c| if c == '"' || c.is_control() { '_' } else { c })
            .collect();
        body.extend_from_slice(
            format!(
                "--{boundary}\r\nContent-Disposition: form-data; name=\"{field}\"; filename=\"{name}\"\r\n\
                 Content-Type: {}\r\n\r\n",
                file_type(path)
            )
            .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (boundary, body)
}

/// What kind of file a file is, from its name; some models refuse audio
/// they are not told the kind of.
fn file_type(path: &Path) -> &'static str {
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
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

/// The file type an image's first bytes say it is.
fn image_extension(bytes: &[u8]) -> &'static str {
    picture_kind(bytes).unwrap_or("png")
}

/// The file type of a picture every provider takes -- PNG, JPEG, GIF or
/// WebP -- as its first bytes say; None for anything else.
pub fn picture_kind(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("jpg")
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some("gif")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(&b"WEBP"[..]) {
        Some("webp")
    } else {
        None
    }
}

/// Whether `path` is named as a picture a model can be shown.
pub fn is_picture(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        ["png", "jpg", "jpeg", "gif", "webp"]
            .iter()
            .any(|kind| extension.eq_ignore_ascii_case(kind))
    })
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

pub(crate) fn size(bytes: usize) -> String {
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
/// a picture. None when the file cannot be read, is not a picture every
/// provider takes, or is larger than `PICTURE_BYTES`.
pub(crate) fn data_url(path: &Path) -> Option<String> {
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() > PICTURE_BYTES {
        return None;
    }
    let kind = match picture_kind(&bytes)? {
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
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    #[test]
    fn every_media_call_shares_one_client() {
        let first = client().expect("the client builds");
        let second = client().expect("the client builds");
        assert!(std::ptr::eq(first, second));
    }

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
        assert!(saved_by("mcp__demo__shoot", result).is_some());
        for name in ["edit_file", "run_command", "web_fetch", "transcribe"] {
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
            &[("model", "whisper-1")],
            "file",
            Path::new("clips/say \"hi\".mp3"),
            b"ID3audio",
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
        let (_, bare) = form(&[], "file", Path::new("a.wav"), b"");
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
        for name in [
            "make_image",
            "edit_image",
            "speak",
            "transcribe",
            "make_video",
        ] {
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

    /// A renderer that renders nothing: it reports 40% until `done` is set,
    /// then what `end` says, and hands back `b"mp4"`.
    #[derive(Clone)]
    struct Fake {
        done: Arc<AtomicBool>,
        end: Render,
        refuse: bool,
    }

    impl Fake {
        fn new(end: Render) -> Self {
            Self {
                done: Arc::default(),
                end,
                refuse: false,
            }
        }
    }

    impl Renderer for Fake {
        fn start(&self, _: &Value) -> Result<String, String> {
            match self.refuse {
                true => Err("Not enough credit.".into()),
                false => Ok("vid_1".into()),
            }
        }
        fn look(&self, _: &str) -> Result<Render, String> {
            match self.done.load(Ordering::Relaxed) {
                true => Ok(self.end),
                false => Ok(Render::Going(Some(40))),
            }
        }
        fn fetch(&self, _: &str) -> Result<Vec<u8>, String> {
            Ok(b"mp4".to_vec())
        }
        fn every(&self) -> Duration {
            Duration::from_millis(10)
        }
    }

    /// A scratch project for a video test, and a context in it for `owner`.
    fn project(name: &str, owner: u64) -> (PathBuf, ToolContext) {
        let dir = std::env::temp_dir().join(format!("letronna-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let ctx = ToolContext {
            workspace: Some(dir.clone()),
            conversation: owner,
            ..Default::default()
        };
        (dir, ctx)
    }

    /// The job number a started video's reply names.
    fn job_in(reply: &str) -> u64 {
        let (_, rest) = reply.split_once("job #").unwrap();
        rest.split(':').next().unwrap().parse().unwrap()
    }

    /// Poll until `ready`, for at most ten seconds.
    fn until(mut ready: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if ready() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        false
    }

    const ASK: &str = r#"{"prompt": "a small red lighthouse", "model": "veo-3.1"}"#;

    #[test]
    fn a_video_returns_its_job_at_once_and_its_path_when_saved() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (dir, ctx) = project("video-done", 7_401);
        let fake = Fake::new(Render::Done(Some(8)));
        let args: Value = serde_json::from_str(ASK).unwrap();
        let reply = start_video(fake.clone(), &args, &ctx).unwrap();
        assert!(reply.starts_with("Started as job #"), "{reply}");
        let job = job_in(&reply);
        // The render is still going, and the job says how far it has got.
        assert!(until(|| crate::jobs::log(job)
            .unwrap()
            .contains("40% rendered.")));
        assert_eq!(crate::jobs::state(job).unwrap(), "running");
        assert_eq!(
            crate::jobs::log(job).unwrap().matches("40%").count(),
            1,
            "the same progress is said once"
        );

        fake.done.store(true, Ordering::Relaxed);
        let wait = crate::tools::find("wait").unwrap();
        let waited = wait.run(&json!({"job": job, "seconds": 10}), &ctx).unwrap();
        assert!(
            waited.starts_with(&format!(
                "Job #{job} finished. Output since the wait began:"
            )),
            "{waited}"
        );
        assert_eq!(crate::jobs::state(job).unwrap(), "finished");

        let notices = crate::inbox::take(7_401, true);
        assert_eq!(notices.len(), 1, "{notices:?}");
        let (first, rest) = notices[0].text.split_once('\n').unwrap();
        assert_eq!(
            first,
            format!("job #{job} finished (`make_video: a small red lighthouse`):")
        );
        let path = saved(rest).unwrap();
        assert_eq!(path.parent().unwrap(), dir.join("media"));
        assert_eq!(std::fs::read(&path).unwrap(), b"mp4");
        assert!(rest.contains("A 8-second video"), "{rest}");
        assert!(
            crate::jobs::log(job)
                .unwrap()
                .contains(&path.display().to_string())
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn stopping_a_video_job_cancels_it_and_tells_nobody() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (dir, ctx) = project("video-stopped", 7_402);
        let fake = Fake::new(Render::Done(None));
        let args: Value = serde_json::from_str(ASK).unwrap();
        let job = job_in(&start_video(fake.clone(), &args, &ctx).unwrap());
        assert!(until(|| crate::jobs::log(job)
            .unwrap()
            .contains("40% rendered.")));

        crate::jobs::stop(job).unwrap();
        assert!(until(|| !crate::jobs::running(job).unwrap()));
        assert_eq!(crate::jobs::state(job).unwrap(), "stopped");
        let log = crate::jobs::log(job).unwrap();
        assert!(log.contains("make_video with job \"vid_1\""), "{log}");
        // Finishing on the service afterwards saves nothing here.
        fake.done.store(true, Ordering::Relaxed);
        std::thread::sleep(Duration::from_millis(100));
        assert!(crate::inbox::take(7_402, true).is_empty());
        assert!(!dir.join("media").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_video_that_fails_is_reported_and_one_refused_starts_no_job() {
        let _turn = crate::inbox::tests::DOORBELL
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let (dir, ctx) = project("video-failed", 7_403);
        let fake = Fake::new(Render::Failed);
        fake.done.store(true, Ordering::Relaxed);
        let args: Value = serde_json::from_str(ASK).unwrap();
        let job = job_in(&start_video(fake, &args, &ctx).unwrap());
        assert!(until(|| !crate::jobs::running(job).unwrap()));
        assert_eq!(crate::jobs::state(job).unwrap(), "failed");
        let notices = crate::inbox::take(7_403, true);
        assert_eq!(notices.len(), 1, "{notices:?}");
        assert_eq!(
            notices[0].text,
            format!(
                "job #{job} failed (`make_video: a small red lighthouse`):\n\
                 The video could not be made. It is not charged."
            )
        );

        // A refusal comes back from the call itself, before any job.
        let refused = Fake {
            refuse: true,
            ..Fake::new(Render::Failed)
        };
        let args = json!({"prompt": "a refused lighthouse", "model": "veo-3.1"});
        let error = start_video(refused, &args, &ctx).unwrap_err();
        assert_eq!(error, "Not enough credit.");
        let jobs = crate::jobs::list();
        assert!(!jobs.iter().any(|job| job.command.contains("refused")));
        assert!(!dir.join("media").exists());
    }

    /// A data folder of its own, set as `AGENT_DATA_DIR` while the guard is
    /// held, with a project folder beside it.
    fn scratch(name: &str) -> (std::sync::MutexGuard<'static, ()>, PathBuf) {
        let turn = crate::store::tests::DATA_DIR
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let dir = std::env::temp_dir().join(format!("letronna-edit-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("project")).unwrap();
        std::fs::create_dir_all(dir.join("data").join("media")).unwrap();
        unsafe { std::env::set_var("AGENT_DATA_DIR", dir.join("data")) };
        (turn, dir)
    }

    /// A call made in `project` on the gateway at `base`, with a made-up key.
    fn on_gateway(base: &str, project: &Path) -> ToolContext {
        ToolContext::of(&Connection {
            endpoint: format!("{base}/v1"),
            model: "chat-model".into(),
            key: "test-key".into(),
            workspace: Some(project.to_path_buf()),
            ..Default::default()
        })
    }

    /// A gateway on this computer that answers one request with `status`
    /// and `reply`, and hands back that request's head and body.
    fn gateway(status: &str, reply: &str) -> (String, std::thread::JoinHandle<(String, Vec<u8>)>) {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", server.local_addr().unwrap());
        let answer = format!(
            "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
            reply.len()
        );
        let worker = std::thread::spawn(move || {
            let (mut socket, _) = server.accept().unwrap();
            socket
                .set_read_timeout(Some(Duration::from_secs(5)))
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
                let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let len: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .map_or(0, |n| n.parse().unwrap());
                if bytes.len() >= end + 4 + len {
                    break (head, bytes[end + 4..end + 4 + len].to_vec());
                }
            };
            socket.write_all(answer.as_bytes()).unwrap();
            (head, body)
        });
        (base, worker)
    }

    /// A blank picture `width` by `height` in `format`.
    fn picture(width: u32, height: u32, format: image::ImageFormat) -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(width, height)
            .write_to(&mut std::io::Cursor::new(&mut bytes), format)
            .unwrap();
        bytes
    }

    #[test]
    fn an_edit_sends_every_picture_and_the_mask_in_one_form_and_saves_the_result() {
        let (_turn, dir) = scratch("form");
        let project = dir.join("project");
        std::fs::write(
            project.join("cat.png"),
            picture(4, 4, image::ImageFormat::Png),
        )
        .unwrap();
        std::fs::write(
            project.join("hole.png"),
            picture(4, 4, image::ImageFormat::Png),
        )
        .unwrap();
        // One already in the conversation, as a GIF, which the route does not take.
        let earlier = dir.join("data").join("media").join("hat.gif");
        std::fs::write(&earlier, picture(4, 4, image::ImageFormat::Gif)).unwrap();
        let made = picture(2, 2, image::ImageFormat::Png);
        let reply = json!({"data": [{"b64_json": to_base64(&made)}]}).to_string();
        let (base, asked) = gateway("200 OK", &reply);
        let args = json!({
            "images": ["cat.png", earlier.to_string_lossy()],
            "mask": "hole.png",
            "prompt": "put the hat on the cat",
            "model": "gpt-image-1",
            "size": "1024x1024"
        });
        let out = EditImage.run(&args, &on_gateway(&base, &project)).unwrap();
        let (head, body) = asked.join().unwrap();
        assert!(head.starts_with("post /v1/images/edits "), "{head}");
        assert!(head.contains("authorization: bearer test-key"), "{head}");
        let boundary = head
            .lines()
            .find_map(|l| l.strip_prefix("content-type: multipart/form-data; boundary="))
            .unwrap()
            .to_owned();
        let text = String::from_utf8_lossy(&body);
        for field in [
            "name=\"prompt\"\r\n\r\nput the hat on the cat\r\n",
            "name=\"n\"\r\n\r\n1\r\n",
            "name=\"model\"\r\n\r\ngpt-image-1\r\n",
            "name=\"size\"\r\n\r\n1024x1024\r\n",
            "name=\"image[]\"; filename=\"cat.png\"\r\nContent-Type: image/png\r\n\r\n\x7fPNG",
            "name=\"image[]\"; filename=\"hat.png\"\r\nContent-Type: image/png\r\n\r\n",
            "name=\"mask\"; filename=\"hole.png\"\r\nContent-Type: image/png\r\n\r\n",
        ] {
            let field = field.replace("\x7f", "\u{FFFD}");
            assert!(text.contains(&field), "{field:?} missing from {text:?}");
        }
        assert!(
            !text.contains("GIF89a") && !text.contains("response_format"),
            "{text:?}"
        );
        assert!(text.ends_with(&format!("--{boundary}--\r\n")), "{text:?}");
        // Saved as a made image is, and named as the window finds it.
        let path = saved_by("edit_image", &out).unwrap();
        assert_eq!(path.parent().unwrap(), project.join("media"));
        assert_eq!(std::fs::read(&path).unwrap(), made);
        assert!(out.contains("edited from `cat.png`"), "{out}");
        // Off Letronna it is offered signed out, and on it only signed in.
        let custom = Connection {
            endpoint: format!("{base}/v1"),
            ..Default::default()
        };
        let letronna = Connection {
            endpoint: crate::auth::hosted_base(),
            ..Default::default()
        };
        assert!(EditImage.offered(&custom) && !EditImage.offered(&letronna));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_picture_outside_the_project_or_holding_keys_is_never_sent() {
        let (_turn, dir) = scratch("refused");
        let project = dir.join("project");
        std::fs::write(dir.join("away.png"), picture(4, 4, image::ImageFormat::Png)).unwrap();
        std::fs::write(project.join(".env"), "KEY=secret").unwrap();
        std::fs::write(
            project.join("cat.png"),
            picture(4, 4, image::ImageFormat::Png),
        )
        .unwrap();
        // Nothing listens here: a refusal comes before any upload.
        let ctx = on_gateway("http://127.0.0.1:9", &project);
        let away = dir.join("away.png").to_string_lossy().into_owned();
        for (images, mask, expected) in [
            (json!(["../away.png"]), None, "is outside the"),
            (json!([away]), None, "is outside the"),
            (json!([".env"]), None, "keys, passwords or tokens"),
            (
                json!(["cat.png"]),
                Some(".env"),
                "keys, passwords or tokens",
            ),
            (json!([]), None, "`images` is required"),
        ] {
            let mut args = json!({"images": images, "prompt": "a hat"});
            if let Some(mask) = mask {
                args["mask"] = mask.into();
            }
            let refused = EditImage.run(&args, &ctx).unwrap_err();
            assert!(refused.contains(expected), "{refused}");
        }
        // A call with no gateway at all says so rather than guessing one.
        let bare = ToolContext {
            workspace: Some(project.clone()),
            ..Default::default()
        };
        let args = json!({"images": ["cat.png"], "prompt": "a hat"});
        assert!(EditImage.run(&args, &bare).unwrap_err().contains("gateway"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn pictures_the_route_would_refuse_for_their_size_are_stopped_before_upload() {
        let (_turn, dir) = scratch("size");
        let project = dir.join("project");
        let ctx = on_gateway("http://127.0.0.1:9", &project);
        // Sparse, so the size is there without the disk being written.
        let sized = |name: &str, bytes: u64| {
            let file = std::fs::File::create(project.join(name)).unwrap();
            file.set_len(bytes).unwrap();
        };
        sized("huge.png", EDIT_BYTES + 1);
        sized("big.png", MASK_BYTES + 1);
        std::fs::write(
            project.join("cat.png"),
            picture(4, 4, image::ImageFormat::Png),
        )
        .unwrap();
        std::fs::write(
            project.join("wide.png"),
            picture(8, 4, image::ImageFormat::Png),
        )
        .unwrap();
        std::fs::write(project.join("note.png"), "not a picture").unwrap();
        let many: Vec<&str> = vec!["cat.png"; 17];
        for (args, expected) in [
            (json!({"images": ["huge.png"]}), "up to 50.0 MB"),
            (
                json!({"images": ["cat.png"], "mask": "big.png"}),
                "up to 4.0 MB",
            ),
            (
                json!({"images": ["big.png"], "model": "dall-e-2"}),
                "up to 4.0 MB",
            ),
            (
                json!({"images": ["cat.png", "cat.png"], "model": "dall-e-2"}),
                "at most 1",
            ),
            (json!({"images": many}), "at most 16"),
            (
                json!({"images": ["cat.png"], "mask": "wide.png"}),
                "is 8x4, and `cat.png` is 4x4",
            ),
            (
                json!({"images": ["note.png"]}),
                "not a picture that can be edited",
            ),
        ] {
            let mut args = args;
            args["prompt"] = "a hat".into();
            let refused = EditImage.run(&args, &ctx).unwrap_err();
            assert!(refused.contains(expected), "{expected}: {refused}");
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_gateway_without_the_route_or_a_model_that_cannot_edit_says_so() {
        let place = "the gateway at http://localhost/v1";
        let no_route = [
            (404, json!(null)),
            (404, json!({"detail": "Not Found"})),
            (405, json!({"error": "Method Not Allowed"})),
        ];
        for (status, body) in no_route {
            let why = edit_refusal(status, &body, place, Some("gpt-image-1"));
            assert!(
                why.contains("no image edits route on the gateway at"),
                "{why}"
            );
            assert!(why.contains(&format!("answered {status}")), "{why}");
        }
        let cannot = [
            (
                404,
                json!({"error": {"code": "model_not_found", "message": "No such thing."}}),
            ),
            (
                400,
                json!({"error": {"message": "Model flux.2-pro does not support edits."}}),
            ),
            (
                400,
                json!({"error": {"param": "model", "message": "Invalid value."}}),
            ),
        ];
        for (status, body) in cannot {
            let why = edit_refusal(status, &body, place, Some("flux.2-pro"));
            assert!(
                why.starts_with("`flux.2-pro` cannot edit images on"),
                "{why}"
            );
        }
        let unnamed = json!({"error": {"message": "This model cannot edit."}});
        let why = edit_refusal(400, &unnamed, place, None);
        assert!(why.starts_with("The model it picks cannot edit"), "{why}");
        // Anything else is the gateway's own reason, as other media calls give it.
        let credit = json!({"error": {"message": "Not enough credit."}});
        assert_eq!(
            edit_refusal(402, &credit, place, None),
            "Not enough credit."
        );
        assert!(edit_refusal(500, &json!(null), place, None).contains("answered 500"));

        // And end to end, from a server that has no such route.
        let (_turn, dir) = scratch("route");
        let project = dir.join("project");
        std::fs::write(
            project.join("cat.png"),
            picture(4, 4, image::ImageFormat::Png),
        )
        .unwrap();
        let (base, asked) = gateway("404 Not Found", "404 page not found");
        let args = json!({"images": ["cat.png"], "prompt": "a hat"});
        let why = EditImage
            .run(&args, &on_gateway(&base, &project))
            .unwrap_err();
        asked.join().unwrap();
        assert!(why.contains("no image edits route"), "{why}");
        assert!(!project.join("media").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_gateway_that_redirects_the_edit_is_not_followed() {
        let (_turn, dir) = scratch("redirect");
        let project = dir.join("project");
        std::fs::write(
            project.join("cat.png"),
            picture(4, 4, image::ImageFormat::Png),
        )
        .unwrap();
        let made = picture(2, 2, image::ImageFormat::Png);
        let reply = json!({"data": [{"b64_json": to_base64(&made)}]}).to_string();
        let (elsewhere, reached) = gateway("200 OK", &reply);
        let moved = format!("307 Temporary Redirect\r\nLocation: {elsewhere}/v1/images/edits");
        let (base, asked) = gateway(&moved, "");
        let args = json!({"images": ["cat.png"], "prompt": "a hat"});
        let why = EditImage
            .run(&args, &on_gateway(&base, &project))
            .unwrap_err();
        asked.join().unwrap();
        assert!(why.contains("answered 307"), "{why}");
        assert!(!reached.is_finished() && !project.join("media").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_edit_answered_with_a_link_says_it_was_made_and_where() {
        let (_turn, dir) = scratch("link");
        let project = dir.join("project");
        std::fs::write(
            project.join("cat.png"),
            picture(4, 4, image::ImageFormat::Png),
        )
        .unwrap();
        // What OpenAI answers with no model named: dall-e-2, by link.
        let link = "https://files.example/edit/1.png";
        let reply = json!({"data": [{"url": link}]}).to_string();
        let (base, asked) = gateway("200 OK", &reply);
        let args = json!({"images": ["cat.png"], "prompt": "a hat"});
        let why = EditImage
            .run(&args, &on_gateway(&base, &project))
            .unwrap_err();
        asked.join().unwrap();
        assert!(why.contains("was made") && why.contains(link), "{why}");
        assert!(why.contains("gpt-image-1"), "{why}");
        assert!(!project.join("media").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn letronna_named_with_its_chat_route_is_still_paid_from_the_credit() {
        let (_turn, dir) = scratch("hosted");
        let letronna = Connection {
            endpoint: format!("{}/chat/completions", crate::auth::hosted_base()),
            ..Default::default()
        };
        let args = json!({"images": ["cat.png"], "prompt": "a hat"});
        let card = EditImage.preview(&args, &ToolContext::of(&letronna));
        assert!(card.ends_with("paid from your Letronna credit."), "{card}");
        assert!(!EditImage.offered(&letronna));
        std::fs::remove_dir_all(dir).unwrap();
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
