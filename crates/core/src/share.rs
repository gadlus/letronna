//! Sessions shared by link: a session's title and what was asked and each
//! reply (never tool calls or their output) go to the account API, which
//! answers an id; letronna.ai/s/<id> shows it, and anyone with the link can
//! bring it into Letronna. Which of this computer's sessions are shared, and
//! as what, is kept in `shared.json`, so sharing again reuses the link and
//! stopping deletes it. Everything about sharing in core is in this file.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::auth;
use crate::chat::{self as backend, Message};

/// A message as it is shared.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Shared {
    pub role: String,
    pub content: String,
    #[serde(default)]
    pub model: String,
}

/// A shared session as the API gives it back.
#[derive(Debug, Deserialize)]
pub struct Session {
    pub title: String,
    pub messages: Vec<Shared>,
}

/// What of `messages` is shared: what was asked and each reply with text,
/// not notices, tool calls or their output, with any key or token in them
/// hidden, as a pasted one would otherwise go out with the link.
pub fn kept(messages: &[Message]) -> Vec<Shared> {
    let secrets = crate::redact::secrets();
    let hide = |text: &str| {
        crate::redact::hide(
            text,
            &secrets,
            crate::redact::Shapes::Vendor,
            crate::redact::MARKER,
        )
    };
    (messages.iter())
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
        .filter(|m| !m.notice && !m.content.trim().is_empty())
        .map(|m| Shared {
            role: m.role.clone(),
            content: hide(&m.content),
            model: m.model.clone(),
        })
        .collect()
}

/// The page a shared session `id` is at.
pub fn link(id: &str) -> String {
    let site = std::env::var("LETRONNA_WEB").unwrap_or_else(|_| auth::WEBSITE.to_owned());
    format!("{}/s/{id}", site.trim_end_matches('/'))
}

/// The id in `text`: a shared session's link, or the id alone.
pub fn id_of(text: &str) -> Option<String> {
    let text = text.trim().trim_end_matches('/');
    let id = match text.rsplit_once("/s/") {
        Some((_, id)) => id.split(['?', '#', '/']).next().unwrap_or_default(),
        None if !text.contains('/') => text,
        None => return None,
    };
    (id.len() == 10 && id.chars().all(|c| c.is_ascii_alphanumeric())).then(|| id.to_owned())
}

fn record() -> PathBuf {
    backend::data_dir().join("shared.json")
}

/// This computer's shared sessions: session id to share id.
fn shared_here() -> BTreeMap<u64, String> {
    std::fs::read(record())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

fn keep(all: &BTreeMap<u64, String>) -> Result<(), String> {
    let _ = std::fs::create_dir_all(backend::data_dir());
    let text = serde_json::to_vec_pretty(all).map_err(|e| e.to_string())?;
    std::fs::write(record(), text).map_err(|_| "Could not note the shared session.".to_owned())
}

/// The link session `session` is shared at, if it is.
pub fn shared_as(session: u64) -> Option<String> {
    shared_here().get(&session).map(|id| link(id))
}

/// Why the API turned a request down, from its `error`, or `fallback`.
fn refused(response: reqwest::blocking::Response, fallback: &str) -> String {
    #[derive(Deserialize)]
    struct Refused {
        error: String,
    }
    response
        .json::<Refused>()
        .map(|r| capitalise(&r.error))
        .unwrap_or_else(|_| fallback.to_owned())
}

fn capitalise(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => {
            let mut out: String = first.to_uppercase().collect();
            out.push_str(chars.as_str());
            if !out.ends_with('.') {
                out.push('.');
            }
            out
        }
        None => String::new(),
    }
}

/// Share session `session`, called `title`, as a link anyone can open: the
/// link, the same one as before when the session is shared again (its
/// messages as they are now replace what was shared).
pub fn share(session: u64, title: &str, messages: &[Message]) -> Result<String, String> {
    let account = auth::account().ok_or("Sign in to your Letronna account to share by link.")?;
    let messages = kept(messages);
    if messages.is_empty() {
        return Err("There is nothing in this session to share yet.".into());
    }
    let mut all = shared_here();
    // Shared again: the old link goes, so what is shared is the session now.
    if let Some(old) = all.remove(&session) {
        let _ = delete(&account.token, &old);
    }
    let response = auth::client(Duration::from_secs(30))?
        .post(auth::api("/shares"))
        .bearer_auth(&account.token)
        .json(&serde_json::json!({ "title": title, "messages": messages }))
        .send()
        .map_err(|_| "Could not reach Letronna to share it.".to_owned())?;
    if !response.status().is_success() {
        return Err(refused(response, "Letronna would not share it."));
    }
    #[derive(Deserialize)]
    struct Made {
        id: String,
    }
    let made: Made = response
        .json()
        .map_err(|_| "Letronna sent something unexpected.".to_owned())?;
    all.insert(session, made.id.clone());
    keep(&all)?;
    Ok(link(&made.id))
}

fn delete(token: &str, id: &str) -> Result<(), String> {
    let response = auth::client(Duration::from_secs(20))?
        .delete(auth::api(&format!("/shares/{id}")))
        .bearer_auth(token)
        .send()
        .map_err(|_| "Could not reach Letronna to stop sharing.".to_owned())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(refused(response, "Letronna would not stop sharing it."))
    }
}

/// Stop sharing session `session`: its link stops working for everyone.
pub fn unshare(session: u64) -> Result<(), String> {
    let account = auth::account().ok_or("Sign in to your Letronna account to stop sharing.")?;
    let mut all = shared_here();
    let Some(id) = all.get(&session).cloned() else {
        return Ok(());
    };
    // Gone already, from another computer: forgotten here too.
    if let Err(why) = delete(&account.token, &id)
        && !why.contains("No such shared session")
    {
        return Err(why);
    }
    all.remove(&session);
    keep(&all)
}

/// The shared session at `link` (or its id), for bringing it in.
pub fn open(link: &str) -> Result<Session, String> {
    let id = id_of(link).ok_or("That is not a shared session's link.")?;
    let response = auth::client(Duration::from_secs(30))?
        .get(auth::api(&format!("/shares/{id}")))
        .send()
        .map_err(|_| "Could not reach Letronna to open it.".to_owned())?;
    if !response.status().is_success() {
        return Err(refused(
            response,
            "That shared session could not be opened.",
        ));
    }
    response
        .json()
        .map_err(|_| "Letronna sent something unexpected.".to_owned())
}

/// `session` as messages of a session here.
pub fn messages_of(session: &Session) -> Vec<Message> {
    (session.messages.iter())
        .filter(|m| matches!(m.role.as_str(), "user" | "assistant"))
        .map(|m| Message {
            role: m.role.clone(),
            content: m.content.clone(),
            model: m.model.clone(),
            ..Default::default()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_link_or_an_id_names_a_shared_session() {
        assert_eq!(
            id_of("https://letronna.ai/s/LeBJ8QsjVe").as_deref(),
            Some("LeBJ8QsjVe")
        );
        assert_eq!(
            id_of(" https://letronna.ai/s/LeBJ8QsjVe/?x=1 ").as_deref(),
            Some("LeBJ8QsjVe")
        );
        assert_eq!(id_of("LeBJ8QsjVe").as_deref(), Some("LeBJ8QsjVe"));
        assert_eq!(id_of("https://letronna.ai/docs"), None);
        assert_eq!(id_of("short"), None);
        assert_eq!(id_of("https://letronna.ai/s/../../etc"), None);
    }

    #[test]
    fn only_what_was_asked_and_the_replies_are_shared() {
        let m = |role: &str, content: &str| Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        };
        let mut notice = m("user", "files went back");
        notice.notice = true;
        let kept = kept(&[
            m("user", "Fix it"),
            m("tool", "secret"),
            m("assistant", ""),
            notice,
            m("assistant", "Done"),
        ]);
        assert_eq!(
            kept.iter().map(|s| s.content.as_str()).collect::<Vec<_>>(),
            ["Fix it", "Done"]
        );
    }
}
