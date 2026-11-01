//! Sessions that work on with the terminal closed: `letronna --bg`, the
//! terminal's `/background`, and `letronna sessions`, `attach`, `logs` and
//! `stop`, which look in on them.
//!
//! A run in the background is this program again, started with
//! `--background <token>` as a `-p` run in a process of its own, out of the
//! terminal's session so that closing the terminal takes nothing with it. It
//! runs in the mode it was started in; a call that needs a yes waits for
//! `attach` to answer it, and the desktop says so meanwhile. Starting one
//! waits until it says which session it is, so its id can be given, or why
//! it could not start.

use std::io::Write;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use letronna_core::background::{self, Answer, Asking, Record, Run, State, Waiting};
use letronna_core::cancel::Cancel;
use letronna_core::driver::{Outcome, Report, Status};
use letronna_core::notify::{self, Note, Why};
use letronna_core::{store, tools};

use crate::args::Runs;
use crate::session::{self, Runner};
use crate::{banner, input, render};

/// How long a start waits for the run to say which session it is: past the
/// MCP servers' start, which it waits on first.
const STARTING: Duration = Duration::from_secs(30);

/// How often a start looks for that.
const STARTING_GAP: Duration = Duration::from_millis(100);

/// How often `attach` looks for what the run said and asks: about as often
/// as a person reads a line.
const FOLLOW: Duration = Duration::from_millis(500);

/// How long `stop` waits for the run to end what it started.
const STOPPING: Duration = Duration::from_secs(15);

/// What a turn the user stopped is told when it carries on in the
/// background.
const CARRY_ON: &str = "Carry on from where you were stopped.";

/// Start `letronna` with `args`, which are the command line's without
/// `--bg`, working in the background, and say how to reach it.
pub fn start(args: Vec<String>) -> i32 {
    match spawn(args) {
        Ok(record) => {
            say_started(&record);
            0
        }
        Err(why) => {
            eprintln!("{}", render::red(&why));
            1
        }
    }
}

/// The run `/background` handed this terminal's session to, said on the way
/// out on the screen the terminal is given back.
static HANDED: std::sync::OnceLock<Record> = std::sync::OnceLock::new();

/// The run `/background` started, if it did.
pub fn handed() -> Option<&'static Record> {
    HANDED.get()
}

/// Hand the session at `session` to a run in the background, to do `rest`,
/// or to carry on a turn the user stopped when `rest` is empty, in the mode
/// it is in now. One nothing was said in yet is not on disk, so the run
/// starts a session of its own in the same folder instead.
pub fn hand_over(runner: &mut Runner, session: usize, rest: &str) -> Result<u64, String> {
    let conversation = &runner.store.conversations[session];
    let what = match rest.trim() {
        "" if runner.stopped && conversation.length() > 0 => CARRY_ON.to_owned(),
        "" => {
            return Err(
                "Say what it should do: /background <what to do>. A turn you stopped carries on with /background alone."
                    .into(),
            );
        }
        what => what.to_owned(),
    };
    let mut args = Vec::new();
    if conversation.length() > 0 {
        args.extend(["--resume".to_owned(), conversation.id.to_string()]);
    }
    runner.persist();
    args.extend([
        "--mode".to_owned(),
        runner.current_mode().name().to_owned(),
        what,
    ]);
    let record = spawn(args)?;
    let id = record.session;
    let _ = HANDED.set(record);
    Ok(id)
}

/// Start the run and wait for it to say which session it is.
fn spawn(mut args: Vec<String>) -> Result<Record, String> {
    let token = token();
    args.push("--background".into());
    args.push(token.clone());
    let dir = background::dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("Cannot make {}: {e}", dir.display()))?;
    // What it says before it begins, such as why it cannot, is read from
    // here; once it has begun, its record says the rest.
    let said = dir.join(format!("start-{token}.out"));
    let out = std::fs::File::create(&said).map_err(|e| format!("Cannot start it: {e}"))?;
    let err = out
        .try_clone()
        .map_err(|e| format!("Cannot start it: {e}"))?;
    let program = std::env::current_exe().map_err(|e| format!("Cannot start it: {e}"))?;
    let mut command = std::process::Command::new(program);
    command
        .args(&args)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    detach(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("Cannot start it: {e}"))?;
    let began = Instant::now();
    loop {
        if let Some(record) = background::list().into_iter().find(|r| r.token == token) {
            let _ = std::fs::remove_file(&said);
            return Ok(record);
        }
        if let Ok(Some(status)) = child.try_wait() {
            let why = std::fs::read_to_string(&said).unwrap_or_default();
            let _ = std::fs::remove_file(&said);
            let why = render::plain(why.trim());
            return Err(match why.is_empty() {
                true => format!("It stopped before it began ({status})."),
                false => why,
            });
        }
        if began.elapsed() > STARTING {
            return Err(
                "It has not begun after 30 seconds; letronna sessions lists it once it does."
                    .into(),
            );
        }
        std::thread::sleep(STARTING_GAP);
    }
}

/// A token no other start uses at the same moment.
fn token() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| since.as_nanos());
    format!("{nanos:x}-{:x}", std::process::id())
}

/// Start `command` out of the terminal's session, so a closing terminal's
/// hangup and a Ctrl-C typed there never reach it.
#[cfg(unix)]
fn detach(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: setsid touches no memory and is safe to call between fork and
    // exec.
    unsafe {
        command.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
}

/// Start `command` with no console of its own and out of the terminal's
/// group, so closing the terminal or a Ctrl-C there never reaches it.
#[cfg(windows)]
fn detach(command: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Say that the run of `record` began, and how to reach it.
pub fn say_started(record: &Record) {
    let id = record.session;
    render::said(
        &render::blue("●"),
        &render::bold(&format!("Working in the background as session {id}")),
    );
    render::line(&render::dim(&format!(
        "letronna attach {id} to watch it and answer what it asks · letronna logs {id} · letronna stop {id}"
    )));
}

/// Why session `id` cannot be opened here: a run in the background has it.
pub fn busy(id: u64) -> String {
    format!(
        "Session {id} is working in the background: letronna attach {id} to watch it, or letronna stop {id}."
    )
}

/// Take on session `session`'s run in this process, as `--background`
/// starts it with `token`, to do `prompt`.
pub fn claim(
    runner: &Runner,
    session: usize,
    prompt: &str,
    token: &str,
) -> Result<Arc<Run>, String> {
    let conversation = &runner.store.conversations[session];
    let title = match conversation.title.trim() {
        "" => session::first_line(prompt),
        title => title.to_owned(),
    };
    Run::claim(Record {
        session: conversation.id,
        token: token.to_owned(),
        title,
        folder: conversation.workspace.clone(),
        mode: runner.mode.name().to_owned(),
        ..Default::default()
    })
}

/// Have `run` answer its calls' cards through `attach` and hear a stop
/// through `cancel`, while the desktop is told when a card waits. With
/// `lines` (stream-json), each card and its answer are also said on stdout,
/// so whoever started the run (Letronna Cloud) can put the card to its user
/// and write the answer where the run waits for it.
pub fn serve(run: &Arc<Run>, title: &str, cancel: Cancel, lines: bool) {
    let (id, title) = (run.session(), title.to_owned());
    run.serve(
        letronna_core::approvals::serve(),
        cancel.clone(),
        move |waiting| {
            if lines {
                let line = waiting_line(id, &waiting);
                let _ = writeln!(std::io::stdout().lock(), "{line}");
            }
            if let Waiting::Asked(asking) = waiting {
                call_back(id, &title, Why::Waiting(&asking.call, None));
            }
        },
    );
    run.watch(cancel);
}

// What a card says on the stream is cut to this: its preview can be a
// whole file's change, and the line goes on to a phone.
const CARD_CHARS: usize = 1200;

/// A card going up or answered, as a stream-json line. `session` names the
/// run's answer file, `<data>/background/<session>.answer`.
fn waiting_line(session: u64, waiting: &Waiting) -> serde_json::Value {
    match waiting {
        Waiting::Asked(asking) => serde_json::json!({
            "type": "ask",
            "session": session,
            "call": asking.call.id,
            "tool": asking.call.name,
            "preview": asking.preview.chars().take(CARD_CHARS).collect::<String>(),
            "why": asking.why,
            "for_run": asking.for_run,
        }),
        Waiting::Answered(call) => {
            serde_json::json!({ "type": "answered", "session": session, "call": call })
        }
    }
}

/// Keep `run`'s record saying what `status` says it is on, until the
/// sender this gives is dropped.
pub fn follow(run: &Arc<Run>, status: Arc<Status>) -> mpsc::Sender<()> {
    let (going, ended) = mpsc::channel::<()>();
    let run = run.clone();
    std::thread::spawn(move || {
        while let Err(mpsc::RecvTimeoutError::Timeout) = ended.recv_timeout(FOLLOW) {
            let doing = status.doing.lock().map(|d| d.clone()).unwrap_or_default();
            run.doing(&doing);
        }
    });
    going
}

/// Say on `run`'s record how `report` ended, and call the user back.
pub fn end(run: &Run, title: &str, report: &Report) {
    let (state, said) = match &report.status {
        Outcome::Done => (State::Done, report.text.clone()),
        Outcome::Stopped => (State::Stopped, report.text.clone()),
        Outcome::RoundLimit => (
            State::Stopped,
            format!("It used every round it was given. {}", report.text),
        ),
        Outcome::OverBudget => (
            State::Stopped,
            format!("It spent its budget. {}", report.text),
        ),
        Outcome::Failed(why) => (State::Failed, why.clone()),
    };
    run.end(state, &said);
    let why = match state {
        State::Failed => Why::Failed(&said),
        _ => Why::Finished(&said),
    };
    call_back(run.session(), title, why);
}

/// Tell the desktop about session `session`, called `title`, when the user
/// wants notifications and no terminal watches its run already.
fn call_back(session: u64, title: &str, why: Why) {
    if !store::current_settings().notifications || background::watched(session) {
        return;
    }
    desktop(&notify::note(session, title, why));
}

/// The notification centre, through AppleScript, which a program with no
/// app bundle can reach. The words go as arguments, never into the script.
#[cfg(target_os = "macos")]
fn desktop(note: &Note) {
    let _ = std::process::Command::new("osascript")
        .args([
            "-e",
            "on run argv",
            "-e",
            "display notification (item 2 of argv) with title (item 1 of argv)",
            "-e",
            "end run",
            &note.title,
            &note.body,
        ])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// notify-send, which every Linux desktop that shows notifications has.
#[cfg(all(unix, not(target_os = "macos")))]
fn desktop(note: &Note) {
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=Letronna", &note.title, &note.body])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

/// Windows shows a notification only for an app it knows, which a run with
/// no window is not; `letronna sessions` says what waits instead.
#[cfg(windows)]
fn desktop(_: &Note) {}

/// Do what `letronna sessions`, `attach`, `logs` or `stop` asks.
pub fn look(runs: &Runs) -> i32 {
    match runs {
        Runs::List => list(),
        Runs::Attach(id) => attach(*id),
        Runs::Logs(id) => logs(*id),
        Runs::Stop(id) => stop(*id),
    }
}

/// `letronna sessions`: each run going here and those ended in the last day.
fn list() -> i32 {
    let records = background::list();
    if records.is_empty() {
        render::line(&render::dim(
            "Nothing is working in the background. letronna --bg \"task\" starts something.",
        ));
        return 0;
    }
    let now = letronna_core::chat::now();
    render::said(&render::blue("●"), &render::bold("In the background"));
    for record in &records {
        render::line(&row(record, now));
    }
    render::line(&render::dim(
        "letronna attach <id> to watch one and answer it · letronna logs <id> · letronna stop <id>",
    ));
    0
}

/// One line of the list: the session, where it stands, what it is and is on.
fn row(record: &Record, now: u64) -> String {
    let state = match record.state {
        State::Waiting => render::amber(record.state.name()),
        State::Failed | State::Gone => render::red(record.state.name()),
        State::Working => render::green(record.state.name()),
        _ => render::dim(record.state.name()),
    };
    let on = match (&record.asking, record.state) {
        (Some(asking), _) => format!("wants to {}", wants(asking)),
        (None, State::Working) => record.doing.clone(),
        _ => record.said.lines().next().unwrap_or_default().to_owned(),
    };
    let folder = (record.folder.as_ref())
        .and_then(|folder| folder.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no project".into());
    let at = record.ended.unwrap_or(record.started);
    let mut line = format!(
        "{:>6}  {state}  {}",
        record.session,
        render::one_line(&record.title)
    );
    for part in [on.as_str(), &folder, &age(now.saturating_sub(at))] {
        let part = render::one_line(part);
        if !part.is_empty() {
            line.push_str(&render::dim(&format!(" · {part}")));
        }
    }
    line
}

/// What the call `asking` waits on would do, as its card says it, but for
/// a command, which is named: away from the card nothing else shows it.
fn wants(asking: &Asking) -> String {
    let call = &asking.call;
    let summary = tools::summary(&call.name, &call.arguments);
    match call.name.as_str() {
        "run_command" => format!("run `{}`", render::one_line(&summary)),
        name => session::intent(name, &summary),
    }
}

/// `seconds` as the list says how long ago: in seconds, minutes, hours or
/// days, whichever is the largest whole one.
fn age(seconds: u64) -> String {
    match seconds {
        0..60 => format!("{seconds}s"),
        60..3_600 => format!("{}m", seconds / 60),
        3_600..86_400 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

/// Session `id`'s conversation as its file holds it now.
fn read(id: u64) -> Option<store::Conversation> {
    let mut listing = store::load_listing();
    let index = listing.conversations.iter().position(|c| c.id == id)?;
    let mut conversation = listing.conversations.swap_remove(index);
    store::fill(&mut conversation);
    Some(conversation)
}

/// `letronna logs <id>`: what session `id` has said and done so far.
fn logs(id: u64) -> i32 {
    let Some(conversation) = read(id) else {
        eprintln!("{}", render::red(&format!("No session with the id {id}.")));
        return 1;
    };
    banner::replay_from(&conversation.messages, 0);
    if let Some(record) = background::get(id) {
        render::blank();
        render::line(&render::dim(&status_line(&record)));
    }
    0
}

/// Where `record`'s run stands, in a line.
fn status_line(record: &Record) -> String {
    let id = record.session;
    match record.state {
        State::Waiting => match &record.asking {
            Some(asking) => format!(
                "Waiting for you: it wants to {} · letronna attach {id} to answer",
                wants(asking)
            ),
            None => format!("Waiting for you · letronna attach {id} to answer"),
        },
        State::Working => format!("Still working · letronna attach {id} to watch"),
        state => format!(
            "{} · letronna -r {id} to carry on with it here",
            capital(state.name())
        ),
    }
}

fn capital(text: &str) -> String {
    let mut chars = text.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

/// `letronna attach <id>`: what session `id` said so far, then each message
/// as it lands and each card as it comes up, until the run ends or the user
/// leaves it working.
fn attach(id: u64) -> i32 {
    let Some(record) = background::get(id) else {
        eprintln!(
            "{}",
            render::red(&format!(
                "Session {id} has not worked in the background; letronna -r {id} opens it."
            ))
        );
        return 1;
    };
    let _watching = background::watching(id);
    let path = store::sessions_dir().join(format!("{id}.jsonl"));
    let mut shown = 0;
    let mut size = None;
    let mut show_new = |size: &mut Option<u64>| {
        let now = std::fs::metadata(&path).map(|meta| meta.len()).ok();
        if now == *size {
            return;
        }
        *size = now;
        if let Some(conversation) = read(id) {
            banner::replay_from(&conversation.messages, shown);
            shown = conversation.messages.len();
        }
    };
    show_new(&mut size);
    if !record.state.running() {
        render::blank();
        render::line(&render::dim(&status_line(&record)));
        return 0;
    }
    render::blank();
    render::line(&render::dim(&format!(
        "Watching session {id} · ctrl+c leaves it working"
    )));
    let left = Cancel::default();
    letronna_core::cancel::on_interrupt(left.clone());
    let mut carded = String::new();
    let mut doing = String::new();
    while left.sleep(FOLLOW) {
        show_new(&mut size);
        let Some(record) = background::get(id) else {
            break;
        };
        if !record.state.running() {
            show_new(&mut None);
            render::blank();
            render::line(&render::dim(&status_line(&record)));
            return 0;
        }
        if record.state == State::Working && record.doing != doing {
            doing.clone_from(&record.doing);
            if !doing.is_empty() {
                render::line(&render::dim(&format!("· {}", render::one_line(&doing))));
            }
        }
        let Some(asking) = record.asking.filter(|asking| asking.call.id != carded) else {
            continue;
        };
        carded.clone_from(&asking.call.id);
        let Some(answer) = card(&asking, record.folder.as_deref()) else {
            // Left unanswered, it waits for the next attach.
            break;
        };
        if let Err(why) = background::answer(id, &asking.call.id, answer) {
            render::line(&render::dim(&why));
        }
    }
    render::blank();
    render::line(&render::dim(&format!(
        "Left session {id} working · letronna attach {id} to come back"
    )));
    0
}

/// Show the card for the call `asking` waits on, in `folder`, and give what
/// the user answered, or `None` when they left it.
fn card(asking: &Asking, folder: Option<&Path>) -> Option<Answer> {
    let call = &asking.call;
    let mut context = tools::ToolContext::default();
    context.workspace = folder.map(Path::to_path_buf);
    render::blank();
    // A change to a file is shown as a diff, anything else as its preview.
    match crate::diff::of(&call.name, &call.arguments, &context) {
        Some(rows) => rows.iter().for_each(|row| render::out(row, true)),
        None => (asking.preview.lines().take(PREVIEW_LINES))
            .for_each(|line| render::line(&render::dim(line))),
    }
    if !asking.why.is_empty() {
        render::line(&render::dim(&asking.why));
    }
    render::line(&render::bold(&format!(
        "Letronna wants to {}",
        wants(asking)
    )));
    let mut options: Vec<String> = Vec::new();
    let mut answers: Vec<Answer> = Vec::new();
    if asking.places.is_empty() {
        options.push("Yes".into());
        answers.push(Answer::Once);
    }
    for (place, label) in asking.places.iter().enumerate() {
        options.push(label.clone());
        answers.push(Answer::Keep { place });
    }
    if let Some(rule) = &asking.for_run {
        options.push(format!("Yes, and stop asking for {rule} in this run"));
        answers.push(Answer::ForRun);
    }
    options.push("No, tell it why not".into());
    answers.push(Answer::Deny { why: String::new() });
    options.push("No, and stop it".into());
    answers.push(Answer::Stop);
    options.push("Not now: leave it waiting".into());
    let shown: Vec<&str> = options.iter().map(String::as_str).collect();
    let picked = input::menu(&shown, 0)?;
    let answer = answers.get(picked)?.clone();
    if let Answer::Deny { .. } = answer {
        // Left at the question, as a closed terminal leaves it, it waits.
        let why = input::read("  Why not? ")?;
        return Some(Answer::Deny { why });
    }
    Some(answer)
}

/// Lines of a call's preview a card shows, as `-p`'s card does.
const PREVIEW_LINES: usize = 12;

/// `letronna stop <id>`: ask session `id`'s run to stop, and wait for it to.
fn stop(id: u64) -> i32 {
    if let Err(why) = background::stop(id) {
        eprintln!("{}", render::red(&why));
        return 1;
    }
    let began = Instant::now();
    while background::running(id) {
        if began.elapsed() > STOPPING {
            eprintln!(
                "{}",
                render::red(&format!(
                    "Session {id} was asked to stop and has not yet; letronna sessions shows when it has."
                ))
            );
            return 1;
        }
        std::thread::sleep(STARTING_GAP);
    }
    render::line(&render::dim(&format!(
        "Stopped session {id} · letronna -r {id} to carry on with it here"
    )));
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_says_how_long_ago_in_its_largest_whole_unit() {
        assert_eq!(age(0), "0s");
        assert_eq!(age(59), "59s");
        assert_eq!(age(60), "1m");
        assert_eq!(age(3_599), "59m");
        assert_eq!(age(7_200), "2h");
        assert_eq!(age(90_000), "1d");
    }

    #[test]
    fn a_card_and_its_answer_are_said_on_the_stream_naming_the_answer_file() {
        let asking = Asking {
            call: letronna_core::chat::ToolCall {
                id: "call_7".into(),
                name: "run_command".into(),
                arguments: r#"{"command":"rm -rf build"}"#.into(),
            },
            preview: "rm -rf build".repeat(200),
            why: "It deletes files.".into(),
            ..Default::default()
        };
        let asked = waiting_line(42, &Waiting::Asked(&asking));
        assert_eq!(asked["type"], "ask");
        assert_eq!(asked["session"], 42);
        assert_eq!(asked["call"], "call_7");
        assert_eq!(asked["tool"], "run_command");
        assert_eq!(asked["why"], "It deletes files.");
        assert_eq!(
            asked["preview"].as_str().unwrap().chars().count(),
            CARD_CHARS
        );
        let answered = waiting_line(42, &Waiting::Answered("call_7"));
        assert_eq!(
            answered,
            serde_json::json!({ "type": "answered", "session": 42, "call": "call_7" })
        );
    }

    #[test]
    fn a_waiting_run_is_listed_by_what_it_wants_to_do() {
        let record = Record {
            session: 42,
            title: "fix the flaky test".into(),
            state: State::Waiting,
            started: 100,
            asking: Some(Asking {
                call: letronna_core::chat::ToolCall {
                    id: "c".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"src/lib.rs","content":"x"}"#.into(),
                },
                ..Default::default()
            }),
            ..Default::default()
        };
        let shown = render::plain(&row(&record, 400));
        assert!(shown.contains("42"), "{shown}");
        assert!(shown.contains("waiting for you"), "{shown}");
        assert!(shown.contains("wants to write src/lib.rs"), "{shown}");
        assert!(shown.contains("5m"), "{shown}");
        // A command is named, since nothing else away from its card shows it.
        let mut asked = record.clone();
        if let Some(asking) = &mut asked.asking {
            asking.call.name = "run_command".into();
            asking.call.arguments = r#"{"command":"cargo test"}"#.into();
        }
        let shown = render::plain(&row(&asked, 400));
        assert!(shown.contains("wants to run `cargo test`"), "{shown}");
    }

    #[test]
    fn an_ended_run_points_back_to_the_session() {
        let record = Record {
            session: 7,
            state: State::Done,
            ..Default::default()
        };
        assert_eq!(
            status_line(&record),
            "Done · letronna -r 7 to carry on with it here"
        );
    }
}
