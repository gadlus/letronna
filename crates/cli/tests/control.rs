//! `letronna -p --input-format stream-json`, driven over stdin as another
//! program would drive it, against a local stub provider and a data folder of
//! its own.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

/// Far longer than any turn here takes, so only a hang fails a test.
const WAIT: Duration = Duration::from_secs(60);

/// A data folder and a workspace for one test, emptied first.
fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("letronna-control-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    let (data, work) = (root.join("data"), root.join("work"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    (data, work)
}

/// The body of the request coming in on `socket`.
fn request(socket: &mut TcpStream) -> Value {
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
            .unwrap()
            .parse()
            .unwrap();
        if bytes.len() >= end + 4 + length {
            return serde_json::from_slice(&bytes[end + 4..end + 4 + length]).unwrap();
        }
    }
}

/// A provider that answers one request with each of `replies` in turn, or
/// for a None holds the request open unanswered, and passes on what each
/// request carried as it comes.
fn provider(replies: Vec<Option<String>>) -> (String, Receiver<Value>) {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
    let (sent, bodies) = mpsc::channel();
    std::thread::spawn(move || {
        for reply in replies {
            let (mut socket, _) = server.accept().unwrap();
            let _ = sent.send(request(&mut socket));
            let Some(reply) = reply else {
                // Held until the run lets go of it or the test is over.
                socket.set_read_timeout(Some(WAIT)).unwrap();
                let _ = socket.read(&mut [0; 1]);
                continue;
            };
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    });
    (endpoint, bodies)
}

/// A streamed reply that says `text` and makes `calls`.
fn reply(text: &str, calls: &[(&str, Value)]) -> Option<String> {
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
    Some(out)
}

/// A running `letronna` and the JSON lines it has written.
struct Driven {
    child: Child,
    stdin: Option<ChildStdin>,
    lines: Receiver<Value>,
}

impl Driven {
    /// Starts `letronna` with `flags` in `work`, set up in `data` to use the
    /// provider at `endpoint`.
    fn start(data: &Path, work: &Path, endpoint: &str, flags: &[&str]) -> Driven {
        let settings = json!({
            "endpoint": endpoint,
            "model": "stub-model",
            "gateway": "Custom",
            "custom_endpoint": endpoint,
        });
        std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_letronna"))
            .args(flags)
            .current_dir(work)
            // Nothing here may reach the real data folder or home.
            .env("AGENT_DATA_DIR", data)
            .env("HOME", data)
            .env("USERPROFILE", data)
            .env_remove("LOCALAPPDATA")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, lines) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                let value = serde_json::from_str(&line)
                    .unwrap_or_else(|_| panic!("stdout carried a line that is not JSON: {line}"));
                if tx.send(value).is_err() {
                    return;
                }
            }
        });
        let stdin = child.stdin.take();
        Driven {
            child,
            stdin,
            lines,
        }
    }

    /// Starts it with stream-json in and out.
    fn streaming(data: &Path, work: &Path, endpoint: &str, more: &[&str]) -> Driven {
        let mut flags = vec![
            "-p",
            "--input-format",
            "stream-json",
            "--output",
            "stream-json",
        ];
        flags.extend(more);
        Driven::start(data, work, endpoint, &flags)
    }

    fn send(&mut self, line: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn say(&mut self, text: &str) {
        self.send(&json!({"type": "user", "message": {"role": "user", "content": text}}));
    }

    /// The next line it wrote other than progress.
    fn next(&self) -> Value {
        loop {
            let line = self
                .lines
                .recv_timeout(WAIT)
                .expect("letronna wrote nothing more");
            if line["type"] != "progress" {
                return line;
            }
        }
    }

    /// Closes stdin and gives the exit code.
    fn close(mut self) -> i32 {
        drop(self.stdin.take());
        for _ in 0..(WAIT.as_millis() / 50) {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status.code().unwrap();
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        panic!("letronna did not exit once stdin closed");
    }
}

/// The contents of the messages a request sent, by role.
fn said(body: &Value) -> Vec<(String, String)> {
    body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|m| m["role"] != "system")
        .map(|m| {
            let role = m["role"].as_str().unwrap_or_default().to_owned();
            let content = match &m["content"] {
                Value::String(text) => text.clone(),
                other => other.to_string(),
            };
            (role, content)
        })
        .collect()
}

#[test]
fn messages_run_as_turns_in_the_order_sent() {
    let (data, work) = scratch("order");
    let (endpoint, bodies) = provider(vec![reply("First.", &[]), reply("Second.", &[])]);
    let mut run = Driven::streaming(&data, &work, &endpoint, &[]);
    // Both go in before the first turn ends, and stdin closes behind them.
    run.say("one");
    run.say("two");
    let first = run.next();
    assert_eq!(
        (&first["type"], &first["answer"]),
        (&json!("result"), &json!("First."))
    );
    let second = run.next();
    assert_eq!(
        (&second["status"], &second["answer"]),
        (&json!("done"), &json!("Second."))
    );
    assert_eq!(run.close(), 0);
    let _ = bodies.recv_timeout(WAIT).unwrap();
    let later = said(&bodies.recv_timeout(WAIT).unwrap());
    let tail: Vec<&str> = later[later.len() - 3..]
        .iter()
        .map(|(_, content)| content.as_str())
        .collect();
    assert_eq!(tail, ["one", "First.", "two"]);
}

#[test]
fn an_interrupt_stops_the_turn_that_is_running() {
    let (data, work) = scratch("interrupt");
    let (endpoint, bodies) = provider(vec![None]);
    let mut run = Driven::streaming(&data, &work, &endpoint, &[]);
    run.say("take your time");
    // The model has the request and is sitting on it.
    bodies
        .recv_timeout(WAIT)
        .expect("the turn never asked the model");
    run.send(&json!({"type": "control_request", "request_id": "stop-1",
        "request": {"subtype": "interrupt"}}));
    let answered = run.next();
    assert_eq!(answered["type"], "control_response");
    assert_eq!(
        answered["response"],
        json!({"subtype": "success", "request_id": "stop-1", "response": {}})
    );
    assert_eq!(run.next()["status"], "stopped");
    assert_eq!(run.close(), 0);
}

/// A turn that asks to write `out.txt` and is answered `behavior`, giving
/// what the model was told and what the run wrote.
fn asked_to_write(name: &str, behavior: Value) -> (Value, Vec<(String, String)>, PathBuf) {
    let (data, work) = scratch(name);
    let write = ("write_file", json!({"path": "out.txt", "content": "hi"}));
    // A write that runs is sent back once to be checked, and the answer
    // after the check is the same.
    let (endpoint, bodies) = provider(vec![
        reply("", &[write]),
        reply("Over.", &[]),
        reply("Over.", &[]),
    ]);
    let mut run = Driven::streaming(
        &data,
        &work,
        &endpoint,
        &["--permission-prompt-tool", "stdio"],
    );
    run.say("write it");
    let asked = run.next();
    assert_eq!(asked["type"], "control_request");
    let id = asked["request_id"].as_str().unwrap().to_owned();
    run.send(&json!({"type": "control_response", "response": {
        "request_id": id, "subtype": "success", "response": behavior}}));
    let result = run.next();
    assert_eq!(
        (&result["status"], &result["answer"]),
        (&json!("done"), &json!("Over."))
    );
    assert_eq!(run.close(), 0);
    let _ = bodies.recv_timeout(WAIT).unwrap();
    let told = said(&bodies.recv_timeout(WAIT).unwrap());
    (asked, told, work.join("out.txt"))
}

#[test]
fn a_call_allowed_over_stdin_runs() {
    let (asked, _, out) = asked_to_write("allowed", json!({"behavior": "allow"}));
    assert_eq!(
        asked["request"],
        json!({"subtype": "can_use_tool", "tool_name": "write_file",
            "input": {"path": "out.txt", "content": "hi"}, "tool_use_id": "call-0"})
    );
    assert_eq!(std::fs::read_to_string(out).unwrap(), "hi");
}

#[test]
fn a_call_denied_over_stdin_does_not_run_and_the_model_hears_why() {
    let deny = json!({"behavior": "deny", "message": "not today"});
    let (_, told, out) = asked_to_write("denied", deny);
    assert!(!out.exists());
    let (role, result) = told.last().unwrap();
    assert_eq!(role, "tool");
    // Labelled as the program's, since the user never said it.
    let why = "The program driving this run said why: not today";
    assert!(result.contains(why), "{result}");
}

#[test]
fn a_line_that_cannot_be_read_exits_2() {
    let (data, work) = scratch("malformed");
    let (endpoint, bodies) = provider(vec![]);
    let mut run = Driven::streaming(&data, &work, &endpoint, &[]);
    run.send(&json!({"kind": "user"}));
    assert_eq!(run.close(), 2);
    let mut run = Driven::streaming(&data, &work, &endpoint, &[]);
    writeln!(run.stdin.as_mut().unwrap(), "not json").unwrap();
    assert_eq!(run.close(), 2);
    // Asking for stream-json in with text out is bad usage.
    let flags = ["-p", "--input-format", "stream-json"];
    let run = Driven::start(&data, &work, &endpoint, &flags);
    assert_eq!(run.close(), 2);
    assert!(bodies.try_recv().is_err(), "the model was asked");
}

#[test]
fn closing_stdin_with_nothing_sent_ends_cleanly() {
    let (data, work) = scratch("closed");
    let (endpoint, bodies) = provider(vec![]);
    let run = Driven::streaming(&data, &work, &endpoint, &[]);
    assert_eq!(run.close(), 0);
    assert!(bodies.try_recv().is_err(), "the model was asked");
}

/// The program's answer to the request `asked`, with `behavior`.
fn answer(asked: &Value, behavior: Value) -> Value {
    json!({"type": "control_response", "response": {
        "request_id": asked["request_id"], "subtype": "success", "response": behavior}})
}

/// A run asked on stdin about its calls, with `rules` as the user's
/// permission rules and `flags` added, whose model asks to write `out.txt`
/// and then says "Over."; gives the run and its workspace.
fn writer(name: &str, rules: Value, flags: &[&str]) -> (Driven, PathBuf) {
    let (data, work) = scratch(name);
    std::fs::write(data.join("permissions.json"), rules.to_string()).unwrap();
    let write = ("write_file", json!({"path": "out.txt", "content": "hi"}));
    // A write that runs is sent back once to be checked, and the answer
    // after the check is the same.
    let (endpoint, _) = provider(vec![
        reply("", &[write]),
        reply("Over.", &[]),
        reply("Over.", &[]),
    ]);
    let mut all = vec!["--permission-prompt-tool", "stdio"];
    all.extend(flags);
    let mut run = Driven::streaming(&data, &work, &endpoint, &all);
    run.say("write it");
    (run, work)
}

#[test]
fn the_users_rules_and_mode_refuse_before_the_program_is_asked() {
    let deny = json!([{"tool": "write_file", "effect": "deny"}]);
    let cases = [
        ("deny-rule", deny, &[][..]),
        ("plan", json!([]), &["--plan"][..]),
        ("dont-ask", json!([]), &["--mode", "dont-ask"][..]),
    ];
    for (name, rules, flags) in cases {
        let (run, work) = writer(name, rules, flags);
        // A can_use_tool line would come first, and an allow could run it.
        let result = run.next();
        assert_eq!(
            (&result["type"], &result["answer"]),
            (&json!("result"), &json!("Over.")),
            "{name}"
        );
        assert_eq!(run.close(), 0);
        assert!(!work.join("out.txt").exists(), "{name}: the call ran");
    }
}

#[test]
fn an_allow_with_changed_input_runs_the_change_only_within_the_rules() {
    let rules = json!([{"tool": "write_file", "pattern": "secret.txt", "effect": "deny"}]);
    for (name, path, runs) in [
        ("changed", "other.txt", true),
        ("changed-denied", "secret.txt", false),
    ] {
        let (mut run, work) = writer(name, rules.clone(), &[]);
        let asked = run.next();
        assert_eq!(asked["request"]["subtype"], "can_use_tool");
        let changed = json!({"path": path, "content": "changed"});
        run.send(&answer(
            &asked,
            json!({"behavior": "allow", "updatedInput": changed}),
        ));
        assert_eq!(run.next()["answer"], "Over.");
        assert_eq!(run.close(), 0);
        assert!(
            !work.join("out.txt").exists(),
            "{name}: the model's own input ran"
        );
        assert_eq!(work.join(path).exists(), runs, "{name}");
    }
}

#[test]
fn the_budget_covers_every_message_of_the_run() {
    let (data, work) = scratch("budget");
    let (endpoint, bodies) = provider(vec![reply("First.", &[]), reply("Second.", &[])]);
    // One reply spends 1,050 tokens, more than the whole budget.
    let mut run = Driven::streaming(&data, &work, &endpoint, &["--budget", "1000"]);
    run.say("one");
    run.say("two");
    assert_eq!(run.next()["answer"], "First.");
    assert_eq!(run.next()["status"], "over_budget");
    assert_eq!(run.close(), 0);
    let _ = bodies.recv_timeout(WAIT).unwrap();
    assert!(
        bodies.try_recv().is_err(),
        "the second message reached the model"
    );
}

#[test]
fn other_lines_are_left_alone_and_a_message_it_cannot_send_is_refused_alone() {
    let (data, work) = scratch("unknown");
    let (endpoint, _bodies) = provider(vec![reply("Fine.", &[])]);
    let mut run = Driven::streaming(&data, &work, &endpoint, &[]);
    // A byte order mark in front and CRLF behind, as Windows editors write.
    let stdin = run.stdin.as_mut().unwrap();
    write!(stdin, "\u{feff}{}\r\n", json!({"type": "keep_alive"})).unwrap();
    run.send(&json!({"type": "system", "subtype": "status"}));
    let image = json!([{"type": "image", "source": {"type": "base64", "data": ""}}]);
    run.send(&json!({"type": "user", "message": {"role": "user", "content": image}}));
    run.say("still there?");
    assert_eq!(run.next()["status"], "failed");
    assert_eq!(run.next()["answer"], "Fine.");
    assert_eq!(run.close(), 1);
}

#[test]
fn an_interrupt_with_no_turn_running_leaves_the_next_message_alone() {
    let (data, work) = scratch("idle-interrupt");
    let (endpoint, _bodies) = provider(vec![reply("Done.", &[])]);
    let mut run = Driven::streaming(&data, &work, &endpoint, &[]);
    run.send(&json!({"type": "control_request", "request_id": "stop-1",
        "request": {"subtype": "interrupt"}}));
    assert_eq!(run.next()["response"]["request_id"], "stop-1");
    run.say("go on");
    assert_eq!(run.next()["status"], "done");
    assert_eq!(run.close(), 0);
}
