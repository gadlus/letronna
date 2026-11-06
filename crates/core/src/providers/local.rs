//! An OpenAI-compatible endpoint on this computer, off unless Settings turn
//! it on, so other tools can use the accounts connected here. It listens on
//! 127.0.0.1 only and answers nothing without the bearer token kept in the
//! system vault. `GET /v1/models` lists the connected accounts' models;
//! `POST /v1/chat/completions` sends through the router, so a rate-limited
//! account hands the request on as a session's would.

use crate::chat::{Connection, Event, Message};
use crate::store::Settings;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, Shutdown, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, PoisonError};

/// The port when Settings give none.
pub const DEFAULT_PORT: u16 = 8642;

/// Where the token is kept in the vault.
const SLOT: &str = "local_endpoint_token";

/// The token a request must carry, made the first time it is asked for.
/// Without a vault it lasts until Letronna quits.
pub fn token() -> String {
    static MADE: OnceLock<String> = OnceLock::new();
    if let Ok(Some(kept)) = crate::vault::get(SLOT) {
        return kept;
    }
    MADE.get_or_init(|| {
        use ring::rand::SecureRandom;
        let mut bytes = [0u8; 24];
        let _ = ring::rand::SystemRandom::new().fill(&mut bytes);
        let made = format!(
            "ltr-{}",
            bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
        );
        let _ = crate::vault::save(SLOT, &made, "");
        made
    })
    .clone()
}

/// The running endpoint: its port and the flag that stops it.
static RUNNING: Mutex<Option<(u16, Arc<AtomicBool>)>> = Mutex::new(None);

/// Start or stop the endpoint as `settings` say; a changed port restarts it.
/// Gives the error when the port could not be taken.
pub fn apply(settings: &Settings) -> Result<(), String> {
    let want = settings
        .local_endpoint
        .then_some(match settings.local_port {
            0 => DEFAULT_PORT,
            port => port,
        });
    let mut running = RUNNING.lock().unwrap_or_else(PoisonError::into_inner);
    if running.as_ref().map(|(port, _)| *port) == want {
        return Ok(());
    }
    if let Some((port, stop)) = running.take() {
        stop.store(true, Ordering::SeqCst);
        // Wakes the accept so the loop sees the flag.
        let _ = TcpStream::connect((Ipv4Addr::LOCALHOST, port));
    }
    if let Some(port) = want {
        let (bound, stop) = start(port, token())?;
        *running = Some((bound, stop));
    }
    Ok(())
}

/// Listen on 127.0.0.1:`port` (0 for any free one), and give the port taken
/// and the flag that stops it.
pub fn start(port: u16, token: String) -> Result<(u16, Arc<AtomicBool>), String> {
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, port)))
        .map_err(|e| format!("Port {port} could not be taken: {e}"))?;
    let bound = listener.local_addr().map_err(|e| e.to_string())?.port();
    let stop = Arc::new(AtomicBool::new(false));
    let stopping = stop.clone();
    let token: Arc<str> = token.into();
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if stopping.load(Ordering::SeqCst) {
                break;
            }
            let Ok(stream) = stream else { continue };
            let token = token.clone();
            std::thread::spawn(move || {
                let _ = handle(stream, &token);
            });
        }
    });
    Ok((bound, stop))
}

fn handle(stream: TcpStream, token: &str) -> std::io::Result<()> {
    // Dropped last, once the reply is written: it ends the connection for
    // the client even where a program started meanwhile holds a copy of
    // this socket, as Windows can hand one, and closing would leave the
    // client waiting until that program ended.
    let _ends = Ends(stream.try_clone()?);
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut out = stream;
    let mut first = String::new();
    reader.read_line(&mut first)?;
    let (mut length, mut authorized) = (0usize, false);
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 || line.trim().is_empty() {
            break;
        }
        let (name, value) = line.split_once(':').unwrap_or((&line, ""));
        match name.trim().to_ascii_lowercase().as_str() {
            "content-length" => length = value.trim().parse().unwrap_or(0),
            "authorization" => {
                let given = value.trim().strip_prefix("Bearer ").unwrap_or("").trim();
                authorized = same(given.as_bytes(), token.as_bytes());
            }
            _ => {}
        }
    }
    // ponytail: 16 MB cap on a body; raise it if images in requests need more.
    let mut body = vec![0; length.min(16 << 20)];
    reader.read_exact(&mut body)?;
    let mut parts = first.split_whitespace();
    let (method, path) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    let path = path.split('?').next().unwrap_or("");
    if !authorized {
        return reply(&mut out, 401, &error("Missing or wrong bearer token.")); // never echo it
    }
    match (method, path) {
        ("GET", "/v1/models") => reply(&mut out, 200, &models()),
        ("POST", "/v1/chat/completions") => match serde_json::from_slice(&body) {
            Ok(request) => complete(&mut out, &request),
            Err(e) => reply(&mut out, 400, &error(&format!("Not JSON: {e}"))),
        },
        _ => reply(
            &mut out,
            404,
            &error("Only /v1/models and /v1/chat/completions are here."),
        ),
    }
}

/// Says no more is coming on its connection when dropped.
struct Ends(TcpStream);

impl Drop for Ends {
    fn drop(&mut self) {
        let _ = self.0.shutdown(Shutdown::Write);
    }
}

/// Equal without saying where they differ by how long it took.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |d, (x, y)| d | (x ^ y)) == 0
}

fn error(message: &str) -> Value {
    json!({ "error": { "message": message, "type": "letronna_error" } })
}

fn reply(out: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let body = body.to_string();
    write!(
        out,
        "HTTP/1.1 {status} {}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
        if status == 200 { "OK" } else { "Error" },
        body.len()
    )
}

/// Every connected account's models, each once.
fn models() -> Value {
    let settings = crate::store::current_settings();
    let keys = crate::store::load_keys();
    let (all, _) = crate::gateway::all_models(&keys, &settings.custom_endpoint);
    let mut seen = std::collections::BTreeSet::new();
    let data: Vec<Value> = all
        .into_iter()
        .filter(|(_, model)| seen.insert(model.clone()))
        .map(|(owner, id)| json!({ "id": id, "object": "model", "owned_by": owner }))
        .collect();
    json!({ "object": "list", "data": data })
}

/// The first connected account that lists `model`.
fn connection_for(model: &str) -> Option<Connection> {
    let settings = crate::store::current_settings();
    let keys = crate::store::load_keys();
    // ponytail: asks every account for its list on each request; keep the
    // lists if this endpoint sees heavy use.
    let (all, _) = crate::gateway::all_models(&keys, &settings.custom_endpoint);
    let (owner, _) = all.into_iter().find(|(_, m)| m == model)?;
    let (_, endpoint, key) = crate::gateway::ready(&keys, &settings.custom_endpoint)
        .into_iter()
        .find(|(name, ..)| *name == owner)?;
    Some(Connection {
        endpoint,
        key,
        model: model.to_owned(),
        gateway: owner,
        ..Default::default()
    })
}

/// A message's text, whether given as a string or as parts.
fn text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

fn complete(out: &mut TcpStream, request: &Value) -> std::io::Result<()> {
    let model = request["model"].as_str().unwrap_or_default();
    let Some(mut connection) = connection_for(model) else {
        return reply(
            out,
            404,
            &error(&format!("No connected account lists {model:?}.")),
        );
    };
    let (mut system, mut messages) = (Vec::new(), Vec::new());
    for m in request["messages"].as_array().into_iter().flatten() {
        let content = text(&m["content"]);
        match m["role"].as_str().unwrap_or("user") {
            "system" | "developer" => system.push(content),
            "assistant" => messages.push(Message {
                role: "assistant".into(),
                content,
                ..Default::default()
            }),
            _ => messages.push(Message {
                role: "user".into(),
                content,
                ..Default::default()
            }),
        }
    }
    if messages.is_empty() {
        return reply(out, 400, &error("No messages."));
    }
    // Their own instructions, not Letronna's, and no tools of this computer.
    connection.system = Some(system.join("\n\n").into());
    let events = crate::chat::start_with(connection, messages, Default::default());
    let id = format!("chatcmpl-{}", crate::chat::now());
    let stream = request["stream"].as_bool().unwrap_or(false);
    if stream {
        write!(
            out,
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncache-control: no-cache\r\nconnection: close\r\n\r\n"
        )?;
    }
    let chunk = |delta: Value, finish: Value| {
        json!({ "id": id, "object": "chat.completion.chunk", "model": model,
                "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
    };
    let (mut answer, mut usage) = (String::new(), Value::Null);
    while let Ok(event) = events.recv_blocking() {
        match event {
            Event::Delta(piece) if stream => {
                write!(
                    out,
                    "data: {}\n\n",
                    chunk(json!({ "content": piece }), Value::Null)
                )?;
                out.flush()?;
            }
            Event::Delta(piece) => answer.push_str(&piece),
            Event::Usage {
                completion, prompt, ..
            } => {
                usage = json!({ "prompt_tokens": prompt, "completion_tokens": completion,
                                "total_tokens": prompt + completion });
            }
            Event::Finished(Err(failure)) if stream => {
                write!(out, "data: {}\n\n", error(&failure.to_string()))?;
                break;
            }
            Event::Finished(Err(failure)) => return reply(out, 502, &error(&failure.to_string())),
            Event::Finished(Ok(())) => break,
            _ => {}
        }
    }
    if stream {
        write!(
            out,
            "data: {}\n\ndata: [DONE]\n\n",
            chunk(json!({}), json!("stop"))
        )?;
        return out.flush();
    }
    reply(
        out,
        200,
        &json!({
            "id": id, "object": "chat.completion", "created": crate::chat::now(), "model": model,
            "choices": [{ "index": 0, "message": { "role": "assistant", "content": answer },
                          "finish_reason": "stop" }],
            "usage": usage,
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::TcpStream;

    fn get(port: u16, auth: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", port)).unwrap();
        write!(s, "GET /v1/models HTTP/1.1\r\nhost: x\r\n{auth}\r\n").unwrap();
        let mut reply = String::new();
        s.read_to_string(&mut reply).unwrap();
        reply
    }

    #[test]
    fn turns_away_a_request_without_the_token_and_lists_models_with_it() {
        let (port, _stop) = super::start(0, "secret".into()).unwrap();
        assert!(get(port, "").starts_with("HTTP/1.1 401"));
        assert!(get(port, "authorization: Bearer wrong\r\n").starts_with("HTTP/1.1 401"));
        let ok = get(port, "authorization: Bearer secret\r\n");
        assert!(ok.starts_with("HTTP/1.1 200"), "{ok}");
        let body: serde_json::Value =
            serde_json::from_str(ok.split("\r\n\r\n").nth(1).unwrap()).unwrap();
        assert_eq!(body["object"], "list");
        assert!(body["data"].is_array());
    }
}
