//! The bench for what every session pays for, held to `bench/baseline.json`.
//!
//! It extends the many-agents bench in `driver.rs`, which holds the driver to
//! fixed budgets, with the paths that grow with the history: listing 500 and
//! then 5,000 sessions from their index, reading a 10,000-message session in,
//! searching all 5,000, placing the window over those 10,000 messages and
//! parsing a 10,000-event stream from a local server. Each is timed `RUNS`
//! times and the median held to this platform's baseline; the terminal's
//! start and memory are held to the same file by `crates/cli/tests/bench.rs`.
//! The sessions are written for real, some 90 MB of them, so the bench is
//! ignored in an ordinary run and cleans up after itself.

mod baseline;

use crate::chat::{self, Connection, Event, Message, ToolCall};
use crate::store::{self, Conversation, Store, Within};
use baseline::{check, median};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::Instant;

/// Messages in the long session: a long day's work with tools, which is
/// where reading in, searching and placing the window cost the most.
const LONG: usize = 10_000;
/// Messages in every other session: five turns of question, tool call,
/// result and answer.
const SHORT: usize = 20;
/// Events in the timed stream: a long reply, a word at a time.
const EVENTS: usize = 10_000;
/// Tokens the window is placed for: a common model's window today.
const CONTEXT: u64 = 200_000;
/// One session in this many mentions the word searched for with "build",
/// which every session mentions: a search with a common word and a rare one.
const RARE: u64 = 97;

/// Milliseconds since `started`.
fn ms(started: Instant) -> f64 {
    started.elapsed().as_secs_f64() * 1000.0
}

/// Message `i` of session `id`, turn by turn: a question, a reply that
/// reads a file, the file, and the answer.
fn said(id: u64, i: usize) -> Message {
    let call = format!("call-{i}");
    match i % 4 {
        0 => {
            let what = if id.is_multiple_of(RARE) {
                "octopus build"
            } else {
                "build"
            };
            Message {
                role: "user".into(),
                content: format!(
                    "Why does the {what} stop at step {i}? The log says the linker ran out of room again."
                ),
                ..Default::default()
            }
        }
        1 => Message {
            role: "assistant".into(),
            tool_calls: vec![ToolCall {
                id: call,
                name: "read_file".into(),
                arguments: format!("{{\"path\":\"src/step{i}.rs\"}}"),
            }],
            ..Default::default()
        },
        2 => Message {
            role: "tool".into(),
            tool_call_id: call,
            name: "read_file".into(),
            // Forty lines of a source file, some 1,800 characters.
            content: (0..40)
                .map(|n| format!("{n:>4}  let step_{n} = build(step_{n}, &config)?;\n"))
                .collect(),
            ..Default::default()
        },
        _ => Message {
            role: "assistant".into(),
            content: format!(
                "Step {i} links every crate at once. Splitting the build into two \
                 stages keeps the linker under its limit, and the second stage \
                 reuses what the first one cached, so it costs little."
            ),
            model: "bench-model".into(),
            tokens: 60,
            ..Default::default()
        },
    }
}

/// Writes sessions `ids`, each `SHORT` messages long but the first when
/// `long_first`, which is `LONG`.
fn write_sessions(ids: std::ops::RangeInclusive<u64>, long_first: bool) {
    let first = *ids.start();
    let conversations = ids
        .map(|id| {
            let length = if long_first && id == first {
                LONG
            } else {
                SHORT
            };
            Conversation {
                id,
                title: format!("Session {id} about the build"),
                updated: 1_700_000_000 + id,
                messages: (0..length).map(|i| said(id, i)).collect(),
                ..Default::default()
            }
        })
        .collect();
    let mut store = Store {
        conversations,
        ..Default::default()
    };
    store
        .save()
        .expect("the bench's sessions could not be written");
}

/// A local provider that answers every request with a stream of `EVENTS`
/// one-word deltas, sent as fast as the socket takes them, until the bench's
/// process ends: whatever it is asked, since a first call to an endpoint
/// also asks it about the model.
fn provider() -> String {
    let server = TcpListener::bind("127.0.0.1:0").expect("no local port for the bench");
    let endpoint = format!("http://{}/v1", server.local_addr().expect("a bound port"));
    let mut body: String = (0..EVENTS)
        .map(|i| format!("data: {{\"choices\":[{{\"delta\":{{\"content\":\"word{i} \"}}}}]}}\n\n"))
        .collect();
    body.push_str("data: [DONE]\n\n");
    std::thread::spawn(move || {
        for socket in server.incoming() {
            let mut socket = socket.expect("the bench's request");
            read_request(&mut socket);
            let head = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            socket.write_all(head.as_bytes()).expect("the reply's head");
            socket.write_all(body.as_bytes()).expect("the reply");
        }
    });
    endpoint
}

/// Reads a whole request off `socket`, its body too: closing on one not
/// read can reset the connection before the reply is.
fn read_request(socket: &mut std::net::TcpStream) {
    let (mut bytes, mut block) = (Vec::new(), [0; 8192]);
    loop {
        let n = socket.read(&mut block).expect("the bench's request");
        assert_ne!(n, 0, "the request ended early");
        bytes.extend_from_slice(&block[..n]);
        let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") else {
            continue;
        };
        let head = String::from_utf8_lossy(&bytes[..end]).to_lowercase();
        let length: usize = head
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0);
        if bytes.len() >= end + 4 + length {
            return;
        }
    }
}

/// Streams one reply from `endpoint` to its end, in milliseconds.
fn stream(endpoint: &str) -> f64 {
    let connection = Connection {
        endpoint: endpoint.into(),
        model: "bench-model".into(),
        key: "bench".into(),
        ..Default::default()
    };
    let started = Instant::now();
    let events = chat::start(connection, vec![said(1, 0)]);
    let mut text = 0;
    loop {
        match events.recv_blocking().expect("the stream ended unfinished") {
            Event::Delta(delta) => text += delta.len(),
            Event::Finished(Ok(())) => break,
            Event::Finished(Err(failure)) => panic!("the bench's stream failed: {failure:?}"),
            _ => {}
        }
    }
    let taken = ms(started);
    // "wordN " for every N below EVENTS: more than five characters each.
    assert!(text > EVENTS * 5, "only {text} characters came through");
    taken
}

#[test]
#[ignore = "bench, some seconds: cargo test -p letronna-core --release --locked -- --ignored the_core_keeps_to_its_baseline --nocapture"]
fn the_core_keeps_to_its_baseline() {
    let _turn = (store::tests::DATA_DIR.lock()).unwrap_or_else(|e| e.into_inner());
    let dir = std::env::temp_dir().join("letronna-bench");
    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::set_var("AGENT_DATA_DIR", &dir) };
    let mut measured = Vec::new();

    // Listed once first, as every start after the first finds the index.
    write_sessions(1..=500, true);
    assert_eq!(store::load_listing().conversations.len(), 500);
    let listing = || {
        let started = Instant::now();
        let listed = std::hint::black_box(store::load_listing());
        let taken = ms(started);
        // Letting go of 5,000 sessions is no part of listing them.
        drop(listed);
        taken
    };
    measured.push(("listing_500_ms", median(listing)));
    write_sessions(501..=5_000, false);
    assert_eq!(store::load_listing().conversations.len(), 5_000);
    measured.push(("listing_5000_ms", median(listing)));

    let hits = store::search("octopus build", &Within::default(), 20);
    assert!(hits.len() >= 10, "the search found {} hits", hits.len());
    measured.push((
        "search_5000_ms",
        median(|| {
            let started = Instant::now();
            std::hint::black_box(store::search("octopus build", &Within::default(), 20));
            ms(started)
        }),
    ));

    let mut long = Vec::new();
    measured.push((
        "load_10k_ms",
        median(|| {
            let mut open = Conversation {
                id: 1,
                counted: LONG,
                ..Default::default()
            };
            let started = Instant::now();
            store::fill(&mut open);
            let taken = ms(started);
            long = open.messages;
            taken
        }),
    ));
    assert_eq!(long.len(), LONG);
    let start = chat::window_start_within(CONTEXT, &long);
    assert!(start > 0 && start < LONG, "the window starts at {start}");
    measured.push((
        "window_start_10k_ms",
        median(|| {
            let started = Instant::now();
            std::hint::black_box(chat::window_start_within(CONTEXT, &long));
            ms(started)
        }),
    ));

    let endpoint = provider();
    measured.push(("stream_10k_events_ms", median(|| stream(&endpoint))));

    let _ = std::fs::remove_dir_all(&dir);
    unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    if let Err(slower) = check(&measured) {
        panic!("{slower}");
    }
}
