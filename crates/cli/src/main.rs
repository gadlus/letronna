//! Letronna in the terminal.
//!
//! `letronna` on its own opens a conversation; `-p` asks one question and
//! leaves. Sessions are the same files the window uses, so a conversation
//! started here can be carried on there and the other way round.
//!
//! `-p` runs on core's driver rather than the conversation loop: nobody
//! watches it stream, so it waits on the run and says what came of it at the
//! end, as the answer alone or as JSON a script can read.

mod args;
mod banner;
mod bar;
mod commands;
mod diff;
mod doctor;
mod edit;
mod input;
mod install;
mod mcp;
mod render;
mod screen;
mod session;
mod setup;
mod spinner;
mod syntax;
mod title;

use args::{Args, HELP, Output, Start, parse};
use banner::{banner, folder_name, footer, replay};
use commands::{Flow, command, resume};
use letronna_core::approvals::{self, Decision, Request};
use letronna_core::budget::Budget;
use letronna_core::cancel::Cancel;
use letronna_core::chat::{self, Message};
use letronna_core::driver::{self, Allow, Outcome, Report, Spec, Status};
use letronna_core::store::Transcript;
use letronna_core::{agent, store, tools, update};
use serde_json::{Value, json};
use session::Runner;
use std::io::IsTerminal;
use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc};

/// How often a session start quietly asks whether there is a newer version.
const LOOK_FOR_UPDATES: std::time::Duration = std::time::Duration::from_secs(24 * 60 * 60);
/// How long a question asked once waits for MCP servers still starting.
const MCP_WAIT: std::time::Duration = std::time::Duration::from_secs(15);
/// A frontend reads a run's status at most four times a second, and
/// stream-json follows it no more often.
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(250);
/// Lines of a call's preview shown above the question, as the conversation shows.
const PREVIEW_LINES: usize = 12;

fn main() {
    rescue_terminal();
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        Err(error) => {
            render::line(&render::red(&error));
            std::process::exit(1);
        }
    };
    if args.help {
        print!("{HELP}");
        return;
    }
    if args.version {
        println!("letronna {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    if let Some(words) = &args.mcp {
        std::process::exit(mcp::run(words));
    }
    if args.gui {
        launch_window();
        return;
    }
    if args.uninstall {
        match install::remove() {
            Ok(done) => done
                .iter()
                .for_each(|line| render::line(&render::dim(line))),
            Err(error) => render::line(&render::red(&error)),
        }
        return;
    }
    if args.update {
        update_now();
        return;
    }
    if args.doctor {
        std::process::exit(doctor::run(args.offline));
    }
    // Before anything is opened, so a mistyped name leaves nothing behind.
    let gateway = match args.gateway.as_deref().map(gateway_named).transpose() {
        Ok(gateway) => gateway,
        Err(error) => {
            render::line(&render::red(&error));
            std::process::exit(1);
        }
    };
    letronna_core::diag::init("cli");
    let mode = match &args.mode {
        Ok(mode) => *mode,
        Err(error) => {
            render::line(&render::red(error));
            std::process::exit(1);
        }
    };
    // MCP servers can take seconds to start; started now, their tools are
    // there by the first question.
    letronna_core::mcp::start();

    let mut runner = Runner {
        store: store::load_listing(),
        allowed: Default::default(),
        mode,
        // With the question piped in or the answer piped out, a card would be
        // drawn or read where nobody sees it, and wait forever.
        human: args.prompt.is_none()
            || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal()),
        trouble: None,
        bar: bar::Bar::new(String::new()),
        stopped: false,
    };
    letronna_core::browser::share_browser(runner.store.settings.browser_shared);
    std::thread::spawn(letronna_core::browser::close_left_over);
    // With nothing set up there is nothing to talk to, so the first run is
    // the setup -- the same one the installers hand over to. Asking a
    // question from a pipe cannot wait for answers, so that one stops here.
    let first_run = runner.store.settings.model.is_empty();
    if first_run && args.prompt.is_some() {
        let error = "Nothing is set up yet. Run `letronna setup` first.";
        show(args.output, &failed(error.into()));
        std::process::exit(1);
    }

    // Setup is printed straight into the terminal, like an installer, not
    // run inside the conversation screen: installing is not talking to it
    // yet. `letronna setup` stops when it is done; a first run with nothing
    // set up carries on into the conversation once there is a model.
    if args.install || first_run {
        render::title("Letronna setup");
        let ready = setup::run(&mut runner, args.install);
        render::title("");
        if args.install || !ready {
            return;
        }
    }

    let workspace = std::env::current_dir().ok();
    let mut session = match args.start {
        Start::Last => runner.open(true, workspace.clone()),
        Start::Named(id) => match runner.open_id(id) {
            Some(index) => index,
            None => {
                show(
                    args.output,
                    &failed(format!("No session with the id {id}.")),
                );
                std::process::exit(1);
            }
        },
        _ => runner.open(false, workspace.clone()),
    };
    choose_by_flags(&mut runner, session, gateway, &args);

    // One question, one answer, no banner: this form gets piped.
    if let Some(prompt) = &args.prompt {
        // It goes out at once, so the MCP servers' tools would miss it.
        letronna_core::mcp::ready(MCP_WAIT);
        let finished = ask_once(&mut runner, session, prompt, &args);
        // A browser with no window, or a job started in the background, would
        // outlive this run unseen.
        letronna_core::browser::close_hidden();
        letronna_core::jobs::stop_all();
        letronna_core::mcp::stop_all();
        if !finished {
            letronna_core::diag::stop();
            std::process::exit(1);
        }
        letronna_core::diag::stop();
        return;
    }

    let folder = folder_name(workspace.as_deref());
    title::idle();
    // The window has to be ours before the banner, or the banner goes to the
    // screen we are about to swap away from.
    let note = footer(&runner);
    runner.bar.set_footer(note);
    runner.bar.install();
    // Before anything can start a turn. Without the box, a terminal on both
    // ends is still someone to ask.
    runner.serve_helpers(std::io::stdin().is_terminal() && std::io::stdout().is_terminal());
    render::clear();
    banner(&runner, &folder, session);
    if let Some(version) = update::seen() {
        render::line(&render::dim(&format!(
            "Letronna {version} is out · letronna update"
        )));
    }
    // Asked in the background and only said next time, so a start never
    // waits on the network for it.
    if update::enabled() && !runner.store.settings.manual_updates && update::due(LOOK_FOR_UPDATES) {
        std::thread::spawn(|| {
            let _ = update::check();
        });
    }
    if args.start == Start::Choose {
        resume(&mut runner, &mut session, workspace.as_deref(), "");
    } else if runner.store.conversations[session].length() > 0 {
        replay(&runner, session);
    }

    // Only a conversation that goes on hears about work that ends after its
    // turn; `-p` has left by now, with its answer.
    runner.bar.listen();
    while let Some(waited) = runner.bar.wait() {
        if waited == bar::Waited::Asked {
            runner.answer_helpers(true);
            continue;
        }
        let bar::Waited::Line(line) = waited else {
            // Notices came for the open session while it sat idle, and it
            // takes them as a turn of its own -- unless the user stopped the
            // last one, when they wait to go in ahead of the next prompt.
            if !runner.stopped
                && let Err(error) = runner.resume(session)
            {
                render::blank();
                render::line(&render::red(&error));
                screen::settle();
            }
            continue;
        };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        let mut prompt = line.as_str();
        if line.starts_with('/') {
            let flow = command(&mut runner, &line, &mut session, workspace.as_deref());
            // Every turn follows its own session's helpers; this is for the
            // footer's count once a command has opened another.
            let open = runner.store.conversations[session].id;
            runner.bar.show_helpers_of(open);
            if let Flow::Unknown = flow {
                let name = line.split_whitespace().next().unwrap_or(&line);
                render::line(&render::dim(&match bar::closest(name) {
                    Some(meant) => format!("{name} is not a command. Did you mean {meant}?"),
                    None => format!("{line} is not a command. Type / to see what is."),
                }));
            }
            // Painting is throttled, so the last thing a command printed can
            // still be sitting in the buffer with nothing coming after it to
            // bring it out. Whatever it said, say it now.
            screen::settle();
            match flow {
                Flow::Stop => break,
                // The command stays on screen as typed; the model gets its prompt.
                Flow::Ask(asked) => prompt = asked,
                _ => continue,
            }
        }
        if let Err(error) = runner.ask(session, prompt) {
            render::blank();
            render::line(&render::red(&error));
            // Nothing is painted after this until the next key, so without
            // it a failed turn looks like one that ended with no answer.
            screen::settle();
        }
        runner.persist();
        if runner.bar.finished() {
            break;
        }
    }
    runner.bar.uninstall();
    // A job's process is not ended with ours, so a dev server left running
    // would carry on with nobody left to stop it.
    letronna_core::jobs::stop_all();
    letronna_core::mcp::stop_all();
    letronna_core::browser::close_hidden();
    render::title("");
    render::blank();
    letronna_core::diag::stop();
}

/// Runs one prompt on the driver and prints what came of it in the form
/// asked for, saying whether it ended without failing.
fn ask_once(runner: &mut Runner, session: usize, prompt: &str, args: &Args) -> bool {
    let mut connection = runner.connection(session);
    if let Some(model) = &args.model {
        connection.model = model.clone();
    }
    if let Err(error) = chat::validate(&connection) {
        show(args.output, &failed(error));
        return false;
    }
    // Checked here rather than with the flags, since an MCP server's tools
    // are only known once it has started.
    if let Some(name) = args
        .tools
        .iter()
        .flatten()
        .find(|n| tools::find(n).is_none())
    {
        show(
            args.output,
            &failed(format!("There is no tool called `{name}`.")),
        );
        return false;
    }
    let conversation = &mut runner.store.conversations[session];
    if conversation.messages.is_empty() && conversation.title.is_empty() {
        conversation.title = prompt.chars().take(40).collect();
    }
    conversation.updated = chat::now();
    conversation.messages.push(Message {
        role: "user".into(),
        content: prompt.to_owned(),
        ..Default::default()
    });
    runner.persist();
    // The run writes on into the session's own file, so the session is
    // listed and can be continued as before. It leaves the store here and
    // comes back only when the run is over, so that file has one writer.
    let conversation = runner.store.conversations.remove(session);
    let (id, before) = (conversation.id, conversation.messages.len());
    let context = tools::ToolContext::of(&connection);
    let spec = Spec {
        connection,
        transcript: Transcript {
            path: store::sessions_dir().join(format!("{id}.jsonl")),
            conversation,
        },
        id,
        // Nobody started it but the person at the prompt, so mail it never
        // read stays in its own box.
        owner: id,
        root: id,
        run: id,
        label: "Letronna".into(),
        depth: 0,
        fork: false,
        from: 0,
        allow: args.tools.clone().map_or(Allow::All, Allow::Only),
        rounds: args.max_rounds.unwrap_or(agent::MAX_ROUNDS),
        cancel: Cancel::default(),
        budget: args.budget.map(|limit| Arc::new(Budget::new(limit, None))),
        status: Arc::default(),
    };
    // Served before the run starts, since a request with nobody serving is
    // declined at once.
    let keyboard = args.output == Output::Text && render::interactive();
    answer_approvals(
        runner.mode == letronna_core::gate::Mode::Bypass,
        keyboard,
        context,
        spec.cancel.clone(),
    );
    let (running, ended) = mpsc::channel::<()>();
    let progress =
        (args.output == Output::StreamJson).then(|| show_progress(spec.status.clone(), ended));
    let mut report = driver::run(spec);
    drop(running);
    if let Some(progress) = progress {
        let _ = progress.join();
    }
    // The report's text is the last thing said in the whole session, which
    // after -c is an earlier answer when this run said nothing.
    if let Some(said) = take_back(runner, session, id, before) {
        report.text = said;
    }
    show(args.output, &report);
    !matches!(report.status, Outcome::Failed(_))
}

/// Takes the session back into the store from the file the run wrote and
/// saves it with the time the run ended, as a reply in the conversation
/// does, giving the last thing said after its first `before` messages.
fn take_back(runner: &mut Runner, session: usize, id: u64, before: usize) -> Option<String> {
    let mut listing = store::load_listing();
    let index = listing.index_of(id)?;
    let mut conversation = listing.conversations.remove(index);
    store::fill(&mut conversation);
    let said = said_last(conversation.messages.get(before..)?);
    conversation.updated = chat::now();
    runner.store.conversations.insert(session, conversation);
    runner.persist();
    Some(said)
}

/// The last thing the model said among `messages`, or nothing.
fn said_last(messages: &[Message]) -> String {
    messages
        .iter()
        .rev()
        .find(|message| message.role == "assistant" && !message.content.is_empty())
        .map(|message| message.content.clone())
        .unwrap_or_default()
}

/// A run that failed before it began, having spent nothing.
fn failed(error: String) -> Report {
    Report {
        status: Outcome::Failed(error),
        text: String::new(),
        rounds: 0,
        input: 0,
        cached: 0,
        output: 0,
        seconds: 0,
        files: Vec::new(),
        jobs: Vec::new(),
    }
}

/// Prints how a run ended in the form asked for, so a script reading JSON
/// gets JSON for a run that never began too.
fn show(output: Output, report: &Report) {
    match output {
        Output::Text => print_answer(report),
        Output::Json => {
            emit(&as_json(report));
        }
        Output::StreamJson => {
            let mut result = as_json(report);
            result["type"] = "result".into();
            emit(&result);
        }
    }
}

/// Writes one JSON line, saying whether it could: a reader that has gone
/// away must end the lines, not the process before its jobs are stopped.
fn emit(line: &Value) -> bool {
    writeln!(std::io::stdout().lock(), "{line}").is_ok()
}

/// Answers the run's calls that need a yes: every one with `--yes`, by asking
/// when there is a keyboard to ask at, and otherwise no, saying why.
fn answer_approvals(yes: bool, keyboard: bool, context: tools::ToolContext, cancel: Cancel) {
    let requests = approvals::serve();
    std::thread::spawn(move || {
        for request in requests {
            let decision = if yes {
                Decision::Once
            } else if keyboard {
                consult(&request, &context, &cancel)
            } else {
                eprintln!(
                    "`{}` needs a yes and nobody is here to give it; run with --yes to allow it.",
                    request.call.name
                );
                Decision::Deny(String::new())
            };
            request.answer(decision);
        }
    });
}

/// Asks the person at the keyboard about one call, the way the conversation
/// does, stopping the run when they say stop or leave the question.
fn consult(request: &Request, context: &tools::ToolContext, cancel: &Cancel) -> Decision {
    let call = &request.call;
    let summary = tools::summary(&call.name, &call.arguments);
    // A change to a file is shown as a diff, anything else as its preview.
    match crate::diff::of(&call.name, &call.arguments, context) {
        Some(rows) => rows.iter().for_each(|row| render::out(row, true)),
        None => request
            .preview
            .lines()
            .take(PREVIEW_LINES)
            .for_each(|line| render::line(&render::dim(line))),
    }
    render::line(&render::bold(&format!(
        "Letronna wants to {}",
        session::intent(&call.name, &summary)
    )));
    let options = [
        "Yes",
        "Yes, and stop asking for this",
        "No, tell it why not",
        "No, and stop here",
    ];
    match input::menu(&options, 0) {
        Some(0) => Decision::Once,
        Some(1) => Decision::ForRun,
        Some(2) => Decision::Deny(String::new()),
        _ => {
            cancel.set();
            Decision::Deny(String::new())
        }
    }
}

/// Writes a stream-json progress line whenever the run's rounds or tokens
/// have moved on, until `ended` hears that the run is over.
fn show_progress(status: Arc<Status>, ended: mpsc::Receiver<()>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut shown = progress(&status);
        while let Err(mpsc::RecvTimeoutError::Timeout) = ended.recv_timeout(PROGRESS_EVERY) {
            let now = progress(&status);
            if now != shown {
                if !emit(&now) {
                    return;
                }
                shown = now;
            }
        }
    })
}

/// A run's progress so far, as a stream-json line.
fn progress(status: &Status) -> Value {
    json!({
        "type": "progress",
        "rounds": status.rounds.load(Ordering::Relaxed),
        "input": status.input.load(Ordering::Relaxed),
        "cached": status.cached.load(Ordering::Relaxed),
        "output": status.output.load(Ordering::Relaxed),
    })
}

/// Prints the answer as the conversation would, and a line after it when the
/// run did not finish.
fn print_answer(report: &Report) {
    if !report.text.is_empty() {
        let mut out = render::Stream::opening_with(render::bullet());
        out.push(&report.text);
        out.end();
    }
    let note = match &report.status {
        Outcome::Done => return,
        Outcome::Failed(error) => render::red(error),
        Outcome::RoundLimit => render::dim(
            "Stopped: it used every tool round it was given, and --max-rounds allows more.",
        ),
        Outcome::OverBudget => render::dim("Stopped: the token budget is spent."),
        Outcome::Stopped => render::dim("Stopped."),
    };
    render::line(&note);
}

/// What a run came to, as `--output json` prints it.
fn as_json(report: &Report) -> Value {
    let status = match &report.status {
        Outcome::Done => "done",
        Outcome::RoundLimit => "round_limit",
        Outcome::Stopped => "stopped",
        Outcome::Failed(_) => "failed",
        Outcome::OverBudget => "over_budget",
    };
    let mut out = json!({
        "answer": report.text,
        "rounds": report.rounds,
        "input": report.input,
        "cached": report.cached,
        "output": report.output,
        "seconds": report.seconds,
        "files": report.files,
        "status": status,
    });
    if let Outcome::Failed(error) = &report.status {
        out["error"] = error.as_str().into();
    }
    out
}

/// The gateway `name` means, in any case.
fn gateway_named(name: &str) -> Result<&'static str, String> {
    let gateways = letronna_core::gateway::GATEWAYS;
    if let Some(gateway) = gateways
        .iter()
        .find(|g| g.name.eq_ignore_ascii_case(name.trim()))
    {
        return Ok(gateway.name);
    }
    let names: Vec<&str> = gateways.iter().map(|g| g.name).collect();
    Err(format!(
        "There is no gateway called {name}. The gateways are {}.",
        names.join(", ")
    ))
}

/// Put the session on the model, gateway and effort the flags name. Only the
/// session takes them: the settings, and with them new sessions, stay as
/// they were.
fn choose_by_flags(runner: &mut Runner, session: usize, gateway: Option<&str>, args: &args::Args) {
    if gateway.is_some() || args.model.is_some() {
        let (on, model) = runner.store.model(session);
        let gateway = gateway.unwrap_or(on).to_owned();
        let model = args.model.clone().unwrap_or_else(|| model.to_owned());
        let conversation = &mut runner.store.conversations[session];
        conversation.gateway = Some(gateway);
        conversation.model = Some(model);
    }
    if let Some(effort) = &args.effort {
        let effort = effort.trim().to_lowercase();
        // Auto leaves it to the model, as it does for `/effort`.
        let effort = if effort == "auto" {
            String::new()
        } else {
            effort
        };
        runner.store.conversations[session].effort = Some(effort);
    }
}

/// Hand the terminal back if we panic. Without this a crash leaves the
/// window in raw mode on an alternate screen: no echo, no prompt, and the
/// conversation still on the screen over whatever was there before. The
/// report itself is left to the usual hook, which runs straight after.
fn rescue_terminal() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Full scrolling region, cursor back, raw mode off.
        print!("\x1b[r\x1b[?2004l\x1b[?25h");
        let _ = crossterm::terminal::disable_raw_mode();
        render::set_raw(false);
        render::title("");
        previous(info);
    }));
}

/// `letronna update`: download the newest version, check it, and put it in
/// place of this one. The window, when it is installed, is replaced with it.
fn update_now() {
    let release = match input::busy("looking for a newer version", update::check) {
        Ok(Some(release)) => release,
        Ok(None) => {
            render::line(&render::dim(&format!(
                "This is the newest version, {}.",
                update::VERSION
            )));
            return;
        }
        Err(error) => {
            render::line(&render::red(&error));
            std::process::exit(1);
        }
    };
    render::line(&format!(
        "Letronna {} is out {}",
        release.version,
        render::dim(&format!("(this is {})", update::VERSION))
    ));
    let version = release.version.clone();
    // The window may have downloaded it already.
    if update::staged().as_ref() != Some(&version) {
        let label = format!("downloading {version}");
        if let Err(error) = input::busy(&label, move || update::download(&release)) {
            render::line(&render::red(&error));
            std::process::exit(1);
        }
    }
    match update::install() {
        Ok(_) => render::line(&render::dim(&format!(
            "Updated to {version}. Start letronna again to use it."
        ))),
        Err(error) => {
            render::line(&render::red(&error));
            std::process::exit(1);
        }
    }
}

/// Start the window. It is a separate binary because a GUI-subsystem
/// executable on Windows cannot write to the console that launched it.
fn launch_window() {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(window_name())));
    // Installed, the window sits one level up from the bin folder holding
    // this. It is named for the app there rather than for the binary, and on
    // Linux that name has no extension and a lowercase first letter.
    let above = std::env::current_exe().ok().and_then(|p| {
        p.parent()
            .and_then(|d| d.parent())
            .map(|d| d.join(installed_name()))
    });
    for candidate in [beside, above, Some(std::path::PathBuf::from(window_name()))]
        .into_iter()
        .flatten()
    {
        if std::process::Command::new(&candidate).spawn().is_ok() {
            return;
        }
    }
    render::line(&render::red("Could not find the Letronna window binary."));
}

fn window_name() -> &'static str {
    if cfg!(windows) {
        "letronna-gui.exe"
    } else {
        "letronna-gui"
    }
}

/// What the installed window is called, which is not what the binary is
/// called in the build folder.
fn installed_name() -> &'static str {
    if cfg!(windows) {
        "Letronna.exe"
    } else {
        "letronna-gui"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn report(status: Outcome) -> Report {
        Report {
            status,
            text: "The notes say hello.".into(),
            rounds: 2,
            input: 3_000,
            cached: 1_000,
            output: 150,
            seconds: 4,
            files: vec!["notes.txt".into()],
            jobs: Vec::new(),
        }
    }

    #[test]
    fn json_output_carries_the_answer_rounds_and_tokens() {
        let out = as_json(&report(Outcome::Done));
        assert_eq!(
            out,
            json!({
                "answer": "The notes say hello.",
                "rounds": 2,
                "input": 3_000,
                "cached": 1_000,
                "output": 150,
                "seconds": 4,
                "files": ["notes.txt"],
                "status": "done",
            })
        );
        // A run that ended short says how, and a failed one says why.
        assert_eq!(
            as_json(&report(Outcome::RoundLimit))["status"],
            "round_limit"
        );
        assert_eq!(
            as_json(&report(Outcome::OverBudget))["status"],
            "over_budget"
        );
        let failed = as_json(&report(Outcome::Failed("The key was refused.".into())));
        assert_eq!(
            (&failed["status"], &failed["error"]),
            (&json!("failed"), &json!("The key was refused."))
        );
        assert!(out.get("error").is_none());
    }

    #[test]
    fn an_earlier_answer_is_not_taken_for_this_runs() {
        let said = |role: &str, content: &str| Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        };
        let session = [
            said("user", "What is in notes.txt?"),
            said("assistant", "The notes say hello."),
            said("user", "Fix the failing test."),
            // A reply that only calls tools says nothing.
            said("assistant", ""),
        ];
        assert_eq!(said_last(&session[2..]), "");
        assert_eq!(said_last(&session), "The notes say hello.");
    }
}
