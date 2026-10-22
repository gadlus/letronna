//! A cheaper model for the calls made beside the conversation.
//!
//! A summary of the turns the window dropped, a suggested commit message
//! and a goal check each send a short prompt of their own, which shares no
//! prompt cache with the session's requests, and a small model does them
//! about as well as a large one. `Settings.side_model` names one on the
//! session's gateway for them; empty keeps them on the session's model.
//! Memory review stays on the session's model, as it reads the session's
//! cached prefix, which another model would read afresh at full price, and
//! so does Auto mode's judge, which decides what may run.
//!
//! A side model the gateway turns away -- a mistyped id, a model it does not
//! serve, a key or an account that may not use it -- would otherwise fail
//! every summary into a digest. A summary or a commit message goes once more
//! to the session's model instead, so all it costs is a quick refusal, and
//! each session is told once a run which setting to look at. The side model
//! is still asked first every time, so one put right counts from the next
//! call. A goal check is not asked again: it pauses the goal and says why,
//! as one on a helpers' model the gateway turns away does.

use crate::chat::Connection;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

/// A side call that came to nothing: the words for the user, and the HTTP
/// status the gateway answered with, when it answered with one.
pub(crate) struct Failed {
    message: String,
    status: Option<u16>,
}

impl From<String> for Failed {
    fn from(message: String) -> Self {
        Failed {
            message,
            status: None,
        }
    }
}

impl Failed {
    /// `error` from sending a side call, said as `message`, with its status.
    pub(crate) fn sending(error: &reqwest::Error, message: String) -> Self {
        Failed {
            message,
            status: error.status().map(|status| status.as_u16()),
        }
    }

    /// Why the gateway turned the model away, when it did. A busy or broken
    /// gateway is not among these: the session's model would only wait on
    /// it too.
    fn turned_away(&self) -> Option<&'static str> {
        Some(match self.status? {
            // Some gateways answer an id they do not know so.
            400 => "the gateway turned the request down",
            401 | 403 => "the key may not use it",
            402 => "the account has no credit for it",
            404 => "the gateway does not serve it",
            _ => return None,
        })
    }
}

/// What `ask` answers on the side model Settings names, or on `connection`'s
/// own model when it names none, or when the gateway turned the side model
/// away.
pub(crate) fn ask<T>(
    connection: &Connection,
    ask: impl FnMut(&Connection) -> Result<T, Failed>,
) -> Result<T, String> {
    ask_on(
        connection,
        &crate::store::current_settings().side_model,
        ask,
    )
}

/// `ask`, with the side model handed in.
pub(crate) fn ask_on<T>(
    connection: &Connection,
    side: &str,
    mut ask: impl FnMut(&Connection) -> Result<T, Failed>,
) -> Result<T, String> {
    if let Some(on_side) = on(connection, side) {
        let failed = match ask(&on_side) {
            Ok(answer) => return Ok(answer),
            Err(failed) => failed,
        };
        let Some(why) = failed.turned_away() else {
            return Err(failed.message);
        };
        tell(connection.conversation, &on_side.model, why);
    }
    ask(connection).map_err(|failed| failed.message)
}

/// `connection` on the side model `side`, or None when it names none or the
/// session's own.
fn on(connection: &Connection, side: &str) -> Option<Connection> {
    let side = side.trim();
    // A chat on this computer keeps to its own model: another would be
    // loaded in its place.
    if side.is_empty()
        || side == connection.model.trim()
        || crate::on_device::at(&connection.endpoint)
    {
        return None;
    }
    Some(Connection {
        model: side.to_owned(),
        ..connection.clone()
    })
}

/// What each session is to be told about a side model turned away, by the
/// session and the model: the words until they are told, then None.
type Refusals = Mutex<HashMap<(u64, String), Option<String>>>;

/// Every side model turned away in this process.
fn refusals() -> &'static Refusals {
    static REFUSALS: OnceLock<Refusals> = OnceLock::new();
    REFUSALS.get_or_init(Default::default)
}

/// Keep for `session` that `model` was turned away and `why`, unless it was
/// kept already: it is told once.
fn tell(session: u64, model: &str, why: &str) {
    let note = format!(
        "The side model {model} could not be used: {why}. This session's own model was asked instead. Set another with /model side <model>, or clear it with /model side off."
    );
    if let Ok(mut refusals) = refusals().lock() {
        refusals
            .entry((session, model.to_owned()))
            .or_insert(Some(note));
    }
}

/// What session `session` has not been told yet about its side model.
pub(crate) fn notice(session: u64) -> Option<String> {
    let mut refusals = refusals().lock().ok()?;
    refusals
        .iter_mut()
        .filter(|((asked_for, _), _)| *asked_for == session)
        .find_map(|(_, note)| note.take())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::Value;

    /// A gateway on this computer that answers each request with the next of
    /// `replies`, a status and a JSON body, and gives back the requests it
    /// was sent.
    pub(crate) fn gateway(
        replies: Vec<(u16, &'static str)>,
    ) -> (String, std::thread::JoinHandle<Vec<Value>>) {
        use std::io::{Read, Write};
        let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
        let worker = std::thread::spawn(move || {
            let mut sent = Vec::new();
            for (status, reply) in replies {
                let (mut socket, _) = server.accept().unwrap();
                let mut bytes = Vec::new();
                let mut block = [0; 65536];
                let body = loop {
                    let n = socket.read(&mut block).unwrap();
                    assert_ne!(n, 0);
                    bytes.extend_from_slice(&block[..n]);
                    let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                        continue;
                    };
                    let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                    let length: usize = (headers.lines())
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse()
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
                    }
                };
                sent.push(body);
                write!(
                    socket,
                    "HTTP/1.1 {status} Answer\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}",
                    reply.len()
                )
                .unwrap();
            }
            sent
        });
        (endpoint, worker)
    }

    fn session(conversation: u64) -> Connection {
        Connection {
            model: "big-model".into(),
            conversation,
            ..Default::default()
        }
    }

    fn status(status: u16) -> Failed {
        Failed {
            message: format!("status {status}"),
            status: Some(status),
        }
    }

    #[test]
    fn a_side_call_goes_to_the_side_model_when_one_is_named() {
        let mut asked = Vec::new();
        let answer = ask_on(&session(9_001), " small-model ", |on| {
            asked.push(on.model.clone());
            Ok::<_, Failed>("answer")
        });
        assert_eq!(answer, Ok("answer"));
        assert_eq!(asked, ["small-model"]);
        // None named, or the session's own, is the session's.
        for side in ["", "big-model"] {
            let mut asked = Vec::new();
            let _ = ask_on(&session(9_001), side, |on| {
                asked.push(on.model.clone());
                Ok::<_, Failed>(())
            });
            assert_eq!(asked, ["big-model"]);
        }
        assert_eq!(notice(9_001), None);
    }

    #[test]
    fn a_side_model_turned_away_hands_the_call_to_the_session_model_and_says_so_once() {
        for _ in 0..2 {
            let mut asked = Vec::new();
            let answer = ask_on(&session(9_002), "smal-model", |on| {
                asked.push(on.model.clone());
                match on.model.as_str() {
                    "smal-model" => Err(status(404)),
                    _ => Ok("answer"),
                }
            });
            assert_eq!(answer, Ok("answer"));
            assert_eq!(asked, ["smal-model", "big-model"]);
        }
        let told = notice(9_002).unwrap_or_default();
        assert!(told.contains("smal-model"), "{told}");
        assert!(told.contains("does not serve it"), "{told}");
        assert!(told.contains("/model side"), "{told}");
        assert_eq!(notice(9_002), None);
        // Another session hears of it for itself.
        assert_eq!(notice(9_003), None);
    }

    #[test]
    fn a_side_model_that_failed_otherwise_is_not_asked_again_on_the_session_model() {
        for failed in [
            status(500),
            status(429),
            Failed::from("timed out".to_owned()),
        ] {
            let message = failed.message.clone();
            let mut failed = Some(failed);
            let mut asked = Vec::new();
            let answer = ask_on(&session(9_004), "small-model", |on| {
                asked.push(on.model.clone());
                Err::<(), _>(failed.take().unwrap_or_else(|| status(200)))
            });
            assert_eq!(answer, Err(message));
            assert_eq!(asked, ["small-model"]);
        }
        assert_eq!(notice(9_004), None);
    }

    #[test]
    fn a_chat_on_this_computer_keeps_its_side_calls_on_its_own_model() {
        let here = Connection {
            endpoint: crate::on_device::BASE.into(),
            model: "bonsai-2-4b".into(),
            ..Default::default()
        };
        assert!(on(&here, "small-model").is_none());
        let hosted = Connection {
            endpoint: "https://api.example.com/v1".into(),
            ..here
        };
        assert_eq!(
            on(&hosted, "small-model").map(|c| c.model).as_deref(),
            Some("small-model")
        );
    }
}
