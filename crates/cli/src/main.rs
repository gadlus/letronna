//! Letronna in the terminal.
//!
//! `letronna` on its own opens a conversation; `-p` asks one question and
//! leaves. Sessions are the same files the window uses, so a conversation
//! started here can be carried on there and the other way round.

mod bar;
mod diff;
mod edit;
mod input;
mod render;
mod screen;
mod session;
mod spinner;
mod title;

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
        store: store::load(),
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
    title::idle();
    // The window has to be ours before the banner, or the banner goes to the
    // screen we are about to swap away from.
    let note = footer(&runner);
    runner.bar.set_footer(note);
    runner.bar.install();
    render::clear();
    banner(&runner, &folder, session);

    loop {
        let Some(line) = runner.bar.wait() else {
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
    // A command may carry a word after it: `/model gpt` narrows the list
    // rather than making you scroll it.
    let (line, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    match line {
        "/exit" | "/quit" => Flow::Stop,
        "/help" => {
            render::blank();
            for entry in bar::COMMANDS {
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
            runner.bar.redraw();
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
            let note = footer(runner);
            runner.bar.set_footer(note);
            render::blank();
            render::line(&render::dim(if runner.plan {
                "Plan mode on: reads and searches, never writes or runs."
            } else {
                "Plan mode off."
            }));
            Flow::Handled
        }
        "/model" => {
            switch_model(runner, rest);
            Flow::Handled
        }
        "/resume" => {
            resume(runner, session, workspace);
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

/// Sessions offered at once. Older ones are still on disk and still open in
/// the window; this is the shortlist you would actually pick from.
const SESSION_CHOICES: usize = 9;

/// Pick up a session from before. The same files the window uses, so a
/// conversation started there can be carried on here.
fn resume(runner: &mut Runner, session: &mut usize, workspace: Option<&std::path::Path>) {
    // Newest first, skipping the one already open and any that never got a
    // word said in them.
    let mut recent: Vec<usize> = (0..runner.store.conversations.len())
        .filter(|i| *i != *session && !runner.store.conversations[*i].messages.is_empty())
        .collect();
    recent.sort_by_key(|i| std::cmp::Reverse(runner.store.conversations[*i].updated));
    recent.truncate(SESSION_CHOICES);

    if recent.is_empty() {
        render::blank();
        render::line(&render::dim("No earlier sessions to pick up."));
        return;
    }

    let labels: Vec<String> = recent
        .iter()
        .map(|i| {
            let conversation = &runner.store.conversations[*i];
            let title = match conversation.title.trim() {
                "" => "untitled".to_owned(),
                title => title.to_owned(),
            };
            let folder = conversation
                .workspace
                .as_ref()
                .and_then(|p| p.file_name())
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "no project".into());
            format!("{title}  ·  {folder}  ·  {} messages", conversation.messages.len())
        })
        .collect();

    render::blank();
    render::said(&render::blue("●"), &render::bold("Earlier sessions"));
    let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
    options.push("Stay where I am");
    let Some(picked) = runner.bar.choose("Which one?", Vec::new(), &options) else {
        return;
    };
    let Some(index) = recent.get(picked).copied() else {
        render::line(&render::dim("Stayed put."));
        return;
    };

    *session = index;
    runner.store.active = index;
    // A session carries its own project folder; one that never had a folder
    // takes the one this window is standing in.
    if runner.store.conversations[index].workspace.is_none() {
        runner.store.conversations[index].workspace = workspace.map(|p| p.to_path_buf());
    }
    runner.persist();

    let conversation = &runner.store.conversations[index];
    let title = conversation.title.clone();
    let count = conversation.messages.len();
    render::said(&render::blue("›"), &render::bold(&title));
    render::line(&render::dim(&format!(
        "{count} messages. Ask something to carry on."
    )));
}

/// Models offered at once. Some gateways carry hundreds; a list longer than
/// this is narrowed with a word rather than scrolled.
const MODEL_CHOICES: usize = 10;

/// Change the model, picking from what the gateway actually offers. `filter`
/// narrows the list, and an exact name skips the asking entirely.
fn switch_model(runner: &mut Runner, filter: &str) {
    let settings = &runner.store.settings;
    let guest = settings.gateway == letronna_core::gateway::GATEWAYS[0].name;
    let key = store::load_keys()
        .get(letronna_core::gateway::key_slot(&settings.gateway))
        .cloned()
        .unwrap_or_default();
    let endpoint = settings.endpoint.clone();
    let current = settings.model.clone();
    let gateway = settings.gateway.clone();

    render::blank();
    render::said(&render::blue("●"), &render::bold("Models"));
    // Fetching takes a moment over the network; say so rather than freezing.
    render::line(&render::dim(&format!("asking {gateway} what it has…")));
    let found = if guest {
        letronna_core::chat::free_models()
    } else {
        letronna_core::chat::list_models(&endpoint, &key)
    };

    let mut models = match found {
        Ok(models) if !models.is_empty() => models,
        Ok(_) => {
            render::line(&render::dim("That gateway offered no models."));
            return;
        }
        Err(error) => {
            render::line(&render::red(&error));
            return;
        }
    };
    models.sort();

    // An exact name is an instruction, not a search.
    if let Some(exact) = models.iter().find(|m| m.eq_ignore_ascii_case(filter)) {
        let exact = exact.clone();
        set_model(runner, exact);
        return;
    }

    let needle = filter.to_lowercase();
    let matching: Vec<String> = models
        .iter()
        .filter(|m| needle.is_empty() || m.to_lowercase().contains(&needle))
        .cloned()
        .collect();
    if matching.is_empty() {
        render::line(&render::dim(&format!(
            "Nothing matching \"{filter}\" among {} models.",
            models.len()
        )));
        return;
    }

    let shown: Vec<String> = matching.iter().take(MODEL_CHOICES).cloned().collect();
    if matching.len() > shown.len() {
        render::line(&render::dim(&format!(
            "{} of {} shown — type /model <word> to narrow it.",
            shown.len(),
            matching.len()
        )));
    }
    let mut options: Vec<&str> = shown.iter().map(String::as_str).collect();
    options.push("Keep the one I have");

    let question = format!("In use: {current}");
    let Some(picked) = runner.bar.choose(&question, Vec::new(), &options) else {
        return;
    };
    match shown.get(picked) {
        Some(model) => set_model(runner, model.clone()),
        None => render::line(&render::dim("Left as it was.")),
    }
}

fn set_model(runner: &mut Runner, model: String) {
    runner.store.settings.model = model.clone();
    runner.persist();
    let note = footer(runner);
    runner.bar.set_footer(note);
    render::said(&render::blue("›"), &render::bold(&model));
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
        render::dim("shift+enter for a new line · ↑ for what you said before"),
    ];

    // Side by side when the face fits; the details alone when it does not, so
    // a narrow window shows something readable rather than a wrapped mess.
    if !render::colour() || render::width() < render::FACE_WIDTH + render::FACE_GAP + 40 {
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
        render::out(
            &format!(
                "{}{}{}",
                render::MARGIN,
                render::blue(&format!(
                    "{face:width$}",
                    width = render::FACE_WIDTH + render::FACE_GAP
                )),
                text
            ),
            true,
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
