//! `-p --input-format stream-json`: another program drives the run over stdin.
//!
//! Each line on stdin is one JSON object in the shapes of the stream-json
//! control protocol other agents' command lines speak, so a program written
//! to drive one of them can drive this too. A `user` line is a message, run as a turn of its own once
//! the turns before it have ended; a `control_request` for `interrupt` stops
//! the running turn; and with `--permission-prompt-tool stdio` a call that
//! needs a yes goes out as a `can_use_tool` request and waits for the
//! `control_response` that answers it. Every turn ends with its own `result`
//! line on stdout, as `-p` alone does.
//!
//! Closing stdin lets the turns already sent run to their end and then exits,
//! the way a pipe of questions would expect. A line that cannot be read stops
//! everything and exits 2: carrying on would act on a script that has gone
//! wrong, and a turn it meant to stop may be the next to run.
//!
//! Stdin, the approval requests and the running turn each have a thread, and
//! all three feed one channel, so the loop here alone holds what is waiting
//! and nothing needs a lock.

use crate::args::Args;
use crate::session::Runner;
use letronna_core::approvals::{self, Decision, Request};
use letronna_core::cancel::Cancel;
use letronna_core::driver::Report;
use letronna_core::gate::Mode;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::BufRead;
use std::sync::mpsc::{self, Sender};

/// What the loop hears about, from whichever thread.
enum Event {
    /// A line from stdin, read.
    Line(Line),
    /// A line from stdin that could not be read, and why.
    Bad(String),
    /// Stdin was closed.
    Closed,
    /// A call in the running turn needs a yes.
    Asked(Request),
    /// The running turn ended.
    Ended(Report),
}

/// What one line on stdin says.
#[derive(Debug, PartialEq)]
enum Line {
    /// A message, as text.
    User(String),
    /// Stop the running turn, answering the request with this id.
    Interrupt(String),
    /// The answer to the approval request with this id.
    Answer(String, Decision),
    /// A control request of a kind Letronna does not handle, by id and subtype.
    Other(String, String),
}

/// The turn running now, as the loop needs to know it.
struct Running {
    id: u64,
    before: usize,
    cancel: Cancel,
}

/// Runs the messages stdin sends, one turn each in the order sent, until
/// stdin closes, and gives the exit code: 0, 1 when a turn failed, or 2 for
/// a line that could not be read.
pub fn run(runner: &mut Runner, session: usize, args: &Args) -> i32 {
    let (events, heard) = mpsc::channel();
    read_stdin(events.clone());
    // Served before any turn starts, since a request with nobody serving is
    // declined at once.
    let requests = approvals::serve();
    let forward = events.clone();
    std::thread::spawn(move || {
        for request in requests {
            if forward.send(Event::Asked(request)).is_err() {
                return;
            }
        }
    });
    let yes = runner.mode == Mode::Bypass;
    let mut waiting: VecDeque<String> = VecDeque::new();
    let mut running: Option<Running> = None;
    let mut asked: HashMap<String, Request> = HashMap::new();
    let (mut open, mut bad, mut failed, mut asks) = (true, false, false, 0u64);
    loop {
        if running.is_none() {
            if let Some(text) = waiting.pop_front() {
                match start(runner, session, &text, args, &events) {
                    Ok(turn) => running = Some(turn),
                    Err(error) => {
                        crate::show(args.output, &crate::failed(error));
                        failed = true;
                    }
                }
                continue;
            }
            if !open {
                break;
            }
        }
        let Ok(event) = heard.recv() else {
            break;
        };
        match event {
            Event::Line(Line::User(text)) => waiting.push_back(text),
            Event::Line(Line::Interrupt(id)) => {
                if let Some(turn) = &running {
                    turn.cancel.set();
                }
                reply(&id, Ok(json!({})));
            }
            Event::Line(Line::Answer(id, decision)) => match asked.remove(&id) {
                Some(request) => request.answer(decision),
                // An answer that crossed an interrupt on the way is harmless.
                None => eprintln!("No call is waiting on request {id}, so its answer was ignored."),
            },
            Event::Line(Line::Other(id, subtype)) => {
                reply(
                    &id,
                    Err(format!("Letronna does not handle `{subtype}` requests.")),
                );
            }
            Event::Bad(error) => {
                eprintln!("{error}");
                if let Some(turn) = &running {
                    turn.cancel.set();
                }
                waiting.clear();
                (open, bad) = (false, true);
            }
            // The calls waiting are dropped, which declines them: nobody is
            // left to answer.
            Event::Closed => {
                open = false;
                asked.clear();
            }
            Event::Asked(request) if yes || !args.ask_stdio || !open => {
                let decision = crate::unattended(yes, &request);
                request.answer(decision);
            }
            Event::Asked(request) => {
                asks += 1;
                let id = format!("letronna-{asks}");
                crate::emit(&can_use_tool(&id, &request));
                asked.insert(id, request);
            }
            Event::Ended(report) => {
                asked.clear();
                let Some(turn) = running.take() else {
                    continue;
                };
                let output = args.output;
                failed |= !crate::finish(runner, session, turn.id, turn.before, report, output);
            }
        }
    }
    if bad { 2 } else { i32::from(failed) }
}

/// Starts `text` as a turn on a thread of its own, which says on `events`
/// when it has ended.
fn start(
    runner: &mut Runner,
    session: usize,
    text: &str,
    args: &Args,
    events: &Sender<Event>,
) -> Result<Running, String> {
    let (spec, _, before) = crate::begin(runner, session, text, args)?;
    let turn = Running {
        id: spec.id,
        before,
        cancel: spec.cancel.clone(),
    };
    let (events, output) = (events.clone(), args.output);
    std::thread::spawn(move || {
        let report = crate::run_following(spec, output);
        let _ = events.send(Event::Ended(report));
    });
    Ok(turn)
}

/// Reads stdin a line at a time onto `events`, stopping at the first line
/// that cannot be read.
fn read_stdin(events: Sender<Event>) {
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let event = match line {
                Ok(line) if line.trim().is_empty() => continue,
                Ok(line) => read_line(&line).map_or_else(Event::Bad, Event::Line),
                Err(error) => Event::Bad(format!("Stdin could not be read: {error}")),
            };
            let bad = matches!(event, Event::Bad(_));
            if events.send(event).is_err() || bad {
                return;
            }
        }
        let _ = events.send(Event::Closed);
    });
}

/// What one line on stdin says, or why it cannot be read.
fn read_line(text: &str) -> Result<Line, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("A line on stdin is not JSON ({error}): {text}"))?;
    match value["type"].as_str() {
        Some("user") => said(&value["message"]).map(Line::User),
        Some("control_request") => {
            let id = request_id(&value["request_id"], "control_request")?;
            match value["request"]["subtype"].as_str() {
                Some("interrupt") => Ok(Line::Interrupt(id)),
                Some(subtype) => Ok(Line::Other(id, subtype.to_owned())),
                None => Err("A control_request needs a request.subtype.".into()),
            }
        }
        Some("control_response") => answer(&value["response"]),
        _ => Err(format!(
            "A line on stdin needs a type of user, control_request or control_response: {text}"
        )),
    }
}

/// The text of a user message, whose content is text or a list of text blocks.
fn said(message: &Value) -> Result<String, String> {
    if message["role"].as_str().is_some_and(|role| role != "user") {
        return Err("A user line's message needs the role user.".into());
    }
    let text = match &message["content"] {
        Value::String(text) => text.clone(),
        Value::Array(blocks) => {
            let texts: Option<Vec<&str>> = blocks
                .iter()
                .map(|block| {
                    (block["type"] == "text")
                        .then(|| block["text"].as_str())
                        .flatten()
                })
                .collect();
            let Some(texts) = texts else {
                return Err("A user message's content blocks can only be text.".into());
            };
            texts.join("\n\n")
        }
        _ => return Err("A user line needs message.content, as text or text blocks.".into()),
    };
    if text.trim().is_empty() {
        return Err("A user message needs some text.".into());
    }
    Ok(text)
}

/// The answer a `control_response` gives to one of Letronna's requests.
fn answer(response: &Value) -> Result<Line, String> {
    let id = request_id(&response["request_id"], "control_response")?;
    let decision = match response["subtype"].as_str() {
        // A driver that could not decide has not said yes.
        Some("error") => Decision::Deny(response["error"].as_str().unwrap_or_default().into()),
        Some("success") => {
            let inner = &response["response"];
            match inner["behavior"].as_str() {
                Some("allow") => Decision::Once,
                Some("deny") => {
                    Decision::Deny(inner["message"].as_str().unwrap_or_default().into())
                }
                _ => {
                    return Err(
                        "A control_response needs response.response.behavior, allow or deny."
                            .into(),
                    );
                }
            }
        }
        _ => return Err("A control_response needs response.subtype, success or error.".into()),
    };
    Ok(Line::Answer(id, decision))
}

/// A request id, which has to be text.
fn request_id(id: &Value, of: &str) -> Result<String, String> {
    match id.as_str() {
        Some(id) if !id.is_empty() => Ok(id.to_owned()),
        _ => Err(format!("A {of} needs a request_id, as text.")),
    }
}

/// The line asking whoever drives stdin whether `request`'s call may run.
fn can_use_tool(id: &str, request: &Request) -> Value {
    let call = &request.call;
    // Arguments that are not JSON still go out, as the text the model wrote.
    let input = serde_json::from_str(&call.arguments)
        .unwrap_or_else(|_| Value::String(call.arguments.clone()));
    json!({
        "type": "control_request",
        "request_id": id,
        "request": {
            "subtype": "can_use_tool",
            "tool_name": call.name,
            "input": input,
            "tool_use_id": call.id,
        },
    })
}

/// Answers the driver's control request `id`, with what came of it or why it failed.
fn reply(id: &str, outcome: Result<Value, String>) {
    let response = match outcome {
        Ok(response) => json!({"subtype": "success", "request_id": id, "response": response}),
        Err(error) => json!({"subtype": "error", "request_id": id, "error": error}),
    };
    crate::emit(&json!({"type": "control_response", "response": response}));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_line_gives_its_text_in_either_form() {
        let plain = r#"{"type":"user","message":{"role":"user","content":"hello"}}"#;
        assert_eq!(read_line(plain), Ok(Line::User("hello".into())));
        let blocks = r#"{"type":"user","message":{"role":"user","content":[
            {"type":"text","text":"one"},{"type":"text","text":"two"}]}}"#;
        assert_eq!(
            read_line(&blocks.replace('\n', "")),
            Ok(Line::User("one\n\ntwo".into()))
        );
    }

    #[test]
    fn control_lines_carry_their_request_ids() {
        let stop =
            r#"{"type":"control_request","request_id":"r1","request":{"subtype":"interrupt"}}"#;
        assert_eq!(read_line(stop), Ok(Line::Interrupt("r1".into())));
        let other =
            r#"{"type":"control_request","request_id":"r2","request":{"subtype":"initialize"}}"#;
        assert_eq!(
            read_line(other),
            Ok(Line::Other("r2".into(), "initialize".into()))
        );
        let allow = r#"{"type":"control_response","response":{"request_id":"letronna-1","subtype":"success","response":{"behavior":"allow"}}}"#;
        assert_eq!(
            read_line(allow),
            Ok(Line::Answer("letronna-1".into(), Decision::Once))
        );
        let deny = r#"{"type":"control_response","response":{"request_id":"letronna-2","subtype":"success","response":{"behavior":"deny","message":"not today"}}}"#;
        assert_eq!(
            read_line(deny),
            Ok(Line::Answer(
                "letronna-2".into(),
                Decision::Deny("not today".into())
            ))
        );
    }

    #[test]
    fn a_line_that_cannot_be_read_says_why() {
        let refused = |line: &str| read_line(line).unwrap_err();
        assert!(refused("hello").starts_with("A line on stdin is not JSON"));
        assert!(refused(r#"{"type":"system"}"#).starts_with("A line on stdin needs a type"));
        assert_eq!(
            refused(r#"{"type":"user","message":{"content":42}}"#),
            "A user line needs message.content, as text or text blocks."
        );
        assert_eq!(
            refused(r#"{"type":"user","message":{"content":[{"type":"image"}]}}"#),
            "A user message's content blocks can only be text."
        );
        assert_eq!(
            refused(r#"{"type":"user","message":{"role":"system","content":"hi"}}"#),
            "A user line's message needs the role user."
        );
        assert_eq!(
            refused(r#"{"type":"control_request","request":{"subtype":"interrupt"}}"#),
            "A control_request needs a request_id, as text."
        );
        assert!(
            refused(r#"{"type":"control_response","response":{"request_id":"a","subtype":"success","response":{"behavior":"maybe"}}}"#)
                .ends_with("behavior, allow or deny.")
        );
    }
}
