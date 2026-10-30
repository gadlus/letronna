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
mod cloud;
mod commands;
mod control;
mod diff;
mod doctor;
mod edit;
mod history;
mod input;
mod install;
mod mcp;
mod memory;
mod notify;
mod picture;
mod presence;
mod render;
mod screen;
mod serve;
mod session;
mod setup;
mod spinner;
mod syntax;
mod title;
mod wallet;

use args::{Args, HELP, Output, Start, parse};
use banner::{banner, folder_name, footer, replay};
use commands::{Flow, command, offer_added, resume};
use letronna_core::approvals::{self, Decision, Request};
use letronna_core::budget::Budget;
use letronna_core::cancel::Cancel;
use letronna_core::chat::{self, Message};
use letronna_core::driver::{self, Allow, Outcome, Report, Spec, Status};
use letronna_core::gate::Mode;
use letronna_core::store::Transcript;
use letronna_core::{
    agent, cache_miss, crew, hooks, inbox, prompt_rules, store, structured, tools, update, usage,
    worktree,
};
use serde_json::{Value, json};
use session::Runner;
use std::io::IsTerminal;
use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
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
/// Holds the whole seconds a `-p` run may take from the start. A harness
/// that kills a task at a time limit sets it a little under that limit, so
/// the run is warned in time and ends with its result line before the kill.
const DEADLINE: &str = "LETRONNA_DEADLINE";
/// The share of a deadline gone when the run is told to write what it owes,
/// which leaves it a fifth, two minutes of ten, to write it and check it.
const WARN_AT: f64 = 0.8;
/// Four times a conversation's 25 tool rounds, since nobody is there to say
/// "continue" and a task left to run on its own, such as a fix across a
/// repository, often reads and runs more than 25 times.
const PRINT_ROUNDS: usize = 100;

fn main() {
    // The first mark, from which the others are timed.
    letronna_core::diag::mark("start");
    // What a deadline counts from, as whoever started this counts it.
    let started = std::time::Instant::now();
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
    // Before an update goes in or the start counts, as for `--version`: a
    // runner asks this of a build it is about to measure, and only reads.
    if args.dump_config {
        match dump_config(&args, &store::current_settings()) {
            Ok(config) => println!("{config:#}"),
            Err(error) => {
                eprintln!("{}", render::red(&error));
                std::process::exit(1);
            }
        }
        return;
    }
    // Before anything reads a key, so each comes from the vault and any
    // still in a file is moved there.
    letronna_core::vault::open();
    if let Some(words) = &args.mcp {
        std::process::exit(mcp::run(words));
    }
    if let Some(words) = &args.cloud {
        std::process::exit(cloud::run(words));
    }
    if let Some(words) = &args.wallet {
        std::process::exit(wallet::run(words));
    }
    if args.acp {
        let client = match args.acp_phone {
            true => acp::Client::Phone,
            false => acp::Client::Editor,
        };
        std::process::exit(acp::run(client));
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
    // is trusted, and which must not count as a start. Not for a `-p` run
    // either: a script asking one question after another would have the
    // program change under it part way, and its failed answers, often the
    // model's fault, would put a working version back.
    if !args.once()
        && let Some(program) = update::on_start()
    {
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
    // A rule named wrongly would be told all the same, and the run would
    // read as one without it.
    if args.once()
        && let Err(error) = prompt_rules::check()
    {
        eprintln!("{}", render::red(&error));
        std::process::exit(1);
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
        // Only at the prompt: a `-p` answer may be piped where Markdown shows.
        plain_terminal: !args.once(),
        way_out: None,
    };
    letronna_core::diag::mark("listed");
    // At the prompt, what is sent goes into the folder's history, which
    // up and ctrl-r reach from later sessions too.
    runner.bar.keep_history(!args.once());
    // With no flag picking one, the mode the window starts sessions in, so
    // the two agree; a `-p` run keeps to its flags alone.
    if !args.mode_picked && !args.once() {
        runner.mode = runner.store.settings.mode;
    }
    // At the prompt the mode leads the footer, where shift-tab changes it.
    if runner.plain_terminal {
        let mode = runner.mode;
        runner.bar.set_mode(mode);
    }
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
    // and so does serving, which nobody sits at. A question the environment
    // routes has its server and model from there, and needs none of it; so
    // does the terminal on a thrown-away machine, as in Letronna Cloud.
    let routed = env_routes(&args, disposable())
        && given_route(&args, |name| std::env::var(name).ok()).is_some();
    if routed && !args.once() {
        take_route(&mut runner, &args);
    }
    let first_run = runner.store.settings.model.is_empty() && !routed;
    if first_run && (args.once() || args.serve) {
        let error = "Nothing is set up yet. Run `letronna setup` first.";
        show(args.output, &failed(error.into()), None, false);
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
                    None,
                    false,
                );
                std::process::exit(1);
            }
        },
        _ => runner.open(false, workspace.clone()),
    };
    // Before anything runs in it: a session that cannot have its worktree
    // must not work in the checkout instead.
    let conversation = &mut runner.store.conversations[session];
    let placed = if args.worktree {
        let name = format!("session-{}", conversation.id);
        worktree::isolate(conversation, &name)
    } else {
        worktree::resume(conversation)
    };
    if let Err(error) = placed {
        runner.store.release_unused();
        show(args.output, &failed(error), None, false);
        std::process::exit(1);
    }
    // The one it opened with, whose worktree is seen to on the way out
    // even when another session is open by then.
    let opened = runner.store.conversations[session].id;
    choose_by_flags(&mut runner, session, gateway, &args, routed);
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
                let finished = ask_once(&mut runner, session, prompt, &args, &interrupted, started);
                if finished {
                    keep.now();
                }
                i32::from(!finished)
            }
            None => control::run(&mut runner, session, &args, started),
        };
        // A command still running, a browser with no window, or a job started
        // in the background, would outlive this run unseen.
        letronna_core::tools::end_commands(END_WAIT);
        letronna_core::browser::close_hidden();
        letronna_core::jobs::stop_all();
        letronna_core::mcp::stop_all();
        letronna_core::lsp::stop_all();
        letronna_core::on_device::runtime::quit();
        // On stderr, which a script reading the answer leaves alone.
        for kept in left_worktrees(&runner, &[opened]) {
            eprintln!("{kept}");
        }
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
    title::save();
    title::idle();
    // The window has to be ours before the banner, or the banner goes to the
    // screen we are about to swap away from.
    let note = footer(&runner);
    runner.bar.set_footer(note);
    // A SIGTERM or a closing terminal ends the session as leaving does,
    // with the terminal given back and its jobs and worktrees seen to,
    // rather than leaving it in raw mode on the conversation's screen; a
    // second SIGTERM ends it at once.
    let ended = Cancel::default();
    end_on_signals(ended.clone());
    runner.bar.end_on(ended);
    runner.bar.install();
    letronna_core::diag::mark("screen");
    // Before anything can start a turn. Without the box, a terminal on both
    // ends is still someone to ask.
    runner.serve_helpers(std::io::stdin().is_terminal() && std::io::stdout().is_terminal());
    render::clear();
    banner(&runner, &folder, session);
    let (on, model) = runner.store.model(session);
    let keys = store::load_keys();
    let ready = letronna_core::gateway::ready(&keys, &runner.store.settings.custom_endpoint);
    let ready: Vec<&str> = ready.iter().map(|(name, ..)| *name).collect();
    let hint = commands::unpaid_hint(on, model, &ready);
    // The hint asked the account service again, which may have opened the
    // model the footer said was closed.
    let note = footer(&runner);
    runner.bar.set_footer(note);
    if let Some(hint) = hint {
        render::line(&render::dim(&hint));
        // What the file says of the plan is as of the last check; asked
        // again in the background, an upgrade since shows from the next
        // command on.
        std::thread::spawn(|| {
            let _ = letronna_core::auth::refresh();
        });
    }
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
    // Painting is throttled, and nothing else paints until a key: without
    // this a resumed session's conversation stayed off the screen until
    // the first one was pressed.
    screen::settle();
    if let Some(tree) = &runner.store.conversations[session].worktree {
        render::line(&render::dim(&format!(
            "Working in worktree {} on branch {}; your checkout is left as it is.",
            tree.path.display(),
            tree.branch
        )));
    }

    letronna_core::diag::mark("ready");

    // Only a conversation that goes on hears about work that ends after its
    // turn; `-p` has left by now, with its answer.
    runner.bar.listen();
    // The open session's wake-ups fire here, one missed meanwhile at once.
    letronna_core::wake::serve([runner.store.conversations[session].id]);
    runner.ended = Some(letronna_core::notify::listen());
    // Likewise only a terminal that stays open runs the schedules, unless the
    // window already does. A run lands in the sessions, where /resume finds
    // it, and is told at the idle prompt when it ends.
    let (_schedules, events) = letronna_core::schedule::Runner::start();
    runner.bar.hear_schedules(events);
    let here = session_folder(&runner, session, workspace.as_deref());
    offer_added(&mut runner, here.as_deref());
    loop {
        // Before every wait, so a session opened, named or renamed by
        // whatever ran last shows on the box's edge and in the title.
        show_session(&mut runner, session);
        // And a Custom address the window saved meanwhile is where the next
        // message goes, as the keys are read when it is sent.
        runner.store.follow_custom();
        let Some(waited) = runner.bar.wait() else {
            break;
        };
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
        if let bar::Waited::Named(id, was, name) = &waited {
            runner.named(*id, was, name);
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
                runner.show_failure(&error);
                screen::settle();
            }
            continue;
        };
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        // A line starting with `!` runs as a command here, as a shell would.
        if let Some(command) = line.strip_prefix('!') {
            commands::shell_line(&mut runner, session, command.trim());
            screen::settle();
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
            // changed meanwhile is offered without a restart; from the open
            // session's folder, which the command may have changed.
            let here = session_folder(&runner, session, workspace.as_deref());
            offer_added(&mut runner, here.as_deref());
            // Every turn follows its own session's helpers; this is for the
            // footer's count once a command has opened another.
            let open = runner.store.conversations[session].id;
            runner.bar.show_helpers_of(open);
            // Only the open session's wake-ups: another's would leave a
            // notice that starts nothing here.
            letronna_core::wake::serve([open]);
            show_session(&mut runner, session);
            if let Flow::Unknown = flow {
                let name = line.split_whitespace().next().unwrap_or(&line);
                render::line(&render::dim(&match runner.bar.meant(name) {
                    Some(meant) => format!(
                        "{name} is not a command. Did you mean {meant}? Start it with // to send it as text."
                    ),
                    None => format!(
                        "{line} is not a command. Type / to see what is, or start it with // to send it as text."
                    ),
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
        // Sent, it would only be refused for want of a plan or credit.
        if commands::hold_unpaid(&mut runner, session) {
            screen::settle();
            continue;
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
                runner.show_failure(&error);
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
    // After the jobs, which may have been writing in a helper's worktree.
    let helpers_kept = letronna_core::agents::stop_all();
    letronna_core::mcp::stop_all();
    letronna_core::lsp::stop_all();
    letronna_core::on_device::runtime::quit();
    letronna_core::browser::close_hidden();
    letronna_core::schedule::stop();
    runner.store.release_unused();
    title::restore();
    render::blank();
    // On the screen the terminal is given back, where the conversation was
    // not, which session it was and how to come back to it.
    if let Some(left) = runner.store.conversations.get(session)
        && left.length() > 0
    {
        render::line(&render::dim(&format!(
            "Saved \"{}\" · letronna -r {} to come back",
            plain_name(&left.title),
            left.id
        )));
    }
    let open = (runner.store.conversations.get(session)).map_or(opened, |c| c.id);
    for kept in left_worktrees(&runner, &[opened, open])
        .iter()
        .chain(&helpers_kept)
    {
        render::line(&render::dim(kept));
    }
    letronna_core::diag::stop();
}

/// The open session's folder, its worktree among them, else `start`, the
/// folder Letronna started in: where its commands and skills are read from.
fn session_folder(
    runner: &Runner,
    session: usize,
    start: Option<&std::path::Path>,
) -> Option<std::path::PathBuf> {
    (runner.store.conversations[session].workspace.clone())
        .or_else(|| start.map(|p| p.to_path_buf()))
}

/// How long a hangup leaves the session to finish leaving before the
/// process ends without it.
#[cfg(unix)]
const HANGUP_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Set `ended` at a SIGTERM or a hangup, so the session ends as leaving
/// does; a second SIGTERM ends the process at once. Not Ctrl-C, which is a
/// key while the box is up and, while the terminal is lent to an editor or
/// a sign-in, that program's to hear.
#[cfg(unix)]
fn end_on_signals(ended: Cancel) {
    use std::io::Read;
    use std::os::fd::IntoRawFd;
    use std::sync::atomic::{AtomicI32, Ordering};
    static HEARD: AtomicI32 = AtomicI32::new(-1);
    // A signal handler may not lock or allocate, so it only wakes the thread
    // below, which sets the token.
    extern "C" fn heard(signal: libc::c_int) {
        let byte = [signal as u8];
        // SAFETY: write is async-signal-safe, the byte is on this stack, and
        // the pipe stays open for the life of the process.
        unsafe { libc::write(HEARD.load(Ordering::Relaxed), byte.as_ptr().cast(), 1) };
    }
    let Ok((mut reader, writer)) = std::io::pipe() else {
        return;
    };
    HEARD.store(writer.into_raw_fd(), Ordering::Relaxed);
    // A closing terminal sends two hangups, one from its shell and one from
    // the kernel as the shell exits, so a second hangup does not end the
    // process the way a second SIGTERM does.
    for (signal, once) in [(libc::SIGTERM, true), (libc::SIGHUP, false)] {
        // SAFETY: all zeroes is a valid sigaction, for the current one to be
        // read into and for the new one, whose mask is emptied below.
        let (mut before, mut action): (libc::sigaction, libc::sigaction) =
            unsafe { (std::mem::zeroed(), std::mem::zeroed()) };
        // SAFETY: this only reads the current action.
        unsafe { libc::sigaction(signal, std::ptr::null(), &mut before) };
        // Ignored on purpose, as nohup does with a hangup.
        if before.sa_sigaction == libc::SIG_IGN {
            continue;
        }
        action.sa_sigaction = heard as *const () as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART | if once { libc::SA_RESETHAND } else { 0 };
        // SAFETY: the mask is the action's own, and the action is whole when
        // set.
        unsafe {
            libc::sigemptyset(&mut action.sa_mask);
            libc::sigaction(signal, &action, std::ptr::null_mut());
        }
    }
    std::thread::spawn(move || {
        let mut signal = [0u8];
        while reader.read(&mut signal).is_ok_and(|read| read == 1) {
            ended.set();
            // A hangup is the terminal gone, and reading a terminal that has
            // gone never returns, so the box may never see `ended`: it gets
            // a moment to leave as it would, and then the process ends as a
            // hangup always ended it, rather than spinning on for good.
            if i32::from(signal[0]) == libc::SIGHUP {
                std::thread::sleep(HANGUP_GRACE);
                std::process::exit(128 + libc::SIGHUP);
            }
        }
    });
}

/// A console closing ends the process as it always has.
#[cfg(not(unix))]
fn end_on_signals(_: Cancel) {}

/// A session's name as one plain line, for saying on the way out.
fn plain_name(title: &str) -> String {
    let spaced: String = (title.chars())
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    match letronna_core::tools::one_line(&spaced) {
        name if name.is_empty() => "untitled".to_owned(),
        name => name,
    }
}

/// Show which session is open: its name on the box's edge and in the
/// window's title, and its folder for `@` paths and the edge's git state.
fn show_session(runner: &mut Runner, session: usize) {
    let conversation = &runner.store.conversations[session];
    let (name, folder) = (conversation.title.clone(), conversation.workspace.clone());
    runner.bar.set_folder(folder);
    runner.bar.set_session(&name);
    // Read again each time, as `/remember`'s list can make the chat private
    // and `/resume` or `/branch` can open one.
    runner.bar.set_private(is_private(&runner.store, session));
    runner.show_context(session);
}

/// Whether the session is a private chat, as the window names one: it
/// saves to memory only when asked and keeps only its own notes.
fn is_private(store: &store::Store, session: usize) -> bool {
    use letronna_core::memory::{Saving, Scope};
    store.memory_saving(session) == Saving::WhenAsked && store.memory_scope(session) == Scope::Chat
}

/// Finishes the worktrees the sessions `ids` work in, now that nothing this
/// process started runs there, giving the line for each kept for its work.
fn left_worktrees(runner: &Runner, ids: &[u64]) -> Vec<String> {
    (runner.store.conversations.iter())
        .filter(|conversation| ids.contains(&conversation.id))
        .filter_map(|conversation| conversation.worktree.as_ref()?.finish(&[]))
        .collect()
}

/// Runs one prompt on the driver, stopped when `interrupted` is set or at
/// the deadline counted from `started`, and prints what came of it in the
/// form asked for, saying whether it ended without failing.
fn ask_once(
    runner: &mut Runner,
    session: usize,
    prompt: &str,
    args: &Args,
    interrupted: &Cancel,
    started: std::time::Instant,
) -> bool {
    let budget = args.budget.map(|limit| Arc::new(Budget::new(limit, None)));
    // Before the prompt goes on the session, which a mistake leaves alone.
    let deadline = match deadline_of(std::env::var(DEADLINE).ok().as_deref(), started) {
        Ok(deadline) => deadline,
        Err(error) => {
            show(args.output, &failed(error), None, false);
            return false;
        }
    };
    // Named while the session is in the store, which the run takes it out of.
    let opening = init(runner, session);
    let (mut spec, context, before) = match begin(runner, session, prompt, args, budget) {
        Ok(begun) => begun,
        Err(error) => {
            show(args.output, &failed(error), None, false);
            return false;
        }
    };
    // A child, so a decline at the keyboard that stops the run does not read
    // as a Ctrl-C.
    spec.cancel = interrupted.child();
    // The run times its replies against it too, not only stops at it, and
    // is warned before it.
    spec.connection.deadline = deadline;
    spec.connection.warn_at = deadline.map(|deadline| warning(started, deadline));
    let id = spec.id;
    // Stopped through the run's own token, not `interrupted`, so a run out
    // of time exits 0 with what it has rather than as a Ctrl-C does.
    let ran_out = Arc::new(AtomicBool::new(false));
    if let Some(deadline) = deadline {
        hold_to_deadline(deadline, spec.cancel.clone(), ran_out.clone());
    }
    // Served before the run starts, since a request with nobody serving is
    // declined at once.
    let keyboard = args.output == Output::Text && render::interactive();
    // Its own gate asks only when someone can answer.
    spec.connection.human = keyboard;
    // The gate's refusals reach only the model, so a script that left out
    // --yes would otherwise see an answer that changed nothing and no why.
    say_unanswered(spec.connection.mode, keyboard);
    answer_approvals(keyboard, context, spec.cancel.clone());
    if args.output == Output::StreamJson {
        emit(&opening);
    }
    let prompt = prompt_rules::seen(&spec.connection).unwrap_or_default();
    // The run's own, which the environment's route and --model may have set
    // apart from the session's.
    let connection = spec.connection.clone();
    let report = run_following(spec, args.output);
    let ran_out = ran_out.load(Ordering::SeqCst);
    finish(
        runner,
        session,
        (id, before),
        report,
        args.output,
        (&connection, prompt),
        ran_out,
    )
}

/// The deadline `value` sets for a run started at `started`, or None when it
/// is unset or too far off for the clock to hold.
fn deadline_of(
    value: Option<&str>,
    started: std::time::Instant,
) -> Result<Option<std::time::Instant>, String> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    match value.parse::<u64>() {
        Ok(seconds) if seconds > 0 => {
            Ok(started.checked_add(std::time::Duration::from_secs(seconds)))
        }
        _ => Err(format!(
            "{DEADLINE} takes the whole seconds a run may take, above 0, like {DEADLINE}=570."
        )),
    }
}

/// When a run started at `started` is warned that most of its time to
/// `deadline` has gone, which the run tells its model itself, so the time
/// left is counted as the model reads it.
fn warning(started: std::time::Instant, deadline: std::time::Instant) -> std::time::Instant {
    started + deadline.saturating_duration_since(started).mul_f64(WARN_AT)
}

/// Stops a run through `cancel` at `deadline`, unless `cancel` is set
/// first, setting `ran_out` when it does.
fn hold_to_deadline(
    deadline: std::time::Instant,
    cancel: Cancel,
    ran_out: Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        if cancel.sleep(deadline.saturating_duration_since(std::time::Instant::now())) {
            ran_out.store(true, Ordering::SeqCst);
            cancel.set();
        }
    })
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
    // Read on each run and put nowhere else: not in the settings, not with
    // the keys, so the key lives in this process alone.
    if let Some(route) = given_route(args, |name| std::env::var(name).ok()) {
        (connection.endpoint, connection.key, connection.model) = route?;
    }
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
    // Before the prompt goes on the session, so a schema that cannot be read
    // or checked leaves nothing behind.
    let schema = args.schema.as_deref().map(answer_schema).transpose()?;
    let conversation = &mut runner.store.conversations[session];
    // One turn and out: nobody is there to watch a loop or stop it.
    if conversation.goal.is_some() {
        eprintln!(
            "This session has a goal, but goals are pursued only in the terminal and the \
             window, not with -p: this runs one turn."
        );
    }
    if conversation.messages.is_empty() && conversation.title.is_empty() {
        conversation.title = crate::session::first_line(prompt);
    }
    conversation.updated = chat::now();
    let content = letronna_core::references::expand(prompt, conversation.workspace.as_deref());
    conversation.messages.push(Message {
        role: "user".into(),
        content,
        ..Default::default()
    });
    runner.persist();
    runner.hold_memory(session);
    // The run writes on into the session's own file, so the session is
    // listed and can be continued as before. It leaves the store here and
    // comes back only when the run is over, so that file has one writer.
    let conversation = runner.store.conversations.remove(session);
    let (id, before) = (conversation.id, conversation.messages.len());
    // What the user asked for, which Auto mode's judge reads as their wishes.
    connection.typed = letronna_core::judge::typed(&conversation.messages).into();
    // A run that did not name its tools gets Letronna's less used ones on
    // demand, which keeps each request some 3,000 tokens smaller; one that
    // named them gets what it named in the array.
    connection.lean_tools = args.tools.is_none();
    // It stops its jobs on the way out, so a job's start says so, and how to
    // start a server that must outlive it.
    connection.jobs_end_with_run = true;
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
        schema,
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

/// The settings and limits a `-p` run in a new session gets from `args` over
/// `settings`, as `--dump-config` prints them.
fn dump_config(args: &Args, settings: &store::Settings) -> Result<Value, String> {
    let mode = args.mode.clone()?;
    // As `choose_by_flags` puts them on the session, which takes the
    // settings' otherwise.
    let gateway = match args.gateway.as_deref() {
        Some(name) => gateway_named(name)?,
        None => settings.gateway.as_str(),
    };
    let model = args.model.as_deref().unwrap_or(&settings.model);
    Ok(json!({
        "data_dir": chat::data_dir().display().to_string(),
        "model": model,
        "gateway": gateway,
        "mode": mode.name(),
        "max_rounds": rounds(args),
        "tools": args.tools,
        "budget": args.budget,
        "context_cap": chat::cap_tokens(settings.context_cap),
        "verify_edits": settings.verifies_edits(),
        "command_timeout_seconds": tools::COMMAND_TIMEOUT.as_secs(),
        "tool_output_bytes": tools::MAX_OUTPUT,
        // As a run whose answer is piped is told them.
        "prompt": prompt_rules::seen(&chat::Connection {
            tools: true,
            model: model.to_owned(),
            mode,
            plan: mode == Mode::Plan,
            ..Default::default()
        })?,
    }))
}

/// The JSON Schema `--json-schema` gave, read from the file it names unless
/// it is the schema itself, when the driver can check answers against it.
fn answer_schema(given: &str) -> Result<Value, String> {
    // Other agents' command lines take the schema itself after this flag, so
    // a program written for one of them can pass it as it is.
    let text = if given.trim_start().starts_with('{') {
        given.to_owned()
    } else {
        std::fs::read_to_string(given)
            .map_err(|e| format!("Could not read the schema in {given}: {e}"))?
    };
    let schema =
        serde_json::from_str(&text).map_err(|e| format!("The answer's schema is not JSON: {e}"))?;
    // The driver makes this same check, but only once the prompt is on the
    // session and saved.
    structured::check_schema(&schema).map_err(|problems| {
        format!(
            "The answer's schema cannot be checked: {}",
            problems.join("; ")
        )
    })?;
    Ok(schema)
}

/// Runs `spec` to its end on this thread, with progress lines while it runs
/// when the output is stream-json, and each retry or switch of model said
/// on stderr.
fn run_following(spec: Spec, output: Output) -> Report {
    let (running, ended) = mpsc::channel::<()>();
    let progress = show_progress(spec.status.clone(), ended, output == Output::StreamJson);
    let report = driver::run(spec);
    drop(running);
    let _ = progress.join();
    report
}

/// Takes the session back from run `id`, whose messages follow the first
/// `before`, and prints how the run over `connection` ended with what it was
/// told of the prompt's rules, and whether its deadline stopped it, saying
/// whether it ended without failing.
fn finish(
    runner: &mut Runner,
    session: usize,
    (id, before): (u64, usize),
    mut report: Report,
    output: Output,
    (connection, prompt): (&chat::Connection, Value),
    ran_out: bool,
) -> bool {
    // On stderr, apart from the answer: what a hook said or could not do.
    for problem in hooks::problems() {
        eprintln!("{problem}");
    }
    // The report's text is the last thing said in the whole session, which
    // after -c is an earlier answer when this run said nothing.
    let mut extras = None;
    if let Some(said) = take_back(runner, session, id, before) {
        report.text = said;
        extras = Some(Extras::of(runner, session, before, connection, prompt));
    }
    show(output, &report, extras.as_ref(), ran_out);
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
        changes: Vec::new(),
        jobs: Vec::new(),
        contract: Vec::new(),
        value: None,
    }
}

/// Prints how a run ended in the form asked for, with `extras` in the JSON
/// forms, so a script reading JSON gets JSON for a run that never began too.
/// A run stopped once it `ran_out` of time says so.
fn show(output: Output, report: &Report, extras: Option<&Extras>, ran_out: bool) {
    match output {
        Output::Text => print_answer(report, ran_out && report.status == Outcome::Stopped),
        Output::Json => {
            emit(&result_of(report, extras, ran_out));
        }
        Output::StreamJson => {
            let mut result = result_of(report, extras, ran_out);
            result["type"] = "result".into();
            emit(&result);
        }
    }
}

/// `as_json` for a run that ended, whose status is "deadline" rather than
/// "stopped" when it was stopped once it `ran_out` of time, so a harness can
/// tell its own time limit from a stop it made.
fn result_of(report: &Report, extras: Option<&Extras>, ran_out: bool) -> Value {
    let mut result = as_json(report, extras);
    if ran_out && report.status == Outcome::Stopped {
        result["status"] = "deadline".into();
    }
    result
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

/// Says on stderr, when a run in Default `mode` has no `human` to answer for
/// it, that what needs a yes will be refused.
fn say_unanswered(mode: Mode, human: bool) {
    if mode == Mode::Default && !human {
        eprintln!(
            "Nobody can approve calls in this run, so in Default mode each write or command \
             that needs a yes will be refused; run with --yes, or --mode auto to have them judged."
        );
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

/// Writes a stream-json progress line, when `json`, whenever the run's
/// rounds or tokens have moved on, and says each retry or switch of model
/// on stderr, until `ended` hears that the run is over.
fn show_progress(
    status: Arc<Status>,
    ended: mpsc::Receiver<()>,
    json: bool,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut shown = progress(&status);
        let mut notes = status.notes.load(Ordering::Relaxed);
        let tell = |notes: &mut u64| {
            // On stderr, apart from the answer, whatever the output: waiting
            // out a gateway that keeps failing was minutes of silence, the
            // provider's words say whether waiting can help, and an answer
            // from another gateway than the one asked is worth knowing.
            let now = status.notes.load(Ordering::Relaxed);
            if now != *notes {
                *notes = now;
                if let Ok(said) = status.note.lock() {
                    // Dim only on a screen: stderr kept in a file stays plain.
                    match std::io::stderr().is_terminal() {
                        true => eprintln!("{}", render::dim(&said)),
                        false => eprintln!("{said}"),
                    }
                }
            }
        };
        while let Err(mpsc::RecvTimeoutError::Timeout) = ended.recv_timeout(PROGRESS_EVERY) {
            tell(&mut notes);
            if !json {
                continue;
            }
            let now = progress(&status);
            if now != shown {
                if !emit(&now) {
                    return;
                }
                shown = now;
            }
        }
        // A switch made just before the end is said too.
        tell(&mut notes);
    })
}

/// The stream-json line a run opens with: the session at `session`, by the
/// id `--resume` takes, and the model and gateway it runs on.
fn init(runner: &Runner, session: usize) -> Value {
    let (gateway, model) = runner.store.model(session);
    json!({
        "type": "init",
        "session": runner.store.conversations[session].id,
        "model": model,
        "gateway": gateway,
    })
}

/// A run's progress so far, as a stream-json line.
fn progress(status: &Status) -> Value {
    json!({
        "type": "progress",
        "rounds": status.rounds.load(Ordering::Relaxed),
        "input": status.input.load(Ordering::Relaxed),
        "cached": status.cached.load(Ordering::Relaxed),
        // With what is streaming now, at about four characters to a token,
        // so the count moves while a long reply is still being written.
        "output": status.output.load(Ordering::Relaxed) + status.streamed.load(Ordering::Relaxed) / 4,
        "doing": status.doing.lock().map(|d| d.clone()).unwrap_or_default(),
    })
}

/// Prints the answer as the conversation would, or its checked value as JSON,
/// and a line after it when the run did not finish, `timed_out` or not.
fn print_answer(report: &Report, timed_out: bool) {
    // With a schema the checked value is what was asked for, and one line of
    // JSON is what a script reads; the words around it are left out.
    if let Some(value) = &report.value {
        emit(value);
    } else if !report.text.is_empty() {
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
        Outcome::Stopped if timed_out => render::dim(&format!(
            "Stopped: the seconds {DEADLINE} gives the run are up."
        )),
        Outcome::Stopped => render::dim("Stopped."),
    };
    render::line(&note);
}

/// What a run came to, as `--output json` prints it, with `extras` once it
/// has begun.
fn as_json(report: &Report, extras: Option<&Extras>) -> Value {
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
    // Only when a file the request asked for is still wrong, so a result
    // with nothing open reads as it always has.
    if !report.contract.is_empty() {
        out["contract"] = report.contract.clone().into();
    }
    if let Some(extras) = extras {
        out["session"] = extras.session.into();
        out["model"] = extras.model.as_str().into();
        out["gateway"] = extras.gateway.as_str().into();
        out["window"] = json!({
            "tokens": extras.window.tokens,
            "source": extras.window.source,
        });
        out["cost_usd"] = extras.cost.into();
        out["refused"] = extras.refused.into();
        out["prompt"] = extras.prompt.clone();
        out["progress_notes"] = extras.progress.notes.into();
        out["first_edit_round"] = extras.progress.first_edit_round.into();
        out["rounds_after_last_edit"] = extras.progress.rounds_after_last_edit.into();
        out["cache_misses"] = json!({
            "count": extras.cache_misses.count,
            "lost_tokens": extras.cache_misses.lost_tokens,
        });
        out["history_rewrites"] = extras.history_rewrites.into();
    }
    if let Some(value) = &report.value {
        out["value"] = value.clone();
    }
    out
}

/// What the JSON says about a run beyond its report, read from the session
/// it ran in.
struct Extras {
    /// The session's id, which `--resume` takes.
    session: u64,
    model: String,
    gateway: String,
    /// The window its requests were sized by, and where that came from.
    window: chat::Window,
    /// What the run's own model calls cost in US dollars, over those whose
    /// cost is known; None when no call's is.
    cost: Option<f64>,
    /// The run's own calls that were refused rather than run.
    refused: usize,
    /// The prompt rules left out of the run and a hash of those told.
    prompt: Value,
    /// When the run first changed the project, how long it went on after
    /// its last change, and how often it was asked where it stood.
    progress: agent::Progress,
    /// The provider's own cache drops over the run's requests, with the
    /// tokens they cost, so its cache figures can be read net of them.
    cache_misses: cache_miss::Drops,
    /// The run's replies whose cache miss an earlier message changing
    /// caused: old results cleared, or reasoning no longer sent back.
    history_rewrites: usize,
}

impl Extras {
    /// The extras of the run over `connection` whose messages follow the
    /// first `before` in the session at `session`, told `prompt` of the rules.
    fn of(
        runner: &Runner,
        session: usize,
        before: usize,
        connection: &chat::Connection,
        prompt: Value,
    ) -> Extras {
        let conversation = &runner.store.conversations[session];
        let own = conversation.messages.get(before..).unwrap_or_default();
        let (gateway, _) = runner.store.model(session);
        Extras {
            session: conversation.id,
            model: connection.model.trim().to_owned(),
            gateway: gateway.to_owned(),
            window: chat::window(connection),
            cost: cost(own),
            refused: own.iter().filter(|message| refused(message)).count(),
            prompt,
            progress: agent::progress(own),
            cache_misses: cache_miss::drops(conversation.id),
            history_rewrites: history_rewrites(own),
        }
    }
}

/// How many of the model calls behind `messages` missed the cache because
/// an earlier message changed.
fn history_rewrites(messages: &[Message]) -> usize {
    messages
        .iter()
        .filter(|message| message.cache_miss == Some(cache_miss::Reason::History))
        .count()
}

/// What the model calls behind `messages` cost in US dollars, over those
/// whose cost is known, or None when no call's is.
fn cost(messages: &[Message]) -> Option<f64> {
    let mut spent = usage::Totals::default();
    for (_, call) in messages.iter().flat_map(usage::calls) {
        spent.add(&call);
    }
    (spent.unpriced < spent.calls).then_some(spent.cost)
}

/// Whether `result` answers a call that was refused rather than run.
fn refused(result: &Message) -> bool {
    result.role == "tool" && result.is_error && letronna_core::gate::refused(&result.content)
}

/// The gateway `name` means, in any case.
fn gateway_named(name: &str) -> Result<&'static str, String> {
    let gateways = &letronna_core::gateway::gateways();
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

/// Whether `AGENT_BASE_URL` and the rest are read: always for `-p`, and in
/// the terminal only on a machine `LETRONNA_DISPOSABLE` says is thrown away
/// after, such as a Letronna Cloud container, so a shell that happens to
/// export them changes nothing on someone's own computer.
fn env_routes(args: &Args, disposable: bool) -> bool {
    args.once() || disposable
}

fn disposable() -> bool {
    std::env::var_os("LETRONNA_DISPOSABLE").is_some_and(|value| value == "1")
}

/// The environment's route made the terminal's, for this process alone: the
/// Custom gateway at its server with its model, and its key held in memory
/// (`session::GIVEN_KEY`). A server with no model stops here, as `-p` does.
fn take_route(runner: &mut Runner, args: &Args) {
    let Some(route) = given_route(args, |name| std::env::var(name).ok()) else {
        return;
    };
    let (endpoint, key, model) = match route {
        Ok(route) => route,
        Err(error) => {
            eprintln!("{}", render::red(&error));
            std::process::exit(1);
        }
    };
    let _ = session::GIVEN_KEY.set(key);
    let settings = &mut runner.store.settings;
    settings.gateway = route_gateway(&endpoint, letronna_core::auth::signed_in()).into();
    settings.custom_endpoint = endpoint;
    settings.model = model;
}

/// The gateway the environment's server is: Letronna's own when it is the
/// account API and someone is signed in (as `LETRONNA_TOKEN` does in a cloud
/// terminal), so it lists its models and sends the sign-in; Custom otherwise.
fn route_gateway(endpoint: &str, signed_in: bool) -> &'static str {
    match letronna_core::gateway::named(endpoint) {
        Some(letronna_core::gateway::LETRONNA) if signed_in => letronna_core::gateway::LETRONNA,
        _ => "Custom",
    }
}

/// The Custom route, as (endpoint, key, model), that `AGENT_BASE_URL`,
/// `AGENT_API_KEY` and `--model` or `AGENT_MODEL` give a `-p` run when `var`
/// reads them and `--gateway` names no gateway of its own.
fn given_route(
    args: &Args,
    var: impl Fn(&str) -> Option<String>,
) -> Option<Result<(String, String, String), String>> {
    // A gateway named for this run is the nearer choice, as --model is.
    if args.gateway.is_some() {
        return None;
    }
    let given = |name: &str| {
        var(name)
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    };
    let endpoint = given("AGENT_BASE_URL")?;
    // The model set up belongs to another gateway, so it is not guessed at.
    let Some(model) = args.model.clone().or_else(|| given("AGENT_MODEL")) else {
        return Some(Err(
            "AGENT_BASE_URL is set, but no model is: set AGENT_MODEL or pass --model.".into(),
        ));
    };
    Some(Ok((
        endpoint,
        given("AGENT_API_KEY").unwrap_or_default(),
        model,
    )))
}

/// Put the session on the model, gateway and effort the flags name. Only the
/// session takes them: the settings, and with them new sessions, stay as
/// they were.
fn choose_by_flags(
    runner: &mut Runner,
    session: usize,
    gateway: Option<&str>,
    args: &args::Args,
    routed: bool,
) {
    // A model named for a run the environment routes is that server's, which
    // nothing keeps: pinned, it would be asked of the gateway set up later.
    if (gateway.is_some() || args.model.is_some()) && !routed {
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
    // The session's own, as --model and --effort are, and before the flags
    // that change one part of the preset.
    if args.private {
        runner.store.make_private(session);
    }
    if let Some(saving) = args.memory {
        runner.store.choose_memory_saving(session, Some(saving));
    }
    if let Some(scope) = args.memory_scope {
        runner.store.choose_memory_scope(session, Some(scope));
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
    // A watch that found nothing new would say so every few minutes.
    if run.quiet {
        return;
    }
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
        // A thread of a tool or a timer can panic and be caught, with the
        // box still up and in use: only the main thread's panic, which ends
        // the program, gives the terminal back.
        if std::thread::current().name() != Some("main") {
            previous(info);
            return;
        }
        // Out of the conversation's own screen, mouse and key reports off,
        // the keys' protocol popped, pastes as typed, focus reports off, the
        // full scrolling region and the cursor back -- so the message below
        // lands where it can be read and stays there. Only on a terminal: a
        // program reading JSON lines would get them glued on.
        if std::io::stdout().is_terminal() {
            print!("\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l");
            screen::forget_keyboard();
            print!("\x1b[?1049l\x1b[r\x1b[?2004l\x1b[?1004l\x1b[?25h");
            let _ = std::io::stdout().flush();
        }
        let _ = crossterm::terminal::disable_raw_mode();
        render::set_raw(false);
        title::restore();
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
            changes: Vec::new(),
            jobs: Vec::new(),
            contract: Vec::new(),
            value: None,
        }
    }

    #[test]
    fn json_output_carries_the_answer_rounds_and_tokens() {
        let out = as_json(&report(Outcome::Done), None);
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
            as_json(&report(Outcome::RoundLimit), None)["status"],
            "round_limit"
        );
        assert_eq!(
            as_json(&report(Outcome::OverBudget), None)["status"],
            "over_budget"
        );
        let failed = as_json(
            &report(Outcome::Failed("The key was refused.".into())),
            None,
        );
        assert_eq!(
            (&failed["status"], &failed["error"]),
            (&json!("failed"), &json!("The key was refused."))
        );
        assert!(out.get("error").is_none());
    }

    #[test]
    fn json_output_names_the_session_its_model_and_what_it_cost() {
        let mut extras = Extras {
            session: 137,
            model: "stub-model".into(),
            gateway: "Custom".into(),
            window: chat::Window {
                tokens: None,
                source: "none",
            },
            cost: Some(0.25),
            refused: 2,
            prompt: json!({"dropped": ["compute"], "rules_sha": "0123456789ab"}),
            progress: agent::Progress {
                first_edit_round: Some(4),
                rounds_after_last_edit: Some(3),
                notes: 1,
            },
            cache_misses: cache_miss::Drops {
                count: 2,
                lost_tokens: 7_100,
            },
            history_rewrites: 1,
        };
        let out = as_json(&report(Outcome::Done), Some(&extras));
        assert_eq!(
            (
                &out["session"],
                &out["model"],
                &out["gateway"],
                &out["cost_usd"],
                &out["refused"]
            ),
            (
                &json!(137),
                &json!("stub-model"),
                &json!("Custom"),
                &json!(0.25),
                &json!(2)
            )
        );
        assert_eq!(out["answer"], "The notes say hello.");
        assert_eq!(out["prompt"]["dropped"], json!(["compute"]));
        // A run sized by the small budget because nothing gave its window
        // says so, as does one whose window came from somewhere.
        assert_eq!(out["window"], json!({"tokens": null, "source": "none"}));
        extras.window = chat::Window {
            tokens: Some(1_000_000),
            source: "listed-alias",
        };
        let out = as_json(&report(Outcome::Done), Some(&extras));
        assert_eq!(
            out["window"],
            json!({"tokens": 1_000_000, "source": "listed-alias"})
        );
        // A cost nobody knows is said to be unknown, not nothing.
        extras.cost = None;
        let out = as_json(&report(Outcome::Done), Some(&extras));
        assert_eq!(out.get("cost_usd"), Some(&Value::Null));
        // How the run went: when it first changed the project, how long it
        // went on after its last change, and the notes asking where it stood.
        assert_eq!(
            (
                &out["progress_notes"],
                &out["first_edit_round"],
                &out["rounds_after_last_edit"]
            ),
            (&json!(1), &json!(4), &json!(3))
        );
        // A run that changed nothing says so, rather than leaving them out.
        extras.progress = agent::Progress::default();
        let out = as_json(&report(Outcome::Done), Some(&extras));
        assert_eq!(out["first_edit_round"], Value::Null);
        assert_eq!(out["rounds_after_last_edit"], Value::Null);
        assert_eq!(out["progress_notes"], 0);
        assert!(
            as_json(&report(Outcome::Done), None)
                .get("progress_notes")
                .is_none()
        );
        // What the cache did: the provider's own drops with the tokens they
        // cost, and the misses Letronna's own rewrites of history caused.
        assert_eq!(
            out["cache_misses"],
            json!({"count": 2, "lost_tokens": 7_100})
        );
        assert_eq!(out["history_rewrites"], 1);
        assert!(
            as_json(&report(Outcome::Done), None)
                .get("cache_misses")
                .is_none()
        );
    }

    #[test]
    fn the_history_rewrites_are_the_replies_whose_miss_an_earlier_message_caused() {
        let reply = |miss: Option<cache_miss::Reason>| Message {
            role: "assistant".into(),
            cache_miss: miss,
            ..Default::default()
        };
        let run = [
            reply(None),
            reply(Some(cache_miss::Reason::History)),
            reply(Some(cache_miss::Reason::Unknown)),
            reply(Some(cache_miss::Reason::History)),
        ];
        assert_eq!(history_rewrites(&run), 2);
        assert_eq!(history_rewrites(&[]), 0);
    }

    #[test]
    fn the_cost_adds_up_the_calls_whose_cost_is_known() {
        let reply = |cost: Option<f64>| Message {
            role: "assistant".into(),
            cost,
            ..Default::default()
        };
        let summarized = Message {
            summary_usage: Some(usage::Usage {
                cost: Some(0.5),
                ..Default::default()
            }),
            ..reply(Some(0.25))
        };
        let run = [reply(Some(0.125)), reply(None), summarized];
        assert_eq!(cost(&run), Some(0.875));
        assert_eq!(cost(&[reply(None)]), None);
        assert_eq!(cost(&[]), None);
    }

    #[test]
    fn a_call_refused_before_it_ran_is_told_from_one_that_ran_and_failed() {
        let result = |content: &str, is_error| Message {
            role: "tool".into(),
            name: "write_file".into(),
            content: content.into(),
            is_error,
            ..Default::default()
        };
        // The gate's own words, in each mode and by each rule that refuses.
        let ctx = tools::ToolContext::default();
        let none = Ok(Vec::new());
        let denied = Ok(vec![Rule {
            tool: "write_file".into(),
            pattern: String::new(),
            effect: permissions::Effect::Deny,
            project: None,
        }]);
        let gated = [
            (write(), &none, Mode::Default),
            (write(), &none, Mode::DontAsk),
            (write(), &none, Mode::Plan),
            (command("touch notes.txt"), &none, Mode::Plan),
            (write(), &denied, Mode::Bypass),
            (
                command("Remove-Item -Recurse -Force ~"),
                &none,
                Mode::Bypass,
            ),
        ];
        for (call, rules, mode) in gated {
            let Some(Decision::Deny(why)) = answered(&call, &ctx, rules, mode, false) else {
                panic!("{} was not refused in {mode:?}", call.name);
            };
            assert!(refused(&result(&why, true)), "{why}");
        }
        for why in [
            "`write_file` is not available to this helper.",
            "This was not approved, so it did not run. The user said why: not now",
            "Auto mode did not allow this: it deletes files.",
            "A PreToolUse hook blocked this call: no writes here",
            "Step 2 of the batch (click Pay) would be refused on its own, so none of it \
             ran: `use_page` is not available in plan mode: it changes things.",
        ] {
            assert!(refused(&result(why, true)), "{why}");
        }
        // A call that ran and failed, or whose output only reads like a refusal.
        let ran = [
            result("Could not read `notes.txt`: it is not there.", true),
            result("Refused: the server said no.", false),
            result("`gh` is not available on this computer.", true),
            Message {
                role: "assistant".into(),
                ..result("Refused: nothing.", true)
            },
        ];
        for message in ran {
            assert!(!refused(&message), "{}", message.content);
        }
    }

    #[test]
    fn json_output_carries_the_checked_value_only_when_there_is_one() {
        let mut answered = report(Outcome::Done);
        answered.value = Some(json!({"count": 3}));
        assert_eq!(as_json(&answered, None)["value"], json!({"count": 3}));
        assert!(as_json(&report(Outcome::Done), None).get("value").is_none());
    }

    #[test]
    fn a_schema_is_read_from_its_file_unless_it_is_given_itself() {
        let file =
            std::env::temp_dir().join(format!("letronna-schema-{}.json", std::process::id()));
        let path = file.to_str().unwrap();
        std::fs::write(&file, r#"{"type": "array"}"#).unwrap();
        assert_eq!(answer_schema(path), Ok(json!({"type": "array"})));
        std::fs::remove_file(&file).unwrap();
        assert!(
            answer_schema(path)
                .unwrap_err()
                .starts_with("Could not read the schema in")
        );
        let given = r#" {"type": "integer"}"#;
        assert_eq!(answer_schema(given), Ok(json!({"type": "integer"})));
        let broken = answer_schema("{type: integer}").unwrap_err();
        assert!(
            broken.starts_with("The answer's schema is not JSON"),
            "{broken}"
        );
    }

    #[test]
    fn a_schema_the_driver_cannot_check_is_turned_down_before_the_run() {
        assert_eq!(
            answer_schema(r#"{"type": "integer", "minimum": 0}"#),
            Err("The answer's schema cannot be checked: unsupported keyword \"minimum\"".into())
        );
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

    #[test]
    fn dump_config_prints_the_effective_limits() {
        let asked = |words: &[&str]| parse(words.iter().map(|w| w.to_string())).unwrap();
        let settings = store::Settings {
            gateway: "OpenRouter".into(),
            model: "x/y".into(),
            ..Default::default()
        };
        let mut plain = dump_config(&asked(&["--dump-config"]), &settings).unwrap();
        // Wherever the tests' data folder is, which another test can move.
        let data_dir = plain
            .as_object_mut()
            .and_then(|dump| dump.remove("data_dir"));
        assert!(data_dir.is_some_and(|dir| dir.is_string()));
        // Its hash changes with every rule's wording.
        let prompt = plain
            .as_object_mut()
            .and_then(|dump| dump.remove("prompt"))
            .unwrap();
        assert_eq!(prompt["dropped"], json!([]));
        assert_eq!(prompt["rules_sha"].as_str().map(str::len), Some(12));
        assert_eq!(
            plain,
            json!({
                "model": "x/y",
                "gateway": "OpenRouter",
                "mode": "default",
                "max_rounds": 100,
                "tools": null,
                "budget": null,
                "context_cap": 64_000,
                "verify_edits": true,
                "command_timeout_seconds": 120,
                "tool_output_bytes": 8_000,
            })
        );
        // What would stop the run stops the dump, rather than it guessing.
        let nowhere = dump_config(
            &asked(&["--dump-config", "--gateway", "nowhere"]),
            &settings,
        );
        assert!(
            nowhere
                .unwrap_err()
                .starts_with("There is no gateway called nowhere.")
        );
        let fast = dump_config(&asked(&["--dump-config", "--mode", "fast"]), &settings);
        assert!(fast.unwrap_err().starts_with("There is no `fast` mode"));

        // The flags win over the settings, as they do for the run.
        let flagged = asked(&[
            "--dump-config",
            "--model",
            "local-model",
            "--gateway",
            "ollama",
            "--yes",
            "--max-rounds",
            "40",
            "--tools",
            "read_file,edit_file",
            "--budget",
            "200000",
        ]);
        let tight = store::Settings {
            context_cap: 1_000,
            disabled_tools: vec!["run_command".into()],
            ..settings
        };
        let dumped = dump_config(&flagged, &tight).unwrap();
        assert_eq!(
            (&dumped["model"], &dumped["gateway"], &dumped["mode"]),
            (&json!("local-model"), &json!("Ollama"), &json!("bypass"))
        );
        assert_eq!(dumped["max_rounds"], 40);
        assert_eq!(dumped["tools"], json!(["read_file", "edit_file"]));
        assert_eq!(dumped["budget"], 200_000);
        // A cap under the floor gets the floor, and with run_command off the
        // edits are not checked.
        assert_eq!(dumped["context_cap"], 6_000);
        assert_eq!(dumped["verify_edits"], false);
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

    #[test]
    fn a_deadline_is_whole_seconds_from_the_start() {
        let started = std::time::Instant::now();
        assert_eq!(deadline_of(None, started), Ok(None));
        assert_eq!(deadline_of(Some(" "), started), Ok(None));
        assert_eq!(
            deadline_of(Some(" 570 "), started),
            Ok(Some(started + std::time::Duration::from_secs(570)))
        );
        for wrong in ["0", "-5", "1.5", "ten"] {
            let Err(error) = deadline_of(Some(wrong), started) else {
                panic!("{DEADLINE}={wrong} was taken");
            };
            assert!(error.contains("whole seconds"), "{error}");
        }
    }

    #[test]
    fn a_run_is_warned_once_most_of_its_time_is_gone() {
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_secs(100);
        assert_eq!(
            warning(started, deadline),
            started + std::time::Duration::from_secs(80)
        );
    }

    #[test]
    fn a_run_stopped_before_its_deadline_did_not_run_out() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        let (cancel, ran_out) = (Cancel::default(), Arc::new(AtomicBool::new(false)));
        let timer = hold_to_deadline(deadline, cancel.clone(), ran_out.clone());
        // The timer goes without counting the stop as its own.
        cancel.set();
        timer.join().unwrap();
        assert!(!ran_out.load(Ordering::SeqCst));
    }

    #[test]
    fn a_run_its_deadline_stopped_says_so() {
        assert_eq!(
            result_of(&report(Outcome::Stopped), None, true)["status"],
            "deadline"
        );
        assert_eq!(
            result_of(&report(Outcome::Stopped), None, false)["status"],
            "stopped"
        );
        // An answer that came in as time ran out stands as an answer.
        assert_eq!(
            result_of(&report(Outcome::Done), None, true)["status"],
            "done"
        );
    }

    #[test]
    fn a_run_past_its_deadline_stops() {
        let started = std::time::Instant::now();
        let deadline = started + std::time::Duration::from_millis(300);
        let (cancel, ran_out) = (Cancel::default(), Arc::new(AtomicBool::new(false)));
        hold_to_deadline(deadline, cancel.clone(), ran_out.clone())
            .join()
            .unwrap();
        assert!(cancel.is_set());
        assert!(ran_out.load(Ordering::SeqCst));
        assert!(std::time::Instant::now() >= deadline, "stopped early");
    }

    /// What `letronna` makes of `words`.
    fn parsed(words: &[&str]) -> Args {
        parse(words.iter().map(|word| word.to_string())).unwrap()
    }

    #[test]
    fn the_account_api_routes_to_the_letronna_gateway_once_signed_in() {
        let hosted = letronna_core::auth::hosted_base();
        assert_eq!(route_gateway(&hosted, true), "Letronna");
        assert_eq!(route_gateway(&format!("{hosted}/"), true), "Letronna");
        assert_eq!(route_gateway(&hosted, false), "Custom");
        assert_eq!(route_gateway("https://proxy.test/v1", true), "Custom");
    }

    /// An environment with a server, its key and its model in it.
    fn routed(name: &str) -> Option<String> {
        match name {
            "AGENT_BASE_URL" => Some(" https://proxy.test/v1 ".into()),
            "AGENT_API_KEY" => Some("env-key".into()),
            "AGENT_MODEL" => Some("env-model".into()),
            _ => None,
        }
    }

    #[test]
    fn the_environment_routes_a_print_run_with_the_flag_model_first() {
        let route = |words: &[&str]| given_route(&parsed(words), routed);
        let given = |model: &str| {
            Some(Ok((
                "https://proxy.test/v1".into(),
                "env-key".into(),
                model.into(),
            )))
        };
        assert_eq!(route(&["-p", "q"]), given("env-model"));
        assert_eq!(
            route(&["-p", "q", "--model", "flag-model"]),
            given("flag-model")
        );
        assert_eq!(route(&["-p", "q", "--gateway", "Ollama"]), None);
    }

    #[test]
    fn the_terminal_takes_the_environment_only_on_a_thrown_away_machine() {
        assert!(env_routes(&parsed(&["-p", "q"]), false));
        assert!(!env_routes(&parsed(&[]), false));
        assert!(env_routes(&parsed(&[]), true));
    }

    #[test]
    fn no_server_routes_nothing_and_a_server_with_no_model_says_so() {
        let without =
            |gone: &'static str| move |name: &str| (name != gone).then(|| routed(name)).flatten();
        let route = given_route(&parsed(&["-p", "q"]), without("AGENT_BASE_URL"));
        assert_eq!(route, None);
        let blank = |name: &str| {
            (name == "AGENT_BASE_URL")
                .then(|| " ".into())
                .or_else(|| routed(name))
        };
        assert_eq!(given_route(&parsed(&["-p", "q"]), blank), None);
        let Some(Err(why)) = given_route(&parsed(&["-p", "q"]), without("AGENT_MODEL")) else {
            panic!("a server with no model was taken");
        };
        assert!(
            why.contains("AGENT_MODEL") && why.contains("--model"),
            "{why}"
        );
        // A server may take no key, as Custom's need not.
        let keyless = given_route(&parsed(&["-p", "q"]), without("AGENT_API_KEY"));
        assert_eq!(
            keyless,
            Some(Ok((
                "https://proxy.test/v1".into(),
                String::new(),
                "env-model".into()
            )))
        );
    }

    #[test]
    fn the_name_said_on_leaving_is_one_plain_line() {
        assert_eq!(
            super::plain_name("fix the\nbuild\x1b]0;x\x07 now"),
            "fix the build ]0;x now"
        );
        assert_eq!(super::plain_name("  "), "untitled");
    }

    #[test]
    fn a_chat_is_private_however_it_was_made_so() {
        use letronna_core::memory::Saving;
        let mut store = store::Store::default();
        store.conversations.push(store::Conversation {
            id: 68_701,
            ..Default::default()
        });
        assert!(!super::is_private(&store, 0));
        // `/remember`'s Private chat row and `--private` both do this.
        store.make_private(0);
        assert!(super::is_private(&store, 0));
        // A `/branch` copies both choices, and a `/resume` reads them back.
        let copy = store::Conversation {
            id: 68_702,
            memory_saving: store.conversations[0].memory_saving,
            memory_scope: store.conversations[0].memory_scope,
            ..Default::default()
        };
        store.conversations.push(copy);
        assert!(super::is_private(&store, 1));
        // Saving on its own again, it is no longer a private chat.
        store.choose_memory_saving(0, Some(Saving::Auto));
        assert!(!super::is_private(&store, 0));
    }
}
