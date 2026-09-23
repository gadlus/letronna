//! Letronna in the terminal.
//!
//! `letronna` on its own opens a conversation; `-p` asks one question and
//! leaves. Sessions are the same files the window uses, so a conversation
//! started here can be carried on there and the other way round.

mod input;
mod prompt;
mod render;
mod session;
mod spinner;

use letronna_core::store;
use session::Runner;

const HELP: &str = "\
Letronna — a small agent in your terminal

  letronna                 open a conversation
  letronna -p \"question\"    ask once and exit
  letronna --continue      pick up the most recent session
  letronna gui             open the window instead

Options
  -p, --print <prompt>     ask one question, print the answer, exit
  -c, --continue           resume the most recent session
      --yes                run tools without asking (careful)
      --plan               read and search only; never write or run
  -h, --help               this
";

struct Args {
    prompt: Option<String>,
    resume: bool,
    yes: bool,
    plan: bool,
    help: bool,
    gui: bool,
}

fn parse(mut raw: impl Iterator<Item = String>) -> Args {
    let mut args = Args {
        prompt: None,
        resume: false,
        yes: false,
        plan: false,
        help: false,
        gui: false,
    };
    while let Some(arg) = raw.next() {
        match arg.as_str() {
            "gui" => args.gui = true,
            "-p" | "--print" => args.prompt = raw.next(),
            "-c" | "--continue" | "--resume" => args.resume = true,
            "--yes" => args.yes = true,
            "--plan" => args.plan = true,
            "-h" | "--help" => args.help = true,
            // A bare word is the prompt, so `letronna "what is this"` works.
            other if !other.starts_with('-') && args.prompt.is_none() => {
                args.prompt = Some(other.to_owned())
            }
            _ => {}
        }
    }
    args
}

fn main() {
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
        store: store::load(),
        allowed: Default::default(),
        yes: args.yes,
        plan: args.plan,
    };
    if runner.store.settings.model.is_empty() {
        render::line(&render::red(
            "No model configured yet. Open the window once to choose a gateway: letronna gui",
        ));
        std::process::exit(1);
    }

    let workspace = std::env::current_dir().ok();
    let mut session = runner.open(args.resume, workspace.clone());

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
    render::clear();
    render::title(&format!("letronna — {folder}"));
    banner(&runner, &folder, session);

    loop {
        render::blank();
        let Some(line) = prompt::read(&format!("{} ", render::blue("›")), &footer(&runner)) else {
            break;
        };
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
    }
    render::title("");
    render::blank();
}

enum Flow {
    Handled,
    Stop,
    Unknown,
}

fn command(
    runner: &mut Runner,
    line: &str,
    session: &mut usize,
    workspace: Option<&std::path::Path>,
) -> Flow {
    match line {
        "/exit" | "/quit" => Flow::Stop,
        "/help" => {
            render::blank();
            for entry in prompt::COMMANDS {
                render::line(&format!(
                    "{}  {}",
                    render::bold(entry.name),
                    render::dim(entry.blurb)
                ));
            }
            Flow::Handled
        }
        "/clear" => {
            render::clear();
            banner(runner, &folder_name(workspace), *session);
            Flow::Handled
        }
        "/new" => {
            *session = runner.open(false, workspace.map(|p| p.to_path_buf()));
            runner.persist();
            render::blank();
            render::line(&render::dim("New session."));
            Flow::Handled
        }
        "/plan" => {
            runner.plan = !runner.plan;
            render::blank();
            render::line(&render::dim(if runner.plan {
                "Plan mode on: reads and searches, never writes or runs."
            } else {
                "Plan mode off."
            }));
            Flow::Handled
        }
        "/model" => {
            let settings = &runner.store.settings;
            render::blank();
            render::line(&format!(
                "{}  {}",
                render::bold(&settings.model),
                render::dim(&settings.gateway)
            ));
            Flow::Handled
        }
        "/where" => {
            render::blank();
            render::line(&format!(
                "{}  {}",
                render::dim("project"),
                workspace
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "none".into())
            ));
            let id = runner.store.conversations[*session].id;
            render::line(&format!(
                "{}  {}",
                render::dim("session"),
                store::sessions_dir().join(format!("{id}.jsonl")).display()
            ));
            Flow::Handled
        }
        _ => Flow::Unknown,
    }
}

fn folder_name(workspace: Option<&std::path::Path>) -> String {
    workspace
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no project".into())
}

fn banner(runner: &Runner, folder: &str, session: usize) {
    render::blank();

    let settings = &runner.store.settings;
    let conversation = &runner.store.conversations[session];
    let mut facts = vec![
        format!(
            "{}  {}",
            render::bold(letronna_core::BRAND),
            render::dim(env!("CARGO_PKG_VERSION"))
        ),
        String::new(),
        fact("project", folder),
        fact("model", &settings.model),
        fact("gateway", &settings.gateway),
        fact(
            "session",
            &match conversation.messages.len() {
                0 => "new".to_owned(),
                1 => "carrying on, 1 message".to_owned(),
                n => format!("carrying on, {n} messages"),
            },
        ),
        fact(
            "tools",
            if runner.plan {
                "plan mode: reads and searches only"
            } else if runner.yes {
                "running without asking"
            } else {
                "asking before writing or running"
            },
        ),
        String::new(),
        render::dim("type / for commands"),
    ];

    // Side by side when the face fits; the details alone when it does not, so
    // a narrow window shows something readable rather than a wrapped mess.
    if !render::colour() || render::width() < render::FACE_WIDTH + 40 {
        for fact in facts.into_iter().filter(|f| !f.is_empty()) {
            render::line(&fact);
        }
        return;
    }

    // Sit the details against the middle of the face rather than its top.
    let lead = (render::FACE.len().saturating_sub(facts.len())) / 2;
    let mut column = vec![String::new(); lead];
    column.append(&mut facts);

    for row in 0..render::FACE.len().max(column.len()) {
        let face = render::FACE.get(row).copied().unwrap_or("");
        let text = column.get(row).cloned().unwrap_or_default();
        println!(
            "{}{}{}",
            render::PAD,
            render::blue(&format!("{face:width$}", width = render::FACE_WIDTH)),
            text
        );
    }
}

/// The standing note under the input box: who the question is going to, and
/// anything about this session that changes what happens when you press
/// enter.
fn footer(runner: &Runner) -> String {
    let settings = &runner.store.settings;
    let mut parts = vec![settings.model.clone(), settings.gateway.clone()];
    if runner.plan {
        parts.push("plan mode".into());
    } else if runner.yes {
        parts.push("no approvals".into());
    }
    parts.join("  ·  ")
}

/// One `label  value` line of the startup details.
fn fact(label: &str, value: &str) -> String {
    format!("{}  {value}", render::dim(&format!("{label:<8}")))
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

#[cfg(test)]
mod tests {
    use super::parse;

    fn args(words: &[&str]) -> super::Args {
        parse(words.iter().map(|w| w.to_string()))
    }

    #[test]
    fn reads_the_forms_people_actually_type() {
        let bare = args(&[]);
        assert!(bare.prompt.is_none() && !bare.resume && !bare.yes);

        assert_eq!(args(&["-p", "hello"]).prompt.as_deref(), Some("hello"));
        assert_eq!(args(&["--print", "hello"]).prompt.as_deref(), Some("hello"));
        // A bare word is a prompt, so quoting alone is enough.
        assert_eq!(
            args(&["what is this"]).prompt.as_deref(),
            Some("what is this")
        );
        // A flag after it must not be swallowed as the prompt.
        let mixed = args(&["--plan", "look around"]);
        assert!(mixed.plan);
        assert_eq!(mixed.prompt.as_deref(), Some("look around"));

        assert!(args(&["-c"]).resume);
        assert!(args(&["--continue"]).resume);
        assert!(args(&["gui"]).gui);
        assert!(args(&["--yes"]).yes);
        assert!(args(&["-h"]).help);
    }
}
