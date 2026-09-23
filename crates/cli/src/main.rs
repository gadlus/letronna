//! Letronna in the terminal.
//!
//! `letronna` on its own opens a conversation; `-p` asks one question and
//! leaves. Sessions are the same files the window uses, so a conversation
//! started here can be carried on there and the other way round.

mod args;
mod banner;
mod bar;
mod commands;
mod diff;
mod edit;
mod input;
mod render;
mod screen;
mod session;
mod spinner;
mod title;

use args::{HELP, Start, parse};
use banner::{banner, folder_name, footer, replay};
use commands::{Flow, command, resume};
use letronna_core::store;
use session::Runner;

fn main() {
    rescue_terminal();
    let args = parse(std::env::args().skip(1));
    if args.help {
        print!("{HELP}");
        return;
    }
    if args.gui {
        launch_window();
        return;
    }

    let mut runner = Runner {
        store: store::load_listing(),
        allowed: Default::default(),
        yes: args.yes,
        plan: args.plan,
        bar: bar::Bar::new(String::new()),
    };
    if runner.store.settings.model.is_empty() {
        render::line(&render::red(
            "No model configured yet. Open the window once to choose a gateway: letronna gui",
        ));
        std::process::exit(1);
    }

    let workspace = std::env::current_dir().ok();
    let mut session = match args.start {
        Start::Last => runner.open(true, workspace.clone()),
        Start::Named(id) => match runner.open_id(id) {
            Some(index) => index,
            None => {
                render::line(&render::red(&format!("No session with the id {id}.")));
                std::process::exit(1);
            }
        },
        _ => runner.open(false, workspace.clone()),
    };

    // One question, one answer, no banner: this form gets piped.
    if let Some(prompt) = args.prompt {
        match runner.ask(session, &prompt) {
            Ok(_) => runner.persist(),
            Err(error) => {
                render::line(&render::red(&error));
                std::process::exit(1);
            }
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
    render::clear();
    banner(&runner, &folder, session);
    if args.start == Start::Choose {
        resume(&mut runner, &mut session, workspace.as_deref());
    } else if runner.store.conversations[session].length() > 0 {
        replay(&runner, session);
    }

    while let Some(line) = runner.bar.wait() {
        let line = line.trim().to_owned();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('/') {
            match command(&mut runner, &line, &mut session, workspace.as_deref()) {
                Flow::Stop => break,
                Flow::Handled => continue,
                Flow::Unknown => {
                    render::line(&render::dim(&format!(
                        "{line} is not a command. Type / to see what is."
                    )));
                    continue;
                }
            }
        }
        if let Err(error) = runner.ask(session, &line) {
            render::blank();
            render::line(&render::red(&error));
        }
        runner.persist();
        if runner.bar.finished() {
            break;
        }
    }
    runner.bar.uninstall();
    render::title("");
    render::blank();
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

/// Start the window. It is a separate binary because a GUI-subsystem
/// executable on Windows cannot write to the console that launched it.
fn launch_window() {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(window_name())));
    // Installed, the window sits one level up from the bin folder holding this.
    let above = std::env::current_exe().ok().and_then(|p| {
        p.parent()
            .and_then(|d| d.parent())
            .map(|d| d.join("Letronna.exe"))
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
