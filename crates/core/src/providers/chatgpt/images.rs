//! Pictures on a ChatGPT plan: the two routes its backend keeps for Codex's
//! own image tool, beside the Responses API a chat goes to.
//!
//! OpenAI's Images API wants a key, which a plan's sign-in is not, so
//! without these a chat signed in with ChatGPT could make no picture at
//! all. They take JSON and answer whole, as that API does, except that the
//! pictures to edit go in the body as `data:` addresses rather than as a
//! form, and there is no mask. The plan covers them, counted toward an
//! image limit of its own.
//!
//! The backend decides the size and the quality itself: asked for
//! 1024x1536 at low quality it sent 1312x1199 at medium. So a request asks
//! for neither, as Codex's does not, and what comes back is measured
//! rather than taken to be what was asked for.

use super::super::{Provider, accounts, auth, wire::openai_responses};
use super::{NAME, RENEW_IN_CODEX};
use crate::chat::Connection;
use serde_json::{Value, json};
use std::time::Duration;

/// The model Codex names on every request.
const MODEL: &str = "gpt-image-2";

/// The most pictures one edit takes, as Codex holds its own tool to.
pub(crate) const MOST: usize = 5;

/// The largest picture an edit sends: each grows by a third as base64, and
/// `MOST` of them ride in one body.
pub(crate) const PICTURE_BYTES: u64 = 20 * 1024 * 1024;

/// A picture takes about half a minute, and the reply is held open until
/// it is made.
const MAKE_TIME: Duration = Duration::from_secs(300);

#[cfg(test)]
thread_local! {
    /// A test's own stand-in for the backend, so no test reaches ChatGPT's.
    static STAND_IN: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

/// A plan that makes pictures: whose it is, and where it is asked.
#[derive(Clone, PartialEq)]
pub(crate) struct Plan {
    /// The ChatGPT account signed in to it; empty for a chat that names
    /// none, whose key goes as it is.
    account: String,
    /// The backend's address, without a trailing slash.
    base: String,
    /// The chat's own key, which goes when the account's sign-in is not
    /// kept here.
    key: String,
}

// Never prints the key, which for a plan is its sign-in.
impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("account", &self.account)
            .field("base", &self.base)
            .finish_non_exhaustive()
    }
}

/// Whether the chat on `connection` runs on a ChatGPT plan.
pub(crate) fn chats_on(connection: &Connection) -> bool {
    openai_responses::chatgpt(&connection.endpoint)
}

/// The backend's address: always the provider's own, never one a connection
/// names, so the sign-in goes nowhere else.
fn base() -> String {
    #[cfg(test)]
    if let Some(stand_in) = STAND_IN.with(|stand_in| stand_in.borrow().clone()) {
        return stand_in;
    }
    super::ChatGpt.info().base.to_owned()
}

/// The plan that makes pictures for a call over `connection`: the one the
/// chat is on, signed as the chat's own requests are, or else the first
/// ChatGPT account signed in to while the plugin that offers ChatGPT is on.
/// A call that carries no connection, as one checked again before it runs,
/// is answered by what is signed in.
pub(crate) fn plan(connection: Option<&Connection>) -> Option<Plan> {
    if let Some(connection) = connection.filter(|c| chats_on(c)) {
        return Some(Plan {
            account: connection.account(),
            base: base(),
            key: connection.key.trim().to_owned(),
        });
    }
    if !super::super::offered(NAME) {
        return None;
    }
    let account = (accounts::of(NAME).into_iter()).find(|name| auth::signed_in(name))?;
    // Another gateway's key is nothing to send here.
    Some(Plan {
        account: account.to_owned(),
        base: base(),
        key: String::new(),
    })
}

/// Make the picture `prompt` describes on `plan`, and hand back its bytes.
pub(crate) fn make(plan: &Plan, prompt: &str) -> Result<Vec<u8>, String> {
    picture(&post(plan, "generations", &request(prompt, Vec::new()))?)
}

/// Make of `pictures`, each a `data:` address, what `prompt` says on `plan`.
pub(crate) fn edit(plan: &Plan, prompt: &str, pictures: Vec<String>) -> Result<Vec<u8>, String> {
    picture(&post(plan, "edits", &request(prompt, pictures))?)
}

/// What either route is sent, which is what Codex sends: the size and the
/// quality left to the backend, and the pictures to edit, if any. They are
/// taken, not copied: five of them can be over a hundred megabytes.
fn request(prompt: &str, pictures: Vec<String>) -> Value {
    let mut body = json!({
        "prompt": prompt,
        "model": MODEL,
        "quality": "auto",
        "size": "auto",
    });
    if !pictures.is_empty() {
        body["images"] = (pictures.into_iter())
            .map(|picture| json!({"image_url": picture}))
            .collect();
    }
    body
}

/// Send `body` to `route` of the plan's image routes, signed as the plan's
/// account, and hand back the reply or why it was refused.
fn post(plan: &Plan, route: &str, body: &Value) -> Result<Value, String> {
    // As `chat::carrying` signs a chat's: by the account's sign-in, or with
    // the key as it is for a chat that names no account.
    let account = Some(plan.account.as_str()).filter(|account| !account.is_empty());
    let send = || -> Result<reqwest::blocking::Response, String> {
        let request = crate::chat::client()?
            .post(format!("{}/images/{route}", plan.base))
            // For the whole reply, which comes only once the picture is made.
            .timeout(MAKE_TIME)
            .json(body);
        openai_responses::as_chatgpt(request, account, &plan.key)
            .send()
            // A connection that never opens times out too, and is no
            // picture taking long.
            .map_err(|e| match e.is_timeout() && !e.is_connect() {
                true => "ChatGPT took too long to make the picture.".to_owned(),
                false => "Could not reach ChatGPT.".to_owned(),
            })
    };
    let mut reply = send()?;
    // A sign-in refused is refreshed once, and the request sent again with
    // the new token `as_chatgpt` then finds, as a chat's is.
    if reply.status() == reqwest::StatusCode::UNAUTHORIZED && renewed(&plan.account) {
        reply = send()?;
    }
    let status = reply.status().as_u16();
    if (200..300).contains(&status) {
        return reply
            .json()
            .map_err(|_| "The picture came back in a form Letronna could not read.".to_owned());
    }
    // Each reply of these routes says which limit it counts toward, the
    // plan's image limit, and when that one starts again.
    let wait = reply
        .headers()
        .get("x-codex-primary-reset-after-seconds")
        .and_then(|after| after.to_str().ok()?.trim().parse::<u64>().ok());
    let said: Value = reply.json().unwrap_or_default();
    // Only the first account's sign-in can be the Codex app's.
    let shared = plan.account == NAME && super::uses_codex();
    Err(refusal(status, wait, &said, shared))
}

/// Whether `account`'s sign-in, just refused, was refreshed, so the request
/// can go once more: never the Codex app's, which only Codex renews.
fn renewed(account: &str) -> bool {
    auth::tokens(account).is_some_and(|held| auth::refused(account, &held.access).is_ok())
}

/// Why the backend turned a picture down, in words that say what to do
/// about it: `wait` is how many seconds its headers gave until the plan's
/// image limit starts again, and `shared` whether the sign-in is the Codex
/// app's.
fn refusal(status: u16, wait: Option<u64>, body: &Value, shared: bool) -> String {
    let said = said(body);
    match status {
        401 if shared => RENEW_IN_CODEX.to_owned(),
        401 => "ChatGPT refused the sign-in. Sign in again.".to_owned(),
        429 => {
            let again = match wait.filter(|wait| *wait > 0).map(|wait| wait.div_ceil(60)) {
                Some(minutes @ 0..60) => format!(", and makes more in {minutes} min"),
                Some(minutes) => format!(
                    ", and makes more in {} h {} min",
                    minutes / 60,
                    minutes % 60
                ),
                None => String::new(),
            };
            let said = said.map(|said| format!(" ({said})")).unwrap_or_default();
            // Said to the model as well as the user: a limit does not lift
            // on a retry.
            format!(
                "The ChatGPT plan has made all the pictures it allows for now{again}{said}. \
                 Tell the user rather than trying again."
            )
        }
        _ => match said {
            Some(said) => format!("ChatGPT turned the picture down: {said}"),
            None => format!("ChatGPT turned the picture down: its backend answered {status}."),
        },
    }
}

/// The error a reply carries, in the shapes the backend sends one in.
fn said(body: &Value) -> Option<&str> {
    [&body["error"]["message"], &body["error"], &body["detail"]]
        .into_iter()
        .find_map(Value::as_str)
        .map(str::trim)
        .filter(|said| !said.is_empty())
}

/// The picture in `reply`, which can carry a refusal inside a success: it
/// is held open while the picture is made.
fn picture(reply: &Value) -> Result<Vec<u8>, String> {
    if let Some(said) = said(reply) {
        return Err(format!("ChatGPT turned the picture down: {said}"));
    }
    let encoded = reply["data"][0]["b64_json"]
        .as_str()
        .ok_or("No picture came back from ChatGPT.")?;
    crate::media::base64(encoded).ok_or_else(|| "The picture came back damaged.".to_owned())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Have this test's requests for a plan's pictures go to `base`, a
    /// stand-in on this computer.
    pub(crate) fn stand_in(base: &str) {
        STAND_IN.with(|stand_in| *stand_in.borrow_mut() = Some(base.to_owned()));
    }

    /// A chat on a ChatGPT plan, as the gateway of that name makes one.
    pub(crate) fn chat() -> Connection {
        Connection {
            endpoint: super::super::ChatGpt.info().base.to_owned(),
            gateway: NAME.into(),
            model: "gpt-chat".into(),
            key: "chat-key".into(),
            ..Default::default()
        }
    }

    #[test]
    fn a_request_asks_what_codex_asks_and_carries_the_pictures_to_edit() {
        assert_eq!(
            request("a red kite", Vec::new()),
            json!({"prompt": "a red kite", "model": "gpt-image-2", "quality": "auto", "size": "auto"})
        );
        let edit = request(
            "make it blue",
            vec!["data:image/png;base64,AAAA".to_owned()],
        );
        assert_eq!(
            edit["images"],
            json!([{"image_url": "data:image/png;base64,AAAA"}])
        );
        assert_eq!(edit["prompt"], "make it blue");
    }

    #[test]
    fn the_chats_own_plan_makes_its_pictures_and_no_other_address_is_asked() {
        let (_vault, _held) = auth::tests::vault("chatgpt-images-plan");
        // Nothing is signed in and the plugin is off: a chat that is on the
        // plan all the same is taken at its word, and any other has none.
        let on_plan = plan(Some(&chat())).unwrap();
        assert_eq!(on_plan.account, NAME);
        assert_eq!(on_plan.base, "https://chatgpt.com/backend-api/codex");
        assert_eq!(on_plan.key, "chat-key");
        let elsewhere = Connection {
            endpoint: "https://api.example.com/v1".into(),
            key: "another-key".into(),
            ..Default::default()
        };
        assert_eq!(plan(Some(&elsewhere)), None);
        assert_eq!(plan(None), None);
        // A further account's chat is asked as that account, and one that
        // names none with its key alone, as its own requests are.
        let second = Connection {
            gateway: "ChatGPT · work".into(),
            ..chat()
        };
        assert_eq!(plan(Some(&second)).unwrap().account, "ChatGPT · work");
        let unnamed = Connection {
            endpoint: "https://chatgpt.com/backend-api/other".into(),
            gateway: String::new(),
            ..chat()
        };
        let unnamed = plan(Some(&unnamed)).unwrap();
        assert_eq!(
            (unnamed.account.as_str(), unnamed.key.as_str()),
            ("", "chat-key")
        );
        assert_eq!(unnamed.base, "https://chatgpt.com/backend-api/codex");
        // The key never shows where a plan is printed.
        assert!(!format!("{on_plan:?}").contains("chat-key"));
    }

    #[test]
    fn a_plan_signed_in_to_makes_pictures_for_a_chat_that_is_elsewhere() {
        let (_vault, _held) = auth::tests::vault("chatgpt-images-signed-in");
        super::super::super::tests::plugged_in(super::super::PLUGIN);
        assert_eq!(plan(None), None);
        auth::tests::keep(NAME, "access-1");
        let signed_in = plan(None).unwrap();
        assert_eq!(
            (signed_in.account.as_str(), signed_in.key.as_str()),
            (NAME, "")
        );
        // The other gateway's key is not taken along.
        let elsewhere = Connection {
            endpoint: "https://api.example.com/v1".into(),
            key: "another-key".into(),
            ..Default::default()
        };
        assert_eq!(plan(Some(&elsewhere)), Some(signed_in));
        // Not while the plugin that offers ChatGPT is off.
        super::super::super::save_switch(super::super::PLUGIN, false);
        assert_eq!(plan(None), None);
    }

    #[test]
    fn a_refusal_says_what_to_do_about_it() {
        let limit = json!({"error": {"message": "Image limit reached."}});
        let full = refusal(429, Some(7_260), &limit, false);
        assert!(
            full.starts_with(
                "The ChatGPT plan has made all the pictures it allows for now, and makes \
                 more in 2 h 1 min (Image limit reached.)."
            ),
            "{full}"
        );
        assert!(full.ends_with("rather than trying again."), "{full}");
        let soon = refusal(429, Some(61), &json!({}), false);
        assert!(
            soon.contains("for now, and makes more in 2 min. Tell"),
            "{soon}"
        );
        let unsaid = refusal(429, None, &json!({}), false);
        assert!(unsaid.contains("for now. Tell"), "{unsaid}");
        assert_eq!(
            refusal(401, None, &json!({}), false),
            "ChatGPT refused the sign-in. Sign in again."
        );
        assert_eq!(refusal(401, None, &json!({}), true), RENEW_IN_CODEX);
        let policy =
            json!({"error": {"message": "Your request was rejected by the safety system."}});
        assert_eq!(
            refusal(400, None, &policy, false),
            "ChatGPT turned the picture down: Your request was rejected by the safety system."
        );
        assert_eq!(
            refusal(503, None, &json!({"detail": " "}), false),
            "ChatGPT turned the picture down: its backend answered 503."
        );
    }

    #[test]
    fn a_reply_gives_its_picture_or_says_why_there_is_none() {
        let made = json!({"created": 1, "data": [{"b64_json": "aGVsbG8="}], "size": "1536x1024"});
        assert_eq!(picture(&made).unwrap(), b"hello");
        let inside = json!({"error": {"message": "Blocked."}});
        assert_eq!(
            picture(&inside).unwrap_err(),
            "ChatGPT turned the picture down: Blocked."
        );
        assert_eq!(
            picture(&json!({"data": []})).unwrap_err(),
            "No picture came back from ChatGPT."
        );
        assert_eq!(
            picture(&json!({"data": [{"b64_json": "!"}]})).unwrap_err(),
            "The picture came back damaged."
        );
    }
}
