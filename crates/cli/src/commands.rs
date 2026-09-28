//! The slash commands: everything you can ask of Letronna itself rather
//! than of the model.

use letronna_core::agents::Seen;
use letronna_core::store;
use std::sync::atomic::Ordering;
use std::time::Instant;

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
            help();
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
        "/gateways" => {
            gateways(runner);
            Flow::Handled
        }
        "/team" => {
            team(runner);
            Flow::Handled
        }
        "/login" => {
            sign_in(runner);
            Flow::Handled
        }
        "/logout" => {
            let who = letronna_core::auth::account();
            runner
                .bar
                .busy("signing this computer out", letronna_core::auth::sign_out);
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
            whoami(runner);
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
        "/search" if rest == "services" => {
            search_services(runner);
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
        "/agents" => {
            agents(runner.store.conversations[*session].id, rest);
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
        "/undo" => {
            match take_back(runner, *session, workspace) {
                Some(prompt) => {
                    runner.bar.say(&prompt);
                    render::blank();
                    render::line(&render::dim(
                        "Took back the last exchange. Your message is back in the box.",
                    ));
                }
                None => {
                    render::blank();
                    render::line(&render::dim("Nothing to take back."));
                }
            }
            Flow::Handled
        }
        "/retry" => {
            match take_back(runner, *session, workspace) {
                // Queued rather than asked from here, so it goes the way a
                // typed line goes: the loop sends it and reports a failure.
                Some(prompt) => runner.bar.send_first(&prompt),
                None => {
                    render::blank();
                    render::line(&render::dim("Nothing to retry."));
                }
            }
            Flow::Handled
        }
        "/branch" => {
            branch(runner, session, rest);
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

/// Keys the box answers to. None of them are written down anywhere else but
/// the opening screen, which has scrolled away by the time anyone wonders.
const KEYS: &[(&str, &str)] = &[
    ("enter", "send"),
    ("shift+enter", "a new line"),
    ("↑ ↓", "step through what you said before"),
    ("esc", "stop an answer, or clear the line"),
    ("ctrl+c", "copy what is selected, clear the line, or leave"),
    ("page up/down", "scroll back through the conversation"),
    ("drag", "select and copy"),
];

/// The commands, then the keys, each in one column so it reads straight down.
fn help() {
    render::blank();
    let width = bar::name_width();
    for entry in bar::COMMANDS {
        render::line(&format!(
            "{}  {}",
            render::bold(&format!("{:<width$}", entry.name)),
            render::dim(entry.blurb)
        ));
    }
    render::blank();
    let width = KEYS
        .iter()
        .map(|(key, _)| key.chars().count())
        .max()
        .unwrap_or(0);
    for (key, what) in KEYS {
        render::line(&format!(
            "{}  {}",
            render::bold(&format!("{key:<width$}")),
            render::dim(what)
        ));
    }
    render::blank();
    render::line(&render::dim("The guide: https://letronna.ai/docs"));
}

/// Who is signed in, checked with the API; what was known last is shown when
/// it cannot be reached.
fn whoami(runner: &mut Runner) {
    if letronna_core::auth::account().is_none() {
        render::blank();
        render::line(&render::dim("Not signed in. /login to fix that."));
        return;
    }
    let fresh = runner
        .bar
        .busy("checking your account", letronna_core::auth::refresh);
    render::blank();
    let account = match fresh {
        Ok(account) => account,
        Err(error) => match letronna_core::auth::account() {
            Some(known) => {
                render::line(&render::dim(&format!("{error} Showing what was known.")));
                known
            }
            None => {
                render::line(&render::dim(&error));
                return;
            }
        },
    };
    render::said(&render::blue("●"), &render::bold(&account.name));
    if !account.email.is_empty() {
        render::line(&render::dim(&account.email));
    }
    if !account.plan.is_empty() {
        render::line(&format!(
            "{} {}   {} {}",
            render::dim("plan   "),
            account.plan_name(),
            render::dim("credits"),
            account.credit()
        ));
    }
    match &account.team {
        Some(team) => render::line(&format!(
            "{} {}   {} {}",
            render::dim("team   "),
            team.name,
            render::dim("pool   "),
            format_args!(
                "${}.{:02}",
                team.credits / 100,
                team.credits.rem_euclid(100)
            )
        )),
        None => render::line(&render::dim(
            "team    none · hosted models use your own credit · /team to pick one",
        )),
    }
}

/// Pick what hosted models are billed to on this computer: your own credit,
/// or one of your teams' shared pools.
fn team(runner: &mut Runner) {
    if letronna_core::auth::account().is_none() {
        render::blank();
        render::line(&render::dim("Not signed in. /login first."));
        return;
    }
    let found = runner
        .bar
        .busy("loading your teams", letronna_core::auth::teams);
    let teams = match found {
        Ok(teams) => teams,
        Err(error) => {
            render::blank();
            render::line(&render::red(&error));
            return;
        }
    };
    let current = letronna_core::auth::account()
        .and_then(|a| a.team)
        .map(|t| t.id);
    let mark = |on: bool| if on { "● " } else { "  " };
    let mut labels = vec![format!(
        "{}Personal — your own credit",
        mark(current.is_none())
    )];
    labels.extend(teams.iter().map(|t| {
        format!(
            "{}{} — team pool",
            mark(current.as_deref() == Some(t.id.as_str())),
            t.name
        )
    }));
    let options: Vec<&str> = labels.iter().map(String::as_str).collect();
    let detail = if teams.is_empty() {
        vec![
            "You are not in a team yet. Create or join one at letronna.ai/dashboard/team."
                .to_owned(),
        ]
    } else {
        Vec::new()
    };
    let Some(picked) = runner.bar.choose("Bill hosted models to", detail, &options) else {
        return;
    };
    let id = picked
        .checked_sub(1)
        .and_then(|i| teams.get(i))
        .map(|t| t.id.clone());
    let done = runner.bar.busy("switching", move || {
        letronna_core::auth::use_team(id.as_deref())
    });
    render::blank();
    match done {
        Ok(account) => {
            match &account.team {
                Some(team) => render::line(&format!(
                    "Hosted models now bill {}.",
                    render::bold(&team.name)
                )),
                None => render::line("Hosted models now use your own credit."),
            }
            let note = footer(runner);
            runner.bar.set_footer(note);
        }
        Err(error) => render::line(&render::red(&error)),
    }
}

/// Sign in through the browser: the page asks them to allow this computer,
/// then hands a token back to a port only this machine can reach.
pub fn sign_in(runner: &mut Runner) {
    if let Some(account) = letronna_core::auth::account() {
        render::blank();
        render::line(&render::dim(&format!(
            "Already signed in as {}. /logout first to change that.",
            account.name
        )));
        return;
    }

    let pending = match letronna_core::auth::begin(letronna_core::auth::Client::Cli) {
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
    // The link opens in the system's default browser, which may not be the
    // one in use; a sign-in carried across part way fails, so say so now.
    render::line(&render::dim(
        "Not your usual browser? Paste the link into the one you use.",
    ));
    open_in_browser(&pending.url);

    let stop = pending.cancel();
    match runner.bar.busy_cancellable(
        "waiting for the browser · esc to cancel",
        stop,
        move || pending.finish(),
    ) {
        Ok(account) => {
            render::said(&render::blue("›"), &render::bold(&account.name));
            let plan = account.plan_name();
            render::line(&render::dim(&if plan.is_empty() {
                "Signed in.".to_owned()
            } else {
                format!("Signed in · {plan} · {} credit", account.credit())
            }));
        }
        // Given up on with escape: what was asked for, not a failure.
        Err(error) if error == "Sign-in cancelled." => render::line(&render::dim(&error)),
        Err(error) => render::line(&render::red(&error)),
    }
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

/// This session's helpers and how each is getting on, or one of them stopped
/// or told something.
///
/// A helper in the background says nothing until it ends, and the model is
/// told not to ask after it, so this is where the user looks in on one.
fn agents(session: u64, rest: &str) {
    let helpers: Vec<Seen> = (letronna_core::agents::seen().into_iter())
        .filter(|helper| helper.session == session)
        .collect();
    render::blank();
    if helpers.is_empty() {
        render::said(
            &render::blue("●"),
            &render::bold("No helpers in this session"),
        );
        return;
    }

    // `/agents stop explore-2`, `/agents send explore-2 check the tests too`,
    // or a bare `/agents` to look.
    let usage = "/agents stop <name>  ·  /agents send <name> <text>";
    let (action, rest) = rest.split_once(' ').unwrap_or((rest, ""));
    match action {
        "stop" => stop_helper(&helpers, rest.trim()),
        "send" => send_to_helper(&helpers, rest.trim()),
        "" => {
            render::said(&render::blue("●"), &render::bold("Helpers"));
            let width = render::width().saturating_sub(render::PAD.len());
            let rows = agent_rows(&helpers, Instant::now(), width);
            let Some((header, rows)) = rows.split_first() else {
                return;
            };
            render::line(&render::dim(header));
            for (row, helper) in rows.iter().zip(&helpers) {
                // One that has ended stands back from those at work.
                render::line(&match helper.ended {
                    Some(_) => render::dim(row),
                    None => row.clone(),
                });
            }
            render::line(&render::dim(usage));
        }
        // A typo is not taken for a bare look, which would seem to have worked.
        _ => render::line(&render::dim(&format!("/agents to list them  ·  {usage}"))),
    }
}

/// The helpers as a table, as they stand at `now`, a header first, each row
/// cut to `width`. The task comes last, so a narrow window loses the end of
/// that before anything that tells the helpers apart.
fn agent_rows(helpers: &[Seen], now: Instant, width: usize) -> Vec<String> {
    let mut rows = vec![["name", "type", "state", "tokens", "task"].map(String::from)];
    rows.extend(helpers.iter().map(|helper| {
        let state = match &helper.ended {
            Some(how) => how.clone(),
            None => {
                let seconds = now.saturating_duration_since(helper.started).as_secs();
                format!("running · {}", crate::spinner::spell(seconds))
            }
        };
        [
            helper.name.clone(),
            helper.kind.clone(),
            state,
            letronna_core::budget::short(helper.status.input.load(Ordering::Relaxed)),
            letronna_core::tools::one_line(&helper.description),
        ]
    }));
    // Every column before the task as wide as its widest cell, so they line up.
    let columns: Vec<usize> = (0..4)
        .map(|column| {
            let widest = rows.iter().map(|row| row[column].chars().count()).max();
            widest.unwrap_or(0)
        })
        .collect();
    rows.iter()
        .map(|row| {
            let mut line: String = (row.iter().zip(&columns))
                .map(|(cell, &wide)| format!("{cell:<wide$}  "))
                .collect();
            line.push_str(&row[4]);
            render::clip(line.trim_end(), width)
        })
        .collect()
}

/// The helper called `name`, or None once the user has been told there is none.
fn named<'a>(helpers: &'a [Seen], name: &str) -> Option<&'a Seen> {
    let found = helpers.iter().find(|helper| helper.name == name);
    if found.is_none() {
        let names: Vec<&str> = helpers.iter().map(|helper| helper.name.as_str()).collect();
        render::line(&render::dim(&format!(
            "No helper called \"{name}\". This session's: {}.",
            names.join(", ")
        )));
    }
    found
}

/// Stop helper `name` for good: the user's stop is final, so nothing picks
/// the helper back up afterwards.
fn stop_helper(helpers: &[Seen], name: &str) {
    if name.is_empty() {
        render::said(
            &render::blue("●"),
            &render::bold("Which one? /agents stop <name>"),
        );
        return;
    }
    let Some(helper) = named(helpers, name) else {
        return;
    };
    // The stop looks again, since the helper may have ended since the list was read.
    if helper.ended.is_none() && letronna_core::agents::stop(helper.id) {
        // It ends at its next check, not this instant, so the list may still
        // show it running for a moment.
        render::said(
            &render::blue("●"),
            &render::bold(&format!("Stopping {name}")),
        );
    } else {
        render::line(&render::dim(&format!(
            "{name} has already ended; there is nothing to stop."
        )));
    }
}

/// Leave the user's words for helper `name` to read at its next step, as the
/// window's Steer does.
fn send_to_helper(helpers: &[Seen], rest: &str) {
    let (name, message) = rest.split_once(' ').unwrap_or((rest, ""));
    let message = message.trim();
    if name.is_empty() || message.is_empty() {
        render::said(
            &render::blue("●"),
            &render::bold("Tell which one what? /agents send <name> <text>"),
        );
        return;
    }
    let Some(helper) = named(helpers, name) else {
        return;
    };
    // The steer looks again, since the helper may have ended since the list was read.
    if letronna_core::agents::steer(helper.id, message) {
        render::said(
            &render::blue("●"),
            &render::bold(&format!("Sent to {name}; it reads it at its next step")),
        );
    } else {
        render::line(&render::dim(&format!(
            "{name} has ended, so it cannot read this. Ask Letronna to pick it back up."
        )));
    }
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
    // Notices that came for it while another session was open rang for that
    // one, so they are taken now; and a turn stopped over there is no reason
    // for this one to stay quiet.
    runner.stopped = false;
    if let Err(error) = runner.resume(index) {
        render::blank();
        render::line(&render::red(&error));
    }
}

/// Take the session back to just before the last thing you said, and the
/// screen with it. Returns what you said, or `None` when nothing has been.
fn take_back(
    runner: &mut Runner,
    session: usize,
    workspace: Option<&std::path::Path>,
) -> Option<String> {
    let (at, prompt) = last_prompt(&runner.store.conversations[session].messages)?;
    runner.store.conversations[session].rewind(at);
    // What was taken back is still on the screen, where a note saying it is
    // gone would contradict it. Draw the session again from what is left,
    // the way opening it does.
    render::clear();
    banner(runner, &folder_name(workspace), session);
    if at > 0 {
        replay(runner, session);
    }
    // After the redraw, so a failure to save is not cleared off the screen.
    runner.persist();
    Some(prompt)
}

/// Where the last thing you said sits among `messages`, and what it said.
fn last_prompt(messages: &[letronna_core::chat::Message]) -> Option<(usize, String)> {
    let at = messages
        .iter()
        .rposition(|m| m.role == "user" && !m.notice)?;
    Some((at, messages[at].content.clone()))
}

/// Carry on in a copy of the open session, leaving the original as it was.
fn branch(runner: &mut Runner, session: &mut usize, name: &str) {
    let from = &runner.store.conversations[*session];
    // A copy of nothing is only another new session, and /resume offers
    // neither of them back.
    if from.messages.is_empty() {
        render::blank();
        render::line(&render::dim("Nothing to branch from yet."));
        return;
    }
    let title = match name {
        "" => format!("{} (branch)", from.title),
        name => name.to_owned(),
    };
    let copy = store::Conversation {
        id: runner.store.next_id(),
        title: title.clone(),
        messages: from.messages.clone(),
        updated: letronna_core::chat::now(),
        workspace: from.workspace.clone(),
        todos: from.todos.clone(),
        ..Default::default()
    };
    runner.store.conversations.push(copy);
    *session = runner.store.conversations.len() - 1;
    runner.store.active = *session;
    runner.persist();
    render::blank();
    render::line(&render::dim(&format!(
        "Now in {title}. The original is kept; /resume goes back to it."
    )));
}

/// Models offered at once. Some gateways carry hundreds; a list longer than
/// this is narrowed with a word rather than scrolled.
const MODEL_CHOICES: usize = 10;

/// The media kind a `/model` word names, if it names one: `image` or `images`.
fn media_kind(word: &str) -> Option<(&'static str, &'static str)> {
    let word = word.trim().to_lowercase();
    letronna_core::media::KINDS
        .into_iter()
        .find(|(kind, _)| word == *kind || word == format!("{kind}s"))
}

/// Pick the model that makes images, speech, transcripts or video: the one
/// its tool uses when a call names none.
fn switch_media(runner: &mut Runner, kind: &'static str, label: &str) {
    render::blank();
    render::said(
        &render::blue("●"),
        &render::bold(&format!("{label} models")),
    );
    let offers = match runner.bar.busy(
        "asking what each one costs",
        letronna_core::media::catalogue,
    ) {
        Ok(all) => all
            .into_iter()
            .filter(|o| o.kind == kind)
            .collect::<Vec<_>>(),
        Err(why) => {
            render::line(&render::red(&why));
            return;
        }
    };
    if offers.is_empty() {
        render::line(&render::dim("None are offered right now."));
        return;
    }
    let chosen = letronna_core::media::choices().remove(kind);
    let shown = &offers[..offers.len().min(MODEL_CHOICES)];
    let mut labels = vec!["Auto · the cheapest working".to_owned()];
    labels.extend(shown.iter().map(|o| format!("{} · {}", o.id, o.price())));
    let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
    options.push("Keep the one I have");
    let question = format!("In use: {}", chosen.as_deref().unwrap_or("Auto"));
    let Some(picked) = runner.bar.choose(&question, Vec::new(), &options) else {
        return;
    };
    let model = match picked {
        0 => None,
        n if n <= shown.len() => Some(shown[n - 1].id.as_str()),
        _ => return,
    };
    render::blank();
    match letronna_core::media::choose(kind, model) {
        Ok(()) => render::line(&render::dim(&format!(
            "{label} is made by {} now.",
            model.unwrap_or("the cheapest hosted model working")
        ))),
        Err(why) => render::line(&render::red(&why)),
    }
}

/// Change the model, picking from what every set-up gateway actually offers.
/// `filter` narrows the list (by model or gateway name), and an exact model
/// name skips the asking entirely.
pub fn switch_model(runner: &mut Runner, filter: &str) {
    // `/model image`, `/model video`: what makes those, not the chat model.
    if let Some((kind, label)) = media_kind(filter) {
        switch_media(runner, kind, label);
        return;
    }
    let settings = &runner.store.settings;
    let keys = store::load_keys();
    let custom = settings.custom_endpoint.clone();
    let current = format!("{} · {}", settings.model, settings.gateway);

    render::blank();
    render::said(&render::blue("●"), &render::bold("Models"));
    // Fetching takes a moment over the network; say so rather than freezing.
    let (mut models, errors) = runner
        .bar
        .busy("asking every gateway what it has", move || {
            letronna_core::gateway::all_models(&keys, &custom)
        });
    for error in &errors {
        render::line(&render::red(error));
    }
    if models.is_empty() {
        if errors.is_empty() {
            render::line(&render::dim(
                "No gateway offered any models. Sign in with /login, or add a provider with `letronna setup`.",
            ));
        }
        return;
    }
    models.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));

    // An exact name is an instruction, not a search.
    if let Some(exact) = models.iter().find(|(_, m)| m.eq_ignore_ascii_case(filter)) {
        let (gateway, model) = exact.clone();
        set_model(runner, gateway, model);
        return;
    }

    let needle = filter.to_lowercase();
    let matching: Vec<(String, String)> = models
        .iter()
        .filter(|(g, m)| {
            needle.is_empty() || m.to_lowercase().contains(&needle) || g.to_lowercase() == needle
        })
        .cloned()
        .collect();
    if matching.is_empty() {
        render::line(&render::dim(&format!(
            "Nothing matching \"{filter}\" among {} models.",
            models.len()
        )));
        return;
    }

    let shown: Vec<(String, String)> = matching.iter().take(MODEL_CHOICES).cloned().collect();
    if matching.len() > shown.len() {
        render::line(&render::dim(&format!(
            "{} of {} shown — type /model <word> to narrow it.",
            shown.len(),
            matching.len()
        )));
    }
    let labels: Vec<String> = shown.iter().map(|(g, m)| format!("{m} · {g}")).collect();
    let mut options: Vec<&str> = labels.iter().map(String::as_str).collect();
    options.push("Keep the one I have");

    let question = format!("In use: {current}");
    let Some(picked) = runner.bar.choose(&question, Vec::new(), &options) else {
        return;
    };
    match shown.get(picked) {
        Some((gateway, model)) => set_model(runner, gateway.clone(), model.clone()),
        None => render::line(&render::dim("Left as it was.")),
    }
}

/// A model and the gateway it is on, chosen together.
fn set_model(runner: &mut Runner, gateway: String, model: String) {
    runner.store.settings.gateway = gateway.clone();
    runner.store.settings.model = model.clone();
    runner.persist();
    let note = footer(runner);
    runner.bar.set_footer(note);
    render::line(&format!(
        "{} {} {}",
        render::dim("model   "),
        render::bold(&model),
        render::dim(&format!("· {gateway}"))
    ));
}

/// Every search service and where it stands, then set one up to search with,
/// or remove one. The one set up last is used.
pub fn search_services(runner: &mut Runner) {
    use letronna_core::search::{self, SERVICES};
    let mut keys = store::load_keys();
    let mut choice = search::choice();
    let mut rows = Vec::new();
    let mut choices: Vec<(String, &'static str, &'static str)> = Vec::new();
    for s in SERVICES {
        let (status, good, set_up) = search::status(s.name, &choice, &keys);
        rows.push(format!(
            "{} {:<8} {status}",
            if good { "●" } else { "○" },
            s.name
        ));
        let verb = if set_up { "Change" } else { "Set up" };
        choices.push((format!("{verb} {}", s.name), s.name, "set"));
        if set_up {
            choices.push((format!("Remove {}", s.name), s.name, "remove"));
        }
    }
    let mut options: Vec<&str> = choices.iter().map(|c| c.0.as_str()).collect();
    options.push("Done");
    let Some(picked) = runner.bar.choose("Search services", rows, &options) else {
        return;
    };
    let Some((_, name, what)) = choices.get(picked).cloned() else {
        return;
    };
    let Some(service) = search::service(name) else {
        return;
    };
    render::blank();
    if what == "remove" {
        keys.remove(name);
        choice.set_address(service, "");
        if choice.service == name {
            choice.service.clear();
        }
        render::line(&render::dim(&format!("{name} removed.")));
    } else if service.self_hosted() {
        let hint = format!("{name} address, like http://mac-mini:8888");
        let given = runner.bar.ask(&hint, false).unwrap_or_default();
        if given.trim().is_empty() {
            return;
        }
        choice.set_address(service, &given);
    } else {
        let given = runner
            .bar
            .ask(&format!("{name} API key"), true)
            .unwrap_or_default();
        if given.trim().is_empty() {
            return;
        }
        keys.insert(name.to_owned(), given.trim().to_owned());
        // Its official address unless another is given; "official" goes back to it.
        let now = choice.address(service);
        let hint = format!("{name} address (Enter keeps {now}; official resets it)");
        match runner.bar.ask(&hint, false).unwrap_or_default().trim() {
            "" => {}
            "official" => choice.set_address(service, ""),
            other => choice.set_address(service, other),
        }
    }
    if what != "remove" {
        choice.service = name.to_owned();
        render::line(&render::dim(&format!(
            "The model now searches with {name} at {}, asking before each search.",
            choice.address(service)
        )));
    }
    if let Err(error) = store::save_keys(&keys) {
        render::line(&render::red(&format!("Could not save the key: {error}")));
    }
    if let Err(error) = search::choose(&choice) {
        render::line(&render::red(&error));
    }
}

/// Every gateway and where it stands, then set one up, change it or remove it.
/// Everything set up is used at once; `/model` picks among all their models.
pub fn gateways(runner: &mut Runner) {
    use letronna_core::gateway::{self, GATEWAYS, LETRONNA};
    let keys = store::load_keys();
    let custom = runner.store.settings.custom_endpoint.clone();
    let (k, c) = (keys.clone(), custom.clone());
    let (models, _) = runner
        .bar
        .busy("asking every gateway what it has", move || {
            gateway::all_models(&k, &c)
        });

    // The list sits in the box with its choices, not in the conversation.
    // Letronna is the account, not a gateway to add or remove: /login and
    // /logout look after it, so it has a status here and no choice.
    let mut rows = Vec::new();
    let mut choices: Vec<(String, &'static str, &'static str)> = Vec::new();
    for g in GATEWAYS {
        let count = models.iter().filter(|(n, _)| n == g.name).count();
        let (status, good, set_up) = gateway::status(g.name, &keys, &custom, count);
        let status = if g.name == LETRONNA && !set_up {
            format!("{status} · /login")
        } else {
            status
        };
        rows.push(format!(
            "{} {:<11} {status}",
            if good { "●" } else { "○" },
            g.name
        ));
        match (g.name, set_up) {
            (LETRONNA, _) | ("Ollama", _) => {}
            (_, false) => choices.push((format!("Set up {}", g.name), g.name, "set")),
            (_, true) => {
                choices.push((format!("Change {}", g.name), g.name, "set"));
                choices.push((format!("Remove {}", g.name), g.name, "remove"));
            }
        }
    }
    let mut options: Vec<&str> = choices.iter().map(|c| c.0.as_str()).collect();
    options.push("Done");
    let Some(picked) = runner.bar.choose("Gateways", rows, &options) else {
        return;
    };
    let Some((_, name, what)) = choices.get(picked).cloned() else {
        return;
    };
    let mut keys = keys;
    match what {
        "remove" => {
            keys.remove(gateway::key_slot(name));
            if gateway::gateway(name).base.is_empty() {
                runner.store.settings.custom_endpoint.clear();
            }
            render::blank();
            render::line(&render::dim(&format!(
                "{name} removed. Its models leave /model."
            )));
        }
        _ => {
            if gateway::gateway(name).base.is_empty() {
                let url = runner
                    .bar
                    .ask(
                        &format!("{name} base URL, like http://localhost:8000/v1"),
                        false,
                    )
                    .unwrap_or_default();
                if !url.trim().is_empty() {
                    runner.store.settings.custom_endpoint =
                        url.trim().trim_end_matches('/').to_owned();
                }
            }
            let hint = if gateway::gateway(name).needs_key {
                format!("{name} API key")
            } else {
                format!("{name} API key (optional)")
            };
            let key = runner.bar.ask(&hint, true).unwrap_or_default();
            if !key.trim().is_empty() {
                keys.insert(gateway::key_slot(name).to_owned(), key.trim().to_owned());
            }
            render::blank();
            render::line(&render::dim(&format!(
                "{name} saved. /model now lists its models too."
            )));
        }
    }
    if let Err(error) = store::save_keys(&keys) {
        render::line(&render::red(&format!("Could not save the key: {error}")));
    }
    runner.persist();
}

#[cfg(test)]
mod tests {
    use super::{agent_rows, last_prompt};
    use letronna_core::agents::Seen;
    use letronna_core::chat::Message;
    use letronna_core::driver::Status;
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    fn said(role: &str, content: &str) -> Message {
        Message {
            role: role.into(),
            content: content.into(),
            ..Default::default()
        }
    }

    #[test]
    fn taking_back_goes_to_the_last_thing_you_said() {
        // A turn that used a tool ends on its result and the answer after
        // it, and neither of those is something you said.
        let messages = [
            said("user", "first"),
            said("assistant", "ok"),
            said("user", "second"),
            said("assistant", ""),
            said("tool", "a result"),
            said("assistant", "done"),
        ];
        assert_eq!(last_prompt(&messages), Some((2, "second".to_owned())));
    }

    #[test]
    fn nothing_said_means_nothing_to_take_back() {
        assert_eq!(last_prompt(&[]), None);
        assert_eq!(last_prompt(&[said("assistant", "hello")]), None);
    }

    #[test]
    fn rewind_skips_notices() {
        // A notice goes to the model as the user's turn, but the user did
        // not say it, so it is never what gets taken back.
        let notice = Message {
            notice: true,
            ..said("user", "[Letronna notice]\n- job #7 exited with code 0")
        };
        let messages = [
            said("user", "run the tests in the background"),
            said("assistant", "started job #7"),
            notice.clone(),
            said("assistant", "they passed"),
        ];
        assert_eq!(
            last_prompt(&messages),
            Some((0, "run the tests in the background".to_owned()))
        );
        assert_eq!(last_prompt(&[notice]), None);
    }

    fn helper(name: &str, ended: Option<&str>, description: &str, started: Instant) -> Seen {
        let status = Status::default();
        status.input.store(38_000, Ordering::Relaxed);
        Seen {
            id: 1 << 40,
            session: 1,
            name: name.into(),
            kind: "explore".into(),
            description: description.into(),
            n: 1,
            ended: ended.map(String::from),
            started,
            status: Arc::new(status),
        }
    }

    #[test]
    fn agent_rows_fit_the_width() {
        let started = Instant::now();
        let now = started + Duration::from_secs(72);
        let helpers = [
            helper("explore-2", None, "Find where windows start", started),
            helper(
                "a-helper-with-a-rather-long-name",
                Some("stopped by the user"),
                "Run every test in the workspace and report what fails",
                started,
            ),
        ];
        for width in [0, 1, 12, 40, 60, 100] {
            for row in agent_rows(&helpers, now, width) {
                assert!(
                    row.chars().count() <= width,
                    "{row:?} is wider than {width}"
                );
            }
        }
        // A narrow window loses the end of a row, never its start.
        let narrow = agent_rows(&helpers, now, 40);
        assert!(narrow[1].starts_with("explore-2  ") && narrow[1].ends_with('…'));

        // With room for it all, nothing is cut and the columns line up.
        let wide = agent_rows(&helpers, now, 200);
        let at = |row: &str, text: &str| row[..row.find(text).unwrap()].chars().count();
        assert_eq!(
            wide[1],
            format!(
                "explore-2{}explore  running · 1m 12s     38k     Find where windows start",
                " ".repeat(25)
            )
        );
        assert_eq!(at(&wide[0], "task"), at(&wide[1], "Find"));
        assert_eq!(at(&wide[0], "task"), at(&wide[2], "Run every"));
        assert_eq!(at(&wide[0], "state"), at(&wide[2], "stopped"));
    }
}
