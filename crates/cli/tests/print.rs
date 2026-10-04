//! `letronna -p` started by a script, with nobody at a keyboard, against a
//! model server on this computer and a data folder of its own.

use serde_json::json;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

/// The start of what a run says when what needs a yes will be refused.
const UNANSWERED: &str = "Nobody can approve calls in this run";

/// A data folder set up for a model server that answers every request
/// "Done.", and a workspace beside it, for one test.
fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let root = std::env::temp_dir().join(format!("letronna-print-{name}"));
    let _ = std::fs::remove_dir_all(&root);
    let (data, work) = (root.join("data"), root.join("work"));
    std::fs::create_dir_all(&data).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let base =
        model("data: {\"choices\":[{\"delta\":{\"content\":\"Done.\"}}]}\n\ndata: [DONE]\n\n");
    let settings = json!({
        "endpoint": base,
        "model": "test-model",
        "gateway": "Custom",
        "custom_endpoint": base,
    });
    std::fs::write(data.join("settings.json"), settings.to_string()).unwrap();
    (data, work)
}

/// A model server answering every request with the stream `body`, and the
/// base URL to set up for it.
fn model(body: &str) -> String {
    let server = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", server.local_addr().unwrap());
    let answer = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    std::thread::spawn(move || {
        for socket in server.incoming() {
            let Ok(mut socket) = socket else {
                continue;
            };
            // Read whole before answering, so closing does not reset it.
            let mut bytes = Vec::new();
            let mut block = [0; 65536];
            while let Ok(n) = socket.read(&mut block)
                && n > 0
            {
                bytes.extend_from_slice(&block[..n]);
                let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
                    continue;
                };
                let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
                let length: usize = head
                    .lines()
                    .find_map(|l| l.strip_prefix("content-length: "))
                    .and_then(|l| l.trim().parse().ok())
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + length {
                    break;
                }
            }
            let _ = socket.write_all(answer.as_bytes());
        }
    });
    base
}

/// Runs `letronna` with `flags` in `work`, set up in `data`, its stdin and
/// stdout pipes as a script's are.
fn run(data: &Path, work: &Path, flags: &[&str]) -> Output {
    run_leaving(data, work, flags, "")
}

/// `run`, with `LETRONNA_PROMPT_RULES` set to `rules`.
fn run_leaving(data: &Path, work: &Path, flags: &[&str], rules: &str) -> Output {
    Command::new(env!("CARGO_BIN_EXE_letronna"))
        .args(flags)
        .env("LETRONNA_PROMPT_RULES", rules)
        .current_dir(work)
        // Nothing here may reach the real data folder or home.
        .env("AGENT_DATA_DIR", data)
        .env("HOME", data)
        .env("USERPROFILE", data)
        .env_remove("LOCALAPPDATA")
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn a_piped_default_run_warns_on_stderr() {
    let streamed = [
        "-p",
        "--input-format",
        "stream-json",
        "--output",
        "stream-json",
    ];
    let cases = [
        ("text", &["-p", "hello"][..]),
        ("json", &["-p", "--output", "json", "hello"][..]),
        ("stream", &streamed[..]),
    ];
    for (name, flags) in cases {
        let (data, work) = scratch(&format!("warns-{name}"));
        let out = run(&data, &work, flags);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{name}: {said}");
        assert!(said.contains(UNANSWERED), "{name}: {said}");
        assert!(said.contains("run with --yes"), "{name}: {said}");
    }
}

#[test]
fn a_run_in_another_mode_or_asked_over_stdin_does_not_warn() {
    let asked = [
        "-p",
        "--input-format",
        "stream-json",
        "--output",
        "stream-json",
        "--permission-prompt-tool",
        "stdio",
    ];
    let cases = [
        ("yes", &["-p", "--yes", "hello"][..]),
        ("auto", &["-p", "--mode", "auto", "hello"][..]),
        ("plan", &["-p", "--plan", "hello"][..]),
        ("asked", &asked[..]),
    ];
    for (name, flags) in cases {
        let (data, work) = scratch(&format!("quiet-{name}"));
        let out = run(&data, &work, flags);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "{name}: {said}");
        assert!(!said.contains(UNANSWERED), "{name}: {said}");
    }
}

#[test]
fn a_wrong_rule_id_stops_a_print_run_before_it_starts() {
    let (data, work) = scratch("rules-wrong");
    for rules in ["-nope", "-approval", "+compute"] {
        let out = run_leaving(&data, &work, &["-p", "--output", "json", "hello"], rules);
        let said = String::from_utf8_lossy(&out.stderr);
        assert_eq!(out.status.code(), Some(1), "{rules}: {said}");
        assert!(said.contains("LETRONNA_PROMPT_RULES"), "{rules}: {said}");
        assert!(out.stdout.is_empty(), "{rules}");
    }
}

#[test]
fn a_print_run_reports_the_rules_it_left_out() {
    let (data, work) = scratch("rules-left-out");
    let flags = ["-p", "--output", "json", "hello"];
    let told = |rules: &str| -> serde_json::Value {
        let out = run_leaving(&data, &work, &flags, rules);
        assert!(out.status.success(), "{rules}");
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let all = told("");
    let fewer = told("-compute, -literals");
    assert_eq!(all["prompt"]["dropped"], json!([]));
    assert_eq!(fewer["prompt"]["dropped"], json!(["compute", "literals"]));
    assert_ne!(all["prompt"]["rules_sha"], fewer["prompt"]["rules_sha"]);
}
