//! A run of tool calls told in one line: "Read 3 files · Edited main.rs ·
//! Ran 2 commands · 14s".
//!
//! The words come from each tool's own `mutates` and `machine` flags and the
//! arguments its call carried, not from a table of tool names, so a plugin's
//! or an MCP server's tool is told as well as a built-in one. The time comes
//! from when the replies either side of the run finished, which the session
//! already keeps; nothing is timed while the run goes.

use crate::chat::Message;
use crate::tools::{self, Tool};
use serde_json::Value;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

/// Calls in a row that fold into one line; fewer are already one line each.
const FEWEST: usize = 2;

/// A run of tool results in a session, by place: `first` and `last` are tool
/// results, and everything between is a tool result or a reply that only
/// asked for tools and shows nothing of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Run {
    pub first: usize,
    pub last: usize,
}

impl Run {
    /// Whether the message at `at` is inside the run.
    pub fn holds(&self, at: usize) -> bool {
        (self.first..=self.last).contains(&at)
    }
}

/// What a folded run says, in parts the window colours apart: what its
/// calls did, how many failed, and how long it took. Any may be empty.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Told {
    pub did: String,
    pub failed: String,
    pub took: String,
}

/// Whether a tool result failed, a command that exited non-zero included, as
/// opposed to being turned down by the user.
pub fn failed(message: &Message) -> bool {
    (message.is_error && !tools::is_declined(&message.content)) || exit_code(message).is_some()
}

/// The code a command's result says it exited with, when that was not 0.
// `run_command` then ends its output with an `[exit code N]` line, which only
// Letronna's own bracketed notes, from a hook or about a repeated call, follow.
pub fn exit_code(message: &Message) -> Option<i32> {
    const LINE: &str = "\n[exit code ";
    if message.name != "run_command" {
        return None;
    }
    let at = message.content.rfind(LINE)?;
    let (code, after) = message.content[at + LINE.len()..].split_once(']')?;
    let code: i32 = code.parse().ok()?;
    (code != 0 && (after.is_empty() || after.starts_with("\n\n["))).then_some(code)
}

/// Whether a command's result says it was still running at its limit and
/// carries on as a background job. It has no exit code yet, so it is neither
/// a failure nor proof that anything passed: a test suite moved this way
/// used to count as one that ran clean, and a run that had changed files
/// was let end with nothing checked.
pub fn moved(message: &Message) -> bool {
    message.name == "run_command"
        && (message.content.split_once(tools::SO_FAR))
            .is_some_and(|(said, _)| said.contains(tools::MOVED))
}

/// Whether a reply shows nothing between tool rows: it only asked for tools,
/// with no words, and its thinking is hidden or empty.
pub fn silent(message: &Message, thinking: bool) -> bool {
    message.role == "assistant"
        && message.content.is_empty()
        && !message.tool_calls.is_empty()
        && !(thinking && !message.reasoning.is_empty())
}

/// Whether a tool result may be folded away. A question put to the user and
/// the answer, a plan handed over, and a call the user turned down are each
/// something they said, and stay in sight.
fn folds(message: &Message) -> bool {
    message.role == "tool"
        && message.name != tools::ASK_USER
        && message.name != tools::EXIT_PLAN
        // A search is a card of its own, its sources a click away, which a
        // run's one line would hide.
        && message.name != crate::search::NAME
        && !(message.is_error && tools::is_declined(&message.content))
}

/// The runs worth folding among the messages at `places`, in order and by
/// their places in `messages`. `thinking` is whether replies' thinking is
/// shown, which makes a reply that only called tools a row of its own and
/// so ends a run.
// A range, so the window walks only the turns it draws, not the session.
pub fn find(messages: &[Message], places: Range<usize>, thinking: bool) -> Vec<Run> {
    let worth = |(run, calls): (Run, usize)| (calls >= FEWEST).then_some(run);
    let mut runs = Vec::new();
    let mut open: Option<(Run, usize)> = None;
    let within = messages.iter().enumerate().take(places.end);
    for (at, message) in within.skip(places.start) {
        if folds(message) {
            let (run, calls) = open.get_or_insert((
                Run {
                    first: at,
                    last: at,
                },
                0,
            ));
            run.last = at;
            *calls += 1;
        } else if !(open.is_some() && silent(message, thinking)) {
            runs.extend(open.take().and_then(worth));
        }
    }
    runs.extend(open.and_then(worth));
    runs
}

/// The run among `runs`, as `find` gives them, that holds the message at
/// `at`.
// A search, since the window asks for every row it draws each frame.
pub fn holding(runs: &[Run], at: usize) -> Option<Run> {
    let run = runs.get(runs.partition_point(|run| run.last < at))?;
    run.holds(at).then_some(*run)
}

/// Whether `run` may still grow: nothing but silent replies follow it, so
/// the turn in flight is still at it.
pub fn at_the_end(messages: &[Message], run: Run, thinking: bool) -> bool {
    messages
        .get(run.last + 1..)
        .is_some_and(|after| after.iter().all(|m| silent(m, thinking)))
}

/// What `run` of `messages` did, in one line, with how long it took once it
/// is over, telling its calls apart by the flags of `tools`.
// The caller looks the tools up once for every run it draws: each lookup
// checks the search settings on disk, and the window tells runs each frame.
pub fn tell(messages: &[Message], run: Run, tools: &[Arc<dyn Tool>]) -> Told {
    let mut told = tell_with(&messages[run.first..=run.last], |name| {
        let tool = tools.iter().find(|tool| tool.name() == name)?;
        Some((tool.mutates(), tool.machine()))
    });
    told.took = took(messages, run).map(duration).unwrap_or_default();
    told
}

/// What a tool call did, as the line counts it.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Read,
    Searched,
    Fetched,
    Edited,
    Ran,
    Other,
}

/// `tell` without the time, with each tool's `(mutates, machine)` flags
/// from `flags`, or `None` for a tool no longer there.
fn tell_with(messages: &[Message], flags: impl Fn(&str) -> Option<(bool, bool)>) -> Told {
    let mut kinds: Vec<(Kind, Vec<String>)> = Vec::new();
    let mut failed_calls = 0;
    for message in messages.iter().filter(|m| m.role == "tool") {
        if failed(message) {
            failed_calls += 1;
            continue;
        }
        let mut args: Value = serde_json::from_str(&message.args).unwrap_or(Value::Null);
        tools::rename_aliases(&message.name, &mut args);
        let (kind, what) = kind_of(message, &args, flags(&message.name));
        match kinds.iter_mut().find(|(k, _)| *k == kind) {
            Some((_, seen)) => seen.push(what),
            None => kinds.push((kind, vec![what])),
        }
    }
    kinds.sort_by_key(|(kind, _)| *kind);
    let words: Vec<String> = kinds
        .iter()
        .map(|(kind, what)| said(*kind, what, kinds.len() > 1))
        .collect();
    let failures = match failed_calls {
        0 => String::new(),
        1 => "1 step failed".to_owned(),
        n => format!("{n} steps failed"),
    };
    Told {
        did: words.join(" · "),
        failed: failures,
        took: String::new(),
    }
}

/// A call's kind, and what it was about: the file, the command, the page.
/// A call that changes things is an edit when it names a file, a command
/// when it carried one -- whatever files the command changed -- and an
/// edit again when it wrote a file; one that only looks reads a file or a
/// page or searches. One that stays off this computer is a step like any
/// other.
fn kind_of(message: &Message, args: &Value, flags: Option<(bool, bool)>) -> (Kind, String) {
    let text = |key: &str| args[key].as_str().map(str::to_owned);
    let path = text("path");
    let Some((mutates, true)) = flags else {
        return (Kind::Other, message.name.clone());
    };
    if mutates {
        if let Some(file) = path {
            return (Kind::Edited, file);
        }
        if let Some(command) = text("command") {
            return (Kind::Ran, command);
        }
        return match message.files.first() {
            Some(wrote) => (Kind::Edited, wrote.path.to_string_lossy().into_owned()),
            None => (Kind::Other, message.name.clone()),
        };
    }
    if let Some(url) = text("url") {
        (Kind::Fetched, url)
    } else if let Some(pattern) = text("pattern").or_else(|| text("query")) {
        (Kind::Searched, pattern)
    } else if let Some(file) = path {
        (Kind::Read, file)
    } else {
        (Kind::Other, message.name.clone())
    }
}

/// One kind's words: a single file by its name, else a count. `among` is
/// whether other kinds come before it, which makes other steps "more".
fn said(kind: Kind, what: &[String], among: bool) -> String {
    let mut files: Vec<&str> = what.iter().map(String::as_str).collect();
    files.sort_unstable();
    files.dedup();
    let one_file = || {
        let path = Path::new(files[0]);
        path.file_name().map_or_else(
            || files[0].to_owned(),
            |name| name.to_string_lossy().into_owned(),
        )
    };
    let n = what.len();
    match kind {
        Kind::Read if files.len() == 1 => format!("Read {}", one_file()),
        Kind::Read => format!("Read {} files", files.len()),
        Kind::Edited if files.len() == 1 => format!("Edited {}", one_file()),
        Kind::Edited => format!("Edited {} files", files.len()),
        Kind::Searched => counted("Searched", n, "once", "times"),
        Kind::Fetched => counted("Read", n, "1 page", "pages"),
        Kind::Ran => counted("Ran", n, "1 command", "commands"),
        Kind::Other if among => counted("", n, "1 more step", "more steps"),
        Kind::Other => counted("", n, "1 step", "steps"),
    }
}

/// `verb` and a count, as "Ran 1 command" or "Ran 3 commands".
fn counted(verb: &str, n: usize, one: &str, many: &str) -> String {
    let count = if n == 1 {
        one.to_owned()
    } else {
        format!("{n} {many}")
    };
    if verb.is_empty() {
        count
    } else {
        format!("{verb} {count}")
    }
}

/// Seconds from the reply that asked for the run's first call to the one
/// after its last result, or `None` while that reply has not come or a
/// session from before replies were timed says nothing.
fn took(messages: &[Message], run: Run) -> Option<u64> {
    let asked = messages[..run.first]
        .iter()
        .rev()
        .find(|m| m.role == "assistant")?
        .at;
    let after = messages
        .get(run.last + 1)
        .filter(|m| m.role == "assistant")?
        .at;
    (asked > 0 && after >= asked).then(|| after - asked)
}

/// A span of time as the line shows it: "14s", "2m 5s", "1h 3m".
fn duration(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::ToolCall;

    fn call(name: &str, args: Value) -> Message {
        Message {
            role: "tool".into(),
            name: name.into(),
            args: args.to_string(),
            content: "ok".into(),
            ..Default::default()
        }
    }

    fn broke(name: &str, args: Value) -> Message {
        Message {
            is_error: true,
            content: "No such file.".into(),
            ..call(name, args)
        }
    }

    fn asking(at: u64) -> Message {
        Message {
            role: "assistant".into(),
            at,
            tool_calls: vec![ToolCall::default()],
            ..Default::default()
        }
    }

    fn answer(at: u64) -> Message {
        Message {
            role: "assistant".into(),
            content: "Done.".into(),
            at,
            ..Default::default()
        }
    }

    /// The flags the built-in tools have, without looking them up, which
    /// reads the data folder.
    fn flags(name: &str) -> Option<(bool, bool)> {
        match name {
            "read_file" | "list_dir" | "search" | "web_fetch" => Some((false, true)),
            "edit_file" | "write_file" | "apply_patch" | "run_command" | "stop_job" => {
                Some((true, true))
            }
            "todo" => Some((false, false)),
            _ => None,
        }
    }

    #[test]
    fn a_run_reads_as_what_its_calls_did() {
        let run = [
            call("read_file", serde_json::json!({"path": "src/a.rs"})),
            call("read_file", serde_json::json!({"path": "src/b.rs"})),
            call("read_file", serde_json::json!({"path": "src/a.rs"})),
            call("read_file", serde_json::json!({"path": "src/c.rs"})),
            call(
                "edit_file",
                serde_json::json!({"path": "src/ui/composer.rs"}),
            ),
            call("run_command", serde_json::json!({"command": "cargo test"})),
            call("run_command", serde_json::json!({"command": "cargo build"})),
        ];
        let told = tell_with(&run, flags);
        assert_eq!(
            told.did,
            "Read 3 files · Edited composer.rs · Ran 2 commands"
        );
        assert!(told.failed.is_empty());
    }

    #[test]
    fn a_command_that_changed_files_still_reads_as_a_command() {
        let wrote = |name: &str, args: Value| Message {
            files: vec![crate::history::FileChange {
                path: "/p/src/b.rs".into(),
                ..Default::default()
            }],
            ..call(name, args)
        };
        let run = [
            wrote("run_command", serde_json::json!({"command": "cargo fmt"})),
            wrote("apply_patch", serde_json::json!({"patch": "..."})),
        ];
        assert_eq!(tell_with(&run, flags).did, "Edited b.rs · Ran 1 command");
    }

    #[test]
    fn failures_are_counted_apart_from_what_worked() {
        let run = [
            call("run_command", serde_json::json!({"command": "ls"})),
            broke("read_file", serde_json::json!({"path": "gone.rs"})),
            broke("run_command", serde_json::json!({"command": "make"})),
        ];
        let told = tell_with(&run, flags);
        assert_eq!(told.did, "Ran 1 command");
        assert_eq!(told.failed, "2 steps failed");
        let all_failed = tell_with(&run[1..], flags);
        assert_eq!(
            (all_failed.did.as_str(), all_failed.failed.as_str()),
            ("", "2 steps failed")
        );
    }

    #[test]
    fn a_command_that_exits_non_zero_counts_as_failed() {
        let ran = |content: &str| Message {
            content: content.into(),
            ..call("run_command", serde_json::json!({"command": "cargo test"}))
        };
        let run = [
            ran("test result: ok. 3 passed"),
            ran("error[E0425]: cannot find value `x`\n[exit code 101]"),
            ran("\n[exit code -1]"),
            ran(
                "boom\n[exit code 2]\n\n[You have made this exact call 3 times this turn \
                 and got the same result each time. Change your approach instead of \
                 repeating it.]",
            ),
        ];
        let told = tell_with(&run, flags);
        assert_eq!(told.did, "Ran 1 command");
        assert_eq!(told.failed, "3 steps failed");
        let codes: Vec<Option<i32>> = run.iter().map(exit_code).collect();
        assert_eq!(codes, [None, Some(101), Some(-1), Some(2)]);
        // Only the line that ends a command's output says how it exited.
        let said_earlier = ran("$ cat log\n[exit code 1]\nretried: ok");
        let read = Message {
            content: "last run:\n[exit code 1]".into(),
            ..call("read_file", serde_json::json!({"path": "log"}))
        };
        assert!(!failed(&said_earlier) && !failed(&read));
    }

    #[test]
    fn calls_without_a_file_or_command_are_steps() {
        let run = [
            call("todo", serde_json::json!({"items": []})),
            call("stop_job", serde_json::json!({"id": 3})),
            call("mcp__gone__thing", serde_json::json!({})),
        ];
        assert_eq!(tell_with(&run, flags).did, "3 steps");
        let mixed = [
            call("search", serde_json::json!({"pattern": "fn main"})),
            call(
                "web_fetch",
                serde_json::json!({"url": "https://example.com"}),
            ),
            call("todo", serde_json::json!({"items": []})),
        ];
        assert_eq!(
            tell_with(&mixed, flags).did,
            "Searched once · Read 1 page · 1 more step"
        );
    }

    #[test]
    fn a_run_spans_silent_replies_and_stops_at_what_the_user_said() {
        let declined = Message {
            is_error: true,
            content: tools::DECLINED.into(),
            ..call("run_command", serde_json::json!({"command": "rm -r x"}))
        };
        let messages = [
            asking(100),
            call("read_file", serde_json::json!({"path": "a"})),
            asking(105),
            call("read_file", serde_json::json!({"path": "b"})),
            declined,
            call("read_file", serde_json::json!({"path": "c"})),
            call(tools::ASK_USER, serde_json::json!({"question": "Go on?"})),
            call("read_file", serde_json::json!({"path": "d"})),
            call("read_file", serde_json::json!({"path": "e"})),
            answer(114),
        ];
        let runs = find(&messages, 0..messages.len(), false);
        assert_eq!(
            runs,
            vec![Run { first: 1, last: 3 }, Run { first: 7, last: 8 }]
        );
        // A turn's runs keep their places in the session, and one cut short
        // by the end of the range is counted on what lies inside it.
        assert_eq!(
            find(&messages, 5..10, false),
            vec![Run { first: 7, last: 8 }]
        );
        assert_eq!(find(&messages, 0..3, false), vec![]);
        let held: Vec<Option<Run>> = (0..messages.len()).map(|at| holding(&runs, at)).collect();
        let (early, late) = (Some(runs[0]), Some(runs[1]));
        assert_eq!(
            held,
            [
                None, early, early, early, None, None, None, late, late, None
            ]
        );
        // Shown thinking makes the reply between the calls a row of its own.
        let mut thought = messages.clone();
        thought[2].reasoning = "Now b.".into();
        assert_eq!(
            find(&thought, 0..thought.len(), true),
            vec![Run { first: 7, last: 8 }]
        );
    }

    #[test]
    fn a_run_takes_as_long_as_the_replies_either_side_say() {
        let messages = [
            asking(100),
            call("read_file", serde_json::json!({"path": "a"})),
            asking(106),
            call("read_file", serde_json::json!({"path": "b"})),
            answer(114),
        ];
        let run = Run { first: 1, last: 3 };
        assert_eq!(took(&messages, run), Some(14));
        assert!(!at_the_end(&messages, run, false));
        // Still going: no reply after it yet, so no time either.
        let going = &messages[..4];
        assert_eq!(took(going, run), None);
        assert!(at_the_end(going, run, false));
        // A session saved before replies were timed.
        let untimed = [
            asking(0),
            messages[1].clone(),
            messages[3].clone(),
            answer(9),
        ];
        assert_eq!(took(&untimed, Run { first: 1, last: 2 }), None);
    }

    /// A tool with the given flags and nothing else.
    struct Flagged(&'static str, bool, bool);

    impl Tool for Flagged {
        fn name(&self) -> &'static str {
            self.0
        }
        fn description(&self) -> &'static str {
            ""
        }
        fn parameters(&self) -> Value {
            Value::Null
        }
        fn run(&self, _: &Value, _: &tools::ToolContext) -> Result<String, String> {
            Ok(String::new())
        }
        fn mutates(&self) -> bool {
            self.1
        }
        fn machine(&self) -> bool {
            self.2
        }
    }

    #[test]
    fn a_run_is_told_from_the_tools_it_is_given() {
        let messages = [
            asking(100),
            call("read_file", serde_json::json!({"path": "a.rs"})),
            call("plugin_edit", serde_json::json!({"path": "b.rs"})),
            answer(114),
        ];
        let tools: Vec<Arc<dyn Tool>> = vec![
            Arc::new(Flagged("read_file", false, true)),
            Arc::new(Flagged("plugin_edit", true, true)),
        ];
        let told = tell(&messages, Run { first: 1, last: 2 }, &tools);
        assert_eq!(
            (told.did.as_str(), told.took.as_str()),
            ("Read a.rs · Edited b.rs", "14s")
        );
        // A tool not among them is a step, whatever the registry holds.
        let told = tell(&messages, Run { first: 1, last: 2 }, &tools[..1]);
        assert_eq!(told.did, "Read a.rs · 1 more step");
    }

    #[test]
    fn durations_read_in_the_largest_units_that_fit() {
        assert_eq!(duration(14), "14s");
        assert_eq!(duration(125), "2m 5s");
        assert_eq!(duration(3_780), "1h 3m");
    }

    #[test]
    fn a_web_search_stands_on_its_own_between_folded_calls() {
        let messages = [
            asking(1),
            call("read_file", serde_json::json!({"path": "a.rs"})),
            call("read_file", serde_json::json!({"path": "b.rs"})),
            call(
                crate::search::NAME,
                serde_json::json!({"query": "rust 2024 edition"}),
            ),
            call("read_file", serde_json::json!({"path": "c.rs"})),
            call("read_file", serde_json::json!({"path": "d.rs"})),
            answer(2),
        ];
        let runs = find(&messages, 0..messages.len(), false);
        assert_eq!(runs, [Run { first: 1, last: 2 }, Run { first: 4, last: 5 }]);
        assert_eq!(holding(&runs, 3), None);
    }
}
