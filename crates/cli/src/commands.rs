//! The slash commands: everything you can ask of Letronna itself rather
//! than of the model.

use letronna_core::store;

use crate::banner::{banner, folder_name, footer, replay};
use crate::bar;
use crate::render;
use crate::session::Runner;

pub enum Flow {
    Handled,
    Stop,
    Unknown,
}

pub fn command(
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
        "/login" => {
            sign_in(runner);
            Flow::Handled
        }
        "/logout" => {
            let who = letronna_core::auth::account();
            letronna_core::auth::sign_out();
            render::blank();
            match who {
                Some(account) => render::line(&render::dim(&format!(
                    "Signed out {}. Your sessions and settings stay where they are.",
                    account.name
                ))),
                None => render::line(&render::dim("Nobody was signed in.")),
            }
            Flow::Handled
        }
        "/whoami" => {
            render::blank();
            match letronna_core::auth::account() {
                Some(account) => {
                    render::said(&render::blue("●"), &render::bold(&account.name));
                    if !account.email.is_empty() {
                        render::line(&render::dim(&account.email));
                    }
                    if letronna_core::auth::is_stub(&account) {
                        render::line(&render::dim(
                            "Signed in against the stand-in, not a real account.",
                        ));
                    }
                }
                None => render::line(&render::dim("Not signed in. /login to fix that.")),
            }
            Flow::Handled
        }
        "/effort" => {
            effort(runner, rest);
            Flow::Handled
        }
        "/thinking" => {
            runner.store.settings.thinking = !runner.store.settings.thinking;
            runner.persist();
            let note = footer(runner);
            runner.bar.set_footer(note);
            render::blank();
            render::line(&render::dim(if runner.store.settings.thinking {
                "Thinking shown as it arrives."
            } else {
                "Thinking hidden."
            }));
            Flow::Handled
        }
        "/search" => {
            runner.store.settings.search = !runner.store.settings.search;
            runner.persist();
            let note = footer(runner);
            runner.bar.set_footer(note);
            render::blank();
            render::line(&render::dim(if runner.store.settings.search {
                "The model can search the web, and will cite what it read."
            } else {
                "Web search off."
            }));
            Flow::Handled
        }
        "/jobs" => {
            jobs(rest);
            Flow::Handled
        }
        "/skills" => {
            skills(runner, rest);
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

/// Sign in through the browser.
///
/// The page that answers is not up yet, so this offers the stand-in as well:
/// the real flow is tried first and the stand-in writes the same file when
/// the browser has nothing to talk to.
pub fn sign_in(runner: &mut Runner) {
    if let Some(account) = letronna_core::auth::account() {
        render::blank();
        render::line(&render::dim(&format!(
            "Already signed in as {}. /logout first to change that.",
            account.name
        )));
        return;
    }

    let pending = match letronna_core::auth::begin() {
        Ok(pending) => pending,
        Err(error) => {
            render::blank();
            render::line(&render::red(&error));
            return;
        }
    };

    render::blank();
    render::said(&render::blue("●"), &render::bold("Sign in"));
    render::line(&render::dim(
        "Opening your browser. Come back when it is done.",
    ));
    render::line(&render::dim(&pending.url));
    open_in_browser(&pending.url);

    // The site does not exist yet, so waiting the full two minutes for a
    // browser that will land on nothing is not worth anybody's time.
    let picked = runner.bar.choose(
        "Waiting for the browser",
        vec!["The page is not up yet; the stand-in signs you in locally.".into()],
        &["Use the stand-in", "Wait for the browser", "Cancel"],
    );
    match picked {
        Some(0) => match letronna_core::auth::stub("Letronna user") {
            Ok(account) => welcome(&account),
            Err(error) => render::line(&render::red(&error)),
        },
        Some(1) => match pending.finish() {
            Ok(account) => welcome(&account),
            Err(error) => render::line(&render::red(&error)),
        },
        _ => render::line(&render::dim("Left it.")),
    }
}

fn welcome(account: &letronna_core::auth::Account) {
    render::said(&render::blue("›"), &render::bold(&account.name));
    render::line(&render::dim("Signed in."));
}

/// Hand a URL to whatever opens links here.
pub fn open_in_browser(url: &str) {
    let opened = if cfg!(windows) {
        std::process::Command::new("rundll32")
            .args(["url.dll,FileProtocolHandler", url])
            .spawn()
    } else if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    if opened.is_err() {
        render::line(&render::dim("Could not open a browser; the link is above."));
    }
}

/// How hard the model should think.
///
/// Sent to every model and ignored by the ones that cannot be told, which is
/// why there is no capability check here: on a model that can, this is the
/// difference between sixty-four reasoning tokens and seven hundred.
fn effort(runner: &mut Runner, rest: &str) {
    const LEVELS: [&str; 4] = ["auto", "low", "medium", "high"];
    let current = match runner.store.settings.effort.as_str() {
        "" => "auto",
        level => level,
    };
    let chosen = match rest {
        // Nothing named: slide to it. Effort is a point on one line from
        // less to more, and a slider says so where a list would not.
        "" => {
            let start = LEVELS.iter().position(|l| *l == current).unwrap_or(0);
            match runner
                .bar
                .slide("How hard should it think?", &LEVELS, start)
            {
                Some(index) => LEVELS[index].to_owned(),
                None => {
                    render::blank();
                    render::line(&render::dim(&format!("Left at {current}.")));
                    return;
                }
            }
        }
        asked => asked.to_lowercase(),
    };
    runner.store.settings.effort = match chosen.as_str() {
        "low" | "medium" | "high" => chosen,
        _ => String::new(),
    };
    runner.persist();
    let note = footer(runner);
    runner.bar.set_footer(note);
    render::blank();
    render::line(&render::dim(&match runner.store.settings.effort.as_str() {
        "" => "Thinking left to the model.".to_owned(),
        level => format!("Thinking {level}."),
    }));
}

/// What is still running, and a way to stop it.
///
/// A command started in the background outlives the turn that started it and
/// says nothing more unless asked. Without this the only way to find out was
/// to ask the model to ask its tool, which is a long way round to a list the
/// program already has.
fn jobs(rest: &str) {
    let running = letronna_core::jobs::list();
    render::blank();
    if running.is_empty() {
        render::said(&render::blue("●"), &render::bold("Nothing running"));
        return;
    }

    // `/jobs stop 3`, or a bare `/jobs` to look.
    if let Some(id) = rest.strip_prefix("stop").map(str::trim) {
        let Ok(id) = id.parse::<u64>() else {
            render::said(
                &render::blue("●"),
                &render::bold("Which one? /jobs stop <id>"),
            );
            return;
        };
        match letronna_core::jobs::stop(id) {
            Ok(()) => render::said(
                &render::blue("●"),
                &render::bold(&format!("Stopped job {id}")),
            ),
            Err(error) => render::line(&render::red(&error)),
        }
        return;
    }

    render::said(&render::blue("●"), &render::bold("Running"));
    for job in &running {
        let state = if job.running { "running" } else { "finished" };
        render::line(&format!(
            "{}  {}  {}",
            render::bold(&job.id.to_string()),
            letronna_core::tools::one_line(&job.command),
            render::dim(state)
        ));
    }
    render::line(&render::dim("/jobs stop <id> to stop one"));
}

/// The saved instructions, and a way to use one.
///
/// Skills are written in the window and were unreachable from here, which
/// made them a feature of one half of the same program.
fn skills(runner: &mut Runner, rest: &str) {
    let saved = letronna_core::skills::load();
    render::blank();
    if saved.is_empty() {
        render::said(&render::blue("●"), &render::bold("No skills saved"));
        render::line(&render::dim("The window writes them; they show up here."));
        return;
    }

    // Named, it goes to the model as the next thing said.
    if !rest.is_empty() {
        let wanted = rest.to_lowercase();
        match saved
            .iter()
            .find(|s| s.name.to_lowercase() == wanted)
            .or_else(|| {
                saved
                    .iter()
                    .find(|s| s.name.to_lowercase().contains(&wanted))
            }) {
            Some(skill) => {
                render::said(&render::blue("●"), &render::bold(&skill.name));
                runner.bar.say(&skill.prompt);
            }
            None => render::line(&render::dim(&format!("No skill matching \"{rest}\"."))),
        }
        return;
    }

    render::said(&render::blue("●"), &render::bold("Skills"));
    for skill in &saved {
        render::line(&format!(
            "{}  {}",
            render::bold(&skill.name),
            render::dim(&letronna_core::tools::one_line(&skill.prompt))
        ));
    }
    render::line(&render::dim("/skills <name> to use one"));
}

/// Sessions offered at once. Older ones are still on disk and still open in
/// the window; this is the shortlist you would actually pick from.
const SESSION_CHOICES: usize = 9;

/// Pick up a session from before. The same files the window uses, so a
/// conversation started there can be carried on here.
pub fn resume(runner: &mut Runner, session: &mut usize, workspace: Option<&std::path::Path>) {
    // Newest first, skipping the one already open and any that never got a
    // word said in them.
    let mut recent: Vec<usize> = (0..runner.store.conversations.len())
        .filter(|i| *i != *session && runner.store.conversations[*i].length() > 0)
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
            format!(
                "{title}  ·  {folder}  ·  {} messages",
                conversation.messages.len()
            )
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
    runner.enter(index);
    // A session carries its own project folder; one that never had a folder
    // takes the one this window is standing in.
    if runner.store.conversations[index].workspace.is_none() {
        runner.store.conversations[index].workspace = workspace.map(|p| p.to_path_buf());
    }
    runner.persist();

    // No title line here: the first thing replayed is the message the title
    // was taken from, and saying it twice reads as the command having run
    // twice.
    replay(runner, index);
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
