//! `-p --input-format stream-json`: another program drives the run over stdin.
//!
//! Each line on stdin is one JSON object in the shapes of the stream-json
//! control protocol other agents' command lines speak. Only part of it is
//! understood (there is no `initialize` handshake, and results are Letronna's
//! own lines), so a program written for one of them needs changes to drive
//! this. A `user` line is a message, run as a turn of its own once the turns
//! before it have ended; a `control_request` for `interrupt` stops the turn
//! that was running when it was read; and with `--permission-prompt-tool
//! stdio` a call the user's own rules and mode leave open goes out as a
//! `can_use_tool` request and waits for the `control_response` that answers
//! it. What those rules and that mode settle is settled before the program is
//! asked, so its yes never undoes them. Every message ends with its own
//! `result` line on stdout, as `-p` alone does.
//!
//! Closing stdin lets the turns already sent run to their end and then exits,
//! the way a pipe of questions would expect. A line that is not UTF-8 JSON
//! with what it needs stops everything and exits 2: carrying on would act on
//! a script that has gone wrong, and a turn it meant to stop may be the next
//! to run. A line of a type Letronna does not know is left alone, and a
//! message it cannot send gets a failed result in its place.
//!
//! Stdin, the approval requests and the running turn each have a thread, and
//! all three feed one channel, so the loop here alone holds what is waiting
//! and nothing needs a lock.

use crate::args::Args;
use crate::session::Runner;
use letronna_core::approvals::{self, Decision, Request};
use letronna_core::budget::Budget;
use letronna_core::cancel::Cancel;
use letronna_core::chat::ToolCall;
use letronna_core::driver::Report;
use letronna_core::gate::{self, Gate, Grants, Mode};
use letronna_core::permissions::{self, Rule};
use letronna_core::tools;
use serde_json::{Value, json};
use std::collections::{HashMap, VecDeque};
use std::io::{BufRead, Read};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Sender};

/// The longest line read from stdin: 8 MiB holds a message with a large file
/// pasted in, and stops a writer that has gone wrong long before memory runs
/// out.
const MAX_LINE: usize = 8 << 20;

/// The user's own rules, as the terminal reads them.
type Rules = Result<Vec<Rule>, String>;

/// What the loop hears about, from whichever thread.
enum Event {
    /// A line from stdin, read while the turn with this number ran (0: none).
    Line(Line, u64),
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
    /// A message, as text, or why it cannot be sent.
    User(Result<String, String>),
    /// Stop the running turn, answering the request with this id.
    Interrupt(String),
    /// The answer to the approval request with this id.
    Answer(String, Decision),
    /// The program no longer waits on the approval request with this id.
    Cancel(String),
    /// A control request of a kind Letronna does not handle, by id and subtype.
    Other(String, String),
    /// A line Letronna leaves alone, with the note it gets on stderr, if any.
    Ignored(Option<String>),
}

/// The turn running now, as the loop needs to know it.
struct Running {
    /// Counted from 1 across the run, so an interrupt can name it.
    turn: u64,
    id: u64,
    before: usize,
    cancel: Cancel,
}

/// One line as `next_line` read it.
enum Got {
    /// The line, without its line end.
    Line(Vec<u8>),
    /// A line over `MAX_LINE`, skipped to its end.
    Long,
    /// Stdin has no more.
    End,
}

/// Runs the messages stdin sends, one turn each in the order sent, until
/// stdin closes, and gives the exit code: 0, 1 when a turn failed or stdout
/// closed, or 2 for a line that could not be read.
pub fn run(runner: &mut Runner, session: usize, args: &Args) -> i32 {
    let (events, heard) = mpsc::channel();
    let current = Arc::new(AtomicU64::new(0));
    read_stdin(events.clone(), current.clone());
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
    let mode = runner.mode;
    let rules = permissions::load(&permissions::user_file());
    // One for the whole run, as a script counts on it to cap what it spends.
    let budget = args.budget.map(|limit| Arc::new(Budget::new(limit, None)));
    let mut waiting: VecDeque<Result<String, String>> = VecDeque::new();
    let mut running: Option<Running> = None;
    let mut asked: HashMap<String, Request> = HashMap::new();
    let (mut open, mut bad, mut failed, mut lost) = (true, false, false, false);
    let (mut asks, mut turns) = (0u64, 0u64);
    loop {
        if running.is_none() {
            if let Some(message) = waiting.pop_front() {
                let begun = message.and_then(|text| {
                    if lost {
                        return Err("The session was changed elsewhere while a turn ran, \
                            so this message was not sent."
                            .into());
                    }
                    start(runner, session, &text, args, budget.clone(), &events)
                });
                match begun {
                    Ok(mut turn) => {
                        turns += 1;
                        turn.turn = turns;
                        current.store(turns, Ordering::SeqCst);
                        running = Some(turn);
                    }
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
            Event::Line(Line::User(message), _) => {
                if open {
                    waiting.push_back(message);
                }
            }
            Event::Line(Line::Interrupt(id), read_in) => {
                // Only the turn running when it was read: one sent as a turn
                // ended must not stop the next.
                if let Some(turn) = running.as_ref().filter(|turn| turn.turn == read_in) {
                    turn.cancel.set();
                }
                reply(&id, Ok(json!({})));
            }
            Event::Line(Line::Answer(id, decision), _) => match asked.remove(&id) {
                Some(request) => {
                    let decision = checked(decision, &request, mode, &rules);
                    request.answer(decision);
                }
                // An answer that crossed an interrupt on the way is harmless.
                None => eprintln!("No call is waiting on request {id}, so its answer was ignored."),
            },
            // Dropped, the request is declined, as nobody will answer it now.
            Event::Line(Line::Cancel(id), _) => drop(asked.remove(&id)),
            Event::Line(Line::Other(id, subtype), _) => {
                reply(
                    &id,
                    Err(format!("Letronna does not handle `{subtype}` requests.")),
                );
            }
            Event::Line(Line::Ignored(note), _) => {
                if let Some(note) = note {
                    eprintln!("{note}");
                }
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
            // The user's own gate first, as the terminal has it, `--yes`
            // (Bypass) included: what it runs or refuses is not the
            // program's to say.
            Event::Asked(request) => {
                let answerable = args.ask_stdio && open;
                match gated(&request.call, request.root, mode, &rules, answerable) {
                    gate::Outcome::Run => request.answer(Decision::Once),
                    gate::Outcome::Refuse(why) => request.answer(Decision::Deny(why)),
                    gate::Outcome::Ask if !answerable => {
                        crate::say_refused(&request.call.name, "");
                        request.answer(Decision::Deny(String::new()));
                    }
                    gate::Outcome::Ask => {
                        asks += 1;
                        let id = format!("letronna-{asks}");
                        if crate::emit(&can_use_tool(&id, &request)) {
                            asked.insert(id, request);
                            continue;
                        }
                        // Nobody can read the question, so nobody will answer
                        // it: the call is declined and the run ends.
                        eprintln!("Stdout was closed, so the run ends.");
                        drop(request);
                        if let Some(turn) = &running {
                            turn.cancel.set();
                        }
                        waiting.clear();
                        (open, failed) = (false, true);
                    }
                }
            }
            Event::Ended(report) => {
                asked.clear();
                current.store(0, Ordering::SeqCst);
                let Some(turn) = running.take() else {
                    continue;
                };
                let output = args.output;
                failed |= !crate::finish(runner, session, turn.id, turn.before, report, output);
                // A session deleted or cut short elsewhere meanwhile is not
                // taken back, and the next message would land on another.
                lost |= runner
                    .store
                    .conversations
                    .get(session)
                    .is_none_or(|conversation| conversation.id != turn.id);
            }
        }
    }
    if bad { 2 } else { i32::from(failed) }
}

/// What the user's own gate makes of `call` in session `root`, from their
/// rules and `mode` as the terminal decides it, where `answerable` says
/// whether a call left open can be put to someone.
fn gated(call: &ToolCall, root: u64, mode: Mode, rules: &Rules, answerable: bool) -> gate::Outcome {
    let mut ctx = tools::ToolContext::default();
    ctx.conversation = root;
    ctx.workspace = std::env::current_dir().ok();
    let grants = Grants::new();
    gate::decide(
        call,
        &Gate::for_terminal(&ctx, &grants, rules, mode, answerable),
    )
}

/// The program's `decision` on `request` as it may stand. Changed input runs
/// only as that tool's arguments and only if the user's gate would not refuse
/// it; the model's own input sent back unchanged is a plain yes.
fn checked(decision: Decision, request: &Request, mode: Mode, rules: &Rules) -> Decision {
    let Decision::Changed(arguments) = decision else {
        return decision;
    };
    let call = &request.call;
    let changed: Option<Value> = serde_json::from_str(&arguments).ok();
    if serde_json::from_str::<Value>(&call.arguments).ok() == changed {
        return Decision::Once;
    }
    if let Err(why) = fits(&call.name, changed.as_ref()) {
        return Decision::Declined(why);
    }
    let changed_call = ToolCall {
        arguments: arguments.clone(),
        ..call.clone()
    };
    match gated(&changed_call, request.root, mode, rules, true) {
        gate::Outcome::Refuse(why) => Decision::Deny(why),
        gate::Outcome::Run | gate::Outcome::Ask => Decision::Changed(arguments),
    }
}

/// Whether `arguments` can be `tool`'s: an object with every field it requires.
fn fits(tool: &str, arguments: Option<&Value>) -> Result<(), String> {
    let Some(given) = arguments.and_then(Value::as_object) else {
        return Err(format!("Its changed input is not arguments for `{tool}`."));
    };
    let Some(found) = tools::find(tool) else {
        return Err(format!("There is no tool called `{tool}`."));
    };
    let parameters = found.parameters();
    let required = parameters["required"].as_array().into_iter().flatten();
    match required
        .filter_map(Value::as_str)
        .find(|field| !given.contains_key(*field))
    {
        Some(field) => Err(format!("Its changed input for `{tool}` has no `{field}`.")),
        None => Ok(()),
    }
}

/// Starts `text` as a turn on a thread of its own, spending from `budget`,
/// which says on `events` when it has ended.
fn start(
    runner: &mut Runner,
    session: usize,
    text: &str,
    args: &Args,
    budget: Option<Arc<Budget>>,
    events: &Sender<Event>,
) -> Result<Running, String> {
    let (spec, _, before) = crate::begin(runner, session, text, args, budget)?;
    let turn = Running {
        turn: 0,
        id: spec.id,
        before,
        cancel: spec.cancel.clone(),
    };
    let (events, output) = (events.clone(), args.output);
    std::thread::spawn(move || {
        let report = unbroken(|| crate::run_following(spec, output));
        let _ = events.send(Event::Ended(report));
    });
    Ok(turn)
}

/// What `turn` reports, or a failure when it panics: the loop waits for every
/// turn to end, and the program for its result line.
fn unbroken(turn: impl FnOnce() -> Report) -> Report {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(turn))
        .unwrap_or_else(|_| crate::failed("The turn stopped on an internal error.".into()))
}

/// Reads stdin on a thread of its own, as `read_lines` does.
fn read_stdin(events: Sender<Event>, current: Arc<AtomicU64>) {
    std::thread::spawn(move || read_lines(std::io::stdin().lock(), &events, &current));
}

/// Reads `input` a line at a time onto `events`, each with the turn `current`
/// says was running as it was read, stopping at the first line that cannot
/// be read.
fn read_lines(mut input: impl BufRead, events: &Sender<Event>, current: &AtomicU64) {
    let mut first = true;
    loop {
        let got = next_line(&mut input);
        let first_line = std::mem::take(&mut first);
        let line = match got {
            Ok(Got::End) => break,
            Ok(Got::Long) => Ok(Line::User(Err(format!(
                "A line on stdin was over {} MiB, so it was skipped.",
                MAX_LINE >> 20
            )))),
            Ok(Got::Line(bytes)) => match text(bytes, first_line) {
                Ok(text) if text.trim().is_empty() => continue,
                Ok(text) => read_line(&text),
                Err(error) => Err(error),
            },
            Err(error) => Err(format!("Stdin could not be read: {error}")),
        };
        let event = match line {
            Ok(line) => Event::Line(line, current.load(Ordering::SeqCst)),
            Err(error) => Event::Bad(error),
        };
        let bad = matches!(event, Event::Bad(_));
        if events.send(event).is_err() || bad {
            return;
        }
    }
    let _ = events.send(Event::Closed);
}

/// The next line of `input`, without its line end (CRLF too), reading no
/// more than `MAX_LINE` of it into memory.
fn next_line(input: &mut impl BufRead) -> std::io::Result<Got> {
    let mut line = Vec::new();
    // One byte past the cap tells a line of just MAX_LINE from a longer one.
    let cap = MAX_LINE as u64 + 1;
    Read::take(&mut *input, cap).read_until(b'\n', &mut line)?;
    if line.is_empty() {
        return Ok(Got::End);
    }
    if line.last() == Some(&b'\n') {
        line.pop();
        if line.last() == Some(&b'\r') {
            line.pop();
        }
        return Ok(Got::Line(line));
    }
    if line.len() <= MAX_LINE {
        return Ok(Got::Line(line));
    }
    // The rest of it is passed over a buffer at a time, never held.
    loop {
        let buffer = input.fill_buf()?;
        if buffer.is_empty() {
            break;
        }
        let end = buffer.iter().position(|&byte| byte == b'\n');
        let used = end.map_or(buffer.len(), |end| end + 1);
        input.consume(used);
        if end.is_some() {
            break;
        }
    }
    Ok(Got::Long)
}

/// `bytes` as text, which has to be UTF-8; a byte order mark at the start of
/// the first line is dropped, as Windows editors write one.
fn text(bytes: Vec<u8>, first: bool) -> Result<String, String> {
    // Windows PowerShell pipes UTF-16, marked or not; a JSON line in UTF-8
    // has no zero byte in its first two.
    let marked = bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]);
    if first && (marked || bytes.iter().take(2).any(|&byte| byte == 0)) {
        return Err("Stdin is UTF-16, and it has to be UTF-8.".into());
    }
    let bom = if first && bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        3
    } else {
        0
    };
    String::from_utf8(bytes[bom..].to_vec())
        .map_err(|_| "A line on stdin is not UTF-8, and stdin has to be.".into())
}

/// What one line on stdin says, or why it cannot be read.
fn read_line(text: &str) -> Result<Line, String> {
    let value: Value = serde_json::from_str(text)
        .map_err(|error| format!("A line on stdin is not JSON ({error}): {text}"))?;
    match value["type"].as_str() {
        // One that cannot be sent still gets its result line, in its place.
        Some("user") => Ok(Line::User(said(&value["message"]))),
        Some("control_request") => {
            let id = request_id(&value["request_id"], "control_request")?;
            match value["request"]["subtype"].as_str() {
                Some("interrupt") => Ok(Line::Interrupt(id)),
                Some(subtype) => Ok(Line::Other(id, subtype.to_owned())),
                None => Err("A control_request needs a request.subtype.".into()),
            }
        }
        Some("control_response") => answer(&value["response"]),
        Some("control_cancel_request") => {
            request_id(&value["request_id"], "control_cancel_request").map(Line::Cancel)
        }
        Some("keep_alive") => Ok(Line::Ignored(None)),
        // Hosts send more kinds than these, and a run is not worth stopping
        // over one it can do without.
        Some(kind) => Ok(Line::Ignored(Some(format!(
            "Letronna does not handle `{kind}` lines, so one was ignored."
        )))),
        None => Err(format!(
            "A line on stdin needs a type, such as user or control_request: {text}"
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
        // A driver that could not decide has not said yes, and how it failed
        // is no reason to give the model.
        Some("error") => Decision::Declined(String::new()),
        Some("success") => {
            let inner = &response["response"];
            match inner["behavior"].as_str() {
                // The loop tells an echo of the model's input from a change.
                Some("allow") => match &inner["updatedInput"] {
                    Value::Null => Decision::Once,
                    changed => Decision::Changed(changed.to_string()),
                },
                Some("deny") => {
                    Decision::Declined(inner["message"].as_str().unwrap_or_default().into())
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
        assert_eq!(read_line(plain), Ok(Line::User(Ok("hello".into()))));
        let blocks = r#"{"type":"user","message":{"role":"user","content":[
            {"type":"text","text":"one"},{"type":"text","text":"two"}]}}"#;
        assert_eq!(
            read_line(&blocks.replace('\n', "")),
            Ok(Line::User(Ok("one\n\ntwo".into())))
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
                Decision::Declined("not today".into())
            ))
        );
        let changed = r#"{"type":"control_response","response":{"request_id":"letronna-3","subtype":"success","response":{"behavior":"allow","updatedInput":{"command":"ls"}}}}"#;
        assert_eq!(
            read_line(changed),
            Ok(Line::Answer(
                "letronna-3".into(),
                Decision::Changed(r#"{"command":"ls"}"#.into())
            ))
        );
        let cancel = r#"{"type":"control_cancel_request","request_id":"letronna-4"}"#;
        assert_eq!(read_line(cancel), Ok(Line::Cancel("letronna-4".into())));
    }

    #[test]
    fn lines_of_other_kinds_are_left_alone() {
        let quiet = read_line(r#"{"type":"keep_alive"}"#);
        assert_eq!(quiet, Ok(Line::Ignored(None)));
        let Ok(Line::Ignored(Some(note))) = read_line(r#"{"type":"system","subtype":"x"}"#) else {
            panic!("a line of another type was not left alone");
        };
        assert!(note.contains("`system`"), "{note}");
    }

    #[test]
    fn a_message_that_cannot_be_sent_is_refused_on_its_own() {
        let refused = |line: &str| match read_line(line) {
            Ok(Line::User(Err(why))) => why,
            other => panic!("{other:?}"),
        };
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
    }

    #[test]
    fn a_line_that_cannot_be_read_says_why() {
        let refused = |line: &str| read_line(line).unwrap_err();
        assert!(refused("hello").starts_with("A line on stdin is not JSON"));
        assert!(refused(r#"{"kind":"user"}"#).starts_with("A line on stdin needs a type"));
        assert_eq!(
            refused(r#"{"type":"control_request","request":{"subtype":"interrupt"}}"#),
            "A control_request needs a request_id, as text."
        );
        assert!(
            refused(r#"{"type":"control_response","response":{"request_id":"a","subtype":"success","response":{"behavior":"maybe"}}}"#)
                .ends_with("behavior, allow or deny.")
        );
    }

    /// What `read_lines` makes of `input`, read while turn 3 runs: each line,
    /// or why one could not be read.
    fn heard(input: &[u8]) -> Vec<Result<(Line, u64), String>> {
        let (events, heard) = mpsc::channel();
        read_lines(input, &events, &AtomicU64::new(3));
        drop(events);
        let lines = heard.into_iter().filter_map(|event| match event {
            Event::Line(line, turn) => Some(Ok((line, turn))),
            Event::Bad(why) => Some(Err(why)),
            _ => None,
        });
        lines.collect()
    }

    #[test]
    fn lines_carry_the_turn_that_was_running_as_they_were_read() {
        let stop =
            br#"{"type":"control_request","request_id":"r1","request":{"subtype":"interrupt"}}"#;
        assert_eq!(heard(stop), [Ok((Line::Interrupt("r1".into()), 3))]);
    }

    #[test]
    fn a_byte_order_mark_and_crlf_are_read_past_but_utf_16_is_refused() {
        let said = |text: &str| Ok((Line::User(Ok(text.into())), 3));
        let mut input = b"\xEF\xBB\xBF".to_vec();
        input.extend_from_slice(br#"{"type":"user","message":{"content":"one"}}"#);
        input.extend_from_slice(b"\r\n");
        input.extend_from_slice(br#"{"type":"user","message":{"content":"two"}}"#);
        assert_eq!(heard(&input), [said("one"), said("two")]);
        let utf16: Vec<u8> = "\u{feff}{\"type\":\"keep_alive\"}\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        let refused = || Err("Stdin is UTF-16, and it has to be UTF-8.".to_owned());
        assert_eq!(heard(&utf16), [refused()]);
        assert_eq!(heard(&utf16[2..]), [refused()], "unmarked");
    }

    #[test]
    fn a_line_over_the_cap_is_refused_and_the_next_one_read() {
        let mut input = vec![b'x'; MAX_LINE + 10];
        input.push(b'\n');
        input.extend_from_slice(br#"{"type":"keep_alive"}"#);
        let heard = heard(&input);
        let refused = "A line on stdin was over 8 MiB, so it was skipped.";
        assert_eq!(
            heard,
            [
                Ok((Line::User(Err(refused.into())), 3)),
                Ok((Line::Ignored(None), 3))
            ]
        );
    }

    #[test]
    fn a_turn_that_panics_still_ends_with_a_failure() {
        let report = unbroken(|| panic!("a bug in a tool"));
        let stopped = "The turn stopped on an internal error.";
        assert_eq!(
            report.status,
            letronna_core::driver::Outcome::Failed(stopped.into())
        );
    }

    #[test]
    fn changed_input_has_to_be_arguments_for_the_tool() {
        assert_eq!(fits("run_command", Some(&json!({"command": "ls"}))), Ok(()));
        let short = fits("run_command", Some(&json!({"cmd": "ls"})));
        assert!(short.is_err_and(|why| why.contains("`command`")));
        assert!(fits("run_command", Some(&json!("ls"))).is_err());
        assert!(fits("run_command", None).is_err());
    }
}
