//! `letronna -p --input-format stream-json`, driven over stdin as another
//! program would drive it, the JSON `-p --output json` gives a script, and
//! `letronna -p` given a deadline, against a local stub provider and a data
//! folder of its own.

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
    /// Starts `letronna` with `flags` and `env` in `work`, set up in `data` to
    /// use the provider at `endpoint`.
    fn start(
        data: &Path,
        work: &Path,
        endpoint: &str,
        flags: &[&str],
        env: &[(&str, &str)],
    ) -> Driven {
        let settings = json!({
            "endpoint": endpoint,
            "model": "stub-model",
            "gateway": "Custom",
            "custom_endpoint": endpoint,
        });
        std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
        let mut child = Command::new(env!("CARGO_BIN_EXE_letronna"))
            .args(flags)
            .envs(env.iter().copied())
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
        Driven::start(data, work, endpoint, &flags, &[])
    }

    fn send(&mut self, line: &Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{line}").unwrap();
        stdin.flush().unwrap();
    }

    fn say(&mut self, text: &str) {
        self.send(&json!({"type": "user", "message": {"role": "user", "content": text}}));
    }

    /// The next line it wrote other than the opening one and progress.
    fn next(&self) -> Value {
        loop {
            let line = self.first();
            if line["type"] != "progress" && line["type"] != "init" {
                return line;
            }
        }
    }

    /// The next line it wrote, whatever it is.
    fn first(&self) -> Value {
        self.lines
            .recv_timeout(WAIT)
            .expect("letronna wrote nothing more")
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
    let run = Driven::start(&data, &work, &endpoint, &flags, &[]);
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

#[test]
fn a_run_that_never_writes_its_required_file_says_so_in_the_json_result() {
    let (data, work) = scratch("contract");
    // The check sends it back once, and it ends again without the file.
    let (endpoint, bodies) = provider(vec![reply("Done.", &[]), reply("Still done.", &[])]);
    let ask = "Read notes.txt and write what it says to out/summary.json.";
    let run = Driven::start(
        &data,
        &work,
        &endpoint,
        &["-p", ask, "--output", "json"],
        &[],
    );
    let result = run.next();
    // Still done, so a script's idea of success is as it was.
    assert_eq!(
        (&result["status"], &result["answer"], &result["contract"]),
        (
            &json!("done"),
            &json!("Still done."),
            &json!(["`out/summary.json` does not exist."])
        )
    );
    assert_eq!(run.close(), 0);
    let _ = bodies.recv_timeout(WAIT).unwrap();
    let (role, check) = said(&bodies.recv_timeout(WAIT).unwrap()).pop().unwrap();
    assert_eq!(role, "user");
    assert!(
        check.contains("`out/summary.json` does not exist."),
        "{check}"
    );
}

#[test]
fn the_json_result_names_the_session_and_its_model() {
    let (data, work) = scratch("named");
    let (endpoint, bodies) = provider(vec![reply("Hello.", &[]), reply("Again.", &[])]);
    let flags = ["-p", "hello", "--output", "json"];
    let run = Driven::start(&data, &work, &endpoint, &flags, &[]);
    let result = run.next();
    assert_eq!(
        (&result["model"], &result["gateway"], &result["refused"]),
        (&json!("stub-model"), &json!("Custom"), &json!(0))
    );
    // A stub's model has no listed price, so what it cost is not known.
    assert_eq!(result.get("cost_usd"), Some(&Value::Null));
    assert_eq!(run.close(), 0);
    let session = result["session"]
        .as_u64()
        .expect("no session in the result");
    let file = data.join("sessions").join(format!("{session}.jsonl"));
    assert!(file.exists(), "no session file at {}", file.display());
    // That id carries the same conversation on.
    let id = session.to_string();
    let flags = ["-p", "again", "--resume", &id, "--output", "json"];
    let run = Driven::start(&data, &work, &endpoint, &flags, &[]);
    assert_eq!(run.next()["session"], session);
    assert_eq!(run.close(), 0);
    let _ = bodies.recv_timeout(WAIT).unwrap();
    let later = said(&bodies.recv_timeout(WAIT).unwrap());
    let tail: Vec<&str> = later[later.len() - 3..]
        .iter()
        .map(|(_, content)| content.as_str())
        .collect();
    assert_eq!(tail, ["hello", "Hello.", "again"]);
}

#[test]
fn refused_calls_are_counted_in_the_json_result() {
    let deny = json!([{"tool": "write_file", "effect": "deny"}]);
    // Refused by a rule, and outside the tools the run was given.
    let cases = [
        ("count-rule", deny, &[][..]),
        ("count-tools", json!([]), &["--tools", "read_file"][..]),
    ];
    for (name, rules, flags) in cases {
        let (run, work) = writer(name, rules, flags);
        let result = run.next();
        assert_eq!(
            (&result["answer"], &result["refused"]),
            (&json!("Over."), &json!(1)),
            "{name}"
        );
        assert_eq!(run.close(), 0);
        assert!(!work.join("out.txt").exists(), "{name}: the call ran");
    }
    // Answered no, and answered yes.
    for (name, behavior, refused) in [
        ("count-no", json!({"behavior": "deny", "message": "no"}), 1),
        ("count-yes", json!({"behavior": "allow"}), 0),
    ] {
        let (mut run, _) = writer(name, json!([]), &[]);
        let asked = run.next();
        assert_eq!(asked["request"]["subtype"], "can_use_tool", "{name}");
        run.send(&answer(&asked, behavior));
        assert_eq!(run.next()["refused"], refused, "{name}");
        assert_eq!(run.close(), 0);
    }
}

#[test]
fn stream_json_starts_with_an_init_line_naming_the_session() {
    let (data, work) = scratch("init");
    let (endpoint, _bodies) = provider(vec![reply("Hi.", &[]), reply("Hi again.", &[])]);
    let flags = ["-p", "hello", "--output", "stream-json"];
    let run = Driven::start(&data, &work, &endpoint, &flags, &[]);
    let init = run.first();
    assert_eq!(
        (&init["type"], &init["model"], &init["gateway"]),
        (&json!("init"), &json!("stub-model"), &json!("Custom"))
    );
    assert!(init["session"].is_u64(), "{init}");
    let result = run.next();
    assert_eq!(
        (&result["type"], &result["session"]),
        (&json!("result"), &init["session"])
    );
    assert_eq!(run.close(), 0);
    // Driven over stdin, it says so before any message is sent.
    let mut run = Driven::streaming(&data, &work, &endpoint, &[]);
    let init = run.first();
    assert_eq!(init["type"], "init");
    run.say("hello");
    assert_eq!(run.next()["session"], init["session"]);
    assert_eq!(run.close(), 0);
}

#[test]
fn a_print_run_past_its_deadline_stops_with_its_result() {
    let (data, work) = scratch("deadline-past");
    // The model sits on the request, so only the deadline ends the run.
    let (endpoint, _bodies) = provider(vec![None]);
    let flags = ["-p", "take your time", "--output", "json"];
    let run = Driven::start(
        &data,
        &work,
        &endpoint,
        &flags,
        &[("LETRONNA_DEADLINE", "2")],
    );
    assert_eq!(run.next()["status"], "stopped");
    // Out of time is not a Ctrl-C, so a harness counts the run as having ended.
    assert_eq!(run.close(), 0);
}

#[test]
fn a_print_run_near_its_deadline_is_told_to_commit_its_outputs() {
    let (data, work) = scratch("deadline-near");
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/v1", server.local_addr().unwrap());
    let (sent, bodies) = mpsc::channel();
    // The run has 8 s from its start, which is before its first request: the
    // first answer comes past the warning at 6.4 s, and the second request
    // goes out well before 8 s.
    let replies = [
        (Duration::from_millis(6_800), "Done."),
        (Duration::ZERO, "Every output is written."),
    ];
    std::thread::spawn(move || {
        for (held, text) in replies {
            let (mut socket, _) = server.accept().unwrap();
            let _ = sent.send(request(&mut socket));
            std::thread::sleep(held);
            let reply = reply(text, &[]).unwrap();
            write!(socket, "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{reply}", reply.len()).unwrap();
        }
    });
    let flags = ["-p", "write the report", "--output", "json"];
    let run = Driven::start(
        &data,
        &work,
        &endpoint,
        &flags,
        &[("LETRONNA_DEADLINE", "8")],
    );
    let result = run.next();
    assert_eq!(
        (&result["status"], &result["answer"]),
        (&json!("done"), &json!("Every output is written."))
    );
    assert_eq!(run.close(), 0);
    let _ = bodies.recv_timeout(WAIT).unwrap();
    let later = said(&bodies.recv_timeout(WAIT).unwrap());
    let (role, told) = later.last().unwrap();
    assert_eq!(role, "user");
    assert!(
        told.contains("s are left before this run is stopped"),
        "{told}"
    );
}

#[cfg(unix)]
#[test]
fn a_sigterm_stops_a_print_run_and_prints_its_result() {
    let (data, work) = scratch("sigterm");
    let (endpoint, bodies) = provider(vec![None]);
    let flags = ["-p", "take your time", "--output", "json"];
    let run = Driven::start(&data, &work, &endpoint, &flags);
    // The model has the request and is sitting on it.
    bodies
        .recv_timeout(WAIT)
        .expect("the run never asked the model");
    let pid = libc::pid_t::try_from(run.child.id()).unwrap();
    // SAFETY: a signal to the letronna this test started, as a supervisor
    // sends one at its time limit.
    unsafe { libc::kill(pid, libc::SIGTERM) };
    assert_eq!(run.next()["status"], "stopped");
    // What the run says for a Ctrl-C, once what it started is ended.
    assert_eq!(run.close(), 130);
}

#[test]
fn a_print_run_with_a_schema_returns_its_value() {
    let (data, work) = scratch("schema");
    let schema = json!({"type": "object", "required": ["count"],
        "properties": {"count": {"type": "integer"}}});
    std::fs::write(work.join("answer.json"), schema.to_string()).unwrap();
    let given = [("structured_output", json!({"count": 3}))];
    let replies = vec![reply("Three.", &given), reply("Three.", &given)];
    let (endpoint, bodies) = provider(replies);
    let flags = [
        "-p",
        "count them",
        "--output",
        "json",
        "--json-schema",
        "answer.json",
    ];
    let run = Driven::start(&data, &work, &endpoint, &flags, &[]);
    let result = run.next();
    assert_eq!(
        (&result["status"], &result["value"], &result["answer"]),
        (&json!("done"), &json!({"count": 3}), &json!("Three."))
    );
    assert_eq!(run.close(), 0);
    let tools = bodies.recv_timeout(WAIT).unwrap()["tools"].clone();
    let offered = tools
        .as_array()
        .unwrap()
        .iter()
        .find(|tool| tool["function"]["name"] == "structured_output");
    assert_eq!(offered.unwrap()["function"]["parameters"], schema);
    // As text, the value alone is printed, as one line of JSON.
    let flags = ["-p", "count them", "--json-schema", "answer.json"];
    let run = Driven::start(&data, &work, &endpoint, &flags, &[]);
    assert_eq!(run.next(), json!({"count": 3}));
    assert_eq!(run.close(), 0);
}
