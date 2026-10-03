//! Letronna in the terminal.
//!
//! `letronna` on its own opens a conversation; `-p` asks one question and
//! leaves. Sessions are the same files the window uses, so a conversation
//! started here can be carried on there and the other way round.
//!
//! `-p` runs on core's driver rather than the conversation loop: nobody
//! watches it stream, so it waits on the run and says what came of it at the
//! end, as the answer alone or as JSON a script can read. With
//! `--input-format stream-json` another program drives it over stdin instead;
//! `control` holds that.

mod acp;
mod args;
mod banner;
mod bar;
mod commands;
mod control;
mod diff;
mod doctor;
mod edit;
mod input;
mod install;
mod mcp;
mod memory;
mod notify;
mod render;
mod screen;
mod serve;
mod session;
mod setup;
mod spinner;
mod syntax;
mod title;

use args::{Args, HELP, Output, Start, parse};
use banner::{banner, folder_name, footer, replay};
use commands::{Flow, command, offer_added, resume};
use letronna_core::approvals::{self, Decision, Request};
use letronna_core::budget::Budget;
use letronna_core::cancel::Cancel;
use letronna_core::chat::{self, Message};
use letronna_core::driver::{self, Allow, Outcome, Report, Spec, Status};
use letronna_core::store::Transcript;
use letronna_core::{crew, hooks, inbox, store, tools, update};
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
/// How long leaving waits for the commands it ends. Each sees it within
/// 50 ms, so only a machine under load comes near this.
const END_WAIT: std::time::Duration = std::time::Duration::from_secs(2);
/// A frontend reads a run's status at most four times a second, and
/// stream-json follows it no more often.
const PROGRESS_EVERY: std::time::Duration = std::time::Duration::from_millis(250);
/// Lines of a call's preview shown above the question, as the conversation shows.
const PREVIEW_LINES: usize = 12;
/// How long a new version runs before it counts as working and is kept, as
/// in the window.
const SETTLED: std::time::Duration = std::time::Duration::from_secs(10);
/// Four times a conversation's 25 tool rounds, since nobody is there to say
/// "continue" and a task left to run on its own, such as a fix across a
/// repository, often reads and runs more than 25 times.
const PRINT_ROUNDS: usize = 100;

fn main() {
    // The first mark, from which the others are timed.
    letronna_core::diag::mark("start");
    rescue_terminal();
    let args = match parse(std::env::args().skip(1)) {
        Ok(args) => args,
        // 2, apart from a run that went wrong, so a script can tell it
        // called Letronna wrongly; on stderr, where a script looks for it
        // and a reader of JSON lines does not trip on it.
        Err(error) => {
            eprintln!("{}", render::red(&error));
            std::process::exit(2);
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
    // Before anything reads a key, so each comes from the vault and any
    // still in a file is moved there.
    letronna_core::vault::open();
    if let Some(words) = &args.mcp {
        std::process::exit(mcp::run(words));
    }
    if args.acp {
        std::process::exit(acp::run());
    }
    // Before the update is seen to: the window counts its own start and
    // installs what is waiting, so counting here too would put a working
    // version back after half the starts, and removing Letronna has no use
    // for a newer one.
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
    // An update downloaded last time goes in now, before anything is running,
    // and so does the old version when a new one keeps failing to start.
    // After `--version`, which the updater runs a new binary with before it
    // is trusted, and which must not count as a start.
    if let Some(program) = update::on_start() {
        std::process::exit(start_instead(&program));
    }
    let keep = Keep::new(confirm_update);
    // `-p` exits as soon as it has answered, so only an answer proves it.
    if args.prompt.is_none() {
        keep.after(SETTLED);
    }
    if let Some(words) = &args.mcp {
        std::process::exit(mcp::run(words));
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
            eprintln!("{}", render::red(&error));
            std::process::exit(1);
        }
    };
    letronna_core::diag::init("cli");
    let mode = match &args.mode {
        Ok(mode) => *mode,
        Err(error) => {
            eprintln!("{}", render::red(error));
            std::process::exit(1);
        }
    };
    // MCP servers can take seconds to start; started now, their tools are
    // there by the first question.
    letronna_core::mcp::start();
    letronna_core::diag::mark("mcp");

    // A session named on the command line is kept for the start opening it.
    let named = match args.start {
        Start::Named(id) => Some(id),
        _ => None,
    };
    store::prune_old(named.as_slice());
    let mut runner = Runner {
        store: store::load_listing(),
        allowed: Default::default(),
        mode,
        // With the question piped in or the answer piped out, a card would be
        // drawn or read where nobody sees it, and wait forever.
        human: !args.once() || (std::io::stdin().is_terminal() && std::io::stdout().is_terminal()),
        // Set by `serve_helpers`.
        helpers_answered: false,
        trouble: None,
        bar: bar::Bar::new(String::new()),
        stopped: false,
        notices: letronna_core::notify::Notices::new(Box::new(notify::Terminal::here())),
        ended: None,
        images: Vec::new(),
        voice: None,
        review: Default::default(),
    };
    letronna_core::diag::mark("listed");
    runner.match_voice();
    letronna_core::browser::share_browser(runner.store.settings.browser_shared);
    std::thread::spawn(letronna_core::browser::close_left_over);
    // Keys the vault would not give read as missing; this says why.
    if let Some(why) = letronna_core::vault::trouble().filter(|_| runner.human) {
        render::line(&render::red(&why));
    }
    // With nothing set up there is nothing to talk to, so the first run is
    // the setup -- the same one the installers hand over to. Asking a
    // question from a pipe cannot wait for answers, so that one stops here,
    // and so does serving, which nobody sits at.
    let first_run = runner.store.settings.model.is_empty();
    if first_run && (args.once() || args.serve) {
        let error = "Nothing is set up yet. Run `letronna setup` first.";
        show(args.output, &failed(error.into()));
        std::process::exit(1);
    }
    if args.serve {
        std::process::exit(serve::run());
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
    letronna_core::diag::mark("session");

    // One question, one answer, no banner: this form gets piped.
    if args.once() {
        // It goes out at once, so the MCP servers' tools would miss it.
        letronna_core::mcp::ready(MCP_WAIT);
        let interrupted = Cancel::default();
        let code = match &args.prompt {
            Some(prompt) => {
                // From here a Ctrl-C stops the run instead, so what it
                // started is ended below before the process goes. Not during
                // the wait above, which would not hear it.
                letronna_core::cancel::on_interrupt(interrupted.clone());
                let finished = ask_once(&mut runner, session, prompt, &args, &interrupted);
                if finished {
                    keep.now();
                }
                i32::from(!finished)
            }
            None => control::run(&mut runner, session, &args),
        };
        // A command still running, a browser with no window, or a job started
        // in the background, would outlive this run unseen.
        letronna_core::tools::end_commands(END_WAIT);
        letronna_core::browser::close_hidden();
        letronna_core::jobs::stop_all();
        letronna_core::mcp::stop_all();
        letronna_core::lsp::stop_all();
        runner.store.release_unused();
        letronna_core::diag::stop();
        if interrupted.is_set() {
            // What a shell reports for a program a Ctrl-C ended.
            std::process::exit(130);
        }
        if code != 0 {
            std::process::exit(code);
        }
        return;
    }

    let folder = folder_name(workspace.as_deref());
    title::idle();
    // The window has to be ours before the banner, or the banner goes to the
    // screen we are about to swap away from.
    let note = footer(&runner);
    runner.bar.set_footer(note);
    runner.bar.install();
    letronna_core::diag::mark("screen");
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

    letronna_core::diag::mark("ready");

    // Only a conversation that goes on hears about work that ends after its
    // turn; `-p` has left by now, with its answer.
    runner.bar.listen();
    runner.ended = Some(letronna_core::notify::listen());
    // Likewise only a terminal that stays open runs the schedules, unless the
    // window already does. A run lands in the sessions, where /resume finds
    // it, and is told at the idle prompt when it ends.
    let (_schedules, events) = letronna_core::schedule::Runner::start();
    runner.bar.hear_schedules(events);
    offer_added(&mut runner, workspace.as_deref());
    runner
        .bar
        .set_folder(runner.store.conversations[session].workspace.clone());
    while let Some(waited) = runner.bar.wait() {
        // Helpers that ended ring the doorbell as they do.
        runner.tell_ended();
        if let bar::Waited::Scheduled(event) = waited {
            scheduled(event);
            screen::settle();
            continue;
        }
        if waited == bar::Waited::Asked {
            runner.answer_helpers(true);
            continue;
        }
        if let bar::Waited::Note(note) = &waited {
            render::blank();
            render::line(&render::dim(note));
            screen::settle();
            continue;
        }
        let bar::Waited::Line(line) = waited else {
            // Notices came for the open session while it sat idle, and it
            // takes them as a turn of its own -- unless the user stopped the
            // last one, when they wait to go in ahead of the next prompt. A
            // loop's tick is the user's standing order, which a stop does
            // not hold back, or the loop would stall until they typed.
            let id = runner.store.conversations[session].id;
            if (!runner.stopped || letronna_core::wake::waiting(id))
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
        let mut prompt = bar::unescaped(&line);
        // What an MCP server's prompt or a saved workflow's command came to,
        // when the command was one.
        let sent: String;
        let mut run = false;
        if bar::is_command(&line) {
            let flow = command(&mut runner, &line, &mut session, workspace.as_deref());
            // Read again after each command, so a command file made or
            // changed meanwhile is offered without a restart.
            offer_added(&mut runner, workspace.as_deref());
            // Every turn follows its own session's helpers; this is for the
            // footer's count once a command has opened another.
            let open = runner.store.conversations[session].id;
            runner.bar.show_helpers_of(open);
            runner
                .bar
                .set_folder(runner.store.conversations[session].workspace.clone());
            if let Flow::Unknown = flow {
                let name = line.split_whitespace().next().unwrap_or(&line);
                render::line(&render::dim(&match runner.bar.meant(name) {
                    Some(meant) => format!("{name} is not a command. Did you mean {meant}?"),
                    None => format!("{line} is not a command. Type / to see what is."),
                }));
            }
            // The footer names the open session's Crew switch, which the
            // command may have turned, or left behind by opening another.
            let note = footer(&runner);
            runner.bar.set_footer(note);
            // Painting is throttled, so the last thing a command printed can
            // still be sitting in the buffer with nothing coming after it to
            // bring it out. Whatever it said, say it now.
            screen::settle();
            match flow {
                Flow::Stop => break,
                // Asked here rather than queued, where text starting with a
                // slash would be run as a command.
                Flow::Send(text) => {
                    runner.bar.echo(&text);
                    sent = text;
                    prompt = &sent;
                }
                // The command stays on screen as typed; the model gets its prompt.
                Flow::Ask(asked) => prompt = asked,
                Flow::Run(asked) => {
                    sent = asked;
                    prompt = &sent;
                    run = true;
                }
                _ => continue,
            }
        }
        let id = runner.store.conversations[session].id;
        for notice in crew::prompt(id, &line, crew::Source::Bar) {
            inbox::push(id, notice);
        }
        // After `prompt`, which ends what the last command granted.
        if run {
            crew::grant(id);
        }
        match runner.ask(session, prompt) {
            Ok(_) => keep.now(),
            Err(error) => {
                render::blank();
                render::line(&render::red(&error));
                // Nothing is painted after this until the next key, so without
                // it a failed turn looks like one that ended with no answer.
                screen::settle();
            }
        }
        runner.persist();
        if runner.bar.finished() {
            break;
        }
    }
    runner.bar.uninstall();
    // A job's process is not ended with ours, so a dev server left running
    // would carry on with nobody left to stop it. A foreground command is in
    // a group of its own too, so one still running, or still being stopped
    // after a Ctrl-C, is ended and waited for first.
    letronna_core::tools::end_commands(END_WAIT);
    letronna_core::jobs::stop_all();
    letronna_core::mcp::stop_all();
    letronna_core::lsp::stop_all();
    letronna_core::browser::close_hidden();
    letronna_core::schedule::stop();
    runner.store.release_unused();
    render::title("");
    render::blank();
    letronna_core::diag::stop();
}

/// Runs one prompt on the driver, stopped when `interrupted` is set, and
/// prints what came of it in the form asked for, saying whether it ended
/// without failing.
fn ask_once(
    runner: &mut Runner,
    session: usize,
    prompt: &str,
    args: &Args,
    interrupted: &Cancel,
) -> bool {
    let budget = args.budget.map(|limit| Arc::new(Budget::new(limit, None)));
    let (mut spec, context, before) = match begin(runner, session, prompt, args, budget) {
        Ok(begun) => begun,
        Err(error) => {
            show(args.output, &failed(error));
            return false;
        }
    };
    // A child, so a decline at the keyboard that stops the run does not read
    // as a Ctrl-C.
    spec.cancel = interrupted.child();
    let id = spec.id;
    // Served before the run starts, since a request with nobody serving is
    // declined at once.
    let keyboard = args.output == Output::Text && render::interactive();
    // Its own gate asks only when someone can answer.
    spec.connection.human = keyboard;
    answer_approvals(keyboard, context, spec.cancel.clone());
    let report = run_following(spec, args.output);
    finish(runner, session, id, before, report, args.output)
}

/// Puts `prompt` on the session and hands the session to a run spending
/// from `budget`, giving the run, what its calls work in, and where its own
/// messages start.
fn begin(
    runner: &mut Runner,
    session: usize,
    prompt: &str,
    args: &Args,
    budget: Option<Arc<Budget>>,
) -> Result<(Spec, tools::ToolContext, usize), String> {
    let mut connection = runner.connection(session);
    if let Some(model) = &args.model {
        connection.model = model.clone();
    }
    chat::validate(&connection)?;
    // Checked here rather than with the flags, since an MCP server's tools
    // are only known once it has started.
    if let Some(name) = args
        .tools
        .iter()
        .flatten()
        .find(|n| tools::find(n).is_none())
    {
        return Err(format!("There is no tool called `{name}`."));
    }
    let conversation = &mut runner.store.conversations[session];
    // One turn and out: nobody is there to watch a loop or stop it.
    if conversation.goal.is_some() {
        eprintln!(
            "This session has a goal, but goals are pursued only in the terminal and the \
             window, not with -p: this runs one turn."
        );
    }
    if conversation.messages.is_empty() && conversation.title.is_empty() {
        conversation.title = prompt.chars().take(40).collect();
    }
    conversation.updated = chat::now();
    let content = letronna_core::references::expand(prompt, conversation.workspace.as_deref());
    conversation.messages.push(Message {
        role: "user".into(),
        content,
        ..Default::default()
    });
    runner.persist();
    // The run writes on into the session's own file, so the session is
    // listed and can be continued as before. It leaves the store here and
    // comes back only when the run is over, so that file has one writer.
    let conversation = runner.store.conversations.remove(session);
    let (id, before) = (conversation.id, conversation.messages.len());
    // What the user asked for, which Auto mode's judge reads as their wishes.
    connection.typed = letronna_core::judge::typed(&conversation.messages).into();
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
        schema: None,
        watch: None,
        decide: None,
        allow: args.tools.clone().map_or(Allow::All, Allow::Only),
        rounds: rounds(args),
        cancel: Cancel::default(),
        budget,
        status: Arc::default(),
    };
    Ok((spec, context, before))
}

/// The tool rounds a `-p` run gets: what --max-rounds says, else `PRINT_ROUNDS`.
fn rounds(args: &Args) -> usize {
    args.max_rounds.unwrap_or(PRINT_ROUNDS)
}

/// Runs `spec` to its end on this thread, with progress lines while it runs
/// when the output is stream-json.
fn run_following(spec: Spec, output: Output) -> Report {
    let (running, ended) = mpsc::channel::<()>();
    let progress =
        (output == Output::StreamJson).then(|| show_progress(spec.status.clone(), ended));
    let report = driver::run(spec);
    drop(running);
    if let Some(progress) = progress {
        let _ = progress.join();
    }
    report
}

/// Takes the session back from run `id` and prints how the run ended,
/// saying whether it ended without failing.
fn finish(
    runner: &mut Runner,
    session: usize,
    id: u64,
    before: usize,
    mut report: Report,
    output: Output,
) -> bool {
    // On stderr, apart from the answer: what a hook said or could not do.
    for problem in hooks::problems() {
        eprintln!("{problem}");
    }
    // The report's text is the last thing said in the whole session, which
    // after -c is an earlier answer when this run said nothing.
    if let Some(said) = take_back(runner, session, id, before) {
        report.text = said;
    }
    show(output, &report);
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
    // Read as the run's report reads it, so a reply continued after a
    // cut-off is whole.
    driver::last_said(messages)
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
        value: None,
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

/// Answers the run's calls that its own gate, and its helpers', left to a
/// person: by asking when there is a keyboard to ask at, and otherwise no,
/// saying why. The run's mode, the permission rules and the judge have
/// settled the rest before anything is asked.
fn answer_approvals(keyboard: bool, context: tools::ToolContext, cancel: Cancel) {
    let requests = approvals::serve();
    std::thread::spawn(move || {
        for request in requests {
            let decision = if keyboard {
                consult(&request, &context, &cancel)
            } else {
                say_refused(&request.call.name, "");
                Decision::Deny(String::new())
            };
            request.answer(decision);
        }
    });
}

/// Says on stderr why a call was refused with nobody to ask: the gate's
/// reason, or that it needed a yes when it gave none.
fn say_refused(name: &str, why: &str) {
    if why.is_empty() {
        eprintln!(
            "`{name}` needs a yes and nobody is here to give it; run with --yes to allow it."
        );
    } else {
        eprintln!("`{name}` was refused: {}", tools::one_line(why));
    }
}

/// Asks the person at the keyboard about one call, the way the conversation
/// does, stopping the run when they say stop or leave the question.
fn consult(request: &Request, context: &tools::ToolContext, cancel: &Cancel) -> Decision {
    let call = &request.call;
    hooks::card(context.clone(), &call.name);
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

/// Say what the schedules' runner said: a run that ended, with how to open
/// its session, or why the schedules are not running.
fn scheduled(event: letronna_core::schedule::Event) {
    /// Enough of an answer to recognise it by on one line.
    const PREVIEW: usize = 60;
    let run = match event {
        letronna_core::schedule::Event::Ended(run) => run,
        letronna_core::schedule::Event::Trouble(trouble) => {
            render::blank();
            render::line(&render::red(&trouble));
            return;
        }
        // A run is said once, when it ends.
        letronna_core::schedule::Event::Started(_) => return,
    };
    render::blank();
    let text = letronna_core::tools::one_line(&run.text);
    let mut preview: String = text.chars().take(PREVIEW).collect();
    if preview.len() < text.len() {
        preview.push('…');
    }
    let open = if run.session.is_some() {
        " · /resume to open it"
    } else {
        ""
    };
    render::line(&render::dim(&format!(
        "Scheduled job {}: {preview}{open}",
        run.name
    )));
}

/// Hand the terminal back if we panic. Without this a crash leaves the
/// window in raw mode on an alternate screen: no echo, no prompt, and the
/// conversation still on the screen over whatever was there before. The
/// report itself is left to the usual hook, which runs straight after.
fn rescue_terminal() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // Full scrolling region, focus reports off, cursor back, raw mode off.
        // Only on a terminal: a program reading JSON lines would get them
        // glued on.
        if std::io::stdout().is_terminal() {
            print!("\x1b[r\x1b[?2004l\x1b[?1004l\x1b[?25h");
        }
        let _ = crossterm::terminal::disable_raw_mode();
        render::set_raw(false);
        render::title("");
        previous(info);
    }));
}

/// Run `program`, the version put in place of this one, with this run's
/// arguments, and give back how it ended.
fn start_instead(program: &std::path::Path) -> i32 {
    let mut command = std::process::Command::new(program);
    command.args(std::env::args_os().skip(1));
    // It takes this terminal over rather than starting beside it as the
    // window does: were this one to leave, the shell would take the terminal
    // back while the new version was still reading from it.
    #[cfg(unix)]
    let error = {
        use std::os::unix::process::CommandExt;
        command.exec()
    };
    #[cfg(not(unix))]
    let error = match command.status() {
        Ok(status) => return status.code().unwrap_or(1),
        Err(error) => error,
    };
    render::line(&render::red(&format!(
        "Letronna changed version but could not start {}: {error}",
        program.display()
    )));
    1
}

/// Says this version works once a run, when the timer or the first turn to
/// finish comes, whichever is first.
#[derive(Clone)]
struct Keep {
    once: Arc<std::sync::Once>,
    confirm: fn(),
}

impl Keep {
    fn new(confirm: fn()) -> Keep {
        Keep {
            once: Arc::new(std::sync::Once::new()),
            confirm,
        }
    }

    /// Say it now, unless it was said already.
    fn now(&self) {
        self.once.call_once(self.confirm);
    }

    /// Say it once `delay` has passed.
    fn after(&self, delay: std::time::Duration) {
        let keep = self.clone();
        // One sleep and done, so nothing wakes while the terminal sits idle.
        std::thread::spawn(move || {
            std::thread::sleep(delay);
            keep.now();
        });
    }
}

/// Keep the version that is running, when this build updates itself.
fn confirm_update() {
    // As in the window: a development build is not the Letronna someone
    // installed, and settling that one's update would stop a bad release
    // being put back.
    if update::enabled() {
        update::confirm();
    }
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
    use letronna_core::chat::ToolCall;
    use letronna_core::gate::{self, Gate, Grants, Mode};
    use letronna_core::permissions::{self, Rule};

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
            value: None,
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

    #[test]
    fn a_print_run_gets_a_hundred_rounds_unless_told_otherwise() {
        let asked = |words: &[&str]| parse(words.iter().map(|w| w.to_string())).unwrap();
        assert_eq!(rounds(&asked(&["-p", "fix it"])), 100);
        assert_eq!(rounds(&asked(&["-p", "fix it", "--max-rounds", "40"])), 40);
    }

    fn call(name: &str, args: Value) -> ToolCall {
        ToolCall {
            id: format!("{name}-1"),
            name: name.into(),
            arguments: args.to_string(),
        }
    }

    fn command(line: &str) -> ToolCall {
        call("run_command", json!({ "command": line }))
    }

    /// How `-p`'s run decides `call`: its own gate in `mode`, as the driver
    /// puts each call to it, someone being there only with a keyboard;
    /// `None` when it is left to the person.
    fn answered(
        call: &ToolCall,
        ctx: &tools::ToolContext,
        rules: &Result<Vec<Rule>, String>,
        mode: Mode,
        keyboard: bool,
    ) -> Option<Decision> {
        let none = Grants::new();
        let gate = Gate::for_terminal(ctx, &none, rules, mode, keyboard);
        match gate::decide(call, &gate) {
            gate::Outcome::Run => Some(Decision::Once),
            gate::Outcome::Sandboxed => Some(Decision::Sandboxed),
            gate::Outcome::Refuse(why) => Some(Decision::Deny(why)),
            gate::Outcome::Ask => None,
        }
    }

    fn write() -> ToolCall {
        call("write_file", json!({ "path": "notes.txt", "content": "x" }))
    }

    #[test]
    fn yes_answers_under_the_floor_and_the_deny_rules() {
        let ctx = tools::ToolContext::default();
        let none = Ok(Vec::new());
        let yes = |call: &ToolCall, rules| answered(call, &ctx, rules, Mode::Bypass, false);
        assert_eq!(yes(&write(), &none), Some(Decision::Once));
        // Deletes the home folder, in sh and in PowerShell alike.
        let Some(Decision::Deny(why)) = yes(&command("Remove-Item -Recurse -Force ~"), &none)
        else {
            panic!("a hardline command got a yes");
        };
        assert!(why.contains("deletes the home folder"), "{why}");
        // Hard to take back, so it needs a person, and nobody is here.
        let Some(Decision::Deny(why)) = yes(&command("git reset --hard HEAD~1"), &none) else {
            panic!("a dangerous command got a yes with nobody to give it");
        };
        assert!(why.contains("nobody is here to approve it"), "{why}");
        let denied = Ok(vec![Rule {
            tool: "write_file".into(),
            pattern: String::new(),
            effect: permissions::Effect::Deny,
            project: None,
        }]);
        let Some(Decision::Deny(why)) = yes(&write(), &denied) else {
            panic!("a denied write got a yes");
        };
        assert!(
            why.contains("a permission rule denies `write_file`"),
            "{why}"
        );
    }

    #[test]
    fn without_yes_a_call_is_asked_at_the_keyboard_or_refused() {
        let ctx = tools::ToolContext::default();
        let none = Ok(Vec::new());
        let asked = |call: &ToolCall, mode, keyboard| answered(call, &ctx, &none, mode, keyboard);
        assert_eq!(asked(&write(), Mode::Default, true), None);
        let Some(Decision::Deny(why)) = asked(&write(), Mode::Default, false) else {
            panic!("a write got a yes with nobody to give it");
        };
        assert!(why.contains("nobody is here to give it"), "{why}");
        // Someone at the keyboard is asked about what the floor holds back.
        assert_eq!(
            asked(&command("git reset --hard HEAD~1"), Mode::Bypass, true),
            None
        );
    }

    /// Waits up to a second for `kept` to reach `times`.
    fn kept_by_then(kept: &std::sync::atomic::AtomicUsize, times: usize) -> usize {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        while kept.load(Ordering::SeqCst) < times && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        kept.load(Ordering::SeqCst)
    }

    #[test]
    fn the_first_turn_keeps_the_version_before_the_timer_does() {
        static KEPT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let keep = Keep::new(|| {
            KEPT.fetch_add(1, Ordering::SeqCst);
        });
        keep.after(std::time::Duration::from_millis(50));
        keep.now();
        assert_eq!(KEPT.load(Ordering::SeqCst), 1);
        keep.now();
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert_eq!(
            KEPT.load(Ordering::SeqCst),
            1,
            "kept again by a later turn or the timer"
        );
    }

    #[test]
    fn the_timer_keeps_the_version_when_no_turn_has_finished() {
        static KEPT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let keep = Keep::new(|| {
            KEPT.fetch_add(1, Ordering::SeqCst);
        });
        keep.after(std::time::Duration::from_millis(10));
        assert_eq!(kept_by_then(&KEPT, 1), 1);
        keep.now();
        assert_eq!(
            KEPT.load(Ordering::SeqCst),
            1,
            "kept again by the turn after it"
        );
    }
}
