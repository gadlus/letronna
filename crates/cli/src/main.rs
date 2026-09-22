//! Letronna in the terminal.
//!
//! `letronna` on its own opens a conversation; `-p` asks one question and
//! leaves. Sessions are the same files the window uses, so a conversation
//! started here can be carried on there and the other way round.

mod input;
mod render;
mod session;

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
    let session = runner.open(args.resume, workspace.clone());

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

    banner(&runner, workspace.as_deref());
    loop {
        render::blank();
        let Some(line) = input::read(&format!("{} ", render::blue("›"))) else {
            break;
        };
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if matches!(line, "/exit" | "/quit" | "exit" | "quit") {
            break;
        }
        if line == "/help" {
            print!("{HELP}");
            continue;
        }
        if let Err(error) = runner.ask(session, line) {
            render::blank();
            render::line(&render::red(&error));
        }
        runner.persist();
    }
    render::blank();
}

fn banner(runner: &Runner, workspace: Option<&std::path::Path>) {
    let folder = workspace
        .and_then(|p| p.file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "no project".into());
    render::blank();
    render::line(&format!(
        "{}  {}  {}  {}  {}",
        render::bold(letronna_core::BRAND),
        render::dim("·"),
        folder,
        render::dim("·"),
        runner.store.settings.model
    ));
    let mode = if runner.plan {
        Some("plan mode: reads and searches only")
    } else if runner.yes {
        Some("running tools without asking")
    } else {
        None
    };
    if let Some(mode) = mode {
        render::line(&render::dim(mode));
    }
    render::line(&render::dim("/help for commands, /exit to leave"));
}

/// Start the window. It is a separate binary because a GUI-subsystem
/// executable on Windows cannot write to the console that launched it.
fn launch_window() {
    let beside = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join(window_name())));
    let candidates = [beside, Some(std::path::PathBuf::from(window_name()))];
    for candidate in candidates.into_iter().flatten() {
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
        assert_eq!(
            args(&["--print", "hello"]).prompt.as_deref(),
            Some("hello")
        );
        // A bare word is a prompt, so quoting alone is enough.
        assert_eq!(args(&["what is this"]).prompt.as_deref(), Some("what is this"));
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
