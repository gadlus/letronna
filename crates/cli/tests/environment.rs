//! `letronna -p` sent to a server by `AGENT_BASE_URL`, `AGENT_API_KEY` and
//! `AGENT_MODEL` alone, as a benchmark's container starts it: a data folder
//! with nothing set up, and a stub provider that records what it was sent.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc::{self, Receiver};

/// Long enough to be no key the stub could be sent by chance, and past the
/// 16 characters from which a key is hidden.
const KEY: &str = "env-key-0123456789abcdef";

/// A data folder and a workspace for one test, emptied first.
fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("letronna-environment-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    let (data, work) = (root.join("data"), root.join("work"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (data, work)
}

/// The headers, lowercased, and the body of the request coming in on `socket`.
fn request(socket: &mut TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut block = [0; 65536];
    loop {
        let n = socket.read(&mut block).unwrap();
        assert_ne!(n, 0, "the request ended early");
        bytes.extend_from_slice(&block[..n]);
        let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
        let length: usize = headers
            .lines()
            .find_map(|l| l.strip_prefix("content-length: "))
            .map_or(0, |n| n.parse().unwrap());
        if bytes.len() >= end + 4 + length {
            let body =
                serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap_or_default();
            return (headers, body);
        }
    }
}

/// A provider that answers each chat request with the next of `replies`,
/// and passes on the headers and body of each as it comes. Its model list,
/// which a run with a key asks for once, is not there.
fn provider(replies: Vec<String>) -> (String, Receiver<(String, Value)>) {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
    let (sent, requests) = mpsc::channel();
    std::thread::spawn(move || {
        let mut replies = replies.into_iter();
        while let Ok((mut socket, _)) = server.accept() {
            let (headers, body) = request(&mut socket);
            if headers.starts_with("get ") {
                let _ = write!(
                    socket,
                    "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                );
                continue;
            }
            let _ = sent.send((headers, body));
            let Some(reply) = replies.next() else {
                return;
            };
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    });
    (endpoint, requests)
}

/// A streamed reply that says `text` and makes `calls`.
fn reply(text: &str, calls: &[(&str, Value)]) -> String {
    let calls: Vec<Value> = calls
        .iter()
        .enumerate()
        .map(|(i, (name, args))| {
            json!({"index": i, "id": format!("call-{i}"),
                "function": {"name": name, "arguments": args.to_string()}})
        })
        .collect();
    let mut events = Vec::new();
    if !text.is_empty() {
        events.push(json!({"choices": [{"delta": {"content": text}}]}));
    }
    if !calls.is_empty() {
        events.push(json!({"choices": [{"delta": {"tool_calls": calls}}]}));
    }
    events.push(json!({"choices": [],
        "usage": {"prompt_tokens": 1_000, "completion_tokens": 50}}));
    let mut out: String = events
        .iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect();
    out.push_str("data: [DONE]\n\n");
    out
}

/// Runs `letronna -p` with `words` in `work`, answering in JSON, on the
/// data folder `data` and the server at `endpoint`, given by the
/// environment alone.
fn ask(data: &Path, work: &Path, endpoint: &str, words: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_letronna"))
        .args(["--output", "json", "-p"])
        .args(words)
        .current_dir(work)
        // Nothing here may reach the real data folder or home.
        .env("AGENT_DATA_DIR", data)
        .env("HOME", data)
        .env("USERPROFILE", data)
        .env_remove("LOCALAPPDATA")
        .env("AGENT_BASE_URL", endpoint)
        .env("AGENT_API_KEY", KEY)
        .env("AGENT_MODEL", "env-model")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

/// Every file under `dir` that holds `text`.
fn holding(dir: &Path, text: &str) -> Vec<PathBuf> {
    let mut found = Vec::new();
    for entry in std::fs::read_dir(dir).unwrap().map_while(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            found.extend(holding(&path, text));
        } else if String::from_utf8_lossy(&std::fs::read(&path).unwrap_or_default()).contains(text)
        {
            found.push(path);
        }
    }
    found
}

#[test]
fn a_print_run_configured_only_by_the_environment_reaches_the_stub() {
    let (data, work) = scratch("reaches");
    let (endpoint, requests) = provider(vec![reply("Pong.", &[])]);
    let out = ask(&data, &work, &endpoint, &["ping"]);
    let (stdout, stderr) = (
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    assert!(out.status.success(), "{stdout}{stderr}");
    let result: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(result["answer"], "Pong.");
    let (headers, body) = requests.try_recv().expect("the stub was never asked");
    assert_eq!(body["model"], "env-model");
    assert!(
        headers.contains(&format!("authorization: bearer {}", KEY.to_lowercase())),
        "{headers}"
    );
    // For this run only: nothing was set up by it, and the key is nowhere.
    assert!(!format!("{stdout}{stderr}").contains(KEY));
    assert_eq!(holding(&data, KEY), Vec::<PathBuf>::new());
    let settings = std::fs::read_to_string(data.join("settings.json")).unwrap_or_default();
    assert!(!settings.contains(&endpoint) && !settings.contains("env-model"));
}

#[test]
fn the_key_from_the_environment_is_hidden_as_a_saved_one_is() {
    let (data, work) = scratch("hidden");
    std::fs::write(work.join("notes.txt"), format!("notes: {KEY}\n")).unwrap();
    let read = ("read_file", json!({"path": "notes.txt"}));
    let (endpoint, requests) = provider(vec![reply("", &[read]), reply("Read.", &[])]);
    let out = ask(&data, &work, &endpoint, &["what do the notes say?"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = requests.try_recv().expect("the stub was never asked");
    let (_, body) = requests.try_recv().expect("the file was never read back");
    let sent = body.to_string();
    assert!(!sent.contains(KEY), "the model was sent the key: {sent}");
    assert!(
        sent.contains("notes: [secret hidden by Letronna]"),
        "{sent}"
    );
    assert_eq!(holding(&data, KEY), Vec::<PathBuf>::new());
}

#[test]
fn a_model_named_for_a_routed_run_is_not_pinned_on_the_session() {
    let (data, work) = scratch("pin");
    // Set up for a gateway of its own, which nothing listens on.
    let elsewhere = "http://127.0.0.1:9/v1";
    let settings = json!({
        "endpoint": elsewhere,
        "model": "set-up-model",
        "gateway": "Custom",
        "custom_endpoint": elsewhere,
    });
    std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
    let (endpoint, requests) = provider(vec![reply("Pong.", &[])]);
    let out = ask(&data, &work, &endpoint, &["ping", "--model", "flag-model"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let (_, body) = requests.try_recv().expect("the stub was never asked");
    assert_eq!(body["model"], "flag-model");
    // A later run without the environment would ask the gateway set up for
    // a model that is only the routed server's. The reply still says which
    // model wrote it; the session's own line names none.
    let saved = std::fs::read_to_string(data.join("sessions/1.jsonl")).unwrap();
    let meta: Value = saved
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .find(|line: &Value| line["type"] == "meta")
        .expect("the session was not saved");
    assert_eq!(meta.get("model"), None, "{meta}");
    assert_eq!(meta.get("gateway"), None, "{meta}");
}
